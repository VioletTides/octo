//! The trickle queue, and its file's round trip.

use std::sync::Arc;

use super::*;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/rust-migration/fixtures/state/imports-trickle.json"
);

pub(crate) fn song(key: &str) -> ImportTrack {
    ImportTrack {
        key: key.into(),
        title: format!("Song {key}"),
        artist: "Artist".into(),
        seconds: Some(200),
        ..Default::default()
    }
}

pub(crate) fn start() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-04T12:00:00Z")
        .expect("a date")
        .with_timezone(&Utc)
}

/// A clock the test moves along.
pub(crate) fn moving_clock() -> (Arc<Mutex<DateTime<Utc>>>, Clock) {
    let now = Arc::new(Mutex::new(start()));
    let read = now.clone();
    (now, Clock::new(move || *read.lock()))
}

#[test]
fn the_fixture_round_trips_byte_for_byte() {
    let text = std::fs::read_to_string(FIXTURE).expect("the fixture is in the repo");
    let file: TrickleFile = serde_json::from_str(&text).expect("the fixture reads");
    assert_eq!(file.paused, ["anh"]);
    assert_eq!(octo_core::json::to_string(&file), text.trim_end_matches('\n'));
}

/// A job running when Octo stopped is queued again on load.
#[test]
fn a_running_job_is_queued_again_on_load() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("imports-trickle.json");
    let mut file: TrickleFile =
        serde_json::from_str(&std::fs::read_to_string(FIXTURE).expect("the fixture")).expect("reads");
    file.jobs[0].state = import_track_states::DOWNLOADING.to_string();
    assert!(save(&path, &file));
    let loaded = load(&path);
    assert_eq!(loaded.jobs[0].state, import_track_states::QUEUED);
    assert_eq!(
        loaded.jobs[0].detail.as_deref(),
        Some("Octo restarted while this was running; it goes again.")
    );
    assert_eq!(loaded.jobs[1], file.jobs[1]);
}

#[test]
fn the_queue_writes_its_file_as_it_read_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("imports-trickle.json");
    std::fs::copy(FIXTURE, &path).expect("copied");
    let queue = TrickleQueue::new(Some(path.clone()));
    // A change that changes nothing still writes the file whole.
    queue.set_paused("someone", true);
    queue.set_paused("someone", false);
    let text = std::fs::read_to_string(FIXTURE).expect("the fixture");
    assert_eq!(
        std::fs::read_to_string(&path).expect("written"),
        text.trim_end_matches('\n')
    );
}

#[test]
fn a_song_is_queued_once_per_person_however_many_lists_hold_it() {
    let queue = TrickleQueue::default();
    assert_eq!(queue.add("alice", &[song("a"), song("b"), song("a")]), 2);
    assert_eq!(queue.add("alice", &[song("a")]), 0);
    assert_eq!(queue.add("bob", &[song("a")]), 1);
    assert_eq!(queue.snapshot(None).len(), 3);
}

#[test]
fn a_not_found_song_is_tried_again_after_two_weeks_a_skipped_one_is_not() {
    let (now, clock) = moving_clock();
    let queue = TrickleQueue::default();
    queue.set_clock(clock);
    queue.add("alice", &[song("a"), song("b")]);
    queue.update("alice", "a", |job| {
        job.state = import_track_states::NOT_FOUND.into()
    });
    assert_eq!(queue.skip("alice", &["b"]), 1);
    *now.lock() += TimeDelta::days(13);
    assert_eq!(queue.add("alice", &[song("a"), song("b")]), 0);
    *now.lock() += TimeDelta::days(2);
    assert_eq!(queue.add("alice", &[song("a"), song("b")]), 1);
    assert_eq!(
        queue.get("alice", "a").expect("a").state,
        import_track_states::QUEUED
    );
    assert_eq!(
        queue.get("alice", "b").expect("b").state,
        import_track_states::SKIPPED
    );
    assert_eq!(queue.retry("alice", &["b"]), 1);
}

#[test]
fn people_take_turns_and_a_paused_person_is_passed_over() {
    let (now, clock) = moving_clock();
    let queue = TrickleQueue::default();
    queue.set_clock(clock);
    queue.add("alice", &[song("a1"), song("a2"), song("a3")]);
    *now.lock() += TimeDelta::seconds(1);
    queue.add("bob", &[song("b1")]);

    let next = || {
        *now.lock() += TimeDelta::minutes(1);
        let job = queue.take_next().expect("a job");
        queue.update(&job.owner, &job.key, |j| {
            j.state = import_track_states::DONE.into()
        });
        job.key
    };
    assert_eq!(next(), "a1");
    assert_eq!(next(), "b1");
    assert_eq!(next(), "a2");

    queue.set_paused("alice", true);
    assert!(queue.take_next().is_none());
    queue.set_paused("alice", false);
    assert_eq!(next(), "a3");
}

#[test]
fn a_restart_keeps_the_queue_and_the_pause_and_a_running_song_goes_again() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("imports-trickle.json");
    let queue = TrickleQueue::new(Some(path.clone()));
    queue.add("alice", &[song("a"), song("b")]);
    queue.set_paused("bob", true);
    assert_eq!(
        queue.take_next().expect("a job").state,
        import_track_states::DOWNLOADING
    );

    let again = TrickleQueue::new(Some(path));
    assert!(
        again
            .snapshot(None)
            .iter()
            .all(|job| job.state == import_track_states::QUEUED)
    );
    assert!(again.is_paused("bob"));
    assert!(again.last_start_utc().is_some());
}

#[test]
fn canceling_takes_back_only_what_has_not_run() {
    let queue = TrickleQueue::default();
    queue.add("alice", &[song("a"), song("b"), song("c")]);
    queue.take_next();
    assert_eq!(queue.cancel_queued("alice", Some(&["b".to_string()])), 1);
    assert_eq!(queue.cancel_queued("alice", None), 1);
    assert_eq!(queue.snapshot(Some("alice")).len(), 1);
}
