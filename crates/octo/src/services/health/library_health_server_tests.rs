//! Library health on the server: Navidrome's own song list read into what the checks need, the
//! words and plans the app has, and the dashboard's runs of fixes and lookups. The activity feed
//! is tested in `controllers::admin::activity`; the lookup test checks the run here.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use futures::FutureExt;
use indexmap::IndexMap;
use octo_core::common::Clock;
use octo_core::settings::{AppSettings, LibraryActionSettings, SettingsStore};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::Notify;

use super::*;
use crate::controllers::admin::activity::{self, HealthActivity};
use crate::controllers::admin::health::{check_name, parse_check};
use crate::services::health::health_fixes::{
    self as fixes, FixAnswer, FixOutcome, FixStep, TagChange, fix_actions,
};
use crate::services::health::health_words as words;
use crate::services::health::library_health::{self as health, AlbumDifference, AlbumDifferenceValues};
use crate::services::health::library_health::{
    BestReason, DuplicateBasis, HealthTag, SplitBasis, SplitReason,
};
use crate::services::health::{IHealthFields, ServerSongFields};
use crate::services::library::library_snapshot::ReadPage;
use crate::services::library::library_tag_edits::{TagValues, song_tag_fields};
use crate::services::library::quality_upgrade_worker::parse_songs;

// ---- reading Navidrome's song list ----------------------------------------------------------

/// NOKIA as LXC 111's Navidrome 0.64 lists it in /api/song (2026-10-05), trimmed.
const NOKIA: &str = r#"
    [{"id":"6KpragWkvfYGesMZAXBmAm","title":"NOKIA","album":"$ome $exy $ongs 4 U","artist":"Drake",
      "albumArtist":"PARTYNEXTDOOR","albumId":"08qrDbXNlx9NTxtgugKes1","hasCoverArt":true,"trackNumber":14,
      "discNumber":1,"year":2025,"size":35876224,"suffix":"flac","duration":241.92,"bitRate":994,"sampleRate":44100,
      "bitDepth":16,"genre":"R&B","genres":[{"id":"1","name":"R&B"},{"id":"2","name":"Hip Hop"}],
      "mbzRecordingID":"f14b5405-2d38-4693-adaf-38d27821943f","mbzReleaseGroupId":"786a089a-9266-4fd3-984a-1df9e0ca34b4",
      "tags":{"barcode":["808391285880"],"isrc":["USLD91772032"],"recordlabel":["OVO Sound"]},
      "participants":{"albumartist":[{"id":"a","name":"PARTYNEXTDOOR"}],"artist":[{"id":"b","name":"Drake"}]},
      "path":"PARTYNEXTDOOR/$ome $exy $ongs 4 U/14 - NOKIA.flac","missing":false,"createdAt":"2026-10-05T02:16:27.952323177Z"},
     {"id":"gone","title":"Gone","artist":"X","path":"x.flac","missing":true},
     {"id":"bare","title":"Nightcall","artist":"Kavinsky","albumId":"u","hasCoverArt":false,"year":0,"trackNumber":0,
      "createdAt":"2026-08-14T19:43:35.76268Z",
      "suffix":"m4a","bitDepth":0,"duration":257.4,"path":"Kavinsky/Nightcall.m4a"}]
    "#;

fn rows() -> Vec<LibrarySongRow> {
    let root: Value = serde_json::from_str(NOKIA).expect("JSON");
    parse_songs(&root).expect("a list").expect("rows").0
}

#[test]
fn a_song_row_carries_what_the_checks_read() {
    let rows = rows();
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["6KpragWkvfYGesMZAXBmAm", "bare"]);
    let nokia = &rows[0];
    assert_eq!(nokia.seconds, Some(241.92));
    assert_eq!(nokia.duration, Some(242));
    assert_eq!(nokia.genres, ["R&B", "Hip Hop"]);
    assert_eq!(
        (
            nokia.year,
            nokia.track,
            nokia.disc,
            nokia.bit_depth,
            nokia.sample_rate
        ),
        (Some(2025), Some(14), Some(1), Some(16), Some(44100))
    );
    assert!(nokia.has_cover);
    assert_eq!(
        nokia.recording_id.as_deref(),
        Some("f14b5405-2d38-4693-adaf-38d27821943f")
    );
    assert_eq!(
        nokia.release_group_id.as_deref(),
        Some("786a089a-9266-4fd3-984a-1df9e0ca34b4")
    );
    assert_eq!(nokia.barcode.as_deref(), Some("808391285880"));
    assert_eq!(nokia.labels, ["OVO Sound"]);
    assert_eq!(nokia.artists, ["Drake"]);
    assert_eq!(nokia.album_artists, ["PARTYNEXTDOOR"]);
    assert_eq!(nokia.isrcs, ["USLD91772032"]);
    assert!(!rows[1].has_cover);
}

/// Navidrome's search3, which the apps read, lists songs as they were added; the checks take them
/// in that order, so a reason names the same song the app's does.
#[test]
fn the_songs_are_checked_in_the_order_the_apps_read_them() {
    let rows: Vec<Row> = rows().into_iter().map(Arc::new).collect();
    let added = Utc.with_ymd_and_hms(2026, 10, 5, 2, 16, 27).unwrap() + TimeDelta::nanoseconds(952_323_200);
    assert_eq!(rows[0].added_utc, Some(added));
    let ordered: Vec<String> = in_search_order(&rows).iter().map(|row| row.id.clone()).collect();
    assert_eq!(ordered, ["bare", "6KpragWkvfYGesMZAXBmAm"]);
}

#[test]
fn the_servers_reader_reads_the_songs_as_the_app_does_through_subsonic() {
    let rows = rows();
    let fields = ServerSongFields::new(&rows);
    // Subsonic cuts the fraction off: 241.92 s is 241, not the rounded 242.
    assert_eq!(fields.seconds(&rows[0]), 241);
    assert!(fields.lossless(&rows[0]));
    // An m4a with no bit depth is AAC.
    assert!(!fields.lossless(&rows[1]));
    assert_eq!(words::quality_text(&rows[0], &fields), "FLAC, 16-bit, 44.1 kHz");
    assert_eq!(fields.cover(&rows[1]), None);
    assert_eq!(
        fields.release_group_id(&rows[0]),
        Some("786a089a-9266-4fd3-984a-1df9e0ca34b4")
    );
    assert_eq!(fields.labels(&rows[0]), ["OVO Sound"]);
    let report = health::check(&rows, &fields);
    let ids =
        |tag: HealthTag| -> Vec<String> { report.missing[&tag].iter().map(|song| song.id.clone()).collect() };
    assert_eq!(ids(HealthTag::Cover), ["bare"]);
    assert_eq!(ids(HealthTag::Year), ["bare"]);
}

#[test]
fn an_albums_release_is_the_one_most_of_its_songs_carry() {
    let row = |id: &str, group: Option<&str>| LibrarySongRow {
        id: id.into(),
        path: format!("{id}.flac"),
        size: 1,
        suffix: "flac".into(),
        bit_rate: 900,
        title: id.into(),
        artist: "A".into(),
        duration: Some(200),
        album: Some("Album".into()),
        album_id: Some("al".into()),
        album_artist: Some("A".into()),
        release_group_id: group.map(str::to_string),
        ..Default::default()
    };
    let rows = [
        row("1", Some("g2")),
        row("2", Some("g1")),
        row("3", Some("g1")),
        row("4", None),
    ];
    let fields = ServerSongFields::new(&rows);
    for row in &rows {
        assert_eq!(fields.release_group_id(row), Some("g1"), "{}", row.id);
    }
}

// ---- words and plans ---------------------------------------------------------------------

#[test]
fn counts_and_quality_read_naturally() {
    assert_eq!(words::count_label(HealthCheck::NoGenre, 1), "1 song");
    assert_eq!(words::count_label(HealthCheck::NoGenre, 2256), "2,256 songs");
    assert_eq!(
        words::count_label(HealthCheck::Duplicates, 64),
        "64 songs with more than one copy"
    );
    assert_eq!(
        words::quality_text_of(Some("flac"), true, Some(16), Some(44_100), Some(900)),
        "FLAC, 16-bit, 44.1 kHz"
    );
    assert_eq!(
        words::quality_text_of(Some("mp3"), false, None, Some(44_100), Some(320)),
        "MP3, 320 kbps"
    );
    assert_eq!(
        words::quality_text_of(None, false, None, None, None),
        "unknown quality"
    );
}

fn lookup_of(
    state: &str,
    current: &[(&str, Option<&str>)],
    suggested: &[(&str, Option<&str>)],
    confidence: Option<&str>,
    source: Option<&str>,
    release: Option<&str>,
) -> SongLookup {
    let values = |pairs: &[(&str, Option<&str>)]| -> TagValues {
        pairs
            .iter()
            .map(|(tag, value)| (tag.to_string(), value.map(str::to_string)))
            .collect()
    };
    SongLookup {
        state: state.to_string(),
        detail: None,
        current: values(current),
        suggested: values(suggested),
        confidence: confidence.map(str::to_string),
        source: source.map(str::to_string),
        release: release.map(str::to_string),
    }
}

#[test]
fn no_words_use_a_dash() {
    let lookup = lookup_of(
        "found",
        &[],
        &[],
        Some("Medium"),
        Some("Database"),
        Some("'Mezzanine' 1998"),
    );
    let mut lines: Vec<String> = Vec::new();
    for check in HealthCheck::ALL {
        lines.push(words::title(check).to_string());
        lines.push(words::meaning(check).to_string());
        lines.push(words::advice(check).to_string());
        lines.push(words::fix_all_label(check, 2));
        lines.push(words::fix_meaning(check).to_string());
    }
    for kind in AlbumDifference::ALL {
        lines.push(words::difference_words(&AlbumDifferenceValues {
            kind,
            values: vec!["a".into(), "b".into()],
        }));
    }
    lines.extend(DuplicateBasis::ALL.map(|basis| words::basis_words(basis).to_string()));
    lines.extend(BestReason::ALL.map(|reason| words::best_words(reason, "FLAC")));
    for basis in SplitBasis::ALL {
        lines.push(words::reason_words(&SplitReason {
            basis,
            artist: "A".into(),
            other: "B".into(),
            song: "S".into(),
            value: "V".into(),
        }));
    }
    lines.extend(song_tag_fields::ALL.map(fixes::tag_name));
    lines.push(words::ALL_CLEAR.to_string());
    lines.push(TagChange::new("year", Some("1997"), "1998").words());
    lines.push(fixes::origin(&lookup));
    for line in lines {
        assert!(
            !(line.contains('\u{2014}') || line.contains('\u{2013}') || line.contains(" - ")),
            "{line}"
        );
    }
}

#[test]
fn a_lookup_picks_blanks_and_overwrites_only_when_sure_never_the_songs_name() {
    let mut lookup = lookup_of(
        "found",
        &[
            ("title", Some("teardrop")),
            ("year", None),
            ("genre", Some("Electronic")),
            ("track", Some("3")),
        ],
        &[
            ("title", Some("Teardrop")),
            ("year", Some("1998")),
            ("genre", Some("Trip Hop")),
            ("track", Some("3")),
        ],
        Some("Strong"),
        Some("Fingerprint"),
        None,
    );
    let picks = |lookup: &SongLookup| -> IndexMap<String, bool> {
        fixes::changes(lookup)
            .into_iter()
            .map(|(change, pick)| (change.tag, pick))
            .collect()
    };
    let want = |pairs: [(&str, bool); 3]| -> IndexMap<String, bool> {
        pairs
            .into_iter()
            .map(|(tag, pick)| (tag.to_string(), pick))
            .collect()
    };
    assert_eq!(
        picks(&lookup),
        want([("title", false), ("year", true), ("genre", true)])
    );
    lookup.confidence = Some("Low".into());
    assert_eq!(
        picks(&lookup),
        want([("title", false), ("year", true), ("genre", false)])
    );
    lookup.confidence = Some("Strong".into());
    assert_eq!(
        fixes::origin(&lookup),
        "A sure match from the song's fingerprint."
    );
}

#[test]
fn a_run_says_what_was_done_what_was_not_and_how_to_put_it_back() {
    let retag = FixStep::retag_with("flac", "Teardrop", &[TagChange::new("year", None, "1998")]);
    let remove = FixStep::new(fix_actions::REMOVE, "mp3", "Teardrop");
    let outcome = FixOutcome {
        done: vec![retag, remove.clone()],
        unchanged: vec![FixStep::new(fix_actions::COVER, "c", "Angel")],
        failed: vec![(
            FixStep::new(fix_actions::REMOVE, "ogg", "Teardrop"),
            "Could not work out which file this is, so nothing was touched.".to_string(),
        )],
        rehearsed: false,
        stopped: false,
        notes: Vec::new(),
    };

    assert_eq!(outcome.removed(), ["mp3".to_string()].into_iter().collect());
    assert_eq!(
        outcome.undo_steps(),
        [
            FixStep::new(fix_actions::RESTORE, "mp3", "Teardrop"),
            FixStep::new(fix_actions::UNDO, "flac", "Teardrop")
        ]
    );
    assert_eq!(
        outcome.summary(),
        "Removed 1 song, changed the tags of 1 song. Teardrop: Could not work out which file this is, so nothing was touched."
    );
    let rehearsed = FixOutcome {
        rehearsed: true,
        ..FixOutcome::default()
    };
    assert!(rehearsed.summary().contains("dry run"));
    let nothing = FixOutcome {
        unchanged: vec![remove],
        ..FixOutcome::default()
    };
    assert_eq!(nothing.summary(), "Nothing needed changing.");
}

#[test]
fn the_duplicate_plan_removes_copies_as_copies_after_filling_the_kept_one() {
    let row = |id: &str, suffix: &str, bit_rate: i32| LibrarySongRow {
        id: id.into(),
        path: format!("a.{suffix}"),
        size: 1,
        suffix: suffix.into(),
        bit_rate,
        title: "Teardrop".into(),
        artist: "Massive Attack".into(),
        duration: Some(330),
        album: Some("Mezzanine".into()),
        album_id: Some("al".into()),
        album_artist: Some("Massive Attack".into()),
        ..Default::default()
    };
    let rows = [
        LibrarySongRow {
            year: Some(1998),
            seconds: Some(330.0),
            ..row("mp3", "mp3", 320)
        },
        LibrarySongRow {
            bit_depth: Some(16),
            sample_rate: Some(44100),
            seconds: Some(330.4),
            ..row("flac", "flac", 900)
        },
    ];
    let fields = ServerSongFields::new(&rows);
    let sets = health::find_duplicates(&rows, &fields);
    assert_eq!(sets.len(), 1);
    let steps = fixes::duplicate_steps(&fixes::duplicate_fix(&sets[0], &fields, None), &fields, None);

    assert_eq!(
        steps[0],
        FixStep::with(
            fix_actions::RETAG,
            "flac",
            "Teardrop",
            IndexMap::from([("year".to_string(), "1998".to_string())])
        )
    );
    assert_eq!(steps[1], FixStep::remove_copy("mp3", "Teardrop"));
    assert_eq!(steps[1].param("copy"), Some("true"));
}

#[test]
fn check_names_go_both_ways() {
    for (name, check) in [
        ("duplicates", HealthCheck::Duplicates),
        ("splitAlbums", HealthCheck::SplitAlbums),
        ("noTrackNumber", HealthCheck::NoTrackNumber),
        ("NOGENRE", HealthCheck::NoGenre),
    ] {
        assert_eq!(parse_check(Some(name)), Some(check), "{name}");
        assert!(check_name(check).eq_ignore_ascii_case(name), "{name}");
    }
}

#[test]
fn only_known_changes_are_sent() {
    assert_eq!(
        LibraryHealthService::invalid(&[FixStep::remove_copy("a", "A"), FixStep::join("b", "B", "a")]),
        None
    );
    assert!(LibraryHealthService::invalid(&[]).is_some());
    assert!(LibraryHealthService::invalid(&[FixStep::new("delete-everything", "a", "A")]).is_some());
    assert!(
        LibraryHealthService::invalid(&[FixStep::with(
            fix_actions::RETAG,
            "a",
            "A",
            IndexMap::from([("path".to_string(), "/etc".to_string())])
        )])
        .is_some()
    );
    assert!(LibraryHealthService::invalid(&[FixStep::new(fix_actions::JOIN_ALBUM, "a", "A")]).is_some());
    let many: Vec<FixStep> = (0..=LibraryHealthService::MAX_STEPS)
        .map(|i| FixStep::remove_copy(&i.to_string(), "x"))
        .collect();
    assert!(LibraryHealthService::invalid(&many).is_some());
}

// ---- the scan after a run ------------------------------------------------------------------

#[test]
fn a_held_rescan_asks_once_at_the_end() {
    let rescan = Arc::new(LibraryRescan::new(None));
    {
        let _hold = rescan.hold();
        rescan.soon();
        rescan.soon();
        rescan.soon();
        assert_eq!(rescan.scheduled(), 0);
    }
    assert_eq!(rescan.asked(), 3);
    assert_eq!(rescan.scheduled(), 1);

    drop(rescan.hold());
    assert_eq!(rescan.scheduled(), 1);
    rescan.soon();
    assert_eq!(rescan.scheduled(), 2);
}

// ---- the service ---------------------------------------------------------------------------

fn song(
    id: &str,
    title: &str,
    album: &str,
    album_id: &str,
    year: Option<i32>,
    suffix: &str,
) -> LibrarySongRow {
    let flac = suffix == "flac";
    LibrarySongRow {
        id: id.into(),
        path: format!("{id}.{suffix}"),
        size: 1,
        suffix: suffix.into(),
        bit_rate: if flac { 900 } else { 320 },
        title: title.into(),
        artist: "Artist".into(),
        duration: Some(200),
        album: Some(album.into()),
        album_id: Some(album_id.into()),
        album_artist: Some("Artist".into()),
        seconds: Some(200.0),
        year,
        genres: vec!["Rock".into()],
        track: Some(1),
        bit_depth: Some(if flac { 16 } else { 0 }),
        sample_rate: Some(44100),
        ..Default::default()
    }
}

struct Library {
    rows: Mutex<Vec<LibrarySongRow>>,
    reads: AtomicUsize,
    now: Mutex<DateTime<Utc>>,
}

impl Library {
    fn set(&self, rows: Vec<LibrarySongRow>) {
        *self.rows.lock() = rows;
    }

    fn advance(&self, by: TimeDelta) {
        *self.now.lock() += by;
    }
}

fn service(settings: LibraryActionSettings) -> (Arc<LibraryHealthService>, Arc<LibraryRescan>, Arc<Library>) {
    let library = Arc::new(Library {
        rows: Mutex::new(Vec::new()),
        reads: AtomicUsize::new(0),
        now: Mutex::new(Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap()),
    });
    let pages = Arc::clone(&library);
    let read_page: ReadPage = Arc::new(move |start, count| {
        let library = Arc::clone(&pages);
        async move {
            if start == 0 {
                library.reads.fetch_add(1, Ordering::SeqCst);
            }
            let rows = library.rows.lock().clone();
            let start = start as usize;
            let page: Vec<LibrarySongRow> = rows.iter().skip(start).take(count as usize).cloned().collect();
            let held = rows.len().saturating_sub(start).min(count as usize);
            Some((page, held))
        }
        .boxed()
    });
    let clocked = Arc::clone(&library);
    let clock = Clock::new(move || *clocked.now.lock());
    let snapshot = Arc::new(LibrarySnapshot::with(read_page, clock.clone()));
    let rescan = Arc::new(LibraryRescan::new(None));
    let store = Arc::new(SettingsStore::for_tests(AppSettings {
        library_actions: settings,
        ..Default::default()
    }));
    let health = Arc::new(LibraryHealthService::new(
        snapshot,
        Arc::clone(&rescan),
        store,
        HealthServices::default(),
        clock,
    ));
    (health, rescan, library)
}

async fn finished(health: &LibraryHealthService) -> Arc<HealthRun> {
    for _ in 0..500 {
        if !health.run().is_some_and(|run| run.is_running()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    health.run().expect("a run")
}

fn ids_of(songs: &[Row]) -> Vec<String> {
    songs.iter().map(|song| song.id.clone()).collect()
}

#[tokio::test]
async fn the_report_is_the_apps_checks_over_the_library() {
    let (health, _, library) = service(LibraryActionSettings::default());
    library.set(vec![
        song("a", "Song", "Album", "x", Some(2020), "flac"),
        song("b", "Song", "Album", "x", Some(2020), "mp3"),
        song("c", "Other", "Album", "x", None, "flac"),
    ]);

    let found = health.report(false).await.expect("a report");

    assert_eq!(found.report.checked, 3);
    assert_eq!(
        found.report.findings(),
        [HealthCheck::Duplicates, HealthCheck::NoYear]
    );
    assert_eq!(ids_of(&found.report.duplicates[0].copies), ["a", "b"]);
}

#[tokio::test]
async fn a_run_sends_each_step_as_the_admin_asks_for_one_scan_and_leaves_the_fixed_songs_out() {
    let (health, rescan, library) = service(LibraryActionSettings::default());
    library.set(vec![
        song("a", "One", "Album", "x", Some(2020), "flac"),
        song("b", "Two", "Album", "x", None, "flac"),
        song("c", "Three", "Album", "x", None, "flac"),
    ]);
    let sent: Arc<Mutex<Vec<(FixStep, String)>>> = Arc::default();
    {
        let sent = Arc::clone(&sent);
        let rescan = Arc::clone(&rescan);
        health.configure(move |seams| {
            seams.send = Some(Arc::new(move |step: FixStep, user: String| {
                let answer = if step.id == "c" {
                    FixAnswer::new("failed", Some("The file has changed since."))
                } else {
                    FixAnswer::new("applied", Some("Changed the year."))
                };
                sent.lock().push((step, user));
                rescan.soon();
                async move { Ok(answer) }.boxed()
            }));
        });
    }
    assert_eq!(
        health
            .report(false)
            .await
            .expect("a report")
            .report
            .count(HealthCheck::NoYear),
        2
    );

    let steps: Vec<FixStep> = ["b", "c"]
        .iter()
        .map(|id| FixStep::retag_with(id, id, &[TagChange::new("year", None, "2020")]))
        .collect();
    assert!(
        health
            .start_fix("Filling in tags", Some(HealthCheck::NoYear), steps, "winters")
            .is_none()
    );
    let run = finished(&health).await;
    let progress = run.progress();

    assert_eq!(progress.state, "done");
    assert!(sent.lock().iter().all(|(_, user)| user == "winters"));
    assert_eq!(progress.done, 2);
    assert_eq!(
        progress.outcome.expect("an outcome").summary(),
        "Changed the tags of 1 song. c: The file has changed since."
    );
    assert_eq!(rescan.scheduled(), 1);
    assert_eq!(health.undo_count(), 1);
    // The read is from before the fix, so b is left out until a read from after Navidrome's scan.
    let report = health.report(false).await.expect("a report").report;
    assert_eq!(ids_of(&report.songs(HealthCheck::NoYear)), ["c"]);
    library.advance(LibraryHealthService::SETTLE_FOR + TimeDelta::seconds(1));
    // By title within the album: Three, then Two.
    let report = health.report(true).await.expect("a report").report;
    assert_eq!(ids_of(&report.songs(HealthCheck::NoYear)), ["c", "b"]);
}

#[tokio::test]
async fn one_run_at_a_time_and_stop_ends_it_between_songs() {
    let (health, _, _) = service(LibraryActionSettings::default());
    let gate = Arc::new(Notify::new());
    let sent = Arc::new(AtomicUsize::new(0));
    {
        let (gate, sent) = (Arc::clone(&gate), Arc::clone(&sent));
        health.configure(move |seams| {
            seams.send = Some(Arc::new(move |_, _| {
                sent.fetch_add(1, Ordering::SeqCst);
                let gate = Arc::clone(&gate);
                async move {
                    gate.notified().await;
                    Ok(FixAnswer::new("applied", None))
                }
                .boxed()
            }));
        });
    }
    let steps: Vec<FixStep> = (0..5)
        .map(|i| FixStep::remove_copy(&i.to_string(), &format!("Song {i}")))
        .collect();

    assert!(
        health
            .start_fix(
                "Fixing copies",
                Some(HealthCheck::Duplicates),
                steps.clone(),
                "winters"
            )
            .is_none()
    );
    let busy = health
        .start_fix("Joining albums", Some(HealthCheck::SplitAlbums), steps, "winters")
        .expect("busy");
    assert_eq!(busy.label, "Fixing copies");
    assert!(health.start_undo("winters").is_some());

    // Stop while the first song is being sent.
    for _ in 0..500 {
        if sent.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(health.stop());
    gate.notify_one();
    let run = finished(&health).await;
    let progress = run.progress();
    assert_eq!(progress.state, "stopped");
    assert_eq!(sent.load(Ordering::SeqCst), 1);
    assert!(
        progress
            .outcome
            .expect("an outcome")
            .summary()
            .contains("Stopped before the end.")
    );
    assert_eq!(health.undo_count(), 1);
}

#[tokio::test]
async fn undo_puts_back_what_the_last_run_did_the_last_first() {
    let (health, _, _) = service(LibraryActionSettings::default());
    let sent: Arc<Mutex<Vec<FixStep>>> = Arc::default();
    {
        let sent = Arc::clone(&sent);
        health.configure(move |seams| {
            seams.send = Some(Arc::new(move |step, _| {
                sent.lock().push(step);
                async { Ok(FixAnswer::new("applied", None)) }.boxed()
            }));
        });
    }
    let steps = vec![
        FixStep::retag_with("keep", "Keep", &[TagChange::new("year", None, "1998")]),
        FixStep::remove_copy("copy", "Copy"),
    ];
    assert!(
        health
            .start_fix("Fixing copies", Some(HealthCheck::Duplicates), steps, "winters")
            .is_none()
    );
    finished(&health).await;
    sent.lock().clear();

    assert!(health.start_undo("winters").is_none());
    let run = finished(&health).await;

    assert_eq!(run.kind, "undo");
    assert_eq!(
        *sent.lock(),
        [
            FixStep::new(fix_actions::RESTORE, "copy", "Copy"),
            FixStep::new(fix_actions::UNDO, "keep", "Keep")
        ]
    );
    assert_eq!(
        run.progress().outcome.expect("an outcome").summary(),
        "Put back 1 song, undid the changes to 1 song."
    );
    assert_eq!(health.undo_count(), 0);
    assert_eq!(
        health.start_undo("winters").as_deref(),
        Some("There is nothing to undo.")
    );
}

#[tokio::test]
async fn a_lookup_says_what_it_found_and_what_to_pick() {
    let (health, _, _) = service(LibraryActionSettings::default());
    health.configure(|seams| {
        seams.look = Some(Arc::new(|id: String| {
            let lookup = if id == "miss" {
                SongLookup::said("failed", "Nothing matched.")
            } else {
                lookup_of(
                    "found",
                    &[("track", None)],
                    &[("track", Some("3")), ("year", Some("1998"))],
                    Some("Strong"),
                    Some("Fingerprint"),
                    Some("Mezzanine"),
                )
            };
            async move { Ok(lookup) }.boxed()
        }));
    });

    let songs = vec![
        ("hit".to_string(), "Teardrop".to_string()),
        ("miss".to_string(), "Angel".to_string()),
    ];
    assert!(
        health
            .start_lookup(songs, Some(HealthCheck::NoTrackNumber), "winters")
            .is_none()
    );
    let run = finished(&health).await;

    assert_eq!(run.kind, "lookup");
    let lookups = run.lookups_now();
    assert_eq!(
        lookups[0].origin.as_deref(),
        Some("A sure match from the song's fingerprint, on Mezzanine.")
    );
    let changes: Vec<(String, String, bool)> = lookups[0]
        .changes
        .iter()
        .map(|(change, pick)| (change.tag.clone(), change.value.clone(), *pick))
        .collect();
    assert_eq!(
        changes,
        [
            ("year".to_string(), "1998".to_string(), true),
            ("track".to_string(), "3".to_string(), true)
        ]
    );
    assert_eq!(lookups[1].detail.as_deref(), Some("Nothing matched."));
    let finished = run.progress().finished_utc.expect("finished");
    let items = activity::health(Some(&HealthActivity::from(&*run)), None, finished);
    assert_eq!(items.len(), 1);
    assert_eq!(
        (items[0].page, items[0].state, items[0].detail.as_deref()),
        ("health", "done", Some("Found tags for 1 of 2 songs"))
    );
}

/// `TheActivityFeed_ShowsACheckAndARunGoing`, over a real run: a check of the library and a run
/// of fixes on the activity card while they go.
#[tokio::test]
async fn the_activity_feed_shows_a_check_and_a_run_going() {
    let (health, _, _) = service(LibraryActionSettings::default());
    let gate = Arc::new(Notify::new());
    {
        let gate = Arc::clone(&gate);
        health.configure(move |seams| {
            seams.send = Some(Arc::new(move |_, _| {
                let gate = Arc::clone(&gate);
                async move {
                    gate.notified().await;
                    Ok(FixAnswer::new("applied", None))
                }
                .boxed()
            }));
        });
    }
    let steps: Vec<FixStep> = (0..20)
        .map(|i| FixStep::remove_copy(&i.to_string(), "NOKIA"))
        .collect();
    assert!(
        health
            .start_fix("Joining albums", Some(HealthCheck::SplitAlbums), steps, "winters")
            .is_none()
    );
    for _ in 0..500 {
        if health.run().is_some_and(|run| run.progress().current.is_some()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let run = health.run().expect("a run");
    let now = Utc::now();
    let items = activity::health(Some(&HealthActivity::from(&*run)), Some(now), now);

    let titles: Vec<&str> = items.iter().map(|item| item.title.as_str()).collect();
    assert_eq!(titles, ["Checking your library", "Joining albums"]);
    assert_eq!(
        (
            items[1].done,
            items[1].total,
            items[1].unit,
            items[1].detail.as_deref()
        ),
        (Some(0), Some(20), Some("changes"), Some("NOKIA"))
    );
    assert!(activity::health(None, None, now).is_empty());
    assert!(health.stop());
    gate.notify_one();
}

#[tokio::test]
async fn what_the_page_may_do_follows_the_library_action_settings() {
    let off = service(LibraryActionSettings::default()).0.abilities("winters");
    assert!(!off.edit);
    assert!(
        off.why
            .as_deref()
            .unwrap_or("")
            .contains("Library actions are off")
    );

    let rehearsing = service(LibraryActionSettings {
        enabled: true,
        allowed_users: vec!["winters".into()],
        ..Default::default()
    })
    .0
    .abilities("winters");
    assert!(rehearsing.dry_run);
    assert!(!rehearsing.edit);
    assert!(rehearsing.why.as_deref().unwrap_or("").contains("rehearsal"));

    let stranger = service(LibraryActionSettings {
        enabled: true,
        dry_run: false,
        allowed_users: vec!["winters".into()],
        ..Default::default()
    })
    .0
    .abilities("bob");
    assert!(
        stranger
            .why
            .as_deref()
            .unwrap_or("")
            .contains("bob is not on the library actions allowed list")
    );
}

// ---- Song order ---------------------------------------------------------------------------------

/// A read that was answered from the kept report gives the songs in the order the library was
/// read in (the index's rows), while a fresh check gives them in search order.
#[tokio::test]
async fn a_kept_report_lists_the_songs_as_the_library_read_them() {
    let (health, _, library) = service(LibraryActionSettings::default());
    let mut late = song("a", "One", "Album", "x", Some(2020), "flac");
    late.added_utc = Some(Utc.with_ymd_and_hms(2026, 10, 2, 0, 0, 0).unwrap());
    let mut early = song("b", "Two", "Album", "x", Some(2020), "flac");
    early.added_utc = Some(Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap());
    library.set(vec![late, early]);

    let first = health.report(false).await.expect("a report");
    assert_eq!(ids_of(&first.songs), ["b", "a"]);
    let kept = health.report(false).await.expect("a report");
    assert_eq!(ids_of(&kept.songs), ["a", "b"]);
    assert_eq!(library.reads.load(Ordering::SeqCst), 1);
}

// ---- Family ----------------------------------------------------------------------------------

/// Library health over these rows, with Family on or off.
async fn checked_with_family(
    family_on: bool,
    members: &[(&str, i64)],
    rows: Vec<LibrarySongRow>,
) -> HealthSnapshot {
    let read_page: ReadPage = Arc::new(move |start, _| {
        let rows = rows.clone();
        async move {
            let page = if start == 0 { rows.clone() } else { Vec::new() };
            let held = page.len();
            Some((page, held))
        }
        .boxed()
    });
    let clock = Clock::fixed(Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap());
    let snapshot = Arc::new(LibrarySnapshot::with(read_page, clock.clone()));
    let health = Arc::new(LibraryHealthService::new(
        snapshot,
        Arc::new(LibraryRescan::new(None)),
        Arc::new(SettingsStore::for_tests(AppSettings::default())),
        HealthServices {
            scope: Some(crate::services::family::library_index::scope_for_tests(
                family_on, members,
            )),
            ..Default::default()
        },
        clock,
    ));
    health.report(false).await.expect("a report")
}

/// A member's own copy of a shared song is never a duplicate of it: Library health keeps to the
/// shared library (Keep would remove the member's file), with Family on and after it was turned
/// off while members' libraries are still there. With no member library nothing changes.
#[tokio::test]
async fn health_never_sees_personal_libraries() {
    let mut shared = song("a", "Song", "Album", "x", Some(2020), "flac");
    shared.library_path = Some("/music".into());
    shared.path = "Artist/Album/a.flac".into();
    let mut alexs = song("b", "Song", "Album", "x", Some(2020), "mp3");
    alexs.library_path = Some("/members/alex".into());
    alexs.path = "Artist/Album/b.mp3".into();
    let rows = vec![shared, alexs];

    for family_on in [true, false] {
        let kept = checked_with_family(family_on, &[("alex", 3)], rows.clone()).await;
        assert_eq!(kept.report.checked, 1, "Family on: {family_on}");
        assert!(kept.report.duplicates.is_empty());
        assert_eq!(ids_of(&kept.songs), ["a"]);
    }

    let off = checked_with_family(false, &[], rows).await;
    assert_eq!(off.report.checked, 2);
    assert_eq!(ids_of(&off.report.duplicates[0].copies), ["a", "b"]);
}
