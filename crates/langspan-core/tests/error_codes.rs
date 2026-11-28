//! The wire error codes are a committed list: removing, renaming or adding
//! one changes `tests/golden/error_codes.txt` and the usage guide's table.
//! In-process: reads two committed files, starts nothing.

use langspan_core::ErrorCode;

/// Temporary refusals the core decides for a stored command, so the store
/// keeps them as the request's receipt (`tests/store.rs` checks the replay
/// for `workspace_busy`).
const RECORDED: &[&str] = &["session_live", "workspace_busy", "workspace_in_use"];

fn committed() -> Vec<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/error_codes.txt");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn error_codes_match_the_committed_list() {
    let mut names = Vec::new();
    for code in ErrorCode::ALL {
        let serialized = serde_json::to_value(code).unwrap();
        assert_eq!(serialized, code.as_str(), "{code:?}");
        assert_eq!(String::from(*code), code.as_str());
        assert_eq!(code.to_string(), code.as_str());
        let parsed: ErrorCode = serde_json::from_value(serialized).unwrap();
        assert_eq!(parsed, *code);
        names.push(code.as_str().to_owned());
    }
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(names, sorted, "ErrorCode::ALL is sorted with no duplicates");
    assert_eq!(names, committed());
}

#[test]
fn usage_guide_lists_every_code_and_how_to_retry_it() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/usage.md");
    let guide = std::fs::read_to_string(path).unwrap();
    let (_, section) = guide.split_once("\n## Error codes\n").unwrap();
    let section = section.split("\n## ").next().unwrap();
    let mut listed = Vec::new();
    for row in section.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        let code = cells[1].trim_matches('`');
        // A refusal the core decides is a receipt, replayed for its request
        // ID, so retrying it needs a new ID (or a change); only a code
        // written where nothing is recorded retries with the same ID.
        assert!(
            ["Same ID", "New ID", "No"].contains(&cells[2]),
            "{code}: retry is Same ID, New ID or No"
        );
        assert!(
            !(cells[2] == "Same ID" && RECORDED.contains(&code)),
            "{code}: a recorded refusal replays for the same request ID"
        );
        assert!(!cells[3].is_empty(), "{code}: says what it means");
        listed.push(code.to_owned());
    }
    assert_eq!(listed, committed());
}
