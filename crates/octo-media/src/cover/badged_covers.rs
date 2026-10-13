//! Badged covers of songs found outside the library, kept for an hour so a list scrolling past
//! the same songs again does not fetch and stamp each picture again. Bounded by count and by
//! bytes; the oldest go first.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// How long a badged cover is served from memory.
pub const KEEP_FOR: Duration = Duration::from_secs(60 * 60);
/// The most badged covers kept.
pub const MAX_ENTRIES: usize = 2000;
/// The most bytes kept, all covers together.
pub const MAX_BYTES: usize = 128 * 1024 * 1024;

struct Kept {
    bytes: Arc<Vec<u8>>,
    until: Instant,
    /// Which put this is, so a key put again leaves its older place in `order` stale.
    seq: u64,
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Kept>,
    /// Keys oldest put first, each with the put it was; a place whose put is not the key's
    /// current one is stale and skipped, so removing a key never searches this.
    order: VecDeque<(u64, String)>,
    bytes: usize,
    next_seq: u64,
}

impl State {
    fn remove(&mut self, key: &str) {
        if let Some(kept) = self.entries.remove(key) {
            self.bytes -= kept.bytes.len();
        }
    }

    fn is_current(&self, seq: u64, key: &str) -> bool {
        self.entries.get(key).is_some_and(|kept| kept.seq == seq)
    }

    /// Removes the oldest cover kept; false when there is none.
    fn remove_oldest(&mut self) -> bool {
        while let Some((seq, key)) = self.order.pop_front() {
            if self.is_current(seq, &key) {
                self.remove(&key);
                return true;
            }
        }
        false
    }

    /// Removes the covers expired at `now`: every cover is kept as long, so they are the oldest.
    fn remove_expired(&mut self, now: Instant) {
        while let Some((seq, key)) = self.order.front() {
            let expired = match self.entries.get(key) {
                Some(kept) if kept.seq == *seq => kept.until <= now,
                // Stale: dropped as it is reached.
                _ => true,
            };
            if !expired {
                break;
            }
            let (_, key) = self.order.pop_front().expect("a front");
            if self.entries.get(&key).is_some_and(|kept| kept.until <= now) {
                self.remove(&key);
            }
        }
    }

    /// Drops the stale places once they outnumber the covers, so `order` stays in proportion.
    fn compact(&mut self) {
        if self.order.len() > 2 * self.entries.len() + 16 {
            let entries = &self.entries;
            self.order
                .retain(|(seq, key)| entries.get(key).is_some_and(|kept| kept.seq == *seq));
        }
    }
}

/// The badged covers, by key.
pub struct BadgedCovers {
    max_entries: usize,
    max_bytes: usize,
    state: Mutex<State>,
}

impl Default for BadgedCovers {
    fn default() -> Self {
        BadgedCovers::new(MAX_ENTRIES, MAX_BYTES)
    }
}

impl BadgedCovers {
    pub fn new(max_entries: usize, max_bytes: usize) -> BadgedCovers {
        BadgedCovers {
            max_entries,
            max_bytes,
            state: Mutex::new(State::default()),
        }
    }

    /// The cover kept under `key`, while it is fresh at `now`.
    pub fn get(&self, key: &str, now: Instant) -> Option<Arc<Vec<u8>>> {
        let mut state = self.state.lock();
        let fresh = state.entries.get(key).map(|kept| kept.until > now)?;
        if fresh {
            return state.entries.get(key).map(|kept| Arc::clone(&kept.bytes));
        }
        state.remove(key);
        None
    }

    /// Keeps a cover under `key` from `now`. One larger than the whole budget is not kept.
    pub fn put(&self, key: String, bytes: Arc<Vec<u8>>, now: Instant) {
        if bytes.len() > self.max_bytes || self.max_entries == 0 {
            return;
        }
        let mut state = self.state.lock();
        state.remove(&key);
        // Expired covers go first, then the oldest, until the new one fits.
        state.remove_expired(now);
        while state.entries.len() >= self.max_entries || state.bytes + bytes.len() > self.max_bytes {
            if !state.remove_oldest() {
                break;
            }
        }
        let seq = state.next_seq;
        state.next_seq += 1;
        state.bytes += bytes.len();
        state.order.push_back((seq, key.clone()));
        state.entries.insert(
            key,
            Kept {
                bytes,
                until: now + KEEP_FOR,
                seq,
            },
        );
        state.compact();
    }

    pub fn len(&self) -> usize {
        self.state.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cover(size: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![7; size])
    }

    #[test]
    fn a_kept_cover_is_served_for_an_hour_and_then_not() {
        let covers = BadgedCovers::default();
        let now = Instant::now();
        covers.put("a".into(), cover(10), now);

        assert_eq!(
            covers.get("a", now + Duration::from_secs(59 * 60)),
            Some(cover(10))
        );
        assert_eq!(covers.get("a", now + KEEP_FOR), None);
        assert!(covers.is_empty());
    }

    #[test]
    fn the_oldest_go_first_when_the_count_is_reached() {
        let covers = BadgedCovers::new(2, 1000);
        let now = Instant::now();
        covers.put("a".into(), cover(1), now);
        covers.put("b".into(), cover(1), now);
        covers.put("c".into(), cover(1), now);

        assert_eq!(covers.len(), 2);
        assert_eq!(covers.get("a", now), None);
        assert!(covers.get("b", now).is_some() && covers.get("c", now).is_some());
    }

    #[test]
    fn keeping_one_cover_again_and_again_keeps_the_book_small() {
        let covers = BadgedCovers::new(10, 1000);
        let now = Instant::now();
        covers.put("b".into(), cover(1), now);
        for _ in 0..10_000 {
            covers.put("a".into(), cover(1), now);
        }

        let state = covers.state.lock();
        assert_eq!(state.entries.len(), 2);
        assert!(
            state.order.len() <= 2 * state.entries.len() + 16,
            "{}",
            state.order.len()
        );
    }

    #[test]
    fn a_cover_put_again_is_the_newest_and_outlives_older_ones() {
        let covers = BadgedCovers::new(2, 1000);
        let now = Instant::now();
        covers.put("a".into(), cover(1), now);
        covers.put("b".into(), cover(1), now);
        covers.put("a".into(), cover(2), now);
        covers.put("c".into(), cover(1), now);

        assert_eq!(covers.get("b", now), None);
        assert_eq!(covers.get("a", now), Some(cover(2)));
        assert!(covers.get("c", now).is_some());
    }

    #[test]
    fn the_oldest_go_first_when_the_bytes_are_reached_and_a_huge_one_is_not_kept() {
        let covers = BadgedCovers::new(100, 10);
        let now = Instant::now();
        covers.put("a".into(), cover(6), now);
        covers.put("b".into(), cover(6), now);
        assert_eq!(covers.get("a", now), None);
        assert!(covers.get("b", now).is_some());

        covers.put("huge".into(), cover(11), now);
        assert_eq!(covers.get("huge", now), None);
        assert!(covers.get("b", now).is_some());
    }
}
