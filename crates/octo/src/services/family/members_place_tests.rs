use std::sync::Arc;

use octo_core::settings::{AppSettings, SettingsStore, SubsonicSettings};
use serde_json::json;

use super::*;

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("listed")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn auto_path_is_next_to_the_music_folder() {
    let settings = AppSettings::default();
    assert!(settings.family.members_path_is_auto());
    assert_eq!(resolve_in(&settings, "/srv/music/"), "/srv/music/.octo-family");
    assert_eq!(mode_of("/srv/music/.octo-family"), PlaceMode::NextToMusic);
    assert_eq!(mode_of("/members"), PlaceMode::Custom);

    // The same folder downloads go to: Library:DownloadPath when Navidrome's is not detected.
    let music = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(SettingsStore::for_tests(AppSettings {
        subsonic: SubsonicSettings {
            auto_detect_download_path: false,
            ..Default::default()
        },
        ..Default::default()
    }));
    let root = music.path().to_string_lossy().into_owned();
    store.set_raw("Library:DownloadPath", Some(&root));
    let identity = NavidromeIdentityService::new(
        store.clone(),
        crate::services::http_client_factory::default_client(),
    );
    assert_eq!(music_root(&store, &identity), root);
    assert_eq!(resolve(&store, &identity), format!("{root}/.octo-family"));

    // A folder written in the setting is used as it is.
    let mut chosen = AppSettings::default();
    chosen.family.members_path = "/srv/members/".into();
    assert_eq!(resolve_in(&chosen, "/srv/music"), "/srv/members");
}

#[test]
fn ndignore_line_added_once_and_other_lines_kept() {
    let music = tempfile::tempdir().expect("a temp dir");
    let file = music.path().join(".ndignore");
    std::fs::write(&file, "# kept out\nlive/\n*.tmp").expect("written");
    assert!(ensure_ndignore(music.path()).expect("added"));
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "# kept out\nlive/\n*.tmp\n.octo-family/\n"
    );
    assert!(
        !ensure_ndignore(music.path()).expect("checked"),
        "the line is added once"
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "# kept out\nlive/\n*.tmp\n.octo-family/\n"
    );
    assert_eq!(
        files_in(music.path()),
        vec![".ndignore"],
        "no temporary file is left"
    );

    // A line the owner wrote another way counts, and Windows line endings stay.
    std::fs::write(&file, "/.octo-family\r\n").expect("written");
    assert!(!ensure_ndignore(music.path()).expect("checked"));
    std::fs::write(&file, "a\r\nb\r\n").expect("written");
    assert!(ensure_ndignore(music.path()).expect("added"));
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "a\r\nb\r\n.octo-family/\r\n"
    );
}

#[test]
fn ndignore_created_when_missing() {
    let music = tempfile::tempdir().expect("a temp dir");
    assert!(ensure_ndignore(music.path()).expect("made"));
    assert_eq!(
        std::fs::read_to_string(music.path().join(".ndignore")).expect("read"),
        ".octo-family/\n"
    );
    assert_eq!(files_in(music.path()), vec![".ndignore"]);
    // The page says to add the line by hand only while it is missing.
    let path = format!("{}/.octo-family", music.path().to_string_lossy());
    assert!(!next_to_music_message(&path, None).contains("Add the line"));
    let bare = tempfile::tempdir().expect("a temp dir");
    let path = format!("{}/.octo-family", bare.path().to_string_lossy());
    assert!(next_to_music_message(&path, None).contains("Add the line .octo-family/"));
}

const MOUNTS: &str = "\
/dev/sda1 / ext4 rw,relatime 0 0
proc /proc proc rw,nosuid 0 0
gdrive: /music fuse.rclone rw,nosuid,nodev,user_id=0 0 0
//nas/share /mnt/my\\040music cifs rw,vers=3.0 0 0
nas:/export /mnt/nfs nfs4 rw 0 0
host:/srv /mnt/ssh fuse.sshfs rw 0 0
";

#[test]
fn rclone_mount_names_the_links_flag() {
    assert_eq!(
        mount_type(MOUNTS, Path::new("/music/.octo-family")).as_deref(),
        Some("fuse.rclone")
    );
    let hint = link_hint_from(MOUNTS, "/music/.octo-family");
    assert!(
        hint.message.contains(
            "Add --vfs-links to the rclone mount that holds your music, then turn Family off and on to check again."
        ),
        "{hint:?}"
    );
    assert!(hint.message.ends_with("Family works either way."), "{hint:?}");
    assert!(hint.fix.contains("--vfs-links"));
    // A folder on the local disk is not the rclone mount, though its name begins the same.
    assert_eq!(
        mount_type(MOUNTS, Path::new("/musicbox")).as_deref(),
        Some("ext4")
    );
}

#[test]
fn cifs_mount_names_mfsymlinks() {
    let hint = link_hint_from(MOUNTS, "/mnt/my music/.octo-family");
    assert!(
        hint.message.starts_with("Your music is on a network share."),
        "{hint:?}"
    );
    assert!(
        hint.message
            .contains("Add the mfsymlinks mount option so a song two members want is stored once."),
        "{hint:?}"
    );
    assert!(hint.message.ends_with("Family works either way."));
    let smb3 = link_hint_from("//nas/a /data smb3 rw 0 0\n", "/data/x");
    assert!(smb3.fix.contains("mfsymlinks"));
}

#[test]
fn unknown_fuse_mount_gives_the_general_note() {
    let general = "Your storage cannot keep links, so Family stores a copy for each member who wants a song. Check your storage's docs for symlink support. Family works either way.";
    assert_eq!(link_hint_from(MOUNTS, "/mnt/ssh/members").message, general);
    assert_eq!(link_hint_from(MOUNTS, "/mnt/nfs/members").message, general);
    assert_eq!(link_hint_from("", "/members").message, general);
}

#[test]
fn navidrome_dot_folder_answer_is_read() {
    let answer = json!({
        "id": "config",
        "configFile": "/data/navidrome.toml",
        "config": { "Scanner": { "Enabled": true, "IgnoreDotFolders": false } }
    });
    assert_eq!(dot_folder_setting(&answer), Some(false));
    let text = json!({ "config": { "scanner": { "ignoreDotFolders": "true" } } });
    assert_eq!(dot_folder_setting(&text), Some(true));
    assert_eq!(dot_folder_setting(&json!({ "config": {} })), None);
    assert_eq!(dot_folders_json(None), json!("unknown"));
    assert_eq!(dot_folders_json(Some(false)), json!(false));
}

#[test]
fn a_chosen_folder_must_be_a_full_plain_path() {
    assert_eq!(parse_choice(None).expect("fine"), Choice::AsSet);
    assert_eq!(
        parse_choice(Some("nexttomusic")).expect("fine"),
        Choice::NextToMusic
    );
    assert_eq!(
        parse_choice(Some("/srv/family/")).expect("fine"),
        Choice::Folder("/srv/family".into())
    );
    for bad in ["members", "/", "/srv/../etc", "/srv/./x", "/a\nb"] {
        assert!(parse_choice(Some(bad)).is_err(), "{bad}");
    }
}
