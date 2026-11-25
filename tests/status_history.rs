#![cfg(unix)]
//! `status` stays within one frame however much history the coordinator
//! holds, and `status --all` pages through every record of it.

use langspan::{
    client,
    protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION, Request, Response},
    store::Store,
};
use langspan_core::{Command, Event, Lease, LeaseStatus, Outcome, State, StatusRecord};
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
            },
        );
        assert!(frame_bytes(&page) < MAX_FRAME_BYTES);
        let Outcome::StatusRecords { records, next } = page.outcome else {
            panic!("status page failed: {:?}", page.outcome);
        };
        assert!(!records.is_empty() || next.is_none());
        for record in records {
            record.apply(&mut whole);
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

#[test]
fn a_malformed_status_cursor_is_refused_with_a_reason() {
    let mut f = Fixture::new();
    f.fill();
    f.start();
    let response = f.call(
        "read",
        Command::StatusRecords {
            after: Some("no-such-kind:0:x".into()),
        },
    );
    let Outcome::Error { code, message } = response.outcome else {
        panic!("a bad cursor was accepted: {:?}", response.outcome);
    };
    assert_eq!(code, "invalid_cursor");
    assert!(message.contains("--after"), "{message}");
}
