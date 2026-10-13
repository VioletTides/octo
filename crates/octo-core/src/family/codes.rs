//! Invite and hand-over tokens, plus the comparisons and the Subsonic token math that check
//! sign-ins.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use md5::{Digest, Md5};

/// A new invite or session token: 32 random bytes, URL-safe base64 without padding.
pub fn token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

/// A new hidden Navidrome password for a member, which only Octo ever uses: 24 random bytes,
/// 32 URL-safe characters.
pub fn hidden_password() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 24]>())
}

/// Compares two secrets in time that does not depend on where they differ. Lengths are not
/// secret.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Subsonic's token: lower-case hex of `md5(secret + salt)`.
pub fn subsonic_token(secret: &str, salt: &str) -> String {
    hex::encode(Md5::digest(format!("{secret}{salt}").as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_url_safe() {
        let token = token();
        assert_eq!(token.len(), 43);
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_ne!(super::token(), super::token());
    }

    #[test]
    fn constant_time_eq_compares_whole_values() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secrets"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn subsonic_token_matches_the_spec_example() {
        assert_eq!(
            subsonic_token("sesame", "c19b2d"),
            "26719a1196d2a940705a59634eb18eab"
        );
    }
}
