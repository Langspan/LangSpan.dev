//! State snapshots: a replayed `State` compared with a committed JSON file.
//!
//! The snapshot holds the serialized state and every field the state leaves
//! out of its replies (`#[serde(skip)]`), so a change to any part of what
//! `apply` derives shows. To change a snapshot on purpose, run the test with
//! `LANGSPAN_UPDATE_GOLDEN=1`, which rewrites it from the replay, then read
//! `git diff` and say in the pull request why the state changed: a refresh
//! nobody explains hides the regression this test exists to catch.

use langspan_core::{SnapshotRecord, State};
use serde_json::{Value, json};
use std::path::Path;

/// The environment variable that rewrites snapshots instead of comparing.
pub const UPDATE: &str = "LANGSPAN_UPDATE_GOLDEN";

/// Lines of difference a failure shows, at most.
const SHOWN: usize = 40;

/// `state` as pretty JSON with its derived fields, ending in a newline.
pub fn state_json(state: &State) -> String {
    // As the store writes it: a derived field left empty is left out, as the
    // state's own empty fields are, so a snapshot lists only what the
    // replay holds.
    let value = serde_json::to_value(SnapshotRecord::of(state)).expect("a state serializes");
    serde_json::to_string_pretty(&sorted(value)).expect("a state serializes") + "\n"
}

/// `value` with every object's keys in sorted order. The workspace build
/// enables serde_json's `preserve_order` (the root crate needs it) and
/// `langspan-core` alone does not, so without this the same replay prints
/// struct fields in declaration order in one build and sorted in the other.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(fields) => {
            let mut fields: Vec<(String, Value)> = fields.into_iter().collect();
            fields.sort_by(|(a, _), (b, _)| a.cmp(b));
            Value::Object(fields.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// Compare `actual` with the file at `path`, or rewrite the file when
/// `LANGSPAN_UPDATE_GOLDEN` is set. A mismatch panics with the lines that
/// differ and how to refresh.
pub fn check(path: &Path, actual: &str) {
    if let Some(difference) = differs(path, actual) {
        panic!("{difference}");
    }
}

/// As `check`, returning the mismatch instead of panicking.
pub fn differs(path: &Path, actual: &str) -> Option<String> {
    // Named from the crate root, so no local path reaches the output.
    let relative = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .expect("a snapshot is in its crate");
    let name = relative.display();
    if std::env::var_os(UPDATE).is_some() {
        std::fs::write(path, actual).unwrap_or_else(|e| panic!("cannot write {name}: {e}"));
        return None;
    }
    // A missing file compares as empty, so a new snapshot fails until it is
    // written on purpose.
    let expected = std::fs::read_to_string(path).unwrap_or_default();
    if expected == actual {
        return None;
    }
    // The whole replay, to diff with the committed file or copy over it.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|dir| dir.join("Cargo.lock").is_file())
        .expect("the workspace root holds Cargo.lock");
    let written = root.join("target/golden-actual").join(relative);
    let saved = std::fs::create_dir_all(written.parent().expect("a parent"))
        .and_then(|()| std::fs::write(&written, actual))
        .map_or_else(
            |e| format!("could not be written to target/golden-actual: {e}"),
            |()| format!("is in target/golden-actual/{name}"),
        );
    Some(format!(
        "{name} differs from the replay (`-` committed, `+` now):\n{}\nThe whole replay {saved}. If the change is intended, run this test with {UPDATE}=1, review `git diff`, and explain the change in the pull request.",
        difference(&expected, actual)
    ))
}

/// The lines between the longest common start and end of the two texts.
fn difference(expected: &str, actual: &str) -> String {
    let old: Vec<&str> = expected.lines().collect();
    let new: Vec<&str> = actual.lines().collect();
    let start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let end = old[start..]
        .iter()
        .rev()
        .zip(new[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut shown = vec![format!("@@ line {} @@", start + 1)];
    shown.extend(old[start..old.len() - end].iter().map(|l| format!("-{l}")));
    shown.extend(new[start..new.len() - end].iter().map(|l| format!("+{l}")));
    if shown.len() > SHOWN + 1 {
        let more = shown.len() - SHOWN - 1;
        shown.truncate(SHOWN + 1);
        shown.push(format!("... {more} more lines"));
    }
    shown.join("\n")
}

#[test]
fn a_snapshot_sorts_keys_whatever_the_serde_json_features() {
    // `preserve_order` keeps this insertion order; `sorted` must not.
    let mut inner = serde_json::Map::new();
    inner.insert("b".into(), json!(1));
    inner.insert("a".into(), json!([{"d": 1, "c": 2}]));
    let mut outer = serde_json::Map::new();
    outer.insert("z".into(), Value::Object(inner));
    outer.insert("y".into(), json!(0));
    assert_eq!(
        serde_json::to_string(&sorted(Value::Object(outer))).expect("a value serializes"),
        r#"{"y":0,"z":{"a":[{"c":2,"d":1}],"b":1}}"#
    );
}
