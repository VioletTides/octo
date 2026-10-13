//! Where members' music lives, through the Family page and the dashboard's Family entry.

use std::path::Path;

use super::*;
use crate::services::family::members_place::{self, NDIGNORE_LINE};

/// The music folder the harness gives Octo (`Library:DownloadPath`).
fn music_of(f: &Fam) -> String {
    f.state
        .settings
        .raw("Library:DownloadPath")
        .expect("a music folder")
}

/// Members without a library of their own yet, so the members folder may still change.
fn without_libraries(f: &Fam) {
    for mut member in f.state.family_store.members() {
        member.personal_library_id = None;
        f.state.family_store.upsert_member(member);
    }
}

fn auto(s: &mut AppSettings) {
    s.family.enabled = false;
    s.family.members_path = "Auto".into();
}

#[tokio::test]
async fn turning_on_persists_the_resolved_path() {
    let f = fam_with(auto, None).await;
    let music = music_of(&f);
    let wanted = format!("{music}/.octo-family");
    assert_eq!(members_place::members_path(&f.state), wanted);

    let r = f
        .post(
            "/api/family/enable",
            Some(&f.owner_cookie()),
            json!({ "enabled": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let report = r.json();
    assert_eq!(report["membersPath"], wanted.as_str());
    assert_eq!(report["membersPlace"]["mode"], "NextToMusic", "{report}");
    assert_eq!(report["membersPlace"]["path"], wanted.as_str());
    assert_eq!(report["membersPlace"]["linkMode"], "Copy");
    assert!(
        report["membersPlace"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("Members' music is kept next to your library")),
        "{report}"
    );
    assert_eq!(report["navidromeSkipsDotFolders"], "unknown");
    assert_eq!(
        f.state.settings.current().family.members_path,
        wanted,
        "the folder is saved"
    );
    assert!(Path::new(&wanted).is_dir());
    let ignore = std::fs::read_to_string(Path::new(&music).join(".ndignore")).expect("an .ndignore");
    assert_eq!(ignore.lines().filter(|line| *line == NDIGNORE_LINE).count(), 1);

    // A music folder detected somewhere else later never moves members' music.
    let elsewhere = tempfile::tempdir().expect("a temp dir");
    f.state
        .settings
        .set_raw("Library:DownloadPath", Some(&elsewhere.path().to_string_lossy()));
    assert_eq!(members_place::members_path(&f.state), wanted);

    let session = f.get("/api/family/session", Some(&f.owner_cookie())).await.json();
    assert_eq!(session["membersPlace"]["mode"], "NextToMusic", "{session}");
    assert_eq!(session["membersPath"], wanted.as_str());
    let member = f.get("/api/family/session", Some(&f.alex())).await.json();
    assert_eq!(
        member["membersPlace"],
        Value::Null,
        "only managers are told: {member}"
    );
}

#[tokio::test]
async fn existing_members_path_is_kept() {
    let f = fam_with(|s| s.family.enabled = false, None).await;
    let before = f.state.settings.current().family.members_path.clone();
    assert!(before.ends_with("/members"));

    let r = f
        .post(
            "/api/family/enable",
            Some(&f.owner_cookie()),
            json!({ "enabled": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(f.state.settings.current().family.members_path, before);
    assert_eq!(r.json()["membersPlace"]["mode"], "Custom");
    assert_eq!(r.json()["membersPlace"]["path"], before.as_str());
    assert!(
        !Path::new(&music_of(&f)).join(".ndignore").exists(),
        "a folder of the owner's own leaves the music folder alone"
    );

    // Members with music of their own keep their folder, whatever is asked.
    let r = f
        .post(
            "/api/family/enable",
            Some(&f.owner_cookie()),
            json!({ "enabled": true, "membersPath": "NextToMusic" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert!(r.error().contains("does not move it"), "{}", r.body);
    assert_eq!(f.state.settings.current().family.members_path, before);
}

#[tokio::test]
async fn custom_path_keeps_the_mount_message() {
    let f = fam_with(auto, None).await;
    without_libraries(&f);
    let chosen = f._dir.path().join("family-music").to_string_lossy().into_owned();

    let bad = f
        .post(
            "/api/family/enable",
            Some(&f.owner_cookie()),
            json!({ "enabled": true, "membersPath": "family" }),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);

    let unwritable = f
        .post(
            "/api/family/enable",
            Some(&f.owner_cookie()),
            json!({ "enabled": true, "membersPath": "/proc/octo-family" }),
        )
        .await;
    assert_eq!(unwritable.status, StatusCode::CONFLICT, "{}", unwritable.body);
    assert!(
        unwritable
            .error()
            .contains("Octo cannot write to /proc/octo-family")
            && unwritable
                .error()
                .contains("same path in the navidrome container"),
        "{}",
        unwritable.body
    );
    assert!(
        f.state.settings.current().family.members_path_is_auto(),
        "nothing is saved"
    );

    let r = f
        .post(
            "/api/family/enable",
            Some(&f.owner_cookie()),
            json!({ "enabled": true, "membersPath": format!("{chosen}/") }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let report = r.json();
    let mount = "Mount that folder at the same path in both the octo and navidrome containers";
    assert_eq!(report["membersPlace"]["mode"], "Custom", "{report}");
    assert_eq!(report["membersPlace"]["path"], chosen.as_str());
    assert!(
        report["membersPlace"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(mount)),
        "{report}"
    );
    assert!(
        report["message"].as_str().is_some_and(|m| m.contains(mount)),
        "{report}"
    );
    assert_eq!(f.state.settings.current().family.members_path, chosen);
    assert!(Path::new(&chosen).is_dir());
    assert!(!Path::new(&music_of(&f)).join(".ndignore").exists());
}

#[tokio::test]
async fn navidrome_dot_folder_setting_reported() {
    let f = fam_with(auto, Some("required")).await;
    let admin = format!("octo_browse={}", f.state.browse_sessions.create("winters"));

    // A Navidrome that does not offer its settings page: unknown.
    let summary = f.get("/api/admin/family", Some(&admin)).await;
    assert_eq!(summary.status, StatusCode::OK, "{}", summary.body);
    let summary = summary.json();
    assert_eq!(summary["navidromeSkipsDotFolders"], "unknown", "{summary}");
    assert_eq!(summary["membersPlace"]["mode"], "NextToMusic");
    assert_eq!(summary["membersPlace"]["linkMode"], Value::Null);

    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .and(body_partial_json(json!({ "username": "admin" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "id": "u-admin", "isAdmin": true, "token": "g.h.i", "username": "admin" }),
        ))
        .mount(&f.nd)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/config/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "config",
            "configFile": "/data/navidrome.toml",
            "config": { "Scanner": { "IgnoreDotFolders": false, "FollowSymlinks": true } }
        })))
        .mount(&f.nd)
        .await;
    let summary = f.get("/api/admin/family", Some(&admin)).await.json();
    assert_eq!(summary["navidromeSkipsDotFolders"], false, "{summary}");

    // Turning Family on adds the .ndignore line and says it still keeps the library clean.
    let on = f
        .post(
            "/api/admin/family/enable",
            Some(&admin),
            json!({ "enabled": true, "membersPath": "NextToMusic" }),
        )
        .await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.body);
    let on = on.json();
    assert_eq!(on["navidromeSkipsDotFolders"], false, "{on}");
    let note = "the .octo-family/ line in your music folder's .ndignore still keeps members' music out of your library";
    assert!(on["message"].as_str().is_some_and(|m| m.contains(note)), "{on}");
    assert!(
        on["membersPlace"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(note)),
        "{on}"
    );
}
