//! Takes a version marker off the end of a song title: "Hello - Live", "Hello (2011 Remaster)"
//! and "Hello [Mono Version]" are all "Hello". A remix, a mix or a feature is another song, so
//! those titles keep their whole text.

use std::sync::LazyLock;

use regex::Regex;

/// Words that make a bracket or a dash tail "the same song, another take".
const VARIANT_WORDS: &str = "live|remaster(?:ed)?|version|edit|mono|stereo|demo|acoustic|deluxe|bonus|explicit|clean|single|radio|re-?recorded|session|unplugged";

static VARIANT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)\b(?:{VARIANT_WORDS})\b")).expect("a valid pattern"));

/// Words that make it another song, whatever else it says: "Live (Remix)", "Radio Edit (feat. X)".
static OTHER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:remix|mix|rmx|feat|ft|featuring|with|vip|dub|rework|bootleg|flip|cover|instrumental|sped|slowed|reverb|nightcore)\b",
    )
    .expect("a valid pattern")
});

/// "(...)" or "[...]" at the end of a title.
static TRAILING_BRACKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*[\(\[]([^\(\)\[\]]*)[\)\]]\s*$").expect("a valid pattern"));

/// The tail after the last " - " (or an en or em dash with spaces round it).
static DASH_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+[-–—]\s+([^-–—]*)$").expect("a valid pattern"));

/// How many markers are taken off at most: "Hello (Live) (Remastered)" has two.
const MAX_PASSES: usize = 4;

/// The title with its version markers taken off the end, trimmed. A marker that is the whole
/// title stays, and so does one naming a remix, a mix, a feature or any other song.
pub fn strip_versions(title: &str) -> String {
    let mut text = title.trim().to_string();
    for _ in 0..MAX_PASSES {
        let next = strip_one(&text);
        if next == text {
            break;
        }
        text = next;
    }
    text
}

/// Whether `text` holds a word that marks a take of a song ("live", "version", "acoustic"...).
pub fn has_variant_word(text: &str) -> bool {
    VARIANT.is_match(text)
}

fn strip_one(text: &str) -> String {
    for pattern in [&*TRAILING_BRACKET, &*DASH_TAIL] {
        let Some(found) = pattern.captures(text) else {
            continue;
        };
        let whole = found.get(0).expect("a match has its whole text");
        if whole.start() == 0 {
            continue;
        }
        let inner = found.get(1).map_or("", |m| m.as_str());
        if VARIANT.is_match(inner) && !OTHER.is_match(inner) {
            return text[..whole.start()].trim_end().to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
#[path = "song_versions_tests.rs"]
mod tests;
