//! What radio learns from listening, and its file, `radio-outcomes.json`: each listener's radio
//! plays per source, faded by age. Only the counts are saved; what was served lives in memory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use indexmap::IndexMap;
use octo_core::common::dotnet;
use octo_core::json::datetime;
use octo_core::last_fm::last_fm_radio_recommendation_service::invariant_culture_compare;
use octo_core::radio::radio_provider;
use octo_core::settings::SettingsStore;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::services::state_file;

/// A listener's radio plays from one source, faded by age.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct OutcomeCounts {
    pub kept: f64,
    pub skipped: f64,
    #[serde(with = "datetime::utc")]
    pub updated_utc: DateTime<Utc>,
}

impl Default for OutcomeCounts {
    fn default() -> Self {
        OutcomeCounts {
            kept: 0.0,
            skipped: 0.0,
            updated_utc: datetime::min_value(),
        }
    }
}

/// The file: listener, then provider key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct OutcomeState {
    #[serde(deserialize_with = "state_file::null_as_default")]
    pub listeners: IndexMap<String, IndexMap<String, OutcomeCounts>>,
}

/// One source on the dashboard: plays counted across listeners, how many were kept, and the range
/// of the multipliers learning gives it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOutcome {
    pub provider: String,
    pub name: String,
    pub plays: i32,
    pub keep_rate: f64,
    pub lowest_multiplier: f64,
    pub highest_multiplier: f64,
}

/// The state as the store starts from it. A hand-edited or half-written file can hold nulls;
/// they are dropped, not crashed on: a blank listener, a listener with no sources, a source with
/// no counts. A file that cannot be read starts learning again, with a warning.
pub fn load(path: &Path) -> OutcomeState {
    let read = || -> anyhow::Result<OutcomeState> {
        let Some(text) = state_file::read_text(path)? else {
            return Ok(OutcomeState::default());
        };
        let mut value: Value = serde_json::from_str(&text)?;
        if let Some(Value::Object(listeners)) = value.get_mut("Listeners") {
            listeners.retain(|listener, sources| match sources {
                Value::Object(sources) => {
                    sources.retain(|_, counts| !counts.is_null());
                    !listener.trim().is_empty() && !sources.is_empty()
                }
                _ => false,
            });
        }
        Ok(serde_json::from_value::<Option<OutcomeState>>(value)?.unwrap_or_default())
    };
    read().unwrap_or_else(|e| {
        warn!(
            "{} could not be read; radio starts learning again: {e}",
            path.display()
        );
        OutcomeState::default()
    })
}

/// Writes the whole state through `<file>.tmp`. False (with a warning) when it could not.
pub fn save(path: &Path, state: &OutcomeState) -> bool {
    match state_file::save_atomic(path, &octo_core::json::to_string(state)) {
        Ok(()) => true,
        Err(e) => {
            warn!("Could not save {}: {e}", path.display());
            false
        }
    }
}

pub const START_WINDOW: TimeDelta = TimeDelta::hours(6);
pub const FINISH_WINDOW: TimeDelta = TimeDelta::hours(24);
pub const LATE_FINISH_WINDOW: TimeDelta = TimeDelta::days(7);
pub const HALF_LIFE_DAYS: f64 = 60.0;
pub const PRIOR_PLAYS: f64 = 30.0;
pub const LOWEST_MULTIPLIER: f64 = 0.5;
pub const HIGHEST_MULTIPLIER: f64 = 1.5;
pub const DISLIKE_WEIGHT: f64 = 3.0;
pub const MAX_SERVED: usize = 20_000;
/// Listeners remembered, like the radio state's own cap; the longest quiet is forgotten first.
pub const MAX_LISTENERS: usize = 100;
const FLUSH_EVERY: TimeDelta = TimeDelta::seconds(5);

type Key = (String, String);

#[derive(Debug, Clone)]
struct Served {
    provider: String,
    served_utc: DateTime<Utc>,
    used: bool,
}

struct Inner {
    state: OutcomeState,
    served: IndexMap<Key, Served>,
    /// Outcomes started and not yet finished: the provider and the start.
    open: IndexMap<Key, (String, DateTime<Utc>)>,
    /// Outcomes counted as skips when the finish window closed, kept a week in case the finish
    /// arrives late.
    swept: IndexMap<Key, (String, DateTime<Utc>)>,
    dirty: bool,
    last_flush: DateTime<Utc>,
}

/// What radio learns from listening. A song a source suggested, started within [`START_WINDOW`]
/// of being served, is kept when a finished scrobble follows within [`FINISH_WINDOW`] (the Octo
/// apps can send that late, from an offline queue), and skipped otherwise; a finish later still,
/// within [`LATE_FINISH_WINDOW`], turns that skip back into a keep. A finish within the shortest
/// keep (30 s) of its start is a skip, and a one-star rating is [`DISLIKE_WEIGHT`] skips. One
/// serving teaches once. Each listener's counts per source fade by half every [`HALF_LIFE_DAYS`]
/// days and give that source a multiplier around 1: above it when the listener keeps its songs
/// more than their own average, below when less. [`PRIOR_PLAYS`] pretend plays at the average
/// keep a source with few plays near 1. Only the counts are saved; what was served lives in
/// memory for a day.
pub struct RadioOutcomeStore {
    path: PathBuf,
    /// The radio source settings.
    settings: Arc<SettingsStore>,
    inner: Mutex<Inner>,
    /// Held from the snapshot to the file move, so an older snapshot never lands last.
    file_lock: Mutex<()>,
    /// A finish this soon after its start is a skip; per store, so a test can play a song through
    /// at once.
    shortest_keep: Mutex<TimeDelta>,
}

impl RadioOutcomeStore {
    pub fn new(path: impl Into<PathBuf>, settings: Arc<SettingsStore>) -> Self {
        let path = path.into();
        let state = load(&path);
        RadioOutcomeStore {
            path,
            settings,
            inner: Mutex::new(Inner {
                state,
                served: IndexMap::new(),
                open: IndexMap::new(),
                swept: IndexMap::new(),
                dirty: false,
                last_flush: datetime::min_value(),
            }),
            file_lock: Mutex::new(()),
            shortest_keep: Mutex::new(TimeDelta::seconds(30)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_shortest_keep(&self, shortest: TimeDelta) {
        *self.shortest_keep.lock() = shortest;
    }

    fn on(&self) -> bool {
        self.settings.current().radio_sources.learn_from_listening
    }

    /// These songs went out on a radio for this listener, each from its source.
    ///
    /// `refresh`: false for a station: clients that sync playlists fetch them again and again,
    /// and a refetch is not the listener choosing the radio, so it never renews a serving still
    /// remembered. True for song radio: asking again is a new serving.
    pub fn served<'a>(
        &self,
        listener: Option<&str>,
        songs: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
        now: Option<DateTime<Utc>>,
        refresh: bool,
    ) {
        let Some(listener) = listener.filter(|l| !dotnet::is_blank(l)) else {
            return;
        };
        if !self.on() {
            return;
        }
        let at = now.unwrap_or_else(Utc::now);
        let who = who(listener);
        let mut inner = self.inner.lock();
        inner.sweep(at);
        for (song, provider) in songs {
            // Only a source's suggestions teach: a song from the listener's own plays or
            // Navidrome's library picks says nothing about any source.
            let Some(provider) = provider else { continue };
            if song.is_empty() || provider == radio_provider::HISTORY || provider == radio_provider::LIBRARY {
                continue;
            }
            let key = (who.clone(), song.to_string());
            if !refresh && inner.served.contains_key(&key) {
                continue;
            }
            inner.served.insert(
                key,
                Served {
                    provider: provider.to_string(),
                    served_utc: at,
                    used: false,
                },
            );
        }
        if inner.served.len() > MAX_SERVED {
            let excess = inner.served.len() - MAX_SERVED;
            for key in oldest(&inner.served, excess, |served| served.served_utc) {
                inner.served.shift_remove(&key);
            }
        }
    }

    /// A scrobble Navidrome accepted for this listener. A start of a served song opens an
    /// outcome; a finish closes it as kept. A finish with no start seen teaches nothing: a client
    /// that never sends starts would otherwise only ever count keeps.
    pub fn observe(&self, listener: &str, ids: &[String], submissions: &[String], now: DateTime<Utc>) {
        if !self.on() || dotnet::is_blank(listener) {
            return;
        }
        let listener = who(listener);
        let shortest_keep = *self.shortest_keep.lock();
        {
            let mut inner = self.inner.lock();
            inner.sweep(now);
            for (index, id) in ids.iter().enumerate() {
                // Read as learn_from_scrobbles reads it: one submission for several ids is for
                // all of them, and none at all means a completed play.
                let completed = if submissions.len() == 1 {
                    is_true(&submissions[0])
                } else {
                    index >= submissions.len() || is_true(&submissions[index])
                };
                let key = (listener.clone(), id.clone());
                if !completed {
                    // One serving opens one outcome: playing the song again later, from the queue
                    // or the library, is the listener's choice, not the source's suggestion.
                    if let Some(served) = inner.served.get_mut(&key)
                        && !served.used
                        && now - served.served_utc <= START_WINDOW
                    {
                        served.used = true;
                        let provider = served.provider.clone();
                        inner.open.insert(key, (provider, now));
                    }
                } else if let Some((provider, started)) = inner.open.shift_remove(&key) {
                    // A finish this soon after its start is a client scrobbling the skip.
                    if now - started < shortest_keep {
                        inner.count(&listener, &provider, 0.0, 1.0, now);
                    } else {
                        inner.count(&listener, &provider, 1.0, 0.0, now);
                    }
                } else if let Some((provider, _)) = inner.swept.shift_remove(&key) {
                    inner.count(&listener, &provider, 1.0, -1.0, now);
                }
            }
        }
        self.flush_if_due();
    }

    /// One star for a song radio just played counts as several skips for its source.
    pub fn disliked(&self, listener: &str, song_id: &str, now: DateTime<Utc>) {
        if !self.on() || dotnet::is_blank(listener) {
            return;
        }
        let listener = who(listener);
        {
            let mut inner = self.inner.lock();
            let key = (listener.clone(), song_id.to_string());
            if let Some((provider, _)) = inner.swept.shift_remove(&key) {
                // Already counted as one skip when its window closed.
                inner.count(&listener, &provider, 0.0, DISLIKE_WEIGHT - 1.0, now);
            } else {
                let provider = match inner.open.shift_remove(&key) {
                    Some((provider, _)) => Some(provider),
                    None => inner
                        .served
                        .get(&key)
                        .filter(|served| now - served.served_utc <= FINISH_WINDOW)
                        .map(|served| served.provider.clone()),
                };
                if let Some(provider) = provider {
                    inner.count(&listener, &provider, 0.0, DISLIKE_WEIGHT, now);
                }
                // One serving is disliked once.
                inner.served.shift_remove(&key);
            }
        }
        self.flush_if_due();
    }

    /// Each source's multiplier for this listener; empty with no data or learning off.
    pub fn multipliers(&self, listener: Option<&str>, now: Option<DateTime<Utc>>) -> IndexMap<String, f64> {
        let Some(listener) = listener.filter(|l| !dotnet::is_blank(l)) else {
            return IndexMap::new();
        };
        if !self.on() {
            return IndexMap::new();
        }
        let at = now.unwrap_or_else(Utc::now);
        let mut inner = self.inner.lock();
        inner.sweep(at);
        match inner.state.listeners.get(&who(listener)) {
            Some(by_provider) => multipliers_of(
                &by_provider
                    .iter()
                    .map(|(provider, counts)| (provider.clone(), faded(counts, at)))
                    .collect(),
            ),
            None => IndexMap::new(),
        }
    }

    pub fn stats(&self, now: Option<DateTime<Utc>>) -> Vec<ProviderOutcome> {
        let at = now.unwrap_or_else(Utc::now);
        let mut inner = self.inner.lock();
        inner.sweep(at);
        let per_listener: Vec<IndexMap<String, (f64, f64)>> = inner
            .state
            .listeners
            .values()
            .map(|by_provider| {
                by_provider
                    .iter()
                    .map(|(provider, counts)| (provider.clone(), faded(counts, at)))
                    .collect()
            })
            .collect();
        let multipliers: Vec<IndexMap<String, f64>> = per_listener.iter().map(multipliers_of).collect();
        let mut providers: Vec<&String> = Vec::new();
        for by_provider in &per_listener {
            for provider in by_provider.keys() {
                if !providers.contains(&provider) {
                    providers.push(provider);
                }
            }
        }
        providers.sort_by(|a, b| invariant_culture_compare(a, b));
        providers
            .into_iter()
            .map(|provider| {
                let counts: Vec<(f64, f64)> = per_listener
                    .iter()
                    .filter_map(|by_provider| by_provider.get(provider).copied())
                    .collect();
                let kept: f64 = counts.iter().map(|(kept, _)| kept).sum();
                let total: f64 = counts.iter().map(|(kept, skipped)| kept + skipped).sum();
                let range: Vec<f64> = multipliers
                    .iter()
                    .filter_map(|m| m.get(provider).copied())
                    .collect();
                let range = if range.is_empty() { vec![1.0] } else { range };
                ProviderOutcome {
                    provider: provider.clone(),
                    name: radio_provider::display_name(Some(provider))
                        .map(str::to_string)
                        .unwrap_or_else(|| provider.clone()),
                    plays: dotnet::round(total, 0) as i32,
                    keep_rate: if total > 0.0 { kept / total } else { 0.0 },
                    lowest_multiplier: range.iter().copied().fold(f64::INFINITY, f64::min),
                    highest_multiplier: range.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                }
            })
            .collect()
    }

    /// Forget everything learned, from the dashboard.
    pub fn forget(&self) {
        {
            let mut inner = self.inner.lock();
            inner.state = OutcomeState::default();
            inner.served.clear();
            inner.open.clear();
            inner.swept.clear();
            inner.dirty = true;
        }
        self.flush();
    }

    fn flush_if_due(&self) {
        let due = Utc::now() - self.inner.lock().last_flush >= FLUSH_EVERY;
        if due {
            self.flush();
        }
    }

    pub fn flush(&self) {
        let _file = self.file_lock.lock();
        let snapshot = {
            let mut inner = self.inner.lock();
            if !inner.dirty {
                return;
            }
            inner.dirty = false;
            inner.last_flush = Utc::now();
            inner.state.clone()
        };
        if !save(&self.path, &snapshot) {
            self.inner.lock().dirty = true;
        }
    }

    /// It writes every few seconds at most; whatever changed since goes down on the way out,
    /// once `stopping` fires.
    pub async fn run_flusher(self: Arc<Self>, stopping: CancellationToken) -> anyhow::Result<()> {
        stopping.cancelled().await;
        self.flush();
        Ok(())
    }
}

impl Inner {
    /// An outcome still open past the finish window was skipped (and is remembered a week, in
    /// case its finish comes late); a song served a day ago is forgotten.
    fn sweep(&mut self, now: DateTime<Utc>) {
        let closed: Vec<(Key, String)> = self
            .open
            .iter()
            .filter(|(_, (_, started))| now - *started > FINISH_WINDOW)
            .map(|(key, (provider, _))| (key.clone(), provider.clone()))
            .collect();
        for (key, provider) in closed {
            self.open.shift_remove(&key);
            self.count(&key.0, &provider, 0.0, 1.0, now);
            self.swept.insert(key, (provider, now));
        }
        self.served
            .retain(|_, served| now - served.served_utc <= FINISH_WINDOW);
        self.swept
            .retain(|_, (_, swept)| now - *swept <= LATE_FINISH_WINDOW);
        if self.swept.len() > MAX_SERVED {
            let excess = self.swept.len() - MAX_SERVED;
            for key in oldest(&self.swept, excess, |(_, swept)| *swept) {
                self.swept.shift_remove(&key);
            }
        }
    }

    fn count(&mut self, listener: &str, provider: &str, kept: f64, skipped: f64, now: DateTime<Utc>) {
        if !self.state.listeners.contains_key(listener) {
            // A Dictionary fills the slot it freed last, so the new listener takes the forgotten
            // one's place in the file.
            let mut at = self.state.listeners.len();
            if self.state.listeners.len() >= MAX_LISTENERS {
                let quietest = self
                    .state
                    .listeners
                    .iter()
                    .enumerate()
                    .min_by(|(_, (_, a)), (_, (_, b))| last_update(a).cmp(&last_update(b)))
                    .map(|(index, _)| index);
                if let Some(index) = quietest {
                    self.state.listeners.shift_remove_index(index);
                    at = index;
                }
            }
            self.state
                .listeners
                .shift_insert(at, listener.to_string(), IndexMap::new());
        }
        let by_provider = self
            .state
            .listeners
            .get_mut(listener)
            .expect("the listener was added above");
        let counts = by_provider
            .entry(provider.to_string())
            .or_insert_with(|| OutcomeCounts {
                updated_utc: now,
                ..Default::default()
            });
        let (faded_kept, faded_skipped) = faded(counts, now);
        // A late finish takes back a skip that has faded since: never below none.
        counts.kept = (faded_kept + kept).max(0.0);
        counts.skipped = (faded_skipped + skipped).max(0.0);
        counts.updated_utc = now;
        self.dirty = true;
    }
}

/// The latest update of any source of a listener (the minimum time for none).
fn last_update(by_provider: &IndexMap<String, OutcomeCounts>) -> DateTime<Utc> {
    by_provider
        .values()
        .map(|counts| counts.updated_utc)
        .max()
        .unwrap_or_else(datetime::min_value)
}

/// The keys of the `count` entries that are oldest by `at`, the earlier added first among
/// equals.
fn oldest<V>(map: &IndexMap<Key, V>, count: usize, at: impl Fn(&V) -> DateTime<Utc>) -> Vec<Key> {
    let mut entries: Vec<(&Key, DateTime<Utc>)> = map.iter().map(|(key, value)| (key, at(value))).collect();
    entries.sort_by_key(|(_, at)| *at);
    entries
        .into_iter()
        .take(count)
        .map(|(key, _)| key.clone())
        .collect()
}

/// Each source's multiplier from a listener's counts (kept, skipped).
pub fn multipliers_of(counts: &IndexMap<String, (f64, f64)>) -> IndexMap<String, f64> {
    let kept: f64 = counts.values().map(|(kept, _)| kept).sum();
    let all: f64 = counts.values().map(|(kept, skipped)| kept + skipped).sum();
    let average = (kept + 1.0) / (all + 2.0);
    counts
        .iter()
        .map(|(provider, (kept, skipped))| {
            let rate = (kept + PRIOR_PLAYS * average) / (kept + skipped + PRIOR_PLAYS);
            (
                provider.clone(),
                (rate / average).clamp(LOWEST_MULTIPLIER, HIGHEST_MULTIPLIER),
            )
        })
        .collect()
}

/// The counts as they stand now: halved every [`HALF_LIFE_DAYS`] since they were last updated.
fn faded(counts: &OutcomeCounts, now: DateTime<Utc>) -> (f64, f64) {
    let days = total_days(now - counts.updated_utc).max(0.0);
    let keep = 0.5_f64.powf(days / HALF_LIFE_DAYS);
    (counts.kept * keep, counts.skipped * keep)
}

/// The span in fractional days, from whole 100-nanosecond ticks.
fn total_days(span: TimeDelta) -> f64 {
    let ticks = span
        .num_microseconds()
        .map(|micros| micros as f64 * 10.0)
        .unwrap_or_else(|| span.num_milliseconds() as f64 * 10_000.0);
    ticks / 864_000_000_000.0
}

fn is_true(value: &str) -> bool {
    dotnet::eq_ignore_case(value, "true") || value == "1"
}

/// Usernames as Navidrome compares them, without case.
fn who(listener: &str) -> String {
    dotnet::to_lower_invariant(listener.trim())
}

/// A whole listener's counts, for a test to read.
#[cfg(test)]
impl RadioOutcomeStore {
    fn state(&self) -> OutcomeState {
        self.inner.lock().state.clone()
    }
}

#[cfg(test)]
#[path = "radio_outcome_store_tests.rs"]
mod tests;
