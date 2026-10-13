//! Fetches every chart list that is on, so a listener's app finds them on its first look after a
//! start and at each new period: Symfonium and the like list playlists rarely, and a chart that
//! takes a few seconds to arrive would otherwise be missing until the next time they look.
//! Only the shared charts are fetched here; each listener's own library match is still made in
//! their own request, with their own sign-in.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use octo_core::common::Clock;
use octo_core::library::generated_playlist_service::period_index;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::services::common::TopSongsService;
use crate::services::library::ChartPlaylistService;
use crate::services::library::chart_playlist_service::REFRESH_HOURS;

/// How long after the start the first fetch waits, so the server finishes starting first.
const START_DELAY: Duration = Duration::from_secs(10);
/// A little after each period starts, so a clock a moment behind does not fetch the old period.
const AFTER_PERIOD: Duration = Duration::from_secs(30);

pub struct ChartsWarmWorker {
    top_songs: TopSongsService,
    playlists: ChartPlaylistService,
    clock: Clock,
}

impl ChartsWarmWorker {
    pub fn new(top_songs: TopSongsService, playlists: ChartPlaylistService, clock: Clock) -> Self {
        Self {
            top_songs,
            playlists,
            clock,
        }
    }

    /// Fetches every list that is on, Popular right now first. A genre the country lacks answers
    /// at once without asking anyone (`TopSongsService::chart`).
    pub async fn warm_once(&self) -> usize {
        let on = self.playlists.lists_on();
        if on.is_empty() {
            return 0;
        }
        // The country and its menu first, so the lists below are read for them.
        self.top_songs.learn().await;
        let mut fetched = 0;
        for chart in &on {
            if !self.top_songs.chart(chart).await.songs.is_empty() {
                fetched += 1;
            }
        }
        debug!("chart lists warmed: {fetched} of {}", on.len());
        fetched
    }

    pub async fn run(self: Arc<Self>, stopping: CancellationToken) -> anyhow::Result<()> {
        tokio::select! {
            _ = tokio::time::sleep(START_DELAY) => {}
            _ = stopping.cancelled() => return Ok(()),
        }
        while !stopping.is_cancelled() {
            tokio::select! {
                _ = self.warm_once() => {}
                _ = stopping.cancelled() => return Ok(()),
            }
            tokio::select! {
                _ = tokio::time::sleep(until_next_period(self.clock.now())) => {}
                _ = stopping.cancelled() => return Ok(()),
            }
        }
        Ok(())
    }
}

/// How long until the next period starts, plus [`AFTER_PERIOD`].
pub fn until_next_period(now: DateTime<Utc>) -> Duration {
    let next = period_index(now, REFRESH_HOURS) + 1;
    let start = DateTime::UNIX_EPOCH + TimeDelta::hours(next * i64::from(REFRESH_HOURS));
    (start - now).to_std().unwrap_or_default() + AFTER_PERIOD
}

#[cfg(test)]
#[path = "charts_warm_worker_tests.rs"]
mod tests;
