//! Signing in on another device with a QR code: an end-to-end encrypted hand-over between a
//! person's own two devices. Octo only relays.
//!
//! 1. A signed-in app starts a hand-over and shows a QR of a link whose fragment holds the token
//!    and a key Octo never sees.
//! 2. The new device redeems the token (with its name and platform) and waits.
//! 3. The first app sees who asked, and the person allows or denies.
//! 4. On allow, the first app seals its saved sign-in with that key and uploads the box; the new
//!    device fetches it once and opens it.
//!
//! Octo keeps a token only as a hash, for [`TOKEN_TTL`]; once redeemed, the decision has
//! [`DECIDE_TTL`] and the box [`BOX_TTL`], and the box goes the moment it is fetched. Nothing
//! is written to disk. A wrong token counts as a failed try from that peer, and each account
//! takes a bounded number of redemptions.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, TimeDelta, Utc};
use octo_core::common::Clock;
use octo_core::common::dotnet::eq_ignore_case;
use octo_core::family::codes::{constant_time_eq, token};
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::family_service::FamilyError;

/// How long a hand-over token works before a new device redeems it.
pub const TOKEN_TTL: TimeDelta = TimeDelta::seconds(120);
/// How long the first device has to allow or deny once a new device redeemed the token.
pub const DECIDE_TTL: TimeDelta = TimeDelta::seconds(120);
/// How long an allowed hand-over waits for its box, and a box for its fetch.
pub const BOX_TTL: TimeDelta = TimeDelta::seconds(60);
/// The largest sealed box Octo takes, in bytes.
pub const MAX_BOX: usize = 16 * 1024;
/// The longest device name or platform kept.
pub const NAME_LIMIT: usize = 64;
/// Wrong tokens one peer may send in [`WINDOW`] before it waits.
pub const PEER_FAILURES: usize = 10;
/// Redemptions one account may see in [`WINDOW`].
pub const ACCOUNT_REDEEMS: usize = 10;
/// The window the two limits above count in.
pub const WINDOW: TimeDelta = TimeDelta::minutes(15);
/// Live hand-overs kept at most; the oldest go first.
const MAX_RECORDS: usize = 2_000;
/// Live hand-overs one account keeps at most; its own oldest go first, so starting many never
/// pushes out another account's.
pub const MAX_PER_ACCOUNT: usize = 8;
/// How often ended hand-overs are dropped from memory, boxes nobody fetched included.
pub const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(5);
/// Throttle keys kept at most.
const MAX_KEYS: usize = 10_000;

/// The words a wrong, used or expired token gets.
pub const TOKEN_REFUSED: &str =
    "This sign-in code has run out or was already used. Show a new one on your other device.";
const THROTTLED: &str = "Too many tries. Wait a few minutes, then show a new code.";

/// Where a hand-over stands, as the new device sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HandoverState {
    /// Redeemed; the first device has not decided yet.
    Waiting,
    /// Allowed; the box is on its way, or here.
    Allowed,
    Denied,
    /// Ran out, or already fetched.
    Expired,
}

/// A new device's request, as the first device sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub id: String,
    pub device_name: String,
    pub platform: String,
}

/// What the new device fetches: where it stands, and the sealed box once there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fetched {
    pub state: HandoverState,
    #[serde(rename = "box", skip_serializing_if = "Option::is_none")]
    pub sealed: Option<String>,
}

#[derive(Debug, Clone)]
struct Redemption {
    id: String,
    id_hash: String,
    device_name: String,
    platform: String,
    state: HandoverState,
    deadline: DateTime<Utc>,
    sealed: Option<String>,
}

#[derive(Debug, Clone)]
struct Record {
    user: String,
    token_hash: String,
    expires: DateTime<Utc>,
    redeemed: Option<Redemption>,
}

impl Record {
    fn ended(&self, now: DateTime<Utc>) -> bool {
        match &self.redeemed {
            None => self.expires <= now,
            Some(r) => r.deadline <= now,
        }
    }
}

fn hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.trim().as_bytes()))
}

fn kept(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(NAME_LIMIT)
        .collect()
}

/// The hand-over records and their throttles, in memory only.
pub struct Handover {
    clock: Clock,
    records: Mutex<Vec<Record>>,
    peer_failures: Mutex<HashMap<String, VecDeque<DateTime<Utc>>>>,
    account_redeems: Mutex<HashMap<String, VecDeque<DateTime<Utc>>>>,
}

/// Tries kept for `key` within [`WINDOW`] of `now`, after dropping older ones.
fn recent(map: &mut HashMap<String, VecDeque<DateTime<Utc>>>, key: &str, now: DateTime<Utc>) -> usize {
    let Some(times) = map.get_mut(key) else {
        return 0;
    };
    while times.front().is_some_and(|at| now - *at >= WINDOW) {
        times.pop_front();
    }
    times.len()
}

fn count(map: &mut HashMap<String, VecDeque<DateTime<Utc>>>, key: &str, now: DateTime<Utc>, cap: usize) {
    if map.len() >= MAX_KEYS && !map.contains_key(key) {
        map.retain(|_, times| times.back().is_some_and(|at| now - *at < WINDOW));
        if map.len() >= MAX_KEYS
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, times)| times.back().copied())
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    let times = map.entry(key.to_string()).or_default();
    times.push_back(now);
    while times.len() > cap {
        times.pop_front();
    }
}

impl Handover {
    pub fn new(clock: Clock) -> Self {
        Handover {
            clock,
            records: Mutex::new(Vec::new()),
            peer_failures: Mutex::new(HashMap::new()),
            account_redeems: Mutex::new(HashMap::new()),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Starts a hand-over for `user`: a new token and when it runs out.
    pub fn start(&self, user: &str) -> (String, DateTime<Utc>) {
        let now = self.now();
        let token = token();
        let expires = now + TOKEN_TTL;
        let mut records = self.records.lock();
        records.retain(|r| !r.ended(now));
        let own: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, r)| eq_ignore_case(&r.user, user.trim()))
            .map(|(at, _)| at)
            .collect();
        if own.len() >= MAX_PER_ACCOUNT {
            records.remove(own[0]);
        } else if records.len() >= MAX_RECORDS {
            records.remove(0);
        }
        records.push(Record {
            user: user.trim().to_string(),
            token_hash: hash(&token),
            expires,
            redeemed: None,
        });
        (token, expires)
    }

    /// A new device redeems `token` from `peer` (the address Octo can vouch for). Answers the
    /// id it fetches its box with. A wrong, used or expired token counts against the peer.
    pub fn redeem(
        &self,
        peer: &str,
        token: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<String, FamilyError> {
        let now = self.now();
        if recent(&mut self.peer_failures.lock(), peer, now) >= PEER_FAILURES {
            return Err(FamilyError::Throttled(THROTTLED.into(), WINDOW));
        }
        let wanted = hash(token);
        let mut records = self.records.lock();
        records.retain(|r| !r.ended(now));
        let found = records
            .iter_mut()
            .find(|r| r.redeemed.is_none() && constant_time_eq(r.token_hash.as_bytes(), wanted.as_bytes()));
        let Some(record) = found else {
            drop(records);
            count(&mut self.peer_failures.lock(), peer, now, PEER_FAILURES * 2);
            return Err(FamilyError::Refused(TOKEN_REFUSED.into()));
        };
        let account = record.user.to_lowercase();
        {
            let mut redeems = self.account_redeems.lock();
            if recent(&mut redeems, &account, now) >= ACCOUNT_REDEEMS {
                return Err(FamilyError::Throttled(THROTTLED.into(), WINDOW));
            }
            count(&mut redeems, &account, now, ACCOUNT_REDEEMS * 2);
        }
        let id = octo_core::family::codes::token();
        record.redeemed = Some(Redemption {
            id_hash: hash(&id),
            id: id.clone(),
            device_name: kept(device_name),
            platform: kept(platform),
            state: HandoverState::Waiting,
            deadline: now + DECIDE_TTL,
            sealed: None,
        });
        Ok(id)
    }

    /// Runs `change` on `user`'s live hand-over for `token`. Someone else's token, or one that
    /// ran out, is not found.
    fn with_own<T>(
        &self,
        user: &str,
        token: &str,
        change: impl FnOnce(&mut Record, DateTime<Utc>) -> Result<T, FamilyError>,
    ) -> Result<T, FamilyError> {
        let now = self.now();
        let wanted = hash(token);
        let mut records = self.records.lock();
        records.retain(|r| !r.ended(now));
        let record = records
            .iter_mut()
            .find(|r| {
                constant_time_eq(r.token_hash.as_bytes(), wanted.as_bytes()) && eq_ignore_case(&r.user, user)
            })
            .ok_or_else(|| FamilyError::NotFound(TOKEN_REFUSED.into()))?;
        change(record, now)
    }

    /// The new device waiting for `user` to decide, if one redeemed `token`.
    pub fn pending(&self, user: &str, token: &str) -> Result<Option<Pending>, FamilyError> {
        self.with_own(user, token, |record, _| {
            Ok(record
                .redeemed
                .as_ref()
                .filter(|r| r.state == HandoverState::Waiting)
                .map(|r| Pending {
                    id: r.id.clone(),
                    device_name: r.device_name.clone(),
                    platform: r.platform.clone(),
                }))
        })
    }

    /// `user` allows or denies the new device that redeemed `token`.
    pub fn decide(&self, user: &str, token: &str, allow: bool) -> Result<HandoverState, FamilyError> {
        self.with_own(user, token, |record, now| {
            let redeemed = record
                .redeemed
                .as_mut()
                .filter(|r| r.state == HandoverState::Waiting)
                .ok_or_else(|| FamilyError::Conflict("No new device is waiting for an answer.".into()))?;
            redeemed.state = if allow {
                HandoverState::Allowed
            } else {
                HandoverState::Denied
            };
            redeemed.deadline = now + BOX_TTL;
            Ok(redeemed.state)
        })
    }

    /// `user` uploads the sealed box for the new device they allowed. At most [`MAX_BOX`]
    /// bytes, once.
    pub fn put_box(&self, user: &str, token: &str, sealed: &str) -> Result<(), FamilyError> {
        if sealed.len() > MAX_BOX {
            return Err(FamilyError::Invalid(format!(
                "The sign-in box is too large: at most {} KB.",
                MAX_BOX / 1024
            )));
        }
        if sealed.trim().is_empty() {
            return Err(FamilyError::Invalid("The sign-in box is empty.".into()));
        }
        self.with_own(user, token, |record, now| {
            let redeemed = record
                .redeemed
                .as_mut()
                .filter(|r| r.state == HandoverState::Allowed && r.sealed.is_none())
                .ok_or_else(|| FamilyError::Conflict("Allow the new device first.".into()))?;
            redeemed.sealed = Some(sealed.to_string());
            redeemed.deadline = now + BOX_TTL;
            Ok(())
        })
    }

    /// Where the hand-over the new device redeemed (`id`) stands, with the box once it is
    /// there. The box is deleted as it is handed out; a denied hand-over ends as it is read.
    pub fn fetch(&self, id: &str) -> Fetched {
        let now = self.now();
        let wanted = hash(id);
        let mut records = self.records.lock();
        records.retain(|r| !r.ended(now));
        let Some(at) = records.iter().position(|r| {
            r.redeemed
                .as_ref()
                .is_some_and(|x| constant_time_eq(x.id_hash.as_bytes(), wanted.as_bytes()))
        }) else {
            return Fetched {
                state: HandoverState::Expired,
                sealed: None,
            };
        };
        let redeemed = records[at].redeemed.as_mut().expect("found by its redemption");
        match redeemed.state {
            HandoverState::Allowed if redeemed.sealed.is_some() => {
                let sealed = redeemed.sealed.take();
                records.remove(at);
                Fetched {
                    state: HandoverState::Allowed,
                    sealed,
                }
            }
            HandoverState::Denied => {
                records.remove(at);
                Fetched {
                    state: HandoverState::Denied,
                    sealed: None,
                }
            }
            state => Fetched { state, sealed: None },
        }
    }

    /// Drops every ended hand-over, with any box nobody fetched, and the throttle counts that
    /// no longer count. Runs every [`SWEEP_EVERY`] while Family is on, so nothing outlives its
    /// time in memory waiting for another hand-over call.
    pub fn sweep(&self) {
        let now = self.now();
        self.records.lock().retain(|r| !r.ended(now));
        for map in [&self.peer_failures, &self.account_redeems] {
            map.lock()
                .retain(|_, times| times.back().is_some_and(|at| now - *at < WINDOW));
        }
    }

    /// Ends every hand-over of `user`, under way or waiting: their password changed, so what
    /// was started with the old one stops.
    pub fn end_user(&self, user: &str) {
        self.records
            .lock()
            .retain(|r| !eq_ignore_case(&r.user, user.trim()));
    }

    /// Hand-over records held in memory, ended ones not yet dropped included.
    pub fn held(&self) -> usize {
        self.records.lock().len()
    }

    /// Live hand-overs, for tests.
    pub fn live(&self) -> usize {
        let now = self.now();
        self.records.lock().iter().filter(|r| !r.ended(now)).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn handover() -> (Handover, Arc<Mutex<DateTime<Utc>>>) {
        let now = Arc::new(Mutex::new(Utc::now()));
        let clock = {
            let now = now.clone();
            Clock::new(move || *now.lock())
        };
        (Handover::new(clock), now)
    }

    #[test]
    fn a_hand_over_goes_from_start_to_box_and_the_box_is_gone_after_one_fetch() {
        let (h, _) = handover();
        let (token, expires) = h.start("alex");
        assert!(expires > Utc::now());
        assert_eq!(h.pending("alex", &token).expect("own"), None);
        let id = h
            .redeem("10.0.0.2", &token, "Pixel 9", "Android")
            .expect("redeemed");
        let pending = h.pending("ALEX", &token).expect("own").expect("waiting");
        assert_eq!(
            (pending.device_name.as_str(), pending.platform.as_str()),
            ("Pixel 9", "Android")
        );
        assert_eq!(h.fetch(&id).state, HandoverState::Waiting);
        assert_eq!(
            h.decide("alex", &token, true).expect("decided"),
            HandoverState::Allowed
        );
        assert_eq!(
            h.fetch(&id),
            Fetched {
                state: HandoverState::Allowed,
                sealed: None
            }
        );
        h.put_box("alex", &token, "opaque-box").expect("kept");
        assert_eq!(
            h.fetch(&id),
            Fetched {
                state: HandoverState::Allowed,
                sealed: Some("opaque-box".into())
            }
        );
        assert_eq!(h.fetch(&id).state, HandoverState::Expired, "deleted on fetch");
        assert_eq!(h.live(), 0);
    }

    #[test]
    fn a_token_works_once_and_only_for_its_starter() {
        let (h, _) = handover();
        let (token, _) = h.start("alex");
        assert!(
            h.pending("sam", &token).is_err(),
            "someone else's token is not found"
        );
        h.redeem("10.0.0.2", &token, "Pixel", "Android")
            .expect("redeemed");
        assert!(
            h.redeem("10.0.0.3", &token, "Other", "iOS").is_err(),
            "single use"
        );
        assert!(h.decide("sam", &token, true).is_err());
        assert!(
            h.put_box("alex", &token, "box").is_err(),
            "a box only after allow"
        );
    }

    #[test]
    fn deny_ends_it_on_both_sides() {
        let (h, _) = handover();
        let (token, _) = h.start("alex");
        let id = h
            .redeem("10.0.0.2", &token, "Pixel", "Android")
            .expect("redeemed");
        assert_eq!(
            h.decide("alex", &token, false).expect("decided"),
            HandoverState::Denied
        );
        assert!(h.put_box("alex", &token, "box").is_err());
        assert_eq!(h.fetch(&id).state, HandoverState::Denied);
        assert_eq!(h.fetch(&id).state, HandoverState::Expired);
    }

    #[test]
    fn tokens_decisions_and_boxes_run_out() {
        let (h, now) = handover();
        let (token, _) = h.start("alex");
        *now.lock() += TOKEN_TTL + TimeDelta::seconds(1);
        assert!(h.redeem("10.0.0.2", &token, "Pixel", "Android").is_err());
        let (token, _) = h.start("alex");
        let id = h
            .redeem("10.0.0.2", &token, "Pixel", "Android")
            .expect("redeemed");
        *now.lock() += DECIDE_TTL + TimeDelta::seconds(1);
        assert_eq!(h.fetch(&id).state, HandoverState::Expired);
        let (token, _) = h.start("alex");
        let id = h
            .redeem("10.0.0.2", &token, "Pixel", "Android")
            .expect("redeemed");
        h.decide("alex", &token, true).expect("allowed");
        h.put_box("alex", &token, "box").expect("kept");
        *now.lock() += BOX_TTL + TimeDelta::seconds(1);
        assert_eq!(
            h.fetch(&id).state,
            HandoverState::Expired,
            "a box nobody fetched is gone"
        );
    }

    #[test]
    fn a_box_over_sixteen_kilobytes_is_never_taken() {
        let (h, _) = handover();
        let (token, _) = h.start("alex");
        let id = h
            .redeem("10.0.0.2", &token, "Pixel", "Android")
            .expect("redeemed");
        h.decide("alex", &token, true).expect("allowed");
        let big = "x".repeat(MAX_BOX + 1);
        assert!(matches!(
            h.put_box("alex", &token, &big),
            Err(FamilyError::Invalid(_))
        ));
        assert_eq!(h.fetch(&id).sealed, None);
        h.put_box("alex", &token, &"x".repeat(MAX_BOX))
            .expect("the largest fits");
        assert_eq!(h.fetch(&id).sealed.map(|b| b.len()), Some(MAX_BOX));
    }

    #[test]
    fn an_unfetched_box_and_its_record_leave_memory_sixty_seconds_on() {
        let (h, now) = handover();
        let (token, _) = h.start("alex");
        h.redeem("10.0.0.2", &token, "Pixel", "Android")
            .expect("redeemed");
        h.decide("alex", &token, true).expect("allowed");
        h.put_box("alex", &token, "sealed-box").expect("kept");
        *now.lock() += BOX_TTL - TimeDelta::seconds(1);
        h.sweep();
        assert_eq!(h.held(), 1, "the box still waits within its minute");
        *now.lock() += TimeDelta::seconds(1);
        h.sweep();
        assert_eq!(h.held(), 0, "nobody fetched it, and it is gone all the same");
        // So does a token nobody redeemed, a decision nobody made and a box never sent.
        h.start("alex");
        let (waiting, _) = h.start("alex");
        h.redeem("10.0.0.2", &waiting, "Pixel", "Android")
            .expect("redeemed");
        let (allowed, _) = h.start("alex");
        h.redeem("10.0.0.3", &allowed, "Pixel", "Android")
            .expect("redeemed");
        h.decide("alex", &allowed, true).expect("allowed");
        assert_eq!(h.held(), 3);
        *now.lock() += TOKEN_TTL.max(DECIDE_TTL);
        h.sweep();
        assert_eq!(h.held(), 0);
    }

    #[test]
    fn deny_and_every_timeout_leave_nothing_behind() {
        let (h, now) = handover();
        let (token, _) = h.start("alex");
        let id = h
            .redeem("10.0.0.2", &token, "Pixel", "Android")
            .expect("redeemed");
        h.decide("alex", &token, false).expect("denied");
        *now.lock() += BOX_TTL;
        h.sweep();
        assert_eq!(h.held(), 0, "a deny nobody read");
        assert_eq!(h.fetch(&id).state, HandoverState::Expired);
        assert!(h.pending("alex", &token).is_err());
        assert!(h.decide("alex", &token, true).is_err());
        assert!(h.put_box("alex", &token, "box").is_err());
        // The throttle's counts go too once their window is over.
        h.redeem("10.0.0.9", "made-up", "x", "y").expect_err("wrong");
        *now.lock() += WINDOW;
        h.sweep();
        assert!(h.peer_failures.lock().is_empty());
        assert!(h.account_redeems.lock().is_empty());
    }

    #[test]
    fn many_hand_overs_from_one_account_never_push_out_another_accounts() {
        let (h, _) = handover();
        let (sams, _) = h.start("sam");
        let mut last = String::new();
        for _ in 0..(MAX_RECORDS + 10) {
            last = h.start("alex").0;
        }
        assert!(h.held() <= MAX_PER_ACCOUNT + 1, "{}", h.held());
        assert_eq!(h.pending("sam", &sams).expect("still there"), None);
        h.redeem("10.0.0.2", &last, "Pixel", "Android")
            .expect("the newest of alex's works");
    }

    #[test]
    fn ending_an_accounts_hand_overs_leaves_everyone_elses() {
        let (h, _) = handover();
        let (alexs, _) = h.start("alex");
        let id = h
            .redeem("10.0.0.2", &alexs, "Pixel", "Android")
            .expect("redeemed");
        h.decide("alex", &alexs, true).expect("allowed");
        h.put_box("alex", &alexs, "box").expect("kept");
        let (sams, _) = h.start("sam");
        h.end_user("ALEX");
        assert_eq!(
            h.fetch(&id),
            Fetched {
                state: HandoverState::Expired,
                sealed: None
            }
        );
        assert!(h.pending("alex", &alexs).is_err());
        assert_eq!(h.pending("sam", &sams).expect("still there"), None);
    }

    #[test]
    fn redemptions_of_one_token_from_many_threads_leave_exactly_one_waiting() {
        let (h, _) = handover();
        let h = Arc::new(h);
        let (token, _) = h.start("alex");
        let tries: Vec<_> = (0..32)
            .map(|n| {
                let (h, token) = (h.clone(), token.clone());
                std::thread::spawn(move || h.redeem(&format!("10.3.0.{n}"), &token, "x", "y").is_ok())
            })
            .collect();
        let won = tries
            .into_iter()
            .map(|t| t.join().expect("ran"))
            .filter(|won| *won)
            .count();
        assert_eq!(won, 1);
        assert!(h.pending("alex", &token).expect("own").is_some());
    }

    #[test]
    fn wrong_tokens_count_per_peer_and_redemptions_per_account() {
        let (h, now) = handover();
        for _ in 0..PEER_FAILURES {
            assert!(matches!(
                h.redeem("10.0.0.9", "made-up", "x", "y"),
                Err(FamilyError::Refused(_))
            ));
        }
        let (token, _) = h.start("alex");
        assert!(matches!(
            h.redeem("10.0.0.9", &token, "x", "y"),
            Err(FamilyError::Throttled(..))
        ));
        // Another peer is not held back, and the token is still good.
        h.redeem("10.0.0.8", &token, "x", "y").expect("redeemed");
        for n in 1..ACCOUNT_REDEEMS {
            let (token, _) = h.start("alex");
            h.redeem(&format!("10.1.0.{n}"), &token, "x", "y")
                .expect("redeemed");
        }
        let (token, _) = h.start("alex");
        assert!(matches!(
            h.redeem("10.2.0.1", &token, "x", "y"),
            Err(FamilyError::Throttled(..))
        ));
        *now.lock() += WINDOW + TimeDelta::seconds(1);
        let (token, _) = h.start("alex");
        h.redeem("10.2.0.1", &token, "x", "y")
            .expect("the window moved on");
        h.redeem("10.0.0.9", "made-up", "x", "y")
            .expect_err("still wrong");
    }
}
