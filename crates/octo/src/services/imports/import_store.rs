//! The import store and its file, `imports.json`: every imported list, with its songs and what
//! the last match found, so a restart keeps them and their playlist and fetch switches. Readers
//! get copies, so a list never changes under them.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use octo_core::common::dotnet::{eq_ignore_case, to_lower_invariant};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use super::ImportList;
use super::import_models::import_sources;
use crate::services::state_file::null_as_default;

/// The store's file: `{"Lists": [...]}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ImportFile {
    #[serde(deserialize_with = "null_as_default")]
    pub lists: Vec<ImportList>,
}

/// The same shape over the store's own lists, for writing without a copy.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct FileRef<'a> {
    lists: &'a [ImportList],
}

pub fn load(path: &Path) -> ImportFile {
    super::load_or_default(path, "the imported lists")
}

pub fn save(path: &Path, file: &ImportFile) -> bool {
    super::save(path, file, "the imported lists")
}

/// Every imported list, in memory and on disk.
pub struct ImportStore {
    path: Option<PathBuf>,
    lists: Mutex<Vec<ImportList>>,
}

impl Default for ImportStore {
    fn default() -> Self {
        ImportStore::new(None)
    }
}

impl ImportStore {
    /// No path keeps the lists in memory only, as the tests build it.
    pub fn new(path: Option<PathBuf>) -> Self {
        let lists = path.as_deref().map(|p| load(p).lists).unwrap_or_default();
        ImportStore {
            path,
            lists: Mutex::new(lists),
        }
    }

    /// One person's lists, liked songs first, then by name.
    pub fn lists_of(&self, owner: &str) -> Vec<ImportList> {
        let lists = self.lists.lock();
        let mut mine: Vec<ImportList> = lists
            .iter()
            .filter(|list| eq_ignore_case(&list.owner, owner))
            .cloned()
            .collect();
        // A stable sort: lists that tie keep their order.
        mine.sort_by(|a, b| {
            let liked = |list: &ImportList| i32::from(list.source != import_sources::SPOTIFY_LIKED);
            liked(a)
                .cmp(&liked(b))
                .then_with(|| culture_compare_ignore_case(&a.name, &b.name))
        });
        mine
    }

    /// Everyone's lists, for the work that runs for all of them.
    pub fn all(&self) -> Vec<ImportList> {
        self.lists.lock().clone()
    }

    pub fn get(&self, owner: &str, id: &str) -> Option<ImportList> {
        let lists = self.lists.lock();
        find(&lists, owner, id).map(|i| lists[i].clone())
    }

    /// Adds a list, or replaces the songs and details of the one with its id. What the person
    /// chose for it (a playlist, fetching, the playlist's id) stays, and so does what the last
    /// match found for each song that is still there, until the next match says otherwise.
    pub fn save(&self, mut read: ImportList) -> ImportList {
        let mut lists = self.lists.lock();
        if let Some(at) = find(&lists, &read.owner, &read.id) {
            let existing = lists.remove(at);
            read.keep_playlist = existing.keep_playlist;
            read.playlist_id = existing.playlist_id.clone();
            read.playlist_note = existing.playlist_note.clone();
            read.get_missing = existing.get_missing;
            read.family_approved = existing.family_approved;
            read.matched_utc = existing.matched_utc;
            for track in &mut read.tracks {
                if track.library_id.is_none()
                    && let Some(known) = existing.tracks.iter().find(|t| t.key == track.key)
                {
                    track.library_id = known.library_id.clone();
                }
            }
        }
        lists.push(read.clone());
        self.write(&lists);
        read
    }

    /// Changes one list in place. False when there is no such list.
    pub fn update(&self, owner: &str, id: &str, change: impl FnOnce(&mut ImportList)) -> bool {
        let mut lists = self.lists.lock();
        let Some(at) = find(&lists, owner, id) else {
            return false;
        };
        change(&mut lists[at]);
        self.write(&lists);
        true
    }

    /// Changes every list one person has, in one write.
    pub fn update_all(&self, owner: &str, mut change: impl FnMut(&mut ImportList)) {
        let mut lists = self.lists.lock();
        for list in lists.iter_mut().filter(|list| eq_ignore_case(&list.owner, owner)) {
            change(list);
        }
        self.write(&lists);
    }

    pub fn remove(&self, owner: &str, id: &str) -> bool {
        let mut lists = self.lists.lock();
        let Some(at) = find(&lists, owner, id) else {
            return false;
        };
        lists.remove(at);
        self.write(&lists);
        true
    }

    // Called with the lock held.
    fn write(&self, lists: &[ImportList]) {
        if let Some(path) = &self.path {
            super::save(path, &FileRef { lists }, "the imported lists");
        }
    }
}

// Called with the lock held.
fn find(lists: &[ImportList], owner: &str, id: &str) -> Option<usize> {
    lists
        .iter()
        .position(|list| eq_ignore_case(&list.owner, owner) && list.id == id)
}

/// A case-insensitive name order that sorts accented letters with plain ones: letters compared
/// without their accents and case first, then with their accents. Names equal by both compare
/// equal, so a stable sort keeps their order.
pub(crate) fn culture_compare_ignore_case(a: &str, b: &str) -> Ordering {
    let primary = |s: &str| -> String {
        to_lower_invariant(&s.nfd().filter(|c| !is_combining_mark(*c)).collect::<String>())
    };
    primary(a).cmp(&primary(b)).then_with(|| {
        to_lower_invariant(&a.nfd().collect::<String>())
            .cmp(&to_lower_invariant(&b.nfd().collect::<String>()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust-migration/fixtures/state/imports.json"
    );

    #[test]
    fn the_fixture_round_trips_byte_for_byte() {
        let text = std::fs::read_to_string(FIXTURE).expect("the fixture is in the repo");
        let file: ImportFile = serde_json::from_str(&text).expect("the fixture reads");
        assert_eq!(file.lists.len(), 2);
        assert_eq!(file.lists[0].tracks[0].library_id.as_deref(), Some("nd-8f7e6d5c"));
        assert_eq!(octo_core::json::to_string(&file), text.trim_end_matches('\n'));
        assert_eq!(
            octo_core::json::to_string(&FileRef { lists: &file.lists }),
            text.trim_end_matches('\n')
        );
    }

    #[test]
    fn load_and_save_go_through_the_file() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("config").join("imports.json");
        assert_eq!(load(&path), ImportFile::default(), "no file is no lists");
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("creates");
        std::fs::write(&path, "{ not json").expect("writes");
        assert_eq!(
            load(&path),
            ImportFile::default(),
            "an unreadable file is no lists"
        );
        let file: ImportFile =
            serde_json::from_str(&std::fs::read_to_string(FIXTURE).expect("the fixture")).expect("reads");
        assert!(save(&path, &file));
        assert_eq!(load(&path), file);
    }

    fn list(owner: &str, id: &str, name: &str, source: &str) -> ImportList {
        ImportList {
            id: id.into(),
            owner: owner.into(),
            name: name.into(),
            source: source.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_persons_lists_come_liked_songs_first_then_by_name() {
        let store = ImportStore::default();
        store.save(list("alice", "b", "zebra", import_sources::FILE));
        store.save(list("alice", "a", "Émile", import_sources::FILE));
        store.save(list("Alice", "l", "Liked Songs", import_sources::SPOTIFY_LIKED));
        store.save(list("bob", "c", "Apple", import_sources::FILE));
        let names: Vec<String> = store.lists_of("ALICE").into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["Liked Songs", "Émile", "zebra"]);
    }

    #[test]
    fn saving_again_keeps_the_choices_and_what_the_last_match_found() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("imports.json");
        let store = ImportStore::new(Some(path.clone()));
        let mut first = list("alice", "x", "X", import_sources::FILE);
        first.tracks = vec![super::super::ImportTrack {
            key: "k".into(),
            library_id: Some("nd-1".into()),
            ..Default::default()
        }];
        first.keep_playlist = true;
        first.playlist_id = Some("pl".into());
        store.save(first);
        let mut again = list("alice", "x", "X2", import_sources::FILE);
        again.tracks = vec![super::super::ImportTrack {
            key: "k".into(),
            ..Default::default()
        }];
        let saved = store.save(again);
        assert!(saved.keep_playlist);
        assert_eq!(saved.playlist_id.as_deref(), Some("pl"));
        assert_eq!(saved.tracks[0].library_id.as_deref(), Some("nd-1"));
        assert_eq!(
            ImportStore::new(Some(path)).get("ALICE", "x").map(|l| l.name),
            Some("X2".into())
        );
    }
}
