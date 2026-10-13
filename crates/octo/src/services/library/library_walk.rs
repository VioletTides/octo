//! What every walk of the music folder shares: the trash folder is not part of the library.
//!
//! Delete from disk and the replaced originals go to the trash folder at the top of the music
//! folder (`LibraryActions:QuarantineDirectory`, `.octo-trash` by default), under a dated
//! folder. A walk that went into it would list a trashed song as an album of its own, with an id
//! that depended on the dated folder. So every library walk (the cover scan, the genre re-tag,
//! Find lyrics for the library, the review sweep and the explicit marking) leaves the trash out,
//! by its configured name rather than a repeated string.

use std::io;
use std::path::{Path, PathBuf};

use octo_core::settings::LibraryActionSettings;
use octo_core::settings::family::FAMILY_FOLDER;

/// The trash folder of the music folder `root`: `root/<QuarantineDirectory>`, the folder
/// `LibraryActionQuarantine::root_for` moves files into.
pub fn trash_folder(root: &Path, settings: &LibraryActionSettings) -> PathBuf {
    root.join(settings.effective_quarantine_directory())
}

/// Whether `folder` is the trash folder (or a folder inside it).
pub fn is_trash(folder: &Path, trash: Option<&Path>) -> bool {
    trash.is_some_and(|trash| folder.starts_with(trash))
}

/// Whether a walk of the music folder `root` leaves `folder` out: the trash folder, and the
/// folder of Family members' music kept next to the library (`.octo-family`), which is theirs
/// and not part of the owner's library.
pub fn is_left_out(root: &Path, folder: &Path, trash: Option<&Path>) -> bool {
    is_trash(folder, trash) || folder == root.join(FAMILY_FOLDER)
}

/// Every file under `root`, in all its folders, without the trash folder. Symbolic links to
/// folders are followed, and any folder that cannot be read fails the whole listing.
pub fn all_files(root: &Path, trash: Option<&Path>) -> io::Result<Vec<String>> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder)? {
            let entry = entry?;
            let path = entry.path();
            let is_dir = match entry.file_type()? {
                kind if kind.is_symlink() => std::fs::metadata(&path).is_ok_and(|target| target.is_dir()),
                kind => kind.is_dir(),
            };
            if is_dir {
                if !is_left_out(root, &path, trash) {
                    folders.push(path);
                }
            } else {
                found.push(path.to_string_lossy().into_owned());
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port fix: a walk of the music folder never lists what is in the trash.
    #[test]
    fn a_walk_of_the_music_folder_leaves_the_trash_out() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = dir.path();
        for path in [
            "Artist/Album/01 - Song.flac",
            ".octo-trash/2026-10-08/Artist/Album/02 - Gone.flac",
            "Artist/.octo-trash-not/03 - Kept.flac",
        ] {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().expect("a folder")).expect("made");
            std::fs::write(&file, b"x").expect("written");
        }
        let trash = trash_folder(root, &LibraryActionSettings::default());

        let mut walked: Vec<String> = all_files(root, Some(&trash))
            .expect("listed")
            .into_iter()
            .map(|path| path[root.to_string_lossy().len() + 1..].to_string())
            .collect();
        walked.sort();

        assert_eq!(trash, root.join(".octo-trash"));
        assert_eq!(
            walked,
            [
                "Artist/.octo-trash-not/03 - Kept.flac",
                "Artist/Album/01 - Song.flac"
            ]
        );
        assert_eq!(all_files(root, None).expect("listed").len(), 3);
    }

    #[test]
    fn members_music_next_to_the_library_is_left_out() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = dir.path();
        for path in [
            "Artist/Album/01 - Song.flac",
            ".octo-family/alex/Artist/Album/01 - Song.flac",
            ".octo-family/.pool/Artist/Album/02 - Pooled.flac",
        ] {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().expect("a folder")).expect("made");
            std::fs::write(&file, b"x").expect("written");
        }
        let walked = all_files(root, None).expect("listed");
        assert_eq!(walked.len(), 1, "{walked:?}");
        assert!(walked[0].ends_with("Artist/Album/01 - Song.flac"));
        assert!(!is_left_out(root, &root.join("Artist/.octo-family"), None));
    }

    #[test]
    fn the_trash_folder_follows_its_setting() {
        let settings = LibraryActionSettings {
            quarantine_directory: "Bin".to_string(),
            ..Default::default()
        };

        assert_eq!(
            trash_folder(Path::new("/music"), &settings),
            Path::new("/music/Bin")
        );
        assert!(is_trash(
            Path::new("/music/Bin/2026-10-08"),
            Some(Path::new("/music/Bin"))
        ));
        assert!(!is_trash(
            Path::new("/music/Binary"),
            Some(Path::new("/music/Bin"))
        ));
    }
}
