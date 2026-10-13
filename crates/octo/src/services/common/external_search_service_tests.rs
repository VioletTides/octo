//! The external search. Last.fm is a wiremock server answering by the `method` query
//! parameter; the metadata service is a hand-written fake.

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use octo_core::settings::{AppSettings, LastFmSettings, SubsonicSettings};
use tokio::sync::watch;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::services::common::test_fakes::FakeMetadata;

/// A Last.fm whose track.search answers `hits` rows ("Song 1".. by "Artist") and whose
/// artist.gettoptracks answers none.
async fn last_fm(hits: usize) -> (Arc<LastFmService>, MockServer) {
    let server = MockServer::builder().start().await;
    Mock::given(any())
        .respond_with(move |request: &Request| {
            let url = request.url.to_string();
            let tracks: Vec<String> = (1..=hits)
                .map(|i| format!(r#"{{"name":"Song {i}","artist":"Artist"}}"#))
                .collect();
            let body = if url.contains("method=track.search") {
                format!(
                    r#"{{"results":{{"trackmatches":{{"track":[{}]}}}}}}"#,
                    tracks.join(",")
                )
            } else {
                r#"{"toptracks":{"track":[]}}"#.to_string()
            };
            ResponseTemplate::new(200).set_body_string(body)
        })
        .mount(&server)
        .await;
    let settings = Arc::new(SettingsStore::for_tests(AppSettings {
        last_fm: LastFmSettings {
            api_key: "key".into(),
            ..Default::default()
        },
        ..Default::default()
    }));
    let service = Arc::new(LastFmService::with_base_url(
        settings,
        &format!("{}/", server.uri()),
    ));
    (service, server)
}

/// `OneHitLastFm`: every call answers one track.search row, Artist - Song.
async fn one_hit_last_fm() -> (Arc<LastFmService>, MockServer) {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"{"results":{"trackmatches":{"track":[{"name":"Song","artist":"Artist"}]}}}"#,
            ),
        )
        .mount(&server)
        .await;
    let settings = Arc::new(SettingsStore::for_tests(AppSettings {
        last_fm: LastFmSettings {
            api_key: "key".into(),
            ..Default::default()
        },
        ..Default::default()
    }));
    (
        Arc::new(LastFmService::with_base_url(
            settings,
            &format!("{}/", server.uri()),
        )),
        server,
    )
}

fn waiting(wait_for_search_durations: bool) -> Option<Arc<SettingsStore>> {
    Some(Arc::new(SettingsStore::for_tests(AppSettings {
        subsonic: SubsonicSettings {
            wait_for_search_durations,
            ..Default::default()
        },
        ..Default::default()
    })))
}

fn waiting_ms(search_duration_wait_ms: i32) -> Option<Arc<SettingsStore>> {
    Some(Arc::new(SettingsStore::for_tests(AppSettings {
        subsonic: SubsonicSettings {
            search_duration_wait_ms,
            ..Default::default()
        },
        ..Default::default()
    })))
}

/// How many track.search calls Last.fm has had.
async fn track_searches(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.to_string().contains("method=track.search"))
        .count()
}

fn one_song_metadata() -> Arc<FakeMetadata> {
    let metadata = Arc::new(FakeMetadata::default());
    metadata.hits.lock().insert(
        ("Artist".into(), "Song".into()),
        Song {
            artist: "Artist".into(),
            title: "Song".into(),
            ..Default::default()
        },
    );
    metadata
}

/// A gate the durations pass waits on, and the background flag it was called with.
struct DurationsGate {
    open: watch::Sender<bool>,
    started: watch::Receiver<Option<bool>>,
}

fn gate_durations(metadata: &FakeMetadata) -> DurationsGate {
    let (open, opened) = watch::channel(false);
    let (started_tx, started) = watch::channel(None);
    let started_tx = Arc::new(started_tx);
    *metadata.durations.lock() = Some(Arc::new(move |background| {
        started_tx.send_replace(Some(background));
        let mut opened = opened.clone();
        async move {
            let _ = opened.wait_for(|open| *open).await;
        }
        .boxed()
    }));
    DurationsGate { open, started }
}

#[tokio::test]
async fn pads_with_top_tracks_only_when_track_search_is_thin() {
    for (search_hits, expected_top_track_calls) in [(50, 0), (5, 1)] {
        let (last_fm, server) = last_fm(search_hits).await;
        let search = ExternalSearchService::new(Arc::new(FakeMetadata::default()), Some(last_fm), None);

        search.get("artist").await;

        let top_track_calls = server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.to_string().contains("method=artist.gettoptracks"))
            .count();
        assert_eq!(top_track_calls, expected_top_track_calls, "{search_hits} hits");
    }
}

#[tokio::test]
async fn search_waits_for_you_tube_durations_only_when_configured() {
    for wait_for_durations in [true, false] {
        let metadata = one_song_metadata();
        let mut gate = gate_durations(&metadata);
        let (last_fm, _server) = one_hit_last_fm().await;
        let search = Arc::new(ExternalSearchService::new(
            metadata.clone(),
            Some(last_fm),
            waiting(wait_for_durations),
        ));

        let running = search.clone();
        let mut result = tokio::spawn(async move { running.get("song").await });
        let background = tokio::time::timeout(Duration::from_secs(5), gate.started.wait_for(Option::is_some))
            .await
            .expect("the durations pass started")
            .expect("open")
            .expect("a flag");

        // The durations pass is still running, so only a search that does not wait can be done.
        assert_eq!(background, !wait_for_durations);
        if wait_for_durations {
            assert!(!result.is_finished());
        } else {
            let songs = tokio::time::timeout(Duration::from_secs(5), &mut result)
                .await
                .expect("in time")
                .expect("joined");
            assert_eq!(songs.len(), 1);
        }
        gate.open.send_replace(true);
        if wait_for_durations {
            let songs = tokio::time::timeout(Duration::from_secs(5), result)
                .await
                .expect("in time")
                .expect("joined");
            assert_eq!(songs.len(), 1);
        }
    }
}

#[tokio::test]
async fn background_durations_run_before_the_video_prewarm() {
    let metadata = one_song_metadata();
    let mut gate = gate_durations(&metadata);
    let (prewarmed_tx, mut prewarmed) = watch::channel(false);
    let prewarmed_tx = Arc::new(prewarmed_tx);
    *metadata.prewarmed.lock() = Some(Arc::new(move || {
        prewarmed_tx.send_replace(true);
    }));
    let (last_fm, _server) = one_hit_last_fm().await;
    let search = ExternalSearchService::new(metadata.clone(), Some(last_fm), waiting(false));

    tokio::time::timeout(Duration::from_secs(5), search.get("song"))
        .await
        .expect("in time");
    tokio::time::timeout(Duration::from_secs(5), gate.started.wait_for(Option::is_some))
        .await
        .expect("the durations pass started")
        .expect("open");
    assert!(!*prewarmed.borrow());

    gate.open.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), prewarmed.wait_for(|done| *done))
        .await
        .expect("prewarmed")
        .expect("open");
}

/// track.search answers in fuzzy-relevance order; the rows are built most listened first, the
/// live take of a song is folded into the song, and the row in another script comes last.
#[tokio::test]
async fn search_rows_are_built_most_listened_first() {
    let rows = [
        ("Skyfall", "Adele", 1_430_000),
        ("Rolling in the Deep", "Adele", 2_680_000),
        ("Rolling in the Deep (Live)", "Adele", 9_000),
        ("Привет", "Адель", 5_000_000),
        ("Someone Like You", "Adele", 2_200_000),
    ];
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(move |request: &Request| {
            let tracks: Vec<String> = rows
                .iter()
                .map(|(name, artist, listeners)| {
                    format!(r#"{{"name":"{name}","artist":"{artist}","listeners":"{listeners}"}}"#)
                })
                .collect();
            let body = if request.url.to_string().contains("method=track.search") {
                format!(
                    r#"{{"results":{{"trackmatches":{{"track":[{}]}}}}}}"#,
                    tracks.join(",")
                )
            } else {
                r#"{"toptracks":{"track":[]}}"#.to_string()
            };
            ResponseTemplate::new(200).set_body_string(body)
        })
        .mount(&server)
        .await;
    let settings = Arc::new(SettingsStore::for_tests(AppSettings {
        last_fm: LastFmSettings {
            api_key: "key".into(),
            ..Default::default()
        },
        ..Default::default()
    }));
    let last_fm = Arc::new(LastFmService::with_base_url(
        settings,
        &format!("{}/", server.uri()),
    ));
    let metadata = Arc::new(FakeMetadata::default());
    for (title, artist, _) in rows {
        metadata.hits.lock().insert(
            (artist.into(), title.into()),
            Song {
                artist: artist.into(),
                title: title.into(),
                ..Default::default()
            },
        );
    }
    let search = ExternalSearchService::new(metadata, Some(last_fm), waiting(false));

    let songs = search.get("adele").await;

    let titles: Vec<&str> = songs.iter().map(|song| song.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Rolling in the Deep", "Someone Like You", "Skyfall", "Привет"]
    );
}
// ---- empty inputs and limits -------------------------------------------------------------

/// A search waits for the lengths at most its budget, then answers with what it has.
#[tokio::test]
async fn a_search_answers_when_its_wait_for_lengths_runs_out() {
    let metadata = one_song_metadata();
    let mut gate = gate_durations(&metadata);
    let (last_fm, _server) = one_hit_last_fm().await;
    let search = ExternalSearchService::new(metadata.clone(), Some(last_fm), waiting_ms(200));

    let started = std::time::Instant::now();
    let songs = tokio::time::timeout(Duration::from_secs(5), search.get("song"))
        .await
        .expect("answered while the lengths are still being looked up");

    assert_eq!(songs.len(), 1);
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(*gate.started.borrow_and_update(), Some(false));
    gate.open.send_replace(true);
}

/// A wait of 0 is the background mode: the search does not wait at all.
#[tokio::test]
async fn a_wait_of_zero_never_waits() {
    let metadata = one_song_metadata();
    let mut gate = gate_durations(&metadata);
    let (last_fm, _server) = one_hit_last_fm().await;
    let search = ExternalSearchService::new(metadata.clone(), Some(last_fm), waiting_ms(0));

    let songs = tokio::time::timeout(Duration::from_secs(5), search.get("song"))
        .await
        .expect("answered");
    assert_eq!(songs.len(), 1);
    let background = tokio::time::timeout(Duration::from_secs(5), gate.started.wait_for(Option::is_some))
        .await
        .expect("the durations pass started")
        .expect("open")
        .expect("a flag");
    assert!(background);
    gate.open.send_replace(true);
}

/// The same words again, in any case and spacing, answer from the last build: Last.fm is not
/// asked again, the rows take the lengths found since, and the kept list itself is unchanged.
#[tokio::test]
async fn a_repeated_query_answers_from_the_last_build_with_lengths_found_since() {
    let metadata = one_song_metadata();
    let (last_fm, server) = one_hit_last_fm().await;
    let search = ExternalSearchService::new(metadata.clone(), Some(last_fm), waiting(false));

    let first = search.get("song").await;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].duration, None);
    metadata.lengths_now.lock().insert("Song".into(), 245);

    let again = search.get("  SONG ").await;

    assert_eq!(track_searches(&server).await, 1);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].duration, Some(245));
    assert_eq!(first[0].duration, None);
    assert_eq!(search.get("song").await[0].duration, Some(245));
    // Another query is built.
    search.get("song two").await;
    assert_eq!(track_searches(&server).await, 2);
}

/// A build with no rows (Last.fm found nothing, or it failed) is not kept.
#[tokio::test]
async fn a_build_with_no_rows_is_built_again() {
    let (last_fm, server) = last_fm(0).await;
    let search = ExternalSearchService::new(Arc::new(FakeMetadata::default()), Some(last_fm), waiting(false));

    assert!(search.get("nothing").await.is_empty());
    assert!(search.get("nothing").await.is_empty());

    assert_eq!(track_searches(&server).await, 2);
}

#[tokio::test]
async fn album_searches_are_kept_by_query_and_limit() {
    let metadata = Arc::new(FakeMetadata::default());
    metadata.album_hits.lock().push(Album {
        title: "Album".into(),
        ..Default::default()
    });
    let search = ExternalSearchService::new(metadata.clone(), None, None);

    assert_eq!(search.get_albums("Album", 5).await.len(), 1);
    assert_eq!(search.get_albums(" album", 5).await.len(), 1);
    assert_eq!(*metadata.album_searches.lock(), 1);
    search.get_albums("album", 10).await;
    assert_eq!(*metadata.album_searches.lock(), 2);

    let empty = Arc::new(FakeMetadata::default());
    let search = ExternalSearchService::new(empty.clone(), None, None);
    search.get_albums("none", 5).await;
    search.get_albums("none", 5).await;
    assert_eq!(*empty.album_searches.lock(), 2);
}

#[test]
fn kept_results_expire_after_their_time_and_the_oldest_go_past_the_limit() {
    let kept: RecentResults<i32> = RecentResults::new(Duration::from_secs(600), 2);
    let start = Instant::now();
    kept.put(recent_key(" A "), Arc::new(vec![1]), start);
    assert_eq!(
        kept.get("a", start + Duration::from_secs(599)).as_deref(),
        Some(&vec![1])
    );
    assert!(kept.get("a", start + Duration::from_secs(600)).is_none());
    // Gone once expired.
    assert!(kept.get("a", start).is_none());

    kept.put("a".into(), Arc::new(vec![1]), start);
    kept.put("b".into(), Arc::new(vec![2]), start);
    kept.put("a".into(), Arc::new(vec![10]), start);
    kept.put("c".into(), Arc::new(vec![3]), start);
    // "b" was the oldest once "a" was kept again.
    assert!(kept.get("b", start).is_none());
    assert_eq!(kept.get("a", start).as_deref(), Some(&vec![10]));
    assert_eq!(kept.get("c", start).as_deref(), Some(&vec![3]));
    assert_eq!(RECENT_RESULTS_TTL, Duration::from_secs(600));
    assert_eq!(RECENT_RESULTS_MAX, 256);
}

/// No Last.fm key, or a blank query, is no discovery; a blank album query or limit is none.
#[tokio::test]
async fn nothing_to_search_with_is_no_results() {
    let metadata = one_song_metadata();
    let search = ExternalSearchService::new(metadata.clone(), None, None);
    assert!(search.get("song").await.is_empty());
    let (last_fm, _server) = one_hit_last_fm().await;
    let search = ExternalSearchService::new(metadata, Some(last_fm), None);
    assert!(search.get("   ").await.is_empty());
    assert!(search.get_albums("", 5).await.is_empty());
    assert!(search.get_albums("song", 0).await.is_empty());
}

/// A Last.fm whose track.search answers one row, Artist - Song, after `delay`.
async fn slow_last_fm(delay: Duration) -> (Arc<LastFmService>, MockServer) {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"{"results":{"trackmatches":{"track":[{"name":"Song","artist":"Artist"}]}}}"#,
                )
                .set_delay(delay),
        )
        .mount(&server)
        .await;
    let settings = Arc::new(SettingsStore::for_tests(AppSettings {
        last_fm: LastFmSettings {
            api_key: "key".into(),
            ..Default::default()
        },
        ..Default::default()
    }));
    (
        Arc::new(LastFmService::with_base_url(
            settings,
            &format!("{}/", server.uri()),
        )),
        server,
    )
}

fn last_fm_wait(ms: i32) -> Option<Arc<SettingsStore>> {
    Some(Arc::new(SettingsStore::for_tests(AppSettings {
        subsonic: SubsonicSettings {
            wait_for_search_durations: false,
            search_last_fm_wait_ms: ms,
            ..Default::default()
        },
        ..Default::default()
    })))
}

fn catalog_metadata() -> Arc<FakeMetadata> {
    let metadata = one_song_metadata();
    metadata.hits.lock().insert(
        ("Deezer Artist".into(), "Deezer Song".into()),
        Song {
            artist: "Deezer Artist".into(),
            title: "Deezer Song".into(),
            ..Default::default()
        },
    );
    *metadata.catalog_tracks.lock() = Some(vec![
        ("Deezer Artist".into(), "Deezer Song".into()),
        ("deezer artist".into(), "DEEZER SONG".into()),
    ]);
    metadata
}

/// Last.fm late: the rows are the catalog's own track search, deduped, without waiting for
/// Last.fm. Its answer is kept, and the next search of the words uses it without asking again.
#[tokio::test]
async fn a_late_last_fm_gives_way_to_the_catalog_track_search_and_is_kept_for_next_time() {
    let metadata = catalog_metadata();
    let (last_fm, server) = slow_last_fm(Duration::from_millis(800)).await;
    let search = ExternalSearchService::new(metadata, Some(last_fm), last_fm_wait(150));

    let started = std::time::Instant::now();
    let first = search.get("song").await;
    assert!(
        started.elapsed() < Duration::from_millis(700),
        "{:?}",
        started.elapsed()
    );
    let titles: Vec<&str> = first.iter().map(|song| song.title.as_str()).collect();
    assert_eq!(titles, ["Deezer Song"]);

    // Once Last.fm has answered, the kept build gives way and the next search uses its answer.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let next = search.get("song").await;
    let titles: Vec<&str> = next.iter().map(|song| song.title.as_str()).collect();
    assert_eq!(titles, ["Song"]);
    assert_eq!(track_searches(&server).await, 1);
}

/// Last.fm late and the catalog with nothing: Last.fm is waited for after all.
#[tokio::test]
async fn a_late_last_fm_is_waited_for_when_the_catalog_has_nothing() {
    let metadata = one_song_metadata();
    let (last_fm, _server) = slow_last_fm(Duration::from_millis(400)).await;
    let search = ExternalSearchService::new(metadata, Some(last_fm), last_fm_wait(100));

    let songs = search.get("song").await;

    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0].title, "Song");
}

/// A wait of 0 waits for Last.fm however long it takes.
#[tokio::test]
async fn a_last_fm_wait_of_zero_never_uses_the_catalog_search() {
    let metadata = catalog_metadata();
    let (last_fm, _server) = slow_last_fm(Duration::from_millis(300)).await;
    let search = ExternalSearchService::new(metadata, Some(last_fm), last_fm_wait(0));

    let songs = search.get("song").await;

    assert_eq!(songs[0].title, "Song");
}

/// Album ownership is let go once the song build has looked its rows up, while the build
/// still waits for lengths; with the songs kept from before, at once.
#[tokio::test]
async fn row_lookups_are_done_before_the_wait_for_lengths_and_at_once_from_the_kept_build() {
    let metadata = one_song_metadata();
    let gate = gate_durations(&metadata);
    let (last_fm, _server) = one_hit_last_fm().await;
    let search = Arc::new(ExternalSearchService::new(
        metadata,
        Some(last_fm),
        waiting_ms(5000),
    ));

    let looked_up = search.rows_looked_up("song");
    let running = search.clone();
    let build = tokio::spawn(async move { running.get("song").await });
    tokio::time::timeout(Duration::from_secs(3), looked_up)
        .await
        .expect("let go while the lengths are still awaited");
    assert!(!build.is_finished());
    gate.open.send_replace(true);
    build.await.expect("built");

    let again = search.rows_looked_up("song");
    search.get("song").await;
    tokio::time::timeout(Duration::from_millis(200), again)
        .await
        .expect("at once from the kept build");
}
