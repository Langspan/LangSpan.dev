#![cfg(unix)]
//! Gemini CLI hook events through the real hook executable, a real daemon and
//! a real Git repository. The events are the source-derived contract fixtures
//! in `fixtures/gemini-cli-contract` (Gemini CLI 0.70.0), with this test's
//! paths; they are not a replay of a live Gemini session.

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

mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);
const CLAUDE: &str = "00000000-0000-4000-8000-0000000000c1";

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    detail: String,
    stdout: Value,
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

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-gemini-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("notes.txt"), "alpha\nbeta\n").unwrap();
        fs::write(work.join("other.txt"), "other\n").unwrap();
        git(&work, &["init", "-q"]);
        git(&work, &["add", "."]);
        git(
            &work,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "-m",
                "start",
            ],
        );
        let mut fixture = Self { base, daemon: None };
        fixture.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(fixture.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let socket = fixture.state_dir().join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }

    fn hook(&self, vendor: &str, event: &Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", vendor, "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "demo", "--root"])
            .arg(self.work())
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
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("\"allow\""), "{stdout}");
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            stdout: if vendor == "gemini-cli" {
                // Never empty: Gemini would read stderr as the answer.
                serde_json::from_str(&stdout).expect("Gemini CLI always gets a JSON object")
            } else {
                serde_json::from_str(&stdout).unwrap_or(Value::Null)
            },
        }
    }

    /// A contract fixture with this fixture's paths.
    fn gemini(&self, fixture: &str) -> Value {
        let mut event: Value = serde_json::from_str(fixture).unwrap();
        event["cwd"] = json!(self.work());
        if let Some(path) = event["tool_input"]["file_path"].as_str() {
            let name = Path::new(path).file_name().unwrap().to_owned();
            event["tool_input"]["file_path"] = json!(self.work().join(name));
        }
        event
    }

    fn gemini_run(&self, fixture: &str) -> Run {
        self.hook("gemini-cli", &self.gemini(fixture))
    }

    /// A Claude Code `Write` of `file`, admitted and not finished: it holds
    /// the file.
    fn claude_holds(&self, file: &str) -> Run {
        self.hook(
            "claude-code",
            &json!({
                "session_id": CLAUDE, "cwd": self.work(), "hook_event_name": "PreToolUse",
                "tool_name": "Write", "tool_use_id": "claude-1",
                "tool_input": {"file_path": self.work().join(file), "content": "held\n"},
            }),
        )
    }

    fn session(&self) -> String {
        let event: Value = serde_json::from_str(SESSION_START).unwrap();
        format!("gemini.{}", event["session_id"].as_str().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

const SESSION_START: &str = include_str!("../fixtures/gemini-cli-contract/session-start.json");
const BEFORE_AGENT: &str = include_str!("../fixtures/gemini-cli-contract/before-agent.json");
const BEFORE_REPLACE: &str = include_str!("../fixtures/gemini-cli-contract/before-replace.json");
const AFTER_REPLACE: &str = include_str!("../fixtures/gemini-cli-contract/after-replace.json");
const BEFORE_WRITE: &str = include_str!("../fixtures/gemini-cli-contract/before-write-file.json");
const AFTER_READ: &str = include_str!("../fixtures/gemini-cli-contract/after-read-file.json");
const BEFORE_SHELL: &str = include_str!("../fixtures/gemini-cli-contract/before-shell.json");
const AFTER_SHELL: &str = include_str!("../fixtures/gemini-cli-contract/after-shell.json");
const SESSION_END: &str = include_str!("../fixtures/gemini-cli-contract/session-end.json");

#[test]
fn an_edit_to_a_file_a_peer_holds_is_denied_in_geminis_shape() {
    let f = Fixture::new();
    assert_eq!(f.claude_holds("notes.txt").coverage, "admitted");
    let denied = f.gemini_run(BEFORE_REPLACE);
    assert_eq!(denied.coverage, "denied", "{denied:?}");
    assert_eq!(denied.stdout["decision"], "deny", "{denied:?}");
    let reason = denied.stdout["reason"].as_str().unwrap();
    assert!(
        reason.contains("LangSpan coordination conflict"),
        "{reason}"
    );
    assert!(
        reason.contains(&format!("claude-code.{CLAUDE}")),
        "{reason}"
    );
    assert!(reason.contains("not instructions"), "{reason}");
    assert!(denied.stdout.get("hookSpecificOutput").is_none());

    // A write to a file nobody holds goes ahead with no decision.
    let mut elsewhere = f.gemini(BEFORE_WRITE);
    elsewhere["tool_input"]["file_path"] = json!(f.work().join("other.txt"));
    let admitted = f.hook("gemini-cli", &elsewhere);
    assert_eq!(admitted.coverage, "admitted", "{admitted:?}");
    assert_eq!(admitted.stdout, json!({}));
    // ...and Claude Code is now held off that file by the Gemini session.
    let held = f.hook(
        "claude-code",
        &json!({
            "session_id": CLAUDE, "cwd": f.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": "claude-2",
            "tool_input": {"file_path": f.work().join("other.txt"), "content": "x\n"},
        }),
    );
    assert_eq!(held.coverage, "denied", "{held:?}");
    let reason = held.stdout["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains(&f.session()), "{reason}");
}

#[test]
fn an_edit_from_a_stale_read_is_refused_once() {
    let f = Fixture::new();
    let read = f.gemini_run(AFTER_READ);
    assert_eq!(read.coverage, "read_recorded", "{read:?}");
    // The file changes after the session read it, outside any hook.
    fs::write(f.work().join("notes.txt"), "alpha\nbeta\ndelta\n").unwrap();
    let stale = f.gemini_run(BEFORE_REPLACE);
    assert_eq!(stale.coverage, "denied_stale", "{stale:?}");
    assert_eq!(stale.stdout["decision"], "deny");
    let reason = stale.stdout["reason"].as_str().unwrap();
    assert!(reason.contains("LangSpan stale source"), "{reason}");
    assert!(reason.contains("\"notes.txt\""), "{reason}");
    // Reported once: the same edit tried again is not refused for it.
    let retried = f.gemini_run(BEFORE_REPLACE);
    assert_eq!(retried.coverage, "admitted", "{retried:?}");
    fs::write(f.work().join("notes.txt"), "alpha\nBETA\ndelta\n").unwrap();
    let done = f.gemini_run(AFTER_REPLACE);
    assert_eq!(done.coverage, "observed", "{done:?}");
    assert!(done.detail.contains("admission completed"), "{done:?}");
}

#[test]
fn session_start_and_prompt_brief_the_model_about_peer_work() {
    let f = Fixture::new();
    // Nothing to tell yet: an empty object, never nothing.
    let quiet = f.gemini_run(SESSION_START);
    assert_eq!(quiet.coverage, "no_briefing", "{quiet:?}");
    assert_eq!(quiet.stdout, json!({}));
    assert_eq!(f.claude_holds("notes.txt").coverage, "admitted");
    let prompt = f.gemini_run(BEFORE_AGENT);
    assert_eq!(prompt.coverage, "briefed", "{prompt:?}");
    let context = prompt.stdout["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains(&CLAUDE[..8]), "{context}");
    assert!(context.contains("notes.txt"), "{context}");
    // A new session after a restart (SessionStart again) is told again.
    let start = f.gemini_run(SESSION_START);
    assert_eq!(start.coverage, "briefed", "{start:?}");
    assert!(
        start.stdout["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("notes.txt")
    );
    let end = f.gemini_run(SESSION_END);
    assert_eq!(end.coverage, "session_ended", "{end:?}");
    assert_eq!(end.stdout, json!({}));
}

#[test]
fn a_shell_command_is_observed_and_attributed_to_the_gemini_session() {
    let f = Fixture::new();
    let mut before = f.gemini(BEFORE_SHELL);
    before["tool_input"]["command"] = json!("printf x > made.txt");
    let pre = f.hook("gemini-cli", &before);
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    fs::write(f.work().join("made.txt"), "x").unwrap();
    let mut after = f.gemini(AFTER_SHELL);
    after["tool_input"] = before["tool_input"].clone();
    let post = f.hook("gemini-cli", &after);
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert!(
        post.detail.contains("1 changed file(s) attributed"),
        "{}",
        post.detail
    );
    // A peer is told who changed it.
    let prompt = f.hook(
        "claude-code",
        &json!({"session_id": CLAUDE, "cwd": f.work(),
                "hook_event_name": "UserPromptSubmit", "prompt": "continue"}),
    );
    let context = prompt.stdout["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(context.contains(r#""made.txt""#), "{prompt:?}");

    // The same command run again is a new call, observed again: were it
    // the same call, its second report would be "already handled".
    for _ in 0..2 {
        let mut again = f.gemini(BEFORE_SHELL);
        again["tool_input"]["command"] = json!("printf y >> made.txt");
        assert_eq!(f.hook("gemini-cli", &again).coverage, "admitted");
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(f.work().join("made.txt"))
            .unwrap();
        file.write_all(b"y").unwrap();
        again["hook_event_name"] = json!("AfterTool");
        again["tool_response"] = after["tool_response"].clone();
        let observed = f.hook("gemini-cli", &again);
        assert_eq!(observed.coverage, "shell_observed", "{observed:?}");
    }
}

#[test]
fn project_hooks_install_once_run_as_installed_and_remove_exactly() {
    let f = Fixture::new();
    let settings = f.work().join(".gemini/settings.json");
    fs::create_dir(f.work().join(".gemini")).unwrap();
    let original = "{\n  \"general\": {\n    \"vimMode\": true\n  },\n  \"mcpServers\": {\n    \"docs\": {\n      \"command\": \"npx\",\n      \"env\": {\n        \"DOCS_TOKEN\": \"not-a-real-token\"\n      }\n    }\n  }\n}\n";
    fs::write(&settings, original).unwrap();
    let cli = |args: &[&str]| {
        // `doctor` reads the user-level install: this test's own home,
        // never the machine's.
        let output = support::in_home(
            &mut Process::new(env!("CARGO_BIN_EXE_langspan")),
            &f.base.join("home"),
        )
        .args(args)
        .arg("--state-dir")
        .arg(f.state_dir())
        .output()
        .unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
        )
    };
    let root = f.work().to_string_lossy().into_owned();
    let install = [
        "hooks",
        "install",
        "gemini-cli",
        "--workspace",
        "demo",
        "--root",
        &root,
    ];
    let (code, dry) = cli(&install);
    assert_eq!(code, Some(0), "{dry}");
    assert!(dry.contains("Dry run; nothing was written"), "{dry}");
    assert!(dry.contains("LangSpan's hook on 5 event(s)"), "{dry}");
    assert!(
        !dry.contains("not-a-real-token"),
        "the rest of the file is not shown: {dry}"
    );
    assert_eq!(fs::read_to_string(&settings).unwrap(), original);
    let (code, wrote) = cli(&[&install[..], &["--apply"]].concat());
    assert_eq!(code, Some(0), "{wrote}");
    let installed = fs::read_to_string(&settings).unwrap();
    let (_, again) = cli(&[&install[..], &["--apply"]].concat());
    assert!(again.contains("already matches"), "{again}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), installed);

    // The installed command, run through the shell as Gemini CLI runs it.
    let value: Value = serde_json::from_str(&installed).unwrap();
    assert_eq!(value["general"]["vimMode"], true);
    let command = value["hooks"]["BeforeTool"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let mut child = Process::new("/bin/sh")
        .args(["-c", command])
        .current_dir(f.work())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(f.gemini(BEFORE_REPLACE).to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "{}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("\"admitted\"")
    );

    let (_, doctor) = cli(&["doctor", "--root", &root]);
    assert!(
        doctor.contains("ok gemini-cli hooks installed for 5 event(s)"),
        "{doctor}"
    );
    let (code, _) = cli(&["hooks", "remove", "gemini-cli", "--root", &root, "--apply"]);
    assert_eq!(code, Some(0));
    assert_eq!(fs::read_to_string(&settings).unwrap(), original);
}

#[test]
fn a_hook_that_cannot_start_still_answers_gemini_with_an_empty_object() {
    // A bad option fails before any event is read. Gemini would otherwise
    // parse the stderr text as the answer.
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["hook", "gemini-cli", "--ttl-ms", "not-a-number"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "{}");
}
