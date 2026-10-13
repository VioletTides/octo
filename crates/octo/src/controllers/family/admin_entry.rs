//! The dashboard's Family entry, behind the dashboard's own sign-in: a summary of the family,
//! turning Family on or off with the members folder's probe, and the owner's family notices.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post, put};
use indexmap::IndexMap;
use octo_core::family::abilities::Role;
use octo_core::json::dom::Node;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{body_as, family_error, ok, refusal};
use crate::app::AppState;
use crate::http::routes::RouteSet;
use crate::middleware::admin_sign_in_gate::{Caller, caller_of};
use crate::services::admin::{AdminCaller, AdminCallerKind, BrowseSessionStore};
use crate::services::family::members_place;

pub fn routes() -> RouteSet {
    RouteSet::new()
        .route("/api/admin/family", get(summary))
        .route("/api/admin/family/enable", post(enable))
        .route("/api/admin/family/notifications", put(notifications))
        .route("/api/admin/family/open", post(open))
}

/// `GET /api/admin/family`: what the dashboard's Family entry shows.
pub async fn summary(State(state): State<AppState>, caller: Caller) -> Response {
    if !caller_of(&caller).is_some_and(|c| c.is_admin()) {
        return refusal(StatusCode::FORBIDDEN, "This page is for the server owner.");
    }
    let settings = state.settings.current();
    let hooks = state.family.hooks(&state);
    let members: Vec<Value> = state
        .family_store
        .members()
        .iter()
        .map(|m| {
            json!({
                "username": m.username,
                "displayName": m.shown_name(),
                "role": Role::from(m.preset),
                "suspended": m.suspended,
                "joined": !m.account_password.is_empty(),
                "devices": state.family_store.devices_of(&m.username).len(),
            })
        })
        .collect();
    let skips = members_place::navidrome_skips_dot_folders(&state).await;
    let place = members_place::place(&state, skips);
    let links = state.family.link_status(&state);
    ok(&json!({
        "supported": true,
        "enabled": settings.family.enabled,
        "owner": state.family_store.owner(),
        "membersPath": place.path,
        "membersPlace": place,
        "navidromeSkipsDotFolders": members_place::dot_folders_json(skips),
        "links": links.as_ref().map(|status| status.links),
        "linksMessage": links.as_ref().and_then(|status| status.message.clone()),
        "linksFix": links.as_ref().and_then(|status| status.fix.clone()),
        "members": members,
        "devices": state.family_store.devices().len(),
        "pendingRequests": state.family_requests.pending().len(),
        "liveStreams": hooks.streams.count(),
        "manageFromAway": settings.family.manage_from_away,
        "notifyRequests": settings.notifications.notify_family_requests,
        "notifyDevices": settings.notifications.notify_family_devices,
        "familyPage": "/family/",
        "loadWarning": state.family_store.load_warning(),
        "publicUrl": crate::services::family::public_address::public_url(&state),
        "suggestedPublicUrl": state.family.suggested_public_url(),
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EnableBody {
    pub enabled: Option<bool>,
    /// Where members' music lives: `NextToMusic` or a full folder path. Absent keeps the setting.
    #[serde(rename = "membersPath")]
    pub members_path: Option<String>,
}

/// `POST /api/admin/family/enable`: the signed-in Navidrome admin turns Family on (becoming its
/// owner when it has none) or off.
pub async fn enable(State(state): State<AppState>, caller: Caller, body: Bytes) -> Response {
    let Some(admin) = caller_of(&caller).filter(|c| c.is_admin()) else {
        return refusal(StatusCode::FORBIDDEN, "This page is for the server owner.");
    };
    let request: EnableBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    let Some(on) = request.enabled else {
        return refusal(StatusCode::BAD_REQUEST, "Say enabled true or false.");
    };
    let owner = state.family_store.owner();
    let by = match (&admin.kind, admin.user.as_deref()) {
        (AdminCallerKind::Recovery, _) | (_, Some(BrowseSessionStore::RECOVERY_USER)) => {
            return refusal(
                StatusCode::FORBIDDEN,
                "Sign in with your Navidrome admin account to turn Family on or off.",
            );
        }
        (_, Some(user)) => user.to_string(),
        (_, None) if !owner.is_empty() => owner,
        (_, None) => {
            return refusal(
                StatusCode::FORBIDDEN,
                "Sign in with your Navidrome admin account to turn Family on, so Octo knows who the owner is.",
            );
        }
    };
    match state
        .family
        .enable(&state, &by, on, request.members_path.as_deref())
        .await
    {
        Ok(report) => ok(&json!(report)),
        Err(e) => family_error(&e),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NotificationsBody {
    pub requests: Option<bool>,
    pub devices: Option<bool>,
}

/// `PUT /api/admin/family/notifications`: whether the owner's channels hear of new requests
/// and new devices.
pub async fn notifications(State(state): State<AppState>, caller: Caller, body: Bytes) -> Response {
    if !caller_of(&caller).is_some_and(|c| c.is_admin()) {
        return refusal(StatusCode::FORBIDDEN, "This page is for the server owner.");
    }
    let request: NotificationsBody = match body_as(&body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    let mut section = IndexMap::new();
    if let Some(on) = request.requests {
        section.insert("NotifyFamilyRequests".to_string(), Node::Bool(on));
    }
    if let Some(on) = request.devices {
        section.insert("NotifyFamilyDevices".to_string(), Node::Bool(on));
    }
    if section.is_empty() {
        return refusal(StatusCode::BAD_REQUEST, "Say requests or devices, true or false.");
    }
    if state.settings.settings_path().is_some() {
        let mut patch = IndexMap::new();
        patch.insert("Notifications".to_string(), Node::Object(section));
        if let Err(e) = state.settings_writer.merge(&patch, &[]) {
            return family_error(&crate::services::family::FamilyError::Unavailable(format!(
                "Octo could not save the setting: {e}"
            )));
        }
        let _ = state.settings.reload_now();
    } else {
        // A store with no settings file (tests): the running value is all there is.
        let mut settings = (*state.settings.current()).clone();
        if let Some(on) = request.requests {
            settings.notifications.notify_family_requests = on;
        }
        if let Some(on) = request.devices {
            settings.notifications.notify_family_devices = on;
        }
        state.settings.set(settings);
    }
    ok(&json!({
        "notifyRequests": state.settings.current().notifications.notify_family_requests,
        "notifyDevices": state.settings.current().notifications.notify_family_devices,
    }))
}

/// `POST /api/admin/family/open`: a single-use token (60 seconds) that signs this dashboard's
/// admin in on the Family page, which reads it from `/family/#handoff=<token>`. Only a
/// dashboard session signed in as a Navidrome admin gets one.
pub async fn open(State(state): State<AppState>, caller: Caller) -> Response {
    let user = match caller_of(&caller) {
        Some(AdminCaller {
            kind: AdminCallerKind::Dashboard,
            user: Some(user),
        }) if user != BrowseSessionStore::RECOVERY_USER => user.clone(),
        _ => {
            return refusal(
                StatusCode::FORBIDDEN,
                "Sign in to the dashboard with your Navidrome admin account to open Family signed in.",
            );
        }
    };
    let token = state
        .family
        .mint_handoff(&user, crate::services::family::family_service::HANDOFF_TTL);
    ok(&json!({ "token": token, "familyPage": "/family/" }))
}
