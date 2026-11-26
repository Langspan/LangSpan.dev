#![cfg(unix)]
//! The automatic check after a batch of a session's writes (ADR 0030),
//! through the real hook executable, the real daemon, a real Git checkout
//! and real child processes. Claude Code's PostToolBatch is in the shape
//! captured from Claude Code 2.1.330 (`fixtures/claude-code-2.1.330`);
//! Codex's Stop is in its documented shape only.

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{CheckRecord, CheckResult, Command, Outcome, State};
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

#[allow(dead_code)] // Only `alive` is used here.
mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-0000000000a1";

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

struct Run {
    coverage: String,
    detail: String,
    stdout: String,
    took: Duration,
}

impl Run {
    fn context(&self) -> Option<String> {
        let reply: Value = serde_json::from_str(&self.stdout).ok()?;
        Some(
            reply["hookSpecificOutput"]["additionalContext"]
                .as_str()?
                .to_owned(),
        )
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Dev",
            "-c",
            "user.email=dev@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Fixture {
    /// A checkout with one commit and a daemon that waits `debounce_ms`
    /// after a session's last batch of writes before it starts the check.
    fn new(debounce_ms: u64) -> Self {
        Self::with_config(json!({ "auto_check_debounce_ms": debounce_ms }))
    }
    /// As `new`, with the daemon's whole `config.json`.
    fn with_config(config_json: Value) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-auto-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("a.txt"), "hello\n").unwrap();
        git(&work, &["init", "--quiet", "-b", "main"]);
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", "first"]);
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let config = state.join("config.json");
        fs::write(&config, config_json.to_string()).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let daemon = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let fixture = Self {
            base,
            daemon: Some(daemon),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&fixture.state_dir(), &status()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        // The session's first event registers the workspace.
        let start = fixture.run(
            "claude-code",
            json!({"session_id": ALICE, "cwd": fixture.root(), "hook_event_name": "SessionStart",
                   "source": "startup", "transcript_path": "/dev/null"}),
        );
        assert!(!start.coverage.is_empty(), "{}", start.detail);
        fixture
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn root(&self) -> PathBuf {
        self.base.join("work")
    }
    /// Outside the checkout, so writing it does not change the checked tree.
    fn log(&self) -> PathBuf {
        self.base.join("runs.log")
    }
    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    fn state(&self) -> State {
        match client::call(&self.state_dir(), &status()).unwrap().outcome {
            Outcome::Status { state, .. } => state,
            other => panic!("{other:?}"),
        }
    }
    fn checks(&self) -> Vec<CheckRecord> {
        self.state().checks.remove("work").unwrap_or_default()
    }
    /// Wait for the `count`th check record, then return all of them.
    fn wait_for_checks(&self, count: usize) -> Vec<CheckRecord> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let checks = self.checks();
            if checks.len() >= count {
                return checks;
            }
            assert!(
                Instant::now() < deadline,
                "no automatic check was recorded; runs: {:?}",
                self.log_lines()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn tree(&self) -> String {
        langspan::integrate::working_tree(&self.root(), Instant::now() + Duration::from_secs(30))
            .unwrap()
            .1
    }
    fn cli(&self, args: &[&str]) -> (i32, String, String) {
        // Before the arguments: everything after `--` is the check's command.
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.state_dir())
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
    fn enable(&self, timeout_s: u32, script: &str) {
        let log = self.log();
        let timeout = timeout_s.to_string();
        let (code, stdout, stderr) = self.cli(&[
            "check",
            "auto",
            "work",
            "--name",
            "tests",
            "--timeout-s",
            &timeout,
            "--",
            "sh",
            "-c",
            script,
            log.to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "{stdout}{stderr}");
        assert!(stdout.contains("Automatic check tests is on"), "{stdout}");
    }
    fn run(&self, vendor: &str, event: Value) -> Run {
        let started = Instant::now();
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .env_remove("LANGSPAN_CAPTURE_DIR")
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
        let took = started.elapsed();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            took,
        }
    }
    /// One Claude Code Write of `a.txt`, then the batch that ends with it.
    fn write_batch(&self, call: &str, content: &str) -> Run {
        let (file, input, post) = self.write(call, content);
        let batch = self.run(
            "claude-code",
            json!({"session_id": ALICE, "cwd": self.root(), "hook_event_name": "PostToolBatch",
                   "prompt_id": "p0", "permission_mode": "acceptEdits",
                   "tool_calls": [{"tool_name": "Write", "tool_input": input, "tool_use_id": call,
                                   "tool_response": format!("File created successfully at: {}", file.display())}]}),
        );
        // A briefing goes out at the first point that can carry one: the
        // completed call, or else the batch.
        if post.context().is_some() {
            assert!(
                batch.context().is_none(),
                "delivered once: {}",
                batch.stdout
            );
            return Run {
                coverage: batch.coverage,
                ..post
            };
        }
        batch
    }
    /// One Claude Code Write of `a.txt`, admitted and observed, with no
    /// batch after it.
    fn write(&self, call: &str, content: &str) -> (PathBuf, Value, Run) {
        let file = self.root().join("a.txt");
        let input = json!({"file_path": file, "content": content});
        let tool = |name: &str| {
            json!({"session_id": ALICE, "cwd": self.root(), "hook_event_name": name,
                   "tool_name": "Write", "tool_use_id": call, "tool_input": input})
        };
        assert_eq!(
            self.run("claude-code", tool("PreToolUse")).coverage,
            "admitted"
        );
        fs::write(&file, content).unwrap();
        let post = self.run("claude-code", tool("PostToolUse"));
        assert_eq!(post.coverage, "observed", "{}", post.detail);
        (file, input, post)
    }
    fn prompt(&self, id: &str) -> Run {
        self.run(
            "claude-code",
            json!({"session_id": ALICE, "cwd": self.root(), "hook_event_name": "UserPromptSubmit",
                   "prompt": "carry on", "prompt_id": id, "permission_mode": "default"}),
        )
    }
    fn wait_for_log(&self, lines: usize) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.log_lines().len() < lines {
            assert!(Instant::now() < deadline, "runs: {:?}", self.log_lines());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn pid(&self, file: &str) -> i32 {
        let path = self.base.join(file);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(text) = fs::read_to_string(&path)
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "the check never started");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.cli(&["check", "auto", "work", "--off"]);
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn status() -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Status,
    }
}

/// Gone, or a zombie its new parent has not reaped yet: either way not
/// running. Polled, because a killed process is reaped asynchronously.
fn stops(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let state = Process::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&state.stdout).trim().to_owned();
        if !support::alive(pid) || state.is_empty() || state.starts_with('Z') {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Appends a line to the log named by `$0`, prints a failure and exits 3.
const FAILING: &str =
    r#"echo run >> "$0"; echo "running 2 tests"; echo "assertion failed: left == right"; exit 3"#;
const FAILING_SLOWLY: &str = r#"echo run >> "$0"; sleep 1; echo "assertion failed"; exit 3"#;

#[test]
fn with_the_automatic_check_off_nothing_runs_after_a_batch_of_writes() {
    let f = Fixture::new(100);
    let batch = f.write_batch("call-1", "one\n");
    assert_eq!(batch.coverage, "batch_recorded", "{}", batch.detail);
    // Turned on and off again: still nothing.
    f.enable(60, FAILING);
    assert_eq!(f.cli(&["check", "auto", "work", "--off"]).0, 0);
    let (_, shown, _) = f.cli(&["check", "auto", "work"]);
    assert!(shown.contains("Automatic check is off"), "{shown}");
    f.write_batch("call-2", "two\n");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(f.checks().is_empty());
    assert!(f.log_lines().is_empty(), "{:?}", f.log_lines());
    assert!(f.prompt("p1").context().is_none());
}

#[test]
fn a_batch_of_writes_runs_one_check_on_that_exact_tree_and_its_failure_is_delivered_once() {
    let f = Fixture::new(100);
    f.enable(60, FAILING);
    let batch = f.write_batch("call-1", "one\n");
    assert!(batch.coverage.starts_with("batch_"), "{}", batch.detail);
    let tree = f.tree();
    let checks = f.wait_for_checks(1);
    assert_eq!(checks.len(), 1);
    let record = &checks[0];
    assert_eq!(record.name, "tests");
    assert_eq!(record.tree_oid, tree, "bound to the tree the writes left");
    assert_eq!(record.result, CheckResult::Failed { exit_code: 3 });
    assert!(record.inputs.is_empty());
    assert_eq!(f.log_lines(), ["run"]);
    // An ordinary record: `check status` finds that it applies now.
    let (_, status, _) = f.cli(&["check", "status", "work"]);
    assert!(
        status.contains("tests") && status.contains("Applies to the working tree as it is now"),
        "{status}"
    );

    // The next briefing point carries it, labelled as the check's output.
    let next = f.prompt("p1");
    let context = next.context().expect("the result is delivered");
    assert!(context.contains("tests failed (exit 3)"), "{context}");
    assert!(context.contains(&tree), "{context}");
    assert!(
        context.contains("assertion failed: left == right"),
        "{context}"
    );
    assert!(context.contains("not instructions"), "{context}");
    // Once.
    let again = f.prompt("p2");
    assert!(
        again
            .context()
            .is_none_or(|context| !context.contains("assertion failed")),
        "{}",
        again.stdout
    );
    // Nothing more was run.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(f.log_lines(), ["run"]);
}

#[test]
fn a_burst_of_batches_runs_one_check_on_the_tree_it_left() {
    // Far longer than the gaps between the three batches below.
    let f = Fixture::new(5000);
    // It logs as it starts and then takes a second, so a run started
    // before the burst ended would be seen, stopped, as an extra line.
    f.enable(60, FAILING_SLOWLY);
    for (n, content) in ["one\n", "two\n", "three\n"].into_iter().enumerate() {
        f.write_batch(&format!("call-{n}"), content);
        // Longer than a snapshot of this checkout takes, so a check started
        // at the first batch would see the next one change its tree.
        std::thread::sleep(Duration::from_millis(400));
    }
    let tree = f.tree();
    let checks = f.wait_for_checks(1);
    assert_eq!(checks[0].tree_oid, tree);
    std::thread::sleep(Duration::from_millis(5500));
    assert_eq!(f.log_lines(), ["run"], "one burst, one run");
    assert_eq!(f.checks().len(), 1);
    // The result reaches the session at its next batch, mid-turn.
    // The result reaches the session at its next briefing point, mid-turn:
    // here the completed call before the next batch.
    let next = f.write_batch("call-9", "four\n");
    let context = next
        .context()
        .expect("delivered at the next briefing point");
    assert!(context.contains("tests failed (exit 3)"), "{context}");
}

#[test]
fn a_tree_change_while_the_check_runs_cancels_it_and_checks_the_new_tree() {
    let f = Fixture::new(200);
    f.enable(
        60,
        r#"echo start >> "$0"; sleep 4; echo end >> "$0"; exit 0"#,
    );
    f.write_batch("call-1", "one\n");
    f.wait_for_log(1);
    f.write_batch("call-2", "two\n");
    let tree = f.tree();
    let checks = f.wait_for_checks(1);
    assert_eq!(checks.len(), 1, "the cancelled run recorded nothing");
    assert_eq!(checks[0].tree_oid, tree);
    assert_eq!(checks[0].result, CheckResult::Passed {});
    assert_eq!(f.log_lines(), ["start", "start", "end"]);
    let context = f.prompt("p1").context().expect("delivered");
    assert!(context.contains("tests passed"), "{context}");
}

#[test]
fn the_hook_never_waits_for_the_check_and_turning_it_off_stops_one_running() {
    let f = Fixture::new(100);
    let pidfile = f.base.join("check.pid");
    f.enable(
        600,
        &format!(r#"echo $$ > {}; exec sleep 30"#, pidfile.display()),
    );
    let batch = f.write_batch("call-1", "one\n");
    let pid = f.pid("check.pid");
    // Each hook run took far less than the check's 30 seconds, and one
    // made while the check runs is as quick.
    assert!(batch.took < Duration::from_secs(10), "{:?}", batch.took);
    let during = f.prompt("p1");
    assert!(during.took < Duration::from_secs(10), "{:?}", during.took);
    assert!(support::alive(pid), "the check is still running");
    assert!(f.checks().is_empty());
    assert_eq!(f.cli(&["check", "auto", "work", "--off"]).0, 0);
    assert!(stops(pid), "turning the check off stops it");
    // Long enough for the worker to hear that the run ended.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(f.checks().is_empty(), "a cancelled run records nothing");
}

/// A clone of the fixture's checkout, for `workspace move --switch`.
fn switch_to_a_clone(f: &Fixture) {
    let clone = f.base.join("clone");
    git(
        &f.base,
        &[
            "clone",
            "--quiet",
            f.root().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let (code, stdout, stderr) = f.cli(&[
        "workspace",
        "move",
        "work",
        clone.to_str().unwrap(),
        "--switch",
        "--request-id",
        "switch-1",
    ]);
    assert_eq!(code, 0, "{stdout}{stderr}");
}

#[test]
fn switching_the_workspace_stops_a_check_running_in_the_old_checkout() {
    let f = Fixture::new(100);
    let pidfile = f.base.join("check.pid");
    f.enable(
        600,
        &format!(r#"echo $$ > {}; exec sleep 30"#, pidfile.display()),
    );
    f.write_batch("call-1", "one\n");
    let pid = f.pid("check.pid");
    switch_to_a_clone(&f);
    assert!(stops(pid), "the old checkout's run is stopped");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(f.checks().is_empty(), "nothing reported about the new root");
    // The setting itself stays for writes in the new checkout.
    assert!(f.state().auto_checks.contains_key("work"));
}

#[test]
fn switching_the_workspace_forgets_a_check_waiting_to_run_in_the_old_checkout() {
    // Long enough for the switch to land inside the debounce.
    let f = Fixture::new(3000);
    f.enable(60, FAILING);
    f.write_batch("call-1", "one\n");
    switch_to_a_clone(&f);
    std::thread::sleep(Duration::from_millis(4500));
    assert!(f.log_lines().is_empty(), "ran: {:?}", f.log_lines());
    assert!(f.checks().is_empty());
}

#[test]
fn a_check_past_its_time_limit_is_killed_with_its_process_group() {
    let f = Fixture::new(100);
    let pidfile = f.base.join("child.pid");
    f.enable(
        1,
        &format!(r#"sleep 30 & echo $! > {}; wait"#, pidfile.display()),
    );
    f.write_batch("call-1", "one\n");
    let pid = f.pid("child.pid");
    let checks = f.wait_for_checks(1);
    assert_eq!(checks[0].result, CheckResult::Inconclusive {});
    assert!(stops(pid), "the command's own child was killed with it");
    let context = f.prompt("p1").context().expect("delivered");
    assert!(context.contains("did not finish within 1 s"), "{context}");
}

#[test]
fn a_codex_stop_after_a_patch_runs_the_check() {
    let f = Fixture::new(100);
    f.enable(60, FAILING);
    let patch = |name: &str| {
        let mut event = json!({
            "session_id": ALICE, "turn_id": "turn-1", "cwd": f.root(),
            "hook_event_name": name, "model": "gpt-test", "permission_mode": "default",
            "tool_name": "apply_patch", "tool_use_id": "patch-1",
            "tool_input": {"command":
                "*** Begin Patch\n*** Update File: a.txt\n@@\n-hello\n+hi\n*** End Patch"},
        });
        if name == "PostToolUse" {
            event["tool_response"] = json!("Success. Updated the following files:\nM a.txt\n");
        }
        f.run("codex", event)
    };
    assert_eq!(patch("PreToolUse").coverage, "admitted");
    fs::write(f.root().join("a.txt"), "hi\n").unwrap();
    assert_eq!(patch("PostToolUse").coverage, "observed");
    let stop = f.run(
        "codex",
        json!({"session_id": ALICE, "turn_id": "turn-1", "cwd": f.root(),
               "hook_event_name": "Stop", "model": "gpt-test", "permission_mode": "default",
               "stop_hook_active": false, "last_assistant_message": "done",
               "transcript_path": null}),
    );
    assert_eq!(stop.coverage, "stopped", "{}", stop.detail);
    assert!(stop.stdout.is_empty(), "a stop prints nothing");
    let tree = f.tree();
    assert_eq!(f.wait_for_checks(1)[0].tree_oid, tree);
}

#[test]
fn a_command_that_looks_like_it_holds_a_secret_is_refused() {
    let f = Fixture::new(100);
    let (code, _, stderr) = f.cli(&[
        "check", "auto", "work", "--name", "tests", "--", "make", "test", "--token", "abc123",
    ]);
    assert_ne!(code, 0);
    assert!(stderr.contains("secret"), "{stderr}");
    assert!(f.state().auto_checks.is_empty());
}

#[test]
fn a_check_that_changes_the_tree_itself_is_recorded_inconclusive_and_said_so() {
    // The tree is taken here, well inside the debounce, before the check
    // starts and adds its file.
    let f = Fixture::new(3000);
    f.enable(
        60,
        r#"echo run >> "$0"; echo generated > generated.txt; exit 0"#,
    );
    f.write_batch("call-1", "one\n");
    let tree = f.tree();
    let checks = f.wait_for_checks(1);
    assert_eq!(checks[0].result, CheckResult::Inconclusive {});
    assert_eq!(checks[0].tree_oid, tree, "the tree it started on");
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(f.log_lines(), ["run"], "no session wrote: not run again");
    let context = f.prompt("p1").context().expect("delivered");
    assert!(context.contains("was inconclusive"), "{context}");
}

/// A `cargo test`-shaped program that fails on a schedule: its runs are
/// counted in the log named by `$0`, outside the checkout, and the runs in
/// `failing` (from 0) report `net::times_out` failing. A run in `slow`
/// first sleeps for 1.5 s.
fn scheduled(failing: &[u32], slow: &[u32]) -> String {
    scheduled_sleeping(failing, slow, "1.5")
}

/// As `scheduled`, with a run in `slow` sleeping `seconds` first.
fn scheduled_sleeping(failing: &[u32], slow: &[u32], seconds: &str) -> String {
    let list = |runs: &[u32]| {
        if runs.is_empty() {
            "none".to_owned()
        } else {
            runs.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("|")
        }
    };
    // One line: an automatic check's arguments hold no control characters.
    format!(
        concat!(
            r#"n=$(cat "$0" 2>/dev/null | wc -l | tr -d ' '); echo run >> "$0"; "#,
            r#"case "$n" in {slow}) sleep {seconds};; esac; "#,
            r#"echo "running 2 tests"; echo "test parser::keeps_order ... ok"; "#,
            r#"case "$n" in {failing}) echo "test net::times_out ... FAILED"; "#,
            r#"echo "test result: FAILED. 1 passed; 1 failed"; exit 101;; "#,
            r#"*) echo "test net::times_out ... ok"; echo "test result: ok. 2 passed; 0 failed"; exit 0;; esac"#,
        ),
        failing = list(failing),
        slow = list(slow),
        seconds = seconds,
    )
}

/// What the daemon learned about `test` of check `tests`: its flips and
/// runs on unchanged trees.
fn learned(f: &Fixture, test: &str) -> Option<(usize, usize)> {
    let state = f.state();
    let history = state
        .test_learning
        .get("work")?
        .tests
        .get("tests")?
        .get(test)?;
    Some((
        history.runs.iter().filter(|flipped| **flipped).count(),
        history.runs.len(),
    ))
}

/// What `land` asks before it pushes: whether a passing check of `tests`
/// is recorded for exactly `tree`.
fn ready_to_land(f: &Fixture, tree: &str) -> bool {
    let request = Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::LandingReadiness {
            workspace_id: "work".into(),
            name: "tests".into(),
            tree_oid: tree.into(),
        },
    };
    match client::call(&f.state_dir(), &request).unwrap().outcome {
        Outcome::Readiness { ready, .. } => ready,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failure_that_passes_on_a_rerun_of_the_identical_tree_records_one_flip() {
    let f = Fixture::with_config(json!({"auto_check_debounce_ms": 100, "flaky_rerun_max_s": 30}));
    f.enable(60, &scheduled(&[0], &[]));
    f.write_batch("call-1", "one\n");
    let checks = f.wait_for_checks(1);
    // The record is the first run's failure; the rerun that passed is no
    // record of its own.
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].result, CheckResult::Failed { exit_code: 101 });
    assert_eq!(f.log_lines().len(), 2, "run once more on the same tree");
    assert_eq!(learned(&f, "net::times_out"), Some((1, 1)));
    assert_eq!(learned(&f, "parser::keeps_order"), None);
    let context = f.prompt("p1").context().expect("delivered");
    assert!(
        context.contains("1 failure named in its output: net::times_out."),
        "one flip is not yet known to flip: {context}"
    );
    assert!(!ready_to_land(&f, &checks[0].tree_oid));
}

#[test]
fn a_failure_then_a_pass_after_a_write_records_no_flip() {
    // Reruns off: each run is the one after a batch of writes.
    let f = Fixture::with_config(json!({"auto_check_debounce_ms": 100, "flaky_rerun_max_s": 0}));
    f.enable(60, &scheduled(&[0], &[]));
    f.write_batch("call-1", "one\n");
    assert_eq!(
        f.wait_for_checks(1)[0].result,
        CheckResult::Failed { exit_code: 101 }
    );
    f.write_batch("call-2", "two\n");
    let checks = f.wait_for_checks(2);
    assert_eq!(checks[0].result, CheckResult::Passed {});
    assert_eq!(f.log_lines().len(), 2, "no rerun when reruns are off");
    assert_eq!(learned(&f, "net::times_out"), None);
}

#[test]
fn a_rerun_that_would_take_longer_than_its_bound_is_not_started() {
    let f = Fixture::with_config(json!({"auto_check_debounce_ms": 100, "flaky_rerun_max_s": 1}));
    f.enable(60, &scheduled(&[0], &[0]));
    f.write_batch("call-1", "one\n");
    let checks = f.wait_for_checks(1);
    assert_eq!(checks[0].result, CheckResult::Failed { exit_code: 101 });
    assert!(checks[0].duration_ms > 1000, "{}", checks[0].duration_ms);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        f.log_lines().len(),
        1,
        "the 1.5 s failure was not run again"
    );
    assert_eq!(learned(&f, "net::times_out"), None);
}

#[test]
fn a_rerun_that_hangs_is_stopped_at_the_rerun_bound_not_the_check_timeout() {
    let f = Fixture::with_config(json!({"auto_check_debounce_ms": 100, "flaky_rerun_max_s": 1}));
    // The first run fails at once; its rerun would take 30 s, well inside
    // the check's 60 s limit.
    f.enable(60, &scheduled_sleeping(&[0], &[1], "30"));
    let started = Instant::now();
    f.write_batch("call-1", "one\n");
    let checks = f.wait_for_checks(1);
    let took = started.elapsed();
    assert_eq!(checks[0].result, CheckResult::Failed { exit_code: 101 });
    assert_eq!(f.log_lines().len(), 2, "the rerun started");
    // The known failure waits at most the rerun bound (1 s) and a snapshot,
    // not the rerun's 30 s.
    assert!(took < Duration::from_secs(15), "took {took:?}");
    assert_eq!(
        learned(&f, "net::times_out"),
        None,
        "a stopped rerun teaches nothing"
    );
}

#[test]
fn a_write_undone_between_a_failure_and_its_rerun_records_no_flip() {
    let f = Fixture::with_config(json!({"auto_check_debounce_ms": 100, "flaky_rerun_max_s": 30}));
    // The first run fails at once; its rerun passes after 3 s, long enough
    // for a session to write the file and put it back.
    f.enable(60, &scheduled_sleeping(&[0], &[1], "3"));
    f.write_batch("call-1", "one\n");
    f.wait_for_log(2);
    // Written and restored inside one tool batch that has not ended: the
    // tree the rerun ends on is the one it started on.
    f.write("call-2", "changed\n");
    f.write("call-3", "one\n");
    let checks = f.wait_for_checks(1);
    assert_eq!(checks[0].result, CheckResult::Failed { exit_code: 101 });
    assert_eq!(f.log_lines().len(), 2);
    assert_eq!(
        learned(&f, "net::times_out"),
        None,
        "recorded writes between the runs: not compared"
    );
}

#[test]
fn after_two_flips_the_briefing_labels_the_failure_which_stays_failed_until_forgotten() {
    let f = Fixture::with_config(json!({"auto_check_debounce_ms": 100, "flaky_rerun_max_s": 30}));
    // Each batch's run fails and its rerun passes.
    f.enable(60, &scheduled(&[0, 2, 4], &[]));
    f.write_batch("call-1", "one\n");
    f.wait_for_checks(1);
    assert!(f.prompt("p1").context().is_some());
    f.write_batch("call-2", "two\n");
    let checks = f.wait_for_checks(2);
    assert_eq!(f.log_lines().len(), 4);
    assert_eq!(learned(&f, "net::times_out"), Some((2, 2)));
    let context = f.prompt("p2").context().expect("delivered");
    assert!(
        context.contains("tests failed (exit 101)")
            && context.contains(
                "1 failure named in its output: net::times_out (known to flip on unchanged code, 2 of 2 runs). Every one is of a test known to flip on unchanged code; the check still failed."
            ),
        "{context}"
    );
    // Still a failure: in `check status`, and to a landing.
    let (code, status, stderr) = f.cli(&["check", "status", "work"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        status.contains("failed (exit 101)") && status.contains("Applies to the working tree"),
        "{status}"
    );
    assert!(!ready_to_land(&f, &checks[0].tree_oid));
    // Listed, as text and as JSON.
    let (code, listed, stderr) = f.cli(&["check", "flaky", "work"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        listed.contains("tests  \"net::times_out\"  runs 2  flips 2  last flip"),
        "{listed}"
    );
    let (_, listed, _) = f.cli(&["check", "flaky", "work", "--name", "tests", "--json"]);
    let listed: Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(listed["tests"][0]["test"], "net::times_out", "{listed}");
    assert_eq!(listed["tests"][0]["check_name"], "tests");
    assert_eq!(listed["tests"][0]["flips"], 2);
    assert!(listed["tests"][0]["last_flip_age_ms"].is_u64(), "{listed}");
    // Forgotten: the next failure is not labelled.
    let (code, forgot, stderr) = f.cli(&[
        "check",
        "flaky",
        "work",
        "--forget",
        "net::times_out",
        "--request-id",
        "forget-1",
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert!(forgot.contains("Forgot what was learned"), "{forgot}");
    let (_, listed, _) = f.cli(&["check", "flaky", "work"]);
    assert!(listed.contains("No test is known to flip"), "{listed}");
    f.write_batch("call-3", "three\n");
    f.wait_for_checks(3);
    let context = f.prompt("p3").context().expect("delivered");
    assert!(
        context.contains("1 failure named in its output: net::times_out.")
            && !context.contains("known to flip"),
        "{context}"
    );
}

#[test]
fn forgetting_a_secret_looking_test_name_is_refused_and_never_stored() {
    let f = Fixture::new(100);
    let secret = "test_login[password=hunter2-is-the-pass]";
    let token = ["ghp_", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"].concat();
    // Through the CLI, which redacts before it sends.
    let (code, stdout, stderr) = f.cli(&[
        "check",
        "flaky",
        "work",
        "--forget",
        secret,
        "--request-id",
        "forget-secret-cli",
    ]);
    assert_ne!(code, 0, "{stdout}{stderr}");
    assert!(
        stderr.contains("looks like a secret") && !stderr.contains("hunter2"),
        "{stdout}{stderr}"
    );
    // A client that sends the request unredacted: a secret-looking test
    // name, then a secret-looking check name.
    for (id, name, test) in [
        ("forget-secret-raw", None, secret.to_owned()),
        ("forget-secret-name", Some(token.clone()), "ok::test".into()),
    ] {
        let reply = client::call(
            &f.state_dir(),
            &Request {
                version: PROTOCOL_VERSION,
                request_id: id.into(),
                command: Command::ForgetFlakyTest {
                    workspace_id: "work".into(),
                    name,
                    test,
                },
            },
        )
        .unwrap();
        let Outcome::Error { code, message } = &reply.outcome else {
            panic!("{:?}", reply.outcome)
        };
        assert_eq!(code, "secret_in_test_name", "{message}");
        assert!(!message.contains("hunter2") && !message.contains(&token));
    }
    let database = rusqlite::Connection::open(f.state_dir().join("state.sqlite3")).unwrap();
    let stored: String = database
        .query_row(
            "SELECT coalesce(group_concat(request_id || command_json || outcome_json), '') FROM receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!stored.contains("forget-secret"), "{stored}");
    assert!(
        !stored.contains("hunter2") && !stored.contains(&token),
        "{stored}"
    );
    let events: String = database
        .query_row(
            "SELECT coalesce(group_concat(event_json), '') FROM events",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !events.contains("hunter2") && !events.contains(&token),
        "{events}"
    );
}

#[test]
fn two_check_runs_on_one_tree_with_no_write_between_record_a_flip() {
    let f = Fixture::new(100);
    let log = f.log();
    let script = scheduled(&[0], &[]);
    let run = || {
        f.cli(&[
            "check",
            "run",
            "work",
            "--name",
            "tests",
            "--",
            "sh",
            "-c",
            &script,
            log.to_str().unwrap(),
        ])
    };
    let (code, stdout, stderr) = run();
    assert_eq!(code, 1, "{stdout}{stderr}");
    // The command's own output still reaches the caller, on stderr.
    assert!(
        stderr.contains("test net::times_out ... FAILED"),
        "{stderr}"
    );
    assert_eq!(learned(&f, "net::times_out"), None);
    let (code, stdout, stderr) = run();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(learned(&f, "net::times_out"), Some((1, 1)));
}

#[test]
fn two_commands_whose_summaries_are_equal_are_not_compared() {
    let f = Fixture::new(100);
    let log = f.log();
    let script = scheduled(&[0], &[]);
    // The recorded command line is cut at 120 characters, inside the
    // script, so these two commands share it; they differ after it.
    assert!(script.len() > 120);
    let argv = |extra: &str| -> Vec<String> {
        ["sh", "-c", &script, log.to_str().unwrap(), extra]
            .map(str::to_owned)
            .to_vec()
    };
    assert_eq!(
        langspan::landing::command_line(&argv("first")),
        langspan::landing::command_line(&argv("second")),
        "one summary for both"
    );
    let run = |extra: &str| {
        let mut args: Vec<String> = ["check", "run", "work", "--name", "tests", "--"]
            .map(str::to_owned)
            .to_vec();
        args.extend(argv(extra));
        f.cli(&args.iter().map(String::as_str).collect::<Vec<_>>())
    };
    let (code, stdout, stderr) = run("first");
    assert_eq!(code, 1, "{stdout}{stderr}");
    let (code, stdout, stderr) = run("second");
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(
        learned(&f, "net::times_out"),
        None,
        "a failure of one command and a pass of another are no flip"
    );
    // A command that looks like it holds a secret sends no test names, so
    // no digest of it is stored: fail, then pass, is learned from nothing.
    let own_log = log.with_file_name("secret-runs.log");
    for expected in [1, 0] {
        let args = [
            "check",
            "run",
            "work",
            "--name",
            "secret",
            "--",
            "sh",
            "-c",
            &script,
            own_log.to_str().unwrap(),
            "--token",
            "hunter2",
        ];
        let (code, stdout, stderr) = f.cli(&args);
        assert_eq!(code, expected, "{stdout}{stderr}");
    }
    let state = f.state();
    assert!(
        state
            .test_learning
            .get("work")
            .is_none_or(|learning| !learning.last.contains_key("secret")),
        "{:?}",
        state.test_learning
    );
}

#[test]
fn a_test_skipped_in_a_passing_run_between_two_failures_records_no_flip() {
    let f = Fixture::new(100);
    let log = f.log();
    // Lines as `cargo test` prints them: the second run passes as a whole
    // with the failing test ignored, so it says nothing about that test.
    let script = concat!(
        r#"n=$(cat "$0" 2>/dev/null | wc -l | tr -d ' '); echo run >> "$0"; "#,
        r#"echo "running 3 tests"; echo "test needs_net ... ignored, needs network"; "#,
        r#"echo "test parser::keeps_order ... ok"; "#,
        r#"case "$n" in 1) echo "test net::times_out ... ignored"; "#,
        r#"echo "test result: ok. 1 passed; 0 failed; 2 ignored"; exit 0;; "#,
        r#"*) echo "test net::times_out ... FAILED"; "#,
        r#"echo "test result: FAILED. 1 passed; 1 failed; 1 ignored"; exit 101;; esac"#,
    );
    let run = || {
        f.cli(&[
            "check",
            "run",
            "work",
            "--name",
            "tests",
            "--",
            "sh",
            "-c",
            script,
            log.to_str().unwrap(),
        ])
    };
    for expected in [1, 0, 1] {
        let (code, stdout, stderr) = run();
        assert_eq!(code, expected, "{stdout}{stderr}");
    }
    assert_eq!(
        learned(&f, "net::times_out"),
        None,
        "a skipped test neither passed nor failed: fail, skip, fail is no flip"
    );
    assert_eq!(learned(&f, "needs_net"), None);
}

#[test]
fn a_check_run_across_a_written_and_restored_file_is_not_compared_with_the_next() {
    let f = Fixture::new(100);
    let log = f.log();
    // The first run fails after 3 s, long enough for a session to write the
    // file and put it back; the second passes at once.
    let script = scheduled_sleeping(&[0], &[0], "3");
    let run = || {
        f.cli(&[
            "check",
            "run",
            "work",
            "--name",
            "tests",
            "--",
            "sh",
            "-c",
            &script,
            log.to_str().unwrap(),
        ])
    };
    let (code, stdout, stderr) = std::thread::scope(|scope| {
        let first = scope.spawn(run);
        let deadline = Instant::now() + Duration::from_secs(30);
        while fs::read_to_string(&log).map_or(0, |log| log.lines().count()) < 1 {
            assert!(Instant::now() < deadline, "the first run did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Recorded writes while it runs; the tree it ends on is the one it
        // started on.
        f.write("call-1", "changed\n");
        f.write("call-2", "hello\n");
        first.join().unwrap()
    });
    assert_eq!(code, 1, "{stdout}{stderr}");
    let (code, stdout, stderr) = run();
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(
        learned(&f, "net::times_out"),
        None,
        "a write recorded during the first run: the two runs are not of unchanged code"
    );
}
