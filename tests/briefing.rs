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
            Outcome::Status { state, .. } => state,
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

/// The prompt every agent in a swarm starts from, with its own task line.
fn from_template(task: &str) -> String {
    format!(
        "You are a coding agent working on this repository.\n\
         Read the guide first and follow it.\n\n\
         Task: {task}\n\
         When you are done, commit your work."
    )
}

fn session(index: u8) -> String {
    format!("00000000-0000-4000-8000-0000000001{index:02}")
}

fn task_lines(context: &str) -> Vec<&str> {
    context
        .lines()
        .filter_map(|line| line.split_once(" task: ").map(|(_, task)| task))
        .collect()
}

#[test]
fn sessions_started_from_one_template_are_told_apart_in_briefings() {
    let mut f = Fixture::new();
    f.start();
    let tasks = [
        "fix issue #1, the retry loop",
        "fix issue #2, the gate module",
        "fix issue #3, the upload client",
        "fix issue #4, the status line",
        "fix issue #5, the session handler",
    ];
    for (index, task) in tasks.iter().enumerate() {
        f.prompt(
            "claude-code",
            &session(index as u8),
            &from_template(task),
            &[],
        );
    }
    for (index, task) in tasks.iter().enumerate() {
        let start = f.lifecycle(
            "claude-code",
            "SessionStart",
            &session(index as u8),
            &f.root(),
        );
        let context = start.context.unwrap();
        let mut seen = task_lines(&context);
        seen.sort();
        let mut expected: Vec<String> = tasks
            .iter()
            .filter(|other| *other != task)
            .map(|other| format!("{other:?};"))
            .collect();
        expected.sort();
        assert_eq!(seen, expected, "{context}");
        assert!(!context.contains("coding agent"), "{context}");
    }
}

#[test]
fn without_a_task_line_a_prompt_sharing_a_peers_first_lines_is_summarised_where_it_differs() {
    let mut f = Fixture::new();
    f.start();
    let (first, second, third) = (session(10), session(11), session(12));
    let shared = "Shared brief for this sprint.\nRead the guide.\n";
    f.prompt(
        "claude-code",
        &first,
        &format!("{shared}Rename the gate module\nTOKEN=x"),
        &[],
    );
    f.prompt(
        "claude-code",
        &second,
        &format!("{shared}Add retry to the upload client"),
        &[],
    );
    // A one-line prompt is summarised as before, shared or not.
    f.prompt("claude-code", &third, "Shared brief for this sprint.", &[]);
    let start = f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    let context = start.context.unwrap();
    let seen = task_lines(&context);
    assert!(
        seen.contains(&r#""Add retry to the upload client";"#),
        "{context}"
    );
    assert!(
        seen.contains(&r#""Shared brief for this sprint.";"#),
        "{context}"
    );
    assert!(!context.contains("TOKEN"), "{context}");

    // A declared intent replaces the prompt-derived task line.
    let declared = client::call(
        &f.state_dir(),
        &Request {
            version: PROTOCOL_VERSION,
            request_id: "intent".into(),
            command: Command::IntentSet {
                session_id: format!("claude-code.{second}"),
                paths: vec!["src/c.rs".into()],
                summary: "make uploads resumable".into(),
            },
        },
    )
    .unwrap();
    assert!(
        !matches!(declared.outcome, Outcome::Error { .. }),
        "{declared:?}"
    );
    let after = f.lifecycle("claude-code", "SessionStart", ALICE, &f.root());
    let context = after.context.unwrap();
    assert!(
        context.contains(r#"declared: "make uploads resumable""#),
        "{context}"
    );
    assert!(!context.contains("Add retry"), "{context}");
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

/// A linked worktree `tree` of the fixture's checkout, on branch `feature`,
/// with `vendor`'s LangSpan hooks installed in it as workspace `tree`.
fn linked_tree_with_install(f: &Fixture, vendor: &str) -> PathBuf {
    let tree = linked_tree(f);
    install_hooks(f, vendor, &tree);
    tree
}

/// A linked worktree `tree` of the fixture's checkout, on branch `feature`.
fn linked_tree(f: &Fixture) -> PathBuf {
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
    fs::canonicalize(tree).unwrap()
}

fn install_hooks(f: &Fixture, vendor: &str, root: &Path) {
    install_hooks_with(f, vendor, root, &[]);
}

/// `install_hooks`, with `extra` options (such as an opt-out) for the hook.
fn install_hooks_with(f: &Fixture, vendor: &str, root: &Path, extra: &[&str]) {
    let installed = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["hooks", "install", vendor, "--workspace"])
        .arg(root.file_name().unwrap())
        .arg("--root")
        .arg(root)
        .arg("--state-dir")
        .arg(f.state_dir())
        .args(extra)
        .arg("--apply")
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
}

/// Claude Code 2.1.330 started in a linked worktree that has its own
/// project install also runs the main checkout's project hooks. That
/// inherited copy leaves every event to the worktree's own hook, quietly and
/// without the coordinator, so each event is decided once.
#[test]
fn the_main_checkouts_hook_leaves_a_worktree_with_its_own_install_to_that_install() {
    let mut f = Fixture::new();
    let tree = linked_tree_with_install(&f, "claude-code");
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

/// A Codex `apply_patch` of `a.txt` in `cwd`, before it runs.
fn codex_patch(session: &str, call: &str, cwd: &Path) -> Value {
    json!({
        "session_id": session, "turn_id": "turn", "transcript_path": null, "cwd": cwd,
        "hook_event_name": "PreToolUse", "model": "example", "permission_mode": "default",
        "tool_name": "apply_patch", "tool_use_id": call,
        "tool_input": {"command": "*** Begin Patch\n*** Update File: a.txt\n@@\n-hello\n+hi\n*** End Patch"},
    })
}

/// A Claude Code `Write` of `a.txt` in `cwd`, before it runs.
fn claude_write(session: &str, call: &str, cwd: &Path) -> Value {
    json!({
        "session_id": session, "transcript_path": "/dev/null", "cwd": cwd,
        "hook_event_name": "PreToolUse", "tool_name": "Write", "tool_use_id": call,
        "tool_input": {"file_path": cwd.join("a.txt"), "content": "hi\n"},
    })
}

/// Codex 0.171.0 started in a linked worktree runs only the main checkout's
/// project hooks, never the worktree's own (issue #2). The main checkout's
/// hook must not step aside for an install Codex will not run: it decides,
/// as the worktree's own install would, with the worktree as its workspace.
#[test]
fn codex_in_a_worktree_with_its_own_install_is_coordinated_by_the_main_checkouts_hook() {
    let mut f = Fixture::new();
    let tree = linked_tree_with_install(&f, "codex");
    let root = f.root();
    f.start();

    let bob = f.run("codex", &root, &[], &codex_patch(BOB, "bob-1", &tree));
    assert_eq!(bob.coverage, "admitted", "{}", bob.stdout);
    // The session is registered under the worktree's own install, not under
    // a workspace derived from the main checkout's.
    let workspaces: Vec<String> = f.state().workspaces.into_keys().collect();
    assert_eq!(workspaces, ["tree"]);

    // A conflicting edit from another Codex session in the worktree is refused.
    let alice = f.run("codex", &root, &[], &codex_patch(ALICE, "alice-1", &tree));
    assert_eq!(alice.coverage, "denied", "{}", alice.stdout);
    assert!(
        alice.stdout.contains("permissionDecision"),
        "{}",
        alice.stdout
    );
    // So is one from Claude Code, which runs the worktree's own install.
    install_hooks(&f, "claude-code", &tree);
    let carol = f.run("claude-code", &tree, &[], &claude_write(CAROL, "c1", &tree));
    assert_eq!(carol.coverage, "denied", "{}", carol.stdout);

    // Lifecycle events are decided by the main checkout's hook too.
    let start = f.run(
        "codex",
        &root,
        &[],
        &lifecycle("SessionStart", ALICE, &tree, None),
    );
    assert_ne!(start.coverage, "deferred_to_worktree");
}

/// Hosts with no retained evidence of how they load a linked worktree's
/// project hooks (Cursor, Gemini CLI) do not step aside: the main
/// checkout's hook decides, as the worktree's own install would. If the host
/// runs both, both reach the same decision for the same workspace.
#[test]
fn an_unconfirmed_host_decides_in_a_worktree_with_its_own_install_rather_than_stepping_aside() {
    for vendor in ["gemini-cli", "cursor"] {
        let mut f = Fixture::new();
        let tree = linked_tree_with_install(&f, vendor);
        let root = f.root();
        f.start();
        let tree_text = tree.to_str().unwrap();
        let edit = match vendor {
            "gemini-cli" => json!({
                "session_id": "00000000-0000-4000-8000-0000000000e1",
                "transcript_path": "/dev/null", "cwd": tree, "hook_event_name": "BeforeTool",
                "timestamp": "2026-11-24T23:15:03.000Z", "tool_name": "write_file",
                "tool_input": {"file_path": tree.join("a.txt"), "content": "hi\n"},
            }),
            _ => json!({
                "conversation_id": "00000000-0000-4000-8000-0000000000c1",
                "generation_id": "00000000-0000-4000-8000-0000000000e1",
                "model": "default", "session_id": "00000000-0000-4000-8000-0000000000c1",
                "hook_event_name": "preToolUse", "cursor_version": "1.7.2",
                "workspace_roots": [tree_text], "user_email": null, "transcript_path": null,
                "tool_name": "Write",
                "tool_input": {"file_path": tree.join("a.txt"), "content": "hi\n"},
                "tool_use_id": "tool_0001", "cwd": tree_text,
            }),
        };
        let inherited = f.run(vendor, &root, &[], &edit);
        assert_eq!(
            inherited.coverage, "admitted",
            "{vendor}: {}",
            inherited.stdout
        );
        let own = f.run(vendor, &tree, &[], &edit);
        assert_eq!(own.coverage, "admitted", "{vendor}: {}", own.stdout);
        let workspaces: Vec<String> = f.state().workspaces.into_keys().collect();
        assert_eq!(workspaces, ["tree"], "{vendor}");
        // Another session in the worktree is held off the file.
        install_hooks(&f, "claude-code", &tree);
        let carol = f.run("claude-code", &tree, &[], &claude_write(CAROL, "c1", &tree));
        assert_eq!(carol.coverage, "denied", "{vendor}: {}", carol.stdout);
    }
}

/// What `doctor --root root` says that names `tree`: (ok, detail) pairs.
fn doctor_about(f: &Fixture, root: &Path, tree: &Path) -> Vec<(bool, String)> {
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["doctor", "--json", "--state-dir"])
        .arg(f.state_dir())
        .arg("--home")
        .arg(&f.base)
        .arg("--root")
        .arg(root)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["ok"].as_bool().unwrap(),
                item["detail"].as_str().unwrap().to_owned(),
            )
        })
        .filter(|(_, detail)| detail.contains(tree.to_str().unwrap()))
        .collect()
}

/// `doctor`'s finding about Codex not running `tree`'s own install.
fn codex_worktree_finding(f: &Fixture, root: &Path, tree: &Path) -> (bool, String) {
    let findings = doctor_about(f, root, tree);
    findings
        .into_iter()
        .find(|(_, detail)| detail.starts_with("codex:") && detail.contains("does not run"))
        .unwrap_or_else(|| panic!("no Codex worktree finding"))
}

/// `doctor` names a linked worktree whose install the host will not run.
#[test]
fn doctor_reports_a_worktree_install_the_host_will_not_run() {
    let f = Fixture::new();
    let tree = linked_tree_with_install(&f, "codex");

    // No Codex install in the main checkout: nothing Codex runs covers the
    // worktree, so it needs fixing.
    for root in [f.root(), tree.clone()] {
        let finding = codex_worktree_finding(&f, &root, &tree);
        assert!(!finding.0, "{finding:?}");
        assert!(finding.1.contains("Nothing coordinates it"), "{finding:?}");
        assert!(finding.1.contains("0.171.0"), "{finding:?}");
    }

    // With one, the main checkout's hook covers it: reported, not a fault.
    install_hooks(&f, "codex", &f.root());
    let finding = codex_worktree_finding(&f, &f.root(), &tree);
    assert!(finding.0, "{finding:?}");
    assert!(finding.1.contains("as workspace tree"), "{finding:?}");

    // A host that runs the worktree's own install there is not reported.
    install_hooks(&f, "claude-code", &tree);
    let findings = doctor_about(&f, &f.root(), &tree);
    assert!(
        findings
            .iter()
            .all(|(_, detail)| !detail.starts_with("claude-code:")),
        "{findings:?}"
    );
}

/// A main-checkout install that cannot run, because its executable is gone
/// or it lacks an event the host needs, does not cover a linked worktree
/// whose own install the host will not run, whichever root `doctor` is
/// given.
#[test]
fn doctor_does_not_count_a_broken_main_checkout_install_as_covering_a_worktree() {
    let f = Fixture::new();
    let tree = linked_tree_with_install(&f, "codex");
    let hooks = f.root().join(".codex/hooks.json");
    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_langspan")).unwrap();
    let gone = f.base.join("gone").join("langspan");

    install_hooks(&f, "codex", &f.root());
    let text = fs::read_to_string(&hooks).unwrap();
    assert!(text.contains(executable.to_str().unwrap()), "{text}");
    fs::write(
        &hooks,
        text.replace(executable.to_str().unwrap(), gone.to_str().unwrap()),
    )
    .unwrap();
    for root in [f.root(), tree.clone()] {
        let finding = codex_worktree_finding(&f, &root, &tree);
        assert!(!finding.0, "{finding:?}");
        assert!(
            finding.1.contains("executable no longer exists"),
            "{finding:?}"
        );
        assert!(!finding.1.contains("coordinates it as"), "{finding:?}");
    }

    // Reinstalled, then left without the prompt event: still not covered.
    install_hooks(&f, "codex", &f.root());
    assert!(codex_worktree_finding(&f, &f.root(), &tree).0);
    let mut settings: Value = serde_json::from_str(&fs::read_to_string(&hooks).unwrap()).unwrap();
    settings["hooks"]
        .as_object_mut()
        .unwrap()
        .remove("UserPromptSubmit")
        .unwrap();
    fs::write(&hooks, settings.to_string()).unwrap();
    for root in [f.root(), tree.clone()] {
        let finding = codex_worktree_finding(&f, &root, &tree);
        assert!(!finding.0, "{finding:?}");
        assert!(finding.1.contains("UserPromptSubmit"), "{finding:?}");
    }
}

/// A prompt event for `vendor`, submitted in `cwd`. `turn` numbers it.
fn prompt_event(vendor: &str, session: &str, turn: u32, cwd: &Path, text: &str) -> Value {
    match vendor {
        "cursor" => json!({
            "conversation_id": session, "generation_id": format!("gen-{turn}"),
            "model": "default", "session_id": session,
            "hook_event_name": "beforeSubmitPrompt", "cursor_version": "1.7.2",
            "workspace_roots": [cwd], "user_email": null, "transcript_path": null,
            "prompt": text, "attachments": [], "composer_mode": "agent",
        }),
        _ => {
            let mut event = lifecycle("UserPromptSubmit", session, cwd, Some(text));
            event["turn_id"] = json!(format!("turn-{turn}"));
            event["model"] = json!("example");
            event["permission_mode"] = json!("default");
            event
        }
    }
}

/// A host the main checkout's hook decides for in a linked worktree with its
/// own install (Codex, which reads the prompt through `lifecycle_event`, and
/// Cursor, whose adapter parses it and calls `lifecycle_parsed`) keeps that
/// install's opt-outs: with `--prompt-summary off` nothing of the prompt is
/// recorded, and with `--code-briefing off` no source index section is sent,
/// although the main checkout's hook has both on.
#[test]
fn a_worktree_installs_prompt_opt_outs_hold_when_the_main_checkouts_hook_decides() {
    const HEADER: &str = "LangSpan source index:";
    for vendor in ["codex", "cursor"] {
        let mut f = Fixture::new();
        let root = f.root();
        fs::write(root.join("src/upload.rs"), "pub fn zebra_upload() {}\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "--quiet", "-m", "upload"]);
        let tree = linked_tree(&f);
        install_hooks(&f, vendor, &tree);
        f.start();
        let events = || {
            let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
                .arg("--state-dir")
                .arg(f.state_dir())
                .args(["events", "--limit", "1000", "--json"])
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap()
        };

        // With both on in each install, the prompt is shared and its names
        // are looked up in the worktree's source index.
        let mut turn = 0;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            turn += 1;
            let text = "Where is zebra_upload defined?";
            let run = f.run(
                vendor,
                &root,
                &[],
                &prompt_event(vendor, BOB, turn, &tree, text),
            );
            if run.stdout.contains(HEADER) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{vendor}: never code-briefed: {}",
                run.stdout
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(events().contains("Where is zebra_upload"), "{vendor}");

        // The worktree's own install switches both off; the main checkout's
        // hook, the one that runs, still has both on.
        install_hooks_with(
            &f,
            vendor,
            &tree,
            &["--prompt-summary", "off", "--code-briefing", "off"],
        );
        let text = "quokka_marker: where is zebra_upload defined?";
        let run = f.run(
            vendor,
            &root,
            &[],
            &prompt_event(vendor, BOB, turn + 1, &tree, text),
        );
        assert_ne!(run.coverage, "deferred_to_worktree", "{vendor}");
        assert!(
            !events().contains("quokka_marker"),
            "{vendor}: the prompt summary was recorded"
        );
        assert!(!run.stdout.contains(HEADER), "{vendor}: {}", run.stdout);
    }
}

// Presence (roadmap A13): where peers are looking, from recorded Read and
// Grep events and shell calls, through the real hook binary and daemon.

/// A pattern that must never be recorded, briefed or shown anywhere.
const PATTERN: &str = "zq_private_pattern_77";

fn presence_repository(f: &Fixture) {
    let root = f.root();
    for file in ["src/auth.rs", "src/session.rs", "src/token.rs"] {
        fs::write(root.join(file), "fn auth() {}\n").unwrap();
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "presence"]);
}

/// What `langspan` prints for `args` against the fixture's coordinator.
fn cli(f: &Fixture, args: &[&str]) -> String {
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(f.state_dir())
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// `langspan brief SESSION --json`: a look from outside, not a delivery.
fn brief_json(f: &Fixture, session: &str) -> String {
    let id = format!("brief-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    cli(f, &["brief", session, "--json", "--request-id", &id])
}

fn claude_post(
    f: &Fixture,
    extra: &[&str],
    tool: &str,
    session: &str,
    call: &str,
    input: Value,
) -> Run {
    f.run(
        "claude-code",
        &f.root(),
        extra,
        &json!({
            "session_id": session, "cwd": f.root(), "hook_event_name": "PostToolUse",
            "tool_name": tool, "tool_use_id": call, "tool_input": input,
            "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
        }),
    )
}

#[test]
fn a_peers_reads_and_searches_reach_the_next_briefing_as_where_it_is_looking() {
    let mut f = Fixture::new();
    presence_repository(&f);
    f.start();
    let root = f.root();
    f.prompt("claude-code", ALICE, "Rework token refresh", &[]);
    f.prompt("codex", BOB, "Rewrite the auth module", &[]);
    // The branch is part of the line; learn it before the first briefing.
    f.wait_for_repositories(1);

    // Alice reads two files and searches one directory; she writes nothing.
    for (call, file) in [("r1", "src/auth.rs"), ("r2", "src/session.rs")] {
        let read = claude_post(
            &f,
            &[],
            "Read",
            ALICE,
            call,
            json!({"file_path": root.join(file)}),
        );
        assert_eq!(read.coverage, "read_recorded");
    }
    let grep = claude_post(
        &f,
        &[],
        "Grep",
        ALICE,
        "g1",
        json!({"pattern": PATTERN, "path": root.join("src"), "glob": "*.rs", "output_mode": "content"}),
    );
    assert_eq!(grep.coverage, "presence_recorded", "{}", grep.stdout);
    assert_eq!(grep.stdout, "", "a search is never answered or held");

    // Bob, a Codex session, is told at his next prompt.
    let context = f
        .prompt("codex", BOB, "carry on", &[])
        .context
        .expect("presence is news at a prompt");
    let line = context
        .lines()
        .find(|line| line.contains("claude-code session 00000000"))
        .unwrap_or_else(|| panic!("{context}"));
    assert!(line.contains(r#"task: "Rework token refresh""#), "{line}");
    assert!(
        line.contains(r#"looking at "src", "src/session.rs", "src/auth.rs""#),
        "{line}"
    );
    assert!(line.len() <= 700, "{line}");
    assert!(context.len() <= langspan::hook::BRIEFING_LIMIT);

    // The same in `langspan brief --json`, with each path's kind and age.
    let brief: Value = serde_json::from_str(&brief_json(&f, &format!("codex.{BOB}"))).unwrap();
    let looking = &brief["outcome"]["briefing"]["peers"][0]["looking"];
    assert_eq!(looking[0]["path"], "src");
    assert_eq!(looking[0]["kind"], "search");
    assert_eq!(looking[2]["kind"], "read");
    assert!(looking[0]["age_s"].as_u64().unwrap() < 600, "{looking}");
    // And in the status, under the session's activity, with its times.
    let status: Value = serde_json::from_str(&cli(&f, &["status", "--json"])).unwrap();
    let recorded =
        &status["outcome"]["state"]["activity"][format!("claude-code.{ALICE}")]["looking"];
    assert_eq!(recorded.as_array().map(Vec::len), Some(3), "{recorded}");
    assert_eq!(recorded[0]["kind"], "search");

    // A file Alice then edits is shown as edited only.
    let write = json!({"file_path": root.join("src/auth.rs"), "content": "fn auth2() {}\n"});
    assert_eq!(
        claude_tool(&f, "PreToolUse", "Write", ALICE, "w1", write.clone()).coverage,
        "admitted"
    );
    fs::write(root.join("src/auth.rs"), "fn auth2() {}\n").unwrap();
    assert_eq!(
        claude_tool(&f, "PostToolUse", "Write", ALICE, "w1", write).coverage,
        "observed"
    );
    let context = f.prompt("codex", BOB, "carry on", &[]).context.unwrap();
    assert!(context.contains(r#"edited: "src/auth.rs""#), "{context}");
    assert!(
        context.contains(r#"looking at "src", "src/session.rs";"#),
        "{context}"
    );
    // (It may also be listed as shared: Bob's prompt names `auth`, which the
    // source index can find there. Never as looked at.)
    let clause = context.split("looking at").nth(1).unwrap();
    assert!(
        !clause.lines().next().unwrap().contains("auth.rs"),
        "{context}"
    );
    assert_eq!(context.matches("looking at").count(), 1, "{context}");

    // The pattern is nowhere: not in an event, the status or a briefing.
    for output in [
        cli(&f, &["events", "--limit", "1000", "--json"]),
        cli(&f, &["status", "--json"]),
        cli(&f, &["status", "--all", "--json"]),
        brief_json(&f, &format!("codex.{BOB}")),
        context,
    ] {
        assert!(!output.contains(PATTERN), "{output}");
        assert!(!output.contains("*.rs"), "{output}");
    }
}

#[test]
fn presence_off_keeps_a_sessions_reads_searches_and_shell_paths_from_peers() {
    let mut f = Fixture::new();
    presence_repository(&f);
    f.start();
    let root = f.root();
    let off = ["--presence", "off"];
    f.prompt("claude-code", CAROL, "Quiet work", &off);
    let read = claude_post(
        &f,
        &off,
        "Read",
        CAROL,
        "r1",
        json!({"file_path": root.join("src/auth.rs")}),
    );
    assert_eq!(
        read.coverage, "read_recorded",
        "the version is still recorded"
    );
    let grep = claude_post(
        &f,
        &off,
        "Grep",
        CAROL,
        "g1",
        json!({"pattern": PATTERN, "path": root.join("src")}),
    );
    assert_eq!(grep.coverage, "presence_off");
    let shell = claude_post(
        &f,
        &off,
        "Bash",
        CAROL,
        "s1",
        json!({"command": "cat src/token.rs"}),
    );
    assert_ne!(shell.coverage, "unsupported");

    let context = f
        .prompt("codex", BOB, "work", &[])
        .context
        .unwrap_or_default();
    for path in ["src/auth.rs", "src/token.rs", "looking at"] {
        assert!(!context.contains(path), "{context}");
    }
    let state = f.state();
    let carol = format!("claude-code.{CAROL}");
    assert!(state.activity[&carol].looking.is_empty());
    assert!(!state.presence[&carol].shown);
    // Its own read is still a version for its stale checks.
    assert!(
        state.activity[&carol]
            .known
            .iter()
            .any(|k| k.path == "src/auth.rs")
    );
    assert!(!cli(&f, &["events", "--limit", "1000", "--json"]).contains("presence_observed"));
}

#[test]
fn a_shell_command_that_reads_files_is_presence_on_every_host() {
    let mut f = Fixture::new();
    presence_repository(&f);
    f.start();
    f.prompt("claude-code", ALICE, "Look around", &[]);
    // A Codex session's shell call: Codex reports no reads, but its shell
    // commands name what they read.
    let command = format!("grep -n {PATTERN} src/auth.rs && cat src/token.rs missing.rs");
    for phase in ["PreToolUse", "PostToolUse"] {
        let event = json!({
            "session_id": BOB, "turn_id": "turn", "transcript_path": null, "cwd": f.root(),
            "hook_event_name": phase, "model": "example", "permission_mode": "default",
            "tool_name": "Bash", "tool_use_id": "s1",
            "tool_input": {"command": command},
            "tool_response": "",
        });
        f.run("codex", &f.root(), &[], &event);
    }
    let context = f
        .prompt("claude-code", ALICE, "carry on", &[])
        .context
        .unwrap();
    assert!(
        context.contains(r#"looking at "src/auth.rs", "src/token.rs""#),
        "{context}"
    );
    let brief = brief_json(&f, &format!("claude-code.{ALICE}"));
    assert!(brief.contains(r#""kind":"shell""#), "{brief}");
    assert!(
        !brief.contains("missing.rs"),
        "only existing paths: {brief}"
    );
    for output in [
        brief,
        cli(&f, &["events", "--limit", "1000", "--json"]),
        context,
    ] {
        assert!(!output.contains(PATTERN), "{output}");
    }
}
