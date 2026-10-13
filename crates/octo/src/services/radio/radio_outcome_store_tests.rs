//! What radio learns from listening: a started radio song finished within a day is kept, one
//! never finished is skipped, and each listener's counts give each source a multiplier around 1.
//! Plus the file's own round trip.

use chrono::TimeZone;
use octo_core::radio::radio_provider::{HISTORY, LAST_FM, LIBRARY, LISTEN_BRAINZ, YOU_TUBE_MUSIC};
use octo_core::settings::{AppSettings, RadioSourceSettings};

use super::*;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/rust-migration/fixtures/state/radio-outcomes.json"
);

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap()
}

fn minutes(m: f64) -> TimeDelta {
    TimeDelta::milliseconds((m * 60_000.0) as i64)
}

fn hours(h: f64) -> TimeDelta {
    minutes(h * 60.0)
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Fixture {
            dir: tempfile::tempdir().expect("a temp dir"),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("radio-outcomes.json")
    }

    fn store(&self, learn: bool) -> RadioOutcomeStore {
        let settings = Arc::new(SettingsStore::for_tests(AppSettings {
            radio_sources: RadioSourceSettings {
                learn_from_listening: learn,
                ..Default::default()
            },
            ..Default::default()
        }));
        RadioOutcomeStore::new(self.path(), settings)
    }
}

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn play(
    store: &RadioOutcomeStore,
    listener: &str,
    song: &str,
    start: DateTime<Utc>,
    finish: Option<DateTime<Utc>>,
) {
    store.observe(listener, &s(&[song]), &s(&["false"]), start);
    if let Some(end) = finish {
        store.observe(listener, &s(&[song]), &s(&["true"]), end);
    }
}

fn served(
    store: &RadioOutcomeStore,
    listener: &str,
    songs: &[(&str, &str)],
    at: DateTime<Utc>,
    refresh: bool,
) {
    store.served(
        Some(listener),
        songs.iter().map(|(song, provider)| (*song, Some(*provider))),
        Some(at),
        refresh,
    );
}

fn row(store: &RadioOutcomeStore, provider: &str, now: DateTime<Utc>) -> ProviderOutcome {
    let rows = store.stats(Some(now));
    let mut found = rows.into_iter().filter(|row| row.provider == provider);
    let row = found.next().expect("a row for the provider");
    assert!(found.next().is_none());
    row
}

#[test]
fn a_started_song_finished_within_a_day_is_kept_one_never_finished_is_skipped() {
    let f = Fixture::new();
    let store = f.store(true);
    served(
        &store,
        "alice",
        &[("a", YOU_TUBE_MUSIC), ("b", YOU_TUBE_MUSIC)],
        t0(),
        true,
    );
    play(
        &store,
        "alice",
        "a",
        t0() + minutes(1.0),
        Some(t0() + minutes(3.0)),
    );
    play(&store, "alice", "b", t0() + minutes(4.0), None);

    let later = t0() + hours(25.0);
    let row = row(&store, YOU_TUBE_MUSIC, later);
    assert_eq!(row.plays, 2);
    // The keep has faded for the day the skip took to count.
    assert!((row.keep_rate - 0.5).abs() < 0.005, "{}", row.keep_rate);
    assert_eq!(row.name, "YouTube Music");
}

#[test]
fn what_teaches_nothing_teaches_nothing() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("served", LAST_FM)], t0(), true);
    // A finish with no start seen: a client that never sends starts would only ever count keeps.
    store.observe("alice", &s(&["served"]), &s(&["true"]), t0() + minutes(1.0));
    // A song never served on radio.
    play(
        &store,
        "alice",
        "elsewhere",
        t0() + minutes(2.0),
        Some(t0() + minutes(4.0)),
    );
    // A start long after the radio served it.
    play(
        &store,
        "alice",
        "served",
        t0() + hours(7.0),
        Some(t0() + hours(7.1)),
    );
    // A song the listener played themselves.
    served(&store, "alice", &[("mine", HISTORY)], t0(), true);
    play(
        &store,
        "alice",
        "mine",
        t0() + minutes(5.0),
        Some(t0() + minutes(9.0)),
    );
    // A song from Navidrome's own picks: no source suggested it.
    served(&store, "alice", &[("owned", LIBRARY)], t0(), true);
    play(
        &store,
        "alice",
        "owned",
        t0() + minutes(10.0),
        Some(t0() + minutes(14.0)),
    );

    assert!(store.stats(Some(t0() + TimeDelta::days(2))).is_empty());
    assert!(
        store
            .multipliers(Some("alice"), Some(t0() + TimeDelta::days(2)))
            .is_empty()
    );
}

#[test]
fn one_submission_for_several_ids_is_for_all_of_them() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("a", LAST_FM), ("b", LAST_FM)], t0(), true);
    store.observe("alice", &s(&["a", "b"]), &s(&["false"]), t0() + minutes(1.0));
    store.observe("alice", &s(&["a", "b"]), &s(&["true"]), t0() + minutes(2.0));
    assert_eq!(row(&store, LAST_FM, t0() + minutes(3.0)).keep_rate, 1.0);
    assert_eq!(row(&store, LAST_FM, t0() + minutes(3.0)).plays, 2);
}

#[test]
fn one_star_is_three_skips() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("a", LISTEN_BRAINZ)], t0(), true);
    store.observe("alice", &s(&["a"]), &s(&["false"]), t0() + minutes(1.0));
    store.disliked("alice", "a", t0() + minutes(2.0));
    let row = row(&store, LISTEN_BRAINZ, t0() + minutes(3.0));
    assert_eq!(row.plays, 3);
    assert_eq!(row.keep_rate, 0.0);
}

#[test]
fn outcomes_fade_by_half_every_sixty_days() {
    let f = Fixture::new();
    let store = f.store(true);
    for i in 0..10 {
        served(&store, "alice", &[(&format!("s{i}"), LAST_FM)], t0(), true);
        play(
            &store,
            "alice",
            &format!("s{i}"),
            t0() + minutes(f64::from(i)),
            Some(t0() + minutes(f64::from(i + 1))),
        );
    }
    assert_eq!(row(&store, LAST_FM, t0() + hours(1.0)).plays, 10);
    assert_eq!(
        row(&store, LAST_FM, t0() + TimeDelta::days(HALF_LIFE_DAYS as i64)).plays,
        5
    );
}

#[test]
fn a_source_skipped_more_than_the_listeners_average_counts_less_one_kept_more_counts_more() {
    let f = Fixture::new();
    let store = f.store(true);
    let mut at = t0();
    for i in 0..40 {
        let (y, l) = (format!("y{i}"), format!("l{i}"));
        served(&store, "alice", &[(&y, YOU_TUBE_MUSIC), (&l, LAST_FM)], at, true);
        play(&store, "alice", &y, at + minutes(1.0), Some(at + minutes(4.0)));
        play(&store, "alice", &l, at + minutes(5.0), None);
        at += minutes(10.0);
    }
    let multipliers = store.multipliers(Some("alice"), Some(at + TimeDelta::days(2)));
    assert!(
        multipliers[YOU_TUBE_MUSIC] > 1.2,
        "{}",
        multipliers[YOU_TUBE_MUSIC]
    );
    assert!(multipliers[LAST_FM] < 0.8, "{}", multipliers[LAST_FM]);
}

#[test]
fn a_few_plays_barely_move_a_source() {
    let counts: IndexMap<String, (f64, f64)> = [
        (YOU_TUBE_MUSIC.to_string(), (3.0, 0.0)),
        (LAST_FM.to_string(), (30.0, 30.0)),
    ]
    .into_iter()
    .collect();
    let multiplier = multipliers_of(&counts)[YOU_TUBE_MUSIC];
    assert!((1.0..=1.1).contains(&multiplier), "{multiplier}");
}

#[test]
fn multipliers_stay_within_their_bounds() {
    let counts: IndexMap<String, (f64, f64)> = [
        (YOU_TUBE_MUSIC.to_string(), (0.0, 1000.0)),
        (LAST_FM.to_string(), (1000.0, 0.0)),
    ]
    .into_iter()
    .collect();
    let multipliers = multipliers_of(&counts);
    assert_eq!(multipliers[YOU_TUBE_MUSIC], LOWEST_MULTIPLIER);
    assert_eq!(multipliers[LAST_FM], HIGHEST_MULTIPLIER);
}

#[test]
fn with_learning_off_nothing_is_recorded() {
    let f = Fixture::new();
    let store = f.store(false);
    served(&store, "alice", &[("a", LAST_FM)], t0(), true);
    play(
        &store,
        "alice",
        "a",
        t0() + minutes(1.0),
        Some(t0() + minutes(2.0)),
    );
    assert!(store.stats(Some(t0() + hours(1.0))).is_empty());
    assert!(
        store
            .multipliers(Some("alice"), Some(t0() + hours(1.0)))
            .is_empty()
    );
}

#[test]
fn listeners_are_one_person_whatever_the_case() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "Alice", &[("a", LAST_FM)], t0(), true);
    play(
        &store,
        "alice ",
        "a",
        t0() + minutes(1.0),
        Some(t0() + minutes(2.0)),
    );
    assert_eq!(
        store.multipliers(Some("ALICE"), Some(t0() + minutes(3.0))).len(),
        1
    );
}

#[test]
fn a_station_refetch_does_not_renew_the_window() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("a", LAST_FM)], t0(), false);
    served(&store, "alice", &[("a", LAST_FM)], t0() + hours(5.0), false);
    play(&store, "alice", "a", t0() + hours(7.0), Some(t0() + hours(7.1)));
    assert!(store.stats(Some(t0() + hours(8.0))).is_empty());

    // A radio the listener asked for again does renew it.
    served(&store, "alice", &[("b", LAST_FM)], t0(), true);
    served(&store, "alice", &[("b", LAST_FM)], t0() + hours(5.0), true);
    play(&store, "alice", "b", t0() + hours(7.0), Some(t0() + hours(7.1)));
    assert_eq!(store.stats(Some(t0() + hours(8.0))).len(), 1);
}

#[test]
fn the_quietest_listener_is_forgotten_past_the_cap() {
    let f = Fixture::new();
    let store = f.store(true);
    for i in 0..=MAX_LISTENERS {
        let at = t0() + minutes(i as f64 * 10.0);
        let user = format!("user{i}");
        served(&store, &user, &[("a", LAST_FM)], at, true);
        play(&store, &user, "a", at + minutes(1.0), Some(at + minutes(2.0)));
    }
    let day = Some(t0() + TimeDelta::days(1));
    assert!(store.multipliers(Some("user0"), day).is_empty());
    assert!(
        !store
            .multipliers(Some(&format!("user{MAX_LISTENERS}")), day)
            .is_empty()
    );
    // The newcomer took the forgotten listener's place, as a Dictionary's freed slot is reused.
    assert_eq!(
        store.state().listeners.keys().next().map(String::as_str),
        Some(format!("user{MAX_LISTENERS}").as_str())
    );
}

#[test]
fn what_was_learned_survives_a_restart_and_forget_clears_it() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("a", LAST_FM)], t0(), true);
    play(
        &store,
        "alice",
        "a",
        t0() + minutes(1.0),
        Some(t0() + minutes(2.0)),
    );
    store.flush();

    let again = f.store(true);
    assert_eq!(row(&again, LAST_FM, t0() + minutes(3.0)).plays, 1);
    again.forget();
    assert!(f.store(true).stats(Some(t0() + minutes(3.0))).is_empty());
    assert_eq!(
        std::fs::read_to_string(f.path()).expect("written"),
        r#"{"Listeners":{}}"#
    );
}

#[test]
fn one_serving_teaches_once() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("a", LAST_FM)], t0(), true);
    play(
        &store,
        "alice",
        "a",
        t0() + minutes(1.0),
        Some(t0() + minutes(4.0)),
    );
    // Played again from the queue an hour later: the listener's choice, not the source's.
    play(&store, "alice", "a", t0() + hours(1.0), Some(t0() + hours(1.1)));
    assert_eq!(row(&store, LAST_FM, t0() + hours(2.0)).plays, 1);
}

#[test]
fn a_station_refetch_never_renews_a_serving_still_remembered() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("a", LAST_FM)], t0(), false);
    // Past the start window, but the first serving is still remembered.
    served(&store, "alice", &[("a", LAST_FM)], t0() + hours(7.0), false);
    play(&store, "alice", "a", t0() + hours(8.0), Some(t0() + hours(8.1)));
    assert!(store.stats(Some(t0() + hours(9.0))).is_empty());
}

#[test]
fn a_finish_within_thirty_seconds_of_its_start_is_a_skip() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("a", YOU_TUBE_MUSIC)], t0(), true);
    play(
        &store,
        "alice",
        "a",
        t0() + minutes(1.0),
        Some(t0() + minutes(1.0) + TimeDelta::seconds(10)),
    );
    let row = row(&store, YOU_TUBE_MUSIC, t0() + minutes(2.0));
    assert_eq!(row.plays, 1);
    assert_eq!(row.keep_rate, 0.0);
}

#[test]
fn a_finish_arriving_days_late_turns_its_skip_back_into_a_keep() {
    let f = Fixture::new();
    let store = f.store(true);
    served(&store, "alice", &[("a", LISTEN_BRAINZ)], t0(), true);
    play(&store, "alice", "a", t0() + minutes(1.0), None);
    assert_eq!(row(&store, LISTEN_BRAINZ, t0() + hours(25.0)).keep_rate, 0.0);

    // The phone was offline for three days; its queue sends the finish now.
    store.observe("alice", &s(&["a"]), &s(&["true"]), t0() + TimeDelta::days(3));
    let row = row(&store, LISTEN_BRAINZ, t0() + TimeDelta::days(3));
    assert_eq!(row.plays, 1);
    assert!((row.keep_rate - 1.0).abs() < 0.005);
}

#[test]
fn a_file_with_nulls_is_read_without_them() {
    let f = Fixture::new();
    std::fs::write(
        f.path(),
        r#"{"Listeners":{"alice":{"lastfm":{"Kept":2,"Skipped":1,"UpdatedUtc":"2026-10-04T12:00:00Z"},"youtube-music":null},"bob":null}}"#,
    )
    .expect("writes");
    let store = f.store(true);
    assert_eq!(row(&store, LAST_FM, t0()).plays, 3);
    assert_eq!(store.stats(Some(t0())).len(), 1);
}

#[test]
fn the_fixture_round_trips_byte_for_byte() {
    let text = std::fs::read_to_string(FIXTURE).expect("the fixture is in the repo");
    let state: OutcomeState = serde_json::from_str(&text).expect("the fixture reads");
    assert_eq!(state.listeners["brandon"]["lastfm"].kept, 12.75);
    assert_eq!(octo_core::json::to_string(&state), text.trim_end_matches('\n'));
}

/// A hand-edited or half-written file can hold nulls; they are dropped, not crashed on.
#[test]
fn nulls_in_the_file_are_dropped() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("radio-outcomes.json");
    std::fs::write(
        &path,
        r#"{"Listeners":{"  ":{"lastfm":{"Kept":1}},"bob":null,"ann":{"lastfm":null},"cy":{"lastfm":null,"sonic":{"Kept":2,"Skipped":1,"UpdatedUtc":"2026-10-04T00:00:00Z"}}}}"#,
    )
    .expect("writes");
    let state = load(&path);
    assert_eq!(state.listeners.keys().collect::<Vec<_>>(), ["cy"]);
    assert_eq!(state.listeners["cy"].keys().collect::<Vec<_>>(), ["sonic"]);
    std::fs::write(&path, "[").expect("writes");
    assert_eq!(load(&path), OutcomeState::default());
}

/// The dashboard's row names the source and spans its multipliers, a station's refetch sends
/// nothing new, and an empty or blank listener teaches nothing.
#[test]
fn stats_name_the_source_and_blank_listeners_teach_nothing() {
    let f = Fixture::new();
    let store = f.store(true);
    store.served(None, [("a", Some(LAST_FM))], Some(t0()), true);
    store.served(Some("  "), [("a", Some(LAST_FM))], Some(t0()), true);
    store.observe(" ", &s(&["a"]), &s(&["false"]), t0());
    assert!(store.stats(Some(t0())).is_empty());
    served(&store, "bob", &[("x", "custom")], t0(), true);
    play(&store, "bob", "x", t0() + minutes(1.0), Some(t0() + minutes(5.0)));
    let rows = store.stats(Some(t0() + minutes(6.0)));
    assert_eq!(rows[0].name, "custom");
    assert_eq!(rows[0].lowest_multiplier, rows[0].highest_multiplier);
}
