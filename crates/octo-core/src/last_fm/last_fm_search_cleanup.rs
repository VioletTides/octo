//! Repairs Last.fm track.search rows that came from mislabeled scrobbles.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::last_fm_service::SimilarTrack;
use crate::common::SongIdentity;
use crate::common::dotnet;

const PLACEHOLDERS: [&str; 4] = ["unknown", "unknownartist", "notavailable", "na"];

// A hyphen or any of the Unicode dashes (U+2010 to U+2015) or the minus sign (U+2212), with or
// without space around it: "A - T", "A-T".
static DASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*[-\u{2010}-\u{2015}\u{2212}]\s*").expect("a valid pattern"));

static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("a valid pattern"));

/// A row carrying fewer than one in this many of its artist's listeners is noise.
const NOISE_RATIO: i64 = 100;

/// Puts right the rows Last.fm's track.search gets from mislabeled scrobbles, before they become
/// outside songs. A search for "Kavinsky Nightcall" answers, among the real song, artist
/// "Kavinsky Nightcall" with title "Nightcall", title "Kavinsky - Nightcall" under the label
/// "Record Makers", and the same under "[unknown]". Taken as they are, those rows reach search
/// results, playback and each listener's Last.fm under the wrong names.
///
/// Only the song's own names are moved about, and only on evidence from the same answer. The
/// row's own artist at the front of its title is always taken out. Moving a song to another
/// artist needs two things: that artist has rows of their own, and the song as repaired is
/// among them, so Radiohead's "Creep - Acoustic" stays Radiohead's even when a band called
/// Creep answers the same search. A title split at spaces alone ("Kanye West   Stronger") needs
/// the same evidence, even under the song's own artist. A row whose artist field is another
/// artist and a dash ("Kanye West - Stronger" with title "!") is dropped when it has under one
/// in a hundred of that artist's listeners. What cannot be put right is left as it was, except
/// a row with no artist at all. Rows that turn out to be one song then keep the place of the
/// first and the spelling of the most listened.
pub struct LastFmSearchCleanup;

impl LastFmSearchCleanup {
    pub fn clean(tracks: &[SimilarTrack]) -> Vec<SimilarTrack> {
        // Every artist the answer names on its own, keyed loosely, with the spelling of its most
        // listened row.
        let mut known: HashMap<String, (String, i64)> = HashMap::new();
        for track in tracks {
            let key = SongIdentity::key(&track.artist);
            if key.is_empty() || is_placeholder(&track.artist) {
                continue;
            }
            let listeners = track.listeners.unwrap_or(0);
            let replace = match known.get(&key) {
                None => true,
                Some((_, current)) => listeners > *current,
            };
            if replace {
                known.insert(key, (track.artist.trim().to_string(), listeners));
            }
        }

        let songs: HashSet<String> = tracks
            .iter()
            .filter(|track| !is_placeholder(&track.artist))
            .map(|track| SongIdentity::match_key(&track.artist, &track.title))
            .collect();

        let cleaned: Vec<SimilarTrack> = tracks
            .iter()
            .filter_map(|track| repair(track, &known, &songs))
            .collect();

        // One song, several rows: the first place, the most listened spelling.
        let mut order: Vec<String> = Vec::new();
        let mut best: HashMap<String, SimilarTrack> = HashMap::new();
        for track in cleaned {
            let key = SongIdentity::match_key(&track.artist, &track.title);
            match best.get(&key) {
                None => {
                    order.push(key.clone());
                    best.insert(key, track);
                }
                Some(kept) if track.listeners.unwrap_or(0) > kept.listeners.unwrap_or(0) => {
                    best.insert(key, track);
                }
                Some(_) => {}
            }
        }
        order.into_iter().filter_map(|key| best.remove(&key)).collect()
    }
}

fn repair(
    track: &SimilarTrack,
    known: &HashMap<String, (String, i64)>,
    songs: &HashSet<String>,
) -> Option<SimilarTrack> {
    let artist = track.artist.trim();
    let title = track.title.trim();
    let artist_key = SongIdentity::key(artist);

    // "Artist - Title" in the title: under the artist itself ("Kanye West - Stronger" by Kanye
    // West), under no artist, or under someone else the answer names on their own, as a label
    // uploading the song ("Kavinsky - Nightcall" by Record Makers).
    let evidenced = |candidate: &str, rest: &str| {
        (SongIdentity::key(candidate) == artist_key || known.contains_key(&SongIdentity::key(candidate)))
            && songs.contains(&SongIdentity::match_key(candidate, rest))
    };
    if let Some((named, rest)) = split_at(&DASH, title, |candidate, rest| {
        SongIdentity::key(candidate) == artist_key || evidenced(candidate, rest)
    })
    .or_else(|| split_at(&SPACES, title, evidenced))
    {
        let named_key = SongIdentity::key(&named);
        let mut repaired = track.clone();
        repaired.artist = match known.get(&named_key) {
            Some((spelled, _)) => spelled.clone(),
            None => named,
        };
        repaired.title = rest;
        return Some(repaired);
    }

    if is_placeholder(artist) {
        if let Some((spaced_artist, spaced_title)) = split_at(&SPACES, title, |candidate, rest| {
            known.contains_key(&SongIdentity::key(candidate))
                && songs.contains(&SongIdentity::match_key(candidate, rest))
        }) && let Some((spelled, _)) = known.get(&SongIdentity::key(&spaced_artist))
        {
            let mut repaired = track.clone();
            repaired.artist = spelled.clone();
            repaired.title = spaced_title;
            return Some(repaired);
        }
        // With no artist to go on, "A - T" still names one, at the first spaced dash.
        return match title.find(" - ") {
            Some(spaced) if spaced > 0 && spaced + 3 < title.len() => {
                let mut repaired = track.clone();
                repaired.artist = title[..spaced].trim().to_string();
                repaired.title = title[spaced + 3..].trim().to_string();
                Some(repaired)
            }
            _ => None,
        };
    }

    // The title on the end of the artist: "Kavinsky Nightcall" or "Kavinsky - Nightcall" with
    // title "Nightcall", when "Kavinsky" is an artist in its own right in the same answer.
    let title_key = SongIdentity::key(title);
    if !title_key.is_empty()
        && dotnet::utf16_len(&artist_key) > dotnet::utf16_len(&title_key)
        && artist_key.ends_with(&title_key)
        && let Some(cut) = ends_with_ignore_case(artist, title)
    {
        let prefix = DASH.replace_all(&artist[..cut], " ").trim().to_string();
        if let Some((spelled, _)) = known.get(&SongIdentity::key(&prefix))
            && songs.contains(&SongIdentity::match_key(&prefix, title))
        {
            let mut repaired = track.clone();
            repaired.artist = spelled.clone();
            return Some(repaired);
        }
    }

    // Another artist and a dash in the artist field, far less listened than that artist:
    // "Kavinsky - Nightcall" with title "Drive", a file named after the film it was in.
    if let Some(listeners) = track.listeners
        && split_at(&DASH, artist, |candidate, _| {
            let key = SongIdentity::key(candidate);
            key != artist_key
                && known
                    .get(&key)
                    .is_some_and(|(_, named)| *named > listeners.saturating_mul(NOISE_RATIO))
        })
        .is_some()
    {
        return None;
    }

    Some(track.clone())
}

/// The text split at the first `separator` whose two sides `accept` takes, trying each in turn so
/// a name with a hyphen or a space in it ("Jay-Z - 99 Problems") is read whole.
fn split_at(separator: &Regex, text: &str, accept: impl Fn(&str, &str) -> bool) -> Option<(String, String)> {
    for found in separator.find_iter(text) {
        let before = text[..found.start()].trim();
        let after = text[found.end()..].trim();
        if before.is_empty() || after.is_empty() {
            continue;
        }
        if accept(before, after) {
            return Some((before.to_string(), after.to_string()));
        }
    }
    None
}

fn is_placeholder(artist: &str) -> bool {
    let key = SongIdentity::key(artist);
    PLACEHOLDERS.contains(&key.as_str()) || key.is_empty()
}

/// Whether `text` ends with `suffix`, ordinally and ignoring case, answering where the matched
/// end starts in `text` (the text without the suffix is then `text[..cut]`).
fn ends_with_ignore_case(text: &str, suffix: &str) -> Option<usize> {
    let count = suffix.chars().count();
    let total = text.chars().count();
    if count > total {
        return None;
    }
    let cut = text
        .char_indices()
        .nth(total - count)
        .map_or(text.len(), |(index, _)| index);
    dotnet::eq_ignore_case(&text[cut..], suffix).then_some(cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(artist: &str, title: &str, listeners: i64) -> SimilarTrack {
        SimilarTrack::new(artist, title, 1.0).with_listeners(Some(listeners))
    }

    fn clean(tracks: &[SimilarTrack]) -> Vec<(String, String)> {
        LastFmSearchCleanup::clean(tracks)
            .into_iter()
            .map(|track| (track.artist, track.title))
            .collect()
    }

    fn pair(artist: &str, title: &str) -> (String, String) {
        (artist.to_string(), title.to_string())
    }

    #[test]
    fn kavinsky_nightcall_as_last_fm_answers_it_is_the_real_song_once() {
        // Last.fm's answer for "Kavinsky Nightcall" on 2026-09-30, in its order.
        let rows = clean(&[
            t("Kavinsky", "Kavinsky - Nightcall", 1324),
            t("Kavinsky Nightcall", "Nightcall", 44),
            t("Kavinsky", "Nightcall", 1257699),
            t("Record Makers", "Kavinsky - Nightcall", 252),
            t("[unknown]", "Kavinsky - Nightcall", 131),
            t("<Unknown>", "Kavinsky - Nightcall", 129),
            t("Kavinsky & Lovefoxxx", "Nightcall", 114928),
            t("Kavinsky \u{2013} Nightcall", "Nightcall", 73),
        ]);

        assert_eq!(rows[0], pair("Kavinsky", "Nightcall"));
        assert_eq!(
            rows.iter()
                .filter(|r| **r == pair("Kavinsky", "Nightcall"))
                .count(),
            1
        );
        assert!(
            !rows
                .iter()
                .any(|(artist, title)| artist.contains("Nightcall") || title.starts_with("Kavinsky")),
            "{rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|(artist, _)| matches!(artist.as_str(), "[unknown]" | "<Unknown>" | "Record Makers")),
            "{rows:?}"
        );
    }

    #[test]
    fn kanye_west_stronger_the_artist_in_the_title_is_taken_out_and_the_most_listened_spelling_kept() {
        let rows = clean(&[
            t("Kanye West", "Kanye West - Stronger", 2342),
            t("Kanye West", "Kanye west -stronger", 1143),
            t("Kanye West", "Kanye West-Stronger", 370),
            t("Kanye", "Kanye West - Stronger", 56),
            t("Kanye West", "Stronger", 2977765),
        ]);

        assert_eq!(rows, vec![pair("Kanye West", "Stronger")]);
    }

    #[test]
    fn kanye_west_stronger_the_rows_that_got_through_at_first_are_gone_too() {
        // More of Last.fm's answer for "Kanye West Stronger" on 2026-09-30.
        let rows = clean(&[
            t("Kanye West", "Stronger", 2977765),
            t("Jeanne29570", "Kanye West Stronger", 25),
            t("Kanye West \u{2013} Stronger", "!", 79),
            t("Kanye West \u{2013} Stronger", "Stronger", 56),
            t("eleonoraalili", "Kanye West   Stronger", 47),
            t("Kanye West", "Kanye West   Stronger", 48),
            t("[unknown]", "Kanye West   Stronger", 30),
            t("<Unknown>", "Kanye West \u{2013} Stronger", 29),
        ]);

        assert_eq!(rows, vec![pair("Kanye West", "Stronger")]);
    }

    #[test]
    fn an_artist_field_naming_another_artist_and_a_dash_is_dropped_when_far_less_listened() {
        let rows = clean(&[
            t("Kavinsky", "Nightcall", 1257699),
            t("Kavinsky - Nightcall", "Drive", 202),
            t("Kavinsky-Nightcall", "Instrumental-Drive", 32),
        ]);

        assert_eq!(rows, vec![pair("Kavinsky", "Nightcall")]);
    }

    #[test]
    fn a_name_with_a_hyphen_is_not_dropped_when_it_is_the_bigger_artist() {
        let rows = clean(&[t("Jay", "Some Song", 900), t("Jay-Z", "99 Problems", 1500000)]);

        assert!(rows.contains(&pair("Jay-Z", "99 Problems")), "{rows:?}");
    }

    #[test]
    fn a_title_starting_with_an_artists_name_is_only_split_at_spaces_on_evidence() {
        // "Queen Bitch" is David Bowie's even though Queen answers the same search.
        let rows = clean(&[
            t("Queen", "Bohemian Rhapsody", 3000000),
            t("David Bowie", "Queen Bitch", 400000),
        ]);

        assert!(rows.contains(&pair("David Bowie", "Queen Bitch")), "{rows:?}");
    }

    /// Every Unicode dash from U+2010 to U+2015 and the minus sign split a title.
    #[test]
    fn every_unicode_dash_and_the_minus_sign_count_as_a_dash() {
        for dash in [
            '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
        ] {
            assert_eq!(
                clean(&[t("Daft Punk", &format!("Daft Punk {dash} One More Time"), 10)]),
                vec![pair("Daft Punk", "One More Time")],
                "U+{:04X}",
                dash as u32
            );
        }
    }

    #[test]
    fn a_hyphen_in_the_artist_is_read_whole() {
        assert_eq!(
            clean(&[t("Jay-Z", "Jay-Z - 99 Problems", 10)]),
            vec![pair("Jay-Z", "99 Problems")]
        );
    }

    #[test]
    fn titles_and_artists_that_only_look_alike_are_left_as_they_are() {
        let rows = clean(&[
            t("Earth, Wind & Fire", "Fire", 500000),
            t("Radiohead", "Creep - Acoustic", 90000),
            t("The Weeknd", "Blinding Lights", 3000000),
        ]);

        assert_eq!(
            rows,
            vec![
                pair("Earth, Wind & Fire", "Fire"),
                pair("Radiohead", "Creep - Acoustic"),
                pair("The Weeknd", "Blinding Lights"),
            ]
        );
    }

    #[test]
    fn another_artist_at_the_front_of_a_title_needs_their_own_row_of_that_song() {
        // A search for "creep" answers with a band called Creep too; Radiohead keeps its song.
        let rows = clean(&[
            t("Radiohead", "Creep", 4234650),
            t("Creep", "You", 9000),
            t("Radiohead", "Creep - Acoustic", 90000),
        ]);

        assert!(rows.contains(&pair("Radiohead", "Creep - Acoustic")), "{rows:?}");
        assert!(!rows.contains(&pair("Creep", "Acoustic")), "{rows:?}");
    }

    #[test]
    fn an_artist_with_the_title_on_the_end_is_only_trimmed_when_the_answer_names_the_shorter_artist() {
        assert_eq!(
            clean(&[t("Kavinsky Nightcall", "Nightcall", 44)]),
            vec![pair("Kavinsky Nightcall", "Nightcall")]
        );
    }

    #[test]
    fn a_row_with_no_artist_takes_it_from_the_title_or_is_dropped() {
        assert_eq!(
            clean(&[t("[unknown]", "Daft Punk - One More Time", 20)]),
            vec![pair("Daft Punk", "One More Time")]
        );
        assert!(clean(&[t("<Unknown>", "Track 01", 20)]).is_empty());
    }

    /// A case-insensitive suffix match and the cut it gives.
    #[test]
    fn ends_with_ignore_case_answers_where_the_suffix_starts() {
        assert_eq!(ends_with_ignore_case("Kavinsky Nightcall", "NIGHTCALL"), Some(9));
        assert_eq!(ends_with_ignore_case("Sigur Rós", "rÓs"), Some(6));
        assert_eq!(ends_with_ignore_case("a", "ab"), None);
        assert_eq!(ends_with_ignore_case("abc", "x"), None);
    }
}
