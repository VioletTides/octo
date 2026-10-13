use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, TimeDelta, Utc};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use octo_core::common::clock::Clock;
use parking_lot::Mutex;
use tracing::debug;

use super::album_ownership::LibraryAlbumIndex;
use super::navidrome_playlist_api::NavidromePlaylistApi;
use super::quality_upgrade_worker::LibrarySongRow;
use crate::services::family::library_index::row_full_path;
use crate::services::family::{LibraryScope, ScopeResolver, ScopeView};

/// One page of the library, with how many rows Navidrome's page held; None when Navidrome could
/// not be read. Tests answer from a list.
pub type ReadPage =
    Arc<dyn Fn(i32, i32) -> BoxFuture<'static, Option<(Vec<LibrarySongRow>, usize)>> + Send + Sync>;

type Reading = Shared<BoxFuture<'static, Option<Arc<LibraryAlbumIndex>>>>;

#[derive(Default)]
struct State {
    current: Option<(Arc<LibraryAlbumIndex>, DateTime<Utc>)>,
    reading: Option<Reading>,
    failed_at: Option<DateTime<Utc>>,
    /// The whole read with Family on, every library in it (`current` keeps to the shared one),
    /// and each member's own view of it built from that, by name in lower case.
    everything: Option<Arc<LibraryAlbumIndex>>,
    viewers: HashMap<String, Arc<LibraryAlbumIndex>>,
}

struct Inner {
    read_page: ReadPage,
    clock: Clock,
    state: Mutex<State>,
    /// Which library each song is in, while Family is on: what a search counts as the
    /// library's is the shared library's, never a member's own.
    scope: Mutex<Option<ScopeResolver>>,
}

/// Whether a row is in `scope`.
fn row_in(view: &ScopeView, scope: &LibraryScope, row: &LibrarySongRow) -> bool {
    let full = row_full_path(row.library_path.as_deref(), &row.path);
    view.allows(scope, Some(&row.id), Some(&full))
}

/// The whole library, read through Navidrome's own song list and kept, for the questions a
/// search asks of every row at once (which outside albums the library already holds). Reading it
/// is a few large requests, so it is never done on a search's own time once there is a copy: a
/// copy older than [`FRESH_FOR`](Self::FRESH_FOR) is still answered with while a new one is read,
/// and only one read runs at a time. Needs the Navidrome admin sign-in, as Spotify import does;
/// with none the answer is None and searches go out as before.
#[derive(Clone)]
pub struct LibrarySnapshot {
    inner: Arc<Inner>,
}

impl LibrarySnapshot {
    /// A copy this young is answered with as it is.
    pub const FRESH_FOR: TimeDelta = TimeDelta::minutes(5);

    /// A copy older than this is not answered with at all: the next caller waits for a read.
    pub const USABLE_FOR: TimeDelta = TimeDelta::hours(1);

    /// After a read that failed, how long callers are answered "unknown" before another is tried.
    pub const RETRY_AFTER: TimeDelta = TimeDelta::minutes(1);

    const PAGE_SIZE: i32 = 1000;

    /// The library read through Navidrome's native song list, as the admin.
    pub fn new(navidrome: Arc<NavidromePlaylistApi>) -> Self {
        Self::with(
            Arc::new(move |start, count| {
                let navidrome = Arc::clone(&navidrome);
                Box::pin(async move { navidrome.list_songs(start, count).await })
            }),
            Clock::system(),
        )
    }

    /// Pages from `read_page` and the time from `clock` (the test seams).
    pub fn with(read_page: ReadPage, clock: Clock) -> Self {
        Self {
            inner: Arc::new(Inner {
                read_page,
                clock,
                state: Mutex::new(State::default()),
                scope: Mutex::new(None),
            }),
        }
    }

    /// Keeps the library's copy to the shared library while Family is on: a song only in a
    /// member's own library is not the library's, for anyone else.
    pub fn with_scope(self, scope: ScopeResolver) -> Self {
        *self.inner.scope.lock() = Some(scope);
        self
    }

    /// The library as `scope` sees it: [`Self::current`] for the shared library, and for a
    /// member the shared library with their own songs added (never another member's).
    pub async fn current_for(&self, scope: &LibraryScope) -> Option<Arc<LibraryAlbumIndex>> {
        let shared = self.current().await?;
        let LibraryScope::Viewer(user) = scope else {
            return Some(shared);
        };
        let resolver = self.inner.scope.lock().clone().filter(ScopeResolver::active)?;
        let key = user.trim().to_lowercase();
        let everything = {
            let state = self.inner.state.lock();
            if let Some(view) = state.viewers.get(&key) {
                return Some(Arc::clone(view));
            }
            state.everything.clone()
        };
        let Some(everything) = everything else {
            return Some(shared);
        };
        let scopes = resolver.view();
        let view = Arc::new(LibraryAlbumIndex::new(
            everything
                .rows()
                .iter()
                .filter(|row| row_in(&scopes, scope, row))
                .map(|row| (**row).clone()),
        ));
        self.inner.state.lock().viewers.insert(key, Arc::clone(&view));
        Some(view)
    }

    /// The library as last read, reading it first when there is no usable copy.
    pub async fn current(&self) -> Option<Arc<LibraryAlbumIndex>> {
        let reading = {
            let mut state = self.inner.state.lock();
            let now = self.inner.clock.now();
            if let Some((index, at)) = &state.current {
                let age = now - *at;
                if age < Self::FRESH_FOR {
                    return Some(Arc::clone(index));
                }
                if age < Self::USABLE_FOR {
                    let index = Arc::clone(index);
                    if state.reading.is_none() {
                        state.reading = Some(self.start_read());
                    }
                    return Some(index);
                }
            }
            if state.reading.is_none()
                && let Some(failed) = state.failed_at
                && now - failed < Self::RETRY_AFTER
            {
                return None;
            }
            match &state.reading {
                Some(reading) => reading.clone(),
                None => {
                    let reading = self.start_read();
                    state.reading = Some(reading.clone());
                    reading
                }
            }
        };
        reading.await
    }

    /// When the copy answered with now was read, or None before the first read.
    pub fn read_at_utc(&self) -> Option<DateTime<Utc>> {
        self.inner.state.lock().current.as_ref().map(|(_, at)| *at)
    }

    /// A copy read from now on, for a caller that knows the library changed (Library health's
    /// "Check again", after its own fixes). A read already running is joined rather than doubled.
    pub async fn refresh(&self) -> Option<Arc<LibraryAlbumIndex>> {
        let reading = {
            let mut state = self.inner.state.lock();
            state.failed_at = None;
            match &state.reading {
                Some(reading) => reading.clone(),
                None => {
                    let reading = self.start_read();
                    state.reading = Some(reading.clone());
                    reading
                }
            }
        };
        reading.await
    }

    /// A read on a task of its own and without any caller: one caller giving up does not stop
    /// a read every later caller will use. Called with the state locked, so the read cannot
    /// finish (and clear `reading`) before it is stored.
    fn start_read(&self) -> Reading {
        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(async move {
            let _done = ReadDone(Arc::clone(&inner));
            read(&inner).await
        });
        task.map(Result::unwrap_or_default).boxed().shared()
    }
}

/// Lets go of the read when it ends, however it ends.
struct ReadDone(Arc<Inner>);

impl Drop for ReadDone {
    fn drop(&mut self) {
        self.0.state.lock().reading = None;
    }
}

async fn read(inner: &Inner) -> Option<Arc<LibraryAlbumIndex>> {
    let clock = Instant::now();
    let mut rows: Vec<LibrarySongRow> = Vec::new();
    let mut start = 0;
    loop {
        let Some((page, count)) = (inner.read_page)(start, LibrarySnapshot::PAGE_SIZE).await else {
            debug!("Could not read the library for album ownership");
            inner.state.lock().failed_at = Some(inner.clock.now());
            return None;
        };
        rows.extend(page);
        if (count as i32) < LibrarySnapshot::PAGE_SIZE {
            break;
        }
        start += LibrarySnapshot::PAGE_SIZE;
    }
    let read = rows.len();
    // While members have libraries of their own (Family on, or turned off with their libraries
    // still there), the library's copy is the shared library's; members' own songs are kept
    // aside for their own view.
    let resolver = inner.scope.lock().clone().filter(ScopeResolver::keeps_shared);
    let (index, everything) = match resolver {
        Some(resolver) => {
            let scopes = resolver.shared_view();
            let (shared, own): (Vec<LibrarySongRow>, Vec<LibrarySongRow>) = rows
                .into_iter()
                .partition(|row| row_in(&scopes, &LibraryScope::Shared, row));
            let shared_index = Arc::new(LibraryAlbumIndex::new(shared));
            let everything = (!own.is_empty()).then(|| {
                Arc::new(LibraryAlbumIndex::new(
                    shared_index.rows().iter().map(|row| (**row).clone()).chain(own),
                ))
            });
            (shared_index, everything)
        }
        None => (Arc::new(LibraryAlbumIndex::new(rows)), None),
    };
    {
        let mut state = inner.state.lock();
        state.current = Some((Arc::clone(&index), inner.clock.now()));
        state.everything = everything;
        state.viewers.clear();
        state.failed_at = None;
    }
    debug!(
        "Read {read} library songs for album ownership in {} ms",
        clock.elapsed().as_millis()
    );
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::family::library_index::scope_for_tests;

    fn row(id: &str, library_path: &str, artist: &str, title: &str, album: &str) -> LibrarySongRow {
        LibrarySongRow {
            id: id.into(),
            path: format!("{artist}/{album}/{title}.flac"),
            library_path: Some(library_path.into()),
            size: 1,
            suffix: "flac".into(),
            bit_rate: 900,
            title: title.into(),
            artist: artist.into(),
            duration: Some(200),
            album: Some(album.into()),
            album_id: Some(format!("al-{album}")),
            album_artist: Some(artist.into()),
            ..Default::default()
        }
    }

    fn snapshot(rows: Vec<LibrarySongRow>) -> LibrarySnapshot {
        LibrarySnapshot::with(
            Arc::new(move |start, _| {
                let page = if start == 0 { rows.clone() } else { Vec::new() };
                let count = page.len();
                Box::pin(async move { Some((page, count)) })
            }),
            Clock::system(),
        )
    }

    fn library() -> Vec<LibrarySongRow> {
        vec![
            row("shared-1", "/music", "Portishead", "Roads", "Dummy"),
            row("sam-1", "/members/sam", "Massive Attack", "Teardrop", "Mezzanine"),
            row("alex-1", "/members/alex", "Bjork", "Joga", "Homogenic"),
        ]
    }

    #[tokio::test]
    async fn listener_search_keeps_outside_song_owned_only_by_another_member() {
        let snapshot = snapshot(library()).with_scope(scope_for_tests(true, &[("alex", 3), ("sam", 4)]));
        let shared = snapshot.current().await.expect("read");
        // Only Sam has Teardrop, so to anyone else it is still an outside song to show.
        assert!(
            shared
                .find(
                    "Teardrop",
                    Some("Massive Attack"),
                    Some(200),
                    &[],
                    Some("Mezzanine")
                )
                .is_none()
        );
        assert!(
            shared
                .same_album(Some("Mezzanine"), Some("Massive Attack"))
                .is_empty()
        );
        assert!(
            shared
                .find("Roads", Some("Portishead"), Some(200), &[], None)
                .is_some()
        );

        let alex = snapshot
            .current_for(&LibraryScope::Viewer("alex".into()))
            .await
            .expect("read");
        assert!(
            alex.find("Joga", Some("Bjork"), Some(200), &[], None).is_some(),
            "their own"
        );
        assert!(
            alex.find("Teardrop", Some("Massive Attack"), Some(200), &[], None)
                .is_none()
        );
        assert!(
            alex.find("Roads", Some("Portishead"), Some(200), &[], None)
                .is_some()
        );
        let sam = snapshot
            .current_for(&LibraryScope::Viewer("Sam".into()))
            .await
            .expect("read");
        assert!(
            sam.find("Teardrop", Some("Massive Attack"), Some(200), &[], None)
                .is_some()
        );
    }

    #[tokio::test]
    async fn without_member_libraries_every_song_counts() {
        let snapshot = snapshot(library()).with_scope(scope_for_tests(false, &[]));
        let all = snapshot.current().await.expect("read");
        assert_eq!(all.song_count(), 3);
        let unscoped = snapshot_unscoped().current().await.expect("read");
        assert_eq!(unscoped.song_count(), 3);
    }

    #[tokio::test]
    async fn with_family_turned_off_members_songs_are_still_not_the_librarys() {
        let snapshot = snapshot(library()).with_scope(scope_for_tests(false, &[("alex", 3), ("sam", 4)]));
        let shared = snapshot.current().await.expect("read");
        assert_eq!(shared.song_count(), 1);
        assert!(
            shared
                .find("Teardrop", Some("Massive Attack"), Some(200), &[], None)
                .is_none()
        );
    }

    fn snapshot_unscoped() -> LibrarySnapshot {
        snapshot(library())
    }
}
