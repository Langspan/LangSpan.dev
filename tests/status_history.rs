#![cfg(unix)]
//! `status` stays within one frame however much history the coordinator
//! holds, and `status --all` pages through every record of it.

use langspan::{
    client,
    protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION, Request, Response},
    store::Store,
};
use langspan_core::{
    Command, Event, FileKind, FileState, Landing, Lease, LeaseStatus, ObservedFile, Outcome,
    SourceOrigin, State, StatusRecord,
};
use rusqlite::{Connection, params};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SESSIONS: usize = 50;
const PER_SESSION: usize = 200;

struct Fixture {
    root: PathBuf,
    child: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ls-status-history-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        fs::create_dir(root.join("state")).unwrap();
        fs::set_permissions(root.join("state"), fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, child: None }
    }

    fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    fn workspace(&self) -> String {
        fs::canonicalize(self.root.join("workspace"))
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    }

    /// A real store with 10,000 completed admissions across 50 sessions,
    /// written as the events the coordinator itself records, in one
    /// transaction so the fixture is quick to build.
    fn fill(&self) {
        let path = self.state_dir().join("state.sqlite3");
        let generation = {
            let mut store = Store::open(&path).unwrap();
            let register = |store: &mut Store, id: &str, command: Command| {
                let outcome = store.execute(id, &command, &command, 1).unwrap().outcome;
                assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
            };
            register(
                &mut store,
                "workspace",
                Command::WorkspaceRegister {
                    workspace_id: "w".into(),
                    root: self.workspace(),
                },
            );
            for session in 0..SESSIONS {
                let id = format!("session-{session:02}");
                register(
                    &mut store,
                    &format!("register-{id}"),
                    Command::SessionRegister {
                        session_id: id.clone(),
                        workspace_id: "w".into(),
                        parent: None,
                        headless: false,
                    },
                );
            }
            store.state().generation
        };
        let mut connection = Connection::open(&path).unwrap();
        let transaction = connection.transaction().unwrap();
        for session in 0..SESSIONS {
            for edit in 0..PER_SESSION {
                let lease_id = format!("edit-{session:02}-{edit:04}");
                let granted = Event::LeaseGranted {
                    lease: Lease {
                        lease_id: lease_id.clone(),
                        session_id: format!("session-{session:02}"),
                        workspace_id: "w".into(),
                        paths: vec![format!("src/module_{session:02}/file_{edit:04}.rs")],
                        generation,
                        expires_at_ms: 4_000_000_000_000,
                        status: LeaseStatus::Active,
                        operation: None,
                        base_unknown: Vec::new(),
                        regions: Vec::new(),
                    },
                };
                for event in [granted, Event::LeaseCompleted { lease_id }] {
                    transaction
                        .execute(
                            "INSERT INTO events (event_json) VALUES (?1)",
                            params![serde_json::to_string(&event).unwrap()],
                        )
                        .unwrap();
                }
            }
        }
        transaction.commit().unwrap();
        drop(connection);
        // The daemon accepts only owner-only state files.
        for entry in fs::read_dir(self.state_dir()).unwrap() {
            fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn start(&mut self) {
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::call(&self.state_dir(), &request("ready", Command::Status)).is_err() {
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                let output = self.child.take().unwrap().wait_with_output().unwrap();
                panic!(
                    "daemon exited before readiness: {status}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn call(&self, id: &str, command: Command) -> Response {
        client::call(&self.state_dir(), &request(id, command)).unwrap()
    }

    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(args)
            .arg("--state-dir")
            .arg(self.state_dir())
            .output()
            .unwrap();
        assert!(
            output.stdout.len() < MAX_FRAME_BYTES,
            "one CLI answer is larger than a frame"
        );
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn request(id: &str, command: Command) -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: id.into(),
        command,
    }
}

fn frame_bytes(response: &Response) -> usize {
    serde_json::to_vec(response).unwrap().len() + 1
}

/// Two of the old sessions come back and each holds one open admission.
fn open_two(f: &Fixture) {
    for session in ["session-03", "session-41"] {
        assert!(matches!(
            f.call(
                &format!("again-{session}"),
                Command::SessionRegister {
                    session_id: session.into(),
                    workspace_id: "w".into(),
                    parent: None,
                    headless: false,
                },
            )
            .outcome,
            Outcome::Session { .. }
        ));
        let admitted = f.call(
            &format!("open-{session}"),
            Command::Admit {
                session_id: session.into(),
                paths: vec![format!("src/open_{session}.rs")],
                ttl_ms: 60_000,
            },
        );
        assert!(
            matches!(admitted.outcome, Outcome::Admitted { .. }),
            "{:?}",
            admitted.outcome
        );
    }
}

#[test]
fn status_answers_within_a_frame_after_ten_thousand_admissions_and_all_pages_through_them() {
    let mut f = Fixture::new();
    f.fill();
    f.start();
    open_two(&f);

    // The default answer: what is open now, history only as counts.
    let response = f.call("read", Command::Status);
    assert!(frame_bytes(&response) < MAX_FRAME_BYTES);
    let Outcome::Status { state, summary } = &response.outcome else {
        panic!("status failed: {:?}", response.outcome);
    };
    let summary = summary
        .as_ref()
        .expect("a bounded status carries its summary");
    let mut open: Vec<_> = state.leases.keys().cloned().collect();
    open.sort();
    assert_eq!(open, ["open-session-03", "open-session-41"]);
    assert!(
        state
            .leases
            .values()
            .all(|lease| lease.status == LeaseStatus::Active)
    );
    assert!(state.workspaces.contains_key("w"));
    assert!(state.sessions.contains_key("session-03"));
    assert!(state.sessions.contains_key("session-41"));
    let counted: u64 = summary.closed_admissions.values().sum::<u64>() + summary.other_closed;
    assert_eq!(counted, (SESSIONS * PER_SESSION) as u64);
    assert_eq!(summary.closed_admissions.get("session-03"), Some(&200));

    // The same bounded answer through the command line, as text and JSON.
    let (code, text) = f.cli(&["status"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("Admission open-session-03"), "{text}");
    assert!(!text.contains("edit-00-0000"), "{text}");
    assert!(text.contains("10000 closed admission(s)"), "{text}");
    let (code, json) = f.cli(&["status", "--json"]);
    assert_eq!(code, 0, "{json}");
    assert!(!json.contains("edit-00-0000"));

    // The dashboard reads the same store and stays within a frame too.
    let dashboard = f.call("read", Command::Dashboard);
    assert!(matches!(dashboard.outcome, Outcome::Dashboard { .. }));
    assert!(frame_bytes(&dashboard) < MAX_FRAME_BYTES);

    // Every record, page by page, through the socket.
    let mut whole = State::default();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = f.call(
            "read",
            Command::StatusRecords {
                after: after.clone(),
                page_bytes: None,
            },
        );
        assert!(frame_bytes(&page) < MAX_FRAME_BYTES);
        let Outcome::StatusRecords { records, next } = page.outcome else {
            panic!("status page failed: {:?}", page.outcome);
        };
        assert!(!records.is_empty() || next.is_none());
        for record in records {
            record.apply(&mut whole).unwrap();
        }
        pages += 1;
        assert!(pages < 1000, "paging does not end");
        match next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    assert!(pages > 1, "10,000 admissions cannot fit one page");
    assert_eq!(whole.leases.len(), SESSIONS * PER_SESSION + 2);
    assert_eq!(
        whole
            .leases
            .values()
            .filter(|lease| lease.status == LeaseStatus::Completed)
            .count(),
        SESSIONS * PER_SESSION
    );
    assert_eq!(whole.sessions.len(), SESSIONS);
    assert!(whole.workspaces.contains_key("w"));

    // And through `status --all --json`, following the cursor.
    let mut leases = 0;
    let mut after: Option<String> = None;
    loop {
        let mut args = vec!["status", "--all", "--json"];
        if let Some(cursor) = &after {
            args.extend(["--after", cursor.as_str()]);
        }
        let (code, json) = f.cli(&args);
        assert_eq!(code, 0, "{json}");
        let page: Response = serde_json::from_str(&json).unwrap();
        let Outcome::StatusRecords { records, next } = page.outcome else {
            panic!("status --all failed: {:?}", page.outcome);
        };
        leases += records
            .iter()
            .filter(|record| matches!(record, StatusRecord::Lease { .. }))
            .count();
        match next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    assert_eq!(leases, SESSIONS * PER_SESSION + 2);
}

/// The largest automatic check the coordinator accepts: 64 arguments of
/// 1,024 bytes, each full of characters JSON escapes, so that its record is
/// about twice a frame before it is escaped again inside a page.
fn largest_auto_check() -> Vec<String> {
    (0..64)
        .map(|index| format!("{index:02}{}", "\"\\".repeat(511)))
        .collect()
}

impl Fixture {
    /// One workspace, one session and two state entries that are each larger
    /// than a frame: an automatic check at its limits, and the session's 128
    /// known file versions with paths near their 512-byte limit.
    fn fill_large(&self) {
        let path = self.state_dir().join("state.sqlite3");
        let generation = {
            let mut store = Store::open(&path).unwrap();
            for (id, command) in [
                (
                    "workspace",
                    Command::WorkspaceRegister {
                        workspace_id: "w".into(),
                        root: self.workspace(),
                    },
                ),
                (
                    "register",
                    Command::SessionRegister {
                        session_id: "big".into(),
                        workspace_id: "w".into(),
                        parent: None,
                        headless: false,
                    },
                ),
                (
                    "auto-check",
                    Command::AutoCheckEnable {
                        workspace_id: "w".into(),
                        name: "unit".into(),
                        argv: largest_auto_check(),
                        timeout_s: 60,
                        on_interface_change: None,
                    },
                ),
            ] {
                let outcome = store.execute(id, &command, &command, 1).unwrap().outcome;
                assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
            }
            store.state().generation
        };
        let mut connection = Connection::open(&path).unwrap();
        let transaction = connection.transaction().unwrap();
        for batch in 0..2 {
            let mut files: Vec<ObservedFile> = (0..64)
                .map(|index| ObservedFile {
                    path: format!("{}{batch}{index:02}.rs", "abcdefghi/".repeat(49)),
                    state: FileState::Present {
                        digest: format!("{:064}", batch * 64 + index),
                        kind: FileKind::Regular,
                    },
                })
                .collect();
            files.sort_by(|a, b| a.path.cmp(&b.path));
            let observed = Event::SourceObserved {
                session_id: "big".into(),
                files,
                origin: SourceOrigin::Read,
                at_ms: 2,
                generation,
            };
            transaction
                .execute(
                    "INSERT INTO events (event_json) VALUES (?1)",
                    params![serde_json::to_string(&observed).unwrap()],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        drop(connection);
        for entry in fs::read_dir(self.state_dir()).unwrap() {
            fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// So many sessions registered in this generation that the bounded view
    /// leaves out every history it can, `landings` and `workspace_git`
    /// included, before it leaves out idle sessions.
    fn crowd(&self) {
        for session in 0..800 {
            let id = format!("crowd-{session:04}");
            let outcome = self
                .call(
                    &format!("register-{id}"),
                    Command::SessionRegister {
                        session_id: id,
                        workspace_id: "w".into(),
                        parent: None,
                        headless: false,
                    },
                )
                .outcome;
            assert!(matches!(outcome, Outcome::Session { .. }), "{outcome:?}");
        }
    }
}

#[test]
fn a_state_entry_larger_than_a_frame_is_read_whole_in_parts() {
    let mut f = Fixture::new();
    f.fill_large();
    f.start();

    // Every page of the records stays within a frame, however large one
    // entry is.
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = f.call(
            "read",
            Command::StatusRecords {
                after: after.clone(),
                page_bytes: None,
            },
        );
        assert!(frame_bytes(&page) < MAX_FRAME_BYTES);
        let Outcome::StatusRecords { next, .. } = page.outcome else {
            panic!("status page failed: {:?}", page.outcome);
        };
        pages += 1;
        assert!(pages < 100, "paging does not end");
        match next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }

    // And the pages put back exactly what the coordinator holds.
    let whole = client::full_state(&f.state_dir()).unwrap();
    assert_eq!(whole.auto_checks["w"].argv, largest_auto_check());
    let known = &whole.activity["big"].known;
    assert_eq!(known.len(), 128);
    assert!(known.iter().all(|version| version.path.len() > 490));

    // The bounded view fits a frame too, and says what it left out.
    let response = f.call("read", Command::Status);
    assert!(frame_bytes(&response) < MAX_FRAME_BYTES);
    let Outcome::Status { summary, .. } = &response.outcome else {
        panic!("status failed: {:?}", response.outcome);
    };
    let omitted = &summary.as_ref().unwrap().omitted;
    assert!(
        omitted.iter().any(|name| name == "auto_checks"),
        "{omitted:?}"
    );
}

#[test]
fn a_record_that_changes_while_its_parts_are_read_is_read_again_whole() {
    let f = Fixture::new();
    f.fill_large();
    let mut store = Store::open(&f.state_dir().join("state.sqlite3")).unwrap();
    let mut changed = largest_auto_check();
    changed[0] = "changed".into();
    let mut pages = 0;
    let whole = client::read_all(|command| {
        pages += 1;
        // After the first part of the automatic check, the owner changes it.
        if pages == 3 {
            let change = Command::AutoCheckEnable {
                workspace_id: "w".into(),
                name: "unit".into(),
                argv: changed.clone(),
                timeout_s: 60,
                on_interface_change: None,
            };
            store.execute("change", &change, &change, 3).unwrap();
        }
        let page = store
            .execute("read", &command, &command, 3)
            .unwrap()
            .outcome;
        if let Outcome::StatusRecords { records, .. } = &page {
            let size = serde_json::to_vec(records).unwrap().len();
            assert!(size < MAX_FRAME_BYTES, "a page of {size} bytes");
        }
        Ok(page)
    })
    .unwrap();
    assert!(pages > 3, "the check was read in one page");
    assert_eq!(whole.auto_checks["w"].argv, changed);
    assert_eq!(whole.activity["big"].known.len(), 128);
}

/// A reader may ask for smaller pages (the MCP adapter does, since its reply
/// carries each page twice). Even a page size of nothing is raised to the
/// floor, so every page moves the cursor on and the pages still put back
/// exactly the state a full-size read gives.
#[test]
fn smaller_pages_asked_for_still_read_every_record() {
    let f = Fixture::new();
    f.fill_large();
    let mut store = Store::open(&f.state_dir().join("state.sqlite3")).unwrap();
    let full = client::read_all(|command| {
        Ok(store
            .execute("read", &command, &command, 3)
            .unwrap()
            .outcome)
    })
    .unwrap();
    let (mut pages, mut small_pages) = (0, 0);
    let small = client::read_all(|mut command| {
        if let Command::StatusRecords { page_bytes, .. } = &mut command {
            *page_bytes = Some(0);
        }
        let page = store
            .execute("read", &command, &command, 3)
            .unwrap()
            .outcome;
        if let Outcome::StatusRecords { records, .. } = &page {
            small_pages += 1;
            let size = serde_json::to_vec(records).unwrap().len();
            assert!(
                size <= 2 * langspan::store::MIN_PAGE_BYTES,
                "a page of {size} bytes"
            );
            assert!(!records.is_empty(), "a page without a record");
        }
        Ok(page)
    })
    .unwrap();
    client::read_all(|command| {
        pages += 1;
        Ok(store
            .execute("read", &command, &command, 3)
            .unwrap()
            .outcome)
    })
    .unwrap();
    assert!(
        small_pages > pages,
        "{small_pages} small pages, {pages} full"
    );
    assert_eq!(small, full);
}

#[test]
fn a_retried_landing_is_recognised_when_status_leaves_landings_out() {
    let mut f = Fixture::new();
    f.fill();
    // The landing this request already made, recorded as the coordinator
    // records one.
    let path = f.state_dir().join("state.sqlite3");
    let landed = Event::Landed {
        landing: Landing {
            landing_id: "land-done".into(),
            workspace_id: "w".into(),
            onto: "main".into(),
            commit_oid: "a".repeat(40),
            tree_oid: "b".repeat(40),
            inputs: vec!["c".repeat(40)],
            check_id: "check-done".into(),
            rebased: Vec::new(),
            adapted: Vec::new(),
        },
    };
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO events (event_json) VALUES (?1)",
            params![serde_json::to_string(&landed).unwrap()],
        )
        .unwrap();
    f.start();
    f.crowd();
    let Outcome::Status { summary, .. } = f.call("read", Command::Status).outcome else {
        panic!("status failed");
    };
    assert!(
        summary
            .unwrap()
            .omitted
            .iter()
            .any(|name| name == "landings"),
        "the fixture must make status leave landings out"
    );

    // The workspace is not a Git repository: running the landing again
    // would fail at its first fetch. A retry reports the landing it made.
    let (code, json) = f.cli(&[
        "land",
        "w",
        "--onto",
        "main",
        "--name",
        "unit",
        "feature",
        "--apply",
        "--request-id",
        "land-done",
        "--json",
    ]);
    assert_eq!(code, 0, "{json}");
    let report: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["replayed"], true, "{report}");
    assert_eq!(report["landing"]["landing_id"], "land-done", "{report}");
}

#[test]
fn doctor_reports_a_scanned_workspace_when_status_leaves_its_git_out() {
    let mut f = Fixture::new();
    let workspace = f.workspace();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ],
    ] {
        let status = Process::new("git")
            .args(args)
            .current_dir(&workspace)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    f.fill();
    f.start();
    // The coordinator's own scan finds the repository and branch.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let Outcome::Status { state, .. } = f.call("read", Command::Status).outcome else {
            panic!("status failed");
        };
        if state.workspace_git.contains_key("w") {
            break;
        }
        assert!(Instant::now() < deadline, "no scan completed: {state:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    f.crowd();
    let Outcome::Status { summary, .. } = f.call("read", Command::Status).outcome else {
        panic!("status failed");
    };
    assert!(
        summary
            .unwrap()
            .omitted
            .iter()
            .any(|name| name == "workspace_git"),
        "the fixture must make status leave workspace_git out"
    );
    let home = f.root.join("home");
    fs::create_dir(&home).unwrap();
    let (_, text) = f.cli(&["doctor", "--home", home.to_str().unwrap()]);
    assert!(
        text.contains("Workspace w: scanned; repository and branch known"),
        "{text}"
    );
    assert!(!text.contains("no scan has completed yet"), "{text}");
}

#[test]
fn a_malformed_status_cursor_is_refused_with_a_reason() {
    let mut f = Fixture::new();
    f.fill();
    f.start();
    let response = f.call(
        "read",
        Command::StatusRecords {
            after: Some("no-such-kind:0:x".into()),
            page_bytes: None,
        },
    );
    let Outcome::Error { code, message } = response.outcome else {
        panic!("a bad cursor was accepted: {:?}", response.outcome);
    };
    assert_eq!(code, "invalid_cursor");
    assert!(message.contains("--after"), "{message}");
}
