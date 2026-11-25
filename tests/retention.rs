//! Retention for the event log (ADR 0025, J7): compaction into a snapshot,
//! with real SQLite files, real daemons and the real CLI.

use langspan::store::Store;
use langspan_core::{Command, CompactionReport, Outcome, State};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

const DAY_MS: u64 = 86_400_000;
/// 1 January 2026, as the wall clock the generated history starts at.
const START_MS: u64 = 1_767_225_600_000;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ls-retention-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A wall clock a test moves by hand.
#[derive(Clone)]
struct Wall(Arc<AtomicU64>);

impl Wall {
    fn at(ms: u64) -> Self {
        Self(Arc::new(AtomicU64::new(ms)))
    }
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::Relaxed);
    }
    fn now(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
    fn open(&self, path: &Path) -> Store {
        let wall = self.0.clone();
        Store::open_with_wall_clock(path, Box::new(move || wall.load(Ordering::Relaxed))).unwrap()
    }
}

fn run(store: &mut Store, id: &str, command: Command, now: u64) -> langspan::store::Execution {
    store.execute(id, &command, &command, now).unwrap()
}

fn report(outcome: Outcome) -> CompactionReport {
    match outcome {
        Outcome::Compaction { report } => report,
        other => panic!("expected a compaction report, got {other:?}"),
    }
}

fn count(path: &Path, table: &str) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// A byte-for-byte copy of the database, taken through SQLite.
fn copy(from: &Path, to: &Path) {
    Connection::open(from)
        .unwrap()
        .execute("VACUUM INTO ?1", [to.to_str().unwrap()])
        .unwrap();
}

/// Compare two states field by field, naming the first field that differs.
fn assert_same_state(left: &State, right: &State) {
    let (serde_json::Value::Object(left), serde_json::Value::Object(right)) = (
        serde_json::to_value(left).unwrap(),
        serde_json::to_value(right).unwrap(),
    ) else {
        panic!("a state is not a JSON object");
    };
    let keys: std::collections::BTreeSet<_> = left.keys().chain(right.keys()).collect();
    for key in keys {
        assert_eq!(left.get(key), right.get(key), "state field {key} differs");
    }
}

/// A small deterministic generator, so a failing seed can be run again.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Generates coordination history: sessions admitting and completing
/// files, intents, notes, messages and their acknowledgements, and
/// publications, with the wall clock advancing between them.
struct History {
    random: Random,
    step: u64,
    leases: Vec<String>,
    messages: Vec<(String, String)>,
    published: u64,
}

const SESSIONS: [&str; 3] = ["alice", "bob", "carol"];

impl History {
    fn new(seed: u64) -> Self {
        Self {
            random: Random(seed | 1),
            step: 0,
            leases: Vec::new(),
            messages: Vec::new(),
            published: 0,
        }
    }

    fn setup(store: &mut Store) {
        run(
            store,
            "workspace",
            Command::WorkspaceRegister {
                workspace_id: "w".into(),
                root: "/retention-workspace".into(),
            },
            0,
        );
        for session in SESSIONS {
            run(
                store,
                &format!("register-{session}"),
                Command::SessionRegister {
                    session_id: session.into(),
                    workspace_id: "w".into(),
                    parent: None,
                    headless: false,
                },
                0,
            );
        }
        run(
            store,
            "remote-enable",
            Command::RemoteEnable {
                workspace_id: "w".into(),
                remote: "origin".into(),
                participant: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                name: "Alice".into(),
                share_task: false,
                publish_as_branch: false,
            },
            0,
        );
    }

    /// One more operation, at monotonic time `step` seconds.
    fn step(&mut self, store: &mut Store, wall: &Wall) {
        self.step += 1;
        let now = self.step * 1000;
        let id = format!("op-{}", self.step);
        let session = SESSIONS[self.random.below(3) as usize].to_owned();
        let path = format!("src/file{}.rs", self.random.below(8));
        match self.random.below(8) {
            0 | 1 => {
                let execution = run(
                    store,
                    &id,
                    Command::Admit {
                        session_id: session,
                        paths: vec![path],
                        ttl_ms: 60_000,
                    },
                    now,
                );
                if let Outcome::Admitted { lease } = execution.outcome {
                    self.leases.push(lease.lease_id);
                }
            }
            2 if !self.leases.is_empty() => {
                let lease = self
                    .leases
                    .remove(self.random.below(self.leases.len() as u64) as usize);
                let holder = store.state().leases[&lease].session_id.clone();
                run(
                    store,
                    &id,
                    Command::Complete {
                        session_id: holder,
                        lease_id: lease,
                    },
                    now,
                );
            }
            3 => {
                run(
                    store,
                    &id,
                    Command::IntentSet {
                        session_id: session,
                        paths: vec![path],
                        summary: format!("work item {}", self.step),
                    },
                    now,
                );
            }
            4 => {
                run(
                    store,
                    &id,
                    Command::Remember {
                        workspace_id: "w".into(),
                        author: Some(session),
                        text: format!("note {}", self.step),
                        paths: vec![path],
                    },
                    now,
                );
            }
            5 => {
                let to = SESSIONS[self.random.below(3) as usize];
                if to != session
                    && let Outcome::MessageSent { message } = run(
                        store,
                        &id,
                        Command::SendMessage {
                            workspace_id: "w".into(),
                            from: Some(session),
                            to: Some(to.into()),
                            text: format!("message {}", self.step),
                            paths: Vec::new(),
                        },
                        now,
                    )
                    .outcome
                {
                    self.messages.push((to.into(), message.message_id));
                }
            }
            6 if !self.messages.is_empty() => {
                let (to, message) = self.messages.remove(0);
                run(
                    store,
                    &id,
                    Command::AckMessage {
                        session_id: to,
                        message_id: message,
                    },
                    now,
                );
            }
            _ => {
                self.published += 1;
                run(
                    store,
                    &id,
                    Command::RemotePublished {
                        workspace_id: "w".into(),
                        sequence: self.published,
                        signature: format!("signature-{}", self.published),
                        payload: format!("{{\"n\":{}}}", self.published),
                        published_at_ms: wall.now(),
                        as_branch: None,
                    },
                    now,
                );
            }
        }
        store.refresh(now).unwrap();
    }

    /// `days` days of history, `per_day` operations a day.
    fn days(&mut self, store: &mut Store, wall: &Wall, days: u64, per_day: u64) {
        for _ in 0..days {
            for _ in 0..per_day {
                self.step(store, wall);
                wall.set(wall.now() + DAY_MS / per_day);
            }
        }
    }
}

#[test]
fn a_dry_run_deletes_nothing_and_an_applied_one_deletes_what_it_reported() {
    let directory = Directory::new();
    let path = directory.file("state.sqlite3");
    let wall = Wall::at(START_MS);
    let mut store = wall.open(&path);
    History::setup(&mut store);
    History::new(7).days(&mut store, &wall, 120, 10);
    let (events, receipts) = (count(&path, "events"), count(&path, "receipts"));

    let dry = report(store.compact(90, false).unwrap());
    assert!(!dry.applied);
    assert!(dry.events > 0 && dry.receipts > 0, "{dry:?}");
    assert!(dry.bytes > 0);
    assert_eq!(dry.cutoff_ms, wall.now() - 90 * DAY_MS);
    assert_eq!(count(&path, "events"), events, "a dry run deleted events");
    assert_eq!(
        count(&path, "receipts"),
        receipts,
        "a dry run deleted receipts"
    );
    assert_eq!(count(&path, "snapshots"), 0, "a dry run wrote a snapshot");

    let applied = report(store.compact(90, true).unwrap());
    assert!(applied.applied);
    assert_eq!(
        (applied.events, applied.receipts, applied.snapshot_seq),
        (dry.events, dry.receipts, dry.snapshot_seq)
    );
    assert_eq!(count(&path, "events"), events - dry.events as i64);
    assert_eq!(count(&path, "receipts"), receipts - dry.receipts as i64);
    assert_eq!(count(&path, "snapshots"), 1);
    // What is left is all within the window.
    let oldest: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT min(recorded_at_ms) FROM receipts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(oldest as u64 >= dry.cutoff_ms);

    // Nothing more is older than the window, so a second run removes nothing.
    let again = report(store.compact(90, true).unwrap());
    assert_eq!((again.events, again.receipts), (0, 0), "{again:?}");
}

#[test]
fn state_after_restart_equals_the_state_before_compaction_field_by_field() {
    let directory = Directory::new();
    let path = directory.file("state.sqlite3");
    let untouched = directory.file("untouched.sqlite3");
    let wall = Wall::at(START_MS);
    let mut store = wall.open(&path);
    History::setup(&mut store);
    let mut history = History::new(11);
    history.days(&mut store, &wall, 100, 12);
    let before = store.state().clone();
    drop(store);
    copy(&path, &untouched);

    let mut store = wall.open(&path);
    let applied = report(store.compact(30, true).unwrap());
    assert!(applied.events > 0, "{applied:?}");
    drop(store);

    // Both restart: one from the snapshot and the events after it, the other
    // from every event. Each start records one more generation, and the
    // compacted file was opened once more, to compact it.
    drop(wall.open(&untouched));
    let compacted = wall.open(&path);
    let full = wall.open(&untouched);
    assert_same_state(compacted.state(), full.state());
    assert_eq!(compacted.state().generation, before.generation + 2);
    assert_eq!(compacted.state().notes, before.notes);
    assert_eq!(compacted.state().messages, before.messages);

    // And they stay equal as more history arrives and is compacted again.
    let (mut compacted, mut full) = (compacted, full);
    let (mut twin, mut other) = (History::new(13), History::new(13));
    twin.step = history.step;
    other.step = history.step;
    for _ in 0..200 {
        twin.step(&mut compacted, &wall);
        other.step(&mut full, &wall);
        wall.set(wall.now() + DAY_MS / 4);
    }
    assert!(report(compacted.compact(7, true).unwrap()).events > 0);
    drop(compacted);
    drop(full);
    assert_same_state(wall.open(&path).state(), wall.open(&untouched).state());
}

/// The property: for random histories and random retention windows,
/// repeated compactions never change the state a start recovers.
#[test]
fn snapshot_plus_tail_replay_equals_full_replay_for_random_histories() {
    for seed in [1u64, 2, 3, 5, 8, 13, 21, 34] {
        let directory = Directory::new();
        let path = directory.file("compacted.sqlite3");
        let full = directory.file("full.sqlite3");
        let wall = Wall::at(START_MS);
        let mut compacted = wall.open(&path);
        let mut uncompacted = wall.open(&full);
        History::setup(&mut compacted);
        History::setup(&mut uncompacted);
        let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let (mut left, mut right) = (History::new(seed), History::new(seed));
        for round in 0..6 {
            let steps = 20 + random.below(60);
            for _ in 0..steps {
                left.step(&mut compacted, &wall);
                right.step(&mut uncompacted, &wall);
                wall.set(wall.now() + random.below(2 * DAY_MS));
            }
            let keep = 7 + random.below(30) as u32;
            let outcome = compacted.compact(keep, true).unwrap();
            assert!(
                matches!(outcome, Outcome::Compaction { .. }),
                "seed {seed} round {round}: {outcome:?}"
            );
            // A start from the compacted file equals one from every event.
            drop(compacted);
            drop(uncompacted);
            compacted = wall.open(&path);
            uncompacted = wall.open(&full);
            assert_same_state(compacted.state(), uncompacted.state());
        }
    }
}

#[test]
fn an_admission_open_at_compaction_is_never_cut_and_stays_valid_within_its_ttl() {
    let directory = Directory::new();
    let path = directory.file("state.sqlite3");
    let wall = Wall::at(START_MS);
    let mut store = wall.open(&path);
    History::setup(&mut store);
    let mut history = History::new(3);
    history.days(&mut store, &wall, 20, 5);
    // Granted on a wall clock 100 days before the compaction. Expiry is on
    // the daemon's monotonic clock, where it is still well within its TTL.
    let now = history.step * 1000 + 1000;
    let lease = match run(
        &mut store,
        "long-admission",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/held.rs".into()],
            ttl_ms: 60_000,
        },
        now,
    )
    .outcome
    {
        Outcome::Admitted { lease } => lease,
        other => panic!("admission refused: {other:?}"),
    };
    let granted_at: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT max(seq) FROM events WHERE json_extract(event_json, '$.type') = 'lease_granted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // The wall clock moves on 120 days; the monotonic one only 20 seconds,
    // so the admission is still open when the compaction runs.
    wall.set(wall.now() + 120 * DAY_MS);
    for _ in 0..20 {
        history.step(&mut store, &wall);
    }
    assert!(
        store.state().leases[&lease.lease_id].status == langspan_core::LeaseStatus::Active,
        "the admission closed before the compaction"
    );

    let compaction = report(store.compact(30, true).unwrap());
    assert_eq!(compaction.snapshot_seq, Some(granted_at as u64 - 1));
    let held = compaction
        .held_by
        .expect("the compaction says what held it");
    assert!(held.contains(&lease.lease_id), "{held}");
    assert_eq!(
        count_where(&path, "seq = ?1", granted_at),
        1,
        "the admission's grant was deleted"
    );

    // Still valid within its TTL: it is active in this generation and has
    // not expired, a peer is refused the file, and the holder completes it.
    let at = history.step * 1000 + 500;
    let held = &store.state().leases[&lease.lease_id];
    assert_eq!(held.status, langspan_core::LeaseStatus::Active);
    assert_eq!(held.generation, store.state().generation);
    assert!(held.expires_at_ms > at);
    let peer = run(
        &mut store,
        "peer-admission",
        Command::Admit {
            session_id: "bob".into(),
            paths: vec!["src/held.rs".into()],
            ttl_ms: 60_000,
        },
        at,
    );
    assert!(
        matches!(peer.outcome, Outcome::Conflict { .. }),
        "{:?}",
        peer.outcome
    );
    let completed = store
        .execute(
            "complete-long-admission",
            &Command::Complete {
                session_id: "alice".into(),
                lease_id: lease.lease_id.clone(),
            },
            &Command::Complete {
                session_id: "alice".into(),
                lease_id: lease.lease_id.clone(),
            },
            at,
        )
        .unwrap();
    assert!(
        matches!(completed.outcome, Outcome::Completed { .. }),
        "{:?}",
        completed.outcome
    );
    assert_eq!(completed.lease_valid, Some(false), "completed, so released");

    // Once it is closed, the next compaction is no longer held by it.
    let next = report(store.compact(30, true).unwrap());
    assert!(next.held_by.is_none() || !next.held_by.unwrap().contains(&lease.lease_id));
    assert_eq!(count_where(&path, "seq = ?1", granted_at), 0);
}

fn count_where(path: &Path, condition: &str, value: i64) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(
            &format!("SELECT count(*) FROM events WHERE {condition}"),
            [value],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn an_open_admission_is_valid_after_compaction_within_its_ttl() {
    let directory = Directory::new();
    let path = directory.file("state.sqlite3");
    let wall = Wall::at(START_MS);
    let mut store = wall.open(&path);
    History::setup(&mut store);
    let mut history = History::new(4);
    history.days(&mut store, &wall, 60, 5);
    let now = history.step * 1000 + 1000;
    let Outcome::Admitted { lease } = run(
        &mut store,
        "fresh-admission",
        Command::Admit {
            session_id: "bob".into(),
            paths: vec!["src/fresh.rs".into()],
            ttl_ms: 60_000,
        },
        now,
    )
    .outcome
    else {
        panic!("admission refused");
    };
    assert!(report(store.compact(7, true).unwrap()).events > 0);
    // Another session is still refused the file, and the holder completes it.
    let refused = run(
        &mut store,
        "conflicting",
        Command::Admit {
            session_id: "carol".into(),
            paths: vec!["src/fresh.rs".into()],
            ttl_ms: 60_000,
        },
        now + 1000,
    );
    assert!(
        matches!(refused.outcome, Outcome::Conflict { .. }),
        "{:?}",
        refused.outcome
    );
    let retry = run(
        &mut store,
        "fresh-admission",
        Command::Admit {
            session_id: "bob".into(),
            paths: vec!["src/fresh.rs".into()],
            ttl_ms: 60_000,
        },
        now + 2000,
    );
    assert!(retry.replayed);
    assert_eq!(
        retry.lease_valid,
        Some(true),
        "the admission is still valid"
    );
    let _ = lease;
}

#[test]
fn a_retry_within_the_window_is_answered_and_one_after_it_is_a_new_request() {
    let directory = Directory::new();
    let path = directory.file("state.sqlite3");
    let wall = Wall::at(START_MS);
    let mut store = wall.open(&path);
    History::setup(&mut store);
    let note = |text: &str| Command::Remember {
        workspace_id: "w".into(),
        author: None,
        text: text.into(),
        paths: Vec::new(),
    };
    let old = run(&mut store, "old-note", note("old"), 1000);
    wall.set(START_MS + 80 * DAY_MS);
    let recent = run(&mut store, "recent-note", note("recent"), 2000);
    wall.set(START_MS + 100 * DAY_MS);
    report(store.compact(30, true).unwrap());

    let again = run(&mut store, "recent-note", note("recent"), 3000);
    assert!(again.replayed, "a retry within the window was not answered");
    assert_eq!(again.outcome, recent.outcome);

    // Within the window the ID still belongs to its command.
    let reused = run(&mut store, "recent-note", note("something else"), 3500);
    assert!(
        matches!(reused.outcome, Outcome::Error { ref code, .. } if code == "request_id_conflict"),
        "{:?}",
        reused.outcome
    );

    let after = run(&mut store, "old-note", note("old"), 4000);
    assert!(!after.replayed, "a retry after the window was answered");
    let _ = old;
    // After it, the ID is free: another command under it is a new request.
    let free = run(&mut store, "older-note", note("first"), 4500);
    wall.set(wall.now() + 40 * DAY_MS);
    report(store.compact(30, true).unwrap());
    let renewed = run(&mut store, "older-note", note("second"), 5000);
    assert!(!renewed.replayed);
    assert!(
        matches!(renewed.outcome, Outcome::Remembered { .. }),
        "{:?} after {:?}",
        renewed.outcome,
        free.outcome
    );
}

#[test]
fn remote_sent_lists_every_publication_within_the_window_and_says_from_when() {
    let directory = Directory::new();
    let path = directory.file("state.sqlite3");
    let wall = Wall::at(START_MS);
    let mut store = wall.open(&path);
    History::setup(&mut store);
    for sequence in 1..=40u64 {
        run(
            &mut store,
            &format!("publish-{sequence}"),
            Command::RemotePublished {
                workspace_id: "w".into(),
                sequence,
                signature: format!("signature-{sequence}"),
                payload: format!("{{\"n\":{sequence}}}"),
                published_at_ms: wall.now(),
                as_branch: None,
            },
            sequence * 1000,
        );
        wall.set(wall.now() + DAY_MS);
    }
    let sent = |store: &mut Store| match run(
        store,
        "read",
        Command::RemoteSent {
            workspace_id: "w".into(),
            limit: 100,
        },
        100_000,
    )
    .outcome
    {
        Outcome::RemoteSent { sent, since_ms, .. } => (sent, since_ms),
        other => panic!("{other:?}"),
    };
    let (all, since) = sent(&mut store);
    assert_eq!((all.len(), since), (40, None), "nothing removed yet");

    let compaction = report(store.compact(14, true).unwrap());
    drop(store);
    let mut store = wall.open(&path);
    let (kept, since) = sent(&mut store);
    assert_eq!(since, Some(compaction.cutoff_ms));
    let expected: Vec<u64> = all
        .iter()
        .filter(|record| record.published_at_ms.unwrap() >= compaction.cutoff_ms)
        .map(|record| record.sequence)
        .collect();
    assert_eq!(expected.len(), 14);
    assert_eq!(
        kept.iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn a_snapshot_that_differs_from_full_replay_aborts_with_nothing_deleted() {
    let directory = Directory::new();
    let path = directory.file("state.sqlite3");
    let wall = Wall::at(START_MS);
    let mut store = wall.open(&path);
    History::setup(&mut store);
    History::new(9).days(&mut store, &wall, 60, 5);
    // Another writer appends an event the daemon's state never saw: the
    // events on disk no longer give the state it holds.
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO events(event_json, recorded_at_ms) VALUES (?1, ?2)",
            [
                r#"{"type":"intent_ended","session_id":"alice"}"#.to_string(),
                wall.now().to_string(),
            ],
        )
        .unwrap();
    let (events, receipts) = (count(&path, "events"), count(&path, "receipts"));
    match store.compact(7, true).unwrap() {
        Outcome::Error { code, message } => {
            assert_eq!(code, "snapshot_mismatch");
            assert!(message.contains("intents"), "{message}");
            assert!(message.contains("nothing was deleted"), "{message}");
        }
        other => panic!("a differing snapshot was accepted: {other:?}"),
    }
    assert_eq!(count(&path, "events"), events);
    assert_eq!(count(&path, "receipts"), receipts);
    assert_eq!(count(&path, "snapshots"), 0);
}

#[test]
fn a_window_outside_its_bounds_is_refused() {
    let directory = Directory::new();
    let mut store = Wall::at(START_MS).open(&directory.file("state.sqlite3"));
    for days in [0, 6, 3651] {
        assert!(
            matches!(
                store.compact(days, false).unwrap(),
                Outcome::Error { ref code, .. } if code == "invalid_retention"
            ),
            "{days} days accepted"
        );
    }
    assert!(matches!(
        store.compact(7, false).unwrap(),
        Outcome::Compaction { .. }
    ));
}

#[test]
fn records_from_before_the_upgrade_count_from_the_upgrade() {
    let directory = Directory::new();
    let path = directory.file("state.sqlite3");
    // A version 3 store, as the previous build left it.
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE events (seq INTEGER PRIMARY KEY AUTOINCREMENT, event_json TEXT NOT NULL);
             CREATE TABLE receipts (request_id TEXT PRIMARY KEY NOT NULL, command_json TEXT NOT NULL, outcome_json TEXT NOT NULL);
             CREATE TABLE git_snapshots (workspace_id TEXT PRIMARY KEY NOT NULL, snapshot_json TEXT NOT NULL, summary_json TEXT NOT NULL);
             INSERT INTO events(event_json) VALUES ('{\"type\":\"booted\",\"generation\":1}');
             PRAGMA user_version = 3;",
        )
        .unwrap();
    drop(connection);
    let wall = Wall::at(START_MS);
    let mut store = wall.open(&path);
    let marker: i64 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(marker, 4);
    wall.set(START_MS + 10 * DAY_MS);
    assert_eq!(report(store.compact(30, true).unwrap()).events, 0);
    wall.set(START_MS + 31 * DAY_MS);
    let compaction = report(store.compact(30, true).unwrap());
    assert_eq!(compaction.events, 2, "the old boot and the upgrade's boot");
    drop(store);
    assert_eq!(wall.open(&path).state().generation, 3);
}

#[cfg(unix)]
mod daemon {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Child, Command as Process, Output, Stdio};
    use std::time::{Duration, Instant};

    struct Daemon {
        directory: Directory,
        child: Option<Child>,
    }

    impl Daemon {
        /// A state directory holding `days` of generated history that ends
        /// now on the real wall clock.
        fn with_history(days: u64, per_day: u64) -> Self {
            let directory = Directory::new();
            let state = directory.file("state");
            fs::create_dir(&state).unwrap();
            fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
            let path = state.join("state.sqlite3");
            let wall = Wall::at(langspan::store::wall_now_ms() - days * DAY_MS);
            let mut store = wall.open(&path);
            History::setup(&mut store);
            History::new(17).days(&mut store, &wall, days, per_day);
            drop(store);
            for name in ["state.sqlite3", "state.sqlite3-wal", "state.sqlite3-shm"] {
                let file = state.join(name);
                if file.exists() {
                    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
            Self {
                directory,
                child: None,
            }
        }
        fn state(&self) -> PathBuf {
            self.directory.file("state")
        }
        fn database(&self) -> PathBuf {
            self.state().join("state.sqlite3")
        }
        fn configure(&self, json: &str) {
            let path = self.state().join("config.json");
            fs::write(&path, json).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fn start(&mut self, environment: &[(&str, &str)]) {
            let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
            command
                .args(["daemon", "--state-dir"])
                .arg(self.state())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for (key, value) in environment {
                command.env(key, value);
            }
            self.child = Some(command.spawn().unwrap());
            let deadline = Instant::now() + Duration::from_secs(20);
            while !self.cli(&["status", "--json"]).status.success() {
                assert!(Instant::now() < deadline, "daemon did not become ready");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        fn stop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        fn cli(&self, args: &[&str]) -> Output {
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(args)
                .arg("--state-dir")
                .arg(self.state())
                .output()
                .unwrap()
        }
        fn events(&self) -> i64 {
            count(&self.database(), "events")
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            self.stop();
        }
    }

    #[test]
    fn the_cli_dry_run_reports_and_apply_compacts_months_of_history_in_a_real_daemon() {
        let mut daemon = Daemon::with_history(150, 40);
        daemon.start(&[]);
        let before = daemon.events();
        let before_dry = daemon.directory.file("before-dry.sqlite3");
        copy(&daemon.database(), &before_dry);

        let dry = daemon.cli(&["compact", "--keep-days", "90"]);
        let text = String::from_utf8_lossy(&dry.stdout);
        assert!(dry.status.success(), "{text}");
        assert!(text.contains("would delete"), "{text}");
        assert!(text.contains("dry run: add --apply to compact"), "{text}");
        assert!(text.contains("snapshot at seq"), "{text}");
        // The daemon keeps recording while it runs; none of what was there
        // before the dry run is gone.
        let database = daemon.database();
        assert_eq!(missing(&before_dry, &database, "events", "seq"), 0);
        assert_eq!(missing(&before_dry, &database, "receipts", "request_id"), 0);
        assert_eq!(count(&database, "snapshots"), 0);

        // Stop to copy the file as it is, then compact through the daemon.
        daemon.stop();
        let untouched = daemon.directory.file("untouched.sqlite3");
        copy(&daemon.database(), &untouched);
        daemon.start(&[]);
        let applied = daemon.cli(&["compact", "--keep-days", "90", "--apply", "--json"]);
        assert!(
            applied.status.success(),
            "{}{}",
            String::from_utf8_lossy(&applied.stdout),
            String::from_utf8_lossy(&applied.stderr)
        );
        let reply: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
        let report: CompactionReport =
            serde_json::from_value(reply["outcome"]["report"].clone()).unwrap();
        assert!(report.applied && report.events > 0, "{report:?}");
        assert!(daemon.events() < before);

        // The restarted daemon answers from the snapshot and the events after it.
        daemon.stop();
        daemon.start(&[]);
        let status = daemon.cli(&["status", "--all", "--json"]);
        assert!(status.status.success());
        daemon.stop();

        // Full history again: the compacted file without its snapshot, with
        // the deleted events put back from the copy taken before.
        let full = daemon.directory.file("full.sqlite3");
        copy(&daemon.database(), &full);
        let connection = Connection::open(&full).unwrap();
        connection
            .execute("ATTACH ?1 AS before", [untouched.to_str().unwrap()])
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO events(seq, event_json, recorded_at_ms)
                   SELECT seq, event_json, recorded_at_ms FROM before.events
                   WHERE seq NOT IN (SELECT seq FROM events);
                 DELETE FROM snapshots;
                 DETACH before;",
            )
            .unwrap();
        drop(connection);
        let wall = Wall::at(langspan::store::wall_now_ms());
        let compacted = wall.open(&daemon.database());
        let replayed = wall.open(&full);
        assert_same_state(compacted.state(), replayed.state());
    }

    /// Rows of `table` in `before` that `after` no longer holds.
    fn missing(before: &Path, after: &Path, table: &str, key: &str) -> i64 {
        let connection = Connection::open(after).unwrap();
        connection
            .execute("ATTACH ?1 AS before", [before.to_str().unwrap()])
            .unwrap();
        connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM before.{table} WHERE {key} NOT IN (SELECT {key} FROM main.{table})"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn with_retention_days_unset_nothing_is_deleted_and_set_the_daemon_compacts() {
        let mut daemon = Daemon::with_history(60, 10);
        let before = daemon.events();
        daemon.start(&[]);
        // Several maintenance passes, with no retention configured.
        std::thread::sleep(Duration::from_millis(1500));
        daemon.stop();
        assert!(daemon.events() >= before, "events were deleted");
        assert_eq!(count(&daemon.database(), "snapshots"), 0);

        daemon.configure(r#"{"retention_days":30}"#);
        daemon.start(&[]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while count(&daemon.database(), "snapshots") == 0 {
            assert!(Instant::now() < deadline, "the daemon did not compact");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(daemon.events() < before);
        daemon.stop();

        daemon.configure(r#"{"retention_days":6}"#);
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        let output = command
            .args(["daemon", "--state-dir"])
            .arg(daemon.state())
            .output()
            .unwrap();
        assert!(!output.status.success(), "a 6-day window was accepted");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("retention_days 7..3650"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_daemon_killed_mid_compaction_leaves_the_previous_state() {
        let mut daemon = Daemon::with_history(120, 10);
        let untouched = daemon.directory.file("untouched.sqlite3");
        copy(&daemon.database(), &untouched);
        daemon.start(&[("LANGSPAN_TEST_COMPACT_PAUSE_MS", "8000")]);
        let mut compaction = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["compact", "--keep-days", "30", "--apply", "--state-dir"])
            .arg(daemon.state())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // The compaction holds the write lock while it pauses before its
        // commit: wait until another writer has been refused for a while.
        let probe = Connection::open(daemon.database()).unwrap();
        probe.busy_timeout(Duration::ZERO).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut busy_since: Option<Instant> = None;
        loop {
            let busy = probe.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_err();
            match (busy, busy_since) {
                (true, None) => busy_since = Some(Instant::now()),
                (true, Some(since)) if since.elapsed() > Duration::from_millis(1000) => break,
                (false, _) => busy_since = None,
                _ => {}
            }
            if Instant::now() >= deadline {
                daemon.stop();
                let output = compaction.wait_with_output().unwrap();
                panic!(
                    "the compaction never held the log: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(probe);
        daemon.stop(); // SIGKILL, inside the open transaction.
        let _ = compaction.wait();

        // The deletes never committed: every event and receipt from before
        // is still there, and no snapshot was written.
        let database = daemon.database();
        assert_eq!(count(&database, "snapshots"), 0);
        assert_eq!(missing(&untouched, &database, "events", "seq"), 0);
        assert_eq!(missing(&untouched, &database, "receipts", "request_id"), 0);
        // And the store opens from it as from full history.
        drop(Wall::at(langspan::store::wall_now_ms()).open(&database));

        // A daemon started again compacts it completely.
        daemon.start(&[]);
        let applied = daemon.cli(&["compact", "--keep-days", "30", "--apply"]);
        assert!(applied.status.success());
        assert!(String::from_utf8_lossy(&applied.stdout).starts_with("deleted "));
        assert_eq!(count(&daemon.database(), "snapshots"), 1);
    }
}
