//! octoImports v1, the Spotify import page for the Octo apps, the same lists and trickle the
//! dashboard shows, for whoever the app signed in as. Every call checks the caller's sign-in with
//! Navidrome first, as getUpgrades does, and answers JSON. Version 2 adds getImportServices (where to export a library to a file),
//! importFile and importText.
//!
//! The Spotify sign-in from an app: importAction connect with the app's own loopback redirect
//! (`http://127.0.0.1:port/path`) gives the address to open; the app's loopback page catches
//! Spotify's answer and sends its code and state back with importAction finish.

use axum::extract::{DefaultBodyLimit, FromRequest, Request, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use octo_core::common::dotnet::{eq_ignore_case, is_blank, to_lower_invariant};
use octo_subsonic::subsonic_response_builder::SubsonicReply;
use serde_json::{Value, json};
use tracing::info;

use super::caller_checks::{SubsonicCall, is_successful_subsonic_response};
use crate::app::AppState;
use crate::controllers::admin::imports::app_error;
use crate::http::error::{AppError, AppResult, SUBSONIC_VERSION};
use crate::http::routes::RouteSet;
use crate::services::imports::ImportActionResult;
use crate::services::imports::import_file_reader::ImportFileReader;

pub fn routes() -> RouteSet {
    RouteSet::new()
        .subsonic("getImports", get(get_imports).post(get_imports))
        .subsonic("getImport", get(get_import).post(get_import))
        .subsonic("importAction", get(import_action).post(import_action))
        .subsonic(
            "getImportServices",
            get(get_import_services).post(get_import_services),
        )
        .subsonic("importText", get(import_text).post(import_text))
        .subsonic(
            "importFile",
            post(import_file).layer(DefaultBodyLimit::max(IMPORT_FILE_BODY)),
        )
}

/// The largest `importFile` body: the file reader's own limit, and room for the form around it.
const IMPORT_FILE_BODY: usize = ImportFileReader::MAX_BYTES as usize + 64 * 1024;

/// `OctoAnswer`: an ok envelope of Octo's own, always JSON.
fn octo_answer(state: &AppState, key: &str, value: Value) -> SubsonicReply {
    state.subsonic_response_builder.create_json_response(json!({
        "status": "ok",
        "version": SUBSONIC_VERSION,
        "type": "octo",
        "openSubsonic": true,
        key: value,
    }))
}

fn error(state: &AppState, code: i32, message: &str) -> Response {
    state
        .subsonic_response_builder
        .create_error("json", code, message)
        .into_response()
}

/// The caller's username once Navidrome accepts their sign-in, or the error to answer with.
#[allow(clippy::result_large_err)] // the error is the answer itself, sent at once
async fn caller(state: &AppState, call: &SubsonicCall) -> Result<String, Response> {
    let mut auth = call.parameters.clone();
    auth.insert("f".into(), "json".into());
    let Some(check) = call.proxy.relay_safe("rest/ping", &auth).await else {
        return Err(error(
            state,
            0,
            "Octo can't reach Navidrome to check who is asking",
        ));
    };
    if !is_successful_subsonic_response(&check.body, "json") {
        return Err(error(state, 40, "Wrong username or password"));
    }
    // An API key alone names nobody here, and lists belong to someone.
    match call.param_opt("u").filter(|u| !is_blank(u)) {
        Some(user) => Ok(user.trim().to_string()),
        None => Err(error(
            state,
            0,
            "Sign in with a username to import; an API key alone does not say who is asking.",
        )),
    }
}

fn to_value(value: &impl serde::Serialize) -> AppResult<Value> {
    serde_json::to_value(value).map_err(|e| AppError::Internal(e.into()))
}

pub async fn get_imports(State(state): State<AppState>, req: Request) -> AppResult {
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(response) => return Ok(response),
    };
    let user = match caller(&state, &call).await {
        Ok(user) => user,
        Err(refused) => return Ok(refused),
    };
    let overview = to_value(&state.imports.overview(&user))?;
    Ok(octo_answer(&state, "imports", overview).into_response())
}

pub async fn get_import(State(state): State<AppState>, req: Request) -> AppResult {
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(response) => return Ok(response),
    };
    let user = match caller(&state, &call).await {
        Ok(user) => user,
        Err(refused) => return Ok(refused),
    };
    let id = call.param("id").trim().to_string();
    if id.is_empty() {
        return Ok(error(&state, 10, "Required parameter is missing: id"));
    }
    Ok(match state.imports.detail(&user, &id) {
        Some(detail) => octo_answer(&state, "import", to_value(&detail)?).into_response(),
        None => error(&state, 70, "There is no such list."),
    })
}

/// Where the caller can bring their music from (octoImports 2).
pub async fn get_import_services(State(state): State<AppState>, req: Request) -> AppResult {
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(response) => return Ok(response),
    };
    if let Err(refused) = caller(&state, &call).await {
        return Ok(refused);
    }
    let services = to_value(&state.imports.services())?;
    Ok(octo_answer(&state, "importServices", services).into_response())
}

/// A pasted list, one song a line (octoImports 2): `text`, and `name`, "Pasted list" when not
/// given.
pub async fn import_text(State(state): State<AppState>, req: Request) -> AppResult {
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(response) => return Ok(response),
    };
    let user = match caller(&state, &call).await {
        Ok(user) => user,
        Err(refused) => return Ok(refused),
    };
    let Some(text) = call.param_opt("text").map(str::to_string) else {
        return Ok(error(&state, 10, "Required parameter is missing: text"));
    };
    let name = call.param_opt("name").map(str::to_string);
    let imports = state.imports.clone();
    // Not tied to the request: an app that gives up must not leave a list half matched.
    let result = tokio::spawn(async move { imports.add_text(&user, name.as_deref(), &text).await })
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    answer_action(&state, "importText", &result)
}

/// An exported file (octoImports 2): a multipart form with the file in `file`, up to 64 MB,
/// read as the dashboard's file import reads it. `name` names it when the part has no file
/// name.
pub async fn import_file(State(state): State<AppState>, req: Request) -> AppResult {
    let (parts, body) = req.into_parts();
    let Ok(body) = axum::body::to_bytes(body, IMPORT_FILE_BODY).await else {
        return Ok(error(
            &state,
            0,
            "That file is larger than 64 MB. Split it into smaller files.",
        ));
    };
    let call = SubsonicCall::from_parts(&state, &parts, body.clone());
    let user = match caller(&state, &call).await {
        Ok(user) => user,
        Err(refused) => return Ok(refused),
    };
    let given_name = call
        .param_opt("name")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let choose = |state: &AppState| {
        answer_action(
            state,
            "importFile",
            &ImportActionResult::failed("Choose a file first."),
        )
    };
    let multipart = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("multipart/form-data")
        });
    if !multipart {
        return choose(&state);
    }
    let request = Request::from_parts(parts, axum::body::Body::from(body));
    let Ok(mut form) = axum::extract::Multipart::from_request(request, &state).await else {
        return choose(&state);
    };
    let mut found: Option<(Option<String>, Vec<u8>)> = None;
    loop {
        let field = match form.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return choose(&state),
        };
        let wanted = found.is_none() && field.name().is_some_and(|n| n.eq_ignore_ascii_case("file"));
        if !wanted {
            continue;
        }
        let file_name = field
            .file_name()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        let Ok(bytes) = field.bytes().await else {
            return choose(&state);
        };
        found = Some((file_name, bytes.to_vec()));
    }
    let Some((file_name, bytes)) = found.filter(|(_, bytes)| !bytes.is_empty()) else {
        return choose(&state);
    };
    let name = file_name
        .or(given_name)
        .unwrap_or_else(|| "Imported list".to_string());
    let imports = state.imports.clone();
    let result = tokio::spawn(async move { imports.add_file(&user, &name, &bytes).await })
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    answer_action(&state, "importFile", &result)
}

/// An import's result under `importAction`, as every import call answers.
fn answer_action(state: &AppState, call: &str, result: &ImportActionResult) -> AppResult {
    if !result.ok {
        info!("{call}: {}", result.message);
    }
    Ok(octo_answer(state, "importAction", to_value(result)?).into_response())
}

/// One thing to do: connect (redirect), finish (code and state), disconnect, read, addLink (url),
/// playlist (id, on), fetch (id, on), songs (id, key...), refresh (id), remove (id), pause,
/// resume, retry (key... or none for every song not found), skip (key...), clear.
pub async fn import_action(State(state): State<AppState>, req: Request) -> AppResult {
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(response) => return Ok(response),
    };
    let user = match caller(&state, &call).await {
        Ok(user) => user,
        Err(refused) => return Ok(refused),
    };
    let action = call.param("action").trim().to_string();
    let id = call.param("id").trim().to_string();
    let on = !eq_ignore_case(call.param_opt("on").unwrap_or("true"), "false");
    let keys = call.incoming.parameter_values("key");
    let imports = state.imports.clone();
    // Slow work is not tied to the request: an app that gives up must not leave a list half done.
    let (redirect, address, code, sign_in_state, url) = (
        call.param_opt("redirect").map(str::to_string),
        call.param_opt("address").map(str::to_string),
        call.param_opt("code").map(str::to_string),
        call.param_opt("state").map(str::to_string),
        call.param("url").to_string(),
    );
    let (who, what) = (user.clone(), action.clone());
    let result: ImportActionResult = tokio::spawn(async move {
        let user = who;
        anyhow::Ok(match to_lower_invariant(&what).as_str() {
            "connect" => imports.begin_spotify(&user, redirect.as_deref()),
            "finish" => {
                imports
                    .finish_spotify(
                        address.as_deref(),
                        code.as_deref(),
                        sign_in_state.as_deref(),
                        Some(&user),
                    )
                    .await?
            }
            "disconnect" => imports.disconnect_spotify(&user),
            "read" => ImportActionResult::ok(if imports.start_reading(&user) {
                "Reading your Spotify lists again."
            } else {
                "Octo is already reading your Spotify lists."
            }),
            "addlink" => imports.add_link(&user, &url).await?,
            "playlist" => imports.set_playlist(&user, &id, on).await,
            "fetch" => imports.set_get_missing(&user, &id, on),
            "songs" => imports.get_songs(&user, &id, &keys),
            "refresh" => imports.refresh(&user, &id).await?,
            "remove" => imports.remove(&user, &id),
            "pause" => imports.pause(&user, true),
            "resume" => imports.pause(&user, false),
            "retry" => imports.retry(&user, Some(&keys)),
            "skip" => imports.skip(&user, &keys),
            "clear" => imports.clear_finished(&user),
            _ => ImportActionResult::failed(format!("Unknown action \"{what}\".")),
        })
    })
    .await
    .map_err(|e| AppError::Internal(e.into()))?
    .map_err(app_error)?;
    if !result.ok {
        info!("Import action {action} for {user}: {}", result.message);
    }
    Ok(octo_answer(&state, "importAction", to_value(&result)?).into_response())
}
