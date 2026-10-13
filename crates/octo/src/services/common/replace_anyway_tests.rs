//! What stands between a picked copy and the library song it would replace: a copy that is only
//! no better asks, the plain song in place of another take asks, the wrong song is refused.

use octo_core::models::download::AcquisitionCandidate;
use octo_core::settings::SoulseekSettings;

use super::*;
use crate::services::common::download_picks::{LIDARR_SOURCE, PickedCopy, SOULSEEK_SOURCE};

fn library(title: &str, album: &str, length: i32, format: &str, quality: &str, path: &str) -> FindTarget {
    FindTarget {
        library_id: Some("nd-1".into()),
        owned_format: Some(format.into()),
        owned_quality: Some(quality.into()),
        owned_path: Some(path.into()),
        ..FindTarget::new("Air", title, Some(album), Some(length), "nd-1")
    }
}

fn studio_flac() -> FindTarget {
    library(
        "Sexy Boy",
        "Moon Safari",
        298,
        "flac",
        "FLAC 16-bit 44.1 kHz",
        "/music/Air/Moon Safari/02 - Sexy Boy.flac",
    )
}

fn studio_mp3(rate: i32) -> FindTarget {
    library(
        "Sexy Boy",
        "Moon Safari",
        298,
        "mp3",
        &format!("MP3 {rate} kbps"),
        "/music/Air/Moon Safari/02 - Sexy Boy.mp3",
    )
}

/// A Soulseek copy of `file` (a peer's path) in `format`, `length` seconds long.
fn copy(peer: &str, file: &str, format: &str, bit_rate: Option<i32>, length: Option<i32>) -> FoundCopy {
    let leaf = file.rsplit('\\').next().unwrap_or(file);
    FoundCopy::new(
        AcquisitionCandidate {
            source: SOULSEEK_SOURCE.into(),
            peer: Some(peer.into()),
            file: Some(leaf.into()),
            format: Some(format.into()),
            bit_rate,
            size: Some(30_000_000),
            length,
            ..Default::default()
        },
        PickedCopy {
            source: SOULSEEK_SOURCE.into(),
            peer: Some(peer.into()),
            file: Some(file.into()),
            size: 30_000_000,
            format: Some(format.into()),
            bit_rate,
            length,
            ..Default::default()
        },
    )
}

fn studio_copy(peer: &str, format: &str, bit_rate: Option<i32>, length: i32) -> FoundCopy {
    copy(
        peer,
        &format!(r"Music\Air\Moon Safari (1998)\02 - Sexy Boy.{format}"),
        format,
        bit_rate,
        Some(length),
    )
}

fn lidarr_choice() -> FoundCopy {
    FoundCopy::new(
        AcquisitionCandidate {
            source: LIDARR_SOURCE.into(),
            ..Default::default()
        },
        PickedCopy {
            source: LIDARR_SOURCE.into(),
            ..Default::default()
        },
    )
}

fn lidarr_release(quality: &str) -> FoundCopy {
    FoundCopy::new(
        AcquisitionCandidate {
            source: LIDARR_SOURCE.into(),
            format: Some(quality.into()),
            ..Default::default()
        },
        PickedCopy {
            source: LIDARR_SOURCE.into(),
            release_guid: Some("guid-1".into()),
            indexer_id: Some(3),
            release_title: Some("Air - Moon Safari".into()),
            ..Default::default()
        },
    )
}

fn checked(target: &FindTarget, picked: &FoundCopy, listed: &[FoundCopy]) -> ReplaceCheck {
    let settings = SoulseekSettings::default();
    check(
        target,
        picked,
        CheckContext {
            settings: &settings,
            rejected: None,
            remembers: false,
            listed,
        },
    )
}

fn words(answer: &ReplaceCheck) -> &str {
    match answer {
        ReplaceCheck::Confirm { words, .. } => words,
        other => panic!("expected a question, got {other:?}"),
    }
}

// ---- Quality alone ------------------------------------------------------------------------

#[test]
fn a_better_copy_of_the_right_song_fits() {
    let picked = studio_copy("peer1", "flac", None, 298);
    assert_eq!(checked(&studio_mp3(320), &picked, &[]), ReplaceCheck::Fits);
}

#[test]
fn the_same_quality_asks_and_keeps_the_library_song() {
    let picked = studio_copy("peer1", "flac", None, 298);
    let answer = checked(&studio_flac(), &picked, &[]);
    assert_eq!(
        words(&answer),
        "This copy is FLAC like yours, so it is not better. Replace yours anyway?"
    );
    let ReplaceCheck::Confirm { song, .. } = answer else {
        unreachable!()
    };
    assert_eq!(
        song,
        AnywaySong {
            title: "Sexy Boy".into(),
            album: Some("Moon Safari".into()),
            length: Some(298),
            retitle: false,
        }
    );
}

#[test]
fn a_lower_quality_asks_and_says_it_sounds_worse() {
    let picked = studio_copy("peer1", "mp3", Some(320), 298);
    assert_eq!(
        words(&checked(&studio_flac(), &picked, &[])),
        "This copy is MP3 320 and yours is FLAC, so it sounds worse. Replace yours anyway?"
    );
    let lower = studio_copy("peer1", "mp3", Some(192), 298);
    assert_eq!(
        words(&checked(&studio_mp3(320), &lower, &[])),
        "This copy is MP3 192 and yours is MP3 320, so it sounds worse. Replace yours anyway?"
    );
}

#[test]
fn a_lossy_copy_over_a_lossy_one_asks_in_every_case() {
    let same = studio_copy("peer1", "mp3", Some(320), 298);
    assert_eq!(
        words(&checked(&studio_mp3(320), &same, &[])),
        "This copy is MP3 320 like yours, so it is not better. Replace yours anyway?"
    );
    let higher = studio_copy("peer1", "mp3", Some(320), 298);
    assert_eq!(
        words(&checked(&studio_mp3(128), &higher, &[])),
        "This copy is MP3 320 and yours is MP3 128, so Octo does not count it as better. Replace yours anyway?"
    );
}

#[test]
fn another_lossless_kind_is_not_better_either() {
    let picked = studio_copy("peer1", "wav", None, 298);
    assert_eq!(
        words(&checked(&studio_flac(), &picked, &[])),
        "This copy is WAV and yours is FLAC, so it is not better. Replace yours anyway?"
    );
}

#[test]
fn lidarr_asks_by_its_own_quality_names() {
    assert_eq!(
        words(&checked(&studio_flac(), &lidarr_choice(), &[])),
        "Yours is already FLAC, so Lidarr's choice cannot be better. Replace yours anyway?"
    );
    assert_eq!(
        words(&checked(&studio_flac(), &lidarr_release("MP3-320"), &[])),
        "This copy is MP3 320 and yours is FLAC, so it sounds worse. Replace yours anyway?"
    );
    assert_eq!(
        words(&checked(&studio_flac(), &lidarr_release("FLAC 24bit"), &[])),
        "This copy is FLAC like yours, so it is not better. Replace yours anyway?"
    );
    assert_eq!(
        checked(&studio_mp3(320), &lidarr_choice(), &[]),
        ReplaceCheck::Fits
    );
    assert_eq!(
        checked(&studio_mp3(320), &lidarr_release("FLAC"), &[]),
        ReplaceCheck::Fits
    );
}

#[test]
fn no_question_ever_says_lossless() {
    for picked in [
        studio_copy("peer1", "flac", None, 298),
        studio_copy("peer1", "mp3", Some(320), 298),
        lidarr_choice(),
    ] {
        if let ReplaceCheck::Confirm { words, .. } = checked(&studio_flac(), &picked, &[]) {
            assert!(!words.to_lowercase().contains("lossless"), "{words}");
            assert!(!words.contains('\u{2014}'), "{words}");
        }
    }
}

// ---- The wrong song -------------------------------------------------------------------------

#[test]
fn the_wrong_song_is_refused_whatever_the_quality() {
    let other = copy(
        "peer1",
        r"Music\Air\Moon Safari (1998)\04 - Kelly Watch the Stars.flac",
        "flac",
        None,
        Some(298),
    );
    assert_eq!(
        checked(&studio_flac(), &other, &[]),
        ReplaceCheck::Refused(
            "The file name does not match the title, so it cannot take the place of the one in your library."
                .into()
        )
    );
    let remix = copy(
        "peer1",
        r"Music\Air\Singles\Sexy Boy (Beck Remix).flac",
        "flac",
        None,
        Some(298),
    );
    assert!(matches!(
        checked(&studio_mp3(320), &remix, &[]),
        ReplaceCheck::Refused(_)
    ));
}

#[test]
fn a_length_far_off_with_nothing_to_say_which_is_the_song_is_refused() {
    let picked = studio_copy("peer1", "flac", None, 240);
    assert!(matches!(
        checked(&studio_flac(), &picked, std::slice::from_ref(&picked)),
        ReplaceCheck::Refused(why) if why.starts_with("4:00 long; the song is 4:58")
    ));
}

// ---- Another take in the library -------------------------------------------------------------

fn live_library() -> FindTarget {
    library(
        "Sexy Boy (Live)",
        "Live at the Olympia",
        402,
        "flac",
        "FLAC 16-bit 44.1 kHz",
        "/music/Air/Live at the Olympia/05 - Sexy Boy (Live).flac",
    )
}

#[test]
fn the_plain_song_in_place_of_a_live_take_asks_and_is_held_to_the_plain_song() {
    let picked = studio_copy("peer1", "flac", None, 298);
    let answer = checked(&live_library(), &picked, std::slice::from_ref(&picked));
    assert_eq!(
        words(&answer),
        "Yours looks like a live recording and this copy is the plain song. \
         This copy is FLAC like yours, so it is not better. Replace yours anyway?"
    );
    let ReplaceCheck::Confirm { song, .. } = answer else {
        unreachable!()
    };
    assert_eq!(
        song,
        AnywaySong {
            title: "Sexy Boy".into(),
            album: None,
            length: Some(298),
            retitle: true,
        }
    );
}

#[test]
fn a_live_take_found_only_by_its_folder_counts_and_a_better_copy_still_asks() {
    let target = library(
        "Sexy Boy",
        "Olympia 1998",
        402,
        "mp3",
        "MP3 320 kbps",
        "/music/Air/Olympia 1998 (Live)/05 - Sexy Boy.mp3",
    );
    let picked = studio_copy("peer1", "flac", None, 298);
    assert_eq!(
        words(&checked(&target, &picked, &[])),
        "Yours looks like a live recording and this copy is the plain song. Replace yours anyway?"
    );
}

#[test]
fn another_version_in_the_library_asks_for_the_plain_song() {
    let target = library(
        "Sexy Boy (Radio Edit)",
        "Sexy Boy",
        214,
        "flac",
        "FLAC 16-bit 44.1 kHz",
        "/music/Air/Sexy Boy/01 - Sexy Boy (Radio Edit).flac",
    );
    let picked = studio_copy("peer1", "flac", None, 298);
    assert!(
        words(&checked(&target, &picked, &[]))
            .starts_with("Yours looks like another version and this copy is the plain song.")
    );
}

#[test]
fn another_live_take_in_place_of_a_live_one_is_still_refused_as_another_version() {
    let picked = copy(
        "peer1",
        r"Music\Air\Live 2007\03 - Sexy Boy (Live).flac",
        "flac",
        None,
        Some(330),
    );
    assert!(matches!(
        checked(&live_library(), &picked, &[]),
        ReplaceCheck::Refused(_)
    ));
}

#[test]
fn a_take_far_from_the_usual_length_asks_when_the_listed_copies_agree() {
    // Tagged as the studio song, but seven minutes long: a concert recording filed as the song.
    let target = library(
        "Sexy Boy",
        "Moon Safari",
        420,
        "flac",
        "FLAC 16-bit 44.1 kHz",
        "/music/Air/Moon Safari/02 - Sexy Boy.flac",
    );
    let picked = studio_copy("peer1", "flac", None, 298);
    let listed = vec![
        picked.clone(),
        studio_copy("peer2", "flac", None, 300),
        studio_copy("peer3", "mp3", Some(320), 297),
    ];
    let answer = checked(&target, &picked, &listed);
    assert_eq!(
        words(&answer),
        "Yours runs 7:00 and this song usually runs 4:58, so yours looks like another take. \
         This copy is FLAC like yours, so it is not better. Replace yours anyway?"
    );
    let ReplaceCheck::Confirm { song, .. } = answer else {
        unreachable!()
    };
    assert_eq!(song.length, Some(298));
    assert_eq!(song.album.as_deref(), Some("Moon Safari"));

    // Two copies are not enough to call a length the song's usual one.
    assert!(matches!(
        checked(&target, &picked, &listed[..2]),
        ReplaceCheck::Refused(_)
    ));
    // A pick far from the usual length is not the plain song either.
    let odd = studio_copy("peer4", "flac", None, 360);
    let mut with_odd = listed.clone();
    with_odd.push(odd.clone());
    assert!(matches!(
        checked(&target, &odd, &with_odd),
        ReplaceCheck::Refused(_)
    ));
}
