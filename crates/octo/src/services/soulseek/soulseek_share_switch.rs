//! The Share my library switch. The block it keeps in slskd.yml and the readers of slskd's
//! answers are `octo_core::soulseek::soulseek_share_switch`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use octo_core::settings::SettingsStore;
use octo_core::soulseek::ShareControl;
use octo_core::soulseek::soulseek_share_switch::{
    has_own_shares, normalize, open_uploads, parse_directories, read_scan_state, split_block, with_block,
};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::SoulseekClient;

/// What the switch and the Sharing card ask of slskd: a seam, so the switch can be driven
/// against a scripted slskd.
#[async_trait]
pub trait ISlskdShares: Send + Sync {
    /// slskd.yml as text, with slskd's status (403: remote configuration is off).
    async fn read_settings_file(&self) -> (u16, Option<String>);
    /// Replaces slskd.yml. None when written, otherwise why not.
    async fn write_settings_file(&self, yaml: &str) -> Option<String>;
    /// One of slskd's read-only answers as text, or None.
    async fn read(&self, path: &str) -> Option<String>;
    /// Starts a look through the shared folders. None when it started, otherwise why not.
    async fn rescan_shares(&self) -> Option<String>;
    /// Stops a look in progress.
    async fn cancel_share_scan(&self) -> bool;
    /// Cancels one upload.
    async fn cancel_upload(&self, username: &str, id: &str) -> bool;
    /// Soulseek is on, so slskd may be asked at all.
    fn is_on(&self) -> bool {
        true
    }
}

#[async_trait]
impl ISlskdShares for SoulseekClient {
    async fn read_settings_file(&self) -> (u16, Option<String>) {
        SoulseekClient::read_settings_file(self).await
    }
    async fn write_settings_file(&self, yaml: &str) -> Option<String> {
        SoulseekClient::write_settings_file(self, yaml).await
    }
    async fn read(&self, path: &str) -> Option<String> {
        SoulseekClient::read(self, path).await
    }
    async fn rescan_shares(&self) -> Option<String> {
        SoulseekClient::rescan_shares(self).await
    }
    async fn cancel_share_scan(&self) -> bool {
        SoulseekClient::cancel_share_scan(self).await
    }
    async fn cancel_upload(&self, username: &str, id: &str) -> bool {
        SoulseekClient::cancel_upload(self, username, id).await
    }
    fn is_on(&self) -> bool {
        SoulseekClient::is_on(self)
    }
}

/// The Share my library switch. slskd 0.26 cannot change its shares through an options patch
/// (that only covers the listening address), but it does let an administrator replace its
/// settings file, slskd.yml, through its API when remote configuration is on (the bundled
/// compose file turns it on). slskd checks the new file, keeps the old one as slskd.yml.bak and
/// applies it on the spot through its file watch: its share list is read live, no restart. So
/// Octo keeps one marked block in slskd.yml with the share list, waits until slskd reports the
/// new list, then has slskd look through its folders again, which also drops everything no
/// longer shared. Off writes an empty list and cancels uploads still waiting or under way, so
/// off means nothing is shared.
///
/// Octo never touches shares it did not write: a "shares:" section of the person's own in
/// slskd.yml, or shares from the slskd container's environment, make the switch stand aside.
/// It checks every 10 minutes that slskd still matches the switch, so a recreated slskd with a
/// fresh settings file is put right without anyone noticing.
pub struct SoulseekShareSwitch {
    slskd: Arc<dyn ISlskdShares>,
    settings: Arc<SettingsStore>,
    lock: tokio::sync::Mutex<()>,
    state: Mutex<(ShareControl, Option<String>)>,
    /// How long between two looks for slskd to catch up. Only tests shorten it.
    poll_every: Duration,
}

impl SoulseekShareSwitch {
    /// How often the background check looks, once slskd matches.
    pub const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
    /// How soon it looks again after a try that failed.
    pub const RETRY_EVERY: Duration = Duration::from_secs(60);
    /// How many times, half a second apart, to look for slskd to catch up: 15 seconds.
    pub const POLLS: usize = 30;

    pub fn new(slskd: Arc<dyn ISlskdShares>, settings: Arc<SettingsStore>) -> Self {
        Self::with_poll_every(slskd, settings, Duration::from_millis(500))
    }

    /// A switch that waits `poll_every` between looks (the tests' scripted slskd needs none).
    pub fn with_poll_every(
        slskd: Arc<dyn ISlskdShares>,
        settings: Arc<SettingsStore>,
        poll_every: Duration,
    ) -> Self {
        SoulseekShareSwitch {
            slskd,
            settings,
            lock: tokio::sync::Mutex::new(()),
            state: Mutex::new((ShareControl::Unknown, None)),
            poll_every,
        }
    }

    /// Who decides slskd's shares, as of the last look.
    pub fn control(&self) -> ShareControl {
        self.state.lock().0
    }

    /// Why the switch could not do its job the last time it tried, or None.
    pub fn problem(&self) -> Option<String> {
        self.state.lock().1.clone()
    }

    fn set_control(&self, control: ShareControl) {
        self.state.lock().0 = control;
    }

    /// The hosted service: keeps slskd to the saved switch, every 10 minutes once it matches
    /// and every minute while it cannot be made to.
    pub async fn run(self: Arc<Self>, stopping: CancellationToken) -> anyhow::Result<()> {
        while !stopping.is_cancelled() {
            if !self.slskd.is_on() {
                tokio::select! {
                    _ = stopping.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(Self::CHECK_EVERY) => continue,
                }
            }
            let on = self.settings.current().soulseek.share_library;
            let ok = tokio::select! {
                _ = stopping.cancelled() => return Ok(()),
                problem = self.apply(on, false) => problem.is_none(),
            };
            let wait = if ok || matches!(self.control(), ShareControl::Outside | ShareControl::Locked) {
                Self::CHECK_EVERY
            } else {
                Self::RETRY_EVERY
            };
            tokio::select! {
                _ = stopping.cancelled() => return Ok(()),
                _ = tokio::time::sleep(wait) => {}
            }
        }
        Ok(())
    }

    /// Makes slskd share the library, or nothing. `chosen` is true when a person just flipped
    /// the switch, and false for the background check, which never takes over shares Octo did
    /// not write and never writes anything to an slskd that already matches. None when slskd
    /// now matches; otherwise why not, in words for the dashboard.
    pub async fn apply(&self, on: bool, chosen: bool) -> Option<String> {
        let _guard = self.lock.lock().await;
        let problem = self.apply_locked(on, chosen).await;
        self.state.lock().1 = problem.clone();
        if let Some(problem) = &problem
            && chosen
        {
            warn!(
                "Soulseek sharing could not be turned {}: {problem}",
                if on { "on" } else { "off" }
            );
        }
        problem
    }

    async fn apply_locked(&self, on: bool, chosen: bool) -> Option<String> {
        let desired = if on {
            self.settings.current().soulseek.shared_folder_list()
        } else {
            Vec::new()
        };
        if on && desired.is_empty() {
            return Some("No folder to share is set (Soulseek.SharedFolders is empty).".to_string());
        }

        let (status, yaml) = self.slskd.read_settings_file().await;
        if status == 403 {
            self.set_control(ShareControl::Locked);
            return Some("slskd does not let Octo change its shares, because its remote configuration is off. Set SLSKD_REMOTE_CONFIGURATION=true on the slskd container, or set shares in slskd itself.".to_string());
        }
        let Some(yaml) = yaml else {
            self.set_control(ShareControl::Unknown);
            return Some("Octo cannot reach slskd.".to_string());
        };

        let (rest, ours) = split_block(&yaml);
        let mut actual = parse_directories(self.slskd.read("options").await.as_deref());
        if has_own_shares(&rest) {
            self.set_control(ShareControl::Outside);
            return Some("slskd.yml has a shares section of its own, so slskd's shares are set there, not here. Remove that section to let this switch decide.".to_string());
        }
        if ours.is_none() && !chosen {
            // Never written by Octo. Nothing to do when slskd already shares nothing and the
            // switch is off; shares from somewhere else are left alone until someone flips the
            // switch.
            match &actual {
                Some(list) if list.is_empty() && !on => {
                    self.set_control(ShareControl::Octo);
                    return None;
                }
                Some(list) if !list.is_empty() => {
                    self.set_control(ShareControl::Outside);
                    return None;
                }
                _ => {}
            }
        }

        let wanted = with_block(&rest, &desired);
        let changed = normalize(&wanted) != normalize(&yaml);
        if changed && let Some(refused) = self.slskd.write_settings_file(&wanted).await {
            return Some(refused);
        }

        // slskd applies a new settings file through its file watch, a moment after it is written.
        let mut poll = 0;
        while poll < Self::POLLS && actual.as_ref() != Some(&desired) {
            tokio::time::sleep(self.poll_every).await;
            actual = parse_directories(self.slskd.read("options").await.as_deref());
            poll += 1;
        }
        let Some(actual) = actual else {
            self.set_control(ShareControl::Unknown);
            return Some("Octo cannot read slskd's settings to check the change.".to_string());
        };
        if actual != desired {
            self.set_control(ShareControl::Outside);
            return Some(if actual.len() > desired.len() {
                format!(
                    "slskd still shares {}, set outside slskd.yml (for example SLSKD_SHARED_DIR on the slskd container). Remove it there and restart slskd to let this switch decide.",
                    actual[desired.len()..].join(", ")
                )
            } else {
                "slskd did not take the new share list. Its settings file watch may be off; restart slskd."
                    .to_string()
            });
        }

        self.set_control(ShareControl::Octo);
        // A new list needs a look through the folders, and so does one slskd has not looked
        // through yet: a look asked for while slskd was still finishing the last is dropped.
        // A look slskd is running by itself (its daily one) is left to finish.
        let needs_look = changed || {
            let idle = read_scan_state(self.slskd.read("application").await.as_deref())
                .is_some_and(|state| !state.busy);
            idle && !self.looked_through(on).await
        };
        if needs_look && let Some(problem) = self.rescan(on).await {
            return Some(problem);
        }
        if !on {
            self.cancel_uploads().await;
        }
        if changed {
            info!(
                "Soulseek sharing turned {}: {}",
                if on { "on" } else { "off" },
                if on {
                    desired.join(", ")
                } else {
                    "nothing shared".to_string()
                }
            );
        }
        None
    }

    /// A look through the folders, which is also what makes slskd drop what it no longer
    /// shares. slskd 0.26 answers a rescan request with OK even when it drops it because a look
    /// is still finishing (it saves and reloads its index for some seconds after the files are
    /// counted), so Octo waits for slskd to be idle first, stopping a look that read the old
    /// list, and then checks the look really started or already did its work.
    async fn rescan(&self, on: bool) -> Option<String> {
        for _ in 0..3 {
            if !self.wait_idle().await {
                self.slskd.cancel_share_scan().await;
                if !self.wait_idle().await {
                    continue;
                }
            }
            let refused = self.slskd.rescan_shares().await;
            if let Some(refused) = refused
                && !octo_core::soulseek::soulseek_client::contains_ignore_case(&refused, "already")
            {
                return Some(refused);
            }
            for _ in 0..Self::POLLS {
                if read_scan_state(self.slskd.read("application").await.as_deref()).is_some_and(|s| s.busy) {
                    return None;
                }
                if self.looked_through(on).await {
                    return None;
                }
                tokio::time::sleep(self.poll_every).await;
            }
        }
        Some("slskd did not start looking through the folders; press Rescan shared folders.".to_string())
    }

    /// Waits until slskd is not looking through its folders. False when it still is.
    async fn wait_idle(&self) -> bool {
        for _ in 0..Self::POLLS {
            if read_scan_state(self.slskd.read("application").await.as_deref()).is_some_and(|s| !s.busy) {
                return true;
            }
            tokio::time::sleep(self.poll_every).await;
        }
        false
    }

    /// Whether slskd's index matches the switch. On: every shared folder has counts, which
    /// slskd only gives a folder once it has looked through it. Off: slskd counts no file at all.
    async fn looked_through(&self, on: bool) -> bool {
        let Some(state) = read_scan_state(self.slskd.read("application").await.as_deref()) else {
            return false;
        };
        if state.busy {
            return false;
        }
        if !on {
            return state.files == Some(0);
        }
        let shares = self.slskd.read("shares").await;
        let folders: Vec<_> = octo_core::soulseek::soulseek_sharing::parse_folders(shares.as_deref(), None)
            .into_iter()
            .filter(|folder| !folder.excluded)
            .collect();
        !folders.is_empty() && folders.iter().all(|folder| folder.files.is_some())
    }

    /// Off means nothing goes out: uploads still waiting or under way are canceled.
    async fn cancel_uploads(&self) {
        let uploads = self.slskd.read("transfers/uploads").await;
        for (user, id) in open_uploads(uploads.as_deref()) {
            self.slskd.cancel_upload(&user, &id).await;
        }
    }
}

#[cfg(test)]
#[path = "soulseek_share_switch_tests.rs"]
mod tests;
