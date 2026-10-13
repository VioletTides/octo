//! The sonic store and its file, `sonic-features.json`: what every analyzed song sounds like, by
//! Navidrome song id, on disk beside the settings, and the songs nearest a seed. Written at most
//! once a minute, like the review sweep's store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use indexmap::IndexMap;
use octo_core::json::datetime;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::services::state_file::{self, null_as_default};

/// One analyzed song.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SonicSong {
    /// What the song's file was when it was read, so a changed file is read again.
    #[serde(deserialize_with = "null_as_default")]
    pub stamp: String,
    #[serde(deserialize_with = "null_as_default")]
    pub title: String,
    #[serde(deserialize_with = "null_as_default")]
    pub artist: String,
    pub album: Option<String>,
    pub duration: Option<i32>,
    /// The analysis's features version (bliss-audio's `FeaturesVersion`).
    pub version: i32,
    /// The features: single-precision numbers, written as `float` was.
    #[serde(rename = "F", deserialize_with = "null_as_default")]
    pub f: Vec<f32>,
}

/// A song the analysis could not read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SonicFailure {
    #[serde(deserialize_with = "null_as_default")]
    pub stamp: String,
    #[serde(deserialize_with = "null_as_default")]
    pub error: String,
    #[serde(with = "datetime::utc")]
    pub at_utc: DateTime<Utc>,
}

impl Default for SonicFailure {
    fn default() -> Self {
        SonicFailure {
            stamp: String::new(),
            error: String::new(),
            at_utc: datetime::min_value(),
        }
    }
}

/// The file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SonicState {
    pub paused: bool,
    pub pass: i32,
    #[serde(with = "datetime::utc_option")]
    pub pass_finished_utc: Option<DateTime<Utc>>,
    #[serde(with = "datetime::utc_option")]
    pub next_pass_utc: Option<DateTime<Utc>>,
    /// By Navidrome song id.
    #[serde(deserialize_with = "null_as_default")]
    pub songs: IndexMap<String, SonicSong>,
    #[serde(deserialize_with = "null_as_default")]
    pub failed: IndexMap<String, SonicFailure>,
}

/// The state as the store starts from it. A hand-edited or half-written file can hold nulls; a
/// song with no features or no stamp, or a failure with no stamp, is dropped, not crashed on. A
/// file that cannot be read starts over, with a warning.
pub fn load(path: &Path) -> SonicState {
    let read = || -> anyhow::Result<SonicState> {
        let Some(text) = state_file::read_text(path)? else {
            return Ok(SonicState::default());
        };
        let mut value: Value = serde_json::from_str(&text)?;
        let has = |entry: &Value, name: &str| entry.get(name).is_some_and(|v| !v.is_null());
        if let Some(Value::Object(songs)) = value.get_mut("Songs") {
            songs.retain(|_, song| has(song, "F") && has(song, "Stamp"));
        }
        if let Some(Value::Object(failed)) = value.get_mut("Failed") {
            failed.retain(|_, failure| has(failure, "Stamp"));
        }
        Ok(serde_json::from_value::<Option<SonicState>>(value)?.unwrap_or_default())
    };
    read().unwrap_or_else(|e| {
        warn!("{} could not be read; starting over: {e}", path.display());
        SonicState::default()
    })
}

/// Writes the whole state through `<file>.tmp`. False (with a warning) when it could not.
pub fn save(path: &Path, state: &SonicState) -> bool {
    match state_file::save_atomic(path, &octo_core::json::to_string(state)) {
        Ok(()) => true,
        Err(e) => {
            warn!("Could not save {}: {e}", path.display());
            false
        }
    }
}

// A whole library's numbers are megabytes of JSON; once a minute is plenty, and a crash costs at
// most a minute of analysis.
const FLUSH_EVERY: TimeDelta = TimeDelta::minutes(1);

/// bliss 0.13's own distance for feature version 2 (its `VERSION2_WEIGHTS`, a weighted
/// Euclidean): tempo counts a quarter, since its beat tracker is unreliable on music with few
/// onsets, and the 13 harmony values share the weight of 3, so harmony cannot outvote timbre and
/// loudness. Another version's numbers are compared unweighted.
pub const VERSION2_WEIGHTS: [f32; 23] = [
    0.25,
    1.0,
    1.0,
    1.0,
    1.0,
    1.0,
    1.0,
    1.0,
    1.0,
    1.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
    3.0 / 13.0,
];

pub const FIT_CLOSE: f64 = 1.2;
pub const FIT_FAR: f64 = 0.6;

/// The distance between two songs' features. Each term is worked out in single precision and
/// summed in double.
pub fn distance(a: &[f32], b: &[f32], version: i32) -> f64 {
    let weights = (version == 2 && a.len() == VERSION2_WEIGHTS.len()).then_some(&VERSION2_WEIGHTS);
    let mut sum = 0.0_f64;
    for i in 0..a.len() {
        let d = a[i] - b[i];
        let weight = weights.map_or(1.0, |w| w[i]);
        sum += f64::from(weight * d * d);
    }
    sum.sqrt()
}

struct Inner {
    state: SonicState,
    dirty: bool,
    last_flush: DateTime<Utc>,
}

/// What every analyzed song sounds like, on disk beside the settings, and the songs nearest a
/// seed.
pub struct SonicStore {
    path: PathBuf,
    inner: Mutex<Inner>,
    /// Held from the snapshot to the file move, so an older snapshot never lands last.
    file_lock: Mutex<()>,
}

impl SonicStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let state = load(&path);
        SonicStore {
            path,
            inner: Mutex::new(Inner {
                state,
                dirty: false,
                last_flush: datetime::min_value(),
            }),
            file_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read<T>(&self, read: impl FnOnce(&SonicState) -> T) -> T {
        read(&self.inner.lock().state)
    }

    pub fn write(&self, write: impl FnOnce(&mut SonicState)) {
        let due = {
            let mut inner = self.inner.lock();
            write(&mut inner.state);
            inner.dirty = true;
            Utc::now() - inner.last_flush >= FLUSH_EVERY
        };
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

    /// Whatever changed since the last write goes down on the way out (the worker's stop).
    pub async fn run_flusher(self: Arc<Self>, stopping: CancellationToken) -> anyhow::Result<()> {
        stopping.cancelled().await;
        self.flush();
        Ok(())
    }

    /// Library songs that sound most like the seed, nearest first. Nothing for a song not
    /// analyzed yet.
    pub fn nearest(&self, seed_id: &str, count: usize) -> Vec<(String, SonicSong, f64)> {
        let inner = self.inner.lock();
        let Some(seed) = inner.state.songs.get(seed_id).filter(|seed| !seed.f.is_empty()) else {
            return Vec::new();
        };
        let mut near: Vec<(String, SonicSong, f64)> = inner
            .state
            .songs
            .iter()
            .filter(|(id, song)| {
                *id != seed_id && song.version == seed.version && song.f.len() == seed.f.len()
            })
            .map(|(id, song)| (id.clone(), song.clone(), distance(&seed.f, &song.f, seed.version)))
            .collect();
        // A stable sort: equal distances keep the file's order.
        near.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        near.truncate(count);
        near
    }

    /// How well each candidate's sound fits the seed, as a score multiplier: [`FIT_CLOSE`] at or
    /// under the 25th percentile of the seed's distances to the whole library, [`FIT_FAR`] at or
    /// over the 75th, in a straight line between. Measured against the seed's own spread, because
    /// how far "close" is differs from library to library and song to song. Empty when the seed
    /// has not been analyzed; a candidate without a sound is left out.
    pub fn fit_factors<'a>(
        &self,
        seed_id: &str,
        candidate_ids: impl IntoIterator<Item = &'a str>,
    ) -> IndexMap<String, f64> {
        let inner = self.inner.lock();
        let Some(seed) = inner.state.songs.get(seed_id).filter(|seed| !seed.f.is_empty()) else {
            return IndexMap::new();
        };
        let mut all: Vec<f64> = inner
            .state
            .songs
            .iter()
            .filter(|(id, song)| {
                *id != seed_id && song.version == seed.version && song.f.len() == seed.f.len()
            })
            .map(|(_, song)| distance(&seed.f, &song.f, seed.version))
            .collect();
        if all.len() < 8 {
            return IndexMap::new();
        }
        all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let near = all[all.len() / 4];
        let far = all[all.len() * 3 / 4];
        let mut fit = IndexMap::new();
        for id in candidate_ids {
            let Some(song) = inner
                .state
                .songs
                .get(id)
                .filter(|song| song.version == seed.version && song.f.len() == seed.f.len())
            else {
                continue;
            };
            let d = distance(&seed.f, &song.f, seed.version);
            let factor = if d <= near {
                FIT_CLOSE
            } else if d >= far || far <= near {
                FIT_FAR
            } else {
                FIT_CLOSE + (FIT_FAR - FIT_CLOSE) * (d - near) / (far - near)
            };
            fit.insert(id.to_string(), factor);
        }
        fit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/rust-migration/fixtures/state/sonic-features.json"
    );

    /// 23 numbers, all zero but the second, which says how far along a line the song sits.
    fn at(x: f32) -> Vec<f32> {
        let mut f = vec![0.0; 23];
        f[1] = x;
        f
    }

    fn song(f: Vec<f32>, version: i32, title: &str) -> SonicSong {
        SonicSong {
            stamp: "1:1".into(),
            title: title.into(),
            artist: "Artist".into(),
            version,
            f,
            ..Default::default()
        }
    }

    fn store(dir: &tempfile::TempDir) -> SonicStore {
        SonicStore::new(dir.path().join("sonic-features.json"))
    }

    /// Single-precision features are written as the shortest round trip, with exponents as
    /// `E-05` and `E+38`.
    #[test]
    fn the_fixture_round_trips_byte_for_byte() {
        let text = std::fs::read_to_string(FIXTURE).expect("the fixture is in the repo");
        let state: SonicState = serde_json::from_str(&text).expect("the fixture reads");
        assert_eq!(state.songs["nd-8f7e6d5c"].f.len(), 10);
        assert_eq!(octo_core::json::to_string(&state), text.trim_end_matches('\n'));
    }

    #[test]
    fn a_song_without_features_or_stamp_is_dropped_on_load() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("sonic-features.json");
        std::fs::write(
            &path,
            r#"{"Pass":1,"Songs":{"a":{"Stamp":"1","F":null},"b":{"Stamp":null,"F":[1]},"c":{"Stamp":"3","F":[0.5]},"d":null},"Failed":{"x":{"Stamp":null},"y":{"Stamp":"9","Error":"no"}}}"#,
        )
        .expect("writes");
        let state = load(&path);
        assert_eq!(state.songs.keys().collect::<Vec<_>>(), ["c"]);
        assert_eq!(state.failed.keys().collect::<Vec<_>>(), ["y"]);
        assert_eq!(state.pass, 1);
    }

    #[test]
    fn distance_is_bliss_weighted_tempo_a_quarter_harmony_three_thirteenths() {
        let a = vec![0.0_f32; 23];
        let mut tempo = a.clone();
        tempo[0] = 1.0;
        let mut timbre = a.clone();
        timbre[1] = 1.0;
        let mut chroma = a.clone();
        chroma[12] = 1.0;
        assert!((distance(&a, &tempo, 2) - 0.25_f64.sqrt()).abs() < 1e-6);
        assert!((distance(&a, &timbre, 2) - 1.0).abs() < 1e-6);
        assert!((distance(&a, &chroma, 2) - (3.0_f64 / 13.0).sqrt()).abs() < 1e-6);
        // Another version's numbers are compared unweighted.
        assert!((distance(&a, &tempo, 1) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn nearest_is_closest_first_and_only_songs_of_the_same_version() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = store(&dir);
        store.write(|s| {
            s.songs.insert("seed".into(), song(at(0.0), 2, "Song"));
            s.songs.insert("far".into(), song(at(0.9), 2, "Song"));
            s.songs.insert("near".into(), song(at(0.1), 2, "Song"));
            s.songs.insert("mid".into(), song(at(0.5), 2, "Song"));
            s.songs.insert("old".into(), song(at(0.05), 1, "Song"));
        });
        let ids: Vec<String> = store
            .nearest("seed", 2)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(ids, ["near", "mid"]);
        assert!(store.nearest("unknown", 5).is_empty());
    }

    #[test]
    fn fit_factors_lean_on_the_seeds_own_spread() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = store(&dir);
        store.write(|s| {
            s.songs.insert("seed".into(), song(at(0.0), 2, "Song"));
            for i in 1..=20 {
                s.songs
                    .insert(format!("s{i}"), song(at(i as f32 / 20.0), 2, "Song"));
            }
        });
        let fit = store.fit_factors("seed", ["s1", "s20", "s10", "nobody"]);
        assert_eq!(fit["s1"], FIT_CLOSE);
        assert_eq!(fit["s20"], FIT_FAR);
        assert!((FIT_FAR..=FIT_CLOSE).contains(&fit["s10"]));
        assert!(!fit.contains_key("nobody"));
    }

    #[test]
    fn fit_factors_need_an_analyzed_seed_and_enough_songs() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = store(&dir);
        store.write(|s| {
            s.songs.insert("seed".into(), song(at(0.0), 2, "Song"));
            for i in 1..=5 {
                s.songs
                    .insert(format!("s{i}"), song(at(i as f32 / 5.0), 2, "Song"));
            }
        });
        assert!(store.fit_factors("seed", ["s1"]).is_empty());
        assert!(store.fit_factors("unanalysed", ["s1"]).is_empty());
    }

    #[test]
    fn what_was_saved_is_there_after_a_restart_and_a_broken_file_starts_over() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let first = store(&dir);
        first.write(|s| {
            s.songs.insert("a".into(), song(at(0.3), 2, "Roads"));
            s.pass = 2;
        });
        first.flush();
        let again = store(&dir);
        assert_eq!(again.read(|s| s.songs["a"].title.clone()), "Roads");
        assert_eq!(again.read(|s| s.songs["a"].f[1]), 0.3_f32);
        assert_eq!(again.read(|s| s.pass), 2);

        std::fs::write(again.path(), "{ not json").expect("writes");
        assert!(store(&dir).read(|s| s.songs.is_empty()));
    }

    #[test]
    fn a_file_with_nulls_is_read_without_them() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(
            dir.path().join("sonic-features.json"),
            r#"{"Pass":3,"Songs":{"a":{"Stamp":"1:1","Version":2,"F":[0.1]},"b":null,"c":{"Stamp":"1:1","F":null}},"Failed":{"x":null},"Paused":false}"#,
        )
        .expect("writes");
        let store = store(&dir);
        assert_eq!(store.read(|s| s.songs.keys().cloned().collect::<Vec<_>>()), ["a"]);
        assert!(store.read(|s| s.failed.is_empty()));
        assert!(store.nearest("a", 5).is_empty());
    }
}
