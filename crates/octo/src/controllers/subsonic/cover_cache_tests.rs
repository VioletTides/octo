//! `getCoverArt` for library covers through the cover cache, against a Navidrome stand-in: one
//! fetch at full size per cover, every size cut from it, the sign-in still checked, and nothing
//! kept that is not a real picture. Each test owns its Navidrome.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use octo_media::cover::cover_image;
use wiremock::matchers::{path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::test_app::*;
use crate::app::AppState;
use crate::services::cover_art::LocalCoverCache;

const AUTH: &str = "u=alice&t=good&s=salt&v=1.16.1&c=test";

fn jpeg(side: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(side, side)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .expect("encoded");
    bytes
}

/// A cover as Navidrome sends a real one: dated.
fn dated(bytes: Vec<u8>) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_raw(bytes, "image/jpeg")
        .insert_header("Last-Modified", "Mon, 12 Oct 2026 13:49:50 GMT")
}

/// Navidrome accepting the `good` token and refusing any other, answering covers with `cover`.
async fn navidrome(cover: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/rest/ping"))
        .and(query_param("t", "good"))
        .respond_with(navidrome_json(NAVIDROME_OK))
        .mount(&server)
        .await;
    Mock::given(path("/rest/ping"))
        .respond_with(navidrome_json(NAVIDROME_WRONG_PASSWORD))
        .mount(&server)
        .await;
    Mock::given(path("/rest/getCoverArt"))
        .and(query_param("t", "good"))
        .respond_with(cover)
        .mount(&server)
        .await;
    Mock::given(path("/rest/getCoverArt"))
        .respond_with(navidrome_json(NAVIDROME_WRONG_PASSWORD))
        .mount(&server)
        .await;
    server
}

/// The app over `server`, its cover cache in its own folder.
fn cached_state(server: &MockServer) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let cache = Arc::new(LocalCoverCache::new(
        dir.path().to_path_buf(),
        Arc::new(|| 1 << 30),
    ));
    let state = state_with(settings(&server.uri()), |inner| {
        inner.local_cover_cache = cache;
    });
    (state, dir)
}

/// The query of every getCoverArt Navidrome received, in order.
async fn cover_calls(server: &MockServer) -> Vec<HashMap<String, String>> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == "/rest/getCoverArt")
        .map(query_of)
        .collect()
}

#[tokio::test]
async fn a_library_cover_is_fetched_once_at_full_size_and_cut_for_each_size() {
    let original = jpeg(1200);
    let server = navidrome(dated(original.clone())).await;
    let (state, _dir) = cached_state(&server);
    let app = app(&state);

    for (size, side) in [(300, 300), (300, 300), (600, 600), (150, 150), (1200, 1200)] {
        let reply = get(
            &app,
            &format!("/rest/getCoverArt?{AUTH}&id=mf-1_67a0&size={size}"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{size}");
        assert_eq!(reply.header("content-type"), Some("image/jpeg"), "{size}");
        assert_eq!(cover_image::measure(&reply.body), Some((side, side)), "{size}");
    }
    let full = get(&app, &format!("/rest/getCoverArt?{AUTH}&id=mf-1_67a0&size=1200")).await;
    assert_eq!(
        full.body.as_ref(),
        original.as_slice(),
        "the full size as Navidrome sent it"
    );

    let calls = cover_calls(&server).await;
    assert_eq!(calls.len(), 1, "one fetch: {calls:?}");
    assert_eq!(calls[0].get("size").map(String::as_str), Some("1200"));
    assert_eq!(calls[0].get("id").map(String::as_str), Some("mf-1_67a0"));
}

#[tokio::test]
async fn concurrent_requests_for_one_cover_wait_on_one_fetch() {
    let server = navidrome(dated(jpeg(1200)).set_delay(Duration::from_millis(300))).await;
    let (state, _dir) = cached_state(&server);
    let app = app(&state);

    let requests = (0..8).map(|i| {
        let app = app.clone();
        let size = if i % 2 == 0 { 300 } else { 600 };
        async move { get(&app, &format!("/rest/getCoverArt?{AUTH}&id=al-7_aa&size={size}")).await }
    });
    let replies = futures::future::join_all(requests).await;
    for reply in &replies {
        assert_eq!(reply.status, StatusCode::OK);
        assert!(cover_image::measure(&reply.body).is_some(), "a picture");
    }
    assert_eq!(cover_calls(&server).await.len(), 1);
}

#[tokio::test]
async fn navidromes_stand_in_for_missing_art_is_relayed_and_never_kept() {
    // Navidrome's stand-in comes without a Last-Modified header.
    let stand_in = ResponseTemplate::new(200).set_body_raw(vec![1, 2, 3], "image/webp");
    let server = navidrome(stand_in).await;
    let (state, _dir) = cached_state(&server);
    let app = app(&state);

    for _ in 0..2 {
        let reply = get(&app, &format!("/rest/getCoverArt?{AUTH}&id=mf-2_bb&size=300")).await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.header("content-type"), Some("image/webp"));
        assert_eq!(reply.body.as_ref(), [1, 2, 3]);
    }
    assert_eq!(state.local_cover_cache.kept_count(), 0);
    // Each request asked for the picture to keep, then for itself.
    let calls = cover_calls(&server).await;
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[1].get("size").map(String::as_str), Some("300"));
}

#[tokio::test]
async fn a_failed_fetch_is_not_kept_and_is_tried_again() {
    let server = navidrome(ResponseTemplate::new(500)).await;
    let (state, _dir) = cached_state(&server);
    let app = app(&state);

    let reply = get(&app, &format!("/rest/getCoverArt?{AUTH}&id=mf-3_cc&size=300")).await;
    // The unbranded placeholder, as for any relay failure.
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-type"), Some("image/jpeg"));
    assert_eq!(state.local_cover_cache.kept_count(), 0);
    let before = cover_calls(&server).await.len();

    get(&app, &format!("/rest/getCoverArt?{AUTH}&id=mf-3_cc&size=300")).await;
    assert!(cover_calls(&server).await.len() > before, "asked again");
}

#[tokio::test]
async fn a_refused_sign_in_never_gets_a_kept_cover() {
    let server = navidrome(dated(jpeg(1200))).await;
    let (state, _dir) = cached_state(&server);
    let app = app(&state);

    let kept = get(&app, &format!("/rest/getCoverArt?{AUTH}&id=al-4_dd&size=300")).await;
    assert!(cover_image::measure(&kept.body).is_some());

    let refused = get(
        &app,
        "/rest/getCoverArt?u=alice&t=bad&s=salt&v=1.16.1&c=test&id=al-4_dd&size=300",
    )
    .await;
    assert!(cover_image::measure(&refused.body).is_none(), "no picture");
    assert!(refused.text().contains("Wrong username or password"));
    let calls = cover_calls(&server).await;
    assert_eq!(calls.len(), 2, "Navidrome answered the refused request itself");
    assert_eq!(calls[1].get("t").map(String::as_str), Some("bad"));
}

#[tokio::test]
async fn requests_the_cache_cannot_answer_are_relayed_unchanged() {
    let server = navidrome(dated(jpeg(1200))).await;
    let (state, _dir) = cached_state(&server);
    let app = app(&state);

    // No size: the original. square: Navidrome's own crop. Larger than the kept copy.
    for query in ["", "&size=300&square=true", "&size=2000"] {
        let reply = get(&app, &format!("/rest/getCoverArt?{AUTH}&id=al-5_ee{query}")).await;
        assert_eq!(reply.status, StatusCode::OK, "{query}");
    }
    let calls = cover_calls(&server).await;
    let sizes: Vec<Option<&str>> = calls.iter().map(|c| c.get("size").map(String::as_str)).collect();
    // The larger size first fetched the copy to keep, then asked for itself.
    assert_eq!(sizes, [None, Some("300"), Some("1200"), Some("2000")]);
    assert_eq!(calls[1].get("square").map(String::as_str), Some("true"));
}

#[tokio::test]
async fn a_shuffle_warms_its_first_covers() {
    let server = navidrome(dated(jpeg(1200))).await;
    Mock::given(path("/rest/getRandomSongs"))
        .respond_with(navidrome_json(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","randomSongs":{"song":[
                {"id":"1","coverArt":"mf-1_aa"},{"id":"2","coverArt":"al-2_bb"}]}}}"#,
        ))
        .mount(&server)
        .await;
    let (state, _dir) = cached_state(&server);
    let app = app(&state);

    let shuffle = get(&app, &format!("/rest/getRandomSongs?{AUTH}&f=json&size=2")).await;
    assert_eq!(shuffle.status, StatusCode::OK);
    let cache = &state.local_cover_cache;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !(cache.is_kept("mf-1_aa") && cache.is_kept("al-2_bb")) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both covers warmed");

    let reply = get(&app, &format!("/rest/getCoverArt?{AUTH}&id=mf-1_aa&size=300")).await;
    assert_eq!(cover_image::measure(&reply.body), Some((300, 300)));
    let calls = cover_calls(&server).await;
    assert_eq!(calls.len(), 2, "warmed, then answered from the cache");
    assert!(
        calls
            .iter()
            .all(|c| c.get("size").map(String::as_str) == Some("1200"))
    );
    assert!(
        calls
            .iter()
            .all(|c| c.get("u").map(String::as_str) == Some("alice"))
    );
}
