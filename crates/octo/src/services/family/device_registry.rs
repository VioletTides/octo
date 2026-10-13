//! What Octo knows of the devices people listen on: when each was last seen, from where, with
//! which app and under which name. Nobody sets a device up: every device is told apart by what
//! its app sends ([`detected_device_id`]) and recorded the first time it signs in. A member's
//! device is recorded only after its sign-in checked out. Everything here changes memory only;
//! the store's flush worker writes it every 30 seconds.
//!
//! The first time a device is seen, a [`NewDevice`] goes out to whoever listens
//! ([`DeviceRegistry::subscribe`]), for the "new device" notice.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use octo_core::family::abilities::Role;
use octo_core::family::device::{NetworkPlace, app_label, user_agent_family};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use super::caller::FamilyCaller;
use super::family_store::FamilyStore;

/// A device seen for the first time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDevice {
    pub username: String,
    pub device_id: String,
    /// The app, as the Family page names it (`Octo 1.6 (Android)`).
    pub app: String,
    /// The name the device gave, if any.
    pub name: String,
    pub place: NetworkPlace,
    /// A member's device (true), or a device of the owner (false).
    pub managed: bool,
}

/// What one request says about the device it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sighting {
    /// The Subsonic `c` parameter.
    pub client: String,
    pub user_agent: String,
    /// `X-Octo-Device-Id`.
    pub octo_device_id: Option<String>,
    /// `X-Octo-Device-Name`, decoded.
    pub name: Option<String>,
    /// `X-Nd-Client-Unique-Id`.
    pub navidrome_client_id: Option<String>,
}

/// The id of a device: a SHA-256 of the user and the Octo app's install id when it sends one,
/// else of the user, the client name, the User-Agent's product (without its version, so an
/// update is the same device) and Navidrome's client id.
pub fn detected_device_id(user: &str, sighting: &Sighting) -> String {
    let user = user.trim().to_lowercase();
    let text = match sighting
        .octo_device_id
        .as_deref()
        .map(str::trim)
        .filter(|i| !i.is_empty())
    {
        Some(install) => format!("{user}\nocto\n{install}"),
        None => format!(
            "{user}\n{}\n{}\n{}",
            sighting.client.trim().to_lowercase(),
            user_agent_family(&sighting.user_agent).to_lowercase(),
            sighting.navidrome_client_id.as_deref().unwrap_or("").trim()
        ),
    };
    format!("x_{}", &hex::encode(Sha256::digest(text.as_bytes()))[..20])
}

/// New-device notices one person may cause in [`NOTICE_WINDOW`]; past that the devices are
/// still kept, but no more notices go out until the window moves on.
pub const NOTICES_PER_WINDOW: usize = 5;
pub const NOTICE_WINDOW: Duration = Duration::from_secs(60 * 60);

/// How long a device noted after a checked sign-in counts as noted lately: within it, another
/// unchecked sign-in from it is not checked again only to note it.
pub const NOTED_LATELY: Duration = Duration::from_secs(60);
/// Devices remembered as noted lately, at most.
const NOTED_KEPT: usize = 10_000;

/// The device registry over `family.json`.
pub struct DeviceRegistry {
    store: Arc<FamilyStore>,
    events: broadcast::Sender<NewDevice>,
    /// Person to when their recent new-device notices went out.
    notices: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Device to when it was last noted.
    noted: Mutex<HashMap<String, Instant>>,
}

impl DeviceRegistry {
    pub fn new(store: Arc<FamilyStore>) -> Self {
        let (events, _) = broadcast::channel(64);
        DeviceRegistry {
            store,
            events,
            notices: Mutex::new(HashMap::new()),
            noted: Mutex::new(HashMap::new()),
        }
    }

    /// Whether this device was noted less than [`NOTED_LATELY`] ago.
    pub fn noted_lately(&self, device_id: &str) -> bool {
        self.noted
            .lock()
            .get(device_id)
            .is_some_and(|at| at.elapsed() < NOTED_LATELY)
    }

    fn mark_noted(&self, device_id: &str) {
        let now = Instant::now();
        let mut noted = self.noted.lock();
        if noted.len() >= NOTED_KEPT && !noted.contains_key(device_id) {
            noted.retain(|_, at| now.duration_since(*at) < NOTED_LATELY);
            crate::services::admin::sign_in_throttle::trim_oldest(&mut noted, NOTED_KEPT - 1, |at| Some(*at));
        }
        noted.insert(device_id.to_string(), now);
    }

    /// Whether another new-device notice about `user` may go out now, counting it if so.
    fn may_notify(&self, user: &str) -> bool {
        let now = Instant::now();
        let mut notices = self.notices.lock();
        notices.retain(|_, sent| {
            while sent
                .front()
                .is_some_and(|at| now.duration_since(*at) >= NOTICE_WINDOW)
            {
                sent.pop_front();
            }
            !sent.is_empty()
        });
        let sent = notices.entry(user.trim().to_lowercase()).or_default();
        if sent.len() >= NOTICES_PER_WINDOW {
            return false;
        }
        sent.push_back(now);
        true
    }

    /// Every device seen for the first time from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<NewDevice> {
        self.events.subscribe()
    }

    /// The caller's device was used now, after a checked sign-in: recorded the first time,
    /// else its last seen time, app, place and name are kept. The first sighting of a member's
    /// device, or of a new device of the owner, is announced.
    pub fn seen(&self, caller: &FamilyCaller, sighting: &Sighting) {
        let Some(device_id) = caller.device_id.as_deref() else {
            return;
        };
        self.mark_noted(device_id);
        // A request that names no app leaves the app Octo last saw as it was.
        let app = if sighting.client.trim().is_empty() && sighting.user_agent.trim().is_empty() {
            String::new()
        } else {
            app_label(&sighting.client, &sighting.user_agent)
        };
        let now = Utc::now();
        let first = if caller.managed {
            self.store.record_device(
                device_id,
                &caller.user,
                &app,
                caller.place,
                sighting.name.as_deref(),
                now,
            )
        } else {
            let new = self.store.note_detected(
                device_id,
                &caller.user,
                &app,
                caller.place,
                sighting.name.as_deref(),
                now,
            );
            new && caller.role == Role::Owner
        };
        if first && self.may_notify(&caller.user) {
            // Nobody listening is fine: the notice is simply not sent.
            let _ = self.events.send(NewDevice {
                username: caller.user.clone(),
                device_id: device_id.to_string(),
                app,
                name: sighting.name.clone().unwrap_or_default(),
                place: caller.place,
                managed: caller.managed,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::family::member_credentials::tests::member;
    use octo_core::family::abilities::{Abilities, Preset};

    fn sighting(device: Option<&str>, client: &str, ua: &str) -> Sighting {
        Sighting {
            client: client.into(),
            user_agent: ua.into(),
            octo_device_id: device.map(str::to_string),
            name: None,
            navidrome_client_id: None,
        }
    }

    #[test]
    fn owner_devices_are_detected_by_device_header() {
        let phone = detected_device_id("winters", &sighting(Some("uuid-1"), "Octo", "Octo/1.6 (Android)"));
        // The same install after an update, and whatever it says as c.
        assert_eq!(
            phone,
            detected_device_id("Winters", &sighting(Some("uuid-1"), "octo", "Octo/1.7 (Android)"))
        );
        assert_ne!(
            phone,
            detected_device_id("winters", &sighting(Some("uuid-2"), "Octo", "Octo/1.6 (Android)"))
        );
        // Without the header: client, app and Navidrome's client id tell devices apart, and a
        // version change does not.
        let symfonium = detected_device_id("winters", &sighting(None, "Symfonium", "Symfonium/12.3"));
        assert_eq!(
            symfonium,
            detected_device_id("winters", &sighting(None, "Symfonium", "Symfonium/12.4"))
        );
        assert_ne!(
            symfonium,
            detected_device_id("winters", &sighting(None, "Feishin", "Feishin/0.12"))
        );
        assert!(phone.starts_with("x_") && phone.len() == 22, "{phone}");
    }

    #[test]
    fn new_device_notifies_once() {
        let store = Arc::new(FamilyStore::in_memory());
        store.set_owner("winters");
        store.upsert_member(member("alex", String::new()));
        let registry = DeviceRegistry::new(store.clone());
        let mut events = registry.subscribe();

        let mut seen = sighting(Some("install-1"), "Octo", "Octo/1.6 (Android)");
        seen.name = Some("Pixel 9".into());
        let id = detected_device_id("alex", &seen);
        let mut alex = FamilyCaller::member("alex", Preset::Listener, Abilities::preset(Preset::Listener));
        alex.device_id = Some(id.clone());
        assert!(store.device(&id).is_none(), "nothing set up beforehand");
        registry.seen(&alex, &seen);
        registry.seen(&alex, &seen);
        let event = events.try_recv().expect("one notice");
        assert_eq!(event.device_id, id);
        assert_eq!(event.app, "Octo 1.6 (Android)");
        assert_eq!(event.name, "Pixel 9");
        assert!(event.managed);
        assert!(events.try_recv().is_err(), "only the first sighting");
        let recorded = store.device(&id).expect("recorded");
        assert_eq!(recorded.name, "Pixel 9");
        assert_eq!(recorded.username, "alex");

        // The owner's new device is announced once; someone outside the family's is not.
        let mut owner = FamilyCaller::unmanaged("winters", true);
        owner.device_id = Some(detected_device_id("winters", &seen));
        registry.seen(&owner, &seen);
        registry.seen(&owner, &seen);
        assert!(!events.try_recv().expect("the owner's notice").managed);
        assert!(events.try_recv().is_err());
        let mut bob = FamilyCaller::unmanaged("bob", false);
        bob.device_id = Some(detected_device_id("bob", &seen));
        registry.seen(&bob, &seen);
        assert!(events.try_recv().is_err());
        assert_eq!(store.detected().len(), 2);
    }

    #[test]
    fn new_device_notices_are_limited_per_person() {
        let store = Arc::new(FamilyStore::in_memory());
        store.set_owner("winters");
        let registry = DeviceRegistry::new(store.clone());
        let mut events = registry.subscribe();
        for n in 0..(NOTICES_PER_WINDOW * 3) {
            let seen = sighting(Some(&format!("install-{n}")), "Octo", "Octo/1.6 (Android)");
            let mut owner = FamilyCaller::unmanaged("winters", true);
            owner.device_id = Some(detected_device_id("winters", &seen));
            registry.seen(&owner, &seen);
        }
        let mut notices = 0;
        while events.try_recv().is_ok() {
            notices += 1;
        }
        assert_eq!(notices, NOTICES_PER_WINDOW);
        assert_eq!(
            store.detected().len(),
            NOTICES_PER_WINDOW * 3,
            "the devices are still kept"
        );
    }
}
