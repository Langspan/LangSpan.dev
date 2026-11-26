use langspan::store::Store;
use langspan_core::{Command, LeaseStatus, Outcome};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Database {
    directory: PathBuf,
}

impl Database {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "langspan-store-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&directory)
            .expect("create isolated test directory");
        Self { directory }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("state.sqlite")
    }

    fn connection(&self) -> Connection {
        Connection::open(self.path()).expect("open real SQLite fixture")
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.directory) {
            eprintln!("failed to clean up SQLite test fixture: {error}");
        }
    }
}

fn execute(store: &mut Store, id: &str, command: Command, now: u64) -> Outcome {
    store
        .execute(id, &command, &command, now)
        .expect("execute fixture command")
        .outcome
}

fn register(store: &mut Store) {
    assert!(matches!(
        execute(
            store,
            "workspace-registration",
            Command::WorkspaceRegister {
                workspace_id: "workspace".into(),
                root: "/test-workspace".into(),
            },
            0
        ),
        Outcome::Workspace { .. }
    ));
    assert!(matches!(
        execute(
            store,
            "session-registration",
            Command::SessionRegister {
                session_id: "session".into(),
                workspace_id: "workspace".into(),
                parent: None,
                headless: false,
            },
            0
        ),
        Outcome::Session { .. }
    ));
}

#[test]
fn semantic_tool_input_is_part_of_durable_retry_identity() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    let command = Command::AdmitTool {
        session_id: "session".into(),
        paths: vec!["a.rs".into()],
        ttl_ms: 1000,
        operation: langspan_core::ToolIdentity {
            operation_id: "operation".into(),
            source: "codex".into(),
            tool: "apply_patch".into(),
            input_digest: "a".repeat(64),
        },
        base: Vec::new(),
        regions: Vec::new(),
        overwrites: Vec::new(),
    };
    assert!(matches!(
        execute(&mut store, "tool-admit", command.clone(), 1),
        Outcome::Admitted { .. }
    ));
    let mut changed = command.clone();
    if let Command::AdmitTool { operation, .. } = &mut changed {
        operation.input_digest = "b".repeat(64);
    }
    assert!(
        matches!(execute(&mut store, "tool-admit", changed, 2), Outcome::Error {code,..} if code == "request_id_conflict")
    );
    drop(store);
    let mut reopened = Store::open(&database.path()).unwrap();
    let replay = reopened
        .execute("tool-admit", &command, &command, 0)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.lease_valid, Some(false));
}

#[test]
fn observation_pages_fit_frames_and_the_cursor_loses_no_records() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    for index in 0..30 {
        let command = Command::ObserveDisk {
            workspace_id: "workspace".into(),
            path: format!("{index:03}-{}", "x".repeat(400)),
            before: None,
            after: langspan_core::FileState::Present {
                digest: "a".repeat(64),
                kind: langspan_core::FileKind::Regular,
            },
            related_operation_ids: (0..64)
                .map(|n| format!("op-{n:03}-{}", "x".repeat(100)))
                .collect(),
        };
        assert!(matches!(
            execute(&mut store, &format!("disk-{index}"), command, index + 1),
            Outcome::DiskObserved { .. }
        ));
    }
    let mut after = 0;
    let mut count = 0;
    loop {
        let outcome = execute(
            &mut store,
            "read-changes",
            Command::Changes {
                workspace_id: "workspace".into(),
                after,
                limit: 1000,
            },
            31,
        );
        assert!(
            langspan::protocol::encode(&outcome).is_ok(),
            "page exceeds transport frame"
        );
        let Outcome::Changes { events, .. } = outcome else {
            panic!("missing changes projection: {outcome:?}")
        };
        if events.is_empty() {
            break;
        }
        assert!(
            events.len() < 30,
            "byte budget must page before count limit"
        );
        assert!(events.iter().all(|event| event.seq > after));
        after = events.last().unwrap().seq;
        count += events.len();
    }
    assert_eq!(count, 30);
    let events = execute(
        &mut store,
        "events-read",
        Command::Events {
            after: 0,
            limit: 1000,
        },
        31,
    );
    assert!(
        langspan::protocol::encode(&events).is_ok(),
        "event pages also need a byte budget"
    );
}

fn scan_fixture(digest: char, dirty: bool) -> langspan::observer::Snapshot {
    use langspan::observer::{BranchRecord, FileKind, FileVersion, Snapshot};
    Snapshot {
        root: "/test-workspace".into(),
        common_dir: "/test-workspace/.git".into(),
        head: Some("1".repeat(40)),
        head_tree: Some("2".repeat(40)),
        branch: Some("main".into()),
        files: std::collections::BTreeMap::from([(
            "a.rs".into(),
            FileVersion {
                digest: Some(digest.to_string().repeat(64)),
                kind: FileKind::Regular,
                dirty,
                conflicted: false,
            },
        )]),
        branches: vec![BranchRecord {
            name: "main".into(),
            commit: "1".repeat(40),
            tree: "2".repeat(40),
        }],
        ..Snapshot::default()
    }
}

fn changes(
    store: &mut Store,
) -> (
    Vec<langspan_core::RecordedEvent>,
    Option<langspan_core::ScanSummary>,
) {
    let outcome = execute(
        store,
        "read-scan",
        Command::Changes {
            workspace_id: "workspace".into(),
            after: 0,
            limit: 1000,
        },
        100,
    );
    let Outcome::Changes { events, scan } = outcome else {
        panic!("unexpected changes outcome: {outcome:?}")
    };
    (events, scan)
}

#[test]
fn complete_git_scan_retains_baseline_across_restart_without_claiming_an_author() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    let baseline = scan_fixture('a', false);
    assert_eq!(
        store.record_scan("workspace", &baseline, 1).unwrap(),
        2,
        "HEAD and local branch baseline"
    );
    assert!(
        changes(&mut store)
            .0
            .iter()
            .all(|event| matches!(event.event, langspan_core::Event::BranchObserved { .. }))
    );
    let changed = scan_fixture('b', true);
    assert_eq!(store.record_scan("workspace", &changed, 2).unwrap(), 1);
    let (events, summary) = changes(&mut store);
    let observation = events
        .iter()
        .find_map(|event| match &event.event {
            langspan_core::Event::DiskObserved { observation } => Some(observation),
            _ => None,
        })
        .unwrap();
    assert!(
        observation.before.is_some(),
        "previous complete scan supplies a known baseline"
    );
    assert!(
        observation.related_operation_ids.is_empty(),
        "filesystem timing is not an author proof"
    );
    assert_eq!(summary.unwrap().observed_at_ms, 2);
    drop(store);
    let mut reopened = Store::open(&database.path()).unwrap();
    assert_eq!(
        reopened.record_scan("workspace", &changed, 3).unwrap(),
        0,
        "restart must not duplicate unchanged facts"
    );
    let (_, summary) = changes(&mut reopened);
    assert_eq!(summary.unwrap().generation, reopened.state().generation);
}

#[test]
fn initial_dirty_file_has_unknown_before_and_symlink_identity_survives() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    let mut snapshot = scan_fixture('a', true);
    store.record_scan("workspace", &snapshot, 1).unwrap();
    let (events, _) = changes(&mut store);
    assert!(events.iter().any(|event| matches!(&event.event, langspan_core::Event::DiskObserved {observation} if observation.before.is_none())));
    snapshot.files.get_mut("a.rs").unwrap().kind = langspan::observer::FileKind::Symlink;
    assert_eq!(
        store.record_scan("workspace", &snapshot, 2).unwrap(),
        1,
        "same digest with different file kind is a change"
    );
    let (events, _) = changes(&mut store);
    let json = serde_json::to_string(&events.last().unwrap().event).unwrap();
    assert!(
        json.contains("symlink"),
        "persisted observation lost the file kind: {json}"
    );
}

#[test]
fn scan_projection_and_events_commit_together_or_leave_the_previous_scan() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    store
        .record_scan("workspace", &scan_fixture('a', false), 1)
        .unwrap();
    let before = event_count(&database.connection());
    database.connection().execute_batch("CREATE TRIGGER reject_scan BEFORE UPDATE ON git_snapshots BEGIN SELECT RAISE(ABORT, 'injected projection failure'); END;").unwrap();
    let error = store
        .record_scan("workspace", &scan_fixture('b', true), 2)
        .unwrap_err();
    assert!(error.to_string().contains("injected projection failure"));
    assert_eq!(
        event_count(&database.connection()),
        before,
        "rolled-back snapshot must not leave orphan change facts"
    );
    assert_eq!(changes(&mut store).1.unwrap().observed_at_ms, 1);
    database
        .connection()
        .execute_batch("DROP TRIGGER reject_scan")
        .unwrap();
    assert_eq!(
        store
            .record_scan("workspace", &scan_fixture('b', true), 3)
            .unwrap(),
        1
    );
}

#[test]
fn scan_from_a_different_root_cannot_replace_the_registered_workspace() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    let mut snapshot = scan_fixture('a', false);
    snapshot.root = "/different-workspace".into();
    let before = event_count(&database.connection());
    assert!(store.record_scan("workspace", &snapshot, 1).is_err());
    assert_eq!(event_count(&database.connection()), before);
    assert!(changes(&mut store).1.is_none());
}

fn admission(session: &str, path: &str, ttl_ms: u64) -> Command {
    Command::Admit {
        session_id: session.into(),
        paths: vec![path.into()],
        ttl_ms,
    }
}

fn event_count(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .expect("count committed events")
}

#[test]
fn restart_retains_history_and_intent_but_never_revives_a_grant() {
    let database = Database::new();
    let command = admission("session", "src/api.rs", 100);
    let first_generation;
    let original;
    {
        let mut store = Store::open(&database.path()).expect("initialize store");
        first_generation = store.state().generation;
        register(&mut store);
        execute(
            &mut store,
            "intent",
            Command::IntentSet {
                session_id: "session".into(),
                paths: vec!["src/api.rs".into()],
                summary: "update API".into(),
            },
            1,
        );
        original = store.execute("grant", &command, &command, 2).unwrap();
        assert_eq!(original.lease_valid, Some(true));
    }

    let mut reopened = Store::open(&database.path()).expect("recover store");
    assert_eq!(reopened.state().generation, first_generation + 1);
    assert_eq!(
        reopened.state().intents["session"].generation,
        first_generation
    );
    assert_eq!(
        reopened.state().leases["grant"].status,
        LeaseStatus::Invalidated
    );
    let replay = reopened.replay("grant", &command, 3).unwrap().unwrap();
    assert_eq!(replay.outcome, original.outcome);
    assert!(replay.replayed);
    assert_eq!(replay.lease_valid, Some(false));
    assert_eq!(replay.generation, first_generation + 1);
}

#[test]
fn expiry_is_durable_and_retry_keeps_the_original_outcome() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    let command = admission("session", "src/api.rs", 10);
    let original = store.execute("grant", &command, &command, 100).unwrap();
    let replay = store.replay("grant", &command, 110).unwrap().unwrap();
    assert_eq!(replay.outcome, original.outcome);
    assert_eq!(replay.lease_valid, Some(false));
    assert_eq!(store.state().leases["grant"].status, LeaseStatus::Expired);
    let count = event_count(&database.connection());
    store.refresh(111).unwrap();
    assert_eq!(
        event_count(&database.connection()),
        count,
        "expiry is not duplicated"
    );
    drop(store);
    let reopened = Store::open(&database.path()).unwrap();
    assert_eq!(
        reopened.state().leases["grant"].status,
        LeaseStatus::Expired
    );
}

#[test]
fn replay_uses_the_original_command_not_later_path_preparation() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    let original = admission("session", "src/alias.rs", 100);
    let prepared = admission("session", "src/target.rs", 100);
    let first = store.execute("grant", &original, &prepared, 1).unwrap();
    assert_eq!(store.state().leases["grant"].paths, vec!["src/target.rs"]);
    let changed_preparation = admission("session", "src/other.rs", 100);
    let replay = store
        .execute("grant", &original, &changed_preparation, 2)
        .unwrap();
    assert_eq!(replay.outcome, first.outcome);
    assert!(replay.replayed);
    let conflict = store.replay("grant", &prepared, 2).unwrap().unwrap();
    assert!(
        matches!(conflict.outcome, Outcome::Error { ref code, .. } if code == "request_id_conflict")
    );
    assert_eq!(store.state().leases.len(), 1);
}

#[test]
fn denied_and_completed_requests_replay_without_taking_fresh_authority() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    execute(
        &mut store,
        "second-session",
        Command::SessionRegister {
            session_id: "other".into(),
            workspace_id: "workspace".into(),
            parent: None,
            headless: false,
        },
        0,
    );
    execute(
        &mut store,
        "first-grant",
        admission("session", "src/api.rs", 100),
        1,
    );
    let denied_command = admission("other", "src/api.rs", 100);
    let denied = store
        .execute("denied", &denied_command, &denied_command, 2)
        .unwrap();
    assert!(matches!(denied.outcome, Outcome::Conflict { .. }));
    let complete = Command::Complete {
        session_id: "session".into(),
        lease_id: "first-grant".into(),
    };
    let completed = store.execute("complete", &complete, &complete, 3).unwrap();
    assert!(matches!(completed.outcome, Outcome::Completed { .. }));
    assert_eq!(completed.lease_valid, Some(false));
    let completed_replay = store.replay("complete", &complete, 4).unwrap().unwrap();
    assert_eq!(completed_replay.outcome, completed.outcome);
    assert_eq!(completed_replay.lease_valid, Some(false));
    let denied_replay = store.replay("denied", &denied_command, 4).unwrap().unwrap();
    assert_eq!(denied_replay.outcome, denied.outcome);
    assert!(denied_replay.replayed);
    assert!(matches!(
        execute(&mut store, "fresh-grant", denied_command, 5),
        Outcome::Admitted { .. }
    ));
}

#[test]
fn receipt_failure_rolls_back_events_and_does_not_publish_state() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    let connection = database.connection();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_receipt BEFORE INSERT ON receipts
         WHEN NEW.request_id = 'failing-intent'
         BEGIN SELECT RAISE(ABORT, 'injected receipt write failure'); END;",
        )
        .unwrap();
    let before = store.state().clone();
    let events_before = event_count(&connection);
    let command = Command::IntentSet {
        session_id: "session".into(),
        paths: vec!["src/api.rs".into()],
        summary: "atomic write".into(),
    };
    let failure = store
        .execute("failing-intent", &command, &command, 1)
        .unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("injected receipt write failure")
    );
    assert_eq!(
        store.state(),
        &before,
        "uncommitted state must never be published"
    );
    assert_eq!(
        event_count(&connection),
        events_before,
        "event and receipt are atomic"
    );
    let receipts: i64 = connection
        .query_row(
            "SELECT count(*) FROM receipts WHERE request_id = 'failing-intent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 0);
    connection
        .execute_batch("DROP TRIGGER reject_receipt")
        .unwrap();
    let retried = store
        .execute("failing-intent", &command, &command, 2)
        .unwrap();
    assert!(!retried.replayed);
    assert!(matches!(retried.outcome, Outcome::Intent { .. }));
    assert_eq!(event_count(&connection), events_before + 1);
}

#[test]
fn read_requests_are_current_uncached_and_event_cursors_are_bounded() {
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    assert!(store.replay("read", &Command::Status, 0).unwrap().is_none());
    let first = store
        .execute("read", &Command::Status, &Command::Status, 0)
        .unwrap();
    execute(
        &mut store,
        "intent",
        Command::IntentSet {
            session_id: "session".into(),
            paths: vec!["src/api.rs".into()],
            summary: "new activity".into(),
        },
        1,
    );
    let second = store
        .execute("read", &Command::Status, &Command::Status, 2)
        .unwrap();
    assert!(!second.replayed);
    assert_ne!(first.outcome, second.outcome);
    let command = Command::Events { after: 1, limit: 2 };
    let events = store.execute("read", &command, &command, 2).unwrap();
    match events.outcome {
        Outcome::Events { events } => {
            assert_eq!(events.len(), 2);
            assert!(events[0].seq > 1 && events[1].seq > events[0].seq);
        }
        other => panic!("expected event history, got {other:?}"),
    }
    for limit in [0, 1001] {
        assert!(matches!(
            execute(&mut store, "read", Command::Events { after: 0, limit }, 2),
            Outcome::Error { .. }
        ));
    }
    let count: i64 = database
        .connection()
        .query_row(
            "SELECT count(*) FROM receipts WHERE request_id = 'read'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

fn open_fails_without_appending(path: &Path) {
    let connection = Connection::open(path).unwrap();
    let before = event_count(&connection);
    assert!(
        Store::open(path).is_err(),
        "corrupt persistence must not be reset"
    );
    assert_eq!(event_count(&connection), before);
}

#[test]
fn malformed_events_fail_closed_instead_of_resetting_history() {
    let database = Database::new();
    {
        let mut store = Store::open(&database.path()).unwrap();
        register(&mut store);
    }
    database
        .connection()
        .execute_batch("UPDATE events SET event_json = '{broken' WHERE seq = 1")
        .unwrap();
    open_fails_without_appending(&database.path());
}

/// A receipt only answers a retry of its own request. One this build cannot
/// read (written by another version, or damaged) leaves the store opening
/// with its history, and only that request cannot be answered again.
#[test]
fn a_receipt_this_build_cannot_read_is_skipped_and_only_its_request_is_not_replayed() {
    for corrupt in [
        "UPDATE receipts SET outcome_json = '{broken' WHERE request_id = 'session-registration'",
        r#"UPDATE receipts SET command_json = '{"op":"a_command_from_a_newer_build"}' WHERE request_id = 'session-registration'"#,
    ] {
        let database = Database::new();
        {
            let mut store = Store::open(&database.path()).unwrap();
            register(&mut store);
        }
        database.connection().execute_batch(corrupt).unwrap();
        let mut store = Store::open(&database.path()).expect("the store opens");
        assert!(store.state().sessions.contains_key("session"), "{corrupt}");
        let command = Command::SessionRegister {
            session_id: "session".into(),
            workspace_id: "workspace".into(),
            parent: None,
            headless: false,
        };
        let replayed = store.replay("session-registration", &command, 1).unwrap();
        match replayed.map(|execution| execution.outcome) {
            Some(Outcome::Error { code, message }) => {
                assert_eq!(code, "receipt_unreadable", "{corrupt}");
                assert!(message.contains("new request ID"), "{message}");
            }
            other => panic!("{corrupt}: {other:?}"),
        }
        // Every other request is answered as before.
        let workspace = Command::WorkspaceRegister {
            workspace_id: "workspace".into(),
            root: "/test-workspace".into(),
        };
        let other = store
            .replay("workspace-registration", &workspace, 1)
            .unwrap();
        assert!(other.is_some_and(|execution| execution.replayed));
    }
}

#[test]
fn unsupported_schema_is_rejected_and_wal_is_enabled() {
    let database = Database::new();
    {
        let mut store = Store::open(&database.path()).unwrap();
        assert_eq!(store.state().generation, 1);
        let mode: String = database
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        database
            .connection()
            .execute_batch(
                "CREATE TRIGGER require_full BEFORE INSERT ON receipts
             WHEN (SELECT synchronous FROM pragma_synchronous) != 2
             BEGIN SELECT RAISE(ABORT, 'writer connection is not synchronous FULL'); END;",
            )
            .unwrap();
        register(&mut store);
    }
    database
        .connection()
        .execute_batch("PRAGMA user_version = 999")
        .unwrap();
    open_fails_without_appending(&database.path());
}

#[test]
fn a_recap_of_the_largest_valid_notes_still_fits_one_frame() {
    // Review finding: 16 notes of eight 511-byte paths exceeded the frame.
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    for index in 0..20 {
        let paths: Vec<String> = (0..8)
            .map(|n| format!("{}/{index:02}{n}", "d".repeat(500)))
            .collect();
        let command = Command::Remember {
            workspace_id: "workspace".into(),
            author: None,
            text: "n".repeat(400),
            paths,
        };
        let outcome = store
            .execute(&format!("note-{index}"), &command, &command, 10)
            .unwrap()
            .outcome;
        assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    }
    let command = Command::Recap {
        workspace_id: "workspace".into(),
        since: None,
    };
    let outcome = store
        .execute("read", &command, &command, 20)
        .unwrap()
        .outcome;
    let bytes = serde_json::to_vec(&outcome).unwrap().len();
    assert!(bytes <= 48 * 1024 + 64, "{bytes}");
    let Outcome::Recap { recap } = outcome else {
        panic!()
    };
    assert!(recap.omitted_items > 0, "what was left out is counted");
    assert!(!recap.notes.is_empty());
}

#[test]
fn a_store_written_before_the_branch_namespace_choice_still_opens() {
    let database = Database::new();
    let enable = Command::RemoteEnable {
        workspace_id: "workspace".into(),
        remote: "origin".into(),
        participant: "a".repeat(32),
        name: "Alice".into(),
        share_task: false,
        publish_as_branch: true,
    };
    {
        let mut store = Store::open(&database.path()).unwrap();
        register(&mut store);
        let outcome = execute(&mut store, "enable", enable, 1);
        assert!(matches!(outcome, Outcome::RemoteLink { ref link } if link.publish_as_branch));
    }
    // What the previous version wrote: no field at all, in the event, the
    // stored command or the stored outcome.
    database
        .connection()
        .execute_batch(
            "UPDATE events SET event_json = json_remove(event_json, '$.link.publish_as_branch');
             UPDATE receipts SET command_json = json_remove(command_json, '$.publish_as_branch'),
                                 outcome_json = json_remove(outcome_json, '$.link.publish_as_branch')
              WHERE request_id = 'enable';",
        )
        .unwrap();
    let text: String = database
        .connection()
        .query_row("SELECT group_concat(event_json) FROM events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(!text.contains("publish_as_branch"), "{text}");
    let mut store = Store::open(&database.path()).expect("an old store opens");
    let link = &store.state().remote_links["workspace"];
    assert!(
        !link.publish_as_branch,
        "an old link publishes where it did"
    );
    assert!(link.enabled);
    // Its receipt still answers a retry of the old command.
    let old = Command::RemoteEnable {
        workspace_id: "workspace".into(),
        remote: "origin".into(),
        participant: "a".repeat(32),
        name: "Alice".into(),
        share_task: false,
        publish_as_branch: false,
    };
    let outcome = execute(&mut store, "enable", old, 2);
    assert!(matches!(outcome, Outcome::RemoteLink { ref link } if !link.publish_as_branch));
    // Defaults are not written, so a link without the choice stays in the
    // old format.
    let fresh = Database::new();
    let mut store = Store::open(&fresh.path()).unwrap();
    register(&mut store);
    let plain = Command::RemoteEnable {
        workspace_id: "workspace".into(),
        remote: "origin".into(),
        participant: "a".repeat(32),
        name: "Alice".into(),
        share_task: false,
        publish_as_branch: false,
    };
    execute(&mut store, "plain", plain, 1);
    drop(store);
    let text: String = fresh
        .connection()
        .query_row(
            "SELECT group_concat(event_json) || (SELECT group_concat(command_json || outcome_json) FROM receipts) FROM events",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!text.contains("publish_as_branch"), "{text}");
}

#[test]
fn a_store_written_before_publication_payloads_were_kept_still_opens() {
    // Found by restarting a real coordinator on the new build: a receipt for
    // `remote_published` written by the previous build has no `payload`, and
    // the store refused to open at all.
    let database = Database::new();
    drop(Store::open(&database.path()).unwrap());
    let connection = rusqlite::Connection::open(database.path()).unwrap();
    connection
        .execute(
            "INSERT INTO receipts(request_id, command_json, outcome_json) VALUES (?1, ?2, ?3)",
            [
                "old-published",
                r#"{"op":"remote_published","workspace_id":"workspace","sequence":1,"signature":"abc"}"#,
                r#"{"type":"remote_disabled","workspace_id":"workspace"}"#,
            ],
        )
        .unwrap();
    drop(connection);
    Store::open(&database.path()).expect("a store from the previous build opens");
}

/// Golden stores: the state files of both coordinators at the end of
/// `scripts/demo-two-developers.sh`, written by release 0.1.0 (`VACUUM INTO`
/// after the run, so each is one file). Every later build must open them:
/// removing an event variant, renaming one, or making a field required that
/// an older event lacks fails here. To add a newer release's stores, run the
/// demonstration with its cleanup copying `*-state/state.sqlite3`, and add a
/// directory; never replace these.
#[test]
fn stores_written_by_earlier_releases_still_open() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut opened = 0;
    for release in fs::read_dir(&fixtures).unwrap() {
        let release = release.unwrap().path();
        if !release.is_dir() {
            continue;
        }
        for file in fs::read_dir(&release).unwrap() {
            let file = file.unwrap().path();
            if file.extension().is_none_or(|e| e != "sqlite3") {
                continue;
            }
            let database = Database::new();
            fs::copy(&file, database.path()).unwrap();
            let before = event_count(&database.connection());
            let store = Store::open(&database.path())
                .unwrap_or_else(|e| panic!("{} no longer opens: {e}", file.display()));
            let state = store.state();
            assert!(
                !state.workspaces.is_empty() && !state.sessions.is_empty(),
                "{}: its history was not replayed",
                file.display()
            );
            assert!(before > 0);
            opened += 1;
        }
    }
    assert!(opened >= 2, "the golden stores are missing");
}

#[test]
fn a_check_command_recorded_before_redaction_is_redacted_when_shown() {
    let database = Database::new();
    let token = format!("ghp_{}", "aB3".repeat(12));
    let record = Command::RecordCheck {
        workspace_id: "workspace".into(),
        name: "deploy".into(),
        command: format!("./deploy-test --token {token}"),
        tree_oid: "a".repeat(40),
        head_oid: None,
        inputs: Vec::new(),
        result: langspan_core::CheckResult::Passed {},
        duration_ms: 5,
        symbol_findings: None,
        report: None,
        tests: None,
        rerun: None,
        writes_before: None,
        command_id: None,
    };
    {
        // As an earlier build stored it: the store itself does not redact.
        let mut store = Store::open(&database.path()).unwrap();
        register(&mut store);
        execute(&mut store, "old-check", record.clone(), 1);
    }
    let stored: String = database
        .connection()
        .query_row(
            "SELECT event_json FROM events
             WHERE json_extract(event_json, '$.type') = 'check_recorded'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.contains(&token), "the fixture holds the old form");

    let mut store = Store::open(&database.path()).unwrap();
    let status = Command::CheckStatus {
        workspace_id: "workspace".into(),
        tree_oid: None,
    };
    let events = Command::Events {
        after: 0,
        limit: 1000,
    };
    let replayed = store.replay("old-check", &record, 2).unwrap().unwrap();
    let shown = [
        serde_json::to_string(&execute(&mut store, "read", status, 2)).unwrap(),
        serde_json::to_string(&execute(&mut store, "read", events, 2)).unwrap(),
        serde_json::to_string(&execute(&mut store, "read", Command::Dashboard, 2)).unwrap(),
        // A retry of the old request answers from its stored receipt.
        serde_json::to_string(&replayed.outcome).unwrap(),
    ];
    for shown in &shown {
        assert!(!shown.contains(&token), "{shown}");
    }
    assert!(
        shown[0].contains("./deploy-test --token [redacted]"),
        "the program is still named: {}",
        shown[0]
    );
}

fn old_check(command: String) -> Command {
    Command::RecordCheck {
        workspace_id: "workspace".into(),
        name: "deploy".into(),
        command,
        tree_oid: "a".repeat(40),
        head_oid: None,
        inputs: Vec::new(),
        result: langspan_core::CheckResult::Passed {},
        duration_ms: 5,
        symbol_findings: None,
        report: None,
        tests: None,
        rerun: None,
        writes_before: None,
        command_id: None,
    }
}

/// A command an earlier build accepted, 511 of the 512 bytes allowed, that
/// unbounded redaction lengthens to 520.
fn near_the_limit() -> String {
    let command = format!("{} --token x", "a".repeat(501));
    assert_eq!(command.len(), 511);
    command
}

#[test]
fn a_check_command_near_the_limit_recorded_before_redaction_still_opens() {
    let database = Database::new();
    {
        // As an earlier build stored it: the store itself does not redact.
        let mut store = Store::open(&database.path()).unwrap();
        register(&mut store);
        execute(&mut store, "old-check", old_check(near_the_limit()), 1);
    }
    let mut store = Store::open(&database.path()).expect("an older store still opens");
    let status = Command::CheckStatus {
        workspace_id: "workspace".into(),
        tree_oid: None,
    };
    let shown = serde_json::to_string(&execute(&mut store, "read", status, 2)).unwrap();
    assert!(!shown.contains("--token x"), "{shown}");
    assert!(shown.contains("[redacted]"), "{shown}");
}

#[test]
fn a_check_recorded_before_redaction_is_answered_when_retried_after_upgrade() {
    for command in [
        format!("./deploy-test --token ghp_{}", "aB3".repeat(12)),
        near_the_limit(),
    ] {
        let database = Database::new();
        let record = old_check(command);
        let first = {
            let mut store = Store::open(&database.path()).unwrap();
            register(&mut store);
            execute(&mut store, "old-check", record.clone(), 1)
        };
        let Outcome::CheckRecorded { check } = first else {
            panic!("{first:?}")
        };
        // The daemon redacts every request on receipt, so the identical
        // retry reaches the store in its redacted form.
        let mut retry = record.clone();
        langspan::hook::redact_command(&mut retry);
        let mut store = Store::open(&database.path()).unwrap();
        let replayed = store.replay("old-check", &retry, 2).unwrap().unwrap();
        let Outcome::CheckRecorded { check: answered } = &replayed.outcome else {
            panic!(
                "the retry is answered from its record: {:?}",
                replayed.outcome
            )
        };
        assert_eq!(answered.check_id, check.check_id);
        assert!(!answered.command.contains("--token x"), "{answered:?}");
        assert!(!answered.command.contains("ghp_"), "{answered:?}");
    }
}

/// A long-running daemon's generation holds more events than `today` reads:
/// the summary still finds the generation's clock anchor, behind the newest
/// `TODAY_EVENTS`, and dates what it reads by it.
#[test]
fn today_finds_the_anchor_behind_a_hundred_thousand_events_of_one_generation() {
    use langspan_core::{Event, TODAY_EVENTS};
    const EVENTS: usize = 100_000;
    let database = Database::new();
    let mut store = Store::open(&database.path()).unwrap();
    register(&mut store);
    let generation = store.state().generation;
    let mut connection = database.connection();
    let anchor: String = connection
        .query_row(
            "SELECT event_json FROM events WHERE event_json LIKE '%clock_anchored%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let Event::ClockAnchored { anchor } = serde_json::from_str::<Event>(&anchor).unwrap() else {
        panic!("not an anchor: {anchor}");
    };
    let transaction = connection.transaction().unwrap();
    {
        let mut insert = transaction
            .prepare("INSERT INTO events (event_json) VALUES (?1)")
            .unwrap();
        for index in 0..EVENTS {
            let event = Event::SessionPrompted {
                session_id: "session".into(),
                summary: "work on the parser".into(),
                line_digests: Vec::new(),
                at_ms: anchor.clock_ms + index as u64,
                generation,
            };
            insert
                .execute([serde_json::to_string(&event).unwrap()])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    let started = std::time::Instant::now();
    let Outcome::Today { today } = store
        .today(
            Some("workspace"),
            anchor.wall_ms,
            anchor.wall_ms + EVENTS as u64,
            anchor.clock_ms + EVENTS as u64,
        )
        .unwrap()
    else {
        panic!("not a summary");
    };
    eprintln!(
        "today over {EVENTS} events of one generation: {:?}",
        started.elapsed()
    );
    assert_eq!(today.undated, 0, "the anchor was found: {today:?}");
    assert_eq!(today.sessions.len(), 1, "{today:?}");
    assert_eq!(today.sessions[0].prompts as usize, TODAY_EVENTS);
    assert!(today.truncated, "older prompts of the window were not read");
}
