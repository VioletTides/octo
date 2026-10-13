//! Octo's outside address for Family links: the same `Server:PublicUrl` the dashboard's Status
//! page keeps. It is stored, never discovered: Octo only suggests the address it last saw
//! through a trusted reverse proxy, and the owner confirms it.
//!
//! Every invite and login answer offers two addresses, one for anywhere (the public address,
//! with an invite link naming the home address too) and one for home (the address the owner
//! used at home), and says which is the default.

use std::net::IpAddr;
use std::time::Duration;

use axum::http::HeaderMap;
use ipnet::IpNet;
use serde::Serialize;
use serde_json::Value;

use crate::app::AppState;

/// The dashboard's normalization: trimmed, no trailing slash, and https when no scheme is
/// given. Empty stays empty.
pub fn normalize_address(raw: &str) -> String {
    let value = raw.trim().trim_end_matches('/');
    if value.is_empty() {
        return String::new();
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        value.to_string()
    } else {
        format!("https://{value}")
    }
}

/// An address the owner typed, normalized, or the plain words saying what is wrong with it.
/// It must be http or https with a host, and may carry a path prefix (a proxy that serves
/// Octo under `/octo/`), but no sign-in, query or fragment. Empty clears the address.
pub fn validate_address(raw: &str) -> Result<String, String> {
    let normalized = normalize_address(raw);
    if normalized.is_empty() {
        return Ok(String::new());
    }
    let bad = || {
        "Type the address people use to reach Octo from outside, like https://music.example.com.".to_string()
    };
    // Another scheme, or a scheme with nothing after it, would otherwise gain an https:// in
    // front of it.
    let typed = raw.trim().trim_end_matches('/').to_ascii_lowercase();
    let has_http = typed.starts_with("http://") || typed.starts_with("https://");
    if (typed.contains("://") && !has_http) || typed.ends_with(':') {
        return Err(bad());
    }
    let url = url::Url::parse(&normalized).map_err(|_| bad())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(bad());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Leave the sign-in out of the address.".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Leave anything after ? or # out of the address.".to_string());
    }
    Ok(normalized)
}

/// The configured outside address, normalized, or None when it is not set.
pub fn public_url(state: &AppState) -> Option<String> {
    Some(normalize_address(&state.settings.current().server.public_url)).filter(|u| !u.is_empty())
}

/// Whether a link on `base` only works at home: a private, loopback or link-local address, or
/// a bare or local host name.
pub fn is_home_only(base: &str) -> bool {
    let Ok(url) = url::Url::parse(base) else {
        return true;
    };
    match url.host() {
        Some(url::Host::Ipv4(ip)) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
        }
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_private() || v4.is_loopback())
        }
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            !name.contains('.')
                || [
                    ".local",
                    ".lan",
                    ".home",
                    ".internal",
                    ".home.arpa",
                    ".localdomain",
                    ".localhost",
                ]
                .iter()
                .any(|end| name.ends_with(end))
        }
        None => true,
    }
}

/// The public origin a trusted reverse proxy says the request came in on: `X-Forwarded-Host`
/// with `X-Forwarded-Proto` (and `X-Forwarded-Prefix`), or the Host of an https request.
/// None unless the peer that connected is a trusted proxy, and None for a home-only address.
pub fn forwarded_origin(peer: IpAddr, headers: &HeaderMap, https: bool, trusted: &[IpNet]) -> Option<String> {
    let peer = peer.to_canonical();
    if !trusted.iter().any(|net| net.contains(&peer)) {
        return None;
    }
    let last = |name: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .rfind(|v| !v.is_empty())
            .map(str::to_string)
    };
    let origin = match last("x-forwarded-host") {
        Some(host) => {
            let scheme = match last("x-forwarded-proto") {
                Some(proto) if proto.eq_ignore_ascii_case("https") => "https",
                _ => "http",
            };
            let prefix = last("x-forwarded-prefix").unwrap_or_default();
            let prefix = prefix.trim_matches('/');
            if prefix.is_empty() {
                format!("{scheme}://{host}")
            } else {
                format!("{scheme}://{host}/{prefix}")
            }
        }
        None if https => format!("https://{}", last("host")?),
        None => return None,
    };
    let origin = validate_address(&origin).ok().filter(|o| !o.is_empty())?;
    (!is_home_only(&origin)).then_some(origin)
}

/// Whether two addresses name the same host, whatever their scheme, port or path.
fn same_host(a: &str, b: &str) -> bool {
    let host = |text: &str| {
        url::Url::parse(text)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    };
    matches!((host(a), host(b)), (Some(x), Some(y)) if x == y)
}

/// The two bases a link can use, and which is offered first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkBases {
    /// The outside address, when the owner set one.
    pub anywhere: Option<String>,
    /// The address the owner uses at home.
    pub home: String,
    /// The member may listen away from home.
    pub away_allowed: bool,
    /// An anywhere link can be given: there is an outside address and the member may use it.
    pub anywhere_available: bool,
    /// The default link only works at home, and nothing outside is set up.
    pub home_only: bool,
}

impl LinkBases {
    /// The bases for a link made from a request at `origin` for someone who may (or may not)
    /// listen away from home.
    pub fn new(state: &AppState, origin: &str, away_allowed: bool) -> LinkBases {
        let anywhere = public_url(state);
        let origin = origin.trim_end_matches('/').to_string();
        // The request's own address is home, unless it came in on the outside address (the
        // proxy may end https, so the host is what counts); then the last home address seen
        // stands in.
        let through_outside = anywhere.as_deref().is_some_and(|a| same_host(a, &origin));
        let home = match state.family.home_seen() {
            Some(seen) if through_outside => seen,
            _ => origin,
        };
        let anywhere_available = anywhere.is_some() && away_allowed;
        let home_only = anywhere.is_none() && is_home_only(&home);
        LinkBases {
            anywhere,
            home,
            away_allowed,
            anywhere_available,
            home_only,
        }
    }

    /// The base the old single link field carries: anywhere when it is available.
    pub fn default_base(&self) -> &str {
        match &self.anywhere {
            Some(anywhere) if self.anywhere_available => anywhere,
            _ => &self.home,
        }
    }

    /// `tail` (like `/family/join#u=alex&c=482913`) on both bases. The anywhere link names the
    /// home address too (`&home=` in the fragment), so an app can use it at home.
    pub fn links(&self, tail: &str) -> Value {
        let anywhere = self
            .anywhere
            .as_ref()
            .filter(|_| self.anywhere_available)
            .map(|base| {
                let mut link = format!("{base}{tail}");
                if !same_host(base, &self.home) {
                    let home = octo_core::common::dotnet::escape_data_string(&self.home);
                    link.push(if link.contains('#') { '&' } else { '#' });
                    link.push_str(&format!("home={home}"));
                }
                link
            });
        serde_json::json!({ "anywhere": anywhere, "home": format!("{}{tail}", self.home) })
    }

    /// The default link for `tail`, as the old single link field gives it.
    pub fn default_link(&self, tail: &str) -> String {
        let links = self.links(tail);
        let pick = if self.anywhere_available {
            "anywhere"
        } else {
            "home"
        };
        links[pick].as_str().unwrap_or_default().to_string()
    }
}

/// Whether `user` may listen away from home (anyone outside the family may).
pub fn away_allowed(state: &AppState, user: &str) -> bool {
    state.family_store.member(user).is_none_or(|m| m.abilities().away)
}

/// An invite answer with both links. `link` is the invite link on any base.
pub fn invite_answer(link: &str, bases: &LinkBases, days: u32) -> Value {
    let tail = link.find("/family/join").map_or(link, |at| &link[at..]);
    let expires = chrono::Utc::now() + chrono::TimeDelta::days(i64::from(days.max(1)));
    let mut answer = serde_json::json!({
        "inviteLink": bases.default_link(tail),
        "links": bases.links(tail),
        "inviteDays": days,
        "expires": octo_core::json::datetime::format_utc(&expires),
    });
    facts(&mut answer, bases);
    answer
}

fn facts(answer: &mut Value, bases: &LinkBases) {
    answer["anywhereAvailable"] = Value::Bool(bases.anywhere_available);
    answer["awayAllowed"] = Value::Bool(bases.away_allowed);
    answer["homeOnly"] = Value::Bool(bases.home_only);
    answer["publicUrl"] = serde_json::json!(bases.anywhere);
}

/// What reaching the outside address from inside came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Reach {
    Reachable,
    NotThisServer,
    CouldNotReach,
}

impl Reach {
    pub fn message(self) -> &'static str {
        match self {
            Reach::Reachable => "Octo answered at this address, so links on it work away from home.",
            Reach::NotThisServer => {
                "Something answered at this address, but it is not this Octo. Check where the address points."
            }
            Reach::CouldNotReach => {
                "Couldn't reach it from inside your network. That can be normal behind some routers; try it from your phone on mobile data."
            }
        }
    }
}

/// Asks `<url>/family/hello` and compares the instance id it answers with this Octo's.
pub async fn check(http: &reqwest::Client, url: &str, instance: &str) -> Reach {
    let hello = format!("{}/family/hello", url.trim_end_matches('/'));
    let answer = http.get(&hello).timeout(Duration::from_secs(8)).send().await;
    let Ok(answer) = answer else {
        return Reach::CouldNotReach;
    };
    if !answer.status().is_success() {
        return Reach::NotThisServer;
    }
    match answer.json::<Value>().await {
        Ok(body) if body["instance"].as_str() == Some(instance) => Reach::Reachable,
        _ => Reach::NotThisServer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn nets(text: &str) -> Vec<IpNet> {
        text.split(',')
            .map(|n| n.trim().parse().expect("a network"))
            .collect()
    }

    #[test]
    fn normalize_matches_the_dashboard() {
        assert_eq!(
            normalize_address("  music.example.com/ "),
            "https://music.example.com"
        );
        assert_eq!(
            normalize_address("http://10.0.0.5:5274//"),
            "http://10.0.0.5:5274"
        );
        assert_eq!(
            normalize_address("HTTPS://Example.com/octo/"),
            "HTTPS://Example.com/octo"
        );
        assert_eq!(normalize_address("   "), "");
    }

    #[test]
    fn validate_keeps_a_path_prefix_and_refuses_the_rest() {
        assert_eq!(
            validate_address("example.com/octo/").as_deref(),
            Ok("https://example.com/octo")
        );
        assert!(validate_address("ftp://example.com").is_err());
        assert!(validate_address("https://user:pw@example.com").is_err());
        assert!(validate_address("https://example.com/?x=1").is_err());
        assert!(validate_address("https://example.com/#x").is_err());
        assert!(validate_address("https://").is_err());
        assert_eq!(validate_address("").as_deref(), Ok(""));
    }

    #[test]
    fn home_only_addresses() {
        for home in [
            "http://192.168.50.119:5274",
            "http://10.1.2.3",
            "http://172.20.0.4",
            "http://127.0.0.1:18180",
            "http://169.254.1.1",
            "http://[::1]:5274",
            "http://[fd00::1]",
            "http://[fe80::1]",
            "http://localhost:5274",
            "http://octo:5274",
            "http://nas.local",
            "http://octo.lan",
            "http://octo.home.arpa",
        ] {
            assert!(is_home_only(home), "{home}");
        }
        for away in [
            "https://music.example.com",
            "http://203.0.113.9:5274",
            "https://example.com/octo",
        ] {
            assert!(!is_home_only(away), "{away}");
        }
    }

    #[test]
    fn forwarded_origin_needs_a_trusted_peer() {
        let trusted = nets("127.0.0.0/8, 172.18.0.0/16");
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-host", HeaderValue::from_static("music.example.com"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        let proxy: IpAddr = "172.18.0.5".parse().expect("an address");
        let stranger: IpAddr = "192.168.1.20".parse().expect("an address");
        assert_eq!(
            forwarded_origin(proxy, &headers, false, &trusted).as_deref(),
            Some("https://music.example.com")
        );
        assert_eq!(forwarded_origin(stranger, &headers, false, &trusted), None);
        headers.insert("x-forwarded-prefix", HeaderValue::from_static("/octo/"));
        assert_eq!(
            forwarded_origin(proxy, &headers, false, &trusted).as_deref(),
            Some("https://music.example.com/octo")
        );
        // A home address through the proxy is never suggested.
        let mut lan = HeaderMap::new();
        lan.insert("x-forwarded-host", HeaderValue::from_static("192.168.1.2:5274"));
        assert_eq!(forwarded_origin(proxy, &lan, false, &trusted), None);
        // Without X-Forwarded-Host, an https request's Host counts.
        let mut host = HeaderMap::new();
        host.insert("host", HeaderValue::from_static("music.example.com"));
        assert_eq!(
            forwarded_origin(proxy, &host, true, &trusted).as_deref(),
            Some("https://music.example.com")
        );
        assert_eq!(forwarded_origin(proxy, &host, false, &trusted), None);
    }
}
