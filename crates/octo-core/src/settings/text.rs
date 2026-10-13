//! Small string semantics the settings classes lean on: lengths in UTF-16 code units,
//! invariant lower- and upper-casing as per-character mappings, and sets that compare
//! ordinally, ignoring case.

use std::collections::HashSet;

/// A string's length: the number of UTF-16 code units.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Invariant lower-casing: each character mapped on its own, never expanding (unlike
/// `str::to_lowercase`, which applies final-sigma rules and multi-character mappings).
pub(crate) fn lower_invariant(s: &str) -> String {
    s.chars().map(lower_char).collect()
}

/// Invariant upper-casing, per character as above (`ß` stays `ß`).
pub(crate) fn upper_invariant(s: &str) -> String {
    s.chars().map(upper_char).collect()
}

fn lower_char(c: char) -> char {
    if c == '\u{130}' {
        // The simple one-character mapping; Rust's full mapping gives "i\u{307}".
        return 'i';
    }
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn upper_char(c: char) -> char {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// The key an ordinal case-insensitive comparison compares by: every character upper-cased.
pub(crate) fn ignore_case_key(s: &str) -> String {
    upper_invariant(s)
}

/// Whether `a` and `b` are equal, ordinally and ignoring case.
pub(crate) fn eq_ignore_case(a: &str, b: &str) -> bool {
    a == b || ignore_case_key(a) == ignore_case_key(b)
}

/// A set of strings compared ordinally, ignoring case: membership ignores case, and each entry
/// keeps the spelling it was first added with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoreCaseSet {
    keys: HashSet<String>,
    items: Vec<String>,
}

impl IgnoreCaseSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `value` unless an entry equal to it ignoring case is already there. Returns
    /// whether it was added, as `HashSet.Add` does.
    pub fn insert(&mut self, value: impl Into<String>) -> bool {
        let value = value.into();
        if self.keys.insert(ignore_case_key(&value)) {
            self.items.push(value);
            true
        } else {
            false
        }
    }

    pub fn contains(&self, value: &str) -> bool {
        self.keys.contains(&ignore_case_key(value))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The entries in the order they were first added.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(String::as_str)
    }
}

impl<S: Into<String>> FromIterator<S> for IgnoreCaseSet {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        let mut set = IgnoreCaseSet::new();
        for item in iter {
            set.insert(item);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casing_maps_one_character_at_a_time() {
        assert_eq!(lower_invariant("ÉCOLE İ"), "école i");
        assert_eq!(upper_invariant("straße"), "STRAßE");
        assert_eq!(utf16_len("🛠 "), 3);
    }

    #[test]
    fn ignore_case_set_keeps_the_first_spelling() {
        let mut set = IgnoreCaseSet::new();
        assert!(set.insert("Music"));
        assert!(!set.insert("MUSIC"));
        assert!(set.contains("music"));
        assert_eq!(set.iter().collect::<Vec<_>>(), ["Music"]);
    }
}
