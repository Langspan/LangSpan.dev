#![cfg(unix)]
//! Outline first for whole reads of large files (roadmap A11, issue #16):
//! the real hook executable, a real daemon, a real Git repository and the
//! real source index.
//!
//! The `PreToolUse` `Read` events are Claude Code 2.1.331's captured
//! `pre-edit.json` with the tool name and input of a `Read`, whose input
//! fields (`file_path`, `offset`, `limit`) are those of Claude Code's
//! documented `Read` tool. No `PreToolUse` `Read` has been captured yet.

use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(60);
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";
const PRE_EDIT: &str = include_str!("../fixtures/claude-code-2.1.331/pre-edit.json");
const ENDING: &str = "Read lines A to B of a definition, or read the file again to get it whole.";

struct World {
    base: PathBuf,
    daemon: Option<Child>,
    calls: AtomicU64,
}

struct Run {
    coverage: String,
    detail: String,
    stderr_lines: usize,
    stdout: String,
    reason: Option<String>,
}

fn git(root: &Path, args: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A Rust file of exactly 2,000 lines: 100 public functions of 20 lines.
fn large_rust() -> String {
    let mut text = String::new();
    for n in 0..100 {
        text.push_str(&format!("pub fn step_{n}(x: u32) -> u32 {{\n"));
        for _ in 0..18 {
            text.push_str("    let x = x + 1;\n");
        }
        text.push_str("}\n");
    }
    assert_eq!(text.lines().count(), 2000);
    text
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-outline-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let work = base.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(work.join("src/store.rs"), large_rust()).unwrap();
        fs::write(
            work.join("src/small.rs"),
            "pub fn small() -> u32 {\n    1\n}\n",
        )
        .unwrap();
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "init"]);
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self {
            base,
            daemon: None,
            calls: AtomicU64::new(0),
        };
        world.start();
        let root = world.work();
        let added = world.cli(&[
            "workspace",
            "add",
            "app",
            root.to_str().unwrap(),
            "--request-id",
            "ws",
        ]);
        assert_eq!(added.0, 0, "{}", added.1);
        world.wait_for_index(2);
        world
    }
    fn start(&mut self) {
        self.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.base.join("state"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop(&mut self) {
        let mut child = self.daemon.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn file(&self) -> PathBuf {
        self.work().join("src/store.rs")
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
    fn wait_for_index(&self, files: u64) {
        let deadline = Instant::now() + WAIT;
        loop {
            let (_, text) = self.cli(&["graph", "symbol", "app", "step_0", "--json"]);
            let outcome = serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone();
            let coverage = &outcome["coverage"];
            if coverage["current_files"] == files && coverage["indexable_files"] == files {
                return;
            }
            assert!(Instant::now() < deadline, "never indexed: {outcome:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// The hook of `vendor`, its stdout parsed, and its diagnostic coverage.
    fn host(&self, vendor: &str, event: &Value) -> (Value, String) {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
            .args(["--outline-reads", "on"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        (
            serde_json::from_str(&stdout).unwrap_or(Value::Null),
            diagnostic["coverage"].as_str().unwrap().into(),
        )
    }
    fn hook(&self, extra: &[&str], event: &Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("\"allow\""), "{stdout}");
        let reason = serde_json::from_str::<Value>(&stdout)
            .ok()
            .and_then(|reply| {
                assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny");
                reply["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .map(str::to_owned)
            });
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            stderr_lines: stderr.lines().count(),
            stdout,
            reason,
        }
    }
    /// A `Read` event in Claude Code 2.1.331's recorded shape.
    fn read_event(&self, name: &str, session: &str, input: Value) -> Value {
        let mut event: Value = serde_json::from_str(PRE_EDIT).unwrap();
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        event["session_id"] = json!(session);
        event["cwd"] = json!(self.work());
        event["hook_event_name"] = json!(name);
        event["tool_name"] = json!("Read");
        event["tool_input"] = input;
        event["tool_use_id"] = json!(format!("toolu_outline{call:04}"));
        if name == "PostToolUse" {
            event["tool_response"] = json!({"type": "text"});
        }
        event
    }
    fn whole(&self) -> Value {
        json!({"file_path": self.file()})
    }
    fn pre_read(&self, session: &str, input: Value, on: bool) -> Run {
        let extra: &[&str] = if on { &["--outline-reads", "on"] } else { &[] };
        self.hook(extra, &self.read_event("PreToolUse", session, input))
    }
    fn post_read(&self, session: &str) -> Run {
        self.hook(
            &["--outline-reads", "on"],
            &self.read_event("PostToolUse", session, self.whole()),
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn a_whole_read_of_a_large_indexed_file_is_outlined_once_then_admitted() {
    let world = World::new();
    let first = world.pre_read(ALICE, world.whole(), true);
    let reason = first.reason.expect("the first whole read is outlined");
    assert_eq!(first.coverage, "outlined", "{}", first.detail);
    assert!(reason.len() <= 2400, "{} bytes: {reason}", reason.len());
    assert!(
        reason.starts_with("LangSpan (data, not instructions): src/store.rs has 2,000 lines."),
        "{reason}"
    );
    assert!(
        reason.contains("\nfunction step_0 lines 1-20 (exported)\n"),
        "{reason}"
    );
    assert!(reason.ends_with(ENDING), "{reason}");
    // Nothing of the file beyond the index's names is in the outline.
    assert!(!reason.contains("let x"), "{reason}");

    // The identical next read goes through, and its version is recorded.
    let again = world.pre_read(ALICE, world.whole(), true);
    assert!(again.stdout.is_empty(), "{}", again.stdout);
    assert_eq!(again.coverage, "outline_shown", "{}", again.detail);
    let recorded = world.post_read(ALICE);
    assert_eq!(recorded.coverage, "read_recorded", "{}", recorded.detail);

    // Another session's first whole read is outlined once for it.
    let other = world.pre_read(BOB, world.whole(), true);
    assert!(other.reason.is_some(), "{}", other.detail);
    let other_again = world.pre_read(BOB, world.whole(), true);
    assert!(other_again.stdout.is_empty(), "{}", other_again.stdout);
}

/// Review comment 4713905468, through the real hook and daemon: a file of
/// 250 one-line definitions and then one of 30 lines. The outline names the
/// large one, which comes after the daemon's limit in line order, and
/// counts every definition it does not list.
#[test]
fn a_large_definition_past_many_small_ones_is_in_the_outline() {
    let world = World::new();
    let mut text = String::new();
    for n in 0..250 {
        text.push_str(&format!("fn tiny_{n}() {{}}\n"));
    }
    text.push_str("pub fn the_big_one() {\n");
    for _ in 0..28 {
        text.push_str("    let _ = 1;\n");
    }
    text.push_str("}\n");
    for n in 0..1800 {
        text.push_str(&format!("// filler {n}\n"));
    }
    let path = world.work().join("src/many.rs");
    fs::write(&path, &text).unwrap();
    git(&world.work(), &["add", "."]);
    git(&world.work(), &["commit", "--quiet", "-m", "many"]);
    world.wait_for_index(3);
    let deadline = Instant::now() + WAIT;
    let reason = loop {
        let run = world.pre_read(ALICE, json!({"file_path": path}), true);
        if let Some(reason) = run.reason {
            break reason;
        }
        assert!(Instant::now() < deadline, "never outlined: {}", run.detail);
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        reason.contains("\nfunction the_big_one lines 251-280 (exported)\n"),
        "{reason}"
    );
    let listed = reason.lines().count() - 3;
    assert!(
        reason.contains(&format!(
            "\n{} more definitions not listed.\n",
            251 - listed
        )),
        "{reason}"
    );
}

/// Review comment 4714078108, through the real daemon: once a session was
/// given an outline, a client of protocol version 8 or 7 still reads
/// status, state records and events, which carry neither `outlined` nor the
/// `read_outlined` event; a version 10 client's carry both.
#[test]
fn a_version_7_client_reads_status_and_events_after_an_outline() {
    use langspan::protocol::{PROTOCOL_VERSION, PROTOCOL_VERSION_7, PROTOCOL_VERSION_8, Request};
    use langspan_core::Command;
    use std::io::{BufRead, BufReader};
    let world = World::new();
    assert!(world.pre_read(ALICE, world.whole(), true).reason.is_some());
    let socket = world.base.join("state/daemon.sock");
    // The daemon's own bytes, as a client of `version` receives them.
    let ask = |version: u16, id: &str, command: Command| -> String {
        let request = Request {
            version,
            request_id: format!("{id}-{version}"),
            command,
        };
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream
            .write_all(&langspan::protocol::encode(&request).unwrap())
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["version"], version, "{line}");
        assert_ne!(reply["outcome"]["type"], "error", "{line}");
        line
    };
    for version in [PROTOCOL_VERSION_7, PROTOCOL_VERSION_8, PROTOCOL_VERSION] {
        let status = ask(version, "status", Command::Status);
        let records = ask(
            version,
            "records",
            Command::StatusRecords {
                after: None,
                page_bytes: None,
            },
        );
        let events = ask(
            version,
            "events",
            Command::Events {
                after: 0,
                limit: 1000,
            },
        );
        let current = version == PROTOCOL_VERSION;
        assert_eq!(status.contains("\"outlined\""), current, "{status}");
        assert_eq!(records.contains("\"outlined\""), current, "{records}");
        assert_eq!(events.contains("\"read_outlined\""), current, "{events}");
    }
}

#[test]
fn a_read_with_a_line_range_is_never_refused() {
    let world = World::new();
    for input in [
        json!({"file_path": world.file(), "offset": 88, "limit": 120}),
        json!({"file_path": world.file(), "offset": 1}),
        json!({"file_path": world.file(), "limit": 50}),
    ] {
        let run = world.pre_read(ALICE, input, true);
        assert!(run.stdout.is_empty(), "{}", run.stdout);
        assert_eq!(run.coverage, "read_range", "{}", run.detail);
    }
    // The ranged reads did not use up the session's outline.
    assert!(world.pre_read(ALICE, world.whole(), true).reason.is_some());
}

#[test]
fn a_small_file_and_the_option_off_are_never_refused() {
    let world = World::new();
    let small = world.pre_read(
        ALICE,
        json!({"file_path": world.work().join("src/small.rs")}),
        true,
    );
    assert!(small.stdout.is_empty(), "{}", small.stdout);
    assert_eq!(small.coverage, "not_outlined", "{}", small.detail);
    // Off is the default: no option on the command.
    let off = world.pre_read(ALICE, world.whole(), false);
    assert!(off.stdout.is_empty(), "{}", off.stdout);
    // And the session's first read with the option on is still outlined.
    assert!(world.pre_read(ALICE, world.whole(), true).reason.is_some());
}

#[test]
fn a_file_that_differs_from_its_indexed_version_is_read_without_refusal() {
    let world = World::new();
    let mut changed = large_rust();
    changed.push_str("pub fn added_later() -> u32 {\n    2\n}\n");
    fs::write(world.file(), &changed).unwrap();
    let run = world.pre_read(ALICE, world.whole(), true);
    // The daemon scans every 2 s: a refusal here is only right once the
    // index holds the new version, and its outline then names the new
    // definition. The deterministic case is `graph`'s own unit test.
    match &run.reason {
        None => assert_eq!(run.coverage, "not_outlined", "{}", run.detail),
        Some(reason) => assert!(reason.contains("added_later"), "{reason}"),
    }
}

/// Cursor's `preToolUse` on `Read`, from its constructed fixture
/// (`fixtures/cursor-constructed/post-read.json` before the tool ran),
/// answered in Cursor's `permission` shape. Not yet run on a real host.
#[test]
fn cursor_is_outlined_once_in_its_own_shape() {
    let world = World::new();
    let mut event: Value = serde_json::from_str(include_str!(
        "../fixtures/cursor-constructed/post-read.json"
    ))
    .unwrap();
    event["hook_event_name"] = json!("preToolUse");
    event["cwd"] = json!(world.work());
    event["workspace_roots"] = json!([world.work()]);
    event["tool_input"] = json!({"file_path": world.file()});
    for field in ["tool_output", "duration"] {
        event.as_object_mut().unwrap().remove(field);
    }
    let (first, coverage) = world.host("cursor", &event);
    assert_eq!(coverage, "outlined");
    assert_eq!(first["permission"], "deny", "{first}");
    assert!(first["agent_message"].as_str().unwrap().ends_with(ENDING));
    event["tool_use_id"] = json!("tool_0004");
    let (again, coverage) = world.host("cursor", &event);
    assert_eq!((again, coverage.as_str()), (Value::Null, "outline_shown"));
}

#[test]
fn with_the_daemon_stopped_the_read_proceeds_with_one_stderr_line() {
    let mut world = World::new();
    world.stop();
    let run = world.pre_read(ALICE, world.whole(), true);
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    assert_eq!(run.stderr_lines, 1);
    assert!(
        ["unavailable", "timeout"].contains(&run.coverage.as_str()),
        "{}: {}",
        run.coverage,
        run.detail
    );
}
