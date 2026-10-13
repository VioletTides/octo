use std::sync::Arc;

use octo_core::settings::{AppSettings, LastFmSettings, SettingsStore};
use wiremock::matchers::{method, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

async fn last_fm(api_key: &str) -> (MockServer, LastFmService) {
    let server = MockServer::builder().start().await;
    let settings = Arc::new(SettingsStore::for_tests(AppSettings {
        last_fm: LastFmSettings {
            api_key: api_key.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }));
    let service = LastFmService::with_base_url(settings, &format!("{}/2.0/", server.uri()));
    (server, service)
}

const CHART: &str = r#"{"tracks":{"track":[
  {"name":"Dracula","playcount":"1200345","listeners":"98000","artist":{"name":"Tame Impala"}},
  {"name":"","playcount":"5","artist":{"name":"Nobody"}},
  {"name":"Choosin' Texas","playcount":"-3","listeners":"x","artist":{"name":"Ella Langley"}}
]}}"#;

#[tokio::test]
async fn the_chart_reads_artist_title_and_counts() {
    let (server, service) = last_fm("key").await;
    Mock::given(method("GET"))
        .and(query_param("method", "chart.gettoptracks"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_string(CHART))
        .mount(&server)
        .await;

    let chart = service.get_chart(50).await.unwrap();

    assert_eq!(chart.len(), 2);
    assert_eq!(chart[0].artist, "Tame Impala");
    assert_eq!(chart[0].title, "Dracula");
    assert_eq!(chart[0].plays, Some(1_200_345));
    assert_eq!(chart[0].listeners, Some(98_000));
    assert_eq!((chart[1].plays, chart[1].listeners), (None, None));
}

#[tokio::test]
async fn without_a_key_there_is_no_chart_and_nothing_is_asked() {
    let (server, service) = last_fm("").await;
    assert!(service.get_chart(50).await.is_none());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_error_answer_is_none_and_an_empty_one_is_empty() {
    let (server, service) = last_fm("key").await;
    Mock::given(method("GET"))
        .and(query_param("method", "chart.gettoptracks"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"error":29,"message":"Rate limit"}"#))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("method", "artist.gettoptracks"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"toptracks":{}}"#))
        .mount(&server)
        .await;

    assert!(service.get_chart(50).await.is_none());
    assert_eq!(service.get_artist_chart("Daft Punk", 20).await, Some(Vec::new()));
}

#[tokio::test]
async fn an_artists_chart_names_the_artist_when_a_row_does_not() {
    let (server, service) = last_fm("key").await;
    Mock::given(method("GET"))
        .and(query_param("method", "artist.gettoptracks"))
        .and(query_param("autocorrect", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"toptracks":{"track":[{"name":"One More Time","playcount":"10"}]}}"#),
        )
        .mount(&server)
        .await;

    let top = service.get_artist_chart("Daft Punk", 20).await.unwrap();

    assert_eq!(top[0].artist, "Daft Punk");
    assert!(service.get_artist_chart("  ", 20).await.unwrap().is_empty());
}
