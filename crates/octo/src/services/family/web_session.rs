//! The sign-in a member's browser holds for Navidrome's web player. Navidrome's own token
//! would also open Navidrome directly, around every family rule, so a member's browser gets
//! a token Octo signs instead (HS256 under the second half of `family.key`). Octo keeps the
//! real one on the member's web device and swaps it in on the way to Navidrome.
//!
//! The web player only decodes the token (it reads `uid`, and checks the shape), so Octo's
//! token carries the claims Navidrome's does, plus `did`, the device it belongs to.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use octo_core::family::codes::constant_time_eq;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// What Octo's token says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebClaims {
    pub iss: String,
    /// The member's username.
    pub sub: String,
    /// The member's Navidrome user id.
    pub uid: String,
    /// Always false: members are never Navidrome admins.
    pub adm: bool,
    /// Issued at, in seconds since 1970.
    pub iat: i64,
    /// Expires at, in seconds since 1970 (the same as the real token it stands for).
    pub exp: i64,
    /// The web device the real token is kept on.
    pub did: String,
}

const HEADER: &str = r#"{"alg":"HS256","typ":"JWT"}"#;
const BLOCK: usize = 64;

/// HMAC-SHA256 of `message` under `key`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

/// A signed token for `claims`.
pub fn mint(key: &[u8; 32], claims: &WebClaims) -> String {
    let header = URL_SAFE_NO_PAD.encode(HEADER);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap_or_default());
    let signing = format!("{header}.{payload}");
    let signature = URL_SAFE_NO_PAD.encode(hmac_sha256(key, signing.as_bytes()));
    format!("{signing}.{signature}")
}

/// The claims of a token Octo signed that has not expired at `now` (seconds since 1970).
/// None for anything else, a Navidrome token included.
pub fn verify(key: &[u8; 32], token: &str, now: i64) -> Option<WebClaims> {
    let (signing, signature) = token.trim().rsplit_once('.')?;
    let (header, payload) = signing.split_once('.')?;
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    if !constant_time_eq(&signature, &hmac_sha256(key, signing.as_bytes())) {
        return None;
    }
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
    if header.get("alg").and_then(Value::as_str) != Some("HS256") {
        return None;
    }
    let claims: WebClaims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    (claims.exp > now).then_some(claims)
}

/// The claims of any token, read without checking its signature. Only for a token Navidrome
/// will check itself (who it names, when it ends), never to let anything through.
pub fn unverified_claims(token: &str) -> Option<Value> {
    let mut parts = token.trim().split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    parts.next()?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}

/// The `sub` claim of any token, unchecked.
pub fn unverified_subject(token: &str) -> Option<String> {
    unverified_claims(token)?
        .get("sub")?
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// The `exp` claim of any token, unchecked.
pub fn unverified_expiry(token: &str) -> Option<i64> {
    unverified_claims(token)?.get("exp")?.as_i64()
}

/// The token in a `Bearer <token>` header value (any case), or None.
pub fn bearer(value: &str) -> Option<&str> {
    let value = value.trim();
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(exp: i64) -> WebClaims {
        WebClaims {
            iss: "ND".into(),
            sub: "alex".into(),
            uid: "u-alex".into(),
            adm: false,
            iat: 1_000,
            exp,
            did: "d_web".into(),
        }
    }

    #[test]
    fn hmac_matches_the_rfc_4231_example() {
        // Test case 2 of RFC 4231.
        assert_eq!(
            hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // A key longer than a block is hashed first (test case 6).
        assert_eq!(
            hex::encode(hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_minted_token_verifies_until_it_expires() {
        let key = [7u8; 32];
        let token = mint(&key, &claims(2_000));
        assert_eq!(verify(&key, &token, 1_500), Some(claims(2_000)));
        assert_eq!(verify(&key, &token, 2_000), None);
        assert_eq!(verify(&[8u8; 32], &token, 1_500), None);
        // Any change to the payload breaks the signature.
        let (head, rest) = token.split_once('.').expect("three parts");
        let (_, signature) = rest.split_once('.').expect("three parts");
        let forged = URL_SAFE_NO_PAD
            .encode(r#"{"iss":"ND","sub":"winters","uid":"x","adm":true,"iat":1,"exp":9999,"did":"d"}"#);
        assert_eq!(verify(&key, &format!("{head}.{forged}.{signature}"), 1_500), None);
        // Unchecked reads still name the subject.
        assert_eq!(unverified_subject(&token).as_deref(), Some("alex"));
        assert_eq!(unverified_expiry(&token), Some(2_000));
        assert_eq!(unverified_subject("not a token"), None);
    }

    #[test]
    fn bearer_values_are_read_in_any_case() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("BEARER  abc "), Some("abc"));
        assert_eq!(bearer("Basic abc"), None);
        assert_eq!(bearer("Bearer "), None);
    }
}
