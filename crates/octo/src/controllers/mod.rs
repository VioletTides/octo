//! The controllers, one module per controller. Each registers its
//! actions with `pub fn routes() -> RouteSet`, merged into [`crate::http::pipeline::app_routes`].

pub mod admin;
pub mod family;
pub mod lidarr_webhook;
pub mod subsonic;
