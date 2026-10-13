//! The hourly check that each signed-in dashboard user is still a Navidrome admin.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use octo_core::common::Clock;
use octo_core::common::dotnet::{eq_ignore_case, ordinal_ignore_case_key};
use octo_core::settings::SettingsStore;
use parking_lot::Mutex;
use serde_json::Value;
use tracing::warn;

use super::BrowseSessionStore;
use crate::services::subsonic::navidrome_identity_service::NavidromeIdentityService;

/// What Navidrome said of one person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Admin,
    NotAdmin,
    Gone,
    Unknown,
}

/// A dashboard sign-in lasts 90 days, so once an hour per person Octo reads Navidrome's user
/// list, as its own admin identity, and checks that person is still there and still an admin.
/// Gone or no longer an admin ends every session of theirs. Navidrome unreachable, or no admin
/// login that can read the whole list, never locks anyone out: Octo asks again in five minutes.
///
/// Octo's token is whichever admin login it saw last, so it may be the demoted person's own.
/// Then Navidrome shows it only that person, marked not an admin, which is answer enough; for
/// anyone else Octo drops the token and asks again with the admin login in its settings.
///
/// The list, not Subsonic getUser: Navidrome answers getUser only for the caller's own name, and
/// an admin asking about anyone else gets error 50, so getUser can never say someone was demoted.
pub struct AdminRoleCheck {
    identity: NavidromeIdentityService,
    sessions: Arc<BrowseSessionStore>,
    http: reqwest::Client,
    settings: Arc<SettingsStore>,
    /// When each person (case-folded) is next asked about.
    next_ask: Mutex<HashMap<String, DateTime<Utc>>>,
    clock: Mutex<Clock>,
}

impl AdminRoleCheck {
    pub const EVERY: TimeDelta = TimeDelta::hours(1);
    const RETRY_UNKNOWN: TimeDelta = TimeDelta::minutes(5);
    const ASK_FOR: Duration = Duration::from_secs(5);

    pub fn new(
        identity: NavidromeIdentityService,
        sessions: Arc<BrowseSessionStore>,
        http: reqwest::Client,
        settings: Arc<SettingsStore>,
    ) -> Self {
        AdminRoleCheck {
            identity,
            sessions,
            http,
            settings,
            next_ask: Mutex::new(HashMap::new()),
            clock: Mutex::new(Clock::system()),
        }
    }

    /// Reads the time from `clock` from now on; a test sets it on the running instance.
    pub fn set_clock(&self, clock: Clock) {
        *self.clock.lock() = clock;
    }

    /// False only when Navidrome said the person is gone or not an admin; their sessions are
    /// then ended.
    pub async fn still_admin(&self, user: &str) -> bool {
        let now = self.clock.lock().now();
        let key = ordinal_ignore_case_key(user);
        if self.next_ask.lock().get(&key).is_some_and(|next| now < *next) {
            return true;
        }
        let role = self.ask(user).await;
        if matches!(role, Role::NotAdmin | Role::Gone) {
            self.next_ask.lock().remove(&key);
            let ended = self.sessions.revoke_user(user);
            warn!(
                "{user} is {} in Navidrome, so {ended} dashboard sign-ins of theirs were ended.",
                if role == Role::Gone {
                    "gone"
                } else {
                    "no longer an admin"
                }
            );
            return false;
        }
        let wait = if role == Role::Admin {
            Self::EVERY
        } else {
            Self::RETRY_UNKNOWN
        };
        self.next_ask.lock().insert(key, now + wait);
        true
    }

    async fn ask(&self, user: &str) -> Role {
        let url = self.settings.current().subsonic.url.clone().unwrap_or_default();
        let url = url.trim_end_matches('/').to_string();
        if url.is_empty() {
            return Role::Unknown;
        }
        // The whole exchange is bounded.
        tokio::time::timeout(Self::ASK_FOR, self.ask_list(&url, user))
            .await
            .unwrap_or(Role::Unknown)
    }

    async fn ask_list(&self, url: &str, user: &str) -> Role {
        // Twice at most: Octo's token is whichever admin login it saw last, so it can expire
        // (a 401) or belong to someone Navidrome has since demoted (a non-admin's view). Either
        // way it is dropped, and the second try logs in with the admin login in the settings.
        for _ in 0..2 {
            let Some(jwt) = self.identity.ensure_admin_jwt().await.filter(|j| !j.is_empty()) else {
                return Role::Unknown;
            };
            let Ok(response) = self
                .http
                .get(format!("{url}/api/user"))
                .header("X-Nd-Authorization", format!("Bearer {jwt}"))
                .send()
                .await
            else {
                return Role::Unknown;
            };
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.identity.invalidate_admin_jwt(&jwt);
                continue;
            }
            // Navidrome refuses the list to a non-admin with 403 "Permission denied" (0.64.2,
            // seen on a real starter stack), so this token is no longer an admin's. When it came
            // from this person's own login, that is the answer.
            if status == reqwest::StatusCode::FORBIDDEN {
                self.identity.invalidate_admin_jwt(&jwt);
                if self
                    .identity
                    .username_for_native_token(Some(&jwt))
                    .is_some_and(|owner| eq_ignore_case(&owner, user))
                {
                    return Role::NotAdmin;
                }
                continue;
            }
            if !status.is_success() {
                return Role::Unknown;
            }
            let Ok(body) = response.bytes().await else {
                return Role::Unknown;
            };
            let Ok(Value::Array(users)) = serde_json::from_slice::<Value>(&body) else {
                return Role::Unknown;
            };
            let found = users.iter().find(|entry| {
                entry
                    .get("userName")
                    .and_then(Value::as_str)
                    .is_some_and(|name| eq_ignore_case(name, user))
            });

            if users.iter().any(is_admin) {
                return match found {
                    None => Role::Gone,
                    Some(entry) if is_admin(entry) => Role::Admin,
                    Some(_) => Role::NotAdmin,
                };
            }

            // A non-admin's view: Navidrome shows a non-admin only themselves, so the token is
            // no longer an admin's. If it is this person's own, it already says they are not an
            // admin. Otherwise it cannot say anything about anyone else.
            self.identity.invalidate_admin_jwt(&jwt);
            if found.is_some_and(|entry| !is_admin(entry)) {
                return Role::NotAdmin;
            }
        }
        Role::Unknown
    }
}

fn is_admin(entry: &Value) -> bool {
    entry.get("isAdmin") == Some(&Value::Bool(true))
}
