//! Reads Apple Music's public chart, New and room pages (`octo_core::charts::apple_pages` parses
//! them). Every request waits its turn at one gate, one a second, and a page Apple says is not
//! there is not asked for again for an hour, so however many listeners and apps ask, Apple sees
//! a few requests an hour per list.

use std::sync::Arc;
use std::time::Duration;

use octo_core::charts::apple_pages::{
    ApplePage, ApplePageError, RoomKind, SongRoom, is_country_code, parse_countries, parse_song_rooms,
    parse_songs, room_for,
};
use octo_core::charts::chart_settings::{APPLE_COUNTRIES, OVERALL};
use parking_lot::Mutex;
use tokio::time::Instant;

use crate::services::framework::http::client_builder;
use crate::services::framework::memory_cache::MemoryCache;

/// The most of a page that is read. A chart page is about 250 KB, the New page about 2.3 MB.
pub const MAX_BODY: usize = 8 * 1024 * 1024;
/// How far apart two requests to Apple are.
pub const INTERVAL: Duration = Duration::from_secs(1);
/// How long the country Apple sees the server in is kept.
const COUNTRY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long the New page's rooms are kept: a new week's room is noticed within the hour.
const ROOMS_TTL: Duration = Duration::from_secs(60 * 60);
/// How long a page Apple said is not there is not asked for again.
const MISSING_TTL: Duration = Duration::from_secs(60 * 60);
/// How long a failed New page is not asked for again.
const FAILED_TTL: Duration = Duration::from_secs(10 * 60);
/// How long Apple is not asked again after it could not be reached at all (no connection, or no
/// answer in time): a server that cannot reach Apple must not wait out the timeout on every
/// request that asks for its country or a chart.
const UNREACHABLE_TTL: Duration = Duration::from_secs(5 * 60);
/// How long a fetched page is kept here: long enough that finding the country, which reads the
/// overall chart, does not read it a second time a moment later.
const PAGE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    /// Apple answered 404 or 500: the country has no such chart, or the room is gone.
    NoSuchPage,
    /// The New page names no room of that kind this week.
    NoRoom,
    /// The page passed [`MAX_BODY`].
    TooBig,
    /// The page no longer has the shape Octo reads.
    ChangedShape,
    /// Apple could not be reached or answered something else.
    Unavailable(String),
}

impl PageError {
    /// The error in a person's words, for the dashboard.
    pub fn describe(&self) -> String {
        match self {
            PageError::NoSuchPage => "Apple has no such list in this country".into(),
            PageError::NoRoom => "Apple's New page has no such list this week".into(),
            PageError::TooBig => "Apple's page was too big to read".into(),
            PageError::ChangedShape => "Apple's page changed shape".into(),
            PageError::Unavailable(why) => format!("Apple Music could not be reached ({why})"),
        }
    }
}

impl From<ApplePageError> for PageError {
    fn from(_: ApplePageError) -> Self {
        PageError::ChangedShape
    }
}

/// One request at a time, [`INTERVAL`] apart, for every page this reads.
pub struct Gate {
    interval: Duration,
    next: tokio::sync::Mutex<Option<Instant>>,
}

impl Gate {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: tokio::sync::Mutex::new(None),
        }
    }

    /// Waits for this request's turn. Waiters queue on the lock, so they go one after another.
    pub async fn wait(&self) {
        self.wait_unless(|| false).await;
    }

    /// Waits for this request's turn unless, when its place in the queue comes up, `answered`
    /// says it needs none (a request before it already learned the answer). True when the
    /// request may go.
    pub async fn wait_unless(&self, answered: impl Fn() -> bool) -> bool {
        let mut next = self.next.lock().await;
        if answered() {
            return false;
        }
        if let Some(at) = *next
            && at > Instant::now()
        {
            tokio::time::sleep_until(at).await;
        }
        *next = Some(Instant::now() + self.interval);
        true
    }
}

pub struct AppleMusicPages {
    client: reqwest::Client,
    base: String,
    gate: Gate,
    country: MemoryCache<String>,
    rooms: MemoryCache<Arc<Vec<SongRoom>>>,
    pages: MemoryCache<Arc<ApplePage>>,
    missing: MemoryCache<PageError>,
    countries: Mutex<Vec<String>>,
}

impl AppleMusicPages {
    pub const BASE: &'static str = "https://music.apple.com";

    pub fn new() -> Self {
        Self::with_base_url(Self::BASE, INTERVAL)
    }

    /// Pages from another host (a test's mock server), with another gap between requests.
    pub fn with_base_url(base: &str, interval: Duration) -> Self {
        let client = client_builder()
            .gzip(true)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(octo_core::common::octo_user_agent::value())
            .build()
            .expect("the Apple Music client builds");
        Self {
            client,
            base: base.trim_end_matches('/').to_string(),
            gate: Gate::new(interval),
            country: MemoryCache::new(16),
            rooms: MemoryCache::new(256),
            pages: MemoryCache::new(64),
            missing: MemoryCache::new(1024),
            countries: Mutex::new(Vec::new()),
        }
    }

    /// A chart: the overall one (`34`) or a genre's, for a country, or for the server's own when
    /// `country` is None (Apple sends a link without one on to the country it sees the server in).
    pub async fn chart(&self, country: Option<&str>, genre: &str) -> Result<Arc<ApplePage>, PageError> {
        let country = match country {
            Some(code) if is_country_code(code) => Some(code),
            Some(_) => return Err(PageError::Unavailable("not a country".into())),
            None => None,
        };
        if let Some(code) = country
            && let Some(page) = self.pages.get(&page_key(code, genre))
        {
            return Ok(page);
        }
        let mut url = match country {
            Some(code) => format!("{}/{code}/new/top-charts/songs", self.base),
            None => format!("{}/new/top-charts/songs", self.base),
        };
        if genre != OVERALL {
            url.push_str("/?genreId=");
            url.push_str(genre);
        }
        let html = self.fetch(&url).await?;
        if genre == OVERALL {
            let countries = parse_countries(&html);
            if !countries.is_empty() {
                *self.countries.lock() = countries;
            }
        }
        let page = Arc::new(parse_songs(&html)?);
        if is_country_code(&page.country) {
            if country.is_none() {
                self.country.set("country", page.country.clone(), 1, COUNTRY_TTL);
            }
            self.pages
                .set(page_key(&page.country, genre), page.clone(), 1, PAGE_TTL);
        }
        Ok(page)
    }

    /// The country Apple sees the server in, or None while that cannot be learned.
    pub async fn detected_country(&self) -> Option<String> {
        if let Some(code) = self.known_country() {
            return Some(code);
        }
        let page = self.chart(None, OVERALL).await.ok()?;
        is_country_code(&page.country).then(|| page.country.clone())
    }

    /// The country Apple was last seen to put the server in, without asking: None until it was
    /// learned.
    pub fn known_country(&self) -> Option<String> {
        self.country.get("country")
    }

    /// The New page's song rooms this week, asked for in English (so their titles can be
    /// matched), and again without when English is refused.
    pub async fn rooms(&self, country: &str) -> Result<Arc<Vec<SongRoom>>, PageError> {
        if !is_country_code(country) {
            return Err(PageError::Unavailable("not a country".into()));
        }
        if let Some(rooms) = self.rooms.get(country) {
            return Ok(rooms);
        }
        let failed_key = format!("rooms|{country}");
        if let Some(error) = self.missing.get(&failed_key) {
            return Err(error);
        }
        let english = format!("{}/{country}/new?l=en-US", self.base);
        let html = match self.fetch(&english).await {
            Ok(html) => Ok(html),
            Err(PageError::NoSuchPage | PageError::Unavailable(_)) => {
                self.fetch(&format!("{}/{country}/new", self.base)).await
            }
            Err(other) => Err(other),
        };
        let rooms = html.and_then(|html| parse_song_rooms(&html).map_err(PageError::from));
        match rooms {
            Ok(rooms) => {
                let rooms = Arc::new(rooms);
                self.rooms.set(country, rooms.clone(), 1, ROOMS_TTL);
                Ok(rooms)
            }
            Err(error) => {
                self.missing.set(failed_key, error.clone(), 1, FAILED_TTL);
                Err(error)
            }
        }
    }

    /// This week's Best New Songs or Trending Songs: the room's id and its songs.
    pub async fn room(&self, country: &str, kind: RoomKind) -> Result<(String, Arc<ApplePage>), PageError> {
        let rooms = self.rooms(country).await?;
        let room = room_for(&rooms, kind).ok_or(PageError::NoRoom)?;
        let key = page_key(country, &format!("room:{}", room.room_id));
        if let Some(page) = self.pages.get(&key) {
            return Ok((room.room_id.clone(), page));
        }
        let html = self
            .fetch(&format!("{}/{country}/room/{}", self.base, room.room_id))
            .await?;
        let page = Arc::new(parse_songs(&html)?);
        self.pages.set(key, page.clone(), 1, PAGE_TTL);
        Ok((room.room_id.clone(), page))
    }

    /// Apple Music's countries, from the last overall chart's country switcher, else the list
    /// as it was on 2026-10-07.
    pub fn countries(&self) -> Vec<String> {
        let read = self.countries.lock().clone();
        if read.is_empty() {
            APPLE_COUNTRIES.iter().map(|c| (*c).to_string()).collect()
        } else {
            read
        }
    }

    async fn fetch(&self, url: &str) -> Result<String, PageError> {
        if let Some(error) = self.missing.get(url) {
            return Err(error);
        }
        // A request that waited in the queue while the one before it failed takes that failure.
        if !self.gate.wait_unless(|| self.missing.get(url).is_some()).await
            && let Some(error) = self.missing.get(url)
        {
            return Err(error);
        }
        let mut response = match self.client.get(url).send().await {
            Ok(response) => response,
            Err(e) => {
                let error = PageError::Unavailable(short(&e));
                self.missing.set(url, error.clone(), 1, UNREACHABLE_TTL);
                return Err(error);
            }
        };
        let status = response.status();
        if status.as_u16() == 404 || status.as_u16() == 500 {
            self.missing.set(url, PageError::NoSuchPage, 1, MISSING_TTL);
            return Err(PageError::NoSuchPage);
        }
        if !status.is_success() {
            return Err(PageError::Unavailable(format!("HTTP {}", status.as_u16())));
        }
        if response.content_length().is_some_and(|n| n > MAX_BODY as u64) {
            return Err(PageError::TooBig);
        }
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| PageError::Unavailable(short(&e)))?
        {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(PageError::TooBig);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

impl Default for AppleMusicPages {
    fn default() -> Self {
        Self::new()
    }
}

fn page_key(country: &str, page: &str) -> String {
    format!("{country}|{page}")
}

/// A transport error in a few words: the kind of failure, never the URL.
fn short(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timed out".into()
    } else if error.is_connect() {
        "no connection".into()
    } else {
        "the answer broke off".into()
    }
}

#[cfg(test)]
#[path = "apple_music_pages_tests.rs"]
mod tests;
