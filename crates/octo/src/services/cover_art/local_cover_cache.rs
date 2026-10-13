//! Covers of the library's own songs and albums, kept on disk. Each cover id is asked of
//! Navidrome once, at [`MASTER_SIDE`], and every smaller size is cut from that copy here, so a
//! repeat or a new size never waits on Navidrome reading the art out of the music files again.
//!
//! The copies live in `<config>/cover-cache/<first two hex>/<sha-256 of the id>`. An id that
//! ends in `_<hex stamp>` names one version of the art (the stamp changes when the art does),
//! so its copy is good until the cache needs the room; any other id is asked again after
//! [`PLAIN_ID_LIFETIME`]. The cache holds at most the configured number of bytes and lets the
//! least recently used copies go first. Cut sizes are kept in memory for a while.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use octo_core::common::single_flight::SingleFlight;
use octo_media::cover::badged_covers::BadgedCovers;
use octo_media::cover::cover_image;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::{OnceCell, Semaphore};
use tracing::{debug, warn};

/// The longer side of the copy kept of each cover, and the largest size the apps ask for.
pub const MASTER_SIDE: u32 = 1200;
/// How long a copy of a cover whose id carries no stamp is used before it is asked for again.
pub const PLAIN_ID_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The JPEG quality of a cut size.
const CUT_QUALITY: u8 = 85;
/// The longest one fetch may take before everyone waiting on it gives up.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// How many warming fetches run at once.
const WARM_AT_ONCE: usize = 2;
/// How many warming fetches may wait or run at once; more are dropped.
const WARM_QUEUE: usize = 16;
/// The most cut sizes kept in memory, and their bytes together.
const CUT_ENTRIES: usize = 4000;
const CUT_BYTES: usize = 64 * 1024 * 1024;
/// When the cache is over its size, copies go until it is at this share of it, so the next
/// copy does not make it evict again at once.
const EVICT_TO_PERCENT: u64 = 90;

/// What fetching a cover from Navidrome gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// A real picture, worth keeping.
    Picture(Vec<u8>),
    /// An answer that is not one to keep: the stand-in for missing art, an error envelope.
    Other,
    /// Navidrome could not be reached.
    Unreachable,
}

/// Why a cover was not answered from the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotKept {
    /// Ask Navidrome for the request as it came.
    Ask,
    /// Navidrome could not be reached.
    Unreachable,
}

/// A cover answered from the cache.
#[derive(Debug, Clone)]
pub struct CoverPicture {
    pub bytes: Arc<Vec<u8>>,
    pub content_type: &'static str,
}

/// The cache's size limit in bytes, read at every use; 0 turns the cache off.
pub type CacheLimit = Arc<dyn Fn() -> u64 + Send + Sync>;

#[derive(Clone)]
pub struct LocalCoverCache {
    inner: Arc<Inner>,
}

struct Inner {
    dir: PathBuf,
    limit: CacheLimit,
    plain_id_lifetime: Duration,
    index: Mutex<Index>,
    loaded: OnceCell<()>,
    fetching: SingleFlight<String, Result<Arc<Vec<u8>>, NotKept>>,
    cuts: BadgedCovers,
    warming: Arc<Semaphore>,
    warm_queued: AtomicUsize,
}

#[derive(Default)]
struct Index {
    entries: HashMap<String, Kept>,
    bytes: u64,
    tick: u64,
}

struct Kept {
    len: u64,
    stored: SystemTime,
    /// The tick of the last use; the lowest goes first.
    used: u64,
}

impl Index {
    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn remove(&mut self, key: &str) {
        if let Some(kept) = self.entries.remove(key) {
            self.bytes -= kept.len;
        }
    }

    /// The keys taken out so the cache fits `limit` again, least recently used first.
    fn evict_over(&mut self, limit: u64) -> Vec<String> {
        if self.bytes <= limit {
            return Vec::new();
        }
        let target = limit / 100 * EVICT_TO_PERCENT;
        let mut by_use: Vec<(u64, String)> = self
            .entries
            .iter()
            .map(|(key, kept)| (kept.used, key.clone()))
            .collect();
        by_use.sort_unstable();
        let mut victims = Vec::new();
        for (_, key) in by_use {
            if self.bytes <= target {
                break;
            }
            self.remove(&key);
            victims.push(key);
        }
        victims
    }
}

impl LocalCoverCache {
    /// A cache in `dir`, holding at most `limit()` bytes.
    pub fn new(dir: PathBuf, limit: CacheLimit) -> LocalCoverCache {
        LocalCoverCache::with_plain_id_lifetime(dir, limit, PLAIN_ID_LIFETIME)
    }

    /// The same, with copies of covers without a stamp asked for again after `lifetime`.
    pub fn with_plain_id_lifetime(dir: PathBuf, limit: CacheLimit, lifetime: Duration) -> LocalCoverCache {
        LocalCoverCache {
            inner: Arc::new(Inner {
                dir,
                limit,
                plain_id_lifetime: lifetime,
                index: Mutex::new(Index::default()),
                loaded: OnceCell::new(),
                fetching: SingleFlight::new(),
                cuts: BadgedCovers::new(CUT_ENTRIES, CUT_BYTES),
                warming: Arc::new(Semaphore::new(WARM_AT_ONCE)),
                warm_queued: AtomicUsize::new(0),
            }),
        }
    }

    pub fn is_on(&self) -> bool {
        (self.inner.limit)() > 0
    }

    /// The cover `id` at `size` (its longer side at most that), from the copy kept of it, or
    /// fetched with `fetch` (given the side to ask for) when there is none. Concurrent requests
    /// for the same id wait on one fetch.
    ///
    /// `Err(NotKept::Ask)` when the cache cannot answer: it is off, the fetch gave no picture
    /// to keep, or `size` is larger than the copy kept and the copy is not the whole picture.
    pub async fn cover<F, Fut>(&self, id: &str, size: u32, fetch: F) -> Result<CoverPicture, NotKept>
    where
        F: FnOnce(u32) -> Fut,
        Fut: Future<Output = Fetched> + Send + 'static,
    {
        if !self.is_on() || size == 0 {
            return Err(NotKept::Ask);
        }
        self.load().await;
        let key = key_of(id);
        let cut_key = format!("{key}@{size}");
        if let Some(cut) = self.inner.cuts.get(&cut_key, Instant::now()) {
            return Ok(CoverPicture {
                bytes: cut,
                content_type: "image/jpeg",
            });
        }
        let master = self.master(id, &key, fetch).await?;
        let Some((width, height)) = cover_image::measure(&master) else {
            return Err(NotKept::Ask);
        };
        let longest = width.max(height);
        if size > MASTER_SIDE && longest >= MASTER_SIDE {
            return Err(NotKept::Ask);
        }
        if longest <= size {
            return Ok(CoverPicture {
                content_type: cover_image::mime_type(&master),
                bytes: master,
            });
        }
        let cut = tokio::task::spawn_blocking(move || cover_image::shrink_to(&master, size, CUT_QUALITY))
            .await
            .ok()
            .flatten()
            .ok_or(NotKept::Ask)?;
        let cut = Arc::new(cut);
        self.inner.cuts.put(cut_key, Arc::clone(&cut), Instant::now());
        Ok(CoverPicture {
            bytes: cut,
            content_type: "image/jpeg",
        })
    }

    /// Fetches the cover `id` in the background unless a copy is kept, at most [`WARM_AT_ONCE`]
    /// at a time. Dropped when [`WARM_QUEUE`] are already waiting or running.
    pub fn warm<F, Fut>(&self, id: String, fetch: F)
    where
        F: FnOnce(u32) -> Fut + Send + 'static,
        Fut: Future<Output = Fetched> + Send + 'static,
    {
        if !self.is_on() {
            return;
        }
        let queued = &self.inner.warm_queued;
        if queued.fetch_add(1, Ordering::SeqCst) >= WARM_QUEUE {
            queued.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        let cache = self.clone();
        tokio::spawn(async move {
            let semaphore = Arc::clone(&cache.inner.warming);
            if let Ok(_turn) = semaphore.acquire_owned().await {
                cache.load().await;
                let key = key_of(&id);
                let _ = cache.master(&id, &key, fetch).await;
            }
            cache.inner.warm_queued.fetch_sub(1, Ordering::SeqCst);
        });
    }

    /// The bytes of every copy kept on disk.
    pub fn kept_bytes(&self) -> u64 {
        self.inner.index.lock().bytes
    }

    /// How many copies are kept on disk.
    pub fn kept_count(&self) -> usize {
        self.inner.index.lock().entries.len()
    }

    /// Whether a copy of the cover `id` is kept.
    pub fn is_kept(&self, id: &str) -> bool {
        self.inner.index.lock().entries.contains_key(&key_of(id))
    }

    /// Reads what the folder already holds, once.
    async fn load(&self) {
        let inner = Arc::clone(&self.inner);
        self.inner
            .loaded
            .get_or_init(|| async move {
                let dir = inner.dir.clone();
                let found = tokio::task::spawn_blocking(move || scan(&dir))
                    .await
                    .unwrap_or_default();
                let victims = {
                    let mut index = inner.index.lock();
                    for (key, len, stored) in found {
                        let used = index.next_tick();
                        index.bytes += len;
                        index.entries.insert(key, Kept { len, stored, used });
                    }
                    index.evict_over((inner.limit)())
                };
                inner.remove_files(victims).await;
            })
            .await;
    }

    /// The copy kept of `id`, else one fetched and kept.
    async fn master<F, Fut>(&self, id: &str, key: &str, fetch: F) -> Result<Arc<Vec<u8>>, NotKept>
    where
        F: FnOnce(u32) -> Fut,
        Fut: Future<Output = Fetched> + Send + 'static,
    {
        let stamped = has_art_stamp(id);
        if let Some(kept) = self.inner.read_kept(key, stamped).await {
            return Ok(kept);
        }
        let inner = Arc::clone(&self.inner);
        let owned_key = key.to_string();
        let run = self.inner.fetching.run(
            key.to_string(),
            move |_| {
                let fetched = fetch(MASTER_SIDE);
                async move {
                    // A fetch that finished just before this one started already kept it.
                    if let Some(kept) = inner.read_kept(&owned_key, stamped).await {
                        return Ok(Ok(kept));
                    }
                    Ok(inner.keep(owned_key, fetched.await).await)
                }
            },
            FETCH_TIMEOUT,
        );
        run.await.unwrap_or(Err(NotKept::Unreachable))
    }
}

impl Inner {
    fn path_of(&self, key: &str) -> PathBuf {
        self.dir.join(&key[..2]).join(key)
    }

    /// The copy kept under `key` while it is fresh, marked as just used.
    async fn read_kept(&self, key: &str, stamped: bool) -> Option<Arc<Vec<u8>>> {
        {
            let mut index = self.index.lock();
            let tick = index.next_tick();
            let kept = index.entries.get_mut(key)?;
            let expired = !stamped
                && kept
                    .stored
                    .elapsed()
                    .is_ok_and(|age| age > self.plain_id_lifetime);
            if expired {
                return None;
            }
            kept.used = tick;
        }
        match tokio::fs::read(self.path_of(key)).await {
            Ok(bytes) if !bytes.is_empty() => Some(Arc::new(bytes)),
            _ => {
                self.index.lock().remove(key);
                None
            }
        }
    }

    /// Keeps a fetched picture and hands it back. Only a picture that can be read is kept; it is
    /// still answered when the disk refuses it.
    async fn keep(&self, key: String, fetched: Fetched) -> Result<Arc<Vec<u8>>, NotKept> {
        let bytes = match fetched {
            Fetched::Picture(bytes) => bytes,
            Fetched::Other => return Err(NotKept::Ask),
            Fetched::Unreachable => return Err(NotKept::Unreachable),
        };
        if cover_image::measure(&bytes).is_none() {
            return Err(NotKept::Ask);
        }
        let bytes = Arc::new(bytes);
        let len = bytes.len() as u64;
        let limit = (self.limit)();
        if limit == 0 || len > limit {
            return Ok(bytes);
        }
        let path = self.path_of(&key);
        let to_write = Arc::clone(&bytes);
        let written = tokio::task::spawn_blocking(move || write_whole(&path, &to_write)).await;
        match written {
            Ok(Ok(())) => {
                let victims = {
                    let mut index = self.index.lock();
                    let used = index.next_tick();
                    index.remove(&key);
                    index.bytes += len;
                    index.entries.insert(
                        key,
                        Kept {
                            len,
                            stored: SystemTime::now(),
                            used,
                        },
                    );
                    index.evict_over(limit)
                };
                self.remove_files(victims).await;
            }
            Ok(Err(e)) => warn!("cover cache could not keep a cover: {e}"),
            Err(e) => warn!("cover cache could not keep a cover: {e}"),
        }
        Ok(bytes)
    }

    async fn remove_files(&self, keys: Vec<String>) {
        if keys.is_empty() {
            return;
        }
        debug!("cover cache let {} covers go", keys.len());
        let paths: Vec<PathBuf> = keys.iter().map(|key| self.path_of(key)).collect();
        let _ = tokio::task::spawn_blocking(move || {
            for path in paths {
                let _ = std::fs::remove_file(path);
            }
        })
        .await;
    }
}

/// The file name a cover id is kept under.
fn key_of(id: &str) -> String {
    hex::encode(Sha256::digest(id.as_bytes()))
}

/// Whether `id` ends in `_` and a hex stamp, which changes whenever the art does.
pub fn has_art_stamp(id: &str) -> bool {
    id.rsplit_once('_').is_some_and(|(head, stamp)| {
        !head.is_empty() && !stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// Writes the file whole: to a side file first, then renamed over the old one, so a reader
/// never sees half a picture.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let side = path.with_extension("part");
    std::fs::write(&side, bytes)?;
    std::fs::rename(&side, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&side);
    })
}

/// Every copy in the folder, oldest first, as (key, bytes, when it was written). Leftover side
/// files are removed.
fn scan(dir: &Path) -> Vec<(String, u64, SystemTime)> {
    let mut found = Vec::new();
    let Ok(folders) = std::fs::read_dir(dir) else {
        return found;
    };
    for folder in folders.flatten() {
        let Ok(files) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let name = file.file_name().to_string_lossy().into_owned();
            if path.extension().is_some_and(|ext| ext == "part") {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            if name.len() != 64 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let Ok(meta) = file.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let stored = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            found.push((name, meta.len(), stored));
        }
    }
    found.sort_by_key(|(_, _, stored)| *stored);
    found
}

#[cfg(test)]
#[path = "local_cover_cache_tests.rs"]
mod tests;
