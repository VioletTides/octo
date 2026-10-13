//! Where members' music lives, and what the owner is told about it.
//!
//! `Family:MembersPath` is `Auto` by default: a `.octo-family` folder in the music folder Octo
//! shares with Navidrome (the one downloads go to: Navidrome's detected music folder, else
//! `Library:DownloadPath`). Both containers already mount that folder at the same path, so a
//! member's library works with no volume of its own. Navidrome skips folders whose names start
//! with a dot, and the music folder's `.ndignore` lists `.octo-family/` as well, so members'
//! music stays out of the owner's library either way. A member's own library has its root
//! inside the folder, and Navidrome reads `.ndignore` files from a library's root down, so the
//! line never hides a member's music from that member.
//!
//! Any other value is a folder of the owner's choosing, which both containers must mount at the
//! same path. Turning Family on writes the folder in use into `Family:MembersPath`, so a later
//! change in what Octo detects never moves members' music. Nothing here moves or copies files.

use std::io::{self, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use indexmap::IndexMap;
use octo_core::json::dom::Node;
use octo_core::settings::family::{FAMILY_FOLDER, next_to_music};
use octo_core::settings::{AppSettings, SettingsStore};
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};

use super::family_service::{FamilyError, FamilyResult};
use super::pool::{LinkMode, POOL_FOLDER, POOL_MARKER};
use crate::app::AppState;
use crate::services::subsonic::NavidromeIdentityService;

/// Where members' music lived before `Auto`: a `/members` volume of its own.
pub const LEGACY_MEMBERS_PATH: &str = "/members";

/// The line in the music folder's `.ndignore` that keeps the members folder out of the
/// owner's library.
pub const NDIGNORE_LINE: &str = ".octo-family/";

/// The enable body's `membersPath` for the folder next to the music.
pub const NEXT_TO_MUSIC_CHOICE: &str = "NextToMusic";

/// How long Octo waits for Navidrome's settings before calling them unknown.
const NAVIDROME_CONFIG_WAIT: Duration = Duration::from_secs(3);

/// The music folder Octo shares with Navidrome: Navidrome's detected music folder, else
/// `Library:DownloadPath`.
pub fn music_root(settings: &SettingsStore, identity: &NavidromeIdentityService) -> String {
    let configured = settings
        .raw("Library:DownloadPath")
        .unwrap_or_else(|| "./downloads".to_string());
    identity.effective_download_path(&configured)
}

/// The members folder for `settings` with the music folder at `music_root`. `Auto` on an
/// install whose pool is already marked in `/members` stays there.
pub fn resolve_in(settings: &AppSettings, music_root: &str) -> String {
    if settings.family.members_path_is_auto() && legacy_members_in_use() {
        return LEGACY_MEMBERS_PATH.to_string();
    }
    settings.family.members_path_in(music_root)
}

/// The members folder now, without a trailing slash.
pub fn resolve(settings: &SettingsStore, identity: &NavidromeIdentityService) -> String {
    resolve_in(&settings.current(), &music_root(settings, identity))
}

/// The members folder `state` uses now, without a trailing slash.
pub fn members_path(state: &AppState) -> String {
    resolve(&state.settings, &state.navidrome_identity)
}

/// Whether `/members` holds a marked pool (looked at once).
fn legacy_members_in_use() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| pool_marked(Path::new(LEGACY_MEMBERS_PATH)))
}

fn pool_marked(root: &Path) -> bool {
    root.join(POOL_FOLDER).join(POOL_MARKER).is_file()
}

// ------------------------------------------------------------------------------------------
// What the pages show
// ------------------------------------------------------------------------------------------

/// Whether members' music is next to the music folder or somewhere the owner chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PlaceMode {
    NextToMusic,
    Custom,
}

/// Where members' music lives, as the dashboard and the Family page show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MembersPlace {
    pub mode: PlaceMode,
    pub path: String,
    /// How members' libraries get a pool song; None before the first link probe.
    pub link_mode: Option<LinkMode>,
    pub message: String,
}

/// A members folder named `.octo-family` sits next to the music; any other is the owner's own.
pub fn mode_of(path: &str) -> PlaceMode {
    if Path::new(path)
        .file_name()
        .is_some_and(|name| name == FAMILY_FOLDER)
    {
        PlaceMode::NextToMusic
    } else {
        PlaceMode::Custom
    }
}

/// The music folder of a members folder next to the music: its parent.
fn music_of(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Where members' music lives now. `skips_dot_folders` is Navidrome's `IgnoreDotFolders`, when
/// known.
pub fn place(state: &AppState, skips_dot_folders: Option<bool>) -> MembersPlace {
    let path = members_path(state);
    let mode = mode_of(&path);
    let message = match mode {
        PlaceMode::NextToMusic => next_to_music_message(&path, skips_dot_folders),
        PlaceMode::Custom => custom_message(&path),
    };
    MembersPlace {
        mode,
        link_mode: state.family.links(state),
        path,
        message,
    }
}

/// What the owner reads about a members folder next to the music.
pub fn next_to_music_message(path: &str, skips_dot_folders: Option<bool>) -> String {
    let music = music_of(path);
    let mut message = format!(
        "Members' music is kept next to your library, in {path}. It stays out of your own library in Navidrome."
    );
    let listed = std::fs::read_to_string(Path::new(&music).join(".ndignore"))
        .is_ok_and(|text| lists_family_folder(&text));
    if listed && skips_dot_folders == Some(false) {
        message.push(' ');
        message.push_str(DOT_FOLDERS_SCANNED);
    } else if !listed && skips_dot_folders != Some(true) {
        message.push_str(&format!(
            " Add the line {NDIGNORE_LINE} to {music}/.ndignore so it stays out even if Navidrome is set to scan folders whose names start with a dot."
        ));
    }
    message
}

/// Said when Navidrome scans folders whose names start with a dot.
const DOT_FOLDERS_SCANNED: &str = "Navidrome is set to scan folders whose names start with a dot, and the .octo-family/ line in your music folder's .ndignore still keeps members' music out of your library.";

/// What the owner reads about a members folder of their own choosing.
pub fn custom_message(path: &str) -> String {
    format!(
        "Members' music is kept in {path}. Mount that folder at the same path in both the octo and navidrome containers, so Navidrome sees what Octo puts there."
    )
}

/// What turning Family on adds to its answer: the mount advice for a folder of the owner's
/// choosing, and the `.ndignore` note when Navidrome scans folders whose names start with a dot.
pub fn turn_on_note(place: &MembersPlace, skips_dot_folders: Option<bool>) -> Option<String> {
    match place.mode {
        PlaceMode::Custom => Some(custom_message(&place.path)),
        PlaceMode::NextToMusic if skips_dot_folders == Some(false) => Some(DOT_FOLDERS_SCANNED.to_string()),
        PlaceMode::NextToMusic => None,
    }
}

/// `navidromeSkipsDotFolders` as the API answers it: `true`, `false` or `"unknown"`.
pub fn dot_folders_json(skips: Option<bool>) -> Value {
    skips.map_or_else(|| Value::String("unknown".into()), Value::Bool)
}

/// Navidrome's `Scanner.IgnoreDotFolders`, read through its admin settings page
/// (`/api/config/config`). None when Navidrome does not offer that page, refuses, or is slow.
pub async fn navidrome_skips_dot_folders(state: &AppState) -> Option<bool> {
    match tokio::time::timeout(NAVIDROME_CONFIG_WAIT, state.navidrome_admin.server_config()).await {
        Ok(Ok(config)) => dot_folder_setting(&config),
        _ => None,
    }
}

/// `config.Scanner.IgnoreDotFolders` in Navidrome's settings answer, as a bool or its text.
pub fn dot_folder_setting(answer: &Value) -> Option<bool> {
    fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
        value
            .as_object()?
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }
    let config = field(answer, "config").unwrap_or(answer);
    match field(field(config, "Scanner")?, "IgnoreDotFolders")? {
        Value::Bool(on) => Some(*on),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

// ------------------------------------------------------------------------------------------
// Turning Family on
// ------------------------------------------------------------------------------------------

/// Where the owner asked members' music to live when turning Family on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// What `Family:MembersPath` says now.
    AsSet,
    /// Next to the music folder.
    NextToMusic,
    /// A full folder path.
    Folder(String),
}

/// Reads the enable body's `membersPath`: absent, `NextToMusic`, or a full folder path.
pub fn parse_choice(raw: Option<&str>) -> FamilyResult<Choice> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(Choice::AsSet);
    };
    if raw.eq_ignore_ascii_case(NEXT_TO_MUSIC_CHOICE) {
        return Ok(Choice::NextToMusic);
    }
    let path = raw.trim_end_matches('/');
    let plain = path
        .split('/')
        .skip(1)
        .all(|part| !part.is_empty() && part != "." && part != "..");
    if !raw.starts_with('/') || path.is_empty() || !plain || path.chars().any(char::is_control) {
        return Err(FamilyError::Invalid(
            "Say membersPath NextToMusic, or a full folder path that starts with /.".into(),
        ));
    }
    Ok(Choice::Folder(path.to_string()))
}

/// Settles where members' music lives as Family is turned on: the folder chosen (or the one
/// set), made and written to, then saved as `Family:MembersPath`. A folder next to the music
/// also gets its `.ndignore` line. Members who already have music keep their folder.
pub async fn settle(state: &AppState, choice: Choice) -> FamilyResult<String> {
    state.navidrome_identity.detect_music_folder(false).await;
    let current = members_path(state);
    let wanted = match choice {
        Choice::AsSet => current.clone(),
        Choice::NextToMusic => next_to_music(&music_root(&state.settings, &state.navidrome_identity)),
        Choice::Folder(path) => path,
    };
    if wanted != current && state.family_store.has_personal_libraries() {
        return Err(FamilyError::Conflict(format!(
            "Members already have music in {current}, and Octo does not move it. Their music stays there."
        )));
    }
    check_writable(&wanted).await?;
    persist(state, &wanted)?;
    keep_out_of_library(&wanted).await;
    Ok(wanted)
}

/// For Family turned on outside the pages (`FAMILY_ENABLED`, `settings.json` by hand): an `Auto`
/// setting is saved as the folder in use, once Octo can write to it.
pub async fn pin_at_start(state: &AppState) {
    if !state.settings.current().family.members_path_is_auto() {
        return;
    }
    state.navidrome_identity.detect_music_folder(false).await;
    let path = members_path(state);
    if let Err(e) = check_writable(&path).await {
        warn!("FAMILY members folder not saved yet: {}", e.message());
        return;
    }
    if let Err(e) = persist(state, &path) {
        warn!("FAMILY members folder not saved: {}", e.message());
        return;
    }
    keep_out_of_library(&path).await;
}

/// Adds the `.ndignore` line for a members folder next to the music. A failure is logged; the
/// pages then say to add the line by hand.
async fn keep_out_of_library(path: &str) {
    if mode_of(path) != PlaceMode::NextToMusic {
        return;
    }
    let music = music_of(path);
    let folder = music.clone();
    match tokio::task::spawn_blocking(move || ensure_ndignore(Path::new(&folder))).await {
        Ok(Ok(true)) => info!("FAMILY added {NDIGNORE_LINE} to {music}/.ndignore"),
        Ok(Ok(false)) => {}
        Ok(Err(e)) => warn!("FAMILY could not add {NDIGNORE_LINE} to {music}/.ndignore: {e}"),
        Err(e) => warn!("FAMILY could not add {NDIGNORE_LINE} to {music}/.ndignore: {e}"),
    }
}

/// Makes the folder and writes a file in it, with the words that fix a refusal.
async fn check_writable(path: &str) -> FamilyResult<()> {
    let root = Path::new(path).to_path_buf();
    let written = tokio::task::spawn_blocking(move || -> io::Result<()> {
        make_folder(&root)?;
        let probe = root.join(format!(
            ".octo-write-probe-{}",
            hex::encode(rand::random::<[u8; 6]>())
        ));
        std::fs::write(&probe, b"octo")?;
        std::fs::remove_file(&probe)
    })
    .await
    .map_err(|e| FamilyError::Unavailable(e.to_string()))?;
    written.map_err(|e| {
        FamilyError::Conflict(match mode_of(path) {
            PlaceMode::NextToMusic => format!(
                "Octo cannot write to {path} ({e}). Members' music is kept in your music folder, so give Octo write access to it."
            ),
            PlaceMode::Custom => format!(
                "Octo cannot write to {path} ({e}). Mount that folder in the octo container so Octo can write to it, and at the same path in the navidrome container."
            ),
        })
    })
}

/// Makes a folder (and its parents) that Navidrome's user can read and the group can write.
fn make_folder(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o775))?;
    }
    Ok(())
}

/// Saves `path` as `Family:MembersPath`.
fn persist(state: &AppState, path: &str) -> FamilyResult<()> {
    if state.settings.settings_path().is_some() {
        let mut section = IndexMap::new();
        section.insert("MembersPath".to_string(), Node::String(path.to_string()));
        let mut patch = IndexMap::new();
        patch.insert("Family".to_string(), Node::Object(section));
        state
            .settings_writer
            .merge(&patch, &[])
            .map_err(|e| FamilyError::Unavailable(format!("Octo could not save the setting: {e}")))?;
        if let Err(e) = state.settings.reload_now() {
            warn!("FAMILY settings could not be read again: {e}");
        }
    }
    if state.settings.current().family.members_path.trim() == path {
        return Ok(());
    }
    if state.settings.settings_path().is_some() && state.settings.raw("Family:MembersPath").is_some() {
        if members_path(state) == path {
            warn!(
                "FAMILY Family:MembersPath in Octo's environment wins over settings.json; it names {path} today"
            );
            return Ok(());
        }
        return Err(FamilyError::Conflict(format!(
            "Family:MembersPath is set to {} in Octo's environment (FAMILY__MEMBERSPATH), which wins over the dashboard. Change it there.",
            state.settings.current().family.members_path
        )));
    }
    let mut settings = (*state.settings.current()).clone();
    settings.family.members_path = path.to_string();
    state.settings.set(settings);
    Ok(())
}

// ------------------------------------------------------------------------------------------
// .ndignore
// ------------------------------------------------------------------------------------------

/// Whether `.ndignore` text already keeps the members folder out.
fn lists_family_folder(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        let line = line.strip_prefix('/').unwrap_or(line);
        line == NDIGNORE_LINE || line == FAMILY_FOLDER
    })
}

/// Makes sure `<music_root>/.ndignore` has the [`NDIGNORE_LINE`]: the file is made when missing,
/// and the line is added at the end when absent; other lines stay as they were, in order. The
/// new file replaces the old one in one rename. True when the file changed.
pub fn ensure_ndignore(music_root: &Path) -> io::Result<bool> {
    let file = music_root.join(".ndignore");
    let existing = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if lists_family_folder(&existing) {
        return Ok(false);
    }
    let newline = if existing.contains("\r\n") { "\r\n" } else { "\n" };
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push_str(newline);
    }
    text.push_str(NDIGNORE_LINE);
    text.push_str(newline);

    let temp = music_root.join(format!(
        ".ndignore.octo-{}",
        hex::encode(rand::random::<[u8; 6]>())
    ));
    let written = (|| -> io::Result<()> {
        let mut out = std::fs::File::create(&temp)?;
        out.write_all(text.as_bytes())?;
        out.sync_all()?;
        if let Ok(meta) = std::fs::metadata(&file) {
            std::fs::set_permissions(&temp, meta.permissions())?;
        }
        std::fs::rename(&temp, &file)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.map(|()| true)
}

// ------------------------------------------------------------------------------------------
// Links on cloud and network storage
// ------------------------------------------------------------------------------------------

/// What the owner reads when the members folder cannot keep links, and how to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHint {
    pub message: String,
    pub fix: String,
}

const EITHER_WAY: &str = "Family works either way.";

/// The hint for the members folder at `path`, from the file system `/proc/mounts` names for it.
pub fn link_hint(path: &str) -> LinkHint {
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let real = std::fs::canonicalize(path)
        .map(|real| real.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    link_hint_from(&mounts, &real)
}

/// [`link_hint`] over the text of a `/proc/mounts`.
pub fn link_hint_from(mounts: &str, path: &str) -> LinkHint {
    let (message, fix) = match mount_type(mounts, Path::new(path)).as_deref() {
        Some("fuse.rclone") => (
            "This disk cannot link files, so a song two members want takes space twice.",
            "Add --vfs-links to the rclone mount that holds your music, then turn Family off and on to check again.",
        ),
        Some("cifs" | "smb3" | "smbfs") => (
            "Your music is on a network share.",
            "Add the mfsymlinks mount option so a song two members want is stored once.",
        ),
        _ => (
            "Your storage cannot keep links, so Family stores a copy for each member who wants a song.",
            "Check your storage's docs for symlink support.",
        ),
    };
    LinkHint {
        message: format!("{message} {fix} {EITHER_WAY}"),
        fix: fix.to_string(),
    }
}

/// The file system type of the mount that holds `path`: the longest mount point above it.
pub fn mount_type(mounts: &str, path: &Path) -> Option<String> {
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _source = fields.next()?;
            let point = unescape_mount(fields.next()?);
            let kind = fields.next()?;
            path.starts_with(&point).then(|| (point.len(), kind.to_string()))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, kind)| kind)
}

/// A `/proc/mounts` field with its octal escapes (`\040` for a space) read back.
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\'
            && at + 3 < bytes.len()
            && (b'0'..=b'3').contains(&bytes[at + 1])
            && bytes[at + 2..at + 4].iter().all(|b| (b'0'..=b'7').contains(b))
        {
            let code = (bytes[at + 1] - b'0') * 64 + (bytes[at + 2] - b'0') * 8 + (bytes[at + 3] - b'0');
            out.push(code);
            at += 4;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[path = "members_place_tests.rs"]
mod tests;
