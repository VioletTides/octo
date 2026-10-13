//! The request pipeline, outermost first:
//!
//! 1. [`request_log`]: the hosting layer's "Request starting/finished" lines.
//! 2. [`forwarded`]: the forwarded scheme (only `X-Forwarded-Proto`).
//! 3. [`admin_port_split`]: the dashboard's own port, before anything else answers, so the
//!    wrong port never even serves a static file.
//! 4. [`admin_headers`]: the admin security headers, on every /admin and /api/admin answer,
//!    the guard's and the gate's refusals included.
//! 5. [`admin_guard`]: the admin request guard, outside CORS so it strips CORS headers.
//! 6. [`admin_sign_in_gate`]: the sign-in gate, nothing under /api/admin answers a caller who
//!    is not signed in.
//! 7. [`cors`]: CORS with the default policy.
//! 8. [`exception`]: the panic handler, inside CORS so error responses keep the CORS headers.
//! 9. [`size_flag`]: a size on every song in the answers to the apps that hide songs without
//!    one, read from the finished answer so the family rules and every handler came first.
//! 10. Path canonicalization.
//! 11. [`family_gate`]: with Family on, who is asking, members signed in to Navidrome with
//!     their hidden passwords, and the family rules. With Family off it hands every request
//!     on untouched.
//! 12. The routes (static files are routes too), falling back to the catch-all.
//!
//! There is no raw-body layer: an axum handler that needs the raw body extracts it as `Bytes`
//! and parses the form or JSON from those bytes itself, so the bytes are still there for the
//! faithful relay.

pub mod admin_guard;
pub mod admin_headers;
pub mod admin_port_split;
pub mod admin_sign_in_gate;
pub mod cors;
pub mod exception;
#[cfg(test)]
mod family_bypass_tests;
pub mod family_gate;
#[cfg(test)]
mod family_reading_tests;
pub mod forwarded;
pub mod request_log;
pub mod size_flag;
