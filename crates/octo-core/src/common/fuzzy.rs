//! String similarity scores from 0 to 1: the indel ratio, its two word-order-free forms, and
//! Jaro-Winkler. Inputs are compared as given; callers pass text already keyed or folded, with
//! words separated by whitespace.

use std::collections::BTreeSet;

use rapidfuzz::distance::{indel, jaro_winkler as jw};

/// How alike two strings are, by insertions and deletions: 1 minus the indel distance over the
/// two lengths together. Two empty strings are alike (1.0); one empty string is not (0.0).
pub fn ratio(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    indel::normalized_similarity(a.chars(), b.chars())
}

fn sorted_words(text: &str) -> Vec<&str> {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    words.sort_unstable();
    words
}

/// [`ratio`] with each side's words sorted first, so "fonsi luis" and "luis fonsi" are equal.
pub fn token_sort_ratio(a: &str, b: &str) -> f64 {
    ratio(&sorted_words(a).join(" "), &sorted_words(b).join(" "))
}

/// [`ratio`] over the words two strings share and the words only one has. When they share a
/// word and one side has no word the other lacks (its words are a subset of the other's), the
/// score is 1.0. A side with no words at all scores 0.0.
pub fn token_set_ratio(a: &str, b: &str) -> f64 {
    let words_a: BTreeSet<&str> = a.split_whitespace().collect();
    let words_b: BTreeSet<&str> = b.split_whitespace().collect();
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }
    let both: Vec<&str> = words_a.intersection(&words_b).copied().collect();
    let only_a: Vec<&str> = words_a.difference(&words_b).copied().collect();
    let only_b: Vec<&str> = words_b.difference(&words_a).copied().collect();
    if !both.is_empty() && (only_a.is_empty() || only_b.is_empty()) {
        return 1.0;
    }

    let base = both.join(" ");
    let with = |rest: &[&str]| -> String {
        match (base.is_empty(), rest.is_empty()) {
            (true, _) => rest.join(" "),
            (false, true) => base.clone(),
            (false, false) => format!("{base} {}", rest.join(" ")),
        }
    };
    let (left, right) = (with(&only_a), with(&only_b));
    let mut best = ratio(&left, &right);
    if !base.is_empty() {
        best = best.max(ratio(&base, &left)).max(ratio(&base, &right));
    }
    best
}

/// Jaro-Winkler similarity with a prefix weight of 0.1: shared leading characters (up to four)
/// count extra. For ranking only; it favors a shared start too much to accept a match by itself.
pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    jw::normalized_similarity_with_args(a.chars(), b.chars(), &jw::Args::default().prefix_weight(0.1))
}

#[cfg(test)]
#[path = "fuzzy_tests.rs"]
mod tests;
