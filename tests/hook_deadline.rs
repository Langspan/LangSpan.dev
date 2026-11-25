#![cfg(unix)]

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn stalled_host_input_consumes_the_same_hook_budget() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args([
            "hook",
            "claude-code",
            "--workspace",
            "deadline-test",
            "--root",
            "/tmp",
            "--state-dir",
            "/tmp/langspan-unused-deadline-state",
            "--budget-ms",
            "40",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Keep the pipe open with no data. The wrong implementation waits for EOF.
    let input = child.stdin.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("hook did not bound a stalled stdin read by its own deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["coverage"], "timeout");
}

/// Review finding on PR #30: with the observation spool present, a tool hook
/// first asks whether the coordinator accepts a connection. A coordinator
/// that stopped accepting, with its listen backlog full, must not hold the
/// hook past its budget: on Linux a blocking connect waits there forever.
#[test]
fn a_coordinator_whose_backlog_is_full_does_not_hold_a_hook_past_its_budget() {
    use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketType};
    use std::os::unix::fs::PermissionsExt;
    let base = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("ls-backlog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let state = base.join("state");
    let root = base.join("work");
    std::fs::create_dir_all(state.join("spool")).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for dir in [&base, &state, &state.join("spool")] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let socket = state.join("daemon.sock");
    // A listener that never accepts, with the smallest backlog, filled.
    let listener = net::socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
    let address = SocketAddrUnix::new(&socket).unwrap();
    net::bind(&listener, &address).unwrap();
    net::listen(&listener, 0).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut queued = Vec::new();
    for _ in 0..4096 {
        let client = std::os::unix::net::UnixStream::from(
            net::socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap(),
        );
        client.set_nonblocking(true).unwrap();
        let full = net::connect(&client, &address).is_err();
        queued.push(client);
        if full {
            break;
        }
    }
    let event = serde_json::json!({
        "session_id": "00000000-0000-4000-8000-00000000000a", "cwd": root,
        "hook_event_name": "PreToolUse", "tool_name": "Write", "tool_use_id": "w1",
        "tool_input": {"file_path": root.join("a.txt"), "content": "x"},
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["hook", "claude-code", "--workspace", "work", "--root"])
        .arg(&root)
        .arg("--state-dir")
        .arg(&state)
        .args(["--budget-ms", "500"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    // Far above the 500 ms budget, far below forever.
    let deadline = Instant::now() + Duration::from_secs(10);
    let finished = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(queued);
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        finished,
        "the hook waited on a coordinator that never accepts"
    );
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "no decision without the coordinator"
    );
}
