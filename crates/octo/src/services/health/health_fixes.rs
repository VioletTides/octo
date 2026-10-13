//! Library health's fixes: what each finding comes to when it is put right, worked out from the
//! songs alone so it can be shown before anything changes, and then done one song at a time. The
//! same plans the Octo app makes (HealthFixes.kt); only the drawing differs.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use octo_core::common::dotnet::{eq_ignore_case, is_blank, to_lower_invariant};
use octo_core::common::song_identity::SongIdentity;

use super::health_words::{self as words, count_text};
use super::keep_marks::TargetReason;
use super::library_health::{
    AlbumDifference, BestReason, DuplicateSet, HealthTag, IHealthFields, SplitAlbum, tags_of,
};
use crate::services::library::SongLookup;
use crate::services::library::library_tag_edits::song_tag_fields as fields_of;

/// One tag a fix would write: what the song says now and what it gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagChange {
    pub tag: String,
    pub now: Option<String>,
    pub value: String,
    pub from: Option<String>,
}

impl TagChange {
    pub fn new(tag: &str, now: Option<&str>, value: &str) -> Self {
        TagChange {
            tag: tag.to_string(),
            now: now.map(str::to_string),
            value: value.to_string(),
            from: None,
        }
    }

    /// "Year: 1998" or, over a value, "Year: 1998, was 1997".
    pub fn words(&self) -> String {
        match &self.now {
            None => format!("{}: {}", tag_name(&self.tag), self.value),
            Some(now) => format!("{}: {}, was {now}", tag_name(&self.tag), self.value),
        }
    }
}

/// One tag two copies disagree on, each copy's value by the copy's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagChoice {
    pub tag: String,
    pub values: Vec<(String, String)>,
}

/// A set of copies put right: one kept, the rest removed, and the tags the kept one lacks taken
/// from the others.
#[derive(Debug, Clone)]
pub struct DuplicateFix<T> {
    pub keep: T,
    pub remove: Vec<T>,
    /// Why that one is kept, in plain words.
    pub why: String,
    /// Blank tags on the kept copy another copy can fill. Done by "fix all".
    pub fills: Vec<TagChange>,
    /// Tags the copies say differently, for a person to pick from.
    pub differs: Vec<TagChoice>,
    /// Something worth knowing first, like a copy that is the only one on its album.
    pub note: Option<String>,
    /// The copy whose sound moves into the kept one, which keeps its id, path and identity
    /// tags so people's marks stay with it. None when the kept copy keeps its own sound.
    pub audio: Option<T>,
}

/// A split album put right: every song of the other parts takes the album tags of the first
/// part. Where the album artists differ, the moved songs take the first part's.
#[derive(Debug, Clone)]
pub struct AlbumJoin<'a, T> {
    pub album: &'a SplitAlbum<T>,
    pub lead: T,
    pub moving: Vec<T>,
}

impl<T> AlbumJoin<'_, T> {
    pub fn words(&self) -> String {
        let count = count_text(self.moving.len(), "song", "songs");
        let kept = self.album.parts[0].songs.len();
        let largest = self
            .album
            .parts
            .iter()
            .map(|part| part.songs.len())
            .max()
            .unwrap_or(0);
        let why = if kept < largest {
            ", since its album artist names every part's artist"
        } else {
            ""
        };
        let head = format!(
            "Moves {count} onto {}, the part with {}{why}.",
            self.album.title.trim(),
            count_text(kept, "song", "songs")
        );
        let artist = self.album.artist.trim();
        let differs = !self.album.reasons.is_empty()
            || self
                .album
                .differences
                .iter()
                .any(|difference| difference.kind == AlbumDifference::AlbumArtist);
        if differs && !artist.is_empty() {
            format!("{head} Their album artist becomes {artist}.")
        } else {
            head
        }
    }
}

/// The libraryAction names the steps use, the same as the app's.
pub mod fix_actions {
    pub const REMOVE: &str = "remove";
    pub const RETAG: &str = "retag";
    pub const JOIN_ALBUM: &str = "joinAlbum";
    pub const COVER: &str = "cover";
    pub const RESTORE: &str = "restore";
    pub const UNDO: &str = "undo";
    /// The best-sounding copy's audio moves into this copy (`from`), which keeps its id.
    pub const MOVE_SOUND: &str = "moveSound";
    /// A move of the sound put back: both copies as they were.
    pub const MOVE_BACK: &str = "moveBack";

    pub const ALL: [&str; 8] = [
        REMOVE, RETAG, JOIN_ALBUM, COVER, RESTORE, UNDO, MOVE_SOUND, MOVE_BACK,
    ];
}

use fix_actions as actions;

/// One thing done to one song. `with` carries the tags for a retag, `like` (the lead song) for a
/// join, and `copy=true` for a remove of one copy of a song the library keeps another of, so the
/// song itself is still wanted.
#[derive(Debug, Clone)]
pub struct FixStep {
    pub action: String,
    pub id: String,
    pub title: String,
    pub with: Option<IndexMap<String, String>>,
}

impl FixStep {
    pub fn new(action: &str, id: &str, title: &str) -> Self {
        FixStep {
            action: action.to_string(),
            id: id.to_string(),
            title: title.to_string(),
            with: None,
        }
    }

    pub fn with(action: &str, id: &str, title: &str, with: IndexMap<String, String>) -> Self {
        FixStep {
            with: Some(with),
            ..FixStep::new(action, id, title)
        }
    }

    /// The step's parameters, none as empty.
    pub fn params(&self) -> IndexMap<String, String> {
        self.with.clone().unwrap_or_default()
    }

    /// One parameter.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.with.as_ref()?.get(name).map(String::as_str)
    }

    /// The step that puts this one back, once it is done.
    pub fn undo_step(&self) -> Option<FixStep> {
        match self.action.as_str() {
            actions::REMOVE => Some(FixStep::new(actions::RESTORE, &self.id, &self.title)),
            actions::RETAG | actions::JOIN_ALBUM | actions::COVER => {
                Some(FixStep::new(actions::UNDO, &self.id, &self.title))
            }
            actions::RESTORE => Some(FixStep::new(actions::REMOVE, &self.id, &self.title)),
            actions::MOVE_SOUND => Some(FixStep::with(
                actions::MOVE_BACK,
                &self.id,
                &self.title,
                IndexMap::from([("from".to_string(), self.param("from").unwrap_or("").to_string())]),
            )),
            _ => None,
        }
    }

    pub fn remove_copy(id: &str, title: &str) -> Self {
        FixStep::with(
            actions::REMOVE,
            id,
            title,
            IndexMap::from([("copy".to_string(), "true".to_string())]),
        )
    }

    pub fn retag_with<'a>(id: &str, title: &str, changes: impl IntoIterator<Item = &'a TagChange>) -> Self {
        // GroupBy(tag).ToDictionary(last value): each tag where it first came, with its last value.
        let mut with: IndexMap<String, String> = IndexMap::new();
        for change in changes {
            with.insert(change.tag.clone(), change.value.clone());
        }
        FixStep::with(actions::RETAG, id, title, with)
    }

    /// The sound of `from` moved into `id`, with the tags picked for `id` written as well.
    pub fn move_sound<'a>(
        id: &str,
        title: &str,
        from: &str,
        changes: impl IntoIterator<Item = &'a TagChange>,
    ) -> Self {
        let mut with: IndexMap<String, String> = IndexMap::from([("from".to_string(), from.to_string())]);
        for change in changes {
            with.insert(change.tag.clone(), change.value.clone());
        }
        FixStep::with(actions::MOVE_SOUND, id, title, with)
    }

    pub fn join(id: &str, title: &str, like: &str) -> Self {
        FixStep::with(
            actions::JOIN_ALBUM,
            id,
            title,
            IndexMap::from([("like".to_string(), like.to_string())]),
        )
    }
}

impl PartialEq for FixStep {
    /// Value equality: none and empty parameters alike, and the same pairs.
    fn eq(&self, other: &Self) -> bool {
        let (mine, theirs) = (self.params(), other.params());
        self.action == other.action
            && self.id == other.id
            && self.title == other.title
            && mine.len() == theirs.len()
            && mine.iter().all(|(key, value)| theirs.get(key) == Some(value))
    }
}

/// How one step went: applied, rehearsed, skipped, failed, unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixAnswer {
    pub state: String,
    pub detail: Option<String>,
}

impl FixAnswer {
    pub fn new(state: &str, detail: Option<&str>) -> Self {
        FixAnswer {
            state: state.to_string(),
            detail: detail.map(str::to_string),
        }
    }
}

/// How a run of steps went.
#[derive(Debug, Clone, Default)]
pub struct FixOutcome {
    pub done: Vec<FixStep>,
    /// Steps that changed nothing because nothing needed changing.
    pub unchanged: Vec<FixStep>,
    /// Steps that could not be done, with the server's words.
    pub failed: Vec<(FixStep, String)>,
    /// Only rehearsed: library actions are in dry run.
    pub rehearsed: bool,
    /// Stopped before the end.
    pub stopped: bool,
    /// Things worth knowing about what was done, like playlists that could not be changed.
    pub notes: Vec<String>,
}

impl FixOutcome {
    pub fn removed(&self) -> HashSet<String> {
        self.done
            .iter()
            .filter(|step| step.action == actions::REMOVE)
            .map(|step| step.id.clone())
            .collect()
    }

    /// What puts every done step back, the last first.
    pub fn undo_steps(&self) -> Vec<FixStep> {
        self.done.iter().rev().filter_map(FixStep::undo_step).collect()
    }

    /// One line: what was done, and what was not.
    pub fn summary(&self) -> String {
        if self.rehearsed {
            return "The server only rehearsed this, because its library actions are in dry run. Nothing changed."
                .to_string();
        }
        let count = |action: &str| self.done.iter().filter(|step| step.action == action).count();
        let mut parts: Vec<String> = Vec::new();
        let removes = count(actions::REMOVE);
        let retags = count(actions::RETAG);
        let joins = count(actions::JOIN_ALBUM);
        let covers = count(actions::COVER);
        let restores = count(actions::RESTORE);
        let undos = count(actions::UNDO);
        let moves = count(actions::MOVE_SOUND);
        let backs = count(actions::MOVE_BACK);
        if moves > 0 {
            parts.push(format!(
                "moved the best sound into {}",
                count_text(moves, "song", "songs")
            ));
        }
        if removes > 0 {
            parts.push(format!("removed {}", count_text(removes, "song", "songs")));
        }
        if retags > 0 {
            parts.push(format!(
                "changed the tags of {}",
                count_text(retags, "song", "songs")
            ));
        }
        if joins > 0 {
            parts.push(format!(
                "moved {} onto their album",
                count_text(joins, "song", "songs")
            ));
        }
        if covers > 0 {
            parts.push(format!("added {}", count_text(covers, "cover", "covers")));
        }
        if restores > 0 {
            parts.push(format!("put back {}", count_text(restores, "song", "songs")));
        }
        if undos > 0 {
            parts.push(format!(
                "undid the changes to {}",
                count_text(undos, "song", "songs")
            ));
        }
        if backs > 0 {
            parts.push(format!(
                "put back the copies of {}",
                count_text(backs, "song", "songs")
            ));
        }
        let head = if parts.is_empty() {
            if self.unchanged.is_empty() {
                "Nothing was done.".to_string()
            } else {
                "Nothing needed changing.".to_string()
            }
        } else {
            format!("{}.", upper_first(&parts.join(", ")))
        };
        let stop = if self.stopped {
            " Stopped before the end."
        } else {
            ""
        };
        let miss = match self.failed.len() {
            0 => String::new(),
            1 => format!(" {}: {}", self.failed[0].0.title, self.failed[0].1),
            n => format!(
                " {} could not be done, like {}: {}",
                count_text(n, "song", "songs"),
                self.failed[0].0.title,
                self.failed[0].1
            ),
        };
        let notes: String = self.notes.iter().map(|note| format!(" {note}")).collect();
        format!("{head}{stop}{miss}{notes}")
    }
}

/// `char.ToUpperInvariant(text[0]) + text[1..]`.
fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => octo_core::common::dotnet::to_upper_char(first).to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// The tags filled first, in this order.
const FILL_ORDER: [&str; 7] = [
    fields_of::ALBUM,
    fields_of::ALBUM_ARTIST,
    fields_of::TRACK,
    fields_of::DISC,
    fields_of::YEAR,
    fields_of::GENRE,
    fields_of::ISRC,
];

/// Tags that belong to an album: taken only from a copy on the same album, or with the album
/// itself when the kept copy has none.
const ALBUM_TAGS: [&str; 5] = [
    fields_of::ALBUM,
    fields_of::ALBUM_ARTIST,
    fields_of::TRACK,
    fields_of::DISC,
    fields_of::YEAR,
];

/// A song's tags as the server names them, for the ones the reader can see. Blank is None.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagValuesOf(IndexMap<&'static str, Option<String>>);

impl TagValuesOf {
    /// The value of one tag, None when blank or not one of them.
    pub fn get(&self, tag: &str) -> Option<&str> {
        self.0.get(tag).and_then(|value| value.as_deref())
    }
}

/// A song's tags as the server names them, for the ones the reader can see. Blank is None.
pub fn tag_values<T>(song: &T, fields: &dyn IHealthFields<T>) -> TagValuesOf {
    fn text(value: Option<&str>) -> Option<String> {
        value.filter(|value| !is_blank(value)).map(str::to_string)
    }
    fn number(value: Option<i32>) -> Option<String> {
        value.filter(|value| *value > 0).map(|value| value.to_string())
    }
    let genres: Vec<&str> = fields
        .genres(song)
        .into_iter()
        .filter(|genre| !is_blank(genre))
        .collect();
    let mut values = IndexMap::new();
    values.insert(fields_of::TITLE, text(Some(fields.title(song))));
    values.insert(fields_of::ARTIST, text(fields.artist(song)));
    values.insert(fields_of::ALBUM, text(fields.album(song)));
    values.insert(
        fields_of::ALBUM_ARTIST,
        if fields.seen(HealthTag::AlbumArtist) {
            text(fields.album_artist(song))
        } else {
            None
        },
    );
    values.insert(fields_of::YEAR, number(fields.year(song)));
    values.insert(fields_of::GENRE, text(Some(&genres.join("; "))));
    values.insert(fields_of::TRACK, number(fields.track(song)));
    values.insert(fields_of::DISC, number(fields.disc(song)));
    values.insert(fields_of::ISRC, text(fields.isrcs(song).first().copied()));
    TagValuesOf(values)
}

/// The plan for one set of copies, keeping `keep` (the best copy unless a person picked another).
pub fn duplicate_fix<T: Clone>(
    set: &DuplicateSet<T>,
    fields: &dyn IHealthFields<T>,
    keep: Option<&T>,
) -> DuplicateFix<T> {
    let keep = keep.unwrap_or(set.best()).clone();
    let keep_id = fields.id(&keep).to_string();
    let others: Vec<T> = set
        .copies
        .iter()
        .filter(|copy| fields.id(copy) != keep_id)
        .cloned()
        .collect();
    let mine = tag_values(&keep, fields);
    // The fullest tagged donors first.
    let mut donors: Vec<&T> = others.iter().collect();
    donors.sort_by_key(|donor| std::cmp::Reverse(tags_of(*donor, fields)));
    let my_album = SongIdentity::key(mine.get(fields_of::ALBUM).unwrap_or(""));
    // With no album, the album comes from one donor whole: its title, album artist, track and
    // disc together.
    let album_donor: Option<&T> = if my_album.is_empty() {
        donors
            .iter()
            .copied()
            .find(|donor| tag_values(*donor, fields).get(fields_of::ALBUM).is_some())
    } else {
        None
    };
    let mut fills: Vec<TagChange> = Vec::new();
    for tag in FILL_ORDER {
        if mine.get(tag).is_some() {
            continue;
        }
        let from = donors.iter().copied().find(|donor| {
            let theirs = tag_values(*donor, fields);
            if theirs.get(tag).is_none() {
                return false;
            }
            !ALBUM_TAGS.contains(&tag)
                || album_donor.is_some_and(|album| fields.id(album) == fields.id(*donor))
                || (!my_album.is_empty()
                    && SongIdentity::key(theirs.get(fields_of::ALBUM).unwrap_or("")) == my_album)
        });
        let Some(from) = from else {
            continue;
        };
        let value = tag_values(from, fields).get(tag).unwrap_or("").to_string();
        fills.push(TagChange {
            tag: tag.to_string(),
            now: None,
            value,
            from: Some(fields.id(from).to_string()),
        });
    }
    let mut differs: Vec<TagChoice> = Vec::new();
    for tag in [
        fields_of::TITLE,
        fields_of::ARTIST,
        fields_of::ALBUM,
        fields_of::ALBUM_ARTIST,
        fields_of::YEAR,
        fields_of::GENRE,
    ] {
        let Some(value) = mine.get(tag) else {
            continue;
        };
        let mut values: Vec<(String, String)> = vec![(keep_id.clone(), value.to_string())];
        for other in &others {
            if let Some(theirs) = tag_values(other, fields).get(tag) {
                values.push((fields.id(other).to_string(), theirs.to_string()));
            }
        }
        let distinct: HashSet<&str> = values.iter().map(|(_, value)| value.as_str()).collect();
        if distinct.len() > 1 {
            differs.push(TagChoice {
                tag: tag.to_string(),
                values,
            });
        }
    }
    // A copy that is the only song of its album here takes the album with it.
    let lonely = others.iter().find(|other| {
        let album = SongIdentity::key(fields.album(other).unwrap_or(""));
        !album.is_empty() && album != my_album
    });
    let note = lonely.map(|lonely| {
        format!(
            "{} is on {}, so that album will no longer have this song.",
            copy_name(lonely, fields),
            fields.album(lonely).unwrap_or("").trim()
        )
    });
    let why = keep_why(set, &keep, fields);
    DuplicateFix {
        keep,
        remove: others,
        why,
        fills,
        differs,
        note,
        audio: None,
    }
}

/// The plan that keeps people's marks: `target`, the copy whose id carries them, keeps its id
/// and path. With `same_format`, the best-sounding copy's audio moves into it under its identity
/// tags and the best-sounding copy's own file goes; without, `target` is kept as it is when it
/// holds marks or playlist places, since a changed extension would change its path and lose
/// them. When `target` is the best-sounding copy, or only its age or nothing tells the copies
/// apart and the formats differ, the usual plan.
pub fn duplicate_fix_keeping_marks<T: Clone>(
    set: &DuplicateSet<T>,
    fields: &dyn IHealthFields<T>,
    target: &T,
    reason: TargetReason,
    same_format: bool,
) -> DuplicateFix<T> {
    let audio = set.best();
    if fields.id(target) == fields.id(audio) || reason == TargetReason::First {
        return duplicate_fix(set, fields, None);
    }
    // Only marks someone can see are worth a worse sound: the oldest copy is just a likely holder.
    if !same_format && !matches!(reason, TargetReason::Marks | TargetReason::Playlists) {
        return duplicate_fix(set, fields, None);
    }
    let mut fix = duplicate_fix(set, fields, Some(target));
    if !same_format {
        fix.why = words::kept_marks_as_is(&words::quality_text(target, fields), reason);
        return fix;
    }
    let audio_id = fields.id(audio).to_string();
    fix.remove.retain(|copy| fields.id(copy) != audio_id);
    fix.why = words::kept_marks_moved(&words::quality_text(audio, fields), reason);
    fix.audio = Some(audio.clone());
    fix
}

/// Why `keep` is the one kept, in words.
pub fn keep_why<T>(set: &DuplicateSet<T>, keep: &T, fields: &dyn IHealthFields<T>) -> String {
    let quality = words::quality_text(keep, fields);
    if fields.id(keep) != fields.id(set.best()) {
        return format!("You picked this copy ({quality}) to keep.");
    }
    match set.best_reason {
        BestReason::Sound => format!("Keeps the {quality} copy, because it sounds best."),
        BestReason::Tags => {
            format!("The copies sound alike, so this keeps the one with the fullest tags ({quality}).")
        }
        BestReason::None => {
            format!("The copies sound alike and have the same tags, so this keeps the first ({quality}).")
        }
    }
}

/// "Holocene (MP3, 320 kbps)".
pub fn copy_name<T>(song: &T, fields: &dyn IHealthFields<T>) -> String {
    format!("{} ({})", fields.title(song), words::quality_text(song, fields))
}

/// A tag's name for people.
pub fn tag_name(tag: &str) -> String {
    match tag {
        fields_of::TITLE => "Title".to_string(),
        fields_of::ARTIST => "Artist".to_string(),
        fields_of::ALBUM => "Album".to_string(),
        fields_of::ALBUM_ARTIST => "Album artist".to_string(),
        fields_of::YEAR => "Year".to_string(),
        fields_of::GENRE => "Genre".to_string(),
        fields_of::TRACK => "Track number".to_string(),
        fields_of::DISC => "Disc number".to_string(),
        fields_of::ISRC => "ISRC".to_string(),
        _ => upper_first(tag),
    }
}

/// The steps that fix a set of copies: fill the kept one's blank tags, then remove the others.
pub fn duplicate_steps<T>(
    fix: &DuplicateFix<T>,
    fields: &dyn IHealthFields<T>,
    fills: Option<&[TagChange]>,
) -> Vec<FixStep> {
    let fills = fills.unwrap_or(&fix.fills);
    let mut steps = Vec::new();
    if let Some(audio) = &fix.audio {
        // The tags go onto the moved audio before it takes the kept copy's place.
        steps.push(FixStep::move_sound(
            fields.id(&fix.keep),
            fields.title(&fix.keep),
            fields.id(audio),
            fills,
        ));
    } else if !fills.is_empty() {
        steps.push(FixStep::retag_with(
            fields.id(&fix.keep),
            fields.title(&fix.keep),
            fills,
        ));
    }
    steps.extend(
        fix.remove
            .iter()
            .map(|copy| FixStep::remove_copy(fields.id(copy), fields.title(copy))),
    );
    steps
}

pub fn album_join<T: Clone>(album: &SplitAlbum<T>) -> AlbumJoin<'_, T> {
    AlbumJoin {
        album,
        lead: album.parts[0].songs[0].clone(),
        moving: album
            .parts
            .iter()
            .skip(1)
            .flat_map(|part| part.songs.iter().cloned())
            .collect(),
    }
}

pub fn join_steps<T>(join: &AlbumJoin<'_, T>, fields: &dyn IHealthFields<T>) -> Vec<FixStep> {
    join.moving
        .iter()
        .map(|song| FixStep::join(fields.id(song), fields.title(song), fields.id(&join.lead)))
        .collect()
}

/// A tag a song lacks that the rest of its album agrees on, so it can be filled in without
/// looking anything up: the year or genre every other song of the album has, or an album artist
/// from the album or, when every song is by one artist, that artist.
pub fn fills_from_album<T: Clone>(
    missing: &[T],
    all: &[T],
    tag: HealthTag,
    fields: &dyn IHealthFields<T>,
) -> Vec<(T, TagChange)> {
    let name = match tag {
        HealthTag::Year => fields_of::YEAR,
        HealthTag::Genre => fields_of::GENRE,
        HealthTag::AlbumArtist => fields_of::ALBUM_ARTIST,
        _ => return Vec::new(),
    };
    let mut by_album: HashMap<&str, Vec<&T>> = HashMap::new();
    for song in all {
        by_album
            .entry(fields.album_id(song).unwrap_or(""))
            .or_default()
            .push(song);
    }
    let mut found = Vec::new();
    for song in missing {
        let Some(album_id) = fields.album_id(song).filter(|id| !id.is_empty()) else {
            continue;
        };
        let id = fields.id(song);
        let siblings: Vec<&T> = by_album
            .get(album_id)
            .map(|songs| {
                songs
                    .iter()
                    .copied()
                    .filter(|other| fields.id(other) != id)
                    .collect()
            })
            .unwrap_or_default();
        let mut values: Vec<String> = Vec::new();
        for other in &siblings {
            if let Some(value) = tag_values(*other, fields).get(name)
                && !values.iter().any(|known| known == value)
            {
                values.push(value.to_string());
            }
        }
        let mut value = if values.len() == 1 {
            Some(values[0].clone())
        } else {
            None
        };
        if value.is_none() && tag == HealthTag::AlbumArtist && values.is_empty() {
            let mut artists: Vec<&str> = Vec::new();
            for artist in siblings
                .iter()
                .copied()
                .chain(std::iter::once(song))
                .filter_map(|other| fields.artist(other))
            {
                if !is_blank(artist) && !artists.contains(&artist) {
                    artists.push(artist);
                }
            }
            value = (artists.len() == 1).then(|| artists[0].to_string());
        }
        if let Some(value) = value {
            found.push((song.clone(), TagChange::new(name, None, &value)));
        }
    }
    found
}

/// What a lookup would change: each tag found that the file says differently. A blank one is
/// picked to be filled; one the file has is picked only when the match is sure and the tag is not
/// the song's name.
pub fn changes(lookup: &SongLookup) -> Vec<(TagChange, bool)> {
    let sure = lookup.confidence.as_deref().is_some_and(|confidence| {
        eq_ignore_case(confidence, "Strong") || eq_ignore_case(confidence, "Medium")
    });
    let mut found = Vec::new();
    for tag in fields_of::ALL {
        let Some(value) = lookup
            .suggested
            .get(tag)
            .and_then(|value| value.as_deref())
            .filter(|value| !is_blank(value))
        else {
            continue;
        };
        let now = lookup
            .current
            .get(tag)
            .and_then(|value| value.as_deref())
            .filter(|current| !is_blank(current));
        if now.is_some_and(|now| now.trim() == value.trim()) {
            continue;
        }
        let named = matches!(tag, fields_of::TITLE | fields_of::ARTIST | fields_of::ALBUM);
        let pick = now.is_none() || (sure && !named);
        found.push((
            TagChange {
                tag: tag.to_string(),
                now: now.map(str::to_string),
                value: value.to_string(),
                from: lookup.source.clone(),
            },
            pick,
        ));
    }
    found
}

/// Where a lookup came from: "A sure match from the song's fingerprint, on 'Mezzanine' 1998."
pub fn origin(lookup: &SongLookup) -> String {
    let how = match lookup.confidence.as_deref().map(to_lower_invariant).as_deref() {
        Some("strong") => "A sure match",
        Some("medium") => "A likely match",
        None => "Found",
        Some(_) => "A doubtful match",
    };
    let from = lookup
        .source
        .as_deref()
        .map(|source| format!(" from {}", source_name(source)))
        .unwrap_or_default();
    let on = lookup
        .release
        .as_deref()
        .filter(|release| !is_blank(release))
        .map(|release| format!(", on {release}"))
        .unwrap_or_default();
    format!("{how}{from}{on}.")
}

fn source_name(source: &str) -> String {
    match to_lower_invariant(source).as_str() {
        "fingerprint" => "the song's fingerprint".to_string(),
        "database" => "MusicBrainz".to_string(),
        "catalog" => "the catalog".to_string(),
        "filetags" => "the file's own tags".to_string(),
        _ => source.to_string(),
    }
}

const NOTHING_TO_DO: [&str; 4] = [
    "nothing to change",
    "already",
    "picture of its own",
    "has a picture",
];

/// Skipped with nothing to change is not a failure: the song already says it, or the cover is
/// there.
pub fn is_nothing_to_do(detail: Option<&str>) -> bool {
    detail.is_some_and(|detail| {
        let lowered = octo_core::common::dotnet::ordinal_ignore_case_key(detail);
        NOTHING_TO_DO
            .iter()
            .any(|words| lowered.contains(&octo_core::common::dotnet::ordinal_ignore_case_key(words)))
    })
}
