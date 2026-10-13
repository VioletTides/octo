//! Which server answers Octo's Subsonic calls, and whether it is new enough for the library tidy
//! to move files.
//!
//! The tidy rewrites tags, waits for a scan, then moves files, so a song never changes its tags
//! and its path in one scan. Navidrome from 0.55 matches a moved file to the song it was by its
//! tags and keeps that song's stars, plays and playlist entries. Older Navidrome, or another
//! Subsonic server, cannot, so Apply is refused there. Preview always works.

use serde_json::Value;

/// The oldest Navidrome an Apply runs against.
pub const MINIMUM_NAVIDROME: (u32, u32, u32) = (0, 55, 0);

/// Who answered a ping: the envelope's `type` and `serverVersion`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerInfo {
    pub kind: Option<String>,
    pub server_version: Option<String>,
}

impl ServerInfo {
    pub fn new(kind: Option<&str>, server_version: Option<&str>) -> Self {
        Self {
            kind: kind.map(str::to_string),
            server_version: server_version.map(str::to_string),
        }
    }

    /// The `subsonic-response` envelope of a ping, which Navidrome sends with its type and
    /// version even when the sign-in failed. None when the answer is not an envelope.
    pub fn read(doc: &Value) -> Option<Self> {
        let envelope = doc.get("subsonic-response")?.as_object()?;
        let text = |name: &str| envelope.get(name).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            kind: text("type"),
            server_version: text("serverVersion"),
        })
    }

    pub fn is_navidrome(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("navidrome"))
    }

    /// The leading numbers of the version: (0, 58, 0) for "0.58.0 (fc9a1b2e)", (0, 55, 0) for
    /// "0.55", (0, 60, 1) for "v0.60.1-SNAPSHOT". None when it does not start with a number.
    pub fn version(&self) -> Option<(u32, u32, u32)> {
        let text = self.server_version.as_deref()?.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        // "0.58.0 (fc9a1b2e)": the dotted numbers up to the first other character.
        let dotted = text
            .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
            .next()
            .unwrap_or("");
        let mut parts = dotted.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
        Some((major, minor, patch))
    }
}

/// Whether Apply may run, the version to show, and why not when it may not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TidyGate {
    pub apply_allowed: bool,
    pub version: Option<String>,
    pub reason: Option<String>,
}

impl TidyGate {
    fn refused(version: Option<String>, reason: String) -> Self {
        Self {
            apply_allowed: false,
            version,
            reason: Some(reason),
        }
    }
}

/// The gate for the server that answered, or for none at all.
pub fn gate(info: Option<&ServerInfo>) -> TidyGate {
    let minimum = format!("{}.{}", MINIMUM_NAVIDROME.0, MINIMUM_NAVIDROME.1);
    let Some(info) = info else {
        return TidyGate::refused(
            None,
            "Octo could not reach Navidrome to check its version, so Apply is off. Preview still works."
                .to_string(),
        );
    };
    let shown = info.server_version.clone();
    if !info.is_navidrome() {
        return TidyGate::refused(
            shown,
            format!(
                "Apply needs Navidrome {minimum} or newer, and this server says it is \"{}\".",
                info.kind.as_deref().unwrap_or("unknown")
            ),
        );
    }
    let Some(version) = info.version() else {
        return TidyGate::refused(
            shown,
            "Navidrome did not say which version it is, so Apply is off. Preview still works.".to_string(),
        );
    };
    if version < MINIMUM_NAVIDROME {
        return TidyGate::refused(
            shown.clone(),
            format!(
                "Navidrome {} would lose stars, plays and playlist entries for moved songs. Apply needs {minimum} or newer. Preview still works.",
                shown.as_deref().unwrap_or("")
            ),
        );
    }
    TidyGate {
        apply_allowed: true,
        version: shown,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_version_reads_the_leading_numbers() {
        for (raw, expected) in [
            (Some("0.58.0 (fc9a1b2e)"), Some((0, 58, 0))),
            (Some("0.55"), Some((0, 55, 0))),
            (Some("v0.60.1-SNAPSHOT"), Some((0, 60, 1))),
            (Some("0.64.2"), Some((0, 64, 2))),
            (Some("dev"), None),
            (Some(""), None),
            (None, None),
        ] {
            assert_eq!(
                ServerInfo::new(Some("navidrome"), raw).version(),
                expected,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn gate_only_navidrome_055_and_up() {
        for (kind, version, allowed) in [
            (None, None, false),
            (Some("gonic"), Some("0.16.4"), false),
            (Some("navidrome"), Some("0.54.5 (abc)"), false),
            (Some("navidrome"), Some("0.55.0 (abc)"), true),
            (Some("navidrome"), Some("0.58.0"), true),
            (Some("Navidrome"), Some("0.64.2"), true),
            (Some("navidrome"), Some("dev"), false),
        ] {
            let info = kind.map(|kind| ServerInfo::new(Some(kind), version));
            let gate = gate(info.as_ref());
            assert_eq!(gate.apply_allowed, allowed, "{kind:?} {version:?}");
            assert_eq!(gate.reason.is_none(), allowed, "{kind:?} {version:?}");
        }
    }

    #[test]
    fn gate_without_a_server_says_preview_still_works() {
        let gate = gate(None);
        assert!(
            gate.reason
                .as_deref()
                .is_some_and(|why| why.contains("Preview still works"))
        );
    }

    #[test]
    fn a_ping_envelope_names_its_server_even_when_the_sign_in_failed() {
        let doc: Value = serde_json::from_str(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1","type":"navidrome","serverVersion":"0.64.2 (abc)","error":{"code":40}}}"#,
        )
        .expect("json");
        let info = ServerInfo::read(&doc).expect("an envelope");
        assert!(info.is_navidrome());
        assert_eq!(info.version(), Some((0, 64, 2)));
        assert_eq!(ServerInfo::read(&serde_json::json!({"nope": 1})), None);
    }
}
