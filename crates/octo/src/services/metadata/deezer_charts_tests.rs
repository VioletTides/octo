use std::sync::Arc;

use octo_core::settings::{AppSettings, SettingsStore};
use serde_json::json;

use super::*;
use crate::services::metadata::deezer_rate_limit_handler::DeezerRateLimitHandler;
use crate::services::metadata::deezer_rate_limiter::DeezerRateLimiter;
use crate::services::test_support::{Matching, Routes};

async fn deezer(routes: Vec<(&str, Vec<&str>)>) -> (DeezerMetadataService, Routes, wiremock::MockServer) {
    let routes = Routes::new(routes, Matching::IgnoreCase);
    let server = routes.serve().await;
    let settings = SettingsStore::for_tests(AppSettings::default());
    let http = Arc::new(DeezerRateLimitHandler::new(Arc::new(DeezerRateLimiter::new())));
    let service = DeezerMetadataService::with_base_url(http, Arc::new(settings), server.uri());
    (service, routes, server)
}

const CHART: &str = r#"{"data":[
  {"id":3135556,"title":"Dracula (with JENNIE)","readable":true,"duration":185,"explicit_lyrics":true,"explicit_content_lyrics":1,
   "artist":{"id":27,"name":"Tame Impala"},"album":{"id":302127,"title":"Deadbeat","cover_xl":"https://cdn/xl.jpg","cover_big":"https://cdn/big.jpg"}},
  {"id":2,"title":"Gone","readable":false,"artist":{"id":1,"name":"Someone"}},
  {"id":3,"title":"One More Time","duration":320,"explicit_lyrics":false,"explicit_content_lyrics":0,
   "artist":{"id":27,"name":"Daft Punk"},"album":{"id":4,"title":"Discovery","cover_medium":"https://cdn/m.jpg"}},
  {"id":"4","title":"String id","artist":{"name":"X"}},
  {"id":5,"title":"No artist"}
]}"#;

#[tokio::test]
async fn the_chart_reads_each_readable_track_with_its_album_cover_and_marks() {
    let (service, routes, _server) = deezer(vec![("/chart/0/tracks", vec![CHART])]).await;

    let chart = service.get_chart(50).await.unwrap();

    assert_eq!(chart.len(), 2);
    let first = &chart[0];
    assert_eq!(first.catalog_id, "3135556");
    assert_eq!(first.title, "Dracula (with JENNIE)");
    assert_eq!(first.artist, "Tame Impala");
    assert_eq!(first.artist_id.as_deref(), Some("27"));
    assert_eq!(first.album.as_deref(), Some("Deadbeat"));
    assert_eq!(first.cover_url.as_deref(), Some("https://cdn/xl.jpg"));
    assert_eq!(first.duration, Some(185));
    assert_eq!(first.explicit_content, Some(1));
    assert_eq!(chart[1].cover_url.as_deref(), Some("https://cdn/m.jpg"));
    assert_eq!(chart[1].explicit_content, Some(0));
    assert_eq!(routes.calls("/chart/0/tracks"), 1);
}

#[tokio::test]
async fn the_chart_is_kept_and_asked_once() {
    let (service, routes, server) = deezer(vec![("/chart/0/tracks", vec![CHART])]).await;
    service.get_chart(50).await.unwrap();
    service.get_chart(50).await.unwrap();
    assert_eq!(routes.calls("/chart/0/tracks"), 1);
    let asked = &server.received_requests().await.unwrap()[0];
    assert_eq!(asked.url.query(), Some("limit=50"));
}

#[tokio::test]
async fn a_chart_deezer_did_not_answer_is_none_and_kept_nowhere() {
    let (service, _routes, server) = deezer(vec![("/nothing-matches", vec!["{}"])]).await;
    assert!(service.get_chart(50).await.is_none());
    assert!(service.get_chart(50).await.is_none());
    // A 404 is not an answer: the second call asks again.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_throttled_chart_is_none() {
    let throttled =
        json!({"error": {"code": 4, "type": "Exception", "message": "Quota limit exceeded"}}).to_string();
    let (service, _routes, _server) = deezer(vec![("/chart/0/tracks", vec![throttled.as_str()])]).await;
    assert!(service.get_chart(50).await.is_none());
}

#[tokio::test]
async fn the_limit_is_kept_between_one_and_a_hundred() {
    let (service, _routes, server) = deezer(vec![("/chart/0/tracks", vec![CHART])]).await;
    service.get_chart(500).await.unwrap();
    let asked = &server.received_requests().await.unwrap()[0];
    assert_eq!(asked.url.query(), Some("limit=100"));
}

#[tokio::test]
async fn an_artists_top_list_is_asked_by_id() {
    let (service, routes, _server) = deezer(vec![("/artist/27/top", vec![CHART])]).await;
    let top = service.get_artist_top("27", 20).await.unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(routes.calls("/artist/27/top"), 1);
    assert!(service.get_artist_top(" ", 20).await.unwrap().is_empty());
}
