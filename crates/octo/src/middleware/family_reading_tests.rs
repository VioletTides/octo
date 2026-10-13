//! The family gate's one reading of each request, tried through the whole pipeline against a
//! Navidrome stood in for by wiremock that reads a request the way Navidrome does: each key
//! exactly as spelled, its first value, a url-encoded body (on a POST, PUT or PATCH) before the
//! query, and the answer in the format the first `f` names. The stand-in is lenient about the
//! path (any case, escapes decoded), so a rule that is skipped shows up as a leak.
//!
//! It signs in `alex` only with the hidden password or the real browser token Navidrome gave
//! the web device, and the owner `winters` only with `owner-pw`.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use octo_core::family::codes::subsonic_token;
use octo_core::family::device::{Device, NetworkPlace};
use octo_core::settings::{AppSettings, FamilySettings, SubsonicSettings};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{path_regex, query_param};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

use crate::app::{AppState, TestServices};
use crate::http::pipeline::{App, build_with};
use crate::http::routes::RouteSet;
use crate::middleware::family_gate::GateReading;
use crate::services::family::family_caller;
use crate::services::family::member_bridge::{ACCOUNT_FAILURES, MAX_FAILURES, PEER_FAILURES};
use crate::services::family::member_credentials::tests::member;
use crate::services::family::saved_store::SavedSong;
use crate::services::subsonic::IncomingRequest;

const APP_SECRET: &str = "app-secret-1";
const HIDDEN: &str = "hidden-1";
const OWNER_PASSWORD: &str = "owner-pw";
const HOME: &str = "192.168.1.5";
const ALEX: &str = "u=alex&p=app-secret-1&v=1.16.1&c=Test";

// ---------------------------------------------------------------------------------------
// The stand-in Navidrome
// ---------------------------------------------------------------------------------------

/// Every key Navidrome reads from a request, exactly as spelled, with its values in the order
/// `r.Form` holds them: a url-encoded body's first (on a POST, PUT or PATCH), then the query's.
fn navidrome_reads(request: &wiremock::Request) -> Vec<(String, String)> {
    let own =
        |(k, v): (std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)| (k.into_owned(), v.into_owned());
    let form = matches!(request.method.as_str(), "POST" | "PUT" | "PATCH")
        && request
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| {
                ct.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/x-www-form-urlencoded")
            });
    let mut pairs: Vec<(String, String)> = Vec::new();
    if form {
        pairs.extend(url::form_urlencoded::parse(&request.body).map(own));
    }
    pairs.extend(url::form_urlencoded::parse(request.url.query().unwrap_or("").as_bytes()).map(own));
    pairs
}

/// The first value of `key`, spelled exactly so.
fn first<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Every value of `key`, spelled exactly so.
fn all<'a>(pairs: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    pairs
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}

/// The endpoint a path names, leniently: the last segment, escapes decoded, any case, without
/// `.view`.
fn endpoint_of(path: &str) -> String {
    let last = path.split(['/', '\\']).rfind(|s| !s.is_empty()).unwrap_or("");
    let mut name = last.to_string();
    for _ in 0..3 {
        name = percent_encoding::percent_decode_str(&name)
            .decode_utf8_lossy()
            .into_owned();
    }
    let name = name.to_ascii_lowercase();
    name.strip_suffix(".view").unwrap_or(&name).to_string()
}

enum Shape {
    Xml,
    Json,
    Jsonp(String),
}

fn answer(shape: &Shape, inner: Value, xml_inner: &str) -> ResponseTemplate {
    let mut response = json!({ "status": "ok", "version": "1.16.1" });
    if let (Value::Object(fields), Value::Object(more)) = (&mut response, inner) {
        fields.extend(more);
    }
    let json = json!({ "subsonic-response": response }).to_string();
    match shape {
        Shape::Json => ResponseTemplate::new(200).set_body_raw(json, "application/json"),
        Shape::Jsonp(callback) => ResponseTemplate::new(200)
            .set_body_raw(format!("{callback}({json});"), "application/javascript"),
        Shape::Xml => ResponseTemplate::new(200).set_body_raw(
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="1.16.1">{xml_inner}</subsonic-response>"#
            ),
            "application/xml",
        ),
    }
}

fn refused(shape: &Shape) -> ResponseTemplate {
    let json = r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#;
    match shape {
        Shape::Json => ResponseTemplate::new(200).set_body_raw(json, "application/json"),
        Shape::Jsonp(callback) => {
            ResponseTemplate::new(200).set_body_raw(format!("{callback}({json});"), "application/javascript")
        }
        Shape::Xml => ResponseTemplate::new(200).set_body_raw(
            r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="failed" version="1.16.1"><error code="40" message="Wrong username or password"/></subsonic-response>"#,
            "application/xml",
        ),
    }
}

struct Navidrome {
    real_jwt: String,
}

impl Respond for Navidrome {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let reads = navidrome_reads(request);
        let get = |key: &str| first(&reads, key);
        let shape = match get("f") {
            Some("json") => Shape::Json,
            Some("jsonp") => Shape::Jsonp(get("callback").unwrap_or("").to_string()),
            _ => Shape::Xml,
        };
        let hidden = match (get("t"), get("s")) {
            (Some(t), Some(s)) => subsonic_token(HIDDEN, s) == t.to_ascii_lowercase(),
            _ => false,
        };
        let alex = (get("u") == Some("alex") && hidden && get("p").is_none())
            || get("jwt") == Some(self.real_jwt.as_str());
        let owner =
            get("u").is_some_and(|u| u.eq_ignore_ascii_case("winters")) && get("p") == Some(OWNER_PASSWORD);
        if !(alex || owner) {
            return refused(&shape);
        }
        match endpoint_of(request.url.path()).as_str() {
            "getnowplaying" => answer(
                &shape,
                json!({ "nowPlaying": { "entry": [
                    { "id": "np-1", "username": "alex" },
                    { "id": "np-2", "username": "sam" }
                ] } }),
                r#"<nowPlaying><entry id="np-1" username="alex"/><entry id="np-2" username="sam"/></nowPlaying>"#,
            ),
            "getuser" => answer(
                &shape,
                json!({ "user": {
                    "username": "alex", "adminRole": true, "downloadRole": true,
                    "shareRole": true, "streamRole": true
                } }),
                r#"<user username="alex" adminRole="true" downloadRole="true" shareRole="true" streamRole="true"/>"#,
            ),
            "getstarred2" => answer(
                &shape,
                json!({ "starred2": { "song": [ { "id": "lib-1", "title": "Library Song" } ] } }),
                r#"<starred2><song id="lib-1" title="Library Song"/></starred2>"#,
            ),
            _ => answer(&shape, json!({}), ""),
        }
    }
}

// ---------------------------------------------------------------------------------------
// The family
// ---------------------------------------------------------------------------------------

struct Family {
    server: MockServer,
    state: AppState,
    app: App,
    _dir: tempfile::TempDir,
}

/// A Navidrome browser token for `sub`.
fn navidrome_jwt(sub: &str) -> String {
    let payload = json!({ "iss": "ND", "sub": sub, "exp": Utc::now().timestamp() + 3600 });
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(payload.to_string()),
        URL_SAFE_NO_PAD.encode("navidrome-signature")
    )
}

/// What the echo route saw: whether the gate read the request, who it is from, and the
/// parameters a handler reads.
async fn echo(req: Request) -> Response {
    let caller = family_caller(req.extensions()).cloned();
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.expect("a body");
    let read_by_gate = GateReading::of(&parts.extensions).is_some();
    let parameters: Vec<Value> = IncomingRequest::from_parts(&parts, body)
        .parameters()
        .iter()
        .map(|(k, v)| json!([k, v]))
        .collect();
    axum::Json(json!({
        "gate": read_by_gate,
        "parameters": parameters,
        "user": caller.as_ref().map(|c| c.user.clone()),
        "managed": caller.as_ref().map(|c| c.managed),
    }))
    .into_response()
}

/// The owner `winters`, the listener `alex` (an Octo app `d_app` and a web device `d_web`
/// holding Navidrome's real token) with `edits` over the preset, and the member `sam`.
async fn family_with(enabled: bool, edits: Value) -> Family {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("a temp dir");
    let state = AppState::for_tests_with(
        AppSettings {
            subsonic: SubsonicSettings {
                url: Some(server.uri()),
                admin_username: Some("admin".into()),
                admin_password: Some("pw".into()),
                auto_detect_download_path: false,
                ..Default::default()
            },
            family: FamilySettings {
                enabled,
                members_path: dir.path().join("members").to_string_lossy().into_owned(),
                ..Default::default()
            },
            ..Default::default()
        },
        TestServices {
            config_dir: Some(dir.path().join("config")),
            ..Default::default()
        },
    );
    assert!(state.family_vault.create_key_if_missing().expect("a key"));
    let store = &state.family_store;
    store.set_owner("winters");
    let mut alex = member("alex", state.family_vault.seal(HIDDEN).expect("sealed"));
    alex.account_password =
        crate::services::family::passwords::seal(&state.family_vault, APP_SECRET).expect("sealed");
    if let Value::Object(edits) = edits {
        alex.edits = edits;
    }
    store.upsert_member(alex);
    store.upsert_member(member(
        "sam",
        state.family_vault.seal("hidden-2").expect("sealed"),
    ));
    let mut mira = member("mira", state.family_vault.seal("hidden-3").expect("sealed"));
    mira.account_password =
        crate::services::family::passwords::seal(&state.family_vault, "mira-secret").expect("sealed");
    store.upsert_member(mira);
    let real_jwt = navidrome_jwt("alex");
    for (id, user, jwt) in [
        ("d_app", "alex", None),
        ("d_web", "alex", Some(real_jwt.as_str())),
        ("d_mira", "mira", None),
    ] {
        store.add_device(Device {
            id: id.into(),
            username: user.into(),
            name: String::new(),
            created: Utc::now(),
            last_seen: None,
            app: String::new(),
            place: NetworkPlace::Home,
            seen_apps: Vec::new(),
            navidrome_jwt: jwt.and_then(|jwt| state.family_vault.seal(jwt)),
            quality: Default::default(),
        });
    }
    state.family_saved.add(
        "alex",
        SavedSong {
            id: "ext-deezer-song-1".into(),
            title: "Saved Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            cover_art: String::new(),
            duration: 200,
            explicit: 0,
            saved: Utc
                .with_ymd_and_hms(2026, 10, 20, 18, 0, 0)
                .single()
                .expect("a time"),
        },
    );
    Mock::given(path_regex("(?i)rest"))
        .respond_with(Navidrome { real_jwt })
        .with_priority(5)
        .mount(&server)
        .await;
    let routes = RouteSet::new().subsonic("testEcho", routing::get(echo).post(echo));
    let app = build_with(state.clone(), routes);
    Family {
        server,
        state,
        app,
        _dir: dir,
    }
}

async fn family(enabled: bool) -> Family {
    family_with(enabled, json!({})).await
}

impl Family {
    /// Every request Navidrome was sent so far.
    async fn received(&self) -> Vec<wiremock::Request> {
        self.server.received_requests().await.unwrap_or_default()
    }

    /// The browser token Octo signs for Alex's web device.
    fn octo_web_token(&self) -> String {
        use crate::services::family::web_session::{WebClaims, mint};
        let key = self.state.family_vault.jwt_key().expect("a key");
        let now = Utc::now().timestamp();
        mint(
            &key,
            &WebClaims {
                iss: "ND".into(),
                sub: "alex".into(),
                uid: "u-alex".into(),
                adm: false,
                iat: now,
                exp: now + 3600,
                did: "d_web".into(),
            },
        )
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }

    /// The Subsonic error code of the answer, in any format.
    fn error_code(&self) -> Option<i64> {
        if let Ok(value) = serde_json::from_str::<Value>(&self.body) {
            return value["subsonic-response"]["error"]["code"].as_i64();
        }
        let at = self.body.find("<error ")?;
        let rest = &self.body[at..];
        let start = rest.find("code=\"")? + 6;
        let end = rest[start..].find('"')? + start;
        rest[start..end].parse().ok()
    }
}

async fn send_with(
    f: &Family,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Body,
    peer: &str,
) -> Reply {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = req.body(body).expect("request");
    let address: SocketAddr = format!("{peer}:50000").parse().expect("an address");
    req.extensions_mut().insert(ConnectInfo(address));
    let res = f.app.clone().oneshot(req).await.expect("infallible");
    let (parts, body) = res.into_parts();
    let body = body.collect().await.expect("body").to_bytes();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

async fn send(f: &Family, method: Method, uri: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    send_with(f, method, uri, headers, Body::from(body.to_string()), HOME).await
}

async fn get_from(f: &Family, uri: &str, peer: &str) -> Reply {
    send_with(f, Method::GET, uri, &[], Body::empty(), peer).await
}

async fn get(f: &Family, uri: &str) -> Reply {
    get_from(f, uri, HOME).await
}

/// Whether a now-playing answer was filtered for Alex: Alex's entry is there and Sam's is not.
fn only_alex_listening(r: &Reply) -> bool {
    r.body.contains("np-1") && !r.body.contains("np-2") && !r.body.contains("sam")
}

// ---------------------------------------------------------------------------------------
// The format an answer rule reads is the format Navidrome answered in
// ---------------------------------------------------------------------------------------

/// `f` repeated, empty, in another case, or beside a `callback`. Navidrome answers in the
/// format the first exact `f` names.
const FORMATS: &[&str] = &[
    "",
    "f=json",
    "f=jsonp&callback=cb",
    "f=&f=json",
    "f=json&f=xml",
    "f=xml&f=json",
    "f=",
    "f=jsonp&callback=cb&f=json",
    "f=json&f=jsonp&callback=cb",
    "F=json",
    "f=JSON",
    "f=json,xml",
];

#[tokio::test]
async fn now_playing_is_filtered_in_whatever_format_navidrome_answered() {
    let f = family(true).await;
    let mut leaked = Vec::new();
    for format in FORMATS {
        let r = get(&f, &format!("/rest/getNowPlaying?{ALEX}&{format}")).await;
        if !only_alex_listening(&r) {
            leaked.push(format!("{format}: {}", r.body));
        }
    }
    assert!(leaked.is_empty(), "{leaked:#?}");
}

#[tokio::test]
async fn user_roles_are_rewritten_in_whatever_format_navidrome_answered() {
    let f = family(true).await;
    for format in FORMATS {
        let r = get(&f, &format!("/rest/getUser?{ALEX}&username=alex&{format}")).await;
        assert!(r.body.contains("adminRole"), "{format}: {}", r.body);
        for role in ["adminRole", "downloadRole", "shareRole"] {
            assert!(
                !r.body.contains(&format!("\"{role}\":true"))
                    && !r.body.contains(&format!("{role}=\"true\"")),
                "{format}: {role} went out as Navidrome had it: {}",
                r.body
            );
        }
    }
}

#[tokio::test]
async fn saved_songs_join_starred2_in_whatever_format_navidrome_answered() {
    let f = family(true).await;
    for format in FORMATS {
        let r = get(&f, &format!("/rest/getStarred2?{ALEX}&{format}")).await;
        assert!(r.body.contains("lib-1"), "{format}: {}", r.body);
        assert!(r.body.contains("ext-deezer-song-1"), "{format}: {}", r.body);
    }
}

/// An answer the rules cannot read never goes to a member unfiltered: it is error 0. The owner,
/// whose answers no rule changes, gets it as it came.
#[tokio::test]
async fn an_answer_the_rules_cannot_read_is_error_0_for_a_member() {
    let f = family(true).await;
    for (client, body, content_type) in [
        (
            "BrokenJson",
            r#"{"subsonic-response":{"nowPlaying":{"entry":[{"username":"sam"}"#,
            "application/json",
        ),
        (
            "BrokenXml",
            r#"<subsonic-response status="ok"><nowPlaying><entry username="sam">"#,
            "application/xml",
        ),
        ("NotText", "\u{fffd}sam\u{fffd}", "text/plain"),
    ] {
        Mock::given(path_regex("(?i)getNowPlaying"))
            .and(query_param("c", client))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
            .with_priority(1)
            .mount(&f.server)
            .await;
        let r = get(
            &f,
            &format!("/rest/getNowPlaying?u=alex&p={APP_SECRET}&c={client}&f=json"),
        )
        .await;
        assert!(!r.body.contains("sam"), "{client}: {}", r.body);
        assert_eq!(r.error_code(), Some(0), "{client}: {}", r.body);
        let owner = get(
            &f,
            &format!("/rest/getNowPlaying?u=winters&p={OWNER_PASSWORD}&c={client}&f=json"),
        )
        .await;
        assert_eq!(owner.body, body, "{client}");
    }
}

// ---------------------------------------------------------------------------------------
// Navidrome reads the sign-in the gate verified
// ---------------------------------------------------------------------------------------

/// The sign-in keys Navidrome got on the last request it was sent, in any spelling.
fn sign_in_reads(request: &wiremock::Request) -> Vec<(String, String)> {
    navidrome_reads(request)
        .into_iter()
        .filter(|(k, _)| {
            ["u", "p", "t", "s", "apikey", "jwt"]
                .iter()
                .any(|key| k.eq_ignore_ascii_case(key))
        })
        .collect()
}

/// Navidrome got exactly one `u`, `alex`, with one `t` and `s` made from the hidden password,
/// and no other sign-in key in any spelling.
fn signed_in_as_alex(request: &wiremock::Request) -> Result<(), String> {
    let keys = sign_in_reads(request);
    let names: Vec<&str> = keys.iter().map(|(k, _)| k.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    if sorted != ["s", "t", "u"] {
        return Err(format!("sign-in keys {keys:?}"));
    }
    let (u, t, s) = (
        first(&keys, "u").unwrap_or(""),
        first(&keys, "t").unwrap_or(""),
        first(&keys, "s").unwrap_or(""),
    );
    if u != "alex" || subsonic_token(HIDDEN, s) != t {
        return Err(format!("signed as {keys:?}"));
    }
    Ok(())
}

/// One way a client can write Alex's sign-in: a query, and a body with its content type.
struct Written {
    name: &'static str,
    method: Method,
    query: String,
    content_type: Option<&'static str>,
    body: String,
}

fn written(name: &'static str, query: &str) -> Written {
    Written {
        name,
        method: Method::GET,
        query: query.to_string(),
        content_type: None,
        body: String::new(),
    }
}

fn written_with(name: &'static str, query: &str, content_type: &'static str, body: &str) -> Written {
    Written {
        name,
        method: Method::POST,
        query: query.to_string(),
        content_type: Some(content_type),
        body: body.to_string(),
    }
}

const FORM: &str = "application/x-www-form-urlencoded";

/// Alex's sign-in written every way a client might: names in any case or escaped, values with
/// `+` or `%20`, empty values beside real ones, keys repeated across the query and the body.
fn alex_sign_ins() -> Vec<Written> {
    vec![
        written("plain", "u=alex&p=app-secret-1&id=A&id=B"),
        written("upper key", "U=alex&p=app-secret-1"),
        written("upper name", "u=ALEX&p=app-secret-1"),
        written("escaped key", "%75=alex&%70=app-secret-1"),
        written("both spellings", "u=alex&p=app-secret-1&U=ALEX&P=app-secret-1"),
        written("empty after", "u=alex&p=app-secret-1&u=&p="),
        written("empty before", "u=&u=alex&p=&p=app-secret-1"),
        written("plus", "u=alex+&p=app-secret-1"),
        written("space", "u=%20alex%20&p=app-secret-1"),
        written("api key beside", "u=alex&p=app-secret-1&apiKey=k&jwt=j&APIKEY=k2"),
        written("repeated ids", "id=A&U=alex&id=B&p=app-secret-1&u=alex&id=C"),
        written_with(
            "password in body",
            "u=alex&id=A",
            FORM,
            "p=app-secret-1&id=1&id=2",
        ),
        written_with("name in body", "p=app-secret-1", FORM, "u=alex&id=1"),
        written_with("name twice", "u=alex&p=app-secret-1", FORM, "u=alex&id=1"),
        written_with(
            "body in capitals",
            "u=alex&p=app-secret-1",
            FORM,
            "U=alex&P=app-secret-1&id=1",
        ),
        written_with("escaped body keys", "id=A", FORM, "%75=alex&%70=app-secret-1"),
        written_with("plus in body", "id=A", FORM, "u=alex+&p=app-secret-1"),
        written_with(
            "json password",
            "u=alex",
            "application/json",
            r#"{"p":"app-secret-1","id":5}"#,
        ),
        written_with(
            "json both spellings",
            "",
            "application/json",
            r#"{"u":"alex","U":"ALEX","p":"app-secret-1","id":[1,2]}"#,
        ),
        Written {
            name: "form body on a GET",
            method: Method::GET,
            query: "id=A".into(),
            content_type: Some(FORM),
            body: "u=alex&p=app-secret-1".into(),
        },
    ]
}

/// Whichever way Alex's sign-in is written, the gate verifies Alex and Navidrome reads Alex,
/// signed with the hidden password and nothing else.
#[tokio::test]
async fn navidrome_reads_the_member_the_gate_verified_however_the_sign_in_is_written() {
    let f = family(true).await;
    for case in alex_sign_ins() {
        let headers: Vec<(&str, &str)> = case.content_type.iter().map(|ct| ("Content-Type", *ct)).collect();
        let echo = send(
            &f,
            case.method.clone(),
            &format!("/rest/testEcho?{}", case.query),
            &headers,
            &case.body,
        )
        .await;
        assert_eq!(echo.status, StatusCode::OK, "{}: {}", case.name, echo.body);
        assert_eq!(echo.json()["user"], "alex", "{}: {}", case.name, echo.body);
        assert_eq!(echo.json()["managed"], true, "{}", case.name);
        let before = f.received().await.len();
        let r = send(
            &f,
            case.method.clone(),
            &format!("/rest/getThing?{}&f=json", case.query),
            &headers,
            &case.body,
        )
        .await;
        assert_eq!(
            r.json()["subsonic-response"]["status"],
            "ok",
            "{}: {}",
            case.name,
            r.body
        );
        let sent = f.received().await;
        assert_eq!(sent.len(), before + 1, "{}", case.name);
        if let Err(why) = signed_in_as_alex(&sent[before]) {
            panic!("{}: {why}", case.name);
        }
    }
}

/// Two people named at once, a sign-in in a multipart body, or a name that only looks like
/// Alex's under a wider folding than the gate's: refused, and Navidrome is never asked.
#[tokio::test]
async fn a_sign_in_naming_two_people_is_refused_before_navidrome() {
    let f = family(true).await;
    for case in [
        written("two names", "u=alex&u=sam&p=app-secret-1"),
        written("two names in two spellings", "u=alex&U=sam&p=app-secret-1"),
        written("owner and member", "u=winters&U=alex&p=app-secret-1"),
        written("two passwords", "u=alex&p=app-secret-1&P=other"),
        written_with(
            "another name in the body",
            "u=alex&p=app-secret-1",
            FORM,
            "u=winters",
        ),
        written_with(
            "another password in the body",
            "u=alex&p=app-secret-1",
            FORM,
            "p=other",
        ),
        written_with(
            "multipart",
            "u=alex",
            "multipart/form-data; boundary=XyZ",
            "--XyZ\r\nContent-Disposition: form-data; name=\"p\"\r\n\r\napp-secret-1\r\n--XyZ--\r\n",
        ),
    ] {
        let headers: Vec<(&str, &str)> = case.content_type.iter().map(|ct| ("Content-Type", *ct)).collect();
        let before = f.received().await.len();
        let r = send(
            &f,
            case.method.clone(),
            &format!("/rest/getThing?{}&f=json", case.query),
            &headers,
            &case.body,
        )
        .await;
        assert_eq!(r.error_code(), Some(40), "{}: {}", case.name, r.body);
        assert_eq!(f.received().await.len(), before, "{}", case.name);
    }
}

/// Names that differ from a member's only by Unicode case or width. Ignoring case as the gate
/// does (`ALEX`, `MIRA`) names the member, and Navidrome reads the member's own name. Any other
/// folding (fullwidth letters, the dotless or the dotted i) names someone else: the gate never
/// takes them for the member, and Navidrome reads exactly the name that was sent.
#[tokio::test]
async fn names_that_only_fold_to_a_members_name_are_never_that_member() {
    let f = family(true).await;
    for (name, secret, member) in [
        ("ALEX", APP_SECRET, "alex"),
        ("aLeX", APP_SECRET, "alex"),
        ("MIRA", "mira-secret", "mira"),
    ] {
        let echo = get(&f, &format!("/rest/testEcho?u={name}&p={secret}"))
            .await
            .json();
        assert_eq!(
            (echo["user"].as_str(), echo["managed"].as_bool()),
            (Some(member), Some(true)),
            "{name}"
        );
        let before = f.received().await.len();
        let _ = get(&f, &format!("/rest/getThing?u={name}&p={secret}&f=json")).await;
        let sent = f.received().await;
        let calls: Vec<&wiremock::Request> = sent[before..]
            .iter()
            .filter(|r| r.url.path() == "/rest/getThing")
            .collect();
        assert!(!calls.is_empty(), "{name}");
        for call in calls {
            let keys = sign_in_reads(call);
            assert_eq!(all(&keys, "u"), [member], "{name}: {keys:?}");
            assert!(first(&keys, "p").is_none(), "{name}: {keys:?}");
        }
    }
    for (escaped, name, secret) in [
        (
            "%EF%BD%81%EF%BD%8C%EF%BD%85%EF%BD%98",
            "\u{ff41}\u{ff4c}\u{ff45}\u{ff58}",
            APP_SECRET,
        ),
        (
            "%EF%BC%A1%EF%BC%AC%EF%BC%A5%EF%BC%B8",
            "\u{ff21}\u{ff2c}\u{ff25}\u{ff38}",
            APP_SECRET,
        ),
        ("m%C4%B1ra", "m\u{131}ra", "mira-secret"),
        ("M%C4%B0RA", "M\u{130}RA", "mira-secret"),
        ("MI%CC%87RA", "MI\u{307}RA", "mira-secret"),
    ] {
        let echo = get(&f, &format!("/rest/testEcho?u={escaped}&p={secret}"))
            .await
            .json();
        assert_eq!(echo["user"].as_str(), Some(name), "{name}");
        assert_eq!(echo["managed"], false, "{name}");
        let before = f.received().await.len();
        let r = get(&f, &format!("/rest/getThing?u={escaped}&p={secret}&f=json")).await;
        assert_eq!(r.error_code(), Some(40), "{name}: {}", r.body);
        for sent in &f.received().await[before..] {
            let keys = sign_in_reads(sent);
            assert_eq!(all(&keys, "u"), [name], "{name}: {keys:?}");
        }
    }
}

/// The owner's sign-in: whatever the gate checks with Navidrome is exactly what Navidrome then
/// reads on the call itself, so the gate never vouches for a sign-in Navidrome does not see.
#[tokio::test]
async fn the_owner_is_checked_with_exactly_the_sign_in_navidrome_then_reads() {
    let owner_cases = vec![
        written("plain", "u=winters&p=owner-pw"),
        written("upper key", "U=winters&p=owner-pw"),
        written("upper password key", "u=winters&P=owner-pw"),
        written("upper name", "u=WINTERS&p=owner-pw"),
        written("escaped keys", "%75=winters&%70=owner-pw"),
        written("empty name first", "u=&u=winters&p=owner-pw"),
        written("empty password first", "u=winters&p=&p=owner-pw"),
        written("empty after", "u=winters&p=owner-pw&p="),
        written("repeated", "u=winters&p=owner-pw&p=owner-pw"),
        written("plus", "u=winters+&p=owner-pw"),
        written("second spelling later", "u=winters&p=owner-pw&U=winters"),
        written_with("password in body", "u=winters", FORM, "p=owner-pw"),
        written_with("body in capitals", "u=winters&p=owner-pw", FORM, "P=owner-pw"),
        written_with(
            "json sign-in",
            "",
            "application/json",
            r#"{"u":"winters","p":"owner-pw"}"#,
        ),
        written_with(
            "json in capitals",
            "",
            "application/json",
            r#"{"U":"winters","p":"owner-pw"}"#,
        ),
    ];
    let mut differ = Vec::new();
    for (n, case) in owner_cases.into_iter().enumerate() {
        // A family of its own each time, so no check is remembered from another case.
        let f = family(true).await;
        let device = format!("owner-device-{n}");
        let mut headers: Vec<(&str, &str)> = vec![("X-Octo-Device-Id", device.as_str())];
        if let Some(ct) = case.content_type {
            headers.push(("Content-Type", ct));
        }
        let _ = send(
            &f,
            case.method.clone(),
            &format!("/rest/getThing?{}&f=json", case.query),
            &headers,
            &case.body,
        )
        .await;
        let sent = f.received().await;
        let checks: Vec<&wiremock::Request> = sent
            .iter()
            .filter(|r| r.url.path().eq_ignore_ascii_case("/rest/ping"))
            .collect();
        let call = sent
            .iter()
            .find(|r| r.url.path().eq_ignore_ascii_case("/rest/getThing"))
            .unwrap_or_else(|| panic!("{}: the call was not relayed", case.name));
        let read = navidrome_reads(call);
        // A sign-in written plainly is checked; one Navidrome would read differently is not.
        let plain = [
            "plain",
            "upper name",
            "escaped keys",
            "repeated",
            "second spelling later",
            "password in body",
            "json sign-in",
        ];
        if plain.contains(&case.name) && checks.is_empty() {
            differ.push(format!("{}: never checked", case.name));
        }
        for check in checks {
            let checked = navidrome_reads(check);
            for key in ["u", "p", "t", "s", "apiKey", "jwt"] {
                if first(&checked, key) != first(&read, key) {
                    differ.push(format!(
                        "{}: the gate checked {key} as {:?}, Navidrome reads {:?}",
                        case.name,
                        first(&checked, key),
                        first(&read, key)
                    ));
                }
            }
        }
    }
    assert!(differ.is_empty(), "{differ:#?}");
}

/// The relay sends a repeated key as its separate values again; for a member that never brings
/// back a sign-in key the gate took out.
#[tokio::test]
async fn repeated_keys_come_back_without_the_member_sign_in() {
    let f = family(true).await;
    let _ = send(
        &f,
        Method::GET,
        "/rest/getThing?id=A&u=alex&id=B&p=app-secret-1&U=alex&id=C&f=json",
        &[],
        "",
    )
    .await;
    let _ = send(
        &f,
        Method::POST,
        "/rest/getThing?u=alex&id=A&f=json",
        &[("Content-Type", FORM)],
        "id=1&p=app-secret-1&id=2&P=app-secret-1",
    )
    .await;
    let sent = f.received().await;
    assert_eq!(sent.len(), 2);
    let read = navidrome_reads(&sent[0]);
    assert_eq!(all(&read, "id"), ["A", "B", "C"], "{read:?}");
    signed_in_as_alex(&sent[0]).unwrap_or_else(|why| panic!("{why}"));
    let read = navidrome_reads(&sent[1]);
    // A form value overrides the query's, as it always has.
    assert_eq!(all(&read, "id"), ["1", "2"], "{read:?}");
    signed_in_as_alex(&sent[1]).unwrap_or_else(|why| panic!("{why}"));
}

// ---------------------------------------------------------------------------------------
// Every way to write a call reaches the same rules
// ---------------------------------------------------------------------------------------

/// Paths that name `getNowPlaying` to a lenient Navidrome, written every way a client might.
const NOW_PLAYING_PATHS: &[&str] = &[
    "/rest/getNowPlaying",
    "/rest/getNowPlaying.view",
    "/REST/GETNOWPLAYING.VIEW",
    "/Rest/GetNowPlaying",
    "/rest/getNowPlaying/",
    "/rest/%67etNowPlaying",
    "/rest/getNowPlaying%2Eview",
    "/rest/%2567etNowPlaying",
    "/rest/x/../getNowPlaying",
    "/rest/./getNowPlaying",
    "/rest/%2e/getNowPlaying",
    "/rest/x/%2E%2E/getNowPlaying",
    "/rest//getNowPlaying",
    "//rest/getNowPlaying",
    "/api/../rest/getNowPlaying",
    "/rest%2FgetNowPlaying",
    "/rest%5CgetNowPlaying",
    "/rest/getNow%09Playing",
    "/%72est/getNowPlaying",
];

#[tokio::test]
async fn now_playing_is_filtered_however_the_path_is_written() {
    let f = family(true).await;
    let octo = f.octo_web_token();
    let mut leaked = Vec::new();
    for path in NOW_PLAYING_PATHS {
        for sign_in in [ALEX.to_string(), format!("jwt={octo}")] {
            let r = get(&f, &format!("{path}?{sign_in}&f=json")).await;
            if r.body.contains("np-2") || r.body.contains("sam") {
                leaked.push(format!("{path} ({})", &sign_in[..6]));
            }
        }
    }
    assert!(
        leaked.is_empty(),
        "another member's listening went out: {leaked:#?}"
    );
}

/// HEAD, a Range, a chunked body and a Content-Length that does not match the body all reach
/// the same rule.
#[tokio::test]
async fn now_playing_is_filtered_whatever_the_method_and_framing() {
    let f = family(true).await;
    // HEAD: no body goes out at all, so no length of Navidrome's answer either.
    let r = send(
        &f,
        Method::HEAD,
        &format!("/rest/getNowPlaying?{ALEX}&f=json"),
        &[],
        "",
    )
    .await;
    assert!(!r.body.contains("np-2"), "{}", r.body);
    assert!(
        r.headers.get("content-length").is_none_or(|v| v == "0"),
        "{:?}",
        r.headers
    );
    // A Range: Navidrome is asked for the whole answer, which the rule reads.
    let before = f.received().await.len();
    let r = send(
        &f,
        Method::GET,
        &format!("/rest/getNowPlaying?{ALEX}&f=json"),
        &[("Range", "bytes=0-40"), ("If-Range", "\"x\"")],
        "",
    )
    .await;
    assert!(only_alex_listening(&r), "{}", r.body);
    let sent = f.received().await;
    assert!(
        sent[before].headers.get("range").is_none(),
        "a Range went to Navidrome for an answer a rule reads"
    );
    // A chunked body.
    let chunks = futures::stream::iter([
        Ok::<_, std::io::Error>(bytes::Bytes::from("u=alex&p=app")),
        Ok(bytes::Bytes::from("-secret-1&f=json")),
    ]);
    let r = send_with(
        &f,
        Method::POST,
        "/rest/getNowPlaying",
        &[("Content-Type", FORM), ("Transfer-Encoding", "chunked")],
        Body::from_stream(chunks),
        HOME,
    )
    .await;
    assert!(only_alex_listening(&r), "chunked: {}", r.body);
    // A Content-Length shorter, and longer, than the body.
    for length in ["3", "4000"] {
        let r = send(
            &f,
            Method::POST,
            "/rest/getNowPlaying",
            &[("Content-Type", FORM), ("Content-Length", length)],
            "u=alex&p=app-secret-1&f=json",
        )
        .await;
        assert!(only_alex_listening(&r), "length {length}: {}", r.body);
    }
}

/// A JSON body's numbers, arrays and objects for the keys the rules read are read as Navidrome
/// is sent them: the rule follows the answer, and the family limit holds.
#[tokio::test]
async fn json_values_of_every_kind_reach_the_same_rules() {
    let f = family(true).await;
    for body in [
        r#"{"u":"alex","p":"app-secret-1","f":"json"}"#,
        r#"{"u":"alex","p":"app-secret-1","f":["json"]}"#,
        r#"{"u":"alex","p":"app-secret-1","f":{"f":"json"}}"#,
        r#"{"u":"alex","p":"app-secret-1","f":1}"#,
        r#"{"u":"alex","p":"app-secret-1","f":null}"#,
    ] {
        let r = send(
            &f,
            Method::POST,
            "/rest/getNowPlaying",
            &[("Content-Type", "application/json")],
            body,
        )
        .await;
        assert!(only_alex_listening(&r), "{body}: {}", r.body);
    }
    let capped = family_with(true, json!({ "streamCap": 192 })).await;
    for body in [
        r#"{"id":["lib-1"],"maxBitRate":[0],"format":["raw"]}"#,
        r#"{"id":{"a":1},"maxBitRate":{"v":0},"format":{"v":"raw"}}"#,
        r#"{"id":7,"maxBitRate":0.0,"format":"raw"}"#,
        r#"{"id":"lib-1","maxBitRate":1e9,"format":"RAW"}"#,
    ] {
        let before = capped.received().await.len();
        let _ = send(
            &capped,
            Method::POST,
            &format!("/rest/stream?{ALEX}&f=json"),
            &[("Content-Type", "application/json")],
            body,
        )
        .await;
        let sent = capped.received().await;
        let streams: Vec<&wiremock::Request> = sent[before..]
            .iter()
            .filter(|r| r.url.path().eq_ignore_ascii_case("/rest/stream"))
            .collect();
        for stream in streams {
            let read = navidrome_reads(stream);
            assert_eq!(first(&read, "maxBitRate"), Some("192"), "{body}: {read:?}");
            assert!(
                !first(&read, "format").is_some_and(|f| f.eq_ignore_ascii_case("raw")),
                "{body}: {read:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// With Family off nothing is read
// ---------------------------------------------------------------------------------------

/// Every request shape above with Family off: handlers read the parameters the parser always
/// gave, Navidrome is sent what it always was, and no answer is changed.
#[tokio::test]
async fn family_off_reads_and_relays_every_request_shape_as_before() {
    let f = family(false).await;
    let shapes: Vec<(&str, &str)> = vec![
        ("formats", "u=alex&p=x&f=&f=json&F=xml"),
        ("escaped keys", "%75=alex&%70=x&id=A"),
        ("plus and space", "u=a+b&p=a%20b&q=c+d%2Be"),
        ("empty values", "u=&u=alex&id=&id=B&p="),
        ("case variants", "u=alex&U=ALEX&Id=1&id=2"),
        ("fullwidth", "u=%EF%BD%81%EF%BD%8C%EF%BD%85%EF%BD%98&p=x"),
    ];
    let mut seen = Vec::new();
    for (name, query) in &shapes {
        let echo = get(&f, &format!("/rest/testEcho?{query}")).await.json();
        assert_eq!(echo["gate"], false, "{name}");
        assert_eq!(echo["user"], Value::Null, "{name}");
        seen.push(echo["parameters"].clone());
        let _ = get(&f, &format!("/rest/getThing?{query}")).await;
    }
    assert_eq!(
        seen,
        vec![
            json!([["u", "alex"], ["p", "x"], ["f", "json,xml"]]),
            json!([["u", "alex"], ["p", "x"], ["id", "A"]]),
            json!([["u", "a b"], ["p", "a b"], ["q", "c d+e"]]),
            json!([["u", "alex"], ["id", "B"], ["p", ""]]),
            json!([["u", "alex,ALEX"], ["Id", "1,2"]]),
            json!([["u", "\u{ff41}\u{ff4c}\u{ff45}\u{ff58}"], ["p", "x"]]),
        ]
    );
    let relayed: Vec<Option<String>> = f
        .received()
        .await
        .iter()
        .map(|r| r.url.query().map(str::to_string))
        .collect();
    assert_eq!(
        relayed,
        vec![
            Some("u=alex&p=x&f=&f=json&f=xml".to_string()),
            Some("u=alex&p=x&id=A".to_string()),
            Some("u=a%20b&p=a%20b&q=c%20d%2Be".to_string()),
            Some("u=&u=alex&id=&id=B&p=".to_string()),
            Some("u=alex&u=ALEX&Id=1&Id=2".to_string()),
            Some("u=%EF%BD%81%EF%BD%8C%EF%BD%85%EF%BD%98&p=x".to_string()),
        ]
    );
    // The paths, the methods and the framing: nothing is filtered, nothing is read.
    let owner = format!("u=winters&p={OWNER_PASSWORD}");
    for path in NOW_PLAYING_PATHS.iter().take(5) {
        for format in FORMATS {
            let r = get(&f, &format!("{path}?{owner}&{format}")).await;
            assert!(r.body.contains("np-2"), "{path} {format}: {}", r.body);
        }
    }
    let before = f.received().await.len();
    let r = send(
        &f,
        Method::GET,
        &format!("/rest/getNowPlaying?{owner}&f=json"),
        &[("Range", "bytes=0-40")],
        "",
    )
    .await;
    assert!(r.body.contains("np-2"), "{}", r.body);
    assert!(
        f.received().await[before].headers.get("range").is_some(),
        "with Family off a Range goes on as it came"
    );
    let r = send(
        &f,
        Method::POST,
        "/rest/getNowPlaying",
        &[("Content-Type", "application/json")],
        r#"{"u":"winters","p":"owner-pw","f":["json"]}"#,
    )
    .await;
    assert!(r.body.contains("np-2"), "{}", r.body);
}

// ---------------------------------------------------------------------------------------
// Lockouts
// ---------------------------------------------------------------------------------------

/// The exact limits on wrong sign-ins for a member on `/rest`, in a 15 minute window: 20 from
/// one address for one account, 100 from one peer whatever accounts it names, and 100 for one
/// account from every address that never signed in to it. A device at an address that signed
/// in to the account before is not held back by that last count; its own address still allows
/// 20, and such addresses together 100 more.
#[tokio::test]
async fn wrong_tries_from_new_addresses_never_lock_out_a_device_at_a_known_address() {
    assert_eq!((MAX_FAILURES, PEER_FAILURES, ACCOUNT_FAILURES), (20, 100, 100));
    let f = family(true).await;
    let known = "198.51.100.50";
    let r = get_from(&f, &format!("/rest/ping?{ALEX}&f=json"), known).await;
    assert_eq!(r.json()["subsonic-response"]["status"], "ok", "{}", r.body);
    let wrong = "u=alex&p=wrong-secret&v=1.16.1&c=x&f=json";
    for n in 0..ACCOUNT_FAILURES {
        let from = format!("203.0.{}.{}", n / 200, n % 200 + 1);
        let r = get_from(&f, &format!("/rest/ping?{wrong}"), &from).await;
        assert_eq!(r.error_code(), Some(40));
    }
    // Guessing stays bounded: a new address waits, even with the right secret.
    let r = get_from(&f, &format!("/rest/ping?{ALEX}&f=json"), "198.51.100.9").await;
    assert_eq!(r.error_code(), Some(40), "{}", r.body);
    // The device that signed in before still does.
    let r = get_from(&f, &format!("/rest/ping?{ALEX}&f=json"), known).await;
    assert_eq!(
        r.json()["subsonic-response"]["status"],
        "ok",
        "a flood from new addresses locked out a known device: {}",
        r.body
    );
    // Its own address still allows only 20 wrong tries.
    for _ in 0..MAX_FAILURES {
        let r = get_from(&f, &format!("/rest/ping?{wrong}"), known).await;
        assert_eq!(r.error_code(), Some(40));
    }
    let r = get_from(&f, &format!("/rest/ping?{ALEX}&f=json"), known).await;
    assert_eq!(r.error_code(), Some(40), "{}", r.body);
}

async fn web_sign_in(f: &Family, password: &str, from: &str) -> Reply {
    send_with(
        f,
        Method::POST,
        "/auth/login",
        &[("Content-Type", "application/json")],
        Body::from(json!({ "username": "alex", "password": password }).to_string()),
        from,
    )
    .await
}

/// The same for the web player's sign-in.
#[tokio::test]
async fn web_player_sign_ins_from_new_addresses_never_lock_out_a_known_address() {
    let f = family(true).await;
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "u-alex", "name": "Alex", "username": "alex", "isAdmin": false,
            "token": navidrome_jwt("alex"), "subsonicSalt": "x", "subsonicToken": "y"
        })))
        .mount(&f.server)
        .await;
    let known = "198.51.100.50";
    assert_eq!(web_sign_in(&f, APP_SECRET, known).await.status, StatusCode::OK);
    for n in 0..ACCOUNT_FAILURES {
        let from = format!("203.0.{}.{}", n / 200, n % 200 + 1);
        assert_eq!(
            web_sign_in(&f, "wrong-guess", &from).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        web_sign_in(&f, APP_SECRET, "198.51.100.9").await.status,
        StatusCode::UNAUTHORIZED,
        "a new address waits"
    );
    assert_eq!(
        web_sign_in(&f, APP_SECRET, known).await.status,
        StatusCode::OK,
        "a known address does not"
    );
}
