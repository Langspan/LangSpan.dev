#![cfg(unix)]

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{Command, Outcome};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    child: Option<Child>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ls-daemon-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        Self { root, child: None }
    }
    fn state_dir(&self) -> PathBuf {
        self.root.join("state")
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
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if client::call(
                &self.state_dir(),
                &Request {
                    version: PROTOCOL_VERSION,
                    request_id: "ready".into(),
                    command: Command::Status,
                },
            )
            .is_ok()
            {
                break;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                let output = self.child.take().unwrap().wait_with_output().unwrap();
                panic!(
                    "daemon exited before readiness: {status}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn call(&self, id: &str, command: Command) -> langspan::protocol::Response {
        client::call(
            &self.state_dir(),
            &Request {
                version: PROTOCOL_VERSION,
                request_id: id.into(),
                command,
            },
        )
        .unwrap()
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn setup(&self) {
        assert!(matches!(
            self.call(
                "workspace",
                Command::WorkspaceRegister {
                    workspace_id: "w".into(),
                    root: self.root.join("workspace").to_str().unwrap().into()
                }
            )
            .outcome,
            Outcome::Workspace { .. }
        ));
        for id in ["alice", "bob"] {
            assert!(matches!(
                self.call(
                    id,
                    Command::SessionRegister {
                        session_id: id.into(),
                        workspace_id: "w".into(),
                        parent: None,
                        headless: false,
                    }
                )
                .outcome,
                Outcome::Session { .. }
            ));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn daemon_coordinates_two_sessions_and_recovers_original_grants() {
    let mut f = Fixture::new();
    f.start();
    f.setup();
    assert!(matches!(
        f.call(
            "intent",
            Command::IntentSet {
                session_id: "alice".into(),
                paths: vec!["api.rs".into()],
                summary: "Change API".into()
            }
        )
        .outcome,
        Outcome::Intent { .. }
    ));
    let admission = Command::Admit {
        session_id: "alice".into(),
        paths: vec!["api.rs".into()],
        ttl_ms: 60_000,
    };
    let first = f.call("edit-a", admission.clone());
    assert_eq!(first.lease_valid, Some(true));
    let conflict = f.call(
        "edit-b",
        Command::Admit {
            session_id: "bob".into(),
            paths: vec!["api.rs".into(), "other.rs".into()],
            ttl_ms: 60_000,
        },
    );
    assert!(matches!(conflict.outcome, Outcome::Conflict { .. }));
    assert!(
        matches!(
            f.call(
                "edit-c",
                Command::Admit {
                    session_id: "bob".into(),
                    paths: vec!["other.rs".into()],
                    ttl_ms: 60_000
                }
            )
            .outcome,
            Outcome::Admitted { .. }
        ),
        "denied whole set must not hold its free member"
    );
    let retry = f.call("edit-a", admission.clone());
    assert!(retry.replayed);
    assert_eq!(retry.outcome, first.outcome);
    let generation = first.generation;
    f.stop();
    f.start();
    let retry = f.call("edit-a", admission);
    assert!(retry.replayed);
    assert_eq!(retry.outcome, first.outcome);
    assert_eq!(retry.lease_valid, Some(false));
    assert!(retry.generation > generation);
    assert!(matches!(
        f.call(
            "old-complete",
            Command::Complete {
                session_id: "alice".into(),
                lease_id: "edit-a".into()
            }
        )
        .outcome,
        Outcome::Error { .. }
    ));
}

#[test]
fn daemon_rejects_path_escape_and_unifies_symlink_aliases() {
    let mut f = Fixture::new();
    fs::write(f.root.join("workspace/real.rs"), "fn main() {}\n").unwrap();
    std::os::unix::fs::symlink("real.rs", f.root.join("workspace/alias.rs")).unwrap();
    std::os::unix::fs::symlink(&f.root, f.root.join("workspace/outside")).unwrap();
    f.start();
    f.setup();
    assert!(matches!(
        f.call(
            "alias",
            Command::Admit {
                session_id: "alice".into(),
                paths: vec!["alias.rs".into()],
                ttl_ms: 60_000
            }
        )
        .outcome,
        Outcome::Admitted { .. }
    ));
    assert!(matches!(
        f.call(
            "real",
            Command::Admit {
                session_id: "bob".into(),
                paths: vec!["real.rs".into()],
                ttl_ms: 60_000
            }
        )
        .outcome,
        Outcome::Conflict { .. }
    ));
    for (n, path) in ["../escape.rs", "outside/new.rs", "/absolute.rs"]
        .iter()
        .enumerate()
    {
        assert!(matches!(
            f.call(
                &format!("escape-{n}"),
                Command::Admit {
                    session_id: "bob".into(),
                    paths: vec![(*path).into()],
                    ttl_ms: 1000
                }
            )
            .outcome,
            Outcome::Error { .. }
        ));
    }
}

#[test]
fn second_daemon_cannot_replace_live_socket() {
    let mut f = Fixture::new();
    f.start();
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["daemon", "--state-dir"])
        .arg(f.state_dir())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another daemon"));
    assert!(matches!(
        f.call("still-live", Command::Status).outcome,
        Outcome::Status { .. }
    ));
    let mode = fs::metadata(f.state_dir()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    assert_eq!(
        fs::metadata(f.state_dir().join("daemon.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(f.state_dir().join("state.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn parallel_clients_get_one_grant_for_the_same_file() {
    let mut f = Fixture::new();
    f.start();
    f.setup();
    let requests = (0..16)
        .map(|n| {
            let directory = f.state_dir();
            std::thread::spawn(move || {
                client::call(
                    &directory,
                    &Request {
                        version: PROTOCOL_VERSION,
                        request_id: format!("parallel-{n}"),
                        command: Command::Admit {
                            session_id: if n % 2 == 0 {
                                "alice".into()
                            } else {
                                "bob".into()
                            },
                            paths: vec!["shared.rs".into()],
                            ttl_ms: 60_000,
                        },
                    },
                )
                .unwrap()
                .outcome
            })
        })
        .collect::<Vec<_>>();
    let outcomes = requests
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|v| matches!(v, Outcome::Admitted { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|v| matches!(v, Outcome::Conflict { .. }))
            .count(),
        15
    );
}

#[test]
fn cli_uses_the_same_daemon_and_exposes_conflict_exit_codes() {
    let mut f = Fixture::new();
    f.start();
    f.setup();
    let cli = |args: &[&str]| {
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(f.state_dir())
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    };
    let grant = cli(&[
        "admit",
        "alice",
        "from-cli.rs",
        "--ttl-ms",
        "60000",
        "--request-id",
        "cli-a",
    ]);
    assert!(
        grant.status.success(),
        "{}",
        String::from_utf8_lossy(&grant.stderr)
    );
    let response: langspan::protocol::Response = serde_json::from_slice(&grant.stdout).unwrap();
    assert_eq!(response.lease_valid, Some(true));
    let conflict = cli(&["admit", "bob", "from-cli.rs", "--request-id", "cli-b"]);
    assert_eq!(conflict.status.code(), Some(2));
    let done = cli(&["complete", "alice", "cli-a", "--request-id", "cli-done"]);
    assert!(done.status.success());
    let retry = cli(&[
        "admit",
        "alice",
        "from-cli.rs",
        "--ttl-ms",
        "60000",
        "--request-id",
        "cli-a",
    ]);
    assert_eq!(
        retry.status.code(),
        Some(3),
        "a completed original grant must not exit successfully on replay"
    );
}

#[test]
fn malformed_frames_do_not_break_subsequent_requests() {
    use std::io::Write;
    let mut f = Fixture::new();
    f.start();
    for bytes in [b"{bad}\n".to_vec(), vec![b'a'; 65_537]] {
        let mut stream =
            std::os::unix::net::UnixStream::connect(f.state_dir().join("daemon.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(&bytes).unwrap();
        let reply = langspan::protocol::read_frame(stream).unwrap();
        let reply: langspan::protocol::Response = serde_json::from_slice(&reply).unwrap();
        assert!(matches!(reply.outcome, Outcome::Error { .. }));
    }
    assert!(matches!(
        f.call("after-bad-frame", Command::Status).outcome,
        Outcome::Status { .. }
    ));
}
