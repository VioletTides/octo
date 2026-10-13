//! The pure half of the Lidarr client: its records, the strict readers for Lidarr's JSON (a
//! value of the wrong kind is an error), and the album choice. The HTTP side is
//! `octo::services::lidarr::lidarr_client`.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::common::dotnet;
use crate::common::{ArtistAgreement, SongIdentity};

/// What can go wrong talking to Lidarr, by kind, so callers can branch on the kind. Each
/// carries its message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LidarrError {
    /// Not set up, no single album, an answer of the wrong shape, or a JSON value of the wrong
    /// kind.
    #[error("{0}")]
    InvalidOperation(String),
    /// Lidarr answered with an error status, or could not be reached.
    #[error("{0}")]
    Http(String),
    /// The client's timeout, or the caller's cancellation.
    #[error("{0}")]
    Canceled(String),
    /// An answer that is not JSON.
    #[error("{0}")]
    Json(String),
    /// Two track files with one id.
    #[error("{0}")]
    Argument(String),
    /// The track fetcher found no album, or no good enough copy.
    #[error("{0}")]
    FileNotFound(String),
    /// Copying the file out failed.
    #[error("{0}")]
    Io(String),
}

impl LidarrError {
    /// The caller canceled the operation.
    pub fn operation_canceled() -> Self {
        LidarrError::Canceled("The operation was canceled.".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LidarrChoice {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LidarrRootFolder {
    pub id: i32,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LidarrOptions {
    pub root_folders: Vec<LidarrRootFolder>,
    pub quality_profiles: Vec<LidarrChoice>,
    pub metadata_profiles: Vec<LidarrChoice>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LidarrAlbumCandidate {
    pub id: i32,
    pub foreign_album_id: String,
    pub title: String,
    pub artist: String,
    pub year: Option<i32>,
    /// The lookup row as Lidarr sent it, which is what adding the album posts back.
    pub resource: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LidarrImportedTrack {
    pub id: i32,
    pub title: String,
    pub track_number: Option<i32>,
    pub duration_seconds: Option<i32>,
    pub has_file: bool,
    pub path: Option<String>,
    pub size_bytes: i64,
    pub artist: Option<String>,
    /// 0 when the track has no file.
    pub track_file_id: i32,
    pub quality: Option<String>,
}

/// An album Lidarr already has, as it stood before Octo touched it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LidarrAlbumState {
    pub id: i32,
    pub monitored: bool,
}

/// A search Lidarr accepted, and whether Octo added or monitored the album to get it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LidarrSearchStarted {
    pub album_id: i32,
    pub existed: bool,
    pub was_monitored: bool,
}

/// One release an indexer offered for an album, as Lidarr's interactive search lists it.
/// `rejected` says Lidarr would not take it on its own, and `rejections` says why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LidarrRelease {
    pub guid: String,
    pub indexer_id: i32,
    pub title: String,
    pub indexer: Option<String>,
    pub quality: Option<String>,
    pub size: Option<i64>,
    pub seeders: Option<i32>,
    pub leechers: Option<i32>,
    pub protocol: Option<String>,
    pub age_days: Option<i32>,
    pub rejected: bool,
    pub rejections: Vec<String>,
}

/// A release someone picked in Find songs, grabbed instead of Lidarr's own search. Its title
/// and size find it again when Lidarr has forgotten the search that listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LidarrReleasePick {
    pub guid: String,
    pub indexer_id: i32,
    pub title: Option<String>,
    pub size: Option<i64>,
}

/// What a picked release no indexer offers any more is told as.
pub const PICK_GONE_TEXT: &str =
    "The release you picked is no longer offered by the indexers. Search again and pick another.";

/// What grabbing a picked release says when Lidarr refuses the release found again.
pub const PICK_REFUSED_TEXT: &str =
    "Lidarr would not take the release you picked. Search again and pick another.";

/// Whether a release is the picked one in a newer listing: the same indexer and title, and the
/// same size when both say.
pub fn same_release(release: &LidarrRelease, pick: &LidarrReleasePick) -> bool {
    release.indexer_id == pick.indexer_id
        && pick.title.as_deref().is_some_and(|title| {
            !dotnet::is_blank(title) && dotnet::eq_ignore_case(release.title.trim(), title.trim())
        })
        && (release.size.is_none() || pick.size.is_none() || release.size == pick.size)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LidarrAlbumImportState {
    pub tracks: Vec<LidarrImportedTrack>,
    pub track_count: i32,
    pub track_file_count: i32,
}

impl LidarrAlbumImportState {
    pub fn is_complete(&self) -> bool {
        self.track_count > 0 && self.track_file_count >= self.track_count
    }
}

// --- Reading Lidarr's JSON strictly. A missing property and a JSON null both read as absent;
// a value of another kind is an error, never read as absent. ---

fn kind_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "Null",
        Value::Bool(true) => "True",
        Value::Bool(false) => "False",
        Value::Number(_) => "Number",
        Value::String(_) => "String",
        Value::Array(_) => "Array",
        Value::Object(_) => "Object",
    }
}

fn cannot_convert(value: &Value, type_name: &str) -> LidarrError {
    match value {
        Value::Array(_) | Value::Object(_) => {
            LidarrError::InvalidOperation("The node must be of type 'JsonValue'.".to_string())
        }
        _ => LidarrError::InvalidOperation(format!(
            "An element of type '{}' cannot be converted to a '{type_name}'.",
            kind_name(value)
        )),
    }
}

/// The property, or None when it is missing or null.
pub fn property<'a>(o: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    o.get(key).filter(|v| !v.is_null())
}

/// The property of a node that may not be an object: anything but an object is an error.
pub fn child<'a>(node: &'a Value, key: &str) -> Result<Option<&'a Value>, LidarrError> {
    match node {
        Value::Object(o) => Ok(property(o, key)),
        _ => Err(LidarrError::InvalidOperation(
            "The node must be of type 'JsonObject'.".to_string(),
        )),
    }
}

/// The property when it is an object, else None.
pub fn object<'a>(o: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    o.get(key).and_then(Value::as_object)
}

/// The value as a string; any other kind is an error.
pub fn value_str(value: &Value) -> Result<&str, LidarrError> {
    value
        .as_str()
        .ok_or_else(|| cannot_convert(value, "System.String"))
}

/// The property as a string, None when it is missing or null.
pub fn nullable_str(o: &Map<String, Value>, key: &str) -> Result<Option<String>, LidarrError> {
    property(o, key)
        .map(|v| value_str(v).map(str::to_string))
        .transpose()
}

/// The property as a string, empty when it is missing or null.
pub fn str_or_empty(o: &Map<String, Value>, key: &str) -> Result<String, LidarrError> {
    Ok(nullable_str(o, key)?.unwrap_or_default())
}

/// The property as an `i32`, None when it is missing or null. A number with a fraction or out
/// of range is an error.
pub fn nullable_int(o: &Map<String, Value>, key: &str) -> Result<Option<i32>, LidarrError> {
    property(o, key)
        .map(|v| {
            v.as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| cannot_convert(v, "System.Int32"))
        })
        .transpose()
}

/// The property as an `i32`, 0 when it is missing or null.
pub fn int_or_zero(o: &Map<String, Value>, key: &str) -> Result<i32, LidarrError> {
    Ok(nullable_int(o, key)?.unwrap_or(0))
}

/// The property as an `i64`, None when it is missing or null.
pub fn nullable_long(o: &Map<String, Value>, key: &str) -> Result<Option<i64>, LidarrError> {
    property(o, key)
        .map(|v| v.as_i64().ok_or_else(|| cannot_convert(v, "System.Int64")))
        .transpose()
}

/// The property as a bool, None when it is missing or null.
pub fn nullable_bool(o: &Map<String, Value>, key: &str) -> Result<Option<bool>, LidarrError> {
    property(o, key)
        .map(|v| v.as_bool().ok_or_else(|| cannot_convert(v, "System.Boolean")))
        .transpose()
}

/// The server's root folders and profiles.
pub fn map_options(
    roots: &[Map<String, Value>],
    quality: &[Map<String, Value>],
    metadata: &[Map<String, Value>],
) -> Result<LidarrOptions, LidarrError> {
    let choice = |x: &Map<String, Value>| -> Result<LidarrChoice, LidarrError> {
        Ok(LidarrChoice {
            id: int_or_zero(x, "id")?,
            name: str_or_empty(x, "name")?,
        })
    };
    Ok(LidarrOptions {
        root_folders: roots
            .iter()
            .map(|x| {
                Ok(LidarrRootFolder {
                    id: int_or_zero(x, "id")?,
                    path: str_or_empty(x, "path")?,
                })
            })
            .collect::<Result<_, LidarrError>>()?,
        quality_profiles: quality.iter().map(choice).collect::<Result<_, _>>()?,
        metadata_profiles: metadata.iter().map(choice).collect::<Result<_, _>>()?,
    })
}

/// One row of an album lookup, its year from the first four characters of its release date.
pub fn parse_album(row: &Map<String, Value>) -> Result<LidarrAlbumCandidate, LidarrError> {
    let artist = object(row, "artist");
    let date = nullable_str(row, "releaseDate")?;
    let year = date.as_deref().and_then(|d| {
        // The first four UTF-16 units; a cut surrogate pair is no number either.
        if dotnet::utf16_len(d) < 4 {
            return None;
        }
        let units: Vec<u16> = d.encode_utf16().take(4).collect();
        String::from_utf16(&units)
            .ok()
            .and_then(|head| int_try_parse(&head))
    });
    Ok(LidarrAlbumCandidate {
        id: nullable_int(row, "id")?.unwrap_or(0),
        foreign_album_id: nullable_str(row, "foreignAlbumId")?.unwrap_or_default(),
        title: nullable_str(row, "title")?.unwrap_or_default(),
        artist: match artist {
            Some(a) => nullable_str(a, "artistName")?.unwrap_or_default(),
            None => String::new(),
        },
        year,
        resource: row.clone(),
    })
}

/// A whole number however Lidarr wrote it: read as a double, then rounded half to even.
/// Anything but a number is None.
fn nullable_number(o: &Map<String, Value>, key: &str) -> Option<i32> {
    o.get(key)
        .and_then(Value::as_f64)
        .map(|n| n.round_ties_even() as i32)
}

/// One row of `GET /api/v1/release`.
pub fn parse_release(row: &Map<String, Value>) -> Result<LidarrRelease, LidarrError> {
    let quality = object(row, "quality").and_then(|q| object(q, "quality"));
    let age_days = match row.get("ageHours").and_then(Value::as_f64) {
        Some(hours) => Some((hours / 24.0).round_ties_even() as i32),
        None => nullable_number(row, "age"),
    };
    let rejections = match row.get("rejections") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => Ok(String::new()),
                other => value_str(other).map(str::to_string),
            })
            .filter(|r| r.as_ref().map_or(true, |r| !r.is_empty()))
            .collect::<Result<Vec<_>, _>>()?,
        _ => Vec::new(),
    };
    Ok(LidarrRelease {
        guid: nullable_str(row, "guid")?.unwrap_or_default(),
        indexer_id: nullable_number(row, "indexerId").unwrap_or(0),
        title: nullable_str(row, "title")?.unwrap_or_default(),
        indexer: nullable_str(row, "indexer")?,
        quality: match quality {
            Some(q) => nullable_str(q, "name")?,
            None => None,
        },
        size: row.get("size").and_then(Value::as_f64).map(|bytes| bytes as i64),
        seeders: nullable_number(row, "seeders"),
        leechers: nullable_number(row, "leechers"),
        protocol: nullable_str(row, "protocol")?,
        age_days,
        rejected: row.get("rejected").and_then(Value::as_bool).unwrap_or(false),
        rejections,
    })
}

/// The album whose title reads the same as asked, by [`SongIdentity::key`], credited to the
/// artist asked for by [`SongIdentity::compare_artists`] (a shared artist and no clash of guests:
/// "Jay-Z & Kanye West" for "Jay-Z", "Kanye West" for "Ye", never "Airbourne" for "Air");
/// several releases of it are told apart by the year. None when there is no single one.
pub fn select_best_album<'a>(
    candidates: &'a [LidarrAlbumCandidate],
    artist: &str,
    album: &str,
    year: Option<i32>,
) -> Option<&'a LidarrAlbumCandidate> {
    let wanted_album = SongIdentity::key(album);
    let mut exact: Vec<&LidarrAlbumCandidate> = Vec::new();
    for c in candidates {
        if matches!(
            SongIdentity::compare_artists(&c.artist, artist),
            ArtistAgreement::Agree | ArtistAgreement::Loose
        ) && SongIdentity::key(&c.title) == wanted_album
            && !exact
                .iter()
                .any(|e| dotnet::eq_ignore_case(&e.foreign_album_id, &c.foreign_album_id))
        {
            exact.push(c);
        }
    }
    if exact.len() == 1 {
        return Some(exact[0]);
    }
    if exact.is_empty() {
        return None;
    }

    if let Some(y) = year {
        let same_year: Vec<_> = exact.iter().filter(|c| c.year == Some(y)).collect();
        if same_year.len() == 1 {
            return Some(same_year[0]);
        }
    }

    // Multiple releases with the same display identity are unsafe to choose
    // automatically. A wrong album is much worse than an actionable failure.
    None
}

/// A track number from "3", "1-3", "A/2" and the like, by the part before the first `-`, `/`
/// or `.`.
pub fn parse_track_number(value: &str) -> Option<i32> {
    let head = value.split(['-', '/', '.']).next().unwrap_or("");
    int_try_parse(head)
}

/// An `i32` from ASCII digits, allowing white space around it and a leading sign.
fn int_try_parse(s: &str) -> Option<i32> {
    let is_white = |c: char| matches!(c, '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ');
    let s = s.trim_matches(is_white);
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut n: i64 = 0;
    for b in digits.bytes() {
        n = n.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
        if n > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    i32::try_from(if negative { -n } else { n }).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn candidate(
        foreign_id: &str,
        title: &str,
        artist: &str,
        year: Option<i32>,
    ) -> LidarrAlbumCandidate {
        let resource = json!({
            "foreignAlbumId": foreign_id,
            "title": title,
            "releaseDate": year.map(|y| format!("{y}-01-01T00:00:00Z")),
            "artist": { "artistName": artist, "foreignArtistId": "artist-mbid" },
        });
        LidarrAlbumCandidate {
            id: 0,
            foreign_album_id: foreign_id.into(),
            title: title.into(),
            artist: artist.into(),
            year,
            resource: resource.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn album_selection_requires_exact_identity_and_uses_year_to_disambiguate() {
        let candidates = [
            candidate("a", "In Rainbows", "Radiohead", Some(2007)),
            candidate("b", "In Rainbows", "Radiohead", Some(2025)),
        ];

        assert_eq!(
            select_best_album(&candidates, "RADIOHEAD", "In-Rainbows", Some(2007))
                .map(|c| c.foreign_album_id.as_str()),
            Some("a")
        );
        assert!(select_best_album(&candidates, "Radiohead", "In Rainbows", None).is_none());
        assert!(select_best_album(&candidates, "Other", "In Rainbows", Some(2007)).is_none());
    }

    #[test]
    fn album_artist_is_compared_as_a_credit() {
        let duo = [candidate(
            "a",
            "Bridge over Troubled Water",
            "Simon & Garfunkel",
            Some(1970),
        )];
        assert!(select_best_album(&duo, "Simon & Garfunkel", "Bridge over Troubled Water", None).is_some());
        // A credit with a guest is the lead artist's album.
        let guest = [candidate(
            "w",
            "Watch the Throne",
            "Jay-Z & Kanye West",
            Some(2011),
        )];
        assert!(select_best_album(&guest, "Jay-Z", "Watch the Throne", None).is_some());
        // An alias is the same artist.
        let alias = [candidate("d", "Donda", "Kanye West", Some(2021))];
        assert!(select_best_album(&alias, "Ye", "Donda", None).is_some());
        // A name inside another name is not.
        let other = [candidate("s", "Sexy Boy", "Airbourne", Some(1998))];
        assert!(select_best_album(&other, "Air", "Sexy Boy", None).is_none());
    }

    /// Two rows of one release group are one album, compared ignoring case.
    #[test]
    fn one_release_group_listed_twice_is_one_album() {
        let candidates = [
            candidate("RG", "In Rainbows", "Radiohead", Some(2007)),
            candidate("rg", "In Rainbows", "Radiohead", Some(2008)),
        ];
        assert_eq!(
            select_best_album(&candidates, "Radiohead", "In Rainbows", None).map(|c| c.year),
            Some(Some(2007))
        );
    }

    #[test]
    fn track_numbers_and_years_parse_as_plain_integers() {
        for (text, expected) in [
            ("3", Some(3)),
            ("1-3", Some(1)),
            ("2/12", Some(2)),
            (" 4 ", Some(4)),
            ("+5", Some(5)),
            ("A1", None),
            ("", None),
            ("-1", None),
            ("2147483648", None),
        ] {
            assert_eq!(parse_track_number(text), expected, "{text:?}");
        }
        for (date, year) in [
            (Some("1998-04-20"), Some(1998)),
            (Some("199"), None),
            (Some(" 199"), Some(199)),
            (Some("19é8"), None),
            (None, None),
        ] {
            let row = json!({ "releaseDate": date, "foreignAlbumId": "x" });
            let parsed = parse_album(row.as_object().expect("an object")).expect("a row");
            assert_eq!(parsed.year, year, "{date:?}");
        }
    }

    /// A value of another kind is an error, rather than reading as missing.
    #[test]
    fn a_value_of_the_wrong_kind_is_an_error() {
        let row = json!({ "id": "7", "title": 5, "size": 1.5, "ok": null });
        let row = row.as_object().expect("an object");
        assert_eq!(
            nullable_int(row, "id"),
            Err(LidarrError::InvalidOperation(
                "An element of type 'String' cannot be converted to a 'System.Int32'.".into()
            ))
        );
        assert!(nullable_str(row, "title").is_err());
        assert!(nullable_long(row, "size").is_err());
        assert_eq!(nullable_int(row, "ok"), Ok(None));
        assert_eq!(int_or_zero(row, "missing"), Ok(0));
    }

    // Completion reads the album's statistics, not alternate release rows (the HTTP half is in
    // octo's client tests).
    #[test]
    fn an_album_is_complete_once_every_track_has_a_file() {
        let state = |count, files| LidarrAlbumImportState {
            tracks: Vec::new(),
            track_count: count,
            track_file_count: files,
        };
        assert!(state(1, 1).is_complete());
        assert!(!state(2, 1).is_complete());
        assert!(!state(0, 0).is_complete());
    }
}
