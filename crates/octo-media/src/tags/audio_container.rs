//! What a file really is, read from its first bytes rather than its name. A peer's file can be
//! named for one format and hold another: a FLAC named .mp3, an M4A named .flac. The tagger
//! picks how to read and write a file by its extension, so a wrong one writes tags the library
//! server cannot read, or none at all, and the library shows the wrong format. Tubifarry renames
//! such files the same way.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::audio::net_format::get_extension;

/// A file renamed to the extension its bytes call for: the old and new extensions, the format
/// in words, and where the file is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFix {
    pub from: String,
    pub to: String,
    pub format: String,
    pub path: String,
}

/// One format: the extension it gets, the other extensions that also name it, and how it reads
/// in a sentence ("a FLAC").
#[derive(Debug)]
struct Kind {
    extension: &'static str,
    aliases: &'static [&'static str],
    name: &'static str,
}

const FLAC: Kind = Kind {
    extension: ".flac",
    aliases: &[],
    name: "a FLAC",
};
const MP3: Kind = Kind {
    extension: ".mp3",
    aliases: &[],
    name: "an MP3",
};
const AAC: Kind = Kind {
    extension: ".aac",
    aliases: &[],
    name: "a raw AAC stream",
};
const M4A: Kind = Kind {
    extension: ".m4a",
    aliases: &[".mp4", ".m4b", ".alac"],
    name: "an M4A",
};
// Ogg holds Vorbis, Opus or FLAC alike, and the tagger reads all three under any of these names.
const OGG: Kind = Kind {
    extension: ".ogg",
    aliases: &[".oga", ".opus"],
    name: "an Ogg",
};
const OPUS: Kind = Kind {
    extension: ".opus",
    aliases: &[".ogg", ".oga"],
    name: "an Opus",
};
const WAV: Kind = Kind {
    extension: ".wav",
    aliases: &[],
    name: "a WAV",
};
const AIFF: Kind = Kind {
    extension: ".aiff",
    aliases: &[".aif", ".aifc"],
    name: "an AIFF",
};
const APE: Kind = Kind {
    extension: ".ape",
    aliases: &[],
    name: "a Monkey's Audio",
};
const WAV_PACK: Kind = Kind {
    extension: ".wv",
    aliases: &[],
    name: "a WavPack",
};
const WMA: Kind = Kind {
    extension: ".wma",
    aliases: &[".asf"],
    name: "a WMA",
};
const DSF: Kind = Kind {
    extension: ".dsf",
    aliases: &[],
    name: "a DSD",
};

const KINDS: [&Kind; 12] = [
    &FLAC, &MP3, &AAC, &M4A, &OGG, &OPUS, &WAV, &AIFF, &APE, &WAV_PACK, &WMA, &DSF,
];

/// The extensions this knows. A file named anything else is never renamed: it is not a music
/// file Octo would tag.
fn known(extension: &str) -> bool {
    KINDS.iter().any(|kind| {
        kind.extension.eq_ignore_ascii_case(extension)
            || kind
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(extension))
    })
}

const ASF_HEADER: [u8; 8] = [0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11];

/// How far past an ID3 tag an MP3's first frame may sit, after padding.
const FRAME_SEARCH_BYTES: usize = 4096;

/// The extension the file's bytes call for (".flac"), or None when they match no format this
/// knows or the file cannot be read.
pub fn detect(path: &str) -> Option<&'static str> {
    detect_kind(path).map(|kind| kind.extension)
}

/// The file name's extension replaced, or added when it has none.
fn change_extension(path: &str, extension: &str) -> String {
    let name_start = path.rfind('/').map_or(0, |slash| slash + 1);
    match path[name_start..].rfind('.') {
        Some(dot) => format!("{}{extension}", &path[..name_start + dot]),
        None => format!("{path}{extension}"),
    }
}

/// Renames a file whose extension names another format than its bytes, before anything tags
/// it. None when the name is already right, the format is unknown, or the file is not named as
/// music at all. A file already at the new name is never overwritten.
pub fn fix_extension(path: &str) -> std::io::Result<Option<ExtensionFix>> {
    let current = get_extension(path);
    if !known(current) {
        return Ok(None);
    }
    let Some(kind) = detect_kind(path) else {
        return Ok(None);
    };
    if kind.extension.eq_ignore_ascii_case(current)
        || kind
            .aliases
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(current))
    {
        return Ok(None);
    }

    let mut target = change_extension(path, kind.extension);
    let file = Path::new(path);
    let directory = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .map(|name| name[..name.len() - current.len()].to_string())
        .unwrap_or_default();
    let mut n = 2;
    while Path::new(&target).exists() {
        target = directory
            .join(format!("{stem} ({n}){}", kind.extension))
            .to_string_lossy()
            .into_owned();
        n += 1;
    }
    // The loop above found a free name, so the rename overwrites nothing.
    std::fs::rename(path, &target)?;
    Ok(Some(ExtensionFix {
        from: current.to_ascii_lowercase(),
        to: kind.extension.to_string(),
        format: kind.name.to_string(),
        path: target,
    }))
}

fn detect_kind(path: &str) -> Option<&'static Kind> {
    let mut file = File::open(PathBuf::from(path)).ok()?;
    let length = file.metadata().ok()?.len();
    let head = read(&mut file, length, 0, 64)?;
    if let Some(kind) = magic(&head) {
        return Some(kind);
    }
    if let Some(frame) = frame(&head, 0) {
        return Some(frame);
    }
    if !head.starts_with(b"ID3") || head.len() < 10 {
        return None;
    }

    // An ID3 tag in front: what follows it says the format. FLACs carry one now and then.
    let size = (u64::from(head[6] & 0x7F) << 21)
        | (u64::from(head[7] & 0x7F) << 14)
        | (u64::from(head[8] & 0x7F) << 7)
        | u64::from(head[9] & 0x7F);
    let end = 10 + size + if head[5] & 0x10 != 0 { 10 } else { 0 };
    let after = read(&mut file, length, end, FRAME_SEARCH_BYTES)?;
    if let Some(behind) = magic(&after) {
        return Some(behind);
    }
    (0..after.len().saturating_sub(1)).find_map(|at| frame(&after, at))
}

/// Formats that start with a fixed signature.
fn magic(head: &[u8]) -> Option<&'static Kind> {
    if head.starts_with(b"fLaC") {
        return Some(&FLAC);
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return Some(&M4A);
    }
    if head.starts_with(b"OggS") {
        return Some(if head.windows(8).any(|window| window == b"OpusHead") {
            &OPUS
        } else {
            &OGG
        });
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WAVE" {
        return Some(&WAV);
    }
    if head.len() >= 12 && head.starts_with(b"FORM") && (&head[8..12] == b"AIFF" || &head[8..12] == b"AIFC") {
        return Some(&AIFF);
    }
    if head.starts_with(b"MAC ") {
        return Some(&APE);
    }
    if head.starts_with(b"wvpk") {
        return Some(&WAV_PACK);
    }
    if head.starts_with(&ASF_HEADER) {
        return Some(&WMA);
    }
    if head.starts_with(b"DSD ") {
        return Some(&DSF);
    }
    None
}

/// An MPEG audio frame's sync at `at`: eleven bits set, then the layer. Layer bits of 00 are an
/// ADTS header, which is AAC; any other layer is MP3's family.
fn frame(bytes: &[u8], at: usize) -> Option<&'static Kind> {
    if at + 1 >= bytes.len() || bytes[at] != 0xFF || (bytes[at + 1] & 0xE0) != 0xE0 {
        return None;
    }
    let version = (bytes[at + 1] >> 3) & 0x03;
    let layer = (bytes[at + 1] >> 1) & 0x03;
    if layer == 0 {
        return if (bytes[at + 1] & 0xF0) == 0xF0 {
            Some(&AAC)
        } else {
            None
        };
    }
    // 01 is a reserved MPEG version: not a frame.
    if version == 1 { None } else { Some(&MP3) }
}

/// Up to `count` bytes from `offset`; None when the file cannot be read (an I/O or permission
/// error answers no format).
fn read(file: &mut File, length: u64, offset: u64, count: usize) -> Option<Vec<u8>> {
    if offset >= length {
        return Some(Vec::new());
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let wanted = (count as u64).min(length - offset) as usize;
    let mut buffer = vec![0u8; wanted];
    let mut read = 0;
    while read < wanted {
        let n = file.read(&mut buffer[read..]).ok()?;
        if n == 0 {
            break;
        }
        read += n;
    }
    buffer.truncate(read);
    Some(buffer)
}

#[cfg(test)]
mod tests {
    //! A file's real format from its first bytes, and the rename that fixes a name that says
    //! another one. Real headers, written to files in a temp folder.

    use super::*;
    use crate::tags::test_support::{flac, mp3};

    struct Folder(tempfile::TempDir);

    impl Folder {
        fn new() -> Self {
            Folder(
                tempfile::Builder::new()
                    .prefix("octo-container-")
                    .tempdir()
                    .expect("a temp folder"),
            )
        }

        fn write(&self, name: &str, bytes: &[u8]) -> String {
            let path = self.0.path().join(name);
            std::fs::write(&path, bytes).expect("written");
            path.to_string_lossy().into_owned()
        }

        fn at(&self, name: &str) -> String {
            self.0.path().join(name).to_string_lossy().into_owned()
        }
    }

    fn concat(parts: &[&[u8]]) -> Vec<u8> {
        parts.iter().flat_map(|part| part.iter().copied()).collect()
    }

    /// An ID3v2.4 tag of `body_size` zero bytes, its size syncsafe.
    fn id3(body_size: usize) -> Vec<u8> {
        let mut bytes = b"ID3".to_vec();
        bytes.extend_from_slice(&[
            0x04,
            0x00,
            0x00,
            ((body_size >> 21) & 0x7F) as u8,
            ((body_size >> 14) & 0x7F) as u8,
            ((body_size >> 7) & 0x7F) as u8,
            (body_size & 0x7F) as u8,
        ]);
        bytes.extend(std::iter::repeat_n(0u8, body_size));
        bytes
    }

    /// An iTunes M4A's ftyp box, as the file starts.
    fn m4a_head() -> Vec<u8> {
        concat(&[
            &[0x00, 0x00, 0x00, 0x20],
            b"ftypM4A ",
            &[0, 0, 0, 0],
            b"M4A mp42isom",
            &[0, 0, 0, 0],
        ])
    }

    /// The first Ogg page of a stream whose first packet starts with `packet`.
    fn ogg_page(packet: &[u8]) -> Vec<u8> {
        concat(&[
            b"OggS",
            &[0x00, 0x02],
            &[0u8; 8],
            &[0x01, 0x00, 0x00, 0x00],
            &[0u8; 4],
            &[0u8; 4],
            &[0x01, packet.len() as u8],
            packet,
        ])
    }

    fn vorbis_page() -> Vec<u8> {
        ogg_page(&concat(&[&[0x01], b"vorbis", &[0u8; 23]]))
    }

    fn wav_head() -> Vec<u8> {
        concat(&[
            b"RIFF",
            &[0x24, 0x00, 0x00, 0x00],
            b"WAVEfmt ",
            &[
                0x10, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x44, 0xAC, 0x00, 0x00,
            ],
        ])
    }

    fn aiff_head() -> Vec<u8> {
        concat(&[b"FORM", &[0x00, 0x00, 0x00, 0x2E], b"AIFFCOMM", &[0u8; 22]])
    }

    /// Two ADTS headers: AAC LC, 44.1 kHz, stereo.
    const ADTS_HEAD: [u8; 14] = [
        0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x00, 0x49, 0x90, 0x02, 0x19, 0x00,
    ];

    #[test]
    fn the_first_bytes_say_the_format() {
        let folder = Folder::new();
        let heads: Vec<(&str, Vec<u8>, Option<&str>)> = vec![
            ("flac", flac(), Some(".flac")),
            ("mp3", mp3(), Some(".mp3")),
            (
                "flac behind an ID3 tag",
                concat(&[&id3(200), &flac()]),
                Some(".flac"),
            ),
            (
                "mp3 behind an ID3 tag and padding",
                concat(&[&id3(64), &[0u8; 300], &mp3()]),
                Some(".mp3"),
            ),
            ("m4a", concat(&[&m4a_head(), &[0u8; 64]]), Some(".m4a")),
            ("ogg vorbis", vorbis_page(), Some(".ogg")),
            (
                "ogg opus",
                ogg_page(&concat(&[
                    b"OpusHead",
                    &[0x01, 0x02, 0x38, 0x01, 0x80, 0xBB, 0x00, 0x00, 0x00, 0x00, 0x00],
                ])),
                Some(".opus"),
            ),
            ("wav", wav_head(), Some(".wav")),
            ("aiff", aiff_head(), Some(".aiff")),
            ("adts aac", ADTS_HEAD.to_vec(), Some(".aac")),
            ("text", b"not music at all".to_vec(), None),
            ("empty", Vec::new(), None),
        ];
        for (label, bytes, expected) in heads {
            let path = folder.write(&format!("{label}.bin"), &bytes);
            assert_eq!(detect(&path), expected, "{label}");
        }
    }

    #[test]
    fn a_flac_named_mp3_is_renamed_flac_and_keeps_its_bytes() {
        let folder = Folder::new();
        let bytes = flac();
        let path = folder.write("01 - Teardrop.mp3", &bytes);

        let fix = fix_extension(&path).expect("renamed").expect("a fix");

        assert_eq!(
            (fix.from.as_str(), fix.to.as_str(), fix.format.as_str()),
            (".mp3", ".flac", "a FLAC")
        );
        assert_eq!(fix.path, folder.at("01 - Teardrop.flac"));
        assert!(!Path::new(&path).exists());
        assert_eq!(std::fs::read(&fix.path).expect("read"), bytes);
    }

    #[test]
    fn an_m4a_named_flac_is_renamed_m4a() {
        let folder = Folder::new();
        let fix = fix_extension(&folder.write("Angel.flac", &concat(&[&m4a_head(), &[0u8; 64]])))
            .expect("renamed")
            .expect("a fix");

        assert_eq!(
            (fix.from.as_str(), fix.to.as_str(), fix.format.as_str()),
            (".flac", ".m4a", "an M4A")
        );
        assert!(Path::new(&folder.at("Angel.m4a")).exists());
    }

    #[test]
    fn an_mp3_named_flac_is_renamed_mp3() {
        let folder = Folder::new();
        let fix = fix_extension(&folder.write("Angel.flac", &concat(&[&id3(32), &mp3()])))
            .expect("renamed")
            .expect("a fix");

        assert_eq!((fix.from.as_str(), fix.to.as_str()), (".flac", ".mp3"));
    }

    #[test]
    fn a_right_name_is_left_alone() {
        let folder = Folder::new();
        for name in ["right.flac", "upper.FLAC"] {
            let path = folder.write(name, &flac());

            assert_eq!(fix_extension(&path).expect("read"), None, "{name}");
            assert!(Path::new(&path).exists(), "{name}");
        }
    }

    #[test]
    fn another_name_for_the_same_format_is_left_alone() {
        let folder = Folder::new();
        // The tagger reads any Ogg under either name, and an MP4 under .mp4 as under .m4a.
        assert_eq!(
            fix_extension(&folder.write("vorbis.opus", &vorbis_page())).expect("read"),
            None
        );
        assert_eq!(
            fix_extension(&folder.write("song.mp4", &concat(&[&m4a_head(), &[0u8; 64]]))).expect("read"),
            None
        );
    }

    #[test]
    fn a_file_not_named_as_music_or_of_no_known_format_is_left_alone() {
        let folder = Folder::new();
        assert_eq!(
            fix_extension(&folder.write("notes.txt", &flac())).expect("read"),
            None
        );
        let unknown = folder.write("mystery.mp3", b"not music at all");
        assert_eq!(fix_extension(&unknown).expect("read"), None);
        assert!(Path::new(&unknown).exists());
    }

    #[test]
    fn a_file_already_at_the_right_name_is_never_overwritten() {
        let folder = Folder::new();
        let existing = folder.write("Teardrop.flac", &[1, 2, 3]);
        let path = folder.write("Teardrop.mp3", &flac());

        let fix = fix_extension(&path).expect("renamed").expect("a fix");

        assert_eq!(fix.path, folder.at("Teardrop (2).flac"));
        assert_eq!(std::fs::read(&existing).expect("read"), [1, 2, 3]);
    }
}
