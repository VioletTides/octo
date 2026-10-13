//! The dashboard's import page. Connect a Spotify account, add a public link, an exported file
//! or a pasted list, see what the library has and what it is missing, keep a list as a playlist,
//! and the trickle that fetches the rest. Lists belong to the Navidrome user signed in on the
//! page, the same person an Octo app signed in as sees them, so it needs the dashboard's sign-in.
//!
//! Also the page Spotify sends the browser back to when the redirect URI is Octo's own address.

use axum::extract::{DefaultBodyLimit, FromRequest, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use super::json_body::bind_body;
use super::json_body::{FromBody, Kind, Member, Shape};
use super::plumbing::{error_json, header_value, object_result, ok, request_cookie};
use super::request_binding::query;
use crate::app::AppState;
use crate::http::error::{AppError, AppResult, problem};
use crate::http::routes::RouteSet;
use crate::services::imports::ImportActionResult;
use crate::services::imports::import_file_reader::ImportFileReader;
use crate::services::imports::import_service::CALLBACK_PATH;

pub fn routes() -> RouteSet {
    RouteSet::new()
        .route("/api/admin/imports", get(overview))
        .route("/api/admin/imports/lists/{id}", get(list).delete(remove))
        .route("/api/admin/imports/spotify/connect", post(connect))
        .route("/api/admin/imports/spotify/finish", post(finish))
        .route("/api/admin/imports/spotify/disconnect", post(disconnect))
        .route("/api/admin/imports/spotify/read", post(read))
        .route("/api/admin/imports/link", post(link))
        .route(
            "/api/admin/imports/file",
            // [RequestSizeLimit(ImportFileReader.MaxBytes)]
            post(file).layer(DefaultBodyLimit::max(ImportFileReader::MAX_BYTES as usize)),
        )
        .route("/api/admin/imports/text", post(text))
        .route("/api/admin/imports/services", get(services))
        .route("/api/admin/imports/lists/{id}/playlist", post(playlist))
        .route("/api/admin/imports/lists/{id}/fetch", post(fetch))
        .route("/api/admin/imports/lists/{id}/songs", post(songs))
        .route("/api/admin/imports/lists/{id}/refresh", post(refresh))
        .route("/api/admin/imports/trickle/pause", post(pause))
        .route("/api/admin/imports/trickle/retry", post(retry))
        .route("/api/admin/imports/trickle/skip", post(skip))
        .route("/api/admin/imports/trickle/clear", post(clear))
        .route(CALLBACK_PATH, get(callback))
}

/// `SignedIn(header)`: `NavidromeUserOf(cookie ?? header)`. Never the recovery code, and the
/// cookie is not renewed here.
fn signed_in(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let token = request_cookie(headers, super::plumbing::BROWSE_COOKIE_NAME)
        .or_else(|| header_value(headers, super::plumbing::BROWSE_TOKEN_HEADER));
    state.browse_sessions.navidrome_user_of(token.as_deref())
}

/// `Unauthorized(new { error = ... })`.
fn sign_in() -> Response {
    error_json(
        StatusCode::UNAUTHORIZED,
        "Sign in with your Navidrome account first.",
    )
}

/// `result.Ok ? Ok(result) : BadRequest(result)`.
fn answer(result: &ImportActionResult) -> Response {
    object_result(
        if result.ok {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        result,
    )
}

/// A failure the import service did not handle, as the error envelope answers it.
pub(crate) fn app_error(e: anyhow::Error) -> AppError {
    match e.downcast::<reqwest::Error>() {
        Ok(e) => AppError::from_reqwest(e),
        Err(e) => AppError::Internal(e),
    }
}

// ---- Request bodies ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct OnRequest {
    on: bool,
}

impl FromBody for OnRequest {
    const SHAPE: &'static Shape = &Shape {
        name: "Octo.Controllers.ImportsController+OnRequest",
        by_constructor: true,
        members: &[Member {
            name: "on",
            kind: Kind::Bool,
        }],
    };
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PausedRequest {
    paused: bool,
}

impl FromBody for PausedRequest {
    const SHAPE: &'static Shape = &Shape {
        name: "Octo.Controllers.ImportsController+PausedRequest",
        by_constructor: true,
        members: &[Member {
            name: "paused",
            kind: Kind::Bool,
        }],
    };
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct KeysRequest {
    keys: Option<Vec<Option<String>>>,
}

impl FromBody for KeysRequest {
    const SHAPE: &'static Shape = &Shape {
        name: "Octo.Controllers.ImportsController+KeysRequest",
        by_constructor: true,
        members: &[Member {
            name: "keys",
            kind: Kind::TextList,
        }],
    };
}

impl KeysRequest {
    fn keys(&self) -> Option<Vec<String>> {
        self.keys
            .as_ref()
            .map(|keys| keys.iter().map(|k| k.clone().unwrap_or_default()).collect())
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LinkRequest {
    url: Option<String>,
}

impl FromBody for LinkRequest {
    const SHAPE: &'static Shape = &Shape {
        name: "Octo.Controllers.ImportsController+LinkRequest",
        by_constructor: true,
        members: &[Member {
            name: "url",
            kind: Kind::Text,
        }],
    };
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FinishRequest {
    address: Option<String>,
}

impl FromBody for FinishRequest {
    const SHAPE: &'static Shape = &Shape {
        name: "Octo.Controllers.ImportsController+FinishRequest",
        by_constructor: true,
        members: &[Member {
            name: "address",
            kind: Kind::Text,
        }],
    };
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TextRequest {
    name: Option<String>,
    text: Option<String>,
}

impl FromBody for TextRequest {
    const SHAPE: &'static Shape = &Shape {
        name: "Octo.Controllers.ImportsController+TextRequest",
        by_constructor: true,
        members: &[
            Member {
                name: "name",
                kind: Kind::Text,
            },
            Member {
                name: "text",
                kind: Kind::Text,
            },
        ],
    };
}

async fn split(request: Request) -> (axum::http::request::Parts, axum::body::Bytes) {
    super::request_binding::split(request).await
}

// ---- The endpoints ----------------------------------------------------------------------------

async fn overview(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(user) = signed_in(&state, &headers) else {
        return sign_in();
    };
    let overview = state.imports.overview(&user);
    ok(&json!({ "user": user, "overview": overview }))
}

async fn list(State(state): State<AppState>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let Some(user) = signed_in(&state, &headers) else {
        return sign_in();
    };
    match state.imports.detail(&user, &id) {
        Some(detail) => ok(&detail),
        None => error_json(StatusCode::NOT_FOUND, "There is no such list."),
    }
}

async fn connect(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(user) = signed_in(&state, &headers) else {
        return sign_in();
    };
    answer(&state.imports.begin_spotify(&user, None))
}

async fn finish(State(state): State<AppState>, request: Request) -> AppResult {
    let (parts, body) = split(request).await;
    let request: FinishRequest = match bind_body::<FinishRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return Ok(refused),
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return Ok(sign_in());
    };
    let result = state
        .imports
        .finish_spotify(
            Some(request.address.as_deref().unwrap_or("")),
            None,
            None,
            Some(&user),
        )
        .await
        .map_err(app_error)?;
    Ok(answer(&result))
}

async fn disconnect(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(user) = signed_in(&state, &headers) else {
        return sign_in();
    };
    answer(&state.imports.disconnect_spotify(&user))
}

async fn read(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(user) = signed_in(&state, &headers) else {
        return sign_in();
    };
    ok(&ImportActionResult::ok(if state.imports.start_reading(&user) {
        "Reading your Spotify lists again."
    } else {
        "Octo is already reading your Spotify lists."
    }))
}

async fn link(State(state): State<AppState>, request: Request) -> AppResult {
    let (parts, body) = split(request).await;
    let request: LinkRequest = match bind_body::<LinkRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return Ok(refused),
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return Ok(sign_in());
    };
    // Not tied to the request: a closed tab must not leave a list half read.
    let imports = state.imports.clone();
    let url = request.url.unwrap_or_default();
    let result = tokio::spawn(async move { imports.add_link(&user, &url).await })
        .await
        .map_err(|e| AppError::Internal(e.into()))?
        .map_err(app_error)?;
    Ok(answer(&result))
}

async fn file(State(state): State<AppState>, request: Request) -> AppResult {
    // An uploaded file: the action takes a form only; any other body finds no action (404).
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase());
    if content_type
        .as_deref()
        .is_some_and(|media| media != "multipart/form-data")
    {
        return Ok(problem(StatusCode::NOT_FOUND));
    }
    let Some(user) = signed_in(&state, request.headers()) else {
        return Ok(sign_in());
    };
    let choose = || answer(&ImportActionResult::failed("Choose a file first."));
    if content_type.is_none() {
        return Ok(choose());
    }
    let mut form = match axum::extract::Multipart::from_request(request, &state).await {
        Ok(form) => form,
        Err(_) => return Ok(choose()),
    };
    let mut found: Option<(String, Vec<u8>)> = None;
    loop {
        let field = match form.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return Ok(problem(StatusCode::BAD_REQUEST)),
        };
        let wanted = found.is_none()
            && field.name().is_some_and(|n| n.eq_ignore_ascii_case("file"))
            && field.file_name().is_some();
        if !wanted {
            continue;
        }
        let name = field.file_name().unwrap_or_default().to_string();
        let bytes = match field.bytes().await {
            Ok(bytes) => bytes.to_vec(),
            Err(_) => return Ok(problem(StatusCode::BAD_REQUEST)),
        };
        found = Some((name, bytes));
    }
    let Some((name, bytes)) = found.filter(|(_, bytes)| !bytes.is_empty()) else {
        return Ok(choose());
    };
    let imports = state.imports.clone();
    let result = tokio::spawn(async move { imports.add_file(&user, &name, &bytes).await })
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(answer(&result))
}

/// A pasted list, one song a line, kept under its name ("Pasted list" when it has none).
async fn text(State(state): State<AppState>, request: Request) -> AppResult {
    let (parts, body) = split(request).await;
    let request: TextRequest = match bind_body::<TextRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return Ok(refused),
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return Ok(sign_in());
    };
    let text = request.text.unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(answer(&ImportActionResult::failed("Paste a list first.")));
    }
    let imports = state.imports.clone();
    let name = request.name;
    let result = tokio::spawn(async move { imports.add_text(&user, name.as_deref(), &text).await })
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(answer(&result))
}

/// Where people can bring their music from, for the dashboard's guide.
async fn services(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if signed_in(&state, &headers).is_none() {
        return sign_in();
    }
    ok(&state.imports.services())
}

async fn playlist(State(state): State<AppState>, Path(id): Path<String>, request: Request) -> AppResult {
    let (parts, body) = split(request).await;
    let request: OnRequest = match bind_body::<OnRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return Ok(refused),
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return Ok(sign_in());
    };
    let imports = state.imports.clone();
    let result = tokio::spawn(async move { imports.set_playlist(&user, &id, request.on).await })
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(answer(&result))
}

async fn fetch(State(state): State<AppState>, Path(id): Path<String>, request: Request) -> Response {
    let (parts, body) = split(request).await;
    let request: OnRequest = match bind_body::<OnRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return sign_in();
    };
    answer(&state.imports.set_get_missing(&user, &id, request.on))
}

async fn songs(State(state): State<AppState>, Path(id): Path<String>, request: Request) -> Response {
    let (parts, body) = split(request).await;
    let request: KeysRequest = match bind_body::<KeysRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return sign_in();
    };
    answer(
        &state
            .imports
            .get_songs(&user, &id, &request.keys().unwrap_or_default()),
    )
}

async fn refresh(State(state): State<AppState>, Path(id): Path<String>, headers: HeaderMap) -> AppResult {
    let Some(user) = signed_in(&state, &headers) else {
        return Ok(sign_in());
    };
    let imports = state.imports.clone();
    let result = tokio::spawn(async move { imports.refresh(&user, &id).await })
        .await
        .map_err(|e| AppError::Internal(e.into()))?
        .map_err(app_error)?;
    Ok(answer(&result))
}

async fn remove(State(state): State<AppState>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let Some(user) = signed_in(&state, &headers) else {
        return sign_in();
    };
    answer(&state.imports.remove(&user, &id))
}

async fn pause(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = split(request).await;
    let request: PausedRequest = match bind_body::<PausedRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return sign_in();
    };
    answer(&state.imports.pause(&user, request.paused))
}

async fn retry(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = split(request).await;
    let request: KeysRequest = match bind_body::<KeysRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return sign_in();
    };
    answer(&state.imports.retry(&user, request.keys().as_deref()))
}

async fn skip(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = split(request).await;
    let request: KeysRequest = match bind_body::<KeysRequest>(&parts.headers, &body, "request") {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    let Some(user) = signed_in(&state, &parts.headers) else {
        return sign_in();
    };
    answer(&state.imports.skip(&user, &request.keys().unwrap_or_default()))
}

async fn clear(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(user) = signed_in(&state, &headers) else {
        return sign_in();
    };
    answer(&state.imports.clear_finished(&user))
}

/// Where Spotify sends the browser when the redirect URI is Octo's own address. The state says
/// whose sign-in it is, so no dashboard sign-in is needed here, and the PKCE verifier Octo kept
/// is what makes the code worth anything.
async fn callback(State(state): State<AppState>, request: Request) -> AppResult {
    let uri = request.uri().clone();
    let (code, sign_in_state, error) = (query(&uri, "code"), query(&uri, "state"), query(&uri, "error"));
    let result = match error {
        Some(error) => ImportActionResult::failed(if error == "access_denied" {
            "Spotify says access was not allowed.".to_string()
        } else {
            format!("Spotify sent back an error: {error}")
        }),
        None => {
            let imports = state.imports.clone();
            tokio::spawn(async move {
                imports
                    .finish_spotify(None, code.as_deref(), sign_in_state.as_deref(), None)
                    .await
            })
            .await
            .map_err(|e| AppError::Internal(e.into()))?
            .map_err(app_error)?
        }
    };
    if !result.ok {
        info!("Spotify sign-in callback refused: {}", result.message);
    }
    let title = if result.ok {
        "Connected to Spotify"
    } else {
        "Spotify sign-in did not finish"
    };
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
<title>{title}</title><style>body{{font:16px system-ui,sans-serif;background:#0d0f14;color:#e8e8ef;display:grid;place-items:center;min-height:100vh;margin:0}}\n\
main{{max-width:28rem;padding:2rem}}a{{color:#9db4ff}}</style></head>\n\
<body><main><h1>{title}</h1><p>{}</p>\n\
<p>You can close this tab and go back to Octo.</p><p><a href=\"/admin/#imports\">Open the Spotify import page</a></p></main></body></html>",
        html_encode(&result.message)
    );
    let mut response = html.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html"),
    );
    Ok(response)
}

/// `WebUtility.HtmlEncode`: the five markup characters, and Latin-1's upper half and anything
/// past the BMP as numeric references.
pub(crate) fn html_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '&' => out.push_str("&amp;"),
            '\u{a0}'..='\u{ff}' => out.push_str(&format!("&#{};", c as u32)),
            c if (c as u32) > 0xFFFF => out.push_str(&format!("&#{};", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[path = "imports_tests.rs"]
mod tests;
