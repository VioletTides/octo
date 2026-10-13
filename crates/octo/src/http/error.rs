//! What an unhandled failure looks like on the wire.
//!
//! Any failure that escapes a handler becomes a Subsonic error envelope in JSON, whatever `f`
//! asked for, on every route including the admin API, with the HTTP status and Subsonic code
//! chosen by the kind of failure. Handlers return `Result<_, AppError>`, and `AppError`'s
//! variants are those kinds.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use octo_core::json::format::{Escaping, Options, to_string_with};
use serde_json::json;

/// The Subsonic protocol version Octo reports in every envelope it builds.
pub const SUBSONIC_VERSION: &str = "1.16.1";

/// A failure that escaped a handler, by kind.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// No Navidrome URL. The message tells the user what to set, so it goes out verbatim.
    #[error("{0}")]
    NotConfigured(String),
    /// A missing file.
    #[error("file not found: {0}")]
    FileNotFound(String),
    /// A missing directory.
    #[error("directory not found: {0}")]
    DirectoryNotFound(String),
    /// Access refused.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// A required argument missing.
    #[error("missing argument: {0}")]
    ArgumentNull(String),
    /// An invalid argument.
    #[error("invalid argument: {0}")]
    Argument(String),
    /// Input in the wrong format.
    #[error("invalid format: {0}")]
    Format(String),
    /// An operation not valid in the current state.
    #[error("invalid operation: {0}")]
    InvalidOperation(String),
    /// An upstream that could not be reached or answered with a failure the caller chose to
    /// fail on.
    #[error("upstream request failed: {0}")]
    Http(String),
    /// A timeout the code reports as one. An HTTP client timeout is not this: it maps to 500;
    /// see [`AppError::from_reqwest`].
    #[error("timed out: {0}")]
    Timeout(String),
    /// Anything else.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    /// The HTTP status, Subsonic error code and message for each kind.
    pub fn mapping(&self) -> (StatusCode, i32, String) {
        match self {
            AppError::NotConfigured(m) => (StatusCode::SERVICE_UNAVAILABLE, 0, m.clone()),
            AppError::FileNotFound(_) => (StatusCode::NOT_FOUND, 70, "Resource not found".into()),
            AppError::DirectoryNotFound(_) => (StatusCode::NOT_FOUND, 70, "Directory not found".into()),
            AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, 40, "Wrong username or password".into()),
            AppError::ArgumentNull(_) => (
                StatusCode::BAD_REQUEST,
                10,
                "Required parameter is missing".into(),
            ),
            AppError::Argument(_) => (StatusCode::BAD_REQUEST, 10, "Invalid request".into()),
            AppError::Format(_) => (StatusCode::BAD_REQUEST, 10, "Invalid format".into()),
            AppError::InvalidOperation(_) => (StatusCode::BAD_REQUEST, 10, "Operation not valid".into()),
            AppError::Http(_) => (StatusCode::BAD_GATEWAY, 0, "External service unavailable".into()),
            AppError::Timeout(_) => (StatusCode::GATEWAY_TIMEOUT, 0, "Request timeout".into()),
            AppError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                0,
                "An internal server error occurred".into(),
            ),
        }
    }

    /// A failed HTTP client call: a connection or status failure is [`AppError::Http`] (502),
    /// while a timeout is an internal error (500), not [`AppError::Timeout`].
    pub fn from_reqwest(e: reqwest::Error) -> AppError {
        if e.is_timeout() {
            AppError::Internal(anyhow::Error::new(e).context("the request timed out"))
        } else {
            AppError::Http(e.to_string())
        }
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::from_reqwest(e)
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => AppError::FileNotFound(e.to_string()),
            std::io::ErrorKind::PermissionDenied => AppError::Unauthorized(e.to_string()),
            _ => AppError::Internal(e.into()),
        }
    }
}

impl From<octo_core::json::web::WebError> for AppError {
    fn from(e: octo_core::json::web::WebError) -> Self {
        AppError::Format(e.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = ?self, "Unhandled exception occurred: {self}");
        let (status, code, message) = self.mapping();
        let body = json!({
            "subsonic-response": {
                "status": "failed",
                "version": SUBSONIC_VERSION,
                "error": { "code": code, "message": message },
            }
        });
        // `application/json` with the charset appended, and the relaxed encoder (the em dash in
        // the not-configured message goes out as is).
        let mut res = json_relaxed_response(status, &body, "application/json; charset=utf-8");
        // The error response is marked uncacheable.
        let headers = res.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache,no-store"),
        );
        headers.insert(header::EXPIRES, HeaderValue::from_static("-1"));
        headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        res
    }
}

/// The response for a handler that panicked: an unclassified failure (500, code 0, "An
/// internal server error occurred").
pub fn panic_response(message: &str) -> Response {
    AppError::Internal(anyhow::anyhow!("{message}")).into_response()
}

/// A JSON body with the relaxed encoder (non-ASCII and `'` `<` `>` `&` `+` left as they are),
/// with an explicit content type.
pub fn json_relaxed_response(
    status: StatusCode,
    body: &impl serde::Serialize,
    content_type: &'static str,
) -> Response {
    let options = Options {
        escaping: Escaping::Relaxed,
        indented: false,
    };
    let mut res = (status, to_string_with(body, options)).into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    res
}

/// A JSON body with the default encoder's escaping, with an explicit content type.
pub fn json_response(
    status: StatusCode,
    body: &impl serde::Serialize,
    content_type: &'static str,
) -> Response {
    let mut res = (status, octo_core::json::to_string(body)).into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    res
}

/// A 200 JSON body with the default escaping, `application/json; charset=utf-8`.
pub fn json_ok(body: &impl serde::Serialize) -> Response {
    json_response(StatusCode::OK, body, "application/json; charset=utf-8")
}

/// A JSON body with a status, with the default escaping.
pub fn json_status(status: StatusCode, body: &impl serde::Serialize) -> Response {
    json_response(status, body, "application/json; charset=utf-8")
}

/// What a bodiless client-error result (a 404, a 400, any 4xx) becomes: an RFC 7807 problem
/// document.
pub fn problem(status: StatusCode) -> Response {
    // A status with no mapping (502, 503, ...) gets neither a type nor a title: both are left
    // out rather than written as null.
    let body = match problem_type(status) {
        Some((kind, title)) => json!({
            "type": kind,
            "title": title,
            "status": status.as_u16(),
            "traceId": trace_id(),
        }),
        None => json!({ "status": status.as_u16(), "traceId": trace_id() }),
    };
    // Problem documents use the relaxed encoder.
    json_relaxed_response(status, &body, "application/problem+json; charset=utf-8")
}

/// The type URI and title of a problem document for a status.
fn problem_type(status: StatusCode) -> Option<(&'static str, &'static str)> {
    Some(match status.as_u16() {
        400 => (
            "https://tools.ietf.org/html/rfc9110#section-15.5.1",
            "Bad Request",
        ),
        401 => (
            "https://tools.ietf.org/html/rfc9110#section-15.5.2",
            "Unauthorized",
        ),
        403 => ("https://tools.ietf.org/html/rfc9110#section-15.5.4", "Forbidden"),
        404 => ("https://tools.ietf.org/html/rfc9110#section-15.5.5", "Not Found"),
        405 => (
            "https://tools.ietf.org/html/rfc9110#section-15.5.6",
            "Method Not Allowed",
        ),
        406 => (
            "https://tools.ietf.org/html/rfc9110#section-15.5.7",
            "Not Acceptable",
        ),
        408 => (
            "https://tools.ietf.org/html/rfc9110#section-15.5.9",
            "Request Timeout",
        ),
        409 => ("https://tools.ietf.org/html/rfc9110#section-15.5.10", "Conflict"),
        412 => (
            "https://tools.ietf.org/html/rfc9110#section-15.5.13",
            "Precondition Failed",
        ),
        415 => (
            "https://tools.ietf.org/html/rfc9110#section-15.5.16",
            "Unsupported Media Type",
        ),
        422 => (
            "https://tools.ietf.org/html/rfc4918#section-11.2",
            "Unprocessable Entity",
        ),
        426 => (
            "https://tools.ietf.org/html/rfc9110#section-15.5.22",
            "Upgrade Required",
        ),
        500 => (
            "https://tools.ietf.org/html/rfc9110#section-15.6.1",
            "An error occurred while processing your request.",
        ),
        _ => return None,
    })
}

/// The automatic `400` when a request fails validation before the action runs (a validation
/// problem document): `errors` maps each field to its messages.
pub fn validation_problem(errors: &[(&str, &[&str])]) -> Response {
    let mut map = serde_json::Map::new();
    for (field, messages) in errors {
        map.insert((*field).to_string(), json!(messages));
    }
    let body = json!({
        "type": "https://tools.ietf.org/html/rfc9110#section-15.5.1",
        "title": "One or more validation errors occurred.",
        "status": 400,
        "errors": map,
        "traceId": trace_id(),
    });
    // The relaxed encoder (a message quoting the input keeps its `'` as written).
    json_relaxed_response(
        StatusCode::BAD_REQUEST,
        &body,
        "application/problem+json; charset=utf-8",
    )
}

/// The trace id, in the W3C `00-<trace>-<span>-00` form. Values are random per request.
fn trace_id() -> String {
    let trace: u128 = rand::random();
    let span: u64 = rand::random();
    format!("00-{trace:032x}-{span:016x}-00")
}

/// Shorthand for handler results.
pub type AppResult<T = Response> = Result<T, AppError>;
