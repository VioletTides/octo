//! The pure half of the release fact tags: the album grouping a joining track copies, and whether
//! a file was filed under another album than the one it arrived with. The file work is
//! `octo_media::tags::release_fact_tags`.

use crate::common::dotnet::is_blank;
use crate::common::song_identity::SongIdentity;

/// The values an album already in the library groups by, as the library server reads them: its
/// release id, its release date and its version. Copied exactly onto a track that joins it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AlbumGrouping {
    pub album_id: Option<String>,
    pub release_date: Option<String>,
    pub album_version: Option<String>,
}

impl AlbumGrouping {
    pub fn new(
        album_id: Option<String>,
        release_date: Option<String>,
        album_version: Option<String>,
    ) -> Self {
        Self {
            album_id,
            release_date,
            album_version,
        }
    }
}

/// Whether the album the song is written under is another album than the one the file arrived
/// with. Only the title counts: a credit written another way ("JAY-Z & Kanye West" for "JAY-Z")
/// is still the release the peer described. A file that named no album was filed nowhere, so
/// nothing it carries is about another album.
pub fn filed_elsewhere(arrived_album: Option<&str>, album: Option<&str>) -> bool {
    match (arrived_album, album) {
        (Some(arrived), Some(album)) if !is_blank(arrived) && !is_blank(album) => {
            SongIdentity::key(arrived) != SongIdentity::key(album)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filed_elsewhere_is_another_album_title() {
        let cases: [(Option<&str>, Option<&str>, bool); 7] = [
            (Some("The Party Never Ends 2.0"), Some("Love Letter"), true),
            (
                Some("I Want to Die In New Orleans"),
                Some("I Want to Die in New Orleans"),
                false,
            ),
            (Some("Mezzanine"), Some("Mezzanine (Deluxe)"), true),
            (
                Some("good kid, m.A.A.d city"),
                Some("good kid m.A.A.d city"),
                false,
            ),
            (None, Some("Love Letter"), false),
            (Some("  "), Some("Love Letter"), false),
            (Some("Love Letter"), Some(""), false),
        ];
        for (arrived, album, expected) in cases {
            assert_eq!(
                filed_elsewhere(arrived, album),
                expected,
                "{arrived:?} / {album:?}"
            );
        }
    }
}
