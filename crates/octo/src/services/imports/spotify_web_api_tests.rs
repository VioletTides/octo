//! Imports through Spotify's Web API, against a local stand-in for Spotify.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("a date")
        .with_timezone(&Utc)
}

struct Rig {
    server: MockServer,
    api: SpotifyWebApi,
    waits: Arc<Mutex<Vec<Duration>>>,
}

async fn rig() -> Rig {
    let server = MockServer::builder().start().await;
    let api = SpotifyWebApi::with_urls(
        SpotifyWebApi::client(),
        &format!("{}/v1", server.uri()),
        &format!("{}/api/token", server.uri()),
    );
    let waits = Arc::new(Mutex::new(Vec::new()));
    let seen = waits.clone();
    api.set_wait(Arc::new(move |wait| {
        seen.lock().push(wait);
        futures::future::ready(()).boxed()
    }));
    api.set_clock(Clock::fixed(at("2026-10-04T12:00:00Z")));
    Rig { server, api, waits }
}

fn json(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

#[tokio::test]
async fn the_code_is_exchanged_with_the_verifier_and_no_secret() {
    let rig = rig().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(json(
            200,
            r#"{"access_token":"at","refresh_token":"rt","expires_in":3600,"token_type":"Bearer"}"#,
        ))
        .mount(&rig.server)
        .await;
    let tokens = rig
        .api
        .exchange(
            "client-1",
            "the-code",
            "the-verifier",
            "http://127.0.0.1/callback",
        )
        .await
        .expect("tokens");
    assert_eq!(tokens.access_token, "at");
    assert_eq!(tokens.refresh_token.as_deref(), Some("rt"));
    assert_eq!(tokens.expires_utc, at("2026-10-04T13:00:00Z"));
    let calls = rig.server.received_requests().await.expect("recording");
    assert_eq!(calls.len(), 1);
    let form: Vec<(String, String)> = url::form_urlencoded::parse(&calls[0].body)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let get = |name: &str| form.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    assert_eq!(get("grant_type"), Some("authorization_code"));
    assert_eq!(get("code_verifier"), Some("the-verifier"));
    assert_eq!(get("client_id"), Some("client-1"));
    assert_eq!(get("client_secret"), None);
    assert!(!calls[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn a_revoked_or_six_month_old_sign_in_says_to_connect_again() {
    let rig = rig().await;
    Mock::given(method("POST"))
        .respond_with(json(
            400,
            r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#,
        ))
        .mount(&rig.server)
        .await;
    let error = rig.api.refresh("c", "rt").await.expect_err("ended");
    let spotify = error
        .downcast_ref::<SpotifyException>()
        .expect("a Spotify refusal");
    assert!(spotify.sign_in_ended);
    assert!(spotify.message.contains("Connect again"));
}

#[tokio::test]
async fn liked_songs_are_paged_fifty_and_read_under_item_or_track() {
    let rig = rig().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/tracks"))
        .and(query_param("limit", "50"))
        .and(query_param("offset", "0"))
        .respond_with(json(
            200,
            r#"{"total":3,"next":"https://api.spotify.com/v1/me/tracks?offset=2","items":[
              {"track":{"type":"track","id":"4uLU6hMCjMI75M1A2tKUQC","name":"Never Gonna Give You Up","duration_ms":213573,
                "artists":[{"name":"Rick Astley"}],"album":{"name":"Whenever You Need Somebody"},"external_ids":{"isrc":"gbarl9300135"}}},
              {"track":{"type":"episode","id":"ep","name":"A podcast"}}]}"#,
        ))
        .mount(&rig.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me/tracks"))
        .and(query_param("limit", "50"))
        .and(query_param("offset", "2"))
        .respond_with(json(
            200,
            r#"{"total":3,"next":null,"items":[
              {"item":{"type":"track","id":"7ouMYWpwJ422jRcDASZB7P","name":"Teardrop","duration_ms":330000,
                "artists":[{"name":"Massive Attack"},{"name":"Elizabeth Fraser"}],"album":{"name":"Mezzanine"}}}]}"#,
        ))
        .mount(&rig.server)
        .await;
    let tracks = rig.api.liked_songs("at", None).await.expect("read");
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].key, "spotify:4uLU6hMCjMI75M1A2tKUQC");
    assert_eq!(tracks[0].isrc.as_deref(), Some("GBARL9300135"));
    assert_eq!(tracks[0].seconds, Some(214));
    assert_eq!(tracks[1].artist, "Massive Attack, Elizabeth Fraser");
    assert_eq!(tracks[1].isrc, None);
    for call in rig.server.received_requests().await.expect("recording") {
        assert_eq!(
            call.headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer at")
        );
    }
}

#[tokio::test]
async fn playlists_read_their_total_from_items_or_tracks() {
    let rig = rig().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/playlists"))
        .respond_with(json(
            200,
            r#"{"next":null,"items":[
              {"id":"p1","name":"Mine","owner":{"id":"me","display_name":"Me"},"collaborative":false,"snapshot_id":"s1","items":{"total":12},
               "images":[{"url":"https://i.scdn.co/image/a"}]},
              {"id":"p2","name":"Theirs","owner":{"id":"them"},"collaborative":true,"tracks":{"total":7}}]}"#,
        ))
        .mount(&rig.server)
        .await;
    let lists = rig.api.playlists("at").await.expect("read");
    assert_eq!(lists[0].total, 12);
    assert_eq!(lists[0].image_url.as_deref(), Some("https://i.scdn.co/image/a"));
    assert_eq!(lists[1].total, 7);
    assert!(lists[1].collaborative);
    assert_eq!(lists[1].owner_name.as_deref(), Some("them"));
}

#[tokio::test]
async fn someone_elses_playlist_is_hidden_not_an_error() {
    let rig = rig().await;
    Mock::given(method("GET"))
        .respond_with(json(403, r#"{"error":{"status":403,"message":"Forbidden"}}"#))
        .mount(&rig.server)
        .await;
    let items = rig.api.playlist_items("at", "p2", None).await.expect("read");
    assert!(items.hidden);
    assert!(items.tracks.is_empty());
    let calls = rig.server.received_requests().await.expect("recording");
    assert!(calls[0].url.path().ends_with("/playlists/p2/items"));
}

#[tokio::test]
async fn a_too_many_requests_waits_as_long_as_spotify_asks() {
    let rig = rig().await;
    Mock::given(method("GET"))
        .respond_with(json(429, "{}").insert_header("Retry-After", "7"))
        .up_to_n_times(1)
        .mount(&rig.server)
        .await;
    Mock::given(method("GET"))
        .respond_with(json(200, r#"{"total":0,"next":null,"items":[]}"#))
        .mount(&rig.server)
        .await;
    rig.api.liked_songs("at", None).await.expect("read");
    assert_eq!(*rig.waits.lock(), [Duration::from_secs(7)]);
}

#[tokio::test]
async fn a_used_up_quota_stops_at_once() {
    let rig = rig().await;
    Mock::given(method("GET"))
        .respond_with(json(
            429,
            r#"{"error":{"status":429,"message":"Too many","reason":"QUOTA_EXCEEDED"}}"#,
        ))
        .mount(&rig.server)
        .await;
    let error = rig.api.liked_songs("at", None).await.expect_err("refused");
    assert!(error.to_string().contains("used up"), "{error}");
    assert!(rig.waits.lock().is_empty());
}

#[tokio::test]
async fn an_account_not_on_the_apps_list_is_told_where_to_add_it() {
    let rig = rig().await;
    Mock::given(method("GET"))
        .respond_with(json(
            403,
            r#"{"error":{"status":403,"message":"User not registered in the Developer Dashboard"}}"#,
        ))
        .mount(&rig.server)
        .await;
    let error = rig.api.profile("at").await.expect_err("refused");
    assert!(error.to_string().contains("User Management"), "{error}");
}

#[test]
fn keys_come_from_spotify_ids_or_the_songs_words() {
    assert_eq!(
        ImportKeys::for_spotify(Some("spotify:track:4uLU6hMCjMI75M1A2tKUQC")).as_deref(),
        Some("spotify:4uLU6hMCjMI75M1A2tKUQC")
    );
    assert_eq!(
        ImportKeys::for_spotify(Some("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=x"))
            .as_deref(),
        Some("spotify:4uLU6hMCjMI75M1A2tKUQC")
    );
    assert_eq!(ImportKeys::for_spotify(Some("short")), None);
    assert_eq!(ImportKeys::for_spotify(None), None);
    assert_eq!(
        ImportKeys::for_words(Some("Drake, Rihanna"), Some("Too Good")),
        ImportKeys::for_words(Some("Drake feat. Rihanna"), Some("Too Good"))
    );
}
