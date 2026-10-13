//! Where family rules plug into the family gate.
//!
//! **Request rules** run before a request goes on, for every caller the gate identified.
//! [`request_rules`] answers None to let the request through, or the response to send instead.
//! To add one, add a match arm on the endpoint name (a `/rest` endpoint without `.view`, any
//! case, or a native path such as `/api/user`), checking `caller.managed` first when the rule
//! is only for members:
//!
//! ```ignore
//! if caller.managed && endpoint.eq_ignore_ascii_case("download") && !caller.abilities.download_files {
//!     return Some(refusal(...));
//! }
//! ```
//!
//! **Answer rules** rewrite the answer to one `/rest` endpoint. Only endpoints with a rule have
//! their answer read into memory; every other answer streams through untouched. A rule gets
//! the whole answer as plain bytes (the gate asks Navidrome for no compression on these
//! endpoints and skips a rule when an answer still arrives compressed) and returns the bytes
//! to send. Register one in [`AnswerRules::standard`] (or on a running state, as tests do):
//!
//! ```ignore
//! rules.register("getUser", Arc::new(|context: &AnswerContext<'_>, body: Bytes| {
//!     if !context.caller.managed { return body; }
//!     rewrite_roles(body, context.format)
//! }));
//! ```
//!
//! The standard rules: `getUser` reports a member's download and share abilities as roles and
//! never an admin role; `getNowPlaying` shows a member only their own entries (the owner and
//! those who manage the family see everyone's); `getStarred` and `getStarred2` add the outside
//! songs and albums a member saved; `getInternetRadioStations` notes which device opened each
//! radio session, so its stream counts on that device.

use std::sync::Arc;

use axum::http::Method;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::SecondsFormat;
use octo_core::common::dotnet::eq_ignore_case;
use octo_core::family::abilities::Role;
use octo_core::family::endpoints::{EndpointClass, classify_for};
use octo_core::models::domain::{Album, Song};
use octo_subsonic::subsonic_request_parser::Parameters;
use octo_subsonic::xml::{XElement, XNode};
use parking_lot::RwLock;
use serde_json::{Map, Value};

use super::audio_gate::{self, DOWNLOADS_OFF, NOT_AVAILABLE, SHARES_OFF, subsonic_refusal};
use super::caller::FamilyCaller;
use super::list_rules::{self, Edit};
use super::saved_store::{SavedAlbum, SavedSong};
use crate::app::AppState;
use crate::http::error::json_relaxed_response;

/// What a member without the import ability reads.
pub const IMPORTS_OFF: &str = "Importing playlists is off for this account.";

/// The calls that bring lists into Octo, or offer to: a member needs the import ability for
/// each of them. Reading the lists already there (getImports, getImport) stays open.
pub const IMPORT_ENDPOINTS: [&str; 4] = ["importAction", "importFile", "importText", "getImportServices"];

/// The rules to run before a request goes on. None lets it through. `endpoint` is a `/rest`
/// endpoint name, or the path of any other request.
pub fn request_rules(
    state: &AppState,
    caller: &FamilyCaller,
    method: &Method,
    endpoint: &str,
    parameters: &Parameters,
) -> Option<Response> {
    if endpoint.starts_with('/') {
        return native_rules(state, caller, method, endpoint);
    }
    let refuse = |code: i32, message: &str| Some(subsonic_refusal(state, parameters, code, message));
    // A transcode decision lets a client play the file as it is, past any ceiling; refused, a
    // client falls back to `stream`, where the ceiling applies.
    if (eq_ignore_case(endpoint, "getTranscodeDecision") || eq_ignore_case(endpoint, "getTranscodeStream"))
        && audio_gate::ceiling(caller).is_some()
    {
        return refuse(0, NOT_AVAILABLE);
    }
    if !caller.managed {
        return None;
    }
    let strict = audio_gate::is_strict(caller);
    if endpoint.is_empty() {
        return if strict { refuse(0, NOT_AVAILABLE) } else { None };
    }
    if !caller.abilities.import_playlists
        && IMPORT_ENDPOINTS.iter().any(|name| eq_ignore_case(endpoint, name))
    {
        return refuse(50, IMPORTS_OFF);
    }
    match classify_for(&format!("/rest/{endpoint}"), method, strict) {
        EndpointClass::Refused => {
            audio_gate::log_unknown(state, caller, endpoint);
            refuse(0, NOT_AVAILABLE)
        }
        EndpointClass::AccountWrite => refuse(50, &change_in_octo(state)),
        EndpointClass::Audio => {
            let abilities = &caller.abilities;
            if eq_ignore_case(endpoint, "download") && !abilities.download_files {
                return refuse(50, DOWNLOADS_OFF);
            }
            if (eq_ignore_case(endpoint, "createShare") || eq_ignore_case(endpoint, "updateShare"))
                && !abilities.share
            {
                return refuse(50, SHARES_OFF);
            }
            None
        }
        EndpointClass::Read | EndpointClass::LibraryWrite | EndpointClass::Admin => None,
    }
}

/// "Change this in Octo: <address>/family/".
fn change_in_octo(state: &AppState) -> String {
    let base = state
        .settings
        .current()
        .server
        .public_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    format!("Change this in Octo: {base}/family/")
}

fn native_refusal(message: &str) -> Response {
    json_relaxed_response(
        axum::http::StatusCode::FORBIDDEN,
        &serde_json::json!({ "error": message }),
        "application/json",
    )
    .into_response()
}

/// Navidrome's native API for members: their account, players, transcoding and the server are
/// changed in Octo or by the owner, and share links follow the share ability.
fn native_rules(state: &AppState, caller: &FamilyCaller, method: &Method, path: &str) -> Option<Response> {
    if !caller.managed {
        return None;
    }
    match classify_for(path, method, audio_gate::is_strict(caller)) {
        EndpointClass::Read | EndpointClass::LibraryWrite => None,
        EndpointClass::Audio => {
            let share_write = path
                .get(..10)
                .is_some_and(|head| head.eq_ignore_ascii_case("/api/share"));
            (share_write && !caller.abilities.share).then(|| native_refusal(SHARES_OFF))
        }
        EndpointClass::AccountWrite => Some(native_refusal(&change_in_octo(state))),
        EndpointClass::Admin | EndpointClass::Refused => Some(native_refusal(NOT_AVAILABLE)),
    }
}

/// What an answer rule knows about the request it rewrites.
pub struct AnswerContext<'a> {
    pub state: &'a AppState,
    pub caller: &'a FamilyCaller,
    /// The `/rest` endpoint, without `.view`.
    pub endpoint: &'a str,
    /// The request path as the routes spell it.
    pub path: &'a str,
    pub parameters: &'a Parameters,
    /// The answer's content type. A rule reads the answer's format from the answer itself
    /// ([`super::list_rules::edit`]), never from what the request asked for: Navidrome reads a
    /// repeated or empty `f` its own way.
    pub content_type: Option<&'a str>,
}

/// Rewrites one answer: takes the plain body, returns the body to send.
pub type AnswerRule = Arc<dyn Fn(&AnswerContext<'_>, Bytes) -> Bytes + Send + Sync>;

/// The answer rules, by `/rest` endpoint name (matched ignoring case).
#[derive(Default)]
pub struct AnswerRules {
    rules: RwLock<Vec<(String, AnswerRule)>>,
}

impl AnswerRules {
    /// The rules Octo runs.
    pub fn standard() -> Self {
        let rules = AnswerRules::default();
        rules.register("getUser", Arc::new(user_roles));
        rules.register("getNowPlaying", Arc::new(own_now_playing));
        rules.register("getStarred", Arc::new(saved_in_starred));
        rules.register("getStarred2", Arc::new(saved_in_starred));
        rules.register("getInternetRadioStations", Arc::new(radio_openers));

        // Registered after the saved songs join getStarred, so kid profiles, the owner's
        // privacy and the personal marks see the whole answer.
        super::list_rules::register(&rules);
        rules
    }

    /// Adds a rule for `endpoint`. Rules for one endpoint run in the order they were added.
    pub fn register(&self, endpoint: &str, rule: AnswerRule) {
        self.rules.write().push((endpoint.to_string(), rule));
    }

    /// The rules for `endpoint`, in order.
    pub fn for_endpoint(&self, endpoint: &str) -> Vec<AnswerRule> {
        self.rules
            .read()
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(endpoint))
            .map(|(_, rule)| rule.clone())
            .collect()
    }

    /// Whether `endpoint` has any rule.
    pub fn has(&self, endpoint: &str) -> bool {
        self.rules
            .read()
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(endpoint))
    }
}

/// Runs the JSON or the XML form of a rule, by the format the answer is written in. `json`
/// gets the `subsonic-response` object. An answer that cannot be read is never sent as it came:
/// it becomes error 0.
fn rewrite(
    context: &AnswerContext<'_>,
    body: Bytes,
    json: impl FnOnce(&mut Map<String, Value>) -> bool,
    xml: impl FnOnce(&mut XElement) -> bool,
) -> Bytes {
    match list_rules::edit(&body, json, xml) {
        Edit::Changed(changed) => Bytes::from(changed),
        Edit::Unchanged => body,
        Edit::Unreadable => list_rules::unreadable(context, &body),
    }
}

/// `getUser`: a member's download and share roles are their abilities, and a member is never
/// an admin.
fn user_roles(context: &AnswerContext<'_>, body: Bytes) -> Bytes {
    let caller = context.caller;
    if !caller.managed {
        return body;
    }
    let roles = [
        ("downloadRole", caller.abilities.download_files),
        ("shareRole", caller.abilities.share),
        ("adminRole", false),
    ];
    rewrite(
        context,
        body,
        |response| {
            let Some(user) = response.get_mut("user").and_then(Value::as_object_mut) else {
                return false;
            };
            for (name, on) in roles {
                user.insert(name.to_string(), Value::Bool(on));
            }
            true
        },
        |root| {
            let Some(user) = root.elements_mut().find(|e| e.name == "user") else {
                return false;
            };
            for (name, on) in roles {
                user.set_attr(name, on);
            }
            true
        },
    )
}

/// Whether the caller sees everyone's listening: the owner, those who manage the family and
/// accounts outside it.
fn sees_everyone(caller: &FamilyCaller) -> bool {
    !caller.managed || caller.role == Role::Owner || caller.abilities.manage_family
}

/// `getNowPlaying`: a member sees only their own entries.
fn own_now_playing(context: &AnswerContext<'_>, body: Bytes) -> Bytes {
    let caller = context.caller;
    if sees_everyone(caller) {
        return body;
    }
    let mine = |name: Option<&str>| name.is_some_and(|n| eq_ignore_case(n, &caller.user));
    rewrite(
        context,
        body,
        |response| {
            let Some(list) = response.get_mut("nowPlaying").and_then(Value::as_object_mut) else {
                return false;
            };
            match list.get_mut("entry") {
                Some(Value::Array(entries)) => {
                    entries.retain(|entry| mine(entry.get("username").and_then(Value::as_str)));
                    true
                }
                // One entry written as an object rather than a list of one.
                Some(entry @ Value::Object(_)) => {
                    if !mine(entry.get("username").and_then(Value::as_str)) {
                        list.remove("entry");
                    }
                    true
                }
                _ => false,
            }
        },
        |root| {
            let mut found = false;
            for list in root.elements_mut().filter(|e| e.name == "nowPlaying") {
                found = true;
                list.content.retain(|node| match node {
                    XNode::Element(entry) if entry.name == "entry" => mine(entry.attribute("username")),
                    _ => true,
                });
            }
            found
        },
    )
}

/// A saved outside song as the builder writes songs outside the library.
fn saved_song(saved: &SavedSong) -> Song {
    Song {
        id: saved.id.clone(),
        title: saved.title.clone(),
        artist: saved.artist.clone(),
        album: saved.album.clone(),
        duration: i32::try_from(saved.duration).ok().filter(|d| *d > 0),
        explicit_content_lyrics: Some(saved.explicit),
        is_local: false,
        ..Default::default()
    }
}

fn saved_album(saved: &SavedAlbum) -> Album {
    Album {
        id: saved.id.clone(),
        title: saved.name.clone(),
        artist: saved.artist.clone(),
        year: saved.year,
        song_count: i32::try_from(saved.song_count).ok(),
        is_local: false,
        ..Default::default()
    }
}

fn starred_time(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `getStarred` and `getStarred2`: a member's saved outside songs and albums join their
/// starred lists, starred when they were saved.
fn saved_in_starred(context: &AnswerContext<'_>, body: Bytes) -> Bytes {
    let caller = context.caller;
    if !caller.managed {
        return body;
    }
    let saved = &context.state.family_saved;
    let songs = saved.list(&caller.user);
    let albums = saved.albums(&caller.user);
    if songs.is_empty() && albums.is_empty() {
        return body;
    }
    let container = if eq_ignore_case(context.endpoint, "getStarred2") {
        "starred2"
    } else {
        "starred"
    };
    let builder = &context.state.subsonic_response_builder;
    rewrite(
        context,
        body,
        |response| {
            if response.get("status").and_then(Value::as_str) != Some("ok") {
                return false;
            }
            let holder = response
                .entry(container)
                .or_insert_with(|| Value::Object(Default::default()));
            let Some(holder) = holder.as_object_mut() else {
                return false;
            };
            let rows = |name: &str, holder: &mut serde_json::Map<String, Value>, add: Vec<Value>| {
                let list = holder.entry(name).or_insert_with(|| Value::Array(Vec::new()));
                if let Some(list) = list.as_array_mut() {
                    let known: Vec<String> = list
                        .iter()
                        .filter_map(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
                        .collect();
                    list.extend(add.into_iter().filter(|row| {
                        row.get("id")
                            .and_then(Value::as_str)
                            .is_none_or(|id| !known.iter().any(|k| k == id))
                    }));
                }
            };
            let song_rows: Vec<Value> = songs
                .iter()
                .map(|s| {
                    let mut fields = builder.convert_song_to_json(&saved_song(s));
                    fields.insert("starred".into(), starred_time(&s.saved).into());
                    Value::Object(fields)
                })
                .collect();
            let album_rows: Vec<Value> = albums
                .iter()
                .map(|a| {
                    let mut fields = builder.convert_album_to_json(&saved_album(a));
                    fields.insert("starred".into(), starred_time(&a.saved).into());
                    Value::Object(fields)
                })
                .collect();
            if !album_rows.is_empty() {
                rows("album", holder, album_rows);
            }
            if !song_rows.is_empty() {
                rows("song", holder, song_rows);
            }
            true
        },
        |root| {
            if root.attribute("status") != Some("ok") {
                return false;
            }
            let ns = root.namespace.clone();
            if !root.elements().any(|e| e.name == container) {
                root.push(XElement::in_namespace(ns.as_deref(), container));
            }
            let Some(holder) = root.elements_mut().find(|e| e.name == container) else {
                return false;
            };
            let known: Vec<String> = holder
                .elements()
                .filter_map(|e| e.attribute("id").map(str::to_string))
                .collect();
            for album in &albums {
                if known.contains(&album.id) {
                    continue;
                }
                let mut element = builder.convert_album_to_xml(&saved_album(album), ns.as_deref());
                element.set_attr("starred", starred_time(&album.saved));
                holder.push(element);
            }
            for song in &songs {
                if known.contains(&song.id) {
                    continue;
                }
                let mut element = builder.convert_song_to_xml(&saved_song(song), ns.as_deref());
                element.set_attr("starred", starred_time(&song.saved));
                holder.push(element);
            }
            true
        },
    )
}

/// `getInternetRadioStations`: the radio sessions in the answer were opened by the caller's
/// device. The answer goes out unchanged.
fn radio_openers(context: &AnswerContext<'_>, body: Bytes) -> Bytes {
    context
        .state
        .family_audio
        .note_radio_sessions(&body, context.caller);
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use octo_core::family::abilities::{Abilities, Preset};
    use octo_core::settings::{AppSettings, FamilySettings};
    use serde_json::json;

    fn state() -> AppState {
        AppState::for_tests(AppSettings {
            family: FamilySettings {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn listener() -> FamilyCaller {
        FamilyCaller::member("alex", Preset::Listener, Abilities::preset(Preset::Listener))
    }

    /// The rules for `endpoint` over `body`, for a request that asked for `f=format`.
    fn run(state: &AppState, caller: &FamilyCaller, endpoint: &str, format: &str, body: &str) -> String {
        let mut parameters = Parameters::new();
        parameters.insert("f".into(), format.into());
        let context = AnswerContext {
            state,
            caller,
            endpoint,
            path: "/rest/x",
            parameters: &parameters,
            content_type: None,
        };
        let mut body = Bytes::from(body.to_string());
        for rule in state.family_answer_rules.for_endpoint(endpoint) {
            body = rule(&context, body);
        }
        String::from_utf8(body.to_vec()).expect("text")
    }

    #[test]
    fn get_user_reports_family_roles() {
        let state = state();
        let answer = r#"{"subsonic-response":{"status":"ok","user":{"username":"alex","adminRole":true,"downloadRole":true,"shareRole":true,"streamRole":true}}}"#;
        let out: Value =
            serde_json::from_str(&run(&state, &listener(), "getUser", "json", answer)).expect("json");
        let user = &out["subsonic-response"]["user"];
        assert_eq!(user["downloadRole"], false);
        assert_eq!(user["shareRole"], false);
        assert_eq!(user["adminRole"], false);
        assert_eq!(user["streamRole"], true);
        let xml = r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok"><user username="alex" adminRole="true" downloadRole="true" shareRole="true" /></subsonic-response>"#;
        let out = run(&state, &listener(), "getUser", "xml", xml);
        assert!(out.contains(r#"downloadRole="false""#), "{out}");
        assert!(out.contains(r#"adminRole="false""#), "{out}");
        // The owner's answer is Navidrome's.
        let owner = FamilyCaller::unmanaged("winters", true);
        assert_eq!(run(&state, &owner, "getUser", "json", answer), answer);
        // JSONP keeps its callback.
        let jsonp = format!("cb({answer});");
        let out = run(&state, &listener(), "getUser", "jsonp", &jsonp);
        assert!(out.starts_with("cb({") && out.ends_with("});"), "{out}");
        assert!(out.contains(r#""downloadRole":false"#), "{out}");
    }

    #[test]
    fn members_see_only_their_own_now_playing() {
        let state = state();
        let answer = json!({"subsonic-response":{"status":"ok","nowPlaying":{"entry":[
            {"id":"1","username":"alex"},{"id":"2","username":"sam"},{"id":"3","username":"Alex"}
        ]}}})
        .to_string();
        let out: Value =
            serde_json::from_str(&run(&state, &listener(), "getNowPlaying", "json", &answer)).expect("json");
        let ids: Vec<&str> = out["subsonic-response"]["nowPlaying"]["entry"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|e| e["id"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, ["1", "3"]);
        let xml = r#"<subsonic-response status="ok"><nowPlaying><entry id="1" username="alex" /><entry id="2" username="sam" /></nowPlaying></subsonic-response>"#;
        let out = run(&state, &listener(), "getNowPlaying", "xml", xml);
        assert!(out.contains(r#"id="1""#) && !out.contains(r#"id="2""#), "{out}");
        // The owner and a co-admin see everyone.
        let owner = FamilyCaller::unmanaged("winters", true);
        assert_eq!(run(&state, &owner, "getNowPlaying", "json", &answer), answer);
        let co = FamilyCaller::member("jo", Preset::CoAdmin, Abilities::preset(Preset::CoAdmin));
        assert_eq!(run(&state, &co, "getNowPlaying", "json", &answer), answer);
    }

    #[test]
    fn rules_read_the_format_from_the_answer_not_from_the_request() {
        let state = state();
        let json = r#"{"subsonic-response":{"status":"ok","nowPlaying":{"entry":[{"id":"1","username":"alex"},{"id":"2","username":"sam"}]}}}"#;
        let xml = r#"<subsonic-response status="ok"><nowPlaying><entry id="1" username="alex" /><entry id="2" username="sam" /></nowPlaying></subsonic-response>"#;
        let jsonp = format!("cb({json});");
        for asked in ["xml", "json", "jsonp", "json,xml", ""] {
            for answer in [json, xml, jsonp.as_str()] {
                let out = run(&state, &listener(), "getNowPlaying", asked, answer);
                assert!(
                    out.contains("alex") && !out.contains("sam"),
                    "asked {asked:?}: {out}"
                );
            }
            let out = run(
                &state,
                &listener(),
                "getUser",
                asked,
                r#"<subsonic-response status="ok"><user username="alex" adminRole="true" /></subsonic-response>"#,
            );
            assert!(out.contains(r#"adminRole="false""#), "asked {asked:?}: {out}");
        }
        // One entry written as an object is filtered too.
        let single =
            r#"{"subsonic-response":{"status":"ok","nowPlaying":{"entry":{"id":"2","username":"sam"}}}}"#;
        let out = run(&state, &listener(), "getNowPlaying", "json", single);
        assert!(!out.contains("sam"), "{out}");
    }

    #[test]
    fn an_answer_a_rule_must_change_and_cannot_read_is_error_0() {
        let state = state();
        for (answer, code) in [
            (r#"{"subsonic-response":{"nowPlaying":"#, r#""code":0"#),
            (
                r#"<subsonic-response><nowPlaying><entry username="sam">"#,
                r#"code="0""#,
            ),
            ("sam", r#"code="0""#),
            ("cb({not json});", r#""code":0"#),
        ] {
            let out = run(&state, &listener(), "getNowPlaying", "json", answer);
            assert!(!out.contains("sam"), "{answer}: {out}");
            assert!(out.contains(code), "{answer}: {out}");
        }
        // The owner's answer is no rule's to change, so it goes as it came.
        let owner = FamilyCaller::unmanaged("winters", true);
        assert_eq!(run(&state, &owner, "getNowPlaying", "json", "sam"), "sam");
    }

    fn saved(state: &AppState) {
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
        state.family_saved.add_album(
            "alex",
            SavedAlbum {
                id: "ext-deezer-album-9".into(),
                name: "Saved Album".into(),
                artist: "Artist".into(),
                cover_art: String::new(),
                song_count: 10,
                year: Some(2020),
                saved: Utc
                    .with_ymd_and_hms(2026, 10, 21, 18, 0, 0)
                    .single()
                    .expect("a time"),
            },
        );
    }

    #[test]
    fn saved_songs_join_starred2_json() {
        let state = state();
        saved(&state);
        let answer = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","starred2":{"song":[{"id":"lib-1","title":"Library Song"}]}}}"#;
        let out: Value =
            serde_json::from_str(&run(&state, &listener(), "getStarred2", "json", answer)).expect("json");
        let starred = &out["subsonic-response"]["starred2"];
        let songs = starred["song"].as_array().expect("songs");
        assert_eq!(songs.len(), 2);
        assert_eq!(songs[0]["id"], "lib-1");
        assert_eq!(songs[1]["id"], "ext-deezer-song-1");
        assert_eq!(songs[1]["title"], "Saved Song");
        assert_eq!(songs[1]["starred"], "2026-10-20T18:00:00.000Z");
        assert_eq!(songs[1]["isExternal"], true);
        let albums = starred["album"].as_array().expect("albums");
        assert_eq!(albums[0]["name"], "Saved Album");
        assert_eq!(albums[0]["starred"], "2026-10-21T18:00:00.000Z");
        // getStarred takes them too, and nobody else's answer changes.
        let plain = r#"{"subsonic-response":{"status":"ok","starred":{}}}"#;
        let out: Value =
            serde_json::from_str(&run(&state, &listener(), "getStarred", "json", plain)).expect("json");
        assert_eq!(
            out["subsonic-response"]["starred"]["song"][0]["id"],
            "ext-deezer-song-1"
        );
        let owner = FamilyCaller::unmanaged("winters", true);
        assert_eq!(run(&state, &owner, "getStarred2", "json", answer), answer);
    }

    #[test]
    fn saved_songs_join_starred2_xml() {
        let state = state();
        saved(&state);
        let answer = r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="1.16.1"><starred2><song id="lib-1" title="Library Song" /></starred2></subsonic-response>"#;
        let out = run(&state, &listener(), "getStarred2", "xml", answer);
        let root = XElement::parse(&out).expect("xml");
        let holder = root.elements().find(|e| e.name == "starred2").expect("starred2");
        let ids: Vec<(&str, &str)> = holder
            .elements()
            .map(|e| (e.name.as_str(), e.attribute("id").unwrap_or("")))
            .collect();
        assert_eq!(
            ids,
            [
                ("song", "lib-1"),
                ("album", "ext-deezer-album-9"),
                ("song", "ext-deezer-song-1")
            ]
        );
        let saved_song = holder
            .elements()
            .find(|e| e.attribute("id") == Some("ext-deezer-song-1"))
            .expect("there");
        assert_eq!(saved_song.attribute("starred"), Some("2026-10-20T18:00:00.000Z"));
        // An error answer is left alone.
        let failed = r#"<subsonic-response status="failed"><error code="40" /></subsonic-response>"#;
        assert_eq!(run(&state, &listener(), "getStarred2", "xml", failed), failed);
    }
}
