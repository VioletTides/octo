//! Tests of the Lidarr client. A mock server answers every request with 200 and a body chosen by
//! method and path, and keeps the requests.

use std::sync::Arc;

use octo_core::lidarr::LidarrAlbumCandidate;
use octo_core::settings::{AppSettings, LidarrSettings, SettingsStore};
use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::services::lidarr::lidarr_heart_acquisition_service::translate_imported_path;

/// Answers each request with `respond(method, path)`, always 200.
struct Handler(Box<dyn Fn(&str, &str) -> String + Send + Sync>);

impl Respond for Handler {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_string((self.0)(request.method.as_str(), request.url.path()))
    }
}

async fn serve(respond: impl Fn(&str, &str) -> String + Send + Sync + 'static) -> MockServer {
    let server = MockServer::builder().start().await;
    Mock::given(any())
        .respond_with(Handler(Box::new(respond)))
        .mount(&server)
        .await;
    server
}

fn build(server: &MockServer) -> LidarrClient {
    let settings = LidarrSettings {
        base_url: Some(server.uri()),
        api_key: Some("secret".into()),
        root_folder_path: Some("/data/music".into()),
        quality_profile_id: 3,
        metadata_profile_id: 4,
        ..Default::default()
    };
    LidarrClient::new(Arc::new(SettingsStore::for_tests(AppSettings {
        lidarr: settings,
        ..Default::default()
    })))
}

fn candidate(foreign_id: &str, title: &str, artist: &str, year: Option<i32>) -> LidarrAlbumCandidate {
    let resource = json!({
        "foreignAlbumId": foreign_id,
        "title": title,
        "releaseDate": year.map(|y| format!("{y}-01-01T00:00:00Z")),
        "artist": { "artistName": artist, "foreignArtistId": "artist-mbid" },
    });
    LidarrAlbumCandidate {
        id: 0,
        foreign_album_id: foreign_id.into(),
        title: title.into(),
        artist: artist.into(),
        year,
        resource: resource.as_object().cloned().expect("an object"),
    }
}

fn path_and_query(request: &Request) -> String {
    match request.url.query() {
        Some(query) => format!("{}?{query}", request.url.path()),
        None => request.url.path().to_string(),
    }
}

fn api_key(request: &Request) -> Option<String> {
    request
        .headers
        .get("X-Api-Key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("a JSON body")
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.expect("recording is on")
}

#[tokio::test]
async fn options_use_api_key_and_map_server_choices() {
    let server = serve(|_, path| {
        match path {
            "/api/v1/rootfolder" => r#"[{"id":1,"path":"/data/music"}]"#,
            "/api/v1/qualityprofile" => r#"[{"id":2,"name":"Lossless"}]"#,
            "/api/v1/metadataprofile" => r#"[{"id":3,"name":"Standard"}]"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;

    let result = build(&server).get_options().await.expect("options");

    assert_eq!(result.root_folders.len(), 1);
    assert_eq!(result.root_folders[0].path, "/data/music");
    assert_eq!(result.quality_profiles.len(), 1);
    assert_eq!(result.quality_profiles[0].name, "Lossless");
    assert_eq!(result.metadata_profiles.len(), 1);
    assert_eq!(result.metadata_profiles[0].name, "Standard");
    for request in requests(&server).await {
        assert_eq!(api_key(&request).as_deref(), Some("secret"));
    }
}

#[tokio::test]
async fn connection_test_uses_entered_url_and_api_key_without_saving() {
    let server = serve(|_, path| {
        if path == "/api/v1/system/status" {
            "{}"
        } else {
            "[]"
        }
        .to_string()
    })
    .await;
    // The saved settings point elsewhere: the entered address is the one asked.
    let client = LidarrClient::new(Arc::new(SettingsStore::for_tests(AppSettings::default())));

    client
        .test_connection(&format!("{}/", server.uri()), "entered-key")
        .await
        .expect("the server answers");

    let requests = requests(&server).await;
    // The mock server sees the path and the Host header; together they are the URL asked
    // (the trailing slash entered is trimmed, so no "//").
    let host = server.address().to_string();
    assert!(
        requests
            .iter()
            .any(|r| path_and_query(r) == "/api/v1/system/status"
                && r.headers.get("host").and_then(|h| h.to_str().ok()) == Some(host.as_str()))
    );
    for request in &requests {
        assert_eq!(api_key(request).as_deref(), Some("entered-key"));
    }
    for path in [
        "/api/v1/rootfolder",
        "/api/v1/qualityprofile",
        "/api/v1/metadataprofile",
    ] {
        assert!(requests.iter().any(|r| path_and_query(r) == path), "{path}");
    }
}

// AlbumSelectionRequiresExactIdentityAndUsesYearToDisambiguate is in
// octo_core::lidarr::lidarr_client's tests, with select_best_album.

#[tokio::test]
async fn new_album_uses_chosen_defaults_and_does_not_monitor_future_releases() {
    let server = serve(|method, path| {
        match (method, path) {
            ("GET", "/api/v1/album") => "[]",
            ("GET", "/api/v1/artist") => "[]",
            ("POST", "/api/v1/album") => r#"{"id":42}"#,
            ("GET", "/api/v1/track") => r#"[{"id":1}]"#,
            ("POST", "/api/v1/command") => r#"{"id":9}"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;

    let id = build(&server)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    assert_eq!(id, 42);
    let requests = requests(&server).await;
    let adds: Vec<&Request> = requests
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/v1/album")
        .collect();
    assert_eq!(adds.len(), 1);
    let add = body(adds[0]);
    assert_eq!(add["artist"]["rootFolderPath"], "/data/music");
    assert_eq!(add["artist"]["qualityProfileId"], 3);
    assert_eq!(add["artist"]["metadataProfileId"], 4);
    assert_eq!(add["artist"]["monitorNewItems"], "none");
    assert_eq!(add["artist"]["addOptions"]["monitor"], "unknown");
    assert_eq!(add["artist"]["addOptions"]["albumsToMonitor"][0], "mbid");
    let commands: Vec<&Request> = requests
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/v1/command")
        .collect();
    assert_eq!(commands.len(), 1);
    let command = String::from_utf8_lossy(&commands[0].body);
    assert!(command.contains("AlbumSearch"));
    assert!(command.contains("42"));
}

#[tokio::test]
async fn existing_artist_settings_are_preserved_when_adding_album() {
    let server = serve(|method, path| {
        match (method, path) {
            ("GET", "/api/v1/album") => "[]",
            ("GET", "/api/v1/artist") => {
                r#"[{"id":7,"foreignArtistId":"artist-mbid","path":"/existing/Artist","qualityProfileId":9,"metadataProfileId":10}]"#
            }
            ("POST", "/api/v1/album") => r#"{"id":42}"#,
            ("GET", "/api/v1/track") => r#"[{"id":1}]"#,
            ("POST", "/api/v1/command") => r#"{"id":9}"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;

    build(&server)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    let requests = requests(&server).await;
    let adds: Vec<&Request> = requests
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/v1/album")
        .collect();
    assert_eq!(adds.len(), 1);
    let add = body(adds[0]);
    assert_eq!(add["artistId"], 7);
    assert_eq!(add["artist"]["path"], "/existing/Artist");
    assert_eq!(add["artist"]["qualityProfileId"], 9);
    assert_eq!(add["artist"]["metadataProfileId"], 10);
}

#[tokio::test]
async fn unmonitored_existing_artist_is_monitored_and_otherwise_unchanged() {
    let server = serve(|method, path| {
        match (method, path) {
            ("GET", "/api/v1/album") => {
                r#"[{"id":12,"foreignAlbumId":"mbid","monitored":true,"artist":{"id":7,"monitored":false}}]"#
            }
            ("GET", "/api/v1/artist/7") => {
                r#"{"id":7,"monitored":false,"monitorNewItems":"none","qualityProfileId":9}"#
            }
            ("PUT", "/api/v1/artist/7") => r#"{"id":7,"monitored":true}"#,
            ("GET", "/api/v1/track") => r#"[{"id":1}]"#,
            ("POST", "/api/v1/command") => r#"{"id":9}"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;

    build(&server)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    let requests = requests(&server).await;
    let puts: Vec<&Request> = requests.iter().filter(|r| r.method.as_str() == "PUT").collect();
    assert_eq!(puts.len(), 1);
    let update = body(puts[0]);
    assert_eq!(update["monitored"], true);
    assert_eq!(update["monitorNewItems"], "none");
    assert_eq!(update["qualityProfileId"], 9);
}

#[tokio::test]
async fn a_remonitored_artist_does_not_start_following_new_releases() {
    let server = serve(|method, path| {
        match (method, path) {
            ("GET", "/api/v1/album") => {
                r#"[{"id":12,"foreignAlbumId":"mbid","monitored":true,"artist":{"id":7,"monitored":false}}]"#
            }
            ("GET", "/api/v1/artist/7") => {
                r#"{"id":7,"monitored":false,"monitorNewItems":"all","qualityProfileId":9}"#
            }
            ("PUT", "/api/v1/artist/7") => r#"{"id":7,"monitored":true}"#,
            ("GET", "/api/v1/track") => r#"[{"id":1}]"#,
            ("POST", "/api/v1/command") => r#"{"id":9}"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;

    build(&server)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    let requests = requests(&server).await;
    let puts: Vec<&Request> = requests.iter().filter(|r| r.method.as_str() == "PUT").collect();
    assert_eq!(puts.len(), 1);
    let update = body(puts[0]);
    assert_eq!(update["monitored"], true);
    assert_eq!(update["monitorNewItems"], "none");
    assert_eq!(update["qualityProfileId"], 9);
}

#[tokio::test]
async fn existing_album_is_monitored_without_changing_its_artist() {
    let server = serve(|method, path| {
        match (method, path) {
            ("GET", "/api/v1/album") => {
                r#"[{"id":12,"foreignAlbumId":"mbid","monitored":false,"artist":{"id":7,"qualityProfileId":9}}]"#
            }
            ("PUT", "/api/v1/album/12") => r#"{"id":12,"monitored":true}"#,
            ("GET", "/api/v1/track") => r#"[{"id":1}]"#,
            ("POST", "/api/v1/command") => r#"{"id":9}"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;

    let id = build(&server)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    assert_eq!(id, 12);
    let requests = requests(&server).await;
    let puts: Vec<&Request> = requests.iter().filter(|r| r.method.as_str() == "PUT").collect();
    assert_eq!(puts.len(), 1);
    let update = body(puts[0]);
    assert_eq!(update["monitored"], true);
    assert_eq!(update["artist"]["qualityProfileId"], 9);
    assert!(!requests.iter().any(|r| r.url.path() == "/api/v1/artist"));
}

#[tokio::test]
async fn album_tracks_join_track_files_returned_by_separate_endpoint() {
    let server = serve(|_, path| {
        match path {
            "/api/v1/track" => {
                r#"[{"id":1,"title":"Song","trackNumber":"1","duration":123000,"hasFile":true,"trackFileId":8}]"#
            }
            "/api/v1/trackFile" => r#"[{"id":8,"path":"/data/music/Artist/Album/01.flac","size":456}]"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;

    let tracks = build(&server).get_album_tracks(42).await.expect("tracks");

    assert_eq!(tracks.len(), 1);
    let track = &tracks[0];
    assert!(track.has_file);
    assert_eq!(track.path.as_deref(), Some("/data/music/Artist/Album/01.flac"));
    assert_eq!(track.size_bytes, 456);
    assert_eq!(track.duration_seconds, Some(123));
    assert_eq!(track.track_file_id, 8);
    let requests = requests(&server).await;
    assert!(
        requests
            .iter()
            .any(|r| path_and_query(r) == "/api/v1/track?albumId=42")
    );
    assert!(
        requests
            .iter()
            .any(|r| path_and_query(r) == "/api/v1/trackFile?albumId=42")
    );
}

#[tokio::test]
async fn import_completion_uses_album_statistics_not_alternate_release_rows() {
    let server = serve(|_, path| {
        match path {
            "/api/v1/album/42" => r#"{"id":42,"statistics":{"trackCount":1,"trackFileCount":1}}"#,
            "/api/v1/track" => {
                r#"[{"id":1,"title":"Song","hasFile":true,"trackFileId":8},{"id":2,"title":"Alternate release bonus","hasFile":false,"trackFileId":0}]"#
            }
            "/api/v1/trackFile" => r#"[{"id":8,"path":"/data/music/Artist/Album/01.flac","size":456}]"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;

    let state = build(&server).get_album_import_state(42).await.expect("a state");

    assert!(state.is_complete());
    assert_eq!(state.track_count, 1);
    assert_eq!(state.tracks.len(), 2);
}

#[test]
fn imported_paths_translate_relative_to_selected_root_and_reject_escapes() {
    let translated = translate_imported_path(
        "/data/music/Radiohead/In Rainbows/01.flac",
        Some("/data/music"),
        "/music",
    );
    assert_eq!(translated.as_deref(), Ok("/music/Radiohead/In Rainbows/01.flac"));
    assert!(matches!(
        translate_imported_path("/downloads/other.flac", Some("/data/music"), "/music"),
        Err(LidarrError::InvalidOperation(_))
    ));
}

// What the client fails with, by kind.

#[tokio::test]
async fn an_error_status_is_an_http_error_with_the_start_of_the_body() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401).set_body_string("x".repeat(400)))
        .mount(&server)
        .await;

    let error = build(&server).get_options().await.expect_err("401");
    assert_eq!(
        error,
        LidarrError::Http(format!("Lidarr returned HTTP 401: {}", "x".repeat(300)))
    );
    assert!(!build(&server).is_reachable().await);
}

#[tokio::test]
async fn nothing_is_asked_until_lidarr_is_set_up() {
    let client = LidarrClient::new(Arc::new(SettingsStore::for_tests(AppSettings::default())));
    assert_eq!(
        client.get_options().await,
        Err(LidarrError::InvalidOperation(
            "Lidarr URL and API key are required.".into()
        ))
    );
    assert!(!client.is_reachable().await);
    assert!(matches!(
        client.test_connection(" ", "key").await,
        Err(LidarrError::InvalidOperation(_))
    ));
}

#[tokio::test]
async fn adding_an_album_needs_the_root_folder_and_both_profiles() {
    let server = serve(|_, _| "[]".to_string()).await;
    let client = LidarrClient::new(Arc::new(SettingsStore::for_tests(AppSettings {
        lidarr: LidarrSettings {
            base_url: Some(server.uri()),
            api_key: Some("k".into()),
            ..Default::default()
        },
        ..Default::default()
    })));
    assert_eq!(
        client
            .start_album_search(&candidate("mbid", "Album", "Artist", None), None)
            .await,
        Err(LidarrError::InvalidOperation(
            "Choose a Lidarr root folder, quality profile, and metadata profile.".into()
        ))
    );
    assert!(client.is_reachable().await);
}

#[tokio::test]
async fn a_lookup_with_no_single_match_is_an_invalid_operation() {
    let server = serve(|_, path| {
        if path == "/api/v1/album/lookup" {
            r#"[{"foreignAlbumId":"a","title":"In Rainbows","releaseDate":"2007-10-10","artist":{"artistName":"Radiohead"}},
                {"foreignAlbumId":"","title":"In Rainbows","artist":{"artistName":"Radiohead"}}]"#
        } else {
            "[]"
        }
        .to_string()
    })
    .await;
    let client = build(&server);

    let found = client
        .resolve_album("Radiohead", "In Rainbows", None)
        .await
        .expect("one album");
    assert_eq!(found.foreign_album_id, "a");
    assert_eq!(found.year, Some(2007));
    assert_eq!(
        client.resolve_album("Radiohead", "Kid A", None).await,
        Err(LidarrError::InvalidOperation(
            "Lidarr could not unambiguously match 'Radiohead - Kid A'.".into()
        ))
    );
    assert_eq!(
        client
            .resolve_album_by_foreign_id("A")
            .await
            .map(|c| c.map(|c| c.title)),
        Ok(Some("In Rainbows".to_string()))
    );
    let lookups: Vec<String> = requests(&server).await.iter().map(path_and_query).collect();
    assert!(lookups.contains(&"/api/v1/album/lookup?term=Radiohead%20In%20Rainbows".to_string()));
    assert!(lookups.contains(&"/api/v1/album/lookup?term=lidarr%3AA".to_string()));
}

// ---- Releases: the Lidarr half of Find songs picks ------------------------------------------

#[tokio::test]
async fn releases_are_listed_with_their_quality_and_why_lidarr_rejects_one() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::path("/api/v1/release"))
        .and(wiremock::matchers::query_param("albumId", "12"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"guid":"a1","indexerId":3,"indexer":"Redacted","title":"Artist - Album (2020) [FLAC]","size":312000000,
                 "quality":{"quality":{"name":"FLAC"}},"seeders":14,"leechers":1,"protocol":"torrent","ageHours":49.0,
                 "rejected":false,"rejections":[]},
                {"guid":"b2","indexerId":4,"indexer":"NZBgeek","title":"Artist - Album MP3 V0","size":90000000.0,
                 "quality":{"quality":{"name":"MP3-VBR-V0"}},"protocol":"usenet","age":12,
                 "rejected":true,"rejections":["Quality is not wanted in profile"]},
                {"guid":"","indexerId":5,"title":"no guid"}]"#,
        ))
        .mount(&server)
        .await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;

    let releases = build(&server).list_releases(12).await.expect("releases");

    assert_eq!(releases.len(), 2);
    let flac = &releases[0];
    assert_eq!(
        (
            flac.guid.as_str(),
            flac.indexer_id,
            flac.indexer.as_deref(),
            flac.quality.as_deref(),
            flac.size,
            flac.seeders,
            flac.age_days,
            flac.rejected
        ),
        (
            "a1",
            3,
            Some("Redacted"),
            Some("FLAC"),
            Some(312000000),
            Some(14),
            Some(2),
            false
        )
    );
    let mp3 = &releases[1];
    assert!(mp3.rejected);
    assert_eq!(mp3.size, Some(90000000));
    assert_eq!(mp3.age_days, Some(12));
    assert_eq!(
        mp3.rejections,
        vec!["Quality is not wanted in profile".to_string()]
    );
}

#[tokio::test]
async fn a_picked_release_is_grabbed_instead_of_an_album_search() {
    let server = serve(|method, path| {
        match (method, path) {
            ("GET", "/api/v1/album") => {
                r#"[{"id":12,"foreignAlbumId":"mbid","monitored":true,"artist":{"id":7,"monitored":true}}]"#
            }
            ("POST", "/api/v1/release") => r#"{"guid":"a1"}"#,
            ("GET", "/api/v1/track") => r#"[{"id":1}]"#,
            ("POST", "/api/v1/command") => r#"{"id":9}"#,
            _ => "[]",
        }
        .to_string()
    })
    .await;
    let pick = LidarrReleasePick {
        guid: "a1".into(),
        indexer_id: 3,
        title: Some("Artist - Album (2020) [FLAC]".into()),
        size: None,
    };

    let started = build(&server)
        .start_album_search(&candidate("mbid", "Album", "Artist", Some(2020)), Some(&pick))
        .await
        .expect("started");

    assert_eq!(started.album_id, 12);
    let requests = requests(&server).await;
    assert!(!requests.iter().any(|r| r.url.path() == "/api/v1/command"));
    let grabs: Vec<&Request> = requests
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/v1/release")
        .collect();
    assert_eq!(grabs.len(), 1);
    let grab = body(grabs[0]);
    assert_eq!(grab["guid"], "a1");
    assert_eq!(grab["indexerId"], 3);
}

/// A forgotten guid is found again by indexer, title (any case, trimmed) and size, and a size
/// that differs is another release.
#[tokio::test]
async fn a_forgotten_pick_is_grabbed_again_under_its_new_guid() {
    let server = MockServer::builder().start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v1/release"))
        .and(wiremock::matchers::body_partial_json(json!({ "guid": "stale" })))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            r#"{"message":"Couldn't find requested release in cache, cache timeout probably expired."}"#,
        ))
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v1/release"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/release"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{"guid":"bigger","indexerId":3,"title":"Air - Moon Safari [FLAC]","size":999},
                {"guid":"fresh","indexerId":3,"title":" air - moon safari [flac] ","size":300}]"#,
        ))
        .mount(&server)
        .await;
    let client = build(&server);
    let pick = |size| LidarrReleasePick {
        guid: "stale".into(),
        indexer_id: 3,
        title: Some("Air - Moon Safari [FLAC]".into()),
        size,
    };

    client
        .grab_picked(7, &pick(Some(300)))
        .await
        .expect("found again");

    let grabbed: Vec<Value> = requests(&server)
        .await
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .map(body)
        .collect();
    assert_eq!(grabbed.len(), 2);
    assert_eq!(grabbed[1]["guid"], "fresh");
    // Without a size of its own the pick takes the first of that title.
    client.grab_picked(7, &pick(None)).await.expect("found again");
    let last = requests(&server)
        .await
        .into_iter()
        .rev()
        .find(|r| r.method.as_str() == "POST");
    assert_eq!(body(&last.expect("a grab"))["guid"], "bigger");
    // A pick with no title cannot be found again.
    let untitled = LidarrReleasePick {
        title: None,
        ..pick(Some(300))
    };
    assert_eq!(
        client.grab_picked(7, &untitled).await,
        Err(LidarrError::InvalidOperation(PICK_GONE_TEXT.into()))
    );
}

// ---- Waiting for an album's track list before AlbumSearch -----------------------------------

/// A Lidarr whose album 12 (artist 7) has no tracks until a refresh finishes. Each look at the
/// command list moves a running refresh one step on; `refresh_steps` looks finish a refresh
/// that was already running (`usize::MAX`: never), one look a RefreshAlbum asked for.
#[derive(Clone)]
struct RefreshingLidarr {
    state: Arc<parking_lot::Mutex<RefreshState>>,
}

struct RefreshState {
    has_tracks: bool,
    /// The refresh Lidarr is running: (name, body, looks left until it finishes).
    running: Option<(String, Value, usize)>,
    /// Whether a finished refresh brings the tracks.
    refresh_brings_tracks: bool,
    /// What happened, in order: "tracks", "commands", "RefreshAlbum", "AlbumSearch".
    log: Vec<String>,
}

impl Respond for RefreshingLidarr {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.state.lock();
        let ok = |body: Value| ResponseTemplate::new(200).set_body_string(body.to_string());
        match (request.method.as_str(), request.url.path()) {
            ("GET", "/api/v1/album") => ok(json!([
                { "id": 12, "foreignAlbumId": "mbid", "monitored": true, "artistId": 7,
                  "artist": { "id": 7, "monitored": true } }
            ])),
            ("GET", "/api/v1/track") => {
                state.log.push("tracks".into());
                ok(if state.has_tracks {
                    json!([{ "id": 1, "title": "Song" }])
                } else {
                    json!([])
                })
            }
            ("GET", "/api/v1/command") => {
                state.log.push("commands".into());
                let mut listed = Vec::new();
                if let Some((name, body, left)) = state.running.take() {
                    if left == 0 {
                        if state.refresh_brings_tracks {
                            state.has_tracks = true;
                        }
                    } else {
                        listed.push(json!({ "id": 3, "name": name, "status": "started", "body": body }));
                        let left = if left == usize::MAX { left } else { left - 1 };
                        state.running = Some((name, body, left));
                    }
                }
                listed.push(json!({ "id": 2, "name": "RssSync", "status": "started", "body": {} }));
                ok(Value::Array(listed))
            }
            ("POST", "/api/v1/command") => {
                let body: Value = serde_json::from_slice(&request.body).expect("a JSON body");
                let name = body["name"].as_str().unwrap_or_default().to_string();
                state.log.push(name.clone());
                if name == "RefreshAlbum" {
                    state.running = Some((name, body, 1));
                }
                ok(json!({ "id": 9 }))
            }
            _ => ok(json!([])),
        }
    }
}

async fn refreshing_lidarr(
    has_tracks: bool,
    running: Option<(&str, Value)>,
    refresh_steps: usize,
    refresh_brings_tracks: bool,
) -> (RefreshingLidarr, MockServer) {
    let fake = RefreshingLidarr {
        state: Arc::new(parking_lot::Mutex::new(RefreshState {
            has_tracks,
            running: running.map(|(name, body)| (name.to_string(), body, refresh_steps)),
            refresh_brings_tracks,
            log: Vec::new(),
        })),
    };
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(fake.clone()).mount(&server).await;
    (fake, server)
}

fn build_waiting(server: &MockServer, import_timeout_seconds: i32) -> LidarrClient {
    let settings = LidarrSettings {
        base_url: Some(server.uri()),
        api_key: Some("secret".into()),
        root_folder_path: Some("/data/music".into()),
        quality_profile_id: 3,
        metadata_profile_id: 4,
        import_timeout_seconds,
        ..Default::default()
    };
    let client = LidarrClient::new(Arc::new(SettingsStore::for_tests(AppSettings {
        lidarr: settings,
        ..Default::default()
    })));
    client.set_metadata_poll(std::time::Duration::from_millis(5));
    client
}

fn log_of(fake: &RefreshingLidarr) -> Vec<String> {
    fake.state.lock().log.clone()
}

#[tokio::test]
async fn an_album_that_has_its_tracks_is_searched_without_waiting() {
    let (fake, server) = refreshing_lidarr(true, None, 0, false).await;

    build_waiting(&server, 60)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    assert_eq!(log_of(&fake), ["tracks", "AlbumSearch"]);
}

#[tokio::test]
async fn an_album_search_waits_for_the_artist_refresh_already_running() {
    let (fake, server) = refreshing_lidarr(
        false,
        Some(("RefreshArtist", json!({ "artistIds": [7] }))),
        2,
        true,
    )
    .await;

    build_waiting(&server, 60)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    let log = log_of(&fake);
    assert_eq!(log.last().map(String::as_str), Some("AlbumSearch"));
    assert!(!log.iter().any(|step| step == "RefreshAlbum"), "{log:?}");
    let searched_after = log
        .iter()
        .rposition(|step| step == "tracks")
        .expect("tracks looked at");
    assert_eq!(searched_after, log.len() - 2, "{log:?}");
    assert!(fake.state.lock().has_tracks);
}

#[tokio::test]
async fn an_album_with_no_tracks_and_no_refresh_gets_one_refresh_album_first() {
    let (fake, server) = refreshing_lidarr(false, None, 1, true).await;

    build_waiting(&server, 60)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    let requests = requests(&server).await;
    let refreshes: Vec<Value> = requests
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/v1/command")
        .map(body)
        .filter(|b| b["name"] == "RefreshAlbum")
        .collect();
    assert_eq!(refreshes.len(), 1);
    assert_eq!(refreshes[0]["albumId"], 12);
    let log = log_of(&fake);
    let refresh = log
        .iter()
        .position(|step| step == "RefreshAlbum")
        .expect("a refresh");
    let search = log
        .iter()
        .position(|step| step == "AlbumSearch")
        .expect("a search");
    assert!(refresh < search, "{log:?}");
    assert!(fake.state.lock().has_tracks);
}

#[tokio::test]
async fn a_refresh_that_brings_no_tracks_is_asked_once_and_the_search_still_starts() {
    let (fake, server) = refreshing_lidarr(false, None, 1, false).await;

    build_waiting(&server, 60)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    let log = log_of(&fake);
    assert_eq!(
        log.iter().filter(|step| *step == "RefreshAlbum").count(),
        1,
        "{log:?}"
    );
    assert_eq!(log.last().map(String::as_str), Some("AlbumSearch"));
}

#[tokio::test]
async fn a_refresh_that_never_ends_holds_the_search_only_until_the_import_timeout() {
    let (fake, server) = refreshing_lidarr(
        false,
        Some(("RefreshAlbum", json!({ "albumId": 12 }))),
        usize::MAX,
        false,
    )
    .await;

    let started = std::time::Instant::now();
    build_waiting(&server, 1)
        .ensure_album_and_search(&candidate("mbid", "Album", "Artist", Some(2020)))
        .await
        .expect("a search");

    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let log = log_of(&fake);
    assert_eq!(log.last().map(String::as_str), Some("AlbumSearch"));
    assert!(!log.iter().any(|step| step == "RefreshAlbum"), "{log:?}");
}

#[test]
fn the_wait_for_a_track_list_stops_at_five_minutes_whatever_the_import_timeout() {
    let wait = |import_timeout_seconds: i32| {
        track_list_wait(&LidarrSettings {
            import_timeout_seconds,
            ..Default::default()
        })
    };
    assert_eq!(wait(3600), std::time::Duration::from_secs(5 * 60));
    assert_eq!(wait(120), std::time::Duration::from_secs(120));
    assert_eq!(wait(0), std::time::Duration::from_secs(1));
}
