#![cfg(unix)]
//! `langspan today` (roadmap H4) through the real CLI, hooks, daemon and
//! MCP adapter, in a real Git repository. "Yesterday" is everything before
//! a `--since` taken mid-test: the window's start is the simulated midnight.
//! The default window (local midnight) is covered by the core's tests with
//! an injected clock and by the adapter's unit tests.

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

use langspan::protocol::Request;
use langspan_core::{Command, Outcome, ToolResult};
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use rmcp::{ServiceExt, model::ContentBlock};

#[allow(dead_code)] // Only the wall-clock wait is used here.
mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-today-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("work/src")).unwrap();
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let world = Self { base, daemon: None };
        fs::write(world.work().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        world.git(&["init", "-q", "-b", "main"]);
        world.git(&["add", "."]);
        world.git(&["commit", "-q", "-m", "start"]);
        world
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn state(&self) -> PathBuf {
        self.base.join("state")
    }
    fn start(&mut self) {
        self.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.state())
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
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn cli_full(&self, args: &[&str]) -> (i32, String, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let (code, out, _) = self.cli_full(args);
        (code, out)
    }
    fn today_json(&self, args: &[&str]) -> Value {
        let mut all = vec!["today"];
        all.extend_from_slice(args);
        all.push("--json");
        let (code, out, err) = self.cli_full(&all);
        assert_eq!(code, 0, "{out}{err}");
        serde_json::from_str::<Value>(&out).unwrap()["outcome"]["today"].clone()
    }
    fn git(&self, args: &[&str]) {
        let output = Process::new("git")
            .arg("-C")
            .arg(self.work())
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
    /// One hook event; the reply's JSON, if any.
    fn hook(&self, event: Value) -> Value {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }
    fn lifecycle(&self, session: &str, name: &str) -> Value {
        let mut event = json!({"session_id": session, "cwd": self.work(), "hook_event_name": name});
        if name == "UserPromptSubmit" {
            event["prompt"] = json!("carry on with the parser");
        }
        if name == "SessionStart" {
            event["source"] = json!("startup");
        }
        self.hook(event)
    }
    fn write_call(&self, session: &str, call: &str, file: &str, stage: &str) -> Value {
        self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": stage,
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file), "content": "x"},
        }))
    }
    fn edits(&self, session: &str, call: &str, file: &str) {
        self.write_call(session, call, file, "PreToolUse");
        fs::write(self.work().join(file), format!("// {session} {call}\n")).unwrap();
        self.write_call(session, call, file, "PostToolUse");
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Now as RFC 3339 UTC, to the second, from the system's own `date`.
fn now_rfc3339() -> String {
    let output = Process::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn denied(reply: &Value) -> bool {
    reply["hookSpecificOutput"]["permissionDecision"] == "deny"
}

#[test]
fn a_day_is_summarised_from_events_and_reading_it_acknowledges_nothing() {
    let mut w = World::new();
    w.start();
    let (alice, bob) = ("claude-code.alice", "claude-code.bob");
    w.lifecycle("alice", "SessionStart");
    w.lifecycle("bob", "SessionStart");
    // Yesterday: alice edits one file and is refused once.
    w.edits("alice", "y1", "src/old.rs");
    assert!(!denied(&w.write_call(
        "bob",
        "y-hold",
        "src/held.rs",
        "PreToolUse"
    )));
    assert!(denied(&w.write_call(
        "alice",
        "y-try",
        "src/held.rs",
        "PreToolUse"
    )));
    w.write_call("bob", "y-hold", "src/held.rs", "PostToolUse");
    // Midnight, in a second of its own: `--since` counts whole seconds.
    support::next_wall_second();
    let midnight = now_rfc3339();
    support::next_wall_second();
    // Today: three files (one of them twice) and two prompts.
    w.lifecycle("alice", "UserPromptSubmit");
    w.edits("alice", "t1", "src/a.rs");
    w.edits("alice", "t2", "src/b.rs");
    w.lifecycle("alice", "UserPromptSubmit");
    w.edits("alice", "t3", "src/c.rs");
    w.edits("alice", "t4", "src/a.rs");
    // Bob holds a file, unfinished; alice is refused, and the host delivers
    // the same refused call again: one refusal, counted once.
    assert!(!denied(&w.write_call(
        "bob",
        "t-hold",
        "src/lib.rs",
        "PreToolUse"
    )));
    assert!(denied(&w.write_call(
        "alice",
        "t-try",
        "src/lib.rs",
        "PreToolUse"
    )));
    w.write_call("alice", "t-try", "src/lib.rs", "PreToolUse");
    // A message to bob that he has not seen.
    let (code, out) = w.cli(&[
        "message",
        "app",
        "parse() now returns a Result",
        "--from",
        alice,
        "--to",
        bob,
        "--request-id",
        "m1",
    ]);
    assert_eq!(code, 0, "{out}");

    let today = w.today_json(&["app", "--since", &midnight]);
    let sessions = today["sessions"].as_array().unwrap();
    let alice_row = sessions
        .iter()
        .find(|row| row["session_id"] == alice)
        .unwrap_or_else(|| panic!("{today:#}"));
    assert_eq!(alice_row["files_edited"], 3, "{today:#}");
    assert_eq!(alice_row["prompts"], 2, "{today:#}");
    assert_eq!(alice_row["host"], "claude-code");
    assert_eq!(
        alice_row["files"],
        json!(["src/a.rs", "src/b.rs", "src/c.rs"])
    );
    assert_eq!(today["refusals"], json!({"conflict": 1}), "{today:#}");
    let held = today["open"]["admissions"].as_array().unwrap();
    assert!(
        held.iter()
            .any(|row| row["session_id"] == bob && row["paths"] == json!(["src/lib.rs"])),
        "{today:#}"
    );
    let messages = today["open"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "{today:#}");
    assert_eq!(messages[0]["waiting_for"], json!([bob]));
    assert_eq!(today["truncated"], false);
    assert!(
        today["window"]["from_ms"].as_u64().unwrap() <= today["window"]["to_ms"].as_u64().unwrap()
    );

    // The text: one screen, the message and the hold under Open.
    let (code, text) = w.cli(&["today", "app", "--since", &midnight]);
    assert_eq!(code, 0, "{text}");
    assert!(text.lines().count() <= 40, "{text}");
    assert!(text.contains("claude-code.alice"), "{text}");
    assert!(text.contains("3 file(s) edited, 2 prompt(s)"), "{text}");
    assert!(text.contains("Refusals: 1 conflict"), "{text}");
    assert!(
        text.contains("Open: claude-code.bob holds \"src/lib.rs\""),
        "{text}"
    );
    assert!(
        text.contains("Open: message m1 to claude-code.bob not acknowledged"),
        "{text}"
    );

    // Reading the summary acknowledged nothing: bob's next prompt is told.
    let reply = w.lifecycle("bob", "UserPromptSubmit");
    let context = reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(
        context.contains("parse() now returns a Result"),
        "{reply:#}"
    );
    let after = w.today_json(&["app", "--since", &midnight]);
    assert!(
        after["open"]["messages"].as_array().unwrap().is_empty(),
        "{after:#}"
    );

    // A window from before the oldest event holds everything, without error.
    let everything = w.today_json(&["app", "--since", "1970-01-01T00:00:00Z"]);
    let alice_row = everything["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["session_id"] == alice)
        .unwrap()
        .clone();
    assert_eq!(alice_row["files_edited"], 4, "{everything:#}");
    assert_eq!(everything["refusals"], json!({"conflict": 2}));
    // Every workspace, with no workspace named.
    assert_eq!(
        w.today_json(&["--since", &midnight])["workspace_id"],
        Value::Null
    );
    // The default window, local midnight, answers too.
    let (code, text) = w.cli(&["today", "app"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.starts_with("Today in app (since 00:00)"), "{text}");

    // A bad --since says what to write; an unknown workspace is an error.
    let (code, _, err) = w.cli_full(&["today", "app", "--since", "yesterday"]);
    assert_eq!(code, 1);
    assert!(err.contains("RFC 3339"), "{err}");
    let (code, out) = w.cli(&["today", "nowhere", "--since", &midnight]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("unknown_workspace") || out.contains("no workspace"),
        "{out}"
    );

    // A restart keeps the day: the new generation is anchored too.
    w.stop();
    let (code, _, err) = w.cli_full(&["today", "app", "--since", &midnight]);
    assert_eq!(code, 1, "the coordinator is down");
    assert!(err.contains("coordination unavailable"), "{err}");
    w.start();
    w.edits("alice", "t5", "src/d.rs");
    let restarted = w.today_json(&["app", "--since", &midnight]);
    let alice_row = restarted["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["session_id"] == alice)
        .unwrap()
        .clone();
    assert_eq!(alice_row["files_edited"], 4, "{restarted:#}");
    assert_eq!(
        restarted["undated"],
        Value::Null,
        "every generation is anchored"
    );
}

#[test]
fn today_requests_match_the_committed_golden_vectors() {
    let vectors: &[&[u8]] = &[
        include_bytes!("../fixtures/today-request-v3.json"),
        include_bytes!("../fixtures/today-every-workspace-request-v3.json"),
    ];
    for wire in vectors {
        let request: Request = serde_json::from_slice(wire).unwrap();
        assert_eq!(request.version, langspan::protocol::OLDEST_PROTOCOL_VERSION);
        assert_eq!(langspan::protocol::encode(&request).unwrap(), *wire);
    }
    let request: Request = serde_json::from_slice(vectors[0]).unwrap();
    assert!(matches!(
        &request.command,
        Command::Today { workspace_id: Some(id), from_ms: 1_795_564_800_000 } if id == "demo"
    ));
    let request: Request = serde_json::from_slice(vectors[1]).unwrap();
    assert!(matches!(
        request.command,
        Command::Today {
            workspace_id: None,
            from_ms: 0
        }
    ));
    let unknown =
        String::from_utf8_lossy(vectors[1]).replace(r#""from_ms":0"#, r#""from_ms":0,"x":1"#);
    assert!(serde_json::from_str::<Request>(&unknown).is_err());
}

fn call(state: &std::path::Path, id: &str, command: Command) -> Outcome {
    let request = Request::new(id, command);
    langspan::client::call(state, &request).unwrap().outcome
}

#[tokio::test]
async fn the_mcp_answer_stays_within_a_briefing_with_twenty_sessions() {
    let mut w = World::new();
    w.start();
    let state = w.state();
    let root = w.work().to_string_lossy().into_owned();
    let since = now_rfc3339();
    tokio::task::spawn_blocking(support::next_wall_second)
        .await
        .unwrap();
    let setup = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            call(
                &state,
                "w",
                Command::WorkspaceRegister {
                    workspace_id: "app".into(),
                    root,
                },
            );
            for index in 0..20 {
                let session = format!("codex.session-with-a-long-name-{index:02}");
                call(
                    &state,
                    &format!("s{index}"),
                    Command::SessionRegister {
                        session_id: session.clone(),
                        workspace_id: "app".into(),
                        parent: None,
                        headless: false,
                    },
                );
                for file in 0..12 {
                    let outcome = call(
                        &state,
                        &format!("o{index}-{file}"),
                        Command::Observe {
                            session_id: session.clone(),
                            operation_id: format!("op{index}-{file}"),
                            source: "test".into(),
                            tool: "Edit".into(),
                            paths: vec![format!(
                                "src/a/rather/deep/module/path/number_{file:02}.rs"
                            )],
                            result: ToolResult::Succeeded,
                            lease_id: None,
                        },
                    );
                    assert!(matches!(outcome, Outcome::Observed { .. }), "{outcome:?}");
                }
            }
        })
    };
    setup.await.unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command.arg("mcp").arg("--state-dir").arg(&state);
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let request = CallToolRequestParams::new("langspan_today").with_arguments(
        json!({"workspace_id": "app", "since": since})
            .as_object()
            .unwrap()
            .clone(),
    );
    let result = tokio::time::timeout(Duration::from_secs(10), client.call_tool(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let text: String = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    assert!(text.len() <= 2400, "{} bytes:\n{text}", text.len());
    assert!(text.starts_with("LangSpan daily summary."), "{text}");
    assert!(
        text.contains("coordination data"),
        "labelled as data: {text}"
    );
    assert!(text.contains("12 file(s) edited"), "{text}");
    assert!(
        text.contains("more line(s) not shown"),
        "what was cut is said: {text}"
    );
    // A window that starts after now is refused, as a tool error.
    let request = CallToolRequestParams::new("langspan_today").with_arguments(
        json!({"workspace_id": "app", "since": "2999-01-01T00:00:00Z"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let future = client.call_tool(request).await.unwrap();
    assert_eq!(
        future.is_error,
        Some(true),
        "a window starting after now: {future:?}"
    );
    client.cancel().await.unwrap();
    w.stop();
}
