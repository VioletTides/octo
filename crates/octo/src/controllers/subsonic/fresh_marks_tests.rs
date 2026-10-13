//! Lists drawn earlier show the listener's hearts and ratings as they are now: a heart given
//! after a list was made shows on it at once, an unheart takes it off, and a rating set through
//! Octo shows too. Hearts are read from Navidrome at most once a minute.

use std::time::Duration;

use octo_core::settings::{AppSettings, SubsonicSettings};
use serde_json::Value;

use super::test_app::{app, get};
use crate::app::{AppState, TestServices};
use crate::http::pipeline::App;
use crate::services::library::for_you_lists::tests::ForYouUpstream;

pub(crate) const AUTH: &str = "u=alice&t=token&s=salt&v=1.16.1&c=test";

pub(crate) async fn setup() -> (std::sync::Arc<ForYouUpstream>, App) {
    setup_with(|_| {}).await
}

pub(crate) async fn setup_with(
    change: impl FnOnce(&mut AppSettings),
) -> (std::sync::Arc<ForYouUpstream>, App) {
    let upstream = ForYouUpstream::start().await;
    let mut settings = AppSettings {
        subsonic: SubsonicSettings {
            url: Some(upstream.navidrome_url()),
            auto_detect_download_path: false,
            ..Default::default()
        },
        ..Default::default()
    };
    change(&mut settings);
    let state = AppState::for_tests_with(
        settings,
        TestServices {
            deezer_base_url: Some(upstream.deezer_url()),
            ..Default::default()
        },
    );
    let app = app(&state);
    (upstream, app)
}

/// The playlist rows of a getPlaylists answer.
pub(crate) async fn playlists(app: &App) -> Vec<Value> {
    get(app, &format!("/rest/getPlaylists.view?{AUTH}&f=json"))
        .await
        .json()["subsonic-response"]["playlists"]["playlist"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The id of a Made for you list, once it has been made.
async fn list_id(app: &App, kind: &str) -> String {
    for _ in 0..100 {
        if let Some(row) = playlists(app)
            .await
            .into_iter()
            .find(|row| row["octoList"] == kind)
        {
            return row["id"].as_str().expect("an id").to_string();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{kind} was never made");
}

pub(crate) async fn entries(app: &App, id: &str) -> Vec<Value> {
    get(app, &format!("/rest/getPlaylist.view?{AUTH}&f=json&id={id}"))
        .await
        .json()["subsonic-response"]["playlist"]["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn entry<'a>(entries: &'a [Value], id: &str) -> &'a Value {
    entries.iter().find(|e| e["id"] == id).expect("the song")
}

/// A library song of the list.
fn library_song(entries: &[Value]) -> String {
    entries
        .iter()
        .find(|e| e.get("isExternal").and_then(Value::as_bool) != Some(true))
        .and_then(|e| e["id"].as_str())
        .expect("a library song")
        .to_string()
}

#[tokio::test]
async fn a_heart_given_after_the_list_was_made_shows_on_it_and_an_unheart_takes_it_off() {
    let (upstream, app) = setup().await;
    let id = list_id(&app, "deepCuts").await;
    let before = entries(&app, &id).await;
    let song = library_song(&before);
    assert!(entry(&before, &song).get("starred").is_none());

    let starred = get(&app, &format!("/rest/star.view?{AUTH}&f=json&id={song}")).await;
    assert!(starred.status.is_success());
    let after = entries(&app, &id).await;
    assert!(
        entry(&after, &song)["starred"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "{:?}",
        entry(&after, &song)
    );

    let unstarred = get(&app, &format!("/rest/unstar.view?{AUTH}&f=json&id={song}")).await;
    assert!(unstarred.status.is_success());
    let again = entries(&app, &id).await;
    assert!(entry(&again, &song).get("starred").is_none());
    assert!(upstream.hearts.lock().is_empty());
}

#[tokio::test]
async fn hearts_are_read_once_a_minute_not_on_every_open() {
    let (upstream, app) = setup().await;
    let id = list_id(&app, "deepCuts").await;
    entries(&app, &id).await;
    let read = upstream.calls_ending("getStarred2");
    assert!(read >= 1);

    entries(&app, &id).await;
    entries(&app, &id).await;

    assert_eq!(upstream.calls_ending("getStarred2"), read);
}

#[tokio::test]
async fn a_rating_set_through_octo_shows_on_the_list() {
    let (_upstream, app) = setup().await;
    let id = list_id(&app, "deepCuts").await;
    let song = library_song(&entries(&app, &id).await);

    let rated = get(
        &app,
        &format!("/rest/setRating.view?{AUTH}&f=json&id={song}&rating=4"),
    )
    .await;
    assert!(rated.status.is_success());

    assert_eq!(entry(&entries(&app, &id).await, &song)["userRating"], 4);
}

#[tokio::test]
async fn a_heart_on_navidrome_shows_once_the_minute_is_up() {
    let (upstream, app) = setup().await;
    let id = list_id(&app, "deepCuts").await;
    let song = library_song(&entries(&app, &id).await);
    // Hearted in another app, straight on Navidrome: Octo hears of it on its next read.
    upstream.hearts.lock().push((
        "alice".into(),
        song.clone(),
        "2026-10-09T10:00:00.000Z".to_string(),
    ));

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();

    assert_eq!(
        entry(&entries(&app, &id).await, &song)["starred"],
        "2026-10-09T10:00:00.000Z"
    );
}

// ---- Liked Songs --------------------------------------------------------------------------

const BOB: &str = "u=bob&t=token&s=salt&v=1.16.1&c=test";

fn heart(upstream: &ForYouUpstream, user: &str, id: &str, at: &str) {
    upstream
        .hearts
        .lock()
        .push((user.to_string(), id.to_string(), at.to_string()));
}

#[tokio::test]
async fn liked_songs_is_listed_first_in_json_and_xml() {
    let (upstream, app) = setup().await;
    heart(&upstream, "alice", "big1", "2026-10-01T10:00:00.000Z");
    heart(&upstream, "alice", "big2", "2026-10-02T10:00:00.000Z");

    let rows = playlists(&app).await;
    let xml = get(&app, &format!("/rest/getPlaylists.view?{AUTH}")).await.text();

    let first = rows.first().expect("a row");
    assert_eq!(first["name"], "Liked Songs");
    assert_eq!(first["octoList"], "liked");
    assert_eq!(first["songCount"], 2);
    assert_eq!(first["readonly"], true);
    let version = octo_media::cover::CoverBook::shipped().version;
    let id = first["id"].as_str().expect("an id");
    assert_eq!(first["coverArt"], format!("{id}_{version}c2"));
    let first_row = xml.find("<playlist ").expect("a row");
    assert!(xml[first_row..].starts_with("<playlist ") && xml[first_row..].contains(r#"octoList="liked""#));
    let liked_at = xml.find(r#"octoList="liked""#).expect("liked");
    assert!(
        xml[first_row..liked_at].matches("<playlist ").count() == 1,
        "Liked Songs is the first row: {xml}"
    );
}

#[tokio::test]
async fn liked_songs_holds_the_newest_heart_first() {
    let (upstream, app) = setup().await;
    heart(&upstream, "alice", "big1", "2026-10-01T10:00:00.000Z");
    heart(&upstream, "alice", "big3", "2026-10-03T10:00:00.000Z");
    heart(&upstream, "alice", "big2", "2026-10-02T10:00:00.000Z");
    let id = playlists(&app).await[0]["id"]
        .as_str()
        .expect("an id")
        .to_string();

    let songs = entries(&app, &id).await;

    let ids: Vec<&str> = songs.iter().filter_map(|s| s["id"].as_str()).collect();
    assert_eq!(ids, ["big3", "big2", "big1"]);
    assert!(songs.iter().all(|s| s["starred"].as_str().is_some()));
}

#[tokio::test]
async fn liked_songs_takes_the_name_it_is_given() {
    let (upstream, app) = setup_with(|settings| {
        settings.generated_playlists.liked_songs_name = "* Liked Songs".into();
    })
    .await;
    heart(&upstream, "alice", "big1", "2026-10-01T10:00:00.000Z");

    assert_eq!(playlists(&app).await[0]["name"], "* Liked Songs");
}

#[tokio::test]
async fn liked_songs_switched_off_or_with_no_hearts_is_not_listed() {
    let (upstream, app) = setup_with(|settings| settings.generated_playlists.liked_songs = false).await;
    heart(&upstream, "alice", "big1", "2026-10-01T10:00:00.000Z");
    assert!(!playlists(&app).await.iter().any(|row| row["octoList"] == "liked"));

    let (_upstream, app) = setup().await;
    assert!(!playlists(&app).await.iter().any(|row| row["octoList"] == "liked"));
}

#[tokio::test]
async fn each_listener_sees_only_their_own_hearts() {
    let (upstream, app) = setup().await;
    heart(&upstream, "alice", "big1", "2026-10-01T10:00:00.000Z");
    heart(&upstream, "bob", "big2", "2026-10-02T10:00:00.000Z");
    heart(&upstream, "bob", "big3", "2026-10-03T10:00:00.000Z");

    let alices = playlists(&app).await;
    let bobs: Vec<Value> = get(&app, &format!("/rest/getPlaylists.view?{BOB}&f=json"))
        .await
        .json()["subsonic-response"]["playlists"]["playlist"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    assert_eq!(alices[0]["songCount"], 1);
    assert_eq!(bobs[0]["songCount"], 2);
    assert_ne!(alices[0]["id"], bobs[0]["id"]);
    let bobs_songs = get(
        &app,
        &format!(
            "/rest/getPlaylist.view?{BOB}&f=json&id={}",
            bobs[0]["id"].as_str().unwrap()
        ),
    )
    .await
    .json();
    let ids: Vec<&str> = bobs_songs["subsonic-response"]["playlist"]["entry"]
        .as_array()
        .map(|e| e.iter().filter_map(|s| s["id"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(ids, ["big3", "big2"]);
}

#[tokio::test]
async fn liked_songs_has_a_heart_cover() {
    let (upstream, app) = setup().await;
    heart(&upstream, "alice", "big1", "2026-10-01T10:00:00.000Z");
    let id = playlists(&app).await[0]["coverArt"]
        .as_str()
        .expect("a cover id")
        .to_string();

    let cover = get(&app, &format!("/rest/getCoverArt.view?{AUTH}&id={id}&size=600")).await;

    assert!(cover.status.is_success(), "{}", cover.status);
    assert_eq!(cover.media_type().as_deref(), Some("image/jpeg"));
    assert_eq!(&cover.body[..3], &[0xFF, 0xD8, 0xFF]);
    assert_eq!(upstream.calls_ending("getCoverArt"), 0);
}

#[tokio::test]
async fn liked_songs_never_answers_a_caller_without_a_username() {
    let (upstream, app) = setup().await;
    heart(&upstream, "alice", "big1", "2026-10-01T10:00:00.000Z");
    let blank = octo_core::library::generated_playlist_service::playlist_id("", "liked");

    let mut seen = Vec::new();
    for key in ["alice-key", "bob-key"] {
        let answer = get(
            &app,
            &format!("/rest/getPlaylist.view?apiKey={key}&v=1.16.1&c=test&f=json&id={blank}"),
        )
        .await
        .json();
        seen.extend(
            answer["subsonic-response"]["playlist"]["entry"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
    }

    assert!(seen.is_empty(), "{seen:?}");
    assert_eq!(upstream.calls_ending("getStarred2"), 0);
}

#[tokio::test]
async fn liked_songs_changed_when_the_newest_heart_was_given() {
    let (upstream, app) = setup().await;
    heart(&upstream, "alice", "big1", "2026-10-01T10:00:00.000Z");
    heart(&upstream, "alice", "big3", "2026-10-03T10:00:00.000Z");
    heart(&upstream, "alice", "big2", "2026-10-02T10:00:00.000Z");

    let row = playlists(&app).await[0].clone();
    let id = row["id"].as_str().expect("an id").to_string();
    let opened = get(&app, &format!("/rest/getPlaylist.view?{AUTH}&f=json&id={id}"))
        .await
        .json()["subsonic-response"]["playlist"]
        .clone();

    for answer in [&row, &opened] {
        assert_eq!(answer["changed"], "2026-10-03T10:00:00.000Z", "{answer}");
        assert_eq!(answer["created"], "2026-10-03T10:00:00.000Z", "{answer}");
    }
}

#[tokio::test]
async fn an_unheart_navidrome_refused_keeps_the_hearts_already_read() {
    let (upstream, app) = setup().await;
    let id = list_id(&app, "deepCuts").await;
    let song = library_song(&entries(&app, &id).await);
    let read = upstream.calls_ending("getStarred2");

    let refused = get(
        &app,
        &format!("/rest/unstar.view?u=alice&t=bad&s=salt&v=1.16.1&c=test&f=json&id={song}"),
    )
    .await;
    assert_eq!(refused.json()["subsonic-response"]["status"], "failed");
    entries(&app, &id).await;

    assert_eq!(upstream.calls_ending("getStarred2"), read);
}

#[tokio::test]
async fn an_unheart_signed_in_with_an_api_key_takes_the_heart_off_the_list() {
    let (_upstream, app) = setup().await;
    let id = list_id(&app, "deepCuts").await;
    let song = library_song(&entries(&app, &id).await);
    get(&app, &format!("/rest/star.view?{AUTH}&f=json&id={song}")).await;
    assert!(entry(&entries(&app, &id).await, &song).get("starred").is_some());

    let unstarred = get(
        &app,
        &format!("/rest/unstar.view?apiKey=alice-key&v=1.16.1&c=test&f=json&id={song}"),
    )
    .await;
    assert_eq!(unstarred.json()["subsonic-response"]["status"], "ok");

    assert!(entry(&entries(&app, &id).await, &song).get("starred").is_none());
}

#[tokio::test]
async fn a_heart_signed_in_with_an_api_key_shows_on_the_list() {
    let (_upstream, app) = setup().await;
    let id = list_id(&app, "deepCuts").await;
    let song = library_song(&entries(&app, &id).await);
    assert!(entry(&entries(&app, &id).await, &song).get("starred").is_none());

    let starred = get(
        &app,
        &format!("/rest/star.view?apiKey=alice-key&v=1.16.1&c=test&f=json&id={song}"),
    )
    .await;
    assert_eq!(starred.json()["subsonic-response"]["status"], "ok");

    assert!(
        entry(&entries(&app, &id).await, &song)["starred"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
}

#[tokio::test]
async fn a_rating_signed_in_with_an_api_key_shows_on_the_list() {
    let (_upstream, app) = setup().await;
    let id = list_id(&app, "deepCuts").await;
    let song = library_song(&entries(&app, &id).await);

    let rated = get(
        &app,
        &format!("/rest/setRating.view?apiKey=alice-key&v=1.16.1&c=test&f=json&id={song}&rating=4"),
    )
    .await;
    assert_eq!(rated.json()["subsonic-response"]["status"], "ok");

    assert_eq!(entry(&entries(&app, &id).await, &song)["userRating"], 4);
}
