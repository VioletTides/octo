//! Spotify's Web API for imports, with `ImportKeys`.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use futures::FutureExt;
use futures::future::BoxFuture;
use octo_core::common::Clock;
use octo_core::common::dotnet::{self, escape_data_string, is_blank};
use octo_core::common::song_identity::SongIdentity;
use parking_lot::Mutex;
use serde_json::Value;
use tracing::info;

use super::ImportTrack;

/// A refusal from Spotify, in words a person can act on. Status is the HTTP status, when there
/// was one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SpotifyException {
    pub message: String,
    pub status: Option<u16>,
    /// The sign-in itself is over (revoked, or six months old): only signing in again helps.
    pub sign_in_ended: bool,
}

impl SpotifyException {
    pub fn new(message: impl Into<String>) -> Self {
        SpotifyException {
            message: message.into(),
            status: None,
            sign_in_ended: false,
        }
    }

    pub fn with_status(message: impl Into<String>, status: u16) -> Self {
        SpotifyException {
            status: Some(status),
            ..Self::new(message)
        }
    }

    pub fn ended(message: impl Into<String>, status: Option<u16>) -> Self {
        SpotifyException {
            status,
            sign_in_ended: true,
            ..Self::new(message)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyProfile {
    pub id: String,
    pub display_name: Option<String>,
}

/// A playlist as the account lists it. Total is what Spotify says it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyPlaylistInfo {
    pub id: String,
    pub name: String,
    pub owner_id: Option<String>,
    pub owner_name: Option<String>,
    pub collaborative: bool,
    pub snapshot_id: Option<String>,
    pub image_url: Option<String>,
    pub total: i32,
}

/// A playlist's songs, or that Spotify would not show them to this app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyPlaylistItems {
    pub tracks: Vec<ImportTrack>,
    pub hidden: bool,
}

/// How a 429 waits. Tests make it instant.
pub type Wait = Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;

/// Told how far a paged read has got: rows done, rows in all.
pub type Progress<'a> = Option<&'a (dyn Fn(i32, i32) + Send + Sync)>;

/// The few Spotify Web API calls an import needs, as Spotify answers them in 2026: liked songs
/// (GET /me/tracks), the account's playlists (GET /me/playlists) and a playlist's songs
/// (GET /playlists/{id}/items). A development-mode app gets no ISRCs, and the songs only of
/// playlists the account owns or works on; both are read when they are there. Every page is 50
/// rows, the most Spotify gives, and a 429 waits as long as Spotify asks.
pub struct SpotifyWebApi {
    /// The named "spotify" client (20 s).
    http: reqwest::Client,
    api_base: String,
    token_url: String,
    wait: Mutex<Wait>,
    clock: Mutex<Clock>,
}

impl SpotifyWebApi {
    pub const CLIENT_NAME: &str = "spotify";
    pub const API_BASE: &str = "https://api.spotify.com/v1";
    pub const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
    const PAGE_SIZE: i32 = 50;
    /// A guard against a paging loop, far above any real library.
    pub const MAX_ROWS: i32 = 50_000;
    const MAX_RETRIES: u32 = 4;
    const LONGEST_WAIT: Duration = Duration::from_secs(120);

    pub fn new(http: reqwest::Client) -> Self {
        Self::with_urls(http, Self::API_BASE, Self::TOKEN_URL)
    }

    /// The same, against other addresses: tests point it at a local server.
    pub fn with_urls(http: reqwest::Client, api_base: &str, token_url: &str) -> Self {
        SpotifyWebApi {
            http,
            api_base: api_base.trim_end_matches('/').to_string(),
            token_url: token_url.to_string(),
            wait: Mutex::new(Arc::new(|d| tokio::time::sleep(d).boxed())),
            clock: Mutex::new(Clock::system()),
        }
    }

    /// The HTTP client: a 20 second timeout.
    pub fn client() -> reqwest::Client {
        crate::services::http_client_factory::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("the spotify client builds")
    }

    pub fn set_wait(&self, wait: Wait) {
        *self.wait.lock() = wait;
    }

    pub fn set_clock(&self, clock: Clock) {
        *self.clock.lock() = clock;
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.lock().now()
    }

    // ---- Tokens ------------------------------------------------------------------------------

    pub async fn exchange(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> anyhow::Result<SpotifyTokens> {
        self.token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", verifier),
        ])
        .await
    }

    /// A fresh access token. Spotify may hand back a new refresh token too, which then replaces
    /// the old one.
    pub async fn refresh(&self, client_id: &str, refresh_token: &str) -> anyhow::Result<SpotifyTokens> {
        self.token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .await
    }

    async fn token(&self, form: &[(&str, &str)]) -> anyhow::Result<SpotifyTokens> {
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", dotnet::form_url_encode(k), dotnet::form_url_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let response = self
            .http
            .post(&self.token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            let (error, description) = read_error(&body);
            // invalid_grant: the code was used or is old, or the refresh token was revoked or
            // has reached Spotify's six months. Either way only a new sign-in helps.
            if error.as_deref() == Some("invalid_grant") {
                return Err(SpotifyException::ended(
                    "Spotify ended this sign-in. Connect again.",
                    Some(status.as_u16()),
                )
                .into());
            }
            if error.as_deref() == Some("invalid_client") {
                return Err(SpotifyException::with_status(
                    "Spotify does not know this Client ID. Check it on the Spotify developer dashboard.",
                    status.as_u16(),
                )
                .into());
            }
            let reason = description
                .or(error)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            return Err(SpotifyException::with_status(
                format!("Spotify refused the sign-in: {reason}"),
                status.as_u16(),
            )
            .into());
        }
        let root: Value = serde_json::from_str(&body)?;
        let access = str_of(&root, "access_token")
            .ok_or_else(|| SpotifyException::new("Spotify sent no access token."))?;
        let seconds = int32(root.get("expires_in")).unwrap_or(3600);
        Ok(SpotifyTokens {
            access_token: access,
            refresh_token: str_of(&root, "refresh_token"),
            expires_utc: self.now() + TimeDelta::seconds(i64::from(seconds)),
        })
    }

    // ---- Reads -------------------------------------------------------------------------------

    pub async fn profile(&self, token: &str) -> anyhow::Result<SpotifyProfile> {
        let root = self.get(token, &format!("{}/me", self.api_base)).await?;
        Ok(SpotifyProfile {
            id: str_of(&root, "id").unwrap_or_default(),
            display_name: str_of(&root, "display_name"),
        })
    }

    /// Every liked song, newest first, as Spotify orders them.
    pub async fn liked_songs(&self, token: &str, progress: Progress<'_>) -> anyhow::Result<Vec<ImportTrack>> {
        let mut tracks = Vec::new();
        self.page(
            token,
            &format!("{}/me/tracks", self.api_base),
            &mut |item| {
                if let Some(track) = track_of(item) {
                    tracks.push(track);
                }
            },
            progress,
        )
        .await?;
        Ok(tracks)
    }

    pub async fn playlists(&self, token: &str) -> anyhow::Result<Vec<SpotifyPlaylistInfo>> {
        let mut playlists = Vec::new();
        self.page(
            token,
            &format!("{}/me/playlists", self.api_base),
            &mut |item| {
                let Some(id) = str_of(item, "id").filter(|id| !id.is_empty()) else {
                    return;
                };
                let owner = item.get("owner").filter(|o| o.is_object());
                // items.total since February 2026; tracks.total before, and still sent for now.
                let total = total(item, "items")
                    .or_else(|| total(item, "tracks"))
                    .unwrap_or(0);
                playlists.push(SpotifyPlaylistInfo {
                    id,
                    name: str_of(item, "name").unwrap_or_else(|| "Untitled playlist".to_string()),
                    owner_id: owner.and_then(|o| str_of(o, "id")),
                    owner_name: owner.and_then(|o| str_of(o, "display_name").or_else(|| str_of(o, "id"))),
                    collaborative: item.get("collaborative") == Some(&Value::Bool(true)),
                    snapshot_id: str_of(item, "snapshot_id"),
                    image_url: first_image(item),
                    total,
                });
            },
            None,
        )
        .await?;
        Ok(playlists)
    }

    /// A playlist's songs in order. A development-mode app is shown the songs only of playlists
    /// the account owns or works on: for any other, Spotify answers 403, which reads as Hidden.
    pub async fn playlist_items(
        &self,
        token: &str,
        playlist_id: &str,
        progress: Progress<'_>,
    ) -> anyhow::Result<SpotifyPlaylistItems> {
        let mut tracks = Vec::new();
        let url = format!(
            "{}/playlists/{}/items?additional_types=track",
            self.api_base,
            escape_data_string(playlist_id)
        );
        let read = self
            .page(
                token,
                &url,
                &mut |item| {
                    if let Some(track) = track_of(item) {
                        tracks.push(track);
                    }
                },
                progress,
            )
            .await;
        if let Err(e) = read {
            if e.downcast_ref::<SpotifyException>()
                .is_some_and(|s| matches!(s.status, Some(403 | 404)))
            {
                return Ok(SpotifyPlaylistItems {
                    tracks: Vec::new(),
                    hidden: true,
                });
            }
            return Err(e);
        }
        Ok(SpotifyPlaylistItems {
            tracks,
            hidden: false,
        })
    }

    async fn page(
        &self,
        token: &str,
        url: &str,
        each: &mut (dyn FnMut(&Value) + Send),
        progress: Progress<'_>,
    ) -> anyhow::Result<()> {
        let mut offset = 0;
        while offset < Self::MAX_ROWS {
            let join = if url.contains('?') { '&' } else { '?' };
            let root = self
                .get(
                    token,
                    &format!("{url}{join}limit={}&offset={offset}", Self::PAGE_SIZE),
                )
                .await?;
            let mut count = 0;
            if let Some(items) = root.get("items").and_then(Value::as_array) {
                for item in items {
                    count += 1;
                    each(item);
                }
            }
            offset += count;
            let total = int32(root.get("total")).unwrap_or(offset);
            if let Some(progress) = progress {
                progress(offset, total);
            }
            let more = matches!(root.get("next"), Some(Value::String(_)));
            if count == 0 || !more {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn get(&self, token: &str, url: &str) -> anyhow::Result<Value> {
        let mut attempt: u32 = 0;
        loop {
            let response = self.http.get(url).bearer_auth(token).send().await?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let body = response.text().await?;
            if status.is_success() {
                return Ok(serde_json::from_str(if body.is_empty() { "{}" } else { &body })?);
            }

            if status.as_u16() == 429 {
                // Since July 2026 a 429 that is the app's quota says so; waiting a few seconds
                // will not help.
                if body.contains("QUOTA_EXCEEDED") {
                    return Err(SpotifyException::with_status(
                        "This Spotify app has used up its requests for now. Octo tries again later.",
                        429,
                    )
                    .into());
                }
                let wait = retry_after.unwrap_or(Duration::from_secs(5 * u64::from(attempt + 1)));
                if attempt >= Self::MAX_RETRIES || wait > Self::LONGEST_WAIT {
                    return Err(SpotifyException::with_status(
                        "Spotify asked Octo to slow down. Octo tries again later.",
                        429,
                    )
                    .into());
                }
                info!("Spotify asked to wait {} s before {url}", wait.as_secs_f64());
                let wait_fn = self.wait.lock().clone();
                wait_fn(wait).await;
                attempt += 1;
                continue;
            }
            if status.as_u16() >= 500 && attempt < 2 {
                let wait_fn = self.wait.lock().clone();
                wait_fn(Duration::from_secs(2 * u64::from(attempt + 1))).await;
                attempt += 1;
                continue;
            }
            let (error, description) = read_error(&body);
            let reason = description.or(error);
            let code = status.as_u16();
            return Err(match code {
                401 => SpotifyException::with_status("Spotify did not take the sign-in.", code),
                403 if reason
                    .as_deref()
                    .is_some_and(|r| r.to_lowercase().contains("user")) =>
                {
                    SpotifyException::with_status(
                        "Spotify refused: this account is not on the app's list of users. Add it under User Management on the Spotify developer dashboard.",
                        code,
                    )
                }
                _ => SpotifyException::with_status(
                    match reason {
                        Some(reason) => format!("Spotify answered {code}: {reason}"),
                        None => format!("Spotify answered {code}"),
                    },
                    code,
                ),
            }
            .into());
        }
    }
}

// ---- Shapes ------------------------------------------------------------------------------

/// One saved or playlist row as a song, or None for a podcast episode or an empty slot. The song
/// sits under "item" since February 2026 and under "track" before; both are read.
pub fn track_of(row: &Value) -> Option<ImportTrack> {
    let track = row
        .get("item")
        .filter(|i| i.is_object())
        .or_else(|| row.get("track").filter(|t| t.is_object()))?;
    if str_of(track, "type").is_some_and(|t| t != "track") {
        return None;
    }
    let title = str_of(track, "name").filter(|t| !is_blank(t))?;
    let artists: Vec<String> = track
        .get("artists")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|artist| str_of(artist, "name"))
                .filter(|name| !is_blank(name))
                .collect()
        })
        .unwrap_or_default();
    let album = track
        .get("album")
        .filter(|a| a.is_object())
        .and_then(|a| str_of(a, "name"));
    let seconds = seconds_of(track.get("duration_ms"));
    let isrc = track
        .get("external_ids")
        .filter(|i| i.is_object())
        .and_then(|i| str_of(i, "isrc"))
        .and_then(|i| SongIdentity::normalize_isrc(&i));
    let artist = artists.join(", ");
    let id = str_of(track, "id");
    Some(ImportTrack {
        // A local file on someone's Spotify has no id; its words are all there is.
        key: match id.filter(|id| !id.is_empty()) {
            Some(id) => format!("spotify:{id}"),
            None => ImportKeys::for_words(Some(&artist), Some(&title)),
        },
        title: title.trim().to_string(),
        artist,
        album,
        seconds,
        isrc,
        library_id: None,
    })
}

/// `duration_ms` as whole seconds: a whole number of milliseconds above 0, rounded half to even.
pub(crate) fn seconds_of(value: Option<&Value>) -> Option<i32> {
    let ms = value.and_then(Value::as_i64).filter(|ms| *ms > 0)?;
    Some(dotnet::round(ms as f64 / 1000.0, 0) as i32)
}

/// A whole number that fits in an `i32`, or None.
pub(crate) fn int32(value: Option<&Value>) -> Option<i32> {
    value.and_then(Value::as_i64).and_then(|n| i32::try_from(n).ok())
}

fn total(item: &Value, name: &str) -> Option<i32> {
    item.get(name)
        .filter(|h| h.is_object())
        .and_then(|h| int32(h.get("total")))
}

fn first_image(item: &Value) -> Option<String> {
    item.get("images")
        .and_then(Value::as_array)
        .and_then(|images| images.iter().find_map(|image| str_of(image, "url")))
}

fn read_error(body: &str) -> (Option<String>, Option<String>) {
    let Ok(root) = serde_json::from_str::<Value>(body) else {
        return (None, None);
    };
    match root.get("error") {
        Some(Value::String(error)) => (Some(error.clone()), str_of(&root, "error_description")),
        Some(error @ Value::Object(_)) => (str_of(error, "reason"), str_of(error, "message")),
        _ => (None, None),
    }
}

/// `Str`: a member that is a string, or None.
pub(crate) fn str_of(e: &Value, name: &str) -> Option<String> {
    match e.as_object()?.get(name)? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Keys for songs that have no Spotify id.
pub struct ImportKeys;

impl ImportKeys {
    /// The song by its words, the same for "Drake feat. Rihanna" and "Drake, Rihanna".
    pub fn for_words(artist: Option<&str>, title: Option<&str>) -> String {
        format!(
            "song:{}",
            SongIdentity::match_key(artist.unwrap_or(""), title.unwrap_or(""))
        )
    }

    /// Spotify's key for a track id or a spotify:track: URI, or None.
    pub fn for_spotify(id_or_uri: Option<&str>) -> Option<String> {
        let value = id_or_uri.filter(|v| !is_blank(v))?.trim();
        const PREFIX: &str = "spotify:track:";
        let value = if dotnet::starts_with_ignore_case(value, PREFIX) {
            &value[PREFIX.len()..]
        } else if find_ascii_ignore_case(value, "open.spotify.com/track/").is_some() {
            let at = find_ascii_ignore_case(value, "/track/").unwrap_or(0);
            value[at + 7..].split(['?', '/']).next().unwrap_or("")
        } else {
            value
        };
        let length = dotnet::utf16_len(value);
        (length > 10
            && length < 40
            && value
                .chars()
                .all(|c| dotnet::is_letter_utf16(c) || dotnet::is_digit_utf16(c)))
        .then(|| format!("spotify:{value}"))
    }
}

fn find_ascii_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    (0..=h.len().checked_sub(n.len())?).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

#[cfg(test)]
#[path = "spotify_web_api_tests.rs"]
mod tests;
