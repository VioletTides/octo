//! Soulseek files in words, for the downloads log and Find songs: what each one is, why Octo
//! tried it, and why it would pass one over. Reads only; the choosing itself stays in the
//! download.

use std::sync::LazyLock;

use octo_core::common::dotnet;
use octo_core::common::version_variant;
use octo_core::models::download::AcquisitionCandidate;
use octo_core::models::download::acquisition_event::quality_text;
use octo_core::settings::SoulseekSettings;
use octo_core::soulseek::SoulseekFileHit;
use octo_core::soulseek::soulseek_client::normalize_extension;
use octo_core::soulseek::soulseek_download_service::{
    adds_version, duration_plausible, filename_plausibly_matches_title, from_live_folder, from_version_folder,
};
use regex::Regex;

use super::rejected_peer_registry::RejectedPeerRegistry;
use super::soulseek_download_service::candidate_allowed;
use crate::services::common::acquisition_tracker::size_text;
use crate::services::common::download_picks::SOULSEEK_SOURCE;

const LOSSLESS: [&str; 7] = ["flac", "wav", "alac", "ape", "aiff", "aif", "wv"];

/// "01 - ", "01. ", "1-03 " in front of a title.
static TRACK_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d{1,2}[-.])?\d{1,3}\s*[-._)]?\s+").expect("a fixed pattern"));

static DISC_FOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(cd|disc|disk)\s*\d+$").expect("a fixed pattern"));

/// A file as the log shows it: its own name, the folder it sits in, and what the peer said
/// about it.
pub fn of(hit: &SoulseekFileHit, rank: Option<i32>, note: Option<&str>) -> AcquisitionCandidate {
    let normalized = hit.filename.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    let leaf = parts.last().copied().unwrap_or(hit.filename.as_str());
    let folders: &[&str] = if parts.len() > 1 {
        &parts[..parts.len() - 1]
    } else {
        &[]
    };
    AcquisitionCandidate {
        source: SOULSEEK_SOURCE.to_string(),
        peer: Some(hit.username.clone()),
        file: Some(leaf.to_string()),
        folder: (!folders.is_empty()).then(|| folders[folders.len().saturating_sub(2)..].join("/")),
        format: Some(extension(hit)),
        bit_rate: hit.bit_rate,
        bit_depth: hit.bit_depth,
        sample_rate: hit.sample_rate,
        size: (hit.size > 0).then_some(hit.size),
        length: hit.length,
        queue_length: hit.queue_length,
        free_slot: hit.has_free_upload_slot,
        speed: hit.upload_speed,
        rank,
        note: note.map(str::to_string),
        title: Some(title_of(leaf)),
        album: album_of(folders),
    }
}

/// The file's kind, from the peer's word or its name: "flac".
pub fn extension(hit: &SoulseekFileHit) -> String {
    normalize_extension(Some(&hit.extension), &hit.filename)
}

pub fn is_lossless(hit: &SoulseekFileHit) -> bool {
    let extension = extension(hit);
    LOSSLESS
        .iter()
        .any(|kind| dotnet::eq_ignore_case(kind, &extension))
}

/// "Trying Octo's first choice", "Trying the album folder's copy", ...
pub fn trying_text(attempt: usize, total: usize, from_album: bool, picked: bool) -> String {
    if picked {
        "Trying the copy you picked".to_string()
    } else if from_album {
        "Trying the copy from the album's folder".to_string()
    } else if total <= 1 {
        "Trying the only copy that fits".to_string()
    } else {
        format!("Trying choice {attempt} of {total}")
    }
}

/// Why a peer's copy is worth trying, in a few words: its quality and how soon the peer can
/// send it.
pub fn why_chosen(hit: &SoulseekFileHit) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(quality) = quality_text(
        Some(&extension(hit)),
        hit.bit_rate,
        hit.bit_depth,
        hit.sample_rate,
    ) {
        parts.push(if is_lossless(hit) {
            format!("lossless ({quality})")
        } else {
            quality
        });
    }
    if hit.has_free_upload_slot == Some(true) {
        parts.push("the peer can send it now".to_string());
    } else if let Some(queue) = hit.queue_length.filter(|q| *q > 0) {
        parts.push(format!("{queue} ahead in the peer's queue"));
    }
    if let Some(speed) = hit.upload_speed.filter(|s| *s > 0) {
        parts.push(speed_text(speed));
    }
    if hit.size > 0 {
        parts.push(size_text(hit.size));
    }
    if parts.is_empty() {
        "the best of what was found".to_string()
    } else {
        parts.join(", ")
    }
}

/// What the download's ranking is told about one song: [`judge`]'s arguments.
#[derive(Clone, Copy)]
pub struct Judged<'a> {
    pub title: &'a str,
    pub album: Option<&'a str>,
    pub duration: Option<i32>,
    pub settings: &'a SoulseekSettings,
    pub rejected: Option<&'a RejectedPeerRegistry>,
    pub remembers_rejections: bool,
    pub artist: Option<&'a str>,
}

/// Why the download would pass a file over, or None when it would take it. The same tests the
/// download's ranking applies, read one by one so the first that fails can be named.
pub fn why_not(hit: &SoulseekFileHit, song: Judged<'_>) -> Option<String> {
    judge(hit, song).0
}

/// The reason, and how far the file is from the song: 0 it fits, 1 it is the song in another
/// format, 2 it is likely not the song asked for. Find songs lists them in that order.
pub fn judge(hit: &SoulseekFileHit, song: Judged<'_>) -> (Option<String>, i32) {
    let Judged {
        title,
        album,
        duration,
        settings,
        rejected,
        remembers_rejections,
        artist,
    } = song;
    let not = |text: &str, tier: i32| (Some(text.to_string()), tier);
    if !candidate_allowed(hit, rejected, remembers_rejections) {
        return not(
            "Octo downloaded this file before and it was the wrong recording",
            2,
        );
    }
    if hit.size > 0 && hit.size < settings.min_file_size_bytes && is_lossless(hit) {
        return not("Too small to be the whole song", 2);
    }
    if !filename_plausibly_matches_title(&hit.filename, title, false) {
        return not("The file name does not match the title", 2);
    }
    if !duration_plausible(hit.length, duration, false) {
        return match (hit.length, duration) {
            (Some(seconds), Some(expected)) => (
                Some(format!(
                    "{} long; the song is {}",
                    length_text(seconds),
                    length_text(expected)
                )),
                2,
            ),
            _ => not("A different length from the song", 2),
        };
    }
    if version_variant::mash_up(&hit.filename, Some(title), artist) {
        return not("A mash-up or medley of this song with another", 2);
    }
    if adds_version(&hit.filename, title, artist) {
        return not("Another version: a remix, an edit or a live take", 2);
    }
    if from_live_folder(&hit.filename, title, album, artist) {
        return not("From a live album", 2);
    }
    if from_version_folder(&hit.filename, title, album, artist) {
        return not(
            "From a record of other versions: remixes, edits or a single's radio edit",
            2,
        );
    }
    if version_variant::lacks_requested(&hit.filename, Some(title), artist) {
        return not("Not the version asked for: the file is the plain song", 2);
    }
    if version_variant::cut_not_plain(&hit.filename, Some(title), artist) {
        return not(
            "Its name has more than the song's title, so it is not taken for the edit asked for",
            2,
        );
    }
    let wanted = normalize_extension(Some(&settings.preferred_extension), "");
    if !dotnet::eq_ignore_case(&extension(hit), &wanted) {
        return (
            Some(format!(
                "Not {}, which Octo looks for first",
                dotnet::to_upper_invariant(&wanted)
            )),
            1,
        );
    }
    (None, 0)
}

/// Why a copy picked in Find songs may not be fetched, or None when it may. A file Octo
/// downloaded before and found to be the wrong recording is never fetched again. A pick that
/// would replace a library file meets the download's own name, length and version checks too,
/// only the format preference relaxed: a person can see a copy is lossless, not that a file from
/// a live album or of another version is the song in their library.
pub fn pick_refusal(hit: &SoulseekFileHit, song: Judged<'_>, replacing: bool) -> Option<String> {
    if !candidate_allowed(hit, song.rejected, song.remembers_rejections) {
        return Some("Octo downloaded this file before and it was the wrong recording".to_string());
    }
    if !replacing {
        return None;
    }
    let (note, tier) = judge(hit, song);
    if tier >= 2 { note } else { None }
}

/// "3:58".
pub fn length_text(seconds: i32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// slskd reports bytes a second: "1.2 MB/s", "340 KB/s".
pub fn speed_text(bytes_per_second: i32) -> String {
    if bytes_per_second >= 1024 * 1024 {
        format!(
            "{} MB/s",
            dotnet::format_fixed(f64::from(bytes_per_second) / 1_048_576.0, 1)
        )
    } else {
        format!("{} KB/s", (bytes_per_second / 1024).max(1))
    }
}

/// The song's name from a file's name: no extension, no track number.
pub fn title_of(leaf: &str) -> String {
    let name = match leaf.rfind('.') {
        Some(dot) if dot > 0 => &leaf[..dot],
        _ => leaf,
    };
    let bare = TRACK_NUMBER
        .replace(&dotnet::utf16_class_view(name), "")
        .into_owned();
    // The view keeps byte offsets, so what was taken off the view is taken off the name.
    let bare = &name[name.len() - bare.len()..];
    let bare = bare.trim();
    if bare.is_empty() {
        name.trim().to_string()
    } else {
        bare.to_string()
    }
}

/// The record a file sits in: its folder, or the one above a "CD1" or "Disc 2" folder.
pub fn album_of(folders: &[&str]) -> Option<String> {
    let mut last = *folders.last()?;
    if DISC_FOLDER.is_match(&dotnet::utf16_class_view(last)) && folders.len() > 1 {
        last = folders[folders.len() - 2];
    }
    Some(last.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_and_speeds_read_like_a_person() {
        assert_eq!(length_text(238), "3:58");
        assert_eq!(length_text(5), "0:05");
        assert_eq!(speed_text(1_500_000), "1.4 MB/s");
        assert_eq!(speed_text(348_160), "340 KB/s");
        assert_eq!(speed_text(10), "1 KB/s");
    }

    #[test]
    fn a_title_loses_its_number_and_an_album_its_disc_folder() {
        assert_eq!(title_of("03 - Da Funk.flac"), "Da Funk");
        assert_eq!(title_of("1-03 Da Funk.flac"), "Da Funk");
        assert_eq!(title_of("99.flac"), "99");
        assert_eq!(album_of(&["Homework", "CD1"]).as_deref(), Some("Homework"));
        assert_eq!(album_of(&["Homework"]).as_deref(), Some("Homework"));
        assert_eq!(album_of(&[]), None);
    }
}
