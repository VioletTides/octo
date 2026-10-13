//! The Recently removed routes: the trash and Put back on the dashboard, for admins whose
//! listeners use other Subsonic apps. `LibraryEditService` answers both; the download log and
//! Find songs are in `downloads`.
//!
//! Everything here names files or people, or changes the library, so every call needs a
//! Navidrome admin's own dashboard sign-in (never the recovery code) and acts as that person: the
//! library actions gates (switched on, on the allowed list, dry run only rehearses), exactly as in
//! the apps.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Response;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use super::json_body::bind_optional_body;
use super::json_body::{FromBody, Shape, text_record};
use super::plumbing::{BROWSE_COOKIE_NAME, BROWSE_TOKEN_HEADER, ok, request_cookie};
use super::request_binding::{SIGN_IN_FIRST, error, split, utc, utc_opt};
use crate::app::AppState;
use crate::http::routes::RouteSet;
use crate::services::subsonic::subsonic_response_builder::with_replaced;

pub fn routes() -> RouteSet {
    RouteSet::new()
        .route("/api/admin/trash", get(trash))
        .route("/api/admin/trash/restore", post(restore))
}

/// The Navidrome admin signed in on this browser, or None (no sign-in, or the recovery code).
fn admin(state: &AppState, parts: &Parts) -> Option<String> {
    let token = request_cookie(&parts.headers, BROWSE_COOKIE_NAME).or_else(|| {
        parts
            .headers
            .get(BROWSE_TOKEN_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    });
    state.browse_sessions.navidrome_user_of(token.as_deref())
}

fn sign_in() -> Response {
    error(StatusCode::UNAUTHORIZED, SIGN_IN_FIRST)
}

/// Why this admin may not see or put back removed songs, or None when they may.
fn trash_refusal(state: &AppState, user: &str) -> Option<String> {
    state.library_edits.refusal(Some(user), true)
}

/// The songs removed from the library that are still in the server's trash, newest first: who
/// removed each and when, when the sweep deletes it for good, the files of its own that went
/// with it, and whether the removal keeps it from being downloaded again.
async fn trash(State(state): State<AppState>, request: Request) -> Response {
    let (parts, _) = request.into_parts();
    let Some(user) = admin(&state, &parts) else {
        return sign_in();
    };
    if let Some(refused) = trash_refusal(&state, &user) {
        return error(StatusCode::FORBIDDEN, refused);
    }
    let settings = state.settings.current().library_actions.clone();
    let songs: Vec<_> = state
        .library_edits
        .trash()
        .into_iter()
        .map(|song| {
            let mut json = json!({
                "id": song.id,
                "title": song.title,
                "artist": song.artist,
                "album": song.album,
                "removedBy": song.username,
                "removedAt": utc(&song.removed_utc),
                "goneAt": utc_opt(&song.gone_utc),
                "sidecars": song.sidecars,
                "key": song.key,
                "blocksDownloads": song.blocks_downloads,
            });
            with_replaced(&mut json, &song);
            json
        })
        .collect();
    ok(&json!({
        "keepDays": settings.effective_quarantine_retention_days(),
        "dryRun": settings.dry_run,
        "songs": songs,
    }))
}

/// `RestoreRequest(string? Id)`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestoreRequest {
    id: Option<String>,
}

impl FromBody for RestoreRequest {
    const SHAPE: &'static Shape =
        text_record!("Octo.Controllers.AdminDownloadsController+RestoreRequest", "id");
}

/// Puts a removed song back where it was, with its own files, as the signed-in admin.
async fn restore(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = split(request).await;
    let request: Option<RestoreRequest> = match bind_optional_body(&parts.headers, &body) {
        Ok(r) => r,
        Err(answer) => return answer,
    };
    let Some(user) = admin(&state, &parts) else {
        return sign_in();
    };
    if let Some(refused) = trash_refusal(&state, &user) {
        return error(StatusCode::FORBIDDEN, refused);
    }
    let Some(id) = request
        .and_then(|request| request.id)
        .filter(|id| !id.trim().is_empty())
    else {
        return error(StatusCode::BAD_REQUEST, "Which song? The id is missing.");
    };
    // Not tied to the request: a move that has started finishes.
    let edits = state.library_edits.clone();
    let (task_id, task_user) = (id.trim().to_string(), user.clone());
    let outcome = tokio::spawn(async move { edits.restore(&task_id, &task_user).await })
        .await
        .unwrap_or_else(|e| {
            crate::services::library::LibraryEditOutcome::new(
                "failed",
                format!("The change did not finish ({e})."),
            )
        });
    info!(
        "{user} asked to put back {id} from the dashboard: {} - {}",
        outcome.state,
        outcome.detail.as_deref().unwrap_or("")
    );
    ok(&json!({ "state": outcome.state, "detail": outcome.detail }))
}

#[cfg(test)]
#[path = "trash_tests.rs"]
mod tests;
