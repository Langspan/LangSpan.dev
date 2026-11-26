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
/// The root of the events captured from Antigravity 1.5.14.
const CAPTURED_ROOT: &str = "/work/repo";
/// `PreToolUse`'s answer when LangSpan has no objection: Antigravity 1.5.14
/// reads `{}` there as a refusal.
const ALLOW: &str = r#"{"decision":"allow"}"#;
const CALC: &str = "def add(a, b):\n    return a + b\n\n\ndef multiply(a, b):\n    return a * b\n";
const MAIN: &str = "from calc import add, multiply\n\n\ndef main():\n    print(add(2, 3))\n    print(multiply(2, 3))\n\n\nif __name__ == \"__main__\":\n    main()\n";

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

/// An event captured from Antigravity 1.5.14, with its root replaced by
/// `root`.
fn captured_event(name: &str, root: &Path) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/antigravity-1.5.14")
        .join(name);
    let text = fs::read_to_string(&path).unwrap();
    serde_json::from_str(&text.replace(CAPTURED_ROOT, root.to_str().unwrap())).unwrap()
}

/// The `PreToolUse` of a captured call that was seen only at `PostToolUse`:
/// the same event without `error` (see the fixtures' README).
fn before(mut post: Value) -> Value {
    post.as_object_mut().unwrap().remove("error");
    post["hookEventName"] = json!("PreToolUse");
    post
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
            // host's shape: every answer is a JSON object. The one allow is
            // the explicit "no objection" `PreToolUse` needs, and only there.
            for word in [
                "auto_approve",
                "\"ask\"",
                "force_ask",
                "hookSpecificOutput",
                "overwrite",
                "permissionOverrides",
            ] {
                assert!(!stdout.contains(word), "{word}: {stdout}");
            }
            if stdout.contains("allow") {
                assert_eq!(stdout, ALLOW, "{event_name:?}");
                assert_eq!(event_name, Some("PreToolUse"), "{stdout}");
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
        self.bob_holds("a.txt");
    }

    fn bob_holds(&self, file: &str) {
        let pre = self.run(
            "claude-code",
            None,
            &json!({
                "session_id": BOB, "cwd": self.root(), "hook_event_name": "PreToolUse",
                "tool_name": "Write", "tool_use_id": format!("bob-{file}"),
                "tool_input": {"file_path": self.root().join(file), "content": "bob\n"},
            }),
        );
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
    }

    /// The repository of the captured run: `calc.py` and `main.py`.
    fn captured_repository(&self) {
        fs::write(self.root().join("calc.py"), CALC).unwrap();
        fs::write(self.root().join("main.py"), MAIN).unwrap();
        git(&self.root(), &["add", "calc.py", "main.py"]);
        git(
            &self.root(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "calc",
            ],
        );
    }

    fn captured(&self, event_name: &str, body: &Value) -> Run {
        self.run("antigravity", Some(event_name), body)
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
fn an_unheld_edit_is_allowed_and_its_completion_is_observed() {
    let f = Fixture::new();
    let pre = f.agy("PreToolUse", "pre-replace.json");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    assert_eq!(pre.stdout, ALLOW, "no objection");
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), ALLOW);
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
        let expected = if name == "PreToolUse" { ALLOW } else { "{}" };
        assert_eq!(stdout, expected, "{name}");
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
        assert_eq!(stdout, ALLOW);
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
        assert_eq!(stdout, ALLOW);
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

/// An in-flight signature (roadmap C7) that a later workspace folder's
/// share of a joined briefing has no room for is not recorded as told: it
/// reaches the model at a later call, once (review comment 4704149464).
#[test]
fn an_in_flight_line_a_joined_briefing_has_no_room_for_is_told_later() {
    let f = Fixture::new();
    let state = f.base.join("state");
    // The second folder's file calls `quota`, which only an uncommitted file
    // in a linked worktree of its repository defines.
    let second = f.base.join("second");
    fs::create_dir_all(second.join("src")).unwrap();
    fs::write(
        second.join("src/main.rs"),
        "fn main() {\n    let _ = quota(\"bob\");\n}\n",
    )
    .unwrap();
    git(&second, &["init", "--quiet", "-b", "main"]);
    git(&second, &["add", "."]);
    git(
        &second,
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
    let tree = f.base.join("tree");
    git(
        &second,
        &["worktree", "add", "--quiet", "-b", "feature"]
            .into_iter()
            .chain([tree.to_str().unwrap()])
            .collect::<Vec<_>>(),
    );
    let second = fs::canonicalize(&second).unwrap();
    let tree = fs::canonicalize(&tree).unwrap();
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
    // The conversation reads the calling file in the second folder.
    let mut read = event("post-view-file.json", &second);
    read["workspacePaths"] = folders.clone();
    read["toolCall"]["args"]["AbsolutePath"] = json!(second.join("src/main.rs"));
    let (coverage, _) = user_hook(&state, "PostToolUse", &read);
    assert!(coverage.starts_with("read_recorded:"), "{coverage}");

    // Another conversation, open on the worktree, writes `quota` there.
    let quota = tree.join("src/quota.rs");
    let content = "pub fn quota(user: &str) -> u64 {\n    user.len() as u64\n}\n";
    let mut write = event("pre-write-to-file.json", &tree);
    write["conversationId"] = json!("00000000-0000-4000-8000-0000000000c4");
    write["workspacePaths"] = json!([tree]);
    write["toolCall"]["args"]["TargetFile"] = json!(quota);
    write["toolCall"]["args"]["CodeContent"] = json!(content);
    let (coverage, _) = user_hook(&state, "PreToolUse", &write);
    assert!(coverage.starts_with("admitted:"), "{coverage}");
    fs::write(&quota, content).unwrap();
    write["error"] = json!("");
    let (coverage, _) = user_hook(&state, "PostToolUse", &write);
    assert!(coverage.starts_with("observed:"), "{coverage}");

    let workspace = langspan::hook::user_workspace_id(&second);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let listed: Value = serde_json::from_str(&langspan(
            &state,
            &["graph", "inflight", &workspace, "--json"],
        ))
        .unwrap();
        if listed["outcome"]["inflight"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
        {
            break;
        }
        assert!(Instant::now() < deadline, "quota never in flight: {listed}");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Messages in the first folder take almost all of the one limit, so the
    // second folder's share has no room for the in-flight line.
    let first = langspan::hook::user_workspace_id(&fs::canonicalize(f.root()).unwrap());
    for n in 0..2 {
        let text = format!("first-message-{n} {}", "\"".repeat(300));
        langspan(
            &state,
            &["message", &first, &text, "--request-id", &format!("m{n}")],
        );
    }
    let mut seen = String::new();
    for invocation in 1..5 {
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
        seen.push('\n');
    }
    assert!(seen.contains("first-message-1"), "{seen}");
    let told = seen
        .lines()
        .filter(|line| line.starts_with("In flight in worktree") && line.contains("\"quota\""))
        .count();
    assert_eq!(told, 1, "the in-flight line was not told once: {seen}");
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

// Antigravity 1.5.14, replayed from the events captured in a real run
// (`fixtures/antigravity-1.5.14`, issue #187).

#[test]
fn a_captured_edit_file_is_allowed_admitted_and_completed() {
    let f = Fixture::new();
    f.captured_repository();
    let post = captured_event("post-edit-file.json", &f.root());
    let pre = f.captured("PreToolUse", &before(post.clone()));
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    assert_eq!(pre.stdout, ALLOW);
    fs::write(
        f.root().join("main.py"),
        MAIN.replace("add(2, 3)", "add(2, 4)"),
    )
    .unwrap();
    let done = f.captured("PostToolUse", &post);
    assert_eq!(done.coverage, "observed", "{done:?}");
    assert_eq!(done.stdout, "{}", "PostToolUse takes no decision");
}

#[test]
fn a_captured_edit_file_of_a_held_file_is_refused_with_the_conflict() {
    let f = Fixture::new();
    f.captured_repository();
    f.bob_holds("main.py");
    let pre = f.captured(
        "PreToolUse",
        &before(captured_event("post-edit-file.json", &f.root())),
    );
    assert_eq!(pre.coverage, "denied", "{pre:?}");
    assert_eq!(pre.reply["decision"], "deny");
    let reason = pre.reply["reason"].as_str().unwrap();
    assert!(
        reason.contains("LangSpan coordination conflict"),
        "{reason}"
    );
    assert!(reason.contains(r#"["main.py"]"#), "{reason}");
    assert!(reason.contains(&format!("claude-code.{BOB}")), "{reason}");

    // The shell write of the same run, to the same held file.
    let shell = f.captured(
        "PreToolUse",
        &captured_event("pre-run-command-append.json", &f.root()),
    );
    assert_eq!(shell.coverage, "denied", "{shell:?}");
    assert_eq!(shell.reply["decision"], "deny");
}

#[test]
fn a_captured_read_file_then_an_outside_change_makes_the_edit_stale() {
    let f = Fixture::new();
    f.captured_repository();
    let read = f.captured(
        "PostToolUse",
        &captured_event("post-read-file.json", &f.root()),
    );
    assert_eq!(read.coverage, "read_recorded", "{read:?}");
    assert_eq!(read.stdout, "{}");
    // The run's outside change to `multiply`.
    fs::write(
        f.root().join("calc.py"),
        CALC.replace("return a * b", "result = a * b\n    return result"),
    )
    .unwrap();
    // The same conversation's edit, from the version it read.
    let edit = before(captured_event("post-edit-file-failed.json", &f.root()));
    let stale = f.captured("PreToolUse", &edit);
    assert_eq!(stale.coverage, "denied_stale", "{stale:?}");
    assert_eq!(stale.reply["decision"], "deny");
    let reason = stale.reply["reason"].as_str().unwrap();
    assert!(reason.contains("LangSpan stale source"), "{reason}");
    assert!(reason.contains("calc.py"), "{reason}");
}

#[test]
fn a_captured_harmless_run_command_is_allowed() {
    let f = Fixture::new();
    f.captured_repository();
    let pre = f.captured(
        "PreToolUse",
        &captured_event("pre-run-command.json", &f.root()),
    );
    assert_eq!(pre.stdout, ALLOW, "{pre:?}");
    assert!(
        !pre.coverage.starts_with("denied") && pre.coverage != "unsupported",
        "{pre:?}"
    );
}

#[test]
fn a_resumed_conversation_registers_at_its_first_pre_invocation_and_is_named_plainly() {
    let f = Fixture::new();
    f.captured_repository();
    // No SessionStart: a resumed conversation's first event.
    let first = f.captured(
        "PreInvocation",
        &captured_event("pre-invocation-resumed.json", &f.root()),
    );
    assert!(
        !matches!(first.coverage.as_str(), "unsupported" | "unknown"),
        "{first:?}"
    );
    let peer = f.run(
        "claude-code",
        None,
        &json!({"session_id": BOB, "cwd": f.root(), "hook_event_name": "SessionStart",
                "source": "startup"}),
    );
    let context = peer.reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        context.contains("- antigravity session 00000000"),
        "{context}"
    );
    // Its workspace suffix is not a subagent.
    assert!(!context.contains("subagent"), "{context}");
    assert!(
        context.contains(r#"task: "Now add a one-line docstring to multiply"#),
        "{context}"
    );
}

#[test]
fn the_installed_command_resolves_the_workspace_from_the_event_not_its_directory() {
    let f = Fixture::new();
    f.captured_repository();
    let state = f.base.join("state");
    hooks(&state, &f.root(), "install", &["--apply"]);
    let installed: Value =
        serde_json::from_str(&fs::read_to_string(f.root().join(".agents/hooks.json")).unwrap())
            .unwrap();
    let command = installed["langspan"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    // Antigravity 1.5.14 runs a hook through a shell, in `<workspace>/.agents`.
    let mut child = Process::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(f.root().join(".agents"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let body = before(captured_event("post-edit-file.json", &f.root()));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), ALLOW, "{stderr}");
    assert!(stderr.contains(r#""coverage":"admitted""#), "{stderr}");
}

#[test]
fn the_installed_matchers_name_the_tools_antigravity_1_5_14_sends() {
    let base = scratch("matchers");
    let root = base.join("repo");
    fs::create_dir_all(&root).unwrap();
    hooks(&base.join("state"), &root, "install", &["--apply"]);
    let installed: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".agents/hooks.json")).unwrap())
            .unwrap();
    let names = |event: &str| -> Vec<String> {
        installed["langspan"][event][0]["matcher"]
            .as_str()
            .unwrap()
            .strip_prefix("^(")
            .and_then(|rest| rest.strip_suffix(")$"))
            .unwrap()
            .split('|')
            .map(str::to_owned)
            .collect()
    };
    let pre = names("PreToolUse");
    let post = names("PostToolUse");
    for tool in [
        "edit_file",
        "run_command",
        "write_to_file",
        "replace_file_content",
    ] {
        assert!(pre.iter().any(|name| name == tool), "{tool}: {pre:?}");
        assert!(post.iter().any(|name| name == tool), "{tool}: {post:?}");
    }
    for tool in ["read_file", "view_file"] {
        assert!(!pre.iter().any(|name| name == tool), "{tool}: {pre:?}");
        assert!(post.iter().any(|name| name == tool), "{tool}: {post:?}");
    }
    fs::remove_dir_all(&base).unwrap();
}

fn doctor(state: &Path, root: Option<&Path>, home: &Path) -> String {
    let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
    command.arg("--state-dir").arg(state).arg("doctor");
    if let Some(root) = root {
        command.arg("--root").arg(root);
    }
    let output = command
        .arg("--home")
        .arg(home)
        .env_remove("GEMINI_CLI_HOME")
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn an_empty_mcp_config_is_an_empty_configuration_at_both_levels() {
    let f = Fixture::new();
    let state = f.base.join("state");
    let root = f.root();
    // Antigravity leaves both files empty itself.
    fs::create_dir_all(root.join(".agents")).unwrap();
    fs::write(root.join(".agents/mcp_config.json"), "").unwrap();
    let home = f.base.join("home");
    let config = home.join(".gemini/config");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("mcp_config.json"), "").unwrap();

    // Before anything is installed, an empty file is not a problem.
    let text = doctor(&state, Some(&root), &home);
    assert!(
        !text
            .lines()
            .any(|line| line.starts_with("FIX ") && line.contains("mcp_config.json")),
        "{text}"
    );

    let dry = hooks(&state, &root, "install", &[]);
    assert!(dry.contains("Dry run"), "{dry}");
    hooks(&state, &root, "install", &["--apply"]);
    let home_arg = home.to_str().unwrap();
    langspan(
        &state,
        &[
            "install",
            "--home",
            home_arg,
            "--hosts",
            "antigravity",
            "--apply",
        ],
    );
    for file in [
        root.join(".agents/mcp_config.json"),
        config.join("mcp_config.json"),
    ] {
        let mcp: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(mcp["mcpServers"]["langspan"]["args"][0], "mcp", "{file:?}");
    }
    let text = doctor(&state, Some(&root), &home);
    for expected in [
        "antigravity hooks installed",
        "antigravity: MCP server `langspan` registered in",
        "antigravity: user-level hooks installed in",
    ] {
        assert!(
            text.lines()
                .any(|line| line.starts_with("ok ") && line.contains(expected)),
            "{expected}: {text}"
        );
    }

    // An empty file is nothing to remove, not an error.
    fs::write(root.join(".agents/mcp_config.json"), "\n").unwrap();
    let removed = hooks(&state, &root, "remove", &["--apply"]);
    assert!(!removed.contains("not valid JSON"), "{removed}");
}

#[test]
fn doctor_reports_fix_for_matchers_that_name_none_of_the_edit_tools() {
    let f = Fixture::new();
    let state = f.base.join("state");
    let root = f.root();
    let home = f.base.join("home");
    hooks(&state, &root, "install", &["--apply"]);
    let path = root.join(".agents/hooks.json");
    let mut installed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    // The matchers the first install wrote, which name no tool 1.5.14 edits
    // or reads with.
    installed["langspan"]["PreToolUse"][0]["matcher"] =
        json!("^(write_to_file|replace_file_content|multi_replace_file_content|run_command)$");
    installed["langspan"]["PostToolUse"][0]["matcher"] = json!(
        "^(write_to_file|replace_file_content|multi_replace_file_content|run_command|view_file)$"
    );
    fs::write(&path, serde_json::to_string_pretty(&installed).unwrap()).unwrap();
    let text = doctor(&state, Some(&root), &home);
    let line = text
        .lines()
        .find(|line| line.contains("antigravity hooks installed"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.starts_with("FIX "), "{line}");
    assert!(line.contains("PreToolUse, PostToolUse"), "{line}");
    assert!(line.contains("hooks install antigravity"), "{line}");

    // Installing again writes this version's matchers.
    hooks(&state, &root, "install", &["--apply"]);
    let text = doctor(&state, Some(&root), &home);
    let line = text
        .lines()
        .find(|line| line.contains("antigravity hooks installed"))
        .unwrap();
    assert!(line.starts_with("ok "), "{line}");
}

#[test]
fn doctor_reports_fix_for_user_level_matchers_that_name_none_of_the_edit_tools() {
    let base = scratch("user-matchers");
    let home = base.join("home");
    let config = home.join(".gemini/config");
    fs::create_dir_all(&config).unwrap();
    let state = base.join("state");
    let home_arg = home.to_str().unwrap();
    langspan(
        &state,
        &[
            "install",
            "--home",
            home_arg,
            "--hosts",
            "antigravity",
            "--apply",
        ],
    );
    let path = config.join("hooks.json");
    let mut installed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    installed["langspan"]["PreToolUse"][0]["matcher"] = json!("^(run_command)$");
    fs::write(&path, serde_json::to_string_pretty(&installed).unwrap()).unwrap();
    let text = doctor(&state, None, &home);
    let line = text
        .lines()
        .find(|line| line.contains("antigravity: user-level hooks installed in"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.starts_with("FIX "), "{line}");
    assert!(line.contains("PreToolUse"), "{line}");
    fs::remove_dir_all(&base).unwrap();
}
