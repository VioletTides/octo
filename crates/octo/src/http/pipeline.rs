//! The assembled application: every route, the catch-all and the middleware (see
//! `crate::middleware` for the order and why).
//!
//! # Registering routes
//!
//! Each feature module exposes `pub fn routes() -> RouteSet` with its templates, in axum
//! syntax and the casing the apps use (matching ignores case and a trailing slash):
//!
//! ```ignore
//! pub fn routes() -> RouteSet {
//!     RouteSet::new()
//!         .subsonic("ping", get(ping).post(ping))           // rest/ping and rest/ping.view
//!         .route("/api/admin/settings", get(get_settings).post(save_settings))
//! }
//! ```
//!
//! and [`app_routes`] merges it. Handlers take `State<AppState>`. A method a route does not list
//! falls through to the catch-all, never a 405, and so does HEAD on a GET route unless the route
//! is registered with `route_with_head`.

use std::convert::Infallible;

use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::response::Response;
use tower::ServiceBuilder;
use tower::util::BoxCloneSyncService;

use super::routes::RouteSet;
use super::static_files::StaticAssets;
use super::{admin_root, catch_all};
use crate::app::AppState;
use crate::middleware::admin_sign_in_gate::GateState;
use crate::middleware::{
    admin_guard, admin_headers, admin_port_split, admin_sign_in_gate, cors, exception, family_gate,
    forwarded, request_log, size_flag,
};
use crate::services::you_tube::stream_size_warmer;

/// The whole application as one service, for `axum::serve` and for tests (`oneshot`).
pub type App = BoxCloneSyncService<Request, Response, Infallible>;

/// Every route Octo answers itself. Add each controller's `routes()` here.
pub fn app_routes(assets: &StaticAssets) -> RouteSet {
    RouteSet::new()
        .merge(assets.routes())
        .merge(admin_root::routes())
        .merge(crate::controllers::subsonic::routes())
        .merge(crate::controllers::admin::routes())
        .merge(crate::controllers::lidarr_webhook::routes())
        .merge(crate::controllers::family::routes())
}

/// The whole application. `admin_port`: the dashboard's own port (`Admin:Port`, 0 when it
/// shares Octo's), which the listener puts on each request it takes.
pub fn build(state: AppState, assets: &StaticAssets, admin_port: u16) -> App {
    build_with_port(state, app_routes(assets), admin_port)
}

/// The pipeline around an explicit route set (tests add their own routes this way), with the
/// dashboard on Octo's own port.
pub fn build_with(state: AppState, routes: RouteSet) -> App {
    build_with_port(state, routes, 0)
}

/// [`build_with`] with the dashboard on a port of its own.
pub fn build_with_port(state: AppState, routes: RouteSet, admin_port: u16) -> App {
    let (router, canon) = routes.finish(catch_all::catch_all);
    let gate = GateState {
        app: state.clone(),
        canon: Arc::new(canon.clone()),
    };
    let warmer = state.stream_size_warmer.clone();
    let size_flag_state = state.clone();
    // The family gate reads the path after it is spelled as the routes spell it.
    let family_state = state.clone();
    let router = router.with_state(state);
    let service = ServiceBuilder::new()
        .layer(from_fn(request_log::request_log))
        .layer(from_fn(forwarded::forwarded_proto))
        .layer(from_fn_with_state(admin_port, admin_port_split::admin_port_split))
        .layer(from_fn(admin_headers::admin_security_headers))
        .layer(from_fn(admin_guard::admin_request_guard))
        .layer(from_fn_with_state(gate, admin_sign_in_gate::admin_sign_in_gate))
        .layer(from_fn(cors::cors))
        .layer(from_fn(exception::catch_panic))
        .layer(from_fn_with_state(size_flag_state, size_flag::size_flag))
        // Each answer is one listing for the stream size warmer.
        .layer(from_fn_with_state(warmer, stream_size_warmer::listing_scope))
        .map_request(move |mut req: Request| {
            // The path as the client spelled it, which a few actions read (the relay
            // target of getSimilarSongs and the playlist mutations keeps the client's casing).
            // axum's router keeps an `OriginalUri` it finds rather than adding its own.
            let original = axum::extract::OriginalUri(req.uri().clone());
            req.extensions_mut().insert(original);
            canon.rewrite(&mut req);
            req
        })
        .layer(from_fn_with_state(family_state, family_gate::family_gate))
        // axum adds `Allow` to whatever a route's method fallback answers; that fallback is
        // the catch-all, which answers with no such header (no Octo answer has one).
        .map_response(|mut res: Response| {
            res.headers_mut().remove(axum::http::header::ALLOW);
            res
        })
        .service(router);
    BoxCloneSyncService::new(service)
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
