//! Wrong lyrics and Wrong cover through the executor: a stand-in Navidrome (wiremock) resolves
//! the song, the lyrics sources and the cover finder are fakes, and the download queue is
//! watched so neither answer ever fetches the song again.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use async_trait::async_trait;
use octo_core::lyrics::{ILyricsSource, LyricsLookup};
use octo_core::settings::{
    AppSettings, LibraryActionDefinition, LibraryActionSettings, MetadataSettings, SubsonicSettings,
};
use octo_media::tags::{FRONT_COVER, TagFile, TagPicture};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{any, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::services::cover_art::{
    AlbumCoverQuery, CoverUpgradeJournal, CoverUpgradeServices, CoverUpgradeStore, FoundCover,
    IAlbumCoverFinder,
};
use crate::services::library::library_action_test_support::{Extras, RecordingLibrary, executor, store};
use crate::services::library::library_write_gate::LibraryWriteGate;

const FILE_NAME: &str = "Massive Attack - Teardrop";
const LINES: &str = "[00:01.00]love, love is a verb";

/// Twenty silent MPEG-1 Layer III frames, enough for the tags to be written.
fn mp3() -> Vec<u8> {
    let mut bytes = vec![0u8; 417 * 20];
    for frame in 0..20 {
        bytes[frame * 417..frame * 417 + 4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x64]);
    }
    bytes
}

fn jpeg_of(image: image::RgbImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .expect("a JPEG encodes");
    out.into_inner()
}

/// A left-to-right ramp.
fn ramp(side: u32) -> Vec<u8> {
    jpeg_of(image::RgbImage::from_fn(side, side, |x, _| {
        image::Rgb([(255 * x / side) as u8; 3])
    }))
}

/// Eight bars: looks nothing like the ramp.
fn bars(side: u32) -> Vec<u8> {
    jpeg_of(image::RgbImage::from_fn(side, side, |x, _| {
        image::Rgb([(255 * ((x / (side / 8)) % 2)) as u8; 3])
    }))
}

/// A lyrics source with one entry, which its search finds and its id fetches again.
struct OneEntry {
    key: &'static str,
    candidate: &'static str,
    asked: AtomicUsize,
}

impl OneEntry {
    fn new(key: &'static str, candidate: &'static str) -> Arc<Self> {
        Arc::new(Self {
            key,
            candidate,
            asked: AtomicUsize::new(0),
        })
    }

    fn lookup(&self) -> LyricsLookup {
        LyricsLookup::new(
            Some(
                LyricsResult::new(self.key.to_uppercase(), Some(LINES.to_string()), None, false)
                    .with_candidate_id(self.candidate),
            ),
            false,
        )
    }
}

#[async_trait]
impl ILyricsSource for OneEntry {
    fn key(&self) -> &str {
        self.key
    }

    async fn find(&self, _query: &LyricsQuery, _ct: &CancellationToken) -> LyricsLookup {
        self.asked.fetch_add(1, AtomicOrdering::SeqCst);
        self.lookup()
    }

    async fn fetch(&self, id: &str, _ct: &CancellationToken) -> LyricsLookup {
        if self.candidate.ends_with(id) {
            self.lookup()
        } else {
            LyricsLookup::miss()
        }
    }
}

/// Always offers the same cover, passing over it when it is turned down.
struct OneCover(Vec<u8>);

#[async_trait]
impl IAlbumCoverFinder for OneCover {
    async fn find(&self, _query: &AlbumCoverQuery) -> Option<FoundCover> {
        Some(FoundCover::new(self.0.clone(), "iTunes", 1200))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    original: String,
    queue: Arc<TrackAcquisitionQueue>,
    journal: Arc<LibraryActionJournal>,
    rejections: Arc<RejectionStore>,
    choices: Arc<LyricsChoiceService>,
    covers: Arc<CoverUpgradeWorker>,
    executor: Arc<LibraryActionExecutor>,
    gate: LibraryWriteGate,
    root: PathBuf,
    sidecars: Arc<LyricsSidecarWriter>,
    undo: Arc<crate::services::lyrics::LyricsUndoJournal>,
    _navidrome: MockServer,
}

impl Fixture {
    async fn new(action: LibraryAction, front: Option<&[u8]>, cover: Vec<u8>) -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let root: PathBuf = dir.path().join("music");
        std::fs::create_dir_all(&root).expect("the music root");
        let config = dir.path().join("config");
        let original = root.join(format!("{FILE_NAME}.mp3"));
        std::fs::write(&original, mp3()).expect("written");
        {
            let mut file = TagFile::open(&original).expect("an MP3");
            file.set_title(Some("Teardrop"));
            file.set_album(Some("Mezzanine"));
            file.set_performers(&["Massive Attack".to_string()]);
            file.set_album_artists(&["Massive Attack".to_string()]);
            if let Some(front) = front {
                file.set_pictures(&[TagPicture {
                    picture_type: FRONT_COVER,
                    mime_type: "image/jpeg".into(),
                    description: String::new(),
                    data: front.to_vec(),
                }]);
            }
            file.save().expect("saved");
        }
        let size = std::fs::metadata(&original).expect("there").len();

        let navidrome = MockServer::start().await;
        Mock::given(path("/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"token":"jwt","isAdmin":true,"username":"admin"}"#,
                "application/json",
            ))
            .mount(&navidrome)
            .await;
        Mock::given(path("/api/song/nd-1"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"id":"nd-1","path":"{FILE_NAME}.mp3","size":{size},"title":"Teardrop","artist":"Massive Attack","album":"Mezzanine","suffix":"mp3","duration":330}}"#
                ),
                "application/json",
            ))
            .mount(&navidrome)
            .await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_raw("[]", "application/json"))
            .mount(&navidrome)
            .await;

        let settings = store(AppSettings {
            library_actions: LibraryActionSettings {
                enabled: true,
                dry_run: false,
                allowed_users: vec!["alice".into()],
                actions: vec![LibraryActionDefinition {
                    action,
                    enabled: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            subsonic: SubsonicSettings {
                url: Some(navidrome.uri()),
                admin_username: Some("admin".into()),
                admin_password: Some("secret".into()),
                auto_detect_download_path: false,
                ..Default::default()
            },
            metadata: MetadataSettings {
                lyrics_sources: "kugou,lrclib".into(),
                embed_full_size_covers: true,
                ..MetadataSettings::default()
            },
            ..Default::default()
        });
        settings.set_raw("Library:DownloadPath", Some(&root.to_string_lossy()));

        let lyrics = Arc::new(LyricsService::new(
            vec![
                OneEntry::new("kugou", "kugou:1.a") as Arc<dyn ILyricsSource>,
                OneEntry::new("lrclib", "lrclib:9") as Arc<dyn ILyricsSource>,
            ],
            settings.clone(),
        ));
        let choices = Arc::new(LyricsChoiceService::new(
            lyrics.clone(),
            Arc::new(crate::services::lyrics::LyricsChoiceStore::new(None)),
        ));
        let gate = LibraryWriteGate::new();
        let covers = Arc::new(CoverUpgradeWorker::new(
            Arc::new(CoverUpgradeStore::new(None)),
            Arc::new(CoverUpgradeJournal::new(Some(
                config.join("cover-upgrade-journal.jsonl"),
            ))),
            Arc::new(OneCover(cover)),
            settings.clone(),
            CoverUpgradeServices {
                write_gate: Some(gate.clone()),
                ..CoverUpgradeServices::default()
            },
        ));
        let sidecars = Arc::new(LyricsSidecarWriter::new(lyrics.clone(), settings.clone()));
        sidecars.set_write_gate(gate.clone());
        let undo = Arc::new(crate::services::lyrics::LyricsUndoJournal::new(None));
        sidecars.set_undo_journal(undo.clone());
        let rejections = Arc::new(RejectionStore::new(Some(config.join("rejections.json"))));
        let queue = Arc::new(TrackAcquisitionQueue::new());
        let journal = Arc::new(LibraryActionJournal::new());
        let executor = executor(
            &settings,
            Extras {
                journal: Some(journal.clone()),
                library: Some(Arc::new(RecordingLibrary::default())),
                queue: Some(queue.clone()),
                answers: Some(SongAnswers {
                    lyrics: lyrics.clone(),
                    choices: choices.clone(),
                    sidecars: sidecars.clone(),
                    covers: covers.clone(),
                    rejections: rejections.clone(),
                }),
                ..Default::default()
            },
        );
        Fixture {
            original: original.to_string_lossy().into_owned(),
            _dir: dir,
            queue,
            journal,
            rejections,
            choices,
            covers,
            executor,
            gate,
            root,
            sidecars,
            undo,
            _navidrome: navidrome,
        }
    }

    async fn press(&self, action: LibraryAction) -> LibraryActionOutcome {
        self.executor
            .apply(LibraryActionRequest::new(action, "nd-1", "alice"))
            .await
            .expect("an outcome")
    }

    fn front(&self) -> Vec<u8> {
        TagFile::open(&self.original)
            .expect("opens")
            .pictures()
            .into_iter()
            .find(|picture| picture.picture_type == FRONT_COVER)
            .map(|picture| picture.data)
            .unwrap_or_default()
    }

    fn never_downloaded(&self) {
        assert!(self.queue.is_idle(), "nothing was queued for download");
    }
}

#[tokio::test]
async fn wrong_lyrics_pins_the_next_source_and_never_downloads_the_song() {
    let t = Fixture::new(LibraryAction::WrongLyrics, None, Vec::new()).await;
    let before = std::fs::read(&t.original).expect("read");

    let outcome = t.press(LibraryAction::WrongLyrics).await;

    assert_eq!(outcome.state, LibraryActionState::Applied, "{outcome:?}");
    assert_eq!(outcome.detail.as_deref(), Some("Now showing lyrics from LRCLIB."));
    assert_eq!(
        t.choices.pin_for("nd-1").map(|pin| pin.choice).as_deref(),
        Some("lrclib:9")
    );
    assert!(t.rejections.rejected_lyrics("nd-1").contains("kugou:1.a"));
    assert_eq!(
        before,
        std::fs::read(&t.original).expect("read"),
        "the audio is untouched"
    );
    t.never_downloaded();
    let entries = t.journal.recent(200);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].action, LibraryAction::WrongLyrics);
    assert_eq!(entries[0].state, LibraryActionState::Applied);
}

#[tokio::test]
async fn a_rejected_lyrics_candidate_is_never_offered_again_and_the_last_one_hides_them() {
    let t = Fixture::new(LibraryAction::WrongLyrics, None, Vec::new()).await;

    assert_eq!(
        t.press(LibraryAction::WrongLyrics).await.state,
        LibraryActionState::Applied
    );
    // The second press turns down the pinned LRCLIB answer; KuGou's was turned down already.
    let outcome = t.press(LibraryAction::WrongLyrics).await;

    assert_eq!(outcome.state, LibraryActionState::Applied, "{outcome:?}");
    assert_eq!(
        outcome.detail.as_deref(),
        Some(LibraryActionExecutor::NO_OTHER_LYRICS_TEXT)
    );
    let pin = t.choices.pin_for("nd-1").expect("a pin");
    assert!(pin.is_hidden());
    let rejected = t.rejections.rejected_lyrics("nd-1");
    assert!(rejected.contains("kugou:1.a") && rejected.contains("lrclib:9"));
    t.never_downloaded();
    assert_eq!(t.journal.recent(200).len(), 2, "each press is its own entry");
}

#[tokio::test]
async fn wrong_cover_puts_in_another_and_the_cover_undo_puts_the_old_one_back() {
    let wrong = ramp(400);
    let right = bars(1200);
    let t = Fixture::new(LibraryAction::WrongCover, Some(&wrong), right.clone()).await;

    let outcome = t.press(LibraryAction::WrongCover).await;

    assert_eq!(outcome.state, LibraryActionState::Applied, "{outcome:?}");
    assert_eq!(t.front(), right);
    t.never_downloaded();
    assert!(t.covers.can_undo());

    let stopping = CancellationToken::new();
    let worker = tokio::spawn(t.covers.clone().run(stopping.clone()));
    assert!(
        t.covers
            .try_enqueue(crate::services::cover_art::CoverUpgradeRequest::undo())
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while t.covers.can_undo() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stopping.cancel();
    worker.await.expect("ends").expect("ends cleanly");
    assert_eq!(t.front(), wrong);
}

#[tokio::test]
async fn wrong_cover_with_no_other_cover_is_skipped_and_leaves_the_file_untouched() {
    let wrong = ramp(400);
    // The only cover to be found is the same artwork, larger.
    let t = Fixture::new(LibraryAction::WrongCover, Some(&wrong), ramp(1200)).await;
    let before = std::fs::read(&t.original).expect("read");

    let outcome = t.press(LibraryAction::WrongCover).await;

    assert_eq!(outcome.state, LibraryActionState::Skipped, "{outcome:?}");
    assert_eq!(
        outcome.detail.as_deref(),
        Some(LibraryActionExecutor::NO_OTHER_COVER_TEXT)
    );
    assert_eq!(before, std::fs::read(&t.original).expect("read"));
    assert!(!t.covers.can_undo());
    t.never_downloaded();
}

/// The lyrics files beside the song: the .lrc or .txt Wrong lyrics writes.
fn lyrics_files(root: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .expect("the music root")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "lrc" || e == "txt"))
        .collect()
}

#[tokio::test]
async fn wrong_lyrics_waits_while_a_job_writes_the_music_folder() {
    let t = Arc::new(Fixture::new(LibraryAction::WrongLyrics, None, Vec::new()).await);
    let tidy = t.gate.begin_job("Tidy artists").await.expect("free");

    let pressing = {
        let t = Arc::clone(&t);
        tokio::spawn(async move { t.press(LibraryAction::WrongLyrics).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!pressing.is_finished(), "lyrics were written during a job");
    assert!(lyrics_files(&t.root).is_empty());

    drop(tidy);
    let outcome = pressing.await.expect("joined");
    assert_eq!(outcome.state, LibraryActionState::Applied, "{outcome:?}");
    assert_eq!(lyrics_files(&t.root).len(), 1);
}

#[tokio::test]
async fn wrong_cover_waits_while_a_job_writes_the_music_folder() {
    let wrong = ramp(400);
    let right = bars(1200);
    let t = Arc::new(Fixture::new(LibraryAction::WrongCover, Some(&wrong), right.clone()).await);
    let tidy = t.gate.begin_job("Tidy artists").await.expect("free");

    let pressing = {
        let t = Arc::clone(&t);
        tokio::spawn(async move { t.press(LibraryAction::WrongCover).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!pressing.is_finished(), "a cover was written during a job");
    assert_eq!(t.front(), wrong);

    drop(tidy);
    let outcome = pressing.await.expect("joined");
    assert_eq!(outcome.state, LibraryActionState::Applied, "{outcome:?}");
    assert_eq!(t.front(), right);
}

#[tokio::test]
async fn wrong_lyrics_writes_go_through_the_lyrics_undo() {
    let t = Fixture::new(LibraryAction::WrongLyrics, None, Vec::new()).await;

    let outcome = t.press(LibraryAction::WrongLyrics).await;

    assert_eq!(outcome.state, LibraryActionState::Applied, "{outcome:?}");
    let written = lyrics_files(&t.root);
    assert_eq!(written.len(), 1);
    let entries = t.undo.read_all().expect("read");
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].path, written[0].to_string_lossy());
    assert_eq!(entries[0].before, None);

    // The lyrics page's Undo takes the file Wrong lyrics added back out.
    assert!(t.sidecars.restore(&entries[0]));
    assert!(lyrics_files(&t.root).is_empty());
}
