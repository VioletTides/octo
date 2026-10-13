//! How radio weighs its sources' answers (multi-source radio): rank decay, agreement adding up,
//! weights, thin answers, guesses only as a last resort, and when the library leads.

use super::*;
use crate::radio::radio_provider::{LAST_FM, LIBRARY, LISTEN_BRAINZ, SOUNDS_ALIKE, YOU_TUBE_MUSIC};

fn track(artist: &str, title: &str, seconds: Option<i32>) -> SimilarTrack {
    SimilarTrack::new(artist, title, 1.0).with_duration(seconds)
}

fn catalog(provider: &str, r#match: RadioMatch, songs: &[(&str, &str)]) -> RadioAnswer {
    RadioAnswer::new(
        provider,
        r#match,
        songs.iter().map(|(a, t)| track(a, t, Some(200))).collect(),
        Vec::new(),
    )
}

fn full(provider: &str, prefix: &str, count: usize) -> RadioAnswer {
    RadioAnswer::new(
        provider,
        RadioMatch::Song,
        (1..=count)
            .map(|i| track(&format!("{prefix} {i}"), &format!("Song {i}"), Some(200)))
            .collect(),
        Vec::new(),
    )
}

fn even() -> RadioWeights {
    [LAST_FM, YOU_TUBE_MUSIC, LISTEN_BRAINZ, SOUNDS_ALIKE, LIBRARY]
        .into_iter()
        .map(|key| (key.to_string(), 1.0))
        .collect()
}

fn titles(picks: &[RadioPick]) -> Vec<String> {
    picks
        .iter()
        .map(|pick| match &pick.track {
            Some(track) => track.title.clone(),
            None => pick.library_song.as_ref().expect("a song").title.clone(),
        })
        .collect()
}

fn song(id: &str, artist: &str, title: &str) -> Song {
    Song {
        id: id.into(),
        artist: artist.into(),
        title: title.into(),
        ..Default::default()
    }
}

#[test]
fn a_song_two_sources_suggest_outranks_one_only_one_does() {
    let last_fm = full(LAST_FM, "A", 25);
    let mut tracks = vec![track("B 1", "Song 1", Some(200))];
    tracks.extend((2..26).map(|i| track(&format!("A {}", i + 5), &format!("Song {}", i + 5), Some(200))));
    let ytm = RadioAnswer::new(YOU_TUBE_MUSIC, RadioMatch::Song, tracks, Vec::new());
    let picks = blend(&[last_fm, ytm], &even(), 10, "seed", None, FULL_ANSWER);
    // "A 7 - Song 7" is seventh for Last.fm and second for YouTube Music: together it beats
    // either source's own first song.
    assert_eq!(titles(&picks)[0], "Song 7");
}

#[test]
fn a_weight_of_nothing_leaves_a_source_out() {
    let mut weights = even();
    weights.insert(YOU_TUBE_MUSIC.into(), 0.0);
    let picks = blend(
        &[full(LAST_FM, "A", 25), full(YOU_TUBE_MUSIC, "Y", 25)],
        &weights,
        50,
        "seed",
        None,
        FULL_ANSWER,
    );
    assert!(!picks.iter().any(|pick| pick.provider == YOU_TUBE_MUSIC));
    assert_eq!(picks.len(), 25);
}

#[test]
fn a_thin_answer_counts_less_than_a_full_one() {
    let thin_answer = catalog(LAST_FM, RadioMatch::Song, &[("Thin", "Only")]);
    let full_answer = full(YOU_TUBE_MUSIC, "Y", 25);
    assert!((thin(&thin_answer, FULL_ANSWER) - 1.0 / f64::from(FULL_ANSWER)).abs() < 1e-6);
    assert_eq!(thin(&full_answer, FULL_ANSWER), 1.0);
    let picks = blend(
        &[thin_answer, full_answer],
        &even(),
        50,
        "seed",
        None,
        FULL_ANSWER,
    );
    let position = titles(&picks).iter().position(|t| t == "Only").expect("listed");
    assert!(position > 10, "a one-song answer does not lead");
}

#[test]
fn a_library_answer_is_never_thin() {
    let album = RadioAnswer::new(
        LIBRARY,
        RadioMatch::Song,
        Vec::new(),
        vec![song("a", "X", "One"), song("b", "X", "Two")],
    );
    assert_eq!(thin(&album, FULL_ANSWER), 1.0);
}

#[test]
fn guessed_artists_play_only_when_nothing_else_came() {
    let guess = catalog(
        LAST_FM,
        RadioMatch::ArtistsGuessed,
        &[("The Year", "Full Damage")],
    );
    assert_eq!(
        titles(&blend(
            std::slice::from_ref(&guess),
            &even(),
            10,
            "seed",
            None,
            FULL_ANSWER
        )),
        ["Full Damage"]
    );
    let genre = catalog(LAST_FM, RadioMatch::Genre, &[("Kordhell", "Murder In My Mind")]);
    assert_eq!(
        titles(&blend(&[guess, genre], &even(), 10, "seed", None, FULL_ANSWER)),
        ["Murder In My Mind"]
    );
}

#[test]
fn the_seed_never_comes_back() {
    let seed_key = seed_normalizer::track_key("Massive Attack", "Teardrop");
    let answer = catalog(
        LAST_FM,
        RadioMatch::Song,
        &[("Massive Attack", "Teardrop"), ("Portishead", "Roads")],
    );
    let library = RadioAnswer::new(
        LIBRARY,
        RadioMatch::Song,
        Vec::new(),
        vec![song("seed-id", "Other", "Other")],
    );
    assert_eq!(
        titles(&blend(
            &[answer, library],
            &even(),
            10,
            &seed_key,
            Some("seed-id"),
            FULL_ANSWER
        )),
        ["Roads"]
    );
}

#[test]
fn a_library_copy_wins_over_a_track_to_look_up_and_a_complete_copy_over_a_stub() {
    let mut stub = RadioAnswer::new(
        SOUNDS_ALIKE,
        RadioMatch::Song,
        Vec::new(),
        vec![song("lib-1", "Portishead", "Roads")],
    );
    stub.library_songs_complete = false;
    let lookup = catalog(LAST_FM, RadioMatch::Song, &[("Portishead", "Roads")]);
    let picks = blend(&[stub.clone(), lookup], &even(), 10, "seed", None, FULL_ANSWER);
    assert_eq!(picks.len(), 1);
    assert_eq!(
        picks[0].library_song.as_ref().map(|s| s.id.as_str()),
        Some("lib-1")
    );
    assert!(picks[0].needs_reading);

    let complete = RadioAnswer::new(
        LIBRARY,
        RadioMatch::Song,
        Vec::new(),
        vec![Song {
            suffix: Some("flac".into()),
            ..song("lib-1", "Portishead", "Roads")
        }],
    );
    let both = blend(&[stub, complete], &even(), 10, "seed", None, FULL_ANSWER);
    assert_eq!(both.len(), 1);
    assert!(!both[0].needs_reading);
    assert_eq!(
        both[0].library_song.as_ref().and_then(|s| s.suffix.as_deref()),
        Some("flac")
    );
}

#[test]
fn the_library_leads_when_the_catalogs_have_fewer_than_twenty_songs_like_it() {
    for (songs, led) in [(19, true), (20, false)] {
        let exact = full(YOU_TUBE_MUSIC, "Y", songs);
        let mut guesses = full(LAST_FM, "A", 25);
        guesses.r#match = RadioMatch::ArtistsTrusted;
        assert_eq!(library_led(&[exact, guesses], FULL_ANSWER), led, "{songs}");
    }
}

#[test]
fn filler_and_one_star_songs_are_left_out() {
    let answer = RadioAnswer::new(
        LAST_FM,
        RadioMatch::Song,
        vec![
            track("A", "Intro", Some(60)),
            track("B", "Keep", Some(200)),
            track("C", "Banned", Some(200)),
            track("D", "Interview with D", Some(200)),
        ],
        Vec::new(),
    );
    let bans: IgnoreCaseKeys = [seed_normalizer::track_key("C", "Banned")].into_iter().collect();
    let kept: Vec<String> = without_filler_or_bans(answer, &bans)
        .tracks
        .into_iter()
        .map(|track| track.title)
        .collect();
    assert_eq!(kept, ["Keep"]);
}

#[test]
fn primary_genre_is_the_first_real_one() {
    for (genre, expected) in [
        (Some("Phonk"), Some("Phonk")),
        (Some("Phonk; Drift Phonk"), Some("Phonk")),
        (Some("Electronic/Dance"), Some("Electronic")),
        (Some("Unknown"), None),
        (Some(""), None),
        (None, None),
    ] {
        assert_eq!(primary_genre(genre).as_deref(), expected, "{genre:?}");
    }
}

#[test]
fn an_answer_with_all_the_songs_asked_is_full_however_short() {
    let eight = RadioAnswer::new(
        LAST_FM,
        RadioMatch::Song,
        (1..=8)
            .map(|i| track("A", &format!("Song {i}"), Some(200)))
            .collect(),
        Vec::new(),
    );
    assert_eq!(thin(&eight, 8), 1.0);
    assert!((thin(&eight, 50) - 0.4).abs() < 1e-6);
    assert!(!library_led(std::slice::from_ref(&eight), 8));
    assert!(library_led(std::slice::from_ref(&eight), 50));
}

#[test]
fn the_same_song_from_a_source_that_knows_its_video_keeps_the_video() {
    let last_fm = catalog(LAST_FM, RadioMatch::Song, &[("Portishead", "Roads")]);
    let ytm = RadioAnswer::new(
        YOU_TUBE_MUSIC,
        RadioMatch::Song,
        vec![
            SimilarTrack::new("Portishead", "Roads", 1.0)
                .with_duration(Some(305))
                .with_you_tube_id(Some("vid".into())),
        ],
        Vec::new(),
    );
    let picks = blend(&[last_fm, ytm], &even(), 10, "seed", None, FULL_ANSWER);
    assert_eq!(picks.len(), 1);
    assert_eq!(
        picks[0].track.as_ref().and_then(|t| t.you_tube_id.as_deref()),
        Some("vid")
    );
}

/// The station factor discounts a guess, and a pick's source is the one that counted
/// most for it.
#[test]
fn stations_keep_guesses_at_a_discount_and_a_pick_names_its_strongest_source() {
    assert_eq!(station_factor(RadioMatch::ArtistsGuessed), GUESS_FACTOR);
    assert_eq!(station_factor(RadioMatch::Genre), 0.8);
    assert_eq!(station_factor(RadioMatch::None), 0.0);
    let mut weights = even();
    weights.insert(LAST_FM.into(), 0.5);
    let picks = blend(
        &[
            catalog(LAST_FM, RadioMatch::Song, &[("A", "One")]),
            catalog(YOU_TUBE_MUSIC, RadioMatch::Song, &[("A", "One")]),
        ],
        &weights,
        10,
        "seed",
        None,
        1,
    );
    assert_eq!(picks[0].provider, YOU_TUBE_MUSIC);
    assert!((picks[0].score - 1.5).abs() < 1e-9);
}
