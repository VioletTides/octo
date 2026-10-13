//! getTopChart, getCharts, getArtistTopSongs, the standard getTopSongs and the chart playlists in
//! getPlaylists and getPlaylist, as apps see them.

use octo_core::settings::{AppSettings, GeneratedPlaylistSettings, SubsonicSettings};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

use super::super::test_app::*;
use crate::app::{AppState, TestServices};
use crate::http::pipeline::App;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/rust-migration/fixtures/apple-music/",
            $name,
            ".html"
        ))
    };
}

struct Harness {
    app: App,
    _state: AppState,
    navidrome: MockServer,
    _apple: MockServer,
    _deezer: MockServer,
}

/// Navidrome accepts the token "good"; its library has Quebec by Drake.
async fn navidrome() -> MockServer {
    let server = MockServer::builder().start().await;
    Mock::given(method("GET"))
        .respond_with(|request: &MockRequest| {
            let query = query_of(request);
            if query.get("t").map(String::as_str) != Some("good") {
                return navidrome_json(NAVIDROME_WRONG_PASSWORD);
            }
            match request.url.path() {
                "/rest/search3" | "/rest/search3.view" => {
                    let songs = if query.get("query").map(String::as_str) == Some("Drake Quebec") {
                        json!([{"id": "nd-quebec", "title": "Quebec", "artist": "Drake", "album": "HABIBTI (FOMO)", "coverArt": "al-1", "duration": 130}])
                    } else {
                        json!([])
                    };
                    navidrome_json(&json!({"subsonic-response": {"status": "ok", "version": "1.16.1", "searchResult3": {"song": songs}}}).to_string())
                }
                "/rest/getPlaylists" | "/rest/getPlaylists.view" => {
                    if query.get("f").map(String::as_str) == Some("json") {
                        navidrome_json(r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlists":{"playlist":[]}}}"#)
                    } else {
                        ResponseTemplate::new(200).set_body_raw(
                            br#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="1.16.1"><playlists></playlists></subsonic-response>"#.to_vec(),
                            "application/xml",
                        )
                    }
                }
                // Alice hearted Quebec after the chart was made.
                "/rest/getStarred2" | "/rest/getStarred2.view" => navidrome_json(
                    &json!({"subsonic-response": {"status": "ok", "version": "1.16.1", "starred2": {"song": [
                        {"id": "nd-quebec", "title": "Quebec", "artist": "Drake", "starred": "2026-10-09T10:00:00Z", "userRating": 4}
                    ]}}})
                    .to_string(),
                ),
                "/rest/getTopSongs" | "/rest/getTopSongs.view" => navidrome_json(
                    r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","topSongs":{}}}"#,
                ),
                // Navidrome answers in the format asked for: XML unless f=json.
                _ if query.get("f").map(String::as_str) == Some("json") => navidrome_json(NAVIDROME_OK),
                _ => ResponseTemplate::new(200).set_body_raw(
                    br#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="1.16.1"></subsonic-response>"#.to_vec(),
                    "application/xml",
                ),
            }
        })
        .mount(&server)
        .await;
    server
}

async fn apple() -> MockServer {
    let server = MockServer::start().await;
    for (at, genre, body) in [
        ("/us/new/top-charts/songs", None, fixture!("us_all")),
        ("/us/new/top-charts/songs/", Some("18"), fixture!("us_18")),
        ("/us/new", None, fixture!("us_new")),
        ("/us/room/6818396128", None, fixture!("us_room_best_new")),
        ("/us/room/6818395867", None, fixture!("us_room_trending")),
    ] {
        let mock = Mock::given(method("GET")).and(path(at));
        let mock = match genre {
            Some(genre) => mock.and(query_param("genreId", genre)),
            None => mock.and(query_param_is_missing("genreId")),
        };
        mock.respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/html"))
            .mount(&server)
            .await;
    }
    server
}

/// Deezer for the artist top songs: one artist, Daft Punk, and its top list.
async fn deezer() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/artist"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"data": [{"id": 27, "name": "Daft Punk", "nb_fan": 1000, "nb_album": 10}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/artist/27/top"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            {"id": 1, "title": "One More Time", "duration": 320, "artist": {"id": 27, "name": "Daft Punk"}, "album": {"id": 4, "title": "Discovery"}},
            {"id": 2, "title": "Around the World", "duration": 429, "artist": {"id": 27, "name": "Daft Punk"}, "album": {"id": 5, "title": "Homework"}}
        ]})))
        .mount(&server)
        .await;
    server
}

async fn harness(charts: &str, discovery: bool) -> Harness {
    let (navidrome, apple, deezer) = (navidrome().await, apple().await, deezer().await);
    let settings = AppSettings {
        subsonic: SubsonicSettings {
            url: Some(navidrome.uri()),
            auto_detect_download_path: false,
            enable_search_discovery: discovery,
            ..Default::default()
        },
        generated_playlists: GeneratedPlaylistSettings {
            charts: charts.to_string(),
            chart_country: "us".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let state = AppState::for_tests_with(
        settings,
        TestServices {
            apple_music_base_url: Some(apple.uri()),
            deezer_base_url: Some(deezer.uri()),
            ..Default::default()
        },
    );
    Harness {
        app: app(&state),
        _state: state,
        navidrome,
        _apple: apple,
        _deezer: deezer,
    }
}

const ALICE: &str = "u=alice&t=good&s=salt&v=1.16.1&c=test";
const WRONG: &str = "u=alice&t=bad&s=salt&v=1.16.1&c=test";

async fn top(h: &Harness, query: &str) -> Value {
    get(&h.app, &format!("/rest/{query}")).await.json()["subsonic-response"].clone()
}

// ---- getTopChart ------------------------------------------------------------------------

#[tokio::test]
async fn top_chart_without_a_chart_is_the_overall() {
    let h = harness("", true).await;

    let answer = top(&h, &format!("getTopChart?{ALICE}")).await;

    let list = &answer["topSongs"];
    assert_eq!(answer["type"], "octo");
    assert_eq!(list["source"], "apple");
    assert_eq!(list["chart"], "34");
    assert_eq!(list["name"], "Popular right now");
    assert_eq!(list["country"], "us");
    let entries = list["entry"].as_array().unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0]["song"]["title"], "Solar Eclipse");
    assert_eq!(entries[0]["inLibrary"], false);
    // Quebec is in Alice's library: her own copy, as Navidrome describes it.
    assert_eq!(entries[1]["inLibrary"], true);
    assert_eq!(entries[1]["song"]["id"], "nd-quebec");
}

#[tokio::test]
async fn top_chart_takes_a_chart() {
    let h = harness("", true).await;

    let genre = top(&h, &format!("getTopChart?{ALICE}&chart=18&count=2")).await;
    let new = top(&h, &format!("getTopChart.view?{ALICE}&chart=NEW")).await;

    assert_eq!(genre["topSongs"]["name"], "Top Hip-Hop/Rap");
    assert_eq!(genre["topSongs"]["entry"].as_array().unwrap().len(), 2);
    assert_eq!(new["topSongs"]["name"], "Best New Songs");
    assert_eq!(
        new["topSongs"]["entry"][1]["song"]["title"],
        "Chelsea Boots (Bonus Track)"
    );
}

#[tokio::test]
async fn top_chart_for_a_missing_genre_is_empty() {
    let h = harness("", true).await;
    let answer = top(&h, &format!("getTopChart?{ALICE}&chart=99999")).await;
    assert_eq!(answer["status"], "ok");
    assert!(answer["topSongs"]["entry"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn top_chart_refuses_an_unknown_chart_and_a_wrong_password() {
    let h = harness("", true).await;
    assert_eq!(
        top(&h, &format!("getTopChart?{ALICE}&chart=pop")).await["error"]["code"],
        10
    );
    assert_eq!(
        top(&h, &format!("getTopChart?{WRONG}")).await["error"]["code"],
        40
    );
}

#[tokio::test]
async fn without_search_discovery_the_chart_is_empty() {
    let h = harness("", false).await;
    let answer = top(&h, &format!("getTopChart?{ALICE}")).await;
    assert!(answer["topSongs"]["entry"].as_array().unwrap().is_empty());
    assert!(answer["topSongs"].get("chart").is_none());
}

// ---- getCharts --------------------------------------------------------------------------

#[tokio::test]
async fn charts_list_overall_new_trending_then_the_countrys_genres() {
    let h = harness("new,18", true).await;

    let answer = top(&h, &format!("getCharts?{ALICE}")).await;

    let charts = &answer["charts"];
    assert_eq!(charts["country"], "us");
    let rows = charts["chart"].as_array().unwrap();
    let ids: Vec<&str> = rows.iter().take(4).filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids, vec!["34", "new", "trending", "20"]);
    assert_eq!(rows.len(), 3 + 21, "the US menu less All Genres");
    assert_eq!(rows[0]["label"], "Top songs");
    assert_eq!(rows[0]["on"], true);
    assert_eq!(rows[2]["on"], false, "trending is not in this server's list");
    let hip_hop = rows.iter().find(|r| r["id"] == "18").unwrap();
    assert_eq!(
        (
            hip_hop["name"].as_str(),
            hip_hop["label"].as_str(),
            hip_hop["on"].as_bool()
        ),
        (Some("Top Hip-Hop/Rap"), Some("Hip-Hop/Rap"), Some(true))
    );
}

#[tokio::test]
async fn charts_mark_the_playlists_once_made() {
    let h = harness("", true).await;
    get(
        &h.app,
        &format!("/rest/getPlaylist?{ALICE}&f=json&id={}", popular_id()),
    )
    .await;

    let answer = top(&h, &format!("getCharts?{ALICE}")).await;

    assert_eq!(answer["charts"]["chart"][0]["playlist"], popular_id().as_str());
}

#[tokio::test]
async fn charts_refuse_a_wrong_password() {
    let h = harness("", true).await;
    assert_eq!(top(&h, &format!("getCharts?{WRONG}")).await["error"]["code"], 40);
}

// ---- the chart playlists ---------------------------------------------------------------

fn popular_id() -> String {
    octo_core::library::generated_playlist_service::playlist_id("alice", "chart:popular")
}

#[tokio::test]
async fn get_playlists_lists_the_chart_lists_read_only() {
    let h = harness("new", true).await;
    // The first listing makes the lists; the next one shows them all.
    get(
        &h.app,
        &format!("/rest/getPlaylist?{ALICE}&f=json&id={}", popular_id()),
    )
    .await;

    let answer = top(&h, &format!("getPlaylists?{ALICE}&f=json")).await;

    let rows = answer["playlists"]["playlist"].as_array().unwrap();
    let names: Vec<&str> = rows.iter().filter_map(|r| r["name"].as_str()).collect();
    // Alice has hearted a song, so her Liked Songs comes first.
    assert_eq!(names, vec!["Liked Songs", "Popular right now", "Best New Songs"]);
    let rows = &rows[1..];
    let popular = &rows[0];
    assert_eq!(popular["id"], popular_id().as_str());
    assert_eq!(popular["readonly"], true);
    assert_eq!(popular["public"], false);
    assert_eq!(popular["songCount"], 5);
    let version = octo_media::cover::CoverBook::shipped().version;
    assert_eq!(popular["coverArt"], format!("{}_{version}c5", popular_id()));
    // The Octo apps put "Popular" on its cover; the other charts keep their names.
    assert_eq!(popular["octoCoverTitle"], "Popular");
    assert!(rows[1].get("octoCoverTitle").is_none());
    assert_eq!(
        popular["comment"],
        octo_subsonic::subsonic_response_builder::POPULAR_COMMENT
    );
    assert_eq!(
        rows[1]["comment"],
        "The best new songs of the week, made by Octo. Songs you have play from your library."
    );

    // The cover id the row names draws the chart's own cover; Navidrome is never asked for it.
    let cover_id = popular["coverArt"].as_str().expect("a cover id");
    let cover = get(
        &h.app,
        &format!("/rest/getCoverArt?{ALICE}&id={cover_id}&size=300"),
    )
    .await;
    assert!(cover.status.is_success(), "{}", cover.status);
    assert_eq!(cover.media_type().as_deref(), Some("image/jpeg"));
    let asked = h.navidrome.received_requests().await.unwrap_or_default();
    assert!(
        !asked
            .iter()
            .any(|r| r.url.path().ends_with("getCoverArt") && r.url.as_str().contains(&popular_id())),
        "Navidrome was asked for the chart's cover"
    );
}

#[tokio::test]
async fn get_playlists_xml_lists_them_too() {
    let h = harness("", true).await;
    get(
        &h.app,
        &format!("/rest/getPlaylist?{ALICE}&f=json&id={}", popular_id()),
    )
    .await;

    let xml = get(&h.app, &format!("/rest/getPlaylists?{ALICE}")).await.text();

    assert!(xml.contains("name=\"Popular right now\""), "{xml}");
    assert!(xml.contains("readonly=\"true\""), "{xml}");
    assert!(xml.contains("octoCoverTitle=\"Popular\""), "{xml}");
}

#[tokio::test]
async fn get_playlist_library_songs_are_the_listeners_own_the_rest_outside() {
    let h = harness("", true).await;

    let answer = top(&h, &format!("getPlaylist?{ALICE}&f=json&id={}", popular_id())).await;

    let playlist = &answer["playlist"];
    assert_eq!(playlist["name"], "Popular right now");
    let entries = playlist["entry"].as_array().unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(playlist["songCount"], 5);
    assert_eq!(entries[1]["id"], "nd-quebec");
    assert_eq!(entries[0]["title"], "Solar Eclipse");
    assert_eq!(entries[0]["isExternal"], true);
    // The listener's hearts and ratings as they are now, on library songs only.
    assert_eq!(entries[1]["starred"], "2026-10-09T10:00:00Z");
    assert_eq!(entries[1]["userRating"], 4);
    assert!(entries[0].get("starred").is_none());
}

#[tokio::test]
async fn get_playlist_xml_entries_carry_their_fields() {
    let h = harness("", true).await;

    let xml = get(&h.app, &format!("/rest/getPlaylist?{ALICE}&id={}", popular_id()))
        .await
        .text();

    assert!(xml.contains("<entry "), "{xml}");
    assert!(xml.contains("id=\"nd-quebec\""), "{xml}");
}

#[tokio::test]
async fn get_playlist_refuses_a_wrong_password() {
    let h = harness("", true).await;
    let answer = top(&h, &format!("getPlaylist?{WRONG}&f=json&id={}", popular_id())).await;
    assert_eq!(answer["error"]["code"], 40);
}

#[tokio::test]
async fn off_its_id_is_navidromes_to_answer() {
    let h = harness("", true).await;
    let mut settings = (*h._state.settings.current()).clone();
    settings.generated_playlists.popular_now = false;
    h._state.settings.set(settings);

    get(
        &h.app,
        &format!("/rest/getPlaylist?{ALICE}&f=json&id={}", popular_id()),
    )
    .await;

    let asked = received_paths(&h.navidrome).await;
    assert!(asked.iter().any(|p| p == "/rest/getPlaylist"), "{asked:?}");
}

// ---- the standard getTopSongs ----------------------------------------------------------

#[tokio::test]
async fn standard_top_songs_ranks_as_octo_does() {
    let h = harness("", true).await;

    let answer = top(&h, &format!("getTopSongs?{ALICE}&f=json&artist=Daft%20Punk")).await;

    let songs = answer["topSongs"]["song"].as_array().unwrap();
    let titles: Vec<&str> = songs.iter().filter_map(|s| s["title"].as_str()).collect();
    assert_eq!(titles, vec!["One More Time", "Around the World"]);
}

#[tokio::test]
async fn standard_top_songs_with_nothing_to_give_is_navidromes_answer() {
    let h = harness("", true).await;

    let answer = top(&h, &format!("getTopSongs?{ALICE}&f=json&artist=Nobody%20Known")).await;

    assert_eq!(answer["type"], "navidrome");
}

#[tokio::test]
async fn a_missing_artist_is_navidromes_to_refuse() {
    let h = harness("", true).await;
    top(&h, &format!("getTopSongs?{ALICE}&f=json")).await;
    assert!(
        received_paths(&h.navidrome)
            .await
            .iter()
            .any(|p| p == "/rest/getTopSongs")
    );
}
