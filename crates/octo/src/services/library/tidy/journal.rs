//! What the tidy did, one line per change, written and synced to disk BEFORE the change is
//! made. A change nobody wrote down is a change nobody can undo, so a line that cannot be
//! written stops the run. One JSON object per line: a run killed mid-write leaves a torn last
//! line, which costs that one line rather than the whole journal.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::planner::TidyTags;
use crate::services::state_file;

/// One change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TidyJournalEntry {
    /// A file's artist tags are about to be rewritten.
    Tags {
        path: String,
        before: TidyTags,
        after: TidyTags,
        run: String,
        at: DateTime<Utc>,
    },
    /// The file as the rewrite left it: a move only goes ahead while it is still exactly this.
    Stamp {
        path: String,
        size: u64,
        modified: DateTime<Utc>,
        run: String,
    },
    /// An album folder is about to move.
    Move {
        from: String,
        to: String,
        run: String,
        at: DateTime<Utc>,
    },
    /// A playlist file is about to be rewritten; its text before.
    Playlist {
        path: String,
        before: String,
        run: String,
        at: DateTime<Utc>,
    },
}

impl TidyJournalEntry {
    pub fn run(&self) -> &str {
        match self {
            TidyJournalEntry::Tags { run, .. }
            | TidyJournalEntry::Stamp { run, .. }
            | TidyJournalEntry::Move { run, .. }
            | TidyJournalEntry::Playlist { run, .. } => run,
        }
    }
}

/// The journal. With no path it lives in memory (tests).
pub struct TidyJournal {
    path: Option<PathBuf>,
    memory: Mutex<Vec<TidyJournalEntry>>,
    lock: Mutex<()>,
    /// Tests: every write fails, as on a full or read-only disk.
    refuse_writes: AtomicBool,
}

impl TidyJournal {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            memory: Mutex::new(Vec::new()),
            lock: Mutex::new(()),
            refuse_writes: AtomicBool::new(false),
        }
    }

    /// Tests: make every later write fail.
    pub fn refuse_writes(&self) {
        self.refuse_writes.store(true, Ordering::SeqCst);
    }

    /// Whether there is anything to undo.
    pub fn exists(&self) -> bool {
        match &self.path {
            None => !self.memory.lock().is_empty(),
            Some(path) => std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0),
        }
    }

    /// Record one change and sync it to disk. An error means the change must not be made.
    pub fn append(&self, entry: &TidyJournalEntry) -> io::Result<()> {
        if self.refuse_writes.load(Ordering::SeqCst) {
            return Err(io::Error::other("the journal refuses writes"));
        }
        let Some(path) = &self.path else {
            self.memory.lock().push(entry.clone());
            return Ok(());
        };
        let _guard = self.lock.lock();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        let mut line = String::new();
        // A torn last line gets its own line ended first, so this entry is never joined to it.
        if file.metadata()?.len() > 0 {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::End(-1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                line.push('\n');
            }
        }
        line.push_str(&serde_json::to_string(entry).map_err(io::Error::other)?);
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.sync_all()
    }

    /// Every entry, oldest first. A line that does not read (a torn last line) is skipped.
    pub fn read_all(&self) -> Vec<TidyJournalEntry> {
        let Some(path) = &self.path else {
            return self.memory.lock().clone();
        };
        let _guard = self.lock.lock();
        let text = match state_file::read_text(path) {
            Ok(Some(text)) => text,
            Ok(None) => return Vec::new(),
            Err(error) => {
                warn!("tidy journal could not be read: {error}");
                return Vec::new();
            }
        };
        let mut skipped = 0;
        let entries: Vec<TidyJournalEntry> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| match serde_json::from_str(line) {
                Ok(entry) => Some(entry),
                Err(_) => {
                    skipped += 1;
                    None
                }
            })
            .collect();
        if skipped > 0 {
            warn!("tidy journal had {skipped} unreadable line(s), skipped");
        }
        entries
    }

    /// The run of the newest entry: what Undo puts back unless asked for everything.
    pub fn last_run(&self) -> Option<String> {
        self.read_all().last().map(|entry| entry.run().to_string())
    }

    /// Replace the journal with these entries, oldest first: what an undo could not put back
    /// stays undoable. Written to a temporary file and moved into place.
    pub fn rewrite(&self, oldest_first: &[TidyJournalEntry]) -> io::Result<()> {
        let Some(path) = &self.path else {
            *self.memory.lock() = oldest_first.to_vec();
            return Ok(());
        };
        let _guard = self.lock.lock();
        if oldest_first.is_empty() {
            return match std::fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let mut text = String::new();
        for entry in oldest_first {
            text.push_str(&serde_json::to_string(entry).map_err(io::Error::other)?);
            text.push('\n');
        }
        state_file::write_atomic(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(at: &str) -> TidyJournalEntry {
        TidyJournalEntry::Tags {
            path: at.to_string(),
            before: TidyTags {
                artist: vec!["Beyonce".into()],
                ..TidyTags::default()
            },
            after: TidyTags {
                artist: vec!["Beyonc\u{e9}".into()],
                artists: vec!["Beyonc\u{e9}".into(), "Jay-Z".into()],
                ..TidyTags::default()
            },
            run: "r1".into(),
            at: Utc::now(),
        }
    }

    #[test]
    fn a_torn_last_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("tidy-journal.jsonl");
        let journal = TidyJournal::new(Some(path.clone()));
        journal.append(&tags("/m/a.flac")).expect("written");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(b"{\"kind\":\"tags\",\"path\":\"/m/c.fl"))
            .expect("torn");

        let entries = journal.read_all();
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], TidyJournalEntry::Tags { path, .. } if path == "/m/a.flac"));
    }

    #[test]
    fn an_entry_after_a_torn_last_line_is_written_on_a_line_of_its_own() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("tidy-journal.jsonl");
        let journal = TidyJournal::new(Some(path.clone()));
        journal.append(&tags("/m/a.flac")).expect("written");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(b"{\"kind\":\"tags\",\"path\":\"/m/c.fl"))
            .expect("torn");

        journal.append(&tags("/m/b.flac")).expect("written");

        let paths: Vec<String> = journal
            .read_all()
            .into_iter()
            .filter_map(|entry| match entry {
                TidyJournalEntry::Tags { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        assert_eq!(paths, ["/m/a.flac", "/m/b.flac"]);
    }

    #[test]
    fn tags_round_trip_and_the_last_run_is_the_newest() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let journal = TidyJournal::new(Some(dir.path().join("j.jsonl")));
        let first = tags("/m/a.flac");
        let second = TidyJournalEntry::Move {
            from: "/m/A".into(),
            to: "/m/B".into(),
            run: "r2".into(),
            at: Utc::now(),
        };
        journal.append(&first).expect("written");
        journal.append(&second).expect("written");

        let back = TidyJournal::new(Some(dir.path().join("j.jsonl"))).read_all();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].run(), "r1");
        assert_eq!(journal.last_run().as_deref(), Some("r2"));

        journal.rewrite(&[]).expect("emptied");
        assert!(!journal.exists());
    }

    #[test]
    fn a_journal_that_cannot_be_written_says_so() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let blocker = dir.path().join("not-a-folder");
        std::fs::write(&blocker, "").expect("written");
        let journal = TidyJournal::new(Some(blocker.join("j.jsonl")));
        assert!(journal.append(&tags("/m/a.flac")).is_err());
        assert!(!journal.exists());
    }
}
