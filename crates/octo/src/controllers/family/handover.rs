//! Signing in on another device with a QR code, on the Family page's API. A signed-in app
//! starts a hand-over and shows a QR of `<base>/family/signin#t=<token>&k=<key>&s=<server>`
//! (plus `&h=<home>`); the key stays in the fragment and never reaches Octo. The new device
//! redeems the token, the first app allows or denies, uploads the sealed box on allow, and the
//! new device fetches it once. See [`crate::services::family::handover`].
//!
//! - `POST /api/family/signin/start` (signed in, any account)
//! - `POST /api/family/signin/redeem {token, deviceName, platform}` (open, throttled)
//! - `GET /api/family/signin/{token}/pending` (the starter)
//! - `POST /api/family/signin/{token}/decide {allow}` (the starter)
//! - `POST /api/family/signin/{token}/box {box}` (the starter, after allow; at most 16 KB)
//! - `GET /api/family/signin/redeem/{id}` (the new device)

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Web, body_as, family_error, no_content, ok, refusal, require_on};
use crate::app::AppState;
use crate::http::routes::RouteSet;
use crate::services::family::handover::MAX_BOX;
use crate::services::family::public_address::{self, LinkBases};

pub fn routes() -> RouteSet {
    RouteSet::new()
        .route("/api/family/signin/start", post(start))
        .route(
            "/api/family/signin/redeem",
            post(redeem).layer(DefaultBodyLimit::max(4 * 1024)),
        )
        .route("/api/family/signin/redeem/{id}", get(fetch))
        .route("/api/family/signin/{token}/pending", get(pending))
        .route("/api/family/signin/{token}/decide", post(decide))
        .route(
            "/api/family/signin/{token}/box",
            post(put_box).layer(DefaultBodyLimit::max(MAX_BOX + 1024)),
        )
}

/// A new hand-over for `user`: `{token, expires, links: {anywhere, home}, servers: {anywhere,
/// home}, anywhereAvailable}`. The links are the `/family/signin` page on each base, for the QR;
/// the app adds the fragment (`#t=..&k=..&s=..&h=..`), with `s` and `h` from `servers`.
pub fn start_json(state: &AppState, user: &str, origin: &str) -> Value {
    let (token, expires) = state.family.handover.start(user);
    let bases = LinkBases::new(state, origin, public_address::away_allowed(state, user));
    let anywhere = bases.anywhere.as_ref().filter(|_| bases.anywhere_available);
    json!({
        "token": token,
        "expires": octo_core::json::datetime::format_utc(&expires),
        "links": {
            "anywhere": anywhere.map(|base| format!("{base}/family/signin")),
            "home": format!("{}/family/signin", bases.home),
        },
        "servers": { "anywhere": anywhere, "home": bases.home },
        "anywhereAvailable": bases.anywhere_available,
    })
}

/// `POST /api/family/signin/start`.
pub async fn start(State(state): State<AppState>, web: Web) -> Response {
    if let Err(res) = require_on(&state) {
        return res;
    }
    let signed = match web.signed(&state).await {
        Ok(signed) => signed,
        Err(res) => return res,
    };
    ok(&start_json(&state, signed.user(), &web.origin))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RedeemBody {
    pub token: String,
    pub device_name: String,
    pub platform: String,
}

/// `POST /api/family/signin/redeem`: open. Answers `{id}`.
pub async fn redeem(State(state): State<AppState>, web: Web, body: Bytes) -> Response {
    if let Err(res) = require_on(&state) {
        return res;
    }
    let request: RedeemBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    if request.token.trim().is_empty() {
        return refusal(StatusCode::BAD_REQUEST, "Scan the code again.");
    }
    match state.family.handover.redeem(
        &web.peer.to_string(),
        &request.token,
        &request.device_name,
        &request.platform,
    ) {
        Ok(id) => ok(&json!({ "id": id })),
        Err(e) => family_error(&e),
    }
}

/// `GET /api/family/signin/redeem/{id}`: open. `{state, box?}`; the box is handed out once.
pub async fn fetch(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if let Err(res) = require_on(&state) {
        return res;
    }
    ok(&json!(state.family.handover.fetch(&id)))
}

/// `GET /api/family/signin/{token}/pending`: the new device waiting for an answer, or 204.
pub async fn pending(State(state): State<AppState>, web: Web, Path(token): Path<String>) -> Response {
    if let Err(res) = require_on(&state) {
        return res;
    }
    let signed = match web.signed(&state).await {
        Ok(signed) => signed,
        Err(res) => return res,
    };
    match state.family.handover.pending(signed.user(), &token) {
        Ok(Some(pending)) => ok(&json!(pending)),
        Ok(None) => no_content(),
        Err(e) => family_error(&e),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DecideBody {
    pub allow: Option<bool>,
}

/// `POST /api/family/signin/{token}/decide {allow}`.
pub async fn decide(
    State(state): State<AppState>,
    web: Web,
    Path(token): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(res) = require_on(&state) {
        return res;
    }
    let signed = match web.signed(&state).await {
        Ok(signed) => signed,
        Err(res) => return res,
    };
    let request: DecideBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    let Some(allow) = request.allow else {
        return refusal(StatusCode::BAD_REQUEST, "Say allow true or false.");
    };
    match state.family.handover.decide(signed.user(), &token, allow) {
        Ok(decided) => ok(&json!({ "state": decided })),
        Err(e) => family_error(&e),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct BoxBody {
    #[serde(rename = "box")]
    pub sealed: String,
}

/// `POST /api/family/signin/{token}/box {box}`: at most 16 KB, after allow.
pub async fn put_box(
    State(state): State<AppState>,
    web: Web,
    Path(token): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(res) = require_on(&state) {
        return res;
    }
    let signed = match web.signed(&state).await {
        Ok(signed) => signed,
        Err(res) => return res,
    };
    let request: BoxBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    match state
        .family
        .handover
        .put_box(signed.user(), &token, &request.sealed)
    {
        Ok(()) => no_content(),
        Err(e) => family_error(&e),
    }
}
