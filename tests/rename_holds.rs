#![cfg(unix)]
//! A rename held once (issue #213): a write that renames or removes a name
//! another worktree's uncommitted files still use is refused once, naming
//! those files and suggesting a forwarding alias; repeating it goes
//! through. A process test: what it verifies is the Git worktrees' state as
//! the real observer and index see it, and the real hook executable's exit
//! and reply, against a real daemon.

mod support;

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
const WAIT: Duration = Duration::from_secs(60);
const CONFIG: &str = "pub fn parse_config(text: &str) -> u32 {\n    text.len() as u32\n}\n\npub fn other_name(x: u32) -> u32 {\n    x\n}\n";
const CALLER: &str = "mod config;\nfn main() {\n    let _ = config::parse_config(\"a\");\n}\n";

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

struct Run {
    coverage: String,
    detail: String,
    stdout: String,
}

impl Run {
    fn reason(&self) -> String {
        let reply: Value = serde_json::from_str(&self.stdout).unwrap();
        let specific = &reply["hookSpecificOutput"];
        assert_eq!(specific["permissionDecision"], "deny", "{}", self.stdout);
        specific["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
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

impl World {
    /// A repository with `src/config.rs` and a linked worktree of it,
    /// registered as workspaces `app` and `tree`.
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-rename-holds-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let work = base.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(work.join("src/config.rs"), CONFIG).unwrap();
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "init"]);
        let tree = base.join("tree");
        git(
            &work,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature",
                tree.to_str().unwrap(),
            ],
        );
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self { base, daemon: None };
        world.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .env("LANGSPAN_TEST_SCAN_EVERY_MS", "200")
                .args(["daemon", "--state-dir"])
                .arg(&state)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while world.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        for (workspace, root) in [("app", world.root("app")), ("tree", world.root("tree"))] {
            let added = world.cli(&[
                "workspace",
                "add",
                workspace,
                root.to_str().unwrap(),
                "--request-id",
                &format!("ws-{workspace}"),
            ]);
            assert_eq!(added.0, 0, "{}", added.1);
        }
        world
    }
    fn root(&self, workspace: &str) -> PathBuf {
        self.base.join(if workspace == "app" {
            "work"
        } else {
            workspace
        })
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
    fn hook(&self, vendor: &str, workspace: &str, event: Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", workspace, "--root"])
            .arg(self.root(workspace))
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
            detail: diagnostic["detail"].as_str().unwrap_or("").into(),
            stdout: String::from_utf8(output.stdout).unwrap(),
        }
    }
    /// A Claude Code `Write` of `file` in `workspace` before it lands.
    fn write(&self, workspace: &str, session: &str, call: &str, file: &str, text: &str) -> Run {
        self.claude(workspace, session, call, file, text, "PreToolUse")
    }
    fn claude(
        &self,
        workspace: &str,
        session: &str,
        call: &str,
        file: &str,
        text: &str,
        phase: &str,
    ) -> Run {
        let root = self.root(workspace);
        self.hook(
            "claude-code",
            workspace,
            json!({
                "session_id": session, "cwd": root, "hook_event_name": phase,
                "tool_name": "Write", "tool_use_id": call,
                "tool_input": {"file_path": root.join(file), "content": text},
            }),
        )
    }
    /// The write did not land: its admission ends, the file is unchanged.
    fn failed(&self, workspace: &str, session: &str, call: &str, file: &str, text: &str) {
        self.claude(workspace, session, call, file, text, "PostToolUseFailure");
    }
    fn codex_patch(&self, session: &str, call: &str, patch: &str) -> Run {
        self.hook(
            "codex",
            "app",
            json!({
                "session_id": session, "turn_id": "turn-1", "transcript_path": null,
                "cwd": self.root("app"), "hook_event_name": "PreToolUse", "model": "example",
                "permission_mode": "default", "tool_name": "apply_patch",
                "tool_use_id": call, "tool_input": {"command": patch},
            }),
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn a_rename_with_a_peers_uncommitted_caller_is_held_once_and_a_repeat_goes_through() {
    let w = World::new();
    // Bob, in the linked worktree, writes an uncommitted caller.
    let bob = w.write("tree", "bob", "b1", "src/main.rs", CALLER);
    assert_eq!(bob.coverage, "admitted", "{}", bob.detail);
    fs::write(w.root("tree").join("src/main.rs"), CALLER).unwrap();
    let post = w.claude("tree", "bob", "b1", "src/main.rs", CALLER, "PostToolUse");
    assert_eq!(post.coverage, "observed", "{}", post.detail);

    // Alice renames parse_config. Until the index has Bob's file, her
    // write is admitted; each such attempt is reported failed.
    let renamed = CONFIG.replace("parse_config", "load_config");
    let mut call = 0;
    let held = support::wait_for("the rename to be held", WAIT, || {
        call += 1;
        let id = format!("a{call}");
        let run = w.write("app", "alice", &id, "src/config.rs", &renamed);
        if run.coverage == "held_rename" {
            return Ok((id, run));
        }
        assert_eq!(run.coverage, "admitted", "{}", run.detail);
        w.failed("app", "alice", &id, "src/config.rs", &renamed);
        Err(run.detail)
    });
    let reason = held.1.reason();
    let expected = format!(
        r#""parse_config" (renamed to "load_config") is used in worktree "{}" (claude-code session bob, not committed): "src/main.rs""#,
        w.root("tree").display()
    );
    assert!(
        reason.starts_with("LangSpan: rename held once."),
        "{reason}"
    );
    assert!(reason.contains(&expected), "{reason}");
    assert!(reason.contains("forwarding alias"), "{reason}");
    // A held write takes no admission: another session's write of the
    // same file, renaming nothing, is admitted at once.
    let body = CONFIG.replace("text.len() as u32", "1");
    let dave = w.write("app", "dave", "d1", "src/config.rs", &body);
    assert_eq!(dave.coverage, "admitted", "{}", dave.detail);
    w.failed("app", "dave", "d1", "src/config.rs", &body);

    // The same call delivered again gets the same answer.
    let again = w.write("app", "alice", &held.0, "src/config.rs", &renamed);
    assert_eq!(again.coverage, "held_rename", "{}", again.detail);
    // Repeated as a new call, the write goes through.
    let repeated = w.write("app", "alice", "a-repeat", "src/config.rs", &renamed);
    assert_eq!(repeated.coverage, "admitted", "{}", repeated.detail);
    assert!(repeated.stdout.is_empty(), "{}", repeated.stdout);
    w.failed("app", "alice", "a-repeat", "src/config.rs", &renamed);

    // A name no other worktree uses is renamed with no hold, the index
    // being current (it just found Bob's caller).
    let other = CONFIG.replace("other_name", "another_name");
    let unused = w.write("app", "alice", "a-other", "src/config.rs", &other);
    assert_eq!(unused.coverage, "admitted", "{}", unused.detail);
    assert!(unused.stdout.is_empty(), "{}", unused.stdout);
    assert!(
        unused.detail.contains("rename check: no callers"),
        "{}",
        unused.detail
    );
    w.failed("app", "alice", "a-other", "src/config.rs", &other);

    // Codex's apply_patch, removing the name, is held for its own session.
    let patch = w.codex_patch(
        "carol",
        "c1",
        "*** Begin Patch\n*** Update File: src/config.rs\n@@\n-pub fn parse_config(text: &str) -> u32 {\n-    text.len() as u32\n-}\n-\n pub fn other_name(x: u32) -> u32 {\n*** End Patch",
    );
    assert_eq!(patch.coverage, "held_rename", "{}", patch.detail);
    let reason = patch.reason();
    assert!(
        reason.contains(r#""parse_config" (removed) is used in worktree"#),
        "{reason}"
    );
}
