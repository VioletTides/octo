//! Where the library's artists are from, for the country mixes ("Spain Mix").
//!
//! [`ArtistCountryLookup`] asks MusicBrainz once per artist and keeps every answer in
//! `artist-countries.json`: `{"mbid:<id>" | "name:<key>": "ES" | ""}`, where "" means MusicBrainz
//! was asked and names no country. A failure is never kept, so the artist is asked again, after
//! a back-off held in memory that doubles from an hour up to a day. An artist with a
//! MusicBrainz id is asked by that id, and searched by name when the id cannot be asked; one
//! without is searched by its full credit, then by the credit's first artist, and a search only
//! counts when it finds exactly one artist of the same name scoring 90 or more.
//!
//! [`CountryLibrary`] is the one walk of the whole library the country mixes share, read as the
//! Navidrome admin at most every six hours, with each artist's MusicBrainz id from one
//! `getArtists`. A pass looks up at most CountryLookupsPerRefresh artists, those with the most
//! songs first, on the MusicBrainz client's background lane, so a download being verified never
//! waits behind it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use indexmap::IndexMap;
use octo_core::common::{SingleFlight, SongIdentity, dotnet};
use octo_core::json::dom::Node;
use octo_core::library::generated_playlist_service::count_countries;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::generated_playlist_service::{parameters, walk_library};
use crate::services::fingerprint::MusicBrainzClient;
use crate::services::state_file;
use crate::services::subsonic::SubsonicProxyService;

/// Where the answers are kept, beside the other state files.
pub const STORE_FILE: &str = "artist-countries.json";

/// How long one walk of the library serves every country mix.
pub const WALK_FOR: TimeDelta = TimeDelta::hours(6);

/// How long after a pass that could not walk the library the next one may start.
const RETRY_AFTER_FAILURE: TimeDelta = TimeDelta::minutes(5);

/// The longest one walk of the library may take.
const WALK_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// What Navidrome says of a song for the one account that asked.
const PERSONAL_FIELDS: &[&str] = &["starred", "userRating", "playCount", "played", "bookmarkPosition"];

/// How often a pass writes what it has learned so far.
const SAVE_EVERY: usize = 25;

/// How long a lookup that failed is left alone after its first failure; each failure in a row
/// doubles it, up to [`BACK_OFF_MAX`].
const BACK_OFF_FIRST: Duration = Duration::from_secs(60 * 60);
const BACK_OFF_MAX: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a lookup is left alone after `failures` failures in a row.
fn back_off(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    BACK_OFF_FIRST.saturating_mul(1 << doublings).min(BACK_OFF_MAX)
}

/// Where an artist is from, as a two-letter country, "" when the answer is that none is known,
/// None when it could not be asked.
#[async_trait::async_trait]
pub trait CountrySource: Send + Sync {
    async fn by_mbid(&self, mbid: &str) -> Option<String>;
    async fn by_name(&self, name: &str) -> Option<String>;
}

#[async_trait::async_trait]
impl CountrySource for MusicBrainzClient {
    async fn by_mbid(&self, mbid: &str) -> Option<String> {
        self.artist_country(mbid).await.ok().flatten()
    }

    async fn by_name(&self, name: &str) -> Option<String> {
        self.search_artist_country(name).await.ok().flatten()
    }
}

/// Every artist's country as far as it is known, and the asking for the rest.
pub struct ArtistCountryLookup {
    source: Arc<dyn CountrySource>,
    path: Option<PathBuf>,
    known: Mutex<BTreeMap<String, String>>,
    /// Lookups that failed: until when each is left alone, and how many failed in a row.
    failed: Mutex<HashMap<String, (Instant, u32)>>,
}

impl ArtistCountryLookup {
    /// Loads what `path` holds (None keeps the answers in memory only).
    pub fn new(source: Arc<dyn CountrySource>, path: Option<PathBuf>) -> Self {
        let lookup = ArtistCountryLookup {
            source,
            path: path.filter(|path| !dotnet::is_blank(&path.to_string_lossy())),
            known: Mutex::new(BTreeMap::new()),
            failed: Mutex::new(HashMap::new()),
        };
        lookup.load();
        lookup
    }

    /// The country already known for an artist credit, without asking: Some("") when the
    /// answer was that MusicBrainz names none, None when it is not known yet.
    pub fn known(&self, name: &str, mbid: Option<&str>) -> Option<String> {
        let known = self.known.lock();
        if let Some(mbid) = mbid.filter(|mbid| !dotnet::is_blank(mbid)) {
            if let Some(country) = known.get(&mbid_key(mbid)) {
                return Some(country.clone());
            }
            // An id that could not be asked may have been found by name; only a country counts,
            // so the id is still asked when the name search named none.
            let key = name_key(name)?;
            let full = known.get(&key).filter(|country| !country.is_empty());
            let primary = || {
                let key = name_key(&primary_of(name)?)?;
                known.get(&key).filter(|country| !country.is_empty())
            };
            return full.or_else(primary).cloned();
        }
        let Some(key) = name_key(name) else {
            return Some(String::new());
        };
        let full = known.get(&key).cloned()?;
        if !full.is_empty() {
            return Some(full);
        }
        match primary_of(name) {
            Some(primary) => known.get(&name_key(&primary)?).cloned(),
            None => Some(full),
        }
    }

    /// The country of an artist credit, asking MusicBrainz for what is not known yet while
    /// `asks_left` lasts. By MusicBrainz id when there is one; else, or when the id cannot be
    /// asked, the full credit by name, then its first artist. Some("") when MusicBrainz names
    /// none, None when it could not be asked (or the asks ran out).
    pub async fn country(&self, name: &str, mbid: Option<&str>, asks_left: &mut u32) -> Option<String> {
        if let Some(mbid) = mbid.filter(|mbid| !dotnet::is_blank(mbid)) {
            let key = mbid_key(mbid);
            let by_id = self
                .answer(key, asks_left, || self.source.by_mbid(mbid.trim()))
                .await;
            if by_id.is_some() {
                return by_id;
            }
        }
        let full = self.by_name(name, asks_left).await?;
        if !full.is_empty() {
            return Some(full);
        }
        match primary_of(name) {
            Some(primary) => self.by_name(&primary, asks_left).await,
            None => Some(full),
        }
    }

    async fn by_name(&self, name: &str, asks_left: &mut u32) -> Option<String> {
        let Some(key) = name_key(name) else {
            return Some(String::new());
        };
        self.answer(key, asks_left, || self.source.by_name(name.trim()))
            .await
    }

    /// What is kept under `key`, else one ask, kept when it was answered. A failed ask is left
    /// alone for its back-off, without spending an ask.
    async fn answer<F, Fut>(&self, key: String, asks_left: &mut u32, ask: F) -> Option<String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Option<String>>,
    {
        if let Some(known) = self.known.lock().get(&key) {
            return Some(known.clone());
        }
        if self
            .failed
            .lock()
            .get(&key)
            .is_some_and(|(until, _)| Instant::now() < *until)
        {
            return None;
        }
        if *asks_left == 0 {
            return None;
        }
        *asks_left -= 1;
        let Some(answer) = ask().await else {
            let mut failed = self.failed.lock();
            let failures = failed.get(&key).map_or(0, |(_, failures)| *failures) + 1;
            failed.insert(key, (Instant::now() + back_off(failures), failures));
            return None;
        };
        self.failed.lock().remove(&key);
        self.known.lock().insert(key, answer.clone());
        Some(answer)
    }

    /// How many answers are kept.
    pub fn len(&self) -> usize {
        self.known.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Writes the answers to the file.
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        let text = match serde_json::to_string_pretty(&*self.known.lock()) {
            Ok(text) => text,
            Err(error) => {
                warn!("Artist countries could not be written: {error}");
                return;
            }
        };
        if let Err(error) = state_file::save_atomic(path, &text) {
            warn!("Artist countries could not be written: {error}");
        }
    }

    fn load(&self) {
        let Some(path) = self.path.as_ref().filter(|path| path.is_file()) else {
            return;
        };
        let read = state_file::read_all_text(path)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                serde_json::from_str::<BTreeMap<String, String>>(&text).map_err(|error| error.to_string())
            });
        match read {
            Ok(known) => *self.known.lock() = known,
            // Only answers MusicBrainz will give again: a bad file starts over.
            Err(message) => {
                warn!("Artist countries could not be read ({message}); they will be looked up again")
            }
        }
    }
}

fn mbid_key(mbid: &str) -> String {
    format!("mbid:{}", dotnet::to_lower_invariant(mbid.trim()))
}

/// None for a name with nothing to look up by.
fn name_key(name: &str) -> Option<String> {
    let key = SongIdentity::key(name);
    (!key.is_empty()).then(|| format!("name:{key}"))
}

/// The credit's first artist, when that is another name than the whole credit.
fn primary_of(name: &str) -> Option<String> {
    let primary = SongIdentity::primary_artist(name);
    let key = SongIdentity::key(&primary);
    (!key.is_empty() && key != SongIdentity::key(name)).then_some(primary)
}

/// One artist credit of the library: what it is looked up by, and how many songs carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryArtist {
    pub name: String,
    pub mbid: Option<String>,
    pub songs: usize,
}

/// The whole library, each song with the artist credit it is filed under.
#[derive(Debug, Default)]
pub struct WalkedLibrary {
    pub songs: Vec<Node>,
    pub artists: Vec<LibraryArtist>,
    /// The index into `artists` of each song.
    pub artist_of_song: Vec<usize>,
}

impl WalkedLibrary {
    /// Groups the songs by artist credit: Navidrome's artist id, else the credit's name. An
    /// artist's MusicBrainz id comes from `mbids`, by artist id. The walk is the admin's, so
    /// the admin's own marks and plays are taken off every song: each listener's are put on
    /// when a mix is served.
    pub fn new(mut songs: Vec<Node>, mbids: &HashMap<String, String>) -> Self {
        for song in &mut songs {
            if let Some(fields) = song.as_object_mut() {
                for personal in PERSONAL_FIELDS {
                    fields.shift_remove(*personal);
                }
            }
        }
        let mut artists: Vec<LibraryArtist> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut artist_of_song = Vec::with_capacity(songs.len());
        for song in &songs {
            let name = text(song, "artist").unwrap_or_default().trim().to_string();
            let id = text(song, "artistId").filter(|id| !id.is_empty());
            let key = match id {
                Some(id) => format!("id:{id}"),
                None => format!("name:{}", SongIdentity::key(&name)),
            };
            let at = *index.entry(key).or_insert_with(|| {
                artists.push(LibraryArtist {
                    name: name.clone(),
                    mbid: id
                        .and_then(|id| mbids.get(id))
                        .filter(|mbid| !dotnet::is_blank(mbid))
                        .cloned(),
                    songs: 0,
                });
                artists.len() - 1
            });
            artists[at].songs += 1;
            artist_of_song.push(at);
        }
        WalkedLibrary {
            songs,
            artists,
            artist_of_song,
        }
    }

    /// Each artist's country as far as `lookup` knows it.
    fn countries(&self, lookup: &ArtistCountryLookup) -> Vec<Option<String>> {
        self.artists
            .iter()
            .map(|artist| lookup.known(&artist.name, artist.mbid.as_deref()))
            .collect()
    }

    /// Songs per country mix, by what `lookup` knows; only `allowed` countries when it names any.
    pub fn counts(&self, lookup: &ArtistCountryLookup, allowed: &[&str]) -> IndexMap<String, i32> {
        let countries = self.countries(lookup);
        count_countries(
            self.artist_of_song
                .iter()
                .map(|artist| countries[*artist].as_deref()),
            allowed,
        )
    }

    /// Songs per country mix among the songs with an id in `ids` (one listener's library), by
    /// what `lookup` knows; only `allowed` countries when it names any.
    pub fn counts_among(
        &self,
        lookup: &ArtistCountryLookup,
        allowed: &[&str],
        ids: &HashSet<String>,
    ) -> IndexMap<String, i32> {
        let countries = self.countries(lookup);
        count_countries(
            self.songs
                .iter()
                .zip(&self.artist_of_song)
                .filter(|(song, _)| text(song, "id").is_some_and(|id| ids.contains(id)))
                .map(|(_, artist)| countries[*artist].as_deref()),
            allowed,
        )
    }

    /// The ids of the songs whose artist is from `code`.
    pub fn ids_from(&self, lookup: &ArtistCountryLookup, code: &str) -> HashSet<String> {
        let countries = self.countries(lookup);
        self.songs
            .iter()
            .zip(&self.artist_of_song)
            .filter(|(_, artist)| countries[**artist].as_deref() == Some(code))
            .filter_map(|(song, _)| text(song, "id").map(str::to_string))
            .collect()
    }

    /// The songs whose artist is from `code`.
    pub fn songs_from(&self, lookup: &ArtistCountryLookup, code: &str) -> Vec<Node> {
        let countries = self.countries(lookup);
        self.songs
            .iter()
            .zip(&self.artist_of_song)
            .filter(|(_, artist)| countries[**artist].as_deref() == Some(code))
            .map(|(song, _)| song.clone())
            .collect()
    }
}

fn text<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    node.get(name).and_then(Node::as_str)
}

/// The Navidrome admin's Subsonic parameters (u, t, s, v, c), or None before Octo has them.
pub type AdminAuth = Arc<dyn Fn() -> Option<IndexMap<String, String>> + Send + Sync>;

#[derive(Default)]
struct PassState {
    running: bool,
    started: Option<DateTime<Utc>>,
    failed: bool,
}

/// The library walk every country mix shares, and the passes that learn where its artists are
/// from.
pub struct CountryLibrary {
    lookup: Arc<ArtistCountryLookup>,
    proxy: SubsonicProxyService,
    admin_auth: AdminAuth,
    walked: Mutex<Option<(Arc<WalkedLibrary>, DateTime<Utc>)>>,
    walks: SingleFlight<(), Option<Arc<WalkedLibrary>>>,
    pass: Mutex<PassState>,
}

impl CountryLibrary {
    pub fn new(lookup: Arc<ArtistCountryLookup>, proxy: SubsonicProxyService, admin_auth: AdminAuth) -> Self {
        CountryLibrary {
            lookup,
            proxy,
            admin_auth,
            walked: Mutex::new(None),
            walks: SingleFlight::new(),
            pass: Mutex::new(PassState::default()),
        }
    }

    pub fn lookup(&self) -> &ArtistCountryLookup {
        &self.lookup
    }

    /// The library as last walked, without walking it.
    pub fn last_walk(&self) -> Option<Arc<WalkedLibrary>> {
        self.walked.lock().as_ref().map(|(walked, _)| Arc::clone(walked))
    }

    /// The library, walked again when the last walk is [`WALK_FOR`] old or there is none. A
    /// walk that fails leaves the last one in place; None when there has never been one.
    pub async fn walk(self: &Arc<Self>) -> Option<Arc<WalkedLibrary>> {
        if let Some((walked, at)) = self.walked.lock().as_ref()
            && Utc::now() - *at < WALK_FOR
        {
            return Some(Arc::clone(walked));
        }
        let library = Arc::clone(self);
        let fresh = self
            .walks
            .run(
                (),
                move |token| async move { Ok(library.walk_now(&token).await) },
                WALK_DEADLINE,
            )
            .await
            .ok()
            .flatten();
        fresh.or_else(|| self.last_walk())
    }

    async fn walk_now(&self, token: &CancellationToken) -> Option<Arc<WalkedLibrary>> {
        let Some(auth) = (self.admin_auth)() else {
            debug!("Country mixes wait for Octo to have the Navidrome admin sign-in");
            return None;
        };
        let mbids = self.artist_mbids(&auth).await?;
        let songs = walk_library(&self.proxy, &auth, token).await.ok().flatten()?;
        let walked = Arc::new(WalkedLibrary::new(songs, &mbids));
        *self.walked.lock() = Some((Arc::clone(&walked), Utc::now()));
        Some(walked)
    }

    /// Each artist's MusicBrainz id by Navidrome artist id, from one getArtists; artists
    /// Navidrome has no id for are left out. None when Navidrome did not answer.
    async fn artist_mbids(&self, auth: &IndexMap<String, String>) -> Option<HashMap<String, String>> {
        let result = self
            .proxy
            .relay_safe("rest/getArtists", parameters(auth, &[]))
            .await?;
        let root = Node::parse(&String::from_utf8_lossy(&result.body)).ok()?;
        let response = root.get("subsonic-response")?;
        if response.get("status").and_then(Node::as_str) != Some("ok") {
            return None;
        }
        let mut mbids = HashMap::new();
        let indexes = match response.get("artists").and_then(|artists| artists.get("index")) {
            Some(Node::Array(indexes)) => indexes.as_slice(),
            _ => &[],
        };
        for index in indexes {
            let Some(Node::Array(artists)) = index.get("artist") else {
                continue;
            };
            for artist in artists {
                if let (Some(id), Some(mbid)) = (text(artist, "id"), text(artist, "musicBrainzId"))
                    && !id.is_empty()
                    && !dotnet::is_blank(mbid)
                {
                    mbids.insert(id.to_string(), mbid.trim().to_string());
                }
            }
        }
        Some(mbids)
    }

    /// Whether a pass is due: none running, and none started in the last [`WALK_FOR`] (five
    /// minutes after one that could not walk the library).
    pub fn pass_due(&self) -> bool {
        let pass = self.pass.lock();
        if pass.running {
            return false;
        }
        let wait = if pass.failed {
            RETRY_AFTER_FAILURE
        } else {
            WALK_FOR
        };
        pass.started.is_none_or(|started| Utc::now() - started >= wait)
    }

    /// One pass: the library walked when due, then up to `budget` lookups of artists whose
    /// country is not known yet, those with the most songs first. Whether it learned anything
    /// a mix could change by. Only one pass runs at a time; a second call while one runs does
    /// nothing.
    pub async fn run_pass(self: &Arc<Self>, budget: u32) -> bool {
        {
            let mut pass = self.pass.lock();
            if pass.running {
                return false;
            }
            pass.running = true;
            pass.started = Some(Utc::now());
            pass.failed = false;
        }
        let learned = self.pass_now(budget).await;
        let mut pass = self.pass.lock();
        pass.running = false;
        pass.failed = learned.is_none();
        learned.unwrap_or(false)
    }

    async fn pass_now(self: &Arc<Self>, budget: u32) -> Option<bool> {
        let before = self.last_walk();
        let walked = self.walk().await?;
        let mut learned = before.is_none_or(|before| !Arc::ptr_eq(&before, &walked));
        let mut order: Vec<&LibraryArtist> = walked.artists.iter().collect();
        order.sort_by_key(|artist| std::cmp::Reverse(artist.songs));
        let mut asks_left = budget;
        let mut asked = 0usize;
        for artist in order {
            if asks_left == 0 {
                break;
            }
            if self.lookup.known(&artist.name, artist.mbid.as_deref()).is_some() {
                continue;
            }
            let left = asks_left;
            let answer = self
                .lookup
                .country(&artist.name, artist.mbid.as_deref(), &mut asks_left)
                .await;
            if asks_left < left {
                asked += 1;
                if asked.is_multiple_of(SAVE_EVERY) {
                    self.lookup.save();
                }
            }
            learned |= answer.is_some_and(|country| !country.is_empty());
        }
        self.lookup.save();
        let known = walked
            .countries(&self.lookup)
            .iter()
            .filter(|country| country.is_some())
            .count();
        info!(
            "Country mixes: {asked} artists looked up, where {known} of {} artists are from is known",
            walked.artists.len()
        );
        Some(learned)
    }
}

#[cfg(test)]
#[path = "artist_country_tests.rs"]
mod tests;
