//! Removing the Navidrome rows of songs deleted for good, against a stand-in Navidrome.

use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::services::family::navidrome_admin::tests::admin_for;

/// A Navidrome of its own that signs the admin in and keeps `missing` as missing songs. Any
/// other id answers 404, as Navidrome does for a song that is not missing.
async fn navidrome(missing: &[Value]) -> MockServer {
    let server = MockServer::builder().start().await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": "jwt-1", "isAdmin": true, "username": "admin",
            "subsonicToken": "tok", "subsonicSalt": "salt"
        })))
        .mount(&server)
        .await;
    for song in missing {
        let id = song["id"].as_str().expect("id");
        Mock::given(method("GET"))
            .and(path(format!("/api/missing/{id}")))
            .and(header("X-Nd-Authorization", "Bearer jwt-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(song.clone()))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/missing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ids": []})))
        .mount(&server)
        .await;
    server
}

fn rows_for(url: &str) -> MissingRows {
    let (admin, _, _) = admin_for(url);
    MissingRows::new(Arc::new(admin))
}

/// Every removal call the server saw, as its list of ids.
async fn removals(server: &MockServer) -> Vec<Vec<String>> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .map(|r| {
            r.url
                .query_pairs()
                .filter(|(key, _)| key == "id")
                .map(|(_, value)| value.into_owned())
                .collect()
        })
        .collect()
}

#[test]
fn a_row_with_plays_a_favorite_a_rating_or_a_bookmark_carries_history() {
    assert!(carries_history(&json!({"id": "a", "playCount": 6})));
    assert!(carries_history(&json!({"id": "a", "starred": true})));
    assert!(carries_history(&json!({"id": "a", "rating": 4})));
    assert!(carries_history(&json!({"id": "a", "bookmarkPosition": 74000})));
    assert!(carries_history(&json!({"id": "a", "playCount": "2"})));
}

#[test]
fn a_bare_row_carries_no_history() {
    assert!(!carries_history(&json!({"id": "a"})));
    assert!(!carries_history(
        &json!({"id": "a", "playCount": 0, "starred": false, "rating": 0, "missing": true})
    ));
}

/// The bare row goes, the played one stays for a person to decide, and the one Navidrome no
/// longer keeps as missing (its file came back) is never part of the removal.
#[tokio::test]
async fn forget_removes_only_bare_rows_navidrome_still_keeps_as_missing() {
    let server = navidrome(&[
        json!({"id": "bare", "path": "A - Song.flac", "missing": true}),
        json!({"id": "played", "path": "B - Song.flac", "missing": true, "playCount": 6}),
    ])
    .await;
    let rows = rows_for(&server.uri());

    let done = rows
        .forget(vec!["bare".into(), "played".into(), "back".into()])
        .await;

    assert_eq!(done.removed, ["bare"]);
    assert_eq!(done.kept, ["played"]);
    assert!(done.waiting.is_empty());
    assert_eq!(removals(&server).await, [vec!["bare".to_string()]]);
    assert!(rows.waiting().is_empty());
}

/// Navidrome reads a removal without ids as every missing song, so with nothing to ask about
/// nothing is sent at all.
#[tokio::test]
async fn forget_with_nothing_to_ask_sends_nothing() {
    let server = navidrome(&[]).await;
    let rows = rows_for(&server.uri());

    let done = rows.forget(vec![" ".into(), String::new()]).await;

    assert_eq!(done, Forgotten::default());
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
}

/// When every id turns out to be back or kept, the removal call is never made.
#[tokio::test]
async fn forget_never_sends_an_empty_removal() {
    let server = navidrome(&[json!({"id": "starred", "missing": true, "starred": true})]).await;
    let rows = rows_for(&server.uri());

    let done = rows.forget(vec!["starred".into(), "back".into()]).await;

    assert_eq!(done.kept, ["starred"]);
    assert!(removals(&server).await.is_empty());
}

/// A Navidrome that cannot answer leaves the id waiting, and the next pass asks again.
#[tokio::test]
async fn an_id_navidrome_could_not_answer_for_waits_for_the_next_pass() {
    let server = MockServer::builder().start().await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": "jwt-1", "isAdmin": true, "username": "admin"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/missing/gone"))
        .respond_with(ResponseTemplate::new(500).set_body_string("busy"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/missing/gone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "gone", "missing": true})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/missing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ids": ["gone"]})))
        .mount(&server)
        .await;
    let rows = rows_for(&server.uri());

    let first = rows.forget(vec!["gone".into()]).await;
    assert_eq!(first.waiting, ["gone"]);
    assert_eq!(rows.waiting(), ["gone"]);

    let second = rows.forget(Vec::new()).await;
    assert_eq!(second.removed, ["gone"]);
    assert!(rows.waiting().is_empty());
    assert_eq!(removals(&server).await, [vec!["gone".to_string()]]);
}

/// A long list goes in calls of at most fifty ids each.
#[tokio::test]
async fn many_ids_go_in_calls_of_fifty() {
    let songs: Vec<Value> = (0..120)
        .map(|n| json!({"id": format!("song-{n}"), "missing": true}))
        .collect();
    let server = navidrome(&songs).await;
    let rows = rows_for(&server.uri());

    let done = rows.forget((0..120).map(|n| format!("song-{n}")).collect()).await;

    assert_eq!(done.removed.len(), 120);
    let sizes: Vec<usize> = removals(&server).await.iter().map(Vec::len).collect();
    assert_eq!(sizes, [50, 50, 20]);
}
