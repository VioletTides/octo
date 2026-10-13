//! Which artist names a folder, in one place, so a new download and the library tidy that
//! brings older files in line decide the same way. Two copies of this rule would drift, and the
//! tidy would then file a song where a fresh download of it would not go.

use std::sync::LazyLock;

use regex::Regex;

use super::dotnet;

/// What may follow the first artist of a credit for the credit to be a list.
static CREDIT_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(,|&|\+|/|;|\bx\b|\bvs\.?|\bfeat\.?|\bft\.?|\bfeaturing\b|\bwith\b|\band\b)")
        .expect("the credit separator pattern is valid")
});

/// The artist a folder is named after. Splits only on proof: a structured source (the
/// MusicBrainz credit, the catalog's main artist, an ARTISTS tag) that names either the whole
/// credit, which keeps it whole, or its FIRST artist followed by a separator, which splits
/// there. "Earth, Wind & Fire" and "Tyler, The Creator" survive because every structured source
/// names them whole. A separator on its own never splits anything.
pub fn primary_credit(requested: &str, structured: &[Option<&str>]) -> String {
    let whole = requested.trim();
    let candidates: Vec<&str> = structured
        .iter()
        .flatten()
        .filter(|value| !dotnet::is_blank(value))
        .map(|value| value.trim())
        .collect();

    if whole.is_empty() {
        return candidates.first().map(|c| c.to_string()).unwrap_or_default();
    }
    if candidates.iter().any(|c| dotnet::eq_ignore_case(c, whole)) {
        return whole.to_string();
    }

    for candidate in &candidates {
        if dotnet::utf16_len(whole) > dotnet::utf16_len(candidate)
            && dotnet::starts_with_ignore_case(whole, candidate)
        {
            // The comparison ignores case one character for one, so the candidate spans as
            // many characters of the whole as it has itself.
            let rest: String = whole.chars().skip(candidate.chars().count()).collect();
            if CREDIT_SEPARATOR.is_match(&rest) {
                return candidate.to_string();
            }
        }
    }
    whole.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_naming_the_first_artist_splits_there() {
        for (requested, first) in [
            ("Bizarrap, Rauw Alejandro", "Bizarrap"),
            ("Drake feat. Rihanna", "Drake"),
            ("Calvin Harris & Dua Lipa", "Calvin Harris"),
            ("A x B", "A"),
        ] {
            assert_eq!(primary_credit(requested, &[Some(first)]), first, "{requested}");
        }
    }

    #[test]
    fn a_source_naming_the_whole_credit_keeps_it_whole() {
        for name in ["Earth, Wind & Fire", "Tyler, The Creator", "Simon & Garfunkel"] {
            assert_eq!(primary_credit(name, &[Some("Earth"), Some(name)]), name);
        }
    }

    #[test]
    fn no_source_or_a_later_artist_never_splits() {
        assert_eq!(
            primary_credit("Bizarrap, Rauw Alejandro", &[None, Some("  ")]),
            "Bizarrap, Rauw Alejandro"
        );
        assert_eq!(
            primary_credit("Bizarrap, Rauw Alejandro", &[Some("Rauw Alejandro")]),
            "Bizarrap, Rauw Alejandro"
        );
        assert_eq!(primary_credit("Earthling", &[Some("Earth")]), "Earthling");
        assert_eq!(primary_credit("", &[None, Some("Muse")]), "Muse");
    }
}
