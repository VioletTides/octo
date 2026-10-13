//! The dashboard sign-in and the endpoints that read files: the Navidrome admin sign-in
//! (`browse/auth`), the recovery code (`auth/recovery`), the folder picker (`browse`), who is
//! signed in (`browse/session`), signing out, and the tag preview.

use std::net::SocketAddr;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use octo_core::common::dotnet::is_blank;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::json_body::{FromBody, Shape, text_record};
use super::json_body::{bind_body, bind_optional_body};
use super::plumbing::{
    BROWSE_COOKIE_NAME, BROWSE_TOKEN_HEADER, BrowseUser, browse_cookie, browse_cookie_deleted,
    browse_cookie_for, camel_case_value, error_json, header_value, is_https, ok, query_value, request_cookie,
};
use crate::app::AppState;
use crate::middleware::forwarded::RequestScheme;
use crate::services::admin::sign_in_throttle::key_of;
use crate::services::admin::{BrowseSessionStore, SignInThrottle};

/// The address a request came from, for the sign-in throttle and the log.
pub type Remote = Option<Extension<ConnectInfo<SocketAddr>>>;

fn remote_key(remote: &Remote) -> String {
    key_of(remote.as_ref().map(|Extension(ConnectInfo(addr))| *addr))
}

/// A 429 with `Retry-After` in whole seconds, as the sign-in endpoints answer a wait.
fn too_many(wait: chrono::TimeDelta, words: &str) -> Response {
    let mut res = error_json(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("{words} Try again in {}.", SignInThrottle::minutes(wait)),
    );
    if let Ok(value) = HeaderValue::from_str(&SignInThrottle::retry_after_seconds(wait).to_string()) {
        res.headers_mut().insert(header::RETRY_AFTER, value);
    }
    res
}

/// Credentials for `browse/auth`. Body-only by design.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct BrowseAuthRequest {
    pub username: Option<String>,
    pub password: Option<String>,
}

impl FromBody for BrowseAuthRequest {
    const SHAPE: &'static Shape = text_record!(
        "Octo.Controllers.AdminController+BrowseAuthRequest",
        "username",
        "password"
    );
}

/// `POST /api/admin/browse/auth`: exchanges Navidrome admin credentials for a dashboard sign-in,
/// remembered by this browser.
///
/// Credentials arrive in the body, never the query string, so they cannot end up in access
/// logs or a referrer. Verification is delegated to the Navidrome Octo already fronts: no new
/// credential store, and admin rights are Navidrome's call rather than something Octo asserts
/// for itself.
pub async fn browse_auth(
    State(state): State<AppState>,
    scheme: Option<Extension<RequestScheme>>,
    remote: Remote,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request: BrowseAuthRequest = match bind_body(&headers, &body, "req") {
        Ok(request) => request,
        Err(res) => return res,
    };
    let url = state
        .settings
        .current()
        .subsonic
        .url
        .as_deref()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    if url.is_empty() {
        return error_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "Navidrome URL is not configured yet. Use the recovery code instead.",
        );
    }
    let username = request.username.unwrap_or_default();
    let password = request.password.unwrap_or_default();
    if is_blank(&username) || is_blank(&password) {
        return error_json(StatusCode::UNAUTHORIZED, "Username and password are required.");
    }

    let throttle = &state.sign_in_throttle;
    let try_key = remote_key(&remote);
    if let Some(wait) = throttle.retry_after(&try_key) {
        return too_many(wait, "Too many wrong sign-ins.");
    }

    let payload = octo_core::json::to_string(&json!({ "username": username, "password": password }));
    let reply = state
        .http
        .post(format!("{url}/auth/login"))
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .body(payload)
        .send()
        .await;
    /// What Navidrome's answer means for the sign-in.
    enum Outcome {
        /// Navidrome's own limit (5 sign-ins per 20 seconds per address) counts every sign-in
        /// through Octo as Octo's, so it is said rather than calling the password wrong.
        Limited,
        /// Wrong credentials: counts toward the wait.
        Rejected,
        NotAdmin,
        Admin(Bytes),
    }
    let outcome: Result<Outcome, String> = match reply {
        Err(e) => Err(crate::services::http_client_factory::connect_failure_message(&e)),
        Ok(response) if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => Ok(Outcome::Limited),
        Ok(response) if !response.status().is_success() => Ok(Outcome::Rejected),
        Ok(response) => match response.bytes().await {
            Err(e) => Err(e.to_string()),
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Err(e) => Err(e.to_string()),
                Ok(document) => {
                    let is_admin = document.get("isAdmin") == Some(&serde_json::Value::Bool(true));
                    Ok(if is_admin {
                        Outcome::Admin(bytes)
                    } else {
                        Outcome::NotAdmin
                    })
                }
            },
        },
    };
    match outcome {
        Err(message) => {
            warn!("Browse auth against Navidrome failed: {message}");
            error_json(
                StatusCode::BAD_GATEWAY,
                "Could not reach Navidrome to verify credentials. If it is down, use the recovery code.",
            )
        }
        Ok(Outcome::Limited) => error_json(
            StatusCode::TOO_MANY_REQUESTS,
            "Navidrome is limiting sign-ins right now. Wait half a minute and try again.",
        ),
        Ok(Outcome::Rejected) => {
            throttle.failed(&try_key);
            error_json(StatusCode::UNAUTHORIZED, "Navidrome rejected those credentials.")
        }
        Ok(Outcome::NotAdmin) => {
            error_json(StatusCode::UNAUTHORIZED, "That account is not a Navidrome admin.")
        }
        Ok(Outcome::Admin(login_body)) => {
            // Octo's own admin identity, as a relayed admin login already sets it: rescans and
            // the hourly role check need one, and a fresh install may have no other.
            state.navidrome_identity.capture_login(&login_body);
            throttle.succeeded(&try_key);
            info!("Browse session opened for Navidrome admin {username}");
            let token = state.browse_sessions.create(&username);
            // Hand the session back as an HttpOnly cookie rather than something the page has
            // to hold. It survives a reload, so the user is not asked to sign in again every
            // time they come back to the settings, and script on the page cannot read it even
            // if something managed to inject some.
            let mut res = ok(&json!({ "ok": true, "user": username }));
            res.headers_mut().append(
                header::SET_COOKIE,
                browse_cookie(&token, is_https(scheme.as_deref())),
            );
            res
        }
    }
}

/// `GET /api/admin/browse?path=`: lists directories so the download folder can be picked
/// rather than typed. Requires a browse session; a 401 discloses nothing about the filesystem,
/// not even whether a path exists.
pub async fn browse(
    State(state): State<AppState>,
    scheme: Option<Extension<RequestScheme>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    // Cookie first (how the admin UI authenticates), header second so the endpoint stays
    // usable from curl or a script without one.
    let token = header_value(&headers, BROWSE_TOKEN_HEADER);
    let signed_in = BrowseUser::check(&state, &headers, scheme.as_deref(), token.as_deref());
    if !signed_in.signed_in() {
        return error_json(StatusCode::UNAUTHORIZED, "Browse session required.");
    }
    let path = query_value(query.as_deref(), "path");
    let result = state.directory_browser.browse(path.as_deref());
    let mut answer = serde_json::to_value(&result).unwrap_or_default();
    if let Some(map) = answer.as_object_mut() {
        // Under Docker this is the container's mount namespace, not the host's drives. Saying
        // so in the payload keeps the UI honest about why a user's D: drive is nowhere to be
        // seen. (Only a directory counts: /.dockerenv is a file, so this is false in practice.)
        map.insert(
            "containerised".into(),
            serde_json::Value::Bool(std::path::Path::new("/.dockerenv").is_dir()),
        );
    }
    signed_in.finish(ok(&answer))
}

/// The recovery code, typed on the sign-in screen. Body-only, like the password.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RecoveryRequest {
    pub code: Option<String>,
}

impl FromBody for RecoveryRequest {
    const SHAPE: &'static Shape = text_record!("Octo.Controllers.AdminController+RecoveryRequest", "code");
}

/// `POST /api/admin/auth/recovery`: sign in with the one-time code from `admin-recovery-code`,
/// for when Navidrome cannot vouch for anyone. The session lasts an hour, is never written to
/// disk, and cannot reach the endpoints that change library files. The code is replaced as soon
/// as it works.
pub async fn recovery_sign_in(
    State(state): State<AppState>,
    scheme: Option<Extension<RequestScheme>>,
    remote: Remote,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request: Option<RecoveryRequest> = match bind_optional_body(&headers, &body) {
        Ok(request) => request,
        Err(res) => return res,
    };
    let throttle = &state.sign_in_throttle;
    let recovery = &state.admin_recovery_code;
    let try_key = remote_key(&remote);
    if let Some(wait) = throttle.retry_after(&try_key) {
        return too_many(wait, "Too many wrong tries.");
    }
    if !recovery.try_use(request.and_then(|r| r.code).as_deref()) {
        throttle.failed(&try_key);
        return error_json(
            StatusCode::UNAUTHORIZED,
            "That is not the recovery code. Each code works once, so read the file again.",
        );
    }
    throttle.succeeded(&try_key);
    warn!(
        "The dashboard was opened with the recovery code from {try_key}. A new code is in {}.",
        recovery.file_path().display()
    );
    let token = state.browse_sessions.create_recovery();
    let mut res = ok(&json!({
        "ok": true,
        "user": BrowseSessionStore::RECOVERY_USER,
        "recovery": true,
    }));
    res.headers_mut().append(
        header::SET_COOKIE,
        browse_cookie_for(
            &token,
            is_https(scheme.as_deref()),
            BrowseSessionStore::RECOVERY_TTL,
        ),
    );
    res
}

/// `GET /api/admin/browse/session`: who this browser is signed in as, and whether the sign-in is
/// off. Never prompts; answers without a sign-in. The cookie alone counts here.
pub async fn browse_session(
    State(state): State<AppState>,
    scheme: Option<Extension<RequestScheme>>,
    headers: HeaderMap,
) -> Response {
    let cookie = request_cookie(&headers, BROWSE_COOKIE_NAME);
    let user = state.browse_sessions.user_of(cookie.as_deref());
    let recovery = user.as_deref() == Some(BrowseSessionStore::RECOVERY_USER);
    let off = state.settings.current().admin.sign_in_off();
    let mut res = ok(&json!({
        "signedIn": user.is_some(),
        "user": user,
        "recovery": recovery,
        "signInOff": off,
    }));
    if user.is_some()
        && let Some(cookie) = cookie
    {
        let age = if recovery {
            BrowseSessionStore::RECOVERY_TTL
        } else {
            BrowseSessionStore::TTL
        };
        res.headers_mut().append(
            header::SET_COOKIE,
            browse_cookie_for(&cookie, is_https(scheme.as_deref()), age),
        );
    }
    res
}

/// `POST /api/admin/browse/signout`: signs this browser out; the session is forgotten and the
/// cookie removed. With `everywhere`, every browser and script signed in as the same person is
/// signed out too.
pub async fn browse_sign_out(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let everywhere = match super::request_binding::query_bool(&uri, "everywhere") {
        Ok(everywhere) => everywhere,
        Err(res) => return *res,
    };
    let cookie = request_cookie(&headers, BROWSE_COOKIE_NAME);
    let mut ended = 0;
    let user = everywhere
        .then(|| state.browse_sessions.user_of(cookie.as_deref()))
        .flatten()
        .filter(|user| user != BrowseSessionStore::RECOVERY_USER);
    match user {
        Some(user) => {
            ended = state.browse_sessions.revoke_user(&user);
            info!("{user} signed out everywhere ({ended} sign-ins ended)");
        }
        None => state.browse_sessions.revoke(cookie.as_deref()),
    }
    let mut res = ok(&json!({ "ok": true, "ended": ended }));
    res.headers_mut()
        .append(header::SET_COOKIE, browse_cookie_deleted());
    res
}

/// A file inside the music folder, or an artist and a title, to try the matching on.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TagPreviewRequest {
    pub path: Option<String>,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
}

impl FromBody for TagPreviewRequest {
    const SHAPE: &'static Shape = text_record!(
        "Octo.Controllers.AdminController+TagPreviewRequest",
        "path",
        "artist",
        "title",
        "album"
    );
}

/// `POST /api/admin/tags/preview`: how a song would be matched and tagged, without touching
/// anything. Gated on the browse sign-in like the other endpoints that read files, and a path
/// is only ever a file inside the music folder, resolved in full, with no link on the way.
pub async fn preview_tags(
    State(state): State<AppState>,
    scheme: Option<Extension<RequestScheme>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request: TagPreviewRequest = match bind_body(&headers, &body, "request") {
        Ok(request) => request,
        Err(res) => return res,
    };
    let token = header_value(&headers, BROWSE_TOKEN_HEADER);
    let signed_in = BrowseUser::check(&state, &headers, scheme.as_deref(), token.as_deref());
    if !signed_in.signed_in() {
        return error_json(
            StatusCode::UNAUTHORIZED,
            "Sign in with your Navidrome admin account first.",
        );
    }

    let mut path = None;
    if let Some(candidate) = request.path.as_deref().filter(|p| !is_blank(p)) {
        let configured = state
            .settings
            .raw("Library:DownloadPath")
            .unwrap_or_else(|| "./downloads".to_string());
        let root = state.navidrome_identity.effective_download_path(&configured);
        path = resolve_under_root(candidate, &root);
        if path.is_none() {
            return signed_in.finish(error_json(
                StatusCode::BAD_REQUEST,
                "The path must be a file inside the music folder.",
            ));
        }
    } else if request.artist.as_deref().is_none_or(is_blank) || request.title.as_deref().is_none_or(is_blank)
    {
        return signed_in.finish(error_json(
            StatusCode::BAD_REQUEST,
            "Give a path inside the music folder, or an artist and a title.",
        ));
    }

    let report = state
        .tag_preview
        .preview(
            path.as_deref(),
            request.artist.as_deref(),
            request.title.as_deref(),
            request.album.as_deref(),
            &CancellationToken::new(),
        )
        .await;
    let answer = match report {
        Ok(report) => {
            let value = serde_json::to_value(&report).unwrap_or_default();
            ok(&camel_case_value(value, &["fields", "stageSeconds"]))
        }
        Err(_) => crate::http::error::problem(StatusCode::INTERNAL_SERVER_ERROR),
    };
    signed_in.finish(answer)
}

/// Made absolute against the working folder, with `.`, `..` (never above the root) and
/// repeated separators worked out on the text alone.
fn full_path(path: &str) -> Option<String> {
    if path.contains('\0') {
        return None;
    }
    let combined = if path.starts_with('/') {
        path.to_string()
    } else {
        let cwd = std::env::current_dir().ok()?.to_string_lossy().into_owned();
        format!("{}/{path}", cwd.trim_end_matches('/'))
    };
    let trailing = combined.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut full = format!("/{}", parts.join("/"));
    if trailing && full != "/" && !combined.ends_with("/.") && !combined.ends_with("/..") {
        full.push('/');
    }
    Some(full)
}

/// The path from `relative_to` to `path`, for two full Unix paths (case-sensitive).
fn relative_path(relative_to: &str, path: &str) -> String {
    let from: Vec<&str> = relative_to.split('/').filter(|s| !s.is_empty()).collect();
    let to: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == from.len() && common == to.len() {
        return ".".to_string();
    }
    let mut parts: Vec<&str> = vec![".."; from.len() - common];
    parts.extend(&to[common..]);
    parts.join("/")
}

fn is_link(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// The full path of a file that really sits under the root: no ".." out of it, no path of its
/// own, and no link on the way, so the preview cannot be pointed at any other file.
pub fn resolve_under_root(candidate: &str, root: &str) -> Option<String> {
    let full = full_path(candidate)?;
    let root_full = full_path(root)?;
    let root_full = root_full.trim_end_matches('/');
    let relative = relative_path(root_full, &full);
    if relative == "." || relative.starts_with("..") || relative.starts_with('/') {
        return None;
    }

    // A file (not a folder) that exists, and is not itself a link.
    let metadata = std::fs::symlink_metadata(&full).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let mut dir = std::path::Path::new(&full).parent();
    while let Some(d) = dir {
        let text = d.to_string_lossy();
        if text.len() <= root_full.len() {
            break;
        }
        if is_link(&text) {
            return None;
        }
        dir = d.parent();
    }
    Some(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(prefix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("{prefix}{}", uuid::Uuid::new_v4().simple()))
    }

    #[test]
    fn resolve_under_root_refuses_dot_dot_rooted_and_missing_accepts_a_file_inside() {
        let root = temp("octo-root-");
        std::fs::create_dir_all(root.join("Artist")).unwrap();
        let inside = root.join("Artist").join("song.flac");
        std::fs::write(&inside, [1u8]).unwrap();
        let outside =
            std::env::temp_dir().join(format!("octo-outside-{}.flac", uuid::Uuid::new_v4().simple()));
        std::fs::write(&outside, [1u8]).unwrap();
        let root_s = root.to_string_lossy().to_string();
        let inside_s = inside.to_string_lossy().to_string();

        assert_eq!(
            resolve_under_root(&inside_s, &root_s).as_deref(),
            Some(inside_s.as_str())
        );
        assert_eq!(
            resolve_under_root(&format!("{root_s}/Artist/../Artist/song.flac"), &root_s).as_deref(),
            Some(inside_s.as_str())
        );
        let name = outside.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(resolve_under_root(&format!("{root_s}/../{name}"), &root_s), None);
        assert_eq!(resolve_under_root(&outside.to_string_lossy(), &root_s), None);
        assert_eq!(
            resolve_under_root(&format!("{root_s}/Artist/missing.flac"), &root_s),
            None
        );
        assert_eq!(resolve_under_root(&root_s, &root_s), None);

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }

    /// A link inside the folder that points outside is refused. Unix only: making a symlink on
    /// Windows needs another API and, usually, an elevated process; Octo runs on Linux.
    #[cfg(unix)]
    #[test]
    fn resolve_under_root_refuses_a_symlink() {
        let root = temp("octo-root-");
        std::fs::create_dir_all(&root).unwrap();
        let outside =
            std::env::temp_dir().join(format!("octo-outside-{}.flac", uuid::Uuid::new_v4().simple()));
        std::fs::write(&outside, [1u8]).unwrap();
        let link = root.join("link.flac");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_eq!(
            resolve_under_root(&link.to_string_lossy(), &root.to_string_lossy()),
            None
        );

        // A linked folder on the way is refused too.
        let real_dir = temp("octo-real-");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join("song.flac"), [1u8]).unwrap();
        std::os::unix::fs::symlink(&real_dir, root.join("linked")).unwrap();
        assert_eq!(
            resolve_under_root(
                &root.join("linked/song.flac").to_string_lossy(),
                &root.to_string_lossy()
            ),
            None
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&real_dir);
        let _ = std::fs::remove_file(&outside);
    }
}
