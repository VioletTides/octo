//! What the family library tests share: a members folder in a temp dir, members with their own
//! library ids, a library service that records scans and mappings, stars that are only
//! remembered, and the pool over all of it.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use octo_core::common::Clock;
use octo_core::family::abilities::Preset;
use octo_core::models::domain::Song;
use octo_core::models::subsonic::ScanStatus;
use octo_core::settings::{AppSettings, SettingsStore, SubsonicSettings};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::family_playlist_store::FamilyPlaylistStore;
use super::family_pool_store::FamilyPoolStore;
use super::family_store::{FamilyStore, Member, SharedLibraries};
use super::library_index::LibraryIndex;
use super::pool::{FamilyPool, MemberStars, PoolParts, PoolTiming, SteadyClock};
use super::request_service::{
    FamilyEvents, Fetched, OutsideTarget, RequestCatalog, RequestFacts, RequestFetcher, RequestParts,
    RequestService,
};
use super::{LibraryWriteGate, PersonalLibrary};
use crate::services::local::i_local_library_service::{ParsedExternalId, ParsedSongId};
use crate::services::local::{ILocalLibraryService, LocalSongMapping};
use crate::services::subsonic::NavidromeIdentityService;
use octo_core::family::abilities::RequestQuality;

/// A library service that remembers what it was asked. Its Navidrome says it is scanning
/// while `scanning` is set, and that its scan is finished otherwise.
#[derive(Default)]
pub(crate) struct RecordingLibrary {
    /// `provider:id` to the path Octo downloaded it to.
    pub mappings: Mutex<HashMap<String, String>>,
    pub targeted: Mutex<Vec<i64>>,
    pub full_scans: AtomicUsize,
    pub forgotten: Mutex<Vec<String>>,
    pub restored: Mutex<Vec<String>>,
    pub scanning: AtomicBool,
    /// While set, starting a scan of one library panics, as a bug in the middle of a change
    /// would.
    pub panics: AtomicBool,
}

impl RecordingLibrary {
    pub fn map(&self, provider: &str, id: &str, path: &str) {
        self.mappings
            .lock()
            .insert(format!("{provider}:{id}"), path.to_string());
    }

    pub fn targeted(&self) -> Vec<i64> {
        self.targeted.lock().clone()
    }

    pub fn full_scans(&self) -> usize {
        self.full_scans.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ILocalLibraryService for RecordingLibrary {
    async fn get_local_path_for_external_song(&self, provider: &str, id: &str) -> Option<String> {
        self.mappings.lock().get(&format!("{provider}:{id}")).cloned()
    }

    async fn register_downloaded_song(&self, song: &Song, local_path: &str) -> anyhow::Result<()> {
        if let (Some(provider), Some(id)) = (&song.external_provider, &song.external_id) {
            self.map(provider, id, local_path);
        }
        Ok(())
    }

    async fn get_local_id_for_external_song(&self, _: &str, _: &str) -> Option<String> {
        None
    }

    fn parse_song_id(&self, song_id: &str) -> ParsedSongId {
        match song_id.strip_prefix("ext-test-song-") {
            Some(id) => (true, Some("test".into()), Some(id.into())),
            None => (false, None, None),
        }
    }

    fn parse_external_id(&self, _: &str) -> ParsedExternalId {
        (false, None, None, None)
    }

    async fn get_mappings(&self) -> Vec<LocalSongMapping> {
        Vec::new()
    }

    async fn find_mapping_by_tags(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Option<LocalSongMapping> {
        None
    }

    async fn forget_mapping(&self, local_path: &str) -> anyhow::Result<Vec<LocalSongMapping>> {
        self.forgotten.lock().push(local_path.to_string());
        self.mappings.lock().retain(|_, path| path != local_path);
        Ok(Vec::new())
    }

    async fn restore_mappings(&self, _: &[LocalSongMapping], local_path: &str) -> anyhow::Result<()> {
        self.restored.lock().push(local_path.to_string());
        Ok(())
    }

    async fn trigger_library_scan(&self, _force: bool) -> bool {
        self.full_scans.fetch_add(1, Ordering::SeqCst);
        true
    }

    async fn trigger_library_scan_of(&self, library_id: i64) -> bool {
        assert!(
            !self.panics.load(Ordering::SeqCst),
            "a scan of library {library_id} panicked"
        );
        self.targeted.lock().push(library_id);
        true
    }

    async fn get_scan_status(&self) -> Option<ScanStatus> {
        Some(ScanStatus {
            scanning: self.scanning.load(Ordering::SeqCst),
            count: None,
            last_scan: None,
            elapsed_time: None,
            folder_count: None,
        })
    }
}

/// Stars kept in memory: which songs each member has starred, and every star asked for. Asking
/// whether a song is starred takes `delay`, as a call to Navidrome takes a moment.
#[derive(Default)]
pub(crate) struct RecordingStars {
    pub starred: Mutex<HashSet<(String, String)>>,
    pub asked: Mutex<Vec<(String, String)>>,
    pub delay: Mutex<Duration>,
}

#[async_trait]
impl MemberStars for RecordingStars {
    async fn is_starred(&self, user: &str, song_id: &str) -> bool {
        let delay = *self.delay.lock();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        self.starred
            .lock()
            .contains(&(user.to_string(), song_id.to_string()))
    }

    async fn star(&self, user: &str, song_id: &str) -> bool {
        self.asked.lock().push((user.to_string(), song_id.to_string()));
        self.starred
            .lock()
            .insert((user.to_string(), song_id.to_string()));
        true
    }
}

pub(crate) fn member(name: &str, preset: Preset, library: i64) -> Member {
    Member {
        username: name.to_string(),
        display_name: String::new(),
        preset,
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

/// A members folder, a shared music folder and a pool over them, with Alex (a Member, library
/// 3) and Sam (a Listener, library 4). The index reads `navidrome` when one is given. The pool
/// folder holds its marker file, as when Family was turned on, and the pool reads the time
/// from a clock the test moves along ([`World::later`]).
pub(crate) struct World {
    pub dir: tempfile::TempDir,
    pub settings: Arc<SettingsStore>,
    pub family: Arc<FamilyStore>,
    pub index: Arc<LibraryIndex>,
    pub library: Arc<RecordingLibrary>,
    pub stars: Arc<RecordingStars>,
    pub playlists: Arc<FamilyPlaylistStore>,
    pub pool: Arc<FamilyPool>,
    pub now: Arc<Mutex<DateTime<Utc>>>,
    /// The time that has really passed, by the pool's steady clock.
    pub elapsed: Arc<Mutex<Duration>>,
}

impl World {
    pub fn new(navidrome: Option<&str>) -> World {
        Self::with_family(navidrome, true)
    }

    pub fn with_family(navidrome: Option<&str>, family_on: bool) -> World {
        let dir = tempfile::tempdir().expect("a temp dir");
        let members = dir.path().join("members");
        let music = dir.path().join("music");
        std::fs::create_dir_all(&members).expect("members folder");
        std::fs::create_dir_all(&music).expect("music folder");
        let mut app = AppSettings {
            subsonic: SubsonicSettings {
                url: navidrome.map(str::to_string),
                admin_username: Some("admin".into()),
                admin_password: Some("pw".into()),
                auto_detect_download_path: false,
                ..Default::default()
            },
            ..Default::default()
        };
        app.family.enabled = family_on;
        app.family.members_path = members.to_string_lossy().into_owned();
        let settings = Arc::new(SettingsStore::for_tests(app));
        settings.set_raw("Library:DownloadPath", Some(&music.to_string_lossy()));
        let http = crate::services::http_client_factory::default_client();
        let index = Arc::new(LibraryIndex::new(
            NavidromeIdentityService::new(settings.clone(), http.clone()),
            http,
            settings.clone(),
            Arc::new(Notify::new()),
        ));
        let family = Arc::new(FamilyStore::in_memory());
        family.set_owner("winters");
        family.upsert_member(member("alex", Preset::Member, 3));
        family.upsert_member(member("sam", Preset::Listener, 4));
        let library = Arc::new(RecordingLibrary::default());
        let stars = Arc::new(RecordingStars::default());
        let playlists = Arc::new(FamilyPlaylistStore::in_memory());
        let shared = music.to_string_lossy().into_owned();
        let pool = FamilyPool::with_timing(
            PoolParts {
                settings: settings.clone(),
                store: Arc::new(FamilyPoolStore::in_memory()),
                family: family.clone(),
                index: index.clone(),
                library: library.clone(),
                playlists: playlists.clone(),
                stars: stars.clone(),
                shared_root: Box::new(move || shared.clone()),
            },
            PoolTiming {
                poll: Duration::from_millis(20),
                max_wait: Duration::from_millis(600),
            },
        );
        pool.mark_pool_root().expect("the pool's marker file");
        let now = Arc::new(Mutex::new(Utc::now()));
        let clock = {
            let now = now.clone();
            Clock::new(move || *now.lock())
        };
        pool.set_clock(clock);
        let elapsed = Arc::new(Mutex::new(Duration::ZERO));
        pool.set_steady_clock({
            let elapsed = elapsed.clone();
            SteadyClock::new(move || *elapsed.lock())
        });
        World {
            dir,
            settings,
            family,
            index,
            library,
            stars,
            playlists,
            pool,
            now,
            elapsed,
        }
    }

    /// Lets `minutes` really pass: the wall clock and the steady clock both move on.
    pub fn later(&self, minutes: i64) {
        *self.now.lock() += TimeDelta::minutes(minutes);
        *self.elapsed.lock() += Duration::from_secs(60 * minutes.max(0) as u64);
    }

    /// Moves the wall clock alone `minutes` on (back, when negative), as a clock set by hand or
    /// by a time server does, while no real time passes.
    pub fn jump_wall(&self, minutes: i64) {
        *self.now.lock() += TimeDelta::minutes(minutes);
    }

    /// Points Octo at a Navidrome started after the world was made.
    pub fn point_at(&self, url: &str) {
        let mut settings = self.settings.current().as_ref().clone();
        settings.subsonic.url = Some(url.to_string());
        self.settings.set(settings);
    }

    pub fn members(&self) -> String {
        self.pool.members_root()
    }

    pub fn music(&self) -> String {
        self.dir.path().join("music").to_string_lossy().into_owned()
    }

    /// A real file in the pool at `relative`, with `bytes` of content.
    pub fn pool_file(&self, relative: &str, bytes: usize) -> String {
        let path = format!("{}/{relative}", self.pool.pool_root_path());
        write_file(&path, bytes);
        path
    }
}

pub(crate) fn write_file(path: &str, bytes: usize) {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).expect("folders");
    }
    std::fs::write(path, vec![7u8; bytes]).expect("written");
}

/// A Navidrome whose admin signs in and whose native song list is `songs`.
pub(crate) async fn navidrome_with_songs(songs: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": "jwt-1", "isAdmin": true, "username": "admin",
            "subsonicToken": "tok", "subsonicSalt": "salt"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/song"))
        .respond_with(ResponseTemplate::new(200).set_body_json(songs))
        .mount(&server)
        .await;
    server
}

/// One native song row: `id` at `relative` in library `library` (whose folder is `root`).
pub(crate) fn song_row(id: &str, library: i64, root: &str, relative: &str, size: u64) -> Value {
    json!({
        "id": id, "albumId": format!("al-{id}"), "artistId": "ar-1",
        "libraryId": library, "libraryPath": root, "path": relative,
        "explicitStatus": "", "size": size, "title": "Song"
    })
}

/// Waits up to two seconds for `check` to hold.
pub(crate) async fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    check()
}

/// A catalog that knows nothing, for requests that need no lookups (import lists).
pub(crate) struct NoCatalog;

#[async_trait]
impl RequestCatalog for NoCatalog {
    fn outside(&self, _: &str) -> Option<OutsideTarget> {
        None
    }

    async fn song(&self, _: &str, _: &str) -> Option<Song> {
        None
    }

    async fn album(&self, _: &str, _: &str) -> Option<octo_core::models::domain::Album> {
        None
    }

    async fn shared_copy(&self, _: &Song) -> Option<crate::services::library::OwnedCopy> {
        None
    }
}

/// A fetcher that fetches nothing and remembers the import lists handed to it.
#[derive(Default)]
pub(crate) struct ListFetcher {
    pub imports: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl RequestFetcher for ListFetcher {
    fn in_flight(&self, _: &str, _: &str) -> bool {
        false
    }

    async fn song(&self, _: &str, _: &str, _: &str, _: RequestQuality, _: Option<&Song>) -> Fetched {
        Fetched::Failed("Nothing is fetched here.".into())
    }

    async fn album(&self, _: &str, _: &str, _: &str) -> Vec<String> {
        Vec::new()
    }

    fn upgrade(&self, _: &str, _: &str, _: &RequestFacts) -> bool {
        false
    }

    fn upgrade_result(&self, _: &str) -> Option<Result<(), String>> {
        None
    }

    async fn import(&self, user: &str, list_id: &str) -> Result<String, String> {
        self.imports.lock().push((user.to_string(), list_id.to_string()));
        Ok("Queued.".into())
    }
}

impl World {
    /// The world's members' own libraries.
    pub fn personal(&self) -> Arc<PersonalLibrary> {
        Arc::new(PersonalLibrary::new(
            self.settings.clone(),
            self.family.clone(),
            self.index.clone(),
            self.pool.clone(),
        ))
    }

    /// The write gate over the world.
    pub fn gate(&self) -> Arc<LibraryWriteGate> {
        Arc::new(LibraryWriteGate::new(
            self.settings.clone(),
            self.family.clone(),
            self.personal(),
        ))
    }

    /// Requests over the world that look nothing up and fetch only import lists.
    pub fn list_requests(&self) -> (Arc<RequestService>, Arc<ListFetcher>) {
        let fetcher = Arc::new(ListFetcher::default());
        let service = RequestService::new(RequestParts {
            settings: self.settings.clone(),
            family: self.family.clone(),
            requests: Arc::new(super::request_store::RequestStore::in_memory()),
            personal: self.personal(),
            catalog: Arc::new(NoCatalog),
            fetcher: fetcher.clone(),
            stars: self.stars.clone(),
            events: Arc::new(FamilyEvents::new()),
            playlists: self.playlists.clone(),
        });
        (service, fetcher)
    }
}
