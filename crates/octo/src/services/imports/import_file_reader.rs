//! Reads imported lists from exported files.

use std::io::{Cursor, Read};

use octo_core::common::dotnet::{self, is_blank, to_lower_invariant};
use octo_core::common::song_identity::SongIdentity;
use serde_json::Value;

use super::ImportTrack;
use super::spotify_web_api::{ImportKeys, str_of};
use super::text_list;
use crate::controllers::admin::json_body::stj_parse_error;

/// A list a file held: its name and songs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileList {
    pub name: String,
    pub tracks: Vec<ImportTrack>,
}

/// What a file held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRead {
    /// Lists whose columns say which text is the artist.
    Lists(Vec<FileList>),
    /// A plain list's lines, either way round.
    Text(Vec<text_list::TextList>),
}

/// Why a file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileReadError {
    /// Octo's own words, shown as they are.
    #[error("{0}")]
    Format(String),
    /// The JSON parser's words.
    #[error("{0}")]
    Json(String),
    /// The zip reader's words.
    #[error("{0}")]
    InvalidData(String),
}

/// Lists from files other tools write, for when there is no Spotify sign-in, or a playlist the
/// sign-in cannot read in full:
///
/// - CSV from Exportify, TuneMyMusic, Soundiiz and the like. Columns are found by their header,
///   so any CSV with a title and an artist column works; album, length and ISRC are read when
///   there.
/// - Spotify's own "Download your data" export (Account, Privacy): YourLibrary.json holds the
///   liked songs and Playlist1.json (and on) every playlist, whole. The zip Spotify sends works
///   as it is.
/// - A plain list, one "Artist - Title" per line ([`super::text_list`]).
pub struct ImportFileReader;

impl ImportFileReader {
    /// Bigger than any real export, small enough that a wrong file cannot fill memory.
    pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

    /// Songs one file may hold, across its lists: far past any real library export.
    pub const MAX_SONGS: usize = 50_000;

    /// Files a zip may hold. Spotify's data export is a few dozen.
    pub const MAX_ZIP_ENTRIES: usize = 200;

    /// What a zip's files may come to once unpacked, all together.
    pub const MAX_UNZIPPED_BYTES: u64 = 256 * 1024 * 1024;

    pub const TOO_MANY_SONGS: &str = "It holds more than 50,000 songs. Split it into smaller files.";

    /// The lists in a file, a plain list read Artist - Title.
    pub fn read(file_name: &str, content: &[u8]) -> Result<Vec<FileList>, FileReadError> {
        Ok(match Self::read_parts(file_name, content)? {
            FileRead::Lists(lists) => lists,
            FileRead::Text(lists) => text_list::to_file_lists(lists, text_list::Orientation::ArtistTitle),
        })
    }

    /// What a file holds: its lists, or a plain list's lines, whose orientation the caller
    /// decides ([`text_list::choose_orientation`]).
    pub fn read_parts(file_name: &str, content: &[u8]) -> Result<FileRead, FileReadError> {
        let read = read_any(file_name, content)?;
        if let FileRead::Lists(lists) = &read
            && song_count(lists) > Self::MAX_SONGS
        {
            return Err(FileReadError::Format(Self::TOO_MANY_SONGS.to_string()));
        }
        Ok(read)
    }

    /// A length in milliseconds, seconds, or m:ss.
    pub fn seconds(value: Option<&str>, milliseconds: bool) -> Option<i32> {
        let value = value.filter(|v| !is_blank(v))?;
        if value.contains(':') {
            let parts: Vec<i32> = value
                .split(':')
                .map(|part| int_try_parse(part).unwrap_or(-1))
                .collect();
            if parts.iter().any(|n| *n < 0) {
                return None;
            }
            return Some(
                parts
                    .iter()
                    .fold(0i32, |total, part| total.wrapping_mul(60).wrapping_add(*part)),
            );
        }
        let number = value.trim().parse::<f64>().ok().filter(|n| n.is_finite())?;
        if number <= 0.0 {
            return None;
        }
        // A bare number above an hour is surely milliseconds, whatever the column says.
        let seconds = if milliseconds || number > 3600.0 {
            number / 1000.0
        } else {
            number
        };
        Some(dotnet::round(seconds, 0) as i32)
    }
}

fn song_count(lists: &[FileList]) -> usize {
    lists.iter().map(|list| list.tracks.len()).sum()
}

/// A whole number in `i32` range: white space around allowed, an optional sign, then only digits.
fn int_try_parse(text: &str) -> Option<i32> {
    let text = text.trim();
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i32>().ok()
}

/// The file name of a path: what follows the last `/` (the only separator on Linux).
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The extension of a file name: from the last dot, when there is one and it is not the last
/// character; otherwise empty.
fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot + 1 < name.len() => &name[dot..],
        _ => "",
    }
}

/// A file name without its extension: everything before the last dot, or the whole name.
fn without_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) => &name[..dot],
        None => name,
    }
}

/// The file's text: UTF-8 by default, or UTF-16 when a byte order mark says so (a UTF-8 mark is
/// dropped). Invalid bytes become replacement characters.
fn decode(content: &[u8]) -> String {
    if let Some(rest) = content.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf16 = |rest: &[u8], little: bool| {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|pair| {
                let (a, b) = (pair[0], pair.get(1).copied().unwrap_or(0));
                if little {
                    u16::from_le_bytes([a, b])
                } else {
                    u16::from_be_bytes([a, b])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = content.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = content.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    String::from_utf8_lossy(content).into_owned()
}

fn read_any(file_name_given: &str, content: &[u8]) -> Result<FileRead, FileReadError> {
    let name = file_name(file_name_given);
    let extension = to_lower_invariant(extension(name));
    if extension == ".zip" {
        return read_zip(content).map(FileRead::Lists);
    }
    let text = decode(content);
    let start = text.trim_start();
    if extension == ".json" || start.starts_with('{') || start.starts_with('[') {
        return read_json(without_extension(name), &text).map(FileRead::Lists);
    }
    if is_text_list(&extension, &text) {
        return text_list::read_lines(without_extension(name), &text).map(FileRead::Text);
    }
    read_csv(without_extension(name), &text).map(FileRead::Lists)
}

/// A `.csv` file is a table. Any other file, a `.txt` one included, is a table only when its
/// first line names a title or an artist column among commas or semicolons (a table some tool
/// saved as text), and a plain list otherwise: a comma alone proves nothing, since "Earth, Wind
/// & Fire - September" has one.
fn is_text_list(extension: &str, text: &str) -> bool {
    if extension == ".csv" {
        return false;
    }
    match text.lines().find(|line| !line.trim().is_empty()) {
        Some(first) => !is_csv_header(first),
        // Nothing to go by: a text file reads as a list, anything else as a table.
        None => extension == ".txt",
    }
}

fn is_csv_header(line: &str) -> bool {
    if !line.contains([',', ';']) {
        return false;
    }
    line.split([',', ';']).any(|cell| {
        let name = to_lower_invariant(cell.trim().trim_matches('"').trim());
        TITLE_COLUMNS.contains(&name.as_str()) || ARTIST_COLUMNS.contains(&name.as_str())
    })
}

fn read_zip(content: &[u8]) -> Result<Vec<FileList>, FileReadError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(content)).map_err(zip_error)?;
    if zip.len() > ImportFileReader::MAX_ZIP_ENTRIES {
        return Err(FileReadError::Format(format!(
            "The zip holds more than {} files, more than any export Octo reads.",
            ImportFileReader::MAX_ZIP_ENTRIES
        )));
    }
    let mut entries: Vec<(String, usize, u64)> = Vec::new();
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(zip_error)?;
        entries.push((entry.name().to_string(), index, entry.size()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    // Counted as the files unpack, never from the sizes the zip claims for them.
    let mut budget: u64 = 0;
    let mut lists = Vec::new();
    for (full_name, index, size) in entries {
        let file = file_name(&full_name).to_string();
        let lower = to_lower_invariant(&file);
        let wanted = lower == "yourlibrary.json"
            || (lower.starts_with("playlist") && lower.ends_with(".json"))
            || lower.ends_with(".csv");
        if !wanted || size > ImportFileReader::MAX_BYTES {
            continue;
        }
        let mut entry = zip.by_index(index).map_err(zip_error)?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = entry
                .read(&mut buffer)
                .map_err(|e| FileReadError::InvalidData(e.to_string()))?;
            if read == 0 {
                break;
            }
            budget += read as u64;
            if budget > ImportFileReader::MAX_UNZIPPED_BYTES {
                return Err(FileReadError::Format(format!(
                    "The zip unpacks to more than {} MB, more than any export Octo reads.",
                    ImportFileReader::MAX_UNZIPPED_BYTES / (1024 * 1024)
                )));
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        lists.extend(match read_any(&file, &bytes)? {
            FileRead::Lists(read) => read,
            FileRead::Text(read) => text_list::to_file_lists(read, text_list::Orientation::ArtistTitle),
        });
        if song_count(&lists) > ImportFileReader::MAX_SONGS {
            return Err(FileReadError::Format(
                ImportFileReader::TOO_MANY_SONGS.to_string(),
            ));
        }
    }
    Ok(lists)
}

/// The zip reader's refusal: an archive that is not a zip reads as one fixed message.
fn zip_error(error: zip::result::ZipError) -> FileReadError {
    match error {
        zip::result::ZipError::InvalidArchive(_) | zip::result::ZipError::Io(_) => {
            FileReadError::InvalidData("End of Central Directory record could not be found.".to_string())
        }
        other => FileReadError::InvalidData(other.to_string()),
    }
}

// ---- Spotify's data export ----------------------------------------------------------------

pub(crate) fn read_json(name: &str, text: &str) -> Result<Vec<FileList>, FileReadError> {
    if let Some(error) = stj_parse_error(text) {
        return Err(FileReadError::Json(format!(
            "{} {}",
            error.message,
            error.position()
        )));
    }
    let root: Value = serde_json::from_str(text).map_err(|e| FileReadError::Json(e.to_string()))?;
    let mut lists = Vec::new();
    if !root.is_object() {
        return Ok(lists);
    }

    // YourLibrary.json: { "tracks": [ { "artist", "album", "track", "uri" } ], ... }
    if let Some(liked) = root.get("tracks").and_then(Value::as_array) {
        let tracks: Vec<ImportTrack> = liked
            .iter()
            .filter_map(|row| {
                track(
                    str_of(row, "track").as_deref(),
                    str_of(row, "artist").as_deref(),
                    str_of(row, "album").as_deref(),
                    None,
                    None,
                    str_of(row, "uri").as_deref(),
                )
            })
            .collect();
        if !tracks.is_empty() {
            lists.push(FileList {
                name: "Liked Songs".to_string(),
                tracks,
            });
        }
    }

    // Playlist1.json:
    // { "playlists": [ { "name", "items": [ { "track": {
    //     "trackName", "artistName", "albumName", "trackUri" } } ] } ] }
    if let Some(playlists) = root.get("playlists").and_then(Value::as_array) {
        for playlist in playlists {
            let mut tracks = Vec::new();
            if let Some(items) = playlist.get("items").and_then(Value::as_array) {
                for item in items {
                    let Some(t) = item.get("track").filter(|t| t.is_object()) else {
                        continue;
                    };
                    if let Some(track) = track(
                        str_of(t, "trackName").as_deref(),
                        str_of(t, "artistName").as_deref(),
                        str_of(t, "albumName").as_deref(),
                        None,
                        None,
                        str_of(t, "trackUri").as_deref(),
                    ) {
                        tracks.push(track);
                    }
                }
            }
            if !tracks.is_empty() {
                lists.push(FileList {
                    name: str_of(playlist, "name").unwrap_or_else(|| name.to_string()),
                    tracks,
                });
            }
        }
    }
    Ok(lists)
}

// ---- CSV ----------------------------------------------------------------------------------

pub(super) const TITLE_COLUMNS: &[&str] = &[
    "track name",
    "title",
    "track",
    "song",
    "song name",
    "name",
    "track title",
];
pub(super) const ARTIST_COLUMNS: &[&str] = &[
    "artist name(s)",
    "artist names",
    "artist name",
    "artists",
    "artist",
    "artist(s)",
];
const ALBUM_COLUMNS: &[&str] = &["album name", "album", "album title", "release"];
const LENGTH_COLUMNS: &[&str] = &[
    "track duration (ms)",
    "duration (ms)",
    "duration_ms",
    "duration ms",
    "duration",
    "length",
    "time",
];
const ISRC_COLUMNS: &[&str] = &["isrc"];
const SPOTIFY_COLUMNS: &[&str] = &[
    "track uri",
    "spotify - id",
    "spotify uri",
    "spotify id",
    "uri",
    "spotify track id",
];
const PLAYLIST_COLUMNS: &[&str] = &["playlist name", "playlist"];

pub(crate) fn read_csv(name: &str, text: &str) -> Result<Vec<FileList>, FileReadError> {
    // One row past the most a file may hold is enough to know it holds too many.
    let rows: Vec<Vec<String>> = rows(text).take(ImportFileReader::MAX_SONGS + 2).collect();
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    if rows.len() > ImportFileReader::MAX_SONGS + 1 {
        return Err(FileReadError::Format(
            ImportFileReader::TOO_MANY_SONGS.to_string(),
        ));
    }
    let header: Vec<String> = rows[0]
        .iter()
        .map(|cell| to_lower_invariant(cell.trim()))
        .collect();
    let column =
        |names: &[&str]| -> Option<usize> { names.iter().find_map(|n| header.iter().position(|h| h == n)) };
    let (Some(title), Some(artist)) = (column(TITLE_COLUMNS), column(ARTIST_COLUMNS)) else {
        return Err(FileReadError::Format(
            "The file needs a title column and an artist column, named in its first row, or a plain list with one Artist - Title per line."
                .to_string(),
        ));
    };
    let album = column(ALBUM_COLUMNS);
    let length = column(LENGTH_COLUMNS);
    let isrc = column(ISRC_COLUMNS);
    let spotify = column(SPOTIFY_COLUMNS);
    let playlist = column(PLAYLIST_COLUMNS);
    let length_in_ms = length.is_some_and(|l| header[l].contains("ms"));

    fn cell(row: &[String], index: Option<usize>) -> Option<String> {
        let value = row.get(index?)?.trim();
        (!value.is_empty()).then(|| value.to_string())
    }

    // One file can hold several playlists (TuneMyMusic, Soundiiz): a playlist column splits them.
    let mut order: Vec<(String, Vec<ImportTrack>)> = Vec::new();
    for row in rows.iter().skip(1) {
        let (mut title_text, mut artist_text) = (cell(row, Some(title)), cell(row, Some(artist)));
        // A list a tool built from plain text (TuneMyMusic's free text) leaves the artist
        // column empty and keeps the whole "Artist - Title" line in the title column.
        if artist_text.is_none()
            && let Some((a, t)) = title_text.as_deref().and_then(text_list::split_artist_title)
        {
            (title_text, artist_text) = (Some(t.to_string()), Some(a.to_string()));
        }
        let Some(found) = track(
            title_text.as_deref(),
            artist_text.as_deref(),
            cell(row, album).as_deref(),
            ImportFileReader::seconds(cell(row, length).as_deref(), length_in_ms),
            cell(row, isrc).as_deref(),
            cell(row, spotify).as_deref(),
        ) else {
            continue;
        };
        let list = cell(row, playlist).unwrap_or_else(|| name.to_string());
        match order.iter_mut().find(|(n, _)| *n == list) {
            Some((_, tracks)) => tracks.push(found),
            None => order.push((list, vec![found])),
        }
    }
    Ok(order
        .into_iter()
        .map(|(name, tracks)| FileList { name, tracks })
        .collect())
}

/// RFC 4180 rows: quoted cells may hold commas, quotes ("") and line breaks. A semicolon file is
/// read too.
pub(crate) fn rows(text: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    let first_line = text.split('\n').next().unwrap_or("");
    let separator = if first_line.matches(';').count() > first_line.matches(',').count() {
        ';'
    } else {
        ','
    };
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let mut finished = false;
    std::iter::from_fn(move || {
        if finished {
            return None;
        }
        let mut row: Vec<String> = Vec::new();
        let mut cell = String::new();
        let mut quoted = false;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            if quoted {
                if c == '"' && chars.get(i) == Some(&'"') {
                    cell.push('"');
                    i += 1;
                } else if c == '"' {
                    quoted = false;
                } else {
                    cell.push(c);
                }
                continue;
            }
            if c == '"' {
                quoted = true;
            } else if c == separator {
                row.push(std::mem::take(&mut cell));
            } else if c == '\n' || c == '\r' {
                if c == '\r' && chars.get(i) == Some(&'\n') {
                    i += 1;
                }
                row.push(std::mem::take(&mut cell));
                if row.iter().any(|value| !value.is_empty()) {
                    return Some(row);
                }
                row = Vec::new();
            } else {
                cell.push(c);
            }
        }
        finished = true;
        row.push(cell);
        row.iter().any(|value| !value.is_empty()).then_some(row)
    })
}

pub(super) fn track(
    title: Option<&str>,
    artist: Option<&str>,
    album: Option<&str>,
    seconds: Option<i32>,
    isrc: Option<&str>,
    spotify: Option<&str>,
) -> Option<ImportTrack> {
    let title = title.filter(|t| !is_blank(t))?;
    let artist = artist.filter(|a| !is_blank(a))?;
    // Exportify joins artists with commas, Spotify's export gives one, and some tools use
    // semicolons.
    let credit = artist
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Some(ImportTrack {
        key: ImportKeys::for_spotify(spotify)
            .unwrap_or_else(|| ImportKeys::for_words(Some(&credit), Some(title))),
        title: title.trim().to_string(),
        artist: credit,
        album: album.filter(|a| !is_blank(a)).map(|a| a.trim().to_string()),
        seconds,
        isrc: isrc.and_then(SongIdentity::normalize_isrc),
        library_id: None,
    })
}

#[cfg(test)]
#[path = "import_file_reader_tests.rs"]
mod tests;
