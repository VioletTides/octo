//! Answers to the apps named in `Subsonic:SizeFlagClients` carry a size on every song they
//! should show (see [`octo_subsonic::size_flag`]). Each answer is read once it is finished, so
//! the rule holds for every endpoint, every cached search and every stored list alike. Every
//! other app's answer passes through untouched, as does anything that is not a Subsonic XML
//! or JSON answer: audio, covers, a compressed relay, a HEAD.

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, ETAG, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use bytes::BytesMut;
use futures::{StreamExt, stream};
use http_body_util::BodyExt;
use octo_subsonic::size_flag::{Format, flag_sizes};
use tracing::warn;

use crate::app::AppState;

/// The largest answer read; a larger one passes through untouched, whether or not it said its
/// length up front.
pub const MAX_BODY: usize = 64 * 1024 * 1024;

pub async fn size_flag(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if request.method() == Method::HEAD || !from_a_size_flag_client(&state, &request) {
        return next.run(request).await;
    }
    flag(next.run(request).await).await
}

/// A Subsonic call (under `/rest/`) from an app the setting names, by its `c` parameter or its
/// User-Agent. As everywhere in a Subsonic call, the key matches ignoring case and a repeated
/// key is all its values joined with commas.
fn from_a_size_flag_client(state: &AppState, request: &Request) -> bool {
    if !request.uri().path().to_ascii_lowercase().contains("/rest/") {
        return false;
    }
    let client = request.uri().query().map(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .filter(|(name, value)| name.eq_ignore_ascii_case("c") && !value.is_empty())
            .map(|(_, value)| value.into_owned())
            .collect::<Vec<_>>()
            .join(",")
    });
    let user_agent = request
        .headers()
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok());
    state
        .settings
        .current()
        .subsonic
        .is_size_flag_client(client.as_deref(), user_agent)
}

async fn flag(response: Response) -> Response {
    let Some(format) = answer_format(response.status(), response.headers()) else {
        return response;
    };
    let (mut parts, body) = response.into_parts();
    let bytes = match read_whole(body, MAX_BODY).await {
        Read::Whole(bytes) => bytes,
        Read::Unread(body) => return Response::from_parts(parts, body),
    };
    let Some(flagged) = flag_sizes(&bytes, format) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    parts
        .headers
        .insert(CONTENT_LENGTH, HeaderValue::from(flagged.len()));
    parts.headers.remove(ETAG);
    Response::from_parts(parts, Body::from(flagged))
}

/// An answer's body, read whole, or handed back unchanged.
enum Read {
    Whole(Bytes),
    /// Longer than the limit, or failed part way: the bytes read so far followed by the rest,
    /// exactly as the app would have had them.
    Unread(Body),
}

async fn read_whole(mut body: Body, limit: usize) -> Read {
    let mut chunks: Vec<Bytes> = Vec::new();
    let mut length = 0;
    loop {
        match body.frame().await {
            None => break,
            Some(Ok(frame)) => {
                let Ok(chunk) = frame.into_data() else {
                    continue;
                };
                length += chunk.len();
                chunks.push(chunk);
                if length > limit {
                    let rest = body.into_data_stream();
                    return Read::Unread(Body::from_stream(
                        stream::iter(chunks.into_iter().map(Ok)).chain(rest),
                    ));
                }
            }
            Some(Err(error)) => {
                warn!("An answer failed part way while being read for its song sizes: {error}");
                let read = stream::iter(chunks.into_iter().map(Ok));
                return Read::Unread(Body::from_stream(read.chain(stream::once(async { Err(error) }))));
            }
        }
    }
    let mut whole = BytesMut::with_capacity(length);
    for chunk in chunks {
        whole.extend_from_slice(&chunk);
    }
    Read::Whole(whole.freeze())
}

/// The answer's shape when it is a plain 200 Subsonic XML or JSON answer small enough to read.
fn answer_format(status: StatusCode, headers: &HeaderMap) -> Option<Format> {
    if status != StatusCode::OK || headers.contains_key(CONTENT_ENCODING) {
        return None;
    }
    let too_large = headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_BODY);
    if too_large {
        return None;
    }
    let content_type = headers.get(CONTENT_TYPE)?.to_str().ok()?.to_ascii_lowercase();
    let essence = content_type.split(';').next().unwrap_or_default().trim();
    match essence {
        "application/xml" | "text/xml" => Some(Format::Xml),
        "application/json" | "text/json" => Some(Format::Json),
        _ => None,
    }
}

#[cfg(test)]
#[path = "size_flag_tests.rs"]
mod tests;
