//! Signing in as a member for work done later. A member's Navidrome password is hidden and
//! can change at any time (a repair sets a new one), so a sign-in kept from a member's request
//! may stop working while it waits. Anything that keeps a sign-in for later asks here at the
//! moment it uses it, and gets one made from the member's current password.

use std::sync::Arc;

use indexmap::IndexMap;
use octo_core::common::dotnet::eq_ignore_case;
use octo_core::family::codes::subsonic_token;
use octo_core::settings::SettingsStore;
use octo_subsonic::SubsonicCredential;
use sha2::{Digest, Sha256};

use super::family_store::FamilyStore;
use super::vault::FamilyVault;

/// Every key a Subsonic sign-in can use.
const SIGN_IN_KEYS: [&str; 6] = ["u", "p", "t", "s", "apiKey", "jwt"];

/// Fresh sign-ins for members, from their current hidden password.
pub struct MemberCredentials {
    store: Arc<FamilyStore>,
    vault: Arc<FamilyVault>,
    settings: Arc<SettingsStore>,
}

impl MemberCredentials {
    pub fn new(store: Arc<FamilyStore>, vault: Arc<FamilyVault>, settings: Arc<SettingsStore>) -> Self {
        MemberCredentials {
            store,
            vault,
            settings,
        }
    }

    /// The member's username as Octo keeps it and their current hidden password, when Family
    /// is on and `user` is a member whose password opens.
    fn hidden(&self, user: &str) -> Option<(String, String)> {
        if !self.settings.current().family.enabled {
            return None;
        }
        let member = self.store.member(user.trim())?;
        let hidden = self.vault.open_sealed(&member.navidrome_password)?;
        Some((member.username, hidden))
    }

    /// Whether `user` is a member Octo signs in for (Family on).
    pub fn is_member(&self, user: &str) -> bool {
        self.settings.current().family.enabled && self.store.member(user.trim()).is_some()
    }

    /// A sign-in for `user` made now from their current hidden password: `u`, `t` and `s`
    /// with a new salt. None when Family is off, `user` is not a member, or their password
    /// cannot be opened.
    pub fn fresh(&self, user: &str) -> Option<SubsonicCredential> {
        self.fresh_as(user, "1.16.1", "octo")
    }

    /// [`MemberCredentials::fresh`] keeping the version and client name of a sign-in that
    /// was held.
    pub fn fresh_as(&self, user: &str, version: &str, client: &str) -> Option<SubsonicCredential> {
        let (name, hidden) = self.hidden(user)?;
        let salt = hex::encode(rand::random::<[u8; 6]>());
        let values = [
            ("u".to_string(), name),
            ("t".to_string(), subsonic_token(&hidden, &salt)),
            ("s".to_string(), salt),
            ("v".to_string(), version.to_string()),
            ("c".to_string(), client.to_string()),
        ];
        SubsonicCredential::from(values.iter().map(|(k, v)| (k, v)))
    }

    /// The sign-in to use in place of `held`: a fresh one when it signs in as a member, else
    /// None (use `held` as it is).
    pub fn refresh(&self, held: &SubsonicCredential) -> Option<SubsonicCredential> {
        self.fresh_as(held.user()?, held.version(), held.client())
    }

    /// Replaces the sign-in in `parameters` (any of `u`, `p`, `t`, `s`, `apiKey`, `jwt`,
    /// matched ignoring case) with a fresh one when `u` names a member. Answers whether it did.
    pub fn refresh_parameters(&self, parameters: &mut IndexMap<String, String>) -> bool {
        let Some(user) = parameters
            .iter()
            .find(|(k, _)| eq_ignore_case(k, "u"))
            .map(|(_, v)| v.clone())
        else {
            return false;
        };
        let Some((name, hidden)) = self.hidden(&user) else {
            return false;
        };
        parameters.retain(|k, _| !SIGN_IN_KEYS.iter().any(|key| eq_ignore_case(k, key)));
        let salt = hex::encode(rand::random::<[u8; 6]>());
        parameters.insert("u".into(), name);
        parameters.insert("t".into(), subsonic_token(&hidden, &salt));
        parameters.insert("s".into(), salt);
        true
    }

    /// [`MemberCredentials::stable_key`] when `credential` is a member's `u`, `t` and `s` made
    /// from their current hidden password; None for anything else (a device secret as sent by
    /// an app, an old password, someone outside the family).
    pub fn key_if_current(&self, credential: &SubsonicCredential) -> Option<String> {
        let user = credential.user()?;
        let (_, hidden) = self.hidden(user)?;
        let parameters = credential.parameters(&[]);
        if parameters.contains_key("p") || parameters.contains_key("apiKey") {
            return None;
        }
        let token = parameters.get("t")?.to_ascii_lowercase();
        let expected = subsonic_token(&hidden, parameters.get("s")?);
        octo_core::family::codes::constant_time_eq(token.as_bytes(), expected.as_bytes())
            .then(|| self.stable_key(user))
            .flatten()
    }

    /// A key that names a member's current password without holding it (a SHA-256 of the two),
    /// for a cache that files answers by sign-in: it stays the same across requests and
    /// changes when the password does. None when `user` is not a member.
    pub fn stable_key(&self, user: &str) -> Option<String> {
        let (name, hidden) = self.hidden(user)?;
        let digest = Sha256::digest(format!("family\n{}\n{hidden}", name.to_lowercase()).as_bytes());
        Some(hex::encode_upper(digest))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::services::family::family_store::{Member, SharedLibraries};
    use chrono::Utc;
    use octo_core::family::abilities::Preset;
    use octo_core::settings::{AppSettings, FamilySettings};

    pub(crate) fn member(name: &str, sealed_password: String) -> Member {
        Member {
            username: name.to_string(),
            display_name: String::new(),
            preset: Preset::Listener,
            edits: Default::default(),
            navidrome_user_id: format!("u-{name}"),
            navidrome_password: sealed_password,
            personal_library_id: None,
            shared_libraries: SharedLibraries::All,
            account_password: String::new(),
            created: Utc::now(),
            suspended: false,
            ntfy_topic: String::new(),
        }
    }

    fn credentials(enabled: bool) -> (MemberCredentials, Arc<FamilyStore>, Arc<FamilyVault>) {
        let store = Arc::new(FamilyStore::in_memory());
        let vault = Arc::new(FamilyVault::in_memory());
        let settings = Arc::new(SettingsStore::for_tests(AppSettings {
            family: FamilySettings {
                enabled,
                ..Default::default()
            },
            ..Default::default()
        }));
        (
            MemberCredentials::new(store.clone(), vault.clone(), settings),
            store,
            vault,
        )
    }

    #[test]
    fn fresh_sign_ins_follow_the_current_password() {
        let (members, store, vault) = credentials(true);
        store.upsert_member(member("alex", vault.seal("hidden-1").expect("sealed")));
        let first = members.fresh("ALEX").expect("a member");
        let p = first.parameters(&[]);
        assert_eq!(p["u"], "alex");
        assert_eq!(p["t"], subsonic_token("hidden-1", &p["s"]));
        store.upsert_member(member("alex", vault.seal("hidden-2").expect("sealed")));
        let held = first.clone();
        let refreshed = members.refresh(&held).expect("a member");
        let p = refreshed.parameters(&[]);
        assert_eq!(p["t"], subsonic_token("hidden-2", &p["s"]));
        assert!(members.fresh("winters").is_none());
        assert_ne!(members.stable_key("alex"), None);
        // Only a sign-in made from the current password files under the member's key.
        assert_eq!(members.key_if_current(&refreshed), members.stable_key("alex"));
        assert_eq!(members.key_if_current(&first), None);
        let device_secret = SubsonicCredential::from(
            [
                ("u".to_string(), "alex".to_string()),
                ("p".to_string(), "hidden-2".to_string()),
            ]
            .iter()
            .map(|(k, v)| (k, v)),
        )
        .expect("a sign-in");
        assert_eq!(members.key_if_current(&device_secret), None);
    }

    #[test]
    fn parameters_lose_every_other_sign_in() {
        let (members, store, vault) = credentials(true);
        store.upsert_member(member("alex", vault.seal("hidden").expect("sealed")));
        let mut parameters: IndexMap<String, String> =
            [("U", "alex"), ("p", "enc:6162"), ("apiKey", "k"), ("id", "7")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        assert!(members.refresh_parameters(&mut parameters));
        assert_eq!(parameters["id"], "7");
        assert_eq!(parameters["u"], "alex");
        assert!(!parameters.contains_key("p") && !parameters.contains_key("apiKey"));
        assert_eq!(parameters["t"], subsonic_token("hidden", &parameters["s"]));
        let mut owner: IndexMap<String, String> = [("u".to_string(), "winters".to_string())].into();
        assert!(!members.refresh_parameters(&mut owner));
    }

    #[test]
    fn nothing_changes_with_family_off() {
        let (members, store, vault) = credentials(false);
        store.upsert_member(member("alex", vault.seal("hidden").expect("sealed")));
        assert!(members.fresh("alex").is_none());
        assert!(!members.is_member("alex"));
    }
}
