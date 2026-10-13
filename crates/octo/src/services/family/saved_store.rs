//! `family-saved.json`: the outside songs and albums each member saved with a heart instead
//! of downloading them. They play from their source and show in the member's starred lists.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use octo_core::json::datetime;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::store_file;

/// A saved outside song.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SavedSong {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub cover_art: String,
    /// Seconds.
    #[serde(default)]
    pub duration: i64,
    /// The source's explicit mark (Deezer's `explicit_content_lyrics`: 1 explicit, 0 clean).
    #[serde(default)]
    pub explicit: i32,
    #[serde(with = "datetime::utc")]
    pub saved: DateTime<Utc>,
}

/// A saved outside album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SavedAlbum {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub cover_art: String,
    #[serde(default)]
    pub song_count: i64,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(with = "datetime::utc")]
    pub saved: DateTime<Utc>,
}

/// One member's saved songs and albums.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Saved {
    pub songs: Vec<SavedSong>,
    pub albums: Vec<SavedAlbum>,
}

/// The store behind `family-saved.json`, keyed by lower-case username.
pub struct SavedStore {
    saved: Mutex<IndexMap<String, Saved>>,
    path: Option<PathBuf>,
    save_lock: Mutex<()>,
    load_warning: Option<String>,
}

fn key(user: &str) -> String {
    user.trim().to_lowercase()
}

impl SavedStore {
    pub fn in_memory() -> Self {
        Self::new(None::<PathBuf>)
    }

    pub fn new(path: Option<impl AsRef<Path>>) -> Self {
        let path = store_file::clean_path(path);
        let (loaded, load_warning) = store_file::load::<IndexMap<String, Saved>>(path.as_deref());
        // Names are matched ignoring case; a file edited by hand may spell one two ways.
        let mut saved: IndexMap<String, Saved> = IndexMap::new();
        for (user, entry) in loaded {
            let merged = saved.entry(key(&user)).or_default();
            merged.songs.extend(entry.songs);
            merged.albums.extend(entry.albums);
        }
        SavedStore {
            saved: Mutex::new(saved),
            path,
            save_lock: Mutex::new(()),
            load_warning,
        }
    }

    pub fn load_warning(&self) -> Option<&str> {
        self.load_warning.as_deref()
    }

    /// Saves a song for `user`. False when they already saved that id.
    pub fn add(&self, user: &str, song: SavedSong) -> bool {
        let added = {
            let mut saved = self.saved.lock();
            let entry = saved.entry(key(user)).or_default();
            if entry.songs.iter().any(|s| s.id == song.id) {
                false
            } else {
                entry.songs.push(song);
                true
            }
        };
        if added {
            self.save();
        }
        added
    }

    /// Forgets a saved song or album of `user`. False when there was none with that id.
    pub fn remove(&self, user: &str, id: &str) -> bool {
        let removed = {
            let mut saved = self.saved.lock();
            match saved.get_mut(&key(user)) {
                Some(entry) => {
                    let before = entry.songs.len() + entry.albums.len();
                    entry.songs.retain(|s| s.id != id);
                    entry.albums.retain(|a| a.id != id);
                    before != entry.songs.len() + entry.albums.len()
                }
                None => false,
            }
        };
        if removed {
            self.save();
        }
        removed
    }

    /// `user`'s saved songs, in the order they were saved.
    pub fn list(&self, user: &str) -> Vec<SavedSong> {
        self.saved
            .lock()
            .get(&key(user))
            .map(|entry| entry.songs.clone())
            .unwrap_or_default()
    }

    /// Saves an album for `user`. False when they already saved that id.
    pub fn add_album(&self, user: &str, album: SavedAlbum) -> bool {
        let added = {
            let mut saved = self.saved.lock();
            let entry = saved.entry(key(user)).or_default();
            if entry.albums.iter().any(|a| a.id == album.id) {
                false
            } else {
                entry.albums.push(album);
                true
            }
        };
        if added {
            self.save();
        }
        added
    }

    /// `user`'s saved albums, in the order they were saved.
    pub fn albums(&self, user: &str) -> Vec<SavedAlbum> {
        self.saved
            .lock()
            .get(&key(user))
            .map(|entry| entry.albums.clone())
            .unwrap_or_default()
    }

    /// Whether `user` saved a song or album with this id.
    pub fn is_saved(&self, user: &str, id: &str) -> bool {
        self.saved.lock().get(&key(user)).is_some_and(|entry| {
            entry.songs.iter().any(|s| s.id == id) || entry.albums.iter().any(|a| a.id == id)
        })
    }

    /// Forgets everything `user` saved (the member was removed).
    pub fn remove_user(&self, user: &str) {
        if self.saved.lock().shift_remove(&key(user)).is_some() {
            self.save();
        }
    }

    fn save(&self) {
        store_file::save_with(self.path.as_deref(), &self.save_lock, || {
            self.saved.lock().clone()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 18, 0, 0).unwrap()
    }

    fn song(id: &str) -> SavedSong {
        SavedSong {
            id: id.to_string(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            cover_art: id.to_string(),
            duration: 215,
            explicit: 0,
            saved: now(),
        }
    }

    fn album(id: &str) -> SavedAlbum {
        SavedAlbum {
            id: id.to_string(),
            name: "Album".into(),
            artist: "Artist".into(),
            cover_art: id.to_string(),
            song_count: 12,
            year: Some(2024),
            saved: now(),
        }
    }

    #[test]
    fn add_refuses_duplicate_ids() {
        let store = SavedStore::in_memory();
        assert!(store.add("alex", song("ext-deezer-song-1")));
        assert!(!store.add("ALEX", song("ext-deezer-song-1")));
        assert!(store.add("alex", song("ext-deezer-song-2")));
        assert_eq!(store.list("Alex").len(), 2);
        assert!(store.list("sam").is_empty());
    }

    #[test]
    fn remove_forgets_one_song_or_album() {
        let store = SavedStore::in_memory();
        store.add("alex", song("s1"));
        store.add_album("alex", album("a1"));
        assert!(store.is_saved("alex", "a1"));
        assert!(store.remove("alex", "s1"));
        assert!(!store.remove("alex", "s1"));
        assert!(store.remove("alex", "a1"));
        assert!(!store.is_saved("alex", "a1"));
        assert!(!store.remove("nobody", "s1"));
    }

    #[test]
    fn albums_are_kept_beside_songs() {
        let store = SavedStore::in_memory();
        assert!(store.add_album("alex", album("ext-deezer-album-9")));
        assert!(!store.add_album("alex", album("ext-deezer-album-9")));
        assert_eq!(store.albums("alex"), vec![album("ext-deezer-album-9")]);
        assert!(store.list("alex").is_empty());
    }

    #[test]
    fn the_file_round_trips_and_names_ignore_case() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("family-saved.json");
        let store = SavedStore::new(Some(&path));
        store.add("Alex", song("s1"));
        store.add_album("alex", album("a1"));
        let text = std::fs::read_to_string(&path).expect("reads");
        assert!(text.starts_with(r#"{"alex":{"Songs":[{"Id":"s1""#), "{text}");
        let reloaded = SavedStore::new(Some(&path));
        assert_eq!(reloaded.list("alex"), vec![song("s1")]);
        assert_eq!(reloaded.albums("ALEX"), vec![album("a1")]);
        reloaded.remove_user("alex");
        assert!(SavedStore::new(Some(&path)).list("alex").is_empty());
    }
}
