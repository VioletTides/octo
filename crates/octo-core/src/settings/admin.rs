//! `Octo.Models.Settings.AdminSettings` (the `Admin` section).

use serde::{Deserialize, Serialize};

/// Who may use the dashboard and the admin API, and which port they answer on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct AdminSettings {
    /// "required" (the default): the dashboard and /api/admin need a sign-in with a Navidrome
    /// admin account, or the recovery code. "off": anyone who can reach Octo can use them, as
    /// before the sign-in existed. Only for a proxy in front that signs people in itself
    /// (Authelia, Authentik). Any other value counts as "required".
    /// Environment variable: ADMIN__SIGNIN (docker-compose maps ADMIN_SIGN_IN)
    pub sign_in: String,

    /// A second port for the dashboard and the admin API alone (default 0: they share Octo's
    /// port). When set, Octo's main port answers 404 for /admin and /api/admin, and this port
    /// answers 404 for everything else. Restart to apply.
    /// Environment variable: ADMIN__PORT (docker-compose maps ADMIN_PORT)
    pub port: i32,
}

impl Default for AdminSettings {
    fn default() -> Self {
        Self {
            sign_in: "required".to_string(),
            port: 0,
        }
    }
}

impl AdminSettings {
    /// True only for the exact word "off", so a typo never opens the dashboard.
    pub fn sign_in_off(&self) -> bool {
        self.sign_in.trim().eq_ignore_ascii_case("off")
    }

    /// Whether the value is one Octo knows ("required" or "off", any case): the start-up log
    /// warns about any other, which keeps the sign-in on.
    pub fn sign_in_known(&self) -> bool {
        let value = self.sign_in.trim();
        value.eq_ignore_ascii_case("required") || value.eq_ignore_ascii_case("off")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_word_off_turns_the_sign_in_off() {
        let with = |value: &str| AdminSettings {
            sign_in: value.to_string(),
            ..Default::default()
        };
        for (value, off, known) in [
            ("required", false, true),
            ("off", true, true),
            (" OFF ", true, true),
            ("of", false, false),
            ("false", false, false),
            ("", false, false),
        ] {
            assert_eq!(with(value).sign_in_off(), off, "{value:?}");
            assert_eq!(with(value).sign_in_known(), known, "{value:?}");
        }
        assert!(!AdminSettings::default().sign_in_off());
        assert_eq!(AdminSettings::default().port, 0);
    }
}
