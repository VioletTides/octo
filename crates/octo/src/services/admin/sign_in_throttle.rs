//! The dashboard sign-in's guard against guessing.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, TimeDelta, Utc};
use octo_core::common::Clock;
use parking_lot::Mutex;

/// Slows down guessing at the dashboard's sign-in: after ten wrong tries from one address in
/// fifteen minutes, that address waits until the oldest try is fifteen minutes old. In memory
/// only. Behind a reverse proxy every request comes from the proxy, so this becomes one shared
/// limit; that can hold up a real sign-in for fifteen minutes, never let a guess through.
pub struct SignInThrottle {
    failures: Mutex<HashMap<String, VecDeque<DateTime<Utc>>>>,
    clock: Clock,
}

impl Default for SignInThrottle {
    fn default() -> Self {
        Self::new()
    }
}

impl SignInThrottle {
    pub const MAX_FAILURES: usize = 10;
    pub const WINDOW: TimeDelta = TimeDelta::minutes(15);

    /// A flood from many addresses must not grow the table forever: past this many addresses,
    /// the ones with nothing recent are dropped.
    const MAX_ADDRESSES: usize = 10_000;

    /// Tries kept per key at most, enough for the highest ceiling a key is given.
    const KEPT_TRIES: usize = 200;

    pub fn new() -> Self {
        SignInThrottle {
            failures: Mutex::new(HashMap::new()),
            clock: Clock::system(),
        }
    }

    /// Reads the time from `clock` from now on.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// How long this address must wait, or None when it may try now.
    pub fn retry_after(&self, key: &str) -> Option<TimeDelta> {
        let mut failures = self.failures.lock();
        let tries = failures.get_mut(key)?;
        let now = self.clock.now();
        while tries.front().is_some_and(|first| now - *first >= Self::WINDOW) {
            tries.pop_front();
        }
        if tries.len() >= Self::MAX_FAILURES {
            tries.front().map(|first| *first + Self::WINDOW - now)
        } else {
            None
        }
    }

    pub fn failed(&self, key: &str) {
        let now = self.clock.now();
        let mut failures = self.failures.lock();
        let tries = failures.entry(key.to_string()).or_default();
        while tries.front().is_some_and(|first| now - *first >= Self::WINDOW) {
            tries.pop_front();
        }
        tries.push_back(now);
        while tries.len() > Self::KEPT_TRIES {
            tries.pop_front();
        }
        if failures.len() > Self::MAX_ADDRESSES {
            failures.retain(|_, tries| tries.back().is_some_and(|last| now - *last < Self::WINDOW));
            trim_oldest(&mut failures, Self::MAX_ADDRESSES, |tries| tries.back().copied());
        }
    }

    /// How many keys the throttle holds now.
    pub fn len(&self) -> usize {
        self.failures.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Counts one try for this key now, before it is checked, unless the key has to wait: then
    /// answers how long, and counts nothing. Checking and counting happen under one lock, so
    /// tries sent all at once cannot all slip under the limit. A right try is cleared with
    /// [`SignInThrottle::succeeded`].
    pub fn attempt(&self, key: &str) -> Option<TimeDelta> {
        let now = self.clock.now();
        let mut failures = self.failures.lock();
        if let Some(tries) = failures.get_mut(key) {
            while tries.front().is_some_and(|first| now - *first >= Self::WINDOW) {
                tries.pop_front();
            }
            if tries.len() >= Self::MAX_FAILURES {
                return tries.front().map(|first| *first + Self::WINDOW - now);
            }
        }
        failures.entry(key.to_string()).or_default().push_back(now);
        if failures.len() > Self::MAX_ADDRESSES {
            failures.retain(|_, tries| tries.back().is_some_and(|last| now - *last < Self::WINDOW));
            trim_oldest(&mut failures, Self::MAX_ADDRESSES, |tries| tries.back().copied());
        }
        None
    }

    pub fn succeeded(&self, key: &str) {
        self.failures.lock().remove(key);
    }

    /// How long the longest wait among these keys is, each with its own ceiling, or None when
    /// none of them has to wait.
    pub fn retry_after_any(&self, keys: &[(String, usize)]) -> Option<TimeDelta> {
        let now = self.clock.now();
        let mut failures = self.failures.lock();
        Self::wait_of(&mut failures, keys, now)
    }

    fn wait_of(
        failures: &mut HashMap<String, VecDeque<DateTime<Utc>>>,
        keys: &[(String, usize)],
        now: DateTime<Utc>,
    ) -> Option<TimeDelta> {
        keys.iter()
            .filter_map(|(key, ceiling)| {
                let tries = failures.get_mut(key)?;
                while tries.front().is_some_and(|first| now - *first >= Self::WINDOW) {
                    tries.pop_front();
                }
                // The try that has to age out before another fits under the ceiling.
                (tries.len() >= *ceiling)
                    .then(|| {
                        tries
                            .get(tries.len() - ceiling)
                            .map(|at| *at + Self::WINDOW - now)
                    })
                    .flatten()
            })
            .max()
    }

    /// [`SignInThrottle::attempt`] under several keys at once, each with its own ceiling: the
    /// try counts under all of them, or under none when any has to wait.
    pub fn attempt_all(&self, keys: &[(String, usize)]) -> Option<TimeDelta> {
        let now = self.clock.now();
        let mut failures = self.failures.lock();
        if let Some(wait) = Self::wait_of(&mut failures, keys, now) {
            return Some(wait);
        }
        for (key, _) in keys {
            failures.entry(key.clone()).or_default().push_back(now);
        }
        if failures.len() > Self::MAX_ADDRESSES {
            failures.retain(|_, tries| tries.back().is_some_and(|last| now - *last < Self::WINDOW));
            trim_oldest(&mut failures, Self::MAX_ADDRESSES, |tries| tries.back().copied());
        }
        None
    }

    /// One more wrong try under every one of these keys.
    pub fn failed_all(&self, keys: &[(String, usize)]) {
        for (key, _) in keys {
            self.failed(key);
        }
    }

    /// Takes back the newest try under every key: one counted before it was checked, which
    /// turned out right or could not be checked at all.
    pub fn forgive_all(&self, keys: &[(String, usize)]) {
        let mut failures = self.failures.lock();
        for (key, _) in keys {
            if let Some(tries) = failures.get_mut(key) {
                tries.pop_back();
                if tries.is_empty() {
                    failures.remove(key);
                }
            }
        }
    }

    /// A wait in words: "1 minute", "12 minutes".
    pub fn minutes(wait: TimeDelta) -> String {
        let seconds = wait.num_milliseconds() as f64 / 1000.0;
        let n = ((seconds / 60.0).ceil() as i64).max(1);
        if n == 1 {
            "1 minute".to_string()
        } else {
            format!("{n} minutes")
        }
    }

    /// `Retry-After` in whole seconds, rounded up, as the sign-in endpoints send it.
    pub fn retry_after_seconds(wait: TimeDelta) -> i64 {
        (wait.num_milliseconds() as f64 / 1000.0).ceil() as i64
    }
}

/// Drops entries of `map` until at most `keep` are left, the ones whose `newest` is oldest (or
/// None) first. A table of tries or log times stays bounded however many keys a flood brings.
pub fn trim_oldest<K, V, T>(map: &mut HashMap<K, V>, keep: usize, newest: impl Fn(&V) -> Option<T>)
where
    K: Clone + Eq + std::hash::Hash,
    T: Ord,
{
    if map.len() <= keep {
        return;
    }
    let mut ages: Vec<(Option<T>, K)> = map
        .iter()
        .map(|(key, value)| (newest(value), key.clone()))
        .collect();
    ages.sort_by(|a, b| a.0.cmp(&b.0));
    let extra = map.len() - keep;
    for (_, key) in ages.into_iter().take(extra) {
        map.remove(&key);
    }
}

/// The key for a request: its remote IP address, or "unknown".
pub fn key_of(remote: Option<std::net::SocketAddr>) -> String {
    remote.map_or_else(|| "unknown".to_string(), |addr| addr.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::Arc;

    #[test]
    fn ten_wrong_tries_make_the_address_wait_until_the_oldest_is_fifteen_minutes_old() {
        let now = Arc::new(Mutex::new(Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap()));
        let clock = {
            let now = now.clone();
            Clock::new(move || *now.lock())
        };
        let throttle = SignInThrottle::new().with_clock(clock);
        for _ in 0..SignInThrottle::MAX_FAILURES - 1 {
            throttle.failed("10.0.0.5");
            *now.lock() += TimeDelta::minutes(1);
        }
        assert_eq!(throttle.retry_after("10.0.0.5"), None);
        throttle.failed("10.0.0.5");
        let wait = throttle.retry_after("10.0.0.5").expect("waits");
        assert_eq!(wait, TimeDelta::minutes(6));
        assert_eq!(SignInThrottle::minutes(wait), "6 minutes");
        assert_eq!(throttle.retry_after("10.0.0.6"), None, "another address may try");

        *now.lock() += TimeDelta::minutes(6);
        assert_eq!(
            throttle.retry_after("10.0.0.5"),
            None,
            "the oldest try has aged out"
        );

        throttle.failed("10.0.0.7");
        throttle.succeeded("10.0.0.7");
        assert!(throttle.failures.lock().get("10.0.0.7").is_none());
    }

    #[test]
    fn an_attempt_counts_before_the_check_and_waits_once_the_tries_are_used_up() {
        let throttle = SignInThrottle::new();
        for _ in 0..SignInThrottle::MAX_FAILURES {
            assert_eq!(throttle.attempt("10.0.0.5|sam"), None);
        }
        assert!(throttle.attempt("10.0.0.5|sam").is_some());
        assert!(throttle.retry_after("10.0.0.5|sam").is_some());
        assert_eq!(throttle.attempt("10.0.0.5|alex"), None, "another account may try");
        throttle.succeeded("10.0.0.5|alex");
        assert!(
            throttle.retry_after("10.0.0.5|sam").is_some(),
            "only that account is cleared"
        );
    }

    #[test]
    fn the_table_stays_bounded_under_a_flood() {
        let throttle = SignInThrottle::new();
        for n in 0..(SignInThrottle::MAX_ADDRESSES + 50) {
            throttle.failed(&format!("10.{}.{}.1", n / 256, n % 256));
        }
        assert!(
            throttle.len() <= SignInThrottle::MAX_ADDRESSES + 1,
            "{}",
            throttle.len()
        );
        for n in 0..(SignInThrottle::MAX_ADDRESSES + 50) {
            let _ = throttle.attempt(&format!("11.{}.{}.1", n / 256, n % 256));
        }
        assert!(
            throttle.len() <= SignInThrottle::MAX_ADDRESSES,
            "{}",
            throttle.len()
        );
        for _ in 0..(SignInThrottle::KEPT_TRIES + 100) {
            throttle.failed("12.0.0.1");
        }
        assert!(throttle.failures.lock()["12.0.0.1"].len() <= SignInThrottle::KEPT_TRIES);
    }

    #[test]
    fn trimming_keeps_the_newest() {
        let mut map: HashMap<&str, Option<u32>> = HashMap::new();
        map.insert("old", Some(1));
        map.insert("none", None);
        map.insert("new", Some(9));
        map.insert("mid", Some(5));
        trim_oldest(&mut map, 2, |at| *at);
        let mut kept: Vec<&str> = map.keys().copied().collect();
        kept.sort_unstable();
        assert_eq!(kept, ["mid", "new"]);
        trim_oldest(&mut map, 5, |at| *at);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn a_wait_reads_in_whole_minutes_rounded_up() {
        assert_eq!(SignInThrottle::minutes(TimeDelta::seconds(5)), "1 minute");
        assert_eq!(SignInThrottle::minutes(TimeDelta::seconds(61)), "2 minutes");
        assert_eq!(
            SignInThrottle::retry_after_seconds(TimeDelta::milliseconds(1500)),
            2
        );
    }

    #[test]
    fn the_key_is_the_remote_address() {
        assert_eq!(key_of(None), "unknown");
        assert_eq!(key_of(Some("10.1.2.3:5555".parse().unwrap())), "10.1.2.3");
        assert_eq!(key_of(Some("[::1]:5555".parse().unwrap())), "::1");
    }

    #[test]
    fn several_keys_count_together_each_with_its_ceiling() {
        let throttle = SignInThrottle::new();
        let keys = |address: &str| {
            vec![
                (format!("{address}|alex"), SignInThrottle::MAX_FAILURES),
                ("account|alex".to_string(), 15),
            ]
        };
        for n in 0..15 {
            assert!(throttle.attempt_all(&keys(&format!("10.0.0.{n}"))).is_none());
        }
        let wait = throttle
            .attempt_all(&keys("10.0.0.99"))
            .expect("the account waits");
        assert!(wait > TimeDelta::zero() && wait <= SignInThrottle::WINDOW);
        assert!(throttle.retry_after_any(&keys("10.0.0.99")).is_some());
        // Taking one try back makes room again; a failure counts it again.
        throttle.forgive_all(&keys("10.0.0.14"));
        assert!(throttle.retry_after_any(&keys("10.0.0.99")).is_none());
        throttle.failed_all(&keys("10.0.0.99"));
        assert!(throttle.retry_after_any(&keys("10.0.0.98")).is_some());
    }
}
