//! The scores against reference values in docs/fuzzy-golden.json, and the edge cases they rest on.

use super::*;
use serde_json::Value;

fn golden() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/fuzzy-golden.json");
    let text = std::fs::read_to_string(path).expect("docs/fuzzy-golden.json is in the repo");
    serde_json::from_str(&text).expect("the golden file is JSON")
}

#[test]
fn matches_the_golden_values() {
    let golden = golden();
    let pairs = golden["pairs"].as_array().expect("pairs");
    assert!(pairs.len() >= 100, "{} pairs", pairs.len());

    let scorers: [(&str, fn(&str, &str) -> f64); 4] = [
        ("ratio", ratio),
        ("tokenSort", token_sort_ratio),
        ("tokenSet", token_set_ratio),
        ("jaroWinkler", jaro_winkler),
    ];
    let mut failures = Vec::new();
    for pair in pairs {
        let a = pair["a"].as_str().expect("a");
        let b = pair["b"].as_str().expect("b");
        for (name, scorer) in scorers {
            let want = pair[name].as_f64().expect("a score");
            let got = scorer(a, b);
            if (got - want).abs() > 0.001 {
                failures.push(format!("{name}({a:?}, {b:?}) = {got}, expected {want}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} off:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_subset_of_words_is_one() {
    assert_eq!(token_set_ratio("luis fonsi", "luis fonsi daddy yankee"), 1.0);
    assert_eq!(token_set_ratio("despacito remix", "despacito"), 1.0);
    assert!(token_set_ratio("one two", "three four") < 0.5);
}

#[test]
fn two_empty_strings_are_alike() {
    assert_eq!(ratio("", ""), 1.0);
    assert_eq!(token_sort_ratio("", ""), 1.0);
}

#[test]
fn one_empty_side_is_nothing_alike() {
    assert_eq!(ratio("", "abc"), 0.0);
    assert_eq!(ratio("abc", ""), 0.0);
    assert_eq!(token_sort_ratio("abc", ""), 0.0);
    assert_eq!(token_set_ratio("abc", ""), 0.0);
    assert_eq!(token_set_ratio("", ""), 0.0);
    assert_eq!(jaro_winkler("abc", ""), 0.0);
}

#[test]
fn word_order_does_not_count_for_token_sort() {
    assert_eq!(token_sort_ratio("fonsi luis", "luis fonsi"), 1.0);
    assert!(ratio("fonsi luis", "luis fonsi") < 1.0);
}
