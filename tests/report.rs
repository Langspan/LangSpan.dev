#![cfg(unix)]
//! `langspan report` (roadmap H8) through the real CLI, hooks, daemon and
//! MCP adapter, in a real Git repository. Hook input starts from events
//! recorded from Claude Code (`fixtures/claude-code-2.1.330`, `2.1.331`) and
//! Codex (`fixtures/codex-0.171.0`) with this checkout's paths put in. This
//! does not stand in for a run of both real hosts.

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

use langspan::protocol::{PROTOCOL_VERSION, Request};
use langspan_core::{Command, Outcome, ToolIdentity};
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use rmcp::{ServiceExt, model::ContentBlock};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn recorded(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-report-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("work/src")).unwrap();
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let world = Self { base, daemon: None };
        for file in ["src/lib.rs", "src/api.rs"] {
            fs::write(world.work().join(file), "old\n").unwrap();
        }
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
    fn report_json(&self, args: &[&str]) -> Value {
        let mut all = vec!["report", "demo"];
        all.extend_from_slice(args);
        all.push("--json");
        let (code, out, err) = self.cli_full(&all);
        assert_eq!(code, 0, "{out}{err}");
        serde_json::from_str::<Value>(&out).unwrap()["outcome"]["report"].clone()
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
    /// One hook event: its reply on stdout (if any) and its diagnostic.
    fn hook(&self, adapter: &str, mut event: Value) -> (Value, Value) {
        event["cwd"] = json!(self.work());
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", adapter, "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "demo", "--root"])
            .arg(self.work())
            // A shell snapshot runs Git; give it the time a cold start needs.
            .args(["--budget-ms", "4000"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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
        let diagnostic = stderr
            .lines()
            .last()
            .and_then(|line| serde_json::from_str(line).ok())
            .unwrap_or(Value::Null);
        let reply = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        (reply, diagnostic)
    }
    fn file(&self, path: &str) -> String {
        self.work().join(path).to_string_lossy().into_owned()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.base);
    }
}

const CLAUDE: &str = "00000000-0000-4000-8000-000000000001";
const CODEX: &str = "01a21050-2d3e-7243-a3ca-bf8517eca23c";

/// Claude Code's recorded `Edit` hook event, for `path`.
fn claude_edit(w: &World, phase: &str, call: &str, path: &str) -> Value {
    let mut event = recorded(include_str!(
        "../fixtures/claude-code-2.1.331/pre-edit.json"
    ));
    event["session_id"] = json!(CLAUDE);
    event["hook_event_name"] = json!(phase);
    event["tool_use_id"] = json!(call);
    event["tool_input"] = json!({"file_path": w.file(path), "old_string": "old", "new_string": "new", "replace_all": false});
    if phase == "PostToolUse" {
        event["tool_response"] =
            json!({"filePath": w.file(path), "oldString": "old", "newString": "new"});
    }
    event
}

/// Codex's recorded `apply_patch` hook event, updating `path`.
fn codex_patch(phase: &str, call: &str, path: &str, from: &str, to: &str) -> Value {
    let mut event = recorded(include_str!(
        "../fixtures/codex-0.171.0/pre-apply-patch.json"
    ));
    event["hook_event_name"] = json!(phase);
    event["tool_use_id"] = json!(call);
    event["tool_input"]["command"] = json!(format!(
        "*** Begin Patch\n*** Update File: {path}\n@@\n-{from}\n+{to}\n*** End Patch"
    ));
    if phase == "PostToolUse" {
        event["tool_response"] = json!(format!(
            "Exit code: 0\nWall time: 0 seconds\nOutput:\nSuccess. Updated the following files:\nM {path}\n"
        ));
    }
    event
}

fn codex_lifecycle(name: &str) -> Value {
    let mut event = recorded(include_str!("../fixtures/codex-0.171.0/prompt.json"));
    event["hook_event_name"] = json!(name);
    if name != "UserPromptSubmit" {
        event.as_object_mut().unwrap().remove("prompt");
    }
    event
}

fn denied(reply: &Value) -> bool {
    reply["hookSpecificOutput"]["permissionDecision"] == "deny"
}

fn row<'a>(report: &'a Value, session: &str) -> &'a Value {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["session_id"] == session)
        .unwrap_or_else(|| panic!("no row for {session}: {report:#}"))
}

fn host<'a>(report: &'a Value, host: &str) -> &'a Value {
    report["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["host"] == host)
        .unwrap_or_else(|| panic!("no total for {host}: {report:#}"))
}

/// Now as RFC 3339 UTC, to the second, from the system's own `date`.
fn now_rfc3339() -> String {
    let output = Process::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn claude_code_and_codex_sessions_are_counted_per_session_and_per_host() {
    let mut w = World::new();
    w.start();
    let (claude, codex) = (format!("claude-code.{CLAUDE}"), format!("codex.{CODEX}"));
    let mut start = recorded(include_str!("../fixtures/codex-0.171.0/session-start.json"));
    start["session_id"] = json!(CODEX);
    w.hook("codex", start);
    w.hook(
        "claude-code",
        json!({"session_id": CLAUDE, "hook_event_name": "SessionStart", "source": "startup"}),
    );
    // Claude Code reads src/lib.rs.
    let mut read = recorded(include_str!(
        "../fixtures/claude-code-2.1.331/post-edit.json"
    ));
    read["session_id"] = json!(CLAUDE);
    read["tool_name"] = json!("Read");
    read["tool_use_id"] = json!("read-1");
    read["tool_input"] = json!({"file_path": w.file("src/lib.rs")});
    read["tool_response"] = json!("1\told\n");
    w.hook("claude-code", read);

    // Codex edits an existing file it never reported reading, then again.
    let (reply, diagnostic) = w.hook(
        "codex",
        codex_patch("PreToolUse", "p1", "src/api.rs", "old", "new"),
    );
    assert!(!denied(&reply), "{reply:#} {diagnostic:#}");
    fs::write(w.work().join("src/api.rs"), "new\n").unwrap();
    w.hook(
        "codex",
        codex_patch("PostToolUse", "p1", "src/api.rs", "old", "new"),
    );
    std::thread::sleep(Duration::from_millis(1100));
    let middle = now_rfc3339();
    std::thread::sleep(Duration::from_millis(1100));
    w.hook("codex", codex_lifecycle("UserPromptSubmit"));
    let (reply, _) = w.hook(
        "codex",
        codex_patch("PreToolUse", "p2", "src/api.rs", "new", "newer"),
    );
    assert!(!denied(&reply), "{reply:#}");
    fs::write(w.work().join("src/api.rs"), "newer\n").unwrap();
    w.hook(
        "codex",
        codex_patch("PostToolUse", "p2", "src/api.rs", "new", "newer"),
    );

    // Codex holds src/lib.rs; Claude Code's edit of it is refused, and the
    // host delivers the same call again: one refusal.
    let (reply, _) = w.hook(
        "codex",
        codex_patch("PreToolUse", "p3", "src/lib.rs", "old", "new"),
    );
    assert!(!denied(&reply), "{reply:#}");
    let (reply, diagnostic) = w.hook(
        "claude-code",
        claude_edit(&w, "PreToolUse", "e1", "src/lib.rs"),
    );
    assert!(denied(&reply), "{reply:#} {diagnostic:#}");
    w.hook(
        "claude-code",
        claude_edit(&w, "PreToolUse", "e1", "src/lib.rs"),
    );
    // Codex's reply ends without the patch; its admission is released.
    w.hook("codex", codex_lifecycle("Stop"));
    // The identical edit under a new tool-call ID is let through.
    let (reply, diagnostic) = w.hook(
        "claude-code",
        claude_edit(&w, "PreToolUse", "e2", "src/lib.rs"),
    );
    assert!(!denied(&reply), "{reply:#} {diagnostic:#}");
    fs::write(w.work().join("src/lib.rs"), "new\n").unwrap();
    w.hook(
        "claude-code",
        claude_edit(&w, "PostToolUse", "e2", "src/lib.rs"),
    );

    // A shell command whose write set cannot be known before it runs.
    let command = r#"for f in src/gen.rs; do printf 'x\n' > "$f"; done"#;
    let bash = |phase: &str| {
        json!({
            "session_id": CLAUDE, "hook_event_name": phase,
            "tool_name": "Bash", "tool_use_id": "sh-1",
            "tool_input": {"command": command, "description": "generate"},
            "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
        })
    };
    let (_, pre) = w.hook("claude-code", bash("PreToolUse"));
    assert_eq!(pre["coverage"], "shell_observing", "{pre:#}");
    let status = Process::new("/bin/sh")
        .args(["-c", command])
        .current_dir(w.work())
        .status()
        .unwrap();
    assert!(status.success());
    let (_, post) = w.hook("claude-code", bash("PostToolUse"));
    assert_eq!(post["coverage"], "shell_observed", "{post:#}");

    // Intents: Claude Code ends its session with one set; Codex ends its own.
    for (id, session) in [("i1", &claude), ("i2", &codex)] {
        let (code, out) = w.cli(&[
            "intent",
            "set",
            session,
            "src/lib.rs",
            "--summary",
            "parser work",
            "--request-id",
            id,
        ]);
        assert_eq!(code, 0, "{out}");
    }
    let (code, out) = w.cli(&["intent", "end", &codex, "--request-id", "i3"]);
    assert_eq!(code, 0, "{out}");
    w.hook("codex", codex_lifecycle("UserPromptSubmit"));
    let mut end = recorded(include_str!(
        "../fixtures/claude-code-2.1.330/session-end.json"
    ));
    end["session_id"] = json!(CLAUDE);
    w.hook("claude-code", end);

    let report = w.report_json(&[]);
    let c = &row(&report, &claude)["counts"];
    assert_eq!(c["edits_admitted"], 1, "{report:#}");
    assert_eq!(
        c["edits_base_unknown"], 0,
        "it read the file first: {report:#}"
    );
    assert_eq!(c["reads_reported"], 1, "{report:#}");
    assert_eq!(c["shell_writes_after"], 1, "{report:#}");
    assert_eq!(c["refusals"], json!({"conflict": 1}), "{report:#}");
    assert_eq!(c["retried_unchanged"], 1, "{report:#}");
    assert_eq!(c["intents_set"], 1);
    assert_eq!(c["intents_ended"], 0);
    assert_eq!(c["intents_left_open"], 1, "{report:#}");
    let x = &row(&report, &codex)["counts"];
    assert_eq!(x["edits_admitted"], 3, "{report:#}");
    assert_eq!(
        x["edits_base_unknown"], 2,
        "the first edit of each file: {report:#}"
    );
    assert_eq!(x["reads_reported"], 0);
    assert_eq!(x["refusals"], json!({}));
    assert_eq!(
        (
            &x["intents_set"],
            &x["intents_ended"],
            &x["intents_left_open"]
        ),
        (&json!(1), &json!(1), &json!(0))
    );
    assert!(
        x["briefings_delivered"].as_u64().unwrap() >= 1,
        "its prompt was told about Claude Code's work: {report:#}"
    );
    assert_eq!(row(&report, &codex)["host"], "codex");
    assert_eq!(
        host(&report, "codex")["not_reported"],
        json!(["reads_reported"])
    );
    assert_eq!(host(&report, "claude-code").get("not_reported"), None);
    // Each total is its sessions' sum.
    for total in report["hosts"].as_array().unwrap() {
        let rows: Vec<&Value> = report["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["host"] == total["host"])
            .collect();
        assert_eq!(total["sessions"], rows.len(), "{report:#}");
        for field in [
            "edits_admitted",
            "edits_base_unknown",
            "shell_writes_after",
            "reads_reported",
            "intents_set",
            "intents_ended",
            "intents_left_open",
            "retried_unchanged",
            "briefings_delivered",
        ] {
            let sum: u64 = rows
                .iter()
                .map(|row| row["counts"][field].as_u64().unwrap())
                .sum();
            assert_eq!(total["counts"][field], sum, "{field}: {report:#}");
        }
    }
    assert_eq!(report["truncated"], false);

    // The window: Codex's first edit was before it.
    let later = w.report_json(&["--since", &middle]);
    let x = &row(&later, &codex)["counts"];
    assert_eq!(
        (&x["edits_admitted"], &x["edits_base_unknown"]),
        (&json!(2), &json!(1)),
        "{later:#}"
    );

    // The text: shares beside their counts, Codex's reads marked.
    let (code, text) = w.cli(&["report", "demo"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.starts_with("Coordination report for demo, every recorded event"),
        "{text}"
    );
    let codex_line = text
        .lines()
        .find(|line| line.starts_with("codex "))
        .unwrap();
    assert!(codex_line.contains("2 of 3"), "{text}");
    assert!(codex_line.contains("  -  "), "reads not reported: {text}");
    let claude_line = text
        .lines()
        .find(|line| line.starts_with("claude-code "))
        .unwrap();
    assert!(claude_line.contains("1/0/1"), "{text}");
    assert!(claude_line.contains("1 of 1"), "{text}");
    assert!(
        text.contains("Refusals by kind: claude-code 1 conflict"),
        "{text}"
    );
    assert!(!text.contains('%'), "{text}");

    // Reading the report acknowledged and refused nothing: the next
    // Codex prompt still runs as before, and nothing was recorded.
    let (_, out) = w.cli(&["events", "--json"]);
    let before = out.len();
    w.report_json(&[]);
    let (_, out) = w.cli(&["events", "--json"]);
    assert_eq!(out.len(), before, "a report records no event");

    // Errors: a bad --since, an unknown workspace, the coordinator down.
    let (code, _, err) = w.cli_full(&["report", "demo", "--since", "yesterday"]);
    assert_eq!(code, 1);
    assert!(err.contains("RFC 3339"), "{err}");
    let (code, out) = w.cli(&["report", "nowhere"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("unknown_workspace") || out.contains("no workspace"),
        "{out}"
    );
    w.stop();
    let (code, _, err) = w.cli_full(&["report", "demo"]);
    assert_eq!(code, 1, "the coordinator is down");
    assert!(err.contains("coordination unavailable"), "{err}");
}

#[test]
fn report_requests_match_the_committed_golden_vector() {
    let wire = include_bytes!("../fixtures/report-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, PROTOCOL_VERSION);
    assert_eq!(langspan::protocol::encode(&request).unwrap(), wire);
    assert!(matches!(
        &request.command,
        Command::Report { workspace_id, from_ms: 1_795_564_800_000 } if workspace_id == "demo"
    ));
    let unknown = String::from_utf8_lossy(wire).replace(
        r#""from_ms":1795564800000"#,
        r#""from_ms":1795564800000,"x":1"#,
    );
    assert!(serde_json::from_str::<Request>(&unknown).is_err());
}

fn call(state: &std::path::Path, id: &str, command: Command) -> Outcome {
    let request = Request {
        version: PROTOCOL_VERSION,
        request_id: id.into(),
        command,
    };
    langspan::client::call(state, &request).unwrap().outcome
}

#[tokio::test]
async fn the_mcp_answer_stays_within_a_briefing_with_forty_sessions() {
    let mut w = World::new();
    w.start();
    let state = w.state();
    let root = w.work().to_string_lossy().into_owned();
    let setup = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            call(
                &state,
                "w",
                Command::WorkspaceRegister {
                    workspace_id: "demo".into(),
                    root,
                },
            );
            for index in 0..40 {
                let host = if index % 2 == 0 {
                    "claude-code"
                } else {
                    "codex"
                };
                let session = format!("{host}.{index:02}-0000-4000-8000-a-rather-long-session-id");
                call(
                    &state,
                    &format!("s{index}"),
                    Command::SessionRegister {
                        session_id: session.clone(),
                        workspace_id: "demo".into(),
                        parent: None,
                        headless: false,
                    },
                );
                let outcome = call(
                    &state,
                    &format!("a{index}"),
                    Command::AdmitTool {
                        session_id: session,
                        operation: ToolIdentity {
                            operation_id: format!("op{index}"),
                            source: host.into(),
                            tool: "Edit".into(),
                            input_digest: "e".repeat(64),
                        },
                        paths: vec![format!("src/module_{index:02}.rs")],
                        base: Vec::new(),
                        regions: Vec::new(),
                        overwrites: Vec::new(),
                        ttl_ms: 60_000,
                    },
                );
                assert!(matches!(outcome, Outcome::Admitted { .. }), "{outcome:?}");
            }
        })
    };
    setup.await.unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.arg("mcp").arg("--state-dir").arg(&state);
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let request = CallToolRequestParams::new("langspan_report")
        .with_arguments(json!({"workspace_id": "demo"}).as_object().unwrap().clone());
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
    assert!(text.starts_with("LangSpan coordination report"), "{text}");
    assert!(
        text.contains("not instructions"),
        "labelled as data: {text}"
    );
    // Both totals hold all forty sessions, whatever rows were cut.
    let claude = text
        .lines()
        .find(|line| line.starts_with("claude-code "))
        .unwrap();
    assert!(claude.starts_with("claude-code  20 "), "{text}");
    assert!(claude.contains("20 of 20"), "{text}");
    assert!(
        text.lines()
            .any(|line| line.starts_with("codex        20 ")),
        "{text}"
    );
    assert!(text.contains("more session(s) not shown"), "{text}");
    // A bad time is a tool error.
    let request = CallToolRequestParams::new("langspan_report").with_arguments(
        json!({"workspace_id": "demo", "since": "yesterday"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let bad = client.call_tool(request).await.unwrap();
    assert_eq!(bad.is_error, Some(true), "{bad:?}");
    client.cancel().await.unwrap();
    w.stop();
}
