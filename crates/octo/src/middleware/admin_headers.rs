//! The admin security headers: the dashboard may be framed only by Octo itself, nothing sniffs
//! its answers into another type, no address leaks into a referrer, and admin answers (settings
//! among them) are never kept by a browser or proxy cache unless the endpoint says otherwise
//! (cover thumbnails do).

use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

use super::admin_guard::is_admin_path;

/// Whether a path is `/admin` or below it: case-insensitive, and only at a segment boundary.
pub fn is_dashboard_path(path: &str) -> bool {
    let prefix = "/admin";
    path.len() >= prefix.len()
        && path[..prefix.len()].eq_ignore_ascii_case(prefix)
        && (path.len() == prefix.len() || path.as_bytes()[prefix.len()] == b'/')
}

pub async fn admin_security_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let api = is_admin_path(path);
    if !api && !is_dashboard_path(path) {
        return next.run(req).await;
    }
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'self'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    if api && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_paths_match_at_segment_boundaries_ignoring_case() {
        assert!(is_dashboard_path("/admin"));
        assert!(is_dashboard_path("/Admin/index.html"));
        assert!(!is_dashboard_path("/administrator"));
        assert!(!is_dashboard_path("/api/admin"));
    }
}
