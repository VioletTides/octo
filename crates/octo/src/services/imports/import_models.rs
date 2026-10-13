//! The imported lists and the trickle's jobs, as `imports.json` and `imports-trickle.json` keep
//! them.

use chrono::{DateTime, Utc};
use octo_core::json::datetime;
use serde::{Deserialize, Serialize};

use crate::services::state_file::null_as_default;

/// Where a list came from. The words are a contract with the dashboard and the Octo apps.
pub mod import_sources {
    pub const SPOTIFY_LIKED: &str = "spotifyLiked";
    pub const SPOTIFY_PLAYLIST: &str = "spotifyPlaylist";
    pub const LINK: &str = "link";
    pub const FILE: &str = "file";

    /// Read again on a schedule: from the account, or from a public link.
    pub fn can_refresh(source: &str) -> bool {
        matches!(source, SPOTIFY_LIKED | SPOTIFY_PLAYLIST | LINK)
    }
}

/// How one song of a list stands. The words are a contract with the dashboard and the Octo apps.
pub mod import_track_states {
    /// In the library already.
    pub const HAVE: &str = "have";
    /// Not in the library, and nobody asked for it yet.
    pub const MISSING: &str = "missing";
    pub const QUEUED: &str = "queued";
    pub const DOWNLOADING: &str = "downloading";
    /// Fetched; waiting for Navidrome to show it, after which it reads as have.
    pub const DONE: &str = "done";
    pub const NOT_FOUND: &str = "notFound";
    pub const SKIPPED: &str = "skipped";

    /// Still to run or running.
    pub fn open(state: &str) -> bool {
        matches!(state, QUEUED | DOWNLOADING)
    }
}

/// One song as the source named it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ImportTrack {
    /// The same song in every list: Spotify's track id when there is one, else its artist and
    /// title.
    #[serde(deserialize_with = "null_as_default")]
    pub key: String,
    #[serde(deserialize_with = "null_as_default")]
    pub title: String,
    /// Every credited artist, joined with commas, the way Spotify shows them.
    #[serde(deserialize_with = "null_as_default")]
    pub artist: String,
    pub album: Option<String>,
    pub seconds: Option<i32>,
    pub isrc: Option<String>,
    /// The library's song for it, from the last match, or None.
    pub library_id: Option<String>,
}

/// One list of songs someone imported: their liked songs, a playlist, a link or a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ImportList {
    /// Stable for the same source, so reading a list again updates it rather than adding one.
    #[serde(deserialize_with = "null_as_default")]
    pub id: String,
    /// The Navidrome user it belongs to.
    #[serde(deserialize_with = "null_as_default")]
    pub owner: String,
    #[serde(deserialize_with = "null_as_default")]
    pub name: String,
    #[serde(deserialize_with = "null_as_default")]
    pub source: String,
    /// Spotify's playlist id, or the link it was read from.
    pub source_ref: Option<String>,
    /// Who made it on Spotify, when that is someone else.
    pub by: Option<String>,
    pub image_url: Option<String>,
    /// Spotify's version of the playlist, so an unchanged one is not read again.
    pub snapshot_id: Option<String>,
    /// How many songs the source says it has, which can be more than were readable.
    pub total: i32,
    #[serde(deserialize_with = "null_as_default")]
    pub tracks: Vec<ImportTrack>,
    /// Why only part of it could be read, in words, or None when it is whole.
    pub partial: Option<String>,
    #[serde(with = "datetime::utc")]
    pub read_utc: DateTime<Utc>,
    #[serde(with = "datetime::utc_option")]
    pub matched_utc: Option<DateTime<Utc>>,
    /// Kept as a Navidrome playlist of the songs the library has, in the source's order.
    pub keep_playlist: bool,
    pub playlist_id: Option<String>,
    /// Whose the playlist is in Navidrome, when Octo could not give it to the owner.
    pub playlist_note: Option<String>,
    /// Its missing songs go to the trickle, and so does every song added to it later.
    pub get_missing: bool,
    /// No longer on the account. Kept while it is a playlist or fetching, so nothing is lost.
    pub gone: bool,
    /// An approver said yes to fetching this family member's list, so it fetches like any
    /// other. Written only once it is true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub family_approved: bool,
}

impl Default for ImportList {
    fn default() -> Self {
        ImportList {
            id: String::new(),
            owner: String::new(),
            name: String::new(),
            source: import_sources::FILE.to_string(),
            source_ref: None,
            by: None,
            image_url: None,
            snapshot_id: None,
            total: 0,
            tracks: Vec::new(),
            partial: None,
            read_utc: datetime::min_value(),
            matched_utc: None,
            keep_playlist: false,
            playlist_id: None,
            playlist_note: None,
            get_missing: false,
            gone: false,
            family_approved: false,
        }
    }
}

/// One missing song the trickle is to fetch for someone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct TrickleJob {
    #[serde(deserialize_with = "null_as_default")]
    pub owner: String,
    #[serde(deserialize_with = "null_as_default")]
    pub key: String,
    #[serde(deserialize_with = "null_as_default")]
    pub title: String,
    #[serde(deserialize_with = "null_as_default")]
    pub artist: String,
    pub album: Option<String>,
    pub seconds: Option<i32>,
    pub isrc: Option<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub state: String,
    /// What happened, in words, or what it is waiting for.
    pub detail: Option<String>,
    /// provider:externalId of its download, while and after it runs.
    pub acquisition_key: Option<String>,
    pub library_id: Option<String>,
    pub attempts: i32,
    #[serde(with = "datetime::utc")]
    pub queued_utc: DateTime<Utc>,
    #[serde(with = "datetime::utc")]
    pub updated_utc: DateTime<Utc>,
    #[serde(with = "datetime::utc_option")]
    pub started_utc: Option<DateTime<Utc>>,
}

impl Default for TrickleJob {
    fn default() -> Self {
        TrickleJob {
            owner: String::new(),
            key: String::new(),
            title: String::new(),
            artist: String::new(),
            album: None,
            seconds: None,
            isrc: None,
            state: import_track_states::QUEUED.to_string(),
            detail: None,
            acquisition_key: None,
            library_id: None,
            attempts: 0,
            queued_utc: datetime::min_value(),
            updated_utc: datetime::min_value(),
            started_utc: None,
        }
    }
}
