use std::sync::Arc;

use axum::http::StatusCode;
use octo_core::settings::SettingsStore;
use octo_subsonic::subsonic_request_parser::escape_data_string;
use reqwest::Method;
use serde_json::Value;
use tracing::warn;

use super::library_action_playlist_worker::{self as playlist_worker, PlaylistRow, PlaylistTrackRow};
use super::quality_upgrade_worker::{self, LibrarySongRow};
use crate::services::subsonic::NavidromeIdentityService;

/// Navidrome's native playlist API, as the admin. Shared by the action-playlist sweep, which
/// removes what people asked for, and the notice worker, which adds what Octo is asking about.
///
/// An admin may edit any user's playlist (Navidrome's checkWritable), which is what lets Octo
/// fill a playlist the user owns while the user keeps it private.
///
/// A caller that stops drops the future, which cancels the HTTP calls.
pub struct NavidromePlaylistApi {
    http: reqwest::Client,
    identity: NavidromeIdentityService,
    settings: Arc<SettingsStore>,
}

/// A request to send with the admin token, built again for the retry.
struct Call {
    method: Method,
    url: String,
    json_body: Option<String>,
    /// The body's content type.
    content_type: &'static str,
}

/// JSON in UTF-8, the content type of every JSON body but the playlist create.
const JSON_UTF8: &str = "application/json; charset=utf-8";

/// The playlist create's content type. Navidrome 0.64 creates a playlist from JSON only when the
/// content type is exactly this (`r.Header.Get("Content-type") == "application/json"`); with
/// `; charset=utf-8` it reads the body as an M3U file, makes an empty playlist named after the
/// time and answers in M3U, which breaks "Keep as playlist".
const JSON_EXACT: &str = "application/json";

impl NavidromePlaylistApi {
    pub fn new(
        http: reqwest::Client,
        identity: NavidromeIdentityService,
        settings: Arc<SettingsStore>,
    ) -> Self {
        NavidromePlaylistApi {
            http,
            identity,
            settings,
        }
    }

    fn base_url(&self) -> String {
        self.settings
            .current()
            .subsonic
            .url
            .clone()
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string()
    }

    pub async fn list_playlists(&self) -> Vec<PlaylistRow> {
        let call = Call::get(format!("{}/api/playlist?_start=0&_end=1000", self.base_url()));
        match self.read_json(call).await {
            Ok(Some(root)) => playlist_worker::parse_playlists(&root),
            Ok(None) => Vec::new(),
            Err(message) => {
                warn!("Could not list playlists: {message}");
                Vec::new()
            }
        }
    }

    pub async fn list_tracks(&self, playlist_id: &str) -> Vec<PlaylistTrackRow> {
        let call = Call::get(format!(
            "{}/api/playlist/{}/tracks?_start=0&_end=500",
            self.base_url(),
            escape_data_string(playlist_id)
        ));
        match self.read_json(call).await {
            Ok(Some(root)) => playlist_worker::parse_tracks(&root),
            Ok(None) => Vec::new(),
            Err(message) => {
                warn!("Could not list tracks in playlist {playlist_id}: {message}");
                Vec::new()
            }
        }
    }

    /// One page of Navidrome's own song list, with how many rows it held, or `None` when it
    /// could not be read. The native list rather than search3: only this one reports the real
    /// library path, and the weekly upgrade remembers files by path (#70).
    pub async fn list_songs(&self, start: i32, count: i32) -> Option<(Vec<LibrarySongRow>, usize)> {
        let call = Call::get(format!(
            "{}/api/song?_start={start}&_end={}&_sort=id&_order=ASC",
            self.base_url(),
            start + count
        ));
        // The client's own timeout is Navidrome not answering, the same as any other failure.
        let parsed = match self.read_json(call).await {
            Ok(Some(root)) => quality_upgrade_worker::parse_songs(&root),
            Ok(None) => return None,
            Err(message) => Err(message),
        };
        match parsed {
            Ok(rows) => rows,
            Err(message) => {
                warn!("Could not list songs: {message}");
                None
            }
        }
    }

    /// Every playlist of every user with what decides whether its tracks may be edited, or
    /// `None` when Navidrome could not be read (never an empty list for a failure).
    pub async fn list_playlist_kinds(&self) -> Option<Vec<PlaylistKind>> {
        let call = Call::get(format!("{}/api/playlist?_start=0&_end=10000", self.base_url()));
        match self.read_json(call).await {
            Ok(Some(root)) => Some(parse_playlist_kinds(&root)),
            Ok(None) => None,
            Err(message) => {
                warn!("Could not list playlists: {message}");
                None
            }
        }
    }

    /// The admin's own marks on one song, as Navidrome's song page holds them, or `None` when
    /// the song could not be read.
    pub async fn song_marks(&self, song_id: &str) -> Option<SongMarks> {
        let call = Call::get(format!(
            "{}/api/song/{}",
            self.base_url(),
            escape_data_string(song_id)
        ));
        match self.read_json(call).await {
            Ok(Some(root)) => Some(parse_song_marks(&root)),
            Ok(None) => None,
            Err(message) => {
                warn!("Could not read the marks on song {song_id}: {message}");
                None
            }
        }
    }

    /// Moves the track at `position` so it sits at `insert_before`, every track between shifting
    /// by one, as Navidrome's playlist editor drags a row. `Err` when the call itself failed.
    pub async fn move_track(
        &self,
        playlist_id: &str,
        position: usize,
        insert_before: usize,
    ) -> Result<bool, String> {
        let call = Call {
            method: Method::PUT,
            url: format!(
                "{}/api/playlist/{}/tracks/{position}",
                self.base_url(),
                escape_data_string(playlist_id)
            ),
            json_body: Some(octo_core::json::to_string(
                &serde_json::json!({ "insert_before": insert_before.to_string() }),
            )),
            content_type: JSON_UTF8,
        };
        let status = self.try_send(call).await?.map(|r| r.status());
        if status.is_some_and(|s| s.is_success()) {
            return Ok(true);
        }
        warn!(
            "Could not move track {position} of playlist {playlist_id}: HTTP {}",
            status_text(status)
        );
        Ok(false)
    }

    /// Remove tracks by POSITION. Positions are reassigned on every change, so callers re-read
    /// the list immediately before and send every position in one call. `Err` when the call
    /// itself failed.
    pub async fn remove_positions(&self, playlist_id: &str, positions: &[String]) -> Result<bool, String> {
        if positions.is_empty() {
            return Ok(true);
        }
        let query = positions
            .iter()
            .map(|p| format!("id={}", escape_data_string(p)))
            .collect::<Vec<_>>()
            .join("&");
        let call = Call {
            method: Method::DELETE,
            url: format!(
                "{}/api/playlist/{}/tracks?{query}",
                self.base_url(),
                escape_data_string(playlist_id)
            ),
            json_body: None,
            content_type: JSON_UTF8,
        };
        let status = self.try_send(call).await?.map(|r| r.status());
        if status.is_some_and(|s| s.is_success()) {
            return Ok(true);
        }
        warn!(
            "Could not remove {} track(s) from playlist {playlist_id}: HTTP {}",
            positions.len(),
            status_text(status)
        );
        Ok(false)
    }

    /// Append tracks. Navidrome answers {"added": n}. `Err` when the call itself failed.
    pub async fn add_tracks(&self, playlist_id: &str, media_file_ids: &[String]) -> Result<bool, String> {
        if media_file_ids.is_empty() {
            return Ok(true);
        }
        let call = Call {
            method: Method::POST,
            url: format!(
                "{}/api/playlist/{}/tracks",
                self.base_url(),
                escape_data_string(playlist_id)
            ),
            json_body: Some(octo_core::json::to_string(
                &serde_json::json!({ "ids": media_file_ids }),
            )),
            content_type: JSON_UTF8,
        };
        let status = self.try_send(call).await?.map(|r| r.status());
        if status.is_some_and(|s| s.is_success()) {
            return Ok(true);
        }
        warn!(
            "Could not add {} track(s) to playlist {playlist_id}: HTTP {}",
            media_file_ids.len(),
            status_text(status)
        );
        Ok(false)
    }

    /// A new private playlist, made as the admin, so its owner is the admin until
    /// [`NavidromePlaylistApi::give_to`] hands it on. Its id, or `None`; `Err` when the call
    /// itself failed.
    ///
    /// Sent as `application/json` exactly, the one content type Navidrome creates a playlist
    /// from (see [`JSON_EXACT`]). An answer that is not the JSON create's `{"id": ...}` is no
    /// playlist Octo knows, never a crash.
    pub async fn create_playlist(&self, name: &str, comment: Option<&str>) -> Result<Option<String>, String> {
        let call = Call {
            method: Method::POST,
            url: format!("{}/api/playlist", self.base_url()),
            json_body: Some(octo_core::json::to_string(&serde_json::json!({
                "name": name,
                "comment": comment.unwrap_or(""),
                "public": false,
            }))),
            content_type: JSON_EXACT,
        };
        let Some(response) = self.try_send(call).await? else {
            warn!("Could not create playlist '{name}': HTTP ");
            return Ok(None);
        };
        let status = response.status();
        if !status.is_success() {
            warn!("Could not create playlist '{name}': HTTP {}", status.as_u16());
            return Ok(None);
        }
        let body = response.bytes().await.map_err(|e| e.to_string())?;
        let id = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|root| root.get("id").and_then(Value::as_str).map(str::to_string));
        if id.is_none() {
            warn!("Navidrome's answer to creating playlist '{name}' named no playlist");
        }
        Ok(id)
    }

    /// Makes a Navidrome user the playlist's owner, the way the admin changes it in Navidrome's
    /// own playlist editor. True when it is theirs afterwards, already or now.
    pub async fn give_to(&self, playlist_id: &str, username: &str) -> bool {
        match self.try_give_to(playlist_id, username).await {
            Ok(given) => given,
            Err(message) => {
                warn!("Could not give playlist {playlist_id} to {username}: {message}");
                false
            }
        }
    }

    async fn try_give_to(&self, playlist_id: &str, username: &str) -> Result<bool, String> {
        let users = self
            .read_json(Call::get(format!(
                "{}/api/user?_start=0&_end=1000",
                self.base_url()
            )))
            .await?;
        let Some(users) = users else {
            return Ok(false);
        };
        let owner_id = users.as_array().and_then(|users| {
            users
                .iter()
                .filter(|user| {
                    user.get("userName")
                        .and_then(Value::as_str)
                        .is_some_and(|name| octo_core::common::dotnet::eq_ignore_case(name, username))
                })
                .map(|user| user.get("id").and_then(Value::as_str).map(str::to_string))
                .next()
                .flatten()
        });
        let Some(owner_id) = owner_id else {
            return Ok(false);
        };

        let url = format!(
            "{}/api/playlist/{}",
            self.base_url(),
            escape_data_string(playlist_id)
        );
        let Some(mut playlist) = self.read_json(Call::get(url.clone())).await? else {
            return Ok(false);
        };
        let Some(fields) = playlist.as_object_mut() else {
            return Ok(false);
        };
        if fields.get("ownerId").and_then(Value::as_str) == Some(owner_id.as_str()) {
            return Ok(true);
        }
        // The whole playlist goes back with only its owner changed, so nothing else is reset.
        fields.insert("ownerId".into(), Value::String(owner_id));
        let call = Call {
            method: Method::PUT,
            url,
            json_body: Some(serde_json::to_string(&playlist).map_err(|e| e.to_string())?),
            content_type: JSON_UTF8,
        };
        Ok(self
            .try_send(call)
            .await?
            .is_some_and(|r| r.status().is_success()))
    }

    /// Whether the playlist is still there: `None` when Navidrome could not say.
    pub async fn playlist_exists(&self, playlist_id: &str) -> Result<Option<bool>, String> {
        let call = Call::get(format!(
            "{}/api/playlist/{}",
            self.base_url(),
            escape_data_string(playlist_id)
        ));
        let Some(response) = self.try_send(call).await? else {
            return Ok(None);
        };
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(Some(false));
        }
        Ok(response.status().is_success().then_some(true))
    }

    /// Deletes a playlist Octo made, so a keep that failed half way leaves nothing empty behind
    /// ("Keep as playlist").
    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<bool, String> {
        let call = Call {
            method: Method::DELETE,
            url: format!(
                "{}/api/playlist/{}",
                self.base_url(),
                escape_data_string(playlist_id)
            ),
            json_body: None,
            content_type: JSON_UTF8,
        };
        Ok(self
            .try_send(call)
            .await?
            .is_some_and(|r| r.status().is_success()))
    }

    /// Every track of a playlist in order, however long, or `None` when it could not be read
    /// (never an empty list for a failure, which a caller would take for an empty playlist).
    pub async fn read_all_tracks(&self, playlist_id: &str) -> Result<Option<Vec<PlaylistTrackRow>>, String> {
        const PAGE: usize = 500;
        let mut rows = Vec::new();
        let mut start = 0;
        loop {
            let call = Call::get(format!(
                "{}/api/playlist/{}/tracks?_start={start}&_end={}",
                self.base_url(),
                escape_data_string(playlist_id),
                start + PAGE
            ));
            let Some(root) = self.read_json(call).await? else {
                return Ok(None);
            };
            rows.extend(playlist_worker::parse_tracks(&root));
            if root.as_array().is_none_or(|a| a.len() < PAGE) {
                return Ok(Some(rows));
            }
            start += PAGE;
        }
    }

    /// The JSON of a successful answer: `Ok(None)` when there is no answer or it is not a
    /// success, `Err` when the call or the parse failed.
    async fn read_json(&self, call: Call) -> Result<Option<Value>, String> {
        let Some(response) = self.try_send(call).await? else {
            return Ok(None);
        };
        if !response.status().is_success() {
            return Ok(None);
        }
        let body = response.bytes().await.map_err(|e| e.to_string())?;
        serde_json::from_slice(&body).map(Some).map_err(|e| e.to_string())
    }

    /// Send with the admin token, and on a 401 log in again and resend ONCE. `None` when there
    /// is no admin credential at all, or the fresh token is refused too.
    async fn try_send(&self, call: Call) -> Result<Option<reqwest::Response>, String> {
        if self
            .settings
            .current()
            .subsonic
            .url
            .as_deref()
            .is_none_or(str::is_empty)
        {
            return Ok(None);
        }
        let Some(jwt) = self.identity.ensure_admin_jwt().await.filter(|j| !j.is_empty()) else {
            return Ok(None);
        };

        let response = self
            .request(&call, &jwt)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(Some(response));
        }

        self.identity.invalidate_admin_jwt(&jwt);
        let fresh = self.identity.ensure_admin_jwt().await;
        let Some(fresh) = fresh.filter(|f| !f.is_empty() && *f != jwt) else {
            warn!("Navidrome refused Octo's admin token and no fresh one could be had");
            return Ok(None);
        };
        self.request(&call, &fresh)
            .send()
            .await
            .map(Some)
            .map_err(|e| e.to_string())
    }

    fn request(&self, call: &Call, jwt: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .request(call.method.clone(), &call.url)
            .header("X-Nd-Authorization", format!("Bearer {jwt}"));
        if let Some(body) = &call.json_body {
            request = request
                .header("Content-Type", call.content_type)
                .body(body.clone());
        }
        request
    }
}

impl Call {
    fn get(url: String) -> Call {
        Call {
            method: Method::GET,
            url,
            json_body: None,
            content_type: JSON_UTF8,
        }
    }
}

/// One playlist and whether its tracks are people's own: a smart playlist's come from its rules
/// and a synced one's from its file, so Navidrome refuses to edit either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistKind {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub smart: bool,
    pub synced: bool,
}

impl PlaylistKind {
    pub fn editable(&self) -> bool {
        !self.smart && !self.synced
    }
}

/// One listener's marks on one song: a favorite, a rating from 1 to 5 (0 for none) and plays.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SongMarks {
    pub starred: bool,
    pub rating: i32,
    pub plays: i64,
}

pub fn parse_playlist_kinds(root: &Value) -> Vec<PlaylistKind> {
    let Value::Array(items) = root else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())?;
            let rules = item.get("rules").filter(|rules| match rules {
                Value::Null => false,
                Value::Object(fields) => !fields.is_empty(),
                _ => true,
            });
            Some(PlaylistKind {
                id: id.to_string(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                owner: item
                    .get("ownerName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                smart: rules.is_some(),
                synced: item.get("sync").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

pub fn parse_song_marks(song: &Value) -> SongMarks {
    let number = |name: &str| -> i64 {
        match song.get(name) {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    };
    SongMarks {
        starred: song.get("starred").and_then(Value::as_bool).unwrap_or(false),
        rating: i32::try_from(number("rating")).unwrap_or(0),
        plays: number("playCount"),
    }
}

/// `(int?)response?.StatusCode` as a log value: empty for no answer.
fn status_text(status: Option<StatusCode>) -> String {
    status.map(|s| s.as_u16().to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use octo_core::settings::{AppSettings, SubsonicSettings};
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn api(url: &str) -> NavidromePlaylistApi {
        let settings = Arc::new(SettingsStore::for_tests(AppSettings {
            subsonic: SubsonicSettings {
                url: Some(url.to_string()),
                admin_username: Some("admin".into()),
                admin_password: Some("pw".into()),
                ..Default::default()
            },
            ..Default::default()
        }));
        let http = crate::services::http_client_factory::default_client();
        let identity = NavidromeIdentityService::new(Arc::clone(&settings), http.clone());
        NavidromePlaylistApi::new(http, identity, settings)
    }

    async fn login(server: &MockServer, tokens: &[&str]) {
        for token in tokens {
            Mock::given(method("POST"))
                .and(path("/auth/login"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "token": token, "isAdmin": true, "username": "admin",
                })))
                .up_to_n_times(1)
                .mount(server)
                .await;
        }
    }

    #[tokio::test]
    async fn a_refused_token_is_renewed_once_and_the_call_sent_again() {
        let server = MockServer::start().await;
        login(&server, &["old", "new"]).await;
        Mock::given(method("GET"))
            .and(path("/api/playlist"))
            .and(header("X-Nd-Authorization", "Bearer old"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/playlist"))
            .and(header("X-Nd-Authorization", "Bearer new"))
            .and(query_param("_end", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "p1", "name": "Keep", "ownerName": "alice" },
                { "id": "p2", "name": "" },
                { "id": 3, "name": "Numbered" },
            ])))
            .mount(&server)
            .await;

        let rows = api(&server.uri()).list_playlists().await;

        assert_eq!(
            rows,
            [
                PlaylistRow {
                    id: "p1".into(),
                    name: "Keep".into(),
                    owner: "alice".into()
                },
                PlaylistRow {
                    id: "3".into(),
                    name: "Numbered".into(),
                    owner: String::new()
                },
            ]
        );
    }

    #[tokio::test]
    async fn tracks_are_added_as_json_and_removed_by_position() {
        let server = MockServer::builder().start().await;
        login(&server, &["jwt"]).await;
        Mock::given(method("POST"))
            .and(path("/api/playlist/pl%201/tracks"))
            .and(body_json(serde_json::json!({ "ids": ["a", "b"] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "added": 2 })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/playlist/pl%201/tracks"))
            .and(query_param("id", "1"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let api = api(&server.uri());

        assert_eq!(api.add_tracks("pl 1", &["a".into(), "b".into()]).await, Ok(true));
        assert_eq!(
            api.remove_positions("pl 1", &["1".into(), "2".into()]).await,
            Ok(true)
        );
        assert_eq!(api.add_tracks("pl 1", &[]).await, Ok(true));
        let deleted = server
            .received_requests()
            .await
            .expect("recording")
            .into_iter()
            .find(|r| r.method == wiremock::http::Method::DELETE)
            .expect("a delete");
        assert_eq!(deleted.url.query(), Some("id=1&id=2"));
    }

    #[tokio::test]
    async fn songs_are_listed_a_page_at_a_time() {
        let server = MockServer::start().await;
        login(&server, &["jwt"]).await;
        Mock::given(method("GET"))
            .and(path("/api/song"))
            .and(query_param("_start", "100"))
            .and(query_param("_end", "150"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "s1", "path": "A/B/1.flac", "size": 10, "bitRate": 900, "duration": 2.5, "suffix": "flac" },
                { "id": "s2", "path": "A/B/2.flac", "missing": true },
            ])))
            .mount(&server)
            .await;

        let (rows, count) = api(&server.uri()).list_songs(100, 50).await.expect("a page");

        assert_eq!(count, 2);
        assert_eq!(rows.len(), 1);
        // Rounded half to even.
        assert_eq!(
            (rows[0].size, rows[0].bit_rate, rows[0].duration),
            (10, 900, Some(2))
        );
    }

    #[tokio::test]
    async fn no_admin_identity_means_no_call() {
        let api = NavidromePlaylistApi::new(
            crate::services::http_client_factory::default_client(),
            NavidromeIdentityService::new(
                Arc::new(SettingsStore::for_tests(AppSettings::default())),
                crate::services::http_client_factory::default_client(),
            ),
            Arc::new(SettingsStore::for_tests(AppSettings {
                subsonic: SubsonicSettings {
                    url: Some("http://127.0.0.1:1".into()),
                    ..Default::default()
                },
                ..Default::default()
            })),
        );
        assert!(api.list_playlists().await.is_empty());
        assert_eq!(api.add_tracks("p", &["a".into()]).await, Ok(false));
    }

    /// The "Keep as playlist" fix: Navidrome 0.64 creates a playlist from JSON only when the
    /// content type is exactly `application/json`; with `; charset=utf-8` it imports the body as
    /// an M3U file instead. Its answer is `{"id": ...}`; anything else names no playlist.
    #[tokio::test]
    async fn a_playlist_is_created_as_json_navidrome_reads_as_json() {
        let server = MockServer::start().await;
        login(&server, &["jwt"]).await;
        Mock::given(method("POST"))
            .and(path("/api/playlist"))
            .and(header("Content-Type", "application/json"))
            .and(body_json(
                serde_json::json!({ "name": "Gym Mix", "comment": "c", "public": false }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": "pl-9" })))
            .mount(&server)
            .await;
        let api = api(&server.uri());
        assert_eq!(
            api.create_playlist("Gym Mix", Some("c")).await,
            Ok(Some("pl-9".to_string()))
        );

        let m3u = MockServer::start().await;
        login(&m3u, &["jwt"]).await;
        Mock::given(method("POST"))
            .and(path("/api/playlist"))
            .respond_with(
                ResponseTemplate::new(201).set_body_string("#EXTM3U\n#PLAYLIST:2026-10-08T00:30:00Z\n"),
            )
            .mount(&m3u)
            .await;
        assert_eq!(
            api_at(&m3u.uri()).create_playlist("Gym Mix", None).await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn a_playlist_is_given_deleted_and_read_whole() {
        let server = MockServer::start().await;
        login(&server, &["jwt"]).await;
        Mock::given(method("GET"))
            .and(path("/api/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "u1", "userName": "admin" },
                { "id": "u2", "userName": "Alice" },
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/playlist/p1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "p1", "name": "Gym", "ownerId": "u1", "comment": "c",
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/playlist/p1"))
            .and(body_json(
                serde_json::json!({ "id": "p1", "name": "Gym", "ownerId": "u2", "comment": "c" }),
            ))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/playlist/p1"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/playlist/p1/tracks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "1", "mediaFileId": "a" },
                { "id": "2", "mediaFileId": "b" },
            ])))
            .mount(&server)
            .await;
        let api = api(&server.uri());
        assert!(api.give_to("p1", "alice").await);
        assert_eq!(api.playlist_exists("p1").await, Ok(Some(true)));
        assert_eq!(api.playlist_exists("gone").await, Ok(Some(false)));
        let rows = api.read_all_tracks("p1").await.expect("read").expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].media_file_id, "b");
        assert_eq!(api.delete_playlist("p1").await, Ok(true));
    }

    fn api_at(url: &str) -> NavidromePlaylistApi {
        api(url)
    }
}
