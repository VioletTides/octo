//! The import page's endpoints. The dashboard's need its sign-in, the apps' check the caller with
//! Navidrome, both answer in the shapes the page and the apps read, and Spotify's return to
//! Octo's own address finishes a sign-in without one.

use axum::body::Body;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use octo_core::settings::{AppSettings, ImportSettings, SubsonicSettings};
use tower::ServiceExt;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::test_app::{Reply, app, get, send};
use super::*;
use crate::app::TestServices;
use crate::http::pipeline::App;

/// The fake Navidrome: a ping that takes `t=good`, a login, one library song.
async fn navidrome() -> MockServer {
    let server = MockServer::start().await;
    let json =
        |body: &str| ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json");
    Mock::given(path_regex("/rest/ping(\\.view)?$"))
        .and(query_param("t", "good"))
        .respond_with(json(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome"}}"#,
        ))
        .mount(&server)
        .await;
    Mock::given(path_regex("/rest/ping(\\.view)?$"))
        .respond_with(json(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#,
        ))
        .mount(&server)
        .await;
    Mock::given(path_regex("/rest/getOpenSubsonicExtensions"))
        .respond_with(json(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","openSubsonic":true,"openSubsonicExtensions":[{"name":"formPost","versions":[1]}]}}"#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(json(
            r#"{"token":"jwt","isAdmin":true,"username":"admin","subsonicToken":"t","subsonicSalt":"s"}"#,
        ))
        .mount(&server)
        .await;
    Mock::given(path("/api/song"))
        .respond_with(json(
            r#"[{"id":"nd-1","path":"/m/a.flac","suffix":"flac","bitRate":900,"title":"Teardrop","artist":"Massive Attack","duration":330.2}]"#,
        ))
        .mount(&server)
        .await;
    server
}

struct Rig {
    _navidrome: MockServer,
    state: AppState,
    app: App,
}

async fn rig(client_id: &str) -> Rig {
    let server = navidrome().await;
    let state = AppState::for_tests_with(
        AppSettings {
            subsonic: SubsonicSettings {
                url: Some(server.uri()),
                admin_username: Some("admin".into()),
                admin_password: Some("synthetic".into()),
                ..Default::default()
            },
            imports: ImportSettings {
                spotify_client_id: client_id.into(),
                ..Default::default()
            },
            ..Default::default()
        },
        TestServices::default(),
    );
    Rig {
        _navidrome: server,
        app: app(state.clone()),
        state,
    }
}

impl Rig {
    fn sign_in(&self, user: &str) -> String {
        self.state.browse_sessions.create(user)
    }

    async fn upload(&self, token: &str, file_name: &str, text: &str) -> Reply {
        let boundary = "octo-test-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: text/csv\r\n\r\n{text}\r\n--{boundary}--\r\n"
        );
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/imports/file")
            .header("X-Octo-Admin", "1")
            .header("X-Octo-Browse-Token", token)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("infallible");
        let (parts, body) = response.into_parts();
        Reply {
            status: parts.status,
            headers: parts.headers,
            body: body.collect().await.expect("body").to_bytes().to_vec(),
        }
    }
}

#[tokio::test]
async fn every_dashboard_endpoint_needs_the_sign_in() {
    let rig = rig("").await;
    let cases = [
        (Method::GET, "/api/admin/imports"),
        (Method::GET, "/api/admin/imports/lists/spotify-liked"),
        (Method::POST, "/api/admin/imports/spotify/connect"),
        (Method::POST, "/api/admin/imports/spotify/disconnect"),
        (Method::POST, "/api/admin/imports/link"),
        (Method::POST, "/api/admin/imports/lists/x/fetch"),
        (Method::POST, "/api/admin/imports/trickle/pause"),
        (Method::DELETE, "/api/admin/imports/lists/x"),
    ];
    for (verb, uri) in cases {
        let body = (verb == Method::POST).then_some(r#"{"on":true}"#);
        let reply = send(&rig.app, verb.clone(), uri, &[("X-Octo-Admin", "1")], body).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "{verb} {uri}: {}",
            reply.text()
        );
    }
}

#[tokio::test]
async fn the_dashboard_shows_the_signed_in_persons_lists_and_adds_a_file() {
    let rig = rig("client-1").await;
    let token = rig.sign_in("alice");
    let upload = rig
        .upload(
            &token,
            "Gym.csv",
            "Track Name,Artist Name(s)\nTeardrop,Massive Attack\nAngel,Massive Attack\n",
        )
        .await;
    assert_eq!(upload.status, StatusCode::OK, "{}", upload.text());
    let added = upload.json();
    assert_eq!(added["message"], "Added Gym, 2 songs.");
    let list_id = added["listId"].as_str().expect("an id").to_string();

    let auth = [("X-Octo-Browse-Token", token.as_str()), ("X-Octo-Admin", "1")];
    let page = send(&rig.app, Method::GET, "/api/admin/imports", &auth, None)
        .await
        .json();
    assert_eq!(page["user"], "alice");
    let overview = &page["overview"];
    assert_eq!(overview["spotify"]["configured"], true);
    assert_eq!(overview["spotify"]["connected"], false);
    assert_eq!(overview["spotify"]["redirectUri"], "http://127.0.0.1/callback");
    let lists = overview["lists"].as_array().expect("lists");
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0]["have"], 1);
    assert_eq!(lists[0]["missing"], 1);
    assert_eq!(overview["trickle"]["state"], "idle");

    let fetch = send(
        &rig.app,
        Method::POST,
        &format!("/api/admin/imports/lists/{list_id}/fetch"),
        &auth,
        Some(r#"{"on":true}"#),
    )
    .await;
    assert_eq!(fetch.status, StatusCode::OK, "{}", fetch.text());
    let detail = send(
        &rig.app,
        Method::GET,
        &format!("/api/admin/imports/lists/{list_id}"),
        &auth,
        None,
    )
    .await
    .json();
    let states: Vec<&str> = detail["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .map(|t| t["state"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(states, ["have", "queued"]);

    let connect = send(
        &rig.app,
        Method::POST,
        "/api/admin/imports/spotify/connect",
        &auth,
        None,
    )
    .await;
    let url = connect.json()["url"].as_str().unwrap_or_default().to_string();
    assert!(
        url.starts_with(crate::services::imports::SpotifyAuth::AUTHORIZE_URL),
        "{url}"
    );
}

#[tokio::test]
async fn a_write_without_the_admin_header_is_refused() {
    let rig = rig("").await;
    let token = rig.sign_in("alice");
    let reply = send(
        &rig.app,
        Method::POST,
        "/api/admin/imports/link",
        &[("X-Octo-Browse-Token", token.as_str())],
        Some(r#"{"url":"https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M"}"#),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn spotifys_return_to_octo_with_an_unknown_state_says_so() {
    let rig = rig("").await;
    let html = get(&rig.app, "/imports/spotify/callback?code=abc&state=nope").await;
    assert_eq!(html.header("content-type"), Some("text/html"));
    assert!(html.text().contains("Spotify sign-in did not finish"));
    assert!(html.text().contains("too old or was already used"));
    let denied = get(
        &rig.app,
        "/imports/spotify/callback?error=access_denied&state=nope",
    )
    .await;
    assert!(denied.text().contains("access was not allowed"));
}

const AUTH: &str = "u=alice&t=good&s=salt&v=1.16.1&c=Octo&f=json";

#[tokio::test]
async fn the_apps_see_the_same_lists_as_whoever_they_signed_in_as() {
    let rig = rig("").await;
    let token = rig.sign_in("alice");
    let upload = rig
        .upload(
            &token,
            "Gym.csv",
            "Track Name,Artist Name(s)\nTeardrop,Massive Attack\n",
        )
        .await;
    assert_eq!(upload.status, StatusCode::OK, "{}", upload.text());

    let all = get(&rig.app, &format!("/rest/getImports.view?{AUTH}"))
        .await
        .json();
    let envelope = &all["subsonic-response"];
    assert_eq!(envelope["status"], "ok");
    let list = &envelope["imports"]["lists"][0];
    assert_eq!(list["name"], "Gym");

    let one = get(
        &rig.app,
        &format!(
            "/rest/getImport?{AUTH}&id={}",
            list["id"].as_str().unwrap_or_default()
        ),
    )
    .await
    .json();
    assert_eq!(one["subsonic-response"]["import"]["tracks"][0]["state"], "have");

    let bob = get(
        &rig.app,
        "/rest/getImports?u=bob&t=good&s=salt&v=1.16.1&c=Octo&f=json",
    )
    .await
    .json();
    assert_eq!(
        bob["subsonic-response"]["imports"]["lists"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn an_app_action_answers_in_words_and_a_wrong_password_is_refused() {
    let rig = rig("").await;
    let pause = get(&rig.app, &format!("/rest/importAction?{AUTH}&action=pause"))
        .await
        .json();
    let answer = &pause["subsonic-response"]["importAction"];
    assert_eq!(answer["ok"], true);
    assert!(
        answer["message"]
            .as_str()
            .unwrap_or_default()
            .starts_with("Paused")
    );

    let connect = get(
        &rig.app,
        &format!("/rest/importAction?{AUTH}&action=connect&redirect=http://127.0.0.1:5555/callback"),
    )
    .await
    .json();
    assert!(
        connect["subsonic-response"]["importAction"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Client ID")
    );

    let wrong = get(
        &rig.app,
        "/rest/getImports?u=alice&t=bad&s=salt&v=1.16.1&c=Octo&f=json",
    )
    .await
    .json();
    assert_eq!(wrong["subsonic-response"]["error"]["code"], 40);
}

#[tokio::test]
async fn the_apps_are_told_imports_are_here() {
    let rig = rig("").await;
    let doc = get(
        &rig.app,
        "/rest/getOpenSubsonicExtensions.view?f=json&v=1.16.1&c=Octo",
    )
    .await
    .json();
    let names: Vec<&str> = doc["subsonic-response"]["openSubsonicExtensions"]
        .as_array()
        .expect("extensions")
        .iter()
        .map(|e| e["name"].as_str().unwrap_or_default())
        .collect();
    assert!(names.contains(&"octoImports"), "{names:?}");
}

// ---- The binder's recorded answers ------------------------------------------------------------

#[tokio::test]
async fn a_member_of_the_wrong_type_is_refused_with_the_reference_message() {
    let rig = rig("").await;
    let token = rig.sign_in("alice");
    let auth = [("X-Octo-Browse-Token", token.as_str()), ("X-Octo-Admin", "1")];
    let reply = send(
        &rig.app,
        Method::POST,
        "/api/admin/imports/lists/x/fetch",
        &auth,
        Some(r#"{"on": "yes"}"#),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let errors = &reply.json()["errors"];
    assert_eq!(errors["request"][0], "The request field is required.");
    assert_eq!(
        errors["$.on"][0],
        "The JSON value could not be converted to Octo.Controllers.ImportsController+OnRequest. Path: $.on | LineNumber: 0 | BytePositionInLine: 12."
    );
    let keys: Vec<&str> = errors
        .as_object()
        .expect("errors")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["request", "$.on"]);
}

#[tokio::test]
async fn a_file_sent_as_json_finds_no_action_and_an_empty_one_is_asked_for() {
    let rig = rig("").await;
    let token = rig.sign_in("alice");
    let auth = [("X-Octo-Browse-Token", token.as_str()), ("X-Octo-Admin", "1")];
    let json = send(
        &rig.app,
        Method::POST,
        "/api/admin/imports/file",
        &auth,
        Some(r#"{"file":"Gym.csv"}"#),
    )
    .await;
    assert_eq!(json.status, StatusCode::NOT_FOUND);
    let empty = rig.upload(&token, "Empty.csv", "").await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    assert_eq!(empty.json()["message"], "Choose a file first.");
}

#[test]
fn html_is_encoded_as_web_utility_did() {
    assert_eq!(html_encode("a<b>&\"c' é"), "a&lt;b&gt;&amp;&quot;c&#39; &#233;");
}

// ---- octoImports version 2 ------------------------------------------------------------------

impl Rig {
    /// A multipart POST to `uri` with one part named `field`, and text fields after it.
    async fn post_form(
        &self,
        uri: &str,
        field: &str,
        file_name: Option<&str>,
        text: &str,
        extra: &[(&str, &str)],
    ) -> Reply {
        let boundary = "octo-test-boundary";
        let disposition = match file_name {
            Some(name) => format!("form-data; name=\"{field}\"; filename=\"{name}\""),
            None => format!("form-data; name=\"{field}\""),
        };
        let mut body = format!("--{boundary}\r\nContent-Disposition: {disposition}\r\n\r\n{text}\r\n");
        for (name, value) in extra {
            body.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ));
        }
        body.push_str(&format!("--{boundary}--\r\n"));
        let request = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("infallible");
        let (parts, body) = response.into_parts();
        Reply {
            status: parts.status,
            headers: parts.headers,
            body: body.collect().await.expect("body").to_bytes().to_vec(),
        }
    }
}

#[tokio::test]
async fn the_apps_get_the_export_services_and_whether_spotify_can_connect() {
    let rig = rig("").await;
    let answer = get(&rig.app, &format!("/rest/getImportServices.view?{AUTH}"))
        .await
        .json();
    let envelope = &answer["subsonic-response"];
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["type"], "octo");
    let services = envelope["importServices"]["services"]
        .as_array()
        .expect("services");
    assert_eq!(services.len(), 10);
    assert_eq!(services[0]["id"], "spotify");
    assert_eq!(
        services[4]["exportUrl"],
        "https://www.tunemymusic.com/transfer/tidal-to-file"
    );
    assert_eq!(services[4]["tile"], "TIDAL");
    assert_eq!(envelope["importServices"]["spotifyConnect"], false);

    let connected = self::rig("client-1").await;
    let answer = get(&connected.app, &format!("/rest/getImportServices?{AUTH}"))
        .await
        .json();
    assert_eq!(
        answer["subsonic-response"]["importServices"]["spotifyConnect"],
        true
    );

    let wrong = get(
        &rig.app,
        "/rest/getImportServices?u=alice&t=bad&s=salt&v=1.16.1&c=Octo&f=json",
    )
    .await
    .json();
    assert_eq!(wrong["subsonic-response"]["error"]["code"], 40);
}

#[tokio::test]
async fn an_app_pastes_a_list_and_reads_it_back() {
    let rig = rig("").await;
    let pasted = get(
        &rig.app,
        &format!(
            "/rest/importText?{AUTH}&name=Gym&text=Massive%20Attack%20-%20Teardrop%0ANobody%20-%20Nothing"
        ),
    )
    .await
    .json();
    let answer = &pasted["subsonic-response"]["importAction"];
    assert_eq!(answer["ok"], true, "{pasted}");
    assert_eq!(answer["message"], "Added Gym, 2 songs.");
    let lists = get(&rig.app, &format!("/rest/getImports?{AUTH}")).await.json();
    let list = &lists["subsonic-response"]["imports"]["lists"][0];
    assert_eq!(list["name"], "Gym");
    assert_eq!(list["have"], 1);
    assert!(lists["subsonic-response"]["imports"].get("approval").is_none());

    let unnamed = rig
        .post_form(
            &format!("/rest/importText?{AUTH}"),
            "text",
            None,
            "Massive Attack - Teardrop",
            &[],
        )
        .await
        .json();
    assert_eq!(
        unnamed["subsonic-response"]["importAction"]["message"],
        "Added Pasted list, 1 songs."
    );

    let empty = get(&rig.app, &format!("/rest/importText?{AUTH}&text=%23%20nothing"))
        .await
        .json();
    let answer = &empty["subsonic-response"]["importAction"];
    assert_eq!(answer["ok"], false);
    assert_eq!(
        answer["message"],
        "No songs found. Each line should read Artist - Title."
    );

    let missing = get(&rig.app, &format!("/rest/importText?{AUTH}")).await.json();
    assert_eq!(missing["subsonic-response"]["error"]["code"], 10);
}

#[tokio::test]
async fn an_app_uploads_a_file_as_a_multipart_form() {
    let rig = rig("").await;
    let uri = format!("/rest/importFile.view?{AUTH}");
    let csv = rig
        .post_form(
            &uri,
            "file",
            Some("Gym.csv"),
            "Track Name,Artist Name(s)\nTeardrop,Massive Attack\nAngel,Massive Attack\n",
            &[],
        )
        .await;
    assert_eq!(csv.status, StatusCode::OK);
    let reply = csv.json();
    let answer = &reply["subsonic-response"]["importAction"];
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["message"], "Added Gym, 2 songs.");
    assert_eq!(answer["count"], 2);

    // A part with no file name takes the name parameter.
    let txt = rig
        .post_form(
            &uri,
            "file",
            None,
            "Teardrop - Massive Attack\n",
            &[("name", "Mine.txt")],
        )
        .await
        .json();
    assert_eq!(
        txt["subsonic-response"]["importAction"]["message"],
        "Added Mine, 1 songs."
    );

    let unreadable = rig
        .post_form(&uri, "file", Some("x.txt"), "\n\n", &[])
        .await
        .json();
    let answer = &unreadable["subsonic-response"]["importAction"];
    assert_eq!(answer["ok"], false);
    assert!(
        answer["message"]
            .as_str()
            .unwrap_or_default()
            .contains("No songs found. Each line should read Artist - Title."),
        "{answer}"
    );

    let no_file = rig.post_form(&uri, "other", Some("x.csv"), "a", &[]).await.json();
    assert_eq!(
        no_file["subsonic-response"]["importAction"]["message"],
        "Choose a file first."
    );

    let wrong = rig
        .post_form(
            "/rest/importFile?u=alice&t=bad&s=salt&v=1.16.1&c=Octo&f=json",
            "file",
            Some("Other.csv"),
            "Track Name,Artist Name(s)\nTeardrop,Massive Attack\n",
            &[],
        )
        .await
        .json();
    assert_eq!(wrong["subsonic-response"]["error"]["code"], 40);
    // Nothing was added for a refused caller.
    assert_eq!(rig.state.imports.overview("alice").lists.len(), 2);
}

#[tokio::test]
async fn the_dashboard_pastes_a_list_and_lists_the_export_services() {
    let rig = rig("").await;
    let token = rig.sign_in("alice");
    let auth = [("X-Octo-Browse-Token", token.as_str()), ("X-Octo-Admin", "1")];
    let pasted = send(
        &rig.app,
        Method::POST,
        "/api/admin/imports/text",
        &auth,
        Some(r#"{"name":"Road trip","text":"Massive Attack - Teardrop\nNobody - Nothing"}"#),
    )
    .await;
    assert_eq!(pasted.status, StatusCode::OK, "{}", pasted.text());
    assert_eq!(pasted.json()["message"], "Added Road trip, 2 songs.");

    let empty = send(
        &rig.app,
        Method::POST,
        "/api/admin/imports/text",
        &auth,
        Some(r#"{"text":"  "}"#),
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    assert_eq!(empty.json()["message"], "Paste a list first.");

    let services = send(&rig.app, Method::GET, "/api/admin/imports/services", &auth, None).await;
    assert_eq!(services.status, StatusCode::OK);
    let services = services.json();
    assert_eq!(services["services"].as_array().expect("services").len(), 10);
    assert_eq!(services["spotifyConnect"], false);

    for (verb, uri) in [
        (Method::GET, "/api/admin/imports/services"),
        (Method::POST, "/api/admin/imports/text"),
    ] {
        let body = (verb == Method::POST).then_some(r#"{"text":"A - B"}"#);
        let reply = send(&rig.app, verb.clone(), uri, &[("X-Octo-Admin", "1")], body).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{verb} {uri}");
    }
}

#[tokio::test]
async fn the_apps_are_told_imports_are_at_versions_one_and_two() {
    let rig = rig("").await;
    let doc = get(
        &rig.app,
        "/rest/getOpenSubsonicExtensions.view?f=json&v=1.16.1&c=Octo",
    )
    .await
    .json();
    let imports = doc["subsonic-response"]["openSubsonicExtensions"]
        .as_array()
        .expect("extensions")
        .iter()
        .find(|e| e["name"] == "octoImports")
        .cloned()
        .expect("octoImports");
    assert_eq!(imports["versions"], serde_json::json!([1, 2]));
}
