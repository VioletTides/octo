//! Fixing a set of copies so the best sound ends up in the copy people marked. Navidrome keeps a
//! song's id, and with it every favorite, rating, play and playlist place, for as long as a file
//! stays at the same path. So the copy whose id carries the most marks keeps its path, the
//! best-sounding audio moves onto that path under that copy's own identity tags, and the
//! best-sounding copy's old file goes to the trash. Playlist entries of the copy that went are
//! moved onto the one that stayed, each in its own place, once Navidrome shows the moved song as
//! the same song; when it does not, both copies go back as they were.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use futures::future::{BoxFuture, join_all};
use indexmap::IndexMap;
use octo_core::common::dotnet::{eq_ignore_case, is_blank};
use octo_core::library::generated_playlist_service::dotnet_ticks;
use octo_core::settings::{LibraryAction, SettingsStore};
use octo_media::tags::TagFile;
use octo_media::tags::kept_identity::{ARTISTS_KEYS, KeptIdentityTags, NavidromeView};
use octo_media::tags::tag_writer_extras::{self as extras, TagField, TagFields};
use parking_lot::Mutex;
use tracing::{info, warn};

use super::health_fixes::{self as fixes, DuplicateFix, FixAnswer};
use super::library_health::{DuplicateSet, IHealthFields};
use super::library_health_service::Row;
use crate::services::library::library_tag_edits::{self as tag_edits, song_tag_fields};
use crate::services::library::library_write_gate::{LibraryWrite, LibraryWriteGate};
use crate::services::library::navidrome_playlist_api::{PlaylistKind, SongMarks};
use crate::services::library::{
    LibraryActionEntry, LibraryActionExecutor, LibraryActionJournal, LibraryActionQuarantine,
    LibraryActionRequest, LibraryActionState, NavidromePlaylistApi, NavidromeSongPathResolver,
    ResolvedSongFile,
};
use crate::services::local::ILocalLibraryService;

/// One copy of a set, as the pick reads it: its id and when Navidrome first saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRef {
    pub id: String,
    pub created: Option<DateTime<Utc>>,
}

/// What one reader's marks on a copy are worth: a favorite 3, any rating 2, and each play 1 up
/// to 50, so one person's habit of replaying a song never outweighs everyone's favorites.
pub fn marks_score(marks: &SongMarks) -> i64 {
    let starred = if marks.starred { 3 } else { 0 };
    let rated = if marks.rating > 0 { 2 } else { 0 };
    starred + rated + marks.plays.clamp(0, 50)
}

/// The marks people other than the admin left on a song, each person's read with their own
/// sign-in.
pub trait MemberMarks: Send + Sync {
    /// Each member's marks on one song; a member whose marks cannot be read is left out.
    fn marks_of<'a>(&'a self, song_id: &'a str) -> BoxFuture<'a, Vec<SongMarks>>;
}

/// Why the copy that keeps its id was picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetReason {
    /// People starred, rated or played it more than the others.
    Marks,
    /// No marks anywhere, and more playlist entries than the others.
    Playlists,
    /// No marks or playlist entries anywhere: the oldest copy, the likeliest holder of marks
    /// nobody here can read.
    Oldest,
    /// Nothing tells the copies apart: the first in the set's own order.
    First,
}

/// The copy that keeps its id, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPick {
    pub target: String,
    pub reason: TargetReason,
}

/// The copy whose id carries the most: the highest marks score, then the most playlist entries,
/// then the oldest, then the set's own order. None for no copies.
pub fn pick_target(
    copies: &[CopyRef],
    scores: &HashMap<String, i64>,
    entries: &HashMap<String, usize>,
) -> Option<TargetPick> {
    let score = |copy: &CopyRef| scores.get(&copy.id).copied().unwrap_or(0);
    let listed = |copy: &CopyRef| entries.get(&copy.id).copied().unwrap_or(0);
    let mut order: Vec<usize> = (0..copies.len()).collect();
    order.sort_by(|a, b| {
        let (a_copy, b_copy) = (&copies[*a], &copies[*b]);
        score(b_copy)
            .cmp(&score(a_copy))
            .then(listed(b_copy).cmp(&listed(a_copy)))
            .then(match (a_copy.created, b_copy.created) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then(a.cmp(b))
    });
    let top = &copies[*order.first()?];
    let reason = if score(top) > 0 {
        TargetReason::Marks
    } else if listed(top) > 0 {
        TargetReason::Playlists
    } else if copies.iter().any(|copy| copy.created.is_some())
        && copies.iter().any(|copy| copy.created != top.created)
    {
        TargetReason::Oldest
    } else {
        TargetReason::First
    };
    Some(TargetPick {
        target: top.id.clone(),
        reason,
    })
}

/// How many entries each song has across the playlists people made. Smart playlists are left
/// out: their tracks follow their rules, not anyone's choice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistUses(pub HashMap<String, usize>);

/// Whether two files are the same format by extension. Navidrome's id follows the path, and a
/// changed extension is a changed path.
pub fn same_extension(a: &str, b: &str) -> bool {
    let ext = |path: &str| {
        Path::new(path)
            .extension()
            .map(|ext| ext.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let (a, b) = (ext(a), ext(b));
    !a.is_empty() && eq_ignore_case(&a, &b)
}

/// The plan for a set of copies that keeps people's marks: with `pick`, the best sound moves
/// into the picked copy when both are the same format, and the picked copy is kept as it is when
/// they are not. Without a pick, or when the best-sounding copy is the picked one, the usual
/// plan.
pub fn plan(
    set: &DuplicateSet<Row>,
    fields: &dyn IHealthFields<Row>,
    pick: Option<&TargetPick>,
) -> DuplicateFix<Row> {
    let target = pick.and_then(|pick| set.copies.iter().find(|copy| copy.id == pick.target));
    match (pick, target) {
        (Some(pick), Some(target)) => {
            let same_format = same_extension(&target.path, &set.best().path);
            fixes::duplicate_fix_keeping_marks(set, fields, target, pick.reason, same_format)
        }
        _ => fixes::duplicate_fix(set, fields, None),
    }
}

/// The ARTISTS field Navidrome reads every track artist from.
const ARTISTS: TagField = TagField {
    id3_frame: None,
    id3_description: Some("ARTISTS"),
    vorbis: "ARTISTS",
    mp4: "ARTISTS",
};

const ARTIST_ID_KEYS: [&str; 2] = ["musicbrainz_artistid", "musicbrainz artist id"];

fn view_values(view: &NavidromeView, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| view.get(*key).filter(|values| !values.is_empty()))
        .cloned()
        .unwrap_or_default()
}

/// Writes `from`'s identity onto `onto`: title, track artists, album, album artists, the
/// MusicBrainz ids, and the track and disc numbers, everything Navidrome files the song by. The
/// audio and every other tag of `onto` stay. `album_artist` is the album artist Navidrome stored
/// for `from`, used only when its file names none.
pub fn carry_identity(from: &Path, onto: &Path, album_artist: Option<&str>) -> Result<(), String> {
    let identity =
        KeptIdentityTags::read(from, album_artist).ok_or("the marked copy's tags could not be read")?;
    let mut source = TagFile::open(from).map_err(|e| tag_edits::why(&e))?;
    let performers = source.performers();
    let view = KeptIdentityTags::navidrome_view(&source);
    let artists = view_values(&view, &ARTISTS_KEYS);
    let artist_ids = view_values(&view, &ARTIST_ID_KEYS);
    let recording = extras::read_recording_id(&mut source);
    KeptIdentityTags::apply(onto, &identity).map_err(|e| tag_edits::why(&e))?;
    let mut file = TagFile::open(onto).map_err(|e| tag_edits::why(&e))?;
    file.set_performers(&performers);
    extras::set_exact(&mut file, ARTISTS, &artists);
    extras::set_exact(&mut file, TagFields::ARTIST_ID, &artist_ids);
    if let Some(recording) = recording {
        extras::set_recording_id(&mut file, &recording);
    }
    file.save().map_err(|e| tag_edits::why(&e))
}

/// The best audio copied beside the marked copy under a hidden name Navidrome's scanner skips,
/// carrying the marked copy's identity and the tags a person picked. Nothing in the library has
/// changed when this fails, and nothing is left behind.
fn stage(
    audio: &Path,
    target: &Path,
    album_artist: Option<&str>,
    changes: &IndexMap<String, Option<String>>,
) -> Result<PathBuf, String> {
    let folder = target.parent().ok_or("the marked copy has no folder")?;
    let extension = target
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staged = folder.join(format!(
        "{STAGING_PREFIX}{}.{extension}",
        uuid::Uuid::new_v4().simple()
    ));
    let made = (|| {
        let copied = std::fs::copy(audio, &staged).map_err(|e| e.to_string())?;
        let size = std::fs::metadata(audio).map_err(|e| e.to_string())?.len();
        if copied != size {
            return Err("the copy did not match the original's size".to_string());
        }
        carry_identity(target, &staged, album_artist)?;
        if !changes.is_empty()
            && let Some(error) = tag_edits::write(&staged, changes).error
        {
            return Err(error);
        }
        Ok(())
    })();
    match made {
        Ok(()) => Ok(staged),
        Err(why) => {
            let _ = std::fs::remove_file(&staged);
            Err(why)
        }
    }
}

/// One move done here: both copies as they were, where each waits in the trash, and the file
/// that moved in.
#[derive(Clone)]
struct Moved {
    target: ResolvedSongFile,
    audio: ResolvedSongFile,
    user: String,
    target_key: String,
    target_trash: String,
    audio_key: String,
    audio_trash: String,
    size: u64,
    modified: Option<SystemTime>,
    /// The playlist places moved from the dropped copy to the kept one: playlist and position.
    rewritten: Vec<(String, usize)>,
}

/// How the playlist entries of one song moved onto another.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Rewrite {
    /// Each place moved: the playlist and the entry's position, unchanged.
    pub moved: Vec<(String, usize)>,
    /// Smart and synced playlists holding the song, whose tracks Navidrome does not let anyone
    /// edit, by name.
    pub skipped: Vec<String>,
    /// Playlists where moving an entry failed part way, by name.
    pub failed: Vec<String>,
}

pub const FORMAT_DIFFERS: &str = "The best-sounding copy is a different format, so moving it would change the song's path and lose its marks. Nothing changed.";
pub const CHANGED_SINCE: &str = "The file has changed since, so it was left as it is.";
pub const CHECKING: &str = "Checking that Navidrome kept the moved songs";

/// Fixing duplicates by keeping the best sound in the copy people marked.
pub struct KeepMarks {
    api: Arc<NavidromePlaylistApi>,
    resolver: Arc<NavidromeSongPathResolver>,
    quarantine: Arc<LibraryActionQuarantine>,
    journal: Arc<LibraryActionJournal>,
    library: Arc<dyn ILocalLibraryService>,
    executor: Arc<LibraryActionExecutor>,
    settings: Arc<SettingsStore>,
    members: Option<Arc<dyn MemberMarks>>,
    write_gate: Option<LibraryWriteGate>,
    moves: Mutex<HashMap<String, Moved>>,
    returning: Mutex<HashMap<String, Moved>>,
}

/// What [`KeepMarks`] is built from.
pub struct KeepMarksParts {
    pub api: Arc<NavidromePlaylistApi>,
    pub resolver: Arc<NavidromeSongPathResolver>,
    pub quarantine: Arc<LibraryActionQuarantine>,
    pub journal: Arc<LibraryActionJournal>,
    pub library: Arc<dyn ILocalLibraryService>,
    pub executor: Arc<LibraryActionExecutor>,
    pub settings: Arc<SettingsStore>,
    pub members: Option<Arc<dyn MemberMarks>>,
    /// The music folder's one-writer gate: each move takes a share, so it waits while a job
    /// rewrites the folder.
    pub write_gate: Option<LibraryWriteGate>,
}

impl KeepMarks {
    pub fn new(parts: KeepMarksParts) -> Self {
        KeepMarks {
            api: parts.api,
            resolver: parts.resolver,
            quarantine: parts.quarantine,
            journal: parts.journal,
            library: parts.library,
            executor: parts.executor,
            settings: parts.settings,
            members: parts.members,
            write_gate: parts.write_gate,
            moves: Mutex::new(HashMap::new()),
            returning: Mutex::new(HashMap::new()),
        }
    }

    // ---- which copy keeps its id ----------------------------------------------------------

    /// Every song's entries across the playlists people made, or `None` when Navidrome could not
    /// list them.
    pub async fn playlist_uses(&self) -> Option<PlaylistUses> {
        let playlists = self.api.list_playlist_kinds().await?;
        let mut uses: HashMap<String, usize> = HashMap::new();
        for playlist in playlists.iter().filter(|playlist| !playlist.smart) {
            let Ok(Some(tracks)) = self.api.read_all_tracks(&playlist.id).await else {
                continue;
            };
            for track in tracks {
                *uses.entry(track.media_file_id).or_default() += 1;
            }
        }
        Some(PlaylistUses(uses))
    }

    /// Every reader's marks on one copy, summed: the admin's own, then each member's.
    pub async fn score(&self, song_id: &str) -> i64 {
        let mut score = self
            .api
            .song_marks(song_id)
            .await
            .map_or(0, |marks| marks_score(&marks));
        if let Some(members) = &self.members {
            score += members
                .marks_of(song_id)
                .await
                .iter()
                .map(marks_score)
                .sum::<i64>();
        }
        score
    }

    /// The copy of `copies` whose id keeps the most people's marks.
    pub async fn marked_target(&self, copies: &[CopyRef], uses: &PlaylistUses) -> Option<TargetPick> {
        let scores: Vec<i64> = join_all(copies.iter().map(|copy| self.score(&copy.id))).await;
        let scores: HashMap<String, i64> = copies
            .iter()
            .zip(scores)
            .map(|(copy, score)| (copy.id.clone(), score))
            .collect();
        pick_target(copies, &scores, &uses.0)
    }

    /// The copy of a set of duplicates whose id keeps the most people's marks.
    pub async fn pick_for(&self, set: &DuplicateSet<Row>, uses: &PlaylistUses) -> Option<TargetPick> {
        let copies: Vec<CopyRef> = set
            .copies
            .iter()
            .map(|copy| CopyRef {
                id: copy.id.clone(),
                created: copy.added_utc,
            })
            .collect();
        self.marked_target(&copies, uses).await
    }

    // ---- the move -------------------------------------------------------------------------

    /// A share of the music folder's gate for one move: waits while a job holds it.
    async fn write_share(&self) -> Option<LibraryWrite> {
        match &self.write_gate {
            Some(gate) => Some(gate.write().await),
            None => None,
        }
    }

    fn entry(
        key: &str,
        action: LibraryAction,
        file: &ResolvedSongFile,
        user: &str,
        copy: bool,
    ) -> LibraryActionEntry {
        LibraryActionEntry {
            key: key.to_string(),
            action,
            navidrome_id: file.navidrome_id.clone(),
            username: user.to_string(),
            title: file.title.clone(),
            artist: file.artist.clone(),
            album: file.album.clone(),
            source_path: Some(file.absolute_path.clone()),
            quarantine_path: None,
            resolution: Some(file.source),
            state: LibraryActionState::Pending,
            detail: None,
            dry_run: false,
            at_utc: Utc::now(),
            history_kept: None,
            revealed_path: None,
            copy,
            download_allowed: false,
        }
    }

    /// Moves `audio_id`'s audio onto `target_id`'s path, under `target_id`'s identity tags and
    /// `tags` besides, and moves `audio_id`'s own file to the trash as a removed copy. The marked
    /// copy waits in the trash until Navidrome shows the moved song as the same one
    /// ([`Self::finish_run`]).
    pub async fn move_sound(
        &self,
        target_id: &str,
        audio_id: &str,
        user: &str,
        tags: &IndexMap<String, String>,
    ) -> FixAnswer {
        let settings = self.settings.current().library_actions.clone();
        if !settings.enabled {
            return FixAnswer::new("skipped", Some("Library actions are off."));
        }
        let removing = settings
            .effective_actions()
            .into_iter()
            .any(|entry| entry.action == LibraryAction::Delete && entry.enabled);
        if !removing {
            return FixAnswer::new(
                "skipped",
                Some("Removing songs is off: turn on the Delete action under Library actions."),
            );
        }
        if target_id == audio_id {
            return FixAnswer::new("skipped", Some("The best sound is already in that copy."));
        }
        let (target, audio) = (
            self.resolver.resolve(target_id).await,
            self.resolver.resolve(audio_id).await,
        );
        let (Some(target), Some(audio)) = (target, audio) else {
            return FixAnswer::new(
                "unresolved",
                Some("Could not work out which files these are, so nothing was touched."),
            );
        };
        if !same_extension(&target.absolute_path, &audio.absolute_path) {
            return FixAnswer::new("failed", Some(FORMAT_DIFFERS));
        }
        let Some(_claim) = self
            .executor
            .claim_songs(&[&target.absolute_path, &audio.absolute_path])
        else {
            return FixAnswer::new("skipped", Some(LibraryActionExecutor::BUSY_TEXT));
        };
        if settings.dry_run {
            return FixAnswer::new(
                "rehearsed",
                Some(&format!(
                    "Dry run: would move the sound of {} into {}",
                    audio.absolute_path, target.absolute_path
                )),
            );
        }
        let changes: IndexMap<String, Option<String>> = tags
            .iter()
            .filter_map(|(key, value)| {
                song_tag_fields::canonical(key).map(|tag| (tag.to_string(), Some(value.clone())))
            })
            .collect();

        let _share = self.write_share().await;
        let staged = {
            let (audio_path, target_path) = (
                PathBuf::from(&audio.absolute_path),
                PathBuf::from(&target.absolute_path),
            );
            let album_artist = target.album_artist.clone();
            tokio::task::spawn_blocking(move || {
                stage(&audio_path, &target_path, album_artist.as_deref(), &changes)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        };
        let staged = match staged {
            Ok(staged) => staged,
            Err(why) => {
                return FixAnswer::new("failed", Some(&format!("Nothing changed: {why}.")));
            }
        };

        let root = self.resolver.music_root();
        let stamp = format!("keep-marks:{}", dotnet_ticks(Utc::now()));
        let target_key = LibraryActionJournal::make_key(LibraryAction::BetterQuality, target_id, &stamp);
        self.journal.record(Self::entry(
            &target_key,
            LibraryAction::BetterQuality,
            &target,
            user,
            false,
        ));
        let failed = |why: &str| {
            let _ = std::fs::remove_file(&staged);
            self.journal.complete(
                &target_key,
                LibraryActionState::Failed,
                Some(why),
                None,
                None,
                None,
            );
            self.journal.flush();
            FixAnswer::new("failed", Some(why))
        };
        if !self.journal.flush() {
            return failed("Could not write the action journal, so the files were not touched.");
        }
        let out = self
            .quarantine
            .move_file(&target, &root, LibraryAction::BetterQuality, user);
        let Some(target_trash) = out.quarantine_path.filter(|_| out.moved) else {
            return failed(&format!(
                "Nothing changed: the marked copy could not be set aside ({}).",
                out.error.unwrap_or_default()
            ));
        };
        self.journal.complete(
            &target_key,
            LibraryActionState::Pending,
            Some("Moved to quarantine; finishing."),
            Some(&target_trash),
            None,
            None,
        );
        self.journal.flush();
        if let Err(e) = std::fs::rename(&staged, &target.absolute_path) {
            self.quarantine.restore(&target_trash, Some(&root));
            return failed(&format!(
                "Nothing changed: the best sound could not be moved in ({e})."
            ));
        }
        self.journal.complete(
            &target_key,
            LibraryActionState::Pending,
            Some("Replacement placed; finishing."),
            Some(&target_trash),
            None,
            Some(&target.absolute_path),
        );
        self.journal.flush();

        // The best-sounding copy's own file goes to the trash as a removed copy, with its lyrics.
        let audio_key = LibraryActionJournal::make_key(LibraryAction::Delete, audio_id, &stamp);
        self.journal
            .record(Self::entry(&audio_key, LibraryAction::Delete, &audio, user, true));
        self.journal.flush();
        let gone = self
            .quarantine
            .move_file_with_sidecars(&audio, &root, LibraryAction::Delete, user, true);
        let Some(audio_trash) = gone.quarantine_path.filter(|_| gone.moved) else {
            let why = format!(
                "Nothing changed: the best-sounding copy could not be moved to the trash ({}).",
                gone.error.unwrap_or_default()
            );
            self.journal.complete(
                &audio_key,
                LibraryActionState::Failed,
                Some(&why),
                None,
                None,
                None,
            );
            // The moved-in file is a copy made here a moment ago; the best-sounding copy is
            // still in place, so the copy goes and the marked copy comes back.
            let _ = std::fs::remove_file(&target.absolute_path);
            self.quarantine.restore(&target_trash, Some(&root));
            self.journal.complete(
                &target_key,
                LibraryActionState::Failed,
                Some(&why),
                Some(&target_trash),
                None,
                None,
            );
            self.journal.flush();
            return FixAnswer::new("failed", Some(&why));
        };
        self.journal.complete(
            &audio_key,
            LibraryActionState::Applied,
            Some("Removed this copy. Its sound moved into the copy people marked."),
            Some(&audio_trash),
            None,
            None,
        );
        self.journal.flush();
        match self.library.forget_mapping(&audio.absolute_path).await {
            Ok(forgotten) => self.quarantine.record_mappings(&audio_trash, &forgotten),
            Err(e) => warn!(
                "The outside songs of {} could not be forgotten: {e}",
                audio.absolute_path
            ),
        }

        let metadata = std::fs::metadata(&target.absolute_path).ok();
        info!(
            "{user} moved the sound of {} into {} to keep its marks",
            audio.absolute_path, target.absolute_path
        );
        let words = format!(
            "Moved the sound of {} into {}.",
            file_name(&audio.absolute_path),
            file_name(&target.absolute_path)
        );
        self.moves.lock().insert(
            target_id.to_string(),
            Moved {
                target,
                audio,
                user: user.to_string(),
                target_key,
                target_trash,
                audio_key,
                audio_trash,
                size: metadata.as_ref().map_or(0, std::fs::Metadata::len),
                modified: metadata.and_then(|m| m.modified().ok()),
                rewritten: Vec::new(),
            },
        );
        FixAnswer::new("applied", Some(&words))
    }

    /// Puts both copies of one move back as they were: the marked copy's own file at its path,
    /// the best-sounding copy's out of the trash. Only while the moved-in file has not changed
    /// since. Playlist places moved over go back at the end of the run ([`Self::finish_run`]).
    pub async fn move_back(&self, target_id: &str, user: &str) -> FixAnswer {
        let Some(moved) = self.moves.lock().get(target_id).cloned() else {
            return FixAnswer::new("skipped", Some("There is no move of this song to put back."));
        };
        if self.settings.current().library_actions.dry_run {
            return FixAnswer::new(
                "rehearsed",
                Some(&format!(
                    "Dry run: would put back {} and {}",
                    moved.target.absolute_path, moved.audio.absolute_path
                )),
            );
        }
        match self.put_back(&moved, &format!("Put back by {user}.")).await {
            Ok(()) => {
                self.moves.lock().remove(target_id);
                if !moved.rewritten.is_empty() {
                    self.returning.lock().insert(target_id.to_string(), moved);
                }
                FixAnswer::new("applied", Some("Put both copies back as they were."))
            }
            Err(why) => FixAnswer::new("failed", Some(&why)),
        }
    }

    /// The marked copy back at its path and the best-sounding copy out of the trash. Only while
    /// the file moved in is exactly as the move left it: anything newer at the path stays.
    async fn put_back(&self, moved: &Moved, detail: &str) -> Result<(), String> {
        let Some(_claim) = self
            .executor
            .claim_songs(&[&moved.target.absolute_path, &moved.audio.absolute_path])
        else {
            return Err(LibraryActionExecutor::BUSY_TEXT.to_string());
        };
        let _share = self.write_share().await;
        let now = std::fs::metadata(&moved.target.absolute_path).ok();
        let unchanged = now
            .as_ref()
            .is_some_and(|m| m.len() == moved.size && m.modified().ok() == moved.modified);
        if !unchanged {
            return Err(CHANGED_SINCE.to_string());
        }
        let root = self.resolver.music_root();
        let path = Path::new(&moved.target.absolute_path);
        let aside = path.with_file_name(format!(
            "{STAGING_PREFIX}back-{}.{}",
            uuid::Uuid::new_v4().simple(),
            path.extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        std::fs::rename(path, &aside).map_err(|e| format!("Could not put it back: {e}."))?;
        let restored = self.quarantine.restore(&moved.target_trash, Some(&root));
        if !restored.moved {
            let _ = std::fs::rename(&aside, path);
            return Err(format!(
                "Could not put back the marked copy: {}.",
                restored.error.unwrap_or_default()
            ));
        }
        // A copy made here; the best-sounding copy's own file comes out of the trash next.
        let _ = std::fs::remove_file(&aside);
        self.journal.complete(
            &moved.target_key,
            LibraryActionState::Restored,
            Some(detail),
            None,
            None,
            None,
        );
        let audio = self.quarantine.restore(&moved.audio_trash, Some(&root));
        if !audio.moved {
            self.journal.flush();
            return Err(format!(
                "Put back the marked copy, but the best-sounding copy is still in the trash: {}.",
                audio.error.unwrap_or_default()
            ));
        }
        if let Some(mappings) = audio.mappings.as_ref().filter(|m| !m.is_empty())
            && let Err(e) = self
                .library
                .restore_mappings(mappings, &moved.audio.absolute_path)
                .await
        {
            warn!(
                "The outside songs of {} could not be linked to it again: {e}",
                moved.audio.absolute_path
            );
        }
        self.journal.complete(
            &moved.audio_key,
            LibraryActionState::Restored,
            Some(detail),
            None,
            None,
            None,
        );
        self.journal.flush();
        info!(
            "Put back {} and {} as they were",
            moved.target.absolute_path, moved.audio.absolute_path
        );
        Ok(())
    }

    /// Removes the hidden copies a move makes beside a song (`.octo-keep-…`), which a stop part
    /// way leaves behind. Run at startup, when no move is going. Answers how many went.
    pub async fn sweep_staging(&self) -> usize {
        let root = PathBuf::from(self.resolver.music_root());
        tokio::task::spawn_blocking(move || sweep_staging_under(&root))
            .await
            .unwrap_or(0)
    }

    // ---- the end of a run -----------------------------------------------------------------

    /// After a run: every song moved into is checked until Navidrome shows it as the same song,
    /// and its playlist places are moved over then; a move Navidrome does not keep is put back.
    /// Every song put back gets its playlist places back once Navidrome shows it again. Answers
    /// the moves that were put back, each with why, and notes for the run's summary.
    pub async fn finish_run(
        &self,
        moved: &[String],
        back: &[String],
    ) -> (Vec<(String, String)>, Vec<String>) {
        let records: Vec<Moved> = {
            let moves = self.moves.lock();
            moved.iter().filter_map(|id| moves.get(id).cloned()).collect()
        };
        let checked = join_all(records.into_iter().map(|record| async move {
            let request = LibraryActionRequest::new(
                LibraryAction::BetterQuality,
                record.target.navidrome_id.clone(),
                record.user.clone(),
            );
            let kept = self
                .executor
                .confirm_history_kept(
                    &request,
                    &record.target,
                    &record.target.absolute_path,
                    &record.target_key,
                    false,
                )
                .await;
            (record, kept)
        }))
        .await;
        let mut failed = Vec::new();
        let mut notes = Vec::new();
        for (record, kept) in checked {
            let id = record.target.navidrome_id.clone();
            if !kept {
                let why =
                    "Navidrome did not show the moved song as the same song, so both copies were put back.";
                match self.put_back(&record, why).await {
                    Ok(()) => {
                        self.journal.complete(
                            &record.target_key,
                            LibraryActionState::Failed,
                            Some(why),
                            None,
                            Some(false),
                            None,
                        );
                        self.journal.flush();
                        failed.push((id.clone(), why.to_string()));
                    }
                    Err(problem) if problem == CHANGED_SINCE => failed.push((
                        id.clone(),
                        "Navidrome did not show the moved song as the same song, but the file has changed since, so both copies were left as they are."
                            .to_string(),
                    )),
                    Err(problem) => failed.push((id.clone(), format!("{why} {problem}"))),
                }
                self.moves.lock().remove(&id);
                continue;
            }
            let rewrite = self.rewrite(&record.audio.navidrome_id, &id, None).await;
            notes.extend(rewrite_notes(&record.target.title, &rewrite));
            if let Some(stored) = self.moves.lock().get_mut(&id) {
                stored.rewritten = rewrite.moved;
            }
        }
        let returning: Vec<Moved> = {
            let mut returning = self.returning.lock();
            back.iter().filter_map(|id| returning.remove(id)).collect()
        };
        for record in returning {
            let shown = self
                .executor
                .wait_until_shown(&record.audio.navidrome_id, &record.audio.absolute_path)
                .await;
            if !shown {
                notes.push(format!(
                    "Navidrome has not shown {} again yet, so its playlist places stay with the other copy.",
                    record.audio.title
                ));
                continue;
            }
            let rewrite = self
                .rewrite(
                    &record.target.navidrome_id,
                    &record.audio.navidrome_id,
                    Some(&record.rewritten),
                )
                .await;
            if !rewrite.failed.is_empty() {
                notes.push(format!(
                    "Some playlist places of {} could not be put back, in {}.",
                    record.audio.title,
                    rewrite.failed.join(", ")
                ));
            }
        }
        (failed, notes)
    }

    // ---- playlists ------------------------------------------------------------------------

    /// Moves every playlist entry of `from` onto `to`, each in its own place: or only the
    /// places in `only`. Smart and synced playlists are left as they are and named.
    pub async fn rewrite(&self, from: &str, to: &str, only: Option<&[(String, usize)]>) -> Rewrite {
        let mut done = Rewrite::default();
        let Some(playlists) = self.api.list_playlist_kinds().await else {
            if only.is_some_and(|only| !only.is_empty()) {
                done.failed.push("every playlist".to_string());
            }
            return done;
        };
        for playlist in &playlists {
            let wanted: Option<Vec<usize>> = only.map(|only| {
                only.iter()
                    .filter(|(id, _)| *id == playlist.id)
                    .map(|(_, position)| *position)
                    .collect()
            });
            if wanted.as_ref().is_some_and(Vec::is_empty) {
                continue;
            }
            let Ok(Some(tracks)) = self.api.read_all_tracks(&playlist.id).await else {
                if wanted.is_some() {
                    done.failed.push(playlist.name.clone());
                }
                continue;
            };
            let mut positions: Vec<usize> = tracks
                .iter()
                .filter(|track| track.media_file_id == from)
                .filter_map(|track| track.position.trim().parse().ok())
                .collect();
            if let Some(wanted) = &wanted {
                positions.retain(|position| wanted.contains(position));
            }
            if positions.is_empty() {
                continue;
            }
            if !playlist.editable() {
                done.skipped.push(playlist.name.clone());
                continue;
            }
            positions.sort_unstable();
            for position in positions {
                if self.swap_entry(playlist, position, from, to).await {
                    done.moved.push((playlist.id.clone(), position));
                } else {
                    done.failed.push(playlist.name.clone());
                    break;
                }
            }
        }
        done
    }

    /// The entry of `from` at `position` becomes `to`, in the same place: `to` is added at the
    /// end, moved up to `position`, and `from`, one place down now, is removed.
    async fn swap_entry(&self, playlist: &PlaylistKind, position: usize, from: &str, to: &str) -> bool {
        let at = |tracks: &[crate::services::library::library_action_playlist_worker::PlaylistTrackRow],
                  place: usize| {
            tracks
                .iter()
                .find(|track| track.position.trim().parse::<usize>().ok() == Some(place))
                .map(|track| track.media_file_id.clone())
        };
        if !matches!(
            self.api.add_tracks(&playlist.id, &[to.to_string()]).await,
            Ok(true)
        ) {
            return false;
        }
        let Ok(Some(tracks)) = self.api.read_all_tracks(&playlist.id).await else {
            return false;
        };
        let last = tracks
            .iter()
            .filter_map(|track| track.position.trim().parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        if at(&tracks, last).as_deref() != Some(to) {
            return false;
        }
        if last != position && !matches!(self.api.move_track(&playlist.id, last, position).await, Ok(true)) {
            return false;
        }
        let Ok(Some(tracks)) = self.api.read_all_tracks(&playlist.id).await else {
            return false;
        };
        if at(&tracks, position).as_deref() != Some(to) || at(&tracks, position + 1).as_deref() != Some(from)
        {
            return false;
        }
        matches!(
            self.api
                .remove_positions(&playlist.id, &[(position + 1).to_string()])
                .await,
            Ok(true)
        )
    }
}

/// What a playlist rewrite leaves for the run's summary, in plain words.
fn rewrite_notes(title: &str, rewrite: &Rewrite) -> Vec<String> {
    let mut notes = Vec::new();
    if !rewrite.skipped.is_empty() {
        notes.push(format!(
            "{} stays as it is in {}: smart and synced playlists cannot be edited.",
            title,
            rewrite.skipped.join(", ")
        ));
    }
    if !rewrite.failed.is_empty() {
        notes.push(format!(
            "Some playlist places of {} could not be moved over, in {}.",
            title,
            rewrite.failed.join(", ")
        ));
    }
    notes
}

/// The hidden copies a move makes beside a song.
const STAGING_PREFIX: &str = ".octo-keep-";

/// Every `.octo-keep-…` file under `root`, removed; links are never followed. Answers how many.
pub fn sweep_staging_under(root: &Path) -> usize {
    let mut removed = 0;
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file() && entry.file_name().to_string_lossy().starts_with(STAGING_PREFIX) {
                match std::fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(e) => warn!("Could not remove {}: {e}", path.display()),
                }
            }
        }
    }
    if removed > 0 {
        info!("Removed {removed} copy(ies) a stopped move left beside songs");
    }
    removed
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |name| name.to_string_lossy().into_owned())
}

/// Whether a step's tag names are all ones a fix may write, apart from `from`.
pub fn unknown_tag(tags: &IndexMap<String, String>) -> Option<&str> {
    tags.keys()
        .map(String::as_str)
        .filter(|key| *key != "from")
        .find(|key| song_tag_fields::canonical(key).is_none() || is_blank(key))
}

#[cfg(test)]
#[path = "keep_marks_tests.rs"]
mod tests;
