#![cfg(unix)]

use langspan::{observer, store::Store};
use langspan_core::{
    Command, DiskObservation, Event, FileKind, FileState, Outcome, RecordedEvent, ScanSummary,
};
use rusqlite::{Connection, OptionalExtension};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    database: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "langspan-store-scan-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("checkout");
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        let database = base.join("state.sqlite");
        Self {
            base,
            root,
            database,
        }
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        fs::write(self.root.join(path), bytes).unwrap();
    }

    fn commit(&self) {
        git(&self.root, &["add", "--all"]);
        git(
            &self.root,
            &[
                "-c",
                "user.name=Store Scan Test",
                "-c",
                "user.email=store-scan@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "--message",
                "synthetic fixture",
            ],
        );
    }

    fn store(&self) -> Store {
        let mut store = Store::open(&self.database).unwrap();
        let command = Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: self.root.to_str().unwrap().into(),
        };
        let execution = store
            .execute("register-work", &command, &command, 0)
            .unwrap();
        assert!(matches!(execution.outcome, Outcome::Workspace { .. }));
        store
    }

    fn scan(&self) -> observer::Snapshot {
        observer::scan(&self.root, Instant::now() + Duration::from_secs(10)).unwrap()
    }

    fn connection(&self) -> Connection {
        Connection::open(&self.database).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = ProcessCommand::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn changes(store: &mut Store, after: u64) -> (Vec<RecordedEvent>, Option<ScanSummary>) {
    let command = Command::Changes {
        workspace_id: "work".into(),
        after,
        limit: 1000,
    };
    let execution = store
        .execute("read-changes", &command, &command, 100)
        .unwrap();
    let Outcome::Changes { events, scan } = execution.outcome else {
        panic!("unexpected changes response: {:?}", execution.outcome);
    };
    (events, scan)
}

fn disk_events(events: &[RecordedEvent]) -> Vec<&DiskObservation> {
    events
        .iter()
        .filter_map(|record| match &record.event {
            Event::DiskObserved { observation } => Some(observation),
            _ => None,
        })
        .collect()
}

fn event_rows(connection: &Connection) -> Vec<(i64, String)> {
    let mut statement = connection
        .prepare("SELECT seq,event_json FROM events ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn projection(connection: &Connection) -> Option<(String, String)> {
    connection
        .query_row(
            "SELECT snapshot_json,summary_json FROM git_snapshots WHERE workspace_id='work'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
}

#[test]
fn real_initial_dirty_files_have_unknown_before_but_later_new_paths_were_missing() {
    let fixture = Fixture::new();
    fixture.write("clean.txt", b"clean");
    fixture.write("dirty.txt", b"before");
    fixture.write("deleted.txt", b"remove");
    fixture.commit();
    fixture.write("dirty.txt", b"after");
    fixture.write("initial-untracked.txt", b"untracked");
    fs::remove_file(fixture.root.join("deleted.txt")).unwrap();
    let mut store = fixture.store();
    assert_eq!(store.record_scan("work", &fixture.scan(), 1).unwrap(), 5);
    let (initial, summary) = changes(&mut store, 0);
    let disk = disk_events(&initial);
    assert_eq!(
        disk.iter()
            .map(|event| event.path.as_str())
            .collect::<Vec<_>>(),
        ["deleted.txt", "dirty.txt", "initial-untracked.txt"]
    );
    assert!(
        disk.iter()
            .all(|event| event.before.is_none() && event.related_operation_ids.is_empty())
    );
    assert_eq!(disk[0].after, FileState::Missing);
    assert_eq!(summary.unwrap().file_count, 4);

    fixture.write("later-created.txt", b"new after baseline");
    let cursor = initial.last().unwrap().seq;
    assert_eq!(store.record_scan("work", &fixture.scan(), 2).unwrap(), 1);
    let (later, _) = changes(&mut store, cursor);
    let disk = disk_events(&later);
    assert_eq!(disk.len(), 1);
    assert_eq!(disk[0].path, "later-created.txt");
    assert_eq!(disk[0].before, Some(FileState::Missing));
    assert!(matches!(
        disk[0].after,
        FileState::Present {
            kind: FileKind::Regular,
            ..
        }
    ));
}

#[test]
fn real_type_changes_and_deletions_survive_restart_without_duplicate_facts() {
    let fixture = Fixture::new();
    fixture.write("file", b"target");
    fixture.commit();
    let mut store = fixture.store();
    store.record_scan("work", &fixture.scan(), 1).unwrap();
    fs::remove_file(fixture.root.join("file")).unwrap();
    symlink("target", fixture.root.join("file")).unwrap();
    assert_eq!(store.record_scan("work", &fixture.scan(), 2).unwrap(), 1);
    let (events, _) = changes(&mut store, 0);
    let disk = disk_events(&events);
    assert_eq!(disk.len(), 1);
    let Some(FileState::Present {
        digest: before_digest,
        kind: FileKind::Regular,
    }) = &disk[0].before
    else {
        panic!("missing regular before state");
    };
    let FileState::Present {
        digest: after_digest,
        kind: FileKind::Symlink,
    } = &disk[0].after
    else {
        panic!("missing symlink after state");
    };
    assert_eq!(
        before_digest, after_digest,
        "type identity must survive equal raw digests"
    );
    let previous_generation = store.state().generation;
    drop(store);

    let mut reopened = Store::open(&fixture.database).unwrap();
    assert_eq!(reopened.state().generation, previous_generation + 1);
    assert_eq!(reopened.record_scan("work", &fixture.scan(), 3).unwrap(), 0);
    let (events, summary) = changes(&mut reopened, 0);
    assert_eq!(disk_events(&events).len(), 1);
    let summary = summary.unwrap();
    assert_eq!(summary.generation, reopened.state().generation);
    assert_eq!(summary.observed_at_ms, 3);

    fs::remove_file(fixture.root.join("file")).unwrap();
    assert_eq!(reopened.record_scan("work", &fixture.scan(), 4).unwrap(), 1);
    let (events, _) = changes(&mut reopened, 0);
    let disk = disk_events(&events);
    assert_eq!(disk.len(), 2);
    assert!(matches!(
        disk[1].before,
        Some(FileState::Present {
            kind: FileKind::Symlink,
            ..
        })
    ));
    assert_eq!(disk[1].after, FileState::Missing);
    fixture.commit();
    reopened.record_scan("work", &fixture.scan(), 5).unwrap();
    let (events, _) = changes(&mut reopened, 0);
    assert_eq!(
        disk_events(&events).len(),
        2,
        "committing a known deletion must not report it again"
    );
}

#[test]
fn real_branch_creation_deletion_and_detachment_keep_commit_tree_pairs() {
    let fixture = Fixture::new();
    fixture.write("file", b"content");
    fixture.commit();
    let mut store = fixture.store();
    let baseline = fixture.scan();
    store.record_scan("work", &baseline, 1).unwrap();
    let (events, _) = changes(&mut store, 0);
    let mut cursor = events.last().unwrap().seq;
    git(&fixture.root, &["branch", "feature/testing"]);
    assert_eq!(store.record_scan("work", &fixture.scan(), 2).unwrap(), 1);
    let (created, _) = changes(&mut store, cursor);
    let Event::BranchObserved { observation } = &created[0].event else {
        panic!("expected created branch fact");
    };
    assert_eq!(observation.branch_ref, "refs/heads/feature/testing");
    assert_eq!(observation.head_oid, baseline.head);
    assert_eq!(observation.tree_oid, baseline.head_tree);
    cursor = created.last().unwrap().seq;

    git(&fixture.root, &["checkout", "--quiet", "--detach"]);
    assert_eq!(store.record_scan("work", &fixture.scan(), 3).unwrap(), 1);
    let (detached, summary) = changes(&mut store, cursor);
    let Event::BranchObserved { observation } = &detached[0].event else {
        panic!("expected detached HEAD fact");
    };
    assert_eq!(observation.branch_ref, "HEAD");
    assert_eq!(observation.head_oid, baseline.head);
    assert_eq!(observation.tree_oid, baseline.head_tree);
    assert_eq!(summary.unwrap().branch_ref, None);
    cursor = detached.last().unwrap().seq;

    git(&fixture.root, &["branch", "--delete", "feature/testing"]);
    assert_eq!(store.record_scan("work", &fixture.scan(), 4).unwrap(), 1);
    let (deleted, _) = changes(&mut store, cursor);
    let Event::BranchObserved { observation } = &deleted[0].event else {
        panic!("expected deleted branch fact");
    };
    assert_eq!(observation.branch_ref, "refs/heads/feature/testing");
    assert_eq!(observation.head_oid, None);
    assert_eq!(observation.tree_oid, None);
    assert!(disk_events(&changes(&mut store, 0).0).is_empty());
}

#[test]
fn real_scan_failures_roll_back_events_and_projection_and_retry_after_restart() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"before a");
    fixture.write("z.txt", b"before z");
    fixture.commit();
    let mut store = fixture.store();
    store.record_scan("work", &fixture.scan(), 1).unwrap();
    let connection = fixture.connection();
    let events_before = event_rows(&connection);
    let projection_before = projection(&connection);
    let state_before = store.state().clone();
    fixture.write("a.txt", b"after a");
    fixture.write("z.txt", b"after z");
    fixture.write("created.txt", b"created");
    git(&fixture.root, &["branch", "new-branch"]);
    let changed = fixture.scan();

    connection.execute_batch("CREATE TRIGGER reject_partial_scan BEFORE INSERT ON events WHEN json_extract(NEW.event_json,'$.observation.path')='z.txt' BEGIN SELECT RAISE(ABORT,'injected event failure'); END;").unwrap();
    let error = store.record_scan("work", &changed, 2).unwrap_err();
    assert!(error.to_string().contains("injected event failure"));
    assert_eq!(
        event_rows(&connection),
        events_before,
        "an event failure must roll back preceding scan events"
    );
    assert_eq!(
        projection(&connection),
        projection_before,
        "an event failure must retain the complete prior baseline"
    );
    assert_eq!(store.state(), &state_before);
    connection.execute_batch("DROP TRIGGER reject_partial_scan; CREATE TRIGGER reject_scan_projection BEFORE UPDATE ON git_snapshots BEGIN SELECT RAISE(ABORT,'injected projection failure'); END;").unwrap();

    let error = store.record_scan("work", &changed, 3).unwrap_err();
    assert!(error.to_string().contains("injected projection failure"));
    assert_eq!(
        event_rows(&connection),
        events_before,
        "snapshot failure must not leave orphan scan facts"
    );
    assert_eq!(
        projection(&connection),
        projection_before,
        "snapshot failure must not advance the baseline or summary"
    );
    assert_eq!(store.state(), &state_before);
    drop(store);

    let mut reopened = Store::open(&fixture.database).unwrap();
    assert_eq!(projection(&connection), projection_before);
    connection
        .execute_batch("DROP TRIGGER reject_scan_projection")
        .unwrap();
    assert_eq!(
        reopened.record_scan("work", &changed, 4).unwrap(),
        4,
        "the retry must recover all three files and the new branch"
    );
    let (events, summary) = changes(&mut reopened, 0);
    let disk = disk_events(&events);
    assert_eq!(disk.len(), 3);
    let states: BTreeMap<_, _> = disk
        .iter()
        .map(|event| (event.path.as_str(), &event.before))
        .collect();
    assert_eq!(states["created.txt"], &Some(FileState::Missing));
    assert_eq!(summary.unwrap().observed_at_ms, 4);
    assert_eq!(reopened.record_scan("work", &fixture.scan(), 5).unwrap(), 0);
}

#[test]
fn a_clean_git_filename_outside_the_core_grammar_cannot_poison_the_baseline() {
    let fixture = Fixture::new();
    fixture.write("ordinary.txt", b"normal");
    fixture.write(
        r"legal\unix.txt",
        b"legal Git path, unsupported coordination key",
    );
    fixture.commit();
    let mut store = fixture.store();
    let connection = fixture.connection();
    let before = event_rows(&connection);
    let error = store.record_scan("work", &fixture.scan(), 1).unwrap_err();
    assert!(error.to_string().contains("invalid_path"), "{error}");
    assert_eq!(event_rows(&connection), before);
    assert_eq!(
        projection(&connection),
        None,
        "an unsupported clean path must not become a persisted baseline"
    );

    fs::rename(
        fixture.root.join(r"legal\unix.txt"),
        fixture.root.join("supported.txt"),
    )
    .unwrap();
    fixture.commit();
    assert_eq!(store.record_scan("work", &fixture.scan(), 2).unwrap(), 2);
    let (events, summary) = changes(&mut store, 0);
    assert!(
        disk_events(&events).is_empty(),
        "the first accepted clean baseline remains silent"
    );
    assert_eq!(summary.unwrap().file_count, 2);
}

/// Persisted: a partial scan is recorded
/// with its coverage, and what it left out is never a deletion.
#[test]
fn a_partial_scan_is_recorded_with_its_coverage_and_omissions_are_not_deletions() {
    let fixture = Fixture::new();
    for index in 0..8 {
        fixture.write(&format!("f{index}.txt"), format!("file {index}").as_bytes());
    }
    fixture.write("big.bin", &[1; 64]);
    fixture.commit();
    let limits = observer::Limits {
        max_files: 3,
        max_file_bytes: 32,
        ..observer::Limits::default()
    };
    let scan = || {
        observer::scan_with_limits(
            &fixture.root,
            Instant::now() + Duration::from_secs(10),
            limits,
        )
        .unwrap()
    };
    let mut store = fixture.store();
    store.record_scan("work", &scan(), 1).unwrap();
    let (events, summary) = changes(&mut store, 0);
    assert!(
        disk_events(&events).is_empty(),
        "a clean baseline is silent"
    );
    let summary = summary.unwrap();
    assert_eq!(summary.file_count, 3);
    let coverage = summary.coverage.expect("a partial scan says so");
    assert_eq!(coverage.too_large_paths, ["big.bin"]);
    assert_eq!(coverage.omitted, 5);

    // A late file changes: it is versioned first, and the clean files it
    // pushes out of the selection are not reported deleted.
    fixture.write("f7.txt", b"changed");
    store.record_scan("work", &scan(), 2).unwrap();
    let (events, _) = changes(&mut store, 0);
    let disk = disk_events(&events);
    assert_eq!(
        disk.iter().map(|o| o.path.as_str()).collect::<Vec<_>>(),
        ["f7.txt"],
        "{disk:?}"
    );
    assert_eq!(disk[0].before, None, "the previous scan did not version it");
    assert!(matches!(disk[0].after, FileState::Present { .. }));
}

/// `git stash` and `git reset --hard`
/// change the checkout under its sessions; a commit does not.
#[test]
fn stash_and_reset_hard_are_checkout_changes_and_a_commit_is_not() {
    let fixture = Fixture::new();
    for file in ["a.txt", "b.txt"] {
        fixture.write(file, b"one");
    }
    fixture.commit();
    let mut store = fixture.store();
    let checkouts = |store: &Store| {
        store
            .state()
            .checkouts
            .get("work")
            .cloned()
            .unwrap_or_default()
    };
    store.record_scan("work", &fixture.scan(), 1).unwrap();
    for file in ["a.txt", "b.txt"] {
        fixture.write(file, b"two");
    }
    store.record_scan("work", &fixture.scan(), 2).unwrap();
    fixture.commit();
    store.record_scan("work", &fixture.scan(), 3).unwrap();
    assert!(checkouts(&store).is_empty(), "a commit changes no file");

    for file in ["a.txt", "b.txt"] {
        fixture.write(file, b"three");
    }
    store.record_scan("work", &fixture.scan(), 4).unwrap();
    git(
        &fixture.root,
        &[
            "-c",
            "user.name=Store Scan Test",
            "-c",
            "user.email=store-scan@example.invalid",
            "stash",
            "--quiet",
        ],
    );
    store.record_scan("work", &fixture.scan(), 5).unwrap();
    let stash = checkouts(&store);
    assert_eq!(stash.len(), 1, "{stash:?}");
    assert_eq!(stash[0].from_head, stash[0].to_head);
    assert_eq!(stash[0].files, 2);

    git(&fixture.root, &["reset", "--quiet", "--hard", "HEAD~1"]);
    store.record_scan("work", &fixture.scan(), 6).unwrap();
    let reset = checkouts(&store);
    assert_eq!(reset.len(), 2, "{reset:?}");
    assert_ne!(reset[0].from_head, reset[0].to_head);
    assert_eq!(reset[0].from_branch.as_deref(), Some("refs/heads/main"));
    assert_eq!(reset[0].to_branch.as_deref(), Some("refs/heads/main"));
}

/// Issue #129: a Codex session resumed under another path for the same
/// directory runs none of LangSpan's hooks. A real scan that sees a file
/// change no session reported records it once, naming the Codex session; a
/// reported write and a checkout change are not uncoordinated.
#[test]
fn a_real_change_no_session_reported_names_the_registered_codex_session() {
    let fixture = Fixture::new();
    for file in ["a.txt", "b.txt"] {
        fixture.write(file, b"one");
    }
    fixture.commit();
    let mut store = fixture.store();
    let uncoordinated = |store: &Store| {
        store
            .state()
            .uncoordinated
            .get("work")
            .cloned()
            .unwrap_or_default()
    };
    store.record_scan("work", &fixture.scan(), 1).unwrap();
    // No Codex session yet: an unreported change names nobody.
    fixture.write("a.txt", b"by hand");
    store.record_scan("work", &fixture.scan(), 2).unwrap();
    assert!(uncoordinated(&store).is_empty());

    let codex = "codex.0198c0de-0000-7000-8000-000000000001";
    let register = Command::SessionRegister {
        session_id: codex.into(),
        workspace_id: "work".into(),
        parent: None,
        headless: false,
    };
    store.execute("s", &register, &register, 3).unwrap();

    // A write the session reported is coordinated.
    fixture.write("b.txt", b"reported");
    let version = fixture.scan().files["b.txt"].clone();
    let written = Command::ObserveSource {
        session_id: codex.into(),
        files: vec![langspan_core::ObservedFile {
            path: "b.txt".into(),
            state: FileState::Present {
                digest: version.digest.unwrap(),
                kind: FileKind::Regular,
            },
        }],
        origin: langspan_core::SourceOrigin::Write,
    };
    let outcome = store.execute("w", &written, &written, 4).unwrap().outcome;
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    store.record_scan("work", &fixture.scan(), 5).unwrap();
    assert!(uncoordinated(&store).is_empty());

    // An edit with no hook event behind it names the Codex session.
    fixture.write("a.txt", b"no hook ran");
    store.record_scan("work", &fixture.scan(), 6).unwrap();
    let recorded = uncoordinated(&store);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].sessions, [codex]);
    assert_eq!(
        recorded[0]
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["a.txt"]
    );

    // A checkout change explains its own files.
    drop(store);
    let mut store = Store::open(&fixture.database).unwrap();
    store.execute("s2", &register, &register, 7).unwrap();
    store.record_scan("work", &fixture.scan(), 8).unwrap();
    git(&fixture.root, &["checkout", "--quiet", "--", "."]);
    store.record_scan("work", &fixture.scan(), 9).unwrap();
    let generation = store.state().generation;
    assert!(
        uncoordinated(&store)
            .iter()
            .all(|change| change.generation != generation),
        "{:?}",
        uncoordinated(&store)
    );
}

/// Found by Linux CI on PR #30: a session edits a file, the edit lands
/// before any scan sees the file changed, then the edit is committed. The
/// next scan sees HEAD moved and the file changed, as a checkout would look,
/// but the file's new version is the one the session wrote: that is a commit,
/// not the checkout changing under the sessions.
#[test]
fn a_commit_of_a_session_edit_no_scan_saw_is_not_a_checkout_change() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"one");
    fixture.commit();
    let mut store = fixture.store();
    store.record_scan("work", &fixture.scan(), 1).unwrap();
    let register = Command::SessionRegister {
        session_id: "bob".into(),
        workspace_id: "work".into(),
        parent: None,
        headless: false,
    };
    store.execute("s", &register, &register, 2).unwrap();
    fixture.write("a.txt", b"bob's edit");
    let version = fixture.scan().files["a.txt"].clone();
    let written = Command::ObserveSource {
        session_id: "bob".into(),
        files: vec![langspan_core::ObservedFile {
            path: "a.txt".into(),
            state: FileState::Present {
                digest: version.digest.unwrap(),
                kind: langspan_core::FileKind::Regular,
            },
        }],
        origin: langspan_core::SourceOrigin::Write,
    };
    let outcome = store.execute("w", &written, &written, 3).unwrap().outcome;
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    fixture.commit();
    store.record_scan("work", &fixture.scan(), 4).unwrap();
    let checkouts = store
        .state()
        .checkouts
        .get("work")
        .cloned()
        .unwrap_or_default();
    assert!(checkouts.is_empty(), "{checkouts:?}");

    // HEAD moving to a version no session wrote is still a checkout change.
    git(&fixture.root, &["reset", "--quiet", "--hard", "HEAD~1"]);
    store.record_scan("work", &fixture.scan(), 5).unwrap();
    assert_eq!(store.state().checkouts.get("work").map(Vec::len), Some(1));
}
