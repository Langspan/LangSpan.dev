#![cfg(unix)]
//! A workspace-wide formatter run by one session while another session in the
//! same checkout has uncommitted changes in files it may rewrite (issue #126),
//! through the real hook executable, a real daemon and a real Git repository.
//!
//! The pre-tool hook tells the formatting session, once, which other sessions'
//! uncommitted files the command may rewrite. It never refuses the command.

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
const NOTICE: &str = "LangSpan: this command may reformat files";

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
    calls: AtomicU64,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    detail: String,
    /// Context the host gives the model before the call runs.
    context: String,
    /// A permission decision, if the reply made one.
    decision: Option<String>,
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

impl Fixture {
    fn new(files: &[&str]) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-formatter-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        for file in files {
            let path = work.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("// {file}\n")).unwrap();
        }
        git(&work, &["init", "-q"]);
        git(&work, &["config", "user.name", "Owner"]);
        git(&work, &["config", "user.email", "owner@example.invalid"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "start"]);
        let daemon = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let fixture = Self {
            base,
            daemon: Some(daemon),
            calls: AtomicU64::new(0),
        };
        let socket = fixture.state().join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn call_id(&self) -> String {
        format!("call-{}", self.calls.fetch_add(1, Ordering::Relaxed))
    }

    fn hook(&self, event: Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "work", "--root"])
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
        let stdout: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            context: stdout["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or_default()
                .into(),
            decision: stdout["hookSpecificOutput"]["permissionDecision"]
                .as_str()
                .map(str::to_owned),
        }
    }

    fn write_event(
        &self,
        session: &str,
        phase: &str,
        call: &str,
        file: &str,
        content: &str,
    ) -> Run {
        let path = self.work().join(file);
        self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": phase,
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": path, "content": content},
            "tool_response": {"filePath": path},
        }))
    }

    /// A whole Claude Code `Write` of `file`: its pre hook, the write as the
    /// host would make it, and its post hook.
    fn write(&self, session: &str, file: &str) {
        let content = format!("// {file}, changed by {session} in {}\n", self.call_id());
        self.write_content(session, file, &content);
    }

    /// A whole Claude Code `Write` of `file` with `content`.
    fn write_content(&self, session: &str, file: &str, content: &str) {
        let call = self.call_id();
        let pre = self.write_event(session, "PreToolUse", &call, file, content);
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
        let path = self.work().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
        let post = self.write_event(session, "PostToolUse", &call, file, content);
        assert_eq!(post.coverage, "observed", "{post:?}");
    }

    /// A shell call as Claude Code sends it: the pre hook, then (the command
    /// itself is not run: only what LangSpan says before it matters here)
    /// the post hook. Returns the pre hook's reply.
    fn bash(&self, session: &str, command: &str) -> Run {
        let call = self.call_id();
        let event = |phase: &str| {
            json!({
                "session_id": session, "cwd": self.work(), "hook_event_name": phase,
                "tool_name": "Bash", "tool_use_id": call,
                "tool_input": {"command": command, "description": "format"},
                "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
            })
        };
        let pre = self.hook(event("PreToolUse"));
        let post = self.hook(event("PostToolUse"));
        assert!(!post.coverage.starts_with("denied"), "{post:?}");
        pre
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The reply lets the command run: no permission decision, no denial.
fn not_refused(run: &Run) {
    assert!(!run.coverage.starts_with("denied"), "{run:?}");
    assert_eq!(run.decision, None, "{run:?}");
}

fn alice() -> String {
    format!("claude-code session {ALICE}")
}

#[test]
fn a_workspace_formatter_names_another_sessions_uncommitted_files_once() {
    let fixture = Fixture::new(&["src/lib.rs", "src/bob.rs", "web/app.ts", "notes.txt"]);
    fixture.write(ALICE, "src/lib.rs");
    fixture.write(ALICE, "web/app.ts");
    // A file cargo fmt and prettier never format is not named.
    fixture.write(ALICE, "notes.txt");
    fixture.write(BOB, "src/bob.rs");

    let run = fixture.bash(BOB, "cargo fmt --all");
    not_refused(&run);
    assert!(run.context.starts_with(NOTICE), "{run:?}");
    assert!(run.context.contains(&alice()), "{run:?}");
    assert!(run.context.contains("\"src/lib.rs\""), "{run:?}");
    // Not a Rust file; nor Bob's own.
    assert!(!run.context.contains("web/app.ts"), "{run:?}");
    assert!(!run.context.contains("notes.txt"), "{run:?}");
    assert!(!run.context.contains("src/bob.rs"), "{run:?}");
    assert!(run.detail.contains("formatter notice"), "{run:?}");

    // Once: the same files are not named again, by either form of the command.
    for command in ["cargo fmt --all", "cargo fmt"] {
        let again = fixture.bash(BOB, command);
        not_refused(&again);
        assert_eq!(again.context, "", "{command}: {again:?}");
    }

    let run = fixture.bash(BOB, "prettier --write .");
    not_refused(&run);
    assert!(run.context.starts_with(NOTICE), "{run:?}");
    assert!(run.context.contains(&alice()), "{run:?}");
    assert!(run.context.contains("\"web/app.ts\""), "{run:?}");
    assert!(!run.context.contains("src/lib.rs"), "{run:?}");
    let again = fixture.bash(BOB, "npx prettier --write .");
    assert_eq!(again.context, "", "{again:?}");
}

#[test]
fn cargo_fmt_with_no_file_arguments_gives_the_notice() {
    let fixture = Fixture::new(&["src/lib.rs", "src/bob.rs"]);
    fixture.write(ALICE, "src/lib.rs");
    fixture.write(BOB, "src/bob.rs");
    let run = fixture.bash(BOB, "cargo fmt");
    not_refused(&run);
    assert!(run.context.starts_with(NOTICE), "{run:?}");
    assert!(run.context.contains(&alice()), "{run:?}");
    assert!(run.context.contains("\"src/lib.rs\""), "{run:?}");
}

#[test]
fn a_formatter_given_only_this_sessions_files_gives_no_notice() {
    let fixture = Fixture::new(&["src/lib.rs", "src/bob.rs", "web/app.ts", "web/bob.ts"]);
    fixture.write(ALICE, "src/lib.rs");
    fixture.write(ALICE, "web/app.ts");
    fixture.write(BOB, "src/bob.rs");
    fixture.write(BOB, "web/bob.ts");
    for command in [
        "rustfmt --edition 2024 src/bob.rs",
        "prettier --write web/bob.ts",
        "npx prettier -w web/bob.ts",
        // A check rewrites nothing.
        "cargo fmt --all -- --check",
        "cargo fmt --check",
        "prettier --check .",
    ] {
        let run = fixture.bash(BOB, command);
        not_refused(&run);
        assert_eq!(run.context, "", "{command}: {run:?}");
    }
    // Named by file, another session's file is still named.
    let run = fixture.bash(BOB, "rustfmt --edition 2024 src/lib.rs");
    not_refused(&run);
    assert!(run.context.contains("\"src/lib.rs\""), "{run:?}");
}

#[test]
fn a_notice_names_at_most_eight_files_and_counts_the_rest() {
    let files: Vec<String> = (0..10).map(|n| format!("src/f{n}.rs")).collect();
    let mut all: Vec<&str> = files.iter().map(String::as_str).collect();
    all.push("src/bob.rs");
    let fixture = Fixture::new(&all);
    for file in &files {
        fixture.write(ALICE, file);
    }
    fixture.write(BOB, "src/bob.rs");
    let run = fixture.bash(BOB, "cargo fmt --all");
    not_refused(&run);
    let named = files
        .iter()
        .filter(|file| run.context.contains(&format!("\"{file}\"")))
        .count();
    assert_eq!(named, 8, "{run:?}");
    assert!(run.context.contains("and 2 more file(s)"), "{run:?}");
}

#[test]
fn committed_changes_are_not_named() {
    let fixture = Fixture::new(&["src/lib.rs", "src/bob.rs"]);
    fixture.write(ALICE, "src/lib.rs");
    fixture.write(BOB, "src/bob.rs");
    git(&fixture.work(), &["commit", "-q", "-am", "both"]);
    let run = fixture.bash(BOB, "cargo fmt --all");
    not_refused(&run);
    assert_eq!(run.context, "", "{run:?}");
    // A new uncommitted change of the same file is named.
    fixture.write(ALICE, "src/lib.rs");
    let run = fixture.bash(BOB, "cargo fmt --all");
    assert!(run.context.contains("\"src/lib.rs\""), "{run:?}");
}

/// The review of #142: Alice's journaled write of a file is committed with
/// plain Git (the journal keeps it; only `commit --mine` forgets it), then Bob
/// changes the same file. The file differs from `HEAD` only by Bob's lines, so
/// Alice is not named, and the once-only notice is not spent on her: her
/// later uncommitted change of the file is still named.
#[test]
fn a_session_whose_changes_of_a_dirty_file_were_committed_with_git_is_not_named() {
    let fixture = Fixture::new(&["src/lib.rs"]);
    fixture.write_content(ALICE, "src/lib.rs", "// src/lib.rs\nfn alice() {\n}\n");
    git(&fixture.work(), &["commit", "-q", "-am", "alice"]);
    // Bob's lines include one Alice also wrote (`}`): his own copy.
    fixture.write_content(
        BOB,
        "src/lib.rs",
        "// src/lib.rs\nfn alice() {\n}\nfn bob() {\n}\n",
    );
    let run = fixture.bash(BOB, "cargo fmt --all");
    not_refused(&run);
    assert_eq!(run.context, "", "{run:?}");

    // Bob changed the file since Alice wrote it: her next write is refused
    // once as stale, then admitted.
    let content = "// src/lib.rs\nfn alice() {\n    todo!()\n}\nfn bob() {\n}\n";
    let path = fixture.work().join("src/lib.rs");
    let stale = fixture.hook(json!({
        "session_id": ALICE, "cwd": fixture.work(), "hook_event_name": "PreToolUse",
        "tool_name": "Write", "tool_use_id": fixture.call_id(),
        "tool_input": {"file_path": path, "content": content},
    }));
    assert_eq!(stale.coverage, "denied_stale", "{stale:?}");
    fixture.write_content(ALICE, "src/lib.rs", content);
    let run = fixture.bash(BOB, "cargo fmt --all");
    not_refused(&run);
    assert!(run.context.starts_with(NOTICE), "{run:?}");
    assert!(run.context.contains(&alice()), "{run:?}");
    assert!(run.context.contains("\"src/lib.rs\""), "{run:?}");
}
