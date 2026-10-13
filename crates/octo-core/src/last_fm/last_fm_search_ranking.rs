//! The order of what Last.fm's track.search answers, after [`LastFmSearchCleanup`] has put the
//! names right.
//!
//! [`LastFmSearchCleanup`]: super::LastFmSearchCleanup

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::last_fm_service::SimilarTrack;
use crate::common::SongIdentity;
use crate::common::song_versions::{has_variant_word, strip_versions};

/// One letter of any script.
static LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{L}").expect("a valid pattern"));

/// Share of an artist and title's letters that has to be in another script for the row to count
/// as that script: more than half.
const NON_LATIN_SHARE: f64 = 0.5;

/// track.search ranks by fuzzy relevance, so "adele" answers with Skyfall (1.4 M listeners)
/// before Rolling in the Deep (2.7 M), and the first rows of a search are whatever matched the
/// text best rather than what people listen to. Every row carries its listener count, so the
/// answer is put in that order instead, after two steps that would otherwise let noise reach the
/// top:
///
///   1. Versions of one song ("Hello", "Hello - Live", "Hello (2011 Remaster)") are one row, the
///      most listened. Remixes and features are other songs and stay. A query that names a take
///      ("love story taylor's version") folds nothing, and a version whose marker shares a word
///      with the query keeps its own row, so the take asked for is never folded away.
///   2. For a Latin query, rows whose letters are mostly in another script ("Adele" answered with
///      Cyrillic scrobbles of a cover) go after every Latin row. They stay listed, since a
///      romanized search ("yorushika") can have its real answers in the native script. A Latin
///      title with one Greek letter in it ("Θ. Macarena") is a Latin title.
pub struct LastFmSearchRanking;

impl LastFmSearchRanking {
    /// The rows of `tracks` without repeated versions, most listened first. Rows without a count
    /// go after the counted ones, rows with the same count keep the order Last.fm gave them, and
    /// for a Latin query the rows in another script go last, in that same order among themselves.
    pub fn rank(query: &str, tracks: Vec<SimilarTrack>) -> Vec<SimilarTrack> {
        let demote_foreign = !Self::is_mostly_non_latin(query);

        // One row per song and version-less title, the most listened (the first on a tie).
        let fold = !has_variant_word(query);
        let query_words = words(query);
        let keys: Vec<String> = tracks
            .iter()
            .enumerate()
            .map(|(at, track)| match version_key(track, &query_words) {
                Some(key) if fold => key,
                _ => format!("row|{at}"),
            })
            .collect();
        let mut best: HashMap<&str, usize> = HashMap::new();
        for (at, key) in keys.iter().enumerate() {
            match best.get(key.as_str()) {
                Some(&kept) if listeners(&tracks[at]) <= listeners(&tracks[kept]) => {}
                _ => {
                    best.insert(key.as_str(), at);
                }
            }
        }
        let survives: Vec<bool> = keys
            .iter()
            .enumerate()
            .map(|(at, key)| best[key.as_str()] == at)
            .collect();

        let mut ranked: Vec<(bool, SimilarTrack)> = tracks
            .into_iter()
            .zip(survives)
            .filter(|(_, survives)| *survives)
            .map(|(track, _)| {
                let foreign =
                    demote_foreign && Self::is_mostly_non_latin(&format!("{} {}", track.artist, track.title));
                (foreign, track)
            })
            .collect();
        // A stable sort, so equal rows keep Last.fm's order.
        ranked.sort_by(|(a_foreign, a), (b_foreign, b)| {
            a_foreign
                .cmp(b_foreign)
                .then_with(|| listeners(b).cmp(&listeners(a)))
        });
        ranked.into_iter().map(|(_, track)| track).collect()
    }

    /// Whether more than half of the letters of `text` are in a script other than Latin
    /// (Cyrillic, Arabic, Hebrew, Thai, CJK, Hangul, Greek...). Text without letters is not.
    pub fn is_mostly_non_latin(text: &str) -> bool {
        let (mut letters, mut foreign) = (0usize, 0usize);
        for letter in LETTER.find_iter(text) {
            letters += 1;
            if !letter.as_str().chars().all(is_latin_letter) {
                foreign += 1;
            }
        }
        letters > 0 && foreign as f64 > letters as f64 * NON_LATIN_SHARE
    }
}

/// A row's listener count, with no count below every real one.
fn listeners(track: &SimilarTrack) -> i64 {
    track.listeners.unwrap_or(-1)
}

/// Basic Latin, Latin-1 Supplement, Latin Extended-A and B, IPA and spacing modifiers, phonetic
/// extensions, Latin Extended Additional, Latin Extended-C, D and E, and fullwidth Latin. Letters
/// from anywhere else are another script.
fn is_latin_letter(letter: char) -> bool {
    let cp = u32::from(letter);
    cp < 0x0250
        || (0x1D00..=0x1DBF).contains(&cp)
        || (0x1E00..=0x1EFF).contains(&cp)
        || (0x2C60..=0x2C7F).contains(&cp)
        || (0xA720..=0xA7FF).contains(&cp)
        || (0xAB30..=0xAB6F).contains(&cp)
        || (0xFF21..=0xFF3A).contains(&cp)
        || (0xFF41..=0xFF5A).contains(&cp)
}

/// The primary artist and the title without its version markers, as one key. None when the
/// markers taken off share a word with the query: that version is what was asked for.
fn version_key(track: &SimilarTrack, query_words: &HashSet<String>) -> Option<String> {
    let title = track.title.trim();
    let stripped = strip_versions(title);
    let tail = title.get(stripped.len()..).unwrap_or_default();
    if words(tail).iter().any(|word| query_words.contains(word)) {
        return None;
    }
    Some(format!(
        "{}|{}",
        SongIdentity::key(&SongIdentity::primary_artist(&track.artist)),
        SongIdentity::key(&stripped)
    ))
}

/// The words of `text`, lower case, apostrophes dropped ("Taylor's" is "taylors").
fn words(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .replace(['\'', '\u{2019}'], "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[path = "last_fm_search_ranking_tests.rs"]
mod tests;
