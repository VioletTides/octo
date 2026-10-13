//! The playlist worker's parsers, the wanted playlists, and a sweep over a stand-in Navidrome.
//!
//! Navidrome's native playlist rows are what the sweep reads. The shapes matter more than they
//! look: a playlist row's owner is the allowlist check, and a track row's id is a POSITION
//! rather than an identifier.

use std::path::Path;

use octo_core::settings::{AppSettings, SubsonicSettings};
use serde_json::json;
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::services::library::library_action_test_support::{Extras, executor, store};

#[test]
fn parse_playlists_reads_id_name_and_owner() {
    let rows = parse_playlists(&json!([
        {"id": "p1", "name": "🛠 Delete", "ownerName": "alice"},
        {"id": "p2", "name": "Road trip", "ownerName": "bob"}
    ]));

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "p1");
    assert_eq!(rows[0].owner, "alice");
}

#[test]
fn parse_playlists_rows_missing_id_or_name_are_skipped_not_fatal() {
    let rows = parse_playlists(&json!([{"id": "p1"}, {"name": "no id"}, {"id": "p2", "name": "ok"}]));

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "p2");
}

#[test]
fn parse_playlists_missing_owner_is_empty_rather_than_null() {
    assert_eq!(parse_playlists(&json!([{"id": "p1", "name": "x"}]))[0].owner, "");
}

/// PlaylistTrack.ID is the 1-based POSITION, reassigned on every mutation. Keeping it separate
/// from mediaFileId is what lets the sweep re-read before deleting and map the tracks it actually
/// applied onto their CURRENT positions.
#[test]
fn parse_tracks_keeps_the_position_and_the_media_file_id_apart() {
    let rows = parse_tracks(&json!([
        {"id": "1", "mediaFileId": "song-a"},
        {"id": "2", "mediaFileId": "song-b"}
    ]));

    assert_eq!(rows[0].position, "1");
    assert_eq!(rows[0].media_file_id, "song-a");
    assert_eq!(rows[1].position, "2");
}

/// Navidrome has sent the position as a number in places, so both shapes parse.
#[test]
fn parse_tracks_numeric_position_is_read() {
    assert_eq!(
        parse_tracks(&json!([{"id": 3, "mediaFileId": "song-c"}]))[0].position,
        "3"
    );
}

#[test]
fn parse_tracks_row_without_a_media_file_id_is_skipped() {
    assert!(parse_tracks(&json!([{"id": "1"}])).is_empty());
}

#[test]
fn parsers_non_array_payloads_are_empty_rather_than_throwing() {
    for raw in ["{}", "null", "\"not an array\""] {
        let root: Value = serde_json::from_str(raw).expect("JSON");
        assert!(parse_playlists(&root).is_empty(), "{raw}");
        assert!(parse_tracks(&root).is_empty(), "{raw}");
    }
}

/// A notice playlist named like an action playlist would have every track Octo asked about acted
/// on. Octo's own questions are never commands, whatever they are called.
#[test]
fn wanted_playlists_a_notice_playlist_named_like_an_action_is_never_a_command() {
    let settings = LibraryActionSettings {
        enabled: true,
        review_enabled: true,
        playlist_prefix: "* ".into(),
        notice_prefix: "* ".into(),
        review_playlist_name: "Delete".into(),
        actions: vec![LibraryActionDefinition {
            action: LibraryAction::Delete,
            enabled: true,
            rating: Some(1),
            ..Default::default()
        }],
        ..Default::default()
    };

    let wanted = LibraryActionPlaylistWorker::wanted_playlists(&settings);

    assert!(!wanted.iter().any(|(title, _)| title == "* Delete"));
    assert_eq!(
        find(&wanted, "* keep").map(|a| a.action),
        Some(LibraryAction::Keep)
    );
}

// ---- A sweep -----------------------------------------------------------------------------

struct Sweep {
    root: String,
    _dir: tempfile::TempDir,
    song: String,
    navidrome: MockServer,
    worker: Arc<LibraryActionPlaylistWorker>,
}

async fn sweep_fixture(enabled: bool) -> Sweep {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("music");
    std::fs::create_dir_all(root.join("Artist")).expect("folders");
    let song = root.join("Artist/Song.flac");
    std::fs::write(&song, [7u8; 64]).expect("written");

    let navidrome = MockServer::builder().start().await;
    let answer = |body: Value| ResponseTemplate::new(200).set_body_json(body);
    Mock::given(path("/auth/login"))
        .respond_with(answer(
            json!({"token": "jwt", "isAdmin": true, "username": "admin"}),
        ))
        .mount(&navidrome)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/playlist"))
        .respond_with(answer(json!([
            {"id": "p1", "name": "🛠 Delete", "ownerName": "alice"},
            {"id": "p2", "name": "🛠 delete", "ownerName": "mallory"},
            {"id": "p3", "name": "Road trip", "ownerName": "alice"}
        ])))
        .mount(&navidrome)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/playlist/p1/tracks"))
        .respond_with(answer(json!([{"id": "1", "mediaFileId": "nd-1"}])))
        .mount(&navidrome)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/playlist/p1/tracks"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&navidrome)
        .await;
    Mock::given(path("/api/song/nd-1"))
        .respond_with(answer(json!({
            "id": "nd-1", "path": "Artist/Song.flac", "size": 64, "title": "Song",
            "artist": "Artist", "album": "Album", "suffix": "flac", "duration": 100
        })))
        .mount(&navidrome)
        .await;
    Mock::given(any())
        .respond_with(answer(json!([])))
        .mount(&navidrome)
        .await;

    let settings = store(AppSettings {
        library_actions: LibraryActionSettings {
            enabled,
            dry_run: false,
            allowed_users: vec!["alice".into()],
            actions: vec![LibraryActionDefinition {
                action: LibraryAction::Delete,
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        subsonic: SubsonicSettings {
            url: Some(navidrome.uri()),
            admin_username: Some("admin".into()),
            admin_password: Some("secret".into()),
            auto_detect_download_path: false,
            ..Default::default()
        },
        ..Default::default()
    });
    settings.set_raw("Library:DownloadPath", Some(&root.to_string_lossy()));
    let journal = Arc::new(LibraryActionJournal::new());
    let executor = executor(
        &settings,
        Extras {
            journal: Some(journal.clone()),
            ..Default::default()
        },
    );
    let http = crate::services::http_client_factory::default_client();
    let identity = NavidromeIdentityService::new(settings.clone(), http.clone());
    let admin =
        crate::services::family::NavidromeAdmin::new(identity.clone(), http.clone(), settings.clone());
    let worker = Arc::new(
        LibraryActionPlaylistWorker::new(
            executor.clone(),
            journal,
            Arc::new(LibraryActionQuarantine::new(settings.clone())),
            Arc::new(NavidromeSongPathResolver::new(
                identity.clone(),
                Arc::new(crate::services::local::test_support::FakeLocalLibrary::default()),
                http.clone(),
                settings.clone(),
            )),
            identity.clone(),
            Arc::new(NavidromePlaylistApi::new(http, identity, settings.clone())),
            settings,
        )
        .with_missing_rows(Arc::new(MissingRows::new(Arc::new(admin)))),
    );
    Sweep {
        root: root.to_string_lossy().into_owned(),
        _dir: dir,
        song: song.to_string_lossy().into_owned(),
        navidrome,
        worker,
    }
}

/// A file the trash deleted for good leaves no row behind in Navidrome; a song trashed today,
/// which can still be put back, keeps its row.
#[tokio::test]
async fn a_sweep_removes_the_navidrome_rows_of_songs_the_trash_deleted_for_good() {
    let f = sweep_fixture(true).await;
    let old_day = (chrono::Utc::now() - chrono::TimeDelta::days(400)).format("%Y-%m-%d");
    let old = format!("{}/.octo-trash/{old_day}", f.root);
    std::fs::create_dir_all(&old).expect("old trash");
    std::fs::write(format!("{old}/Old - Song.flac"), [1u8; 8]).expect("written");
    std::fs::write(
        format!("{old}/Old - Song.flac.octo-action.json"),
        json!({"OriginalPath": format!("{}/Old - Song.flac", f.root), "NavidromeId": "nd-old",
               "Action": "Delete", "Username": "alice", "AtUtc": "2025-01-01T00:00:00Z"})
        .to_string(),
    )
    .expect("written");
    Mock::given(method("GET"))
        .and(path("/api/missing/nd-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "nd-old", "missing": true})))
        .with_priority(1)
        .mount(&f.navidrome)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/missing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ids": ["nd-old"]})))
        .with_priority(1)
        .mount(&f.navidrome)
        .await;

    f.worker.sweep(&CancellationToken::new()).await;

    assert!(!Path::new(&old).exists());
    let requests = f.navidrome.received_requests().await.expect("recorded");
    let removals: Vec<String> = requests
        .iter()
        .filter(|r| r.method == http::Method::DELETE && r.url.path() == "/api/missing")
        .map(|r| r.url.query().unwrap_or("").to_string())
        .collect();
    assert_eq!(removals, ["id=nd-old"]);
    // The song trashed in this sweep can still be put back, so its row is never asked about.
    assert!(!requests.iter().any(|r| r.url.path() == "/api/missing/nd-1"));
}

/// The owner's own playlist is applied, someone off the allowlist is ignored, and the track
/// whose action landed is cleared by its position.
#[tokio::test]
async fn a_sweep_applies_the_owners_playlist_and_clears_what_landed() {
    let f = sweep_fixture(true).await;

    f.worker.sweep(&CancellationToken::new()).await;

    assert!(!Path::new(&f.song).exists());
    let requests = f.navidrome.received_requests().await.expect("recorded");
    let deletes: Vec<String> = requests
        .iter()
        .filter(|r| r.method == http::Method::DELETE)
        .map(|r| format!("{}?{}", r.url.path(), r.url.query().unwrap_or("")))
        .collect();
    assert_eq!(deletes, ["/api/playlist/p1/tracks?id=1"]);
    assert!(
        !requests
            .iter()
            .any(|r| r.url.path().starts_with("/api/playlist/p2"))
    );
}

/// Off when the worker starts is off until a restart: it returns at once.
#[tokio::test]
async fn the_worker_is_idle_when_library_actions_are_off() {
    let f = sweep_fixture(false).await;

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.worker.clone().run(CancellationToken::new()),
    )
    .await
    .expect("returns at once")
    .expect("no error");

    assert!(Path::new(&f.song).exists());
}
