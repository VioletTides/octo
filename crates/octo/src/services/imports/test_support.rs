//! Stand-ins the import tests share: a Navidrome that keeps playlists in memory, and library rows.

use std::collections::HashMap;

use async_trait::async_trait;
use parking_lot::Mutex;

use super::import_playlists::PlaylistNavidrome;
use crate::services::library::quality_upgrade_worker::LibrarySongRow;

/// A Navidrome with its playlists in memory and every call written down. A create hands out the
/// first free id of `pl1`, `pl2`, ...
pub struct FakeNavidrome {
    pub playlists: Mutex<HashMap<String, Vec<String>>>,
    pub calls: Mutex<Vec<String>>,
    pub admin: Mutex<String>,
    pub give_works: Mutex<bool>,
    pub given_to: Mutex<Option<String>>,
    pub fail_add: Mutex<bool>,
    pub fail_delete: Mutex<bool>,
    pub fail_create: Mutex<bool>,
}

impl Default for FakeNavidrome {
    fn default() -> Self {
        FakeNavidrome {
            playlists: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            admin: Mutex::new("admin".into()),
            give_works: Mutex::new(true),
            given_to: Mutex::new(None),
            fail_add: Mutex::new(false),
            fail_delete: Mutex::new(false),
            fail_create: Mutex::new(false),
        }
    }
}

impl FakeNavidrome {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }

    pub fn playlist(&self, id: &str) -> Vec<String> {
        self.playlists.lock().get(id).cloned().unwrap_or_default()
    }
}

#[async_trait]
impl PlaylistNavidrome for FakeNavidrome {
    async fn create(&self, name: &str, _comment: Option<&str>) -> Option<String> {
        self.calls.lock().push(format!("create {name}"));
        if *self.fail_create.lock() {
            return None;
        }
        let mut playlists = self.playlists.lock();
        let mut n = playlists.len() + 1;
        while playlists.contains_key(&format!("pl{n}")) {
            n += 1;
        }
        let id = format!("pl{n}");
        playlists.insert(id.clone(), Vec::new());
        Some(id)
    }

    async fn give_to(&self, id: &str, user: &str) -> bool {
        self.calls.lock().push(format!("give {id} {user}"));
        *self.given_to.lock() = Some(user.to_string());
        *self.give_works.lock()
    }

    async fn exists(&self, id: &str) -> Option<bool> {
        Some(self.playlists.lock().contains_key(id))
    }

    async fn read_tracks(&self, id: &str) -> Option<Vec<(String, String)>> {
        Some(
            self.playlist(id)
                .into_iter()
                .enumerate()
                .map(|(i, song)| ((i + 1).to_string(), song))
                .collect(),
        )
    }

    async fn remove(&self, id: &str, positions: &[String]) -> bool {
        self.calls.lock().push(format!("remove {}", positions.join(",")));
        let mut positions: Vec<usize> = positions.iter().filter_map(|p| p.parse().ok()).collect();
        positions.sort_unstable_by(|a, b| b.cmp(a));
        let mut playlists = self.playlists.lock();
        let songs = playlists.entry(id.to_string()).or_default();
        for position in positions {
            songs.remove(position - 1);
        }
        true
    }

    async fn add(&self, id: &str, songs: &[String]) -> bool {
        self.calls.lock().push(format!("add {}", songs.join(",")));
        if *self.fail_add.lock() {
            return false;
        }
        self.playlists
            .lock()
            .entry(id.to_string())
            .or_default()
            .extend(songs.iter().cloned());
        true
    }

    async fn delete(&self, id: &str) -> bool {
        self.calls.lock().push(format!("delete {id}"));
        if *self.fail_delete.lock() {
            return false;
        }
        self.playlists.lock().remove(id).is_some()
    }

    fn admin_name(&self) -> Option<String> {
        Some(self.admin.lock().clone())
    }
}

/// A library row for a song at `/music/{id}.{suffix}`.
#[allow(clippy::too_many_arguments)]
pub fn row(
    id: &str,
    artist: &str,
    title: &str,
    seconds: Option<i32>,
    album: Option<&str>,
    suffix: &str,
    bit_rate: i32,
) -> LibrarySongRow {
    LibrarySongRow {
        id: id.into(),
        path: format!("/music/{id}.{suffix}"),
        library_path: None,
        size: 1,
        suffix: suffix.into(),
        bit_rate,
        title: title.into(),
        artist: artist.into(),
        duration: seconds,
        album: album.map(str::to_string),
        album_id: None,
        album_artist: None,
        isrcs: Vec::new(),
        ..Default::default()
    }
}
