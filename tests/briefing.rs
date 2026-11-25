#![cfg(unix)]
//! Session lifecycle hooks as real child processes against a real daemon and
//! real Git worktrees: who is told about whose work, and when.

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

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
}

struct Run {
    context: Option<String>,
    event_name: Option<String>,
    stdout: String,
    coverage: String,
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
                "ls-brief-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self { base, child: None };
        fixture.repository("work");
        fixture
    }
    fn repository(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        for file in ["a.txt", "b.txt", "src/c.rs"] {
            fs::write(root.join(file), "hello\n").unwrap();
        }
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "--quiet", "-m", "fixture"]);
        root
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
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&self.state_dir(), &request()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn state(&self) -> State {
        match client::call(&self.state_dir(), &request()).unwrap().outcome {
            Outcome::Status { state } => state,
            other => panic!("{other:?}"),
        }
    }
    /// The background observer learns each workspace's repository on its own
    /// schedule; a worktree briefing needs that fact for both sides.
    fn wait_for_repositories(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.state().workspace_git.len() < count {
            assert!(Instant::now() < deadline, "repositories were not observed");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn run(&self, vendor: &str, anchor: &Path, extra: &[&str], event: &Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", vendor, "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace"])
            .arg(anchor.file_name().unwrap())
            .arg("--root")
            .arg(anchor)
            .args(extra)
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
        let reply: Option<Value> = serde_json::from_str(&stdout).ok();
        let field = |name: &str| {
            reply
                .as_ref()
                .and_then(|reply| reply["hookSpecificOutput"][name].as_str())
                .map(str::to_owned)
        };
        Run {
            context: field("additionalContext"),
            event_name: field("hookEventName"),
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            stdout,
        }
    }
    fn lifecycle(&self, vendor: &str, name: &str, session: &str, cwd: &Path) -> Run {
        self.run(
            vendor,
            &self.root(),
            &[],
            &lifecycle(name, session, cwd, None),
        )
    }
    fn prompt(&self, vendor: &str, session: &str, text: &str, extra: &[&str]) -> Run {
        let event = lifecycle("UserPromptSubmit", session, &self.root(), Some(text));
        self.run(vendor, &self.root(), extra, &event)
    }
    /// A Codex `apply_patch` that is admitted and, with `finish`, completed.
    fn codex_edit(&self, session: &str, call: &str, cwd: &Path, file: &str, finish: bool) {
        let mut event = json!({
            "session_id": session, "turn_id": "turn", "transcript_path": null, "cwd": cwd,
            "hook_event_name": "PreToolUse", "model": "example", "permission_mode": "default",
            "tool_name": "apply_patch", "tool_use_id": call,
            "tool_input": {"command": format!(
                "*** Begin Patch\n*** Update File: {file}\n@@\n-hello\n+hi\n*** End Patch"
            )},
        });
        assert_eq!(
            self.run("codex", &self.root(), &[], &event).coverage,
            "admitted"
        );
        if finish {
            event["hook_event_name"] = json!("PostToolUse");
            event["tool_response"] =
                json!(format!("Success. Updated the following files:\nM {file}\n"));
            assert_eq!(
                self.run("codex", &self.root(), &[], &event).coverage,
                "observed"
            );
        }
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

fn request() -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Status,
    }
}

fn lifecycle(name: &str, session: &str, cwd: &Path, prompt: Option<&str>) -> Value {
    let mut event = json!({
        "session_id": session, "transcript_path": "/dev/null", "cwd": cwd,
        "hook_event_name": name,
    });
    match name {
        "SessionStart" => event["source"] = json!("startup"),
        "SessionEnd" => event["reason"] = json!("other"),
        _ => event["prompt"] = json!(prompt.unwrap_or("continue")),
    }
    event
}

#[test]
fn a_starting_session_is_told_who_is_editing_what_and_only_again_when_it_changes() {
    let mut f = Fixture::new();
    f.start();
    f.codex_edit(BOB, "call-1", &f.root(), "a.txt", false);
    // The peer's branch is part of what a briefing says; learn it before the
    // first briefing so that a scan finishing mid-test is not news.
    f.wait_for_repositories(1);

    let start = f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    assert_eq!(start.coverage, "briefed");
    assert_eq!(start.event_name.as_deref(), Some("SessionStart"));
    let context = start.context.unwrap();
    assert!(
        context.contains("codex session 00000000 in this checkout"),
        "{context}"
    );
    assert!(context.contains(r#"editing now: "a.txt""#), "{context}");
    assert!(context.contains("not instructions"), "{context}");
    assert!(context.len() <= langspan::hook::BRIEFING_LIMIT);
    assert!(
        !start.stdout.contains("permissionDecision"),
        "a briefing never decides"
    );

    // Nothing changed: the next prompt adds nothing to the model's context.
    let quiet = f.prompt("claude-code", ALICE, "carry on", &[]);
    assert_eq!(
        (quiet.coverage.as_str(), quiet.stdout.as_str()),
        ("no_briefing", "")
    );

    // Bob moves to another file: the next prompt carries the difference.
    f.codex_edit(BOB, "call-1", &f.root(), "a.txt", true);
    f.codex_edit(BOB, "call-2", &f.root(), "b.txt", false);
    let changed = f.prompt("claude-code", ALICE, "carry on", &[]);
    assert_eq!(changed.event_name.as_deref(), Some("UserPromptSubmit"));
    let context = changed.context.unwrap();
    assert!(context.contains(r#"editing now: "b.txt""#), "{context}");
    assert!(context.contains(r#"edited: "a.txt" ("#), "{context}");

    // A resumed or compacted context has lost the briefing: start delivers again.
    let resumed = f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    assert_eq!(resumed.coverage, "briefed");

    // Bob, a Codex session, is told about Alice through the same path.
    let alice_prompt = f.prompt(
        "claude-code",
        ALICE,
        "Add retry to   the upload client\nTOKEN=not-for-peers",
        &[],
    );
    assert_eq!(alice_prompt.coverage, "no_briefing");
    let bob = f.lifecycle("codex", "SessionStart", BOB, &f.root());
    let context = bob.context.unwrap();
    assert!(
        context.contains("claude-code session 00000000"),
        "{context}"
    );
    assert!(
        context.contains(r#"task: "Add retry to the upload client""#),
        "{context}"
    );
    assert!(!context.contains("TOKEN"), "only the first line is shared");
}

#[test]
fn a_prompt_summary_stays_private_when_switched_off_and_session_end_withdraws_work() {
    let mut f = Fixture::new();
    f.start();
    f.prompt("claude-code", BOB, "Rename the gate module", &[]);
    f.prompt(
        "claude-code",
        CAROL,
        "unannounced plan",
        &["--prompt-summary", "off"],
    );
    f.prompt("claude-code", CAROL, "/compact", &[]);
    let start = f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    let context = start.context.unwrap();
    assert!(
        context.contains(r#"task: "Rename the gate module""#),
        "{context}"
    );
    assert!(
        !context.contains("unannounced") && !context.contains("/compact"),
        "{context}"
    );

    assert_eq!(
        f.lifecycle("claude-code", "SessionEnd", BOB, &f.root())
            .coverage,
        "session_ended"
    );
    let after = f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    assert_eq!(
        (after.coverage.as_str(), after.stdout.as_str()),
        ("no_briefing", "")
    );
    assert!(f.state().activity[&format!("claude-code.{BOB}")].ended);
    // Had a session left edits behind, they would still be reported, as an
    // ended session's.
    f.codex_edit(CAROL, "call-c", &f.root(), "b.txt", true);
    f.lifecycle("codex", "SessionEnd", CAROL, &f.root());
    let left = f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    let context = left.context.unwrap();
    assert!(
        context.contains("(session ended)") && context.contains(r#"edited: "b.txt""#),
        "{context}"
    );
}

#[test]
fn a_linked_worktree_is_briefed_as_logical_overlap_and_another_repository_is_not() {
    let mut f = Fixture::new();
    let tree = f.base.join("tree");
    git(
        &f.root(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            tree.to_str().unwrap(),
        ],
    );
    let other = f.repository("other");
    f.start();

    // Carol works in the linked worktree, Dave in an unrelated repository,
    // Alice in the main checkout. All three touch a file named a.txt.
    f.codex_edit(CAROL, "call-c", &tree, "a.txt", true);
    let mut dave = lifecycle("UserPromptSubmit", "dave", &other, Some("unrelated work"));
    assert_eq!(
        f.run("claude-code", &other, &[], &dave).coverage,
        "no_briefing"
    );
    dave["hook_event_name"] = json!("SessionStart");
    f.codex_edit(ALICE, "call-a", &f.root(), "a.txt", true);
    f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    f.wait_for_repositories(3);

    let state = f.state();
    assert_eq!(state.workspaces.len(), 3, "a worktree is its own workspace");
    let briefing = f.lifecycle("codex", "SessionStart", ALICE, &f.root());
    let context = briefing.context.unwrap();
    assert!(context.contains("in worktree"), "{context}");
    assert!(context.contains(r#"on branch "feature""#), "{context}");
    assert!(
        context.contains(r#"shares files with your work: "a.txt""#),
        "{context}"
    );
    assert!(!context.contains("unrelated work"), "{context}");
    // The same file name in the worktree was never an admission conflict.
    assert!(
        state.leases.values().all(|lease| lease.paths == ["a.txt"]),
        "both edits were admitted in their own workspaces"
    );
    // Dave's repository hears nothing about the other two.
    let dave_start = f.run("claude-code", &other, &[], &dave);
    assert_eq!(dave_start.coverage, "no_briefing");
}

#[test]
fn lifecycle_hooks_print_nothing_when_coordination_is_unavailable_or_unsupported() {
    let f = Fixture::new();
    let down = f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    assert_eq!(
        (down.coverage.as_str(), down.stdout.as_str()),
        ("unavailable", "")
    );
    let mut subagent = lifecycle("SessionStart", ALICE, &f.root(), None);
    subagent["agent_id"] = json!("agent-1");
    let run = f.run("codex", &f.root(), &[], &subagent);
    assert_eq!(
        (run.coverage.as_str(), run.stdout.as_str()),
        ("unsupported", "")
    );
    let outside = lifecycle("SessionStart", ALICE, &f.base, None);
    let run = f.run("claude-code", &f.root(), &[], &outside);
    assert_eq!(
        (run.coverage.as_str(), run.stdout.as_str()),
        ("unsupported", "")
    );
}

fn claude_tool(
    f: &Fixture,
    name: &str,
    tool: &str,
    session: &str,
    call: &str,
    input: Value,
) -> Run {
    f.run(
        "claude-code",
        &f.root(),
        &[],
        &json!({
            "session_id": session, "cwd": f.root(), "hook_event_name": name,
            "tool_name": tool, "tool_use_id": call, "tool_input": input,
        }),
    )
}

#[test]
fn news_since_the_last_prompt_reaches_the_model_after_its_next_tool_call() {
    let mut f = Fixture::new();
    f.start();
    let input = json!({"file_path": f.root().join("b.txt"), "content": "x"});
    // Alice is briefed at her prompt; nothing is happening yet.
    f.prompt("claude-code", ALICE, "work on b", &[]);
    // Bob's branch is part of the news. Without this, the first scan can
    // finish between the two tool calls and make the second one news too.
    f.wait_for_repositories(1);
    // Mid-turn, Bob starts editing a.txt.
    f.codex_edit(BOB, "bob-1", &f.root(), "a.txt", false);
    assert_eq!(
        claude_tool(&f, "PreToolUse", "Write", ALICE, "a1", input.clone()).coverage,
        "admitted"
    );
    let post = claude_tool(&f, "PostToolUse", "Write", ALICE, "a1", input.clone());
    assert_eq!(post.coverage, "observed");
    assert_eq!(post.event_name.as_deref(), Some("PostToolUse"));
    let context = post.context.expect("the change arrives mid-turn");
    assert!(context.contains(r#"editing now: "a.txt""#), "{context}");
    assert!(
        !post.stdout.contains("permissionDecision"),
        "context only, never a decision"
    );
    // Nothing new since: the next completed call adds nothing.
    claude_tool(&f, "PreToolUse", "Write", ALICE, "a2", input.clone());
    let quiet = claude_tool(&f, "PostToolUse", "Write", ALICE, "a2", input);
    assert_eq!(quiet.stdout, "", "unchanged news is not repeated");
}

#[test]
fn multi_edit_and_notebook_edit_are_coordinated_like_edit() {
    let mut f = Fixture::new();
    f.start();
    let multi = json!({"file_path": f.root().join("a.txt"), "edits": [{"old_string": "hello", "new_string": "hi"}]});
    assert_eq!(
        claude_tool(&f, "PreToolUse", "MultiEdit", ALICE, "m1", multi.clone()).coverage,
        "admitted"
    );
    let denied = claude_tool(&f, "PreToolUse", "MultiEdit", BOB, "m2", multi);
    assert_eq!(denied.coverage, "denied", "{}", denied.stdout);
    let notebook = json!({"notebook_path": f.root().join("b.txt"), "new_source": "x"});
    assert_eq!(
        claude_tool(
            &f,
            "PreToolUse",
            "NotebookEdit",
            ALICE,
            "n1",
            notebook.clone()
        )
        .coverage,
        "admitted"
    );
    assert_eq!(
        claude_tool(&f, "PreToolUse", "NotebookEdit", BOB, "n2", notebook).coverage,
        "denied"
    );
    let other = claude_tool(
        &f,
        "PreToolUse",
        "Grep",
        ALICE,
        "x1",
        json!({"pattern": "x"}),
    );
    assert_eq!(other.coverage, "unsupported");
}

/// Claude Code started in a linked worktree that has its own project
/// install also runs the main checkout's project hooks. That inherited copy
/// leaves every event to the worktree's own hook, quietly and without the
/// coordinator, so each event is decided once.
#[test]
fn the_main_checkouts_hook_leaves_a_worktree_with_its_own_install_to_that_install() {
    let mut f = Fixture::new();
    let tree = f.base.join("tree");
    git(
        &f.root(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            tree.to_str().unwrap(),
        ],
    );
    let tree = fs::canonicalize(tree).unwrap();
    let installed = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args([
            "hooks",
            "install",
            "claude-code",
            "--workspace",
            "tree",
            "--root",
        ])
        .arg(&tree)
        .arg("--state-dir")
        .arg(f.state_dir())
        .arg("--apply")
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    let edit = json!({
        "session_id": CAROL, "transcript_path": "/dev/null", "cwd": tree,
        "hook_event_name": "PreToolUse", "tool_name": "Write", "tool_use_id": "toolu_c1",
        "tool_input": {"file_path": tree.join("a.txt"), "content": "hi\n"},
    });
    let root = f.root();

    // No coordinator is running: the inherited copy does not need one.
    let run = f.run("claude-code", &root, &[], &edit);
    assert_eq!(
        (run.coverage.as_str(), run.stdout.as_str()),
        ("deferred_to_worktree", "")
    );

    f.start();
    let own = f.run("claude-code", &tree, &[], &edit);
    assert_eq!(own.coverage, "admitted");
    for event in [
        edit.clone(),
        lifecycle("SessionStart", CAROL, &tree, None),
        lifecycle("UserPromptSubmit", CAROL, &tree, Some("work in the tree")),
        json!({
            "session_id": CAROL, "transcript_path": "/dev/null", "cwd": tree,
            "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_use_id": "toolu_c2",
            "tool_input": {"command": "git commit -am x"},
        }),
        lifecycle("Stop", CAROL, &tree, None),
        lifecycle("SessionEnd", CAROL, &tree, None),
    ] {
        let run = f.run("claude-code", &root, &[], &event);
        assert_eq!(
            (run.coverage.as_str(), run.stdout.as_str()),
            ("deferred_to_worktree", ""),
            "{event}"
        );
    }
    // Only the worktree's own install registered a workspace.
    let workspaces: Vec<String> = f.state().workspaces.into_keys().collect();
    assert_eq!(workspaces, ["tree"]);

    // Without an install of its own, the main checkout's hook still covers
    // the worktree, as before.
    fs::remove_file(tree.join(".claude/settings.local.json")).unwrap();
    let run = f.run(
        "claude-code",
        &root,
        &[],
        &json!({
            "session_id": CAROL, "transcript_path": "/dev/null", "cwd": tree,
            "hook_event_name": "PreToolUse", "tool_name": "Write", "tool_use_id": "toolu_c3",
            "tool_input": {"file_path": tree.join("b.txt"), "content": "hi\n"},
        }),
    );
    assert_ne!(run.coverage, "deferred_to_worktree");
}
