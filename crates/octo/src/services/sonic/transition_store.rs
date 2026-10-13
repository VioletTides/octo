//! Every song's transition profile from octo-sonic: one small file per song under
//! `transition-profiles/`, and an index (`transition-profiles.json`) of which song has one,
//! of which file stamp and analysis version. The index is written at most once a minute, like
//! `sonic-features.json`; a profile's own file is written as it comes.
//!
//! Library songs are kept by their Navidrome id, songs from outside the library by their
//! acquisition key (`provider:id`) with the path of their file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use indexmap::IndexMap;
use octo_core::json::datetime;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::services::state_file::{self, null_as_default};

/// One song's profile, as the index knows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct TransitionEntry {
    /// What the song's file was when it was read, so a changed file is read again.
    #[serde(deserialize_with = "null_as_default")]
    pub stamp: String,
    /// The analysis version the profile comes from.
    pub version: i32,
    /// For a song from outside the library: its file.
    pub path: Option<String>,
}

/// The index file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct TransitionIndex {
    /// The analysis version octo-sonic writes, as it last said; 0 before it has.
    pub version: i32,
    #[serde(deserialize_with = "null_as_default")]
    pub songs: IndexMap<String, TransitionEntry>,
}

/// What there is for a song.
#[derive(Debug, Clone, PartialEq)]
pub enum TransitionLookup {
    /// The profile, as octo-sonic wrote it.
    Ready(Value),
    /// None yet, or only one from an older analysis.
    NotReady,
}

// The index of a whole library is a few megabytes; once a minute is plenty.
const FLUSH_EVERY: TimeDelta = TimeDelta::minutes(1);

struct Inner {
    index: TransitionIndex,
    dirty: bool,
    last_flush: DateTime<Utc>,
}

pub struct TransitionStore {
    dir: PathBuf,
    index_path: PathBuf,
    inner: Mutex<Inner>,
    /// Held from the snapshot to the file move, so an older snapshot never lands last.
    file_lock: Mutex<()>,
}

impl TransitionStore {
    /// The store in `dir` (`<dir>/transition-profiles.json` and `<dir>/transition-profiles/`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let index_path = dir.join("transition-profiles.json");
        let index = load(&index_path);
        TransitionStore {
            dir: dir.join("transition-profiles"),
            index_path,
            inner: Mutex::new(Inner {
                index,
                dirty: false,
                last_flush: datetime::min_value(),
            }),
            file_lock: Mutex::new(()),
        }
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// The analysis version octo-sonic writes, when it has said.
    pub fn current_version(&self) -> Option<i32> {
        Some(self.inner.lock().index.version).filter(|&v| v > 0)
    }

    /// What octo-sonic said it writes now; profiles of other versions are no longer served.
    pub fn set_current_version(&self, version: i32) {
        let mut inner = self.inner.lock();
        if inner.index.version != version {
            inner.index.version = version;
            inner.dirty = true;
        }
    }

    pub fn entry(&self, key: &str) -> Option<TransitionEntry> {
        self.inner.lock().index.songs.get(key).cloned()
    }

    /// How many songs have a profile of the current version.
    pub fn ready_count(&self) -> usize {
        let inner = self.inner.lock();
        let version = inner.index.version;
        inner
            .index
            .songs
            .values()
            .filter(|e| e.version == version)
            .count()
    }

    /// Keeps a song's profile. False (with a warning) when its file could not be written.
    pub fn put(&self, key: &str, stamp: &str, path: Option<&str>, profile: &Value) -> bool {
        let version = profile
            .get("version")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
        let file = self.file_of(key);
        if let Err(e) = state_file::save_atomic(&file, &profile.to_string()) {
            warn!("Could not save {}: {e}", file.display());
            return false;
        }
        self.write(|index| {
            index.songs.insert(
                key.to_string(),
                TransitionEntry {
                    stamp: stamp.to_string(),
                    version,
                    path: path.map(str::to_string),
                },
            );
        });
        true
    }

    /// The song's profile when there is one of the analysis version octo-sonic writes now.
    pub fn lookup(&self, key: &str) -> TransitionLookup {
        let current = {
            let inner = self.inner.lock();
            match inner.index.songs.get(key) {
                Some(entry) if inner.index.version == 0 || entry.version == inner.index.version => {
                    entry.version
                }
                _ => return TransitionLookup::NotReady,
            }
        };
        let read = state_file::read_text(&self.file_of(key))
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        match read {
            Some(profile @ Value::Object(_))
                if profile.get("version").and_then(Value::as_i64) == Some(i64::from(current)) =>
            {
                TransitionLookup::Ready(profile)
            }
            _ => TransitionLookup::NotReady,
        }
    }

    /// Forgets the library songs not in `present`, and the outside songs whose file is gone.
    pub fn keep_only(&self, present: &HashSet<String>) {
        let mut gone = Vec::new();
        self.write(|index| {
            index.songs.retain(|key, entry| {
                let keep = match &entry.path {
                    Some(path) => Path::new(path).is_file(),
                    None => present.contains(key),
                };
                if !keep {
                    gone.push(key.clone());
                }
                keep
            });
        });
        for key in gone {
            let _ = std::fs::remove_file(self.file_of(&key));
        }
    }

    /// Forgets every profile.
    pub fn clear(&self) {
        let keys: Vec<String> = self.inner.lock().index.songs.keys().cloned().collect();
        self.write(|index| index.songs.clear());
        for key in keys {
            let _ = std::fs::remove_file(self.file_of(&key));
        }
        self.flush();
    }

    fn write(&self, write: impl FnOnce(&mut TransitionIndex)) {
        let due = {
            let mut inner = self.inner.lock();
            write(&mut inner.index);
            inner.dirty = true;
            Utc::now() - inner.last_flush >= FLUSH_EVERY
        };
        if due {
            self.flush();
        }
    }

    pub fn flush(&self) {
        let _file = self.file_lock.lock();
        let snapshot = {
            let mut inner = self.inner.lock();
            if !inner.dirty {
                return;
            }
            inner.dirty = false;
            inner.last_flush = Utc::now();
            inner.index.clone()
        };
        if let Err(e) = state_file::save_atomic(&self.index_path, &octo_core::json::to_string(&snapshot)) {
            warn!("Could not save {}: {e}", self.index_path.display());
            self.inner.lock().dirty = true;
        }
    }

    /// Whatever changed since the last write goes down on the way out.
    pub async fn run_flusher(self: Arc<Self>, stopping: CancellationToken) -> anyhow::Result<()> {
        stopping.cancelled().await;
        self.flush();
        Ok(())
    }

    // A song's file, named for its key so any id is a safe name, in one of 256 folders.
    fn file_of(&self, key: &str) -> PathBuf {
        let name = hex::encode(Sha256::digest(key.as_bytes()));
        self.dir.join(&name[..2]).join(format!("{}.json", &name[2..32]))
    }
}

/// The index as the store starts from it; a file that cannot be read starts over, with a
/// warning.
fn load(path: &Path) -> TransitionIndex {
    let read = || -> anyhow::Result<TransitionIndex> {
        let Some(text) = state_file::read_text(path)? else {
            return Ok(TransitionIndex::default());
        };
        Ok(serde_json::from_str::<Option<TransitionIndex>>(&text)?.unwrap_or_default())
    };
    read().unwrap_or_else(|e| {
        warn!("{} could not be read; starting over: {e}", path.display());
        TransitionIndex::default()
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn profile(version: i32) -> Value {
        json!({ "version": version, "durationMs": 200_000, "head": {}, "tail": {} })
    }

    #[test]
    fn a_kept_profile_is_served_until_the_analysis_moves_on() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = TransitionStore::new(dir.path());
        assert_eq!(store.lookup("nd-1"), TransitionLookup::NotReady);
        store.set_current_version(1);
        assert!(store.put("nd-1", "10:20", None, &profile(1)));
        assert_eq!(store.lookup("nd-1"), TransitionLookup::Ready(profile(1)));
        assert_eq!(store.ready_count(), 1);
        // octo-sonic now writes version 2: the old profile is stale until read again.
        store.set_current_version(2);
        assert_eq!(store.lookup("nd-1"), TransitionLookup::NotReady);
        assert_eq!(store.ready_count(), 0);
        store.put("nd-1", "10:20", None, &profile(2));
        assert_eq!(store.lookup("nd-1"), TransitionLookup::Ready(profile(2)));
    }

    #[test]
    fn what_was_kept_is_there_after_a_restart_and_a_broken_index_starts_over() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let first = TransitionStore::new(dir.path());
        first.set_current_version(1);
        first.put("nd-1", "10:20", None, &profile(1));
        first.put("youtube:abc", "5:6", Some("/music/a.mp3"), &profile(1));
        first.flush();
        let again = TransitionStore::new(dir.path());
        assert_eq!(again.current_version(), Some(1));
        assert_eq!(again.entry("nd-1").map(|e| e.stamp), Some("10:20".to_string()));
        assert_eq!(
            again.entry("youtube:abc").and_then(|e| e.path),
            Some("/music/a.mp3".to_string())
        );
        assert_eq!(again.lookup("nd-1"), TransitionLookup::Ready(profile(1)));

        std::fs::write(again.index_path(), "{ not json").expect("writes");
        let broken = TransitionStore::new(dir.path());
        assert_eq!(broken.lookup("nd-1"), TransitionLookup::NotReady);
        assert_eq!(broken.current_version(), None);
    }

    #[test]
    fn a_missing_or_damaged_profile_file_is_not_ready() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = TransitionStore::new(dir.path());
        store.set_current_version(1);
        store.put("nd-1", "1:1", None, &profile(1));
        std::fs::write(store.file_of("nd-1"), "{ half").expect("writes");
        assert_eq!(store.lookup("nd-1"), TransitionLookup::NotReady);
        std::fs::remove_file(store.file_of("nd-1")).expect("removes");
        assert_eq!(store.lookup("nd-1"), TransitionLookup::NotReady);
    }

    #[test]
    fn gone_songs_are_forgotten_with_their_files() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = TransitionStore::new(dir.path());
        store.set_current_version(1);
        let landed = dir.path().join("landed.mp3");
        std::fs::write(&landed, b"x").expect("writes");
        let landed = landed.to_string_lossy().into_owned();
        store.put("nd-keep", "1:1", None, &profile(1));
        store.put("nd-gone", "1:1", None, &profile(1));
        store.put("youtube:here", "1:1", Some(&landed), &profile(1));
        store.put("youtube:gone", "1:1", Some("/nowhere/x.mp3"), &profile(1));
        let gone_file = store.file_of("nd-gone");
        store.keep_only(&HashSet::from(["nd-keep".to_string()]));
        assert!(store.entry("nd-keep").is_some() && store.entry("youtube:here").is_some());
        assert!(store.entry("nd-gone").is_none() && store.entry("youtube:gone").is_none());
        assert!(!gone_file.exists());
        store.clear();
        assert_eq!(store.lookup("nd-keep"), TransitionLookup::NotReady);
    }

    #[test]
    fn any_id_makes_a_safe_file_name() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = TransitionStore::new(dir.path());
        let file = store.file_of("../../etc/passwd");
        assert!(file.starts_with(dir.path().join("transition-profiles")));
        assert_eq!(file.file_name().map(|n| n.len()), Some(35));
    }
}
