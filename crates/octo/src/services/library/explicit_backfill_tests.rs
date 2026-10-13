//! The explicit backfill's run: the preview writes nothing, Apply writes only explicit and clean
//! in place and journals each write, a file changed since the preview is left alone, and Undo
//! takes every mark out again. The lookup's tests are in `octo_core::tagging::explicit_lookup`;
//! the dashboard's calls are at the bottom.

use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request};
use octo_core::metadata::deezer_metadata_service::{CatalogTrackAnswer, CatalogTrackFacts};
use octo_core::settings::AppSettings;
use tower::ServiceExt;

use super::*;
use crate::app::AppState;
use crate::services::test_support::{flac, mp3};

/// A catalog that answers from fixed lists.
#[derive(Default)]
struct FakeCatalog {
    by_name: HashMap<String, CatalogTrackAnswer>,
    searches: AtomicUsize,
}

#[async_trait]
impl IExplicitCatalog for FakeCatalog {
    async fn by_isrc(&self, _: &str) -> CatalogTrackAnswer {
        CatalogTrackAnswer::default()
    }

    async fn search(&self, artist: Option<&str>, title: Option<&str>) -> CatalogTrackAnswer {
        self.searches.fetch_add(1, Ordering::SeqCst);
        let key = format!("{}|{}", artist.unwrap_or(""), title.unwrap_or("")).to_lowercase();
        self.by_name.get(&key).cloned().unwrap_or_default()
    }
}

fn track(id: &str, title: &str, artist: &str, content: i32) -> CatalogTrackFacts {
    // The fixtures are two seconds long, so the catalog's lengths match them.
    CatalogTrackFacts {
        track_id: id.into(),
        title: title.into(),
        artist: artist.into(),
        duration: Some(2),
        explicit_content: Some(content),
        ..Default::default()
    }
}

struct Root {
    dir: tempfile::TempDir,
}

impl Root {
    fn new() -> Root {
        Root {
            dir: tempfile::tempdir().expect("a temp dir"),
        }
    }

    fn file(&self, name: &str, bytes: Vec<u8>, artist: &str, title: &str, advisory: Option<i32>) -> String {
        let path = self.dir.path().join(name);
        std::fs::write(&path, bytes).expect("written");
        let mut file = TagFile::open(&path).expect("opens");
        file.set_performers(&[artist.to_string()]);
        file.set_title(Some(title));
        if advisory.is_some() {
            tag_writer_extras::set_advisory(&mut file, advisory);
        }
        file.save().expect("saved");
        path.to_string_lossy().into_owned()
    }

    fn flac(&self, name: &str, artist: &str, title: &str) -> String {
        self.file(name, flac(), artist, title, None)
    }

    fn mp3(&self, name: &str, artist: &str, title: &str) -> String {
        self.file(name, mp3(), artist, title, None)
    }

    /// The worker over these songs (each with Navidrome's explicit status), and its journal.
    fn worker(
        &self,
        songs: &[(&str, &str)],
    ) -> (Arc<ExplicitBackfill>, Arc<TagEditJournal>, Arc<FakeCatalog>) {
        let mut catalog = FakeCatalog::default();
        for (key, found) in [
            ("logic|america", vec![track("1", "America", "Logic", 1)]),
            ("edit|radio", vec![track("2", "Radio", "Edit", 3)]),
            ("john mayer|gravity", vec![track("3", "Gravity", "John Mayer", 0)]),
            (
                "kendrick lamar|squabble up",
                vec![
                    track("4", "squabble up", "Kendrick Lamar", 1),
                    track("5", "squabble up", "Kendrick Lamar", 3),
                ],
            ),
        ] {
            catalog
                .by_name
                .insert(key.into(), CatalogTrackAnswer::found(found));
        }
        let catalog = Arc::new(catalog);
        let journal = Arc::new(TagEditJournal::new(Some(self.dir.path().join("tag-edits.json"))));
        let listed: Vec<(ExplicitBackfillSong, Option<String>)> = songs
            .iter()
            .map(|(path, status)| {
                (
                    ExplicitBackfillSong {
                        path: path.to_string(),
                        navidrome_id: Some(format!("nd-{}", file_stem(path))),
                    },
                    Some(status.to_string()),
                )
            })
            .collect();
        let worker = ExplicitBackfill::new(
            Arc::new(ExplicitBackfillStore::new(None)),
            journal.clone(),
            catalog.clone(),
            Arc::new(SettingsStore::for_tests(AppSettings::default())),
            ExplicitBackfillServices::default(),
        )
        .for_tests(Arc::new(move || {
            let listed = listed.clone();
            Box::pin(async move { listed })
        }));
        (Arc::new(worker), journal, catalog)
    }
}

fn advisory_of(path: &str) -> Option<i32> {
    tag_writer_extras::read_advisory(&TagFile::open(path).expect("opens"))
}

#[tokio::test]
async fn preview_looks_up_only_unmarked_songs_and_writes_nothing() {
    let root = Root::new();
    let america = root.flac("america.flac", "Logic", "America");
    let marked = root.file("marked.flac", flac(), "Logic", "America", Some(1));
    let navidrome_marked = root.flac("e.flac", "Logic", "America");
    let bytes = std::fs::read(&america).expect("read");
    let (worker, _, catalog) = root.worker(&[(&america, ""), (&marked, ""), (&navidrome_marked, "e")]);

    worker
        .run_to_end(ExplicitBackfillRequest::of(ExplicitBackfillMode::Preview))
        .await;

    let run = worker.current();
    assert_eq!(run.status, ExplicitBackfillStatus::Completed);
    assert_eq!(run.explicit, 1);
    assert_eq!(run.already_marked, 2);
    assert_eq!(catalog.searches.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(&america).expect("read"), bytes);
    assert!(run.can_apply());
}

#[tokio::test]
async fn apply_writes_explicit_and_clean_in_place_journals_each_and_undo_takes_them_out() {
    let root = Root::new();
    let america = root.flac("america.flac", "Logic", "America");
    let radio = root.mp3("radio.mp3", "Edit", "Radio");
    let gravity = root.flac("gravity.flac", "John Mayer", "Gravity");
    let squabble = root.flac("squabble.flac", "Kendrick Lamar", "squabble up");
    let changed = root.flac("changed.flac", "Logic", "America");
    let (worker, journal, _) = root.worker(&[
        (&america, ""),
        (&radio, ""),
        (&gravity, ""),
        (&squabble, ""),
        (&changed, ""),
    ]);

    worker
        .run_to_end(ExplicitBackfillRequest::of(ExplicitBackfillMode::Preview))
        .await;
    let preview = worker.current();
    assert_eq!(
        (
            preview.explicit,
            preview.clean,
            preview.not_explicit,
            preview.unsure
        ),
        (2, 1, 1, 1)
    );
    // Changed after the preview: Apply must leave it alone.
    {
        let mut file = TagFile::open(&changed).expect("opens");
        file.set_title(Some("America (edited by hand)"));
        file.save().expect("saved");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&changed)
            .and_then(|f| f.set_modified(later))
            .expect("touched");
    }

    worker
        .run_to_end(ExplicitBackfillRequest::new(
            ExplicitBackfillMode::Apply,
            "winters",
        ))
        .await;

    assert_eq!(advisory_of(&america), Some(1));
    assert_eq!(advisory_of(&radio), Some(2));
    assert_eq!(advisory_of(&gravity), None);
    assert_eq!(advisory_of(&squabble), None);
    assert_eq!(advisory_of(&changed), None);
    let applied = worker.current();
    assert_eq!((applied.written, applied.left_alone), (2, 1));
    let entries = journal.of_run(applied.applied_run_id.as_deref().expect("an applied run"));
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.kind == tag_edit_kinds::ADVISORY));
    assert!(entries.iter().any(|entry| entry.navidrome_id == "nd-america"
        && entry.after[tag_edit_kinds::ADVISORY].as_deref() == Some("1")
        && entry.before[tag_edit_kinds::ADVISORY].is_none()
        && entry.username == "winters"));
    assert!(root.dir.path().join("tag-edits.json").is_file());
    assert!(worker.can_undo());

    worker
        .run_to_end(ExplicitBackfillRequest::of(ExplicitBackfillMode::Undo))
        .await;

    assert_eq!(advisory_of(&america), None);
    assert_eq!(advisory_of(&radio), None);
    assert!(
        journal
            .of_run(applied.applied_run_id.as_deref().expect("an applied run"))
            .is_empty()
    );
    assert!(!worker.can_undo());
    assert_eq!(worker.current().written, 2);
}

/// Port fix: the walk the marking falls back to when Navidrome cannot list the songs leaves the
/// trash out, and a trashed song Navidrome lists (a trash folder with no leading dot) too.
#[tokio::test]
async fn the_songs_looked_at_leave_the_trash_out() {
    let root = Root::new();
    let music = root.dir.path().join("music");
    std::fs::create_dir_all(music.join("Bin/2026-10-08")).expect("made");
    std::fs::write(music.join("Song.flac"), flac()).expect("written");
    std::fs::write(music.join("Bin/2026-10-08/Gone.flac"), flac()).expect("written");
    let mut settings = AppSettings::default();
    settings.library_actions.quarantine_directory = "Bin".to_string();
    let store = Arc::new(SettingsStore::for_tests(settings.clone()));
    store.set_raw("Library:DownloadPath", Some(&music.to_string_lossy()));
    let worker = ExplicitBackfill::new(
        Arc::new(ExplicitBackfillStore::new(None)),
        Arc::new(TagEditJournal::new(None)),
        Arc::new(FakeCatalog::default()),
        store,
        ExplicitBackfillServices::default(),
    );

    let walked = worker
        .songs(&CancellationToken::new())
        .await
        .ok()
        .expect("listed");
    let listed = from_navidrome(
        HashMap::from([
            (
                music.join("Song.flac").to_string_lossy().into_owned(),
                navidrome_entry(&music.join("Song.flac")),
            ),
            (
                music
                    .join("Bin/2026-10-08/Gone.flac")
                    .to_string_lossy()
                    .into_owned(),
                navidrome_entry(&music.join("Bin/2026-10-08/Gone.flac")),
            ),
        ]),
        &library_walk::trash_folder(&music, &settings.library_actions),
    );

    let names = |songs: &[(ExplicitBackfillSong, Option<String>)]| -> Vec<String> {
        songs.iter().map(|(song, _)| file_stem(&song.path)).collect()
    };
    assert_eq!(names(&walked), ["Song"]);
    assert_eq!(names(&listed), ["Song"]);
}

fn navidrome_entry(path: &Path) -> NavidromeSongEntry {
    NavidromeSongEntry {
        full_path: path.to_string_lossy().into_owned(),
        album_id: None,
        artist: None,
        title: None,
        album: None,
        lyrics: None,
        id: Some(format!("nd-{}", file_stem(&path.to_string_lossy()))),
        duration: None,
        explicit_status: Some(String::new()),
        missing: false,
    }
}

// ---- The dashboard's calls ----------------------------------------------------------------

#[tokio::test]
async fn endpoints_need_a_sign_in_and_apply_needs_the_music_path_typed() {
    let state = AppState::for_tests(AppSettings::default());
    state.explicit_backfill.store().replace(ExplicitBackfillRun {
        run_id: "r1".into(),
        status: ExplicitBackfillStatus::Completed,
        mode: ExplicitBackfillMode::Preview,
        explicit: 1,
        rows: vec![ExplicitBackfillRow {
            path: "/music/a.flac".into(),
            artist: Some("Logic".into()),
            title: Some("America".into()),
            outcome: "explicit".into(),
            how: "same ISRC".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    let app = crate::http::pipeline::build(
        state.clone(),
        &crate::http::static_files::StaticAssets::default(),
        0,
    );
    let token = state.browse_sessions.create("admin");
    let send = |request: Request<Body>| {
        let app = app.clone();
        async move {
            let response = app.oneshot(request).await.expect("an answer");
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("a body");
            (status, String::from_utf8_lossy(&body).into_owned())
        }
    };

    let (anonymous, _) = send(
        Request::get("/api/admin/explicit/backfill")
            .body(Body::empty())
            .expect("a request"),
    )
    .await;
    let (status, body) = send(
        Request::get("/api/admin/explicit/backfill")
            .header("X-Octo-Browse-Token", &token)
            .body(Body::empty())
            .expect("a request"),
    )
    .await;
    let (refused, refusal) = send(
        Request::builder()
            .method(Method::POST)
            .uri("/api/admin/explicit/backfill/apply")
            .header("X-Octo-Browse-Token", &token)
            .header("X-Octo-Admin", "1")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"confirm":"wrong"}"#))
            .expect("a request"),
    )
    .await;

    assert_eq!(anonymous, axum::http::StatusCode::UNAUTHORIZED);
    assert_eq!(status, axum::http::StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(json["explicit"], 1);
    assert_eq!(json["toWrite"], 1);
    assert_eq!(json["canApply"], true);
    assert_eq!(json["rows"][0]["outcome"], "explicit");
    assert_eq!(refused, axum::http::StatusCode::BAD_REQUEST);
    assert!(refusal.contains("type the music path"), "{refusal}");
}

/// While members have libraries of their own (Family on, or turned off since), explicit marks
/// are only ever written to the shared library.
#[test]
fn explicit_marks_keep_to_the_shared_library() {
    let worker_with = |family_on: bool, members: &[(&str, i64)]| {
        let dir = tempfile::tempdir().expect("a temp dir");
        ExplicitBackfill::new(
            Arc::new(ExplicitBackfillStore::new(None)),
            Arc::new(TagEditJournal::new(Some(dir.path().join("tag-edits.json")))),
            Arc::new(FakeCatalog::default()),
            Arc::new(SettingsStore::for_tests(AppSettings::default())),
            ExplicitBackfillServices::default(),
        )
        .with_library_scope(crate::services::family::library_index::scope_for_tests(
            family_on, members,
        ))
    };
    let listed = vec![
        (
            ExplicitBackfillSong {
                path: "/music/A/a.flac".into(),
                navidrome_id: Some("a".into()),
            },
            None,
        ),
        (
            ExplicitBackfillSong {
                path: "/members/alex/A/b.flac".into(),
                navidrome_id: Some("b".into()),
            },
            None,
        ),
    ];
    for family_on in [true, false] {
        let kept = worker_with(family_on, &[("alex", 3)]).shared_only(listed.clone());
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].0.path, "/music/A/a.flac");
    }
    assert_eq!(worker_with(false, &[]).shared_only(listed).len(), 2);
}
