//! `octoFamily` v1: Octo Family for the apps, on `/rest`, signed like any Subsonic call. The
//! family gate has already signed members in; the owner and accounts outside the family are
//! checked here through the gate's reading, which has Navidrome check exactly the sign-in the
//! gate read. Every answer is a normal `subsonic-response` with
//! `"type":"octo"`, JSON or XML (XML writes scalars as attributes and lists as elements).
//! While Family is off these calls reach Navidrome as any unknown call would.
//!
//! Everyone signs in with their one login. `redeemFamilySignIn` and `getFamilySignInRedeem` are
//! open: a new device redeeming a sign-in hand-over has no sign-in yet. Wrong hand-over tokens
//! count per peer, and each account takes a bounded number of redemptions.

use crate::services::family::public_address::{self, LinkBases};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use octo_core::common::dotnet::eq_ignore_case;
use octo_core::family::abilities::RequestQuality;
use octo_core::family::device::{ClientOrigin, NetworkPlace, client_origin};
use octo_core::family::request::RequestKind;
use octo_subsonic::subsonic_response_builder::{
    SUBSONIC_NAMESPACE, SUBSONIC_VERSION, SubsonicReply, json_shape_to_xml,
};
use octo_subsonic::xml::XElement;
use serde_json::{Value, json};

use super::caller_checks::SubsonicCall;
use crate::app::AppState;
use crate::controllers::family::account::{quality_choice, quality_mode, set_preference};
use crate::controllers::family::manage::request_state;
use crate::http::routes::RouteSet;
use crate::middleware::family_gate::{GateReading, Unverified};
use crate::middleware::forwarded::RequestScheme;
use crate::services::family::family_service::{Actor, FamilyError, NewRequest, quality_json};
use crate::services::family::{FamilyCaller, FamilyService};

pub fn routes() -> RouteSet {
    RouteSet::new()
        .subsonic("getFamily", get(get_family).post(get_family))
        .subsonic(
            "setFamilyQuality",
            get(set_family_quality).post(set_family_quality),
        )
        .subsonic(
            "setFamilyDeviceQuality",
            get(set_family_device_quality).post(set_family_device_quality),
        )
        .subsonic(
            "getFamilyRequests",
            get(get_family_requests).post(get_family_requests),
        )
        .subsonic("requestCopy", get(request_copy).post(request_copy))
        .subsonic(
            "cancelFamilyRequest",
            get(cancel_family_request).post(cancel_family_request),
        )
        .subsonic(
            "decideFamilyRequest",
            get(decide_family_request).post(decide_family_request),
        )
        .subsonic(
            "getFamilyDevices",
            get(get_family_devices).post(get_family_devices),
        )
        .subsonic(
            "forgetFamilyDevice",
            get(forget_family_device).post(forget_family_device),
        )
        .subsonic("getFamilyLogin", get(get_family_login).post(get_family_login))
        .subsonic(
            "changeFamilyPassword",
            get(change_family_password).post(change_family_password),
        )
        .subsonic(
            "removeFromMyLibrary",
            get(remove_from_my_library).post(remove_from_my_library),
        )
        .subsonic(
            "createFamilyPlaylist",
            get(create_family_playlist).post(create_family_playlist),
        )
        .subsonic(
            "startFamilySignIn",
            get(start_family_sign_in).post(start_family_sign_in),
        )
        .subsonic(
            "redeemFamilySignIn",
            get(redeem_family_sign_in).post(redeem_family_sign_in),
        )
        .subsonic(
            "getFamilySignInPending",
            get(get_family_sign_in_pending).post(get_family_sign_in_pending),
        )
        .subsonic(
            "decideFamilySignIn",
            get(decide_family_sign_in).post(decide_family_sign_in),
        )
        .subsonic(
            "putFamilySignInBox",
            get(put_family_sign_in_box).post(put_family_sign_in_box),
        )
        .subsonic(
            "getFamilySignInRedeem",
            get(get_family_sign_in_redeem).post(get_family_sign_in_redeem),
        )
}

/// The extension's name and versions, as `getOpenSubsonicExtensions` lists it.
pub const FAMILY_EXTENSION: (&str, i32) = ("octoFamily", 1);

/// Adds `octoFamily` to a `getOpenSubsonicExtensions` answer (an ok one; a failed or unreadable
/// answer goes out as it came).
pub fn with_family_extension(reply: SubsonicReply, format: &str) -> SubsonicReply {
    let (name, version) = FAMILY_EXTENSION;
    if format.eq_ignore_ascii_case("json") {
        let Some(mut value) = reply.json_value() else {
            return reply;
        };
        let Some(envelope) = value.get_mut("subsonic-response").and_then(Value::as_object_mut) else {
            return reply;
        };
        if envelope.get("status").and_then(Value::as_str) != Some("ok") {
            return reply;
        }
        let list = envelope
            .entry("openSubsonicExtensions")
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(list) = list.as_array_mut() else {
            return reply;
        };
        if !list
            .iter()
            .any(|item| item.get("name").and_then(Value::as_str) == Some(name))
        {
            list.push(json!({ "name": name, "versions": [version] }));
        }
        return SubsonicReply::json(&value);
    }
    let Ok(mut response) = XElement::parse(&reply.text()) else {
        return reply;
    };
    if response.attribute("status") != Some("ok") {
        return reply;
    }
    let ns = response.namespace.clone();
    let listed = response
        .elements_named(ns.as_deref(), "openSubsonicExtensions")
        .any(|e| e.attribute("name") == Some(name));
    if !listed {
        response.push(
            XElement::in_namespace(ns.as_deref(), "openSubsonicExtensions")
                .attr("name", name)
                .child(XElement::in_namespace(ns.as_deref(), "versions").text(version.to_string())),
        );
    }
    SubsonicReply::content(response.to_xml_string(), &reply.content_type)
}

/// `json` for `json` and `jsonp`, else `xml`.
fn format_of(call: &SubsonicCall) -> String {
    match call.format().to_ascii_lowercase().as_str() {
        "json" | "jsonp" => "json".to_string(),
        _ => "xml".to_string(),
    }
}

/// An ok answer of Octo's own carrying `key` (or nothing).
pub fn octo_reply(state: &AppState, format: &str, key: Option<(&str, &Value)>) -> Response {
    if format == "json" {
        let mut body = json!({
            "status": "ok",
            "version": SUBSONIC_VERSION,
            "type": "octo",
            "openSubsonic": true,
        });
        if let (Some((key, value)), Value::Object(map)) = (key, &mut body) {
            map.insert(key.to_string(), value.clone());
        }
        return state
            .subsonic_response_builder
            .create_json_response(body)
            .into_response();
    }
    let mut envelope = XElement::ns(SUBSONIC_NAMESPACE, "subsonic-response")
        .attr("status", "ok")
        .attr("version", SUBSONIC_VERSION)
        .attr("type", "octo")
        .attr("openSubsonic", "true");
    if let Some((key, value)) = key {
        envelope.push(json_shape_to_xml(Some(SUBSONIC_NAMESPACE), key, value));
    }
    SubsonicReply::xml(&envelope).into_response()
}

fn error(state: &AppState, format: &str, code: i32, message: &str) -> Response {
    state
        .subsonic_response_builder
        .create_error(format, code, message)
        .into_response()
}

fn family_failure(state: &AppState, format: &str, e: &FamilyError) -> Response {
    error(state, format, e.subsonic_code(), e.message())
}

/// One `octoFamily` call: its parameters, its caller and the format to answer in.
struct FamilyCall {
    call: SubsonicCall,
    caller: FamilyCaller,
    format: String,
    origin: String,
}

impl FamilyCall {
    fn actor(&self) -> Actor {
        Actor {
            user: self.caller.user.clone(),
            role: self.caller.role,
        }
    }

    fn param(&self, key: &str) -> &str {
        self.call.param(key).trim()
    }

    fn flag(&self, key: &str) -> bool {
        eq_ignore_case(self.param(key), "true") || self.param(key) == "1"
    }
}

/// The address the client reached Octo by: `scheme://host`.
fn origin_of(req: &Request) -> String {
    let https = req
        .extensions()
        .get::<RequestScheme>()
        .is_some_and(|s| s.0.eq_ignore_ascii_case("https"));
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .unwrap_or("localhost")
        .to_string();
    format!("{}://{host}", if https { "https" } else { "http" })
}

/// Starts a call: hands it to the catch-all while Family is off, and checks who is asking.
#[allow(clippy::result_large_err)] // the error is the answer itself, sent at once
async fn begin(state: &AppState, req: Request) -> Result<FamilyCall, Response> {
    if !state.settings.current().family.enabled {
        return Err(crate::http::catch_all::catch_all(State(state.clone()), req).await);
    }
    let reading = GateReading::of(req.extensions()).cloned();
    let origin = origin_of(&req);
    let call = SubsonicCall::read(state, req).await?;
    let format = format_of(&call);
    // Only the gate checks who is asking, with exactly the sign-in it read: a member's against
    // their devices, anyone else's with Navidrome (which notes their device when it checks out).
    let verified = match reading {
        Some(reading) => reading.verify(state).await,
        None => Err(Unverified::Missing),
    };
    let caller = match verified {
        Ok(verified) => verified.caller().clone(),
        Err(Unverified::Missing) => {
            return Err(error(state, &format, 10, "Required parameter is missing: u"));
        }
        Err(Unverified::Refused) => {
            return Err(error(state, &format, 40, "Wrong username or password"));
        }
        Err(Unverified::Unreachable) => {
            return Err(error(
                state,
                &format,
                0,
                "Octo can't reach Navidrome to check who is asking",
            ));
        }
    };
    Ok(FamilyCall {
        call,
        caller,
        format,
        origin,
    })
}

macro_rules! started {
    ($state:expr, $req:expr) => {
        match begin(&$state, $req).await {
            Ok(call) => call,
            Err(res) => return res,
        }
    };
}

/// Whether the caller manages the family from where they are now.
fn manages_here(state: &AppState, call: &FamilyCall) -> bool {
    FamilyService::may_manage(state, &call.actor()) && may_act_from_here(state, call)
}

fn approves_here(state: &AppState, call: &FamilyCall) -> bool {
    FamilyService::may_approve(state, &call.actor()) && may_act_from_here(state, call)
}

fn may_act_from_here(state: &AppState, call: &FamilyCall) -> bool {
    call.caller.place == NetworkPlace::Home || state.settings.current().family.manage_from_away
}

/// `getFamily`.
pub async fn get_family(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let me = state.family.me_json(
        &state,
        &call.caller.user,
        call.caller.device_id.as_deref(),
        call.caller.place,
    );
    let mut family = json!({ "me": me });
    if manages_here(&state, &call)
        && let Some(manager) = state.family.manager_json(&state, &call.actor())
        && let Value::Object(map) = &mut family
    {
        map.insert("manager".into(), manager);
    }
    octo_reply(&state, &call.format, Some(("family", &family)))
}

/// `setFamilyQuality`: the account's home and away quality (either may be left out).
pub async fn set_family_quality(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let (home, away) = match (
        quality_choice(call.call.param_opt("home")),
        quality_choice(call.call.param_opt("away")),
    ) {
        (Ok(home), Ok(away)) => (home, away),
        (Err(words), _) | (_, Err(words)) => return error(&state, &call.format, 10, words),
    };
    let preference = set_preference(&state, &call.caller.user, home, away);
    let quality = quality_json(
        &preference,
        &FamilyService::abilities_of(&state, &call.caller.user),
    );
    octo_reply(&state, &call.format, Some(("quality", &quality)))
}

/// `setFamilyDeviceQuality`: `id` (own device), `mode` (`Account` or `App`).
pub async fn set_family_device_quality(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let id = call.param("id");
    if id.is_empty() {
        return error(&state, &call.format, 10, "Required parameter is missing: id");
    }
    let Some(mode) = quality_mode(call.param("mode")) else {
        return error(&state, &call.format, 10, "Choose mode Account or App.");
    };
    match state
        .family
        .set_device(&state, &call.caller.user, id, Some(mode), None)
    {
        Ok(device) => {
            let device = state
                .family
                .device_json(&state, &device, call.caller.device_id.as_deref());
            octo_reply(&state, &call.format, Some(("device", &device)))
        }
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `getFamilyRequests`: the caller's own, or everyone's (`all`, approvers).
pub async fn get_family_requests(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let all = call.flag("all");
    if all && !approves_here(&state, &call) {
        return error(
            &state,
            &call.format,
            50,
            "Only people who approve requests can see everyone's.",
        );
    }
    let filter = match call.param("state") {
        "" => None,
        text => match request_state(text) {
            Some(found) => Some(found),
            None => return error(&state, &call.format, 10, "Unknown request state."),
        },
    };
    let user = (!all).then(|| call.caller.user.clone());
    let list: Vec<Value> = FamilyService::requests(&state, user.as_deref(), filter)
        .iter()
        .map(|r| FamilyService::request_json(&state, r))
        .collect();
    octo_reply(
        &state,
        &call.format,
        Some(("familyRequests", &json!({ "request": list }))),
    )
}

/// Reads a request kind by name, any case.
fn request_kind(text: &str) -> Option<RequestKind> {
    serde_json::from_value(Value::String(
        ["Song", "Album", "Upgrade", "Import", "PlaylistAdd"]
            .into_iter()
            .find(|k| k.eq_ignore_ascii_case(text.trim()))?
            .to_string(),
    ))
    .ok()
}

/// `requestCopy`: `id`, `quality` (default Best), `kind` (optional).
pub async fn request_copy(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    if !call.caller.managed {
        return error(&state, &call.format, 50, "Requests are for family members.");
    }
    let id = call.param("id");
    if id.is_empty() {
        return error(&state, &call.format, 10, "Required parameter is missing: id");
    }
    let quality = match call.param("quality") {
        "" => RequestQuality::Best,
        text => match RequestQuality::parse(text) {
            Some(quality) => quality,
            None => return error(&state, &call.format, 10, "Choose quality Best, Flac or Mp3."),
        },
    };
    let kind = match call.param("kind") {
        "" => None,
        text => match request_kind(text) {
            Some(kind) => Some(kind),
            None => return error(&state, &call.format, 10, "Unknown request kind."),
        },
    };
    let hooks = state.family.hooks(&state);
    match hooks
        .requests
        .create(NewRequest {
            username: call.caller.user.clone(),
            target: id.to_string(),
            quality,
            kind,
        })
        .await
    {
        Ok(made) => {
            let value = FamilyService::request_json(&state, &made);
            octo_reply(&state, &call.format, Some(("familyRequest", &value)))
        }
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `cancelFamilyRequest`: the requester's own pending request.
pub async fn cancel_family_request(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let id = call.param("id");
    if id.is_empty() {
        return error(&state, &call.format, 10, "Required parameter is missing: id");
    }
    match state
        .family
        .hooks(&state)
        .requests
        .cancel(id, &call.caller.user)
        .await
    {
        Ok(request) => {
            let value = FamilyService::request_json(&state, &request);
            octo_reply(&state, &call.format, Some(("familyRequest", &value)))
        }
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `decideFamilyRequest`: `id`, `approve`, `note`; needs Approve requests.
pub async fn decide_family_request(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    if !approves_here(&state, &call) {
        return error(
            &state,
            &call.format,
            50,
            "Only people who approve requests can do that.",
        );
    }
    let id = call.param("id");
    if id.is_empty() || call.param("approve").is_empty() {
        return error(
            &state,
            &call.format,
            10,
            "Required parameter is missing: id or approve",
        );
    }
    let approve = call.flag("approve");
    match state
        .family
        .hooks(&state)
        .requests
        .decide(id, approve, &call.caller.user, call.param("note"))
        .await
    {
        Ok(request) => {
            let value = FamilyService::request_json(&state, &request);
            octo_reply(&state, &call.format, Some(("familyRequest", &value)))
        }
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `getFamilyDevices`: the caller's own, or everyone's (`all`, managers).
pub async fn get_family_devices(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let all = call.flag("all");
    if all && !manages_here(&state, &call) {
        return error(
            &state,
            &call.format,
            50,
            "Only people who manage the family can see everyone's devices.",
        );
    }
    let devices = if all {
        FamilyService::all_devices(&state)
    } else {
        FamilyService::devices_of(&state, &call.caller.user)
    };
    let list: Vec<Value> = devices
        .iter()
        .map(|d| {
            state
                .family
                .device_json(&state, d, call.caller.device_id.as_deref())
        })
        .collect();
    octo_reply(
        &state,
        &call.format,
        Some(("familyDevices", &json!({ "device": list }))),
    )
}

/// `forgetFamilyDevice`: `id`, "Remove from list". Own devices, or any member's for managers.
/// The device is recorded again if it signs in again.
pub async fn forget_family_device(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let id = call.param("id");
    if id.is_empty() {
        return error(&state, &call.format, 10, "Required parameter is missing: id");
    }
    let manager = manages_here(&state, &call);
    match state.family.forget_device(&state, &call.actor(), id, manager) {
        Ok(()) => octo_reply(&state, &call.format, None),
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `getFamilyLogin`: the caller's username and the server addresses to sign in with anywhere.
pub async fn get_family_login(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let bases = LinkBases::new(
        &state,
        &call.origin,
        public_address::away_allowed(&state, &call.caller.user),
    );
    let login = FamilyService::login_json(&state, &call.caller.user, &bases);
    octo_reply(&state, &call.format, Some(("familyLogin", &login)))
}

/// `changeFamilyPassword` (form POST): `current`, `next`. "Sign out everywhere": every app
/// needs the new password, and everything else signed in with the old one ends. A wrong
/// current password counts as a wrong sign-in from where the call came.
pub async fn change_family_password(State(state): State<AppState>, req: Request) -> Response {
    let from = origin_of_client(&state, &req);
    let call = started!(state, req);
    if !call.caller.managed {
        return error(
            &state,
            &call.format,
            50,
            "Change this account's password in Navidrome.",
        );
    }
    let keys = state.family_bridge.try_keys(
        &from.address,
        &from.peer,
        &call.caller.user,
        crate::services::family::member_bridge::MAX_FAILURES,
    );
    match state
        .family
        .change_password(
            &state,
            &call.caller.user,
            call.call.param("current"),
            call.call.param("next"),
            &keys,
        )
        .await
    {
        Ok(()) => octo_reply(&state, &call.format, None),
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `removeFromMyLibrary`: `id`, a song in the caller's own library.
pub async fn remove_from_my_library(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    if !call.caller.managed {
        return error(
            &state,
            &call.format,
            50,
            "Only family members have a library of their own.",
        );
    }
    let id = call.param("id");
    if id.is_empty() {
        return error(&state, &call.format, 10, "Required parameter is missing: id");
    }
    match state
        .family
        .hooks(&state)
        .my_library
        .remove(&call.caller.user, id)
        .await
    {
        Ok(()) => octo_reply(&state, &call.format, None),
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `createFamilyPlaylist`: `name`, `approveAdditions` (managers); answers the playlist node.
pub async fn create_family_playlist(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let name = call.param("name");
    if name.is_empty() {
        return error(&state, &call.format, 10, "Required parameter is missing: name");
    }
    if !FamilyService::abilities_of(&state, &call.caller.user).family_playlists_edit {
        return error(
            &state,
            &call.format,
            50,
            "Family playlists cannot be changed from this account.",
        );
    }
    let approve = call.flag("approveAdditions");
    if approve && !manages_here(&state, &call) {
        return error(
            &state,
            &call.format,
            50,
            "Only people who manage the family can ask for approval of additions.",
        );
    }
    match state
        .family
        .hooks(&state)
        .playlists
        .create(name, &call.caller.user, approve)
        .await
    {
        Ok(playlist) => {
            let node = json!({
                "id": playlist.id,
                "name": playlist.name,
                "comment": "",
                "owner": "Family",
                "public": true,
                "songCount": playlist.song_count,
                "duration": 0,
                "created": playlist.created,
                "changed": playlist.created,
                "entry": [],
            });
            octo_reply(&state, &call.format, Some(("playlist", &node)))
        }
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// Where a request comes from: the client address, and the peer Octo can vouch for.
fn origin_of_client(state: &AppState, req: &Request) -> ClientOrigin {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let forwarded: Vec<String> = req
        .headers()
        .get_all("x-forwarded-for")
        .iter()
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .collect();
    let forwarded = (!forwarded.is_empty()).then(|| forwarded.join(","));
    let settings = state.settings.current();
    client_origin(
        peer,
        forwarded.as_deref(),
        &settings.family.trusted_proxy_list(),
        &settings.family.home_network_list(),
    )
}

// --- signing in on another device with a QR code ---------------------------------------------

/// `startFamilySignIn`: a hand-over token for a QR code (any signed-in account).
pub async fn start_family_sign_in(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let answer = crate::controllers::family::handover::start_json(&state, &call.caller.user, &call.origin);
    octo_reply(&state, &call.format, Some(("familySignIn", &answer)))
}

/// `getFamilySignInPending`: `token`; the new device waiting for an answer, if any.
pub async fn get_family_sign_in_pending(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    match state
        .family
        .handover
        .pending(&call.caller.user, call.param("token"))
    {
        Ok(pending) => octo_reply(
            &state,
            &call.format,
            Some(("familySignInPending", &json!(pending))),
        ),
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `decideFamilySignIn`: `token`, `allow` (true or false).
pub async fn decide_family_sign_in(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    let allow = call.flag("allow");
    match state
        .family
        .handover
        .decide(&call.caller.user, call.param("token"), allow)
    {
        Ok(decided) => octo_reply(
            &state,
            &call.format,
            Some(("familySignInDecided", &json!({ "state": decided }))),
        ),
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `putFamilySignInBox` (form POST): `token`, `box` (the sealed sign-in, at most 16 KB).
pub async fn put_family_sign_in_box(State(state): State<AppState>, req: Request) -> Response {
    let call = started!(state, req);
    match state
        .family
        .handover
        .put_box(&call.caller.user, call.param("token"), call.call.param("box"))
    {
        Ok(()) => octo_reply(&state, &call.format, None),
        Err(e) => family_failure(&state, &call.format, &e),
    }
}

/// `redeemFamilySignIn` (open): `token`, `deviceName`, `platform`; answers the `id` to fetch
/// the sealed sign-in with.
pub async fn redeem_family_sign_in(State(state): State<AppState>, req: Request) -> Response {
    if !state.settings.current().family.enabled {
        return crate::http::catch_all::catch_all(State(state.clone()), req).await;
    }
    let from = origin_of_client(&state, &req);
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(res) => return res,
    };
    let format = format_of(&call);
    let token = call.param("token").trim();
    if token.is_empty() {
        return error(&state, &format, 10, "Required parameter is missing: token");
    }
    match state.family.handover.redeem(
        &from.peer.to_string(),
        token,
        call.param("deviceName"),
        call.param("platform"),
    ) {
        Ok(id) => octo_reply(
            &state,
            &format,
            Some(("familySignInRedeemed", &json!({ "id": id }))),
        ),
        Err(FamilyError::Refused(message)) => error(&state, &format, 40, &message),
        Err(e) => family_failure(&state, &format, &e),
    }
}

/// `getFamilySignInRedeem` (open): `id`; where the hand-over stands, and the sealed box once it
/// is there (handed out once).
pub async fn get_family_sign_in_redeem(State(state): State<AppState>, req: Request) -> Response {
    if !state.settings.current().family.enabled {
        return crate::http::catch_all::catch_all(State(state.clone()), req).await;
    }
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(res) => return res,
    };
    let format = format_of(&call);
    let fetched = state.family.handover.fetch(call.param("id").trim());
    octo_reply(&state, &format, Some(("familySignInRedeem", &json!(fetched))))
}
