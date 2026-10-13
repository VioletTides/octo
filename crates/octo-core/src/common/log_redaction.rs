//! The pure half of log redaction: the redaction itself. Wrapping the log pipeline belongs to
//! the tracing setup in the `octo` crate.
//!
//! Keeps Subsonic credentials out of the log. Subsonic clients sign in through the query string,
//! so every request line written ("Request starting ... /rest/star?u=...&t=...&s=...")
//! carries a token and salt that replay as that user, and `p=` and `apiKey=` are worse.
//!
//! The redaction wraps the logging pipeline rather than any one log statement, so it holds for
//! request lines, HTTP client lines and Octo's own, at whatever level an operator turns them
//! up to. Only the values are masked; the lines keep their shape.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::{Captures, Regex};

pub const MASK: &str = "***";

// t and s are the token login, p the password (plain or enc:), apiKey the OpenSubsonic key,
// token what several clients call theirs, api_key Last.fm's key and client AcoustID's.
// Subsonic reads names case-insensitively, so this does. sk is a Last.fm session key,
// which scrobbles as that listener, and api_sig is signed with the Last.fm shared secret.
// user is the AcoustID user's own API key, sent with every submission. Subsonic's u is
// not it, and stays readable. password and invite are Octo Family's account passwords and
// invite tokens, current and next the old and new password of a password change, box a sealed
// sign-in hand-over and k the key that opens one (no Octo route takes it, and a client that
// sends it anyway never has it written down), code and secret the codes and secrets older
// Family apps send, and jwt a browser's sign-in.
//
// The `regex` crate has no lookbehind, so rather than `(?<=[?&;](?:...)=)[^&#\s"'<>]+` the
// name is group 1 and is written back. The two find the same values: a value never holds an
// `&`, so the next parameter's `[?&;]name=` always starts at or after the end of the last value.
//
// Every `s` is spelled `(?-i:[sS])`: Rust's case folding also matches the long s (ſ) there,
// and the long s must not count as an s.
static SECRET_PARAMETER: LazyLock<Regex> = LazyLock::new(|| {
    let s = "(?-i:[sS])";
    Regex::new(&format!(
        r#"(?i)([?&;](?:t|{s}|p|apikey|token|api_key|client|u{s}er|{s}k|api_{s}ig|code|{s}ecret|pa{s}{s}word|invite|jwt|box|k|current|next)=)[^&#\s"'<>]+"#
    ))
    .expect("a fixed pattern compiles")
});

/// A sign-in hand-over token or redemption id in a Family page path:
/// `/api/family/signin/<token>/pending` and `/api/family/signin/redeem/<id>`. Group 2 is the
/// segment; `start` and `redeem` themselves are route names and stay readable.
static HAND_OVER_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(/api/family/signin/(?:redeem/)?)([^/?#\s"'<>]+)"#).expect("a fixed pattern compiles")
});

/// The `id` of `getFamilySignInRedeem`, the apps' way to fetch a sign-in hand-over's box: there
/// it is the redemption id, which fetches the box. Every other `id` stays readable.
static HAND_OVER_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(/rest/getFamilySignInRedeem(?:\.view)?\?(?:[^#\s"'<>]*?[&;])?id=)[^&#\s"'<>]+"#)
        .expect("a fixed pattern compiles")
});

/// The text with every secret query parameter's value, and any sign-in hand-over token or
/// redemption id in a path or query, replaced by [`MASK`]. Borrows the text unchanged when there
/// is nothing to mask.
pub fn redact(text: &str) -> Cow<'_, str> {
    let text = if text.contains("/family/signin/") {
        HAND_OVER_PATH.replace_all(text, |caps: &Captures| match &caps[2] {
            "start" | "redeem" => caps[0].to_string(),
            _ => format!("{}{MASK}", &caps[1]),
        })
    } else {
        Cow::Borrowed(text)
    };
    let text = if text.to_ascii_lowercase().contains("getfamilysigninredeem") {
        match HAND_OVER_ID.replace_all(&text, |caps: &Captures| format!("{}{MASK}", &caps[1])) {
            Cow::Borrowed(_) => text,
            Cow::Owned(masked) => Cow::Owned(masked),
        }
    } else {
        text
    };
    if !text.contains('=') {
        return text;
    }
    let masked = match SECRET_PARAMETER.replace_all(&text, |caps: &Captures| format!("{}{MASK}", &caps[1])) {
        Cow::Borrowed(_) => None,
        Cow::Owned(masked) => Some(masked),
    };
    masked.map_or(text, Cow::Owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_NAMES: [&str; 16] = [
        "t", "s", "p", "apiKey", "token", "api_key", "client", "sk", "api_sig", "T", "APIKEY", "Token",
        "API_KEY", "Client", "user", "User",
    ];

    #[test]
    fn redact_masks_only_secret_values() {
        let cases = [
            ("/rest/ping?u=a&t=abc&s=def", "/rest/ping?u=a&t=***&s=***"),
            ("?p=enc:6162&u=a", "?p=***&u=a"),
            ("?apikey=K1&Token=K2#top", "?apikey=***&Token=***#top"),
            (
                "/2.0/?method=track.search&api_key=K1&format=json",
                "/2.0/?method=track.search&api_key=***&format=json",
            ),
            (
                "v2/lookup?client=K1&meta=recordings",
                "v2/lookup?client=***&meta=recordings",
            ),
            ("GET /rest/x?t=abc - 200", "GET /rest/x?t=*** - 200"),
            (
                "<a href='/rest/x?u=a&amp;t=abc'>",
                "<a href='/rest/x?u=a&amp;t=***'>",
            ),
            // Names that only start or end like a secret, and empty values, are left alone.
            (
                "?ts=1&st=2&sort=3&apiKeyId=4&tokens=5&clientId=6&api_keys=7&c=Octo",
                "?ts=1&st=2&sort=3&apiKeyId=4&tokens=5&clientId=6&api_keys=7&c=Octo",
            ),
            ("?t=&s=", "?t=&s="),
            ("no query here, t=abc", "no query here, t=abc"),
        ];
        for (text, expected) in cases {
            assert_eq!(redact(text), expected, "{text}");
        }
    }

    #[test]
    fn case_is_ignored_with_the_kelvin_sign_but_not_the_long_s() {
        // The Kelvin sign is a k, the long s is not an s. A Kelvin sign alone is the hand-over key `k`, masked too.
        assert_eq!(
            redact("?apiKEY=1&S=2&\u{212A}=3&s\u{212A}=4"),
            "?apiKEY=***&S=***&\u{212A}=***&s\u{212A}=***"
        );
        assert_eq!(
            redact("?\u{17F}=secret&u\u{17F}er=x"),
            "?\u{17F}=secret&u\u{17F}er=x"
        );
    }

    #[test]
    fn redact_returns_the_same_string_when_nothing_is_masked() {
        let text = "/rest/ping?u=winters&v=1.16.1";
        assert!(matches!(redact(text), Cow::Borrowed(same) if std::ptr::eq(same, text)));
    }

    #[test]
    fn request_lines_mask_each_secret_parameter() {
        // Each secret name, sent on its own, is masked and the rest of the line kept.
        for name in SECRET_NAMES {
            let secret = format!("Secret{}", uuid::Uuid::new_v4().simple());
            let line = format!("/rest/ping.view?u=winters&{name}={secret}&v=1.16.1&c=Octo&f=json");
            let redacted = redact(&line);
            assert!(
                redacted.contains(&format!(
                    "/rest/ping.view?u=winters&{name}=***&v=1.16.1&c=Octo&f=json"
                )),
                "{name}: {redacted}"
            );
            assert!(!redacted.contains(&secret), "{name}");

            let own =
                format!("relay to http://navidrome:4533/rest/getSong?u=winters&{name}={secret}&id=7 failed");
            assert!(
                redact(&own).contains(&format!("u=winters&{name}=***&id=7")),
                "{name}"
            );
        }
    }

    #[test]
    fn request_lines_mask_every_secret_at_once_and_keep_the_rest() {
        let secret = || format!("Secret{}", uuid::Uuid::new_v4().simple());
        let (t, s, p, api_key, token) = (
            secret(),
            secret(),
            format!("enc:{}", secret()),
            secret(),
            secret(),
        );
        let line = format!(
            "Request finished HTTP/1.1 GET http://localhost/rest/star.view?u=winters&t={t}&s={s}&p={p}&apiKey={api_key}&token={token}&v=1.16.1&c=Octo&f=json&id=42 - 200"
        );

        let redacted = redact(&line);
        assert!(
            redacted.contains(
                "/rest/star.view?u=winters&t=***&s=***&p=***&apiKey=***&token=***&v=1.16.1&c=Octo&f=json&id=42"
            ),
            "{redacted}"
        );
        for secret in [t, s, p, api_key, token] {
            assert!(!redacted.contains(&secret));
        }
    }

    #[test]
    fn hand_over_tokens_are_masked_in_paths_and_queries() {
        let cases = [
            (
                "GET /api/family/signin/AbC-123_x/pending - 200",
                "GET /api/family/signin/***/pending - 200",
            ),
            (
                "POST /api/family/signin/AbC/decide",
                "POST /api/family/signin/***/decide",
            ),
            (
                "GET /api/family/signin/redeem/Id-9?f=json",
                "GET /api/family/signin/redeem/***?f=json",
            ),
            ("POST /api/family/signin/start", "POST /api/family/signin/start"),
            ("POST /api/family/signin/redeem", "POST /api/family/signin/redeem"),
            (
                "/rest/putFamilySignInBox?u=a&t=T&s=S&token=K&box=B",
                "/rest/putFamilySignInBox?u=a&t=***&s=***&token=***&box=***",
            ),
            (
                "GET /rest/getFamilySignInRedeem?id=Id-9&v=1.16.1&f=json - 200",
                "GET /rest/getFamilySignInRedeem?id=***&v=1.16.1&f=json - 200",
            ),
            (
                "GET /rest/getFamilySignInRedeem.view?f=json&ID=Id-9&c=Octo",
                "GET /rest/getFamilySignInRedeem.view?f=json&ID=***&c=Octo",
            ),
            (
                "POST /api/family/signin/redeem?k=Key-1&token=T",
                "POST /api/family/signin/redeem?k=***&token=***",
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(redact(text), expected, "{text}");
        }
    }

    #[test]
    fn a_password_change_and_a_hand_over_key_are_masked_and_other_ids_are_not() {
        assert_eq!(
            redact("GET /rest/changeFamilyPassword?u=alex&p=P&current=Old-1&next=New-2&k=K-3 - 200"),
            "GET /rest/changeFamilyPassword?u=alex&p=***&current=***&next=***&k=*** - 200"
        );
        for kept in [
            "/rest/getSong?id=song-1&u=a",
            "/rest/getFamilySignInPending?id=x&u=a",
            "?kind=1&nexts=2&currently=3&key=4",
        ] {
            assert_eq!(redact(kept), kept);
        }
    }

    #[test]
    fn family_codes_are_masked() {
        let line = "/rest/octoFamilyPair?username=alex&code=123456&deviceName=Pixel&secret=S1&password=P1&invite=I1&jwt=J1&Code=2";
        assert_eq!(
            redact(line),
            "/rest/octoFamilyPair?username=alex&code=***&deviceName=Pixel&secret=***&password=***&invite=***&jwt=***&Code=***"
        );
        // Names that only start like one of them stay readable.
        let other = "?codec=mp3&secrets=1&passwords=2&invited=3&jwtx=4";
        assert_eq!(redact(other), other);
    }
}
