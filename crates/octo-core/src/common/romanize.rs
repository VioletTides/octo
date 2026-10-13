//! Text written in another script, spelled in Latin letters, for telling whether two titles or
//! names are the same words written two ways ("Камин" and "Kamin").

use any_ascii::any_ascii;
use unicode_script::{Script, UnicodeScript};

use super::SongIdentity;

/// The text folded, spelled in ASCII letters, then keyed by [`SongIdentity::key`]: "Камин"
/// gives "kamin", "Café" gives "cafe". Plain ASCII keys as it always does. Tones and some
/// distinctions are lost on the way, so two different words can share one, most of all in
/// Chinese; compare it only across scripts.
pub fn romanized_key(text: &str) -> String {
    SongIdentity::key(&any_ascii(&SongIdentity::fold(text)))
}

/// The text folded, lowercased and spelled in ASCII letters, with every run of characters that
/// are not letters or digits read as one space: words that can be compared word by word.
pub fn romanized_words(text: &str) -> String {
    words(&any_ascii(&SongIdentity::fold(text)))
}

/// The text folded and lowercased, accents stripped, with every run of characters that are not
/// letters, digits or marks read as one space.
pub fn words(text: &str) -> String {
    SongIdentity::plain(text)
        .split(|ch: char| !(ch.is_alphanumeric() || is_mark(ch)))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_mark(ch: char) -> bool {
    use unicode_general_category::{GeneralCategory, get_general_category};
    matches!(
        get_general_category(ch),
        GeneralCategory::NonspacingMark | GeneralCategory::SpacingMark | GeneralCategory::EnclosingMark
    )
}

/// Whether the text has a letter outside ASCII, the only text whose romanized key can differ
/// from its key.
pub fn has_non_ascii_letters(text: &str) -> bool {
    text.chars().any(|ch| !ch.is_ascii() && ch.is_alphabetic())
}

/// The writing system a text starts in: the script of its first letter, after folding. Chinese
/// characters and Japanese kana count as one, so a Japanese title is never compared to a Chinese
/// one through their spellings. None when the text has no letter.
pub fn first_script(text: &str) -> Option<Script> {
    SongIdentity::fold(text)
        .chars()
        .filter(|ch| ch.is_alphabetic())
        .map(|ch| ch.script())
        .find(|script| !matches!(script, Script::Common | Script::Inherited | Script::Unknown))
        .map(|script| match script {
            Script::Hiragana | Script::Katakana | Script::Bopomofo => Script::Han,
            other => other,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_scripts_spell_in_latin_letters() {
        assert_eq!(romanized_key("Камин"), "kamin");
        assert_eq!(romanized_key("Café"), "cafe");
        assert_eq!(romanized_key("Bohemian Rhapsody"), "bohemianrhapsody");
        assert_eq!(
            romanized_key("Bohemian Rhapsody"),
            SongIdentity::key("Bohemian Rhapsody")
        );
    }

    #[test]
    fn bengali_spells_letter_by_letter() {
        // "শ" spells as "s", where the usual Latin spelling of this title writes "sh", so the
        // two keys differ by a letter.
        assert_eq!(romanized_key("নেশার বোঝা"), "nesarbojha");
        assert_ne!(romanized_key("নেশার বোঝা"), romanized_key("Neshar Bojha"));
    }

    #[test]
    fn words_split_on_punctuation_and_space() {
        assert_eq!(words("Fonsi, Luis"), "fonsi luis");
        assert_eq!(words("  AC/DC  "), "ac dc");
        assert_eq!(romanized_words("Луис Фонси"), "luis fonsi");
    }

    #[test]
    fn only_letters_outside_ascii_count() {
        assert!(has_non_ascii_letters("Камин"));
        assert!(has_non_ascii_letters("Café"));
        assert!(!has_non_ascii_letters("Kamin 2"));
        assert!(!has_non_ascii_letters("Song – Live"));
    }

    #[test]
    fn the_first_letter_decides_the_script() {
        assert_eq!(first_script("Kamin"), Some(Script::Latin));
        assert_eq!(first_script("2 Камин"), Some(Script::Cyrillic));
        assert_eq!(first_script("紅蓮華"), Some(Script::Han));
        assert_eq!(first_script("よるにかける"), Some(Script::Han));
        assert_eq!(first_script("사랑"), Some(Script::Hangul));
        assert_eq!(first_script("123"), None);
    }
}
