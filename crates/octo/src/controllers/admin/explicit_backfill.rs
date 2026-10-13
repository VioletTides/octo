//! The dashboard's "Mark explicit songs". Look the library up, then write the explicit and
//! clean marks the preview found, and undo them. It rewrites the owner's files, so every call
//! needs a Navidrome admin sign-in like the genre re-tag, and Apply needs the music path typed.
//!
//! The admin gate refuses a recovery-code session on every library-file route
//! (`LIBRARY_FILE_ROUTES`, `/api/admin/explicit`), and the session here is the Navidrome user's
//! (`navidrome_user_of`), as the cover upgrade's.

use std::cmp::Ordering;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Response;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use super::json_body::{FromBody, Shape, text_record};
use super::request_binding::{
    BROWSE_COOKIE_NAME, BROWSE_TOKEN_HEADER, bind_body, error, ok, query, request_cookie, sign_in, split,
    status, utc_opt,
};
use crate::app::AppState;
use crate::http::routes::RouteSet;
use crate::services::library::explicit_backfill::{
    ExplicitBackfill, ExplicitBackfillMode, ExplicitBackfillRequest, ExplicitBackfillStatus, writes,
};
use crate::services::library::library_write_gate::LibraryWriteGate;

pub fn routes() -> RouteSet {
    RouteSet::new()
        .route("/api/admin/explicit/backfill", get(get_run))
        .route("/api/admin/explicit/backfill/preview", post(preview))
        .route("/api/admin/explicit/backfill/resume", post(resume))
        .route("/api/admin/explicit/backfill/apply", post(apply))
        .route("/api/admin/explicit/backfill/cancel", post(cancel))
        .route("/api/admin/explicit/backfill/undo", post(undo))
}

/// How many rows a status answer carries; the counts cover them all.
pub const ROWS_SHOWN: usize = 400;

const BUSY: &str = "Explicit marking is already running.";

/// The signed-in user (the cookie, else the header), a Navidrome user only (see the module
/// notes).
fn user(state: &AppState, parts: &Parts) -> Option<String> {
    let token = request_cookie(&parts.headers, BROWSE_COOKIE_NAME).or_else(|| {
        parts
            .headers
            .get(BROWSE_TOKEN_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    });
    state.browse_sessions.navidrome_user_of(token.as_deref())
}

fn status_name(status: ExplicitBackfillStatus) -> &'static str {
    match status {
        ExplicitBackfillStatus::Idle => "Idle",
        ExplicitBackfillStatus::Running => "Running",
        ExplicitBackfillStatus::Completed => "Completed",
        ExplicitBackfillStatus::Cancelled => "Cancelled",
        ExplicitBackfillStatus::Interrupted => "Interrupted",
        ExplicitBackfillStatus::Failed => "Failed",
    }
}

fn mode_name(mode: ExplicitBackfillMode) -> &'static str {
    match mode {
        ExplicitBackfillMode::Preview => "Preview",
        ExplicitBackfillMode::Apply => "Apply",
        ExplicitBackfillMode::Undo => "Undo",
    }
}

/// The apply body: an optional `confirm`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ApplyRequest {
    confirm: Option<String>,
}

impl FromBody for ApplyRequest {
    const SHAPE: &'static Shape = text_record!(
        "Octo.Controllers.ExplicitBackfillController+ApplyRequest",
        "confirm"
    );
}

async fn get_run(State(state): State<AppState>, request: Request) -> Response {
    let (parts, _) = request.into_parts();
    if user(&state, &parts).is_none() {
        return sign_in();
    }
    let show = query(&parts.uri, "show");
    let worker = &state.explicit_backfill;
    let run = worker.current();
    // Explicit and clean first (what Apply writes), then unsure (why nothing was written), then
    // not explicit; "show" narrows it to one kind.
    let order = |outcome: &str| match outcome {
        "explicit" => 0,
        "clean" => 1,
        "unsure" => 2,
        _ => 3,
    };
    let mut rows: Vec<_> = run
        .rows
        .iter()
        .filter(|row| show.as_deref().is_none_or(|show| row.outcome == show))
        .collect();
    rows.sort_by(|a, b| {
        order(&a.outcome)
            .cmp(&order(&b.outcome))
            .then_with(|| compare_ordinal(&a.path, &b.path))
    });
    let rows: Vec<Value> = rows
        .into_iter()
        .take(ROWS_SHOWN)
        .map(|row| {
            json!({
                "path": row.path,
                "artist": row.artist,
                "title": row.title,
                "outcome": row.outcome,
                "how": row.how,
                "written": row.written,
            })
        })
        .collect();
    ok(&json!({
        "runId": run.run_id,
        "status": status_name(run.status),
        "mode": mode_name(run.mode),
        "startedUtc": utc_opt(&run.started_utc),
        "finishedUtc": utc_opt(&run.finished_utc),
        "total": run.total,
        "processed": run.processed,
        "explicit": run.explicit,
        "clean": run.clean,
        "notExplicit": run.not_explicit,
        "unsure": run.unsure,
        "alreadyMarked": run.already_marked,
        "failed": run.failed,
        "stepTotal": run.step_total,
        "stepDone": run.step_done,
        "written": run.written,
        "leftAlone": run.left_alone,
        "lastSong": run.last_song,
        "reason": run.reason,
        "errors": run.errors,
        "toWrite": run.rows.iter().filter(|row| !row.written && writes(&row.outcome)).count(),
        "rows": rows,
        "canResume": run.can_resume(),
        "canApply": run.can_apply(),
        "canUndo": worker.can_undo() && run.status != ExplicitBackfillStatus::Running,
        "musicPath": worker.music_path(),
    }))
}

/// Look every unmarked song up. Writes nothing.
async fn preview(State(state): State<AppState>, request: Request) -> Response {
    let (parts, _) = request.into_parts();
    let Some(user) = user(&state, &parts) else {
        return sign_in();
    };
    if !state
        .explicit_backfill
        .try_enqueue(ExplicitBackfillRequest::new(ExplicitBackfillMode::Preview, &user))
    {
        return error(StatusCode::CONFLICT, BUSY);
    }
    info!("Explicit marking preview requested by {user}");
    status(StatusCode::ACCEPTED, &json!({ "started": true }))
}

/// Carry on a stopped or interrupted preview where it was.
async fn resume(State(state): State<AppState>, request: Request) -> Response {
    let (parts, _) = request.into_parts();
    let Some(user) = user(&state, &parts) else {
        return sign_in();
    };
    if !state.explicit_backfill.current().can_resume() {
        return error(StatusCode::BAD_REQUEST, "There is nothing to resume.");
    }
    if !state
        .explicit_backfill
        .try_enqueue(ExplicitBackfillRequest::new(ExplicitBackfillMode::Preview, user))
    {
        return error(StatusCode::CONFLICT, BUSY);
    }
    status(StatusCode::ACCEPTED, &json!({ "resumed": true }))
}

/// Write what the finished preview found explicit or clean.
async fn apply(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = split(request).await;
    let request: ApplyRequest = match bind_body(&parts, &body) {
        Ok(request) => request,
        Err(answer) => return *answer,
    };
    let Some(user) = user(&state, &parts) else {
        return sign_in();
    };
    let worker = &state.explicit_backfill;
    if !worker.current().can_apply() {
        return error(
            StatusCode::BAD_REQUEST,
            "Preview first: nothing found is waiting to be written.",
        );
    }
    let root = worker.music_path();
    if request.confirm.as_deref().map(str::trim) != Some(root.as_str()) {
        return error(
            StatusCode::BAD_REQUEST,
            format!("To mark songs in your library, type the music path exactly: {root}"),
        );
    }
    if let Some(busy) = other_job_writing(&state) {
        return busy;
    }
    if !worker.try_enqueue(ExplicitBackfillRequest::new(ExplicitBackfillMode::Apply, &user)) {
        return error(StatusCode::CONFLICT, BUSY);
    }
    info!("Explicit marking apply requested by {user}");
    status(StatusCode::ACCEPTED, &json!({ "started": true }))
}

async fn cancel(State(state): State<AppState>, request: Request) -> Response {
    let (parts, _) = request.into_parts();
    if user(&state, &parts).is_none() {
        return sign_in();
    }
    state.explicit_backfill.request_cancel();
    status(StatusCode::ACCEPTED, &json!({ "cancelling": true }))
}

/// Take out every mark the last Apply wrote, where the file has not changed since.
async fn undo(State(state): State<AppState>, request: Request) -> Response {
    let (parts, _) = request.into_parts();
    let Some(user) = user(&state, &parts) else {
        return sign_in();
    };
    if !state.explicit_backfill.can_undo() {
        return error(StatusCode::BAD_REQUEST, "There is no marking to undo.");
    }
    if let Some(busy) = other_job_writing(&state) {
        return busy;
    }
    if !state
        .explicit_backfill
        .try_enqueue(ExplicitBackfillRequest::new(ExplicitBackfillMode::Undo, &user))
    {
        return error(StatusCode::CONFLICT, BUSY);
    }
    info!("Explicit marking undo requested by {user}");
    status(StatusCode::ACCEPTED, &json!({ "started": true }))
}

/// 409 while another library job (the tidy, a genre re-tag) writes the music folder.
fn other_job_writing(state: &AppState) -> Option<Response> {
    state
        .library_write_gate
        .job()
        .filter(|holder| holder != ExplicitBackfill::JOB_NAME)
        .map(|holder| error(StatusCode::CONFLICT, LibraryWriteGate::busy_message(&holder)))
}

/// Ordinal order: by UTF-16 code unit.
fn compare_ordinal(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
