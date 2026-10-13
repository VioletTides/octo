use std::time::Duration;

use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

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

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/html")
}

async fn apple() -> (MockServer, AppleMusicPages) {
    let server = MockServer::builder().start().await;
    let pages = AppleMusicPages::with_base_url(&server.uri(), Duration::ZERO);
    (server, pages)
}

async fn asked(server: &MockServer) -> usize {
    server.received_requests().await.map_or(0, |r| r.len())
}

#[tokio::test]
async fn reads_a_genre_chart() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .and(path("/us/new/top-charts/songs/"))
        .and(query_param("genreId", "18"))
        .respond_with(html(fixture!("us_18")))
        .mount(&server)
        .await;

    let page = pages.chart(Some("us"), "18").await.unwrap();

    assert_eq!(page.title, "Top Hip-Hop/Rap Songs");
    assert_eq!(page.songs[0].title, "Janice STFU");
    let request = &server.received_requests().await.unwrap()[0];
    assert!(
        request
            .headers
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("Octo/")
    );
}

#[tokio::test]
async fn the_overall_has_no_genre_id_and_brings_the_countries() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .and(path("/us/new/top-charts/songs"))
        .and(query_param_is_missing("genreId"))
        .respond_with(html(fixture!("us_all")))
        .mount(&server)
        .await;

    let page = pages.chart(Some("us"), "34").await.unwrap();

    assert_eq!(page.genre_id.as_deref(), Some("34"));
    assert_eq!(pages.countries().len(), 172);
}

#[tokio::test]
async fn before_any_page_the_countries_are_the_saved_list() {
    let (_server, pages) = apple().await;
    assert_eq!(pages.countries().len(), 172);
}

#[tokio::test]
async fn a_missing_page_is_not_asked_again_for_an_hour() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .and(path("/gb/new/top-charts/songs/"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    assert_eq!(
        pages.chart(Some("gb"), "12").await.unwrap_err(),
        PageError::NoSuchPage
    );
    assert_eq!(
        pages.chart(Some("gb"), "12").await.unwrap_err(),
        PageError::NoSuchPage
    );
    assert_eq!(asked(&server).await, 1);
}

#[tokio::test]
async fn a_changed_page_is_changed_shape() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .respond_with(html(fixture!("shape_changed")))
        .mount(&server)
        .await;
    assert_eq!(
        pages.chart(Some("us"), "18").await.unwrap_err(),
        PageError::ChangedShape
    );
}

#[tokio::test]
async fn another_answer_is_unavailable_and_asked_again() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    assert_eq!(
        pages.chart(Some("us"), "18").await.unwrap_err(),
        PageError::Unavailable("HTTP 503".into())
    );
    assert!(pages.chart(Some("us"), "18").await.is_err());
    assert_eq!(asked(&server).await, 2);
}

#[tokio::test]
async fn an_unreachable_apple_is_not_asked_again_for_a_while() {
    // Requests five seconds apart: a second request would wait on the gate.
    let base = format!("http://{}", crate::services::test_support::refused_address());
    let pages = AppleMusicPages::with_base_url(&base, Duration::from_secs(5));

    let first = pages.detected_country().await;
    let started = std::time::Instant::now();
    let again = pages.chart(None, "34").await;

    assert_eq!(first, None);
    assert_eq!(again.unwrap_err(), PageError::Unavailable("no connection".into()));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "Apple was asked again"
    );
}

#[tokio::test]
async fn requests_waiting_at_the_gate_take_the_failure_before_them() {
    let base = format!("http://{}", crate::services::test_support::refused_address());
    let pages = AppleMusicPages::with_base_url(&base, Duration::from_secs(2));

    // Five asks at once: the first fails, the four behind it must not each wait their turn.
    let started = std::time::Instant::now();
    let asks = futures::future::join_all((0..5).map(|_| pages.detected_country())).await;

    assert!(asks.iter().all(Option::is_none));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "every waiting ask went to Apple"
    );
}

#[tokio::test]
async fn rooms_come_from_the_new_page_in_english_and_are_kept_an_hour() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .and(path("/us/new"))
        .and(query_param("l", "en-US"))
        .respond_with(html(fixture!("us_new")))
        .mount(&server)
        .await;

    let rooms = pages.rooms("us").await.unwrap();
    pages.rooms("us").await.unwrap();

    assert_eq!(rooms.len(), 2);
    assert_eq!(asked(&server).await, 1);
}

#[tokio::test]
async fn a_new_page_that_fails_in_english_is_read_without_it() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .and(path("/jp/new"))
        .and(query_param("l", "en-US"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/jp/new"))
        .and(query_param_is_missing("l"))
        .respond_with(html(fixture!("jp_new")))
        .mount(&server)
        .await;

    let rooms = pages.rooms("jp").await.unwrap();

    assert_eq!(rooms[0].title, "ベストニューソング");
    assert_eq!(rooms[0].room_id, "6818340711");
}

#[tokio::test]
async fn best_new_is_the_first_room_trending_the_second() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .and(path("/us/new"))
        .respond_with(html(fixture!("us_new")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/us/room/6818396128"))
        .respond_with(html(fixture!("us_room_best_new")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/us/room/6818395867"))
        .respond_with(html(fixture!("us_room_trending")))
        .mount(&server)
        .await;

    let (best_id, best) = pages.room("us", RoomKind::BestNewSongs).await.unwrap();
    let (trending_id, trending) = pages.room("us", RoomKind::TrendingSongs).await.unwrap();

    assert_eq!(
        (best_id.as_str(), best.title.as_str()),
        ("6818396128", "Best New Songs")
    );
    assert_eq!(
        (trending_id.as_str(), trending.title.as_str()),
        ("6818395867", "Trending Songs")
    );
}

#[tokio::test]
async fn a_new_page_with_one_room_has_no_trending() {
    let (server, pages) = apple().await;
    let one_room = fixture!("us_new").replacen(
        "copper-track-swoosh - 6818395867\",\"itemKind\":\"trackLockup",
        "copper-track-swoosh - 6818395867\",\"itemKind\":\"squareLockup",
        1,
    );
    assert_ne!(
        one_room,
        fixture!("us_new"),
        "the fixture's second song shelf was found"
    );
    Mock::given(method("GET"))
        .and(path("/us/new"))
        .respond_with(html(&one_room))
        .mount(&server)
        .await;

    assert_eq!(
        pages.room("us", RoomKind::TrendingSongs).await.unwrap_err(),
        PageError::NoRoom
    );
}

#[tokio::test]
async fn detection_reads_the_country_and_is_cached() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .and(path("/new/top-charts/songs"))
        .respond_with(html(fixture!("us_all")))
        .mount(&server)
        .await;

    assert_eq!(pages.detected_country().await.as_deref(), Some("us"));
    assert_eq!(pages.detected_country().await.as_deref(), Some("us"));
    // The overall page read to find the country serves the country's overall chart too.
    let overall = pages.chart(Some("us"), "34").await.unwrap();
    assert_eq!(overall.songs[0].title, "Solar Eclipse");
    assert_eq!(asked(&server).await, 1);
}

#[tokio::test]
async fn a_country_apple_names_oddly_is_not_used() {
    let (server, pages) = apple().await;
    let odd = fixture!("us_all").replace("\"storefront\":\"us\"", "\"storefront\":\"u/\"");
    Mock::given(method("GET"))
        .and(path("/new/top-charts/songs"))
        .respond_with(html(&odd))
        .mount(&server)
        .await;

    assert_eq!(pages.detected_country().await, None);
}

#[tokio::test]
async fn a_bad_country_is_never_put_in_a_url() {
    let (server, pages) = apple().await;
    assert!(pages.chart(Some("../"), "18").await.is_err());
    assert!(pages.rooms("x/").await.is_err());
    assert_eq!(asked(&server).await, 0);
}

#[tokio::test]
async fn a_page_over_eight_megabytes_is_refused() {
    let (server, pages) = apple().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![b'a'; MAX_BODY + 1], "text/html"))
        .mount(&server)
        .await;
    assert_eq!(
        pages.chart(Some("us"), "18").await.unwrap_err(),
        PageError::TooBig
    );
}

#[tokio::test(start_paused = true)]
async fn the_gate_spaces_requests_a_second() {
    let gate = Gate::new(Duration::from_secs(1));
    let start = Instant::now();
    gate.wait().await;
    gate.wait().await;
    gate.wait().await;
    assert_eq!(Instant::now() - start, Duration::from_secs(2));
}
