//! How radio weighs its sources' answers. Each song scores weight x match x rank decay, the
//! stations' shape ([`RANK_HALF_DEPTH`]), and the same song from several sources adds up, since
//! agreement is the best sign a suggestion fits.

use std::cmp::Ordering;
use std::collections::HashMap;

use indexmap::IndexMap;

use super::radio_provider;
use super::radio_types::{RadioAnswer, RadioMatch};
use crate::common::dotnet;
use crate::last_fm::SimilarTrack;
use crate::last_fm::last_fm_radio_recommendation_service::IgnoreCaseKeys;
use crate::last_fm::last_fm_radio_seed_normalizer as seed_normalizer;
use crate::last_fm::radio_filler;
use crate::models::domain::Song;

/// What each source counts, by provider key, in the order they were listed.
pub type RadioWeights = IndexMap<String, f64>;

/// A song the blend kept, and the source that counted most for it.
///
/// `needs_reading`: the library song is a stub to read from Navidrome first.
#[derive(Debug, Clone)]
pub struct RadioPick {
    pub track: Option<SimilarTrack>,
    pub library_song: Option<Song>,
    pub provider: String,
    pub score: f64,
    pub needs_reading: bool,
}

pub const RANK_HALF_DEPTH: i32 = 20;
/// What a guess from similar artists counts, when nothing better came back.
pub const GUESS_FACTOR: f64 = 0.3;

pub fn match_factor(r#match: RadioMatch) -> f64 {
    match r#match {
        RadioMatch::Song => 1.0,
        RadioMatch::Genre => 0.8,
        RadioMatch::Original => 0.6,
        RadioMatch::ArtistsTrusted => 0.5,
        _ => 0.0,
    }
}

/// Songs a catalog answer needs before it counts in full.
pub const FULL_ANSWER: i32 = 20;
/// What Sounds alike counts at least when no catalog knows the song: then the sound is one of
/// the few signs of its style.
pub const SOUNDS_ALIKE_WHEN_UNKNOWN: f64 = 0.8;

/// A catalog answer of five songs is a weaker sign than one of fifty. Library answers (an album
/// of three, say) are what they are and count in full.
///
/// `wanted`: how many songs the radio asked for: an answer that has them all is full however
/// short, so a radio of eight songs is not thin for having eight. ([`FULL_ANSWER`] when no
/// count was asked for.)
pub fn thin(answer: &RadioAnswer, wanted: i32) -> f64 {
    if radio_provider::is_catalog(&answer.provider) {
        (answer.count() as f64 / f64::from(wanted.clamp(1, FULL_ANSWER))).min(1.0)
    } else {
        1.0
    }
}

/// Stations always had similar artists to fall back on, guesses included, so they keep them at a
/// discount.
pub fn station_factor(r#match: RadioMatch) -> f64 {
    if r#match == RadioMatch::ArtistsGuessed {
        GUESS_FACTOR
    } else {
        match_factor(r#match)
    }
}

/// The catalogs found fewer than [`FULL_ANSWER`] songs like this very song, so the seed's own
/// album and genre join and lead, and no spacing by artist is applied (#78). A thin exact answer
/// counts as none: three YouTube Music rows must not push a playlist's own songs out of the
/// radio. (`wanted` is [`FULL_ANSWER`] when no count was asked for.)
pub fn library_led<'a>(answers: impl IntoIterator<Item = &'a RadioAnswer>, wanted: i32) -> bool {
    let exact: usize = answers
        .into_iter()
        .filter(|answer| radio_provider::is_catalog(&answer.provider) && answer.r#match == RadioMatch::Song)
        .map(RadioAnswer::count)
        .sum();
    (exact as i64) < i64::from(wanted.clamp(1, FULL_ANSWER))
}

/// Filler (intros, skits, interviews, extreme lengths) and the listener's one-star songs, out.
pub fn without_filler_or_bans(answer: RadioAnswer, bans: &IgnoreCaseKeys) -> RadioAnswer {
    let kept = |artist: &str, title: &str, duration: Option<i32>| {
        !radio_filler::is_filler(Some(title), duration)
            && !bans.contains(&seed_normalizer::track_key(artist, title))
    };
    RadioAnswer {
        tracks: answer
            .tracks
            .into_iter()
            .filter(|track| kept(&track.artist, &track.title, track.duration))
            .collect(),
        library_songs: answer
            .library_songs
            .into_iter()
            .filter(|song| kept(&song.artist, &song.title, song.duration))
            .collect(),
        ..answer
    }
}

/// `wanted`: how many songs the radio is for ([`thin`]; [`FULL_ANSWER`] when no count was asked
/// for).
pub fn blend(
    answers: &[RadioAnswer],
    weights: &RadioWeights,
    count: usize,
    seed_key: &str,
    seed_id: Option<&str>,
    wanted: i32,
) -> Vec<RadioPick> {
    let mut picks = score(answers, weights, seed_key, seed_id, false, wanted);
    if picks.is_empty() {
        picks = score(answers, weights, seed_key, seed_id, true, wanted);
    }
    picks.truncate(count);
    picks
}

struct Entry {
    order: usize,
    score: f64,
    best: f64,
    provider: String,
    song: Option<Song>,
    song_complete: bool,
    track: Option<SimilarTrack>,
}

fn score(
    answers: &[RadioAnswer],
    weights: &RadioWeights,
    seed_key: &str,
    seed_id: Option<&str>,
    guesses: bool,
    wanted: i32,
) -> Vec<RadioPick> {
    let mut by_key: HashMap<String, Entry> = HashMap::new();
    for answer in answers {
        let r#match = if guesses && answer.r#match == RadioMatch::ArtistsGuessed {
            GUESS_FACTOR
        } else {
            match_factor(answer.r#match)
        };
        let factor = r#match * thin(answer, wanted) * weights.get(&answer.provider).copied().unwrap_or(1.0);
        if factor <= 0.0 {
            continue;
        }
        for (rank, (artist, title, song, track)) in answer.items().enumerate() {
            let term = factor / (1.0 + rank as f64 / f64::from(RANK_HALF_DEPTH));
            let key = seed_normalizer::track_key(artist, title);
            if key.is_empty() || key == seed_key || song.is_some_and(|song| Some(song.id.as_str()) == seed_id)
            {
                continue;
            }
            let order = by_key.len();
            let entry = by_key.entry(key).or_insert_with(|| Entry {
                order,
                score: 0.0,
                best: 0.0,
                provider: String::new(),
                song: None,
                song_complete: false,
                track: None,
            });
            entry.score += term;
            if term > entry.best {
                entry.best = term;
                entry.provider = answer.provider.clone();
            }
            // A library copy wins over a track to look up: it needs no resolving. A complete copy
            // wins over a stub, which would have to be read again.
            if let Some(song) = song
                && (entry.song.is_none() || (!entry.song_complete && answer.library_songs_complete))
            {
                entry.song = Some(song.clone());
                entry.song_complete = answer.library_songs_complete;
            }
            match (&mut entry.track, track) {
                (None, track) => entry.track = track.cloned(),
                // The same song from a source that knows its video (YouTube Music) keeps the
                // video, whichever source answered first, so it plays what that source named.
                (Some(kept), Some(track)) if kept.you_tube_id.is_none() && track.you_tube_id.is_some() => {
                    kept.you_tube_id = track.you_tube_id.clone();
                    kept.duration = kept.duration.or(track.duration);
                }
                _ => {}
            }
        }
    }
    let mut entries: Vec<Entry> = by_key.into_values().collect();
    entries.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.order.cmp(&b.order))
    });
    entries
        .into_iter()
        .map(|entry| RadioPick {
            needs_reading: entry.song.is_some() && !entry.song_complete,
            track: entry.track,
            library_song: entry.song,
            provider: entry.provider,
            score: entry.score,
        })
        .collect()
}

/// The first of a song's genres, the one a tag lookup can use; None for none or a placeholder.
pub fn primary_genre(genre: Option<&str>) -> Option<String> {
    let first = genre
        .unwrap_or("")
        .split([';', '/', '|', ','])
        .map(str::trim)
        .find(|part| !part.is_empty())?;
    if dotnet::eq_ignore_case(first, "Unknown") || dotnet::eq_ignore_case(first, "Other") {
        None
    } else {
        Some(first.to_string())
    }
}

#[cfg(test)]
#[path = "radio_blend_tests.rs"]
mod tests;
