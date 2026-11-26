#![cfg(unix)]
//! What a session did after a refusal (roadmap H9), through the real hook
//! executable, a real daemon and real SQLite, with the files really changing
//! on disk. Hook input is constructed in the shapes the recorded Claude Code
//! and Codex events use; this does not stand in for a run of the real hosts.

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
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
    calls: AtomicU64,
}

impl Fixture {
    fn new() -> Self {
        Self::with_env(&[])
    }
    /// A daemon started with `env` set, for the test-only overrides.
    fn with_env(env: &[(&str, &str)]) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-outcome-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(base.join("work")).unwrap();
        fs::write(base.join("work/a.txt"), "hello\n").unwrap();
        fs::write(base.join("work/b.txt"), "hello\n").unwrap();
        let mut daemon = Process::new(env!("CARGO_BIN_EXE_langspan"));
        daemon
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in env {
            daemon.env(name, value);
        }
        let fixture = Self {
            child: Some(daemon.spawn().unwrap()),
            base,
            calls: AtomicU64::new(0),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fixture.base.join("state/daemon.sock").exists() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }
    fn file(&self, name: &str) -> PathBuf {
        self.base.join("work").join(name)
    }
    /// One hook run: its diagnostic's `coverage`.
    fn run(&self, vendor: &str, event: Value) -> String {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "work", "--root"])
            .arg(self.base.join("work"))
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
        diagnostic["coverage"].as_str().unwrap().into()
    }
    /// A Claude Code `Write` of `file` with `content`, as tool call `call`.
    fn claude_write(&self, phase: &str, call: &str, file: &str, content: &str) -> String {
        self.run(
            "claude-code",
            json!({
                "session_id": ALICE, "cwd": self.base.join("work"), "hook_event_name": phase,
                "tool_name": "Write", "tool_use_id": call,
                "tool_input": {"file_path": self.file(file), "content": content},
            }),
        )
    }
    /// A whole Claude Code write: admitted, written to disk, reported.
    fn claude_writes(&self, call: &str, file: &str, content: &str) -> String {
        let pre = self.claude_write("PreToolUse", call, file, content);
        if pre == "admitted" {
            fs::write(self.file(file), content).unwrap();
            self.claude_write("PostToolUse", call, file, content);
        }
        pre
    }
    fn claude_read(&self, file: &str) -> String {
        let call = format!("read-{}", self.calls.fetch_add(1, Ordering::Relaxed));
        self.run(
            "claude-code",
            json!({
                "session_id": ALICE, "cwd": self.base.join("work"),
                "hook_event_name": "PostToolUse", "tool_name": "Read", "tool_use_id": call,
                "tool_input": {"file_path": self.file(file)},
            }),
        )
    }
    fn claude_prompt(&self) {
        self.run(
            "claude-code",
            json!({"session_id": ALICE, "cwd": self.base.join("work"),
                   "hook_event_name": "UserPromptSubmit", "prompt": "carry on"}),
        );
    }
    fn claude_ends(&self) {
        self.run(
            "claude-code",
            json!({"session_id": ALICE, "cwd": self.base.join("work"),
                   "hook_event_name": "SessionEnd", "reason": "prompt_input_exit"}),
        );
    }
    fn codex_patch(&self, session: &str, phase: &str, call: &str, to: &str) -> String {
        let mut event = json!({
            "session_id": session, "turn_id": "turn", "cwd": self.base.join("work"),
            "hook_event_name": phase, "tool_name": "apply_patch", "tool_use_id": call,
            "tool_input": {"command": format!(
                "*** Begin Patch\n*** Update File: a.txt\n@@\n-hello\n+{to}\n*** End Patch")},
        });
        if phase == "PostToolUse" {
            event["tool_response"] = json!("Success. Updated the following files:\nM a.txt\n");
        }
        self.run("codex", event)
    }
    /// A whole Codex patch of `a.txt`: admitted, written to disk, reported.
    fn codex_writes(&self, session: &str, call: &str, to: &str) -> String {
        let pre = self.codex_patch(session, "PreToolUse", call, to);
        if pre == "admitted" {
            fs::write(self.file("a.txt"), format!("{to}\n")).unwrap();
            assert_eq!(
                self.codex_patch(session, "PostToolUse", call, to),
                "observed"
            );
        }
        pre
    }
    /// The coordination report's counts for `session` (a host-prefixed ID).
    fn counts(&self, session: &str) -> Value {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(["report", "work", "--json"])
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{text}");
        let reply: Value = serde_json::from_str(&text).unwrap();
        reply["outcome"]["report"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["session_id"] == session)
            .unwrap_or_else(|| panic!("no row for {session}: {text}"))["counts"]
            .clone()
    }
    fn text(&self) -> String {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(["report", "work"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn alice() -> String {
    format!("claude-code.{ALICE}")
}

fn outcomes(counts: &Value) -> &Value {
    &counts["refusal_outcomes"]
}

/// Alice reads `a.txt`, Bob changes it, and Alice's write is refused stale.
fn stale_refusal(f: &Fixture) {
    assert_eq!(f.claude_read("a.txt"), "read_recorded");
    assert_eq!(f.codex_writes(BOB, "bob-1", "hi"), "admitted");
    assert_eq!(
        f.claude_write("PreToolUse", "a-1", "a.txt", "mine\n"),
        "denied_stale"
    );
}

/// Bob (Codex) holds `a.txt`, and Alice's write of it is refused.
fn conflict_refusal(f: &Fixture, call: &str, content: &str) {
    assert_eq!(
        f.claude_write("PreToolUse", call, "a.txt", content),
        "denied"
    );
}

#[test]
fn a_stale_refusal_followed_by_a_read_and_an_edit_is_adapted() {
    let f = Fixture::new();
    stale_refusal(&f);
    assert_eq!(
        outcomes(&f.counts(&alice())),
        &json!({"pending": 1}),
        "nothing has settled it yet"
    );
    assert_eq!(f.claude_read("a.txt"), "read_recorded");
    assert_eq!(f.claude_writes("a-2", "a.txt", "mine, on hi\n"), "admitted");
    let counts = f.counts(&alice());
    assert_eq!(outcomes(&counts), &json!({"adapted": 1}), "{counts:#}");
    assert_eq!(counts["refusals"], json!({"stale_base": 1}));
    let text = f.text();
    assert!(
        text.contains("Refusal outcomes: claude-code 1 adapted"),
        "{text}"
    );
}

#[test]
fn a_refusal_followed_by_the_identical_retry_is_retried_unchanged() {
    let f = Fixture::new();
    stale_refusal(&f);
    // The same write under a new tool-call ID is let through once.
    assert_eq!(f.claude_writes("a-2", "a.txt", "mine\n"), "admitted");
    let counts = f.counts(&alice());
    assert_eq!(
        outcomes(&counts),
        &json!({"retried_unchanged": 1}),
        "{counts:#}"
    );
}

#[test]
fn a_conflict_refusal_then_the_holders_completion_then_a_write_is_waited() {
    let f = Fixture::new();
    assert_eq!(f.codex_patch(BOB, "PreToolUse", "bob-1", "hi"), "admitted");
    conflict_refusal(&f, "a-1", "mine\n");
    // Bob's patch finishes: its admission is completed (`lease_completed`).
    fs::write(f.file("a.txt"), "hi\n").unwrap();
    assert_eq!(f.codex_patch(BOB, "PostToolUse", "bob-1", "hi"), "observed");
    assert_eq!(f.claude_read("a.txt"), "read_recorded");
    assert_eq!(f.claude_writes("a-2", "a.txt", "mine, on hi\n"), "admitted");
    let counts = f.counts(&alice());
    assert_eq!(outcomes(&counts), &json!({"waited": 1}), "{counts:#}");
}

#[test]
fn no_write_within_the_window_settles_abandoned() {
    // The 30-minute window, shortened for the test only.
    let f = Fixture::with_env(&[("LANGSPAN_TEST_REFUSAL_WINDOW_MS", "1000")]);
    stale_refusal(&f);
    // A write of another file settles nothing.
    assert_eq!(f.claude_writes("b-1", "b.txt", "other\n"), "admitted");
    assert_eq!(outcomes(&f.counts(&alice())), &json!({"pending": 1}));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(outcomes(&f.counts(&alice())), &json!({"abandoned": 1}));
    // A later prompt, then a write: it changes nothing, as the refusal was
    // settled once. (An admission has no time of its own; the report places
    // it at the timed event before it, here the prompt.)
    f.claude_prompt();
    assert_eq!(f.claude_writes("a-2", "a.txt", "mine\n"), "admitted");
    assert_eq!(outcomes(&f.counts(&alice())), &json!({"abandoned": 1}));
}

#[test]
fn ending_the_session_settles_abandoned_at_once() {
    let f = Fixture::new();
    stale_refusal(&f);
    f.claude_ends();
    let counts = f.counts(&alice());
    assert_eq!(outcomes(&counts), &json!({"abandoned": 1}), "{counts:#}");
}

#[test]
fn three_refusals_of_one_session_and_path_set_are_one_outcome_settled_once() {
    // Bob (Codex) holds a.txt, and Alice is refused three different writes.
    let f = Fixture::new();
    assert_eq!(f.codex_patch(BOB, "PreToolUse", "bob-1", "hi"), "admitted");
    conflict_refusal(&f, "a-1", "one\n");
    conflict_refusal(&f, "a-2", "two\n");
    conflict_refusal(&f, "a-3", "three\n");
    assert_eq!(outcomes(&f.counts(&alice())), &json!({"pending": 1}));
    // Bob's patch finishes, and the first call is sent again.
    fs::write(f.file("a.txt"), "hi\n").unwrap();
    assert_eq!(f.codex_patch(BOB, "PostToolUse", "bob-1", "hi"), "observed");
    assert_eq!(f.claude_writes("a-4", "a.txt", "one\n"), "admitted");
    let counts = f.counts(&alice());
    assert_eq!(counts["refusals"], json!({"conflict": 3}), "{counts:#}");
    assert_eq!(outcomes(&counts), &json!({"waited": 1}), "{counts:#}");
    // Later writes and the session's end settle nothing again.
    assert_eq!(f.claude_writes("a-5", "a.txt", "four\n"), "admitted");
    f.claude_ends();
    assert_eq!(outcomes(&f.counts(&alice())), &json!({"waited": 1}));
}

#[test]
fn a_codex_identical_retry_is_unknown_read_and_a_changed_one_adapted() {
    let f = Fixture::new();
    let bob = format!("codex.{BOB}");
    // Bob's first patch records the file as he found it.
    assert_eq!(f.codex_writes(BOB, "bob-1", "hi"), "admitted");
    // Alice changes the file; Bob's next patch is refused stale.
    assert_eq!(f.claude_read("a.txt"), "read_recorded");
    assert_eq!(f.claude_writes("a-1", "a.txt", "hello\n"), "admitted");
    assert_eq!(
        f.codex_patch(BOB, "PreToolUse", "bob-2", "yo"),
        "denied_stale"
    );
    // Codex reports no reads: the identical retry cannot be told apart.
    assert_eq!(f.codex_writes(BOB, "bob-3", "yo"), "admitted");
    assert_eq!(outcomes(&f.counts(&bob)), &json!({"unknown_read": 1}));

    // Refused again; this time the patch differs from the refused one.
    assert_eq!(f.claude_read("a.txt"), "read_recorded");
    assert_eq!(f.claude_writes("a-2", "a.txt", "hello\n"), "admitted");
    assert_eq!(
        f.codex_patch(BOB, "PreToolUse", "bob-4", "hey"),
        "denied_stale"
    );
    assert_eq!(f.codex_writes(BOB, "bob-5", "hey there"), "admitted");
    assert_eq!(
        outcomes(&f.counts(&bob)),
        &json!({"adapted": 1, "unknown_read": 1})
    );
}

impl Fixture {
    /// One request written to the daemon's socket as it is, as a client of
    /// any build would send it.
    fn raw(&self, request: &Value) -> Value {
        use std::io::{BufRead, BufReader};
        let mut stream =
            std::os::unix::net::UnixStream::connect(self.base.join("state/daemon.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    /// The report of `work`, asked for in protocol `version`.
    fn report_in(&self, version: u16) -> Value {
        self.raw(&json!({
            "version": version, "request_id": format!("report-{version}"),
            "command": {"op": "report", "workspace_id": "work", "from_ms": 0},
        }))
    }
}

/// The fields of a report's counts before protocol version 5: a client of
/// version 3 or 4 reads them with `deny_unknown_fields`.
const COUNTS_BEFORE_5: [&str; 10] = [
    "edits_admitted",
    "edits_base_unknown",
    "shell_writes_after",
    "reads_reported",
    "intents_set",
    "intents_ended",
    "intents_left_open",
    "refusals",
    "retried_unchanged",
    "briefings_delivered",
];

/// Every `counts` object of a report answer: each session's and each host's.
fn every_counts(reply: &Value) -> Vec<&Value> {
    let report = &reply["outcome"]["report"];
    ["sessions", "hosts"]
        .iter()
        .flat_map(|list| report[*list].as_array().unwrap())
        .map(|row| &row["counts"])
        .collect()
}

/// Review comment 4711096877: a CLI or MCP adapter of an earlier build reads
/// a report's counts with `deny_unknown_fields`, so an answer carrying
/// `refusal_outcomes` would fail to read as a whole. Protocol version 5 adds
/// the field; clients of versions 4 and 3 get the report without it, in
/// their own version, with every other figure the same.
#[test]
fn clients_of_protocol_4_and_3_get_reports_without_refusal_outcomes() {
    let f = Fixture::new();
    stale_refusal(&f);
    // Version 5 added it, and version 6 (issue #222) keeps it.
    for version in [
        langspan::protocol::PROTOCOL_VERSION,
        langspan::protocol::PROTOCOL_VERSION_5,
    ] {
        let reply = f.report_in(version);
        assert_eq!(reply["version"], version, "{reply:#}");
        assert!(
            every_counts(&reply)
                .iter()
                .any(|counts| counts["refusal_outcomes"] == json!({"pending": 1})),
            "{reply:#}"
        );
    }
    let current = f.report_in(langspan::protocol::PROTOCOL_VERSION);
    let with = every_counts(&current);
    for version in [4, 3] {
        let reply = f.report_in(version);
        assert_eq!(reply["version"], version, "{reply:#}");
        assert_eq!(reply["outcome"]["type"], "report", "{reply:#}");
        let counts = every_counts(&reply);
        assert_eq!(counts.len(), with.len(), "{reply:#}");
        for (old, new) in counts.iter().zip(&with) {
            let keys: Vec<&str> = old
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, COUNTS_BEFORE_5, "version {version}: {reply:#}");
            let mut stripped = (*new).clone();
            stripped.as_object_mut().unwrap().remove("refusal_outcomes");
            assert_eq!(*old, &stripped, "version {version}");
        }
    }
}
