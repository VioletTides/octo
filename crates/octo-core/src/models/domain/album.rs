use serde::{Deserialize, Serialize};

use super::song::Song;

/// Represents an album
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Album {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: Option<i32>,
    pub song_count: Option<i32>,
    pub cover_art_url: Option<String>,
    pub genre: Option<String>,

    /// OpenSubsonic's release types, such as "album", "ep" or "single": lowercase, as
    /// Navidrome relays them for library albums. What lets a client group an artist's page
    /// the way the catalog does, without guessing from a track count.
    pub release_types: Vec<String>,
    pub is_local: bool,

    /// For an outside album: how many of its songs the library already holds, whatever album it
    /// filed them under, sent as `ownedCount` beside `songCount`. None when its songs were not
    /// looked at, which a client reads as it always has.
    pub owned_count: Option<i32>,

    /// Whether the album's words are explicit (1), the clean edit (3) or neither (0), as the
    /// catalog says; None when it does not. See [`super::explicit_status`].
    pub explicit_content_lyrics: Option<i32>,
    pub external_provider: Option<String>,
    pub external_id: Option<String>,
    pub songs: Vec<Song>,
}
