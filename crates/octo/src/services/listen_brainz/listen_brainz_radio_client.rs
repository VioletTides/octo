//! ListenBrainz's listening data for radio: the labs datasets (no token, at most one call a
//! second as its docs ask) and LB Radio (a user token since 2026-08-21).

use std::sync::Arc;
use std::time::Duration;

use octo_core::common::{dotnet, octo_user_agent};
use octo_core::json::element::{ElementResult, try_get_property};
use octo_core::settings::SettingsStore;
use parking_lot::Mutex;
use reqwest::header::AUTHORIZATION;
use serde_json::Value;
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::services::framework::MemoryCache;
use crate::services::framework::http::client_builder;

/// A song ListenBrainz suggested.
#[derive(Debug, Clone, PartialEq)]
pub struct ListenBrainzTrack {
    pub artist: String,
    pub title: String,
    pub score: f64,
    pub duration_seconds: Option<i32>,
}

/// What the one cache holds: a recording lookup or a list of tracks.
#[derive(Clone)]
enum Cached {
    Recording(Option<String>),
    Tracks(Vec<ListenBrainzTrack>),
}

pub struct ListenBrainzRadioClient {
    /// `listenbrainz-radio`: 10 seconds, with Octo's user agent.
    http: reqwest::Client,
    labs_url: String,
    api_url: String,
    /// The ListenBrainz settings: the token is read at every call.
    settings: Arc<SettingsStore>,
    minimum_gap: Duration,
    /// Longest a call waits for its one-a-second slot. Past that it gives up with nothing, which
    /// is not a failure: a station build's calls queued ahead are not the source going wrong, and
    /// song radio must not wait out its whole timeout behind them.
    max_queue_wait: Duration,
    next: Mutex<Option<Instant>>,
    cache: MemoryCache<Cached>,
}

impl ListenBrainzRadioClient {
    pub const CLIENT_NAME: &'static str = "listenbrainz-radio";
    pub const LABS_URL: &'static str = "https://labs.api.listenbrainz.org";
    pub const API_URL: &'static str = "https://api.listenbrainz.org";
    pub const MINIMUM_GAP: Duration = Duration::from_secs(1);
    pub const MAX_QUEUE_WAIT: Duration = Duration::from_secs(3);

    pub fn new(settings: Arc<SettingsStore>) -> Self {
        Self::with_urls(settings, Self::LABS_URL, Self::API_URL)
    }

    /// A client that asks other hosts: a test's mock server.
    pub fn with_urls(settings: Arc<SettingsStore>, labs_url: &str, api_url: &str) -> Self {
        ListenBrainzRadioClient {
            http: client_builder()
                .timeout(Duration::from_secs(10))
                .user_agent(octo_user_agent::value())
                .build()
                .expect("the ListenBrainz radio client builds"),
            labs_url: labs_url.trim_end_matches('/').to_string(),
            api_url: api_url.trim_end_matches('/').to_string(),
            settings,
            minimum_gap: Self::MINIMUM_GAP,
            max_queue_wait: Self::MAX_QUEUE_WAIT,
            next: Mutex::new(None),
            cache: MemoryCache::new(4000),
        }
    }

    /// The pacing, for tests (`MinimumGap`, `MaxQueueWait`).
    pub fn with_pacing(mut self, minimum_gap: Duration, max_queue_wait: Duration) -> Self {
        self.minimum_gap = minimum_gap;
        self.max_queue_wait = max_queue_wait;
        self
    }

    fn token(&self) -> String {
        self.settings.current().listen_brainz.token.clone()
    }

    pub fn has_token(&self) -> bool {
        !dotnet::is_blank(&self.token())
    }

    /// The recording MusicBrainz knows for an artist and title, by ListenBrainz's near-exact
    /// lookup.
    pub async fn recording_mbid(&self, artist: &str, title: &str) -> Option<String> {
        let key = dotnet::to_lower_invariant(&format!("acr|{artist}|{title}"));
        if let Some(Cached::Recording(cached)) = self.cache.get(&key) {
            return cached;
        }
        let rows = self
            .labs(&format!(
                "/acr-lookup/json?artist_credit_name={}&recording_name={}",
                dotnet::escape_data_string(artist),
                dotnet::escape_data_string(title)
            ))
            .await;
        let mbid = rows
            .as_ref()
            .and_then(|rows| rows.iter().find_map(|row| text(row, "recording_mbid")));
        // Only a real answer is kept: a failed or skipped call says nothing about the song.
        if rows.is_some() {
            self.cache.set(
                key,
                Cached::Recording(mbid.clone()),
                1,
                Duration::from_secs(7 * 24 * 3600),
            );
        }
        mbid
    }

    pub async fn similar_recordings(
        &self,
        recording_mbid: &str,
        algorithm: &str,
        count: i32,
    ) -> Vec<ListenBrainzTrack> {
        let key = format!("similar|{recording_mbid}|{algorithm}");
        let tracks = match self.cache.get(&key) {
            Some(Cached::Tracks(tracks)) => tracks,
            _ => {
                let Some(rows) = self
                    .labs(&format!(
                        "/similar-recordings/json?recording_mbids={}&algorithm={}",
                        dotnet::escape_data_string(recording_mbid),
                        dotnet::escape_data_string(algorithm)
                    ))
                    .await
                else {
                    return Vec::new();
                };
                let parsed: Vec<(String, String, f64)> = rows
                    .iter()
                    .filter_map(|row| {
                        let score = number(row, "score").unwrap_or(0.0);
                        Some((
                            text(row, "artist_credit_name")?,
                            text(row, "recording_name")?,
                            score,
                        ))
                    })
                    .collect();
                // ListenBrainz's score is a count of shared listening sessions (216, 149, ...), so
                // it is scaled to the answer's best, 0 to 1, like every other source's match.
                let best = parsed
                    .iter()
                    .map(|(_, _, score)| *score)
                    .reduce(f64::max)
                    .unwrap_or(0.0);
                let tracks: Vec<ListenBrainzTrack> = parsed
                    .into_iter()
                    .map(|(artist, title, score)| ListenBrainzTrack {
                        artist,
                        title,
                        score: if best > 0.0 { score / best } else { 1.0 },
                        duration_seconds: None,
                    })
                    .collect();
                // ListenBrainz rebuilds these every Sunday.
                self.cache.set(
                    key,
                    Cached::Tracks(tracks.clone()),
                    1,
                    Duration::from_secs(24 * 3600),
                );
                tracks
            }
        };
        tracks.into_iter().take(count.max(0) as usize).collect()
    }

    /// LB Radio for a prompt ("artist:(Kordhell)", "tag:(phonk)"), in the mode the settings
    /// choose (each mode's answer kept apart); nothing without a token. `Err`
    /// for an answer whose `payload` is not the shape LB Radio writes.
    pub async fn lb_radio(&self, prompt: &str, count: i32) -> ElementResult<Vec<ListenBrainzTrack>> {
        let token = self.token();
        if dotnet::is_blank(&token) {
            return Ok(Vec::new());
        }
        let take = |tracks: Vec<ListenBrainzTrack>| tracks.into_iter().take(count.max(0) as usize).collect();
        let mode = self
            .settings
            .current()
            .radio_sources
            .effective_listen_brainz_radio_mode();
        let key = format!("lbradio|{mode}|{prompt}");
        if let Some(Cached::Tracks(cached)) = self.cache.get(&key) {
            return Ok(take(cached));
        }
        let request = self
            .http
            .get(format!(
                "{}/1/explore/lb-radio?prompt={}&mode={mode}",
                self.api_url,
                dotnet::escape_data_string(prompt)
            ))
            .header(AUTHORIZATION, format!("Token {}", token.trim()));
        let Some(doc) = self.send(request, "/1/explore/lb-radio").await else {
            return Ok(Vec::new());
        };
        let mut tracks = Vec::new();
        if let Some(payload) = try_get_property(&doc, "payload")?
            && let Some(jspf) = try_get_property(payload, "jspf")?
            && let Some(playlist) = try_get_property(jspf, "playlist")?
            && let Some(Value::Array(list)) = try_get_property(playlist, "track")?
        {
            for track in list {
                let title = text(track, "title");
                let artist = text(track, "creator");
                let seconds = number(track, "duration")
                    .filter(|ms| *ms > 1000.0)
                    .map(|ms| (ms / 1000.0) as i32);
                if let (Some(title), Some(artist)) = (title, artist) {
                    tracks.push(ListenBrainzTrack {
                        artist,
                        title,
                        score: 1.0,
                        duration_seconds: seconds,
                    });
                }
            }
        }
        self.cache.set(
            key,
            Cached::Tracks(tracks.clone()),
            1,
            Duration::from_secs(6 * 3600),
        );
        Ok(take(tracks))
    }

    /// The rows of a labs answer; None when there was no answer to read (failed or skipped).
    async fn labs(&self, path_and_query: &str) -> Option<Vec<Value>> {
        let request = self.http.get(format!("{}{path_and_query}", self.labs_url));
        let path = path_and_query.split('?').next().unwrap_or("");
        let doc = self.send(request, path).await?;
        let mut rows = Vec::new();
        collect_rows(&doc, &mut rows);
        Some(rows)
    }

    /// The wait for this call's one-a-second slot, the slot taken; None when it is more than the
    /// longest queue wait away. Only the slot is under the lock, never the call.
    fn reserve_slot(&self) -> Option<Duration> {
        let mut next = self.next.lock();
        let now = Instant::now();
        let slot = next.filter(|next| *next > now).unwrap_or(now);
        if slot - now > self.max_queue_wait {
            return None;
        }
        *next = Some(slot + self.minimum_gap);
        Some(slot - now)
    }

    /// An answer, or None when the call failed, was refused or was skipped as too far back in the
    /// queue.
    async fn send(&self, request: reqwest::RequestBuilder, path: &str) -> Option<Value> {
        let Some(wait) = self.reserve_slot() else {
            debug!("ListenBrainz is busy; {path} skipped");
            return None;
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                debug!("ListenBrainz {path} failed: {e}");
                return None;
            }
        };
        if response.status().as_u16() == 429 {
            warn!("ListenBrainz asked Octo to slow down ({path})");
            let mut next = self.next.lock();
            let later = Instant::now() + Duration::from_secs(10);
            if next.is_none_or(|next| later > next) {
                *next = Some(later);
            }
            return None;
        }
        if !response.status().is_success() {
            return None;
        }
        let body = match response.text().await {
            Ok(body) => body,
            Err(e) => {
                debug!("ListenBrainz {path} failed: {e}");
                return None;
            }
        };
        match serde_json::from_str(&body) {
            Ok(doc) => Some(doc),
            Err(e) => {
                debug!("ListenBrainz {path} failed: {e}");
                None
            }
        }
    }
}

/// Rows as the labs datasets give them: an array of rows, or blocks holding a "data" array of
/// rows.
pub fn collect_rows(element: &Value, rows: &mut Vec<Value>) {
    match element {
        Value::Array(items) => {
            for item in items {
                collect_rows(item, rows);
            }
        }
        Value::Object(map) => match map.get("data") {
            Some(data) => collect_rows(data, rows),
            None => rows.push(element.clone()),
        },
        _ => {}
    }
}

/// A number, or None for a missing, null or text value: one odd row never fails the answer.
fn number(element: &Value, name: &str) -> Option<f64> {
    element.as_object()?.get(name)?.as_f64()
}

/// A non-empty string.
fn text(element: &Value, name: &str) -> Option<String> {
    element
        .as_object()?
        .get(name)?
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}
