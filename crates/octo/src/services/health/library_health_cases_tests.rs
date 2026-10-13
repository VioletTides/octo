//! Every case in docs/library-health-cases.json, the Octo app's Library health tests written as
//! data, so the server and the app are checked against the same songs and the same answers. The
//! songs are Subsonic songs, read here the way the app's SubsonicHealth reads them.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::health_fixes::{self as fixes, fix_actions};
use super::health_words as words;
use super::library_health::{
    self as health, DuplicateSet, HealthCheck, HealthTag, IHealthFields, SplitAlbum,
};
use super::server_song_fields::ServerSongFields;

fn cases() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/library-health-cases.json"
    );
    let text = std::fs::read_to_string(path).expect("the shared library health cases");
    serde_json::from_str(&text).expect("the cases are JSON")
}

#[test]
fn every_app_test_is_a_case() {
    assert!(cases()["cases"].as_array().map_or(0, Vec::len) >= 55);
}

/// A song as the app's Song type holds it, from a Subsonic answer.
#[derive(Debug, Clone, PartialEq)]
struct CaseSong {
    id: String,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    album_id: Option<String>,
    display_album_artist: Option<String>,
    album_artist_names: Vec<String>,
    artist_names: Vec<String>,
    genre: Option<String>,
    genre_names: Vec<String>,
    year: Option<i32>,
    disc: Option<i32>,
    track: Option<i32>,
    duration: i32,
    suffix: Option<String>,
    bit_rate: Option<i32>,
    bit_depth: Option<i32>,
    sampling_rate: Option<i32>,
    isrc: Vec<String>,
    music_brainz_id: Option<String>,
    cover_art: Option<String>,
}

fn str_of(e: &Value, name: &str) -> Option<String> {
    e.get(name).and_then(Value::as_str).map(str::to_string)
}

fn int_of(e: &Value, name: &str) -> Option<i32> {
    e.get(name).and_then(Value::as_i64).map(|n| n as i32)
}

fn names(e: &Value, name: &str) -> Vec<String> {
    match e.get(name) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                _ => str_of(item, "name").unwrap_or_default(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn song(e: &Value) -> CaseSong {
    CaseSong {
        id: str_of(e, "id").expect("an id"),
        title: str_of(e, "title").unwrap_or_default(),
        artist: str_of(e, "artist"),
        album: str_of(e, "album"),
        album_id: str_of(e, "albumId"),
        display_album_artist: str_of(e, "displayAlbumArtist"),
        album_artist_names: names(e, "albumArtists"),
        artist_names: names(e, "artists"),
        genre: str_of(e, "genre"),
        genre_names: names(e, "genres"),
        year: int_of(e, "year"),
        disc: int_of(e, "discNumber"),
        track: int_of(e, "track"),
        duration: int_of(e, "duration").unwrap_or(0),
        suffix: str_of(e, "suffix"),
        bit_rate: int_of(e, "bitRate"),
        bit_depth: int_of(e, "bitDepth"),
        sampling_rate: int_of(e, "samplingRate"),
        isrc: names(e, "isrc"),
        music_brainz_id: str_of(e, "musicBrainzId"),
        cover_art: str_of(e, "coverArt"),
    }
}

fn blank(value: &str) -> bool {
    value.trim().is_empty()
}

/// The app's SubsonicHealth and SubsonicAlbumHealth, with a case's facts and places.
struct SubsonicFields {
    facts: Option<HashMap<String, HashMap<String, String>>>,
    albums: Vec<Value>,
    places: HashMap<String, String>,
    seen: HashSet<HealthTag>,
}

impl SubsonicFields {
    fn new(case: &Value) -> Self {
        let facts = case.get("facts").and_then(Value::as_object).map(|facts| {
            facts
                .iter()
                .map(|(kind, map)| {
                    let values = map
                        .as_object()
                        .map(|map| {
                            map.iter()
                                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or("").to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    (kind.clone(), values)
                })
                .collect()
        });
        let places = case
            .get("places")
            .and_then(Value::as_object)
            .map(|places| {
                places
                    .iter()
                    .map(|(id, place)| (id.clone(), place.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let seen = match case.get("seen").and_then(Value::as_array) {
            Some(seen) => seen
                .iter()
                .map(|tag| HealthTag::parse(tag.as_str().unwrap_or("")).expect("a tag"))
                .collect(),
            None => HealthTag::ALL.into_iter().collect(),
        };
        SubsonicFields {
            facts,
            albums: case
                .get("albums")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            places,
            seen,
        }
    }

    fn fact<'a>(&'a self, kind: &str, song: &CaseSong) -> Option<&'a str> {
        self.facts
            .as_ref()?
            .get(kind)?
            .get(song.album_id.as_deref().unwrap_or(""))
            .map(String::as_str)
    }

    fn album_of(&self, song: &CaseSong) -> Option<&Value> {
        self.albums
            .iter()
            .find(|album| album.get("id").and_then(Value::as_str) == song.album_id.as_deref())
    }
}

impl IHealthFields<CaseSong> for SubsonicFields {
    fn id<'a>(&'a self, song: &'a CaseSong) -> &'a str {
        &song.id
    }

    fn title<'a>(&'a self, song: &'a CaseSong) -> &'a str {
        &song.title
    }

    fn artist<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        song.artist.as_deref()
    }

    fn album<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        song.album.as_deref()
    }

    fn album_id<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        song.album_id.as_deref()
    }

    fn album_artist<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        match song.display_album_artist.as_deref() {
            Some(name) if !blank(name) => Some(name),
            _ => song.album_artist_names.first().map(String::as_str),
        }
    }

    fn genres<'a>(&'a self, song: &'a CaseSong) -> Vec<&'a str> {
        if !song.genre_names.is_empty() {
            return song.genre_names.iter().map(String::as_str).collect();
        }
        song.genre
            .as_deref()
            .filter(|genre| !blank(genre))
            .into_iter()
            .collect()
    }

    fn year(&self, song: &CaseSong) -> Option<i32> {
        song.year
    }

    fn disc(&self, song: &CaseSong) -> Option<i32> {
        song.disc
    }

    fn track(&self, song: &CaseSong) -> Option<i32> {
        song.track
    }

    fn seconds(&self, song: &CaseSong) -> i32 {
        song.duration
    }

    fn format(&self, song: &CaseSong) -> Option<String> {
        song.suffix
            .as_deref()
            .filter(|suffix| !blank(suffix))
            .map(octo_core::common::dotnet::to_lower_invariant)
    }

    fn lossless(&self, song: &CaseSong) -> bool {
        ServerSongFields::is_lossless(song.suffix.as_deref(), song.bit_depth)
    }

    fn bit_rate(&self, song: &CaseSong) -> Option<i32> {
        song.bit_rate
    }

    fn bit_depth(&self, song: &CaseSong) -> Option<i32> {
        song.bit_depth
    }

    fn sample_rate(&self, song: &CaseSong) -> Option<i32> {
        song.sampling_rate
    }

    fn isrcs<'a>(&'a self, song: &'a CaseSong) -> Vec<&'a str> {
        song.isrc.iter().map(String::as_str).collect()
    }

    fn recording_id<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        song.music_brainz_id.as_deref()
    }

    fn cover<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        song.cover_art.as_deref()
    }

    fn artists<'a>(&'a self, song: &'a CaseSong) -> Vec<&'a str> {
        let named: Vec<&str> = song
            .artist_names
            .iter()
            .map(String::as_str)
            .filter(|name| !blank(name))
            .collect();
        if named.is_empty() {
            self.artist(song).into_iter().collect()
        } else {
            named
        }
    }

    fn album_artists<'a>(&'a self, song: &'a CaseSong) -> Vec<&'a str> {
        let named: Vec<&str> = song
            .album_artist_names
            .iter()
            .map(String::as_str)
            .filter(|name| !blank(name))
            .collect();
        if named.is_empty() {
            self.album_artist(song).into_iter().collect()
        } else {
            named
        }
    }

    fn release_id<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        if self.facts.is_some() {
            return self.fact("releases", song);
        }
        self.album_of(song)?
            .get("musicBrainzId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    }

    fn release_group_id<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        self.fact("releaseGroups", song)
    }

    fn barcode<'a>(&'a self, song: &'a CaseSong) -> Option<&'a str> {
        self.fact("barcodes", song)
    }

    fn labels<'a>(&'a self, song: &'a CaseSong) -> Vec<&'a str> {
        if self.facts.is_some() {
            return self.fact("labels", song).into_iter().collect();
        }
        match self.album_of(song).and_then(|album| album.get("recordLabels")) {
            Some(Value::Array(labels)) => labels
                .iter()
                .filter_map(|label| label.get("name").and_then(Value::as_str))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn place<'a>(&'a self, song: &'a CaseSong) -> &'a str {
        self.places.get(&song.id).map_or("", String::as_str)
    }

    fn seen(&self, tag: HealthTag) -> bool {
        self.seen.contains(&tag)
    }
}

fn strings(array: &Value) -> Vec<String> {
    array
        .as_array()
        .expect("an array")
        .iter()
        .map(|item| item.as_str().expect("a string").to_string())
        .collect()
}

fn check_of(name: &str) -> HealthCheck {
    HealthCheck::parse(name).unwrap_or_else(|| panic!("a check: {name}"))
}

fn tag_of(name: &str) -> HealthTag {
    HealthTag::parse(name).unwrap_or_else(|| panic!("a tag: {name}"))
}

fn titles(songs: &[CaseSong]) -> Vec<String> {
    songs.iter().map(|song| song.title.clone()).collect()
}

fn ids(songs: &[CaseSong]) -> Vec<String> {
    songs.iter().map(|song| song.id.clone()).collect()
}

#[test]
fn case() {
    let cases = cases();
    for case in cases["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("a name");
        run_case(case, name);
    }
}

fn run_case(case: &Value, name: &str) {
    let songs: Vec<CaseSong> = case["songs"]
        .as_array()
        .expect("songs")
        .iter()
        .map(song)
        .collect();
    let fields = SubsonicFields::new(case);
    let report = health::check(&songs, &fields);
    let expect = &case["expect"];

    if let Some(duplicates) = expect.get("duplicates").and_then(Value::as_array) {
        assert_eq!(duplicates.len(), report.duplicates.len(), "{name}: duplicates");
        for (want, set) in duplicates.iter().zip(&report.duplicates) {
            check_set(want, set, &fields, name);
        }
    }
    if let Some(splits) = expect.get("splitAlbums").and_then(Value::as_array) {
        assert_eq!(splits.len(), report.split_albums.len(), "{name}: split albums");
        for (want, album) in splits.iter().zip(&report.split_albums) {
            check_split(want, album, &fields, name);
        }
    }
    if let Some(missing) = expect.get("missing").and_then(Value::as_object) {
        for (tag, want) in missing {
            let got = report
                .missing
                .get(&tag_of(tag))
                .map(|songs| titles(songs))
                .unwrap_or_default();
            assert_eq!(strings(want), got, "{name}: missing {tag}");
        }
    }
    if let Some(absent) = expect.get("missingAbsent") {
        for tag in strings(absent) {
            assert!(
                !report.missing.contains_key(&tag_of(&tag)),
                "{name}: {tag} absent"
            );
        }
    }
    if let Some(keys) = expect.get("missingKeys") {
        let got: Vec<String> = report.missing.keys().map(|tag| tag.name().to_string()).collect();
        assert_eq!(strings(keys), got, "{name}: missing keys");
    }
    if let Some(songs_of) = expect.get("songsOf").and_then(Value::as_object) {
        for (check, want) in songs_of {
            assert_eq!(
                strings(want),
                titles(&report.songs(check_of(check))),
                "{name}: songs of {check}"
            );
        }
    }
    if let Some(no_length) = expect.get("noLength") {
        assert_eq!(strings(no_length), titles(&report.no_length), "{name}: no length");
    }
    if let Some(findings) = expect.get("findings") {
        let got: Vec<String> = report
            .findings()
            .iter()
            .map(|check| check.name().to_string())
            .collect();
        assert_eq!(strings(findings), got, "{name}: findings");
    }
    if let Some(counts) = expect.get("counts").and_then(Value::as_object) {
        for (check, want) in counts {
            assert_eq!(
                want.as_u64().expect("a count") as usize,
                report.count(check_of(check)),
                "{name}: count of {check}"
            );
        }
    }
    if let Some(overview) = expect.get("overview") {
        assert_eq!(
            overview.as_str(),
            Some(words::overview(&report).as_str()),
            "{name}: overview"
        );
    }
    if let Some(without) = expect.get("without") {
        let gone: HashSet<String> = strings(&without["ids"]).into_iter().collect();
        let after = report.without(&gone, &fields);
        assert_eq!(
            without["duplicates"].as_u64().expect("a count") as usize,
            after.duplicates.len(),
            "{name}: duplicates without"
        );
        for (check, want) in without["songsOf"].as_object().expect("songsOf") {
            assert_eq!(
                strings(want),
                ids(&after.songs(check_of(check))),
                "{name}: {check} without"
            );
        }
    }
    if let Some(fix) = expect.get("fix") {
        assert_eq!(report.duplicates.len(), 1, "{name}: one set to fix");
        check_fix(fix, &report.duplicates[0], &fields, name);
    }
    if let Some(fills) = expect.get("fillsFromAlbum") {
        let tag = tag_of(fills["tag"].as_str().expect("a tag"));
        let wanting: Vec<CaseSong> = match fills.get("missing") {
            Some(ids) => {
                let ids = strings(ids);
                songs
                    .iter()
                    .filter(|song| ids.contains(&song.id))
                    .cloned()
                    .collect()
            }
            None => report.missing[&tag].clone(),
        };
        let found = fixes::fills_from_album(&wanting, &songs, tag, &fields);
        let want: Vec<(String, String, String)> = fills["fills"]
            .as_array()
            .expect("fills")
            .iter()
            .map(|fill| {
                (
                    fill[0].as_str().unwrap_or("").to_string(),
                    fill[1].as_str().unwrap_or("").to_string(),
                    fill[2].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let got: Vec<(String, String, String)> = found
            .iter()
            .map(|(song, change)| (song.id.clone(), change.tag.clone(), change.value.clone()))
            .collect();
        assert_eq!(want, got, "{name}: fills from the album");
    }
}

fn check_set(expect: &Value, set: &DuplicateSet<CaseSong>, fields: &SubsonicFields, name: &str) {
    if let Some(copies) = expect.get("copies") {
        assert_eq!(strings(copies), ids(&set.copies), "{name}: copies");
    }
    if let Some(size) = expect.get("size") {
        assert_eq!(size.as_u64(), Some(set.copies.len() as u64), "{name}: size");
    }
    if let Some(basis) = expect.get("basis") {
        assert_eq!(basis.as_str(), Some(set.basis.name()), "{name}: basis");
    }
    if let Some(best) = expect.get("best") {
        assert_eq!(best.as_str(), Some(set.best_reason.name()), "{name}: best");
    }
    if let Some(heading) = expect.get("heading") {
        assert_eq!(
            heading.as_str(),
            Some(words::set_heading(set, fields).as_str()),
            "{name}: heading"
        );
    }
    if let Some(summary) = expect.get("summary") {
        assert_eq!(
            summary.as_str(),
            Some(words::set_summary(set, fields).as_str()),
            "{name}: summary"
        );
    }
}

fn check_split(expect: &Value, album: &SplitAlbum<CaseSong>, fields: &SubsonicFields, name: &str) {
    if let Some(parts) = expect.get("parts") {
        let got: Vec<String> = album.parts.iter().map(|part| part.album_id.clone()).collect();
        assert_eq!(strings(parts), got, "{name}: parts");
    }
    if let Some(sizes) = expect.get("sizes").and_then(Value::as_array) {
        let want: Vec<u64> = sizes.iter().map(|size| size.as_u64().expect("a size")).collect();
        let got: Vec<u64> = album.parts.iter().map(|part| part.songs.len() as u64).collect();
        assert_eq!(want, got, "{name}: sizes");
    }
    if let Some(artist) = expect.get("artist") {
        assert_eq!(artist.as_str(), Some(album.artist.as_str()), "{name}: artist");
    }
    if let Some(heading) = expect.get("heading") {
        assert_eq!(
            heading.as_str(),
            Some(words::album_heading(album).as_str()),
            "{name}: heading"
        );
    }
    if let Some(summary) = expect.get("summary") {
        assert_eq!(
            summary.as_str(),
            Some(words::album_summary(album).as_str()),
            "{name}: summary"
        );
    }
    if let Some(reasons) = expect.get("reasons").and_then(Value::as_array) {
        assert_eq!(reasons.len(), album.reasons.len(), "{name}: reasons");
        for (want, got) in reasons.iter().zip(&album.reasons) {
            assert_eq!(
                want["basis"].as_str(),
                Some(got.basis.name()),
                "{name}: reason basis"
            );
            if let Some(artist) = want.get("artist") {
                assert_eq!(
                    artist.as_str(),
                    Some(got.artist.as_str()),
                    "{name}: reason artist"
                );
            }
            if let Some(other) = want.get("other") {
                assert_eq!(other.as_str(), Some(got.other.as_str()), "{name}: reason other");
            }
            if let Some(song) = want.get("song") {
                assert_eq!(song.as_str(), Some(got.song.as_str()), "{name}: reason song");
            }
        }
    }
    if let Some(reason_words) = expect.get("reasonWords") {
        let got: Vec<String> = album.reasons.iter().map(words::reason_words).collect();
        assert_eq!(strings(reason_words), got, "{name}: reason words");
    }
    if let Some(differences) = expect.get("differences").and_then(Value::as_array) {
        assert_eq!(differences.len(), album.differences.len(), "{name}: differences");
        for (want, got) in differences.iter().zip(&album.differences) {
            assert_eq!(
                want["kind"].as_str(),
                Some(got.kind.name()),
                "{name}: difference kind"
            );
            assert_eq!(strings(&want["values"]), got.values, "{name}: difference values");
        }
    }
    let join = fixes::album_join(album);
    if let Some(moving) = expect.get("joinMoving") {
        let mut want = strings(moving);
        want.sort();
        let mut got = ids(&join.moving);
        got.sort();
        assert_eq!(want, got, "{name}: join moving");
    }
    if let Some(lead) = expect.get("joinLeadAlbum") {
        assert_eq!(lead.as_str(), join.lead.album_id.as_deref(), "{name}: join lead");
        for step in fixes::join_steps(&join, fields) {
            assert_eq!(step.action, fix_actions::JOIN_ALBUM, "{name}: join step");
            assert_eq!(
                step.param("like"),
                Some(join.lead.id.as_str()),
                "{name}: join like"
            );
        }
    }
    if let Some(join_words) = expect.get("joinWords") {
        assert_eq!(
            join_words.as_str(),
            Some(join.words().as_str()),
            "{name}: join words"
        );
    }
}

fn check_fix(expect: &Value, set: &DuplicateSet<CaseSong>, fields: &SubsonicFields, name: &str) {
    let pick = expect.get("pick").map(|picked| {
        set.copies
            .iter()
            .find(|song| Some(song.id.as_str()) == picked.as_str())
            .expect("the picked copy")
    });
    let fix = fixes::duplicate_fix(set, fields, pick);
    if let Some(keep) = expect.get("keep") {
        assert_eq!(keep.as_str(), Some(fix.keep.id.as_str()), "{name}: keep");
    }
    if let Some(remove) = expect.get("remove") {
        assert_eq!(strings(remove), ids(&fix.remove), "{name}: remove");
    }
    if let Some(why) = expect.get("why") {
        assert_eq!(why.as_str(), Some(fix.why.as_str()), "{name}: why");
    }
    if let Some(fills) = expect.get("fills").and_then(Value::as_object) {
        let want: HashMap<String, String> = fills
            .iter()
            .map(|(tag, value)| (tag.clone(), value.as_str().unwrap_or("").to_string()))
            .collect();
        let got: HashMap<String, String> = fix
            .fills
            .iter()
            .map(|change| (change.tag.clone(), change.value.clone()))
            .collect();
        assert_eq!(want, got, "{name}: fills");
    }
    if let Some(from) = expect.get("fillsFrom") {
        for change in &fix.fills {
            assert_eq!(from.as_str(), change.from.as_deref(), "{name}: fills from");
            assert_eq!(change.now, None, "{name}: a fill is over a blank");
        }
    }
    if let Some(note) = expect.get("note") {
        assert_eq!(note.as_str(), fix.note.as_deref(), "{name}: note");
    }
    if let Some(differs) = expect.get("differs").and_then(Value::as_array) {
        assert_eq!(differs.len(), fix.differs.len(), "{name}: differs");
        for (want, got) in differs.iter().zip(&fix.differs) {
            assert_eq!(
                want["tag"].as_str(),
                Some(got.tag.as_str()),
                "{name}: differs tag"
            );
            let values: Vec<(String, String)> = want["values"]
                .as_array()
                .expect("values")
                .iter()
                .map(|pair| {
                    (
                        pair[0].as_str().unwrap_or("").to_string(),
                        pair[1].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect();
            assert_eq!(values, got.values, "{name}: differs values");
        }
    }
}
