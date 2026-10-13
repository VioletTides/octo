//! The pure half of Soulseek sharing: the Sharing card's records and the readers of slskd's
//! answers it is built from. The service that asks slskd, and Soulseek's port test, are
//! `octo::services::soulseek::soulseek_sharing`.

use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::{DateTime, TimeDelta, Utc};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use super::soulseek_client::{contains_ignore_case, parse_server_reading};
use super::soulseek_link::SoulseekLinkState;
use crate::common::dotnet::{self, is_blank};
use crate::json::datetime;

/// One folder slskd shares, as slskd reports it. Alias is the name other people see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedFolder {
    pub alias: String,
    pub path: String,
    pub excluded: bool,
    pub directories: Option<i32>,
    pub files: Option<i32>,
}

/// What people have downloaded from you. Now is what is moving or waiting this minute;
/// the rest covers the last seven days.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadActivity {
    pub sending: i32,
    pub waiting: i32,
    pub files: i32,
    pub bytes: i64,
    pub people: i32,
    pub failed: i32,
    #[serde(with = "datetime::utc_option")]
    pub last_upload_at: Option<DateTime<Utc>>,
}

/// Written as its name, which is what the dashboard reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PortState {
    Unknown,
    Open,
    Closed,
    Off,
}

/// Soulseek's port test's answer for slskd's listening port. CheckedAt is when it was
/// asked; Error says why there is no answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortCheckResult {
    pub state: PortState,
    pub port: Option<i32>,
    #[serde(with = "datetime::utc_option")]
    pub checked_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

impl PortCheckResult {
    pub fn new(
        state: PortState,
        port: Option<i32>,
        checked_at: Option<DateTime<Utc>>,
        error: Option<String>,
    ) -> Self {
        PortCheckResult {
            state,
            port,
            checked_at,
            error,
        }
    }
}

/// Something about sharing that needs a person. Code is stable for the dashboard; Text
/// is what it says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharingWarning {
    pub code: String,
    pub text: String,
}

impl SharingWarning {
    fn new(code: &str, text: impl Into<String>) -> Self {
        SharingWarning {
            code: code.to_string(),
            text: text.into(),
        }
    }
}

/// Who decides what slskd shares. Octo: the dashboard switch. Outside: slskd.yml or the
/// slskd container's environment sets shares Octo did not write, and Octo leaves them alone.
/// Locked: slskd does not allow remote configuration. Unknown: slskd has not answered yet.
/// Written as its name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum ShareControl {
    #[default]
    Unknown,
    Octo,
    Outside,
    Locked,
}

/// The Share my library switch: what it is set to, who decides slskd's shares, and why the
/// switch could not do its job the last time it tried, if it could not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareSwitchState {
    pub on: bool,
    pub control: ShareControl,
    pub problem: Option<String>,
}

/// The Sharing card on the Soulseek page: whether this server gives back to the network, built
/// from slskd's own answers. Counts are None when slskd did not say.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharingReport {
    pub reachable: bool,
    pub login: String,
    pub folders: Vec<SharedFolder>,
    pub directories: Option<i32>,
    pub files: Option<i32>,
    pub scanning: bool,
    pub scan_progress: Option<f64>,
    pub scan_failed: bool,
    pub network_files: Option<i32>,
    pub network_directories: Option<i32>,
    pub upload_slots: Option<i32>,
    #[serde(rename = "uploadSpeedLimitKiB")]
    pub upload_speed_limit_kib: Option<i32>,
    pub rescan_minutes: Option<i32>,
    pub uploads: UploadActivity,
    pub listen_port: Option<i32>,
    pub port: PortCheckResult,
    pub switch: ShareSwitchState,
    pub warnings: Vec<SharingWarning>,
}

/// Puts slskd's answers together. Any of them may be None, which reads as "slskd did not
/// say", never as zero. No application answer at all means slskd is out of reach.
#[allow(clippy::too_many_arguments)] // one argument per slskd answer
pub fn build(
    app_json: Option<&str>,
    options_json: Option<&str>,
    shares_json: Option<&str>,
    uploads_json: Option<&str>,
    port: PortCheckResult,
    share_switch: ShareSwitchState,
    default_login: bool,
    now: DateTime<Utc>,
) -> SharingReport {
    let Some(app_json) = app_json else {
        return SharingReport {
            reachable: false,
            login: "Unknown".to_string(),
            folders: Vec::new(),
            directories: None,
            files: None,
            scanning: false,
            scan_progress: None,
            scan_failed: false,
            network_files: None,
            network_directories: None,
            upload_slots: None,
            upload_speed_limit_kib: None,
            rescan_minutes: None,
            uploads: UploadActivity::default(),
            listen_port: None,
            port,
            switch: share_switch,
            warnings: vec![SharingWarning::new(
                "unreachable",
                "Octo cannot reach slskd, so it cannot tell what you share.",
            )],
        };
    };

    let login = parse_server_reading(app_json).link;
    let (mut directories, mut files, mut network_files, mut network_directories) = (None, None, None, None);
    let (mut scanning, mut faulted) = (false, false);
    let mut progress = None;
    if let Some(app) = try_parse(Some(app_json)) {
        if let Some(summary) = child(&app, "shares") {
            directories = int(summary, "directories");
            files = int(summary, "files");
            scanning =
                boolean(summary, "scanning") == Some(true) || boolean(summary, "scanPending") == Some(true);
            faulted = boolean(summary, "faulted") == Some(true);
            progress = double(summary, "scanProgress");
        }
        if let Some(stats) = child(&app, "user").and_then(|user| child(user, "statistics")) {
            network_files = int(stats, "fileCount");
            network_directories = int(stats, "directoryCount");
        }
    }

    let folders = parse_folders(shares_json, options_json);
    let (slots, speed, rescan) = parse_upload_options(options_json);
    let uploads = match uploads_json {
        None => UploadActivity::default(),
        Some(json) => parse_uploads(json, now),
    };
    let listen_port = parse_listen_port(options_json);

    let mut warnings = Vec::new();
    if default_login {
        warnings.push(SharingWarning::new(
            "defaultLogin",
            "slskd still has its default sign-in, slskd and slskd, so anyone on your network can open it. Set SLSKD_USERNAME and SLSKD_PASSWORD in .env, then the same under Connection below.",
        ));
    }
    if login == SoulseekLinkState::NotLoggedIn {
        warnings.push(SharingWarning::new(
            "signedOut",
            "slskd is not signed in to Soulseek right now, so nobody can download from you until it is.",
        ));
    }
    let shared: Vec<&SharedFolder> = folders.iter().filter(|folder| !folder.excluded).collect();
    if let Some(problem) = &share_switch.problem {
        warnings.push(SharingWarning::new("switch", problem.clone()));
    }
    // Sharing off by choice is a choice: the card says so plainly, and it is not a warning.
    let off_by_choice =
        !share_switch.on && matches!(share_switch.control, ShareControl::Octo | ShareControl::Unknown);
    if shared.is_empty() && options_json.is_some() && !off_by_choice && share_switch.problem.is_none() {
        warnings.push(SharingWarning::new(
            "nothingShared",
            "You share nothing. Many people on Soulseek will not send files to someone who shares nothing, so your downloads fail more often.",
        ));
    } else if !shared.is_empty() && files == Some(0) && !scanning && !faulted {
        let paths: Vec<&str> = shared.iter().map(|folder| folder.path.as_str()).collect();
        warnings.push(SharingWarning::new(
            "emptyShares",
            format!(
                "slskd shares {} but found no files there. Check that the folder is mounted into the slskd container, then rescan.",
                paths.join(", ")
            ),
        ));
    }
    if faulted {
        warnings.push(SharingWarning::new(
            "scanFailed",
            "slskd's last look through your shared folders failed. Rescan, and read slskd's log if it fails again.",
        ));
    }
    if port.state == PortState::Closed {
        let number = port.port.map(|p| p.to_string()).unwrap_or_default();
        warnings.push(SharingWarning::new(
            "portClosed",
            if shared.is_empty() {
                format!(
                    "Other people cannot connect to slskd on port {number}, so some of your downloads fail. Forward TCP port {number} on your router to this server."
                )
            } else {
                format!(
                    "Other people cannot connect to slskd on port {number}. Forward TCP port {number} on your router to this server. Until then, only people whose own port is open can download from you, and some of your downloads fail too."
                )
            },
        ));
    }

    SharingReport {
        reachable: true,
        login: login.name().to_string(),
        folders,
        directories,
        files,
        scanning,
        scan_progress: progress,
        scan_failed: faulted,
        network_files,
        network_directories,
        upload_slots: slots,
        upload_speed_limit_kib: speed,
        rescan_minutes: rescan,
        uploads,
        listen_port,
        port,
        switch: share_switch,
        warnings,
    }
}

/// The shared folders from /shares, which carries each one's counts; from the options when
/// slskd gave no shares answer. A raw entry is "[Alias]/path", or "!/path" for a folder kept
/// out of a share.
pub fn parse_folders(shares_json: Option<&str>, options_json: Option<&str>) -> Vec<SharedFolder> {
    let mut folders = Vec::new();
    if let Some(Value::Object(hosts)) = try_parse(shares_json) {
        for (_, host) in hosts {
            let Value::Array(shares) = host else { continue };
            for share in &shares {
                folders.push(SharedFolder {
                    alias: text(share, "alias").unwrap_or_default(),
                    path: text(share, "localPath")
                        .or_else(|| text(share, "raw"))
                        .unwrap_or_default(),
                    excluded: boolean(share, "isExcluded") == Some(true),
                    directories: int(share, "directories"),
                    files: int(share, "files"),
                });
            }
        }
        return folders;
    }

    if let Some(options) = try_parse(options_json)
        && let Some(Value::Array(raw)) =
            child(&options, "shares").and_then(|shares| child(shares, "directories"))
    {
        for entry in raw {
            if let Value::String(entry) = entry
                && let Some(folder) = folder_from_raw(entry)
            {
                folders.push(folder);
            }
        }
    }
    folders
}

static ALIAS_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(?<alias>[^\]]+)\](?<path>.+)$").expect("a valid pattern"));

/// One raw share entry as a folder, or None for a blank one.
pub fn folder_from_raw(raw: &str) -> Option<SharedFolder> {
    let mut raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let excluded = raw.starts_with(['!', '-']);
    if excluded {
        raw = &raw[1..];
    }
    let (alias, path) = match ALIAS_PREFIX.captures(raw) {
        Some(found) => (found["alias"].to_string(), found["path"].to_string()),
        None => {
            let last = raw
                .trim_end_matches(['/', '\\'])
                .split(['/', '\\'])
                .next_back()
                .unwrap_or(raw);
            (last.to_string(), raw.to_string())
        }
    };
    Some(SharedFolder {
        alias,
        path,
        excluded,
        directories: None,
        files: None,
    })
}

/// Upload slots, the total upload speed limit in KiB/s, and the rescan interval in minutes.
/// slskd's "no limit" (`int.MaxValue`) reads as None.
pub fn parse_upload_options(options_json: Option<&str>) -> (Option<i32>, Option<i32>, Option<i32>) {
    let Some(options) = try_parse(options_json) else {
        return (None, None, None);
    };
    let (mut slots, mut speed, mut rescan) = (None, None, None);
    if let Some(upload) = child(&options, "transfers").and_then(|transfers| child(transfers, "upload")) {
        slots = int(upload, "slots");
        speed = int(upload, "speedLimit").filter(|limit| *limit < i32::MAX);
    }
    if let Some(cache) = child(&options, "shares").and_then(|shares| child(shares, "cache")) {
        rescan = int(cache, "retention");
    }
    (slots, speed, rescan)
}

/// slskd's Soulseek listening port, the one other people connect to.
pub fn parse_listen_port(options_json: Option<&str>) -> Option<i32> {
    let options = try_parse(options_json)?;
    int(child(&options, "soulseek")?, "listenPort")
}

/// What people downloaded from you, from slskd's uploads list (user, then folder, then file).
/// A state is slskd's flags in words, such as "InProgress", "Queued, Locally" or
/// "Completed, Succeeded". Finished uploads count when they ended in the last seven days.
pub fn parse_uploads(uploads_json: &str, now: DateTime<Utc>) -> UploadActivity {
    let Some(Value::Array(users)) = try_parse(Some(uploads_json)) else {
        return UploadActivity::default();
    };

    let since = now - TimeDelta::days(7);
    let mut activity = UploadActivity::default();
    let mut people: HashSet<String> = HashSet::new();
    for user in &users {
        let name = text(user, "username").unwrap_or_default();
        let Some(Value::Array(dirs)) = child(user, "directories") else {
            continue;
        };
        for dir in dirs {
            let Some(Value::Array(list)) = child(dir, "files") else {
                continue;
            };
            for file in list {
                let state = text(file, "state").unwrap_or_default();
                if contains_ignore_case(&state, "InProgress") || contains_ignore_case(&state, "Initializing")
                {
                    activity.sending += 1;
                    continue;
                }
                if contains_ignore_case(&state, "Queued") || contains_ignore_case(&state, "Requested") {
                    activity.waiting += 1;
                    continue;
                }
                if !contains_ignore_case(&state, "Completed") {
                    continue;
                }
                let Some(ended) = date(file, "endedAt").filter(|ended| *ended >= since) else {
                    continue;
                };
                if contains_ignore_case(&state, "Succeeded") {
                    activity.files += 1;
                    activity.bytes += long(file, "size").unwrap_or(0);
                    people.insert(dotnet::ordinal_ignore_case_key(&name));
                    if activity.last_upload_at.is_none_or(|last| ended > last) {
                        activity.last_upload_at = Some(ended);
                    }
                } else if ["Errored", "TimedOut", "Rejected", "Aborted"]
                    .iter()
                    .any(|flag| contains_ignore_case(&state, flag))
                {
                    activity.failed += 1;
                }
            }
        }
    }
    activity.people = i32::try_from(people.len()).unwrap_or(i32::MAX);
    activity
}

/// The answer read as JSON, or None for a blank answer or one that does not parse.
fn try_parse(json: Option<&str>) -> Option<Value> {
    let json = json.filter(|json| !is_blank(json))?;
    serde_json::from_str(json).ok()
}

/// A property by name ignoring case; None for an element that is not an object.
fn child<'a>(element: &'a Value, name: &str) -> Option<&'a Value> {
    let Value::Object(map) = element else { return None };
    map.iter()
        .find(|(property, _)| dotnet::eq_ignore_case(property, name))
        .map(|(_, value)| value)
}

fn text(element: &Value, name: &str) -> Option<String> {
    match child(element, name) {
        Some(Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

/// The property as an `i32`: an integer literal that fits, else None.
fn int(element: &Value, name: &str) -> Option<i32> {
    match child(element, name) {
        Some(Value::Number(n)) => n.as_i64().and_then(|n| i32::try_from(n).ok()),
        _ => None,
    }
}

fn long(element: &Value, name: &str) -> Option<i64> {
    match child(element, name) {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

fn double(element: &Value, name: &str) -> Option<f64> {
    match child(element, name) {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

fn boolean(element: &Value, name: &str) -> Option<bool> {
    match child(element, name) {
        Some(Value::Bool(value)) => Some(*value),
        _ => None,
    }
}

/// An ISO 8601 time, as slskd writes it.
fn date(element: &Value, name: &str) -> Option<DateTime<Utc>> {
    datetime::parse_utc(&text(element, name)?)
}

/// Soulseek's port test: the address it is asked at, with the port in place of `{0}`.
pub const PORT_TEST_URL: &str = "http://tools.slsknet.org/porttest.php?port={0}";

static PORT_ANSWER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Port:\s*(?<port>\d+)/tcp\s+(?<state>OPEN|CLOSED)").expect("a valid pattern")
});

/// The test's page says "Port: 50300/tcp OPEN" or "... CLOSED". Anything else, or an
/// answer about another port, is Unknown.
pub fn parse_port_answer(page: &str, port: i32) -> PortState {
    let Some(found) = PORT_ANSWER.captures(page) else {
        return PortState::Unknown;
    };
    if found["port"] != port.to_string() {
        return PortState::Unknown;
    }
    if found["state"].eq_ignore_ascii_case("OPEN") {
        PortState::Open
    } else {
        PortState::Closed
    }
}

#[cfg(test)]
#[path = "soulseek_sharing_tests.rs"]
mod tests;
