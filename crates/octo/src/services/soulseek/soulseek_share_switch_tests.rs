//! The Share my library switch against a scripted slskd that behaves like 0.26: slskd.yml is read
//! and replaced through the API, and the share list slskd reports follows the file once its file
//! watch has caught up. That the block is replaced in place is tested in
//! `octo_core::soulseek::soulseek_share_switch`; that the share endpoint saves the choice is
//! tested with the controller.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use octo_core::settings::{AppSettings, SettingsStore, SoulseekSettings};
use octo_core::soulseek::ShareControl;
use octo_core::soulseek::soulseek_share_switch::BLOCK_START;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::json;

use super::{ISlskdShares, SoulseekShareSwitch};

// The start of the commented-out slskd.yml slskd writes on first run, with the Linux line ends
// slskd writes whatever this file was checked out with.
const STOCK_FILE: &str =
    "# debug: false\n# remote_configuration: false\n# shares:\n#   directories:\n#     - 'D:\\Music'";

struct FakeState {
    file: String,
    status: u16,
    writes: usize,
    rescans: usize,
    /// The folders slskd last looked through; None before any look.
    scanned: Option<Vec<String>>,
    /// Rescan requests slskd answers OK and then drops, as 0.26 does while a look is finishing.
    drop: usize,
    busy: bool,
    cancelled: Vec<String>,
    /// Shares from somewhere other than slskd.yml, such as SLSKD_SHARED_DIR on the container.
    from_environment: Vec<String>,
    uploads: String,
    /// How many option reads it takes slskd to notice a new file.
    lag: usize,
    reads_since_write: usize,
    seen: Vec<String>,
}

#[derive(Clone)]
struct FakeSlskd(Arc<Mutex<FakeState>>);

impl FakeSlskd {
    fn new() -> Self {
        FakeSlskd(Arc::new(Mutex::new(FakeState {
            file: STOCK_FILE.to_string(),
            status: 200,
            writes: 0,
            rescans: 0,
            scanned: None,
            drop: 0,
            busy: false,
            cancelled: Vec::new(),
            from_environment: Vec::new(),
            uploads: "[]".to_string(),
            lag: 1,
            reads_since_write: 0,
            seen: Vec::new(),
        })))
    }

    fn with(change: impl FnOnce(&mut FakeState)) -> Self {
        let fake = Self::new();
        change(&mut fake.0.lock());
        fake
    }

    fn directories(state: &FakeState) -> Vec<String> {
        // Every uncommented list entry in the file is a share; the stock file's are all comments.
        let entry = Regex::new(r"(?m)^\s+- '(.*)'$").expect("a valid pattern");
        let from_file: Vec<String> = entry
            .captures_iter(&state.file)
            .map(|found| found[1].replace("''", "'"))
            .collect();
        // A config array merges by position: the file's entries replace the environment's.
        let mut merged = from_file.clone();
        merged.extend(state.from_environment.iter().skip(from_file.len()).cloned());
        merged
    }

    fn switch(&self, on: bool) -> SoulseekShareSwitch {
        self.switch_sharing(on, "[Music]/share")
    }

    fn switch_sharing(&self, on: bool, folders: &str) -> SoulseekShareSwitch {
        {
            let mut state = self.0.lock();
            state.seen = Self::directories(&state);
        }
        let settings = Arc::new(SettingsStore::for_tests(AppSettings {
            soulseek: SoulseekSettings {
                share_library: on,
                shared_folders: folders.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }));
        SoulseekShareSwitch::with_poll_every(Arc::new(self.clone()), settings, Duration::ZERO)
    }

    fn state(&self) -> parking_lot::MutexGuard<'_, FakeState> {
        self.0.lock()
    }
}

#[async_trait]
impl ISlskdShares for FakeSlskd {
    async fn read_settings_file(&self) -> (u16, Option<String>) {
        let state = self.0.lock();
        (state.status, (state.status == 200).then(|| state.file.clone()))
    }

    async fn write_settings_file(&self, yaml: &str) -> Option<String> {
        let mut state = self.0.lock();
        state.file = yaml.to_string();
        state.writes += 1;
        state.reads_since_write = 0;
        None
    }

    async fn read(&self, path: &str) -> Option<String> {
        let mut state = self.0.lock();
        let answer = match path {
            "options" => {
                state.reads_since_write += 1;
                if state.reads_since_write > state.lag {
                    state.seen = Self::directories(&state);
                }
                json!({ "shares": { "directories": state.seen } })
            }
            "application" => {
                let files = state.scanned.as_ref().map_or(0, Vec::len) * 2;
                json!({ "shares": { "scanning": state.busy, "ready": !state.busy, "files": files } })
            }
            "shares" => json!({
                "local": state.seen.iter().map(|dir| json!({
                    "alias": "Music",
                    "localPath": dir,
                    "isExcluded": false,
                    "files": if state.scanned.as_ref().is_some_and(|s| s.contains(dir)) { json!(2) } else { json!(null) },
                })).collect::<Vec<_>>(),
            }),
            "transfers/uploads" => return Some(state.uploads.clone()),
            other => panic!("unexpected read {other}"),
        };
        Some(answer.to_string())
    }

    async fn rescan_shares(&self) -> Option<String> {
        let mut state = self.0.lock();
        state.rescans += 1;
        if state.drop > 0 {
            state.drop -= 1;
        } else {
            state.scanned = Some(state.seen.clone());
        }
        None
    }

    async fn cancel_share_scan(&self) -> bool {
        true
    }

    async fn cancel_upload(&self, username: &str, id: &str) -> bool {
        self.0.lock().cancelled.push(format!("{username}/{id}"));
        true
    }
}

#[tokio::test]
async fn on_writes_octos_block_waits_for_slskd_then_rescans() {
    let slskd = FakeSlskd::new();
    let share = slskd.switch(true);

    assert_eq!(share.apply(true, true).await, None);

    assert_eq!(share.control(), ShareControl::Octo);
    {
        let state = slskd.state();
        assert_eq!(FakeSlskd::directories(&state), ["[Music]/share"]);
        assert!(state.file.starts_with(STOCK_FILE.trim_end()));
        assert!(state.file.contains(BLOCK_START));
        assert_eq!(state.rescans, 1);
        assert!(state.cancelled.is_empty());
    }

    // Already as wanted: nothing is written and slskd is not made to look again.
    assert_eq!(share.apply(true, false).await, None);
    assert_eq!(slskd.state().writes, 1);
    assert_eq!(slskd.state().rescans, 1);
}

/// slskd 0.26 says OK to a rescan it then drops while the last look is finishing. The switch
/// checks the look happened and asks again; a later background check puts right a list slskd
/// never looked through, without writing anything.
#[tokio::test]
async fn a_dropped_rescan_is_asked_again_and_the_background_check_heals() {
    let slskd = FakeSlskd::with(|s| s.drop = 1);
    assert_eq!(slskd.switch(true).apply(true, true).await, None);
    assert_eq!(slskd.state().rescans, 2);
    assert_eq!(
        slskd.state().scanned.as_deref(),
        Some(&["[Music]/share".to_string()][..])
    );

    slskd.state().scanned = None;
    assert_eq!(slskd.switch(true).apply(true, false).await, None);
    assert_eq!(slskd.state().writes, 1);
    assert_eq!(
        slskd.state().scanned.as_deref(),
        Some(&["[Music]/share".to_string()][..])
    );

    // A look slskd is running by itself is never stopped by the background check.
    slskd.state().scanned = None;
    slskd.state().busy = true;
    assert_eq!(slskd.switch(true).apply(true, false).await, None);
    assert_eq!(slskd.state().rescans, 3);
}

#[tokio::test]
async fn off_shares_nothing_rescans_so_slskd_forgets_and_cancels_open_uploads() {
    let slskd = FakeSlskd::new();
    slskd.switch(true).apply(true, true).await;
    slskd.state().uploads = r#"[ { "username": "alice", "directories": [ { "directory": "Music", "files": [
        { "id": "u1", "state": "InProgress" },
        { "id": "u2", "state": "Queued, Locally" },
        { "id": "u3", "state": "Completed, Succeeded" } ] } ] } ]"#
        .to_string();

    let share = slskd.switch(false);
    assert_eq!(share.apply(false, true).await, None);

    let state = slskd.state();
    assert!(FakeSlskd::directories(&state).is_empty());
    assert!(state.file.contains("  directories: []"));
    assert_eq!(state.file.split(BLOCK_START).count() - 1, 1);
    assert_eq!(state.rescans, 2);
    assert_eq!(state.cancelled, ["alice/u1", "alice/u2"]);
}

/// The background check on an install that never shared, with the switch off: nothing to do,
/// so slskd.yml is not touched at all.
#[tokio::test]
async fn off_on_an_install_that_never_shared_writes_nothing() {
    let slskd = FakeSlskd::new();

    assert_eq!(slskd.switch(false).apply(false, false).await, None);

    assert_eq!(slskd.state().writes, 0);
    assert_eq!(slskd.state().file, STOCK_FILE);
}

/// Shares someone set on the slskd container are theirs: the background check leaves them, and
/// a person flipping the switch is told where they come from.
#[tokio::test]
async fn shares_from_the_environment_are_left_alone_and_named() {
    let slskd = FakeSlskd::with(|s| s.from_environment = vec!["/data/music".to_string()]);

    let background = slskd.switch(false);
    assert_eq!(background.apply(false, false).await, None);
    assert_eq!(background.control(), ShareControl::Outside);
    assert_eq!(slskd.state().writes, 0);

    let chosen = slskd.switch(false);
    let problem = chosen.apply(false, true).await.expect("a problem");
    assert!(problem.contains("/data/music"), "{problem}");
    assert!(problem.contains("SLSKD_SHARED_DIR"), "{problem}");
    assert_eq!(slskd.state().rescans, 0);
    assert_eq!(chosen.problem().as_deref(), Some(problem.as_str()));
}

#[tokio::test]
async fn shares_in_slskd_yml_are_theirs_and_never_overwritten() {
    let own = format!("{STOCK_FILE}\nshares:\n  directories:\n    - '/data/music'\n");
    let slskd = FakeSlskd::with(|s| s.file = own.clone());

    let share = slskd.switch(true);
    let problem = share.apply(true, true).await.expect("a problem");

    assert!(problem.contains("shares section of its own"), "{problem}");
    assert_eq!(share.control(), ShareControl::Outside);
    assert_eq!(slskd.state().file, own);
}

#[tokio::test]
async fn remote_configuration_off_is_said_plainly() {
    let slskd = FakeSlskd::with(|s| s.status = 403);

    let share = slskd.switch(true);
    let problem = share.apply(true, true).await.expect("a problem");

    assert_eq!(share.control(), ShareControl::Locked);
    assert!(problem.contains("SLSKD_REMOTE_CONFIGURATION"), "{problem}");
    assert_eq!(slskd.state().writes, 0);
}

/// No folder to share is a problem in words, before slskd is asked anything.
#[tokio::test]
async fn on_with_no_folder_to_share_is_refused() {
    let slskd = FakeSlskd::new();
    let problem = slskd
        .switch_sharing(true, " ; ")
        .apply(true, true)
        .await
        .expect("a problem");
    assert_eq!(
        problem,
        "No folder to share is set (Soulseek.SharedFolders is empty)."
    );
    assert_eq!(slskd.state().writes, 0);
}
