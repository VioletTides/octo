//! getTransitionProfile (octoTransitions): a song's transition profile, which the Octo apps
//! use to plan the blend into and out of it without reading the song themselves. Octo only
//! hands out what octo-sonic made beforehand; asking never starts any work.

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use super::caller_checks::{SubsonicCall, refuse_unless_signed_in};
use crate::app::AppState;
use crate::http::error::AppResult;
use crate::http::routes::RouteSet;
use crate::services::common::acquisition_tracker::key_of;
use crate::services::sonic::TransitionLookup;

pub fn routes() -> RouteSet {
    RouteSet::new().subsonic(
        "getTransitionProfile",
        get(get_transition_profile).post(get_transition_profile),
    )
}

/// Whether the extension is listed: once octo-sonic has said it makes profiles, which it does
/// whether or not Sounds alike is on.
pub fn offered(state: &AppState) -> bool {
    state.sonic_worker.transitions().current_version().is_some()
}

/// The profile of the song `id` (a library id, or the id of a song from outside the library
/// whose file Octo has fetched), or `ready: false`. Always JSON; the caller must be signed in.
pub async fn get_transition_profile(State(state): State<AppState>, req: Request) -> AppResult {
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(response) => return Ok(response),
    };
    if let Some(refused) = refuse_unless_signed_in(&state, &call, "json").await? {
        return Ok(refused);
    }
    let id = call.param_opt("id").unwrap_or("").trim().to_string();
    if id.is_empty() {
        return Ok(state
            .subsonic_response_builder
            .create_error("json", 10, "Required parameter is missing: id")
            .into_response());
    }
    Ok(answer(&state, &id))
}

fn answer(state: &AppState, id: &str) -> Response {
    let store = state.sonic_worker.transitions();
    let found = match store.lookup(&key_for(state, id)) {
        TransitionLookup::Ready(profile) => Some(profile),
        TransitionLookup::NotReady => None,
    };
    state
        .subsonic_response_builder
        .create_transition_profile_response(id, found.as_ref(), store.current_version())
        .into_response()
}

/// A library song is kept by its id, a song from outside the library by its acquisition key.
fn key_for(state: &AppState, id: &str) -> String {
    match state.local_library.parse_song_id(id) {
        (true, Some(provider), Some(external_id)) => key_of(&provider, &external_id),
        _ => id.to_string(),
    }
}
