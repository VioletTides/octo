//! Where a radio's songs from outside the library land. The same rule as the Octo apps'
//! radio (`RadioMix.kt`, `outsideDue`): a draw at the share, so outside songs come at random
//! instead of on a schedule, pulled back once the radio is more than [`SHARE_SLACK`] songs
//! off its share, and never more than [`RARER_RUN`] of the rarer kind in a row.

use super::last_fm_radio_recommendation_service::RadioRandom;
use super::last_fm_radio_spacing::artist_key;

/// How far a radio may drift from its share, in songs, before the next pick puts it right.
pub const SHARE_SLACK: f64 = 1.5;
/// The most songs of the rarer kind in a row.
pub const RARER_RUN: i32 = 2;

/// Whether the next song should be one from outside the library. `run` counts the songs in a
/// row just before: above 0 outside, below 0 library.
pub fn outside_due(
    share: f64,
    picked: usize,
    outside: usize,
    run: i32,
    random: &mut dyn RadioRandom,
) -> bool {
    if share <= 0.0 {
        return false;
    }
    if share >= 1.0 {
        return true;
    }
    let target = share * (picked as f64 + 1.0);
    if target - outside as f64 > SHARE_SLACK {
        return true;
    }
    if outside as f64 + 1.0 - target > SHARE_SLACK {
        return false;
    }
    if share < 0.5 && run >= RARER_RUN {
        return false;
    }
    if share > 0.5 && run <= -RARER_RUN {
        return true;
    }
    random.next_double() < share
}

/// How far apart [`place`] keeps one artist's songs, and one title's.
pub struct Spacing<'a, T> {
    /// A song waits while its artist is among the last this many artists played, counting the
    /// seed's.
    pub artist_gap: usize,
    /// A song's title key, and how many songs must play before that title comes again. None
    /// spaces no titles.
    pub title: Option<(&'a dyn Fn(&T) -> String, usize)>,
}

impl<T> Spacing<'_, T> {
    /// Artists only, as `spread` spaces song radio ([`super::last_fm_radio_spacing::GAP`]).
    pub fn artists(artist_gap: usize) -> Self {
        Spacing {
            artist_gap,
            title: None,
        }
    }
}

/// The share [`place`] paces at: `share`, held to what the radio holds. A radio of `slots`
/// songs drawn from `outside` songs from outside the library and `library` songs from it can be
/// no more than `outside / slots` outside, and no less than `1 - library / slots`. Paced at a
/// share past either bound, one kind would play at the front and leave a solid block of the
/// other at the tail. When the songs cannot fill the slots, the share is their own outside
/// fraction. The same rule as the Octo apps' twin, `pacedShare` in `RadioMix.kt`.
pub fn paced_share(share: f64, outside: usize, library: usize, slots: usize) -> f64 {
    if slots == 0 || outside + library == 0 {
        return share;
    }
    if outside + library < slots {
        return outside as f64 / (outside + library) as f64;
    }
    let slots = slots as f64;
    let most = (outside as f64 / slots).clamp(0.0, 1.0);
    let least = (1.0 - library as f64 / slots).clamp(0.0, 1.0);
    // Held from below and then from above, never with f64::clamp: when the songs just fill the
    // slots the two bounds are one number, and rounding can leave `least` a hair above `most`.
    share.max(least).min(most)
}

/// The songs, each kind (`is_outside`) in its own order, interleaved by [`outside_due`] at
/// `share`, paced to what the list holds ([`paced_share`]): placement keeps every song, so the
/// pace is the list's own outside fraction, and neither kind is left in a block at the tail.
/// Within the due kind a song waits while its artist or its title is too recent (see
/// [`Spacing`]). When every song of the due kind waits, the first song of the other kind that
/// need not wait plays instead. When no song of either kind can play spaced, the due kind's
/// first goes in at the latest place after the opening song where it keeps both gaps (putting
/// a song in only moves the songs after it further from their neighbors). When there is no
/// such place, the first song of either kind, the due kind first, whose artist is not the one
/// that just played goes at the end, and only when there is none the due kind's first: an
/// artist plays twice in a row only when nothing else can. Nothing is dropped: when one kind
/// runs out the other plays on.
pub fn place<T, O, A>(
    songs: &[T],
    is_outside: O,
    artist_of: A,
    seed_artist: Option<&str>,
    spacing: Spacing<'_, T>,
    share: f64,
    random: &mut dyn RadioRandom,
) -> Vec<T>
where
    T: Clone,
    O: Fn(&T) -> bool,
    A: Fn(&T) -> Option<String>,
{
    // Each song beside its keys, worked out once.
    let (mut outside, mut library): (Vec<(T, Keys)>, Vec<(T, Keys)>) = songs
        .iter()
        .map(|song| {
            let key = Keys {
                artist: artist_key(artist_of(song).as_deref()),
                title: spacing
                    .title
                    .map(|(title_of, _)| title_of(song))
                    .unwrap_or_default(),
            };
            (song.clone(), key)
        })
        .partition(|(song, _)| is_outside(song));
    let share = paced_share(share, outside.len(), library.len(), songs.len());
    let gaps = Gaps {
        seed: artist_key(seed_artist),
        artist: spacing.artist_gap,
        title: spacing.title.map_or(0, |(_, gap)| gap),
    };
    let mut placed: Vec<T> = Vec::with_capacity(songs.len());
    // Beside each placed song, its keys.
    let mut keys: Vec<Keys> = Vec::with_capacity(songs.len());
    let (mut taken_outside, mut run) = (0usize, 0i32);
    while !outside.is_empty() || !library.is_empty() {
        let want_outside = if outside.is_empty() {
            false
        } else if library.is_empty() {
            true
        } else {
            outside_due(share, placed.len(), taken_outside, run, random)
        };
        // Which kind the song comes from, where it is in that kind's list, and where it goes.
        let (from_outside, next, at) = {
            let (due, other) = if want_outside {
                (&outside, &library)
            } else {
                (&library, &outside)
            };
            let fits_at_end =
                |pool: &[(T, Keys)]| pool.iter().position(|(_, key)| gaps.fit(&keys, keys.len(), key));
            match (fits_at_end(due), fits_at_end(other)) {
                (Some(next), _) => (want_outside, next, keys.len()),
                // Every song of the due kind would come too soon: the other kind plays first.
                (None, Some(next)) => (!want_outside, next, keys.len()),
                (None, None) => {
                    // Never ahead of the radio's opening song.
                    let first = &due[0].1;
                    match (1..=keys.len()).rev().find(|&at| gaps.fit(&keys, at, first)) {
                        Some(at) => (want_outside, 0, at),
                        None => {
                            let last = keys.last().map(Keys::artist);
                            let differs = |(_, key): &(T, Keys)| {
                                last.is_none_or(|last| key.artist.is_empty() || key.artist != last)
                            };
                            match (due.iter().position(differs), other.iter().position(differs)) {
                                (Some(next), _) => (want_outside, next, keys.len()),
                                (None, Some(next)) => (!want_outside, next, keys.len()),
                                (None, None) => (want_outside, 0, keys.len()),
                            }
                        }
                    }
                }
            }
        };
        let (song, key) = if from_outside {
            outside.remove(next)
        } else {
            library.remove(next)
        };
        if from_outside {
            taken_outside += 1;
        }
        // The run counts the radio's last songs, which a song put in earlier does not change.
        if at == keys.len() {
            run = if from_outside {
                run.max(0) + 1
            } else {
                run.min(0) - 1
            };
        }
        placed.insert(at, song);
        keys.insert(at, key);
    }
    placed
}

/// A placed song's artist and title keys. A blank key (no artist, or no title spacing) never
/// holds a song back and does not count toward a gap.
struct Keys {
    artist: String,
    title: String,
}

impl Keys {
    fn artist(&self) -> &str {
        &self.artist
    }

    fn title(&self) -> &str {
        &self.title
    }
}

/// The gaps [`place`] keeps, with the seed's artist, which plays before the first song.
struct Gaps {
    seed: String,
    artist: usize,
    title: usize,
}

impl Gaps {
    /// Whether a song with `key` can go in at `at` among the placed songs: its artist is not one
    /// of the [`Gaps::artist`] artists just before it (counting the seed) or just after it, nor
    /// its title one of the [`Gaps::title`] titles either side.
    fn fit(&self, placed: &[Keys], at: usize, key: &Keys) -> bool {
        let clear = |of: fn(&Keys) -> &str, mine: &str, gap: usize, seed: Option<&str>| {
            mine.is_empty()
                || !(among(placed[..at].iter().rev().map(of).chain(seed), gap, mine)
                    || among(placed[at..].iter().map(of), gap, mine))
        };
        clear(Keys::artist, key.artist(), self.artist, Some(&self.seed))
            && clear(Keys::title, key.title(), self.title, None)
    }
}

/// Whether `mine` is one of the first `gap` different keys, blank ones not counted, as
/// `spread` keeps its recent artists.
fn among<'a>(keys: impl Iterator<Item = &'a str>, gap: usize, mine: &str) -> bool {
    let mut seen: Vec<&str> = Vec::with_capacity(gap);
    for key in keys.filter(|key| !key.is_empty()) {
        if seen.len() == gap {
            break;
        }
        if key == mine {
            return true;
        }
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::common::dotnet_random::DotnetRandom;
    use crate::last_fm::last_fm_radio_spacing::GAP;

    /// A draw that always gives the same number, so a rule, not the draw, decides.
    struct Fixed(f64);

    impl RadioRandom for Fixed {
        fn next_double(&mut self) -> f64 {
            self.0
        }
    }

    /// `(id, outside)`, each song by an artist of its own, so no artist ever waits.
    fn songs(library: usize, outside: usize) -> Vec<(usize, bool)> {
        (0..library)
            .map(|id| (id, false))
            .chain((library..library + outside).map(|id| (id, true)))
            .collect()
    }

    fn placed(songs: &[(usize, bool)], share: f64, seed: i32) -> Vec<(usize, bool)> {
        place(
            songs,
            |(_, outside)| *outside,
            |(id, _)| Some(format!("Artist {id}")),
            None,
            Spacing::artists(GAP),
            share,
            &mut DotnetRandom::new(seed),
        )
    }

    /// O for a song from outside the library, L for one from it.
    fn pattern(songs: &[(usize, bool)]) -> String {
        songs
            .iter()
            .map(|(_, outside)| if *outside { 'O' } else { 'L' })
            .collect()
    }

    /// The longest run of one kind in a pattern, and the run it ends on.
    fn runs(pattern: &str) -> (usize, usize) {
        let kinds: Vec<char> = pattern.chars().collect();
        let mut longest = 0;
        let mut run = 0;
        for (i, kind) in kinds.iter().enumerate() {
            run = if i > 0 && kinds[i - 1] == *kind {
                run + 1
            } else {
                1
            };
            longest = longest.max(run);
        }
        (longest, run)
    }

    /// The first 30 of each of 200 radios, each placed from 65 library songs and 35 outside
    /// ones, a list that holds the share 0.35.
    fn radios() -> Vec<String> {
        let pool = songs(65, 35);
        (0..200)
            .map(|seed| pattern(&placed(&pool, 0.35, seed)[..30]))
            .collect()
    }

    #[test]
    fn outside_songs_land_at_random_not_on_a_schedule() {
        let radios = radios();
        let openings: HashSet<&str> = radios.iter().map(|radio| &radio[..9]).collect();
        assert!(openings.len() >= 20, "only {} different openings", openings.len());
        let scheduled = radios
            .iter()
            .filter(|radio| radio.starts_with("LLOLLOLLO"))
            .count();
        assert!(
            scheduled < 10,
            "{scheduled} of 200 radios open on the old schedule"
        );

        // The share holds in each radio and across them.
        let mut outside_total = 0;
        for radio in &radios {
            let outside = radio.chars().filter(|kind| *kind == 'O').count();
            assert!((8..=13).contains(&outside), "{outside} outside songs in {radio}");
            outside_total += outside;
        }
        let share = outside_total as f64 / (radios.len() * 30) as f64;
        assert!((0.33..=0.37).contains(&share), "outside share {share}");

        // Under half the radio, outside songs never come three in a row.
        for radio in &radios {
            assert!(!radio.contains("OOO"), "three outside songs in a row in {radio}");
        }
    }

    #[test]
    fn outside_due_follows_its_rules() {
        assert!(
            !outside_due(0.0, 3, 0, 0, &mut Fixed(0.0)),
            "share 0 never plays an outside song"
        );
        assert!(
            outside_due(1.0, 3, 3, 3, &mut Fixed(0.99)),
            "share 1 always plays one"
        );
        assert!(
            outside_due(0.35, 4, 0, -3, &mut Fixed(0.99)),
            "more than SHARE_SLACK short puts it right"
        );
        assert!(
            !outside_due(0.35, 2, 2, 2, &mut Fixed(0.0)),
            "more than SHARE_SLACK over holds it back"
        );
        assert!(
            !outside_due(0.35, 5, 2, 2, &mut Fixed(0.0)),
            "two outside in a row is the most under half"
        );
        assert!(
            outside_due(0.85, 13, 11, -2, &mut Fixed(0.99)),
            "two library in a row is the most over half"
        );
    }

    #[test]
    fn nothing_is_dropped_and_each_kind_keeps_its_order() {
        let pool = songs(7, 20);
        for seed in 0..20 {
            let placed = placed(&pool, 0.35, seed);
            let mut ids: Vec<usize> = placed.iter().map(|(id, _)| *id).collect();
            let library: Vec<usize> = placed.iter().filter(|(_, o)| !o).map(|(id, _)| *id).collect();
            let outside: Vec<usize> = placed.iter().filter(|(_, o)| *o).map(|(id, _)| *id).collect();
            assert_eq!(library, (0..7).collect::<Vec<_>>(), "seed {seed}");
            assert_eq!(outside, (7..27).collect::<Vec<_>>(), "seed {seed}");
            ids.sort_unstable();
            assert_eq!(ids, (0..27).collect::<Vec<_>>(), "seed {seed}");
        }
    }

    #[test]
    fn an_artist_waits_when_another_song_of_the_kind_can_play() {
        let songs = ["A", "A", "B", "C", "D"];
        let placed = place(
            &songs,
            |_| false,
            |artist: &&str| Some(artist.to_string()),
            None,
            Spacing::artists(GAP),
            0.35,
            &mut DotnetRandom::new(1),
        );
        let first = placed
            .iter()
            .position(|artist| *artist == "A")
            .expect("an A plays");
        let second = placed
            .iter()
            .rposition(|artist| *artist == "A")
            .expect("an A plays");
        assert!(
            second - first > 3,
            "the second A came {} after the first: {placed:?}",
            second - first
        );
        assert_eq!(placed.len(), 5);
    }

    #[test]
    fn paced_share_holds_the_share_to_what_the_radio_holds() {
        let cases = [
            // (share, outside, library, slots, paced, why)
            (
                0.35,
                85,
                15,
                100,
                0.85,
                "every song plays: the list's own fraction",
            ),
            (0.35, 15, 85, 100, 0.15, "the other way around"),
            (0.85, 35, 100, 50, 0.7, "no more outside than there are"),
            (
                0.1,
                100,
                35,
                50,
                0.3,
                "no fewer outside than the library leaves room for",
            ),
            (0.35, 60, 60, 30, 0.35, "a share both kinds can fill stays"),
            (
                0.35,
                3,
                2,
                10,
                0.6,
                "too few songs for the slots: their own fraction",
            ),
            (0.35, 0, 0, 10, 0.35, "no songs: the share as asked"),
            (0.35, 5, 5, 0, 0.35, "no slots: the share as asked"),
            (0.0, 2, 1, 3, 2.0 / 3.0, "bounds that meet, rounding and all"),
        ];
        for (share, outside, library, slots, paced, why) in cases {
            let got = paced_share(share, outside, library, slots);
            assert!((got - paced).abs() < 1e-9, "{why}: {got} for {paced}");
        }
    }

    #[test]
    fn any_share_is_paced_to_the_lists_own_mix() {
        // Placement keeps every song, so the share asked for is paced to the list's own: share
        // 0 no longer plays the whole library first and the outside songs in a block after.
        let pool = songs(5, 5);
        for seed in 0..20 {
            let at_zero = placed(&pool, 0.0, seed);
            assert_ne!(pattern(&at_zero), "LLLLLOOOOO", "seed {seed}");
            assert_eq!(at_zero, placed(&pool, 0.5, seed), "seed {seed}");
            assert_eq!(at_zero, placed(&pool, 1.0, seed), "seed {seed}");
        }
    }

    /// The Check Mode finding M1 (2026-10-08): paced at a share the list could not hold, 15
    /// library songs and 85 outside ones played the library early and ended on 78 outside songs
    /// in a row. Paced to the list, neither kind is left in a block. The bounds are what 200
    /// seeds give: the rarer kind never more than [`RARER_RUN`] in a row, the radio never ending
    /// on more than [`LONGEST_TAIL`] of one kind, and the commoner kind never more than
    /// [`LONGEST_COMMON_RUN`] in a row anywhere. At 85 to 15 the commoner kind's runs average
    /// about 6 songs whatever the draw, and the slack lets a lucky streak of draws run on until
    /// the radio is [`SHARE_SLACK`] songs over its share.
    #[test]
    fn a_mix_the_share_cannot_reach_ends_on_no_long_run() {
        for (library, outside) in [(15, 85), (85, 15)] {
            let pool = songs(library, outside);
            let rarer = if outside < library { 'O' } else { 'L' };
            let rarer_run = std::iter::repeat_n(rarer, RARER_RUN as usize + 1).collect::<String>();
            let (mut longest, mut tail, mut tails) = (0, 0, 0);
            for seed in 0..200 {
                let radio = pattern(&placed(&pool, 0.35, seed));
                let (run, last) = runs(&radio);
                assert!(
                    !radio.contains(&rarer_run),
                    "{library}/{outside} seed {seed}: {radio}"
                );
                assert!(last <= LONGEST_TAIL, "{library}/{outside} seed {seed}: {radio}");
                assert!(
                    run <= LONGEST_COMMON_RUN,
                    "{library}/{outside} seed {seed}: {radio}"
                );
                longest = longest.max(run);
                tail = tail.max(last);
                tails += last;
            }
            eprintln!(
                "{library} library + {outside} outside: longest run {longest}, longest tail {tail}, \
                 mean tail {:.2}",
                tails as f64 / 200.0
            );
        }
    }

    /// The most songs of one kind a placed radio ends on, at 85 songs of one kind to 15.
    const LONGEST_TAIL: usize = 10;
    /// The longest run of the commoner kind anywhere, at 85 songs of one kind to 15.
    const LONGEST_COMMON_RUN: usize = 20;

    /// The Check Mode finding M1 (2026-10-08): a short Your Mix or Starter Radio walk, 5
    /// familiar tracks and 18 discoveries, paced at Your Mix's 36% ended on 16 discoveries in a
    /// row. Paced to the list, it ends on 6 at most over 200 seeds.
    #[test]
    fn a_short_walk_ends_on_no_long_run_of_discoveries() {
        let pool = songs(5, 18);
        let (mut tail, mut tails) = (0, 0);
        for seed in 0..200 {
            let radio = pattern(&placed(&pool, 0.36, seed));
            let (_, last) = runs(&radio);
            assert!(last <= 6, "seed {seed}: {radio}");
            tail = tail.max(last);
            tails += last;
        }
        eprintln!(
            "5 familiar + 18 discovery: longest tail {tail}, mean tail {:.2}",
            tails as f64 / 200.0
        );
    }

    /// The Check Mode finding s1 (2026-10-08): when no song can play spaced and there is no
    /// spaced place to put one in, the last resort takes a song whose artist is not the one
    /// that just played before it puts one artist back to back.
    #[test]
    fn an_artist_plays_twice_in_a_row_only_when_nothing_else_can() {
        let songs = [
            ("B1", "B", false),
            ("B2", "B", false),
            ("A1", "A", false),
            ("B3", "B", true),
            ("A2", "A", true),
        ];
        let placed = place(
            &songs,
            |(_, _, outside): &Named| *outside,
            |(_, artist, _): &Named| Some(artist.to_string()),
            None,
            Spacing::artists(GAP),
            0.35,
            &mut Fixed(0.5),
        );
        let artists: Vec<&str> = placed.iter().map(|(_, artist, _)| *artist).collect();
        assert!(
            artists.windows(2).all(|pair| pair[0] != pair[1]),
            "an artist back to back: {:?}",
            names(&placed)
        );
        let mut sorted = names(&placed);
        sorted.sort_unstable();
        assert_eq!(sorted, ["A1", "A2", "B1", "B2", "B3"]);
    }

    /// The Check Mode finding s2 (2026-10-08): the last resort never puts a song in ahead of
    /// the radio's opening song. Here D2 fits nowhere but the front, so it goes at the end.
    #[test]
    fn the_last_resort_never_goes_ahead_of_the_opening_song() {
        let songs = [
            ("A", "A", false),
            ("B", "B", false),
            ("C", "C", false),
            ("D1", "D", false),
            ("E", "E", false),
            ("D2", "D", false),
        ];
        let placed = place(
            &songs,
            |(_, _, outside): &Named| *outside,
            |(_, artist, _): &Named| Some(artist.to_string()),
            None,
            Spacing::artists(GAP),
            0.35,
            &mut Fixed(0.5),
        );
        assert_eq!(names(&placed), ["A", "B", "C", "D1", "E", "D2"]);
    }

    /// The Check Mode finding s2 (2026-10-08): with only two artists the gaps can never all be
    /// kept, so the last resort runs again and again. Nothing is dropped or played twice, and
    /// the radio still opens on the first song of the kind first due.
    #[test]
    fn two_artists_lose_no_song_and_keep_the_opening() {
        let songs: Vec<(String, &str, bool)> = (0..24)
            .map(|i| {
                (
                    format!("song {i}"),
                    if i % 3 == 0 { "A" } else { "B" },
                    i % 4 == 0,
                )
            })
            .collect();
        let outside = songs.iter().filter(|(_, _, outside)| *outside).count();
        let share = paced_share(0.35, outside, songs.len() - outside, songs.len());
        for seed in 0..50 {
            let placed = place(
                &songs,
                |(_, _, outside): &(String, &str, bool)| *outside,
                |(_, artist, _): &(String, &str, bool)| Some(artist.to_string()),
                None,
                Spacing::artists(GAP),
                0.35,
                &mut DotnetRandom::new(seed),
            );
            let mut got: Vec<&str> = placed.iter().map(|(name, _, _)| name.as_str()).collect();
            got.sort_unstable();
            let mut want: Vec<&str> = songs.iter().map(|(name, _, _)| name.as_str()).collect();
            want.sort_unstable();
            assert_eq!(got, want, "seed {seed}");
            // The first draw place makes is the first song's kind.
            let first_outside = outside_due(share, 0, 0, 0, &mut DotnetRandom::new(seed));
            let opening = songs
                .iter()
                .find(|(_, _, outside)| *outside == first_outside)
                .expect("both kinds");
            assert_eq!(placed[0].0, opening.0, "seed {seed}");
        }
    }

    /// `(name, artist, outside)`.
    type Named = (&'static str, &'static str, bool);

    fn names(placed: &[Named]) -> Vec<&'static str> {
        placed.iter().map(|(name, _, _)| *name).collect()
    }

    #[test]
    fn the_other_kind_plays_before_an_artist_comes_back_to_back() {
        let place_named = |songs: &[Named], share: f64| {
            place(
                songs,
                |(_, _, outside): &Named| *outside,
                |(_, artist, _): &Named| Some(artist.to_string()),
                None,
                Spacing::artists(GAP),
                share,
                &mut Fixed(0.5),
            )
        };
        // The draw (0.5) makes the library due every time at this list's share (1 in 3), but
        // its second song is by the artist that just played: the outside song plays between
        // them.
        let songs = [("A1", "A", false), ("A2", "A", false), ("X", "X", true)];
        assert_eq!(names(&place_named(&songs, 0.0)), ["A1", "X", "A2"]);
        // And the other way around: outside is due every time (share 2 in 3).
        let songs = [("B1", "B", true), ("B2", "B", true), ("L", "L", false)];
        assert_eq!(names(&place_named(&songs, 1.0)), ["B1", "L", "B2"]);
    }

    #[test]
    fn a_title_waits_its_gap_when_titles_are_spaced() {
        let title_of = |(name, _, _): &Named| name.trim_end_matches(char::is_numeric).to_string();
        let songs = [
            ("Intro1", "A", false),
            ("Intro2", "B", false),
            ("Song", "C", false),
            ("Tune", "D", false),
        ];
        let placed = place(
            &songs,
            |(_, _, outside): &Named| *outside,
            |(_, artist, _): &Named| Some(artist.to_string()),
            None,
            Spacing {
                artist_gap: GAP,
                title: Some((&title_of, 2)),
            },
            0.35,
            &mut Fixed(0.5),
        );
        assert_eq!(names(&placed), ["Intro1", "Song", "Tune", "Intro2"]);
    }
}
