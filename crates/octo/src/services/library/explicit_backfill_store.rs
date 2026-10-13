//! What the explicit marking's store keeps on disk, `explicit-backfill.json`: the run marking
//! the library explicit or clean (a preview, then the writes it found), so a preview survives a
//! restart. The run itself is `explicit_backfill`.

use std::path::Path;

use chrono::{DateTime, Utc};
use octo_core::json::datetime;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use serde_repr::{Deserialize_repr, Serialize_repr};
use tracing::warn;

use crate::services::state_file::{self, null_as_default};

/// Written as its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
pub enum ExplicitBackfillStatus {
    #[default]
    Idle = 0,
    Running = 1,
    Completed = 2,
    Cancelled = 3,
    Interrupted = 4,
    Failed = 5,
}

/// Written as its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize_repr, Deserialize_repr)]
#[repr(i32)]
pub enum ExplicitBackfillMode {
    #[default]
    Preview = 0,
    Apply = 1,
    Undo = 2,
}

/// One song to look up: its file and Navidrome's id for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ExplicitBackfillSong {
    #[serde(deserialize_with = "null_as_default")]
    pub path: String,
    pub navidrome_id: Option<String>,
}

/// One song the preview looked up, and what became of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ExplicitBackfillRow {
    #[serde(deserialize_with = "null_as_default")]
    pub path: String,
    pub navidrome_id: Option<String>,
    pub artist: Option<String>,
    pub title: Option<String>,
    /// "explicit", "clean", "notExplicit" or "unsure".
    #[serde(deserialize_with = "null_as_default")]
    pub outcome: String,
    #[serde(deserialize_with = "null_as_default")]
    pub how: String,
    pub catalog_track_id: Option<String>,
    /// The file as the preview read it, so Apply leaves alone a file changed since.
    pub size: i64,
    pub write_ticks: i64,
    pub written: bool,
}

/// Whether an outcome is one Apply writes (`ExplicitBackfill.Writes`).
pub fn writes(outcome: &str) -> bool {
    matches!(outcome, "explicit" | "clean")
}

/// The run, serialized by hand ([`Serialize`] below) because the file also carries the computed
/// `CanResume` and `CanApply`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ExplicitBackfillRun {
    #[serde(deserialize_with = "null_as_default")]
    pub run_id: String,
    pub status: ExplicitBackfillStatus,
    pub mode: ExplicitBackfillMode,
    #[serde(with = "datetime::utc_option")]
    pub started_utc: Option<DateTime<Utc>>,
    #[serde(with = "datetime::utc_option")]
    pub finished_utc: Option<DateTime<Utc>>,
    pub total: i32,
    pub processed: i32,
    pub explicit: i32,
    pub clean: i32,
    pub not_explicit: i32,
    pub unsure: i32,
    /// Songs that already carry an advisory, which a preview never looks up.
    pub already_marked: i32,
    pub failed: i32,
    /// Apply and Undo: how many files the step has to go through, and has.
    pub step_total: i32,
    pub step_done: i32,
    /// Apply: files written. Undo: files put back.
    pub written: i32,
    /// Apply and Undo: files changed since, so left as they are.
    pub left_alone: i32,
    pub last_song: Option<String>,
    pub reason: Option<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub errors: Vec<String>,
    /// Every song the preview looked up. Apply writes from these and nothing else.
    #[serde(deserialize_with = "null_as_default")]
    pub rows: Vec<ExplicitBackfillRow>,
    /// What a preview still has to look up, for a resume.
    #[serde(deserialize_with = "null_as_default")]
    pub queue: Vec<ExplicitBackfillSong>,
    pub cursor: i32,
    /// The run whose writes Undo takes out: the last Apply.
    pub applied_run_id: Option<String>,
}

impl ExplicitBackfillRun {
    pub fn can_resume(&self) -> bool {
        self.mode == ExplicitBackfillMode::Preview
            && matches!(
                self.status,
                ExplicitBackfillStatus::Cancelled | ExplicitBackfillStatus::Interrupted
            )
            && usize::try_from(self.cursor).is_ok_and(|cursor| cursor < self.queue.len())
    }

    /// A finished preview with something worth writing.
    pub fn can_apply(&self) -> bool {
        self.mode == ExplicitBackfillMode::Preview
            && self.status == ExplicitBackfillStatus::Completed
            && self.rows.iter().any(|row| !row.written && writes(&row.outcome))
    }
}

impl Serialize for ExplicitBackfillRun {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Utc<'a>(&'a Option<DateTime<chrono::Utc>>);
        impl Serialize for Utc<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                datetime::utc_option::serialize(self.0, s)
            }
        }
        let mut s = serializer.serialize_struct("ExplicitBackfillRun", 26)?;
        s.serialize_field("RunId", &self.run_id)?;
        s.serialize_field("Status", &self.status)?;
        s.serialize_field("Mode", &self.mode)?;
        s.serialize_field("StartedUtc", &Utc(&self.started_utc))?;
        s.serialize_field("FinishedUtc", &Utc(&self.finished_utc))?;
        s.serialize_field("Total", &self.total)?;
        s.serialize_field("Processed", &self.processed)?;
        s.serialize_field("Explicit", &self.explicit)?;
        s.serialize_field("Clean", &self.clean)?;
        s.serialize_field("NotExplicit", &self.not_explicit)?;
        s.serialize_field("Unsure", &self.unsure)?;
        s.serialize_field("AlreadyMarked", &self.already_marked)?;
        s.serialize_field("Failed", &self.failed)?;
        s.serialize_field("StepTotal", &self.step_total)?;
        s.serialize_field("StepDone", &self.step_done)?;
        s.serialize_field("Written", &self.written)?;
        s.serialize_field("LeftAlone", &self.left_alone)?;
        s.serialize_field("LastSong", &self.last_song)?;
        s.serialize_field("Reason", &self.reason)?;
        s.serialize_field("Errors", &self.errors)?;
        s.serialize_field("Rows", &self.rows)?;
        s.serialize_field("Queue", &self.queue)?;
        s.serialize_field("Cursor", &self.cursor)?;
        s.serialize_field("AppliedRunId", &self.applied_run_id)?;
        s.serialize_field("CanResume", &self.can_resume())?;
        s.serialize_field("CanApply", &self.can_apply())?;
        s.end()
    }
}

/// At most this many errors are kept, the newest.
pub const MAX_ERRORS: usize = 20;

/// At most this many rows are kept, the first.
pub const MAX_ROWS: usize = 50_000;

/// The run as the store starts from it: None when there is no file or it cannot be read (with a
/// warning), so the store starts Idle. A run that was going is never resumed by itself: the
/// restart may have been how it was stopped.
pub fn load(path: &Path) -> Option<ExplicitBackfillRun> {
    let read = || -> anyhow::Result<Option<ExplicitBackfillRun>> {
        let Some(text) = state_file::read_text(path)? else {
            return Ok(None);
        };
        Ok(serde_json::from_str::<Option<ExplicitBackfillRun>>(&text)?)
    };
    let mut run = read().unwrap_or_else(|e| {
        warn!("explicit marking state could not be read: {e}");
        None
    })?;
    if run.status == ExplicitBackfillStatus::Running {
        run.status = ExplicitBackfillStatus::Interrupted;
        run.reason = Some("Octo restarted while this run was going.".to_string());
    }
    Some(run)
}

/// Writes the run through `<file>.tmp`. False (with a warning) when it could not.
pub fn save(path: &Path, run: &ExplicitBackfillRun) -> bool {
    match state_file::save_atomic(path, &octo_core::json::to_string(run)) {
        Ok(()) => true,
        Err(e) => {
            warn!("explicit marking state could not be written: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust-migration/fixtures/state/explicit-backfill.json"
    );

    /// The computed CanResume and CanApply are written too.
    #[test]
    fn the_fixture_round_trips_byte_for_byte() {
        let text = std::fs::read_to_string(FIXTURE).expect("the fixture is in the repo");
        let run: ExplicitBackfillRun = serde_json::from_str(&text).expect("the fixture reads");
        assert!(run.can_apply());
        assert!(!run.can_resume());
        assert!(text.contains(r#""CanResume":false,"CanApply":true"#));
        assert_eq!(octo_core::json::to_string(&run), text.trim_end_matches('\n'));
    }

    #[test]
    fn a_run_that_was_going_is_interrupted_on_load() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("explicit-backfill.json");
        assert_eq!(load(&path), None);
        let mut run: ExplicitBackfillRun =
            serde_json::from_str(&std::fs::read_to_string(FIXTURE).expect("the fixture")).expect("reads");
        run.status = ExplicitBackfillStatus::Running;
        run.cursor = 0;
        assert!(save(&path, &run));
        let loaded = load(&path).expect("a run");
        assert_eq!(loaded.status, ExplicitBackfillStatus::Interrupted);
        assert_eq!(
            loaded.reason.as_deref(),
            Some("Octo restarted while this run was going.")
        );
        assert!(loaded.can_resume(), "a preview with songs left can go on");
    }
}
