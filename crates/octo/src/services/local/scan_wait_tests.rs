use std::collections::VecDeque;

use async_trait::async_trait;
use chrono::TimeZone;
use octo_core::models::domain::Song;
use octo_core::models::subsonic::ScanStatus;
use parking_lot::Mutex;

use super::*;
use crate::services::local::{LocalSongMapping, ParsedExternalId, ParsedSongId};

/// Answers the statuses it was given, one per request, then none.
struct Scripted(Mutex<VecDeque<Option<ScanStatus>>>);

impl Scripted {
    fn new(statuses: Vec<Option<ScanStatus>>) -> Self {
        Self(Mutex::new(statuses.into()))
    }
}

#[async_trait]
impl ILocalLibraryService for Scripted {
    async fn get_local_path_for_external_song(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    async fn register_downloaded_song(&self, _: &Song, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_local_id_for_external_song(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn parse_song_id(&self, _: &str) -> ParsedSongId {
        (false, None, None)
    }
    fn parse_external_id(&self, _: &str) -> ParsedExternalId {
        (false, None, None, None)
    }
    async fn get_mappings(&self) -> Vec<LocalSongMapping> {
        Vec::new()
    }
    async fn find_mapping_by_tags(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Option<LocalSongMapping> {
        None
    }
    async fn forget_mapping(&self, _: &str) -> anyhow::Result<Vec<LocalSongMapping>> {
        Ok(Vec::new())
    }
    async fn trigger_library_scan(&self, _: bool) -> bool {
        true
    }
    async fn get_scan_status(&self) -> Option<ScanStatus> {
        self.0.lock().pop_front().flatten()
    }
}

fn at(minute: u32, second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 9, 10, minute, second)
        .single()
        .expect("a time")
}

/// A finished scan that ran from `start` for `seconds`.
fn finished(start: DateTime<Utc>, seconds: i64) -> Option<ScanStatus> {
    Some(ScanStatus {
        scanning: false,
        last_scan: Some((start + chrono::TimeDelta::seconds(seconds)).to_rfc3339()),
        elapsed_time: Some(seconds * 1_000_000_000),
        ..ScanStatus::default()
    })
}

fn running() -> Option<ScanStatus> {
    Some(ScanStatus {
        scanning: true,
        last_scan: Some(at(0, 0).to_rfc3339()),
        ..ScanStatus::default()
    })
}

fn quick() -> ScanWaitTiming {
    ScanWaitTiming {
        poll: Duration::from_millis(1),
        timeout: Duration::from_secs(5),
    }
}

/// A scan that finished after the write but started before it does not count: it may have read
/// the file before the change.
#[tokio::test]
async fn a_scan_counts_only_when_it_started_after_the_write() {
    let write = at(10, 0);
    let library = Scripted::new(vec![finished(at(9, 0), 120), running(), finished(at(10, 5), 30)]);

    let seen = wait_for_scan(&library, write, quick(), &CancellationToken::new()).await;

    assert_eq!(seen, Ok(at(10, 35)));
}

#[tokio::test]
async fn a_scan_that_never_comes_times_out() {
    let library = Scripted::new((0..10_000).map(|_| finished(at(0, 0), 10)).collect());
    let timing = ScanWaitTiming {
        poll: Duration::from_millis(1),
        timeout: Duration::from_millis(30),
    };

    let seen = wait_for_scan(&library, at(10, 0), timing, &CancellationToken::new()).await;

    assert_eq!(seen, Err(ScanWaitError::TimedOut));
}

#[tokio::test]
async fn navidrome_not_answering_is_said_so() {
    let library = Scripted::new(Vec::new());

    let seen = wait_for_scan(&library, at(10, 0), quick(), &CancellationToken::new()).await;

    assert_eq!(seen, Err(ScanWaitError::NoStatus));
}

#[tokio::test]
async fn a_cancelled_wait_stops() {
    let library = Scripted::new((0..10_000).map(|_| running()).collect());
    let cancel = CancellationToken::new();
    cancel.cancel();

    assert_eq!(
        wait_for_scan(&library, at(10, 0), quick(), &cancel).await,
        Err(ScanWaitError::Cancelled)
    );
    assert!(!wait_while_scanning(&library, quick(), &cancel).await);
}

#[tokio::test]
async fn waiting_out_a_running_scan_needs_an_idle_answer() {
    let idle_later = Scripted::new(vec![running(), running(), finished(at(0, 0), 1)]);
    assert!(wait_while_scanning(&idle_later, quick(), &CancellationToken::new()).await);

    let silent = Scripted::new(Vec::new());
    assert!(!wait_while_scanning(&silent, quick(), &CancellationToken::new()).await);
}

#[test]
fn a_hold_remembers_a_trigger_and_the_last_release_reports_it() {
    let hold = ScanHold::new();
    assert!(!hold.defer());

    let first = hold.hold();
    let second = hold.hold();
    assert!(hold.defer());
    assert!(!first.release());
    assert!(hold.is_held());
    assert!(second.release());
    assert!(!hold.is_held());

    let quiet = hold.hold();
    assert!(!quiet.release());
}
