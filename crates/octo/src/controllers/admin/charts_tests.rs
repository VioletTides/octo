use axum::http::StatusCode;
use octo_core::settings::{AppSettings, GeneratedPlaylistSettings, SubsonicSettings};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::test_app::{app, get};
use crate::app::{AppState, TestServices};

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

async fn apple() -> MockServer {
    let server = MockServer::start().await;
    for (at, genre, body) in [
        ("/new/top-charts/songs", None, fixture!("us_all")),
        ("/us/new/top-charts/songs", None, fixture!("us_all")),
        ("/us/new/top-charts/songs/", Some("18"), fixture!("us_18")),
        ("/us/new", None, fixture!("us_new")),
        ("/us/room/6818396128", None, fixture!("us_room_best_new")),
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

fn state(apple: &MockServer, sign_in: Option<&str>) -> AppState {
    AppState::for_tests_with(
        AppSettings {
            subsonic: SubsonicSettings {
                auto_detect_download_path: false,
                ..Default::default()
            },
            generated_playlists: GeneratedPlaylistSettings {
                charts: "new,18".into(),
                chart_country: "auto".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        TestServices {
            apple_music_base_url: Some(apple.uri()),
            sign_in: sign_in.map(str::to_string),
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn charts_lists_country_choices_and_last_fetches() {
    let apple = apple().await;
    let state = state(&apple, None);
    state.top_songs.chart("34").await;

    let reply = get(&app(state), "/api/admin/charts").await;

    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    assert_eq!(body["country"]["setting"], "auto");
    assert_eq!(body["country"]["detected"], "us");
    assert_eq!(body["country"]["used"], "us");
    assert_eq!(body["countries"].as_array().unwrap().len(), 172);
    assert_eq!(body["popularNow"], true);
    let choices = body["choices"].as_array().unwrap();
    assert_eq!(
        (choices[0]["id"].as_str(), choices[0]["on"].as_bool()),
        (Some("new"), Some(true))
    );
    assert_eq!(
        (choices[1]["id"].as_str(), choices[1]["on"].as_bool()),
        (Some("trending"), Some(false))
    );
    let hip_hop = choices.iter().find(|c| c["id"] == "18").unwrap();
    assert_eq!(
        (hip_hop["name"].as_str(), hip_hop["on"].as_bool()),
        (Some("Hip-Hop/Rap"), Some(true))
    );
    let lists = body["lists"].as_array().unwrap();
    let charts: Vec<&str> = lists.iter().filter_map(|l| l["chart"].as_str()).collect();
    assert_eq!(charts, vec!["34", "new", "18"]);
    assert_eq!(lists[0]["name"], "Popular right now");
    assert_eq!(lists[0]["source"], "apple");
    assert_eq!(lists[0]["songs"], 5);
    assert!(lists[0]["fetchedUtc"].as_str().is_some());
    assert!(lists[1]["source"].is_null(), "not fetched yet");
}

#[tokio::test]
async fn charts_needs_the_sign_in() {
    let apple = apple().await;
    let reply = get(&app(state(&apple, Some("required"))), "/api/admin/charts").await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}
