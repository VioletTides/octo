//! The lists Octo marks for what they are, as clients see them: the listener's Review and
//! Duplicates playlists carry the notice's painted cover and `octoNotice`.

use std::sync::Arc;

use octo_core::settings::{AppSettings, LibraryActionSettings, NoticeKind};
use parking_lot::Mutex;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

use super::test_app::*;
use crate::app::AppState;
use crate::http::pipeline::App;

/// What the stand-in Navidrome holds: its playlists, and what it was asked.
#[derive(Default)]
pub(crate) struct Library {
    pub playlists: Vec<Value>,
    pub asked: Vec<String>,
}

pub(crate) struct Harness {
    pub app: App,
    _state: AppState,
    pub library: Arc<Mutex<Library>>,
    _navidrome: MockServer,
}

fn xml_of(rows: &[Value], one: bool) -> String {
    let attributes = |row: &Value| {
        row.as_object()
            .map(|fields| {
                fields
                    .iter()
                    .map(|(k, v)| format!(" {k}=\"{}\"", v.as_str().map_or(v.to_string(), str::to_string)))
                    .collect::<String>()
            })
            .unwrap_or_default()
    };
    let inner = if one {
        rows.first()
            .map(|row| format!("<playlist{}></playlist>", attributes(row)))
            .unwrap_or_default()
    } else {
        format!(
            "<playlists>{}</playlists>",
            rows.iter()
                .map(|row| format!("<playlist{}></playlist>", attributes(row)))
                .collect::<String>()
        )
    };
    format!(
        r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="1.16.1">{inner}</subsonic-response>"#
    )
}

async fn navidrome(library: Arc<Mutex<Library>>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(move |request: &MockRequest| {
            let query = query_of(request);
            let path = request.url.path().trim_end_matches(".view").to_string();
            library.lock().asked.push(path.clone());
            if query.get("t").map(String::as_str) != Some("good") {
                return navidrome_json(NAVIDROME_WRONG_PASSWORD);
            }
            let json = query.get("f").map(String::as_str) == Some("json");
            let library = library.lock();
            match path.as_str() {
                "/rest/getPlaylists" if json => navidrome_json(
                    &json!({"subsonic-response": {"status": "ok", "version": "1.16.1",
                        "playlists": {"playlist": library.playlists}}})
                    .to_string(),
                ),
                "/rest/getPlaylists" => ResponseTemplate::new(200)
                    .set_body_raw(xml_of(&library.playlists, false).into_bytes(), "application/xml"),
                "/rest/getPlaylist" => {
                    let id = query.get("id").cloned().unwrap_or_default();
                    let found: Vec<Value> = library
                        .playlists
                        .iter()
                        .filter(|row| row["id"] == id.as_str())
                        .cloned()
                        .collect();
                    if json {
                        navidrome_json(
                            &json!({"subsonic-response": {"status": "ok", "version": "1.16.1",
                                "playlist": found.first().cloned().unwrap_or(json!({}))}})
                            .to_string(),
                        )
                    } else {
                        ResponseTemplate::new(200)
                            .set_body_raw(xml_of(&found, true).into_bytes(), "application/xml")
                    }
                }
                _ if json => navidrome_json(NAVIDROME_OK),
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

pub(crate) async fn harness(change: impl FnOnce(&mut AppSettings)) -> Harness {
    let library = Arc::new(Mutex::new(Library::default()));
    let server = navidrome(Arc::clone(&library)).await;
    let mut settings = settings(&server.uri());
    settings.library_actions = LibraryActionSettings {
        review_enabled: true,
        duplicates_enabled: true,
        ..Default::default()
    };
    change(&mut settings);
    let state = AppState::for_tests(settings);
    Harness {
        app: app(&state),
        _state: state,
        library,
        _navidrome: server,
    }
}

const ALICE: &str = "u=alice&t=good&s=salt&v=1.16.1&c=test";

fn title(kind: NoticeKind) -> String {
    LibraryActionSettings::default().notice_title(kind)
}

fn playlist(id: &str, name: &str, owner: &str) -> Value {
    json!({"id": id, "name": name, "owner": owner, "coverArt": format!("pl-{id}"), "songCount": "3"})
}

async fn with_notices(change: impl FnOnce(&mut AppSettings)) -> Harness {
    let h = harness(change).await;
    h.library.lock().playlists = vec![
        playlist("p1", &title(NoticeKind::Review), "alice"),
        playlist("p2", &title(NoticeKind::Duplicates), "alice"),
        playlist("p3", &title(NoticeKind::Review), "bob"),
        playlist("p4", "Road Trip", "alice"),
    ];
    h
}

fn rows(reply: &Reply) -> Vec<Value> {
    reply.json()["subsonic-response"]["playlists"]["playlist"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter().find(|row| row["id"] == id).expect("the row")
}

#[tokio::test]
async fn the_listeners_notice_playlists_carry_the_notice_cover_in_json() {
    let h = with_notices(|_| {}).await;

    let rows = rows(&get(&h.app, &format!("/rest/getPlaylists.view?{ALICE}&f=json")).await);

    assert_eq!(row(&rows, "p1")["coverArt"], "oc-notice-review");
    assert_eq!(row(&rows, "p1")["octoNotice"], "review");
    assert_eq!(row(&rows, "p2")["coverArt"], "oc-notice-duplicates");
    assert_eq!(row(&rows, "p2")["octoNotice"], "duplicates");
    // Someone else's playlist of the same name, and the listener's other playlists, are theirs.
    assert_eq!(row(&rows, "p3")["coverArt"], "pl-p3");
    assert!(row(&rows, "p3").get("octoNotice").is_none());
    assert_eq!(row(&rows, "p4")["coverArt"], "pl-p4");
    assert!(row(&rows, "p4").get("octoNotice").is_none());
}

#[tokio::test]
async fn the_listeners_notice_playlists_carry_the_notice_cover_in_xml() {
    let h = with_notices(|_| {}).await;

    let xml = get(&h.app, &format!("/rest/getPlaylists.view?{ALICE}"))
        .await
        .text();

    assert!(xml.contains(r#"coverArt="oc-notice-review""#), "{xml}");
    assert!(xml.contains(r#"octoNotice="review""#), "{xml}");
    assert!(xml.contains(r#"octoNotice="duplicates""#), "{xml}");
    assert!(xml.contains(r#"coverArt="pl-p3""#), "{xml}");
    assert_eq!(xml.matches("octoNotice=").count(), 2, "{xml}");
}

#[tokio::test]
async fn one_notice_playlist_carries_it_too() {
    let h = with_notices(|_| {}).await;

    let json = get(&h.app, &format!("/rest/getPlaylist.view?{ALICE}&f=json&id=p1"))
        .await
        .json();
    let xml = get(&h.app, &format!("/rest/getPlaylist.view?{ALICE}&id=p1"))
        .await
        .text();
    let other = get(&h.app, &format!("/rest/getPlaylist.view?{ALICE}&f=json&id=p3"))
        .await
        .json();

    assert_eq!(
        json["subsonic-response"]["playlist"]["coverArt"],
        "oc-notice-review"
    );
    assert_eq!(json["subsonic-response"]["playlist"]["octoNotice"], "review");
    assert!(xml.contains(r#"octoNotice="review""#), "{xml}");
    assert_eq!(other["subsonic-response"]["playlist"]["coverArt"], "pl-p3");
}

#[tokio::test]
async fn a_notice_that_is_off_leaves_its_playlist_alone() {
    let h = with_notices(|settings| settings.library_actions.duplicates_enabled = false).await;

    let rows = rows(&get(&h.app, &format!("/rest/getPlaylists.view?{ALICE}&f=json")).await);

    assert_eq!(row(&rows, "p1")["octoNotice"], "review");
    assert_eq!(row(&rows, "p2")["coverArt"], "pl-p2");
    assert!(row(&rows, "p2").get("octoNotice").is_none());
}

#[tokio::test]
async fn a_notice_cover_is_a_painted_picture_and_never_relayed() {
    let h = with_notices(|_| {}).await;

    for id in ["oc-notice-review", "oc-notice-duplicates"] {
        let cover = get(
            &h.app,
            &format!("/rest/getCoverArt.view?{ALICE}&id={id}&size=600"),
        )
        .await;
        assert!(cover.status.is_success(), "{id}: {}", cover.status);
        assert_eq!(cover.media_type().as_deref(), Some("image/jpeg"), "{id}");
        assert_eq!(&cover.body[..3], &[0xFF, 0xD8, 0xFF], "{id}");
    }
    assert!(
        !h.library
            .lock()
            .asked
            .iter()
            .any(|path| path.contains("getCoverArt")),
        "a notice cover is Octo's own"
    );
}
