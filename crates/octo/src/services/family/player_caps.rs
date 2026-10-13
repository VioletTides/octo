//! Navidrome's own players as a second guard. When a member has a family limit on their
//! quality, every Navidrome player of theirs is set to that `maxBitRate`, so Navidrome holds
//! the limit even on a path Octo did not see. Players are only ever lowered: one already at or
//! below the limit is left alone, and a player is never raised.
//!
//! The players are checked when Family's work starts, every 30 minutes, and shortly after a
//! member's device is seen for the first time (Navidrome makes a player on a device's first
//! request, so the check waits a little for it).

use std::sync::Arc;
use std::time::Duration;

use octo_core::settings::SettingsStore;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::device_registry::DeviceRegistry;
use super::family_store::{FamilyStore, Member};
use super::navidrome_admin::NavidromeAdmin;

/// How often every member's players are checked.
const EVERY: Duration = Duration::from_secs(30 * 60);
/// How long after a new device is seen its player is looked for.
const AFTER_NEW_DEVICE: Duration = Duration::from_secs(15);

/// The `maxBitRate` a member's players are held to: their family limit at home. None when they
/// have none.
pub fn player_cap(member: &Member) -> Option<u32> {
    member.abilities().stream_cap.kbps()
}

/// Whether a player at `current` kbps (0 is no limit) must come down to `cap`.
pub fn must_lower(current: i64, cap: u32) -> bool {
    current <= 0 || current > i64::from(cap)
}

/// Keeps members' Navidrome players at or below their family limit.
pub struct PlayerCaps {
    settings: Arc<SettingsStore>,
    store: Arc<FamilyStore>,
    admin: Arc<NavidromeAdmin>,
    devices: Arc<DeviceRegistry>,
    timing: (Duration, Duration),
}

impl PlayerCaps {
    pub fn new(
        settings: Arc<SettingsStore>,
        store: Arc<FamilyStore>,
        admin: Arc<NavidromeAdmin>,
        devices: Arc<DeviceRegistry>,
    ) -> Self {
        PlayerCaps {
            settings,
            store,
            admin,
            devices,
            timing: (EVERY, AFTER_NEW_DEVICE),
        }
    }

    /// The same checks on other timings (tests).
    pub fn with_timing(mut self, every: Duration, after_new_device: Duration) -> Self {
        self.timing = (every, after_new_device);
        self
    }

    /// Lowers one member's players to their limit. Answers how many were lowered.
    pub async fn sync_member(&self, member: &Member) -> usize {
        let Some(cap) = player_cap(member) else {
            return 0;
        };
        if member.navidrome_user_id.trim().is_empty() {
            return 0;
        }
        let players = match self.admin.list_players(&member.navidrome_user_id).await {
            Ok(players) => players,
            Err(e) => {
                warn!(
                    "FAMILY could not read {}'s Navidrome players: {}",
                    member.username,
                    e.body().unwrap_or("no answer")
                );
                return 0;
            }
        };
        let mut lowered = 0;
        for player in players.iter().filter(|p| must_lower(p.max_bit_rate, cap)) {
            match self.admin.set_player_max_bit_rate(&player.id, cap).await {
                Ok(()) => lowered += 1,
                Err(e) => warn!(
                    "FAMILY could not set {}'s player {} to {cap} kbps: {}",
                    member.username,
                    player.name,
                    e.body().unwrap_or("no answer")
                ),
            }
        }
        if lowered > 0 {
            info!(
                "FAMILY set {lowered} of {}'s Navidrome players to {cap} kbps",
                member.username
            );
        }
        lowered
    }

    /// Lowers every member's players. Answers how many were lowered.
    pub async fn sync_all(&self) -> usize {
        if !self.settings.current().family.enabled {
            return 0;
        }
        let mut lowered = 0;
        for member in self.store.members() {
            lowered += self.sync_member(&member).await;
        }
        lowered
    }

    /// Checks now, every 30 minutes, and after each new member device, until `token` is
    /// canceled.
    pub async fn run(self: Arc<Self>, token: CancellationToken) -> anyhow::Result<()> {
        let mut events = self.devices.subscribe();
        let (every, after_new_device) = self.timing;
        let mut ticker = tokio::time::interval(every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = token.cancelled() => return Ok(()),
                _ = ticker.tick() => {
                    self.sync_all().await;
                }
                event = events.recv() => match event {
                    Ok(device) if device.managed => {
                        let caps = Arc::clone(&self);
                        let token = token.clone();
                        tokio::spawn(async move {
                            tokio::select! {
                                () = token.cancelled() => {}
                                () = tokio::time::sleep(after_new_device) => {
                                    if caps.settings.current().family.enabled
                                        && let Some(member) = caps.store.member(&device.username)
                                    {
                                        caps.sync_member(&member).await;
                                    }
                                }
                            }
                        });
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        self.sync_all().await;
                    }
                    Err(RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::family::caller::FamilyCaller;
    use crate::services::family::device_registry::Sighting;
    use crate::services::family::member_credentials::tests::member;
    use crate::services::family::navidrome_admin::tests::{admin_for, navidrome};
    use octo_core::family::abilities::{Abilities, Preset};
    use octo_core::settings::{AppSettings, FamilySettings};
    use serde_json::{Map, Value, json};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn capped(name: &str, kbps: u32) -> Member {
        let mut m = member(name, String::new());
        let mut edits = Map::new();
        edits.insert("streamCap".into(), Value::from(kbps));
        m.edits = edits;
        m
    }

    async fn players(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/player"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": "p-open", "name": "Phone", "userId": "u-alex", "maxBitRate": 0},
                {"id": "p-high", "name": "Laptop", "userId": "u-alex", "maxBitRate": 320},
                {"id": "p-low", "name": "Car", "userId": "u-alex", "maxBitRate": 128},
                {"id": "p-same", "name": "Tablet", "userId": "u-alex", "maxBitRate": 192},
                {"id": "p-sam", "name": "Sam's", "userId": "u-sam", "maxBitRate": 0},
            ])))
            .mount(server)
            .await;
    }

    fn caps_for(server: &MockServer, store: Arc<FamilyStore>) -> (PlayerCaps, Arc<DeviceRegistry>) {
        let (admin, _, _) = admin_for(&server.uri());
        let settings = Arc::new(SettingsStore::for_tests(AppSettings {
            family: FamilySettings {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        }));
        let devices = Arc::new(DeviceRegistry::new(Arc::clone(&store)));
        (
            PlayerCaps::new(settings, store, Arc::new(admin), Arc::clone(&devices)),
            devices,
        )
    }

    fn puts(requests: &[wiremock::Request]) -> Vec<(String, Value)> {
        requests
            .iter()
            .filter(|r| r.method.as_str() == "PUT")
            .map(|r| {
                (
                    r.url.path().to_string(),
                    serde_json::from_slice(&r.body).unwrap_or(Value::Null),
                )
            })
            .collect()
    }

    #[test]
    fn players_are_only_ever_lowered() {
        assert!(must_lower(0, 192));
        assert!(must_lower(320, 192));
        assert!(!must_lower(192, 192));
        assert!(!must_lower(128, 192));
        assert_eq!(player_cap(&capped("alex", 192)), Some(192));
        assert_eq!(player_cap(&member("alex", String::new())), None);
    }

    #[tokio::test]
    async fn a_limited_members_players_come_down_to_the_limit() {
        let server = navidrome().await;
        players(&server).await;
        Mock::given(method("PUT"))
            .and(body_json(json!({ "maxBitRate": 192 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let store = Arc::new(FamilyStore::in_memory());
        store.upsert_member(capped("alex", 192));
        let (caps, _) = caps_for(&server, store);
        assert_eq!(caps.sync_all().await, 2);
        let sent = puts(&server.received_requests().await.unwrap_or_default());
        let paths: Vec<&str> = sent.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["/api/player/p-open", "/api/player/p-high"]);
    }

    #[tokio::test]
    async fn no_limit_touches_no_player() {
        let server = navidrome().await;
        players(&server).await;
        let store = Arc::new(FamilyStore::in_memory());
        store.upsert_member(member("alex", String::new()));
        let (caps, _) = caps_for(&server, store);
        assert_eq!(caps.sync_all().await, 0);
        let requests = server.received_requests().await.unwrap_or_default();
        assert!(puts(&requests).is_empty());
        assert!(
            !requests.iter().any(|r| r.url.path() == "/api/player"),
            "no list is read for a member with no limit"
        );
    }

    #[tokio::test]
    async fn a_new_device_is_checked_again() {
        let server = navidrome().await;
        players(&server).await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let store = Arc::new(FamilyStore::in_memory());
        store.upsert_member(capped("alex", 192));
        let (caps, devices) = caps_for(&server, Arc::clone(&store));
        let caps = Arc::new(caps.with_timing(Duration::from_secs(3600), Duration::from_millis(10)));
        let token = CancellationToken::new();
        let running = tokio::spawn(Arc::clone(&caps).run(token.clone()));
        // The first check runs at once.
        for _ in 0..200 {
            if puts(&server.received_requests().await.unwrap_or_default()).len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            puts(&server.received_requests().await.unwrap_or_default()).len(),
            2
        );
        // A device seen for the first time, after its sign-in checked out.
        let mut alex = FamilyCaller::member("alex", Preset::Listener, Abilities::preset(Preset::Listener));
        alex.device_id = Some("x_1".into());
        devices.seen(&alex, &Sighting::default());
        for _ in 0..200 {
            if puts(&server.received_requests().await.unwrap_or_default()).len() >= 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            puts(&server.received_requests().await.unwrap_or_default()).len(),
            4
        );
        token.cancel();
        running.await.expect("joins").expect("ends");
    }
}
