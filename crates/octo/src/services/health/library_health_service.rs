//! Library health on the server, for the dashboard.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::{DateTime, TimeDelta, Utc};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use indexmap::IndexMap;
use octo_core::common::Clock;
use octo_core::settings::{LibraryAction, SettingsStore};
use parking_lot::Mutex;
use serde::Serialize;
use tracing::{debug, error, info, warn};

use super::health_fixes::{self as fixes, FixAnswer, FixOutcome, FixStep, TagChange, fix_actions as actions};
use super::health_words::{self as words, count_text};
use super::keep_marks::{self, KeepMarks};
use super::library_health::{self as health, HealthCheck, HealthReport};
use super::server_song_fields::ServerSongFields;
use crate::services::family::library_index::row_full_path;
use crate::services::family::{LibraryScope, ScopeResolver};
use crate::services::library::library_tag_edits::{self as tag_edits, song_tag_fields};
use crate::services::library::{
    LibraryActionExecutor, LibraryActionRequest, LibraryActionState, LibraryAlbumIndex, LibraryEditOutcome,
    LibraryEditService, LibraryRescan, LibrarySnapshot, LibrarySongRow, SongLookup, UpgradeSources,
};

/// One song of the library as the report holds it.
pub type Row = Arc<LibrarySongRow>;

/// The report on the library as last read, with the songs fixed here left out.
#[derive(Clone)]
pub struct HealthSnapshot {
    pub report: HealthReport<Row>,
    pub songs: Vec<Row>,
    pub fields: Arc<ServerSongFields>,
    pub read_utc: DateTime<Utc>,
}

/// One song a lookup found tags for, or nothing for.
#[derive(Debug, Clone)]
pub struct HealthLookup {
    pub id: String,
    pub title: String,
    pub state: String,
    pub detail: Option<String>,
    pub origin: Option<String>,
    pub changes: Vec<(TagChange, bool)>,
}

/// How a run is going or went: the part of `HealthRun` that changes while it runs.
#[derive(Debug, Clone)]
pub struct RunProgress {
    pub done: usize,
    /// running, done, stopped or failed.
    pub state: String,
    pub finished_utc: Option<DateTime<Utc>>,
    pub current: Option<String>,
    pub outcome: Option<FixOutcome>,
    pub error: Option<String>,
}

/// A run of fixes or lookups, and how it is going or went.
#[derive(Debug)]
pub struct HealthRun {
    pub id: String,
    /// fix, lookup or undo.
    pub kind: String,
    pub label: String,
    pub check: Option<HealthCheck>,
    pub user: String,
    pub total: usize,
    pub started_utc: DateTime<Utc>,
    progress: Mutex<RunProgress>,
    /// Added to while the run goes.
    lookups: Mutex<Vec<HealthLookup>>,
    stop_asked: AtomicBool,
}

impl HealthRun {
    fn new(
        kind: &str,
        label: String,
        check: Option<HealthCheck>,
        user: &str,
        total: usize,
        now: DateTime<Utc>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        HealthRun {
            id,
            kind: kind.to_string(),
            label,
            check,
            user: user.to_string(),
            total,
            started_utc: now,
            progress: Mutex::new(RunProgress {
                done: 0,
                state: "running".to_string(),
                finished_utc: None,
                current: None,
                outcome: None,
                error: None,
            }),
            lookups: Mutex::new(Vec::new()),
            stop_asked: AtomicBool::new(false),
        }
    }

    /// How it is going now, copied.
    pub fn progress(&self) -> RunProgress {
        self.progress.lock().clone()
    }

    /// The lookups so far, copied.
    pub fn lookups_now(&self) -> Vec<HealthLookup> {
        self.lookups.lock().clone()
    }

    pub fn is_running(&self) -> bool {
        self.progress.lock().state == "running"
    }

    fn stop_asked(&self) -> bool {
        self.stop_asked.load(Ordering::SeqCst)
    }

    fn step(&self, current: Option<String>) {
        self.progress.lock().current = current;
    }

    fn one_done(&self) {
        self.progress.lock().done += 1;
    }
}

/// What the page may offer: the same flags the app's library actions answer gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthAbilities {
    pub remove: bool,
    pub edit: bool,
    pub join_albums: bool,
    pub look_up: bool,
    pub add_cover: bool,
    pub restore: bool,
    pub upgrade: bool,
    pub dry_run: bool,
    pub keep_days: i32,
    pub why: Option<String>,
}

/// The services a run uses, each optional: the tests run with none of them.
#[derive(Default, Clone)]
pub struct HealthServices {
    pub edits: Option<Arc<LibraryEditService>>,
    pub executor: Option<Arc<LibraryActionExecutor>>,
    pub upgrade_sources: Option<Arc<UpgradeSources>>,
    /// Fixing duplicates so the best sound ends up in the copy people marked.
    pub keep_marks: Option<Arc<KeepMarks>>,
    /// With Family on, Library health keeps to the shared library: members' own libraries are
    /// never checked or fixed here.
    pub scope: Option<ScopeResolver>,
}

/// How one step is sent; tests answer here instead of touching files.
pub type SendFn = Arc<dyn Fn(FixStep, String) -> BoxFuture<'static, anyhow::Result<FixAnswer>> + Send + Sync>;

/// How one song is looked up; tests answer here.
pub type LookFn = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<SongLookup>> + Send + Sync>;

/// The test seams (`Send`, `Look`).
#[derive(Default, Clone)]
pub struct HealthSeams {
    pub send: Option<SendFn>,
    pub look: Option<LookFn>,
}

type Checking = Shared<BoxFuture<'static, Option<Arc<HealthSnapshot>>>>;

/// The report of a library read, kept while that read is the current one.
struct Checked {
    index: Arc<LibraryAlbumIndex>,
    report: HealthReport<Row>,
    fields: Arc<ServerSongFields>,
    read_utc: DateTime<Utc>,
}

#[derive(Default)]
struct State {
    checked: Option<Checked>,
    checking: Option<Checking>,
    checking_since: Option<DateTime<Utc>>,
    // Songs fixed here: by check, each with when; and songs removed here, with when.
    settled: IndexMap<HealthCheck, HashMap<String, DateTime<Utc>>>,
    removed: HashMap<String, DateTime<Utc>>,
    run: Option<Arc<HealthRun>>,
    undo: Option<(String, Vec<FixStep>, String)>,
}

/// Library health on the server, for the dashboard: the same checks as the Octo app over the
/// library snapshot (Navidrome's own song list, read with Octo's admin sign-in and kept a few
/// minutes), and the same fixes, run one song at a time through the same services the app's
/// libraryAction calls, as the signed-in admin. One run at a time, never tied to a request, and
/// one Navidrome scan at its end. Songs fixed here are left out of their finding until a read of
/// the library from after Navidrome's scan shows them as they are now.
pub struct LibraryHealthService {
    snapshot: Arc<LibrarySnapshot>,
    rescan: Arc<LibraryRescan>,
    /// The library action settings, read at use.
    settings: Arc<SettingsStore>,
    services: HealthServices,
    clock: Clock,
    /// How long after a fix a library read is still taken to be from before Navidrome's scan.
    settle_for: Mutex<TimeDelta>,
    seams: Mutex<HealthSeams>,
    state: Mutex<State>,
}

impl LibraryHealthService {
    /// Most steps one run takes.
    pub const MAX_STEPS: usize = 5000;

    /// The default settle time: ninety seconds.
    pub const SETTLE_FOR: TimeDelta = TimeDelta::seconds(90);

    pub fn new(
        snapshot: Arc<LibrarySnapshot>,
        rescan: Arc<LibraryRescan>,
        settings: Arc<SettingsStore>,
        services: HealthServices,
        clock: Clock,
    ) -> Self {
        LibraryHealthService {
            snapshot,
            rescan,
            settings,
            services,
            clock,
            settle_for: Mutex::new(Self::SETTLE_FOR),
            seams: Mutex::new(HealthSeams::default()),
            state: Mutex::new(State::default()),
        }
    }

    /// Sets the test seams.
    pub fn configure(&self, change: impl FnOnce(&mut HealthSeams)) {
        change(&mut self.seams.lock());
    }

    pub fn settle_for(&self) -> TimeDelta {
        *self.settle_for.lock()
    }

    pub fn set_settle_for(&self, settle_for: TimeDelta) {
        *self.settle_for.lock() = settle_for;
    }

    /// Fixing duplicates so the best sound ends up in the copy people marked, when this server
    /// can.
    pub fn keep_marks(&self) -> Option<&Arc<KeepMarks>> {
        self.services.keep_marks.as_ref()
    }

    /// The run going now or last, for the page and the activity toasts.
    pub fn run(&self) -> Option<Arc<HealthRun>> {
        self.state.lock().run.clone()
    }

    /// When the library check now running began, or None.
    pub fn checking_since(&self) -> Option<DateTime<Utc>> {
        self.state.lock().checking_since
    }

    /// Whether the last finished run can be undone, and how many songs that touches.
    pub fn undo_count(&self) -> usize {
        self.state
            .lock()
            .undo
            .as_ref()
            .map_or(0, |(_, steps, _)| steps.len())
    }

    /// The report, checking the library first when it changed since: `fresh` reads it again
    /// from Navidrome (the page's Check again). None when Navidrome cannot be read.
    pub async fn report(self: &Arc<Self>, fresh: bool) -> Option<HealthSnapshot> {
        if fresh {
            {
                let mut state = self.state.lock();
                if state.checking_since.is_none() {
                    state.checking_since = Some(self.clock.now());
                }
            }
            // A read from now on; the check below then finds it as the current one.
            let _done = RefreshDone(self);
            self.snapshot.refresh().await;
        }
        let checking = {
            let mut state = self.state.lock();
            match &state.checking {
                Some(checking) => checking.clone(),
                None => {
                    let service = Arc::clone(self);
                    let task = tokio::spawn(async move { service.check().await });
                    let checking: Checking = task.map(Result::unwrap_or_default).boxed().shared();
                    state.checking = Some(checking.clone());
                    checking
                }
            }
        };
        let found = checking.await?;
        Some(self.settled(&found))
    }

    async fn check(self: Arc<Self>) -> Option<Arc<HealthSnapshot>> {
        tokio::task::yield_now().await;
        {
            let mut state = self.state.lock();
            if state.checking_since.is_none() {
                state.checking_since = Some(self.clock.now());
            }
        }
        let _done = CheckDone(&self);
        let index = self.snapshot.current().await?;
        let read_utc = self.snapshot.read_at_utc().unwrap_or_else(|| self.clock.now());
        {
            let state = self.state.lock();
            if let Some(known) = &state.checked
                && Arc::ptr_eq(&known.index, &index)
            {
                return Some(Arc::new(HealthSnapshot {
                    report: known.report.clone(),
                    songs: self.shared_rows(index.rows()),
                    fields: Arc::clone(&known.fields),
                    read_utc: known.read_utc,
                }));
            }
        }
        let started = Instant::now();
        let rows = in_search_order(&self.shared_rows(index.rows()));
        let checked = tokio::task::spawn_blocking(move || {
            let fields = ServerSongFields::new(rows.iter().map(|row| &**row));
            let report = health::check(&rows, &fields);
            (report, fields, rows)
        })
        .await;
        let (report, fields, rows) = match checked {
            Ok(checked) => checked,
            Err(e) => {
                warn!("Library health could not check the library: {e}");
                return None;
            }
        };
        let findings: Vec<String> = report
            .findings()
            .into_iter()
            .map(|check| format!("{} {}", check.name(), report.count(check)))
            .collect();
        info!(
            "Library health checked {} songs in {} ms: {}",
            report.checked,
            started.elapsed().as_millis(),
            findings.join(", ")
        );
        let fields = Arc::new(fields);
        self.state.lock().checked = Some(Checked {
            index,
            report: report.clone(),
            fields: Arc::clone(&fields),
            read_utc,
        });
        Some(Arc::new(HealthSnapshot {
            report,
            songs: rows,
            fields,
            read_utc,
        }))
    }

    /// The rows Library health looks at: every row, or the shared library's while members have
    /// libraries of their own (Family on, or turned off with their libraries still there).
    fn shared_rows(&self, rows: &[Row]) -> Vec<Row> {
        match self.services.scope.as_ref().filter(|scope| scope.keeps_shared()) {
            Some(scope) => {
                let view = scope.shared_view();
                rows.iter()
                    .filter(|row| {
                        let full = row_full_path(row.library_path.as_deref(), &row.path);
                        view.allows(&LibraryScope::Shared, Some(&row.id), Some(&full))
                    })
                    .cloned()
                    .collect()
            }
            None => rows.to_vec(),
        }
    }

    /// The report with what was fixed here left out, while the library read is older than the fix.
    fn settled(&self, found: &HealthSnapshot) -> HealthSnapshot {
        let settle_for = self.settle_for();
        let mut state = self.state.lock();
        let stale = |at: &DateTime<Utc>| found.read_utc < *at + settle_for;
        state.removed.retain(|_, at| stale(at));
        for ids in state.settled.values_mut() {
            ids.retain(|_, at| stale(at));
        }
        let fields: &ServerSongFields = &found.fields;
        let removed: HashSet<String> = state.removed.keys().cloned().collect();
        let mut report = found.report.without(&removed, fields);
        for (check, ids) in &state.settled {
            let ids: HashSet<String> = ids.keys().cloned().collect();
            report = report.settled_for(*check, &ids, fields);
        }
        HealthSnapshot {
            report,
            ..found.clone()
        }
    }

    // ---- who may, and what ------------------------------------------------------------------

    /// What the server lets this admin do from the page, the same flags the app reads.
    pub fn abilities(&self, user: &str) -> HealthAbilities {
        let settings = self.settings.current().library_actions.clone();
        let enabled: HashSet<LibraryAction> = settings
            .effective_actions()
            .into_iter()
            .filter(|action| action.enabled)
            .map(|action| action.action)
            .collect();
        let edits = self.services.edits.as_ref();
        let allowed = settings.is_allowed(Some(user));
        let real = settings.enabled && allowed && !settings.dry_run;
        let upgrade_ready = self
            .services
            .upgrade_sources
            .as_ref()
            .is_some_and(|sources| sources.ready());
        let why = if !settings.enabled {
            Some("Library actions are off, so nothing can be fixed from here. Turn them on under Library actions.".to_string())
        } else if !allowed {
            Some(format!(
                "{user} is not on the library actions allowed list, so Octo will not change files for them."
            ))
        } else if settings.dry_run {
            Some("Library actions are in rehearsal mode, so a fix would only be rehearsed. Turn rehearsal off under Library actions.".to_string())
        } else if edits.is_none() {
            Some("This server cannot change library files.".to_string())
        } else {
            None
        };
        HealthAbilities {
            remove: real && enabled.contains(&LibraryAction::Delete) && self.services.executor.is_some(),
            edit: real && edits.is_some(),
            join_albums: real && edits.is_some(),
            look_up: real && edits.is_some(),
            add_cover: real && edits.is_some_and(|edits| edits.has_cover_finder()),
            restore: real && edits.is_some(),
            upgrade: settings.enabled
                && allowed
                && !settings.dry_run
                && enabled.contains(&LibraryAction::BetterQuality)
                && upgrade_ready,
            dry_run: settings.dry_run,
            keep_days: settings.effective_quarantine_retention_days(),
            why,
        }
    }

    /// Why this caller may not change files, or None when they may: the app path's checks.
    pub fn refusal(&self, user: Option<&str>) -> Option<String> {
        match &self.services.edits {
            None => Some("This server cannot change library files.".to_string()),
            Some(edits) => edits.refusal(user, true),
        }
    }

    /// Why these steps are not ones the page may send, or None.
    pub fn invalid(steps: &[FixStep]) -> Option<String> {
        if steps.is_empty() {
            return Some("Nothing to do.".to_string());
        }
        if steps.len() > Self::MAX_STEPS {
            return Some(format!(
                "At most {} changes at a time.",
                words::n0(Self::MAX_STEPS as i64)
            ));
        }
        for step in steps {
            if step.id.trim().is_empty() {
                return Some("A change names no song.".to_string());
            }
            if !actions::ALL.contains(&step.action.as_str()) {
                return Some(format!("Unknown change \"{}\".", step.action));
            }
            if step.action == actions::RETAG {
                let params = step.params();
                if params.is_empty() {
                    return Some(format!("No tags to write for {}.", step.title));
                }
                if let Some(unknown) = params
                    .keys()
                    .find(|key| song_tag_fields::canonical(key).is_none())
                {
                    return Some(format!("Unknown tag \"{unknown}\"."));
                }
                let changes: IndexMap<String, Option<String>> = params
                    .iter()
                    .filter_map(|(key, value)| {
                        song_tag_fields::canonical(key).map(|tag| (tag.to_string(), Some(value.clone())))
                    })
                    .collect();
                if let Some(bad) = tag_edits::invalid(&changes) {
                    return Some(bad);
                }
            }
            if step.action == actions::MOVE_SOUND {
                let from = step.param("from").map(str::trim).unwrap_or("");
                if from.is_empty() || from == step.id.trim() {
                    return Some(format!("No copy to move the sound from for {}.", step.title));
                }
                let params = step.params();
                if let Some(unknown) = keep_marks::unknown_tag(&params) {
                    return Some(format!("Unknown tag \"{unknown}\"."));
                }
                let changes: IndexMap<String, Option<String>> = params
                    .iter()
                    .filter_map(|(key, value)| {
                        song_tag_fields::canonical(key).map(|tag| (tag.to_string(), Some(value.clone())))
                    })
                    .collect();
                if let Some(bad) = tag_edits::invalid(&changes) {
                    return Some(bad);
                }
            }
            if step.action == actions::JOIN_ALBUM
                && step.param("like").is_none_or(|like| like.trim().is_empty())
            {
                return Some(format!("No album to join for {}.", step.title));
            }
        }
        None
    }

    // ---- runs ---------------------------------------------------------------------------------

    /// Starts a run of fix steps as `user`; the run going now, when one is.
    pub fn start_fix(
        self: &Arc<Self>,
        label: &str,
        check: Option<HealthCheck>,
        steps: Vec<FixStep>,
        user: &str,
    ) -> Option<Arc<HealthRun>> {
        self.start_fix_as(label, check, steps, user, false)
    }

    fn start_fix_as(
        self: &Arc<Self>,
        label: &str,
        check: Option<HealthCheck>,
        steps: Vec<FixStep>,
        user: &str,
        is_undo: bool,
    ) -> Option<Arc<HealthRun>> {
        let run = {
            let mut state = self.state.lock();
            if let Some(busy) = state.run.as_ref().filter(|run| run.is_running()) {
                return Some(Arc::clone(busy));
            }
            let kind = if is_undo { "undo" } else { "fix" };
            let run = Arc::new(HealthRun::new(
                kind,
                label.to_string(),
                check,
                user,
                steps.len(),
                self.clock.now(),
            ));
            state.run = Some(Arc::clone(&run));
            if is_undo {
                state.undo = None;
            }
            run
        };
        info!(
            "{user} started Library health \"{label}\" from the dashboard: {} change(s)",
            steps.len()
        );
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let job = tokio::spawn({
                let service = Arc::clone(&service);
                let run = Arc::clone(&run);
                async move { service.fix(run, steps, check).await }
            });
            if let Err(e) = job.await {
                error!("Library health run \"{}\" failed: {e}", run.label);
                let _state = service.state.lock();
                let mut progress = run.progress.lock();
                progress.outcome = Some(FixOutcome {
                    stopped: true,
                    ..FixOutcome::default()
                });
                progress.state = "failed".to_string();
                progress.error = Some(e.to_string());
                progress.finished_utc = Some(service.clock.now());
            }
        });
        None
    }

    /// Starts a lookup of these songs, one after another (each a fingerprint and a catalog search,
    /// a few seconds apiece); the run going now, when one is.
    pub fn start_lookup(
        self: &Arc<Self>,
        songs: Vec<(String, String)>,
        check: Option<HealthCheck>,
        user: &str,
    ) -> Option<Arc<HealthRun>> {
        let run = {
            let mut state = self.state.lock();
            if let Some(busy) = state.run.as_ref().filter(|run| run.is_running()) {
                return Some(Arc::clone(busy));
            }
            let label = if songs.len() == 1 {
                words::LOOK_UP_TAGS.to_string()
            } else {
                format!("Looking up {}", count_text(songs.len(), "song", "songs"))
            };
            let run = Arc::new(HealthRun::new(
                "lookup",
                label,
                check,
                user,
                songs.len(),
                self.clock.now(),
            ));
            state.run = Some(Arc::clone(&run));
            run
        };
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let job = tokio::spawn({
                let service = Arc::clone(&service);
                let run = Arc::clone(&run);
                async move { service.lookup(run, songs).await }
            });
            if let Err(e) = job.await {
                error!("Library health lookup failed: {e}");
                let _state = service.state.lock();
                let mut progress = run.progress.lock();
                progress.state = "failed".to_string();
                progress.error = Some(e.to_string());
                progress.finished_utc = Some(service.clock.now());
            }
        });
        None
    }

    /// Puts back everything the last finished run did. None when it started; else why not.
    pub fn start_undo(self: &Arc<Self>, user: &str) -> Option<String> {
        let undo = {
            let state = self.state.lock();
            if state.run.as_ref().is_some_and(|run| run.is_running()) {
                return Some("A fix is still running. Undo once it is done.".to_string());
            }
            let Some((_, steps, _)) = &state.undo else {
                return Some("There is nothing to undo.".to_string());
            };
            steps.clone()
        };
        self.start_fix_as("Undoing the last change", None, undo, user, true)
            .map(|_| "A fix is still running.".to_string())
    }

    /// Ends the run going now after the song it is on.
    pub fn stop(&self) -> bool {
        let state = self.state.lock();
        match state.run.as_ref().filter(|run| run.is_running()) {
            Some(run) => {
                run.stop_asked.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    async fn fix(self: Arc<Self>, run: Arc<HealthRun>, steps: Vec<FixStep>, check: Option<HealthCheck>) {
        let mut done: Vec<FixStep> = Vec::new();
        let mut unchanged: Vec<FixStep> = Vec::new();
        let mut failed: Vec<(FixStep, String)> = Vec::new();
        let mut rehearsed = false;
        let mut stopped = false;
        {
            let _hold = self.rescan.hold();
            for step in steps {
                if run.stop_asked() {
                    stopped = true;
                    break;
                }
                run.step(Some(step.title.clone()));
                let answer = match self.send(&step, &run.user).await {
                    Ok(answer) => answer,
                    Err(e) => FixAnswer::new("failed", Some(&e.to_string())),
                };
                match answer.state.as_str() {
                    "applied" => done.push(step),
                    "rehearsed" => rehearsed = true,
                    "skipped" if fixes::is_nothing_to_do(answer.detail.as_deref()) => unchanged.push(step),
                    state => {
                        let why = answer
                            .detail
                            .clone()
                            .filter(|words| !words.is_empty())
                            .unwrap_or_else(|| format!("the server said {state}"));
                        failed.push((step, why));
                    }
                }
                run.one_done();
            }
        }
        let notes = self.finish_moves(&run, &mut done, &mut failed).await;
        let any_done = !done.is_empty();
        let outcome = FixOutcome {
            done,
            unchanged,
            failed,
            rehearsed,
            stopped,
            notes,
        };
        let now = self.clock.now();
        {
            let mut state = self.state.lock();
            for id in outcome.removed() {
                state.removed.insert(id, now);
            }
            for step in outcome.done.iter().filter(|step| step.action == actions::RESTORE) {
                state.removed.remove(&step.id);
            }
            // The copy whose sound moved is gone from the library; put back, it is there again.
            for step in &outcome.done {
                let Some(from) = step.param("from").filter(|from| !from.is_empty()) else {
                    continue;
                };
                if step.action == actions::MOVE_SOUND {
                    state.removed.insert(from.to_string(), now);
                } else if step.action == actions::MOVE_BACK {
                    state.removed.remove(from);
                }
            }
            if let Some(check) = check {
                let ids = state.settled.entry(check).or_default();
                for step in outcome.done.iter().filter(|step| {
                    [actions::RETAG, actions::JOIN_ALBUM, actions::COVER].contains(&step.action.as_str())
                }) {
                    ids.insert(step.id.clone(), now);
                }
            }
            // An undo puts songs back as they were, so they belong in their findings again.
            if run.kind == "undo" {
                for ids in state.settled.values_mut() {
                    for step in outcome.done.iter().filter(|step| step.action == actions::UNDO) {
                        ids.remove(&step.id);
                    }
                }
            }
            let undo = outcome.undo_steps();
            if run.kind != "undo" && !undo.is_empty() {
                state.undo = Some((run.user.clone(), undo, run.id.clone()));
            }
            let mut progress = run.progress.lock();
            progress.outcome = Some(outcome.clone());
            progress.state = if stopped { "stopped" } else { "done" }.to_string();
            progress.finished_utc = Some(now);
            progress.current = None;
        }
        info!(
            "{} ran Library health \"{}\": {}",
            run.user,
            run.label,
            outcome.summary()
        );
        if any_done {
            self.schedule_read();
        }
    }

    /// The end of a run that moved sound between copies: each move is checked until Navidrome
    /// keeps the song as the same one, and one it does not keep was put back, so it counts as
    /// failed. Songs put back get their playlist places back. Answers what is worth saying.
    async fn finish_moves(
        &self,
        run: &HealthRun,
        done: &mut Vec<FixStep>,
        failed: &mut Vec<(FixStep, String)>,
    ) -> Vec<String> {
        let Some(keep) = &self.services.keep_marks else {
            return Vec::new();
        };
        let ids = |action: &str| -> Vec<String> {
            done.iter()
                .filter(|step| step.action == action)
                .map(|step| step.id.clone())
                .collect()
        };
        let (moved, back) = (ids(actions::MOVE_SOUND), ids(actions::MOVE_BACK));
        if moved.is_empty() && back.is_empty() {
            return Vec::new();
        }
        run.step(Some(keep_marks::CHECKING.to_string()));
        let (put_back, notes) = keep.finish_run(&moved, &back).await;
        for (id, why) in put_back {
            if let Some(at) = done
                .iter()
                .position(|step| step.action == actions::MOVE_SOUND && step.id == id)
            {
                failed.push((done.remove(at), why));
            }
        }
        notes
    }

    async fn lookup(self: Arc<Self>, run: Arc<HealthRun>, songs: Vec<(String, String)>) {
        let look = self.seams.lock().look.clone();
        let edits = self.services.edits.clone();
        for (id, title) in songs {
            if run.stop_asked() {
                break;
            }
            run.step(Some(title.clone()));
            let looked = match (&look, &edits) {
                (Some(look), _) => look(id.clone()).await,
                (None, Some(edits)) => Ok(edits.lookup(&id).await),
                (None, None) => Err(anyhow::anyhow!("This server cannot change library files.")),
            };
            let lookup = looked.unwrap_or_else(|e| SongLookup::said("failed", e.to_string()));
            let found = lookup.state == "found";
            let entry = HealthLookup {
                id,
                state: lookup.state.clone(),
                detail: if found {
                    None
                } else {
                    Some(
                        lookup
                            .detail
                            .clone()
                            .filter(|detail| !detail.is_empty())
                            .unwrap_or_else(|| format!("Nothing was found for {title}.")),
                    )
                },
                origin: found.then(|| fixes::origin(&lookup)),
                changes: if found {
                    fixes::changes(&lookup)
                } else {
                    Vec::new()
                },
                title,
            };
            run.lookups.lock().push(entry);
            run.one_done();
        }
        let _state = self.state.lock();
        let mut progress = run.progress.lock();
        progress.state = if run.stop_asked() && progress.done < run.total {
            "stopped"
        } else {
            "done"
        }
        .to_string();
        progress.finished_utc = Some(self.clock.now());
        progress.current = None;
    }

    /// One step: the test seam when set, else the same service the app's libraryAction uses.
    async fn send(&self, step: &FixStep, user: &str) -> anyhow::Result<FixAnswer> {
        let seam = self.seams.lock().send.clone();
        match seam {
            Some(send) => send(step.clone(), user.to_string()).await,
            None => self.send_through_services(step, user).await,
        }
    }

    /// One step, through the same service the app's libraryAction uses.
    pub async fn send_through_services(&self, step: &FixStep, user: &str) -> anyhow::Result<FixAnswer> {
        if step.action == actions::MOVE_SOUND || step.action == actions::MOVE_BACK {
            let Some(keep) = &self.services.keep_marks else {
                return Ok(FixAnswer::new(
                    "skipped",
                    Some("This server cannot move songs between copies."),
                ));
            };
            return Ok(if step.action == actions::MOVE_SOUND {
                keep.move_sound(&step.id, step.param("from").unwrap_or(""), user, &step.params())
                    .await
            } else {
                keep.move_back(&step.id, user).await
            });
        }
        if step.action == actions::REMOVE {
            let Some(executor) = &self.services.executor else {
                return Ok(FixAnswer::new("skipped", Some("Library actions are off.")));
            };
            let copy = matches!(step.param("copy"), Some("true" | "1"));
            let mut request = LibraryActionRequest::new(LibraryAction::Delete, step.id.clone(), user);
            request.only_a_copy = copy;
            let outcome = executor.apply(request).await?;
            // The executor never asks for a scan itself; the run's one scan covers it.
            if outcome.state == LibraryActionState::Applied {
                self.rescan.soon();
            }
            let state = match outcome.state {
                LibraryActionState::Applied => "applied",
                LibraryActionState::Rehearsed => "rehearsed",
                LibraryActionState::Skipped => "skipped",
                LibraryActionState::Unresolved => "unresolved",
                _ => "failed",
            };
            return Ok(FixAnswer {
                state: state.to_string(),
                detail: outcome.detail,
            });
        }
        let Some(edits) = &self.services.edits else {
            return Ok(FixAnswer::new(
                "skipped",
                Some("This server cannot change library files."),
            ));
        };
        let result: LibraryEditOutcome = match step.action.as_str() {
            actions::RETAG => {
                let changes: IndexMap<String, Option<String>> = step
                    .params()
                    .into_iter()
                    .map(|(key, value)| {
                        let tag = song_tag_fields::canonical(&key).map_or(key, str::to_string);
                        (tag, Some(value))
                    })
                    .collect();
                edits.retag(&step.id, user, changes).await
            }
            actions::JOIN_ALBUM => {
                edits
                    .join_album(&step.id, step.param("like").unwrap_or(""), user)
                    .await
            }
            actions::COVER => edits.add_cover(&step.id, user, false).await,
            actions::UNDO => edits.undo(&step.id, user).await,
            actions::RESTORE => edits.restore(&step.id, user).await,
            other => LibraryEditOutcome::new("failed", format!("Unknown change \"{other}\".")),
        };
        Ok(FixAnswer {
            state: result.state,
            detail: result.detail,
        })
    }

    /// A read of the library a little after Navidrome's scan, so the page catches up by itself.
    fn schedule_read(self: &Arc<Self>) {
        let service = Arc::clone(self);
        let wait = self.settle_for().to_std().unwrap_or_default();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            if service.report(true).await.is_none() {
                debug!("Library health re-read failed: the library could not be read");
            }
        });
    }
}

/// The songs in the order the apps read them (Navidrome's search3 lists them as they were added),
/// so where a finding names one song of several, like the song that ties a split album's parts
/// together, it names the same one the apps do.
pub fn in_search_order(rows: &[Row]) -> Vec<Row> {
    let mut rows = rows.to_vec();
    // By when each was added: stable, unknown last.
    rows.sort_by_key(|row| (row.added_utc.is_none(), row.added_utc));
    rows
}

/// After a Check again, however it ends: clears when the check began, unless another is running.
struct RefreshDone<'a>(&'a LibraryHealthService);

impl Drop for RefreshDone<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        if state.checking.is_none() {
            state.checking_since = None;
        }
    }
}

/// `finally { _checking = null; _checkingSince = null; }` at the end of a check.
struct CheckDone<'a>(&'a LibraryHealthService);

impl Drop for CheckDone<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        state.checking = None;
        state.checking_since = None;
    }
}

#[cfg(test)]
#[path = "library_health_server_tests.rs"]
mod tests;
