//! Every audio body Octo hands out while Family is on: whose it is, which device it plays on,
//! what it is, and how many bytes have gone out.
//!
//! [`StreamTracker::track`] wraps a response body. The stream counts once its body is first
//! read, counts every byte it sends, and stops counting when the body is dropped. Each stream
//! has its own cancellation, which ends its body early.
//!
//! - **Devices at once** counts devices, not connections: a phone fetching the next song while
//!   it plays this one is one device. When a device starts and the person already has their
//!   limit of other devices playing, the device that started playing first is ended, and until
//!   the newer device has been quiet for three minutes the ended one is told where the music is
//!   now ([`StreamTracker::bumped_by`]) instead of taking it back.
//! - Offline copies and downloads are tracked for the bytes they send but never count as
//!   playing.
//! - **Fast-pull pacing**: a person whose streams are given a rate is held to it on average,
//!   after a 50 MB start, across all their paced streams together (one after another or at
//!   once). Chunks wait; nothing is refused.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt};
use indexmap::IndexMap;
use octo_core::family::device::NetworkPlace;
use parking_lot::Mutex;
use serde::Serialize;
use tokio::time::{Instant, Sleep};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// The device key of audio no device could be told apart for (an API key, a radio stream whose
/// opener is unknown). Each person has one such slot.
pub const UNKNOWN_DEVICE: &str = "unknown";

/// How long a newer device keeps an ended one from playing after its own last audio.
pub const BUMP_HOLD: Duration = Duration::from_secs(3 * 60);

/// What a paced stream may send at once before the rate holds it.
pub const PACING_BURST_BYTES: u64 = 50_000_000;

/// How often the reported rate of a stream is worked out again.
const RATE_WINDOW: Duration = Duration::from_secs(5);

/// Why audio is going out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StreamPurpose {
    /// Playback in an app.
    Play,
    /// A radio stream.
    Radio,
    /// An offline copy the Octo app keeps (`X-Octo-Purpose: offline`).
    Offline,
    /// A file download.
    Download,
}

impl StreamPurpose {
    /// Whether this counts as a device playing.
    pub fn is_playing(self) -> bool {
        matches!(self, StreamPurpose::Play | StreamPurpose::Radio)
    }
}

/// What the tracker is told about a stream when it is wrapped.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamStart {
    pub user: String,
    /// The device, or None for the person's unknown device slot.
    pub device_id: Option<String>,
    /// The device's name as the person reads it ("Pixel 9").
    pub device_name: String,
    /// The song (or radio station) the audio is.
    pub song_id: String,
    pub place: NetworkPlace,
    pub purpose: StreamPurpose,
    /// Devices this person may play on at once; 0 means no limit.
    pub devices_at_once: u32,
    /// The average rate the stream is held to, in bytes per second; None sends at full speed.
    pub pace_bytes_per_second: Option<u64>,
}

/// One live stream, for the load view and now playing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStreamView {
    pub username: String,
    pub device_id: Option<String>,
    pub device_name: String,
    pub song_id: String,
    pub place: NetworkPlace,
    pub purpose: StreamPurpose,
    pub started: DateTime<Utc>,
    pub bytes: u64,
    /// The rate the stream has been sending at lately.
    pub kbps: u32,
    /// The song's title and artist, once a lookup that began when it started has found them.
    pub name: Option<SongName>,
}

/// A song's title and artist, as a lookup found them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SongName {
    pub title: String,
    pub artist: String,
}

/// How long a looked-up name is kept for streams of the song that start later.
const NAME_HOLD: Duration = Duration::from_secs(3 * 60);

struct LiveEntry {
    user_key: String,
    device_key: String,
    view: LiveStreamView,
    started: Instant,
    window_start: Instant,
    window_bytes: u64,
    cancel: CancellationToken,
}

/// A device ended by a newer one.
#[derive(Debug, Clone)]
struct Bump {
    by_device: String,
    by_name: String,
}

#[derive(Default)]
struct Book {
    next: u64,
    live: IndexMap<u64, LiveEntry>,
    /// (person, device) to when it last sent playing audio.
    last_audio: HashMap<(String, String), Instant>,
    /// (person, ended device) to the device that ended it.
    bumps: HashMap<(String, String), Bump>,
    /// Song id to the name a lookup found (None while it looks, or when it found nothing) and
    /// when that lookup began.
    names: HashMap<String, (Option<SongName>, Instant)>,
    /// Person to the bucket every paced stream of theirs draws on.
    pacers: HashMap<String, Pacer>,
}

impl Book {
    /// Forgets names no live stream uses that were looked up longer than [`NAME_HOLD`] ago.
    fn forget_old_names(&mut self, now: Instant) {
        let live = &self.live;
        self.names.retain(|song_id, (_, at)| {
            now.saturating_duration_since(*at) < NAME_HOLD
                || live.values().any(|entry| entry.view.song_id == *song_id)
        });
    }

    fn playing_entries<'a>(&'a self, user_key: &'a str) -> impl Iterator<Item = &'a LiveEntry> + 'a {
        self.live.values().filter(move |entry| {
            entry.user_key == user_key && entry.view.purpose.is_playing() && !entry.cancel.is_cancelled()
        })
    }

    fn device_active(&self, user_key: &str, device_key: &str, now: Instant) -> bool {
        self.playing_entries(user_key)
            .any(|entry| entry.device_key == device_key)
            || self
                .last_audio
                .get(&(user_key.to_string(), device_key.to_string()))
                .is_some_and(|at| now.saturating_duration_since(*at) < BUMP_HOLD)
    }
}

/// The live streams, shared by every wrapped body.
#[derive(Default)]
pub struct StreamTracker {
    book: Arc<Mutex<Book>>,
}

fn user_key(user: &str) -> String {
    user.trim().to_lowercase()
}

fn device_key(device_id: Option<&str>) -> String {
    match device_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => id.to_string(),
        None => UNKNOWN_DEVICE.to_string(),
    }
}

impl StreamTracker {
    pub fn new() -> Self {
        StreamTracker::default()
    }

    /// Wraps `body` so the stream is tracked from its first read until it is dropped.
    pub fn track(&self, start: StreamStart, body: Body) -> Body {
        let cancel = CancellationToken::new();
        let pacer = start
            .pace_bytes_per_second
            .filter(|rate| *rate > 0)
            .map(|rate| Pace {
                user_key: user_key(&start.user),
                rate: rate as f64,
            });
        Body::from_stream(TrackedStream {
            inner: body.into_data_stream().boxed(),
            book: Arc::clone(&self.book),
            start: Some(start),
            id: None,
            canceled: Box::pin(cancel.clone().cancelled_owned()),
            cancel,
            pacer,
            sleep: None,
        })
    }

    /// The device that ended this one, by name, while that device has played within
    /// [`BUMP_HOLD`]; None when this device may play. A hold that has run out is forgotten.
    pub fn bumped_by(&self, user: &str, device_id: Option<&str>) -> Option<String> {
        let mut book = self.book.lock();
        let key = (user_key(user), device_key(device_id));
        let bump = book.bumps.get(&key)?.clone();
        if book.device_active(&key.0, &bump.by_device, Instant::now()) {
            return Some(bump.by_name);
        }
        book.bumps.remove(&key);
        None
    }

    /// Every live stream, oldest first.
    pub fn live(&self) -> Vec<LiveStreamView> {
        let book = self.book.lock();
        let now = Instant::now();
        book.live
            .values()
            .filter(|entry| !entry.cancel.is_cancelled())
            .map(|entry| {
                let mut view = entry.view.clone();
                if view.kbps == 0 {
                    view.kbps = kbps(view.bytes, now.saturating_duration_since(entry.started));
                }
                view
            })
            .collect()
    }

    /// How many streams are live now.
    pub fn len(&self) -> usize {
        self.book
            .lock()
            .live
            .values()
            .filter(|entry| !entry.cancel.is_cancelled())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Claims the name lookup for `song_id`: true when none has begun for it lately, and the
    /// caller then looks the song up and reports with [`StreamTracker::name_song`].
    pub fn start_naming(&self, song_id: &str) -> bool {
        let mut book = self.book.lock();
        book.forget_old_names(Instant::now());
        if book.names.contains_key(song_id) {
            return false;
        }
        book.names.insert(song_id.to_string(), (None, Instant::now()));
        true
    }

    /// Gives every live stream of `song_id` the name a lookup found, and keeps it for streams of
    /// the song that start soon after. None (nothing found) leaves them as they are.
    pub fn name_song(&self, song_id: &str, name: Option<SongName>) {
        let Some(name) = name else {
            return;
        };
        let mut book = self.book.lock();
        for entry in book.live.values_mut() {
            if entry.view.song_id == song_id {
                entry.view.name = Some(name.clone());
            }
        }
        let at = book.names.get(song_id).map_or_else(Instant::now, |(_, at)| *at);
        book.names.insert(song_id.to_string(), (Some(name), at));
    }

    /// Ends every live stream of one person's device.
    pub fn end_device(&self, user: &str, device_id: Option<&str>) {
        let book = self.book.lock();
        let (user, device) = (user_key(user), device_key(device_id));
        for entry in book.live.values() {
            if entry.user_key == user && entry.device_key == device {
                entry.cancel.cancel();
            }
        }
    }
}

fn kbps(bytes: u64, over: Duration) -> u32 {
    let millis = over.as_millis().max(1) as u64;
    u32::try_from(bytes.saturating_mul(8) / millis).unwrap_or(u32::MAX)
}

/// Starts counting a stream: the newest device wins when the person is over their limit.
fn register(book: &Mutex<Book>, start: StreamStart, cancel: CancellationToken) -> u64 {
    let mut book = book.lock();
    let now = Instant::now();
    let user = user_key(&start.user);
    let device = device_key(start.device_id.as_deref());
    if start.purpose.is_playing() {
        book.bumps.remove(&(user.clone(), device.clone()));
        let limit = start.devices_at_once as usize;
        let already_playing = book
            .playing_entries(&user)
            .any(|entry| entry.device_key == device);
        if limit > 0 && !already_playing {
            // Other devices playing now, the one that started first first.
            let mut others: IndexMap<String, Instant> = IndexMap::new();
            for entry in book.playing_entries(&user) {
                let first = others.entry(entry.device_key.clone()).or_insert(entry.started);
                if entry.started < *first {
                    *first = entry.started;
                }
            }
            others.sort_by(|_, a, _, b| a.cmp(b));
            while others.len() >= limit {
                let Some((oldest, _)) = others.shift_remove_index(0) else {
                    break;
                };
                for entry in book.live.values() {
                    if entry.user_key == user && entry.device_key == oldest {
                        entry.cancel.cancel();
                    }
                }
                book.bumps.insert(
                    (user.clone(), oldest),
                    Bump {
                        by_device: device.clone(),
                        by_name: start.device_name.clone(),
                    },
                );
            }
        }
        book.last_audio.insert((user.clone(), device.clone()), now);
    }
    book.next += 1;
    let id = book.next;
    let name = book.names.get(&start.song_id).and_then(|(name, _)| name.clone());
    book.live.insert(
        id,
        LiveEntry {
            user_key: user,
            device_key: device,
            view: LiveStreamView {
                username: start.user,
                device_id: start.device_id,
                device_name: start.device_name,
                song_id: start.song_id,
                place: start.place,
                purpose: start.purpose,
                started: Utc::now(),
                bytes: 0,
                kbps: 0,
                name,
            },
            started: now,
            window_start: now,
            window_bytes: 0,
            cancel,
        },
    );
    id
}

/// A token bucket: `rate` bytes a second, holding [`PACING_BURST_BYTES`] at most. A chunk may
/// take it below zero; the next chunk then waits until it is back at zero. Each person has one,
/// kept in the book, which every paced stream of theirs draws on: songs one after another, or
/// many at once, share one rate.
struct Pacer {
    tokens: f64,
    last: Instant,
    /// The rate it was last filled at.
    rate: f64,
}

impl Pacer {
    fn new(now: Instant, rate: f64) -> Self {
        Pacer {
            tokens: PACING_BURST_BYTES as f64,
            last: now,
            rate,
        }
    }

    fn refill(&mut self, rate: f64, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(PACING_BURST_BYTES as f64);
        self.last = now;
        self.rate = rate;
    }

    /// Whether the bucket has filled back up by `now`, so forgetting it changes nothing.
    fn full(&self, now: Instant) -> bool {
        self.tokens + now.saturating_duration_since(self.last).as_secs_f64() * self.rate
            >= PACING_BURST_BYTES as f64
    }
}

/// How one stream is paced: whose bucket it draws on, and at what rate.
struct Pace {
    user_key: String,
    rate: f64,
}

impl Pace {
    /// How long to wait before the next chunk, if at all.
    fn wait(&self, book: &Mutex<Book>) -> Option<Duration> {
        let mut book = book.lock();
        let now = Instant::now();
        let pacer = book
            .pacers
            .entry(self.user_key.clone())
            .or_insert_with(|| Pacer::new(now, self.rate));
        pacer.refill(self.rate, now);
        (pacer.tokens < 0.0).then(|| Duration::from_secs_f64(-pacer.tokens / self.rate))
    }

    fn spend(&self, book: &Mutex<Book>, bytes: usize) {
        let mut book = book.lock();
        let now = Instant::now();
        let pacer = book
            .pacers
            .entry(self.user_key.clone())
            .or_insert_with(|| Pacer::new(now, self.rate));
        pacer.refill(self.rate, now);
        pacer.tokens -= bytes as f64;
    }
}

type Inner = Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>;

struct TrackedStream {
    inner: Inner,
    book: Arc<Mutex<Book>>,
    start: Option<StreamStart>,
    id: Option<u64>,
    canceled: Pin<Box<WaitForCancellationFutureOwned>>,
    cancel: CancellationToken,
    pacer: Option<Pace>,
    sleep: Option<Pin<Box<Sleep>>>,
}

impl TrackedStream {
    fn counted(&self, bytes: usize) {
        let Some(id) = self.id else {
            return;
        };
        let mut book = self.book.lock();
        let now = Instant::now();
        let mut audio = None;
        if let Some(entry) = book.live.get_mut(&id) {
            entry.view.bytes += bytes as u64;
            entry.window_bytes += bytes as u64;
            let window = now.saturating_duration_since(entry.window_start);
            if window >= RATE_WINDOW {
                entry.view.kbps = kbps(entry.window_bytes, window);
                entry.window_start = now;
                entry.window_bytes = 0;
            }
            if entry.view.purpose.is_playing() {
                audio = Some((entry.user_key.clone(), entry.device_key.clone()));
            }
        }
        if let Some(key) = audio {
            book.last_audio.insert(key, now);
        }
    }
}

impl Stream for TrackedStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        if let Some(start) = this.start.take() {
            this.id = Some(register(&this.book, start, this.cancel.clone()));
        }
        if this.canceled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Some(Err(std::io::Error::other(
                "this stream was ended for a newer device",
            ))));
        }
        if let Some(sleep) = this.sleep.as_mut() {
            if sleep.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            this.sleep = None;
        }
        if let Some(wait) = this.pacer.as_ref().and_then(|pace| pace.wait(&this.book)) {
            let mut sleep = Box::pin(tokio::time::sleep(wait));
            if sleep.as_mut().poll(cx).is_pending() {
                this.sleep = Some(sleep);
                return Poll::Pending;
            }
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.counted(chunk.len());
                if let Some(pace) = this.pacer.as_ref() {
                    pace.spend(&this.book, chunk.len());
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(std::io::Error::other(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for TrackedStream {
    fn drop(&mut self) {
        let Some(id) = self.id else {
            return;
        };
        let mut book = self.book.lock();
        if let Some(entry) = book.live.shift_remove(&id)
            && entry.view.purpose.is_playing()
            && !entry.cancel.is_cancelled()
        {
            book.last_audio
                .insert((entry.user_key, entry.device_key), Instant::now());
        }
        // Old quiet devices are forgotten so the book does not grow.
        let now = Instant::now();
        book.last_audio
            .retain(|_, at| now.saturating_duration_since(*at) < BUMP_HOLD * 4);
        book.forget_old_names(now);
        // A bucket that has filled back up is the same as none, so it is forgotten.
        let Book { live, pacers, .. } = &mut *book;
        pacers.retain(|user, pacer| !pacer.full(now) || live.values().any(|entry| entry.user_key == *user));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn start(user: &str, device: Option<&str>, purpose: StreamPurpose, limit: u32) -> StreamStart {
        StreamStart {
            user: user.to_string(),
            device_id: device.map(str::to_string),
            device_name: device.map_or("another device".to_string(), |d| format!("{d} name")),
            song_id: "s1".to_string(),
            place: NetworkPlace::Home,
            purpose,
            devices_at_once: limit,
            pace_bytes_per_second: None,
        }
    }

    /// A body that sends `chunks` chunks of `size` bytes, then stays open until dropped.
    fn endless(chunks: usize, size: usize) -> Body {
        let chunk = Bytes::from(vec![7u8; size]);
        let sent =
            futures::stream::iter((0..chunks).map(move |_| Ok::<Bytes, std::io::Error>(chunk.clone())));
        Body::from_stream(sent.chain(futures::stream::pending()))
    }

    fn finite(chunks: usize, size: usize) -> Body {
        let chunk = Bytes::from(vec![7u8; size]);
        Body::from_stream(futures::stream::iter(
            (0..chunks).map(move |_| Ok::<Bytes, std::io::Error>(chunk.clone())),
        ))
    }

    async fn next(body: &mut Body) -> Option<Result<Bytes, axum::Error>> {
        body.frame()
            .await
            .map(|frame| frame.map(|f| f.into_data().unwrap_or_default()))
    }

    #[tokio::test]
    async fn bytes_are_counted() {
        let tracker = StreamTracker::new();
        let mut body = tracker.track(
            start("alex", Some("d_1"), StreamPurpose::Play, 0),
            finite(3, 1000),
        );
        assert!(tracker.live().is_empty(), "nothing counts before the first read");
        assert_eq!(next(&mut body).await.expect("a chunk").expect("ok").len(), 1000);
        let live = tracker.live();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].username, "alex");
        assert_eq!(live[0].device_id.as_deref(), Some("d_1"));
        assert_eq!(live[0].bytes, 1000);
        while next(&mut body).await.is_some() {}
        assert_eq!(tracker.live()[0].bytes, 3000);
        drop(body);
        assert!(tracker.live().is_empty(), "a dropped body stops counting");
    }

    #[tokio::test]
    async fn second_device_over_limit_ends_the_oldest() {
        let tracker = StreamTracker::new();
        let mut phone = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 1),
            endless(1, 10),
        );
        next(&mut phone).await.expect("plays").expect("ok");
        let mut laptop = tracker.track(
            start("Alex", Some("laptop"), StreamPurpose::Play, 1),
            endless(1, 10),
        );
        next(&mut laptop).await.expect("plays").expect("ok");
        // The phone started first, so it is the one ended.
        let ended = next(&mut phone).await.expect("an end");
        assert!(ended.is_err(), "the phone's body ends with an error");
        let live = tracker.live();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].device_id.as_deref(), Some("laptop"));
        // Someone else's devices are not touched.
        let mut sam = tracker.track(start("sam", Some("tv"), StreamPurpose::Play, 1), endless(1, 10));
        next(&mut sam).await.expect("plays").expect("ok");
        assert_eq!(tracker.len(), 2);
    }

    #[tokio::test]
    async fn prefetch_from_one_device_counts_once() {
        let tracker = StreamTracker::new();
        let mut now = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 1),
            endless(1, 10),
        );
        next(&mut now).await.expect("plays").expect("ok");
        let mut upcoming = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 1),
            endless(1, 10),
        );
        next(&mut upcoming).await.expect("plays").expect("ok");
        assert_eq!(tracker.len(), 2, "one device, two streams, nothing ended");
        assert_eq!(tracker.bumped_by("alex", Some("phone")), None);
    }

    #[tokio::test(start_paused = true)]
    async fn bumped_device_is_told_while_winner_plays() {
        let tracker = StreamTracker::new();
        let mut phone = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 1),
            endless(1, 10),
        );
        next(&mut phone).await.expect("plays").expect("ok");
        let mut laptop = tracker.track(
            start("alex", Some("laptop"), StreamPurpose::Play, 1),
            endless(1, 10),
        );
        next(&mut laptop).await.expect("plays").expect("ok");
        drop(phone);
        assert_eq!(
            tracker.bumped_by("alex", Some("phone")).as_deref(),
            Some("laptop name")
        );
        // The laptop stops; for three minutes after its last audio the phone still waits.
        drop(laptop);
        tokio::time::advance(Duration::from_secs(120)).await;
        assert!(tracker.bumped_by("alex", Some("phone")).is_some());
        tokio::time::advance(Duration::from_secs(61)).await;
        assert_eq!(tracker.bumped_by("alex", Some("phone")), None);
        // The laptop itself was never ended.
        assert_eq!(tracker.bumped_by("alex", Some("laptop")), None);
    }

    #[tokio::test]
    async fn offline_purpose_never_counts() {
        let tracker = StreamTracker::new();
        let mut phone = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 1),
            endless(1, 10),
        );
        next(&mut phone).await.expect("plays").expect("ok");
        // An offline copy on another device neither ends the phone nor is ended by it.
        let mut offline = tracker.track(
            start("alex", Some("tablet"), StreamPurpose::Offline, 1),
            endless(1, 10),
        );
        next(&mut offline).await.expect("copies").expect("ok");
        let mut download = tracker.track(
            start("alex", Some("laptop"), StreamPurpose::Download, 1),
            endless(1, 10),
        );
        next(&mut download).await.expect("copies").expect("ok");
        assert_eq!(tracker.len(), 3);
        assert_eq!(tracker.bumped_by("alex", Some("phone")), None);
        let purposes: Vec<StreamPurpose> = tracker.live().iter().map(|s| s.purpose).collect();
        assert_eq!(
            purposes,
            vec![
                StreamPurpose::Play,
                StreamPurpose::Offline,
                StreamPurpose::Download
            ]
        );
    }

    #[tokio::test]
    async fn unknown_devices_share_one_slot_per_person() {
        let tracker = StreamTracker::new();
        let mut first = tracker.track(start("alex", None, StreamPurpose::Radio, 1), endless(1, 10));
        next(&mut first).await.expect("plays").expect("ok");
        let mut second = tracker.track(start("alex", None, StreamPurpose::Play, 1), endless(1, 10));
        next(&mut second).await.expect("plays").expect("ok");
        assert_eq!(tracker.len(), 2, "the unknown slot is one device");
        assert_eq!(tracker.live()[0].device_id, None);
    }

    #[tokio::test(start_paused = true)]
    async fn songs_pulled_one_after_another_share_one_pace() {
        let tracker = StreamTracker::new();
        let began = Instant::now();
        for _ in 0..10 {
            let mut paced = start("alex", Some("phone"), StreamPurpose::Play, 0);
            paced.pace_bytes_per_second = Some(1_000_000);
            let body = tracker.track(paced, finite(40, 1_000_000));
            let bytes = body.collect().await.expect("every byte").to_bytes();
            assert_eq!(bytes.len(), 40_000_000);
        }
        let took = Instant::now() - began;
        // 400 MB at 1 MB a second after a 50 MB start.
        assert!(
            took >= Duration::from_secs(300),
            "400 MB went out in {took:?} at a 1 MB/s pace"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn songs_pulled_at_once_share_one_pace() {
        let tracker = Arc::new(StreamTracker::new());
        let began = Instant::now();
        let pulls = (0..10).map(|n| {
            let tracker = Arc::clone(&tracker);
            async move {
                let mut paced = start("alex", Some(&format!("phone-{n}")), StreamPurpose::Download, 0);
                paced.pace_bytes_per_second = Some(1_000_000);
                let body = tracker.track(paced, finite(40, 1_000_000));
                body.collect().await.expect("every byte").to_bytes().len()
            }
        });
        let sent: usize = futures::future::join_all(pulls).await.into_iter().sum();
        assert_eq!(sent, 400_000_000);
        let took = Instant::now() - began;
        assert!(
            took >= Duration::from_secs(300),
            "400 MB went out in {took:?} at a 1 MB/s pace"
        );
        // Someone else is paced on their own.
        let mut other = start("sam", Some("tablet"), StreamPurpose::Play, 0);
        other.pace_bytes_per_second = Some(1_000_000);
        let again = Instant::now();
        let body = tracker.track(other, finite(10, 1_000_000));
        assert_eq!(
            body.collect().await.expect("every byte").to_bytes().len(),
            10_000_000
        );
        assert!(
            Instant::now() - again < Duration::from_secs(1),
            "sam starts with a full allowance"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_bucket_that_filled_back_up_is_forgotten() {
        let tracker = StreamTracker::new();
        let mut paced = start("alex", Some("phone"), StreamPurpose::Play, 0);
        paced.pace_bytes_per_second = Some(1_000_000);
        let body = tracker.track(paced, finite(1, 1_000));
        body.collect().await.expect("every byte");
        tokio::time::advance(Duration::from_secs(1)).await;
        let unpaced = tracker.track(start("sam", None, StreamPurpose::Play, 0), finite(1, 1));
        unpaced.collect().await.expect("every byte");
        assert!(tracker.book.lock().pacers.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn fast_pull_is_slowed_not_refused() {
        let tracker = StreamTracker::new();
        let mut paced = start("alex", Some("phone"), StreamPurpose::Play, 0);
        // 1 MB a second after the 50 MB start.
        paced.pace_bytes_per_second = Some(1_000_000);
        let body = tracker.track(paced, finite(60, 1_000_000));
        let began = Instant::now();
        let bytes = body.collect().await.expect("every byte").to_bytes();
        assert_eq!(bytes.len(), 60_000_000);
        let took = Instant::now() - began;
        // The ten megabytes past the start take about ten seconds.
        assert!(took >= Duration::from_secs(9), "{took:?}");
        assert!(took <= Duration::from_secs(11), "{took:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn uncapped_is_never_paced() {
        let tracker = StreamTracker::new();
        let body = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 0),
            finite(60, 1_000_000),
        );
        let began = Instant::now();
        assert_eq!(body.collect().await.expect("bytes").to_bytes().len(), 60_000_000);
        assert_eq!(Instant::now() - began, Duration::ZERO);
    }

    #[tokio::test]
    async fn a_device_can_be_ended_by_hand() {
        let tracker = StreamTracker::new();
        let mut phone = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 0),
            endless(1, 10),
        );
        next(&mut phone).await.expect("plays").expect("ok");
        tracker.end_device("ALEX", Some("phone"));
        assert!(next(&mut phone).await.expect("an end").is_err());
        assert!(tracker.is_empty());
    }

    fn named(title: &str) -> Option<SongName> {
        Some(SongName {
            title: title.to_string(),
            artist: "Somebody".to_string(),
        })
    }

    #[tokio::test]
    async fn a_found_name_reaches_live_streams_of_the_song_and_ones_that_start_after() {
        let tracker = StreamTracker::new();
        let mut first = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 0),
            endless(1, 10),
        );
        next(&mut first).await.expect("plays").expect("ok");
        assert_eq!(tracker.live()[0].name, None);

        assert!(tracker.start_naming("s1"));
        assert!(!tracker.start_naming("s1"), "one lookup per song at a time");
        tracker.name_song("s1", named("Outside"));
        assert_eq!(tracker.live()[0].name, named("Outside"));

        let mut second = tracker.track(start("sam", Some("tv"), StreamPurpose::Play, 0), endless(1, 10));
        next(&mut second).await.expect("plays").expect("ok");
        let live = tracker.live();
        assert_eq!(live.len(), 2);
        assert!(live.iter().all(|stream| stream.name == named("Outside")));
    }

    #[tokio::test]
    async fn a_name_found_before_the_stream_is_first_read_is_kept_for_it() {
        let tracker = StreamTracker::new();
        let mut body = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 0),
            endless(1, 10),
        );
        assert!(tracker.start_naming("s1"));
        tracker.name_song("s1", named("Early"));
        next(&mut body).await.expect("plays").expect("ok");
        assert_eq!(tracker.live()[0].name, named("Early"));
    }

    #[tokio::test]
    async fn a_lookup_that_found_nothing_leaves_the_stream_unnamed() {
        let tracker = StreamTracker::new();
        let mut body = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 0),
            endless(1, 10),
        );
        next(&mut body).await.expect("plays").expect("ok");
        assert!(tracker.start_naming("s1"));
        tracker.name_song("s1", None);
        assert_eq!(tracker.live()[0].name, None);
        assert!(!tracker.start_naming("s1"), "not asked again at once");
    }

    #[tokio::test(start_paused = true)]
    async fn names_no_stream_uses_are_forgotten_after_a_while() {
        let tracker = StreamTracker::new();
        assert!(tracker.start_naming("gone"));
        tracker.name_song("gone", named("Gone"));
        let mut body = tracker.track(
            start("alex", Some("phone"), StreamPurpose::Play, 0),
            endless(1, 10),
        );
        next(&mut body).await.expect("plays").expect("ok");
        assert!(tracker.start_naming("s1"));
        tokio::time::advance(NAME_HOLD + Duration::from_secs(1)).await;
        // The playing song keeps its claim; the other may be looked up again.
        assert!(!tracker.start_naming("s1"));
        assert!(tracker.start_naming("gone"));
    }
}
