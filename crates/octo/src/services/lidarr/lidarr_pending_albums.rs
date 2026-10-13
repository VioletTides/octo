//! `lidarr-pending.json`: the albums Octo asked Lidarr for whose files have not all come in yet.
//! A heart waits for Lidarr's import only so long; when Lidarr imports the album later, or
//! after Octo restarted, its webhook names the album, and an album listed here is still taken
//! through the download pipeline. An album nobody listed here is the Lidarr owner's own and is
//! left alone.

use std::path::PathBuf;

use chrono::{DateTime, TimeDelta, Utc};
use indexmap::IndexMap;
use octo_core::common::dotnet;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::services::state_file;

/// How long an album stays listed after Octo asked for it.
pub const KEPT_FOR: TimeDelta = TimeDelta::days(14);

/// One album Octo asked Lidarr for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct PendingAlbum {
    /// The MusicBrainz release group id Lidarr knows the album by.
    pub foreign_album_id: String,
    /// Lidarr's own id for the album.
    pub lidarr_album_id: i32,
    pub artist: String,
    pub title: String,
    pub year: Option<i32>,
    /// The track files Lidarr had before Octo's search; they are the owner's, never taken.
    pub files_before: Vec<i32>,
    /// The tracks that already had a file before Octo's search. Their files stay the owner's
    /// even when Lidarr later replaces them with a new file id.
    pub tracks_before: Vec<i32>,
    /// Who hearted it.
    pub requested_by: Vec<String>,
    pub asked_at: DateTime<Utc>,
}

impl Default for PendingAlbum {
    fn default() -> Self {
        PendingAlbum {
            foreign_album_id: String::new(),
            lidarr_album_id: 0,
            artist: String::new(),
            title: String::new(),
            year: None,
            files_before: Vec::new(),
            tracks_before: Vec::new(),
            requested_by: Vec::new(),
            asked_at: Utc::now(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct PendingFile {
    albums: Vec<PendingAlbum>,
}

/// The listed albums, by foreign album id ignoring case, saved on every change.
pub struct LidarrPendingAlbums {
    path: Option<PathBuf>,
    albums: Mutex<IndexMap<String, PendingAlbum>>,
}

impl LidarrPendingAlbums {
    /// The list over its file; None keeps it in memory only.
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut albums = IndexMap::new();
        if let Some(path) = &path {
            match state_file::read_text(path) {
                Ok(Some(text)) => match serde_json::from_str::<PendingFile>(&text) {
                    Ok(file) => {
                        for album in file.albums {
                            if !album.foreign_album_id.trim().is_empty() {
                                albums
                                    .insert(dotnet::ordinal_ignore_case_key(&album.foreign_album_id), album);
                            }
                        }
                    }
                    Err(e) => warn!(
                        "Could not read {}: {e}; starting with no pending Lidarr albums",
                        path.display()
                    ),
                },
                Ok(None) => {}
                Err(e) => warn!(
                    "Could not read {}: {e}; starting with no pending Lidarr albums",
                    path.display()
                ),
            }
        }
        LidarrPendingAlbums {
            path,
            albums: Mutex::new(albums),
        }
    }

    /// Lists an album, replacing what was listed for it.
    pub fn remember(&self, album: PendingAlbum) {
        let mut albums = self.albums.lock();
        albums.insert(dotnet::ordinal_ignore_case_key(&album.foreign_album_id), album);
        Self::drop_old(&mut albums, Utc::now());
        self.save(&albums);
    }

    /// The listed album, when it was asked for within [`KEPT_FOR`].
    pub fn get(&self, foreign_album_id: &str) -> Option<PendingAlbum> {
        let albums = self.albums.lock();
        albums
            .get(&dotnet::ordinal_ignore_case_key(foreign_album_id))
            .filter(|album| Utc::now() - album.asked_at <= KEPT_FOR)
            .cloned()
    }

    /// Takes an album off the list.
    pub fn forget(&self, foreign_album_id: &str) {
        let mut albums = self.albums.lock();
        if albums
            .shift_remove(&dotnet::ordinal_ignore_case_key(foreign_album_id))
            .is_some()
        {
            self.save(&albums);
        }
    }

    fn drop_old(albums: &mut IndexMap<String, PendingAlbum>, now: DateTime<Utc>) {
        albums.retain(|_, album| now - album.asked_at <= KEPT_FOR);
    }

    fn save(&self, albums: &IndexMap<String, PendingAlbum>) {
        let Some(path) = &self.path else {
            return;
        };
        let file = PendingFile {
            albums: albums.values().cloned().collect(),
        };
        let written = serde_json::to_string_pretty(&file)
            .map_err(std::io::Error::other)
            .and_then(|text| state_file::save_atomic(path, &text));
        if let Err(e) = written {
            warn!("Could not save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(id: &str, asked_at: DateTime<Utc>) -> PendingAlbum {
        PendingAlbum {
            foreign_album_id: id.into(),
            lidarr_album_id: 7,
            artist: "Massive Attack".into(),
            title: "Mezzanine".into(),
            files_before: vec![3],
            requested_by: vec!["alice".into()],
            asked_at,
            ..Default::default()
        }
    }

    #[test]
    fn a_pending_album_survives_a_restart_and_is_found_ignoring_case() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("lidarr-pending.json");
        LidarrPendingAlbums::new(Some(path.clone())).remember(album("rg-ABC", Utc::now()));

        let reloaded = LidarrPendingAlbums::new(Some(path.clone()));
        let found = reloaded.get("rg-abc").expect("listed");
        assert_eq!(found.lidarr_album_id, 7);
        assert_eq!(found.files_before, [3]);
        assert_eq!(found.requested_by, ["alice"]);

        reloaded.forget("RG-abc");
        assert!(LidarrPendingAlbums::new(Some(path)).get("rg-abc").is_none());
    }

    #[test]
    fn an_album_asked_for_too_long_ago_is_no_longer_pending() {
        let pending = LidarrPendingAlbums::new(None);
        pending.remember(album("old", Utc::now() - KEPT_FOR - TimeDelta::hours(1)));
        pending.remember(album("new", Utc::now()));

        assert!(pending.get("old").is_none());
        assert!(pending.get("new").is_some());
    }

    #[test]
    fn an_unreadable_file_starts_empty() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("lidarr-pending.json");
        std::fs::write(&path, "{ not json").expect("written");

        assert!(LidarrPendingAlbums::new(Some(path)).get("anything").is_none());
    }
}
