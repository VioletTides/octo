//! The MusicBrainz client, its HTTP half: the one-a-second gate, the cache and the "musicbrainz"
//! client (base address https://musicbrainz.org/ws/2/, a 10 s timeout, Octo's User-Agent). The
//! query builders and the readers are in `octo_core::fingerprint::music_brainz_client`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use octo_core::common::{SongIdentity, dotnet, octo_user_agent};
use octo_core::fingerprint::music_brainz_client as pure;
use octo_core::json::element::ElementError;
use octo_core::tagging::{ReleaseDetails, ReleaseLookup};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::time::Instant;
use tracing::debug;
use url::Url;

use crate::services::framework::http::{client_builder, parse_json, resolve};
use crate::services::framework::{HttpAnswer, MemoryCache};

/// The client's errors, which callers catch.
#[derive(Debug, thiserror::Error)]
pub enum MusicBrainzError {
    /// The client's timeout elapsed. It reaches the caller rather than reading as "nothing to
    /// say".
    #[error("No answer came within 10 seconds, so Octo stopped waiting.")]
    TimedOut,
    /// The answer could not be read: a field of the wrong kind.
    #[error(transparent)]
    Malformed(#[from] ElementError),
}

#[derive(Clone)]
enum Cached {
    Release(Arc<ReleaseDetails>),
    Json(Value),
    Text(String),
}

/// One question for MusicBrainz, asked only when a person keeps a track AcoustID had never heard
/// of: which recording is this? That is the MusicBrainz id a confirmed fingerprint needs before
/// AcoustID will take it (#47). And one more, asked only while verifying a download whose
/// fingerprint named a recording that reads differently from the request: which ISRCs does
/// that recording carry?
///
/// MusicBrainz allows about one request a second and wants a User-Agent naming the application,
/// and it holds near-duplicate recordings (two "Teardrop" by Massive Attack, 27 ms apart), so an
/// answer is only returned when exactly one recording fits. Anything else is a guess, and a
/// guess is never submitted with someone's name on it.
pub struct MusicBrainzClient {
    client: reqwest::Client,
    base: Url,
    /// One call at a time, and when the last one was made: held across the wait and the call. The
    /// background lane only ever tries it (see [`Self::get_background`]).
    gate: tokio::sync::Mutex<Option<LastCall>>,
    /// How long the background lane waits when the gate is busy.
    background_backoff: Duration,
    /// Release lookups are remembered a day, since an album's tracks ask for the same
    /// release one by one; searches six hours. Every entry counts as one, so the limit is a count.
    cache: MemoryCache<Cached>,
    /// Exact-name artist answers by lowercase name, kept for the life of the process.
    artists: Mutex<HashMap<String, Vec<pure::MusicBrainzArtist>>>,
    /// Recording credits by lowercase recording id, kept for the life of the process.
    credits: Mutex<HashMap<String, Vec<pure::CreditPart>>>,
}

impl MusicBrainzClient {
    pub const CLIENT_NAME: &'static str = "musicbrainz";
    pub const BASE_ADDRESS: &'static str = "https://musicbrainz.org/ws/2/";
    const TIMEOUT: Duration = Duration::from_secs(10);
    const MINIMUM_GAP: Duration = Duration::from_millis(1100);
    const RELEASE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
    const SEARCH_TTL: Duration = Duration::from_secs(6 * 60 * 60);
    const ARTIST_TTL: Duration = Duration::from_secs(24 * 60 * 60);
    const BACKGROUND_BACKOFF: Duration = Duration::from_secs(5);

    pub fn new() -> Self {
        Self::with_base_url(Url::parse(Self::BASE_ADDRESS).expect("the MusicBrainz base address parses"))
    }

    /// A client for another base address: a test's mock server.
    pub fn with_base_url(base: Url) -> Self {
        let client = client_builder()
            .timeout(Self::TIMEOUT)
            .user_agent(octo_user_agent::value())
            .build()
            .expect("the MusicBrainz HTTP client builds");
        Self {
            client,
            base,
            gate: tokio::sync::Mutex::new(None),
            background_backoff: Self::BACKGROUND_BACKOFF,
            cache: MemoryCache::new(2000),
            artists: Mutex::new(HashMap::new()),
            credits: Mutex::new(HashMap::new()),
        }
    }

    /// One release in full: its label and catalog number, barcode, status, country, date, its
    /// group's kind and first release date, every track's position and id, and the genres people
    /// voted on. Remembered a day, since an album's tracks ask one by one.
    pub async fn lookup_release(
        &self,
        release_id: &str,
    ) -> Result<Option<Arc<ReleaseDetails>>, MusicBrainzError> {
        if dotnet::is_blank(release_id) {
            return Ok(None);
        }
        let key = format!("release|{}", dotnet::to_lower_invariant(release_id.trim()));
        if let Some(Cached::Release(cached)) = self.cache.get(&key) {
            return Ok(Some(cached));
        }

        let url = format!(
            "release/{}?inc=labels+release-groups+artist-credits+recordings+isrcs+genres&fmt=json",
            dotnet::escape_data_string(release_id.trim())
        );
        let Some(doc) = self.get(&url, "release lookup").await? else {
            return Ok(None);
        };
        let details = ReleaseDetails::parse(&doc).map(Arc::new);
        if let Some(details) = &details {
            self.cache
                .set(key, Cached::Release(Arc::clone(details)), 1, Self::RELEASE_TTL);
        }
        Ok(details)
    }

    /// Recordings by name and length, for a download the fingerprint service could not name. Up
    /// to 25, each with its releases and their groups.
    pub async fn search_recordings(
        &self,
        artist: &str,
        title: &str,
        duration_seconds: i32,
    ) -> Result<Option<Value>, MusicBrainzError> {
        if dotnet::is_blank(artist) && dotnet::is_blank(title) {
            return Ok(None);
        }
        let key = format!(
            "search|{}|{duration_seconds}",
            SongIdentity::match_key(artist, title)
        );
        if let Some(Cached::Json(cached)) = self.cache.get(&key) {
            return Ok(Some(cached));
        }

        let url = pure::build_recording_search_url(artist, title, duration_seconds);
        let Some(doc) = self.get(&url, "recording search").await? else {
            return Ok(None);
        };
        self.cache
            .set(key, Cached::Json(doc.clone()), 1, Self::SEARCH_TTL);
        Ok(Some(doc))
    }

    /// The recordings that carry one code, with their releases.
    pub async fn lookup_isrc(&self, isrc: &str) -> Result<Option<Value>, MusicBrainzError> {
        let Some(code) = SongIdentity::normalize_isrc(isrc) else {
            return Ok(None);
        };
        let key = format!("isrc|{code}");
        if let Some(Cached::Json(cached)) = self.cache.get(&key) {
            return Ok(Some(cached));
        }

        let url = format!("isrc/{code}?inc=artist-credits+releases+release-groups+media&fmt=json");
        let Some(doc) = self.get(&url, "code lookup").await? else {
            return Ok(None);
        };
        self.cache
            .set(key, Cached::Json(doc.clone()), 1, Self::SEARCH_TTL);
        Ok(Some(doc))
    }

    /// The one recording that is this song, this version, by this artist, at this length.
    pub async fn find_recording(
        &self,
        artist: &str,
        title: &str,
        duration_seconds: i32,
    ) -> Result<Option<String>, MusicBrainzError> {
        let Some(url) = pure::find_recording_url(artist, title, duration_seconds) else {
            return Ok(None);
        };
        match self.get(&url, "recording search").await? {
            Some(doc) => Ok(pure::pick(&doc, artist, title, duration_seconds)?),
            None => Ok(None),
        }
    }

    /// The ISRCs MusicBrainz lists for one recording: empty when it lists none, None when it
    /// could not be asked. One lookup by id with inc=isrcs, inside the same one-a-second budget
    /// as the search above.
    pub async fn fetch_isrcs(&self, recording_id: &str) -> Result<Option<Vec<String>>, MusicBrainzError> {
        if dotnet::is_blank(recording_id) {
            return Ok(None);
        }
        let url = format!(
            "recording/{}?inc=isrcs&fmt=json",
            dotnet::escape_data_string(recording_id)
        );
        match self.get(&url, "ISRC lookup").await? {
            Some(doc) => Ok(Some(pure::parse_isrcs(&doc)?)),
            None => Ok(None),
        }
    }

    /// The release group of the oldest official studio album a song appears on, or of a
    /// soundtrack when no studio album has it. None when MusicBrainz knows neither.
    pub async fn find_studio_album(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Option<String>, MusicBrainzError> {
        let Some((url, plain_title)) = pure::studio_album_search(artist, title) else {
            return Ok(None);
        };
        match self.get(&url, "studio album search").await? {
            Some(doc) => Ok(pure::pick_studio_album(&doc, &plain_title)?),
            None => Ok(None),
        }
    }

    /// Where the artist with this MusicBrainz id is from: a two-letter country, empty when
    /// MusicBrainz names none, None when it could not be asked. On the background lane;
    /// answers are remembered a day.
    pub async fn artist_country(&self, mbid: &str) -> Result<Option<String>, MusicBrainzError> {
        let Some(url) = pure::artist_lookup_url(mbid) else {
            return Ok(None);
        };
        let key = format!("artist|{}", dotnet::to_lower_invariant(mbid.trim()));
        if let Some(Cached::Text(cached)) = self.cache.get(&key) {
            return Ok(Some(cached));
        }
        let Some(doc) = self.get_background(&url, "artist lookup").await? else {
            return Ok(None);
        };
        let country = pure::artist_country(&doc);
        self.cache
            .set(key, Cached::Text(country.clone()), 1, Self::ARTIST_TTL);
        Ok(Some(country))
    }

    /// Where the artist of this name is from, by a name search that takes only the same name
    /// (`pure::searched_artist_country`): a two-letter country, empty when no result is that
    /// artist or it names no country, None when MusicBrainz could not be asked. On the
    /// background lane; answers are remembered a day.
    pub async fn search_artist_country(&self, name: &str) -> Result<Option<String>, MusicBrainzError> {
        let Some(url) = pure::artist_search_url(name) else {
            return Ok(None);
        };
        let key = format!("artist-search|{}", SongIdentity::key(name));
        if let Some(Cached::Text(cached)) = self.cache.get(&key) {
            return Ok(Some(cached));
        }
        let Some(doc) = self.get_background(&url, "artist search").await? else {
            return Ok(None);
        };
        let country = pure::searched_artist_country(&doc, name);
        self.cache
            .set(key, Cached::Text(country.clone()), 1, Self::ARTIST_TTL);
        Ok(Some(country))
    }

    /// The MusicBrainz artists named exactly `name`, by name or alias, case and accents aside:
    /// empty when there is none, None when MusicBrainz could not be asked. Answers are kept for
    /// the life of the process, so a second preview asks nothing it asked before; a failed
    /// request is not kept, so it is "could not ask", never "no such artist".
    pub async fn find_artists_named(
        &self,
        name: &str,
    ) -> Result<Option<Vec<pure::MusicBrainzArtist>>, MusicBrainzError> {
        let wanted = name.trim();
        if wanted.is_empty() {
            return Ok(Some(Vec::new()));
        }
        if let Some(known) = self.known_artists_named(wanted) {
            return Ok(Some(known));
        }
        let Some(doc) = self
            .get(&pure::artists_named_url(wanted), "artist search")
            .await?
        else {
            return Ok(None);
        };
        let found = pure::parse_artists_named(&doc, wanted);
        let mut artists = self.artists.lock();
        make_room(&mut artists, 1, KNOWN_MAX);
        artists.insert(wanted.to_lowercase(), found.clone());
        Ok(Some(found))
    }

    /// The answer [`Self::find_artists_named`] already has for `name`, without asking.
    pub fn known_artists_named(&self, name: &str) -> Option<Vec<pure::MusicBrainzArtist>> {
        self.artists.lock().get(&name.trim().to_lowercase()).cloned()
    }

    /// The artist credits of these recordings, asked 100 to a search: every id asked gets an
    /// answer, empty when MusicBrainz does not know the recording. None when it could not be
    /// asked. Answers are kept for the life of the process.
    pub async fn recording_credits(
        &self,
        ids: &[String],
    ) -> Result<Option<HashMap<String, Vec<pure::CreditPart>>>, MusicBrainzError> {
        let wanted: Vec<String> = ids
            .iter()
            .map(|id| id.trim().to_lowercase())
            .filter(|id| pure::is_mbid(id) && self.known_credits(id).is_none())
            .collect();
        // Room for every answer this call adds, so none of them is dropped before it is read.
        make_room(&mut self.credits.lock(), wanted.len(), KNOWN_MAX);
        for batch in wanted.chunks(100) {
            let Some(url) = pure::recording_credits_url(batch) else {
                continue;
            };
            let Some(doc) = self.get(&url, "recording credits").await? else {
                return Ok(None);
            };
            let found = pure::parse_recording_credits(&doc);
            let mut credits = self.credits.lock();
            for id in batch {
                credits.insert(id.clone(), found.get(id).cloned().unwrap_or_default());
            }
        }
        let credits = self.credits.lock();
        Ok(Some(
            ids.iter()
                .map(|id| id.trim().to_lowercase())
                .filter_map(|id| credits.get(&id).map(|parts| (id, parts.clone())))
                .collect(),
        ))
    }

    /// The credit [`Self::recording_credits`] already has for one recording, without asking.
    pub fn known_credits(&self, id: &str) -> Option<Vec<pure::CreditPart>> {
        self.credits.lock().get(&id.trim().to_lowercase()).cloned()
    }

    /// One request, spaced at least [`Self::MINIMUM_GAP`] after the last. None on any
    /// failure, which every caller reads as "MusicBrainz had nothing to say"; the timeout is
    /// the one error that reaches the caller.
    async fn get(&self, relative: &str, what: &str) -> Result<Option<Value>, MusicBrainzError> {
        let mut last_call = self.gate.lock().await;
        if let Some(last) = *last_call {
            let due = last.at + Self::MINIMUM_GAP;
            if due > Instant::now() {
                tokio::time::sleep_until(due).await;
            }
        }
        *last_call = Some(LastCall {
            at: Instant::now(),
            background: false,
        });
        self.send(relative, what).await
    }

    /// One request on the background lane, for work nobody is waiting on. It never queues on
    /// the gate: while the gate is held, or a call that is not its own went out less than
    /// [`Self::MINIMUM_GAP`] ago, it steps back for [`Self::BACKGROUND_BACKOFF`] and tries
    /// again, so a download being verified goes first. It takes its turn and lets go of the gate
    /// before it sends, so a caller arriving meanwhile waits one gap at most.
    async fn get_background(&self, relative: &str, what: &str) -> Result<Option<Value>, MusicBrainzError> {
        loop {
            let wait = match self.gate.try_lock() {
                Err(_) => self.background_backoff,
                Ok(mut last_call) => {
                    let now = Instant::now();
                    match *last_call {
                        Some(last) if last.at + Self::MINIMUM_GAP > now => {
                            if last.background {
                                last.at + Self::MINIMUM_GAP - now
                            } else {
                                self.background_backoff
                            }
                        }
                        _ => {
                            *last_call = Some(LastCall {
                                at: now,
                                background: true,
                            });
                            break;
                        }
                    }
                }
            };
            tokio::time::sleep(wait).await;
        }
        self.send(relative, what).await
    }

    /// The request itself, once its turn has come.
    async fn send(&self, relative: &str, what: &str) -> Result<Option<Value>, MusicBrainzError> {
        let attempt = async {
            let url = resolve(&self.base, relative)?;
            let response = self.client.get(url).send().await?;
            let answer = HttpAnswer::read(response).await?;
            if !answer.is_success() {
                return anyhow::Ok(None);
            }
            Ok(Some(parse_json(&answer.body)?))
        };
        match attempt.await {
            Ok(doc) => Ok(doc),
            Err(e)
                if e.downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout) =>
            {
                Err(MusicBrainzError::TimedOut)
            }
            Err(e) => {
                debug!("musicbrainz {what} failed: {e}");
                Ok(None)
            }
        }
    }
}

/// When the last request went out, and whether the background lane sent it.
#[derive(Debug, Clone, Copy)]
struct LastCall {
    at: Instant,
    background: bool,
}

/// The music database calls the release identifier makes. A timeout is the error the
/// identifier sees; every other failure is already None.
#[async_trait::async_trait]
impl ReleaseLookup for MusicBrainzClient {
    async fn lookup_release(&self, release_id: &str) -> anyhow::Result<Option<ReleaseDetails>> {
        Ok(MusicBrainzClient::lookup_release(self, release_id)
            .await?
            .map(|details| (*details).clone()))
    }

    async fn search_recordings(
        &self,
        artist: &str,
        title: &str,
        duration_seconds: i32,
    ) -> anyhow::Result<Option<Value>> {
        Ok(MusicBrainzClient::search_recordings(self, artist, title, duration_seconds).await?)
    }

    async fn lookup_isrc(&self, isrc: &str) -> anyhow::Result<Option<Value>> {
        Ok(MusicBrainzClient::lookup_isrc(self, isrc).await?)
    }
}

impl Default for MusicBrainzClient {
    fn default() -> Self {
        Self::new()
    }
}

/// The most artist searches, and the most recording credits, kept in memory.
const KNOWN_MAX: usize = 10_000;

/// Drops every kept answer when `adding` more would pass `max`: answers MusicBrainz gives again,
/// so the map stays bounded however long the process runs.
fn make_room<K, V>(map: &mut HashMap<K, V>, adding: usize, max: usize) {
    if map.len() + adding > max {
        map.clear();
    }
}

#[cfg(test)]
#[path = "music_brainz_client_tests.rs"]
mod tests;
