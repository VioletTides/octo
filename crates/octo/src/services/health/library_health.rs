//! Plain checks over the library's songs that find what is worth fixing in the files. Nothing is
//! guessed from outside the library; the same songs always give the same report.
//!
//! Mirrors the Octo app's shared core (shared/core/.../health/LibraryHealth.kt), rule for rule,
//! so the dashboard and the apps find the same things in one library.
//! docs/library-health-cases.json holds the cases both sides are checked against; change a rule
//! here and there together.

use std::collections::{BTreeMap, HashMap, HashSet};

use indexmap::{IndexMap, IndexSet};
use octo_core::common::dotnet::{is_blank, is_digit, to_lower_invariant};
use octo_core::common::song_identity::{SongIdentity, SongMatchOptions, SongRef};

use crate::services::library::quality_upgrade_worker::compare_ordinal;

/// A tag the checks look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HealthTag {
    Genre,
    Year,
    AlbumArtist,
    TrackNumber,
    Cover,
}

impl HealthTag {
    /// Every tag, in the enum's order.
    pub const ALL: [HealthTag; 5] = [
        HealthTag::Genre,
        HealthTag::Year,
        HealthTag::AlbumArtist,
        HealthTag::TrackNumber,
        HealthTag::Cover,
    ];

    /// The tag's name, in PascalCase.
    pub fn name(self) -> &'static str {
        match self {
            HealthTag::Genre => "Genre",
            HealthTag::Year => "Year",
            HealthTag::AlbumArtist => "AlbumArtist",
            HealthTag::TrackNumber => "TrackNumber",
            HealthTag::Cover => "Cover",
        }
    }

    /// `Enum.Parse<HealthTag>(name)`.
    pub fn parse(name: &str) -> Option<HealthTag> {
        HealthTag::ALL.into_iter().find(|tag| tag.name() == name)
    }
}

/// What the checks read from a song. Each caller reads its own kind of song.
pub trait IHealthFields<T> {
    fn id<'a>(&'a self, song: &'a T) -> &'a str;
    fn title<'a>(&'a self, song: &'a T) -> &'a str;
    fn artist<'a>(&'a self, song: &'a T) -> Option<&'a str>;
    fn album<'a>(&'a self, song: &'a T) -> Option<&'a str>;
    fn album_id<'a>(&'a self, song: &'a T) -> Option<&'a str>;
    fn album_artist<'a>(&'a self, song: &'a T) -> Option<&'a str>;
    fn genres<'a>(&'a self, song: &'a T) -> Vec<&'a str>;
    fn year(&self, song: &T) -> Option<i32>;
    fn disc(&self, song: &T) -> Option<i32>;
    fn track(&self, song: &T) -> Option<i32>;
    /// Whole seconds, 0 when unknown.
    fn seconds(&self, song: &T) -> i32;
    fn format(&self, song: &T) -> Option<String>;
    fn lossless(&self, song: &T) -> bool;

    /// Kilobits per second, bits per sample, samples per second.
    fn bit_rate(&self, song: &T) -> Option<i32>;
    fn bit_depth(&self, song: &T) -> Option<i32>;
    fn sample_rate(&self, song: &T) -> Option<i32>;

    /// Codes that name the recording: its ISRCs and its MusicBrainz recording id.
    fn isrcs<'a>(&'a self, song: &'a T) -> Vec<&'a str>;
    fn recording_id<'a>(&'a self, song: &'a T) -> Option<&'a str>;
    fn cover<'a>(&'a self, song: &'a T) -> Option<&'a str>;

    /// Every artist the song credits, and every album artist it names. One credit each when
    /// that is all there is.
    fn artists<'a>(&'a self, song: &'a T) -> Vec<&'a str> {
        self.artist(song).into_iter().collect()
    }

    fn album_artists<'a>(&'a self, song: &'a T) -> Vec<&'a str> {
        self.album_artist(song).into_iter().collect()
    }

    /// What names the release the song came out on, when it is known: its MusicBrainz release
    /// and release group, its barcode, and its record labels.
    fn release_id<'a>(&'a self, _song: &'a T) -> Option<&'a str> {
        None
    }

    fn release_group_id<'a>(&'a self, _song: &'a T) -> Option<&'a str> {
        None
    }

    fn barcode<'a>(&'a self, _song: &'a T) -> Option<&'a str> {
        None
    }

    fn labels<'a>(&'a self, _song: &'a T) -> Vec<&'a str> {
        Vec::new()
    }

    /// Where the song is kept. Copies in two places are not duplicates, and an album is never
    /// split across places. The server has one place.
    fn place<'a>(&'a self, _song: &'a T) -> &'a str {
        ""
    }

    /// Whether this reader can see the tag at all. One it never keeps is never reported missing.
    fn seen(&self, _tag: HealthTag) -> bool {
        true
    }
}

/// Why copies were taken for one recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateBasis {
    /// Their tags name the same recording: an ISRC or a MusicBrainz id.
    Tags,
    /// The same title and artist, in the same version, and lengths within a few seconds.
    TitleAndLength,
}

impl DuplicateBasis {
    pub const ALL: [DuplicateBasis; 2] = [DuplicateBasis::Tags, DuplicateBasis::TitleAndLength];

    pub fn name(self) -> &'static str {
        match self {
            DuplicateBasis::Tags => "Tags",
            DuplicateBasis::TitleAndLength => "TitleAndLength",
        }
    }
}

/// Why the first copy of a set is the one to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BestReason {
    /// It sounds better: lossless, more bits, a higher rate.
    Sound,
    /// The copies sound alike, and it has more of its tags.
    Tags,
    /// Nothing sets them apart.
    None,
}

impl BestReason {
    pub const ALL: [BestReason; 3] = [BestReason::Sound, BestReason::Tags, BestReason::None];

    pub fn name(self) -> &'static str {
        match self {
            BestReason::Sound => "Sound",
            BestReason::Tags => "Tags",
            BestReason::None => "None",
        }
    }
}

/// Copies of one recording, the best first.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateSet<T> {
    pub copies: Vec<T>,
    pub basis: DuplicateBasis,
    pub best_reason: BestReason,
}

impl<T> DuplicateSet<T> {
    pub fn best(&self) -> &T {
        &self.copies[0]
    }
}

/// One way the parts of a split album differ, with each part's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumDifference {
    Title,
    AlbumArtist,
    Year,
    Other,
}

impl AlbumDifference {
    pub const ALL: [AlbumDifference; 4] = [
        AlbumDifference::Title,
        AlbumDifference::AlbumArtist,
        AlbumDifference::Year,
        AlbumDifference::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            AlbumDifference::Title => "Title",
            AlbumDifference::AlbumArtist => "AlbumArtist",
            AlbumDifference::Year => "Year",
            AlbumDifference::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumDifferenceValues {
    pub kind: AlbumDifference,
    pub values: Vec<String>,
}

/// One album entry the server shows, and its songs in album order.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumPart<T> {
    pub album_id: String,
    pub songs: Vec<T>,
}

/// Why parts of one name with different album artists are one album.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitBasis {
    /// One part's album artist is credited on the other part too: a song there is by them, or
    /// its album artist names them. A collaboration album filed under each of its artists.
    SharedArtist,
    SameRelease,
    SameReleaseGroup,
    SameBarcode,
    /// The parts came out on the same label in the same year.
    SameLabelAndYear,
}

impl SplitBasis {
    pub const ALL: [SplitBasis; 5] = [
        SplitBasis::SharedArtist,
        SplitBasis::SameRelease,
        SplitBasis::SameReleaseGroup,
        SplitBasis::SameBarcode,
        SplitBasis::SameLabelAndYear,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SplitBasis::SharedArtist => "SharedArtist",
            SplitBasis::SameRelease => "SameRelease",
            SplitBasis::SameReleaseGroup => "SameReleaseGroup",
            SplitBasis::SameBarcode => "SameBarcode",
            SplitBasis::SameLabelAndYear => "SameLabelAndYear",
        }
    }
}

/// One reason, with what it rests on: for a shared artist, `artist` is one part's album artist,
/// `other` the other part's, and `song` a song of the other part by `artist` (empty when its
/// album artist names them); for the rest, `value` is what the parts share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitReason {
    pub basis: SplitBasis,
    pub artist: String,
    pub other: String,
    pub song: String,
    pub value: String,
}

impl SplitReason {
    pub fn new(basis: SplitBasis) -> Self {
        SplitReason {
            basis,
            artist: String::new(),
            other: String::new(),
            song: String::new(),
            value: String::new(),
        }
    }

    fn valued(basis: SplitBasis, value: String) -> Self {
        SplitReason {
            value,
            ..SplitReason::new(basis)
        }
    }

    fn shared(artist: &str, other: &str, song: &str) -> Self {
        SplitReason {
            artist: artist.to_string(),
            other: other.to_string(),
            song: song.to_string(),
            ..SplitReason::new(SplitBasis::SharedArtist)
        }
    }
}

/// An album the server shows as two or more, because its songs' tags do not agree. The first
/// part is the one the others join: the largest, or the one whose album artist names every
/// part's artist. Reasons say why parts with different album artists were taken for one album;
/// parts by one album artist need none.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitAlbum<T> {
    pub title: String,
    pub artist: String,
    pub parts: Vec<AlbumPart<T>>,
    pub differences: Vec<AlbumDifferenceValues>,
    pub reasons: Vec<SplitReason>,
}

/// What one check is about. The order is the order they are shown in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HealthCheck {
    Duplicates,
    SplitAlbums,
    NoLength,
    NoTrackNumber,
    NoAlbumArtist,
    NoCover,
    NoYear,
    NoGenre,
}

impl HealthCheck {
    /// Every check, in the order they are shown in.
    pub const ALL: [HealthCheck; 8] = [
        HealthCheck::Duplicates,
        HealthCheck::SplitAlbums,
        HealthCheck::NoLength,
        HealthCheck::NoTrackNumber,
        HealthCheck::NoAlbumArtist,
        HealthCheck::NoCover,
        HealthCheck::NoYear,
        HealthCheck::NoGenre,
    ];

    /// The check's name, in PascalCase (the log line and the shared cases).
    pub fn name(self) -> &'static str {
        match self {
            HealthCheck::Duplicates => "Duplicates",
            HealthCheck::SplitAlbums => "SplitAlbums",
            HealthCheck::NoLength => "NoLength",
            HealthCheck::NoTrackNumber => "NoTrackNumber",
            HealthCheck::NoAlbumArtist => "NoAlbumArtist",
            HealthCheck::NoCover => "NoCover",
            HealthCheck::NoYear => "NoYear",
            HealthCheck::NoGenre => "NoGenre",
        }
    }

    /// `Enum.Parse<HealthCheck>(name)`.
    pub fn parse(name: &str) -> Option<HealthCheck> {
        HealthCheck::ALL.into_iter().find(|check| check.name() == name)
    }
}

/// Everything the checks found.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthReport<T> {
    pub checked: usize,
    pub duplicates: Vec<DuplicateSet<T>>,
    pub split_albums: Vec<SplitAlbum<T>>,
    pub no_length: Vec<T>,
    /// By tag, in the tags' order.
    pub missing: BTreeMap<HealthTag, Vec<T>>,
}

impl<T: Clone> HealthReport<T> {
    /// How many things a check found: sets of copies, albums, or songs.
    pub fn count(&self, check: HealthCheck) -> usize {
        match check {
            HealthCheck::Duplicates => self.duplicates.len(),
            HealthCheck::SplitAlbums => self.split_albums.len(),
            HealthCheck::NoLength => self.no_length.len(),
            _ => tag_of(check)
                .and_then(|tag| self.missing.get(&tag))
                .map_or(0, Vec::len),
        }
    }

    /// The checks that found something, in order.
    pub fn findings(&self) -> Vec<HealthCheck> {
        HealthCheck::ALL
            .into_iter()
            .filter(|check| self.count(*check) > 0)
            .collect()
    }

    pub fn clean(&self) -> bool {
        self.findings().is_empty()
    }

    /// The songs a check is about, for a list: each set of copies together with the best first,
    /// each split album's parts one after the other, and missing tags by artist and album.
    pub fn songs(&self, check: HealthCheck) -> Vec<T> {
        match check {
            HealthCheck::Duplicates => self
                .duplicates
                .iter()
                .flat_map(|set| set.copies.iter().cloned())
                .collect(),
            HealthCheck::SplitAlbums => self
                .split_albums
                .iter()
                .flat_map(|album| album.parts.iter().flat_map(|part| part.songs.iter().cloned()))
                .collect(),
            HealthCheck::NoLength => self.no_length.clone(),
            _ => tag_of(check)
                .and_then(|tag| self.missing.get(&tag))
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// The report without some songs, as after they were removed from the library: a set left
    /// with one copy is no longer a duplicate.
    pub fn without(&self, ids: &HashSet<String>, fields: &dyn IHealthFields<T>) -> HealthReport<T> {
        if ids.is_empty() {
            return self.clone();
        }
        let keep = |song: &&T| !ids.contains(fields.id(song));
        HealthReport {
            checked: self.checked,
            duplicates: self
                .duplicates
                .iter()
                .map(|set| DuplicateSet {
                    copies: set.copies.iter().filter(keep).cloned().collect(),
                    basis: set.basis,
                    best_reason: set.best_reason,
                })
                .filter(|set| set.copies.len() >= 2)
                .collect(),
            split_albums: self
                .split_albums
                .iter()
                .map(|album| SplitAlbum {
                    parts: album
                        .parts
                        .iter()
                        .map(|part| AlbumPart {
                            album_id: part.album_id.clone(),
                            songs: part.songs.iter().filter(keep).cloned().collect(),
                        })
                        .filter(|part| !part.songs.is_empty())
                        .collect(),
                    ..album.clone()
                })
                .filter(|album| album.parts.len() >= 2)
                .collect(),
            no_length: self.no_length.iter().filter(keep).cloned().collect(),
            missing: self
                .missing
                .iter()
                .map(|(tag, songs)| (*tag, songs.iter().filter(keep).cloned().collect::<Vec<T>>()))
                .filter(|(_, songs)| !songs.is_empty())
                .collect(),
        }
    }

    /// The report with songs fixed for one check left out of that check only, until the server's
    /// list catches up: a song given its year still lacks its genre.
    pub fn settled_for(
        &self,
        check: HealthCheck,
        ids: &HashSet<String>,
        fields: &dyn IHealthFields<T>,
    ) -> HealthReport<T> {
        if ids.is_empty() {
            return self.clone();
        }
        let only = self.without(ids, fields);
        match check {
            HealthCheck::Duplicates => {
                return HealthReport {
                    duplicates: only.duplicates,
                    ..self.clone()
                };
            }
            HealthCheck::SplitAlbums => {
                return HealthReport {
                    split_albums: only.split_albums,
                    ..self.clone()
                };
            }
            HealthCheck::NoLength => {
                return HealthReport {
                    no_length: only.no_length,
                    ..self.clone()
                };
            }
            _ => {}
        }
        let Some(tag) = tag_of(check) else {
            return self.clone();
        };
        let left: Vec<T> = self
            .missing
            .get(&tag)
            .map(|songs| {
                songs
                    .iter()
                    .filter(|song| !ids.contains(fields.id(song)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        // Keep the tags in their order (the map is kept in it).
        let mut missing = self.missing.clone();
        missing.remove(&tag);
        if !left.is_empty() {
            missing.insert(tag, left);
        }
        HealthReport {
            missing,
            ..self.clone()
        }
    }
}

/// Lengths of copies of one recording may be this far apart, in seconds.
pub const SAME_LENGTH_SECONDS: i32 = SongIdentity::LENGTH_TOLERANCE_SECONDS;

/// A shared ISRC or MusicBrainz id counts only when the lengths are this close, so an album
/// whose every song was tagged with one code by mistake is not taken for one song many times.
pub const TAGGED_LENGTH_SECONDS: i32 = 10;

fn duplicate_titles() -> SongMatchOptions {
    SongMatchOptions {
        length_tolerance_seconds: Some(SAME_LENGTH_SECONDS),
        extras_must_agree: true,
        ..Default::default()
    }
}

pub fn tag_of(check: HealthCheck) -> Option<HealthTag> {
    match check {
        HealthCheck::NoTrackNumber => Some(HealthTag::TrackNumber),
        HealthCheck::NoAlbumArtist => Some(HealthTag::AlbumArtist),
        HealthCheck::NoCover => Some(HealthTag::Cover),
        HealthCheck::NoYear => Some(HealthTag::Year),
        HealthCheck::NoGenre => Some(HealthTag::Genre),
        _ => None,
    }
}

/// Runs every check over the songs.
pub fn check<T: Clone>(songs: &[T], fields: &dyn IHealthFields<T>) -> HealthReport<T> {
    HealthReport {
        checked: songs.len(),
        duplicates: find_duplicates(songs, fields),
        split_albums: find_split_albums(songs, fields),
        no_length: songs
            .iter()
            .filter(|song| fields.seconds(song) <= 0)
            .cloned()
            .collect(),
        missing: find_missing_tags(songs, fields),
    }
}

// ---- duplicates -----------------------------------------------------------------------------

/// Songs that are the same recording, in sets. A set's copies are all in one place. Songs whose
/// tags name one recording (an ISRC or a MusicBrainz recording id, with lengths close) go
/// together first; then songs by the same artist whose titles read the same, version and all,
/// and whose lengths are within a few seconds.
pub fn find_duplicates<T: Clone>(songs: &[T], fields: &dyn IHealthFields<T>) -> Vec<DuplicateSet<T>> {
    let mut sets = Sets::new(songs.len());
    let mut by_tag: HashMap<String, usize> = HashMap::new();
    let mut by_title: IndexMap<String, Vec<usize>> = IndexMap::new();
    let keys = match_keys(songs, fields);
    let mut tagged = vec![false; songs.len()];

    for (index, song) in songs.iter().enumerate() {
        let place = fields.place(song);
        let mut codes: Vec<String> = SongIdentity::isrcs(fields.isrcs(song))
            .into_iter()
            .map(|isrc| format!("i:{isrc}"))
            .collect();
        if let Some(recording) = fields
            .recording_id(song)
            .map(|id| to_lower_invariant(id.trim()))
            .filter(|id| !id.is_empty())
        {
            codes.push(format!("m:{recording}"));
        }
        for code in codes {
            let key = format!("{place}\0{code}");
            match by_tag.get(&key) {
                None => {
                    by_tag.insert(key, index);
                }
                Some(&first) => {
                    if lengths_close(
                        fields.seconds(&songs[first]),
                        fields.seconds(song),
                        TAGGED_LENGTH_SECONDS,
                    ) && sets.join(first, index)
                    {
                        tagged[first] = true;
                        tagged[index] = true;
                    }
                }
            }
        }
        let matched = &keys[index];
        // A title with nothing to read in it is never matched on its words.
        let words = matched
            .find('|')
            .map_or(matched.as_str(), |bar| &matched[bar + 1..]);
        if !words.is_empty() {
            by_title
                .entry(format!("{place}\0{matched}"))
                .or_insert_with(|| Vec::with_capacity(2))
                .push(index);
        }
    }

    let options = duplicate_titles();
    for bucket in by_title.values() {
        if bucket.len() < 2 {
            continue;
        }
        for i in 0..bucket.len() {
            let a = &songs[bucket[i]];
            let ref_a = ref_of(a, fields);
            for j in i + 1..bucket.len() {
                if sets.same(bucket[i], bucket[j]) {
                    continue;
                }
                let b = &songs[bucket[j]];
                if !lengths_close(fields.seconds(a), fields.seconds(b), SAME_LENGTH_SECONDS) {
                    continue;
                }
                if SongIdentity::same(&ref_a, &ref_of(b, fields), Some(&options)).is_same() {
                    sets.join(bucket[i], bucket[j]);
                }
            }
        }
    }

    let mut groups: IndexMap<usize, Vec<usize>> = IndexMap::new();
    for index in 0..songs.len() {
        if sets.size(index) <= 1 {
            continue;
        }
        let root = sets.root(index);
        groups
            .entry(root)
            .or_insert_with(|| Vec::with_capacity(2))
            .push(index);
    }
    let mut found: Vec<DuplicateSet<T>> = groups
        .values()
        .map(|members| {
            let mut copies: Vec<T> = members.iter().map(|&i| songs[i].clone()).collect();
            copies.sort_by(|a, b| {
                sound_of(b, fields)
                    .cmp(&sound_of(a, fields))
                    .then_with(|| tags_of(b, fields).cmp(&tags_of(a, fields)))
                    .then_with(|| compare_ordinal(fields.id(a), fields.id(b)))
            });
            let basis = if members.iter().any(|&i| tagged[i]) {
                DuplicateBasis::Tags
            } else {
                DuplicateBasis::TitleAndLength
            };
            let best_reason = best_reason_of(&copies, fields);
            DuplicateSet {
                copies,
                basis,
                best_reason,
            }
        })
        .collect();
    found.sort_by(|a, b| {
        compare_ordinal(
            &sort_key(fields.artist(a.best())),
            &sort_key(fields.artist(b.best())),
        )
        .then_with(|| {
            compare_ordinal(
                &sort_key(Some(fields.title(a.best()))),
                &sort_key(Some(fields.title(b.best()))),
            )
        })
        .then_with(|| compare_ordinal(fields.id(a.best()), fields.id(b.best())))
    });
    found
}

/// Each song's key for "this song in this version". Reading a title is the slow part of the
/// checks, so each artist and title pair is read once.
fn match_keys<T>(songs: &[T], fields: &dyn IHealthFields<T>) -> Vec<String> {
    let mut read: HashMap<(Option<&str>, &str), String> = HashMap::new();
    songs
        .iter()
        .map(|song| {
            let pair = (fields.artist(song), fields.title(song));
            read.entry(pair)
                .or_insert_with(|| SongIdentity::match_key(pair.0.unwrap_or(""), pair.1))
                .clone()
        })
        .collect()
}

fn ref_of<T>(song: &T, fields: &dyn IHealthFields<T>) -> SongRef {
    let seconds = fields.seconds(song);
    SongRef {
        title: fields.title(song).to_string(),
        artist: fields.artist(song).unwrap_or("").to_string(),
        seconds: (seconds > 0).then_some(f64::from(seconds)),
        isrcs: fields.isrcs(song).into_iter().map(str::to_string).collect(),
    }
}

/// Within the tolerance, or a length unknown on either side.
fn lengths_close(a: i32, b: i32, tolerance: i32) -> bool {
    a <= 0 || b <= 0 || (i64::from(a) - i64::from(b)).abs() <= i64::from(tolerance)
}

/// How good a copy sounds, as one number that sorts: lossless first, then more bits per sample,
/// a higher sample rate, and a higher bit rate. The bit rate of two lossless copies of one depth
/// says only how hard they were packed, so it counts there only when the depth is unknown.
pub fn sound_of<T>(song: &T, fields: &dyn IHealthFields<T>) -> i64 {
    let lossless = fields.lossless(song);
    let depth = i64::from(fields.bit_depth(song).unwrap_or(0).clamp(0, 63));
    let rate = i64::from((fields.sample_rate(song).unwrap_or(0) / 100).clamp(0, 65_535));
    let bits = if lossless && depth > 0 {
        0
    } else {
        i64::from(fields.bit_rate(song).unwrap_or(0).clamp(0, 65_535))
    };
    (i64::from(lossless) << 48) | (depth << 40) | (rate << 20) | bits
}

/// How many of the tags the checks look for a song has.
pub fn tags_of<T>(song: &T, fields: &dyn IHealthFields<T>) -> i32 {
    let mut count = 0;
    if fields.genres(song).iter().any(|genre| !is_blank(genre)) {
        count += 1;
    }
    if fields.year(song).unwrap_or(0) > 0 {
        count += 1;
    }
    if fields.album_artist(song).is_some_and(|artist| !is_blank(artist)) {
        count += 1;
    }
    if fields.track(song).unwrap_or(0) > 0 {
        count += 1;
    }
    if fields.cover(song).is_some_and(|cover| !is_blank(cover)) {
        count += 1;
    }
    count
}

fn best_reason_of<T>(copies: &[T], fields: &dyn IHealthFields<T>) -> BestReason {
    let (best, next) = (&copies[0], &copies[1]);
    if sound_of(best, fields) > sound_of(next, fields) {
        return BestReason::Sound;
    }
    if tags_of(best, fields) > tags_of(next, fields) {
        return BestReason::Tags;
    }
    BestReason::None
}

// ---- split albums ---------------------------------------------------------------------------

/// Albums the server shows as more than one: one title, in one place, under two or more album
/// ids, with years that do not tell them apart (the same year, or none on one side). Parts by
/// the same album artist belong together. Parts by different album artists belong together only
/// when something shows they are one release: the same MusicBrainz release or release group, the
/// same barcode, the same label in the same year, or one part's album artist credited on the
/// other part, as a collaboration album filed under each of its artists is. A shared credit
/// alone does not join a title many artists use ("Greatest Hits", "Live") or an artist's own
/// name. Two albums of one name from different years stay apart, and so do albums of one name by
/// artists with nothing in common.
pub fn find_split_albums<T: Clone>(songs: &[T], fields: &dyn IHealthFields<T>) -> Vec<SplitAlbum<T>> {
    let mut by_album: IndexMap<&str, Vec<&T>> = IndexMap::new();
    for song in songs {
        let Some(id) = fields.album_id(song).filter(|id| !id.is_empty()) else {
            continue;
        };
        by_album.entry(id).or_default().push(song);
    }
    let mut buckets: IndexMap<String, Vec<PartFacts>> = IndexMap::new();
    for (id, members) in &by_album {
        let first = members[0];
        let title = SongIdentity::key(fields.album(first).unwrap_or(""));
        if title.is_empty() {
            continue;
        }
        let Some(facts) = part_facts_of(id, members, fields) else {
            continue;
        };
        buckets
            .entry(format!("{}\0{title}", fields.place(first)))
            .or_insert_with(|| Vec::with_capacity(1))
            .push(facts);
    }

    let mut found: Vec<SplitAlbum<T>> = Vec::new();
    for (bucket_key, parts) in &buckets {
        if parts.len() < 2 {
            continue;
        }
        let title = bucket_key
            .find('\0')
            .map_or(bucket_key.as_str(), |at| &bucket_key[at + 1..]);
        let mut sets = Sets::new(parts.len());
        let mut reasons: Vec<(usize, SplitReason)> = Vec::new();
        for i in 0..parts.len() {
            for j in i + 1..parts.len() {
                let (a, b) = (&parts[i], &parts[j]);
                if !years_agree(&a.years, &b.years) {
                    continue;
                }
                if a.artist_key == b.artist_key {
                    sets.join(i, j);
                    continue;
                }
                let Some(reason) = same_release(a, b).or_else(|| shared_artist(a, b, title)) else {
                    continue;
                };
                sets.join(i, j);
                reasons.push((i, reason));
            }
        }

        let mut groups_in_order: IndexMap<usize, Vec<usize>> = IndexMap::new();
        for i in 0..parts.len() {
            groups_in_order.entry(sets.root(i)).or_default().push(i);
        }
        for together in groups_in_order.values().filter(|group| group.len() > 1) {
            let mut by_size = together.clone();
            by_size.sort_by(|&a, &b| {
                parts[b]
                    .size
                    .cmp(&parts[a].size)
                    .then_with(|| compare_ordinal(&parts[a].id, &parts[b].id))
            });
            // A part whose album artist already names every part's artist leads; otherwise the
            // largest does.
            let artists: HashSet<&str> = together.iter().map(|&i| parts[i].artist_key.as_str()).collect();
            let named = if artists.len() > 1 {
                by_size.iter().copied().find(|&i| {
                    artists
                        .iter()
                        .all(|key| parts[i].album_artists.contains_key(*key))
                })
            } else {
                None
            };
            let lead = named.unwrap_or(by_size[0]);
            let ordered: Vec<AlbumPart<T>> = std::iter::once(lead)
                .chain(by_size.iter().copied().filter(|&i| i != lead))
                .map(|i| {
                    let id = parts[i].id.as_str();
                    AlbumPart {
                        album_id: id.to_string(),
                        songs: in_album_order(by_album[id].iter().map(|song| (*song).clone()), fields),
                    }
                })
                .collect();
            let lead_songs = &ordered[0].songs;
            let root = sets.root(together[0]);
            let mut album_reasons: Vec<SplitReason> = Vec::new();
            for (part, reason) in &reasons {
                if sets.root(*part) == root && !album_reasons.contains(reason) {
                    album_reasons.push(reason.clone());
                }
            }
            found.push(SplitAlbum {
                title: fields.album(&lead_songs[0]).unwrap_or("").to_string(),
                artist: album_artist_of(lead_songs, fields),
                differences: differences_of(&ordered, fields),
                parts: ordered,
                reasons: album_reasons,
            });
        }
    }
    found.sort_by(|a, b| {
        compare_ordinal(&sort_key(Some(&a.artist)), &sort_key(Some(&b.artist)))
            .then_with(|| compare_ordinal(&sort_key(Some(&a.title)), &sort_key(Some(&b.title))))
            .then_with(|| compare_ordinal(&a.parts[0].album_id, &b.parts[0].album_id))
    });
    found
}

/// What the split check reads of one album part.
struct PartFacts {
    id: String,
    size: usize,
    artist: String,
    artist_key: String,
    /// Every artist the album artist names, by key, as written (the app's LinkedHashMap: the
    /// order keys were first put in).
    album_artists: IndexMap<String, String>,
    /// Every artist the songs credit, by key, with one song of theirs.
    credits: IndexMap<String, String>,
    years: HashSet<i32>,
    releases: IndexSet<String>,
    groups: IndexSet<String>,
    barcodes: IndexSet<String>,
    /// Labels by key, as written.
    labels: IndexMap<String, String>,
}

fn part_facts_of<T>(id: &str, members: &[&T], fields: &dyn IHealthFields<T>) -> Option<PartFacts> {
    let artist = album_artist_of(members, fields);
    let artist_key = SongIdentity::key(&SongIdentity::primary_artist(&artist));
    if artist_key.is_empty() {
        return None;
    }
    let mut album_artists: IndexMap<String, String> = IndexMap::new();
    let mut named: Vec<&str> = Vec::new();
    for name in members.iter().flat_map(|song| fields.album_artists(song)) {
        if !is_blank(name) && !named.contains(&name) {
            named.push(name);
        }
    }
    if named.is_empty() {
        named.push(&artist);
    }
    for credit in named {
        for name in names_in(credit) {
            album_artists.entry(SongIdentity::key(&name)).or_insert(name);
        }
    }
    let mut credits: IndexMap<String, String> = IndexMap::new();
    for song in members {
        for credit in fields.artists(song) {
            for name in names_in(credit) {
                credits
                    .entry(SongIdentity::key(&name))
                    .or_insert_with(|| fields.title(song).to_string());
            }
        }
    }
    let codes = |of: &dyn Fn(&T) -> Option<String>, clean: &dyn Fn(&str) -> String| -> IndexSet<String> {
        members
            .iter()
            .filter_map(|song| of(song))
            .map(|code| clean(&code))
            .filter(|code| !code.is_empty())
            .collect()
    };
    let lowered = |code: &str| to_lower_invariant(code.trim());
    let mut labels: IndexMap<String, String> = IndexMap::new();
    for song in members {
        for label in fields.labels(song) {
            let key = SongIdentity::key(label);
            if !key.is_empty() {
                labels.entry(key).or_insert_with(|| label.trim().to_string());
            }
        }
    }
    Some(PartFacts {
        id: id.to_string(),
        size: members.len(),
        artist_key,
        album_artists,
        credits,
        years: members
            .iter()
            .filter_map(|song| fields.year(song))
            .filter(|year| *year > 0)
            .collect(),
        releases: codes(&|song| fields.release_id(song).map(str::to_string), &lowered),
        groups: codes(
            &|song| fields.release_group_id(song).map(str::to_string),
            &lowered,
        ),
        barcodes: codes(&|song| fields.barcode(song).map(str::to_string), &|code| {
            let digits: String = code.chars().filter(|ch| is_digit(*ch)).collect();
            digits.trim_start_matches('0').to_string()
        }),
        labels,
        artist,
    })
}

/// The artists a credit names, a guest in brackets left out: both of "PARTYNEXTDOOR & Drake".
fn names_in(credit: &str) -> Vec<String> {
    SongIdentity::parse_artists(credit)
        .names
        .into_iter()
        .filter(|name| !SongIdentity::key(name).is_empty())
        .collect()
}

/// Years that do not tell two parts apart: the same year, or none on one side.
fn years_agree(a: &HashSet<i32>, b: &HashSet<i32>) -> bool {
    a.is_empty() || b.is_empty() || a.iter().any(|year| b.contains(year))
}

/// The same release by its codes, or by its label and year.
fn same_release(a: &PartFacts, b: &PartFacts) -> Option<SplitReason> {
    if a.releases.iter().any(|code| b.releases.contains(code)) {
        return Some(SplitReason::new(SplitBasis::SameRelease));
    }
    if a.groups.iter().any(|code| b.groups.contains(code)) {
        return Some(SplitReason::new(SplitBasis::SameReleaseGroup));
    }
    if let Some(barcode) = a.barcodes.iter().find(|code| b.barcodes.contains(*code)) {
        return Some(SplitReason::valued(SplitBasis::SameBarcode, barcode.clone()));
    }
    let label = a.labels.keys().find(|key| b.labels.contains_key(*key))?;
    let common = a.years.iter().filter(|year| b.years.contains(year)).min()?;
    Some(SplitReason::valued(
        SplitBasis::SameLabelAndYear,
        format!("{}, {common}", a.labels[label]),
    ))
}

/// One part's album artist credited on the other part: a song there by them, though that part is
/// filed under someone else, or else both album artists naming them ("Drake & Future" and
/// "Future"). Never for a title many artists use, or one that is an artist's own name.
fn shared_artist(a: &PartFacts, b: &PartFacts, title: &str) -> Option<SplitReason> {
    if is_common_title(title) || a.album_artists.contains_key(title) || b.album_artists.contains_key(title) {
        return None;
    }
    let mut named: Option<SplitReason> = None;
    for (mine, theirs) in [(a, b), (b, a)] {
        for (key, name) in &mine.album_artists {
            if theirs.album_artists.contains_key(key) {
                // Said of the credit that names more than its own artist.
                if named.is_none() {
                    let other = if &mine.artist_key == key {
                        &theirs.artist
                    } else {
                        &mine.artist
                    };
                    named = Some(SplitReason::shared(name, other, ""));
                }
                continue;
            }
            if let Some(song) = theirs.credits.get(key) {
                return Some(SplitReason::shared(name, &theirs.artist, song));
            }
        }
    }
    named
}

/// Album titles many artists use, by key: sharing one says nothing about being one album.
const COMMON_TITLES: &[&str] = &[
    "album",
    "thealbum",
    "untitled",
    "unknown",
    "unknownalbum",
    "single",
    "singles",
    "thesingles",
    "ep",
    "theep",
    "live",
    "unplugged",
    "acoustic",
    "demo",
    "demos",
    "remixes",
    "theremixes",
    "bsides",
    "rarities",
    "hits",
    "thehits",
    "collection",
    "thecollection",
    "essentials",
    "anthology",
    "mixtape",
    "soundtrack",
    "originalsoundtrack",
    "ost",
    "christmas",
    "christmasalbum",
    "achristmasalbum",
    "lovesongs",
    "covers",
    "instrumentals",
    "deluxe",
    "deluxeedition",
    "remastered",
    "nonalbumsingle",
    "nonalbumsingles",
    "greatest",
    "thebest",
];

const COMMON_TITLE_STARTS: &[&str] = &[
    "greatesthits",
    "thegreatesthits",
    "bestof",
    "thebestof",
    "theverybestof",
    "theessential",
    "essential",
    "liveat",
    "livein",
    "livefrom",
    "mtvunplugged",
    "unplugged",
];

fn is_common_title(title: &str) -> bool {
    COMMON_TITLES.contains(&title) || COMMON_TITLE_STARTS.iter().any(|start| title.starts_with(start))
}

/// The album artist most of an album's songs name, or their artist.
pub fn album_artist_of<T, S: std::borrow::Borrow<T>>(songs: &[S], fields: &dyn IHealthFields<T>) -> String {
    let named: Vec<&str> = songs
        .iter()
        .filter_map(|song| fields.album_artist(song.borrow()))
        .filter(|name| !is_blank(name))
        .collect();
    let pool: Vec<&str> = if named.is_empty() {
        songs
            .iter()
            .filter_map(|song| fields.artist(song.borrow()))
            .filter(|name| !is_blank(name))
            .collect()
    } else {
        named
    };
    // The most named; between equals, the first in plain character order.
    let mut counts: IndexMap<&str, usize> = IndexMap::new();
    for name in pool {
        *counts.entry(name).or_default() += 1;
    }
    counts
        .into_iter()
        .min_by(|(a, count_a), (b, count_b)| count_b.cmp(count_a).then_with(|| compare_ordinal(a, b)))
        .map(|(name, _)| name.to_string())
        .unwrap_or_default()
}

/// What the parts disagree on, as each part says it.
fn differences_of<T>(parts: &[AlbumPart<T>], fields: &dyn IHealthFields<T>) -> Vec<AlbumDifferenceValues> {
    let mut found = Vec::new();
    let mut check = |kind: AlbumDifference, of: &dyn Fn(&[T]) -> String| {
        let values: Vec<String> = parts.iter().map(|part| of(&part.songs)).collect();
        let distinct: HashSet<&str> = values.iter().map(String::as_str).collect();
        if distinct.len() > 1 {
            found.push(AlbumDifferenceValues { kind, values });
        }
    };
    check(AlbumDifference::Title, &|songs| {
        fields.album(&songs[0]).unwrap_or("").to_string()
    });
    check(AlbumDifference::AlbumArtist, &|songs| {
        let mut names: Vec<&str> = Vec::new();
        for name in songs.iter().filter_map(|song| fields.album_artist(song)) {
            if !is_blank(name) && !names.contains(&name) {
                names.push(name);
            }
        }
        names.sort_by(|a, b| compare_ordinal(a, b));
        names.join(", ")
    });
    check(AlbumDifference::Year, &|songs| {
        let mut years: Vec<i32> = songs
            .iter()
            .filter_map(|song| fields.year(song))
            .filter(|year| *year > 0)
            .collect();
        years.sort_unstable();
        years.dedup();
        years.iter().map(i32::to_string).collect::<Vec<_>>().join(", ")
    });
    if found.is_empty() {
        found.push(AlbumDifferenceValues {
            kind: AlbumDifference::Other,
            values: Vec::new(),
        });
    }
    found
}

// ---- missing tags ---------------------------------------------------------------------------

/// Songs missing each tag the reader can see. A track number and an album artist matter only on
/// an album of two or more songs; a single needs neither.
pub fn find_missing_tags<T: Clone>(
    songs: &[T],
    fields: &dyn IHealthFields<T>,
) -> BTreeMap<HealthTag, Vec<T>> {
    let mut album_sizes: HashMap<&str, usize> = HashMap::new();
    for song in songs {
        if let Some(id) = fields.album_id(song) {
            *album_sizes.entry(id).or_default() += 1;
        }
    }
    let on_album = |song: &T| {
        fields
            .album_id(song)
            .map_or(0, |id| album_sizes.get(id).copied().unwrap_or(0))
            > 1
    };
    let mut found = BTreeMap::new();
    for tag in HealthTag::ALL {
        if !fields.seen(tag) {
            continue;
        }
        let missing: Vec<T> = songs
            .iter()
            .filter(|song| match tag {
                HealthTag::Genre => !fields.genres(song).iter().any(|genre| !is_blank(genre)),
                HealthTag::Year => fields.year(song).unwrap_or(0) <= 0,
                HealthTag::AlbumArtist => on_album(song) && fields.album_artist(song).is_none_or(is_blank),
                HealthTag::TrackNumber => on_album(song) && fields.track(song).unwrap_or(0) <= 0,
                HealthTag::Cover => fields.cover(song).is_none_or(is_blank),
            })
            .cloned()
            .collect();
        if !missing.is_empty() {
            found.insert(tag, across_albums(missing, fields));
        }
    }
    found
}

/// Songs of one album in its order: disc, track, then title.
pub fn in_album_order<T>(songs: impl IntoIterator<Item = T>, fields: &dyn IHealthFields<T>) -> Vec<T> {
    let mut songs: Vec<T> = songs.into_iter().collect();
    songs.sort_by(|a, b| {
        fields
            .disc(a)
            .unwrap_or(0)
            .cmp(&fields.disc(b).unwrap_or(0))
            .then_with(|| track_or_last(fields.track(a)).cmp(&track_or_last(fields.track(b))))
            .then_with(|| compare_ordinal(&sort_key(Some(fields.title(a))), &sort_key(Some(fields.title(b)))))
            .then_with(|| compare_ordinal(fields.id(a), fields.id(b)))
    });
    songs
}

/// Songs from many albums: by artist, then album, then album order.
fn across_albums<T>(mut songs: Vec<T>, fields: &dyn IHealthFields<T>) -> Vec<T> {
    songs.sort_by(|a, b| {
        compare_ordinal(
            &sort_key(fields.album_artist(a).or(fields.artist(a))),
            &sort_key(fields.album_artist(b).or(fields.artist(b))),
        )
        .then_with(|| compare_ordinal(&sort_key(fields.album(a)), &sort_key(fields.album(b))))
        .then_with(|| compare_ordinal(fields.album_id(a).unwrap_or(""), fields.album_id(b).unwrap_or("")))
        .then_with(|| fields.disc(a).unwrap_or(0).cmp(&fields.disc(b).unwrap_or(0)))
        .then_with(|| track_or_last(fields.track(a)).cmp(&track_or_last(fields.track(b))))
        .then_with(|| compare_ordinal(&sort_key(Some(fields.title(a))), &sort_key(Some(fields.title(b)))))
        .then_with(|| compare_ordinal(fields.id(a), fields.id(b)))
    });
    songs
}

/// A song with no track number goes after the numbered ones.
fn track_or_last(track: Option<i32>) -> i32 {
    match track {
        Some(track) if track > 0 => track,
        _ => i32::MAX,
    }
}

pub fn sort_key(text: Option<&str>) -> String {
    to_lower_invariant(text.unwrap_or("").trim())
}

/// Sets of songs joined as they are found to belong together, by index.
struct Sets {
    parent: Vec<usize>,
    sizes: Vec<usize>,
}

impl Sets {
    fn new(count: usize) -> Self {
        Sets {
            parent: (0..count).collect(),
            sizes: vec![1; count],
        }
    }

    fn root(&mut self, i: usize) -> usize {
        let mut at = i;
        while self.parent[at] != at {
            self.parent[at] = self.parent[self.parent[at]];
            at = self.parent[at];
        }
        at
    }

    fn same(&mut self, a: usize, b: usize) -> bool {
        self.root(a) == self.root(b)
    }

    fn size(&mut self, i: usize) -> usize {
        let root = self.root(i);
        self.sizes[root]
    }

    /// Joins the two sets; false when they were one already.
    fn join(&mut self, a: usize, b: usize) -> bool {
        let (x, y) = (self.root(a), self.root(b));
        if x == y {
            return false;
        }
        if self.sizes[x] < self.sizes[y] {
            self.parent[x] = y;
            self.sizes[y] += self.sizes[x];
        } else {
            self.parent[y] = x;
            self.sizes[x] += self.sizes[y];
        }
        true
    }
}
