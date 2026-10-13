//! Whether a song's words are explicit, the clean edit, or neither, as the catalog says it and
//! as OpenSubsonic clients read it. Kept as the catalog's own number on a [`Song`]
//! (`Song::explicit_content_lyrics`), which the explicit filter already reads.
//!
//! [`Song`]: super::Song

/// The catalog's numbers: 0 not explicit, 1 explicit, 3 the clean edit.
pub const NOT_EXPLICIT: i32 = 0;
pub const EXPLICIT: i32 = 1;
pub const CLEAN: i32 = 3;

/// The words OpenSubsonic's `explicitStatus` field takes; "" for neither or not known.
pub const EXPLICIT_WORD: &str = "explicit";
pub const CLEAN_WORD: &str = "clean";

/// The catalog's `explicit_content_lyrics` and `explicit_lyrics` as one number: 1 explicit, 3
/// the clean edit, 0 neither, None when it does not say. An album that is partly explicit (4)
/// counts as explicit, as a client marks such an album. Its other values (2 unknown, 5, 6, 7 no
/// advice) say nothing, so only the plain explicit flag can answer.
pub fn from_catalog(content: Option<i32>, explicit_lyrics: Option<bool>) -> Option<i32> {
    match content {
        Some(1 | 4) => Some(EXPLICIT),
        Some(3) => Some(CLEAN),
        Some(0) => Some(NOT_EXPLICIT),
        _ => (explicit_lyrics == Some(true)).then_some(EXPLICIT),
    }
}

/// The word a client gets for the catalog's number.
pub fn for_clients(content: Option<i32>) -> &'static str {
    match content {
        Some(EXPLICIT | 4) => EXPLICIT_WORD,
        Some(CLEAN) => CLEAN_WORD,
        _ => "",
    }
}

/// The version a clean copy is told apart by in an outside id; None for any other.
pub fn version_of(content: Option<i32>) -> Option<String> {
    (content == Some(CLEAN)).then(|| CLEAN_WORD.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_catalog_reads_the_catalogs_numbers_and_flag() {
        for (content, flag, expected) in [
            (Some(1), None, Some(1)),
            (Some(3), None, Some(3)),
            (Some(0), Some(true), Some(0)),
            (Some(4), None, Some(1)),
            (Some(2), Some(true), Some(1)),
            (Some(6), Some(false), None),
            (None, None, None),
        ] {
            assert_eq!(from_catalog(content, flag), expected, "{content:?}, {flag:?}");
        }
    }

    #[test]
    fn for_clients_is_open_subsonics_word() {
        for (content, word) in [
            (Some(1), "explicit"),
            (Some(3), "clean"),
            (Some(0), ""),
            (None, ""),
        ] {
            assert_eq!(for_clients(content), word, "{content:?}");
        }
    }
}
