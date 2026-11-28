#![cfg(unix)]
//! The wider check after an interface change (issue #71, ADR 0030), through
//! the real hook executable, the real daemon, a real Git checkout of a
//! two-crate Rust workspace, the real source index and real child
//! processes. The checks are shell commands that log how they were run, so
//! that the order of runs and their arguments can be read back.

mod support;
use support::git;

use langspan::{client, protocol::Request};
use langspan_core::{CheckRecord, CheckResult, Command, Outcome, State};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-0000000000a1";
const BOB: &str = "00000000-0000-4000-8000-0000000000b2";
const WAIT: Duration = Duration::from_secs(40);
/// Ten scans at the daemon's `LANGSPAN_TEST_SCAN_EVERY_MS` of 200 ms, and
/// four urgent rescans: what a check that should not start is watched for.
const SCAN_ROUNDS: Duration = Duration::from_secs(2);

const LIB: &str = "core/src/lib.rs";
const MAIN: &str = "cli/src/main.rs";
const BEFORE: &str = "pub fn parse_upload(a: u32, b: u32) -> u32 {\n    a + b\n}\n";
const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"cli\"]\n"),
    (
        "core/Cargo.toml",
        "[package]\nname = \"core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (LIB, BEFORE),
    (
        "cli/Cargo.toml",
        "[package]\nname = \"cli\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
    ),
    (
        MAIN,
        "fn main() {\n    println!(\"{}\", core::parse_upload(1, 2));\n}\n",
    ),
];
/// The Rust files of `FILES`: what the index covers.
const INDEXABLE: usize = 2;

/// Logs `fast` and its argument count, passes.
const FAST: &str = r#"echo "fast $#" >> "$0"; exit 0"#;
/// Logs `all` and its argument count, fails as `cargo test` does.
const ALL_FAILS: &str = r#"echo "all $#" >> "$0"; echo "error[E0061]: this function takes 1 argument but 2 arguments were supplied"; exit 101"#;

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

struct Run {
    coverage: String,
    detail: String,
    stdout: String,
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

fn status() -> Request {
    Request::new("read", Command::Status)
}

impl Fixture {
    /// The workspace committed, a daemon with a short debounce, `alice`
    /// registered and `bob` working on the file that calls `parse_upload`,
    /// and the index current at that content.
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-wider-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        for (path, text) in FILES {
            let full = work.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        git(&work, &["init", "--quiet", "-b", "main"]);
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", "first"]);
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let config = state.join("config.json");
        // Turning a check on is an owner grant; the owner allows it.
        let config_json = json!({"auto_check_debounce_ms": 100, "owner_grants": true});
        fs::write(&config, config_json.to_string()).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let daemon = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_TEST_SCAN_EVERY_MS", "200")
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
        for session in [ALICE, BOB] {
            let start = fixture.run(
                "claude-code",
                json!({"session_id": session, "cwd": fixture.root(), "hook_event_name": "SessionStart",
                       "source": "startup", "transcript_path": "/dev/null"}),
            );
            assert!(!start.coverage.is_empty(), "{}", start.detail);
        }
        // Bob works on the file that calls `parse_upload`: a comment, no
        // change of interface, and no boundary, so no check of his.
        fixture.write(
            BOB,
            "bob-1",
            MAIN,
            "// The command line.\nfn main() {\n    println!(\"{}\", core::parse_upload(1, 2));\n}\n",
        );
        fixture.wait_for_index();
        fixture
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn root(&self) -> PathBuf {
        self.base.join("work")
    }
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
    /// The workspace's check records, oldest first.
    fn checks(&self) -> Vec<CheckRecord> {
        let mut checks = self.state().checks.remove("work").unwrap_or_default();
        checks.reverse();
        checks
    }
    fn wait_for_checks(&self, count: usize) -> Vec<CheckRecord> {
        let deadline = Instant::now() + WAIT;
        loop {
            let checks = self.checks();
            if checks.len() >= count {
                return checks;
            }
            assert!(
                Instant::now() < deadline,
                "{} of {count} checks recorded; runs: {:?}",
                checks.len(),
                self.log_lines()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn wait_for_log(&self, lines: usize) {
        let deadline = Instant::now() + WAIT;
        while self.log_lines().len() < lines {
            assert!(Instant::now() < deadline, "runs: {:?}", self.log_lines());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn tree(&self) -> String {
        langspan::integrate::working_tree(&self.root(), Instant::now() + Duration::from_secs(30))
            .unwrap()
            .1
    }
    fn cli(&self, args: &[&str]) -> (i32, String, String) {
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
    /// `fast` after every batch of writes, and `wide` when they changed an
    /// interface other files mention.
    fn enable(&self, fast: &str, wide: &str) {
        let log = self.log();
        let log = log.to_str().unwrap();
        let (code, stdout, stderr) = self.cli(&[
            "check",
            "auto",
            "work",
            "--name",
            "fast",
            "--",
            "sh",
            "-c",
            fast,
            log,
            "--on-interface-change",
            "all",
            "--timeout-s",
            "60",
            "--",
            "sh",
            "-c",
            wide,
            log,
        ]);
        assert_eq!(code, 0, "{stdout}{stderr}");
        assert!(stdout.contains("Automatic check fast is on"), "{stdout}");
        assert!(stdout.contains("all"), "{stdout}");
    }
    fn coverage(&self) -> Value {
        let (_, text, _) = self.cli(&["graph", "symbol", "work", "parse_upload", "--json"]);
        serde_json::from_str::<Value>(&text).unwrap()["outcome"]["coverage"].clone()
    }
    /// Until the index holds every Rust file at its current content.
    fn wait_for_index(&self) {
        let deadline = Instant::now() + WAIT;
        loop {
            let coverage = self.coverage();
            if coverage["current_files"] == INDEXABLE && coverage["indexable_files"] == INDEXABLE {
                return;
            }
            assert!(Instant::now() < deadline, "never indexed: {coverage:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn run(&self, vendor: &str, event: Value) -> Run {
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
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            stdout: String::from_utf8(output.stdout).unwrap(),
        }
    }
    /// One Claude Code Write of `path` by `session`, admitted and observed.
    fn write(&self, session: &str, call: &str, path: &str, content: &str) -> Value {
        let file = self.root().join(path);
        let input = json!({"file_path": file, "content": content});
        let tool = |name: &str| {
            json!({"session_id": session, "cwd": self.root(), "hook_event_name": name,
                   "tool_name": "Write", "tool_use_id": call, "tool_input": input})
        };
        let pre = self.run("claude-code", tool("PreToolUse"));
        assert_eq!(pre.coverage, "admitted", "{}", pre.detail);
        fs::write(&file, content).unwrap();
        let post = self.run("claude-code", tool("PostToolUse"));
        assert_eq!(post.coverage, "observed", "{}", post.detail);
        input
    }
    /// Alice writes `path`, then the batch that ends with it.
    fn write_batch(&self, call: &str, path: &str, content: &str) {
        let input = self.write(ALICE, call, path, content);
        let batch = self.run(
            "claude-code",
            json!({"session_id": ALICE, "cwd": self.root(), "hook_event_name": "PostToolBatch",
                   "prompt_id": "p0", "permission_mode": "acceptEdits",
                   "tool_calls": [{"tool_name": "Write", "tool_input": input, "tool_use_id": call,
                                   "tool_response": format!("File created successfully at: {}", self.root().join(path).display())}]}),
        );
        assert!(batch.coverage.starts_with("batch_"), "{}", batch.detail);
    }
    fn prompt(&self, session: &str, id: &str) -> Option<String> {
        self.run(
            "claude-code",
            json!({"session_id": session, "cwd": self.root(), "hook_event_name": "UserPromptSubmit",
                   "prompt": "carry on", "prompt_id": id, "permission_mode": "default"}),
        )
        .context()
    }
    /// Every briefing `session` is given over a few prompts, joined.
    fn briefings(&self, session: &str) -> String {
        (0..4)
            .filter_map(|n| self.prompt(session, &format!("{session}-p{n}")))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }
    /// The interface notice for `parse_upload` that the index reported.
    fn wait_for_notice(&self) {
        let deadline = Instant::now() + WAIT;
        loop {
            let (_, events, _) = self.cli(&["events", "--limit", "1000", "--json"]);
            if events.contains("interface_changed") && events.contains("\"name\":\"parse_upload\"")
            {
                return;
            }
            assert!(Instant::now() < deadline, "no interface notice");
            std::thread::sleep(Duration::from_millis(100));
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

#[test]
fn a_body_only_change_runs_only_the_regular_check() {
    let f = Fixture::new();
    f.enable(FAST, ALL_FAILS);
    f.write_batch(
        "a-1",
        LIB,
        "pub fn parse_upload(a: u32, b: u32) -> u32 {\n    a * b\n}\n",
    );
    let checks = f.wait_for_checks(1);
    assert_eq!(checks[0].name, "fast");
    // The index has read the new content, and several scan rounds at the
    // test's 200 ms tick pass: a notice, had there been one, would have
    // started the wider check.
    f.wait_for_index();
    support::holds_for("only the regular check ran", SCAN_ROUNDS, || {
        f.log_lines() == ["fast 0"]
    });
    assert_eq!(f.checks().len(), 1);
    let context = f.briefings(ALICE);
    assert!(context.contains("fast passed"), "{context}");
    assert!(!context.contains("Interface change"), "{context}");
}

#[test]
fn a_signature_change_runs_the_whole_wider_command_after_the_regular_one_and_grades_it() {
    let f = Fixture::new();
    f.enable(FAST, ALL_FAILS);
    f.write_batch(
        "a-1",
        LIB,
        "pub fn parse_upload(a: u32) -> u32 {\n    a\n}\n",
    );
    let tree = f.tree();
    let checks = f.wait_for_checks(2);
    assert_eq!(checks.len(), 2, "{checks:?}");
    let (fast, all) = (&checks[0], &checks[1]);
    assert_eq!(fast.name, "fast");
    assert_eq!(fast.result, CheckResult::Passed {});
    assert_eq!(all.name, "all");
    assert_eq!(all.result, CheckResult::Failed { exit_code: 101 });
    assert_eq!(fast.tree_oid, tree);
    assert_eq!(all.tree_oid, tree, "the wider check ran on the same tree");
    // In that order, and the owner's command whole: no path was added.
    assert_eq!(f.log_lines(), ["fast 0", "all 0"]);
    // The grade, the export and the files that mention it, in the event.
    let (_, events, _) = f.cli(&["events", "--limit", "1000", "--json"]);
    assert!(events.contains("\"interface_grade\":\"broke\""), "{events}");

    let alice = f.briefings(ALICE);
    assert!(
        alice.contains(&format!(
            r#"Interface change to parse_upload in {LIB}: "pub fn parse_upload(a: u32, b: u32) -> u32" -> "pub fn parse_upload(a: u32) -> u32""#
        )),
        "{alice}"
    );
    assert!(
        alice.contains("the wider check all failed (exit 101)"),
        "{alice}"
    );
    assert!(alice.contains(MAIN), "{alice}");
    assert!(
        alice.contains("a name lookup, not resolved uses"),
        "{alice}"
    );
    assert!(alice.contains("fast passed"), "{alice}");
    // Bob works on a file that mentions it: he is told at his next briefing.
    let bob = f.briefings(BOB);
    assert!(
        bob.contains("the wider check all failed (exit 101)"),
        "{bob}"
    );
    assert!(bob.contains(MAIN), "{bob}");
    // Nothing more runs.
    support::holds_for("nothing more ran", SCAN_ROUNDS, || {
        f.log_lines() == ["fast 0", "all 0"]
    });
}

#[test]
fn a_passing_wider_check_grades_the_change_kept() {
    let f = Fixture::new();
    f.enable(FAST, r#"echo "all $#" >> "$0"; exit 0"#);
    f.write_batch(
        "a-1",
        LIB,
        "pub fn parse_upload(a: u32) -> u32 {\n    a\n}\n",
    );
    let checks = f.wait_for_checks(2);
    assert_eq!(checks[1].result, CheckResult::Passed {});
    let (_, events, _) = f.cli(&["events", "--limit", "1000", "--json"]);
    assert!(events.contains("\"interface_grade\":\"kept\""), "{events}");
    let alice = f.briefings(ALICE);
    assert!(alice.contains("the wider check all passed"), "{alice}");
}

#[test]
fn a_write_during_the_wider_run_cancels_it_and_restarts_both_on_the_new_tree() {
    let f = Fixture::new();
    f.enable(
        FAST,
        r#"echo "all start" >> "$0"; sleep 4; echo "all end" >> "$0"; exit 0"#,
    );
    f.write_batch(
        "a-1",
        LIB,
        "pub fn parse_upload(a: u32) -> u32 {\n    a\n}\n",
    );
    f.wait_for_notice();
    f.wait_for_log(2);
    assert_eq!(f.log_lines(), ["fast 0", "all start"]);
    f.write_batch("a-2", "notes.txt", "a note\n");
    let tree = f.tree();
    let checks = f.wait_for_checks(3);
    assert_eq!(
        f.log_lines(),
        ["fast 0", "all start", "fast 0", "all start", "all end"]
    );
    let names: Vec<_> = checks.iter().map(|check| check.name.as_str()).collect();
    assert_eq!(
        names,
        ["fast", "fast", "all"],
        "the stopped run recorded nothing"
    );
    assert_eq!(checks[1].tree_oid, tree);
    assert_eq!(checks[2].tree_oid, tree, "the wider check of the new tree");
    assert_eq!(checks[2].result, CheckResult::Passed {});
}

#[test]
fn the_wider_command_is_shown_and_refused_with_a_secret_or_without_a_command() {
    let f = Fixture::new();
    f.enable(FAST, ALL_FAILS);
    let (code, shown, _) = f.cli(&["check", "auto", "work"]);
    assert_eq!(code, 0);
    assert!(shown.contains("Automatic check fast is on"), "{shown}");
    assert!(shown.contains("On an interface change: all"), "{shown}");
    let (code, _, stderr) = f.cli(&[
        "check",
        "auto",
        "work",
        "--name",
        "fast",
        "--",
        "true",
        "--on-interface-change",
        "all",
        "--",
        "deploy",
        "--token",
        "abc123",
    ]);
    assert_ne!(code, 0);
    assert!(stderr.contains("secret"), "{stderr}");
    let (code, _, stderr) = f.cli(&[
        "check",
        "auto",
        "work",
        "--name",
        "fast",
        "--",
        "true",
        "--on-interface-change",
        "all",
    ]);
    assert_ne!(code, 0);
    assert!(stderr.contains("--on-interface-change"), "{stderr}");
    let (code, _, stderr) = f.cli(&[
        "check",
        "auto",
        "work",
        "--name",
        "fast",
        "--",
        "true",
        "--on-interface-change",
        "fast",
        "--",
        "true",
    ]);
    assert_ne!(code, 0);
    assert!(stderr.contains("name"), "{stderr}");
}
