//! Plain song lists: one song per line, written "Artist - Title". What a TXT export from a
//! playlist tool, an old list kept in a notes app, or a list pasted into a box looks like.
//!
//! - Blank lines and lines starting with `#` are skipped.
//! - `#playlist: Name` starts a new list; the lines before the first one form a list named
//!   after the file.
//! - A leading track number ("1. ", "02) ") is dropped.
//! - The line splits on its first spaced dash (` - `, ` – `, ` — `) or its first tab. A line
//!   with neither is a title alone.
//!
//! Which half is the artist is decided once for the whole file: Artist - Title unless
//! [`choose_orientation`] finds the other way round matches more of a library.

use std::sync::LazyLock;

use regex::Regex;

use super::import_file_reader::{
    ARTIST_COLUMNS, FileList, FileReadError, ImportFileReader, TITLE_COLUMNS, track,
};
use super::{ImportKeys, ImportTrack};

/// Which half of a line is the artist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// "Artist - Title", the usual way round.
    #[default]
    ArtistTitle,
    /// "Title - Artist".
    TitleArtist,
}

/// One song line as written: the text before the separator and the text after it, or the
/// whole line when it has no separator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLine {
    pub left: String,
    /// None for a line with no separator, which names a title alone.
    pub right: Option<String>,
}

impl TextLine {
    /// The artist and the title the line names, read the given way round. A title alone has
    /// an empty artist.
    pub fn artist_and_title(&self, orientation: Orientation) -> (&str, &str) {
        match (&self.right, orientation) {
            (None, _) => ("", self.left.as_str()),
            (Some(right), Orientation::ArtistTitle) => (self.left.as_str(), right.as_str()),
            (Some(right), Orientation::TitleArtist) => (right.as_str(), self.left.as_str()),
        }
    }
}

/// One list of a text file: its name and its song lines, not yet read either way round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextList {
    pub name: String,
    pub lines: Vec<TextLine>,
}

/// How many lines [`choose_orientation`] tries each way round.
pub const ORIENTATION_SAMPLE: usize = 50;

pub const NO_SONGS: &str = "No songs found. Each line should read Artist - Title.";

/// "1. ", "02) " in front of a line.
static TRACK_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+[.)]\s*").expect("a fixed pattern"));

/// The first spaced hyphen, en dash or em dash, or the first tab.
static SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]+[-\u{2013}\u{2014}][ \t]+|\t").expect("a fixed pattern"));

/// The first spaced hyphen, en dash or em dash.
static SPACED_DASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]+[-\u{2013}\u{2014}][ \t]+").expect("a fixed pattern"));

/// "Artist - Title" split on its first spaced dash into the artist and the title, or None when
/// it has no spaced dash or either side is empty.
pub fn split_artist_title(text: &str) -> Option<(&str, &str)> {
    let at = SPACED_DASH.find(text)?;
    let (artist, title) = (text[..at.start()].trim(), text[at.end()..].trim());
    (!artist.is_empty() && !title.is_empty()).then_some((artist, title))
}

/// `#playlist: Name`, in any case.
static PLAYLIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^#\s*playlist\s*:(.*)$").expect("a fixed pattern"));

/// The lists in a text file, read Artist - Title.
pub fn read_text(name: &str, text: &str) -> Result<Vec<FileList>, FileReadError> {
    Ok(to_file_lists(read_lines(name, text)?, Orientation::ArtistTitle))
}

/// The lists in a text file as lines, before deciding which half is the artist. Refuses a
/// file with no song line, and one with more songs than a file may hold.
pub fn read_lines(name: &str, text: &str) -> Result<Vec<TextList>, FileReadError> {
    let mut lists: Vec<TextList> = vec![TextList {
        name: name.to_string(),
        lines: Vec::new(),
    }];
    let mut current = 0usize;
    let mut songs = 0usize;
    let mut first_song = true;
    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        if let Some(caps) = PLAYLIST.captures(line) {
            let list_name = caps[1].trim();
            if !list_name.is_empty() {
                current = match lists.iter().position(|l| l.name == list_name) {
                    Some(at) => at,
                    None => {
                        lists.push(TextList {
                            name: list_name.to_string(),
                            lines: Vec::new(),
                        });
                        lists.len() - 1
                    }
                };
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let Some(parsed) = parse_line(line) else {
            continue;
        };
        // A first line naming the columns ("Artist - Title", "Title<tab>Artist") is a header.
        if std::mem::take(&mut first_song) && is_header(&parsed) {
            continue;
        }
        songs += 1;
        if songs > ImportFileReader::MAX_SONGS {
            return Err(FileReadError::Format(
                ImportFileReader::TOO_MANY_SONGS.to_string(),
            ));
        }
        lists[current].lines.push(parsed);
    }
    lists.retain(|list| !list.lines.is_empty());
    if lists.is_empty() {
        return Err(FileReadError::Format(NO_SONGS.to_string()));
    }
    Ok(lists)
}

/// The songs of text lists, each line read the given way round.
pub fn to_file_lists(lists: Vec<TextList>, orientation: Orientation) -> Vec<FileList> {
    lists
        .into_iter()
        .map(|list| FileList {
            name: list.name,
            tracks: list
                .lines
                .iter()
                .filter_map(|line| line_track(line, orientation))
                .collect(),
        })
        .filter(|list| !list.tracks.is_empty())
        .collect()
}

/// Which way round a file's lines are written: the first [`ORIENTATION_SAMPLE`] lines with
/// both halves are tried as Artist - Title and as Title - Artist, and the reading that
/// `is_known(artist, title)` accepts more often wins. A tie, or nothing known either way, is
/// Artist - Title.
pub fn choose_orientation(lines: &[TextLine], is_known: impl Fn(&str, &str) -> bool) -> Orientation {
    let (mut forward, mut backward) = (0usize, 0usize);
    for line in lines
        .iter()
        .filter(|line| line.right.is_some())
        .take(ORIENTATION_SAMPLE)
    {
        let (artist, title) = line.artist_and_title(Orientation::ArtistTitle);
        if is_known(artist, title) {
            forward += 1;
        }
        let (artist, title) = line.artist_and_title(Orientation::TitleArtist);
        if is_known(artist, title) {
            backward += 1;
        }
    }
    if backward > forward {
        Orientation::TitleArtist
    } else {
        Orientation::ArtistTitle
    }
}

/// One line split into its halves, or None when nothing is left of it.
fn parse_line(line: &str) -> Option<TextLine> {
    let unnumbered = TRACK_NUMBER.replace(line, "");
    let line = if unnumbered.trim().is_empty() {
        line
    } else {
        unnumbered.trim()
    };
    let (left, right) = match SEPARATOR.find(line) {
        Some(at) => (line[..at.start()].trim(), Some(line[at.end()..].trim())),
        None => (line, None),
    };
    match (left.is_empty(), right.filter(|r| !r.is_empty())) {
        (false, Some(right)) => Some(TextLine {
            left: left.to_string(),
            right: Some(right.to_string()),
        }),
        (false, None) => Some(TextLine {
            left: left.to_string(),
            right: None,
        }),
        (true, Some(right)) => Some(TextLine {
            left: right.to_string(),
            right: None,
        }),
        (true, None) => None,
    }
}

fn is_header(line: &TextLine) -> bool {
    let Some(right) = &line.right else {
        return false;
    };
    let word = |text: &str| text.trim().to_lowercase();
    let (left, right) = (word(&line.left), word(right));
    let title = |w: &str| TITLE_COLUMNS.contains(&w);
    let artist = |w: &str| ARTIST_COLUMNS.contains(&w);
    (artist(&left) && title(&right)) || (title(&left) && artist(&right))
}

fn line_track(line: &TextLine, orientation: Orientation) -> Option<ImportTrack> {
    let (artist, title) = line.artist_and_title(orientation);
    if artist.is_empty() {
        // A title alone: kept with no artist, so the list shows it and the matcher can still
        // look for it by title.
        return Some(ImportTrack {
            key: ImportKeys::for_words(Some(""), Some(title)),
            title: title.to_string(),
            ..Default::default()
        });
    }
    track(Some(title), Some(artist), None, None, None, None)
}

#[cfg(test)]
#[path = "text_list_tests.rs"]
mod tests;
