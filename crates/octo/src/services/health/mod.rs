//! Library health on the server: the Octo app's checks and fixes over the library snapshot, for
//! the dashboard.

pub mod health_fixes;
pub mod health_words;
pub mod keep_marks;
pub mod library_health;
pub mod library_health_service;
pub mod server_song_fields;

#[cfg(test)]
mod library_health_cases_tests;

pub use health_fixes::{FixAnswer, FixOutcome, FixStep, TagChange, fix_actions};
pub use library_health::{HealthCheck, HealthReport, HealthTag, IHealthFields};
pub use library_health_service::{
    HealthAbilities, HealthLookup, HealthRun, HealthServices, HealthSnapshot, LibraryHealthService,
    RunProgress,
};
pub use server_song_fields::ServerSongFields;
