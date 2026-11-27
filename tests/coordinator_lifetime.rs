#![cfg(unix)]
//! A coordinator outlives nothing that started it for a test (issue #239):
//! it stops when its state directory goes away or opens to others, a test
//! that panics stops its coordinator, and in a debug build a coordinator
//! left by a test that was killed ends once it is idle.

mod support;

use langspan::{client, protocol::Request};
use langspan_core::Command;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use support::Owned;

static NEXT: AtomicU64 = AtomicU64::new(0);
/// Names the folder a supervised test works in. Unset, a supervised test
/// does nothing, so `cargo test -- --ignored` runs it harmlessly.
const SUPERVISED: &str = "LANGSPAN_TEST_SUPERVISED_ROOT";
const IDLE: &str = "LANGSPAN_TEST_IDLE_EXIT_MS";

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ls-life-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn answers(state_dir: &Path) -> bool {
    client::call(state_dir, &Request::new("ready", Command::Status)).is_ok()
}

/// Starts a coordinator for `root/state`, logging to `root/daemon.log`, and
/// waits until it answers.
fn start(root: &Path, idle_ms: Option<u64>) -> Owned {
    start_with(root, idle_ms, &[])
}

/// `start`, with `env` set for the coordinator.
fn start_with(root: &Path, idle_ms: Option<u64>, env: &[(&str, &str)]) -> Owned {
    let state_dir = root.join("state");
    let log = fs::File::create(root.join("daemon.log")).unwrap();
    let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
    command
        .env("LANGSPAN_AUTOSTART", "off")
        .env_remove(IDLE)
        .args(["daemon", "--state-dir"])
        .arg(&state_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .envs(env.iter().copied());
    if let Some(ms) = idle_ms {
        command.env(IDLE, ms.to_string());
    }
    let mut daemon = Owned::spawn(&mut command);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !answers(&state_dir) {
        if let Some(status) = daemon.child().try_wait().unwrap() {
            panic!(
                "daemon exited before readiness: {status}: {}",
                fs::read_to_string(root.join("daemon.log")).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "daemon did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    daemon
}

/// Waits up to `limit` for `daemon` to exit by itself, and returns its log.
fn exits_within(daemon: &mut Owned, root: &Path, limit: Duration) -> String {
    let deadline = Instant::now() + limit;
    while daemon.child().try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "the coordinator was still running after {limit:?}: {}",
            fs::read_to_string(root.join("daemon.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    fs::read_to_string(root.join("daemon.log")).unwrap()
}

fn stopping_line(log: &str) -> serde_json::Value {
    log.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| line["event"] == "daemon_stopping")
        .unwrap_or_else(|| panic!("no daemon_stopping line in: {log}"))
}

#[test]
fn a_coordinator_exits_within_ten_seconds_when_its_state_directory_is_removed() {
    let root = Root::new();
    let mut daemon = start(&root.0, None);
    fs::remove_dir_all(root.0.join("state")).unwrap();
    let log = exits_within(&mut daemon, &root.0, Duration::from_secs(10));
    let line = stopping_line(&log);
    assert_eq!(line["reason"], "state_directory_removed", "{log}");
    assert!(
        line["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "{log}"
    );
}

#[test]
fn a_coordinator_exits_when_its_state_directory_is_opened_to_others() {
    let root = Root::new();
    let mut daemon = start(&root.0, None);
    fs::set_permissions(root.0.join("state"), fs::Permissions::from_mode(0o755)).unwrap();
    let log = exits_within(&mut daemon, &root.0, Duration::from_secs(10));
    assert_eq!(
        stopping_line(&log)["reason"],
        "state_directory_not_private",
        "{log}"
    );
}

/// A later test that reuses the path gets a new folder, never an answer
/// from the coordinator of the old one.
#[test]
fn a_coordinator_exits_when_its_state_directory_is_replaced() {
    let root = Root::new();
    let mut daemon = start(&root.0, None);
    fs::rename(root.0.join("state"), root.0.join("moved")).unwrap();
    fs::create_dir(root.0.join("state")).unwrap();
    fs::set_permissions(root.0.join("state"), fs::Permissions::from_mode(0o700)).unwrap();
    let log = exits_within(&mut daemon, &root.0, Duration::from_secs(10));
    assert_eq!(
        stopping_line(&log)["reason"],
        "state_directory_replaced",
        "{log}"
    );
}

/// Runs `test` from this test binary in a child process, with
/// `SUPERVISED` naming `root`, and returns the child.
fn supervise(test: &str, root: &Path, idle_ms: Option<u64>) -> Owned {
    let mut command = Process::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", test, "--test-threads=1"])
        .env(SUPERVISED, root)
        .env_remove(IDLE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(ms) = idle_ms {
        command.env(IDLE, ms.to_string());
    }
    Owned::spawn(&mut command)
}

/// Waits for the supervised test to write its coordinator's process ID.
fn coordinator_pid(test: &mut Owned, root: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(text) = fs::read_to_string(root.join("pid"))
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            return pid;
        }
        assert!(
            test.child().try_wait().unwrap().is_none(),
            "the supervised test exited before starting its coordinator"
        );
        assert!(
            Instant::now() < deadline,
            "the supervised test never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Kills `pid` when dropped, so a supervising test that fails does not
/// itself leave the coordinator it was checking.
struct KillOnDrop(i32);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = rustix::process::Pid::from_raw(self.0) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

/// Supervised by the test below: starts a coordinator, says which process
/// it is, then fails an assertion.
#[test]
#[ignore = "run by a_test_that_panics_leaves_no_coordinator_running"]
fn supervised_panics_with_a_coordinator_running() {
    let Some(root) = std::env::var_os(SUPERVISED) else {
        return;
    };
    let root = PathBuf::from(root);
    let daemon = start(&root, None);
    fs::write(root.join("pid"), daemon.id().to_string()).unwrap();
    panic!("a failure partway through a test");
}

#[test]
fn a_test_that_panics_leaves_no_coordinator_running() {
    let root = Root::new();
    let mut test = supervise(
        "supervised_panics_with_a_coordinator_running",
        &root.0,
        None,
    );
    let pid = coordinator_pid(&mut test, &root.0);
    let _cleanup = KillOnDrop(pid);
    let status = test.wait_with_output().status;
    assert!(!status.success(), "the supervised test was meant to fail");
    assert!(
        !support::running(pid),
        "coordinator {pid} outlived the test that panicked"
    );
}

/// A process that has exited but that no parent has reaped is a zombie:
/// signal 0 still finds it. A coordinator orphaned to an init that does not
/// reap (some Linux containers) stays one after it exits, so the tests here
/// wait with `support::running`, which counts a zombie as stopped.
#[test]
fn a_zombie_process_counts_as_stopped() {
    let mut child = Process::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
    let pid = i32::try_from(child.id()).unwrap();
    // Not reaped: until `wait`, it stays a zombie of this process.
    let deadline = Instant::now() + Duration::from_secs(10);
    while support::running(pid) {
        assert!(Instant::now() < deadline, "{pid} never exited");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        support::alive(pid),
        "{pid} was reaped before it was checked"
    );
    child.wait().unwrap();
}

/// Supervised by the test below: starts a coordinator with a short idle
/// limit, says which process it is, then hangs until it is killed.
#[test]
#[ignore = "run by a_killed_test_leaves_a_coordinator_that_ends_once_idle"]
fn supervised_hangs_with_a_coordinator_running() {
    let Some(root) = std::env::var_os(SUPERVISED) else {
        return;
    };
    let root = PathBuf::from(root);
    let idle = std::env::var(IDLE).ok().and_then(|ms| ms.parse().ok());
    let daemon = start(&root, idle);
    fs::write(root.join("pid"), daemon.id().to_string()).unwrap();
    std::thread::sleep(Duration::from_secs(600));
}

/// A test killed on a timeout runs no `Drop`; its coordinator, a debug
/// build with an idle limit (five minutes unless the test sets one), ends
/// by itself.
#[cfg(debug_assertions)]
#[test]
fn a_killed_test_leaves_a_coordinator_that_ends_once_idle() {
    let root = Root::new();
    let mut test = supervise(
        "supervised_hangs_with_a_coordinator_running",
        &root.0,
        Some(1000),
    );
    let pid = coordinator_pid(&mut test, &root.0);
    let _cleanup = KillOnDrop(pid);
    test.stop();
    let deadline = Instant::now() + Duration::from_secs(10);
    while support::running(pid) {
        assert!(
            Instant::now() < deadline,
            "coordinator {pid} was still running 10 s after its test was killed: {}",
            fs::read_to_string(root.0.join("daemon.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = fs::read_to_string(root.0.join("daemon.log")).unwrap();
    assert_eq!(stopping_line(&log)["reason"], "idle", "{log}");
}

/// The idle limit counts from the last request: a coordinator that is
/// called more often than that keeps running.
#[cfg(debug_assertions)]
#[test]
fn a_test_coordinator_that_keeps_being_called_does_not_stop_for_idleness() {
    let root = Root::new();
    let mut daemon = start(&root.0, Some(1000));
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        assert!(answers(&root.0.join("state")), "the coordinator stopped");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(daemon.child().try_wait().unwrap().is_none());
    // Then left alone, it stops.
    let log = exits_within(&mut daemon, &root.0, Duration::from_secs(10));
    assert_eq!(stopping_line(&log)["reason"], "idle", "{log}");
}

/// With the idle exit turned off, an idle coordinator keeps running. A
/// release build has none at all (a unit test in `src/daemon.rs`).
#[test]
fn a_coordinator_with_the_idle_exit_turned_off_keeps_running() {
    let root = Root::new();
    let mut daemon = start(&root.0, Some(0));
    std::thread::sleep(Duration::from_secs(2));
    assert!(daemon.child().try_wait().unwrap().is_none());
    assert!(answers(&root.0.join("state")));
}

/// A panic in a coordinator thread stops the process with a failure status
/// and one structured line, and removes its socket, so a login service
/// starts it again instead of leaving it listening without answers (issue
/// #329). Debug builds only: a release build has no switch that panics.
#[cfg(debug_assertions)]
#[test]
fn a_coordinator_thread_that_panics_stops_the_coordinator() {
    let root = Root::new();
    let state_dir = root.0.join("state");
    let mut daemon = start_with(
        &root.0,
        None,
        &[("LANGSPAN_TEST_PANIC_ON_REQUEST", "panic-now")],
    );
    let _ = client::call(&state_dir, &Request::new("panic-now", Command::Status));
    let log = exits_within(&mut daemon, &root.0, Duration::from_secs(1));
    let status = daemon.child().wait().unwrap();
    assert!(!status.success(), "{status}: {log}");
    assert!(!state_dir.join("daemon.sock").exists(), "{log}");
    let line = stopping_line(&log);
    assert_eq!(line["level"], "error", "{log}");
    assert_eq!(line["reason"], "panic", "{log}");
    assert_eq!(line["thread"], "langspan-owner", "{log}");
    let detail = line["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("LANGSPAN_TEST_PANIC_ON_REQUEST"), "{log}");
    // One line: the default hook's unstructured message is not written too.
    assert!(!log.contains("panicked at"), "{log}");
}

/// A state directory whose socket listens but never answers, and an empty
/// home: what `doctor` sees of a stuck coordinator.
fn stuck_socket(root: &Root) -> (PathBuf, PathBuf, std::os::unix::net::UnixListener) {
    let state_dir = root.0.join("state");
    fs::create_dir(&state_dir).unwrap();
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.0.join("home");
    fs::create_dir(&home).unwrap();
    let socket = state_dir.join("daemon.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    (state_dir, home, listener)
}

/// Runs `doctor` for `state_dir` and returns its stdout, failing the test
/// if it runs past `limit`.
fn doctor_within(state_dir: &Path, home: &Path, limit: Duration) -> String {
    let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(state_dir)
        .args(["doctor", "--home"])
        .arg(home)
        .env("LANGSPAN_AUTOSTART", "off")
        .env_remove("GEMINI_CLI_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("doctor ran past {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `doctor` reports a coordinator that no longer accepts and whose listen
/// backlog is full, instead of waiting on its socket: a blocking connect
/// there does not return on Linux.
#[test]
fn doctor_reports_a_coordinator_whose_listen_backlog_is_full() {
    let root = Root::new();
    let (state_dir, home, _listener) = stuck_socket(&root);
    // Queues connections until the backlog is full. On Linux the last
    // connect blocks; on macOS it is refused, and the thread keeps the
    // queued ones open.
    let socket = state_dir.join("daemon.sock");
    let queued = std::sync::Arc::new(AtomicU64::new(0));
    let counter = queued.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok(stream) = std::os::unix::net::UnixStream::connect(&socket) {
            held.push(stream);
            counter.fetch_add(1, Ordering::Relaxed);
        }
        std::thread::park();
    });
    let mut last = u64::MAX;
    while queued.load(Ordering::Relaxed) != last {
        last = queued.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(200));
    }
    let stdout = doctor_within(&state_dir, &home, Duration::from_secs(60));
    assert!(stdout.contains("Coordinator"), "{stdout}");
}

/// `doctor` names a coordinator that accepts connections but does not
/// answer, and says how to restart it (issue #329).
#[test]
fn doctor_names_a_coordinator_that_accepts_but_does_not_answer() {
    let root = Root::new();
    let (state_dir, home, listener) = stuck_socket(&root);
    // Accepts every connection and holds it open without a reply.
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
        }
    });
    let stdout = doctor_within(&state_dir, &home, Duration::from_secs(60));
    assert!(
        stdout.contains("accepts connections but does not answer"),
        "{stdout}"
    );
    assert!(stdout.contains("langspan daemon --state-dir"), "{stdout}");
}
