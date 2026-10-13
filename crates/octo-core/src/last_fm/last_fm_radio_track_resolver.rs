//! The pure half of the radio track resolver: whether a library hit is the recording Last.fm
//! recommended. The lookups are `octo::services::last_fm::last_fm_radio_track_resolver`.

use std::sync::LazyLock;

use crate::common::dotnet;
use crate::common::{Lookup, SongIdentity, SongMatchOptions};

static RADIO_MATCH: LazyLock<SongMatchOptions> = LazyLock::new(|| SongMatchOptions {
    length_tolerance_seconds: None,
    also_neutral: vec!["radio edit".to_string()],
    lookup: Lookup::Owned,
    ..Default::default()
});

/// Whether a library hit is the recording Last.fm recommended.
///
/// Biased to NO, the opposite of the download verifier: a false no plays the external copy
/// instead, a false yes silently plays a different song you own. Substring matching in either
/// direction was doing exactly that ("Air" matched "Airbourne", "Intro" matched every intro).
/// So the whole comparison is [`SongIdentity`]'s: the same title, the same version, a shared
/// artist, both artists known. A radio edit counts as the song here, since playing your radio
/// edit for the album cut is what anyone wants from a station. So does the same title written in
/// another script and the same artists credited another way ([`Lookup::Owned`]); a title a typo
/// away does not.
pub fn is_same_recording(want_artist: &str, want_title: &str, hit_artist: &str, hit_title: &str) -> bool {
    !dotnet::is_blank(want_artist)
        && !dotnet::is_blank(hit_artist)
        && SongIdentity::same_text(want_title, want_artist, hit_title, hit_artist, Some(&RADIO_MATCH))
            .is_same()
}

/// Radio prefers a copy you already own, so matching a Last.fm recommendation to the library
/// decides what actually plays. A false match plays a different song with nothing to say so; a
/// missed match only plays the external copy. These pin the cases that went the wrong way under
/// substring matching, and the owned tags that must keep matching.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_recordings_do_not_match() {
        for (want_artist, want_title, hit_artist, hit_title) in [
            ("Air", "Sexy Boy", "Airbourne", "Sexy Boy"), // artist is a substring
            ("Massive Attack", "Intro", "Massive Attack", "Intro (Live)"), // a different take
            ("Radiohead", "Creep", "Radiohead", "Creep (Acoustic)"),
            ("Radiohead", "", "Radiohead", "Creep"), // nothing to compare
            ("", "Creep", "Radiohead", "Creep"),
            ("Daft Punk", "One More Time", "Daft Punk", "One"), // title is a substring
            ("Them", "Gloria", "M", "Gloria"),                  // "the" is a word, not letters
        ] {
            assert!(
                !is_same_recording(want_artist, want_title, hit_artist, hit_title),
                "{want_artist} - {want_title} / {hit_artist} - {hit_title}"
            );
        }
    }

    #[test]
    fn the_same_recording_matches() {
        for (want_artist, want_title, hit_artist, hit_title) in [
            (
                "Massive Attack",
                "Teardrop",
                "Massive Attack feat. Elizabeth Fraser",
                "Teardrop",
            ),
            ("Björk", "Hyperballad", "Bjork", "Hyperballad"),
            ("The Cure", "Lullaby", "Cure", "Lullaby"),
            (
                "Massive Attack",
                "Teardrop - Remastered 2011",
                "Massive Attack",
                "Teardrop",
            ),
            ("deadmau5", "Strobe", "deadmau5", "Strobe (Original Mix)"),
            ("Oasis", "Wonderwall", "Oasis", "Wonderwall (Remastered)"),
            (
                "Oasis",
                "Wonderwall",
                "Oasis",
                "Wonderwall (2014 Remastered Version)",
            ),
            ("Ftown Band", "Hello", "Ftown Band", "Hello"), // "ft" inside a word is not a separator
            ("AC/DC", "Thunderstruck", "AC/DC", "Thunderstruck"),
        ] {
            assert!(
                is_same_recording(want_artist, want_title, hit_artist, hit_title),
                "{want_artist} - {want_title} / {hit_artist} - {hit_title}"
            );
        }
    }

    #[test]
    fn another_script_or_a_credit_written_another_way_is_the_same_recording() {
        for (want_artist, want_title, hit_artist, hit_title) in [
            ("EMIN", "Kamin", "EMIN", "Камин"),
            ("Kino", "Gruppa krovi", "Кино", "Группа крови"),
            ("Luis Fonsi", "Despacito", "Fonsi, Luis", "Despacito"),
        ] {
            assert!(
                is_same_recording(want_artist, want_title, hit_artist, hit_title),
                "{want_artist} - {want_title} / {hit_artist} - {hit_title}"
            );
        }
    }

    #[test]
    fn a_typo_a_tribute_or_another_version_in_another_script_is_not() {
        for (want_artist, want_title, hit_artist, hit_title) in [
            ("Queen", "Bohemian Rhapsody", "Queen", "Bohemian Rapsody"),
            (
                "Queen",
                "Bohemian Rhapsody",
                "Queen Tribute Band",
                "Bohemian Rhapsody",
            ),
            ("EMIN", "Kamin", "EMIN", "Камин (Live)"),
            ("Luis Fonsi", "Despacito", "Luis Miguel", "Despacito"),
        ] {
            assert!(
                !is_same_recording(want_artist, want_title, hit_artist, hit_title),
                "{want_artist} - {want_title} / {hit_artist} - {hit_title}"
            );
        }
    }

    #[test]
    fn an_original_mix_is_not_a_remix() {
        assert!(!is_same_recording(
            "deadmau5",
            "Strobe",
            "deadmau5",
            "Strobe (Remix)"
        ));
    }
}
