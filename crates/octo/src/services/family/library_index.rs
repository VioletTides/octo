//! Which library every song is in. Octo's background identity is a Navidrome admin, and an
//! admin sees every library, members' own included; this index (built from Navidrome's native
//! song list) is how Octo tells shared songs from a member's own, so library-wide work can keep
//! to the shared library and each member sees only what is theirs.
//!
//! The index is read again after every scan Octo starts and every 6 hours, while Family is on.
//! Between a scan and the next read, a song Octo has just placed is known through
//! [`LibraryIndex::note_placed`]; any other song the index does not know counts as shared.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use octo_core::common::dotnet::eq_ignore_case;
use octo_core::settings::SettingsStore;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::family_store::FamilyStore;
use crate::services::library::navidrome_song_list::{combine, each_song, full_path};
use crate::services::local::ILocalLibraryService;
use crate::services::subsonic::NavidromeIdentityService;

/// One song as the index keeps it (about 80 bytes plus its strings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedSong {
    pub id: String,
    pub album_id: String,
    /// The song's artist and album artist ids, without repeats.
    pub artist_ids: Vec<String>,
    pub library_id: Option<i64>,
    /// Navidrome's advisory mark: `explicit`, `clean`, or empty.
    pub explicit_status: String,
    /// The file's full path as Navidrome sees it.
    pub path: String,
    /// Bytes.
    pub size: u64,
    /// Its genres as Navidrome names them (`genres`, else the one `genre`).
    pub genres: Vec<String>,
    /// Seconds.
    pub duration: u32,
}

/// A file Octo placed in a library since the index was last read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFile {
    pub library_id: i64,
    pub path: String,
    pub size: u64,
}

/// How often and how patiently the index is read again.
#[derive(Debug, Clone, Copy)]
pub struct IndexTiming {
    /// Read again this often.
    pub every: Duration,
    /// After Octo starts a scan, wait this long before asking whether it is done.
    pub settle: Duration,
    /// Ask whether the scan is done this often.
    pub poll: Duration,
    /// Read anyway after this long, scan done or not.
    pub max_wait: Duration,
}

impl Default for IndexTiming {
    fn default() -> Self {
        IndexTiming {
            every: Duration::from_secs(6 * 60 * 60),
            settle: Duration::from_secs(2),
            poll: Duration::from_secs(3),
            max_wait: Duration::from_secs(10 * 60),
        }
    }
}

#[derive(Default)]
struct Songs {
    by_id: HashMap<String, Arc<IndexedSong>>,
    by_path: HashMap<String, Arc<IndexedSong>>,
}

/// Every song's library, read from Navidrome as the admin and kept in memory.
pub struct LibraryIndex {
    identity: NavidromeIdentityService,
    http: reqwest::Client,
    settings: Arc<SettingsStore>,
    songs: RwLock<Arc<Songs>>,
    placed: Mutex<Vec<PlacedFile>>,
    refreshed: Mutex<Option<DateTime<Utc>>>,
    reading: tokio::sync::Mutex<()>,
    scan_notice: Arc<Notify>,
    timing: IndexTiming,
    /// Whether to keep reading while Family is off (members still have libraries of their own).
    keep_reading: RwLock<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
}

/// One song's JSON from `/api/song`, as the index keeps it.
fn indexed(song: &Value) -> Option<IndexedSong> {
    let text = |name: &str| song.get(name).and_then(Value::as_str).unwrap_or("").to_string();
    let id = text("id");
    let path = text("path");
    if id.is_empty() || path.is_empty() {
        return None;
    }
    let relative = path
        .replace('\\', "/")
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let library_path = text("libraryPath");
    let full = if !library_path.is_empty() {
        full_path(&combine(&library_path, &relative))
    } else if path.starts_with('/') {
        // An older Navidrome reports the full path instead.
        full_path(&path)
    } else {
        relative
    };
    let mut artist_ids = Vec::new();
    for name in ["artistId", "albumArtistId"] {
        let artist = text(name);
        if !artist.is_empty() && !artist_ids.contains(&artist) {
            artist_ids.push(artist);
        }
    }
    let library_id = match song.get("libraryId") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };
    Some(IndexedSong {
        id,
        album_id: text("albumId"),
        artist_ids,
        library_id,
        explicit_status: text("explicitStatus"),
        path: full,
        size: song.get("size").and_then(Value::as_u64).unwrap_or(0),
        genres: genres_of(song),
        duration: song
            .get("duration")
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map_or(0, |seconds| seconds.round().min(f64::from(u32::MAX)) as u32),
    })
}

/// A native song row's genres: each of `genres` by name, else its one `genre`, without repeats.
fn genres_of(song: &Value) -> Vec<String> {
    let mut genres: Vec<String> = Vec::new();
    if let Some(Value::Array(list)) = song.get("genres") {
        for genre in list {
            let name = genre
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| genre.as_str())
                .unwrap_or("")
                .trim();
            if !name.is_empty() && !genres.iter().any(|g| g.eq_ignore_ascii_case(name)) {
                genres.push(name.to_string());
            }
        }
    }
    if genres.is_empty() {
        let one = song.get("genre").and_then(Value::as_str).unwrap_or("").trim();
        if !one.is_empty() {
            genres.push(one.to_string());
        }
    }
    genres
}

impl LibraryIndex {
    pub fn new(
        identity: NavidromeIdentityService,
        http: reqwest::Client,
        settings: Arc<SettingsStore>,
        scan_notice: Arc<Notify>,
    ) -> Self {
        LibraryIndex {
            identity,
            http,
            settings,
            songs: RwLock::new(Arc::new(Songs::default())),
            placed: Mutex::new(Vec::new()),
            refreshed: Mutex::new(None),
            reading: tokio::sync::Mutex::new(()),
            scan_notice,
            timing: IndexTiming::default(),
            keep_reading: RwLock::new(None),
        }
    }

    /// The same index read on another schedule (tests).
    pub fn with_timing(mut self, timing: IndexTiming) -> Self {
        self.timing = timing;
        self
    }

    /// Keeps the index read while Family is off for as long as `needed` answers true, so
    /// library-wide work can still tell members' own songs from the shared library.
    pub fn keep_reading_while(&self, needed: impl Fn() -> bool + Send + Sync + 'static) {
        *self.keep_reading.write() = Some(Arc::new(needed));
    }

    /// Whether the index should be read now.
    fn wanted(&self) -> bool {
        self.settings.current().family.enabled
            || self.keep_reading.read().as_ref().is_some_and(|needed| needed())
    }

    /// What the library service notifies when Octo starts a scan.
    pub fn scan_notice(&self) -> Arc<Notify> {
        Arc::clone(&self.scan_notice)
    }

    /// Reads every song from Navidrome now (one read at a time; a second caller waits for the
    /// first and reads again). Answers how many songs the index holds.
    pub async fn refresh(&self) -> anyhow::Result<usize> {
        let _reading = self.reading.lock().await;
        let base = self.settings.current().subsonic.url.clone().unwrap_or_default();
        if base.trim().is_empty() {
            anyhow::bail!("Navidrome's address is not set");
        }
        let mut songs = Songs::default();
        let signed_in = each_song(&self.identity, &self.http, &base, |song| {
            if let Some(song) = indexed(song) {
                let song = Arc::new(song);
                songs.by_path.insert(song.path.clone(), Arc::clone(&song));
                songs.by_id.insert(song.id.clone(), song);
            }
        })
        .await?;
        if !signed_in {
            anyhow::bail!("there is no Navidrome admin sign-in to read the library with");
        }
        let count = songs.by_id.len();
        self.placed
            .lock()
            .retain(|placed| !songs.by_path.contains_key(&placed.path));
        *self.songs.write() = Arc::new(songs);
        *self.refreshed.lock() = Some(Utc::now());
        Ok(count)
    }

    /// When the index was last read; None before the first read.
    pub fn refreshed_at(&self) -> Option<DateTime<Utc>> {
        *self.refreshed.lock()
    }

    /// Whether the index has been read at least once.
    pub fn is_ready(&self) -> bool {
        self.refreshed_at().is_some()
    }

    pub fn len(&self) -> usize {
        self.songs.read().by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn song(&self, id: &str) -> Option<IndexedSong> {
        self.songs.read().by_id.get(id).map(|song| (**song).clone())
    }

    /// The library a song is in, when the index knows the song.
    pub fn library_of(&self, id: &str) -> Option<i64> {
        self.songs.read().by_id.get(id).and_then(|song| song.library_id)
    }

    /// The library whose songs sit under `folder` (any one of them tells); None when the index
    /// holds no song there. Members' songs do not tell for a folder above the members folder
    /// (the music folder, when members' music is kept next to it).
    pub fn library_of_folder(&self, folder: &str) -> Option<i64> {
        let prefix = format!("{}/", folder.trim_end_matches('/'));
        let members = format!("{}/", self.members_path());
        let above_members = !prefix.starts_with(&members);
        self.songs
            .read()
            .by_path
            .iter()
            .find(|(path, _)| path.starts_with(&prefix) && !(above_members && path.starts_with(&members)))
            .and_then(|(_, song)| song.library_id)
    }

    /// The members folder, without a trailing slash.
    pub fn members_path(&self) -> String {
        super::members_place::resolve(&self.settings, &self.identity)
    }

    /// The song whose file is at this full path.
    pub fn find_by_path(&self, path: &str) -> Option<IndexedSong> {
        self.songs.read().by_path.get(path).map(|song| (**song).clone())
    }

    /// Every song in one library.
    pub fn songs_in_library(&self, library_id: i64) -> Vec<IndexedSong> {
        self.songs
            .read()
            .by_id
            .values()
            .filter(|song| song.library_id == Some(library_id))
            .map(|song| (**song).clone())
            .collect()
    }

    /// Calls `visit` with every song the index holds, in no order.
    pub fn each_song(&self, mut visit: impl FnMut(&IndexedSong)) {
        let songs = Arc::clone(&self.songs.read());
        for song in songs.by_id.values() {
            visit(song);
        }
    }

    /// How many songs `keep` answers true for.
    pub fn count_songs(&self, keep: impl Fn(&IndexedSong) -> bool) -> usize {
        self.songs.read().by_id.values().filter(|song| keep(song)).count()
    }

    /// The bytes one library holds: its songs at full size, plus files Octo placed there since
    /// the last read.
    pub fn bytes_in_library(&self, library_id: i64) -> u64 {
        let indexed: u64 = self
            .songs
            .read()
            .by_id
            .values()
            .filter(|song| song.library_id == Some(library_id))
            .map(|song| song.size)
            .sum();
        let placed: u64 = self
            .placed
            .lock()
            .iter()
            .filter(|placed| placed.library_id == library_id)
            .map(|placed| placed.size)
            .sum();
        indexed + placed
    }

    /// A file Octo just placed in a library, known before Navidrome's next scan is read.
    pub fn note_placed(&self, library_id: i64, path: &str, size: u64) {
        let mut placed = self.placed.lock();
        placed.retain(|p| p.path != path);
        placed.push(PlacedFile {
            library_id,
            path: path.to_string(),
            size,
        });
    }

    /// The library of a file Octo placed since the last read.
    pub fn placed_library_of(&self, path: &str) -> Option<i64> {
        self.placed
            .lock()
            .iter()
            .find(|placed| placed.path == path)
            .map(|placed| placed.library_id)
    }

    /// Reads the index now, every [`IndexTiming::every`], and after each scan Octo starts
    /// (once Navidrome says the scan is done), until `token` is cancelled.
    pub async fn run(
        self: Arc<Self>,
        token: CancellationToken,
        library: Arc<dyn ILocalLibraryService>,
    ) -> anyhow::Result<()> {
        self.refresh_logged().await;
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + self.timing.every, self.timing.every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => self.refresh_logged().await,
                () = self.scan_notice.notified() => {
                    self.wait_for_scan(&token, library.as_ref()).await;
                    if token.is_cancelled() {
                        return Ok(());
                    }
                    self.refresh_logged().await;
                }
                () = token.cancelled() => return Ok(()),
            }
        }
    }

    /// Reads the index and logs how that went. While Family is off and no member has a library
    /// of their own, nothing is read: the index keeps what it last read and Navidrome is left
    /// alone.
    async fn refresh_logged(&self) {
        if !self.wanted() {
            return;
        }
        match self.refresh().await {
            Ok(count) => info!("FAMILY library index read: {count} songs"),
            Err(e) => warn!("FAMILY library index could not be read: {e}"),
        }
    }

    /// Waits until Navidrome's scan is over (or it cannot say, or [`IndexTiming::max_wait`]
    /// passes).
    async fn wait_for_scan(&self, token: &CancellationToken, library: &dyn ILocalLibraryService) {
        let started = tokio::time::Instant::now();
        let mut wait = self.timing.settle;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = token.cancelled() => return,
            }
            let scanning = library
                .get_scan_status()
                .await
                .is_some_and(|status| status.scanning);
            if !scanning || started.elapsed() >= self.timing.max_wait {
                return;
            }
            wait = self.timing.poll;
        }
    }
}

/// Which libraries a piece of work may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryScope {
    /// Every library that is not a member's own: what library-wide work (health, duplicates,
    /// quality upgrades, explicit marks) keeps to.
    Shared,
    /// The shared libraries plus this member's own.
    Viewer(String),
}

/// Answers whether a song is in a scope. With Family off a member and their own library see
/// nothing of each other, since no member signs in, so a viewer's scope holds every song.
/// Library-wide work is another matter: members' own libraries and the family pool stay on
/// disk when Family is turned off, so that work keeps to the shared library for as long as
/// any member has a library of their own ([`ScopeResolver::keeps_shared`]).
#[derive(Clone)]
pub struct ScopeResolver {
    settings: Arc<SettingsStore>,
    store: Arc<FamilyStore>,
    index: Arc<LibraryIndex>,
}

/// One look at the scopes, made once and asked about many songs: the members' own libraries
/// and the members folder, read when it was made.
pub struct ScopeView {
    /// Whether songs are told apart at all; when not, every song is in every scope.
    apart: bool,
    /// Each member's own library id and their name.
    owners: HashMap<i64, String>,
    members_path: String,
    index: Arc<LibraryIndex>,
}

impl ScopeResolver {
    pub fn new(settings: Arc<SettingsStore>, store: Arc<FamilyStore>, index: Arc<LibraryIndex>) -> Self {
        ScopeResolver {
            settings,
            store,
            index,
        }
    }

    /// Whether scopes mean anything to a viewer now (Family is on).
    pub fn active(&self) -> bool {
        self.settings.current().family.enabled
    }

    /// Whether library-wide work must keep to the shared library: Family is on, or a member
    /// has a library of their own (Family was on and was turned off).
    pub fn keeps_shared(&self) -> bool {
        self.active() || self.store.has_personal_libraries()
    }

    /// Whether a destructive walk may trust the answers: nothing needs keeping apart, or the
    /// index has been read at least once.
    pub fn ready(&self) -> bool {
        !self.keeps_shared() || self.index.is_ready()
    }

    pub fn index(&self) -> &Arc<LibraryIndex> {
        &self.index
    }

    /// A view for a viewer's scope, apart only while Family is on.
    pub fn view(&self) -> ScopeView {
        self.view_apart(self.active())
    }

    /// A view for library-wide work, apart while [`Self::keeps_shared`].
    pub fn shared_view(&self) -> ScopeView {
        self.view_apart(self.keeps_shared())
    }

    fn view_apart(&self, apart: bool) -> ScopeView {
        ScopeView {
            apart,
            owners: if apart {
                self.store.personal_libraries().into_iter().collect()
            } else {
                HashMap::new()
            },
            members_path: self.index.members_path(),
            index: Arc::clone(&self.index),
        }
    }

    /// Every member's own library id.
    pub fn personal_library_ids(&self) -> Vec<i64> {
        self.store
            .personal_libraries()
            .into_iter()
            .map(|(library, _)| library)
            .collect()
    }

    /// The member whose own library this is.
    pub fn personal_library_owner(&self, library_id: i64) -> Option<String> {
        self.store
            .personal_libraries()
            .into_iter()
            .find(|(library, _)| *library == library_id)
            .map(|(_, owner)| owner)
    }

    /// The member a file under the members folder belongs to (`<members>/<user>/...`).
    /// `Some("")` for a file under the members folder that is no member's library (the family
    /// pool), None for a file outside it.
    pub fn path_owner(&self, full: &str) -> Option<String> {
        path_owner_under(&self.index.members_path(), full)
    }

    /// Whether a song (by id, path, or both) is in `scope`. Asking about many songs, make a
    /// [`ScopeView`] once instead.
    pub fn allows(&self, scope: &LibraryScope, song_id: Option<&str>, full: Option<&str>) -> bool {
        self.view().allows(scope, song_id, full)
    }

    /// Whether a library is in `scope` (an unknown library counts as shared).
    pub fn allows_library(&self, scope: &LibraryScope, library_id: i64) -> bool {
        self.view().allows_library(scope, library_id)
    }
}

impl ScopeView {
    /// Whether songs are told apart in this view.
    pub fn apart(&self) -> bool {
        self.apart
    }

    /// The member whose own library holds this song, from the index, a file Octo just placed,
    /// or the file's path. None for a shared song (or one Octo does not know yet).
    fn owner_of(&self, song_id: Option<&str>, full: Option<&str>) -> Option<String> {
        let library = song_id
            .and_then(|id| self.index.library_of(id))
            .or_else(|| full.and_then(|path| self.index.placed_library_of(path)));
        if let Some(owner) = library.and_then(|id| self.owners.get(&id)) {
            return Some(owner.clone());
        }
        full.and_then(|path| path_owner_under(&self.members_path, path))
    }

    /// Whether a song (by id, path, or both) is in `scope`.
    pub fn allows(&self, scope: &LibraryScope, song_id: Option<&str>, full: Option<&str>) -> bool {
        if !self.apart {
            return true;
        }
        match (self.owner_of(song_id, full), scope) {
            (None, _) => true,
            (Some(_), LibraryScope::Shared) => false,
            (Some(owner), LibraryScope::Viewer(viewer)) => {
                !owner.is_empty() && eq_ignore_case(&owner, viewer)
            }
        }
    }

    /// Whether a library is in `scope` (an unknown library counts as shared).
    pub fn allows_library(&self, scope: &LibraryScope, library_id: i64) -> bool {
        if !self.apart {
            return true;
        }
        match (self.owners.get(&library_id), scope) {
            (None, _) => true,
            (Some(_), LibraryScope::Shared) => false,
            (Some(owner), LibraryScope::Viewer(viewer)) => eq_ignore_case(owner, viewer),
        }
    }
}

/// The member a file under `members` belongs to; see [`ScopeResolver::path_owner`].
fn path_owner_under(members: &str, full: &str) -> Option<String> {
    let rest = full.strip_prefix(members)?.strip_prefix('/')?;
    let first = rest.split('/').next().unwrap_or("");
    if first.starts_with('.') {
        return Some(String::new());
    }
    Some(first.to_string())
}

/// A scope over no Navidrome for tests: Family on or off, and members with their own library
/// ids. Songs are told apart by path (`/members/<user>/...`) alone.
#[cfg(test)]
pub(crate) fn scope_for_tests(family_on: bool, members: &[(&str, i64)]) -> ScopeResolver {
    use octo_core::family::abilities::Preset;
    use octo_core::settings::AppSettings;

    use super::family_store::{Member, SharedLibraries};

    let mut app = AppSettings::default();
    app.family.enabled = family_on;
    app.family.members_path = "/members".into();
    let settings = Arc::new(SettingsStore::for_tests(app));
    let http = reqwest::Client::new();
    let index = Arc::new(LibraryIndex::new(
        NavidromeIdentityService::new(settings.clone(), http.clone()),
        http,
        settings.clone(),
        Arc::new(Notify::new()),
    ));
    let store = Arc::new(FamilyStore::in_memory());
    for (name, library) in members {
        store.upsert_member(Member {
            username: name.to_string(),
            display_name: String::new(),
            preset: Preset::Listener,
            edits: Default::default(),
            navidrome_user_id: String::new(),
            navidrome_password: String::new(),
            personal_library_id: Some(*library),
            shared_libraries: SharedLibraries::All,
            account_password: String::new(),
            created: Utc::now(),
            suspended: false,
            ntfy_topic: String::new(),
        });
    }
    ScopeResolver::new(settings, store, index)
}

/// A song row's full path from its library path and its path in the library (or a full path
/// an older Navidrome gave).
pub fn row_full_path(library_path: Option<&str>, path: &str) -> String {
    match library_path.filter(|base| !base.is_empty()) {
        Some(base) if !path.starts_with('/') => full_path(&combine(base, path.trim_start_matches('/'))),
        _ => full_path(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::family::navidrome_admin::tests::{admin_for, navidrome};
    use crate::services::local::test_support::FakeLocalLibrary;
    use octo_core::family::abilities::Preset;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::services::family::family_store::{Member, SharedLibraries};

    fn song(id: &str, library: i64, library_path: &str, rel: &str, explicit: &str, size: u64) -> Value {
        json!({
            "id": id, "albumId": format!("al-{id}"), "artistId": "ar-1", "albumArtistId": "ar-1",
            "libraryId": library, "libraryPath": library_path, "path": rel,
            "explicitStatus": explicit, "size": size, "title": "T",
            "duration": 240.6, "genre": "Rock", "genres": [{"id": "g1", "name": "Rock"}, {"id": "g2", "name": "Pop"}]
        })
    }

    async fn mount_songs(server: &MockServer, songs: Value) {
        Mock::given(method("GET"))
            .and(path("/api/song"))
            .and(query_param("_start", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(songs))
            .mount(server)
            .await;
    }

    fn index_for(url: &str) -> (Arc<LibraryIndex>, Arc<SettingsStore>) {
        let (_, identity, settings) = admin_for(url);
        let mut app = (*settings.current()).clone();
        app.family.members_path = "/members".into();
        settings.set(app);
        let index = LibraryIndex::new(
            identity,
            crate::services::http_client_factory::default_client(),
            settings.clone(),
            Arc::new(Notify::new()),
        );
        (Arc::new(index), settings)
    }

    fn member(name: &str, library: i64) -> Member {
        Member {
            username: name.to_string(),
            display_name: String::new(),
            preset: Preset::Listener,
            edits: Default::default(),
            navidrome_user_id: format!("u-{name}"),
            navidrome_password: String::new(),
            personal_library_id: Some(library),
            shared_libraries: SharedLibraries::All,
            account_password: String::new(),
            created: Utc::now(),
            suspended: false,
            ntfy_topic: String::new(),
        }
    }

    #[tokio::test]
    async fn the_index_reads_the_native_song_list_shape() {
        let server = navidrome().await;
        mount_songs(
            &server,
            json!([
                song("s1", 1, "/music", "Artist/Album/01 Song.flac", "explicit", 30_000_000),
                song("s2", 3, "/members/alex", "Artist/Album/02 Other.mp3", "", 8_000_000),
                {"id": "s3", "path": ""},
            ]),
        )
        .await;
        let (index, _) = index_for(&server.uri());
        assert!(!index.is_ready());
        assert_eq!(index.refresh().await.expect("read"), 2);
        assert!(index.is_ready());
        let s1 = index.song("s1").expect("indexed");
        assert_eq!(
            s1,
            IndexedSong {
                id: "s1".into(),
                album_id: "al-s1".into(),
                artist_ids: vec!["ar-1".into()],
                library_id: Some(1),
                explicit_status: "explicit".into(),
                path: "/music/Artist/Album/01 Song.flac".into(),
                size: 30_000_000,
                genres: vec!["Rock".into(), "Pop".into()],
                duration: 241,
            }
        );
        assert_eq!(index.library_of("s2"), Some(3));
        assert_eq!(
            index
                .find_by_path("/members/alex/Artist/Album/02 Other.mp3")
                .map(|s| s.id),
            Some("s2".into())
        );
        assert_eq!(index.songs_in_library(3).len(), 1);
        assert_eq!(index.bytes_in_library(3), 8_000_000);
    }

    #[tokio::test]
    async fn placed_files_count_until_the_next_read_finds_them() {
        let server = navidrome().await;
        let (index, _) = index_for(&server.uri());
        index.note_placed(3, "/members/alex/A/B/new.flac", 1_000);
        index.note_placed(3, "/members/alex/A/B/new.flac", 2_000);
        assert_eq!(index.bytes_in_library(3), 2_000);
        mount_songs(
            &server,
            json!([song("s9", 3, "/members/alex", "A/B/new.flac", "", 2_000)]),
        )
        .await;
        index.refresh().await.expect("read");
        // Counted once: by the index now, no longer as placed.
        assert_eq!(index.bytes_in_library(3), 2_000);
        assert_eq!(index.placed_library_of("/members/alex/A/B/new.flac"), None);
    }

    #[tokio::test]
    async fn without_an_admin_sign_in_the_index_is_not_ready() {
        let server = MockServer::start().await;
        let settings = Arc::new(SettingsStore::for_tests(octo_core::settings::AppSettings {
            subsonic: octo_core::settings::SubsonicSettings {
                url: Some(server.uri()),
                auto_detect_download_path: false,
                ..Default::default()
            },
            ..Default::default()
        }));
        let http = crate::services::http_client_factory::default_client();
        let index = LibraryIndex::new(
            NavidromeIdentityService::new(settings.clone(), http.clone()),
            http,
            settings,
            Arc::new(Notify::new()),
        );
        assert!(index.refresh().await.is_err());
        assert!(!index.is_ready());
    }

    fn fast() -> IndexTiming {
        IndexTiming {
            every: Duration::from_millis(300),
            settle: Duration::from_millis(10),
            poll: Duration::from_millis(10),
            max_wait: Duration::from_secs(2),
        }
    }

    async fn song_reads(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|r| r.url.path() == "/api/song")
            .count()
    }

    fn turn_family(settings: &SettingsStore, enabled: bool) {
        let mut changed = settings.current().as_ref().clone();
        changed.family.enabled = enabled;
        settings.set(changed);
    }

    #[tokio::test]
    async fn the_index_is_read_again_after_a_scan_octo_starts() {
        let server = navidrome().await;
        mount_songs(&server, json!([])).await;
        let (index, settings) = index_for(&server.uri());
        turn_family(&settings, true);
        let index = Arc::new(
            Arc::into_inner(index)
                .expect("one owner")
                .with_timing(IndexTiming {
                    every: Duration::from_secs(3600),
                    ..fast()
                }),
        );
        let token = CancellationToken::new();
        let library: Arc<dyn ILocalLibraryService> = Arc::new(FakeLocalLibrary::default());
        let running = tokio::spawn(Arc::clone(&index).run(token.clone(), library));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(song_reads(&server).await, 1, "read once at start");
        index.scan_notice().notify_one();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(song_reads(&server).await, 2, "read again after the scan");
        token.cancel();
        running.await.expect("joins").expect("stops cleanly");
    }

    #[tokio::test]
    async fn the_index_stays_quiet_while_family_is_off() {
        let server = navidrome().await;
        mount_songs(&server, json!([])).await;
        let (index, settings) = index_for(&server.uri());
        let index = Arc::new(Arc::into_inner(index).expect("one owner").with_timing(fast()));
        let token = CancellationToken::new();
        let library: Arc<dyn ILocalLibraryService> = Arc::new(FakeLocalLibrary::default());
        let running = tokio::spawn(Arc::clone(&index).run(token.clone(), library));
        index.scan_notice().notify_one();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(
            song_reads(&server).await,
            0,
            "Navidrome is not read while Family is off"
        );
        turn_family(&settings, true);
        tokio::time::sleep(Duration::from_millis(500)).await;
        token.cancel();
        running.await.expect("joins").expect("stops cleanly");
        assert!(song_reads(&server).await >= 1, "read again once Family is on");
    }

    #[tokio::test]
    async fn the_index_is_read_again_on_its_timer() {
        let server = navidrome().await;
        mount_songs(&server, json!([])).await;
        let (index, settings) = index_for(&server.uri());
        turn_family(&settings, true);
        let index = Arc::new(Arc::into_inner(index).expect("one owner").with_timing(fast()));
        let token = CancellationToken::new();
        let library: Arc<dyn ILocalLibraryService> = Arc::new(FakeLocalLibrary::default());
        let running = tokio::spawn(Arc::clone(&index).run(token.clone(), library));
        tokio::time::sleep(Duration::from_millis(800)).await;
        token.cancel();
        running.await.expect("joins").expect("stops cleanly");
        let reads = song_reads(&server).await;
        assert!(
            (2..=4).contains(&reads),
            "{reads} reads in 800 ms at one every 300 ms"
        );
    }

    #[tokio::test]
    async fn scopes_keep_members_libraries_apart_only_when_family_is_on() {
        let server = navidrome().await;
        mount_songs(
            &server,
            json!([
                song("shared", 1, "/music", "A/B/1.flac", "", 1),
                song("alex1", 3, "/members/alex", "A/B/2.flac", "", 1),
                song("sam1", 4, "/members/sam", "A/B/3.flac", "", 1),
            ]),
        )
        .await;
        let (index, settings) = index_for(&server.uri());
        index.refresh().await.expect("read");
        let store = Arc::new(FamilyStore::in_memory());
        store.upsert_member(member("alex", 3));
        store.upsert_member(member("sam", 4));
        let scope = ScopeResolver::new(settings.clone(), store, index);
        let alex = LibraryScope::Viewer("Alex".into());

        // Family off: everything is in every scope.
        assert!(!scope.active());
        assert!(scope.allows(&LibraryScope::Shared, Some("alex1"), None));

        let mut on = settings.current().as_ref().clone();
        on.family.enabled = true;
        settings.set(on);
        assert!(scope.active());
        assert!(scope.ready());
        assert!(scope.allows(&LibraryScope::Shared, Some("shared"), None));
        assert!(!scope.allows(&LibraryScope::Shared, Some("alex1"), None));
        assert!(scope.allows(&alex, Some("alex1"), None));
        assert!(!scope.allows(&alex, Some("sam1"), None));
        assert!(scope.allows(&alex, Some("shared"), None));
        // A song the index does not know yet counts as shared...
        assert!(scope.allows(&LibraryScope::Shared, Some("brand-new"), None));
        // ...unless its path says whose it is, or Octo placed it.
        assert!(!scope.allows(
            &LibraryScope::Shared,
            Some("brand-new"),
            Some("/members/sam/x.flac")
        ));
        assert!(!scope.allows(&alex, None, Some("/members/.pool/A/x.flac")));
        assert!(!scope.allows(&LibraryScope::Shared, None, Some("/members/.pool/A/x.flac")));
        assert!(scope.allows(&LibraryScope::Shared, None, Some("/membership/x.flac")));
        assert!(!scope.allows_library(&LibraryScope::Shared, 3));
        assert!(scope.allows_library(&alex, 3));
        assert!(scope.allows_library(&LibraryScope::Shared, 1));
        assert_eq!(scope.personal_library_ids(), vec![3, 4]);
    }

    #[tokio::test]
    async fn family_off_with_member_libraries_left_keeps_library_wide_work_to_the_shared_library() {
        let server = navidrome().await;
        mount_songs(
            &server,
            json!([
                song("shared", 1, "/music", "A/B/1.flac", "", 1),
                song("alex1", 3, "/members/alex", "A/B/2.flac", "", 1),
            ]),
        )
        .await;
        let (index, settings) = index_for(&server.uri());
        let store = Arc::new(FamilyStore::in_memory());
        store.upsert_member(member("alex", 3));
        let needed = Arc::clone(&store);
        index.keep_reading_while(move || needed.has_personal_libraries());
        let index = Arc::new(Arc::into_inner(index).expect("one owner").with_timing(fast()));
        let scope = ScopeResolver::new(settings, Arc::clone(&store), Arc::clone(&index));
        assert!(!scope.active(), "Family is off");
        assert!(scope.keeps_shared(), "Alex's library is still there");
        assert!(
            !scope.ready(),
            "nothing destructive runs before the index is read"
        );

        let token = CancellationToken::new();
        let library: Arc<dyn ILocalLibraryService> = Arc::new(FakeLocalLibrary::default());
        let running = tokio::spawn(Arc::clone(&index).run(token.clone(), library));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            song_reads(&server).await >= 1,
            "the index is read while member data is left"
        );
        assert!(scope.ready());
        let view = scope.shared_view();
        assert!(view.apart());
        assert!(view.allows(&LibraryScope::Shared, Some("shared"), None));
        assert!(!view.allows(&LibraryScope::Shared, Some("alex1"), None));
        assert!(!view.allows(&LibraryScope::Shared, None, Some("/members/.pool/A/x.flac")));
        assert!(!view.allows_library(&LibraryScope::Shared, 3));
        // A viewer's scope, which only signed-in members have, keeps nothing apart.
        assert!(!scope.view().apart());

        // Once no member has a library of their own, nothing is kept apart and nothing is read.
        store.remove_member("alex");
        assert!(!scope.keeps_shared());
        assert!(scope.ready());
        assert!(!scope.shared_view().apart());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let reads = song_reads(&server).await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        token.cancel();
        running.await.expect("joins").expect("stops cleanly");
        assert_eq!(song_reads(&server).await, reads, "Navidrome is left alone");
    }

    #[test]
    fn row_paths_join_the_library_path() {
        assert_eq!(row_full_path(Some("/music"), "A/B/1.flac"), "/music/A/B/1.flac");
        assert_eq!(row_full_path(Some("/music/"), "/A/1.flac"), "/A/1.flac");
        assert_eq!(
            row_full_path(None, "/members/alex/1.flac"),
            "/members/alex/1.flac"
        );
    }
}
