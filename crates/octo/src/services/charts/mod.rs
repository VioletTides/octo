//! Charts and new songs from Apple Music's public pages: fetching them, and keeping them warm.
//! The pure half (parsing, settings, merging) is `octo_core::charts`.

pub mod apple_music_pages;
pub mod charts_warm_worker;

pub use apple_music_pages::AppleMusicPages;
pub use charts_warm_worker::ChartsWarmWorker;
