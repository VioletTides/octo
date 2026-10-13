//! What the admin controllers add to the helpers in `plumbing` (which they reuse for the browse
//! sign-in, the answer writers and request body binding): the `signed` session check the
//! cover-upgrade and lyrics controllers make (it does not re-issue the cookie), boolean query
//! values, required members, enum names, dates, and the member-name collision in an answer.

use axum::http::StatusCode;
use axum::http::Uri;
use axum::http::request::Parts;
use axum::response::Response;
use chrono::{DateTime, Utc};
use octo_core::common::dotnet::eq_ignore_case;
use octo_core::json::datetime::format_utc;
use serde_json::Value;

pub use super::plumbing::{BROWSE_COOKIE_NAME, BROWSE_TOKEN_HEADER, BrowseUser, ok, request_cookie};
use super::plumbing::{error_json, header_value, object_result, query_value};
use crate::app::AppState;
use crate::http::error::{AppError, validation_problem};
use crate::middleware::forwarded::RequestScheme;

/// What every session-gated endpoint answers without a session.
pub const SIGN_IN_FIRST: &str = "Sign in with your Navidrome admin account first.";

/// The 401 carrying [`SIGN_IN_FIRST`].
pub fn sign_in() -> Response {
    error(StatusCode::UNAUTHORIZED, SIGN_IN_FIRST)
}

/// A JSON answer with any status, written with the relaxed encoder (the `&&` and `<release>`
/// in the update card go out as written).
pub fn status(code: StatusCode, body: &impl serde::Serialize) -> Response {
    object_result(code, body)
}

/// An `{"error": ...}` answer with a status (400, 409, ...).
pub fn error(code: StatusCode, message: impl Into<String>) -> Response {
    error_json(code, &message.into())
}

/// The `X-Octo-Browse-Token` header.
fn header_token(parts: &Parts) -> Option<String> {
    header_value(&parts.headers, BROWSE_TOKEN_HEADER)
}

/// The browse sign-in: cookie first, header second; a live cookie is renewed on the answer
/// ([`BrowseUser::finish`]).
pub fn browse_user(state: &AppState, parts: &Parts) -> BrowseUser {
    BrowseUser::check(
        state,
        &parts.headers,
        parts.extensions.get::<RequestScheme>(),
        header_token(parts).as_deref(),
    )
}

/// Whether a Navidrome user is signed in (the cookie, else the header), for the cover-upgrade
/// and lyrics endpoints. It slides the server-side expiry but, unlike [`browse_user`], never
/// re-issues the cookie. A recovery-code session is not signed in here: these endpoints read or
/// change library files.
pub fn signed(state: &AppState, parts: &Parts) -> bool {
    let token = request_cookie(&parts.headers, BROWSE_COOKIE_NAME).or_else(|| header_token(parts));
    state
        .browse_sessions
        .navidrome_user_of(token.as_deref())
        .is_some()
}

/// An optional query value: the first value, matched ignoring case; empty reads as none.
pub fn query(uri: &Uri, name: &str) -> Option<String> {
    query_value(uri.query(), name)
}

/// The first value of a query key, matched ignoring case, empty kept.
fn query_raw(uri: &Uri, name: &str) -> Option<String> {
    let q = uri.query()?;
    url::form_urlencoded::parse(q.as_bytes())
        .find(|(k, _)| eq_ignore_case(k, name))
        .map(|(_, v)| v.into_owned())
}

/// A boolean query value (default false): `true` or `false` in any case, trimmed, from the
/// first value. Anything else is a 400 validation problem, answered before the action runs.
pub fn query_bool(uri: &Uri, name: &str) -> Result<bool, Rejection> {
    let Some(value) = query_raw(uri, name) else {
        return Ok(false);
    };
    let trimmed = value.trim();
    if eq_ignore_case(trimmed, "true") {
        Ok(true)
    } else if eq_ignore_case(trimmed, "false") {
        Ok(false)
    } else if trimmed.is_empty() {
        let message = format!("The value '{value}' is invalid.");
        Err(Box::new(validation_problem(&[(name, &[message.as_str()])])))
    } else {
        let message = format!("The value '{value}' is not valid.");
        Err(Box::new(validation_problem(&[(name, &[message.as_str()])])))
    }
}

/// An answer given instead of running the action (boxed: a `Response` is large).
pub type Rejection = Box<Response>;

/// The request's head and its whole body.
pub async fn split(request: axum::extract::Request) -> (Parts, axum::body::Bytes) {
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap_or_default();
    (parts, bytes)
}

/// A JSON request body read into `T`, with the parameter named `request` in its errors.
pub fn bind_body<T: super::json_body::FromBody>(parts: &Parts, body: &[u8]) -> Result<T, Rejection> {
    super::json_body::bind_body(&parts.headers, body, "request").map_err(Box::new)
}

/// A required string member: null, empty and whitespace all fail.
pub fn required(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// An enum value from its text: a member name in any case, a number, or several of either
/// joined with commas (OR'ed together). `members` are the names in the order of their values
/// 0, 1, 2, ... A value no member has fails.
pub fn enum_try_parse(members: &[&str], text: Option<&str>) -> Option<usize> {
    let text = text?;
    let mut value: i64 = 0;
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        if let Some(i) = members.iter().position(|m| eq_ignore_case(m, part)) {
            value |= i as i64;
        } else if let Ok(n) = part.parse::<i64>() {
            value |= n;
        } else {
            return None;
        }
    }
    usize::try_from(value).ok().filter(|v| *v < members.len())
}

/// A UTC time as an API answer writes it.
pub fn utc(value: &DateTime<Utc>) -> Value {
    Value::String(format_utc(value))
}

/// An optional UTC time as an API answer writes it, null when there is none.
pub fn utc_opt(value: &Option<DateTime<Utc>>) -> Value {
    value.as_ref().map_or(Value::Null, utc)
}

/// An answer object from its members, in order. Two members with the same name cannot both be
/// written: nothing is written, and the failure is answered as a 400 "Operation not valid".
pub fn anonymous(members: Vec<(&str, Value)>) -> Result<Value, AppError> {
    let mut map = serde_json::Map::with_capacity(members.len());
    for (name, value) in members {
        if map.contains_key(name) {
            return Err(AppError::InvalidOperation(format!(
                "The JSON property name for '{name}' collides with another property."
            )));
        }
        map.insert(name.to_string(), value);
    }
    Ok(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_try_parse_reads_names_numbers_and_combinations() {
        let members = ["OctoDownloads", "WholeLibrary"];
        let cases: [(Option<&str>, Option<usize>); 9] = [
            (None, None),
            (Some("wholelibrary"), Some(1)),
            (Some(" OctoDownloads "), Some(0)),
            (Some("1"), Some(1)),
            (Some("5"), None),
            (Some("OctoDownloads, WholeLibrary"), Some(1)),
            (Some(""), None),
            (Some("nope"), None),
            (Some("-1"), None),
        ];
        for (text, expected) in cases {
            assert_eq!(enum_try_parse(&members, text), expected, "case {text:?}");
        }
    }

    #[test]
    fn colliding_members_are_the_serializers_exception() {
        assert!(anonymous(vec![("busy", Value::from(1)), ("busy", Value::from(true))]).is_err());
        assert!(anonymous(vec![("a", Value::from(1)), ("b", Value::from(true))]).is_ok());
    }
}
