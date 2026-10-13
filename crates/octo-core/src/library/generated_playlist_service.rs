//! The pure half of the generated playlist service: the mix record, the state file's shape
//! (`generated-playlists.json`), and the rules the tests pin: which mixes exist (hysteresis over
//! counts), what a mix holds for a period (a seeded draw with an artist cap and a share for new
//! songs), and the Discovery blend. The service that asks Navidrome is
//! `octo::services::library::generated_playlist_service`.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, TimeDelta, Utc};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::common::SongIdentity;
use crate::common::dotnet;
use crate::common::dotnet_random::DotnetRandom;
use crate::json::datetime;
use crate::json::dom::Node;
use crate::last_fm::last_fm_radio_state_store::to_base62;
use crate::metadata::GenreNormalizer;
use crate::models::domain::Song;
use crate::models::null_as_default;
use crate::settings::{DiscoveryStationSettings, ExplicitFilter, GeneratedPlaylistSettings, IgnoreCaseSet};

pub const POOL_PAGE: i32 = 500;
pub const FIRST_DECADE: i32 = 1950;
pub const MAX_GENRE_PAGES: i32 = 4;
pub const BLEND_CANDIDATES: i32 = 200;

/// One genre or decade mix, as one listener sees it for the current draw. `kind` is "genre" or
/// "decade"; `key` is "genre:Rock" or "decade:1990".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeneratedPlaylist {
    pub id: String,
    pub key: String,
    pub kind: String,
    pub label: String,
    pub name: String,
    pub owner: String,
    pub pool_size: i32,
    pub period_start_utc: DateTime<Utc>,
    pub period_end_utc: DateTime<Utc>,
}

/// What decides which mixes a listener has, persisted so a restart keeps them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserMixes {
    #[serde(deserialize_with = "null_as_default")]
    pub active: Vec<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub counts: IndexMap<String, i32>,
    #[serde(with = "datetime::utc")]
    pub counts_utc: DateTime<Utc>,
    #[serde(deserialize_with = "null_as_default")]
    pub kinds: String,
    /// Each Made for you list's songs as Subsonic song JSON, by kind.
    #[serde(deserialize_with = "null_as_default")]
    pub for_you: IndexMap<String, Vec<String>>,
    /// Album covers New Releases' cover is colored from: outside albums Navidrome does not
    /// know, so they are fetched from the catalog's own image host.
    #[serde(deserialize_with = "null_as_default")]
    pub for_you_covers: Vec<String>,
}

impl Default for UserMixes {
    fn default() -> Self {
        UserMixes {
            active: Vec::new(),
            counts: IndexMap::new(),
            // The minimum date, written without a Z, as existing state files hold it.
            counts_utc: datetime::min_value(),
            kinds: String::new(),
            for_you: IndexMap::new(),
            for_you_covers: Vec::new(),
        }
    }
}

/// The file: `{"Users": {"<trimmed lower-cased user>": UserMixes}}`. `users` is None when the
/// file says `null`, which loading skips.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct StateDocument {
    pub users: Option<IndexMap<String, UserMixes>>,
}

/// The key a listener is filed under: trimmed and lower-cased.
pub fn user_key(username: &str) -> String {
    dotnet::to_lower_invariant(username.trim())
}

/// Ticks: 100 ns intervals since 0001-01-01.
pub fn dotnet_ticks(value: DateTime<Utc>) -> i64 {
    const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
    UNIX_EPOCH_TICKS + value.timestamp() * 10_000_000 + i64::from(value.timestamp_subsec_nanos() / 100)
}

/// Which kinds the counts and lists were made for: "genre,decade" for the mixes (each only when
/// the mixes are on), then the Made for you lists that are on
/// ("newReleases+rediscover+deepCuts"), and what decides their contents: New Releases' weeks,
/// artists and the server's explicit filter, Rediscover's months. A change to any of them makes
/// the lists again.
pub fn kinds_of(settings: &GeneratedPlaylistSettings, explicit_filter: ExplicitFilter) -> String {
    let mixes = mixes_on(settings);
    let for_you: Vec<&str> = crate::library::for_you_lists::KINDS
        .iter()
        .copied()
        .filter(|kind| for_you_on(settings, kind))
        .collect();
    let mut kinds = format!(
        "{},{},{}",
        if mixes && settings.genres { "genre" } else { "" },
        if mixes && settings.decades { "decade" } else { "" },
        for_you.join("+")
    );
    if settings.new_releases {
        // The filter by number, so kinds already saved in the state file still compare equal.
        let filter = match explicit_filter {
            ExplicitFilter::All => 0,
            ExplicitFilter::ExplicitOnly => 1,
            ExplicitFilter::CleanOnly => 2,
        };
        kinds.push_str(&format!(
            ",w{}a{}x{filter}",
            settings.effective_new_release_weeks(),
            settings.effective_new_release_artists()
        ));
    }
    if settings.rediscover {
        kinds.push_str(&format!(",m{}", settings.effective_rediscover_months()));
    }
    if mixes && settings.country_mixes {
        kinds.push_str(&format!(",country:{}", settings.country_codes().join("+")));
    }
    kinds
}

/// The mixes are on: the switch, and one of the kinds (genre, decade, country).
pub fn mixes_on(settings: &GeneratedPlaylistSettings) -> bool {
    settings.enabled && (settings.genres || settings.decades || settings.country_mixes)
}

/// The country mixes are on: the mixes switch and the country switch.
pub fn country_mixes_on(settings: &GeneratedPlaylistSettings) -> bool {
    settings.enabled && settings.country_mixes
}

/// The key of a country's mix: "country:ES".
pub fn country_key(code: &str) -> String {
    format!("{COUNTRY_PREFIX}{code}")
}

/// What starts a country mix's key.
pub const COUNTRY_PREFIX: &str = "country:";

/// Whether this Made for you list is switched on.
pub fn for_you_on(settings: &GeneratedPlaylistSettings, kind: &str) -> bool {
    use crate::library::for_you_lists::{DEEP_CUTS_KIND, NEW_RELEASES_KIND, REDISCOVER_KIND};
    match kind {
        NEW_RELEASES_KIND => settings.new_releases,
        REDISCOVER_KIND => settings.rediscover,
        DEEP_CUTS_KIND => settings.deep_cuts,
        _ => false,
    }
}

/// The counts are older than the refresh period, or were made for other kinds.
pub fn is_stale(
    state: &UserMixes,
    settings: &GeneratedPlaylistSettings,
    explicit_filter: ExplicitFilter,
    now_utc: DateTime<Utc>,
) -> bool {
    now_utc - state.counts_utc >= TimeDelta::hours(i64::from(settings.effective_refresh_hours()))
        || state.kinds != kinds_of(settings, explicit_filter)
}

/// ("decade", "1990s") for "decade:1990", ("genre", "Rock") for "genre:Rock", ("country",
/// "Spain") for "country:ES".
pub fn describe(key: &str) -> (String, String) {
    if let Some(code) = key.strip_prefix(COUNTRY_PREFIX) {
        let name = crate::common::countries::english_name(code).unwrap_or(code);
        return ("country".to_string(), name.to_string());
    }
    match key.strip_prefix("decade:") {
        Some(decade) => ("decade".to_string(), format!("{decade}s")),
        None => (
            "genre".to_string(),
            key.get("genre:".len()..).unwrap_or_default().to_string(),
        ),
    }
}

/// Genres by song count. Spellings that differ only in case are one genre, under the spelling
/// with the most songs; years and the genre blocklist are never a mix of their own.
pub fn parse_genres(rows: Option<&[Node]>, blocked: &IgnoreCaseSet) -> IndexMap<String, i32> {
    let spellings = rows
        .unwrap_or_default()
        .iter()
        .filter(|row| row.is_object())
        .map(|row| {
            (
                str_of(row, "value").unwrap_or_default().trim().to_string(),
                int_of(row, "songCount"),
            )
        })
        .filter(|(value, count)| {
            !value.is_empty()
                && *count > 0
                && !GenreNormalizer::is_year_like(value)
                && !blocked.contains(value)
                && !blocked.contains(&DiscoveryStationSettings::normalize_tag(value))
        });
    // Grouped ignoring case, in first-seen order.
    let mut groups: IndexMap<String, Vec<(String, i32)>> = IndexMap::new();
    for (value, count) in spellings {
        groups
            .entry(dotnet::ordinal_ignore_case_key(&value))
            .or_default()
            .push((value, count));
    }
    let mut counts = IndexMap::new();
    for mut genre in groups.into_values() {
        genre.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| compare_ordinal(&a.0, &b.0)));
        let (value, count) = genre.swap_remove(0);
        counts.insert(format!("genre:{value}"), count);
    }
    counts
}

/// Keep a mix while it has at least `remove_below` tracks, add one once it reaches `create_at`,
/// and show the largest first.
pub fn apply_hysteresis(
    counts: &IndexMap<String, i32>,
    previously_active: &[String],
    create_at: i32,
    remove_below: i32,
    max: i32,
) -> Vec<String> {
    let mut kept: Vec<(&String, i32)> = counts
        .iter()
        .filter(|(key, value)| {
            if previously_active.contains(key) {
                **value >= remove_below
            } else {
                **value >= create_at
            }
        })
        .map(|(key, value)| (key, *value))
        .collect();
    kept.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| compare_ordinal(a.0, b.0)));
    kept.into_iter()
        .take(max.max(0) as usize)
        .map(|(key, _)| key.clone())
        .collect()
}

/// Which mixes a listener has: genres and decades by [`apply_hysteresis`] up to MaxPlaylists,
/// then countries by the same thresholds up to MaxCountryPlaylists, so a country never takes a
/// genre's place.
pub fn active_mixes(
    counts: &IndexMap<String, i32>,
    previously_active: &[String],
    settings: &GeneratedPlaylistSettings,
) -> Vec<String> {
    let (countries, others): (IndexMap<String, i32>, IndexMap<String, i32>) = counts
        .iter()
        .map(|(key, count)| (key.clone(), *count))
        .partition(|(key, _)| key.starts_with(COUNTRY_PREFIX));
    let create_at = settings.effective_create_at();
    let remove_below = settings.effective_remove_below();
    let mut active = apply_hysteresis(
        &others,
        previously_active,
        create_at,
        remove_below,
        settings.effective_max_playlists(),
    );
    if !countries.is_empty() {
        active.extend(apply_hysteresis(
            &countries,
            previously_active,
            create_at,
            remove_below,
            settings.effective_max_country_playlists(),
        ));
    }
    active
}

/// Songs per country mix: each song counted under its artist's country when that is known
/// and, when `allowed` names any, one of them.
pub fn count_countries<'a>(
    countries: impl IntoIterator<Item = Option<&'a str>>,
    allowed: &[&str],
) -> IndexMap<String, i32> {
    let mut counts: IndexMap<String, i32> = IndexMap::new();
    for code in countries.into_iter().flatten() {
        if code.is_empty() || (!allowed.is_empty() && !allowed.contains(&code)) {
            continue;
        }
        *counts.entry(country_key(code)).or_default() += 1;
    }
    counts
}

/// A shuffled draw of up to `count` songs, never more than `max_per_artist` by one artist, with
/// `new_share` percent kept for songs new to the listener when there are enough. The cap is
/// never relaxed: a mix that cannot be filled without breaking it is shorter.
pub fn select(
    pool: &[Node],
    count: i32,
    max_per_artist: i32,
    new_share: i32,
    new_days: i32,
    now_utc: DateTime<Utc>,
    seed: i32,
) -> Vec<Node> {
    let mut shuffled: Vec<&Node> = pool.iter().collect();
    let mut random = DotnetRandom::new(seed);
    for i in (1..shuffled.len()).rev() {
        let j = random.next(i as i32 + 1) as usize;
        shuffled.swap(i, j);
    }

    let mut taken: BTreeSet<usize> = BTreeSet::new();
    let mut per_artist: HashMap<String, i32> = HashMap::new();
    let mut try_take = |index: usize, taken: &mut BTreeSet<usize>| {
        let artist = artist_key(shuffled[index]);
        let already = per_artist.get(&artist).copied().unwrap_or(0);
        if already >= max_per_artist {
            return;
        }
        per_artist.insert(artist, already + 1);
        taken.insert(index);
    };

    let quota = (f64::from(count) * f64::from(new_share) / 100.0).round() as i32;
    let mut i = 0;
    while i < shuffled.len() && (taken.len() as i32) < quota.min(count) {
        if is_new(shuffled[i], now_utc, new_days) {
            try_take(i, &mut taken);
        }
        i += 1;
    }
    let mut i = 0;
    while i < shuffled.len() && (taken.len() as i32) < count {
        if !taken.contains(&i) {
            try_take(i, &mut taken);
        }
        i += 1;
    }
    taken.into_iter().map(|index| shuffled[index].clone()).collect()
}

/// Never played by this listener, or added to the library in the last `new_days`.
pub fn is_new(song: &Node, now_utc: DateTime<Utc>, new_days: i32) -> bool {
    if int_of(song, "playCount") == 0 {
        return true;
    }
    str_of(song, "created")
        .and_then(datetime::parse_utc)
        .is_some_and(|at| now_utc - at <= TimeDelta::days(i64::from(new_days)))
}

fn artist_key(song: &Node) -> String {
    match str_of(song, "artistId") {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => SongIdentity::key(str_of(song, "artist").unwrap_or_default()),
    }
}

/// Put up to `wanted` new library songs into the station, spread from the end so the familiar
/// opening is untouched, never repeating a song already in it. None when nothing was put in:
/// the station stays as it was.
pub fn blend(
    songs: &[Song],
    candidates: &[Node],
    wanted: i32,
    new_days: i32,
    now_utc: DateTime<Utc>,
) -> Option<Vec<Song>> {
    let mut present: std::collections::HashSet<String> = songs
        .iter()
        .map(|song| song_key(Some(&song.artist), Some(&song.title)))
        .collect();
    let mut picks: Vec<Song> = Vec::new();
    for candidate in candidates {
        if picks.len() as i32 >= wanted {
            break;
        }
        let Some(id) = str_of(candidate, "id").filter(|id| !id.is_empty()) else {
            continue;
        };
        if !is_new(candidate, now_utc, new_days) {
            continue;
        }
        if !present.insert(song_key(str_of(candidate, "artist"), str_of(candidate, "title"))) {
            continue;
        }
        picks.push(Song {
            id: id.to_string(),
            title: str_of(candidate, "title").unwrap_or_default().to_string(),
            artist: str_of(candidate, "artist").unwrap_or_default().to_string(),
            artist_id: str_of(candidate, "artistId").map(str::to_string),
            album: str_of(candidate, "album").unwrap_or_default().to_string(),
            album_id: str_of(candidate, "albumId").map(str::to_string),
            duration: nullable_int_of(candidate, "duration"),
            year: nullable_int_of(candidate, "year"),
            track: nullable_int_of(candidate, "track"),
            genre: str_of(candidate, "genre").map(str::to_string),
            suffix: str_of(candidate, "suffix").map(str::to_string),
            bit_rate: nullable_int_of(candidate, "bitRate"),
            isrcs: texts_of(candidate, "isrc"),
            is_local: true,
            ..Default::default()
        });
    }
    if picks.is_empty() {
        return None;
    }

    let mut result = songs.to_vec();
    let step = 1.max(songs.len() / picks.len());
    for (i, pick) in picks.into_iter().enumerate() {
        result[songs.len() - 1 - i * step] = pick;
    }
    Some(result)
}

fn song_key(artist: Option<&str>, title: Option<&str>) -> String {
    SongIdentity::match_key(artist.unwrap_or_default(), title.unwrap_or_default())
}

/// The mix's id: "og" and 20 base-62 characters of SHA-256 over the listener and the mix key.
pub fn playlist_id(username: &str, key: &str) -> String {
    let hash = Sha256::digest(format!("{}|{key}", user_key(username)).as_bytes());
    format!("og{}", to_base62(&hash, 20))
}

/// Which refresh period `now_utc` falls in, counted from the Unix epoch.
pub fn period_index(now_utc: DateTime<Utc>, hours: i32) -> i64 {
    let ticks = dotnet_ticks(now_utc) - dotnet_ticks(DateTime::UNIX_EPOCH);
    let total_hours = ticks as f64 / 36_000_000_000.0;
    (total_hours / f64::from(hours)).floor() as i64
}

/// The draw's seed for one listener, mix and period: the first four bytes of a SHA-256.
pub fn seed(username: &str, key: &str, period: i64) -> i32 {
    let hash = Sha256::digest(format!("{}|{key}|{period}", user_key(username)).as_bytes());
    i32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]])
}

/// The property when it is a JSON string, else None.
pub fn str_of<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    node.get(name).and_then(Node::as_str)
}

/// A number read as an int, 0 when absent or not a number.
pub fn int_of(node: &Node, name: &str) -> i32 {
    nullable_int_of(node, name).unwrap_or(0)
}

/// A number read as an int: an int as it is, a wider integer clamped, anything else with a
/// fraction or an exponent truncated; None when absent or not a number.
pub fn nullable_int_of(node: &Node, name: &str) -> Option<i32> {
    let Some(Node::Number(text)) = node.get(name) else {
        return None;
    };
    if let Ok(number) = text.parse::<i32>() {
        return Some(number);
    }
    if let Ok(wide) = text.parse::<i64>() {
        return Some(wide.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
    }
    text.parse::<f64>().ok().map(|real| real as i32)
}

/// A list of text as Navidrome sent it, OpenSubsonic's `isrc`; empty when absent.
pub fn texts_of(node: &Node, name: &str) -> Vec<String> {
    match node.get(name) {
        Some(Node::Array(values)) => values
            .iter()
            .filter_map(Node::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Ordinal string order: UTF-16 code units.
pub fn compare_ordinal(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

#[cfg(test)]
#[path = "generated_playlist_service_tests.rs"]
mod tests;
