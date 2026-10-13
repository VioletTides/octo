//! Octo Family: the pure model behind multi-user accounts. Nothing here does I/O;
//! the services in `crates/octo/src/services/family` keep the stores and talk to Navidrome.
//!
//! - [`abilities`]: what a member may do, by preset plus per-member edits.
//! - [`policy`]: the decisions those abilities lead to (heart, stream caps, requests).
//! - [`request`]: requests for a copy of a song, and the weekly quota.
//! - [`device`]: devices, app names, home or away, and the client address behind proxies.
//! - [`quality`]: each account's own audio quality, and how a device picks it.
//! - [`codes`]: app passwords, pairing codes, tokens and Subsonic token math.
//! - [`endpoints`]: every Subsonic, OpenSubsonic, Octo and Navidrome endpoint in a class.

pub mod abilities;
pub mod codes;
pub mod device;
pub mod endpoints;
pub mod policy;
pub mod quality;
pub mod request;
