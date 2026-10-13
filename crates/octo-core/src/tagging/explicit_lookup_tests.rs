//! The lookup half of the explicit backfill: only a confident catalog match counts. The run
//! on real files and the dashboard's calls are in `octo::services::library::explicit_backfill`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn track(
    id: &str,
    title: &str,
    artist: &str,
    content: Option<i32>,
    seconds: Option<i32>,
    album: Option<&str>,
    isrc: Option<&str>,
) -> CatalogTrackFacts {
    CatalogTrackFacts {
        track_id: id.into(),
        title: title.into(),
        artist: artist.into(),
        album: album.map(str::to_string),
        duration: seconds,
        isrc: isrc.map(str::to_string),
        explicit_content: content,
    }
}

/// A catalog track from its id, title, artist and content flag: 200 s, no album, no code.
fn plain(id: &str, title: &str, artist: &str, content: Option<i32>) -> CatalogTrackFacts {
    track(id, title, artist, content, Some(200), None, None)
}

fn song(
    artist: &str,
    title: &str,
    album: Option<&str>,
    seconds: Option<f64>,
    isrcs: &[&str],
) -> LibrarySongFacts {
    LibrarySongFacts {
        artist: Some(artist.into()),
        title: Some(title.into()),
        album: album.map(str::to_string),
        seconds,
        isrcs: isrcs.iter().map(|code| code.to_string()).collect(),
        file_name: None,
    }
}

/// A catalog that answers from fixed lists.
#[derive(Default)]
struct FakeCatalog {
    by_isrc: HashMap<String, CatalogTrackAnswer>,
    by_name: HashMap<String, CatalogTrackAnswer>,
    searches: AtomicUsize,
}

#[async_trait]
impl IExplicitCatalog for FakeCatalog {
    async fn by_isrc(&self, isrc: &str) -> CatalogTrackAnswer {
        self.by_isrc.get(isrc).cloned().unwrap_or_default()
    }

    async fn search(&self, artist: Option<&str>, title: Option<&str>) -> CatalogTrackAnswer {
        self.searches.fetch_add(1, Ordering::SeqCst);
        let key = format!("{}|{}", artist.unwrap_or(""), title.unwrap_or("")).to_lowercase();
        self.by_name.get(&key).cloned().unwrap_or_default()
    }
}

#[test]
fn decide_every_copy_that_fits_says_explicit_is_explicit() {
    let finding = decide(
        &song("Logic", "America", Some("Everybody"), Some(200.0), &[]),
        &[
            track(
                "1",
                "America",
                "Logic",
                Some(1),
                Some(200),
                Some("Everybody"),
                None,
            ),
            track(
                "2",
                "America",
                "Logic",
                Some(1),
                Some(200),
                Some("Everybody (Deluxe)"),
                None,
            ),
        ],
    );

    assert_eq!(finding.outcome, ExplicitOutcome::Explicit);
    assert_eq!(finding.advisory(), Some(1));
    assert_eq!(finding.how, "artist, title, length and album, 2 copies agree");
}

/// The catalog lists a clean edit beside the original under the same name and length; with
/// nothing to tell them apart, nothing is written.
#[test]
fn decide_an_explicit_and_a_clean_copy_both_fit_is_unsure() {
    let finding = decide(
        &song("Kendrick Lamar", "squabble up", Some("GNX"), Some(157.0), &[]),
        &[
            track(
                "1",
                "squabble up",
                "Kendrick Lamar",
                Some(1),
                Some(157),
                Some("GNX"),
                None,
            ),
            track(
                "2",
                "squabble up",
                "Kendrick Lamar",
                Some(3),
                Some(157),
                Some("GNX"),
                None,
            ),
        ],
    );

    assert_eq!(finding.outcome, ExplicitOutcome::Unsure);
    assert_eq!(finding.advisory(), None);
    assert_eq!(finding.how, "an explicit and a clean copy both fit");
}

#[test]
fn decide_the_copy_on_the_songs_own_album_speaks_for_it() {
    let finding = decide(
        &song("A", "Song", Some("Real Album"), Some(200.0), &[]),
        &[
            track(
                "1",
                "Song",
                "A",
                Some(3),
                Some(200),
                Some("Radio Hits 2020"),
                None,
            ),
            track("2", "Song", "A", Some(1), Some(200), Some("Real Album"), None),
        ],
    );

    assert_eq!(finding.outcome, ExplicitOutcome::Explicit);
    assert_eq!(finding.catalog_track_id.as_deref(), Some("2"));
}

#[test]
fn decide_another_length_or_another_version_is_not_the_song() {
    assert_eq!(
        decide(
            &song("A", "Song", None, Some(200.0), &[]),
            &[track("1", "Song", "A", Some(1), Some(260), None, None)]
        )
        .outcome,
        ExplicitOutcome::Unsure
    );
    assert_eq!(
        decide(
            &song("A", "Song", None, Some(200.0), &[]),
            &[plain("1", "Song (Live)", "A", Some(1))]
        )
        .outcome,
        ExplicitOutcome::Unsure
    );
}

#[test]
fn decide_not_explicit_and_unsaid() {
    assert_eq!(
        decide(
            &song("John Mayer", "Gravity", None, Some(200.0), &[]),
            &[plain("1", "Gravity", "John Mayer", Some(0))]
        )
        .outcome,
        ExplicitOutcome::NotExplicit
    );
    assert_eq!(
        decide(
            &song("A", "B", None, Some(200.0), &[]),
            &[plain("1", "B", "A", None)]
        )
        .how,
        "the catalog does not say"
    );
}

#[tokio::test]
async fn find_the_isrc_speaks_first_and_a_name_saying_clean_before_that() {
    let mut catalog = FakeCatalog::default();
    catalog.by_isrc.insert(
        "USUM71702791".into(),
        CatalogTrackAnswer::found(vec![track(
            "9",
            "AfricAryaN",
            "Logic",
            Some(1),
            Some(200),
            None,
            Some("USUM71702791"),
        )]),
    );
    catalog.by_name.insert(
        "logic|africaryan".into(),
        CatalogTrackAnswer::found(vec![plain("8", "AfricAryaN", "Logic", Some(3))]),
    );

    let by_code = find(
        &song("Logic", "AfricAryaN", None, Some(200.0), &["usum71702791"]),
        &catalog,
    )
    .await;
    let by_name = find(
        &song("Logic", "AfricAryaN (Clean)", None, Some(200.0), &[]),
        &catalog,
    )
    .await;

    assert_eq!(by_code.outcome, ExplicitOutcome::Explicit);
    assert_eq!(by_code.how, "same ISRC (USUM71702791)");
    assert_eq!(catalog.searches.load(Ordering::SeqCst), 0);
    assert_eq!(by_name.outcome, ExplicitOutcome::Clean);
}

#[tokio::test]
async fn find_a_busy_catalog_is_said_so_not_taken_for_an_answer() {
    let mut catalog = FakeCatalog::default();
    catalog.by_name.insert("a|b".into(), CatalogTrackAnswer::busy());

    let finding = find(&song("A", "B", None, Some(200.0), &[]), &catalog).await;

    assert!(finding.catalog_busy);
    assert_eq!(finding.advisory(), None);
}
