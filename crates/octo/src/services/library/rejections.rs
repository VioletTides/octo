//! What people said was wrong about a song beside its audio: lyrics they turned down, by song and
//! candidate, and album covers they turned down, by album and picture. Kept in `rejections.json`
//! beside the settings, for good: a rejected answer is never offered for that song or album
//! again. Written whole on every change, through a temporary file; a press is a deliberate act
//! that happens a few times a day at most.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use octo_core::common::dotnet::is_blank;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::services::cover_art::RejectedCovers;
use crate::services::state_file;

/// The file's shape. Pictures are kept by their SHA-256 (hex), and, beside it, by how they look
/// (`cover_image::looks_hash`, hex), so another copy of the same artwork is turned down too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Rejections {
    lyrics: BTreeMap<String, BTreeSet<String>>,
    covers: BTreeMap<String, BTreeSet<String>>,
    cover_looks: BTreeMap<String, BTreeSet<String>>,
}

/// The rejected lyrics and covers, on disk when a path is given.
pub struct RejectionStore {
    path: Option<PathBuf>,
    state: Mutex<Rejections>,
}

impl RejectionStore {
    /// `path` None (or blank) keeps everything in memory only.
    pub fn new(path: Option<PathBuf>) -> Self {
        let path = path.filter(|path| !is_blank(&path.to_string_lossy()));
        let state = path.as_deref().map(load).unwrap_or_default();
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    /// Turns down one lyrics candidate for a song. False when it could not be written down, and
    /// then nothing may be acted on as if it were.
    pub fn reject_lyrics(&self, song_id: &str, candidate_id: &str) -> bool {
        if song_id.trim().is_empty() || candidate_id.trim().is_empty() {
            return false;
        }
        let mut state = self.state.lock();
        let before = state.clone();
        let added = state
            .lyrics
            .entry(song_id.to_string())
            .or_default()
            .insert(candidate_id.to_string());
        self.kept(&mut state, before, added)
    }

    /// Every lyrics candidate turned down for the song.
    pub fn rejected_lyrics(&self, song_id: &str) -> HashSet<String> {
        self.state
            .lock()
            .lyrics
            .get(song_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Turns down one picture for an album: its SHA-256, and how it looks when that could be
    /// told. False when it could not be written down.
    pub fn reject_cover(&self, album_key: &str, sha256: &[u8; 32], looks: Option<u64>) -> bool {
        if album_key.trim().is_empty() {
            return false;
        }
        let mut state = self.state.lock();
        let before = state.clone();
        let mut added = state
            .covers
            .entry(album_key.to_string())
            .or_default()
            .insert(hex::encode(sha256));
        if let Some(looks) = looks {
            added |= state
                .cover_looks
                .entry(album_key.to_string())
                .or_default()
                .insert(format!("{looks:016x}"));
        }
        self.kept(&mut state, before, added)
    }

    /// Every picture turned down for the album.
    pub fn rejected_covers(&self, album_key: &str) -> RejectedCovers {
        let state = self.state.lock();
        let hashes = state
            .covers
            .get(album_key)
            .into_iter()
            .flatten()
            .filter_map(|hex_hash| {
                let bytes = hex::decode(hex_hash).ok()?;
                <[u8; 32]>::try_from(bytes.as_slice()).ok()
            })
            .collect();
        let looks = state
            .cover_looks
            .get(album_key)
            .into_iter()
            .flatten()
            .filter_map(|text| u64::from_str_radix(text, 16).ok())
            .collect();
        RejectedCovers { hashes, looks }
    }

    /// Saves a change; one that cannot be saved is taken back, so nothing acts on a rejection
    /// that would be gone after a restart.
    fn kept(&self, state: &mut Rejections, before: Rejections, added: bool) -> bool {
        if !added || self.save(state) {
            return true;
        }
        *state = before;
        false
    }

    fn save(&self, state: &Rejections) -> bool {
        let Some(path) = &self.path else {
            return true;
        };
        let json = octo_core::json::to_string(state);
        match state_file::save_atomic(path, &json) {
            Ok(()) => true,
            Err(error) => {
                warn!("rejected lyrics and covers could not be written: {error}");
                false
            }
        }
    }
}

/// Unreadable rejections are none, never a failure to start. A file that does not read is set
/// aside as `.bad` first, so the next save does not write over what it held.
fn load(path: &std::path::Path) -> Rejections {
    match state_file::read_text(path) {
        Ok(Some(text)) => serde_json::from_str::<Option<Rejections>>(&text)
            .unwrap_or_else(|error| {
                let mut aside = path.as_os_str().to_owned();
                aside.push(".bad");
                match std::fs::rename(path, &aside) {
                    Ok(()) => warn!(
                        "rejected lyrics and covers could not be read: {error}; the file was kept as {}",
                        std::path::Path::new(&aside).display()
                    ),
                    Err(rename) => warn!(
                        "rejected lyrics and covers could not be read: {error}; nor set aside: {rename}"
                    ),
                }
                None
            })
            .unwrap_or_default(),
        Ok(None) => Rejections::default(),
        Err(error) => {
            warn!("rejected lyrics and covers could not be read: {error}");
            Rejections::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_lyrics_candidate_is_remembered_across_a_restart() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("rejections.json");
        let store = RejectionStore::new(Some(path.clone()));
        assert!(store.reject_lyrics("nd-1", "lrclib:7"));
        assert!(store.reject_lyrics("nd-1", "kugou:3.a"));
        assert!(store.reject_lyrics("nd-1", "lrclib:7"));

        let again = RejectionStore::new(Some(path));
        let rejected = again.rejected_lyrics("nd-1");
        assert_eq!(rejected.len(), 2);
        assert!(rejected.contains("lrclib:7") && rejected.contains("kugou:3.a"));
        assert!(again.rejected_lyrics("nd-2").is_empty());
    }

    #[test]
    fn a_rejected_cover_keeps_its_hash_and_its_look_across_a_restart() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("rejections.json");
        let store = RejectionStore::new(Some(path.clone()));
        let hash = [7u8; 32];
        assert!(store.reject_cover("daft punk|discovery", &hash, Some(0xABCD)));

        let text = std::fs::read_to_string(&path).expect("written");
        assert!(text.contains("\"covers\""), "{text}");
        assert!(text.contains(&hex::encode(hash)), "{text}");

        let rejected = RejectionStore::new(Some(path)).rejected_covers("daft punk|discovery");
        assert!(rejected.hashes.contains(&hash));
        assert_eq!(rejected.looks, vec![0xABCD]);
        assert!(
            RejectionStore::new(None)
                .rejected_covers("daft punk|discovery")
                .is_empty()
        );
    }

    #[test]
    fn an_unreadable_file_is_no_rejections() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("rejections.json");
        std::fs::write(&path, "{ not json").expect("written");
        assert!(RejectionStore::new(Some(path)).rejected_lyrics("nd-1").is_empty());
    }

    #[test]
    fn an_unreadable_file_is_set_aside_before_anything_is_written_over_it() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("rejections.json");
        std::fs::write(&path, "{ not json").expect("written");
        let store = RejectionStore::new(Some(path.clone()));
        assert!(store.reject_lyrics("nd-1", "lrclib:7"));

        let aside = dir.path().join("rejections.json.bad");
        assert_eq!(std::fs::read_to_string(aside).expect("kept"), "{ not json");
        assert!(
            std::fs::read_to_string(&path)
                .expect("written")
                .contains("lrclib:7")
        );
    }

    #[test]
    fn a_rejection_that_cannot_be_saved_is_not_kept() {
        let dir = tempfile::tempdir().expect("a temp dir");
        // A folder where the file should be: the save fails.
        let path = dir.path().join("rejections.json");
        std::fs::create_dir_all(path.join("in-the-way")).expect("a folder");
        let store = RejectionStore::new(Some(path.clone()));

        assert!(!store.reject_lyrics("nd-1", "lrclib:7"));
        assert!(store.rejected_lyrics("nd-1").is_empty());
        assert!(!store.reject_cover("daft punk|discovery", &[7u8; 32], Some(1)));
        assert!(store.rejected_covers("daft punk|discovery").is_empty());

        std::fs::remove_dir_all(&path).expect("cleared");
        assert!(store.reject_lyrics("nd-1", "lrclib:7"));
        assert!(store.rejected_lyrics("nd-1").contains("lrclib:7"));
    }
}
