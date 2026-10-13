//! The pure half of the Soulseek share switch: Octo's marked block in slskd.yml and the readers
//! of slskd's answers the switch decides by. The switch itself, which asks slskd, is
//! `octo::services::soulseek::soulseek_share_switch`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::common::dotnet::{self, is_blank};

/// The first line of the part of slskd.yml Octo writes.
pub const BLOCK_START: &str =
    "# >>> Octo: sharing. Octo's dashboard writes this part (Soulseek page, Sharing); change it there.";
/// Its last line.
pub const BLOCK_END: &str = "# <<< Octo: sharing";

/// slskd's share summary: busy while a look runs or is finishing (ready is false from the start
/// of a look until its index is saved and reloaded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanState {
    pub busy: bool,
    pub files: Option<i32>,
}

/// The share summary in slskd's application answer, or None when it did not say.
pub fn read_scan_state(application_json: Option<&str>) -> Option<ScanState> {
    let root = parse(application_json)?;
    let shares = try_get(&root, "shares")?;
    let flag = |name: &str| matches!(try_get(shares, name), Some(Value::Bool(true)));
    let ready = !matches!(try_get(shares, "ready"), Some(Value::Bool(false)));
    // A number that is not a whole `i32` reads as unknown.
    let files = match try_get(shares, "files") {
        Some(Value::Number(n)) => n.as_i64().and_then(|n| i32::try_from(n).ok()),
        _ => None,
    };
    Some(ScanState {
        busy: flag("scanning") || !ready,
        files,
    })
}

/// The uploads still waiting or under way, by user and id: everything not Completed.
pub fn open_uploads(uploads_json: Option<&str>) -> Vec<(String, String)> {
    let mut open = Vec::new();
    let Some(Value::Array(users)) = parse(uploads_json) else {
        return open;
    };
    for user in &users {
        let Some(Value::String(name)) = exact(user, "username") else {
            continue;
        };
        let Some(Value::Array(dirs)) = exact(user, "directories") else {
            continue;
        };
        for dir in dirs {
            let Some(Value::Array(files)) = exact(dir, "files") else {
                continue;
            };
            for file in files {
                let state = match exact(file, "state") {
                    Some(Value::String(state)) => state.as_str(),
                    _ => "",
                };
                if let Some(Value::String(id)) = exact(file, "id")
                    && !crate::soulseek::soulseek_client::contains_ignore_case(state, "Completed")
                {
                    open.push((name.clone(), id.clone()));
                }
            }
        }
    }
    open
}

/// slskd's share list from its options, or None when it did not say.
pub fn parse_directories(options_json: Option<&str>) -> Option<Vec<String>> {
    let root = parse(options_json)?;
    let shares = try_get(&root, "shares")?;
    Some(match try_get(shares, "directories") {
        Some(Value::Array(dirs)) => dirs
            .iter()
            .filter_map(|dir| dir.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    })
}

/// The file without Octo's block, and the block, or None when there is none.
pub fn split_block(yaml: &str) -> (String, Option<String>) {
    let text = yaml.replace("\r\n", "\n");
    let Some(start) = text.find(BLOCK_START) else {
        return (text, None);
    };
    let Some(found) = text[start..].find(BLOCK_END) else {
        return (text, None);
    };
    let mut end = start + found + BLOCK_END.len();
    if text[end..].starts_with('\n') {
        end += 1;
    }
    let block = text[start..end].to_string();
    (format!("{}{}", &text[..start], &text[end..]), Some(block))
}

static OWN_SHARES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^shares\s*:").expect("a valid pattern"));

/// An uncommented top-level "shares:" outside Octo's block: the person's own.
pub fn has_own_shares(rest: &str) -> bool {
    OWN_SHARES.is_match(rest)
}

/// The file with Octo's block at the end, sharing these folders, or nothing.
pub fn with_block(rest: &str, folders: &[String]) -> String {
    let mut block = String::new();
    block.push_str(BLOCK_START);
    block.push('\n');
    block.push_str("shares:\n");
    if folders.is_empty() {
        block.push_str("  directories: []\n");
    } else {
        block.push_str("  directories:\n");
        for folder in folders {
            block.push_str("    - '");
            block.push_str(&folder.replace('\'', "''"));
            block.push_str("'\n");
        }
    }
    block.push_str(BLOCK_END);
    block.push('\n');
    let head = rest.trim_end_matches(['\n', ' ']);
    if head.is_empty() {
        block
    } else {
        format!("{head}\n\n{block}")
    }
}

/// The file as two versions of it are compared: line ends and the trailing space ignored.
pub fn normalize(yaml: &str) -> String {
    yaml.replace("\r\n", "\n")
        .trim_end_matches(['\n', ' '])
        .to_string()
}

fn parse(json: Option<&str>) -> Option<Value> {
    let json = json.filter(|json| !is_blank(json))?;
    serde_json::from_str(json).ok()
}

/// A property by name ignoring case; None for an element that is not an object.
fn try_get<'a>(element: &'a Value, name: &str) -> Option<&'a Value> {
    let Value::Object(map) = element else { return None };
    map.iter()
        .find(|(property, _)| dotnet::eq_ignore_case(property, name))
        .map(|(_, value)| value)
}

/// A property by its exact name; None for an element that is not an object.
fn exact<'a>(element: &'a Value, name: &str) -> Option<&'a Value> {
    element.as_object()?.get(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The start of the commented-out slskd.yml slskd writes on first run, with the Linux line
    // ends slskd writes.
    const STOCK_FILE: &str =
        "# debug: false\n# remote_configuration: false\n# shares:\n#   directories:\n#     - 'D:\\Music'";

    #[test]
    fn the_block_is_replaced_in_place_and_quotes_are_escaped() {
        let once = with_block(
            STOCK_FILE,
            &["[Music]/share".to_string(), "!/share/Bob's Memos".to_string()],
        );
        let (rest, block) = split_block(&once);
        assert_eq!(STOCK_FILE.trim_end(), rest.trim_end());
        assert!(block.expect("a block").contains("    - '!/share/Bob''s Memos'"));
        assert!(!has_own_shares(&rest));

        let twice = with_block(&rest, &[]);
        assert_eq!(twice.split(BLOCK_START).count() - 1, 1);
        assert!(twice.contains("  directories: []"));
    }

    #[test]
    fn a_block_with_crlf_line_ends_is_still_found() {
        let file = with_block("a: 1", &["x".to_string()]).replace('\n', "\r\n");
        let (rest, block) = split_block(&file);
        assert_eq!(rest, "a: 1\n\n");
        assert!(block.is_some());
        assert!(has_own_shares("x: 1\nshares :\n"));
        assert!(!has_own_shares("x: 1\n  shares:\n# shares:\n"));
        assert_eq!(normalize("a\r\nb \n\n"), "a\nb");
    }

    #[test]
    fn the_scan_state_reads_busy_from_scanning_or_not_ready() {
        let cases = [
            (
                r#"{"shares":{"scanning":false,"ready":true,"files":4}}"#,
                Some((false, Some(4))),
            ),
            (r#"{"Shares":{"Scanning":true,"files":4}}"#, Some((true, Some(4)))),
            (r#"{"shares":{"ready":false}}"#, Some((true, None))),
            (r#"{"shares":{}}"#, Some((false, None))),
            (r#"{"server":{}}"#, None),
            ("not json", None),
            ("", None),
        ];
        for (json, expected) in cases {
            assert_eq!(
                read_scan_state(Some(json)).map(|s| (s.busy, s.files)),
                expected,
                "{json}"
            );
        }
    }

    #[test]
    fn the_share_list_is_none_only_when_slskd_did_not_say() {
        assert_eq!(
            parse_directories(Some(r#"{"shares":{"directories":["a",1,"b"]}}"#)),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(parse_directories(Some(r#"{"shares":{}}"#)), Some(Vec::new()));
        assert_eq!(parse_directories(Some(r#"{"other":{}}"#)), None);
        assert_eq!(parse_directories(None), None);
    }

    #[test]
    fn open_uploads_are_everything_not_completed() {
        let uploads = r#"[ { "username": "alice", "directories": [ { "directory": "Music", "files": [
            { "id": "u1", "state": "InProgress" },
            { "id": "u2", "state": "Queued, Locally" },
            { "id": "u3", "state": "Completed, Succeeded" },
            { "state": "Queued" } ] } ] } ]"#;
        assert_eq!(
            open_uploads(Some(uploads)),
            [
                ("alice".to_string(), "u1".to_string()),
                ("alice".to_string(), "u2".to_string())
            ]
        );
        assert!(open_uploads(Some("{}")).is_empty());
    }
}
