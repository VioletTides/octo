//! Runs "Tidy artists": a preview, an apply, or the resume of a stopped apply, one at a time.
//!
//! An apply is two phases with a Navidrome scan between them. Navidrome keeps a song's stars,
//! plays and playlist entries across a move when it can match the moved file by its tags or by
//! its path, but not when both change in one scan. So the tags are rewritten first, then the
//! tidy waits for a scan that STARTED after the last write and finished, and only then moves
//! album folders, each only while every song in it is exactly as that scan saw it. Octo's own
//! scan triggers are held back meanwhile, and the music folder's one-writer gate is held from
//! the first write to the last move.
//!
//! Every change is journaled and synced before it is made; a journal that cannot be written
//! stops the run.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use octo_core::common::PathHelper;
use octo_core::settings::SettingsStore;
use octo_media::tags::TagFile;
use octo_media::tags::tag_writer_extras::{self as extras, TagFields};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::journal::{TidyJournal, TidyJournalEntry};
use super::planner::{
    self, TidyAnswers, TidyFile, TidyKind, TidyMove, TidyOptions, TidyRetag, TidyRow, TidyTags,
};
use super::store::{TidyMode, TidyPlanFile, TidyRun, TidyScope, TidyStatus, TidyStore, TidyWork};
use crate::services::common::base_download_service::BaseDownloadService;
use crate::services::cover_art::CoverUpgradeJournal;
use crate::services::fingerprint::music_brainz_client::MusicBrainzClient;
use crate::services::library::explicit_backfill::ExplicitBackfillStore;
use crate::services::library::library_action_journal::LibraryActionJournal;
use crate::services::library::library_action_quarantine::LibraryActionQuarantine;
use crate::services::library::library_tag_edits::TagEditJournal;
use crate::services::library::library_write_gate::{LibraryJob, LibraryWriteGate};
use crate::services::library::navidrome_song_list::NavidromeSongEntry;
use crate::services::library::notice_queue::NoticeQueue;
use crate::services::local::scan_wait::{self, ScanWaitError, ScanWaitTiming};
use crate::services::local::{DownloadHistoryService, ILocalLibraryService};
use crate::services::lyrics::LyricsUndoJournal;
use crate::services::metadata::genre_backfill_journal::GenreBackfillJournal;
use crate::services::state_file;
use crate::services::subsonic::navidrome_version::{self, TidyGate};

/// What the dashboard asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TidyRequest {
    pub mode: TidyMode,
    pub scope: TidyScope,
    /// Carry on with the work of a stopped apply instead of starting from the preview.
    pub resume: bool,
    /// The opt-in rows the person ticked (an apply).
    pub ticked: Vec<String>,
    /// Folders under the music folder to leave out (a preview).
    pub excluded: Vec<String>,
    /// Undo every run the journal holds, not only the last.
    pub all_runs: bool,
}

/// Navidrome's song list by full path, for the check after the moves.
pub type ListSongs = Arc<dyn Fn() -> BoxFuture<'static, HashMap<String, NavidromeSongEntry>> + Send + Sync>;

/// What the tidy reaches besides its store and journal. A service left out is one the run copes
/// without; the library is needed for an apply.
#[derive(Clone, Default)]
pub struct TidyServices {
    pub library: Option<Arc<dyn ILocalLibraryService>>,
    pub music_brainz: Option<Arc<MusicBrainzClient>>,
    pub notices: Option<Arc<NoticeQueue>>,
    pub genre_journal: Option<Arc<GenreBackfillJournal>>,
    pub tag_edits: Option<Arc<TagEditJournal>>,
    pub actions: Option<Arc<LibraryActionJournal>>,
    pub history: Option<Arc<DownloadHistoryService>>,
    pub list_songs: Option<ListSongs>,
    pub cover_journal: Option<Arc<CoverUpgradeJournal>>,
    pub lyrics_undo: Option<Arc<LyricsUndoJournal>>,
    pub explicit: Option<Arc<ExplicitBackfillStore>>,
    pub quarantine: Option<Arc<LibraryActionQuarantine>>,
}

/// How one change went.
enum Outcome {
    Done,
    /// Done by an earlier run (a resume finds it so).
    AlreadyDone,
    Held(String),
    Failed(String),
    JournalFailed(String),
}

/// Why a run stopped early.
enum Stop {
    Cancelled,
    ShuttingDown,
}

const AUDIO_EXTENSIONS: [&str; 14] = [
    "flac", "mp3", "m4a", "aac", "alac", "ogg", "opus", "wav", "aiff", "aif", "ape", "wv", "wma", "dsf",
];

const PLAYLIST_EXTENSIONS: [&str; 2] = ["m3u", "m3u8"];

pub struct TidyWorker {
    store: Arc<TidyStore>,
    journal: Arc<TidyJournal>,
    settings: Arc<SettingsStore>,
    services: TidyServices,
    write_gate: LibraryWriteGate,
    timing: ScanWaitTiming,
    max_lookups: AtomicUsize,
    cancel_requested: AtomicBool,
    pending: AtomicBool,
    gate_seen: Mutex<Option<(tokio::time::Instant, TidyGate)>>,
    sender: mpsc::Sender<TidyRequest>,
    receiver: tokio::sync::Mutex<mpsc::Receiver<TidyRequest>>,
}

impl TidyWorker {
    /// What the dashboard calls this job when another one has to wait for it.
    pub const JOB_NAME: &'static str = "Tidy artists";

    /// MusicBrainz allows about one request a second. What is not asked in one preview is
    /// reported, and the next preview asks the next batch.
    pub const MAX_LOOKUPS: usize = 60;

    /// Files in a row that could not be written before the run gives up.
    const MAX_CONSECUTIVE_FAILURES: usize = 10;

    pub fn new(
        store: Arc<TidyStore>,
        journal: Arc<TidyJournal>,
        settings: Arc<SettingsStore>,
        services: TidyServices,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(1);
        Self {
            store,
            journal,
            settings,
            services,
            write_gate: LibraryWriteGate::new(),
            timing: ScanWaitTiming::default(),
            max_lookups: AtomicUsize::new(Self::MAX_LOOKUPS),
            cancel_requested: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            gate_seen: Mutex::new(None),
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
        }
    }

    /// The gate shared with the other jobs that write the music folder.
    pub fn with_write_gate(mut self, gate: LibraryWriteGate) -> Self {
        self.write_gate = gate;
        self
    }

    /// How often and how long to wait for Navidrome's scans (tests wait less).
    pub fn with_timing(mut self, timing: ScanWaitTiming) -> Self {
        self.timing = timing;
        self
    }

    /// The MusicBrainz lookups one preview may make (tests ask fewer).
    pub fn set_max_lookups(&self, max: usize) {
        self.max_lookups.store(max, Ordering::SeqCst);
    }

    pub fn current(&self) -> TidyRun {
        self.store.current()
    }

    pub fn store(&self) -> &Arc<TidyStore> {
        &self.store
    }

    pub fn journal(&self) -> &Arc<TidyJournal> {
        &self.journal
    }

    pub fn is_running(&self) -> bool {
        self.store.current().status == TidyStatus::Running
    }

    /// A run accepted and not yet started.
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }

    /// The music folder, as the settings name it.
    pub fn music_root(&self) -> String {
        let root = self
            .settings
            .raw("Library:DownloadPath")
            .unwrap_or_else(|| "./downloads".to_string());
        let trimmed = root.trim_end_matches('/');
        if trimmed.is_empty() {
            root
        } else {
            trimmed.to_string()
        }
    }

    /// False when a run is going or waiting to start.
    pub fn try_enqueue(&self, request: TidyRequest) -> bool {
        if self.is_running()
            || self
                .pending
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return false;
        }
        if self.sender.try_send(request).is_ok() {
            return true;
        }
        self.pending.store(false, Ordering::SeqCst);
        false
    }

    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    /// Whether an apply may run against the Navidrome in front of Octo. Remembered a minute,
    /// so a polling dashboard does not ping on every refresh.
    pub async fn check_navidrome(&self, fresh: bool) -> TidyGate {
        if !fresh
            && let Some((when, gate)) = self.gate_seen.lock().clone()
            && when.elapsed() < Duration::from_secs(60)
        {
            return gate;
        }
        let info = match &self.services.library {
            Some(library) => library.get_server_info().await,
            None => None,
        };
        let gate = navidrome_version::gate(info.as_ref());
        *self.gate_seen.lock() = Some((tokio::time::Instant::now(), gate.clone()));
        gate
    }

    /// The hosted loop: one request at a time until `stopping`.
    pub async fn run(self: Arc<Self>, stopping: CancellationToken) -> anyhow::Result<()> {
        let mut queue = self.receiver.lock().await;
        loop {
            let request = tokio::select! {
                request = queue.recv() => request,
                () = stopping.cancelled() => break,
            };
            let Some(request) = request else {
                break;
            };
            self.run_once(request, &stopping).await;
        }
        Ok(())
    }

    /// One request, start to finish.
    pub async fn run_once(&self, request: TidyRequest, stopping: &CancellationToken) {
        self.cancel_requested.store(false, Ordering::SeqCst);
        match request.mode {
            TidyMode::Preview => self.preview(&request, stopping).await,
            TidyMode::Apply => self.apply(&request, stopping).await,
            TidyMode::Undo => self.undo(&request, stopping).await,
        }
        if self.store.current().status == TidyStatus::Running {
            self.finish_with(
                TidyStatus::Failed,
                "The run ended without saying how.".to_string(),
            );
        }
        self.pending.store(false, Ordering::SeqCst);
    }

    // ---- preview ----------------------------------------------------------------------------

    async fn preview(&self, request: &TidyRequest, stopping: &CancellationToken) {
        let root = self.music_root();
        self.start(TidyMode::Preview, request.scope, "Reading the library");
        info!("Library tidy preview started: scope {}", request.scope.name());
        let Some(files) = self.read_library(&root, request.scope, stopping).await else {
            return;
        };
        let case_insensitive = case_insensitive(&root);
        self.store
            .update(|run| run.phase = "Asking MusicBrainz".to_string());
        let options = TidyOptions {
            case_insensitive,
            excluded: request.excluded.clone(),
            ..TidyOptions::default()
        };
        let Some((answers, asked, not_asked)) = self.ask(&root, &files, &options, stopping).await else {
            return;
        };
        let shown = planner::plan(&root, &files, &answers, &options);
        let full = planner::plan(
            &root,
            &files,
            &answers,
            &TidyOptions {
                tick_all: true,
                ..options.clone()
            },
        );
        let (retags, moves) = (shown.retags.len(), shown.moves.len());
        let saved = self.store.save_plan(TidyPlanFile {
            root: root.clone(),
            scope: request.scope,
            excluded: request.excluded.clone(),
            case_insensitive,
            plan: shown,
            answers,
            work: None,
        });
        if let Err(failure) = saved {
            self.finish_with(
                TidyStatus::Failed,
                format!("The preview could not be saved: {failure}"),
            );
            return;
        }
        self.store.update(|run| {
            run.planned_retags = retags;
            run.planned_moves = moves;
            run.plan_hash = Some(full.hash());
            run.lookups_asked = asked;
            run.lookups_not_asked = not_asked;
        });
        self.finish_with(TidyStatus::Completed, "Nothing has been written.".to_string());
    }

    /// Plan, ask MusicBrainz what the plan wants to know, and plan again, a few rounds at most:
    /// a folder asks about its whole name first and its parts only after. Answers already known
    /// cost nothing; new questions stop at the budget.
    async fn ask(
        &self,
        root: &str,
        files: &[TidyFile],
        options: &TidyOptions,
        stopping: &CancellationToken,
    ) -> Option<(TidyAnswers, usize, usize)> {
        let mut answers = TidyAnswers::default();
        let mut not_asked: HashSet<String> = HashSet::new();
        let mut asked = 0;
        let budget = self.max_lookups.load(Ordering::SeqCst);
        for _round in 0..5 {
            let plan = planner::plan(root, files, &answers, options);
            let artists: Vec<String> = plan
                .wanted_artists
                .into_iter()
                .filter(|name| {
                    let key = planner::artist_lookup_key(name);
                    !answers.artists.contains_key(&key) && !not_asked.contains(&key)
                })
                .collect();
            let recordings: Vec<String> = plan
                .wanted_recordings
                .into_iter()
                .filter(|id| !answers.credits.contains_key(id) && !not_asked.contains(id))
                .collect();
            if artists.is_empty() && recordings.is_empty() {
                break;
            }
            let Some(client) = &self.services.music_brainz else {
                not_asked.extend(artists.iter().map(|name| planner::artist_lookup_key(name)));
                not_asked.extend(recordings);
                continue;
            };
            for name in artists {
                if let Some(stop) = self.should_stop(stopping) {
                    self.stopped(Some(stop));
                    return None;
                }
                let key = planner::artist_lookup_key(&name);
                let known = client.known_artists_named(&name);
                let answer = match known {
                    Some(known) => Some(known),
                    None if asked < budget => {
                        asked += 1;
                        client.find_artists_named(&name).await.ok().flatten()
                    }
                    None => None,
                };
                match answer {
                    Some(found) => {
                        answers
                            .artists
                            .insert(key, found.into_iter().map(|artist| artist.name).collect());
                    }
                    None => {
                        not_asked.insert(key);
                    }
                }
            }
            let mut unknown: Vec<String> = Vec::new();
            for id in recordings {
                match client.known_credits(&id) {
                    Some(credit) => {
                        answers.credits.insert(id, credit);
                    }
                    None => unknown.push(id),
                }
            }
            for batch in unknown.chunks(100) {
                if asked >= budget {
                    not_asked.extend(batch.iter().cloned());
                    continue;
                }
                asked += 1;
                match client.recording_credits(batch).await.ok().flatten() {
                    Some(found) => answers.credits.extend(found),
                    None => not_asked.extend(batch.iter().cloned()),
                }
            }
            self.store.update(|run| run.lookups_asked = asked);
        }
        Some((answers, asked, not_asked.len()))
    }

    // ---- apply ------------------------------------------------------------------------------

    async fn apply(&self, request: &TidyRequest, stopping: &CancellationToken) {
        let previous = self.store.current();
        let plan_file = self.store.plan();
        self.start(TidyMode::Apply, request.scope, "Checking Navidrome");
        let refuse = |why: String| self.finish_with(TidyStatus::Failed, why);

        let Some(plan_file) = plan_file else {
            refuse(PREVIEW_FIRST.to_string());
            return;
        };
        if request.resume {
            if !previous.can_resume() || plan_file.work.is_none() {
                refuse("There is no stopped tidy to resume.".to_string());
                return;
            }
        } else if !previous.is_finished_preview() || plan_file.scope != request.scope {
            refuse(PREVIEW_FIRST.to_string());
            return;
        }
        let gate = self.check_navidrome(true).await;
        if !gate.apply_allowed {
            refuse(gate.reason.unwrap_or_default());
            return;
        }
        let Some(library) = self.services.library.clone() else {
            refuse("Octo has no library service to ask Navidrome for scans.".to_string());
            return;
        };
        let mut job = match self.write_gate.begin_job(Self::JOB_NAME).await {
            Ok(job) => job,
            Err(holder) => {
                refuse(LibraryWriteGate::busy_message(&holder));
                return;
            }
        };

        let root = plan_file.root.clone();
        let work = if request.resume {
            plan_file.work.clone().unwrap_or_default()
        } else {
            self.store
                .update(|run| run.phase = "Reading the library".to_string());
            let Some(files) = self.read_library(&root, plan_file.scope, stopping).await else {
                return;
            };
            let options = TidyOptions {
                case_insensitive: plan_file.case_insensitive,
                excluded: plan_file.excluded.clone(),
                ..TidyOptions::default()
            };
            let full = planner::plan(
                &root,
                &files,
                &plan_file.answers,
                &TidyOptions {
                    tick_all: true,
                    ..options.clone()
                },
            );
            if Some(full.hash()) != previous.plan_hash {
                refuse(
                    "The library changed since the preview, so Apply stopped before touching anything. Preview again."
                        .to_string(),
                );
                return;
            }
            let opt_in: HashSet<&String> = full
                .rows
                .iter()
                .filter(|row| row.opt_in)
                .map(|row| &row.id)
                .collect();
            let ticked: Vec<String> = request
                .ticked
                .iter()
                .filter(|id| opt_in.contains(id))
                .cloned()
                .collect();
            let chosen = planner::plan(
                &root,
                &files,
                &plan_file.answers,
                &TidyOptions {
                    ticked: ticked.iter().cloned().collect(),
                    ..options
                },
            );
            let work = TidyWork {
                run_id: self.store.current().run_id,
                ticked,
                retags: chosen.retags,
                moves: chosen.moves,
            };
            let mut kept = plan_file.clone();
            kept.work = Some(work.clone());
            if let Err(failure) = self.store.save_plan(kept) {
                refuse(format!(
                    "The work could not be saved, so nothing was changed: {failure}"
                ));
                return;
            }
            work
        };
        self.store.update(|run| {
            run.planned_retags = work.retags.len();
            run.planned_moves = work.moves.len();
            run.plan_hash = previous.plan_hash.clone();
        });
        info!(
            "Library tidy apply started: {} file(s) to re-tag, {} album folder(s) to move",
            work.retags.len(),
            work.moves.len()
        );
        self.execute(&root, &work, library, stopping, &mut job).await;
    }

    async fn execute(
        &self,
        root: &str,
        work: &TidyWork,
        library: Arc<dyn ILocalLibraryService>,
        stopping: &CancellationToken,
        job: &mut LibraryJob,
    ) {
        let hold = library.scan_hold().map(|hold| hold.hold());
        let before = self.list_songs().await;
        let stopped = self
            .write_and_move(root, work, library.as_ref(), stopping, job)
            .await;
        // Nothing is written from here on.
        job.pause();

        let wanted = hold.map(|hold| hold.release()).unwrap_or(false);
        let changed = {
            let run = self.store.current();
            run.retagged > 0 || run.moved > 0
        };
        if changed || wanted {
            library.start_scan().await;
        }
        if let Some(stop) = stopped {
            self.stopped(Some(stop));
            return;
        }
        if self.store.current().status != TidyStatus::Running {
            return;
        }
        if self.store.current().moved > 0 {
            self.check_what_navidrome_sees(work, before, library.as_ref(), stopping)
                .await;
        }
        let run = self.store.current();
        self.finish_with(
            TidyStatus::Completed,
            format!(
                "Re-tagged {} file(s) and moved {} album folder(s); {} held back.",
                run.retagged, run.moved, run.held
            ),
        );
    }

    /// The tags, the scan between, the moves and the playlists. Some when the run was stopped.
    async fn write_and_move(
        &self,
        root: &str,
        work: &TidyWork,
        library: &dyn ILocalLibraryService,
        stopping: &CancellationToken,
        job: &mut LibraryJob,
    ) -> Option<Stop> {
        self.store
            .update(|run| run.phase = "Waiting for Navidrome to finish scanning".to_string());
        // Downloads may land while Navidrome scans; the tidy measures every file before it
        // touches it.
        job.pause();
        let idle = scan_wait::wait_while_scanning(library, self.timing, stopping).await;
        job.resume().await;
        if !idle {
            if let Some(stop) = self.should_stop(stopping) {
                return Some(stop);
            }
            self.finish_with(
                TidyStatus::Failed,
                "Navidrome kept scanning, or did not answer, so nothing was changed. Apply again later."
                    .to_string(),
            );
            return None;
        }

        // ---- phase 1: tags ----
        self.store.update(|run| run.phase = "Writing tags".to_string());
        let mut last_write: Option<DateTime<Utc>> = None;
        let mut stamps: HashMap<String, (u64, DateTime<Utc>)> = self.stamps_of(&work.run_id);
        let mut in_a_row = 0;
        for retag in &work.retags {
            if let Some(stop) = self.should_stop(stopping) {
                return Some(stop);
            }
            let (journal, retag_for_write, run_id) =
                (self.journal.clone(), retag.clone(), work.run_id.clone());
            let outcome =
                tokio::task::spawn_blocking(move || retag_file(&retag_for_write, &run_id, &journal))
                    .await
                    .unwrap_or_else(|panic| Outcome::Failed(panic.to_string()));
            match outcome {
                Outcome::Done => {
                    in_a_row = 0;
                    last_write = Some(Utc::now());
                    if let Some(stamp) = stamp_of(&retag.path) {
                        stamps.insert(retag.path.clone(), stamp);
                    }
                    self.store.update(|run| run.retagged += 1);
                }
                Outcome::AlreadyDone => in_a_row = 0,
                Outcome::Held(why) => {
                    in_a_row = 0;
                    self.hold(root, &retag.path, why);
                }
                Outcome::Failed(why) => {
                    in_a_row += 1;
                    self.fail(&retag.path, &why);
                    if in_a_row >= Self::MAX_CONSECUTIVE_FAILURES {
                        self.finish_with(
                            TidyStatus::Failed,
                            format!("{in_a_row} files in a row could not be written. Is the music folder read-only?"),
                        );
                        return None;
                    }
                }
                Outcome::JournalFailed(why) => {
                    self.finish_with(TidyStatus::Failed, journal_failed(&why));
                    return None;
                }
            }
        }
        if work.moves.is_empty() {
            return None;
        }

        // ---- the scan between ----
        self.store
            .update(|run| run.phase = "Waiting for Navidrome to scan".to_string());
        // Every write of this work, an earlier run's too when resuming, must be older than the
        // start of the scan the moves are measured against.
        let newest = stamps
            .values()
            .map(|(_, modified)| *modified)
            .chain(last_write)
            .max();
        // A move goes ahead only while every song in the folder is as this scan saw it, so the
        // folder is open to downloads until the scan is done.
        job.pause();
        let seen = library
            .get_scan_status()
            .await
            .and_then(|status| status.last_scan_started_at());
        let scan_start = match newest {
            Some(_)
                if seen
                    .zip(newest)
                    .is_some_and(|(started, written)| started > written) =>
            {
                seen
            }
            Some(written) => {
                if !library.start_scan().await {
                    self.finish_with(
                        TidyStatus::Failed,
                        "Navidrome would not start a scan, so nothing was moved. Check that Octo has Navidrome admin credentials."
                            .to_string(),
                    );
                    return None;
                }
                match scan_wait::wait_for_scan(library, written, self.timing, stopping).await {
                    Ok(_) => library
                        .get_scan_status()
                        .await
                        .and_then(|status| status.last_scan_started_at()),
                    Err(ScanWaitError::Cancelled) => {
                        return self.should_stop(stopping).or(Some(Stop::Cancelled));
                    }
                    Err(ScanWaitError::NoStatus) => {
                        self.finish_with(TidyStatus::Failed, NO_STATUS.to_string());
                        return None;
                    }
                    Err(ScanWaitError::TimedOut) => {
                        self.finish_with(
                            TidyStatus::Failed,
                            format!(
                                "Navidrome did not finish a scan within {} minutes. The tags are written; nothing was moved. Resume once it has scanned.",
                                self.timing.timeout.as_secs() / 60
                            ),
                        );
                        return None;
                    }
                }
            }
            None => seen,
        };
        let Some(scan_start) = scan_start else {
            self.finish_with(TidyStatus::Failed, NO_STATUS.to_string());
            return None;
        };
        job.resume().await;

        // ---- phase 2: album folders ----
        self.store.update(|run| run.phase = "Moving files".to_string());
        let mut moved: Vec<(String, String)> = Vec::new();
        let mut in_a_row = 0;
        for movement in &work.moves {
            if let Some(stop) = self.should_stop(stopping) {
                self.follow_playlists(root, &moved, &work.run_id);
                return Some(stop);
            }
            job.pause();
            let idle = scan_wait::wait_while_scanning(library, self.timing, stopping).await;
            job.resume().await;
            if !idle {
                if let Some(stop) = self.should_stop(stopping) {
                    self.follow_playlists(root, &moved, &work.run_id);
                    return Some(stop);
                }
                self.hold(
                    root,
                    &movement.from,
                    "Navidrome was scanning or did not answer. Resume once it is idle.".to_string(),
                );
                continue;
            }
            let (journal, movement_for_move, run_id, stamps_for_move) = (
                self.journal.clone(),
                movement.clone(),
                work.run_id.clone(),
                stamps.clone(),
            );
            let outcome = tokio::task::spawn_blocking(move || {
                move_folder(
                    &movement_for_move,
                    scan_start,
                    &stamps_for_move,
                    &run_id,
                    &journal,
                )
            })
            .await
            .unwrap_or_else(|panic| Outcome::Failed(panic.to_string()));
            match outcome {
                Outcome::Done => {
                    in_a_row = 0;
                    self.follow(library, &movement.from, &movement.to).await;
                    let parent = Path::new(&movement.from)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned());
                    BaseDownloadService::try_remove_empty_parents(parent.as_deref(), root);
                    moved.push((movement.from.clone(), movement.to.clone()));
                    self.store.update(|run| run.moved += 1);
                }
                Outcome::AlreadyDone => {
                    in_a_row = 0;
                    moved.push((movement.from.clone(), movement.to.clone()));
                }
                Outcome::Held(why) => {
                    in_a_row = 0;
                    self.hold(root, &movement.from, why);
                }
                Outcome::Failed(why) => {
                    in_a_row += 1;
                    self.fail(&movement.from, &why);
                    if in_a_row >= Self::MAX_CONSECUTIVE_FAILURES {
                        self.follow_playlists(root, &moved, &work.run_id);
                        self.finish_with(
                            TidyStatus::Failed,
                            format!("{in_a_row} folders in a row could not be moved. Is the music folder read-only?"),
                        );
                        return None;
                    }
                }
                Outcome::JournalFailed(why) => {
                    self.follow_playlists(root, &moved, &work.run_id);
                    self.finish_with(TidyStatus::Failed, journal_failed(&why));
                    return None;
                }
            }
        }
        self.store
            .update(|run| run.phase = "Updating playlists".to_string());
        self.follow_playlists(root, &moved, &work.run_id);
        None
    }

    /// Stamps the journal already holds for this work's run: a resume measures files against
    /// what the earlier run left.
    fn stamps_of(&self, run_id: &str) -> HashMap<String, (u64, DateTime<Utc>)> {
        self.journal
            .read_all()
            .into_iter()
            .filter_map(|entry| match entry {
                TidyJournalEntry::Stamp {
                    path,
                    size,
                    modified,
                    run,
                } if run == run_id => Some((path, (size, modified))),
                _ => None,
            })
            .collect()
    }

    /// Octo's own records of where a file is follow the folder.
    async fn follow(&self, library: &dyn ILocalLibraryService, from: &str, to: &str) {
        if let Err(failure) = library.update_local_path(from, to).await {
            warn!("Library tidy moved {from} but its download records could not follow: {failure}");
        }
        if let Some(notices) = &self.services.notices {
            notices.move_paths(from, to);
        }
        if let Some(journal) = &self.services.genre_journal {
            journal.move_paths(from, to);
        }
        if let Some(journal) = &self.services.tag_edits {
            journal.move_paths(from, to);
        }
        if let Some(journal) = &self.services.actions {
            journal.move_paths(from, to);
        }
        if let Some(history) = &self.services.history {
            history.move_paths(from, to);
        }
        if let Some(journal) = &self.services.cover_journal {
            journal.move_paths(from, to);
        }
        if let Some(journal) = &self.services.lyrics_undo
            && let Err(failure) = journal.move_paths(from, to)
        {
            warn!("Library tidy moved {from} but the lyrics undo could not follow: {failure}");
        }
        if let Some(store) = &self.services.explicit {
            store.move_paths(from, to);
        }
        if let Some(quarantine) = &self.services.quarantine {
            quarantine.move_paths(&self.music_root(), from, to);
        }
    }

    /// Playlist files under the music folder that name a moved song name it where it is now.
    /// Each file's text is journaled before it is rewritten.
    fn follow_playlists(&self, root: &str, moved: &[(String, String)], run_id: &str) {
        if moved.is_empty() {
            return;
        }
        let playlists = match walk(Path::new(root), &PLAYLIST_EXTENSIONS) {
            Ok(found) => found,
            Err(failure) => {
                warn!("Library tidy could not look for playlists: {failure}");
                return;
            }
        };
        for playlist in playlists {
            let Ok(bytes) = std::fs::read(&playlist) else {
                continue;
            };
            let Ok(text) = String::from_utf8(bytes) else {
                self.hold(
                    root,
                    &playlist,
                    "This playlist is not UTF-8 text, so the songs it names were not updated.".to_string(),
                );
                continue;
            };
            let Some(rewritten) = rewrite_playlist(&playlist, &text, moved) else {
                continue;
            };
            let entry = TidyJournalEntry::Playlist {
                path: playlist.clone(),
                before: text,
                run: run_id.to_string(),
                at: Utc::now(),
            };
            if let Err(failure) = self.journal.append(&entry) {
                self.fail(&playlist, &journal_failed(&failure.to_string()));
                return;
            }
            if let Err(failure) = state_file::write_atomic(Path::new(&playlist), rewritten.as_bytes()) {
                self.fail(&playlist, &failure.to_string());
            }
        }
    }

    /// After the moves: a scan, then any moved song Navidrome reports missing, or under a new
    /// id, is listed, since its stars and plays may not have followed it.
    async fn check_what_navidrome_sees(
        &self,
        work: &TidyWork,
        before: HashMap<String, NavidromeSongEntry>,
        library: &dyn ILocalLibraryService,
        stopping: &CancellationToken,
    ) {
        if before.is_empty() {
            return;
        }
        self.store
            .update(|run| run.phase = "Checking what Navidrome sees".to_string());
        let after_moves = Utc::now() - chrono::TimeDelta::seconds(1);
        if scan_wait::wait_for_scan(library, after_moves, self.timing, stopping)
            .await
            .is_err()
        {
            self.store.update(|run| {
                run.errors.push(
                    "Navidrome did not finish a scan after the moves, so Octo could not check that every song kept its place."
                        .to_string(),
                )
            });
            return;
        }
        let now = self.list_songs().await;
        let root = self.music_root();
        for movement in &work.moves {
            for file in &movement.files {
                let Some(was) = before.get(file) else {
                    continue;
                };
                let Some(target) = PathHelper::moved_path(file, &movement.from, &movement.to) else {
                    continue;
                };
                let lost = now.get(file).is_some_and(|entry| entry.missing);
                let new_id = match (now.get(&target).and_then(|e| e.id.as_deref()), was.id.as_deref()) {
                    (Some(now_id), Some(was_id)) => now_id != was_id,
                    _ => false,
                };
                if lost || new_id {
                    self.hold(
                        &root,
                        &target,
                        "Navidrome did not match this moved song to the one it was, so its stars, plays and playlist entries may not have followed. Undo puts it back.".to_string(),
                    );
                }
            }
        }
    }

    async fn list_songs(&self) -> HashMap<String, NavidromeSongEntry> {
        match &self.services.list_songs {
            Some(list) => list().await,
            None => HashMap::new(),
        }
    }

    // ---- reading ----------------------------------------------------------------------------

    async fn read_library(
        &self,
        root: &str,
        scope: TidyScope,
        stopping: &CancellationToken,
    ) -> Option<Vec<TidyFile>> {
        if !Path::new(root).is_dir() {
            self.finish_with(TidyStatus::Failed, format!("There is no music folder at {root}."));
            return None;
        }
        let owned: Option<HashSet<String>> = match scope {
            TidyScope::WholeLibrary => None,
            TidyScope::OctoDownloads => {
                let Some(library) = &self.services.library else {
                    self.finish_with(
                        TidyStatus::Failed,
                        "Octo has no record of its downloads to go on.".to_string(),
                    );
                    return None;
                };
                Some(
                    library
                        .get_mappings()
                        .await
                        .into_iter()
                        .filter(|mapping| !mapping.external_provider.eq_ignore_ascii_case("lidarr"))
                        .map(|mapping| mapping.local_path)
                        .filter(|path| !path.is_empty())
                        .collect(),
                )
            }
        };
        let walked = {
            let root = root.to_string();
            tokio::task::spawn_blocking(move || walk(Path::new(&root), &AUDIO_EXTENSIONS))
                .await
                .unwrap_or_else(|panic| Err(std::io::Error::other(panic.to_string())))
        };
        let paths = match walked {
            Ok(paths) => paths,
            Err(failure) => {
                self.finish_with(
                    TidyStatus::Failed,
                    format!("Could not list the music folder: {failure}"),
                );
                return None;
            }
        };
        let mut files = Vec::with_capacity(paths.len());
        for chunk in paths.chunks(50) {
            if let Some(stop) = self.should_stop(stopping) {
                self.stopped(Some(stop));
                return None;
            }
            let chunk: Vec<(String, bool)> = chunk
                .iter()
                .map(|path| {
                    (
                        path.clone(),
                        owned.as_ref().is_none_or(|owned| owned.contains(path)),
                    )
                })
                .collect();
            let read = tokio::task::spawn_blocking(move || {
                chunk
                    .into_iter()
                    .map(|(path, in_scope)| read_file(&path, in_scope))
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            files.extend(read);
            let (count, in_scope) = (files.len(), files.iter().filter(|file| file.in_scope).count());
            self.store.update(|run| {
                run.files = count;
                run.in_scope = in_scope;
            });
        }
        Some(files)
    }

    // ---- bookkeeping ------------------------------------------------------------------------

    fn start(&self, mode: TidyMode, scope: TidyScope, phase: &str) {
        self.store.replace(TidyRun {
            run_id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
            status: TidyStatus::Running,
            mode,
            scope,
            phase: phase.to_string(),
            started_utc: Some(Utc::now()),
            ..TidyRun::default()
        });
    }

    fn should_stop(&self, stopping: &CancellationToken) -> Option<Stop> {
        if stopping.is_cancelled() {
            Some(Stop::ShuttingDown)
        } else if self.cancel_requested.load(Ordering::SeqCst) {
            Some(Stop::Cancelled)
        } else {
            None
        }
    }

    fn stopped(&self, stop: Option<Stop>) {
        match stop {
            Some(Stop::ShuttingDown) => self.finish_with(
                TidyStatus::Interrupted,
                "Octo is shutting down. Resume carries on where it stopped.".to_string(),
            ),
            Some(Stop::Cancelled) => self.finish_with(
                TidyStatus::Cancelled,
                "Cancelled from the dashboard. What was done is in the journal, and Undo reverses it."
                    .to_string(),
            ),
            None => {}
        }
    }

    fn hold(&self, root: &str, path: &str, why: String) {
        let shown = planner::relative(root, path).map_or_else(|| path.to_string(), |s| s.join("/"));
        self.store.update(|run| {
            run.held += 1;
            run.held_rows.push(TidyRow {
                id: format!("held|{shown}"),
                kind: TidyKind::Skip,
                path: shown.clone(),
                before: shown,
                after: String::new(),
                evidence: why,
                files: 0,
                opt_in: false,
                ticked: false,
            });
        });
    }

    fn fail(&self, path: &str, why: &str) {
        warn!("Library tidy could not change {path}: {why}");
        self.store.update(|run| {
            run.failed += 1;
            run.errors.push(format!("{path}: {why}"));
        });
    }

    fn finish_with(&self, status: TidyStatus, reason: String) {
        if status == TidyStatus::Failed {
            error!("Library tidy stopped: {reason}");
        } else {
            info!("Library tidy {}: {reason}", status.name().to_lowercase());
        }
        self.store.update(|run| {
            if run.status != TidyStatus::Running {
                return;
            }
            run.status = status;
            run.reason = Some(reason);
            run.phase = String::new();
            run.finished_utc = Some(Utc::now());
        });
    }
}

const PREVIEW_FIRST: &str =
    "Preview first. Apply only does what a finished preview of the same files showed.";

const NO_STATUS: &str = "Navidrome did not say when it last scanned, so nothing was moved. Check that Octo has Navidrome admin credentials.";

fn journal_failed(why: &str) -> String {
    format!(
        "The undo journal could not be written ({why}), so the tidy stopped. Nothing further was changed."
    )
}

// ---- one file, one folder -------------------------------------------------------------------

fn read_tags(file: &TagFile) -> TidyTags {
    let clean = |values: Vec<String>| -> Vec<String> {
        values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect()
    };
    TidyTags {
        artist: clean(file.performers()),
        album_artist: clean(file.album_artists()),
        artists: extras::read_values(file, TagFields::ARTISTS),
    }
}

/// One audio file's facts. A file whose tags cannot be read still moves with its folder.
pub(crate) fn read_file(path: &str, in_scope: bool) -> TidyFile {
    match TagFile::open(path) {
        Ok(mut file) => TidyFile {
            path: path.to_string(),
            tags: read_tags(&file),
            artist_ids: extras::read_artist_ids(&file),
            recording_id: extras::read_recording_id(&mut file),
            title: file.title(),
            in_scope,
            readable: true,
            slash_artist: extras::artist_frame_splits_on_slash(&file),
        },
        Err(_) => TidyFile {
            path: path.to_string(),
            in_scope,
            readable: false,
            ..TidyFile::default()
        },
    }
}

/// Writes only the fields that differ, so a field read differently from how it is stored is
/// never written back in that reading.
fn write_tags(file: &mut TagFile, from: &TidyTags, to: &TidyTags) {
    if from.artist != to.artist {
        file.set_performers(&to.artist);
    }
    if from.album_artist != to.album_artist {
        file.set_album_artists(&to.album_artist);
    }
    if from.artists != to.artists {
        if to.artists.is_empty() {
            extras::set_or_clear_text(file, TagFields::ARTISTS, None);
        } else {
            extras::set_multi_value(file, "ARTISTS", &to.artists);
        }
    }
}

fn retag_file(retag: &TidyRetag, run_id: &str, journal: &TidyJournal) -> Outcome {
    let mut file = match TagFile::open(&retag.path) {
        Ok(file) => file,
        Err(failure) => {
            return if Path::new(&retag.path).exists() {
                Outcome::Failed(failure.to_string())
            } else {
                Outcome::Held("It is no longer there.".to_string())
            };
        }
    };
    let now = read_tags(&file);
    if now == retag.after {
        return Outcome::AlreadyDone;
    }
    if now != retag.before {
        return Outcome::Held("Its artist tags changed after the preview. Preview again.".to_string());
    }
    let entry = TidyJournalEntry::Tags {
        path: retag.path.clone(),
        before: retag.before.clone(),
        after: retag.after.clone(),
        run: run_id.to_string(),
        at: Utc::now(),
    };
    if let Err(failure) = journal.append(&entry) {
        return Outcome::JournalFailed(failure.to_string());
    }
    write_tags(&mut file, &retag.before, &retag.after);
    if let Err(failure) = file.save() {
        return Outcome::Failed(failure.to_string());
    }
    let Some((size, modified)) = stamp_of(&retag.path) else {
        return Outcome::Failed("It could not be read after it was written.".to_string());
    };
    let stamp = TidyJournalEntry::Stamp {
        path: retag.path.clone(),
        size,
        modified,
        run: run_id.to_string(),
    };
    match journal.append(&stamp) {
        Ok(()) => Outcome::Done,
        Err(failure) => Outcome::JournalFailed(failure.to_string()),
    }
}

/// A file's size and last write time.
fn stamp_of(path: &str) -> Option<(u64, DateTime<Utc>)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), DateTime::<Utc>::from(meta.modified().ok()?)))
}

/// Moves one album folder whole, by renaming it, only while every song in it is as planned and
/// as Navidrome's last scan saw it.
fn move_folder(
    movement: &TidyMove,
    scan_start: DateTime<Utc>,
    stamps: &HashMap<String, (u64, DateTime<Utc>)>,
    run_id: &str,
    journal: &TidyJournal,
) -> Outcome {
    let (from, to) = (Path::new(&movement.from), Path::new(&movement.to));
    let from_meta = std::fs::symlink_metadata(from);
    let to_exists = std::fs::symlink_metadata(to).is_ok();
    match &from_meta {
        Err(_) if to_exists => return Outcome::AlreadyDone,
        Err(_) => return Outcome::Held("It is no longer there.".to_string()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Outcome::Held(
                "It is a link or not a folder, and the tidy only moves real folders.".to_string(),
            );
        }
        Ok(_) => {}
    }
    if to_exists {
        return Outcome::Held(format!(
            "{} is already there, so both stay as they are.",
            movement.to
        ));
    }
    let now = match walk_all(from) {
        Ok(found) => found,
        Err(failure) => return Outcome::Failed(failure.to_string()),
    };
    if now.links {
        return Outcome::Held("It holds a link, and the tidy never moves one.".to_string());
    }
    let planned: HashSet<&String> = movement.files.iter().collect();
    let audio: HashSet<&String> = now.audio.iter().collect();
    if planned != audio {
        return Outcome::Held("Its songs changed since the preview. Preview again.".to_string());
    }
    for file in &movement.files {
        let Some((size, modified)) = stamp_of(file) else {
            return Outcome::Held("A song in it could not be read.".to_string());
        };
        if let Some(stamp) = stamps.get(file)
            && *stamp != (size, modified)
        {
            return Outcome::Held(
                "A song in it changed after Octo re-tagged it. Resume once Navidrome has scanned it."
                    .to_string(),
            );
        }
        if modified >= scan_start {
            return Outcome::Held(
                "A song in it changed after Navidrome's last scan began, so moving it now could cost its stars and plays. Resume once Navidrome has scanned.".to_string(),
            );
        }
    }
    let Some(parent) = to.parent() else {
        return Outcome::Failed("The new folder has no parent.".to_string());
    };
    if !same_device(from, parent) {
        return Outcome::Held(
            "The new folder would be on another disk, and the tidy only renames within one.".to_string(),
        );
    }
    let entry = TidyJournalEntry::Move {
        from: movement.from.clone(),
        to: movement.to.clone(),
        run: run_id.to_string(),
        at: Utc::now(),
    };
    if let Err(failure) = journal.append(&entry) {
        return Outcome::JournalFailed(failure.to_string());
    }
    if let Err(failure) = std::fs::create_dir_all(parent).and_then(|()| std::fs::rename(from, to)) {
        return Outcome::Failed(failure.to_string());
    }
    Outcome::Done
}

/// Whether two paths are on one filesystem, measured at the nearest folder that exists.
#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let device = |path: &Path| {
        path.ancestors()
            .find_map(|ancestor| std::fs::metadata(ancestor).ok())
            .map(|meta| meta.dev())
    };
    match (device(a), device(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_device(_: &Path, _: &Path) -> bool {
    true
}

/// What is under a folder: its audio files, and whether any entry is a link.
struct Contents {
    audio: Vec<String>,
    links: bool,
}

/// Dot folders and dot files are left out, as [`walk`] leaves them out: a half-made copy Octo
/// keeps hidden beside a song is not one of the album's songs.
fn walk_all(folder: &Path) -> std::io::Result<Contents> {
    let mut contents = Contents {
        audio: Vec::new(),
        links: false,
    };
    let mut folders = vec![folder.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_symlink() {
                contents.links = true;
            } else if kind.is_dir() {
                folders.push(path);
            } else if has_extension(&path, &AUDIO_EXTENSIONS) {
                contents.audio.push(path.to_string_lossy().into_owned());
            }
        }
    }
    Ok(contents)
}

/// The files under `root` with one of `extensions`, in path order. Dot folders and dot files
/// (Octo's trash, a NAS's metadata) are left out, as Navidrome leaves them out, and links are
/// never followed.
fn walk(root: &Path, extensions: &[&str]) -> std::io::Result<Vec<String>> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                folders.push(path);
            } else if has_extension(&path, extensions) {
                found.push(path.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    Ok(found)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Whether the filesystem under `root` treats "a" and "A" as one name, told by looking up an
/// existing entry under its name in other capitals. Writes nothing.
pub(crate) fn case_insensitive(root: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let swapped: String = name
            .chars()
            .map(|ch| {
                if ch.is_lowercase() {
                    ch.to_uppercase().next().unwrap_or(ch)
                } else {
                    ch.to_lowercase().next().unwrap_or(ch)
                }
            })
            .collect();
        if swapped == name {
            continue;
        }
        let other = Path::new(root).join(&swapped);
        return match (std::fs::metadata(entry.path()), std::fs::metadata(&other)) {
            (Ok(a), Ok(b)) => same_file(&a, &b),
            _ => false,
        };
    }
    false
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

// ---- playlists ------------------------------------------------------------------------------

/// The playlist's text with every line naming a moved song naming it where it is now, written
/// the way it was (a relative line stays relative). None when nothing changes.
pub(crate) fn rewrite_playlist(playlist: &str, text: &str, moved: &[(String, String)]) -> Option<String> {
    let folder = Path::new(playlist).parent()?;
    let mut changed = false;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let (body, cr) = match line.strip_suffix('\r') {
                Some(body) => (body, "\r"),
                None => (line, ""),
            };
            let trimmed = body.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return line.to_string();
            }
            let relative = !trimmed.starts_with('/');
            let absolute = if relative {
                normalize(&folder.join(trimmed))
            } else {
                normalize(Path::new(trimmed))
            };
            let Some(now) = moved
                .iter()
                .find_map(|(from, to)| PathHelper::moved_path(&absolute, from, to))
            else {
                return line.to_string();
            };
            changed = true;
            let written = if relative {
                relative_to(folder, Path::new(&now))
            } else {
                now
            };
            format!("{written}{cr}")
        })
        .collect();
    changed.then(|| lines.join("\n"))
}

fn normalize(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    format!("/{}", parts.join("/"))
}

fn relative_to(folder: &Path, target: &Path) -> String {
    let from: Vec<String> = normalize(folder)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let to: Vec<String> = normalize(target)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(to[common..].iter().cloned());
    let joined: PathBuf = parts.iter().collect();
    joined.to_string_lossy().into_owned()
}

// ---- undo -----------------------------------------------------------------------------------

impl TidyWorker {
    /// Puts back what the journal holds for the last run (or every run): folders moved back
    /// first, newest first, each only where the old path is free and the songs are as Navidrome
    /// last saw them; the playlists; then a scan that saw the folders back; then the tags, each
    /// only on a file that still has the tags the tidy wrote. What cannot be put back stays in
    /// the journal for another Undo.
    async fn undo(&self, request: &TidyRequest, stopping: &CancellationToken) {
        let entries = self.journal.read_all();
        let runs: HashSet<String> = if request.all_runs {
            entries.iter().map(|entry| entry.run().to_string()).collect()
        } else {
            self.journal.last_run().into_iter().collect()
        };
        self.start(TidyMode::Undo, TidyScope::WholeLibrary, "Checking Navidrome");
        if runs.is_empty() {
            self.finish_with(TidyStatus::Failed, "There is no tidy to undo.".to_string());
            return;
        }
        let gate = self.check_navidrome(true).await;
        if !gate.apply_allowed {
            self.finish_with(TidyStatus::Failed, gate.reason.unwrap_or_default());
            return;
        }
        let Some(library) = self.services.library.clone() else {
            self.finish_with(
                TidyStatus::Failed,
                "Octo has no library service to ask Navidrome for scans.".to_string(),
            );
            return;
        };
        let mut job = match self.write_gate.begin_job(Self::JOB_NAME).await {
            Ok(job) => job,
            Err(holder) => {
                self.finish_with(TidyStatus::Failed, LibraryWriteGate::busy_message(&holder));
                return;
            }
        };
        let root = self.music_root();
        let chosen: Vec<usize> = (0..entries.len())
            .rev()
            .filter(|at| runs.contains(entries[*at].run()))
            .collect();
        self.store.update(|run| {
            run.planned_moves = chosen
                .iter()
                .filter(|at| matches!(entries[**at], TidyJournalEntry::Move { .. }))
                .count();
            run.planned_retags = chosen
                .iter()
                .filter(|at| matches!(entries[**at], TidyJournalEntry::Tags { .. }))
                .count();
        });
        info!("Library tidy undo started: {} journal entries", chosen.len());

        let hold = library.scan_hold().map(|hold| hold.hold());
        let mut done: HashSet<usize> = HashSet::new();
        let stopped = self
            .put_back(
                &root,
                &entries,
                &chosen,
                &mut done,
                library.as_ref(),
                stopping,
                &mut job,
            )
            .await;

        let remaining = remaining(&entries, &done);
        if let Err(failure) = self.journal.rewrite(&remaining) {
            self.store.update(|run| {
                run.errors
                    .push(format!("The journal could not be updated: {failure}"))
            });
        }
        if let Some(mut plan) = self.store.plan()
            && plan.work.take().is_some()
            && let Err(failure) = self.store.save_plan(plan)
        {
            warn!("Library tidy could not forget the undone work: {failure}");
        }
        let wanted = hold.map(|hold| hold.release()).unwrap_or(false);
        let changed = {
            let run = self.store.current();
            run.moved > 0 || run.retagged > 0
        };
        if changed || wanted {
            library.start_scan().await;
        }
        if let Some(stop) = stopped {
            self.stopped(Some(stop));
            return;
        }
        let run = self.store.current();
        let mut summary = format!(
            "Moved {} album folder(s) back and restored the tags on {} file(s).",
            run.moved, run.retagged
        );
        if !remaining.iter().all(|entry| !runs.contains(entry.run())) {
            summary.push_str(
                " Some changes could not be undone yet (the old path is taken, the files changed, or Navidrome had not scanned them); they stay in the journal, so Undo again later.",
            );
        }
        self.finish_with(TidyStatus::Completed, summary);
    }

    #[allow(clippy::too_many_arguments)]
    async fn put_back(
        &self,
        root: &str,
        entries: &[TidyJournalEntry],
        chosen: &[usize],
        done: &mut HashSet<usize>,
        library: &dyn ILocalLibraryService,
        stopping: &CancellationToken,
        job: &mut LibraryJob,
    ) -> Option<Stop> {
        self.store
            .update(|run| run.phase = "Waiting for Navidrome to finish scanning".to_string());
        job.pause();
        let idle = scan_wait::wait_while_scanning(library, self.timing, stopping).await;
        job.resume().await;
        if !idle {
            if let Some(stop) = self.should_stop(stopping) {
                return Some(stop);
            }
            self.finish_with(
                TidyStatus::Failed,
                "Navidrome kept scanning, or did not answer, so nothing was put back. Undo again later."
                    .to_string(),
            );
            return None;
        }
        let Some(scan_start) = library
            .get_scan_status()
            .await
            .and_then(|status| status.last_scan_started_at())
        else {
            self.finish_with(TidyStatus::Failed, NO_STATUS.to_string());
            return None;
        };

        // ---- folders back, newest first ----
        self.store
            .update(|run| run.phase = "Moving files back".to_string());
        let mut moved_back: Vec<(String, String)> = Vec::new();
        for at in chosen {
            let TidyJournalEntry::Move { from, to, .. } = &entries[*at] else {
                continue;
            };
            if let Some(stop) = self.should_stop(stopping) {
                return Some(stop);
            }
            let back = TidyMove {
                from: to.clone(),
                to: from.clone(),
                row: String::new(),
                files: Vec::new(),
            };
            let outcome = tokio::task::spawn_blocking(move || move_back(&back, scan_start))
                .await
                .unwrap_or_else(|panic| Outcome::Failed(panic.to_string()));
            match outcome {
                Outcome::Done => {
                    done.insert(*at);
                    self.follow(library, to, from).await;
                    let parent = Path::new(to).parent().map(|p| p.to_string_lossy().into_owned());
                    BaseDownloadService::try_remove_empty_parents(parent.as_deref(), root);
                    moved_back.push((to.clone(), from.clone()));
                    self.store.update(|run| run.moved += 1);
                }
                Outcome::AlreadyDone => {
                    done.insert(*at);
                }
                Outcome::Held(why) => self.hold(root, to, why),
                Outcome::Failed(why) | Outcome::JournalFailed(why) => self.fail(to, &why),
            }
        }

        // ---- the playlists ----
        let moves_of_run: Vec<(String, String)> = chosen
            .iter()
            .filter_map(|at| match &entries[*at] {
                TidyJournalEntry::Move { from, to, .. } => Some((from.clone(), to.clone())),
                _ => None,
            })
            .collect();
        for at in chosen {
            let TidyJournalEntry::Playlist { path, before, .. } = &entries[*at] else {
                continue;
            };
            let now = std::fs::read_to_string(path).ok();
            let wrote = rewrite_playlist(path, before, &moves_of_run);
            if now.is_some() && now == wrote {
                match state_file::write_atomic(Path::new(path), before.as_bytes()) {
                    Ok(()) => {
                        done.insert(*at);
                    }
                    Err(failure) => self.fail(path, &failure.to_string()),
                }
            } else if now.as_deref() == Some(before.as_str()) {
                done.insert(*at);
            } else {
                self.hold(
                    root,
                    path,
                    "This playlist changed since the tidy updated it, so it is left as it is.".to_string(),
                );
            }
        }

        // ---- the scan between, then the tags ----
        let tags: Vec<usize> = chosen
            .iter()
            .copied()
            .filter(|at| matches!(entries[*at], TidyJournalEntry::Tags { .. }))
            .collect();
        if tags.is_empty() {
            return None;
        }
        if !moved_back.is_empty() {
            self.store
                .update(|run| run.phase = "Waiting for Navidrome to scan".to_string());
            let after = Utc::now();
            job.pause();
            if !library.start_scan().await {
                self.finish_with(
                    TidyStatus::Failed,
                    "Navidrome would not start a scan, so the tags were not restored. Undo again later."
                        .to_string(),
                );
                return None;
            }
            let scanned = scan_wait::wait_for_scan(library, after, self.timing, stopping).await;
            job.resume().await;
            match scanned {
                Ok(_) => {}
                Err(ScanWaitError::Cancelled) => {
                    return self.should_stop(stopping).or(Some(Stop::Cancelled));
                }
                Err(_) => {
                    self.finish_with(
                        TidyStatus::Failed,
                        "Navidrome did not finish a scan after the folders moved back, so the tags were not restored yet. Undo again once it has scanned."
                            .to_string(),
                    );
                    return None;
                }
            }
        }
        self.store.update(|run| run.phase = "Restoring tags".to_string());
        for at in tags {
            if let Some(stop) = self.should_stop(stopping) {
                return Some(stop);
            }
            let TidyJournalEntry::Tags {
                path, before, after, ..
            } = entries[at].clone()
            else {
                continue;
            };
            let shown = path.clone();
            let outcome = tokio::task::spawn_blocking(move || restore_tags(&path, &before, &after))
                .await
                .unwrap_or_else(|panic| Outcome::Failed(panic.to_string()));
            match outcome {
                Outcome::Done => {
                    done.insert(at);
                    self.store.update(|run| run.retagged += 1);
                }
                Outcome::AlreadyDone => {
                    done.insert(at);
                }
                Outcome::Held(why) => self.hold(root, &shown, why),
                Outcome::Failed(why) | Outcome::JournalFailed(why) => self.fail(&shown, &why),
            }
        }
        None
    }
}

/// Moves a folder back, only where the old path is free and its songs are as Navidrome last
/// saw them.
fn move_back(back: &TidyMove, scan_start: DateTime<Utc>) -> Outcome {
    let (from, to) = (Path::new(&back.from), Path::new(&back.to));
    let from_there = std::fs::symlink_metadata(from).is_ok();
    let to_there = std::fs::symlink_metadata(to).is_ok();
    if !from_there && to_there {
        return Outcome::AlreadyDone;
    }
    if !from_there {
        return Outcome::Held(format!("It is no longer at {}.", back.from));
    }
    if to_there {
        return Outcome::Held("Something else is at its old path now, so it stays where it is.".to_string());
    }
    let contents = match walk_all(from) {
        Ok(contents) => contents,
        Err(failure) => return Outcome::Failed(failure.to_string()),
    };
    if contents.links {
        return Outcome::Held("It holds a link, and the tidy never moves one.".to_string());
    }
    for file in &contents.audio {
        match stamp_of(file) {
            Some((_, modified)) if modified < scan_start => {}
            _ => {
                return Outcome::Held(
                    "A song in it changed after Navidrome's last scan began. Undo again once Navidrome has scanned."
                        .to_string(),
                );
            }
        }
    }
    let Some(parent) = to.parent() else {
        return Outcome::Failed("The old folder has no parent.".to_string());
    };
    if let Err(failure) = std::fs::create_dir_all(parent).and_then(|()| std::fs::rename(from, to)) {
        return Outcome::Failed(failure.to_string());
    }
    Outcome::Done
}

/// Puts a file's artist tags back, only while they are still the ones the tidy wrote: a
/// different file now at that path, or tags someone edited since, are never overwritten.
fn restore_tags(path: &str, before: &TidyTags, after: &TidyTags) -> Outcome {
    let mut file = match TagFile::open(path) {
        Ok(file) => file,
        Err(_) if !Path::new(path).exists() => {
            return Outcome::Held("It is not at the path it was re-tagged at.".to_string());
        }
        Err(failure) => return Outcome::Failed(failure.to_string()),
    };
    let now = read_tags(&file);
    if now == *before {
        return Outcome::AlreadyDone;
    }
    if now != *after {
        return Outcome::Held(
            "Its artist tags are not the ones the tidy wrote, so they are left as they are.".to_string(),
        );
    }
    write_tags(&mut file, after, before);
    match file.save() {
        Ok(()) => Outcome::Done,
        Err(failure) => Outcome::Failed(failure.to_string()),
    }
}

/// The journal entries an undo did not put back, oldest first. A tag restore supersedes every
/// NEWER tag entry for the same file: the older entry's before is the original, and keeping a
/// newer one would put an in-between value back next time. A stamp goes with its file's tags.
pub(crate) fn remaining(oldest_first: &[TidyJournalEntry], done: &HashSet<usize>) -> Vec<TidyJournalEntry> {
    let restored: Vec<(usize, &str)> = done
        .iter()
        .filter_map(|at| match &oldest_first[*at] {
            TidyJournalEntry::Tags { path, .. } => Some((*at, path.as_str())),
            _ => None,
        })
        .collect();
    let superseded = |at: usize, path: &str| {
        restored
            .iter()
            .any(|(restored_at, restored_path)| *restored_at <= at && *restored_path == path)
    };
    oldest_first
        .iter()
        .enumerate()
        .filter(|(at, entry)| {
            if done.contains(at) {
                return false;
            }
            match entry {
                TidyJournalEntry::Tags { path, .. } | TidyJournalEntry::Stamp { path, .. } => {
                    !superseded(*at, path)
                }
                _ => true,
            }
        })
        .map(|(_, entry)| entry.clone())
        .collect()
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
