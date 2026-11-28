//! The event log's hash chain (issue #91): every row written carries the
//! digest of the previous digest and its JSON, and `verify` names the first
//! row that does not. These tests use a real SQLite file and edit it
//! directly to stand in for tampering: persistence is what they verify.

use langspan::store::Store;
use langspan::store::chain::{GENESIS, Verdict, link, verify};
use langspan_core::{Command, Outcome};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const DAY_MS: u64 = 86_400_000;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "langspan-chain-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> PathBuf {
        self.0.join("state.sqlite3")
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open(path: &Path, wall: &Arc<AtomicU64>) -> Store {
    let wall = wall.clone();
    Store::open_with_wall_clock(path, Box::new(move || wall.load(Ordering::Relaxed))).unwrap()
}

/// A store with a workspace, a session and two boots: at least six events.
fn populated(path: &Path, wall: &Arc<AtomicU64>) {
    let mut store = open(path, wall);
    for (id, command) in [
        (
            "w",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/test-workspace".into(),
            },
        ),
        (
            "s",
            Command::SessionRegister {
                session_id: "alice".into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
        ),
    ] {
        let outcome = store.execute(id, &command, &command, 0).unwrap().outcome;
        assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    }
    drop(store);
    drop(open(path, wall));
}

fn rows(path: &Path) -> Vec<(i64, String, Option<String>)> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("SELECT seq, event_json, digest FROM events ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn ok(path: &Path) -> Verdict {
    let verdict = verify(path).unwrap();
    assert!(verdict.ok, "{verdict:?}");
    verdict
}

#[test]
fn every_event_written_links_the_previous_digest_and_its_json() {
    let directory = Directory::new();
    populated(&directory.store(), &Arc::new(AtomicU64::new(1)));
    let rows = rows(&directory.store());
    assert!(rows.len() >= 6, "{rows:?}");
    let mut previous = GENESIS.to_string();
    for (seq, json, digest) in &rows {
        let expected = link(&previous, json);
        assert_eq!(digest.as_deref(), Some(expected.as_str()), "event {seq}");
        previous = expected;
    }
    let verdict = ok(&directory.store());
    assert_eq!(verdict.events, rows.len() as u64);
    assert_eq!(verdict.head, Some(previous.clone()));
    assert_eq!(
        verdict.text(),
        format!("ok: {} events, head {previous}", rows.len())
    );
}

#[test]
fn changing_one_byte_of_an_old_event_names_that_event() {
    let directory = Directory::new();
    populated(&directory.store(), &Arc::new(AtomicU64::new(1)));
    let (seq, json, _) = rows(&directory.store())[2].clone();
    let edited = json.replacen("work", "wark", 1);
    assert_ne!(edited, json);
    Connection::open(directory.store())
        .unwrap()
        .execute(
            "UPDATE events SET event_json = ?1 WHERE seq = ?2",
            rusqlite::params![edited, seq],
        )
        .unwrap();
    let verdict = verify(&directory.store()).unwrap();
    assert_eq!((verdict.ok, verdict.broken_at), (false, Some(seq as u64)));
    assert_eq!(verdict.events, 2, "the two events before it checked");
    assert_eq!(verdict.head, None);
}

#[test]
fn deleting_a_row_in_the_middle_names_the_next_event() {
    let directory = Directory::new();
    populated(&directory.store(), &Arc::new(AtomicU64::new(1)));
    let all = rows(&directory.store());
    let (gone, next) = (all[2].0, all[3].0);
    Connection::open(directory.store())
        .unwrap()
        .execute("DELETE FROM events WHERE seq = ?1", [gone])
        .unwrap();
    let verdict = verify(&directory.store()).unwrap();
    assert_eq!(verdict.broken_at, Some(next as u64));
}

/// A row an earlier build wrote has no digest. After the newest chained row
/// the next start chains it; before a chained row it stays a break, and a
/// start does not hide it.
#[test]
fn rows_without_a_digest_are_chained_at_the_end_and_reported_in_the_middle() {
    let directory = Directory::new();
    let wall = Arc::new(AtomicU64::new(1));
    populated(&directory.store(), &wall);
    let connection = Connection::open(directory.store()).unwrap();
    connection
        .execute(
            "INSERT INTO events(event_json) VALUES ('{\"type\":\"booted\",\"generation\":9}')",
            [],
        )
        .unwrap();
    let inserted = connection.last_insert_rowid() as u64;
    assert_eq!(
        verify(&directory.store()).unwrap().broken_at,
        Some(inserted)
    );
    drop(open(&directory.store(), &wall));
    ok(&directory.store());

    let middle = rows(&directory.store())[1].0;
    connection
        .execute("UPDATE events SET digest = NULL WHERE seq = ?1", [middle])
        .unwrap();
    drop(open(&directory.store(), &wall));
    assert_eq!(
        verify(&directory.store()).unwrap().broken_at,
        Some(middle as u64)
    );
}

/// Compaction deletes old rows; the chain of the rows it keeps, and of the
/// rows written after, still checks, across two compactions.
#[test]
fn the_chain_still_checks_after_compaction() {
    let directory = Directory::new();
    let wall = Arc::new(AtomicU64::new(1_700_000_000_000));
    populated(&directory.store(), &wall);
    for round in 1..=2u64 {
        wall.store(1_700_000_000_000 + round * 30 * DAY_MS, Ordering::Relaxed);
        let mut store = open(&directory.store(), &wall);
        let Outcome::Compaction { report } = store.compact(7, true).unwrap() else {
            panic!("expected a compaction report");
        };
        assert!(report.events > 0, "{report:?}");
        ok(&directory.store());
        drop(store);
        drop(open(&directory.store(), &wall));
        assert_eq!(
            ok(&directory.store()).events,
            event_count(&directory.store())
        );
    }
}

/// Every golden store (`tests/fixtures/store-<release>/`) is from before
/// the chain: `verify` refuses it without writing, and once this build
/// opens it, every old event and the new boot check.
#[test]
fn every_golden_store_is_chained_when_this_build_opens_it() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut stores = Vec::new();
    for release in fs::read_dir(&fixtures).unwrap() {
        let release = release.unwrap().path();
        if release.is_dir() {
            for file in fs::read_dir(&release).unwrap() {
                let file = file.unwrap().path();
                if file.extension().is_some_and(|e| e == "sqlite3") {
                    stores.push(file);
                }
            }
        }
    }
    assert!(
        stores.len() >= 20,
        "the golden stores are missing: {stores:?}"
    );
    for fixture in stores {
        let directory = Directory::new();
        fs::copy(&fixture, directory.store()).unwrap();
        let before = event_count(&directory.store());
        let error = verify(&directory.store()).unwrap_err().to_string();
        assert!(
            error.contains("start the coordinator with this build once"),
            "{}: {error}",
            fixture.display()
        );
        assert_eq!(
            fs::read(&fixture).unwrap(),
            fs::read(directory.store()).unwrap(),
            "{}: verify wrote to the store",
            fixture.display()
        );
        drop(Store::open(&directory.store()).unwrap());
        let verdict = ok(&directory.store());
        assert_eq!(verdict.events, before + 2, "{}", fixture.display());
    }
}

fn event_count(path: &Path) -> u64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM events", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap() as u64
}

/// The command a person runs, as a process: its exit code is the interface.
#[test]
fn events_verify_prints_the_verdict_and_exits_1_when_broken() {
    let directory = Directory::new();
    populated(&directory.store(), &Arc::new(AtomicU64::new(1)));
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["events", "verify", "--json", "--state-dir"])
            .arg(&directory.0)
            .output()
            .unwrap()
    };
    let output = run();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answer["ok"], true);
    assert_eq!(answer["head"].as_str().map(str::len), Some(64));

    Connection::open(directory.store())
        .unwrap()
        .execute(
            "UPDATE events SET event_json = event_json || ' ' WHERE seq = 1",
            [],
        )
        .unwrap();
    let output = run();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        (answer["ok"].clone(), answer["broken_at"].clone()),
        (false.into(), 1.into())
    );
}
