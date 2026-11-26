#![cfg(unix)]
//! A session's briefings after its first carry only what changed since it was
//! last told (issue #222), through real hook processes, a real daemon and a
//! real Git repository: no repeats while peers keep to the same work, a
//! peer's change named alone, a hold repeated when the session acts on the
//! held file again, the full view on request, and the context each turn adds
//! measured in `langspan status` and held to its budget.

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{Command, Outcome, State};
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
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";
const CAROL: &str = "00000000-0000-4000-8000-00000000000c";
/// How the briefing labels each session: its host and the first eight
/// characters of its ID, which the three share; the rest tells them apart.
const BOB_LINE: &str = "codex session 00000000";
const CAROL_LINE: &str = "claude-code session 00000000";

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
}

struct Run {
    context: Option<String>,
    coverage: String,
    /// The hook's own diagnostic line, for a failure's message.
    diagnostic: String,
}

fn git(root: &Path, args: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=LangSpan test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-delta-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let root = base.join("work");
        fs::create_dir_all(root.join("src")).unwrap();
        let files = [
            "a.txt", "b.txt", "d.txt", "e.txt", "f0.txt", "f1.txt", "f2.txt", "src/c.rs",
        ];
        for file in files {
            fs::write(root.join(file), "hello\n").unwrap();
        }
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "--quiet", "-m", "fixture"]);
        let mut fixture = Self { base, child: None };
        fixture.start();
        fixture
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn root(&self) -> PathBuf {
        self.base.join("work")
    }
    fn start(&mut self) {
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&self.state_dir(), &status()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn state(&self) -> State {
        match client::call(&self.state_dir(), &status()).unwrap().outcome {
            Outcome::Status { state, .. } => state,
            other => panic!("{other:?}"),
        }
    }
    /// The peer's branch is part of what a briefing says: learn it before the
    /// first briefing, so that a scan finishing mid-test is not news.
    fn wait_for_repository(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.state().workspace_git.is_empty() {
            assert!(Instant::now() < deadline, "the repository was not observed");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn run(&self, vendor: &str, event: &Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
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
        let context = serde_json::from_str::<Value>(&stdout)
            .ok()
            .and_then(|reply| {
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_owned)
            });
        Run {
            context,
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            diagnostic: diagnostic.to_string(),
        }
    }
    fn lifecycle(&self, name: &str, session: &str, prompt: Option<&str>) -> Run {
        let mut event = json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": self.root(),
            "hook_event_name": name,
        });
        match name {
            "SessionStart" => event["source"] = json!("startup"),
            _ => event["prompt"] = json!(prompt.unwrap_or("continue")),
        }
        self.run("claude-code", &event)
    }
    fn start_session(&self, session: &str) -> Run {
        self.lifecycle("SessionStart", session, None)
    }
    fn prompt(&self, session: &str) -> Run {
        self.lifecycle("UserPromptSubmit", session, Some("carry on"))
    }
    fn claude_tool(&self, name: &str, tool: &str, session: &str, call: &str, input: Value) -> Run {
        let mut event = json!({
            "session_id": session, "cwd": self.root(), "hook_event_name": name,
            "tool_name": tool, "tool_use_id": call, "tool_input": input,
        });
        if name == "PostToolUse" && tool == "Read" {
            event["tool_response"] = json!({"stdout": "", "stderr": "", "interrupted": false});
        }
        self.run("claude-code", &event)
    }
    /// A completed Claude Code `Write` of `file`, by a session that has read
    /// it as it is; the post's own reply, which carries any mid-turn
    /// briefing. The read is of the session's own file: nothing new to say.
    fn write(&self, session: &str, call: &str, file: &str, content: &str) -> Run {
        self.read(session, &format!("{call}-read"), file);
        let input = json!({"file_path": self.root().join(file), "content": content});
        let pre = self.claude_tool("PreToolUse", "Write", session, call, input.clone());
        assert_eq!(pre.coverage, "admitted", "{}", pre.diagnostic);
        fs::write(self.root().join(file), content).unwrap();
        let post = self.claude_tool("PostToolUse", "Write", session, call, input);
        assert_eq!(post.coverage, "observed", "{}", post.diagnostic);
        post
    }
    fn read(&self, session: &str, call: &str, file: &str) -> Run {
        let input = json!({"file_path": self.root().join(file)});
        let read = self.claude_tool("PostToolUse", "Read", session, call, input);
        assert_eq!(read.coverage, "read_recorded", "{}", read.diagnostic);
        read
    }
    /// A Codex `apply_patch` of `file`, admitted and left open: Bob holds it.
    fn codex_hold(&self, session: &str, call: &str, file: &str) {
        let event = json!({
            "session_id": session, "turn_id": "turn", "transcript_path": null,
            "cwd": self.root(), "hook_event_name": "PreToolUse", "model": "example",
            "permission_mode": "default", "tool_name": "apply_patch", "tool_use_id": call,
            "tool_input": {"command": format!(
                "*** Begin Patch\n*** Update File: {file}\n@@\n-hello\n+hi\n*** End Patch"
            )},
        });
        assert_eq!(self.run("codex", &event).coverage, "admitted");
    }
    /// A Claude Code `Write` of `file` admitted after a read and not yet
    /// finished: the session holds the file.
    fn claude_hold(&self, session: &str, call: &str, file: &str) {
        self.read(session, &format!("{call}-read"), file);
        let input = json!({"file_path": self.root().join(file), "content": "theirs\n"});
        let pre = self.claude_tool("PreToolUse", "Write", session, call, input);
        assert_eq!(pre.coverage, "admitted", "{}", pre.diagnostic);
    }
    fn cli(&self, args: &[&str]) -> String {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.state_dir())
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
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

fn status() -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Status,
    }
}

/// Two peers at work before Alice starts: Bob holds `a.txt`, Carol edited
/// `b.txt` and then `src/c.rs`.
fn two_peers_at_work(f: &Fixture) {
    f.codex_hold(BOB, "bob-1", "a.txt");
    f.write(CAROL, "carol-1", "b.txt", "one\n");
    f.write(CAROL, "carol-2", "src/c.rs", "fn one() {}\n");
    f.wait_for_repository();
}

/// The text a hook added to the model's context, or none.
fn added(run: &Run) -> &str {
    run.context.as_deref().unwrap_or("")
}

#[test]
fn many_turns_while_peers_keep_to_the_same_files_repeat_nothing_after_the_first() {
    let f = Fixture::new();
    two_peers_at_work(&f);
    let first = f.start_session(ALICE);
    let context = first.context.expect("the first turn carries the full view");
    assert!(context.contains(BOB_LINE), "{context}");
    assert!(context.contains(r#"editing now: "a.txt""#), "{context}");
    assert!(context.contains(r#""b.txt""#), "{context}");
    assert!(context.contains(r#""src/c.rs""#), "{context}");

    // Carol keeps editing the same two files, in turn, so the order of her
    // recent edits keeps changing; Bob keeps holding his. Alice takes many
    // turns, each with a tool call of her own on a file nobody else touches.
    for turn in 0..6 {
        let file = if turn % 2 == 0 { "b.txt" } else { "src/c.rs" };
        f.write(
            CAROL,
            &format!("carol-again-{turn}"),
            file,
            &format!("{turn}\n"),
        );
        let prompt = f.prompt(ALICE);
        assert_eq!(
            (prompt.coverage.as_str(), added(&prompt)),
            ("no_briefing", ""),
            "turn {turn} repeated what Alice was already told"
        );
        let post = f.write(
            ALICE,
            &format!("alice-{turn}"),
            "d.txt",
            &format!("{turn}\n"),
        );
        assert_eq!(added(&post), "", "turn {turn}: a tool call repeated it");
    }
}

#[test]
fn when_a_peer_changes_something_the_next_turn_names_only_that_change() {
    let f = Fixture::new();
    two_peers_at_work(&f);
    assert!(f.start_session(ALICE).context.is_some());
    assert_eq!(added(&f.prompt(ALICE)), "");

    // Carol edits a file she had not touched.
    f.write(CAROL, "carol-3", "e.txt", "new\n");
    let changed = f.prompt(ALICE);
    let context = changed.context.expect("the change reaches the next turn");
    assert!(context.contains(r#""e.txt""#), "{context}");
    assert!(context.contains(CAROL_LINE), "{context}");
    // Nothing Alice was already told comes again: not Bob's hold, not
    // Carol's earlier files.
    for told in [r#""a.txt""#, r#""b.txt""#, r#""src/c.rs""#, BOB_LINE] {
        assert!(!context.contains(told), "{told} repeated: {context}");
    }
    // One line points at the full view instead.
    assert!(context.contains("langspan brief"), "{context}");
    assert_eq!(
        context
            .lines()
            .filter(|line| line.contains("langspan brief"))
            .count(),
        1,
        "{context}"
    );

    // The full view is there on request, every peer's work in it.
    let session = format!("claude-code.{ALICE}");
    let full = f.cli(&["brief", &session, "--request-id", "full-view"]);
    for part in [BOB_LINE, r#""a.txt""#, r#""b.txt""#, r#""e.txt""#] {
        assert!(
            full.contains(part),
            "{part} missing from the full view: {full}"
        );
    }
    // Asking did not use anything up: the next turn is still quiet.
    assert_eq!(added(&f.prompt(ALICE)), "");
}

#[test]
fn a_hold_on_a_file_the_session_works_on_repeats_when_it_acts_on_that_file_again() {
    let f = Fixture::new();
    f.write(ALICE, "alice-1", "a.txt", "mine\n");
    f.claude_hold(CAROL, "carol-1", "a.txt");
    f.wait_for_repository();
    let told = f
        .prompt(ALICE)
        .context
        .expect("Carol's hold on Alice's file");
    assert!(told.contains("shares files with your work"), "{told}");
    assert!(told.contains(r#"editing now: "a.txt""#), "{told}");

    // Alice works elsewhere: the hold is not repeated.
    let elsewhere = f.write(ALICE, "alice-2", "d.txt", "x\n");
    assert_eq!(added(&elsewhere), "");
    assert_eq!(added(&f.prompt(ALICE)), "");

    // Alice reads the held file again: the hold still applies, so it is
    // told again, beside the read.
    let again = f.read(ALICE, "alice-3", "a.txt");
    let context = again
        .context
        .expect("the hold is repeated once she acts on a.txt");
    assert!(context.contains(CAROL_LINE), "{context}");
    assert!(context.contains(r#"editing now: "a.txt""#), "{context}");
    assert!(context.contains("shares files with your work"), "{context}");
    // Once only: her next turn, without another act on a.txt, is quiet.
    assert_eq!(added(&f.prompt(ALICE)), "");
}

/// The most context LangSpan may add to a typical session per turn, on
/// average over its turns, briefings and the mid-turn notes together: about
/// 64 tokens. The session below measured 158 bytes per turn with briefings
/// of changes only, and 338 when every change repeated the full view.
const BUDGET_BYTES_PER_TURN: u64 = langspan::hook::CONTEXT_BUDGET_BYTES_PER_TURN;

#[test]
fn a_typical_session_stays_within_the_context_budget_per_turn_and_status_shows_it() {
    let f = Fixture::new();
    two_peers_at_work(&f);
    f.start_session(ALICE);
    // Twelve turns of three tool calls each; now and then a peer starts on a
    // new file, and between those it keeps to its own.
    let turns = 12;
    for turn in 0..turns {
        let file = match turn % 4 {
            3 => format!("f{}.txt", turn / 4),
            _ => "b.txt".to_owned(),
        };
        f.write(
            CAROL,
            &format!("carol-turn-{turn}"),
            &file,
            &format!("{turn}\n"),
        );
        f.prompt(ALICE);
        for call in 0..3 {
            f.write(
                ALICE,
                &format!("alice-{turn}-{call}"),
                "d.txt",
                &format!("{call}\n"),
            );
        }
    }
    let status: Value = serde_json::from_str(&f.cli(&["status", "--json"])).unwrap();
    let context =
        &status["outcome"]["state"]["activity"][format!("claude-code.{ALICE}")]["context"];
    let bytes = context["bytes"]
        .as_u64()
        .unwrap_or_else(|| panic!("{status}"));
    let measured_turns = context["turns"].as_u64().unwrap();
    assert_eq!(
        measured_turns,
        turns + 1,
        "the start and every prompt: {context}"
    );
    assert!(bytes > 0, "{context}");
    let per_turn = bytes / measured_turns;
    assert!(
        per_turn <= BUDGET_BYTES_PER_TURN,
        "{per_turn} bytes per turn, over the budget of {BUDGET_BYTES_PER_TURN}: {context}"
    );
    // The text view says it in bytes and in estimated tokens.
    let text = f.cli(&["status"]);
    let line = text
        .lines()
        .find(|line| line.contains("Added context"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.contains(&format!("{per_turn} bytes")), "{line}");
    assert!(line.contains("tokens"), "{line}");
}
