//! Made for you: the picking behind New Releases, Rediscover and Deep Cuts. Everything here
//! reads one walk of a listener's library, as that listener sees it, so each song carries their
//! own playCount, played (last played), starred and userRating. Pure, so it is tested without a
//! server; the walk and the catalog calls live in the `octo` crate's
//! `services::library::generated_playlist_service` and `services::library::new_releases_builder`.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Months, NaiveDate, NaiveDateTime, TimeDelta, Utc};
use indexmap::IndexMap;

use super::generated_playlist_service::{compare_ordinal, int_of, str_of};
use crate::common::SongIdentity;
use crate::common::dotnet::to_lower_invariant;
use crate::common::dotnet_random::DotnetRandom;
use crate::json::dom::Node;
use crate::metadata::deezer_metadata_service::{AlbumHit, AlbumTrack};
use crate::settings::ExplicitFilter;

pub const NEW_RELEASES_KIND: &str = "newReleases";
pub const REDISCOVER_KIND: &str = "rediscover";
pub const DEEP_CUTS_KIND: &str = "deepCuts";

/// Liked Songs: every song the listener hearted, newest first. Read from Navidrome whenever it
/// is opened, never drawn, so it is not one of [`KINDS`].
pub const LIKED_KIND: &str = "liked";

/// The three, in the order they are listed: newest first, then what is old to you.
pub const KINDS: [&str; 3] = [NEW_RELEASES_KIND, REDISCOVER_KIND, DEEP_CUTS_KIND];

pub const TRACK_COUNT: usize = 50;
pub const REDISCOVER_PER_ARTIST: i32 = 3;
pub const DEEP_CUTS_PER_ARTIST: i32 = 4;
pub const DEEP_CUT_ARTISTS: usize = 15;
pub const SONGS_PER_RELEASE: usize = 3;
const REDISCOVER_POOL: usize = 150;
const LOVED_PLAYS: i32 = 3;
const LOVED_RATING: i32 = 4;

pub fn name(kind: &str) -> &str {
    match kind {
        NEW_RELEASES_KIND => "New Releases",
        REDISCOVER_KIND => "Rediscover",
        DEEP_CUTS_KIND => "Deep Cuts",
        _ => kind,
    }
}

pub fn is_for_you(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// An artist of the library by how much the listener plays them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopArtist {
    pub key: String,
    pub name: String,
    pub plays: i32,
    pub hearts: i32,
    pub songs: i32,
}

impl TopArtist {
    pub fn new(key: &str, name: &str, plays: i32, hearts: i32, songs: i32) -> Self {
        Self {
            key: key.to_string(),
            name: name.to_string(),
            plays,
            hearts,
            songs,
        }
    }
}

/// The listener's most-played artists: plays summed per artist. Someone with no plays yet is
/// read by their hearts, then by how much of each artist they keep. "Various Artists" is
/// nobody's favorite artist.
pub fn top_artists(songs: &[Node], count: usize) -> Vec<TopArtist> {
    let various = SongIdentity::key("Various Artists");
    // Grouped in the order each artist is first seen.
    let mut groups: IndexMap<String, Vec<&Node>> = IndexMap::new();
    for song in songs {
        groups.entry(artist_key(song)).or_default().push(song);
    }
    let artists: Vec<TopArtist> = groups
        .into_iter()
        .map(|(key, group)| {
            // The name the group's songs use most, the first in ordinal order of equals.
            let mut names: IndexMap<&str, usize> = IndexMap::new();
            for name in group
                .iter()
                .map(|song| str_of(song, "artist").unwrap_or(""))
                .filter(|name| !name.is_empty())
            {
                *names.entry(name).or_default() += 1;
            }
            let mut ranked: Vec<(&str, usize)> = names.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| compare_ordinal(a.0, b.0)));
            TopArtist {
                key,
                name: ranked
                    .first()
                    .map(|(name, _)| name.to_string())
                    .unwrap_or_default(),
                plays: group
                    .iter()
                    .fold(0i32, |sum, song| sum.wrapping_add(int_of(song, "playCount"))),
                hearts: group.iter().filter(|song| is_starred(song)).count() as i32,
                songs: group.len() as i32,
            }
        })
        .filter(|artist| !artist.name.is_empty() && SongIdentity::key(&artist.name) != various)
        .collect();
    // Only artists the listener played or hearted are theirs; an artist they never touched is
    // never one of "your artists", however few they have. Someone with no plays or hearts at
    // all gets the library's biggest artists, so a new listener still has lists.
    let theirs: Vec<TopArtist> = artists
        .iter()
        .filter(|artist| artist.plays > 0 || artist.hearts > 0)
        .cloned()
        .collect();
    let mut ranked = if theirs.is_empty() { artists } else { theirs };
    let by_plays = ranked.iter().any(|artist| artist.plays > 0 || artist.hearts > 0);
    ranked.sort_by(|a, b| {
        let first = if by_plays {
            b.plays
                .cmp(&a.plays)
                .then_with(|| b.hearts.cmp(&a.hearts))
                .then_with(|| b.songs.cmp(&a.songs))
        } else {
            b.songs.cmp(&a.songs)
        };
        first.then_with(|| compare_ordinal(&a.name, &b.name))
    });
    ranked.truncate(count);
    ranked
}

/// Played often, hearted, or rated 4 or more.
pub fn loved(song: &Node) -> bool {
    int_of(song, "playCount") >= LOVED_PLAYS || is_starred(song) || int_of(song, "userRating") >= LOVED_RATING
}

/// Songs the listener loved and has not played for `months`: the best loved first, then a
/// seeded draw from them so the list changes from day to day. A song with no last-played date
/// says nothing about when, so it is left out.
pub fn rediscover(songs: &[Node], now_utc: DateTime<Utc>, months: i32, seed: i32) -> Vec<Node> {
    let before = now_utc
        .checked_sub_months(Months::new(months.max(0) as u32))
        .unwrap_or(DateTime::<Utc>::MIN_UTC);
    let mut pool: Vec<&Node> = songs
        .iter()
        .filter(|song| loved(song) && played(song).is_some_and(|at| at < before))
        .collect();
    pool.sort_by(|a, b| {
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(Ordering::Equal)
            .then_with(|| compare_ordinal(str_of(a, "id").unwrap_or(""), str_of(b, "id").unwrap_or("")))
    });
    pool.truncate(REDISCOVER_POOL);
    draw(&pool, TRACK_COUNT, REDISCOVER_PER_ARTIST, seed)
}

/// Songs played at most once, from the listener's most-played artists, none of them in
/// Rediscover: the corners of artists they already like.
pub fn deep_cuts(
    songs: &[Node],
    artist_keys: &[String],
    excluded_ids: &HashSet<String>,
    seed: i32,
) -> Vec<Node> {
    let artists: HashSet<&str> = artist_keys.iter().map(String::as_str).collect();
    let mut pool: Vec<&Node> = songs
        .iter()
        .filter(|song| {
            artists.contains(artist_key(song).as_str())
                && int_of(song, "playCount") <= 1
                && str_of(song, "id").is_some_and(|id| !excluded_ids.contains(id))
        })
        .collect();
    pool.sort_by(|a, b| compare_ordinal(str_of(a, "id").unwrap_or(""), str_of(b, "id").unwrap_or("")));
    draw(&pool, TRACK_COUNT, DEEP_CUTS_PER_ARTIST, seed)
}

/// An artist's releases out in the last `weeks` weeks, newest first: albums, EPs and singles,
/// never compilations, never a date still to come, never one with no date.
pub fn recent_releases<'a>(
    releases: impl IntoIterator<Item = &'a AlbumHit>,
    today: NaiveDate,
    weeks: i32,
) -> Vec<AlbumHit> {
    let since = today - TimeDelta::days(7 * i64::from(weeks));
    let mut recent: Vec<(&AlbumHit, NaiveDate)> = releases
        .into_iter()
        .filter_map(|release| release_date(release).map(|date| (release, date)))
        .filter(|(release, date)| {
            *date >= since
                && *date <= today
                && matches!(
                    release.record_type.as_deref().map(to_lower_invariant).as_deref(),
                    Some("album" | "ep" | "single")
                )
        })
        .collect();
    recent.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| compare_ordinal(&a.0.title, &b.0.title))
    });
    recent.into_iter().map(|(release, _)| release.clone()).collect()
}

pub fn release_date(release: &AlbumHit) -> Option<NaiveDate> {
    let text = release.release_date.as_deref()?;
    let head: String = text.chars().take(10).collect();
    if head.chars().count() < 10 {
        return None;
    }
    NaiveDate::parse_from_str(&head, "%Y-%m-%d").ok()
}

/// The songs a release brings to New Releases: a single's or an EP's first few, an album's best
/// by the catalog's popularity, in album order when it gives none.
pub fn pick_tracks(record_type: Option<&str>, tracks: &[AlbumTrack]) -> Vec<AlbumTrack> {
    let mut in_order: Vec<&AlbumTrack> = tracks.iter().collect();
    in_order.sort_by(|a, b| {
        a.disc_number
            .unwrap_or(1)
            .cmp(&b.disc_number.unwrap_or(1))
            .then_with(|| {
                a.track_position
                    .unwrap_or(i32::MAX)
                    .cmp(&b.track_position.unwrap_or(i32::MAX))
            })
    });
    let is_album = record_type.map(to_lower_invariant).as_deref() == Some("album");
    if !is_album || in_order.iter().all(|track| track.rank.is_none()) {
        return in_order.into_iter().take(SONGS_PER_RELEASE).cloned().collect();
    }
    let mut best: Vec<(usize, &AlbumTrack)> = in_order.into_iter().enumerate().collect();
    best.sort_by(|a, b| {
        b.1.rank
            .unwrap_or(-1)
            .cmp(&a.1.rank.unwrap_or(-1))
            .then_with(|| a.0.cmp(&b.0))
    });
    best.truncate(SONGS_PER_RELEASE);
    best.sort_by_key(|(index, _)| *index);
    best.into_iter().map(|(_, track)| track.clone()).collect()
}

/// When the listener last played the song, from Navidrome's `played`.
pub fn played(song: &Node) -> Option<DateTime<Utc>> {
    parse_instant(str_of(song, "played")?)
}

/// An instant in UTC, read from the shapes Navidrome writes: an ISO 8601 instant with its zone,
/// or one without (the server's own time, which in the container is UTC).
fn parse_instant(text: &str) -> Option<DateTime<Utc>> {
    if let Ok(at) = DateTime::parse_from_rfc3339(text.trim()) {
        return Some(at.with_timezone(&Utc));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d",
    ] {
        if let Ok(at) = NaiveDateTime::parse_from_str(text.trim(), format) {
            return Some(at.and_utc());
        }
        if format == "%Y-%m-%d"
            && let Ok(day) = NaiveDate::parse_from_str(text.trim(), format)
        {
            return day.and_hms_opt(0, 0, 0).map(|at| at.and_utc());
        }
    }
    None
}

fn score(song: &Node) -> f64 {
    f64::from(int_of(song, "playCount"))
        + if is_starred(song) { 5.0 } else { 0.0 }
        + 2.0 * f64::from(int_of(song, "userRating"))
}

/// A seeded shuffle, never more than `max_per_artist` by one artist.
pub fn draw(pool: &[&Node], count: usize, max_per_artist: i32, seed: i32) -> Vec<Node> {
    let mut shuffled: Vec<&Node> = pool.to_vec();
    let mut random = DotnetRandom::new(seed);
    for i in (1..shuffled.len()).rev() {
        let j = random.next(i as i32 + 1) as usize;
        shuffled.swap(i, j);
    }
    let mut per_artist: HashMap<String, i32> = HashMap::new();
    let mut taken = Vec::new();
    for song in shuffled {
        if taken.len() >= count {
            break;
        }
        let artist = artist_key(song);
        let already = per_artist.get(&artist).copied().unwrap_or(0);
        if already >= max_per_artist {
            continue;
        }
        per_artist.insert(artist, already + 1);
        taken.push(song.clone());
    }
    taken
}

/// The listener's artist a song counts for: its artist id, else its artist's name.
pub fn artist_key(song: &Node) -> String {
    match str_of(song, "artistId") {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => SongIdentity::key(str_of(song, "artist").unwrap_or_default()),
    }
}

fn is_starred(song: &Node) -> bool {
    str_of(song, "starred").is_some_and(|text| !text.is_empty())
}

/// Whether the server's explicit filter lets an outside song in, as search and radio apply it.
pub fn explicit_allowed(explicit_content_lyrics: Option<i32>, filter: ExplicitFilter) -> bool {
    match filter {
        ExplicitFilter::CleanOnly => explicit_content_lyrics != Some(1),
        ExplicitFilter::ExplicitOnly => explicit_content_lyrics != Some(3),
        _ => true,
    }
}

#[cfg(test)]
#[path = "for_you_lists_tests.rs"]
mod tests;
