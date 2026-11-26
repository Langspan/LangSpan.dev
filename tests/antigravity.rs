#![cfg(unix)]
//! The Antigravity adapter through the real hook executable, a real daemon
//! and a real Git repository. The events are the constructed fixtures in
//! `fixtures/antigravity-constructed`: built from Antigravity's documentation,
//! not captured from an Antigravity run (see their README).

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
    stdout: String,
    reply: Value,
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
}

/// A constructed fixture with its placeholder root replaced by `root`.
fn event(name: &str, root: &Path) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/antigravity-constructed")
        .join(name);
    let text = fs::read_to_string(&path).unwrap();
    serde_json::from_str(&text.replace(FIXTURE_ROOT, root.to_str().unwrap())).unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let base = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!(
            "ls-agy-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir(&base).unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
    base
}

/// A new Git repository at `root` with `a.txt` committed.
fn repository(root: &Path) {
    fs::create_dir(root).unwrap();
    git(root, &["init", "--quiet", "-b", "main"]);
    fs::write(root.join("a.txt"), "hello\n").unwrap();
    git(root, &["add", "a.txt"]);
    git(
        root,
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
}

/// One event through the user-level hook (no `--root`, no `--workspace`):
/// its coverage and its answer.
fn user_hook(state: &Path, name: &str, body: &Value) -> (String, String) {
    let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["hook", "antigravity", "--event", name, "--state-dir"])
        .arg(state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{name}: {stderr}");
    let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
    (
        format!(
            "{}: {}",
            diagnostic["coverage"].as_str().unwrap(),
            diagnostic["detail"].as_str().unwrap_or_default()
        ),
        String::from_utf8(output.stdout).unwrap(),
    )
}

impl Fixture {
    fn new() -> Self {
        let base = scratch("run");
        repository(&base.join("work"));
        let child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !base.join("state/daemon.sock").exists() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            base,
            child: Some(child),
        }
    }

    fn root(&self) -> PathBuf {
        self.base.join("work")
    }

    fn hook(&self, vendor: &str, event_name: Option<&str>, event: &Value) -> std::process::Output {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        command.args(["hook", vendor]);
        if let Some(name) = event_name {
            command.args(["--event", name]);
        }
        let mut child = command
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A hook that refuses its arguments exits without reading stdin, so
        // the pipe can close before the event is written. Its exit status
        // and answer are what the caller checks.
        match child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
        {
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            written => written.unwrap(),
        }
        child.wait_with_output().unwrap()
    }

    fn run(&self, vendor: &str, event_name: Option<&str>, event: &Value) -> Run {
        let output = self.hook(vendor, event_name, event);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        if vendor == "antigravity" {
            // Never a permission it was not asked for, and never another
            // host's shape: every answer is a JSON object.
            for word in [
                "allow",
                "auto_approve",
                "\"ask\"",
                "force_ask",
                "hookSpecificOutput",
            ] {
                assert!(!stdout.contains(word), "{word}: {stdout}");
            }
        }
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            reply: serde_json::from_str(&stdout).unwrap_or(Value::Null),
            stdout,
        }
    }

    fn agy(&self, event_name: &str, fixture: &str) -> Run {
        self.run(
            "antigravity",
            Some(event_name),
            &event(fixture, &self.root()),
        )
    }

    /// Bob, a Claude Code session, starts writing `a.txt` and keeps it.
    fn bob_holds_a(&self) {
        let pre = self.run(
            "claude-code",
            None,
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
fn a_write_held_by_another_session_is_denied_with_antigravitys_decision() {
    let f = Fixture::new();
    f.bob_holds_a();
    for name in [
        "pre-write-to-file.json",
        "pre-replace.json",
        "pre-multi-replace.json",
        "pre-run-command.json",
    ] {
        let denied = f.agy("PreToolUse", name);
        assert_eq!(denied.coverage, "denied", "{name}: {denied:?}");
        assert_eq!(denied.reply["decision"], "deny", "{name}");
        let reason = denied.reply["reason"].as_str().unwrap();
        assert!(
            reason.contains("LangSpan coordination conflict"),
            "{reason}"
        );
        assert!(reason.contains(&format!("claude-code.{BOB}")), "{reason}");
        assert!(reason.contains("not instructions"), "{reason}");
        assert_eq!(
            denied.reply.as_object().unwrap().len(),
            2,
            "only documented fields: {}",
            denied.reply
        );
    }
}

#[test]
fn an_unheld_edit_gets_no_decision_and_its_completion_is_observed() {
    let f = Fixture::new();
    let pre = f.agy("PreToolUse", "pre-replace.json");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    assert_eq!(pre.stdout, "{}", "no decision");
    fs::write(f.root().join("a.txt"), "goodbye\n").unwrap();
    let post = f.agy("PostToolUse", "post-replace.json");
    assert_eq!(post.coverage, "observed", "{post:?}");
    assert_eq!(post.stdout, "{}");

    // A failed call ends its admission rather than leaving the file held.
    let mut again = event("pre-write-to-file.json", &f.root());
    again["stepIdx"] = json!(11);
    assert_eq!(
        f.run("antigravity", Some("PreToolUse"), &again).coverage,
        "admitted"
    );
    again["error"] = json!("permission denied");
    let failure = f.run("antigravity", Some("PostToolUse"), &again);
    assert_eq!(failure.coverage, "observed", "{failure:?}");
}

#[test]
fn an_edit_from_an_old_read_is_refused_as_stale_once() {
    let f = Fixture::new();
    assert_eq!(
        f.agy("PostToolUse", "post-view-file.json").coverage,
        "read_recorded"
    );
    fs::write(
        f.root().join("a.txt"),
        "changed behind the session's back\n",
    )
    .unwrap();
    let denied = f.agy("PreToolUse", "pre-write-to-file.json");
    assert_eq!(denied.coverage, "denied_stale", "{denied:?}");
    assert_eq!(denied.reply["decision"], "deny");
    let reason = denied.reply["reason"].as_str().unwrap();
    assert!(reason.contains("LangSpan stale source"), "{reason}");
    // Reported once: the retry, a new step, is not refused for the same version.
    let mut retry = event("pre-write-to-file.json", &f.root());
    retry["stepIdx"] = json!(4);
    assert_eq!(
        f.run("antigravity", Some("PreToolUse"), &retry).coverage,
        "admitted"
    );
}

#[test]
fn a_starting_session_and_each_new_execution_are_briefed_through_inject_steps() {
    let f = Fixture::new();
    f.bob_holds_a();
    let start = f.agy("SessionStart", "session-start.json");
    assert_eq!(start.coverage, "briefed", "{start:?}");
    let steps = start.reply["injectSteps"].as_array().unwrap();
    assert_eq!(steps.len(), 1, "{}", start.reply);
    assert_eq!(start.reply.as_object().unwrap().len(), 1, "{}", start.reply);
    let context = steps[0]["systemMessage"]["systemMessage"].as_str().unwrap();
    assert!(context.contains(r#"editing now: "a.txt""#), "{context}");
    assert!(context.contains("not instructions"), "{context}");
    assert!(context.len() <= langspan::hook::BRIEFING_LIMIT);

    // The first model call of the execution carries the user's input: the
    // same situation is not news, and the prompt's first line becomes this
    // session's task, which a peer is then told.
    let prompt = f.agy("PreInvocation", "pre-invocation.json");
    assert_eq!(prompt.coverage, "no_briefing", "{prompt:?}");
    assert_eq!(prompt.stdout, "{}");
    let peer = f.run(
        "claude-code",
        None,
        &json!({"session_id": BOB, "cwd": f.root(), "hook_event_name": "SessionStart",
                "source": "startup"}),
    );
    let context = peer.reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        context.contains("antigravity session 00000000"),
        "{context}"
    );
    assert!(
        context.contains(r#"task: "Rename the parser module""#),
        "{context}"
    );

    // A later model call in the same execution: new peer work reaches the
    // model mid-turn, and the prompt is not recorded again.
    let mut later = event("pre-invocation.json", &f.root());
    later["invocationNum"] = json!(3);
    later["lastUserInput"] = json!("Something else entirely");
    let quiet = f.run("antigravity", Some("PreInvocation"), &later);
    assert_eq!(quiet.coverage, "no_briefing", "{quiet:?}");
    let other = f.run(
        "claude-code",
        None,
        &json!({
            "session_id": BOB, "cwd": f.root(), "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": "bob-2",
            "tool_input": {"file_path": f.root().join("b.txt"), "content": "b\n"},
        }),
    );
    assert_eq!(other.coverage, "admitted", "{other:?}");
    let news = f.run("antigravity", Some("PreInvocation"), &later);
    assert_eq!(news.coverage, "briefed", "{news:?}");
    let context = news.reply["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap();
    assert!(context.contains("b.txt"), "{context}");
    let peer = f.run(
        "claude-code",
        None,
        &json!({"session_id": BOB, "cwd": f.root(), "hook_event_name": "SessionStart",
                "source": "startup"}),
    );
    let told = peer.reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(!told.contains("Something else entirely"), "{told}");
}

#[test]
fn a_shell_command_is_admitted_for_its_literal_write_and_its_change_observed() {
    let f = Fixture::new();
    let pre = f.agy("PreToolUse", "pre-run-command.json");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    fs::write(f.root().join("a.txt"), "x\n").unwrap();
    let post = f.agy("PostToolUse", "post-run-command.json");
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert_eq!(post.stdout, "{}");
}

#[test]
fn a_hook_without_its_event_fails_with_the_empty_answer() {
    let f = Fixture::new();
    let mut body = event("pre-replace.json", &f.root());
    // Larger than a pipe buffer, so the hook always exits before the whole
    // event is written: it refuses without reading stdin.
    body["padding"] = json!("x".repeat(1 << 20));
    for name in [None, Some("Stop"), Some("preToolUse")] {
        let output = f.hook("antigravity", name, &body);
        assert_eq!(output.status.code(), Some(1), "{name:?}");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "{}", "{name:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("--event"), "{stderr}");
    }
    // Another host never takes one.
    let output = f.hook("cursor", Some("PreToolUse"), &body);
    assert_eq!(output.status.code(), Some(1));
}

fn langspan(state: &Path, arguments: &[&str]) -> String {
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(state)
        .args(arguments)
        .env_remove("GEMINI_CLI_HOME")
        .output()
        .unwrap();
    assert!(output.status.success(), "{arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn hooks(state: &Path, root: &Path, action: &str, extra: &[&str]) -> String {
    let root = root.to_str().unwrap();
    let mut arguments = vec!["hooks", action, "antigravity", "--root", root];
    if action == "install" {
        arguments.extend(["--workspace", "work"]);
    }
    arguments.extend(extra);
    langspan(state, &arguments)
}

// The owner's files, written as `serde_json` pretty-prints them, so that a
// round trip must give back the same bytes.
const OWN_HOOKS: &str = r#"{
  "format-on-save": {
    "PostToolUse": [
      {
        "matcher": "write_to_file",
        "hooks": [
          {
            "type": "command",
            "command": "./fmt.sh",
            "timeout": 10
          }
        ]
      }
    ]
  },
  "notify": {
    "enabled": false,
    "Stop": [
      {
        "type": "command",
        "command": "./say.sh"
      }
    ]
  }
}
"#;
const OWN_MCP: &str = r#"{
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

#[test]
fn project_install_round_trips_hooks_mcp_and_rule_keeping_the_owners_bytes() {
    let base = scratch("install");
    let root = base.join("repo");
    fs::create_dir_all(root.join(".agents/rules")).unwrap();
    let state = base.join("state");
    let own_rule = "---\ntrigger: always_on\n---\nUse tabs.\n";
    fs::write(root.join(".agents/hooks.json"), OWN_HOOKS).unwrap();
    fs::write(root.join(".agents/mcp_config.json"), OWN_MCP).unwrap();
    fs::write(root.join(".agents/rules/style.md"), own_rule).unwrap();

    let dry = hooks(&state, &root, "install", &["--skill"]);
    assert!(dry.contains("Dry run"), "{dry}");
    assert_eq!(
        fs::read_to_string(root.join(".agents/hooks.json")).unwrap(),
        OWN_HOOKS
    );
    assert!(!root.join(".agents/rules/langspan.md").exists());

    let applied = hooks(&state, &root, "install", &["--skill", "--apply"]);
    assert!(
        applied.contains("not yet verified on a real Antigravity"),
        "{applied}"
    );
    let installed: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".agents/hooks.json")).unwrap())
            .unwrap();
    let owner: Value = serde_json::from_str(OWN_HOOKS).unwrap();
    assert_eq!(installed["format-on-save"], owner["format-on-save"]);
    assert_eq!(installed["notify"], owner["notify"]);
    let ours = &installed["langspan"];
    for (event, entry) in [
        ("PreToolUse", &ours["PreToolUse"][0]["hooks"][0]),
        ("PostToolUse", &ours["PostToolUse"][0]["hooks"][0]),
        ("SessionStart", &ours["SessionStart"][0]),
        ("PreInvocation", &ours["PreInvocation"][0]),
    ] {
        let command = entry["command"].as_str().unwrap();
        assert!(
            command.contains(&format!(" hook antigravity --event {event} ")),
            "{command}"
        );
        assert_eq!(entry["timeout"], 5);
    }
    let mcp: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".agents/mcp_config.json")).unwrap())
            .unwrap();
    assert_eq!(mcp["mcpServers"]["other"]["command"], "npx");
    let server = &mcp["mcpServers"]["langspan"];
    assert!(server.get("type").is_none(), "{server}");
    assert!(server["command"].as_str().unwrap().ends_with("/langspan"));
    assert_eq!(
        server["args"],
        json!(["mcp", "--state-dir", std::path::absolute(&state).unwrap()])
    );
    let rule = fs::read_to_string(root.join(".agents/rules/langspan.md")).unwrap();
    assert!(rule.starts_with("---\ntrigger: always_on\n"), "{rule}");

    // The installed command runs as Antigravity would run it: through a
    // shell, with the event on stdin.
    let command = ours["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let mut child = Process::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"conversationId\":\"c\"}")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "{}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unsupported"),
        "the hook ran"
    );

    let again = hooks(&state, &root, "install", &["--skill", "--apply"]);
    assert_eq!(again.matches("already matches").count(), 3, "{again}");

    let removed = hooks(&state, &root, "remove", &["--apply"]);
    assert!(removed.contains("Removed"), "{removed}");
    assert_eq!(
        fs::read_to_string(root.join(".agents/hooks.json")).unwrap(),
        OWN_HOOKS,
        "byte for byte"
    );
    assert_eq!(
        fs::read_to_string(root.join(".agents/mcp_config.json")).unwrap(),
        OWN_MCP,
        "byte for byte"
    );
    assert!(!root.join(".agents/rules/langspan.md").exists());
    assert_eq!(
        fs::read_to_string(root.join(".agents/rules/style.md")).unwrap(),
        own_rule
    );
    let twice = hooks(&state, &root, "remove", &["--apply"]);
    assert_eq!(twice.matches("already matches").count(), 2, "{twice}");

    // From nothing and back: the files LangSpan created hold nothing of ours.
    let fresh = base.join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    hooks(&state, &fresh, "install", &["--apply"]);
    hooks(&state, &fresh, "remove", &["--apply"]);
    for file in [".agents/hooks.json", ".agents/mcp_config.json"] {
        let left: Value =
            serde_json::from_str(&fs::read_to_string(fresh.join(file)).unwrap()).unwrap();
        assert_eq!(left, json!({}), "{file}");
    }
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn doctor_reports_the_project_antigravity_install() {
    let f = Fixture::new();
    let state = f.base.join("state");
    hooks(&state, &f.root(), "install", &["--skill", "--apply"]);
    let home = f.base.join("home");
    let root = f.root();
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(&state)
        .args(["doctor", "--root"])
        .arg(&root)
        .arg("--home")
        .arg(&home)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains("antigravity hooks installed"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.starts_with("ok "), "{line}");
    assert!(line.contains("4 event(s)"), "{line}");
    assert!(
        line.contains("not yet verified on a real Antigravity"),
        "{line}"
    );
    for expected in [
        "antigravity: MCP server `langspan` registered in",
        "antigravity: the LangSpan skill is installed and up to date",
    ] {
        assert!(
            text.lines()
                .any(|line| line.starts_with("ok ") && line.contains(expected)),
            "{expected}: {text}"
        );
    }

    // An event removed by hand is reported, with the command that fixes it.
    let path = root.join(".agents/hooks.json");
    let mut installed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    installed["langspan"]
        .as_object_mut()
        .unwrap()
        .remove("PreInvocation");
    fs::write(&path, serde_json::to_string_pretty(&installed).unwrap()).unwrap();
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(&state)
        .args(["doctor", "--root"])
        .arg(&root)
        .arg("--home")
        .arg(&home)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains("antigravity hooks installed"))
        .unwrap();
    assert!(
        line.starts_with("FIX ") && line.contains("not for PreInvocation"),
        "{line}"
    );

    // A definition switched off runs none of its hooks: doctor says so.
    installed["langspan"]["enabled"] = json!(false);
    fs::write(&path, serde_json::to_string_pretty(&installed).unwrap()).unwrap();
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(&state)
        .args(["doctor", "--root"])
        .arg(&root)
        .arg("--home")
        .arg(&home)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.lines().any(|line| line.starts_with("FIX ")
            && line.contains("\"enabled\": false, so none of LangSpan's hooks there run")),
        "{text}"
    );
}

#[test]
fn the_user_level_hook_steps_aside_for_the_workspaces_project_install() {
    let f = Fixture::new();
    let state = f.base.join("state");
    hooks(&state, &f.root(), "install", &["--apply"]);
    fs::create_dir(f.root().join("sub")).unwrap();
    let mut shell = event("pre-run-command.json", &f.root());
    // A command run below the workspace folder: the project install is still
    // the one Antigravity runs, at the folder's `.agents/hooks.json`.
    shell["toolCall"]["args"]["Cwd"] = json!(f.root().join("sub"));
    for (name, body) in [
        ("PreToolUse", shell),
        ("PreToolUse", event("pre-replace.json", &f.root())),
        ("SessionStart", event("session-start.json", &f.root())),
    ] {
        let (coverage, stdout) = user_hook(&state, name, &body);
        assert!(
            coverage.starts_with("deferred_to_project:"),
            "{name}: {coverage}"
        );
        assert_eq!(stdout, "{}");
    }
}

#[test]
fn the_user_level_hook_decides_where_the_project_definition_is_switched_off() {
    let f = Fixture::new();
    let state = f.base.join("state");
    hooks(&state, &f.root(), "install", &["--apply"]);
    let path = f.root().join(".agents/hooks.json");
    let mut settings: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    // Antigravity runs none of a definition's hooks with `"enabled": false`,
    // so the project install covers nothing and the user-level hook decides.
    settings["langspan"]["enabled"] = json!(false);
    fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
    f.bob_holds_a();
    let (coverage, stdout) = user_hook(&state, "PreToolUse", &event("pre-replace.json", &f.root()));
    assert!(coverage.starts_with("denied:"), "{coverage}");
    let reply: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(reply["decision"], "deny", "{stdout}");

    // Switched back on, the project install covers the workspace again.
    settings["langspan"]["enabled"] = json!(true);
    fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
    let (coverage, _) = user_hook(&state, "PreToolUse", &event("pre-replace.json", &f.root()));
    assert!(coverage.starts_with("deferred_to_project:"), "{coverage}");
}

#[test]
fn a_user_level_conversation_is_coordinated_in_each_of_its_workspace_folders() {
    let f = Fixture::new();
    let state = f.base.join("state");
    let second = f.base.join("second");
    repository(&second);
    let folders = json!([f.root(), second]);
    let with_folders = |name: &str, root: &Path| {
        let mut body = event(name, root);
        body["workspacePaths"] = folders.clone();
        body
    };
    let (coverage, _) = user_hook(
        &state,
        "SessionStart",
        &with_folders("session-start.json", &f.root()),
    );
    assert!(!coverage.starts_with("unsupported:"), "{coverage}");
    let (coverage, _) = user_hook(
        &state,
        "PreInvocation",
        &with_folders("pre-invocation.json", &f.root()),
    );
    assert!(!coverage.starts_with("unsupported:"), "{coverage}");

    // A read in the second folder is recorded for the conversation there, so
    // an edit from that old read is refused as stale.
    let (coverage, _) = user_hook(
        &state,
        "PostToolUse",
        &with_folders("post-view-file.json", &second),
    );
    assert!(coverage.starts_with("read_recorded:"), "{coverage}");
    fs::write(second.join("a.txt"), "changed behind its back\n").unwrap();
    let mut stale = with_folders("pre-replace.json", &second);
    stale["stepIdx"] = json!(20);
    let (coverage, stdout) = user_hook(&state, "PreToolUse", &stale);
    assert!(coverage.starts_with("denied_stale:"), "{coverage}");
    assert!(stdout.contains("LangSpan stale source"), "{stdout}");

    // The same conversation works in both folders: each write is admitted,
    // in its own repository, and so is a command in the second.
    for (step, name, root) in [
        (21, "pre-replace.json", f.root()),
        (22, "pre-replace.json", second.clone()),
        (23, "pre-run-command.json", second.clone()),
    ] {
        let mut body = with_folders(name, &root);
        body["stepIdx"] = json!(step);
        let (coverage, stdout) = user_hook(&state, "PreToolUse", &body);
        assert!(
            coverage.starts_with("admitted:"),
            "{name} {root:?}: {coverage}"
        );
        assert_eq!(stdout, "{}");
    }

    // Another conversation, open only on the second folder, is refused the
    // file the first one holds there.
    let mut other = event("pre-write-to-file.json", &second);
    other["conversationId"] = json!("00000000-0000-4000-8000-0000000000c2");
    let (coverage, stdout) = user_hook(&state, "PreToolUse", &other);
    assert!(coverage.starts_with("denied:"), "{coverage}");
    assert!(stdout.contains("antigravity"), "{stdout}");
}

#[test]
fn a_linked_worktree_inside_the_repository_is_its_own_workspace_folder() {
    let f = Fixture::new();
    let state = f.base.join("state");
    let worktree = f.root().join(".worktrees/topic");
    git(
        &f.root(),
        &["worktree", "add", "--quiet", "-b", "topic"]
            .into_iter()
            .chain([worktree.to_str().unwrap()])
            .collect::<Vec<_>>(),
    );
    // The outer repository first: the worktree's files lie under both
    // folders, and the more specific one is theirs.
    let folders = json!([f.root(), worktree]);
    let in_worktree = |name: &str, step: u64| {
        let mut body = event(name, &worktree);
        body["workspacePaths"] = folders.clone();
        body["stepIdx"] = json!(step);
        body
    };
    for (step, name) in [(30, "pre-replace.json"), (31, "pre-run-command.json")] {
        let (coverage, stdout) = user_hook(&state, "PreToolUse", &in_worktree(name, step));
        assert!(coverage.starts_with("admitted:"), "{name}: {coverage}");
        assert_eq!(stdout, "{}");
    }

    // Admitted under the worktree's workspace: another conversation open
    // only on the worktree is refused the file the first one holds there.
    let mut other = event("pre-write-to-file.json", &worktree);
    other["conversationId"] = json!("00000000-0000-4000-8000-0000000000c3");
    let (coverage, stdout) = user_hook(&state, "PreToolUse", &other);
    assert!(coverage.starts_with("denied:"), "{coverage}");
    assert!(stdout.contains("antigravity"), "{stdout}");
}

/// The text an Antigravity answer places in the model's context.
fn injected(stdout: &str) -> String {
    let reply: Value = serde_json::from_str(stdout).unwrap_or(Value::Null);
    reply["injectSteps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| step["systemMessage"]["systemMessage"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn messages_a_joined_briefing_has_no_room_for_still_reach_the_model_later() {
    let f = Fixture::new();
    let state = f.base.join("state");
    let second = f.base.join("second");
    repository(&second);
    let folders = json!([f.root(), second]);
    let with_folders = |name: &str, invocation: u64| {
        let mut body = event(name, &f.root());
        body["workspacePaths"] = folders.clone();
        body["invocationNum"] = json!(invocation);
        body
    };
    let (coverage, _) = user_hook(
        &state,
        "SessionStart",
        &with_folders("session-start.json", 0),
    );
    assert!(!coverage.starts_with("unsupported:"), "{coverage}");

    // Enough messages in each repository that the two briefings together
    // overflow the one limit: the first repository's take most of it.
    let mut sent = Vec::new();
    for (root, tag) in [(f.root(), "first"), (second.clone(), "second")] {
        let workspace = langspan::hook::user_workspace_id(&fs::canonicalize(&root).unwrap());
        for n in 0..3 {
            let marker = format!("{tag}-message-{n}");
            // Quotes double when quoted: each message line nears its cap.
            let text = format!("{marker} {}", "\"".repeat(300));
            let id = format!("{tag}-{n}");
            langspan(&state, &["message", &workspace, &text, "--request-id", &id]);
            sent.push(marker);
        }
    }

    // Two such messages fill one briefing, so the six take a few model
    // calls; a repository's that do not fit wait for a later one.
    let mut seen = String::new();
    for invocation in 0..6 {
        let (coverage, stdout) = user_hook(
            &state,
            "PreInvocation",
            &with_folders("pre-invocation.json", invocation),
        );
        assert!(!coverage.starts_with("unsupported:"), "{coverage}");
        let text = injected(&stdout);
        assert!(
            text.len() <= langspan::hook::BRIEFING_LIMIT,
            "{}",
            text.len()
        );
        seen.push_str(&text);
    }
    // Every message acknowledged was shown, once: none is lost to a cut.
    for marker in &sent {
        assert_eq!(
            seen.matches(marker.as_str()).count(),
            1,
            "{marker} did not reach the model exactly once: {seen}"
        );
    }
}

#[test]
fn user_install_round_trips_the_global_files_and_doctor_reports_them() {
    let base = scratch("user");
    let home = base.join("home");
    let config = home.join(".gemini/config");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("hooks.json"), OWN_HOOKS).unwrap();
    fs::write(config.join("mcp_config.json"), OWN_MCP).unwrap();
    let state = base.join("state");
    let home_arg = home.to_str().unwrap();

    // Not chosen unless named: `~/.gemini` is shared with Gemini CLI.
    fs::create_dir_all(home.join(".claude")).unwrap();
    let default = langspan(&state, &["install", "--home", home_arg]);
    assert!(!default.contains("mcp_config.json"), "{default}");

    let applied = langspan(
        &state,
        &[
            "install",
            "--home",
            home_arg,
            "--hosts",
            "antigravity",
            "--skill",
            "--apply",
        ],
    );
    assert!(
        applied.contains("not yet verified on a real Antigravity"),
        "{applied}"
    );
    let installed: Value =
        serde_json::from_str(&fs::read_to_string(config.join("hooks.json")).unwrap()).unwrap();
    let command = installed["langspan"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        command.ends_with(" # langspan-managed:antigravity:user:v1"),
        "{command}"
    );
    assert!(!command.contains("--root"), "{command}");
    let mcp: Value =
        serde_json::from_str(&fs::read_to_string(config.join("mcp_config.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["langspan"]["args"][0], "mcp");
    assert!(config.join("rules/langspan.md").is_file());
    // Gemini CLI's own files are not touched.
    assert!(!home.join(".gemini/settings.json").exists());
    assert!(!home.join(".gemini/GEMINI.md").exists());

    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(&state)
        .args(["doctor", "--home", home_arg])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "antigravity: user-level hooks installed in",
        "antigravity: MCP server `langspan` registered in",
        "antigravity: the LangSpan skill is installed and up to date",
    ] {
        assert!(
            text.lines()
                .any(|line| line.starts_with("ok ") && line.contains(expected)),
            "{expected}: {text}"
        );
    }
    assert!(text.contains("for 4 event(s)"), "{text}");

    langspan(
        &state,
        &[
            "uninstall",
            "--home",
            home_arg,
            "--hosts",
            "antigravity",
            "--apply",
        ],
    );
    assert_eq!(
        fs::read_to_string(config.join("hooks.json")).unwrap(),
        OWN_HOOKS
    );
    assert_eq!(
        fs::read_to_string(config.join("mcp_config.json")).unwrap(),
        OWN_MCP
    );
    assert!(!config.join("rules/langspan.md").exists());
    fs::remove_dir_all(&base).unwrap();
}
