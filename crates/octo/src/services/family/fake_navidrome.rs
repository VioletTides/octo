//! A Navidrome for family tests that keeps its own state: users, libraries, a song list made by
//! walking each library's folder on disk (links followed, as Navidrome follows them), stars,
//! playlists, and every audio request it was asked for. A scan (`startScan`) walks the folders
//! again.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use parking_lot::Mutex;
use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// One library: id, name, folder.
#[derive(Debug, Clone)]
pub(crate) struct FakeLibrary {
    pub id: i64,
    pub name: String,
    pub path: String,
}

/// What the fake Navidrome holds.
#[derive(Default)]
pub(crate) struct FakeNavidromeState {
    /// id, userName, admin.
    pub users: Mutex<Vec<(String, String, bool)>>,
    pub libraries: Mutex<Vec<FakeLibrary>>,
    /// Native song rows, as the last scan found them.
    pub songs: Mutex<Vec<Value>>,
    /// (user, song id).
    pub stars: Mutex<Vec<(String, String)>>,
    /// Playlist id to (name, owner).
    pub playlists: Mutex<Vec<(String, String, String)>>,
    /// The query of every `stream` and `download` call.
    pub audio: Mutex<Vec<String>>,
    /// Subsonic passwords by user. A user listed here is signed in only with theirs (`p`,
    /// plain or `enc:` hex, or `t` and `s`); anyone else is taken at their word.
    pub passwords: Mutex<std::collections::HashMap<String, String>>,
    pub scans: AtomicI64,
    next: AtomicI64,
}

impl FakeNavidromeState {
    /// Walks every library's folder and makes the song list from what is there.
    pub fn rescan(&self) {
        let libraries = self.libraries.lock().clone();
        let mut rows = Vec::new();
        for library in &libraries {
            walk(
                Path::new(&library.path),
                Path::new(&library.path),
                library,
                &mut rows,
            );
        }
        rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        *self.songs.lock() = rows;
        self.scans.fetch_add(1, Ordering::SeqCst);
    }

    /// The id of the song at `relative` in library `library`.
    pub fn song_id(library: i64, relative: &str) -> String {
        let mut hasher = DefaultHasher::new();
        (library, relative).hash(&mut hasher);
        format!("nd{:016x}", hasher.finish())
    }

    fn next_id(&self) -> i64 {
        10 + self.next.fetch_add(1, Ordering::SeqCst)
    }
}

/// The browser token the fake's sign-in hands out, the only one it takes back.
const ISSUED_TOKEN: &str = "jwt-admin";

const AUDIO: [&str; 6] = ["flac", "mp3", "m4a", "ogg", "opus", "wav"];

fn walk(root: &Path, folder: &Path, library: &FakeLibrary, rows: &mut Vec<Value>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // Links are followed, to folders and to files alike.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(root, &path, library, rows);
            continue;
        }
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !AUDIO.contains(&extension.as_str()) {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let parts: Vec<&str> = relative.split('/').collect();
        let artist = if parts.len() >= 3 { parts[0] } else { "Unknown" };
        let album = if parts.len() >= 2 {
            parts[parts.len() - 2]
        } else {
            "Unknown"
        };
        let title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        rows.push(json!({
            "id": FakeNavidromeState::song_id(library.id, &relative),
            "title": title,
            "album": album,
            "artist": artist,
            "albumArtist": artist,
            "albumId": format!("al-{}-{album}", library.id),
            "artistId": format!("ar-{artist}"),
            "albumArtistId": format!("ar-{artist}"),
            "libraryId": library.id,
            "libraryPath": library.path,
            "path": relative,
            "suffix": extension,
            "size": meta.len(),
            "duration": 180.4,
            "genre": "Rock",
            "genres": [{"id": "g-rock", "name": "Rock"}],
            "explicitStatus": "",
        }));
    }
}

/// A fake Navidrome on a mock server, and its state.
pub(crate) struct FakeNavidrome {
    pub server: MockServer,
    pub state: Arc<FakeNavidromeState>,
}

impl FakeNavidrome {
    pub async fn start() -> FakeNavidrome {
        let server = MockServer::start().await;
        let state = Arc::new(FakeNavidromeState::default());
        Mock::given(any())
            .respond_with(Responder(Arc::clone(&state)))
            .mount(&server)
            .await;
        FakeNavidrome { server, state }
    }

    pub fn uri(&self) -> String {
        self.server.uri()
    }

    pub fn add_user(&self, id: &str, name: &str, admin: bool) {
        self.state
            .users
            .lock()
            .push((id.to_string(), name.to_string(), admin));
    }

    pub fn add_library(&self, id: i64, name: &str, path: &str) {
        self.state.libraries.lock().push(FakeLibrary {
            id,
            name: name.to_string(),
            path: path.trim_end_matches('/').to_string(),
        });
    }
}

struct Responder(Arc<FakeNavidromeState>);

fn subsonic(fields: Value) -> ResponseTemplate {
    let mut response = json!({"status": "ok", "version": "1.16.1", "type": "navidrome"});
    if let (Value::Object(map), Value::Object(extra)) = (&mut response, fields) {
        map.extend(extra);
    }
    ResponseTemplate::new(200).set_body_json(json!({ "subsonic-response": response }))
}

fn subsonic_error(code: i32, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"subsonic-response": {
        "status": "failed", "version": "1.16.1", "error": {"code": code, "message": message}
    }}))
}

fn query(request: &Request, name: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn song_node(row: &Value) -> Value {
    json!({
        "id": row["id"], "title": row["title"], "album": row["album"], "artist": row["artist"],
        "albumId": row["albumId"], "artistId": row["artistId"], "isDir": false,
        "duration": 180, "suffix": row["suffix"], "size": row["size"], "type": "music",
        "path": row["path"],
    })
}

impl Respond for Responder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = &self.0;
        let method = request.method.as_str().to_ascii_uppercase();
        let path = request.url.path().to_string();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        match (method.as_str(), path.as_str()) {
            ("POST", "/auth/login") => {
                let user = body["username"].as_str().unwrap_or("");
                let admin = user == "admin"
                    || state
                        .users
                        .lock()
                        .iter()
                        .any(|(_, name, admin)| name == user && *admin);
                return ResponseTemplate::new(200).set_body_json(json!({
                    "id": format!("u-{user}"), "token": ISSUED_TOKEN, "isAdmin": admin,
                    "username": user, "subsonicToken": "tok", "subsonicSalt": "salt"
                }));
            }
            ("GET", "/api/song") => {
                let start: usize = query(request, "_start").and_then(|s| s.parse().ok()).unwrap_or(0);
                let end: usize = query(request, "_end")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(usize::MAX);
                let rows = state.songs.lock();
                let page: Vec<Value> = rows
                    .iter()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .cloned()
                    .collect();
                return ResponseTemplate::new(200).set_body_json(Value::Array(page));
            }
            ("GET", "/api/user") => {
                let users: Vec<Value> = state
                    .users
                    .lock()
                    .iter()
                    .map(|(id, name, admin)| {
                        json!({"id": id, "userName": name, "name": name, "isAdmin": admin})
                    })
                    .collect();
                return ResponseTemplate::new(200).set_body_json(Value::Array(users));
            }
            ("POST", "/api/user") => {
                let name = body["userName"].as_str().unwrap_or("").to_string();
                let id = format!("u-{name}");
                state.users.lock().push((id.clone(), name, false));
                return ResponseTemplate::new(200).set_body_json(json!({ "id": id }));
            }
            ("GET", "/api/library") => {
                let libraries: Vec<Value> = state
                    .libraries
                    .lock()
                    .iter()
                    .map(|l| json!({"id": l.id, "name": l.name, "path": l.path}))
                    .collect();
                return ResponseTemplate::new(200).set_body_json(Value::Array(libraries));
            }
            ("POST", "/api/library") => {
                let id = state.next_id();
                state.libraries.lock().push(FakeLibrary {
                    id,
                    name: body["name"].as_str().unwrap_or("").to_string(),
                    path: body["path"]
                        .as_str()
                        .unwrap_or("")
                        .trim_end_matches('/')
                        .to_string(),
                });
                return ResponseTemplate::new(200).set_body_json(json!({ "id": id }));
            }
            ("GET", "/api/player") => return ResponseTemplate::new(200).set_body_json(json!([])),
            _ => {}
        }
        if let Some(id) = path.strip_prefix("/api/library/") {
            if method == "DELETE" {
                let id: i64 = id.parse().unwrap_or(-1);
                state.libraries.lock().retain(|l| l.id != id);
            }
            return ResponseTemplate::new(200).set_body_json(json!({}));
        }
        if let Some(rest) = path.strip_prefix("/api/user/") {
            if method == "DELETE" && !rest.contains('/') {
                state.users.lock().retain(|(id, _, _)| id != rest);
            }
            return ResponseTemplate::new(200).set_body_json(json!({}));
        }
        let Some(endpoint) = path.strip_prefix("/rest/") else {
            return ResponseTemplate::new(404);
        };
        let endpoint = endpoint.trim_end_matches(".view");
        let user = query(request, "u").unwrap_or_default();
        // It checks the tokens it signs: only the one its sign-in hands out is good, and it
        // hands out no API keys.
        if query(request, "jwt").is_some_and(|token| token != ISSUED_TOKEN)
            || query(request, "apiKey").is_some()
        {
            return subsonic_error(40, "Wrong username or password");
        }
        if let Some(expected) = state.passwords.lock().get(&user).cloned() {
            let right = match (query(request, "p"), query(request, "t"), query(request, "s")) {
                (Some(sent), _, _) => {
                    let sent = match sent.strip_prefix("enc:").and_then(|hex| hex::decode(hex).ok()) {
                        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                        None => sent,
                    };
                    sent == expected
                }
                (None, Some(token), Some(salt)) => {
                    token.eq_ignore_ascii_case(&octo_core::family::codes::subsonic_token(&expected, &salt))
                }
                _ => false,
            };
            if !right {
                return subsonic_error(40, "Wrong username or password");
            }
        }
        match endpoint {
            "startScan" => {
                state.rescan();
                subsonic(json!({"scanStatus": {"scanning": false, "count": state.songs.lock().len()}}))
            }
            "getScanStatus" => subsonic(json!({"scanStatus": {"scanning": false}})),
            "star" => {
                if let Some(id) = query(request, "id") {
                    state.stars.lock().push((user, id));
                }
                subsonic(json!({}))
            }
            "getSong" => {
                let id = query(request, "id").unwrap_or_default();
                let found = state.songs.lock().iter().find(|row| row["id"] == id).cloned();
                match found {
                    Some(row) => {
                        let mut node = song_node(&row);
                        if state.stars.lock().iter().any(|(u, s)| *u == user && *s == id) {
                            node["starred"] = json!("2026-10-08T00:00:00Z");
                        }
                        subsonic(json!({ "song": node }))
                    }
                    None => subsonic_error(70, "Song not found"),
                }
            }
            "getPlaylist" => {
                let id = query(request, "id").unwrap_or_default();
                let found = state.playlists.lock().iter().find(|(p, _, _)| *p == id).cloned();
                match found {
                    Some((id, name, owner)) => subsonic(json!({"playlist": {
                        "id": id, "name": name, "owner": owner, "public": false,
                        "songCount": 0, "duration": 0, "entry": []
                    }})),
                    None => subsonic_error(70, "Playlist not found"),
                }
            }
            "getPlaylists" => subsonic(json!({"playlists": {"playlist": []}})),
            "stream" | "download" => {
                state
                    .audio
                    .lock()
                    .push(request.url.query().unwrap_or("").to_string());
                ResponseTemplate::new(200)
                    .insert_header("content-type", "audio/ogg")
                    .set_body_bytes(vec![0u8; 4096])
            }
            _ => subsonic(json!({})),
        }
    }
}
