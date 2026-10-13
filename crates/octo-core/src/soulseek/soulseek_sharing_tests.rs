//! Soulseek sharing tests: the parts that build the card from slskd's answers. The JSON
//! below has the shape slskd 0.26.0 gave on the live box on 2026-10-04.

use chrono::{DateTime, TimeDelta, TimeZone, Utc};

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 4, 21, 0, 0).unwrap()
}

fn port_open() -> PortCheckResult {
    PortCheckResult::new(PortState::Open, Some(50300), Some(now()), None)
}

fn on() -> ShareSwitchState {
    ShareSwitchState {
        on: true,
        control: ShareControl::Octo,
        problem: None,
    }
}

fn off() -> ShareSwitchState {
    ShareSwitchState {
        on: false,
        control: ShareControl::Octo,
        problem: None,
    }
}

// Logged in, sharing nothing, as LXC 111 was found.
const APP_SHARING_NOTHING: &str = r#"{
  "server": { "state": "Connected, LoggedIn", "isConnected": true, "isLoggedIn": true },
  "user": { "username": "someone", "statistics": { "averageSpeed": 0, "directoryCount": 0, "fileCount": 0, "uploadCount": 0 } },
  "shares": { "scanPending": false, "scanning": false, "ready": true, "faulted": false, "scanProgress": 1, "directories": 0, "files": 0 }
}"#;

const OPTIONS_SHARING_NOTHING: &str = r#"{
  "shares": { "directories": [], "filters": [], "cache": { "storageMode": "memory", "workers": 4 } },
  "transfers": { "upload": { "slots": 10, "speedLimit": 2147483647 } },
  "soulseek": { "listenPort": 50300 }
}"#;

const APP_SHARING: &str = r#"{
  "server": { "state": "Connected, LoggedIn", "isConnected": true, "isLoggedIn": true },
  "user": { "username": "someone", "statistics": { "directoryCount": 13, "fileCount": 2438 } },
  "shares": { "scanning": false, "scanPending": false, "faulted": false, "scanProgress": 1, "directories": 13, "files": 2438 }
}"#;

const OPTIONS_SHARING: &str = r#"{
  "shares": { "directories": ["[Music]/share"], "cache": { "retention": 1440 } },
  "transfers": { "upload": { "slots": 4, "speedLimit": 2048 } },
  "soulseek": { "listenPort": 50300 }
}"#;

const SHARES_SHARING: &str = r#"{ "local": [ { "id": "a", "alias": "Music", "isExcluded": false, "localPath": "/share", "raw": "[Music]/share", "remotePath": "Music", "directories": 13, "files": 2438 } ] }"#;

fn codes(report: &SharingReport) -> Vec<&str> {
    report.warnings.iter().map(|w| w.code.as_str()).collect()
}

#[test]
fn sharing_nothing_says_so() {
    let report = build(
        Some(APP_SHARING_NOTHING),
        Some(OPTIONS_SHARING_NOTHING),
        Some(r#"{ "local": [] }"#),
        Some("[]"),
        port_open(),
        on(),
        false,
        now(),
    );

    assert!(report.reachable);
    assert_eq!(report.login, "LoggedIn");
    assert!(report.folders.is_empty());
    assert_eq!(report.files, Some(0));
    assert_eq!(report.upload_slots, Some(10));
    assert_eq!(report.upload_speed_limit_kib, None);
    assert_eq!(report.listen_port, Some(50300));
    assert_eq!(codes(&report), ["nothingShared"]);
}

/// Sharing switched off is a choice: no "you share nothing" warning, and the card carries the
/// switch as off.
#[test]
fn sharing_off_by_choice_is_not_a_warning() {
    let report = build(
        Some(APP_SHARING_NOTHING),
        Some(OPTIONS_SHARING_NOTHING),
        Some(r#"{ "local": [] }"#),
        Some("[]"),
        port_open(),
        off(),
        false,
        now(),
    );

    assert!(!report.switch.on);
    assert!(report.folders.is_empty());
    assert!(report.warnings.is_empty());
}

/// A switch slskd would not obey is said in its own words, in place of the generic "you share
/// nothing".
#[test]
fn a_switch_problem_is_the_warning() {
    let stuck = ShareSwitchState {
        on: true,
        control: ShareControl::Locked,
        problem: Some("slskd does not let Octo change its shares.".into()),
    };

    let report = build(
        Some(APP_SHARING_NOTHING),
        Some(OPTIONS_SHARING_NOTHING),
        Some(r#"{ "local": [] }"#),
        Some("[]"),
        port_open(),
        stuck,
        false,
        now(),
    );

    assert_eq!(codes(&report), ["switch"]);
}

#[test]
fn a_shared_library_is_reported_with_its_limits_and_no_warning() {
    let report = build(
        Some(APP_SHARING),
        Some(OPTIONS_SHARING),
        Some(SHARES_SHARING),
        Some("[]"),
        port_open(),
        on(),
        false,
        now(),
    );

    assert_eq!(
        report.folders,
        [SharedFolder {
            alias: "Music".into(),
            path: "/share".into(),
            excluded: false,
            directories: Some(13),
            files: Some(2438),
        }]
    );
    assert_eq!(report.files, Some(2438));
    assert_eq!(report.network_files, Some(2438));
    assert_eq!(report.upload_slots, Some(4));
    assert_eq!(report.upload_speed_limit_kib, Some(2048));
    assert_eq!(report.rescan_minutes, Some(1440));
    assert!(report.warnings.is_empty());
}

#[test]
fn a_closed_port_and_the_default_login_are_warned_about() {
    let closed = PortCheckResult::new(PortState::Closed, Some(50300), Some(now()), None);

    let report = build(
        Some(APP_SHARING),
        Some(OPTIONS_SHARING),
        Some(SHARES_SHARING),
        Some("[]"),
        closed,
        on(),
        true,
        now(),
    );

    assert_eq!(codes(&report), ["defaultLogin", "portClosed"]);
    assert!(report.warnings[1].text.contains("50300"));
    assert!(!report.warnings.iter().any(|w| w.text.contains('\u{2014}')));
}

#[test]
fn signed_out_failed_scan_and_a_share_with_no_files_are_each_warned_about() {
    let card = |app: &str| {
        build(
            Some(app),
            Some(OPTIONS_SHARING),
            Some(SHARES_SHARING),
            Some("[]"),
            port_open(),
            on(),
            false,
            now(),
        )
    };
    let signed_out = APP_SHARING.replace(r#""isLoggedIn": true"#, r#""isLoggedIn": false"#);
    assert!(codes(&card(&signed_out)).contains(&"signedOut"));

    let failed = APP_SHARING.replace(r#""faulted": false"#, r#""faulted": true"#);
    let failed_report = card(&failed);
    assert!(failed_report.scan_failed);
    assert_eq!(codes(&failed_report), ["scanFailed"]);

    let empty = APP_SHARING.replace(r#""files": 2438"#, r#""files": 0"#);
    assert_eq!(codes(&card(&empty)), ["emptyShares"]);

    // An empty share being scanned is not empty yet.
    let scanning = empty.replace(r#""scanning": false"#, r#""scanning": true"#);
    assert!(card(&scanning).warnings.is_empty());
}

#[test]
fn unreachable_is_one_warning_and_no_zero_counts() {
    let report = build(None, None, None, None, port_open(), on(), false, now());

    assert!(!report.reachable);
    assert_eq!(report.files, None);
    assert_eq!(codes(&report), ["unreachable"]);
}

/// Without a shares answer the folders still come from the options, alias and exclusions
/// included.
#[test]
fn folders_fall_back_to_the_options() {
    let options =
        r#"{ "shares": { "directories": ["[Music]/share", "!/share/Private", "/data/More Music/"] } }"#;

    let folders = parse_folders(None, Some(options));

    let folder = |alias: &str, path: &str, excluded| SharedFolder {
        alias: alias.into(),
        path: path.into(),
        excluded,
        directories: None,
        files: None,
    };
    assert_eq!(
        folders,
        [
            folder("Music", "/share", false),
            folder("Private", "/share/Private", true),
            folder("More Music", "/data/More Music/", false),
        ]
    );
}

#[test]
fn uploads_count_now_and_the_last_seven_days() {
    let at = |delta: TimeDelta| (now() + delta).to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
    let uploads = format!(
        r#"[
  {{ "username": "alice", "directories": [ {{ "directory": "Music", "files": [
    {{ "state": "InProgress", "size": 30000000 }},
    {{ "state": "Queued, Locally", "size": 1 }},
    {{ "state": "Completed, Succeeded", "size": 25000000, "endedAt": "{}" }},
    {{ "state": "Completed, Succeeded", "size": 99, "endedAt": "{}" }}
  ] }} ] }},
  {{ "username": "bob", "directories": [ {{ "directory": "Music", "files": [
    {{ "state": "Completed, Succeeded", "size": 5000000, "endedAt": "{}" }},
    {{ "state": "Completed, Errored", "size": 1, "endedAt": "{}" }},
    {{ "state": "Completed, Cancelled", "size": 1, "endedAt": "{}" }}
  ] }} ] }}
]"#,
        at(TimeDelta::days(-1)),
        at(TimeDelta::days(-9)),
        at(TimeDelta::hours(-2)),
        at(TimeDelta::hours(-3)),
        at(TimeDelta::hours(-3)),
    );

    let activity = parse_uploads(&uploads, now());

    assert_eq!(activity.sending, 1);
    assert_eq!(activity.waiting, 1);
    assert_eq!(activity.files, 2);
    assert_eq!(activity.bytes, 30_000_000);
    assert_eq!(activity.people, 2);
    assert_eq!(activity.failed, 1);
    assert_eq!(activity.last_upload_at, Some(now() - TimeDelta::hours(2)));
}

#[test]
fn port_test_answer_is_read() {
    let cases = [
        (
            "IP: 203.0.113.9 Port: 50300/tcp OPEN. Congratulations",
            PortState::Open,
        ),
        (
            "IP: 203.0.113.9 Port: 50300/tcp CLOSED. Your router",
            PortState::Closed,
        ),
        ("IP: 203.0.113.9 Port: 2234/tcp OPEN.", PortState::Unknown),
        ("<html>maintenance</html>", PortState::Unknown),
    ];
    for (page, expected) in cases {
        assert_eq!(parse_port_answer(page, 50300), expected, "{page}");
    }
}

/// The card as the dashboard reads it: camelCase, the port's and the switch's states by name,
/// "no limit" as null.
#[test]
fn the_card_is_written_as_the_dashboard_reads_it() {
    let report = build(
        Some(APP_SHARING_NOTHING),
        Some(OPTIONS_SHARING_NOTHING),
        Some(r#"{ "local": [] }"#),
        Some("[]"),
        port_open(),
        off(),
        false,
        now(),
    );
    let json = crate::json::to_string(&report);
    assert_eq!(
        json,
        r#"{"reachable":true,"login":"LoggedIn","folders":[],"directories":0,"files":0,"scanning":false,"scanProgress":1,"scanFailed":false,"networkFiles":0,"networkDirectories":0,"uploadSlots":10,"uploadSpeedLimitKiB":null,"rescanMinutes":null,"uploads":{"sending":0,"waiting":0,"files":0,"bytes":0,"people":0,"failed":0,"lastUploadAt":null},"listenPort":50300,"port":{"state":"Open","port":50300,"checkedAt":"2026-10-04T21:00:00Z","error":null},"switch":{"on":false,"control":"Octo","problem":null},"warnings":[]}"#
    );
}
