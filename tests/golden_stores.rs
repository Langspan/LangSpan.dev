//! The golden stores (`tests/fixtures/store-<release>/`, ADR 0015) replayed
//! through the core: each store's events fold to the state committed beside
//! it, `<store>.state.json`. The events are applied as the store recovers
//! them, in sequence order, with the core's `apply`; opening a `Store`
//! would add a boot stamped with this run's clock. A change to the state is
//! accepted as tests/support/snapshot.rs says.

// Shared with the core's replay test, which uses `check`; this one compares
// every store before failing, with `differs`.
#[allow(dead_code)]
#[path = "../crates/langspan-core/tests/support/snapshot.rs"]
mod snapshot;

use langspan_core::{Event, State, apply};
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every golden store, in path order.
fn stores() -> Vec<PathBuf> {
    let mut stores = Vec::new();
    for release in fs::read_dir(fixtures()).expect("read tests/fixtures") {
        let release = release.expect("list tests/fixtures").path();
        if !release.is_dir() {
            continue;
        }
        for file in fs::read_dir(&release).expect("read a release's fixtures") {
            let file = file.expect("list a release's fixtures").path();
            if file.extension().is_some_and(|e| e == "sqlite3") {
                stores.push(file);
            }
        }
    }
    stores.sort();
    stores
}

/// `store`'s events folded into a state. The store is opened read-only and
/// never written: the fixture stays as its release left it.
fn replay(store: &Path) -> State {
    let name = store.strip_prefix(fixtures()).unwrap_or(store).display();
    let connection = Connection::open_with_flags(store, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|e| panic!("{name} does not open: {e}"));
    let mut statement = connection
        .prepare("SELECT seq, event_json FROM events ORDER BY seq")
        .unwrap_or_else(|e| panic!("{name} has no event log: {e}"));
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap_or_else(|e| panic!("{name}: the events cannot be read: {e}"));
    let mut state = State::default();
    for row in rows {
        let (seq, json) = row.unwrap_or_else(|e| panic!("{name}: an event cannot be read: {e}"));
        let event: Event = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{name}: event {seq} does not parse: {e}"));
        apply(&mut state, &event)
            .unwrap_or_else(|e| panic!("{name}: event {seq} does not apply: {e}"));
    }
    state
}

/// The released versions, from the changelog's headings
/// (`## [0.4.0](...) (date)`, or `## 0.1.0 (date)` for the first).
fn released() -> Vec<String> {
    let changelog = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("CHANGELOG.md"))
        .expect("read CHANGELOG.md");
    changelog
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .map(|heading| {
            let version = heading.trim_start_matches('[');
            version[..version.find([']', ' ']).unwrap_or(version.len())].to_string()
        })
        .collect()
}

/// The releases that need a store directory and have none. `current`, the
/// version a release PR bumps to, needs one only once `tagged` finds its
/// tag: the stores are built from the tag, so the release PR cannot hold
/// them (as for the frozen event-type lists, issue #518).
fn missing_stores<'a>(
    releases: &'a [String],
    current: &str,
    has_directory: impl Fn(&str) -> bool,
    tagged: impl Fn(&str) -> bool,
) -> Vec<&'a String> {
    releases
        .iter()
        .filter(|release| *release != current || tagged(release))
        .filter(|release| !has_directory(release))
        .collect()
}

/// Whether the repository has the tag `v<release>`. A failing `git` fails
/// the test: it never counts as untagged.
fn has_release_tag(release: &str) -> bool {
    let output = std::process::Command::new("git")
        .args([
            "--no-optional-locks",
            "tag",
            "--list",
            &format!("v{release}"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap_or_else(|e| panic!("cannot run git to list the tags: {e}; install git"));
    assert!(
        output.status.success(),
        "git tag --list v{release} failed: {}; run the test in a Git checkout",
        String::from_utf8_lossy(&output.stderr)
    );
    !output.stdout.trim_ascii().is_empty()
}

fn versions(list: &[&str]) -> Vec<String> {
    list.iter().map(|v| v.to_string()).collect()
}

/// A release PR names 0.9.0 before its tag exists: no store is asked of it,
/// while an older release without one is still missing.
#[test]
fn the_untagged_version_of_a_release_pr_needs_no_stores_yet() {
    let releases = versions(&["0.9.0", "0.8.0", "0.7.0"]);
    let missing = missing_stores(&releases, "0.9.0", |r| r == "0.8.0", |_| false);
    assert_eq!(missing, ["0.7.0"]);
}

/// Once 0.9.0 is tagged, its stores are missing until they land.
#[test]
fn a_tagged_release_without_stores_is_missing() {
    let releases = versions(&["0.9.0", "0.8.0"]);
    let missing = missing_stores(&releases, "0.9.0", |r| r == "0.8.0", |r| r == "0.9.0");
    assert_eq!(missing, ["0.9.0"]);
}

/// A release that changed nothing the store holds still gets its own
/// directory: the check is then mechanical, and a release cannot be missed.
/// The version the manifest names counts once its `v` tag exists; this test
/// runs `git` because what it checks is the repository's tags.
#[test]
fn every_release_has_a_golden_store_directory() {
    let releases = released();
    let manifest = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".release-please-manifest.json"),
    )
    .expect("read .release-please-manifest.json");
    let current: serde_json::Value = serde_json::from_str(&manifest).expect("the manifest parses");
    let current = current["."].as_str().expect("the manifest names a version");
    assert!(
        releases.iter().any(|release| release == current),
        "CHANGELOG.md has no heading for {current}, the version .release-please-manifest.json names"
    );
    let missing = missing_stores(
        &releases,
        current,
        |release| fixtures().join(format!("store-{release}")).is_dir(),
        has_release_tag,
    );
    assert!(
        missing.is_empty(),
        "no golden stores for release {missing:?}: write them with `bash scripts/golden-stores.sh VERSION`, \
         which builds that release from its tag (docs/development-process.md, Releases)"
    );
}

#[test]
fn every_golden_store_replays_to_its_committed_state() {
    let stores = stores();
    assert!(stores.len() >= 2, "the golden stores are missing");
    // Every store is compared before failing, so one run shows them all.
    let differences: Vec<String> = stores
        .iter()
        .filter_map(|store| {
            let snapshot = store.with_extension("state.json");
            snapshot::differs(&snapshot, &snapshot::state_json(&replay(store)))
        })
        .collect();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
