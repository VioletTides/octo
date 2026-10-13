//! The tags an app can read and change on one library file (`SongTagFields`, `LibraryTagEdits`),
//! and what it keeps on disk, `tag-edits.json` (`TagEditJournal`): the tag edits made from the
//! apps (and by the explicit marking), newest last, so each can be undone and a person can see
//! later which files were changed, by whom and how.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use octo_core::common::dotnet::{is_null_or_white_space, utf16_len};
use octo_core::common::song_identity::SongIdentity;
use octo_core::json::datetime;
use octo_media::cover::cover_image;
use octo_media::tags::kept_identity::{KeptIdentity, KeptIdentityTags};
use octo_media::tags::tag_writer_extras::{self as extras, TagFields};
use octo_media::tags::{TagError, TagFile, TagPicture};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::services::state_file::{self, null_as_default};

/// The tags an app can read and change on one library file, by the names the apps send.
pub mod song_tag_fields {
    pub const TITLE: &str = "title";
    pub const ARTIST: &str = "artist";
    pub const ALBUM: &str = "album";
    pub const ALBUM_ARTIST: &str = "albumArtist";
    pub const YEAR: &str = "year";
    pub const GENRE: &str = "genre";
    pub const TRACK: &str = "track";
    pub const DISC: &str = "disc";
    pub const ISRC: &str = "isrc";

    pub const ALL: [&str; 9] = [TITLE, ARTIST, ALBUM, ALBUM_ARTIST, YEAR, GENRE, TRACK, DISC, ISRC];

    /// Never cleared: without them a song is filed under its file name and an unknown artist.
    pub const REQUIRED: [&str; 3] = [TITLE, ARTIST, ALBUM];

    /// Several values in one field are sent and read joined by this.
    pub const SEPARATOR: &str = "; ";

    /// The field's own name for a name sent in any case, or None for a name that is not one.
    pub fn canonical(name: &str) -> Option<&'static str> {
        ALL.into_iter()
            .find(|field| octo_core::common::dotnet::eq_ignore_case(field, name))
    }
}

use song_tag_fields as fields;

/// The kinds of edit, as the journal names them.
pub mod tag_edit_kinds {
    pub const RETAG: &str = "retag";
    pub const JOIN_ALBUM: &str = "joinAlbum";
    pub const COVER: &str = "cover";
    /// The explicit advisory, written by the library-wide explicit marking.
    pub const ADVISORY: &str = "advisory";
}

/// The fields of a file by tag name, in the order the apps list them; None for a field the file
/// does not have.
pub type TagValues = IndexMap<String, Option<String>>;

/// What one edit did to one file, for the answer and for its undo.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TagEditResult {
    pub changed: bool,
    pub error: Option<String>,
    pub before: IndexMap<String, Option<String>>,
    pub after: IndexMap<String, Option<String>>,
}

impl TagEditResult {
    pub fn failed(error: impl Into<String>) -> TagEditResult {
        TagEditResult {
            changed: false,
            error: Some(error.into()),
            before: IndexMap::new(),
            after: IndexMap::new(),
        }
    }

    fn same(before: TagValues) -> TagEditResult {
        TagEditResult {
            changed: false,
            error: None,
            after: before.clone(),
            before,
        }
    }

    fn done(before: TagValues, after: TagValues) -> TagEditResult {
        TagEditResult {
            changed: true,
            error: None,
            before,
            after,
        }
    }
}

// Changes the tags of one library file in place, for the apps' Library health. In place on
// purpose: Navidrome keeps a song's id, and so its plays, favorites and playlist places, for a
// file that stays at the same path whatever its tags say. Only the tag writer touches the file;
// nothing here moves, renames or deletes one.

const MAX_LENGTH: usize = 500;

/// The fields as the file holds them; a missing one is None.
pub fn read(path: &Path) -> Result<TagValues, TagError> {
    let file = TagFile::open(path)?;
    Ok(read_file(&file))
}

/// [`read`] of a file already open.
pub fn read_file(file: &TagFile) -> TagValues {
    // As written, spaces and all: a stray space is what splits an album, so it has to show.
    fn text(value: Option<String>) -> Option<String> {
        value.filter(|value| !is_null_or_white_space(Some(value)))
    }
    fn joined(values: Vec<String>) -> Option<String> {
        if values.is_empty() {
            return None;
        }
        let kept: Vec<String> = values
            .into_iter()
            .filter(|value| !is_null_or_white_space(Some(value)))
            .collect();
        text(Some(kept.join(fields::SEPARATOR)))
    }
    fn number(value: u32) -> Option<String> {
        (value > 0).then(|| value.to_string())
    }
    let mut values = TagValues::new();
    values.insert(fields::TITLE.into(), text(file.title()));
    values.insert(fields::ARTIST.into(), joined(file.performers()));
    values.insert(fields::ALBUM.into(), text(file.album()));
    values.insert(fields::ALBUM_ARTIST.into(), joined(file.album_artists()));
    values.insert(fields::YEAR.into(), number(file.year()));
    values.insert(fields::GENRE.into(), joined(file.genres()));
    values.insert(fields::TRACK.into(), number(file.track()));
    values.insert(fields::DISC.into(), number(file.disc()));
    values.insert(
        fields::ISRC.into(),
        text(extras::read_text(file, TagFields::ISRC).or_else(|| file.isrc())),
    );
    values
}

/// A whole number of ASCII digits only (no sign, no spaces) that fits in an `i32`.
fn plain_number(value: &str) -> Option<i64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<i32>().ok().map(i64::from)
}

/// Why these changes cannot be written, or None when they can. Names are the apps' field names;
/// an empty value clears the field, except for the ones a song cannot do without.
pub fn invalid(changes: &IndexMap<String, Option<String>>) -> Option<String> {
    if changes.is_empty() {
        return Some("No tags were sent to change.".into());
    }
    for (name, raw) in changes {
        let Some(field) = fields::canonical(name) else {
            return Some(format!("\"{name}\" is not a tag this server changes."));
        };
        let value = raw.as_deref().map(str::trim).unwrap_or("");
        if utf16_len(value) > MAX_LENGTH {
            return Some(format!("The {} is too long.", words(field)));
        }
        if value.chars().any(char::is_control) {
            return Some(format!("The {} has characters a tag cannot hold.", words(field)));
        }
        if value.is_empty() {
            if fields::REQUIRED.contains(&field) {
                return Some(format!(
                    "A song needs its {}, so it cannot be left empty.",
                    words(field)
                ));
            }
            continue;
        }
        match field {
            fields::YEAR if !plain_number(value).is_some_and(|year| (1000..=2999).contains(&year)) => {
                return Some(format!("\"{value}\" is not a year."));
            }
            fields::TRACK | fields::DISC
                if !plain_number(value).is_some_and(|number| (1..=999).contains(&number)) =>
            {
                return Some(format!("\"{value}\" is not a {}.", words(field)));
            }
            fields::ISRC if SongIdentity::normalize_isrc(value).is_none() => {
                return Some(format!("\"{value}\" is not an ISRC."));
            }
            _ => {}
        }
    }
    None
}

/// Writes the changes that differ from what the file holds. Check them with [`invalid`] first.
pub fn write(path: &Path, changes: &IndexMap<String, Option<String>>) -> TagEditResult {
    let run = || -> Result<TagEditResult, TagError> {
        let mut file = TagFile::open(path)?;
        let before = read_file(&file);
        let mut changed: IndexMap<&'static str, Option<String>> = IndexMap::new();
        for (name, raw) in changes {
            let Some(field) = fields::canonical(name) else {
                continue;
            };
            let mut value = raw
                .as_deref()
                .filter(|raw| !is_null_or_white_space(Some(raw)))
                .map(|raw| raw.trim().to_string());
            if field == fields::ISRC
                && let Some(code) = &value
            {
                value = SongIdentity::normalize_isrc(code);
            }
            if before.get(field).cloned().flatten() == value {
                continue;
            }
            set(&mut file, field, value.as_deref());
            changed.insert(field, value);
        }
        if changed.is_empty() {
            return Ok(TagEditResult::same(before));
        }
        file.save()?;
        let mut after = before.clone();
        for (field, value) in changed {
            after.insert(field.to_string(), value);
        }
        Ok(TagEditResult::done(before, after))
    };
    run().unwrap_or_else(|error| TagEditResult::failed(why(&error)))
}

/// One field, written; None clears it.
///
/// The ISRC goes through [`extras::set_or_clear_text`], which takes a cleared field off, so
/// clearing an ISRC (and undoing an ISRC retag) leaves no code in the file.
fn set(file: &mut TagFile, field: &str, value: Option<&str>) {
    fn many(value: Option<&str>) -> Vec<String> {
        value
            .map(|value| {
                value
                    .split(';')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
    fn number(value: Option<&str>) -> u32 {
        value.and_then(|value| value.parse().ok()).unwrap_or(0)
    }
    fn one(value: Option<&str>) -> Vec<String> {
        value.map(|value| vec![value.to_string()]).unwrap_or_default()
    }
    match field {
        fields::TITLE => file.set_title(value),
        // One name as written: a credit like "Simon & Garfunkel" is one artist, not two.
        fields::ARTIST => file.set_performers(&one(value)),
        fields::ALBUM => file.set_album(value),
        fields::ALBUM_ARTIST => file.set_album_artists(&one(value)),
        fields::YEAR => file.set_year(number(value)),
        fields::GENRE => file.set_genres(&many(value)),
        fields::TRACK => file.set_track(number(value)),
        fields::DISC => file.set_disc(number(value)),
        fields::ISRC => extras::set_or_clear_text(file, TagFields::ISRC, value),
        _ => {}
    }
}

/// Gives the target file the album of the lead file: the title, album artists, version, release
/// date, MusicBrainz album id, compilation flag and year, which are what Navidrome tells albums
/// apart by. The target keeps its own title, track and disc. `lead_album_artist` is the album
/// artist Navidrome shows for the lead, used only when its file names none, so the target hashes
/// to the same album.
pub fn join_album(
    target_path: &Path,
    lead_path: &Path,
    lead_album_artist: Option<&str>,
    target_album_artist: Option<&str>,
) -> TagEditResult {
    let Some(lead) = KeptIdentityTags::read(lead_path, lead_album_artist) else {
        return TagEditResult::failed("could not read the album's tags");
    };
    let Some(mine) = KeptIdentityTags::read(target_path, target_album_artist) else {
        return TagEditResult::failed("could not read the song's tags");
    };
    let lead_year = match TagFile::open(lead_path) {
        Ok(file) => file.year(),
        Err(error) => return TagEditResult::failed(why(&error)),
    };

    let joined = KeptIdentity {
        album: lead.album.clone(),
        album_artist: lead.album_artist.clone(),
        album_artists: lead.album_artists.clone(),
        album_version: lead.album_version.clone(),
        release_date: lead.release_date.clone(),
        album_id: lead.album_id.clone(),
        compilation: lead.compilation,
        track_count: if lead.track_count > 0 {
            lead.track_count
        } else {
            mine.track_count
        },
        disc_count: if lead.disc_count > 0 {
            lead.disc_count
        } else {
            mine.disc_count
        },
        ..mine.clone()
    };
    let run = || -> Result<TagEditResult, TagError> {
        let before = read(target_path)?;
        let same_album = KeptIdentityTags::pid_inputs(&joined) == KeptIdentityTags::pid_inputs(&mine);
        let same_year =
            lead_year == 0 || before.get(fields::YEAR).cloned().flatten() == Some(lead_year.to_string());
        if same_album && same_year {
            return Ok(TagEditResult::same(before));
        }
        if !same_album {
            KeptIdentityTags::apply(target_path, &joined)?;
        }
        if !same_year {
            let mut file = TagFile::open(target_path)?;
            file.set_year(lead_year);
            file.save()?;
        }
        Ok(TagEditResult::done(before, read(target_path)?))
    };
    run().unwrap_or_else(|error| TagEditResult::failed(why(&error)))
}

/// Puts back an album identity read before a [`join_album`], and its year.
pub fn restore_album(path: &Path, identity: &KeptIdentity, year: Option<&str>) -> TagEditResult {
    let run = || -> Result<TagEditResult, String> {
        let before = read(path).map_err(|error| why(&error))?;
        KeptIdentityTags::apply(path, identity).map_err(|error| why(&error))?;
        // A year that is not a whole number fails the edit.
        let year = match year {
            None => 0,
            Some(text) => text
                .parse::<u32>()
                .map_err(|_| format!("The input string '{text}' was not in a correct format."))?,
        };
        let mut file = TagFile::open(path).map_err(|error| why(&error))?;
        file.set_year(year);
        file.save().map_err(|error| why(&error))?;
        let after = read(path).map_err(|error| why(&error))?;
        Ok(TagEditResult::done(before, after))
    };
    run().unwrap_or_else(TagEditResult::failed)
}

/// The advisory a file carries now, as the journal keeps it ("1", "2", "0"), or None.
pub fn read_advisory(file: &TagFile) -> Option<String> {
    extras::read_advisory(file).map(|advisory| advisory.to_string())
}

/// Writes the explicit advisory (1 explicit, 2 clean, 0 neither), or takes it out when
/// `advisory` is None. In place, like every edit here.
pub fn set_advisory(path: &Path, advisory: Option<i32>) -> TagEditResult {
    let run = || -> Result<TagEditResult, TagError> {
        let mut file = TagFile::open(path)?;
        let was = read_advisory(&file);
        let now = advisory.map(|advisory| advisory.to_string());
        if was == now {
            return Ok(TagEditResult::same(advisory_only(was)));
        }
        match advisory {
            None => extras::clear_advisory(&mut file),
            Some(_) => extras::set_advisory(&mut file, advisory),
        }
        file.save()?;
        Ok(TagEditResult::done(advisory_only(was), advisory_only(now)))
    };
    run().unwrap_or_else(|error| TagEditResult::failed(why(&error)))
}

/// The one field an advisory edit keeps, under the advisory kind's name.
pub fn advisory_only(value: Option<String>) -> TagValues {
    let mut values = TagValues::new();
    values.insert(tag_edit_kinds::ADVISORY.into(), value);
    values
}

/// Whether the file carries a picture of its own.
pub fn has_picture(path: &Path) -> bool {
    TagFile::open(path).is_ok_and(|file| !file.pictures().is_empty())
}

/// Puts a front cover inside a file that has no picture. A file that has one is left alone: this
/// fills a gap, it never replaces what someone chose.
pub fn add_cover(path: &Path, cover: &[u8]) -> TagEditResult {
    let run = || -> Result<TagEditResult, TagError> {
        let mut file = TagFile::open(path)?;
        let before = read_file(&file);
        if !file.pictures().is_empty() {
            return Ok(TagEditResult::same(before));
        }
        file.set_pictures(&[TagPicture::front_cover(
            cover.to_vec(),
            cover_image::mime_type(cover),
        )]);
        file.save()?;
        Ok(TagEditResult::done(before.clone(), before))
    };
    run().unwrap_or_else(|error| TagEditResult::failed(why(&error)))
}

/// Takes every picture out of a file, to undo [`add_cover`].
pub fn remove_cover(path: &Path) -> TagEditResult {
    let run = || -> Result<TagEditResult, TagError> {
        let mut file = TagFile::open(path)?;
        let before = read_file(&file);
        if file.pictures().is_empty() {
            return Ok(TagEditResult::same(before));
        }
        file.set_pictures(&[]);
        file.save()?;
        Ok(TagEditResult::done(before.clone(), before))
    };
    run().unwrap_or_else(|error| TagEditResult::failed(why(&error)))
}

/// A field's name in a sentence.
pub fn words(field: &str) -> &str {
    match field {
        fields::ALBUM_ARTIST => "album artist",
        fields::TRACK => "track number",
        fields::DISC => "disc number",
        fields::ISRC => "ISRC",
        _ => field,
    }
}

/// What went wrong with a file, in the words the apps show.
pub fn why(error: &TagError) -> String {
    match error {
        TagError::Unsupported(_) => "this kind of file cannot hold tags the server can write".into(),
        TagError::Read(_) => "the file looks damaged, so its tags were left alone".into(),
        TagError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
            "the server is not allowed to write to this file".into()
        }
        TagError::Io(io) => format!("the file could not be written ({io})"),
        other => other.to_string(),
    }
}

/// One edit the apps made to one file, kept so it can be undone and so a person can see later
/// which files were changed, by whom and how. Its fields in the state file's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct TagEditEntry {
    #[serde(deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(deserialize_with = "null_as_default")]
    pub navidrome_id: String,
    #[serde(deserialize_with = "null_as_default")]
    pub path: String,
    #[serde(deserialize_with = "null_as_default")]
    pub kind: String,
    #[serde(deserialize_with = "null_as_default")]
    pub username: String,
    /// The fields as they were, by tag name; None for a field the file did not have.
    #[serde(deserialize_with = "null_as_default")]
    pub before: IndexMap<String, Option<String>>,
    #[serde(deserialize_with = "null_as_default")]
    pub after: IndexMap<String, Option<String>>,
    /// The file as the edit left it, so an undo leaves alone a file changed since.
    pub size_after: i64,
    pub write_ticks_after: i64,
    #[serde(with = "datetime::utc")]
    pub at_utc: DateTime<Utc>,
    /// For an album join: the album as it was, so the undo can put it back exactly.
    pub album_before: Option<KeptIdentity>,
    pub undone: bool,
    /// The library-wide run that made this edit, when one did (the explicit marking), so the
    /// whole run can be undone at once.
    pub run_id: Option<String>,
}

impl Default for TagEditEntry {
    fn default() -> Self {
        TagEditEntry {
            id: String::new(),
            navidrome_id: String::new(),
            path: String::new(),
            kind: String::new(),
            username: String::new(),
            before: IndexMap::new(),
            after: IndexMap::new(),
            size_after: 0,
            write_ticks_after: 0,
            at_utc: datetime::min_value(),
            album_before: None,
            undone: false,
            run_id: None,
        }
    }
}

/// Room for a library-wide run's edits beside the apps' own: the oldest go past this many.
pub const MAX_ENTRIES: usize = 20_000;

/// The journal as `TagEditJournal` starts from it: empty when there is no file, and empty with a
/// warning when it cannot be read.
pub fn load(path: &Path) -> Vec<TagEditEntry> {
    let read = || -> anyhow::Result<Vec<TagEditEntry>> {
        let Some(text) = state_file::read_text(path)? else {
            return Ok(Vec::new());
        };
        Ok(serde_json::from_str::<Option<Vec<TagEditEntry>>>(&text)?.unwrap_or_default())
    };
    read().unwrap_or_else(|e| {
        warn!("tag edit journal could not be read: {e}");
        Vec::new()
    })
}

/// Rewrites the whole journal through `<file>.tmp`. False (with a warning) when it could not be
/// kept on disk.
pub fn save(path: &Path, entries: &[TagEditEntry]) -> bool {
    match state_file::save_atomic(path, &octo_core::json::to_string(entries)) {
        Ok(()) => true,
        Err(e) => {
            warn!("tag edit journal could not be written: {e}");
            false
        }
    }
}

/// The tag edits made from the apps, newest last, kept beside the settings. Bounded, rewritten
/// whole through a temporary file on every change (edits come one at a time from a person, so
/// that is cheap), and read back at start.
pub struct TagEditJournal {
    path: Option<PathBuf>,
    entries: Mutex<Vec<TagEditEntry>>,
}

impl TagEditJournal {
    /// A journal over `path`, read back from it; None keeps it in memory only.
    pub fn new(path: Option<PathBuf>) -> Self {
        let path = path.filter(|path| !is_null_or_white_space(path.to_str()));
        let entries = path.as_deref().map(load).unwrap_or_default();
        TagEditJournal {
            path,
            entries: Mutex::new(entries),
        }
    }

    /// False when it could not be kept on disk. A library-wide run passes `save` false and calls
    /// [`TagEditJournal::flush`] every few files, since every save rewrites the whole journal.
    pub fn record(&self, entry: TagEditEntry, save: bool) -> bool {
        let mut entries = self.entries.lock();
        entries.push(entry);
        if entries.len() > MAX_ENTRIES {
            let extra = entries.len() - MAX_ENTRIES;
            entries.drain(..extra);
        }
        !save || self.save(&entries)
    }

    /// Writes what [`TagEditJournal::record`] and [`TagEditJournal::mark_undone`] kept in memory.
    pub fn flush(&self) -> bool {
        let entries = self.entries.lock();
        self.save(&entries)
    }

    /// Follow a folder the library tidy moved, so an undo of an edit still finds its file.
    /// Answers how many entries followed.
    pub fn move_paths(&self, from: &str, to: &str) -> usize {
        let mut entries = self.entries.lock();
        let mut moved = 0;
        for entry in entries.iter_mut() {
            if let Some(now) = octo_core::common::PathHelper::moved_path(&entry.path, from, to) {
                entry.path = now;
                moved += 1;
            }
        }
        if moved > 0 {
            self.save(&entries);
        }
        moved
    }

    /// The edits one run made that are not undone yet, oldest first.
    pub fn of_run(&self, run_id: &str) -> Vec<TagEditEntry> {
        self.entries
            .lock()
            .iter()
            .filter(|entry| entry.run_id.as_deref() == Some(run_id) && !entry.undone)
            .cloned()
            .collect()
    }

    /// The newest edit of this song not undone yet.
    pub fn last(&self, navidrome_id: &str) -> Option<TagEditEntry> {
        self.entries
            .lock()
            .iter()
            .rev()
            .find(|entry| entry.navidrome_id == navidrome_id && !entry.undone)
            .cloned()
    }

    /// The newest edits first, at most `limit`.
    pub fn recent(&self, limit: usize) -> Vec<TagEditEntry> {
        self.entries.lock().iter().rev().take(limit).cloned().collect()
    }

    pub fn mark_undone(&self, entry_id: &str, save: bool) -> bool {
        let mut entries = self.entries.lock();
        let Some(entry) = entries.iter_mut().find(|entry| entry.id == entry_id) else {
            return false;
        };
        entry.undone = true;
        !save || self.save(&entries)
    }

    fn save(&self, entries: &[TagEditEntry]) -> bool {
        match &self.path {
            None => true,
            Some(path) => save(path, entries),
        }
    }
}

#[cfg(test)]
#[path = "library_tag_edits_tests.rs"]
mod tests;
