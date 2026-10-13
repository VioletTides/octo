//! Octo's outside address on the Family page, and the dashboard's sign-in handoff:
//!
//! - `PUT /api/family/public-address`: the owner saves `Server:PublicUrl`, the setting the
//!   dashboard's Status page shows, normalized the same way.
//! - `POST /api/family/public-address/check`: Octo asks that address for `/family/hello` and
//!   says whether this server answered.
//! - `GET /family/hello`: open; this running Octo's instance id.
//! - `POST /api/family/handoff`: a token from the dashboard signs its admin in here, once.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use indexmap::IndexMap;
use octo_core::json::dom::Node;
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use super::{Web, body_as, ok, refusal};
use crate::app::AppState;
use crate::services::admin::SessionKind;
use crate::services::family::public_address::{self, Reach};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AddressBody {
    pub url: Option<String>,
}

/// `PUT /api/family/public-address` `{url}`: saves the outside address (empty clears it).
pub async fn put_public_address(State(state): State<AppState>, web: Web, body: Bytes) -> Response {
    let signed = match web.owner(&state).await {
        Ok(signed) => signed,
        Err(res) => return res,
    };
    let request: AddressBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    let url = match public_address::validate_address(request.url.as_deref().unwrap_or_default()) {
        Ok(url) => url,
        Err(message) => return refusal(StatusCode::BAD_REQUEST, &message),
    };
    if let Err(message) = save_public_url(&state, &url) {
        return refusal(StatusCode::CONFLICT, &message);
    }
    info!(
        "FAMILY outside address {} by {}",
        if url.is_empty() { "cleared" } else { "saved" },
        signed.user()
    );
    ok(&json!({ "publicUrl": public_address::public_url(&state) }))
}

/// Writes `Server:PublicUrl` through the settings writer and makes it the running value.
fn save_public_url(state: &AppState, url: &str) -> Result<(), String> {
    if state.settings.settings_path().is_some() {
        let mut section = IndexMap::new();
        section.insert("PublicUrl".to_string(), Node::String(url.to_string()));
        let mut patch = IndexMap::new();
        patch.insert("Server".to_string(), Node::Object(section));
        state
            .settings_writer
            .merge(&patch, &[])
            .map_err(|e| format!("Octo could not save the address: {e}"))?;
        let _ = state.settings.reload_now();
        let now = public_address::public_url(state).unwrap_or_default();
        if now != url {
            return Err(
                "Server:PublicUrl is set in Octo's environment (SERVER__PUBLICURL), which wins over this page. Change it there."
                    .to_string(),
            );
        }
    } else {
        // A store with no settings file (tests): the running value is all there is.
        let mut settings = (*state.settings.current()).clone();
        settings.server.public_url = url.to_string();
        state.settings.set(settings);
    }
    Ok(())
}

/// `POST /api/family/public-address/check` `{url?}`: whether this Octo answers at `url`, or at
/// the saved address.
pub async fn check_public_address(State(state): State<AppState>, web: Web, body: Bytes) -> Response {
    if let Err(res) = web.owner(&state).await {
        return res;
    }
    let request: AddressBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    let url = match request.url.as_deref().map(public_address::validate_address) {
        Some(Ok(url)) if !url.is_empty() => url,
        Some(Err(message)) => return refusal(StatusCode::BAD_REQUEST, &message),
        _ => match public_address::public_url(&state) {
            Some(url) => url,
            None => return refusal(StatusCode::BAD_REQUEST, "Save an outside address first."),
        },
    };
    let reach = public_address::check(&state.http, &url, state.family.instance()).await;
    ok(
        &json!({ "url": url, "result": reach, "message": reach.message(), "reachable": reach == Reach::Reachable }),
    )
}

/// `GET /family/hello`: this running Octo's id, for the reachability check. Open to anyone,
/// whether or not Family is on.
pub async fn hello(State(state): State<AppState>) -> Response {
    let mut res = ok(&json!({ "octo": "family", "instance": state.family.instance() }));
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HandoffBody {
    pub token: String,
}

/// `POST /api/family/handoff` `{token}`: the dashboard's admin, signed in here with the
/// single-use token the dashboard made (`POST /api/admin/family/open`).
pub async fn handoff(State(state): State<AppState>, web: Web, body: Bytes) -> Response {
    let request: HandoffBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    let Some(user) = state.family.take_handoff(&request.token) else {
        return refusal(
            StatusCode::UNAUTHORIZED,
            "This sign-in link has run out. Sign in here, or open Family from the dashboard again.",
        );
    };
    let token = state
        .browse_sessions
        .create_family(&user, SessionKind::FamilyNavidrome);
    info!("FAMILY page sign-in for {user} (from the dashboard)");
    let mut res = ok(&json!({ "username": user }));
    res.headers_mut().append(header::SET_COOKIE, web.cookie(&token));
    res
}
