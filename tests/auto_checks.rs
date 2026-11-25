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
        fs::write(
            &config,
            json!({ "auto_check_debounce_ms": debounce_ms }).to_string(),
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let daemon = Process::new(env!("CARGO_BIN_EXE_langspan"))
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
