#![cfg(unix)]
//! The Cursor adapter through the real hook executable, a real daemon and a
//! real Git repository. The events are the constructed fixtures in
//! `fixtures/cursor-constructed`: built from Cursor's documentation and the
//! installed CLI, not captured from a Cursor run (see their README).

mod support;
use support::git;

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
const BOB: &str = "00000000-0000-4000-8000-00000000000b";
const FIXTURE_ROOT: &str = "/work/demo";

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    detail: String,
    stdout: String,
    reply: Option<Value>,
}

/// A constructed fixture with its placeholder root replaced by `root`.
fn event(name: &str, root: &Path) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/cursor-constructed")
        .join(name);
    let text = fs::read_to_string(&path).unwrap();
    serde_json::from_str(&text.replace(FIXTURE_ROOT, root.to_str().unwrap())).unwrap()
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-cursor-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let root = base.join("work");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--quiet", "-b", "main"]);
        fs::write(root.join("a.txt"), "hello\n").unwrap();
        git(&root, &["add", "a.txt"]);
        git(
            &root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        let child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Owned before anything can panic, so `Drop` stops it (issue #239).
        let fixture = Self {
            base,
            child: Some(child),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fixture.base.join("state/daemon.sock").exists() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }
    fn root(&self) -> PathBuf {
        self.base.join("work")
    }
    fn run(&self, vendor: &str, event: &Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
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
        // The adapter never allows, and never answers in another host's shape.
        assert!(!stdout.contains("\"allow\""), "{stdout}");
        if vendor == "cursor" {
            assert!(!stdout.contains("hookSpecificOutput"), "{stdout}");
        }
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            reply: serde_json::from_str(&stdout).ok(),
            stdout,
        }
    }
    fn cursor(&self, name: &str) -> Run {
        self.run("cursor", &event(name, &self.root()))
    }
    /// Bob, a Claude Code session, starts writing `a.txt` and keeps it.
    fn bob_holds_a(&self) {
        let pre = self.run(
            "claude-code",
            &json!({
                "session_id": BOB, "cwd": self.root(), "hook_event_name": "PreToolUse",
                "tool_name": "Write", "tool_use_id": "bob-1",
                "tool_input": {"file_path": self.root().join("a.txt"), "content": "bob\n"},
            }),
        );
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
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

#[test]
fn a_write_held_by_another_session_is_denied_in_cursors_permission_shape() {
    let f = Fixture::new();
    f.bob_holds_a();
    for name in ["pre-write.json", "pre-delete.json"] {
        let denied = f.cursor(name);
        assert_eq!(denied.coverage, "denied", "{name}: {denied:?}");
        let reply = denied.reply.unwrap();
        assert_eq!(reply["permission"], "deny", "{name}");
        for field in ["user_message", "agent_message"] {
            let text = reply[field].as_str().unwrap();
            assert!(text.contains("LangSpan coordination conflict"), "{text}");
            assert!(text.contains(&format!("claude-code.{BOB}")), "{text}");
            assert!(text.contains("not instructions"), "{text}");
        }
        assert_eq!(
            reply.as_object().unwrap().len(),
            3,
            "only documented fields: {reply}"
        );
    }
}

#[test]
fn an_unheld_write_gets_no_decision_and_its_completion_is_observed() {
    let f = Fixture::new();
    let pre = f.cursor("pre-write.json");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    assert!(pre.stdout.is_empty(), "no decision: {}", pre.stdout);
    fs::write(f.root().join("a.txt"), "replacement\n").unwrap();
    let post = f.cursor("post-write.json");
    assert_eq!(post.coverage, "observed", "{post:?}");

    // A failed call with no cwd falls back to the workspace root and ends
    // the admission rather than leaving it held.
    let mut again = event("pre-write.json", &f.root());
    again["tool_use_id"] = json!("tool_0009");
    assert_eq!(f.run("cursor", &again).coverage, "admitted");
    let mut failed = event("post-failure-write.json", &f.root());
    failed["tool_use_id"] = json!("tool_0009");
    let failure = f.run("cursor", &failed);
    assert_eq!(failure.coverage, "observed", "{failure:?}");
}

#[test]
fn an_edit_from_an_old_read_is_refused_as_stale_once() {
    let f = Fixture::new();
    assert_eq!(f.cursor("post-read.json").coverage, "read_recorded");
    // The file changes where no hook saw it.
    fs::write(
        f.root().join("a.txt"),
        "changed behind the session's back\n",
    )
    .unwrap();
    let denied = f.cursor("pre-write.json");
    assert_eq!(denied.coverage, "denied_stale", "{denied:?}");
    let reply = denied.reply.unwrap();
    assert_eq!(reply["permission"], "deny");
    let text = reply["agent_message"].as_str().unwrap();
    assert!(text.contains("LangSpan stale source"), "{text}");
    assert!(text.contains("changed outside any reported edit"), "{text}");
    // Reported once: the retry is not refused for the same version.
    let mut retry = event("pre-write.json", &f.root());
    retry["tool_use_id"] = json!("tool_0010");
    assert_eq!(f.run("cursor", &retry).coverage, "admitted");
}

#[test]
fn a_starting_or_prompted_session_is_briefed_through_additional_context() {
    let f = Fixture::new();
    f.bob_holds_a();
    let start = f.cursor("session-start.json");
    assert_eq!(start.coverage, "briefed", "{start:?}");
    let reply = start.reply.unwrap();
    assert_eq!(reply.as_object().unwrap().len(), 1, "{reply}");
    let context = reply["additional_context"].as_str().unwrap();
    assert!(context.contains(r#"editing now: "a.txt""#), "{context}");
    assert!(context.contains("not instructions"), "{context}");
    assert!(context.len() <= langspan::hook::BRIEFING_LIMIT);

    // The same situation at the next prompt is not news; the prompt's first
    // line is recorded as this session's task, which a peer is then told.
    let prompt = f.cursor("before-submit-prompt.json");
    assert_eq!(prompt.coverage, "no_briefing", "{prompt:?}");
    assert!(prompt.stdout.is_empty());
    let peer = f.run(
        "claude-code",
        &json!({"session_id": BOB, "cwd": f.root(), "hook_event_name": "SessionStart",
                "source": "startup"}),
    );
    let context = peer.reply.unwrap()["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(context.contains("cursor session 00000000"), "{context}");
    assert!(
        context.contains(r#"task: "Rename the parser module""#),
        "{context}"
    );

    assert_eq!(f.cursor("session-end.json").coverage, "session_ended");
}

#[test]
fn a_shell_command_is_admitted_for_its_literal_write_and_its_change_observed() {
    let f = Fixture::new();
    let pre = f.cursor("pre-shell.json");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    fs::write(f.root().join("a.txt"), "x\n").unwrap();
    let post = f.cursor("post-shell.json");
    assert_eq!(post.coverage, "shell_observed", "{post:?}");

    // Held by another session, the same literal write is refused.
    let f = Fixture::new();
    f.bob_holds_a();
    let denied = f.cursor("pre-shell.json");
    assert_eq!(denied.coverage, "denied", "{denied:?}");
    assert_eq!(denied.reply.unwrap()["permission"], "deny");
}

#[test]
fn an_identity_outside_the_grammar_is_replaced_by_a_digest_not_dropped() {
    let f = Fixture::new();
    let mut odd = event("pre-write.json", &f.root());
    odd["tool_use_id"] = json!("call_abc|fc_def ghi");
    let pre = f.run("cursor", &odd);
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    fs::write(f.root().join("a.txt"), "replacement\n").unwrap();
    odd["hook_event_name"] = json!("postToolUse");
    assert_eq!(f.run("cursor", &odd).coverage, "observed");
}

#[test]
fn the_claude_code_adapter_ignores_a_cursor_event_cursor_sends_it() {
    // Cursor runs Claude Code's hooks too, with its own payload. LangSpan's
    // Claude Code entry must then do nothing, so a tool call is never
    // coordinated twice under two identities.
    let f = Fixture::new();
    f.bob_holds_a();
    for name in ["pre-write.json", "session-start.json", "post-read.json"] {
        let run = f.run("claude-code", &event(name, &f.root()));
        assert_eq!(run.coverage, "unsupported", "{name}: {run:?}");
        assert!(run.stdout.is_empty(), "{name}: {}", run.stdout);
        assert!(run.detail.contains("Cursor"), "{name}: {}", run.detail);
    }
}

fn hooks(state: &Path, root: &Path, action: &str, apply: bool) -> String {
    let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("--state-dir")
        .arg(state)
        .args(["hooks", action, "cursor", "--root"])
        .arg(root);
    if action == "install" {
        command.args(["--workspace", "work"]);
    }
    if apply {
        command.arg("--apply");
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn install_writes_hooks_and_mcp_beside_the_users_own_and_removes_only_its_own() {
    let base = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("ls-cursor-install-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("repo");
    fs::create_dir_all(root.join(".cursor")).unwrap();
    let state = base.join("state");
    let own_hooks = r#"{
  "version": 1,
  "hooks": {
    "afterFileEdit": [
      {
        "command": ".cursor/hooks/format.sh"
      }
    ],
    "preToolUse": [
      {
        "command": "./guard.sh",
        "matcher": "Shell"
      }
    ]
  }
}
"#;
    let own_mcp = r#"{
  "mcpServers": {
    "other": {
      "command": "npx",
      "args": [
        "-y",
        "mcp-server"
      ]
    }
  }
}
"#;
    fs::write(root.join(".cursor/hooks.json"), own_hooks).unwrap();
    fs::write(root.join(".cursor/mcp.json"), own_mcp).unwrap();

    let dry = hooks(&state, &root, "install", false);
    assert!(dry.contains("Dry run"), "{dry}");
    assert_eq!(
        fs::read_to_string(root.join(".cursor/hooks.json")).unwrap(),
        own_hooks
    );

    let applied = hooks(&state, &root, "install", true);
    assert!(applied.contains("Wrote"), "{applied}");
    let installed: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".cursor/hooks.json")).unwrap())
            .unwrap();
    assert_eq!(installed["version"], 1);
    // The user's entries keep their place, first in each event.
    assert_eq!(installed["hooks"]["preToolUse"][0]["command"], "./guard.sh");
    assert_eq!(
        installed["hooks"]["afterFileEdit"][0]["command"],
        ".cursor/hooks/format.sh"
    );
    let ours = |event: &str| {
        installed["hooks"][event]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone()
    };
    for (event, matcher) in [
        ("preToolUse", Some("^(Write|Delete|Shell)$")),
        ("postToolUse", Some("^(Write|Delete|Shell|Read|Grep)$")),
        ("postToolUseFailure", Some("^(Write|Delete|Shell)$")),
        ("sessionStart", None),
        ("beforeSubmitPrompt", None),
        ("sessionEnd", None),
    ] {
        let entry = ours(event);
        let command = entry["command"].as_str().unwrap();
        assert!(command.contains(" hook cursor "), "{event}: {command}");
        assert!(
            command.starts_with("LANGSPAN_MANAGED=cursor:v1 "),
            "{command}"
        );
        assert!(
            !command.contains('#'),
            "no comment to swallow a heredoc: {command}"
        );
        assert_eq!(entry["type"], "command");
        assert_eq!(entry["timeout"], 5);
        assert_eq!(
            entry.get("matcher").and_then(Value::as_str),
            matcher,
            "{event}"
        );
        assert!(entry.get("failClosed").is_none(), "fail-open is kept");
    }
    let mcp: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".cursor/mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["other"]["command"], "npx");
    let server = &mcp["mcpServers"]["langspan"];
    assert_eq!(server["type"], "stdio");
    assert!(server["command"].as_str().unwrap().ends_with("/langspan"));
    assert_eq!(server["args"][0], "mcp");
    assert_eq!(
        server["args"][2],
        std::path::absolute(&state).unwrap().to_str().unwrap()
    );

    let again = hooks(&state, &root, "install", true);
    assert_eq!(again.matches("already matches").count(), 2, "{again}");

    let removed = hooks(&state, &root, "remove", true);
    assert!(removed.contains("Wrote"), "{removed}");
    let parse = |text: &str| serde_json::from_str::<Value>(text).unwrap();
    assert_eq!(
        parse(&fs::read_to_string(root.join(".cursor/hooks.json")).unwrap()),
        parse(own_hooks)
    );
    assert_eq!(
        parse(&fs::read_to_string(root.join(".cursor/mcp.json")).unwrap()),
        parse(own_mcp)
    );
    let twice = hooks(&state, &root, "remove", true);
    assert_eq!(twice.matches("already matches").count(), 2, "{twice}");

    // From nothing and back: the files LangSpan created hold nothing of
    // ours. Cursor's required `version` is kept (ADR 0032).
    let fresh = base.join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    hooks(&state, &fresh, "install", true);
    hooks(&state, &fresh, "remove", true);
    for (file, left) in [
        (".cursor/hooks.json", json!({"version": 1})),
        (".cursor/mcp.json", json!({})),
    ] {
        assert_eq!(
            parse(&fs::read_to_string(fresh.join(file)).unwrap()),
            left,
            "{file}"
        );
    }
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn doctor_reports_the_project_cursor_install() {
    let f = Fixture::new();
    let state = f.base.join("state");
    hooks(&state, &f.root(), "install", true);
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("--state-dir")
        .arg(&state)
        .args(["doctor", "--root"])
        .arg(f.root())
        .args(["--home"])
        .arg(f.base.join("home"))
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains("cursor hooks installed"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.starts_with("ok "), "{line}");
    assert!(line.contains("6 event(s)"), "{line}");
    assert!(line.contains("not yet verified on a real Cursor"), "{line}");
    assert!(
        text.lines().any(|line| line.starts_with("ok ")
            && line.contains("cursor: MCP server `langspan` registered in")),
        "{text}"
    );
}

#[test]
fn the_installed_command_reads_its_event_when_cursor_appends_a_heredoc() {
    // Cursor CLI 2026.11.22 runs a command hook as a shell string with the
    // event appended as a heredoc: `COMMAND <<'CURSOR_HOOK_EOF'`, the JSON,
    // then `CURSOR_HOOK_EOF` (its default "argv_heredoc" transport). A
    // trailing shell comment in COMMAND would swallow the redirection and
    // leave the JSON to run as shell code. Event text must stay data.
    let f = Fixture::new();
    let state = f.base.join("state");
    hooks(&state, &f.root(), "install", true);
    let installed: Value =
        serde_json::from_str(&fs::read_to_string(f.root().join(".cursor/hooks.json")).unwrap())
            .unwrap();
    let command = installed["hooks"]["preToolUse"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    let pwned = f.base.join("pwned");
    let mut payload = event("pre-write.json", &f.root());
    payload["tool_input"]["content"] = json!(format!("$(touch {})\n", pwned.display()));
    for shell in ["sh", "bash", "zsh"] {
        if Process::new(shell).arg("-c").arg("true").output().is_err() {
            continue;
        }
        payload["tool_use_id"] = json!(format!("heredoc-{shell}"));
        let script = format!("{command} <<'CURSOR_HOOK_EOF'\n{payload}\nCURSOR_HOOK_EOF");
        let output = Process::new(shell)
            .arg("-c")
            .arg(&script)
            .current_dir(f.root())
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{shell}: {stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        assert_eq!(diagnostic["coverage"], "admitted", "{shell}: {stderr}");
        assert!(!pwned.exists(), "{shell}: event text ran as shell code");
    }
}
