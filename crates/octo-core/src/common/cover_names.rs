//! How well a search result's artist name fits the one asked for, for ranking cover art hits.

use super::SongIdentity;
use super::fuzzy::token_sort_ratio;
use super::romanize::{romanized_key, romanized_words, words};

/// How well `got` names the artist `want`, from 0 to 100, for pushing another artist's hits to
/// the bottom of a cover search:
///
/// - 100: the same name by [`SongIdentity::same_artist_name`] (case, accents, a leading "The",
///   stylized characters and aliases ignored), or the same name spelled in another script;
/// - 80: the same words in any order, allowing a typo ("Fonsi, Luis" for "Luis Fonsi");
/// - 60: one name holds the other as whole words ("Bob Marley" in "Bob Marley & The Wailers",
///   never "Air" in "Airbourne");
/// - otherwise 10 for each word they share, at most 50.
///
/// Either name empty scores 0.
pub fn cover_name_score(want: &str, got: &str) -> u8 {
    if SongIdentity::key(want).is_empty() || SongIdentity::key(got).is_empty() {
        return 0;
    }
    if SongIdentity::same_artist_name(want, got) {
        return 100;
    }
    let (want_roman, got_roman) = (romanized_key(want), romanized_key(got));
    if !want_roman.is_empty() && want_roman == got_roman {
        return 100;
    }

    let (want_words, got_words) = (words(want), words(got));
    let sorted = token_sort_ratio(&want_words, &got_words)
        .max(token_sort_ratio(&romanized_words(want), &romanized_words(got)));
    if sorted >= 0.92 {
        return 80;
    }

    let want_list: Vec<&str> = want_words.split(' ').filter(|word| !word.is_empty()).collect();
    let got_list: Vec<&str> = got_words.split(' ').filter(|word| !word.is_empty()).collect();
    let (short, long) = if want_list.len() <= got_list.len() {
        (&want_list, &got_list)
    } else {
        (&got_list, &want_list)
    };
    if !short.is_empty() && long.windows(short.len()).any(|run| run == short.as_slice()) {
        return 60;
    }

    let mut shared: Vec<&str> = want_list
        .iter()
        .copied()
        .filter(|word| got_list.contains(word))
        .collect();
    shared.sort_unstable();
    shared.dedup();
    (shared.len().min(5) * 10) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_artist_however_written_is_a_full_score() {
        assert_eq!(cover_name_score("Beyoncé", "Beyonce"), 100);
        assert_eq!(cover_name_score("The Weeknd", "weeknd"), 100);
        assert_eq!(cover_name_score("$uicideboy$", "Suicideboys"), 100);
        assert_eq!(cover_name_score("Kanye West", "Ye"), 100);
        assert_eq!(cover_name_score("Кино", "Kino"), 100);
    }

    #[test]
    fn the_same_words_in_another_order_score_eighty() {
        assert_eq!(cover_name_score("Luis Fonsi", "Fonsi, Luis"), 80);
        assert_eq!(cover_name_score("Christina Aguilera", "Christina Aguillera"), 80);
    }

    #[test]
    fn whole_words_inside_the_other_name_score_sixty() {
        assert_eq!(cover_name_score("Bob Marley", "Bob Marley & The Wailers"), 60);
        assert_eq!(cover_name_score("Daft Punk feat. Pharrell", "Daft Punk"), 60);
    }

    #[test]
    fn letters_inside_a_word_are_not_the_name() {
        // A substring used to score 60 here.
        assert_eq!(cover_name_score("Air", "Airbourne"), 0);
        assert_eq!(cover_name_score("Muse", "Museum Choir"), 0);
    }

    #[test]
    fn shared_words_count_ten_each() {
        assert_eq!(cover_name_score("Red Hot Chili Peppers", "Red Hot Band"), 20);
        assert_eq!(cover_name_score("Radiohead", "Coldplay"), 0);
    }

    #[test]
    fn an_empty_name_scores_nothing() {
        assert_eq!(cover_name_score("Radiohead", ""), 0);
        assert_eq!(cover_name_score("", "Radiohead"), 0);
    }
}
