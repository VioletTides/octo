//! `api/admin/sonic` (Sounds alike on the dashboard's Radio page: where the analysis is, and
//! its controls) and `api/admin/radio-outcomes` (what radio has learned from listening).

use axum::extract::State;
use axum::response::Response;
use axum::routing::{get, post};

use super::plumbing::ok;
use crate::app::AppState;
use crate::http::routes::RouteSet;

pub fn routes() -> RouteSet {
    RouteSet::new()
        .route("/api/admin/sonic", get(sonic_status))
        .route("/api/admin/sonic/start", post(sonic_start))
        .route("/api/admin/sonic/pause", post(sonic_pause))
        .route("/api/admin/sonic/reset", post(sonic_reset))
        .route("/api/admin/radio-outcomes", get(outcome_stats))
        .route("/api/admin/radio-outcomes/reset", post(outcome_reset))
}

async fn sonic_status(State(state): State<AppState>) -> Response {
    ok(&state.sonic_worker.status())
}

async fn sonic_start(State(state): State<AppState>) -> Response {
    state.sonic_worker.set_paused(false);
    ok(&state.sonic_worker.status())
}

async fn sonic_pause(State(state): State<AppState>) -> Response {
    state.sonic_worker.set_paused(true);
    ok(&state.sonic_worker.status())
}

async fn sonic_reset(State(state): State<AppState>) -> Response {
    state.sonic_worker.reset();
    ok(&state.sonic_worker.status())
}

async fn outcome_stats(State(state): State<AppState>) -> Response {
    ok(&state.radio_outcomes.stats(None))
}

async fn outcome_reset(State(state): State<AppState>) -> Response {
    state.radio_outcomes.forget();
    ok(&state.radio_outcomes.stats(None))
}
