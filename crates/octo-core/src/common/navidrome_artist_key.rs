//! The artist id Navidrome derives from an artist's name.
//!
//! Navidrome keys an artist by `NewHash(NewHash(Clear(ToLower(name))))`: the name lowercased one
//! character at a time, curly quotes, primes and dashes mapped to their ASCII forms, then an MD5
//! taken twice, each over its input followed by U+200B and written as 22 base62 digits. Stars,
//! ratings and play counts on an artist hang off this id, so two spellings with the same id are
//! one artist to Navidrome and two spellings with different ids are two. Accents are not folded:
//! "Beyonce" and "Beyoncé" are different artists.

use md5::{Digest, Md5};

/// Navidrome's base62 digits: 0-9, then a-z, then A-Z.
const DIGITS: &[u8; 62] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// The id Navidrome gives the artist named `name`.
pub fn navidrome_artist_key(name: &str) -> String {
    new_hash(&new_hash(&navidrome_artist_text(name)))
}

/// The text Navidrome hashes for an artist: lowercased, quotes and dashes made ASCII. Two names
/// with the same text are one artist to Navidrome.
pub fn navidrome_artist_text(name: &str) -> String {
    clear(&lower(name))
}

/// Whether two names are one artist to Navidrome.
pub fn same_navidrome_artist(a: &str, b: &str) -> bool {
    navidrome_artist_text(a) == navidrome_artist_text(b)
}

/// Lowercase one character at a time with the simple case mapping. A final sigma stays σ, and
/// U+0130 becomes a plain "i" rather than "i" plus a combining dot.
fn lower(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch == '\u{0130}' {
            out.push('i');
            continue;
        }
        let mut lowered = ch.to_lowercase();
        match (lowered.next(), lowered.next()) {
            (Some(single), None) => out.push(single),
            _ => out.push(ch),
        }
    }
    out
}

/// Quotes, primes and dashes as their ASCII forms. Nothing else changes.
fn clear(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' => '\'',
            '\u{FF02}' | '\u{3003}' | '\u{02EE}' | '\u{05F2}' | '\u{1CD3}' | '\u{2033}' | '\u{2036}'
            | '\u{02F6}' | '\u{02BA}' | '\u{201C}' | '\u{201D}' | '\u{02DD}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2013}' | '\u{2014}' | '\u{2212}' | '\u{2015}' => '-',
            other => other,
        })
        .collect()
}

/// MD5 over the text followed by U+200B, as 22 zero-padded base62 digits.
fn new_hash(text: &str) -> String {
    let mut hash = Md5::new();
    hash.update(text.as_bytes());
    hash.update("\u{200B}".as_bytes());
    let digest: [u8; 16] = hash.finalize().into();
    encode(u128::from_be_bytes(digest))
}

fn encode(mut value: u128) -> String {
    let mut digits = [b'0'; 22];
    let mut at = digits.len();
    while value > 0 {
        at -= 1;
        digits[at] = DIGITS[(value % 62) as usize];
        value /= 62;
    }
    String::from_utf8(digits.to_vec()).expect("base62 digits are ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values computed with an independent model of Navidrome 0.64.2's `id.NewHash`,
    /// `str.Clear` and Go's `strings.ToLower`.
    #[test]
    fn navidrome_artist_key_matches_navidrome_for_known_names() {
        let cases = [
            ("Beyonc\u{e9}", "3iyHg6jQfKnLyo8uIPPS4u"),
            ("Beyonce", "1w6HnDidV7KNXhkEwW8OXP"),
            ("BEYONC\u{c9}", "3iyHg6jQfKnLyo8uIPPS4u"),
            ("P!nk", "6zC8DNTlRedlHy0kWIIFv3"),
            ("Pink", "5S8c8kaFQDGBadLqnwE5OQ"),
            ("Guns N\u{2019} Roses", "2rkmRt7fqqdrOAOIiIlLXy"),
            ("Guns N' Roses", "2rkmRt7fqqdrOAOIiIlLXy"),
            ("Jay\u{2010}Z", "7e1z1LNsCyCLToKlIliS54"),
            ("Jay-Z", "7e1z1LNsCyCLToKlIliS54"),
            ("AC/DC", "6Jr12ZUrCphs0nyIXE6sSP"),
            ("", "0Bh2lJzVhiznDX7SygAOyP"),
            (
                "\u{3a3}\u{399}\u{393}\u{39c}\u{391}\u{3a3}",
                "4K9uBZo6pSbhgnlQHsDcE2",
            ),
            ("\u{130}stanbul", "3sLwUCMVKmVCTw36MkIyTo"),
            ("M\u{d8}", "33USXxMWXqMyCHbfyZFjNT"),
            ("M\u{f8}", "33USXxMWXqMyCHbfyZFjNT"),
            ("\u{201c}Weird\u{201d} Al", "3eN5zlzNDsmThBUFzUyt9v"),
            ("\"Weird\" Al", "3eN5zlzNDsmThBUFzUyt9v"),
            ("Earth, Wind & Fire", "1QqRS7whXpKW2zDoEN8JWG"),
            ("a", "0XbMEvXY5nyZBz5sRCCbNQ"),
        ];
        for (name, expected) in cases {
            assert_eq!(navidrome_artist_key(name), expected, "{name:?}");
        }
    }

    #[test]
    fn accents_make_a_different_navidrome_artist_and_case_does_not() {
        assert!(!same_navidrome_artist("Beyonce", "Beyonc\u{e9}"));
        assert!(same_navidrome_artist("beyonc\u{e9}", "BEYONC\u{c9}"));
        assert!(same_navidrome_artist("Jay\u{2014}Z", "jay-z"));
        assert!(!same_navidrome_artist("P!nk", "Pink"));
    }

    #[test]
    fn every_key_is_22_base62_digits() {
        for name in ["", "x", "Earth, Wind & Fire", "\u{1F600}"] {
            let key = navidrome_artist_key(name);
            assert_eq!(key.len(), 22);
            assert!(key.bytes().all(|b| DIGITS.contains(&b)));
        }
    }
}
