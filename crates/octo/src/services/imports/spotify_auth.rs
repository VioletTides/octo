//! The Spotify sign-in.

use std::net::IpAddr;

use base64::Engine;
use chrono::{DateTime, TimeDelta, Utc};
use octo_core::common::Clock;
use octo_core::common::dotnet::{escape_data_string, unescape_data_string};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use url::Url;

use crate::services::framework::DotnetDictionary;

/// A sign-in Spotify has not sent back yet: whose it is and the PKCE secret that finishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSpotifySignIn {
    pub user: String,
    pub verifier: String,
    pub redirect_uri: String,
    pub expires_utc: DateTime<Utc>,
}

/// Spotify's authorization code flow with PKCE, the one Spotify asks apps without a safe place for
/// a client secret to use. Octo makes the code verifier, keeps it here and never sends it to the
/// browser, so a code that leaks on its way back (a pasted address, a loopback page) is worth
/// nothing to anyone else.
///
/// Spotify takes only HTTPS redirect URIs, or a loopback address written as an IP (127.0.0.1 or
/// [::1], never "localhost"), and only the ones registered on the app. A loopback one registered
/// without a port takes any port, which is how the Octo apps sign in on a port they pick.
pub struct SpotifyAuth {
    pending: Mutex<DotnetDictionary<PendingSpotifySignIn>>,
    clock: Mutex<Clock>,
}

impl Default for SpotifyAuth {
    fn default() -> Self {
        SpotifyAuth::new(Clock::system())
    }
}

impl SpotifyAuth {
    pub const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";

    /// Liked songs, and every playlist the account owns, follows or works on.
    pub const SCOPES: &str = "user-library-read playlist-read-private playlist-read-collaborative";

    pub const PENDING_FOR: TimeDelta = TimeDelta::minutes(15);

    pub fn new(clock: Clock) -> Self {
        SpotifyAuth {
            pending: Mutex::new(DotnetDictionary::new()),
            clock: Mutex::new(clock),
        }
    }

    /// The `Clock` seam the tests set.
    pub fn set_clock(&self, clock: Clock) {
        *self.clock.lock() = clock;
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.lock().now()
    }

    /// The address to send someone to, and the state that brings their answer back to them.
    pub fn begin(&self, user: &str, client_id: &str, redirect_uri: &str) -> (String, String) {
        self.prune();
        let verifier = Self::verifier();
        let state = base64_url(&rand::random::<[u8; 18]>());
        self.pending.lock().set(
            state.clone(),
            PendingSpotifySignIn {
                user: user.to_string(),
                verifier: verifier.clone(),
                redirect_uri: redirect_uri.to_string(),
                expires_utc: self.now() + Self::PENDING_FOR,
            },
        );
        let url = format!(
            "{}?response_type=code&client_id={}&scope={}&redirect_uri={}&state={}&code_challenge_method=S256&code_challenge={}",
            Self::AUTHORIZE_URL,
            escape_data_string(client_id),
            escape_data_string(Self::SCOPES),
            escape_data_string(redirect_uri),
            escape_data_string(&state),
            Self::challenge(&verifier)
        );
        (url, state)
    }

    /// The sign-in this state belongs to, once: a state answers one return only.
    pub fn take(&self, state: Option<&str>) -> Option<PendingSpotifySignIn> {
        self.prune();
        let state = state?;
        let pending = self.pending.lock().remove(state)?;
        (pending.expires_utc > self.now()).then_some(pending)
    }

    fn prune(&self) {
        let now = self.now();
        let mut pending = self.pending.lock();
        let old: Vec<String> = pending
            .iter()
            .filter(|(_, p)| p.expires_utc <= now)
            .map(|(state, _)| state.clone())
            .collect();
        for state in old {
            pending.remove(&state);
        }
    }

    /// A PKCE code verifier: 64 characters from the unreserved set.
    pub fn verifier() -> String {
        let mut bytes = [0u8; 48];
        bytes[..32].copy_from_slice(&rand::random::<[u8; 32]>());
        bytes[32..].copy_from_slice(&rand::random::<[u8; 16]>());
        base64_url(&bytes)
    }

    /// S256: the verifier's SHA-256, base64url without padding.
    pub fn challenge(verifier: &str) -> String {
        base64_url(&Sha256::digest(verifier.as_bytes()))
    }

    /// Why Spotify would refuse this redirect URI, in words, or None when it is one Spotify takes.
    pub fn redirect_problem(redirect_uri: Option<&str>) -> Option<&'static str> {
        let Some(uri) = absolute(redirect_uri.map(str::trim)) else {
            return Some("The redirect URI is not a full address.");
        };
        if uri
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case("localhost"))
        {
            return Some("Spotify does not take localhost. Use 127.0.0.1 instead.");
        }
        if uri.scheme() == "https" {
            return None;
        }
        if uri.scheme() == "http" && is_loopback(&uri) {
            return None;
        }
        Some("Spotify takes only an https address, or http on 127.0.0.1 or [::1].")
    }

    /// The redirect URI an app may use: the registered one exactly, or, when the registered one is
    /// a loopback address with no port, the same address on any port. None when it is neither,
    /// since Spotify would refuse the sign-in anyway.
    pub fn allowed_redirect(registered: &str, asked: Option<&str>) -> Option<String> {
        let Some(asked) = asked.filter(|a| !a.trim().is_empty()) else {
            return Some(registered.to_string());
        };
        let mine = absolute(Some(registered))?;
        let theirs = absolute(Some(asked.trim()))?;
        if mine.as_str() == theirs.as_str() {
            return Some(asked.trim().to_string());
        }
        let portless = is_loopback(&mine) && mine.port().is_none() && !has_explicit_port(registered);
        if !portless || !is_loopback(&theirs) {
            return None;
        }
        let same_but_port = mine.scheme() == theirs.scheme()
            && mine
                .host_str()
                .zip(theirs.host_str())
                .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            && mine.path() == theirs.path()
            && theirs.query().is_none_or(str::is_empty);
        same_but_port.then(|| asked.trim().to_string())
    }

    /// The code, state and any error from the address Spotify sent the browser to, whether someone
    /// pasted all of it, only its query, or the code by itself.
    pub fn parse_return(pasted: Option<&str>) -> (Option<String>, Option<String>, Option<String>) {
        let text = pasted.map(str::trim).unwrap_or("");
        if text.is_empty() {
            return (None, None, None);
        }
        let mut query = text;
        if let Some(mark) = text.find('?') {
            query = &text[mark + 1..];
        } else if !text.contains('=') {
            return (Some(text.to_string()), None, None);
        }
        if let Some(hash) = query.find('#') {
            query = &query[..hash];
        }
        let (mut code, mut state, mut error) = (None, None, None);
        for part in query.split('&').filter(|p| !p.is_empty()) {
            let (name, value) = match part.find('=') {
                Some(eq) => (
                    &part[..eq],
                    unescape_data_string(&part[eq + 1..].replace('+', " ")),
                ),
                None => (part, String::new()),
            };
            match name {
                "code" => code = Some(value),
                "state" => state = Some(value),
                "error" => error = Some(value),
                _ => {}
            }
        }
        (code, state, error)
    }
}

/// `Uri.TryCreate(text, UriKind.Absolute)`.
pub(crate) fn absolute(text: Option<&str>) -> Option<Url> {
    let text = text?;
    let url = Url::parse(text).ok()?;
    // A scheme alone ("callback:") is not an address with a host for these purposes.
    url.has_host().then_some(url)
}

/// `IPAddress.TryParse(uri.Host.Trim('[', ']'))` and `IPAddress.IsLoopback`.
pub fn is_loopback(uri: &Url) -> bool {
    uri.host_str()
        .map(|h| h.trim_matches(|c| c == '[' || c == ']'))
        .and_then(|h| h.parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback())
}

fn has_explicit_port(uri: &str) -> bool {
    let authority = match uri.find("://") {
        Some(at) => &uri[at + 3..],
        None => uri,
    };
    let authority = match authority.find(['/', '?', '#']) {
        Some(end) => &authority[..end],
        None => authority,
    };
    // [::1]:8080 has its port after the bracket; 127.0.0.1:8080 after the only colon.
    let after = authority.rfind(']').map_or(0, |close| close + 1);
    authority[after..].contains(':')
}

fn base64_url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_of(url: &str) -> Vec<(String, String)> {
        Url::parse(url)
            .expect("a url")
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn get(query: &[(String, String)], name: &str) -> Option<String> {
        query.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn the_challenge_is_the_verifiers_sha256_as_rfc7636_says() {
        // RFC 7636, appendix B.
        assert_eq!(
            SpotifyAuth::challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let verifier = SpotifyAuth::verifier();
        assert_eq!(verifier.len(), 64);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }

    #[test]
    fn a_sign_in_asks_for_read_only_scopes_and_its_state_answers_once() {
        let auth = SpotifyAuth::default();
        let (url, state) = auth.begin("alice", "client-1", "http://127.0.0.1/callback");
        let query = query_of(&url);
        assert_eq!(get(&query, "response_type").as_deref(), Some("code"));
        assert_eq!(get(&query, "client_id").as_deref(), Some("client-1"));
        assert_eq!(get(&query, "code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(
            get(&query, "redirect_uri").as_deref(),
            Some("http://127.0.0.1/callback")
        );
        assert_eq!(get(&query, "scope").as_deref(), Some(SpotifyAuth::SCOPES));
        assert_eq!(get(&query, "state"), Some(state.clone()));

        let pending = auth.take(Some(&state)).expect("pending");
        assert_eq!(pending.user, "alice");
        assert_eq!(
            Some(SpotifyAuth::challenge(&pending.verifier)),
            get(&query, "code_challenge")
        );
        assert!(auth.take(Some(&state)).is_none());
    }

    #[test]
    fn an_old_sign_in_is_forgotten() {
        let now = Arc::new(Mutex::new(
            DateTime::parse_from_rfc3339("2026-10-04T12:00:00Z")
                .expect("a date")
                .with_timezone(&Utc),
        ));
        let clock = {
            let now = now.clone();
            Clock::new(move || *now.lock())
        };
        let auth = SpotifyAuth::new(clock);
        let (_, state) = auth.begin("alice", "c", "http://127.0.0.1/callback");
        *now.lock() += SpotifyAuth::PENDING_FOR + TimeDelta::seconds(1);
        assert!(auth.take(Some(&state)).is_none());
    }

    use std::sync::Arc;

    #[test]
    fn only_redirects_spotify_takes_are_offered() {
        let cases: [(&str, Option<&str>); 7] = [
            ("http://127.0.0.1/callback", None),
            ("http://127.0.0.1:8888/callback", None),
            ("http://[::1]:5000/cb", None),
            ("https://octo.example.com/imports/spotify/callback", None),
            (
                "http://localhost:8888/callback",
                Some("Spotify does not take localhost. Use 127.0.0.1 instead."),
            ),
            (
                "http://192.168.50.21:5274/imports/spotify/callback",
                Some("Spotify takes only an https address, or http on 127.0.0.1 or [::1]."),
            ),
            ("callback", Some("The redirect URI is not a full address.")),
        ];
        for (uri, problem) in cases {
            assert_eq!(SpotifyAuth::redirect_problem(Some(uri)), problem, "case {uri}");
        }
    }

    #[test]
    fn an_app_may_use_its_own_port_only_on_a_portless_loopback() {
        let cases: [(&str, Option<&str>, Option<&str>); 7] = [
            // A loopback registered with no port takes any port on the same path: the apps' own listener.
            (
                "http://127.0.0.1/callback",
                Some("http://127.0.0.1:53111/callback"),
                Some("http://127.0.0.1:53111/callback"),
            ),
            (
                "http://127.0.0.1/callback",
                None,
                Some("http://127.0.0.1/callback"),
            ),
            (
                "http://127.0.0.1/callback",
                Some("http://127.0.0.1:53111/other"),
                None,
            ),
            (
                "http://127.0.0.1/callback",
                Some("http://evil.example/callback"),
                None,
            ),
            // A registered port is exact.
            (
                "http://127.0.0.1:8888/callback",
                Some("http://127.0.0.1:53111/callback"),
                None,
            ),
            (
                "http://127.0.0.1:8888/callback",
                Some("http://127.0.0.1:8888/callback"),
                Some("http://127.0.0.1:8888/callback"),
            ),
            (
                "https://octo.example.com/imports/spotify/callback",
                Some("http://127.0.0.1:53111/callback"),
                None,
            ),
        ];
        for (registered, asked, allowed) in cases {
            assert_eq!(
                SpotifyAuth::allowed_redirect(registered, asked).as_deref(),
                allowed,
                "case {registered} {asked:?}"
            );
        }
    }

    #[test]
    fn the_pasted_address_is_read_however_much_of_it_came() {
        type Case<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);
        let cases: [Case; 5] = [
            (
                "http://127.0.0.1/callback?code=abc%2Fd&state=s1",
                Some("abc/d"),
                Some("s1"),
                None,
            ),
            ("code=abc&state=s1", Some("abc"), Some("s1"), None),
            (
                "  http://127.0.0.1/callback?error=access_denied&state=s1 ",
                None,
                Some("s1"),
                Some("access_denied"),
            ),
            ("AQBxyz", Some("AQBxyz"), None, None),
            ("", None, None, None),
        ];
        for (pasted, code, state, error) in cases {
            let (c, s, e) = SpotifyAuth::parse_return(Some(pasted));
            assert_eq!(
                (c.as_deref(), s.as_deref(), e.as_deref()),
                (code, state, error),
                "case {pasted}"
            );
        }
    }
}
