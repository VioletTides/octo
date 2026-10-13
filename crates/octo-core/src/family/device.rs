//! Devices: what Octo records of the apps people sign in with, the app name it shows for each,
//! whether it is home or away, and the client address behind trusted proxies. A device holds no
//! secret: everyone signs in with their one account password.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::quality::DeviceQualityMode;
use crate::json::datetime;

/// Home or away, from the client address and `Family:HomeNetworks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum NetworkPlace {
    #[default]
    Home,
    Away,
}

/// One device as `family.json` keeps it. Files written before devices lost their kind and
/// secret still load: those fields are ignored and gone at the next write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Device {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub name: String,
    /// When the device was first seen.
    #[serde(with = "datetime::utc")]
    pub created: DateTime<Utc>,
    #[serde(default, with = "datetime::utc_option")]
    pub last_seen: Option<DateTime<Utc>>,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub place: NetworkPlace,
    #[serde(default)]
    pub seen_apps: Vec<String>,
    /// A web player device's real Navidrome JWT, sealed: its web session. Never handed to the
    /// member: their browser holds a token Octo signs, swapped for this one on the way to
    /// Navidrome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navidrome_jwt: Option<String>,
    /// Whether the device follows the account's audio quality or lets its app decide.
    #[serde(default, skip_serializing_if = "DeviceQualityMode::is_account")]
    pub quality: DeviceQualityMode,
}

/// Subsonic clients Octo names by their `c` parameter, matched ignoring case at the start.
const KNOWN_CLIENTS: [(&str, &str); 11] = [
    ("symfonium", "Symfonium"),
    ("feishin", "Feishin"),
    ("amperfy", "Amperfy"),
    ("substreamer", "substreamer"),
    ("tempo", "Tempo"),
    ("dsub", "DSub"),
    ("ultrasonic", "Ultrasonic"),
    ("play:sub", "play:Sub"),
    ("sonixd", "Sonixd"),
    ("supersonic", "Supersonic"),
    ("navidromeui", "Navidrome"),
];

/// The app name the Family page shows for a device, from the Subsonic `c` parameter and the
/// User-Agent. `Octo` with `Octo/1.6 (Android)` is `Octo 1.6 (Android)`; a known client is
/// its name plus its version from the User-Agent when there is one; anything else is `c` as
/// given.
pub fn app_label(client: &str, user_agent: &str) -> String {
    let client = client.trim();
    let user_agent = user_agent.trim();
    if client.eq_ignore_ascii_case("octo") {
        if let Some(rest) = strip_prefix_ignore_case(user_agent, "octo/") {
            let (version, platform) = match rest.split_once(' ') {
                Some((version, tail)) => (version, tail.trim()),
                None => (rest, ""),
            };
            let platform = platform.trim_start_matches('(').trim_end_matches(')').trim();
            return if platform.is_empty() {
                format!("Octo {version}")
            } else {
                format!("Octo {version} ({platform})")
            };
        }
        return "Octo".to_string();
    }
    let lower = client.to_ascii_lowercase();
    if let Some((key, name)) = KNOWN_CLIENTS.iter().find(|(key, _)| lower.starts_with(key)) {
        return match version_in(user_agent, key) {
            Some(version) => format!("{name} {version}"),
            None => (*name).to_string(),
        };
    }
    if client.is_empty() {
        "Unknown app".to_string()
    } else {
        client.to_string()
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

/// The version after `<name>/` anywhere in the User-Agent (`Symfonium/12.3.0 (...)`).
fn version_in(user_agent: &str, name: &str) -> Option<String> {
    user_agent.split_whitespace().find_map(|token| {
        let rest = strip_prefix_ignore_case(token, name)?.strip_prefix('/')?;
        let version: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
            .collect();
        (!version.is_empty()).then_some(version)
    })
}

/// Splits a list of networks written by hand: commas and whitespace both separate, a bare
/// address is a single host (/32 or /128). Returns the networks and the pieces that were not
/// networks.
pub fn parse_networks(text: &str) -> (Vec<IpNet>, Vec<String>) {
    let mut networks = Vec::new();
    let mut rejected = Vec::new();
    for piece in text.split(|c: char| c == ',' || c.is_whitespace()) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        if let Ok(net) = piece.parse::<IpNet>() {
            networks.push(net);
        } else if let Ok(ip) = piece.parse::<IpAddr>() {
            networks.push(IpNet::from(ip));
        } else {
            rejected.push(piece.to_string());
        }
    }
    (networks, rejected)
}

/// An IPv4 address carried in IPv6 (`::ffff:192.168.1.5`) as the IPv4 address it is.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

fn contains(networks: &[IpNet], ip: IpAddr) -> bool {
    let ip = canonical(ip);
    networks.iter().any(|net| net.contains(&ip))
}

/// Home when the client address is in one of the home networks, else away.
pub fn place_of(ip: IpAddr, home: &[IpNet]) -> NetworkPlace {
    if contains(home, ip) {
        NetworkPlace::Home
    } else {
        NetworkPlace::Away
    }
}

/// The most characters a device name keeps.
pub const DEVICE_NAME_LIMIT: usize = 64;

/// A device name as the `X-Octo-Device-Name` header carries it (UTF-8, percent-encoded),
/// decoded: escapes that are not UTF-8 become the replacement character, control characters
/// are dropped, runs of spaces are one space, and anything past [`DEVICE_NAME_LIMIT`]
/// characters is cut. Never fails: a name that cannot be read is simply shorter.
pub fn decode_device_name(header: &str) -> String {
    let bytes = header.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            decoded.push(high * 16 + low);
            i += 3;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let text = String::from_utf8_lossy(&decoded);
    let mut name = String::new();
    for word in text.split(|c: char| c.is_whitespace() || c.is_control()) {
        if word.is_empty() {
            continue;
        }
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
    name.chars()
        .take(DEVICE_NAME_LIMIT)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The product a User-Agent starts with (`Symfonium` for `Symfonium/12.3.0 (Android 15)`),
/// without its version, so an app update does not look like a new device.
pub fn user_agent_family(user_agent: &str) -> &str {
    let first = user_agent.split_whitespace().next().unwrap_or("");
    first.split('/').next().unwrap_or("")
}

/// The address of the person making the request (the rule Navidrome 0.64.0 uses).
///
/// Only `X-Forwarded-For` is read, and only when the TCP peer is a trusted proxy (the tunnel
/// or a reverse proxy running beside Octo). It is walked from the right: each trusted proxy
/// appends the address it took the request from, so the first hop from the right that is not
/// itself a trusted proxy is the person. Hops further left are written by the client and
/// could say anything. With no such hop, the peer is the client. A peer that is not trusted
/// is the client, whatever its headers say.
///
/// No other header names the client. A tunnel such as cloudflared appends the visitor to
/// `X-Forwarded-For` as every proxy does, and a header like `Cf-Connecting-Ip` would let
/// anyone who reaches a trusted proxy without passing the tunnel name any address they like.
///
/// `x_forwarded_for` is every `X-Forwarded-For` value the request carries, joined with commas
/// in the order they arrived.
pub fn client_address(peer: IpAddr, x_forwarded_for: Option<&str>, trusted: &[IpNet]) -> IpAddr {
    let peer = canonical(peer);
    if !contains(trusted, peer) {
        return peer;
    }
    if let Some(list) = x_forwarded_for {
        for hop in list.rsplit(',') {
            let Ok(ip) = hop.trim().parse::<IpAddr>() else {
                // A hop that is not an address cannot be trusted further left either.
                return peer;
            };
            let ip = canonical(ip);
            if !contains(trusted, ip) {
                return ip;
            }
        }
    }
    peer
}

/// Where a request comes from, as the family rules read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientOrigin {
    /// The client address ([`client_address`]); behind a proxy Octo does not trust, the hop
    /// that proxy named, which only keeps one person's tries apart from another's.
    pub address: IpAddr,
    pub place: NetworkPlace,
    /// A peer on a home network that named a client in `X-Forwarded-For` without being a
    /// trusted proxy: a reverse proxy beside Octo that `Family:TrustedProxies` does not list.
    pub untrusted_proxy: Option<IpAddr>,
    /// The address Octo can vouch for: the client behind trusted proxies, else the peer that
    /// connected, whatever headers it sent. Throttles count on it with a higher ceiling, since
    /// `address` may be a hop an untrusted peer named.
    pub peer: IpAddr,
}

/// Where a request comes from: [`client_address`] and its place, except behind a reverse proxy
/// on a home network that is not trusted. Every request through such a proxy would come from
/// its home address and count as home, so instead it counts as away, and the hop the proxy
/// named stands for the client so per-address throttles do not all fall on the proxy.
pub fn client_origin(
    peer: IpAddr,
    x_forwarded_for: Option<&str>,
    trusted: &[IpNet],
    home: &[IpNet],
) -> ClientOrigin {
    let canonical_peer = canonical(peer);
    let forwarded = x_forwarded_for.filter(|list| !list.trim().is_empty());
    if let Some(list) = forwarded
        && !contains(trusted, canonical_peer)
        && contains(home, canonical_peer)
    {
        let named = list
            .rsplit(',')
            .next()
            .and_then(|hop| hop.trim().parse::<IpAddr>().ok())
            .map(canonical);
        return ClientOrigin {
            address: named.unwrap_or(canonical_peer),
            place: NetworkPlace::Away,
            untrusted_proxy: Some(canonical_peer),
            peer: canonical_peer,
        };
    }
    let address = client_address(peer, x_forwarded_for, trusted);
    ClientOrigin {
        address,
        place: place_of(address, home),
        untrusted_proxy: None,
        peer: address,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(text: &str) -> Vec<IpNet> {
        let (nets, rejected) = parse_networks(text);
        assert!(rejected.is_empty(), "{rejected:?}");
        nets
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("an address")
    }

    const HOME: &str = "127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, ::1/128, fc00::/7";
    const TRUSTED: &str = "127.0.0.0/8, ::1/128";

    #[test]
    fn octo_android_label() {
        assert_eq!(app_label("Octo", "Octo/1.6 (Android)"), "Octo 1.6 (Android)");
        assert_eq!(app_label("Octo", "Octo/1.6.2 (Windows)"), "Octo 1.6.2 (Windows)");
        assert_eq!(app_label("Octo", "okhttp/4.12.0"), "Octo");
    }

    #[test]
    fn symfonium_label_from_client() {
        assert_eq!(
            app_label("Symfonium", "Symfonium/12.3.0 (Android 15)"),
            "Symfonium 12.3.0"
        );
        assert_eq!(app_label("Symfonium", "okhttp/4.12.0"), "Symfonium");
        assert_eq!(
            app_label("feishin", "Mozilla/5.0 Feishin/0.12.1 Chrome/1"),
            "Feishin 0.12.1"
        );
        assert_eq!(app_label("NavidromeUI", "Mozilla/5.0"), "Navidrome");
        assert_eq!(app_label("DSub", ""), "DSub");
    }

    #[test]
    fn unknown_client_keeps_its_name() {
        assert_eq!(app_label("MyPlayer", "MyPlayer/2.0"), "MyPlayer");
        assert_eq!(app_label("  ", ""), "Unknown app");
    }

    #[test]
    fn private_ranges_are_home() {
        let home = nets(HOME);
        for address in [
            "192.168.1.5",
            "10.1.2.3",
            "172.20.0.4",
            "127.0.0.1",
            "::1",
            "fd00::5",
            "::ffff:192.168.1.5",
        ] {
            assert_eq!(place_of(ip(address), &home), NetworkPlace::Home, "{address}");
        }
        for address in ["203.0.113.9", "8.8.8.8", "2001:db8::1", "172.32.0.1"] {
            assert_eq!(place_of(ip(address), &home), NetworkPlace::Away, "{address}");
        }
    }

    #[test]
    fn bare_ip_is_a_single_host() {
        let home = nets("203.0.113.9 2001:db8::7");
        assert_eq!(home[0].prefix_len(), 32);
        assert_eq!(home[1].prefix_len(), 128);
        assert_eq!(place_of(ip("203.0.113.9"), &home), NetworkPlace::Home);
        assert_eq!(place_of(ip("203.0.113.10"), &home), NetworkPlace::Away);
    }

    #[test]
    fn spaces_after_commas_are_fine() {
        let (home, rejected) = parse_networks(" 10.0.0.0/8,  192.168.0.0/16 ,,\n::1/128, nonsense ");
        assert_eq!(home.len(), 3);
        assert_eq!(rejected, ["nonsense"]);
    }

    #[test]
    fn spoofed_forwarded_for_stays_away() {
        let trusted = nets(TRUSTED);
        let home = nets(HOME);
        // The client wrote a home address into X-Forwarded-For; the local proxy appended the
        // real one. The rightmost untrusted hop is the real one.
        let address = client_address(ip("127.0.0.1"), Some("192.168.1.5, 203.0.113.9"), &trusted);
        assert_eq!(address, ip("203.0.113.9"));
        assert_eq!(place_of(address, &home), NetworkPlace::Away);
        // A client connecting straight to Octo cannot claim anything with headers.
        let direct = client_address(ip("203.0.113.9"), Some("192.168.1.5"), &trusted);
        assert_eq!(direct, ip("203.0.113.9"));
        assert_eq!(place_of(direct, &home), NetworkPlace::Away);
    }

    #[test]
    fn tunnel_client_address_used() {
        // cloudflared on another machine at home, listed as a trusted proxy. The visitor wrote
        // a home address of their own; the tunnel appended the address it saw.
        let trusted = nets("127.0.0.0/8, ::1/128, 192.168.1.50");
        let home = nets(HOME);
        let address = client_address(ip("192.168.1.50"), Some("192.168.1.5, 203.0.113.9"), &trusted);
        assert_eq!(address, ip("203.0.113.9"));
        assert_eq!(place_of(address, &home), NetworkPlace::Away);
        // cloudflared on the same host: the peer is loopback, the person is in the header.
        assert_eq!(
            client_address(ip("127.0.0.1"), Some("203.0.113.9"), &nets(TRUSTED)),
            ip("203.0.113.9")
        );
        // Without headers the loopback peer is the client (someone on the box itself).
        assert_eq!(client_address(ip("::1"), None, &trusted), ip("::1"));
        // The tunnel's machine not listed: it is the client, and its headers are not read.
        assert_eq!(
            client_address(ip("192.168.1.50"), Some("203.0.113.9"), &nets(TRUSTED)),
            ip("192.168.1.50")
        );
    }

    #[test]
    fn an_untrusted_proxy_at_home_makes_its_clients_away() {
        let (trusted, home) = (nets(TRUSTED), nets(HOME));
        // A reverse proxy in its own container, on a Docker bridge that counts as home, not
        // listed as trusted: whoever it names is away, and keeps their own throttle key.
        let origin = client_origin(ip("172.18.0.5"), Some("198.51.100.7"), &trusted, &home);
        assert_eq!(origin.place, NetworkPlace::Away);
        assert_eq!(origin.address, ip("198.51.100.7"));
        assert_eq!(origin.untrusted_proxy, Some(ip("172.18.0.5")));
        let other = client_origin(ip("172.18.0.5"), Some("198.51.100.8"), &trusted, &home);
        assert_ne!(origin.address, other.address);
        // A hop that is not an address leaves the proxy as the address, still away.
        let garbled = client_origin(ip("172.18.0.5"), Some("nonsense"), &trusted, &home);
        assert_eq!(
            (garbled.address, garbled.place),
            (ip("172.18.0.5"), NetworkPlace::Away)
        );
        // Without the header the peer is a person at home, as before.
        let plain = client_origin(ip("192.168.1.5"), None, &trusted, &home);
        assert_eq!(
            (plain.address, plain.place, plain.untrusted_proxy),
            (ip("192.168.1.5"), NetworkPlace::Home, None)
        );
        // An outside peer is away and names nobody, whatever it writes.
        let outside = client_origin(ip("203.0.113.9"), Some("192.168.1.5"), &trusted, &home);
        assert_eq!(
            (outside.address, outside.place, outside.untrusted_proxy),
            (ip("203.0.113.9"), NetworkPlace::Away, None)
        );
        // Once the proxy is trusted, the usual rule names the person and their place.
        let listed = nets("127.0.0.0/8, ::1/128, 172.18.0.5");
        let fixed = client_origin(ip("172.18.0.5"), Some("192.168.1.9"), &listed, &home);
        assert_eq!(
            (fixed.address, fixed.place, fixed.untrusted_proxy),
            (ip("192.168.1.9"), NetworkPlace::Home, None)
        );
    }

    #[test]
    fn trusted_hops_are_skipped_from_the_right() {
        let trusted = nets("127.0.0.0/8, 10.9.0.0/16");
        let address = client_address(
            ip("127.0.0.1"),
            Some("198.51.100.2, 192.168.1.20, 10.9.0.5"),
            &trusted,
        );
        assert_eq!(address, ip("192.168.1.20"));
        // Every hop trusted, or a hop that is not an address: the peer.
        assert_eq!(
            client_address(ip("127.0.0.1"), Some("10.9.0.5"), &trusted),
            ip("127.0.0.1")
        );
        assert_eq!(
            client_address(ip("127.0.0.1"), Some("unknown, 10.9.0.5"), &trusted),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn utf8_device_name_is_decoded() {
        assert_eq!(
            decode_device_name("Sam%E2%80%99s%20Caf%C3%A9"),
            "Sam\u{2019}s Caf\u{e9}"
        );
        assert_eq!(decode_device_name("Pixel 9"), "Pixel 9");
        // Raw UTF-8, lower-case escapes and a stray percent all read.
        assert_eq!(decode_device_name("Caf\u{e9} 100%"), "Caf\u{e9} 100%");
        assert_eq!(decode_device_name("a%2fb"), "a/b");
        // Escapes that are not UTF-8, control characters and long runs of spaces.
        assert_eq!(decode_device_name("bad%FFname"), "bad\u{fffd}name");
        assert_eq!(decode_device_name("  one%0Atwo%09  three  "), "one two three");
        // Cut at 64 characters, counted as characters, not bytes.
        let long = "%C3%A9".repeat(80);
        assert_eq!(decode_device_name(&long).chars().count(), DEVICE_NAME_LIMIT);
        assert_eq!(decode_device_name(""), "");
    }

    #[test]
    fn user_agent_family_drops_the_version() {
        assert_eq!(user_agent_family("Symfonium/12.3.0 (Android 15)"), "Symfonium");
        assert_eq!(user_agent_family("Mozilla/5.0 (X11) Chrome/1"), "Mozilla");
        assert_eq!(user_agent_family(""), "");
    }

    #[test]
    fn device_round_trips_without_a_jwt_field_when_unset() {
        let device = Device {
            id: "d_1".into(),
            username: "alex".into(),
            name: "Pixel 9".into(),
            created: datetime::parse_utc("2026-10-20T18:00:00Z").expect("a date"),
            last_seen: None,
            app: "Octo 1.6 (Android)".into(),
            place: NetworkPlace::Away,
            seen_apps: vec!["Octo 1.6 (Android)".into()],
            navidrome_jwt: None,
            quality: DeviceQualityMode::Account,
        };
        let text = serde_json::to_string(&device).expect("serializes");
        assert!(!text.contains("NavidromeJwt"));
        assert!(!text.contains("Quality"));
        assert!(!text.contains("Kind") && !text.contains("Secret"));
        let back: Device = serde_json::from_str(&text).expect("reads back");
        assert_eq!(back, device);
    }

    #[test]
    fn a_device_from_before_one_login_loads_without_its_kind_and_secret() {
        let old = r#"{"Id":"d_1","Username":"alex","Name":"Pixel 9","Kind":"OctoApp",
            "Secret":"sealed:abc","Created":"2026-10-20T18:00:00Z","App":"Octo"}"#;
        let device: Device = serde_json::from_str(old).expect("loads");
        assert_eq!(device.id, "d_1");
        let text = serde_json::to_string(&device).expect("serializes");
        assert!(!text.contains("Secret") && !text.contains("sealed:abc"), "{text}");
        assert!(!text.contains("Kind"), "{text}");
    }
}
