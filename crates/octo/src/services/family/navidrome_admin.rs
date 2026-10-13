//! Octo's admin calls to Navidrome's native API for the family: members' accounts, their
//! libraries, their players, and a scan of one library. Every call signs in as Octo's standing
//! admin identity; when Navidrome refuses that token (it expired), Octo signs in again once.

use std::sync::Arc;

use octo_core::common::dotnet::escape_data_string;
use octo_core::settings::SettingsStore;
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::services::subsonic::NavidromeIdentityService;

/// Why an admin call did not happen.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    /// No Navidrome address is set.
    #[error("Navidrome's address is not set")]
    NoServer,
    /// Octo has no admin sign-in (none configured, none captured).
    #[error("Sign in to the dashboard as a Navidrome admin first")]
    NoIdentity,
    /// Navidrome answered something other than success.
    #[error("Navidrome answered {status}: {body}")]
    Status { status: u16, body: String },
    /// The call itself failed, or the answer was not what Navidrome sends.
    #[error("{0}")]
    Http(String),
}

impl AdminError {
    /// Navidrome's own words, for messages that depend on them.
    pub fn body(&self) -> Option<&str> {
        match self {
            AdminError::Status { body, .. } => Some(body),
            _ => None,
        }
    }
}

/// A Navidrome user, kept whole: Navidrome replaces a user with exactly what a PUT sends, so a
/// change is always the full object with one field changed.
#[derive(Debug, Clone, PartialEq)]
pub struct NdUser {
    pub fields: Map<String, Value>,
}

impl NdUser {
    fn text(&self, name: &str) -> &str {
        self.fields.get(name).and_then(Value::as_str).unwrap_or("")
    }

    pub fn id(&self) -> &str {
        self.text("id")
    }

    pub fn user_name(&self) -> &str {
        self.text("userName")
    }

    pub fn name(&self) -> &str {
        self.text("name")
    }

    pub fn is_admin(&self) -> bool {
        self.fields
            .get("isAdmin")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// A Navidrome library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdLibrary {
    pub id: i64,
    pub name: String,
    pub path: String,
}

/// A Navidrome player (one app on one device, as Navidrome tells them apart).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdPlayer {
    pub id: String,
    pub name: String,
    pub client: String,
    pub user_id: String,
    pub max_bit_rate: i64,
}

/// The native API as Octo's admin identity. A cheap handle.
#[derive(Clone)]
pub struct NavidromeAdmin {
    identity: NavidromeIdentityService,
    http: reqwest::Client,
    settings: Arc<SettingsStore>,
}

fn as_id(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

impl NavidromeAdmin {
    pub fn new(
        identity: NavidromeIdentityService,
        http: reqwest::Client,
        settings: Arc<SettingsStore>,
    ) -> Self {
        NavidromeAdmin {
            identity,
            http,
            settings,
        }
    }

    fn base(&self) -> Result<String, AdminError> {
        self.settings
            .current()
            .subsonic
            .url
            .as_deref()
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .filter(|url| !url.is_empty())
            .ok_or(AdminError::NoServer)
    }

    /// One native call with the admin token; a 401 signs in again and tries once more.
    async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value, AdminError> {
        let url = format!("{}{path}", self.base()?);
        for attempt in 0..2 {
            let jwt = self
                .identity
                .ensure_admin_jwt()
                .await
                .filter(|jwt| !jwt.is_empty())
                .ok_or(AdminError::NoIdentity)?;
            let mut request = self
                .http
                .request(method.clone(), &url)
                .header("X-Nd-Authorization", format!("Bearer {jwt}"));
            if let Some(body) = body {
                request = request
                    .header("Content-Type", "application/json")
                    .body(body.to_string());
            }
            let response = request
                .send()
                .await
                .map_err(|e| AdminError::Http(e.to_string()))?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.identity.invalidate_admin_jwt(&jwt);
                continue;
            }
            let text = response
                .text()
                .await
                .map_err(|e| AdminError::Http(e.to_string()))?;
            if !status.is_success() {
                return Err(AdminError::Status {
                    status: status.as_u16(),
                    body: text,
                });
            }
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).map_err(|e| AdminError::Http(e.to_string()));
        }
        Err(AdminError::NoIdentity)
    }

    fn users_from(value: Value) -> Vec<NdUser> {
        match value {
            Value::Array(items) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::Object(fields) => Some(NdUser { fields }),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Every Navidrome user.
    pub async fn list_users(&self) -> Result<Vec<NdUser>, AdminError> {
        Ok(Self::users_from(self.call(Method::GET, "/api/user", None).await?))
    }

    /// One user, whole.
    pub async fn get_user(&self, id: &str) -> Result<NdUser, AdminError> {
        match self
            .call(
                Method::GET,
                &format!("/api/user/{}", escape_data_string(id)),
                None,
            )
            .await?
        {
            Value::Object(fields) => Ok(NdUser { fields }),
            other => Err(AdminError::Http(format!("not a user: {other}"))),
        }
    }

    /// Makes a user who is not an admin; answers their id.
    pub async fn create_user(
        &self,
        user_name: &str,
        name: &str,
        password: &str,
    ) -> Result<String, AdminError> {
        let body = json!({
            "userName": user_name,
            "name": name,
            "email": "",
            "isAdmin": false,
            "password": password,
        });
        let created = self.call(Method::POST, "/api/user", Some(&body)).await?;
        created
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .ok_or_else(|| AdminError::Http(format!("no id in {created}")))
    }

    /// Sets a user's password. Navidrome replaces the whole user on a PUT, so the user is read
    /// first and sent back whole with the new password.
    pub async fn set_password(&self, id: &str, password: &str) -> Result<(), AdminError> {
        let mut user = self.get_user(id).await?;
        user.fields
            .insert("password".into(), Value::String(password.to_string()));
        self.call(
            Method::PUT,
            &format!("/api/user/{}", escape_data_string(id)),
            Some(&Value::Object(user.fields)),
        )
        .await?;
        Ok(())
    }

    pub async fn delete_user(&self, id: &str) -> Result<(), AdminError> {
        self.call(
            Method::DELETE,
            &format!("/api/user/{}", escape_data_string(id)),
            None,
        )
        .await?;
        Ok(())
    }

    /// Every library.
    pub async fn list_libraries(&self) -> Result<Vec<NdLibrary>, AdminError> {
        let Value::Array(items) = self.call(Method::GET, "/api/library", None).await? else {
            return Ok(Vec::new());
        };
        Ok(items
            .iter()
            .filter_map(|item| {
                Some(NdLibrary {
                    id: as_id(item.get("id"))?,
                    name: item.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    path: item.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
                })
            })
            .collect())
    }

    /// Makes a library over `path` (a folder Navidrome can already see) that new users do not
    /// get; answers its id.
    pub async fn create_library(&self, name: &str, path: &str) -> Result<i64, AdminError> {
        let body = json!({ "name": name, "path": path, "defaultNewUsers": false });
        let created = self.call(Method::POST, "/api/library", Some(&body)).await?;
        as_id(created.get("id")).ok_or_else(|| AdminError::Http(format!("no id in {created}")))
    }

    /// Removes a library from Navidrome (its files stay on disk).
    pub async fn delete_library(&self, id: i64) -> Result<(), AdminError> {
        self.call(Method::DELETE, &format!("/api/library/{id}"), None)
            .await?;
        Ok(())
    }

    /// Sets which libraries a user sees (Navidrome wants at least one).
    pub async fn set_user_libraries(&self, id: &str, library_ids: &[i64]) -> Result<(), AdminError> {
        let body = json!({ "libraryIds": library_ids });
        self.call(
            Method::PUT,
            &format!("/api/user/{}/library", escape_data_string(id)),
            Some(&body),
        )
        .await?;
        Ok(())
    }

    /// The players Navidrome has for one user.
    pub async fn list_players(&self, user_id: &str) -> Result<Vec<NdPlayer>, AdminError> {
        let Value::Array(items) = self.call(Method::GET, "/api/player", None).await? else {
            return Ok(Vec::new());
        };
        let text =
            |item: &Value, name: &str| item.get(name).and_then(Value::as_str).unwrap_or("").to_string();
        Ok(items
            .iter()
            .filter(|item| item.get("userId").and_then(Value::as_str) == Some(user_id))
            .map(|item| NdPlayer {
                id: text(item, "id"),
                name: text(item, "name"),
                client: text(item, "client"),
                user_id: text(item, "userId"),
                max_bit_rate: item.get("maxBitRate").and_then(Value::as_i64).unwrap_or(0),
            })
            .collect())
    }

    /// Sets one player's highest bitrate. Navidrome changes only the fields a player PUT
    /// names, so nothing else about the player changes.
    pub async fn set_player_max_bit_rate(&self, player_id: &str, kbps: u32) -> Result<(), AdminError> {
        let body = json!({ "maxBitRate": kbps });
        self.call(
            Method::PUT,
            &format!("/api/player/{}", escape_data_string(player_id)),
            Some(&body),
        )
        .await?;
        Ok(())
    }

    /// Starts a scan of one library (`startScan` with `target=<id>:`), signed with the admin's
    /// Subsonic token.
    pub async fn scan_library(&self, library_id: i64) -> Result<(), AdminError> {
        let base = self.base()?;
        // The Subsonic token comes from the admin sign-in, so make sure there has been one.
        self.identity.ensure_admin_jwt().await;
        let (user, token, salt) = self.identity.get_scan_auth().ok_or(AdminError::NoIdentity)?;
        let url = format!(
            "{base}/rest/startScan?f=json&c=octo&v=1.16.1&u={}&t={}&s={}&target={}",
            escape_data_string(&user),
            escape_data_string(&token),
            escape_data_string(&salt),
            escape_data_string(&format!("{library_id}:"))
        );
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AdminError::Http(e.to_string()))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() || text.contains("\"status\":\"failed\"") {
            return Err(AdminError::Status {
                status: status.as_u16(),
                body: text,
            });
        }
        Ok(())
    }

    /// One song Navidrome keeps as missing (`/api/missing/{id}`), with the plays, favorite,
    /// rating and bookmark of the user Octo signs in as. None when Navidrome has no missing song
    /// by that id: its file came back, or the row is gone already.
    pub async fn missing_song(&self, id: &str) -> Result<Option<Value>, AdminError> {
        match self
            .call(
                Method::GET,
                &format!("/api/missing/{}", escape_data_string(id)),
                None,
            )
            .await
        {
            Ok(song @ Value::Object(_)) => Ok(Some(song)),
            Ok(_) | Err(AdminError::Status { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Removes these missing songs from Navidrome (`DELETE /api/missing?id=..`), as its Missing
    /// Files page does. Navidrome removes only songs still marked missing. No ids sends nothing:
    /// Navidrome reads a call without ids as every missing song.
    pub async fn delete_missing(&self, ids: &[String]) -> Result<(), AdminError> {
        if ids.is_empty() {
            return Ok(());
        }
        let query = ids
            .iter()
            .map(|id| format!("id={}", escape_data_string(id)))
            .collect::<Vec<_>>()
            .join("&");
        self.call(Method::DELETE, &format!("/api/missing?{query}"), None)
            .await?;
        Ok(())
    }

    /// Navidrome's settings as its admin About page reads them (`/api/config/config`). Navidrome
    /// offers the page only while `DevUIShowConfig` is on; otherwise the call is refused.
    pub async fn server_config(&self) -> Result<Value, AdminError> {
        self.call(Method::GET, "/api/config/config", None).await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use octo_core::settings::{AppSettings, SubsonicSettings};
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A Navidrome that signs `admin` in with `jwt-1`, then `jwt-2`, and so on.
    pub(crate) async fn navidrome() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "jwt-1", "isAdmin": true, "username": "admin",
                "subsonicToken": "tok", "subsonicSalt": "salt"
            })))
            .mount(&server)
            .await;
        server
    }

    pub(crate) fn admin_for(url: &str) -> (NavidromeAdmin, NavidromeIdentityService, Arc<SettingsStore>) {
        let settings = Arc::new(SettingsStore::for_tests(AppSettings {
            subsonic: SubsonicSettings {
                url: Some(url.to_string()),
                admin_username: Some("admin".into()),
                admin_password: Some("pw".into()),
                auto_detect_download_path: false,
                ..Default::default()
            },
            ..Default::default()
        }));
        let http = crate::services::http_client_factory::default_client();
        let identity = NavidromeIdentityService::new(settings.clone(), http.clone());
        (
            NavidromeAdmin::new(identity.clone(), http, settings.clone()),
            identity,
            settings,
        )
    }

    #[tokio::test]
    async fn create_user_posts_the_navidrome_fields() {
        let server = navidrome().await;
        Mock::given(method("POST"))
            .and(path("/api/user"))
            .and(header("X-Nd-Authorization", "Bearer jwt-1"))
            .and(body_json(json!({
                "userName": "alex", "name": "Alex", "email": "", "isAdmin": false, "password": "hidden"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "u-alex"})))
            .expect(1)
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        assert_eq!(
            admin.create_user("alex", "Alex", "hidden").await.expect("made"),
            "u-alex"
        );
    }

    #[tokio::test]
    async fn password_change_puts_the_whole_user() {
        let server = navidrome().await;
        let user = json!({
            "id": "u-alex", "userName": "alex", "name": "Alex", "email": "", "isAdmin": false,
            "lastLoginAt": null, "createdAt": "2026-10-20T18:00:00Z", "scrobbleFilter": ""
        });
        Mock::given(method("GET"))
            .and(path("/api/user/u-alex"))
            .respond_with(ResponseTemplate::new(200).set_body_json(user.clone()))
            .mount(&server)
            .await;
        let mut sent = user.clone();
        sent["password"] = json!("new-hidden");
        Mock::given(method("PUT"))
            .and(path("/api/user/u-alex"))
            .and(body_json(sent))
            .respond_with(ResponseTemplate::new(200).set_body_json(user))
            .expect(1)
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        admin.set_password("u-alex", "new-hidden").await.expect("set");
        let puts: Vec<Value> = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|r| r.method.as_str() == "PUT")
            .map(|r| serde_json::from_slice(&r.body).expect("json"))
            .collect();
        assert_eq!(puts[0]["isAdmin"], json!(false));
        assert_eq!(puts[0]["name"], json!("Alex"));
    }

    #[tokio::test]
    async fn expired_admin_token_logs_in_again_once() {
        let server = MockServer::start().await;
        // The first sign-in's token is refused; the second sign-in's works.
        Mock::given(method("POST"))
            .and(path("/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "stale", "isAdmin": true, "username": "admin",
                "subsonicToken": "tok", "subsonicSalt": "salt"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "fresh", "isAdmin": true, "username": "admin",
                "subsonicToken": "tok", "subsonicSalt": "salt"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/user"))
            .and(header("X-Nd-Authorization", "Bearer stale"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/user"))
            .and(header("X-Nd-Authorization", "Bearer fresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": "u1", "userName": "admin", "name": "Admin", "isAdmin": true}
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        let users = admin.list_users().await.expect("listed");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].user_name(), "admin");
        assert!(users[0].is_admin());
    }

    #[tokio::test]
    async fn a_second_refusal_is_an_error_not_a_loop() {
        let server = navidrome().await;
        Mock::given(method("GET"))
            .and(path("/api/user"))
            .respond_with(ResponseTemplate::new(401))
            .expect(2)
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        assert!(matches!(
            admin.list_users().await,
            Err(AdminError::Status { status: 401, .. })
        ));
    }

    #[tokio::test]
    async fn without_an_admin_sign_in_nothing_is_sent() {
        let server = MockServer::start().await;
        let settings = Arc::new(SettingsStore::for_tests(AppSettings {
            subsonic: SubsonicSettings {
                url: Some(server.uri()),
                auto_detect_download_path: false,
                ..Default::default()
            },
            ..Default::default()
        }));
        let http = crate::services::http_client_factory::default_client();
        let admin = NavidromeAdmin::new(
            NavidromeIdentityService::new(settings.clone(), http.clone()),
            http,
            settings,
        );
        let error = admin.list_libraries().await.expect_err("no identity");
        assert_eq!(
            error.to_string(),
            "Sign in to the dashboard as a Navidrome admin first"
        );
        assert!(server.received_requests().await.expect("recorded").is_empty());
    }

    #[tokio::test]
    async fn library_ids_body_shape() {
        let server = navidrome().await;
        Mock::given(method("PUT"))
            .and(path("/api/user/u-alex/library"))
            .and(body_json(json!({"libraryIds": [1, 3]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/library"))
            .and(body_json(
                json!({"name": "Alex's library", "path": "/members/alex", "defaultNewUsers": false}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 3})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/library"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 1, "name": "Music", "path": "/music"},
                {"id": 3, "name": "Alex's library", "path": "/members/alex"}
            ])))
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        assert_eq!(
            admin
                .create_library("Alex's library", "/members/alex")
                .await
                .expect("made"),
            3
        );
        admin.set_user_libraries("u-alex", &[1, 3]).await.expect("set");
        let libraries = admin.list_libraries().await.expect("listed");
        assert_eq!(
            libraries[1],
            NdLibrary {
                id: 3,
                name: "Alex's library".into(),
                path: "/members/alex".into()
            }
        );
    }

    #[tokio::test]
    async fn navidromes_refusal_keeps_its_words() {
        let server = navidrome().await;
        Mock::given(method("POST"))
            .and(path("/api/library"))
            .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"path must exist"}"#))
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        let error = admin
            .create_library("x", "/members/x")
            .await
            .expect_err("refused");
        assert!(error.body().expect("a body").contains("must exist"));
    }

    #[tokio::test]
    async fn players_of_one_user_and_a_partial_bitrate_change() {
        let server = navidrome().await;
        Mock::given(method("GET"))
            .and(path("/api/player"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": "p1", "name": "Symfonium", "client": "Symfonium", "userId": "u-alex", "maxBitRate": 0},
                {"id": "p2", "name": "Feishin", "client": "Feishin", "userId": "u-owner", "maxBitRate": 0}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/player/p1"))
            .and(body_json(json!({"maxBitRate": 192})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        let players = admin.list_players("u-alex").await.expect("listed");
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].id, "p1");
        admin.set_player_max_bit_rate("p1", 192).await.expect("set");
    }

    #[tokio::test]
    async fn scan_targets_one_library() {
        let server = navidrome().await;
        Mock::given(method("GET"))
            .and(path("/rest/startScan"))
            .and(query_param("target", "3:"))
            .and(query_param("u", "admin"))
            .and(query_param("t", "tok"))
            .and(query_param("s", "salt"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"subsonic-response":{"status":"ok","scanStatus":{"scanning":true}}}"#,
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        admin.scan_library(3).await.expect("started");
    }

    #[tokio::test]
    async fn delete_user_sends_a_delete() {
        let server = navidrome().await;
        Mock::given(method("DELETE"))
            .and(path("/api/user/u-alex"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let (admin, _, _) = admin_for(&server.uri());
        admin.delete_user("u-alex").await.expect("deleted");
    }
}
