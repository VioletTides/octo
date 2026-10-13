//! The domain, search, download and Subsonic models.
//!
//! Field names serialize in PascalCase wherever these reach a file, the names existing state
//! files already hold.

use serde::{Deserialize, Deserializer};

/// For a string or list field that is never absent: a JSON `null` in it (a hand edit, say)
/// would make serde fail the whole file. Read `null` as the default instead.
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub mod domain {
    pub mod album;
    pub mod artist;
    pub mod explicit_status;
    pub mod song;

    pub use album::Album;
    pub use artist::Artist;
    pub use song::Song;
}

pub mod search {
    pub mod search_result;

    pub use search_result::SearchResult;
}

pub mod download {
    pub mod acquisition_event;
    pub mod download_history_entry;
    pub mod download_info;
    pub mod download_status;

    pub use acquisition_event::{AcquisitionCandidate, AcquisitionEvent};
    pub use download_history_entry::DownloadHistoryEntry;
    pub use download_info::DownloadInfo;
    pub use download_status::DownloadStatus;
}

pub mod radio {
    pub mod last_fm_radio_state;

    pub use last_fm_radio_state::{
        LastFmRadioBan, LastFmRadioPlay, LastFmRadioStateDocument, LastFmRadioStation,
        LastFmRadioStationKind, LastFmRadioTrack, LastFmRadioUnavailableTrack, LastFmRadioUserState,
        LastFmRadioUserSummary,
    };
}

pub mod subsonic {
    pub mod external_playlist;
    pub mod scan_status;

    pub use external_playlist::ExternalPlaylist;
    pub use scan_status::ScanStatus;
}
