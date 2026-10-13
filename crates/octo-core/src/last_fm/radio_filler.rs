//! Songs a radio leaves out: shorter than 45 seconds or longer than 45 minutes, an interview or a
//! podcast, or an intro, skit or interlude of four minutes or less. A missing or zero length is
//! not held against a song. The Octo apps apply the same rule.

use unicode_general_category::{GeneralCategory, get_general_category};

const SHORTEST_SECONDS: i32 = 45;
const LONGEST_SECONDS: i32 = 45 * 60;
const SHORT_PIECE_SECONDS: i32 = 4 * 60;

// Whole words with the edges spelled out, matching the apps exactly:
// `(?<![\p{L}\p{N}])(interview|podcast)(?![\p{L}\p{N}])`, ignoring case.
const TALK: [&str; 2] = ["interview", "podcast"];
// `(?<![\p{L}\p{N}])(intro|introduction|skit|interlude)(?![\p{L}\p{N}])`, ignoring case.
const FILLER: [&str; 4] = ["intro", "introduction", "skit", "interlude"];

pub fn is_filler(title: Option<&str>, duration_seconds: Option<i32>) -> bool {
    let seconds = duration_seconds.unwrap_or(0);
    if seconds > 0 && !(SHORTEST_SECONDS..=LONGEST_SECONDS).contains(&seconds) {
        return true;
    }
    let text = title.unwrap_or("");
    if has_word(text, &TALK) {
        return true;
    }
    seconds > 0 && seconds <= SHORT_PIECE_SECONDS && has_word(text, &FILLER)
}

/// Whether one of the words stands in the text with no letter or number touching either end.
/// `\p{L}` and `\p{N}` test one UTF-16 code unit, so a character outside the Basic Multilingual
/// Plane (two surrogates) never counts as touching.
fn has_word(text: &str, words: &[&str]) -> bool {
    // ASCII lower-casing keeps every byte offset, so a match in it is a match in the text.
    let lower = text.to_ascii_lowercase();
    words.iter().any(|word| {
        lower.match_indices(word).any(|(start, found)| {
            let before = text[..start].chars().next_back();
            let after = text[start + found.len()..].chars().next();
            !before.is_some_and(letter_or_number) && !after.is_some_and(letter_or_number)
        })
    })
}

fn letter_or_number(c: char) -> bool {
    (c as u32) <= 0xFFFF
        && matches!(
            get_general_category(c),
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
                | GeneralCategory::DecimalNumber
                | GeneralCategory::LetterNumber
                | GeneralCategory::OtherNumber
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_filler_leaves_out_short_pieces_talk_and_odd_lengths() {
        for (title, seconds, filler) in [
            ("Song", Some(30), true),
            ("Song", Some(3000), true),
            ("Song", Some(200), false),
            ("Song", None, false),
            ("Intro", Some(60), true),
            ("Interlude No. 2", Some(90), true),
            ("Intro", Some(300), false),
            ("Introducing the Band", Some(180), false),
            ("Intro", None, false),
            ("Interview with the Band", Some(600), true),
            ("Skité", Some(60), false),
            ("Café Intro", Some(60), true),
        ] {
            assert_eq!(is_filler(Some(title), seconds), filler, "{title} {seconds:?}");
        }
    }

    /// The edges are letters and numbers of any script, a digit counts, and a character outside
    /// the Basic Multilingual Plane never touches the word.
    #[test]
    fn the_word_edges_are_letters_and_numbers() {
        assert!(!is_filler(Some("Intro2"), Some(60)));
        assert!(!is_filler(Some("²Intro"), Some(60)));
        assert!(is_filler(Some("(Intro)"), Some(60)));
        assert!(is_filler(Some("PODCAST #4"), None));
        assert!(is_filler(Some("𝐀Intro"), Some(60)));
        assert!(!is_filler(Some("Introduction"), Some(300)));
        assert!(is_filler(Some("Introduction"), Some(200)));
        assert!(!is_filler(None, None));
    }
}
