//! Octo normally runs one hop behind a container ingress or tunnel. Only the original scheme
//! is accepted from it, which is required when generating absolute Radio stream URLs; client
//! IP and Host continue to come from the direct request. The proxy address is dynamic in
//! container networks, so no proxy list applies: `X-Forwarded-Proto` is trusted from anyone,
//! one hop deep.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

/// The scheme the client used, after the forwarded header is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestScheme(pub String);

pub async fn forwarded_proto(mut req: Request, next: Next) -> Response {
    // One hop deep: the right-most value of the header.
    let scheme = req
        .headers()
        .get_all("x-forwarded-proto")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .rfind(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "http".to_string());
    req.extensions_mut().insert(RequestScheme(scheme));
    next.run(req).await
}
