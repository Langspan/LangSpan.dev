#![cfg(unix)]
//! The end of a reply (Stop, StopFailure, SubagentStop), the end of a tool
//! batch (PostToolBatch), Codex's Stop and SessionEnd, and the version a
//! Codex session is given of a file it first patches, through the real hook
//! executable and a real daemon. Claude Code events are the ones captured
//! from Claude Code 2.1.330 (`fixtures/claude-code-2.1.330`), with paths and
//! session IDs rewritten for each test. Codex Stop and SessionEnd are in the
//! documented shape only: no Codex hook ran in the capture
//! (`fixtures/claude-code-2.1.330/README.md`).

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{Command, Event, LeaseStatus, Outcome, State};
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

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";
const CAPTURED_AGENT: &str = "a8830b110c1d1bfdf";

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
}

struct Run {
    coverage: String,
    detail: String,
    stdout: String,
}

impl Run {
    fn context(&self) -> Option<(String, String)> {
        let reply: Value = serde_json::from_str(&self.stdout).ok()?;
        let output = &reply["hookSpecificOutput"];
        Some((
            output["hookEventName"].as_str()?.to_owned(),
            output["additionalContext"].as_str()?.to_owned(),
        ))
    }
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-turn-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(base.join("work/src")).unwrap();
        for file in ["a.txt", "b.txt", "README.md"] {
            fs::write(base.join("work").join(file), "hello\n").unwrap();
        }
        fs::write(base.join("work/src/lib.rs"), "fn greet() {}\n").unwrap();
        let child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let fixture = Self {
            base,
            child: Some(child),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&fixture.state_dir(), &status()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn root(&self) -> PathBuf {
        self.base.join("work")
    }
    fn state(&self) -> State {
        match client::call(&self.state_dir(), &status()).unwrap().outcome {
            Outcome::Status { state } => state,
            other => panic!("{other:?}"),
        }
    }
    fn events(&self) -> Vec<Event> {
        let request = Request {
            version: PROTOCOL_VERSION,
            request_id: "read".into(),
            command: Command::Events {
                after: 0,
                limit: 1000,
            },
        };
        match client::call(&self.state_dir(), &request).unwrap().outcome {
            Outcome::Events { events } => events.into_iter().map(|e| e.event).collect(),
            other => panic!("{other:?}"),
        }
    }
    /// Lease statuses of one LangSpan session, by path.
    fn leases(&self, session: &str) -> Vec<(Vec<String>, LeaseStatus)> {
        self.state()
            .leases
            .into_values()
            .filter(|lease| lease.session_id == session)
            .map(|lease| (lease.paths, lease.status))
            .collect()
    }
    fn run(&self, vendor: &str, event: Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", vendor, "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .env_remove("LANGSPAN_CAPTURE_DIR")
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
            stdout,
        }
    }
    /// A captured Claude Code event, moved into this fixture's checkout and
    /// given `session` as its session ID.
    fn captured(&self, name: &str, session: &str) -> Value {
        let path = format!(
            "{}/fixtures/claude-code-2.1.330/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = fs::read_to_string(path)
            .unwrap()
            .replace("/work/demo", self.root().to_str().unwrap());
        let mut event: Value = serde_json::from_str(&text).unwrap();
        event["session_id"] = json!(session);
        event
    }
    fn claude_tool(
        &self,
        session: &str,
        agent: Option<&str>,
        name: &str,
        call: &str,
        file: &str,
    ) -> Run {
        let mut event = json!({
            "session_id": session, "cwd": self.root(), "hook_event_name": name,
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.root().join(file), "content": "x\n"},
        });
        if let Some(agent) = agent {
            event["agent_id"] = json!(agent);
            event["agent_type"] = json!("Explore");
        }
        self.run("claude-code", event)
    }
    fn codex_patch(&self, session: &str, name: &str, call: &str, response: Option<&str>) -> Run {
        let mut event = json!({
            "session_id": session, "turn_id": "turn-1", "cwd": self.root(),
            "hook_event_name": name, "model": "gpt-test", "permission_mode": "default",
            "tool_name": "apply_patch", "tool_use_id": call,
            "tool_input": {"command":
                "*** Begin Patch\n*** Update File: a.txt\n@@\n-hello\n+hi\n*** End Patch"},
        });
        if let Some(response) = response {
            event["tool_response"] = json!(response);
        }
        self.run("codex", event)
    }
    /// Codex's documented `Stop` and `SessionEnd` inputs
    /// (developers.openai.com/codex/hooks, "Stop", "SessionEnd").
    fn codex_lifecycle(&self, session: &str, name: &str) -> Run {
        let mut event = json!({
            "session_id": session, "transcript_path": null, "cwd": self.root(),
            "hook_event_name": name, "model": "gpt-test",
        });
        if name == "Stop" {
            event["turn_id"] = json!("turn-1");
            event["permission_mode"] = json!("default");
            event["stop_hook_active"] = json!(false);
            event["last_assistant_message"] = json!("done");
        } else {
            event["reason"] = json!("other");
        }
        self.run("codex", event)
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

fn status() -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Status,
    }
}

fn claude(session: &str) -> String {
    format!("claude-code.{session}")
}

#[test]
fn a_claude_stop_releases_an_admission_whose_post_tool_event_never_came() {
    let f = Fixture::new();
    // Alice's write was admitted, then refused at the owner's permission
    // prompt: Claude Code sends no post-tool event for it.
    assert_eq!(
        f.claude_tool(ALICE, None, "PreToolUse", "call-a", "a.txt")
            .coverage,
        "admitted"
    );
    assert_eq!(
        f.claude_tool(BOB, None, "PreToolUse", "call-b", "a.txt")
            .coverage,
        "denied",
        "the open admission still holds the file"
    );
    // Captured from that case: the batch reports the refusal as the tool's
    // result; it does not end the admission.
    let batch = f.run(
        "claude-code",
        f.captured("post-tool-batch-refused-edit", ALICE),
    );
    assert!(batch.coverage.starts_with("batch_"), "{}", batch.detail);
    assert_eq!(
        f.leases(&claude(ALICE)),
        [(vec!["a.txt".to_owned()], LeaseStatus::Active)]
    );
    let stop = f.run("claude-code", f.captured("stop", ALICE));
    assert_eq!(stop.coverage, "stopped", "{}", stop.detail);
    assert!(stop.detail.contains("1 open admission"), "{}", stop.detail);
    // Output on Stop would keep Claude Code going: nothing is ever printed.
    assert!(stop.stdout.is_empty(), "{}", stop.stdout);
    assert_eq!(
        f.leases(&claude(ALICE)),
        [(vec!["a.txt".to_owned()], LeaseStatus::Completed)]
    );
    assert_eq!(
        f.claude_tool(BOB, None, "PreToolUse", "call-b2", "a.txt")
            .coverage,
        "admitted"
    );
    assert!(f.events().iter().any(|event| matches!(event,
        Event::SessionStopped { session_id, released, failure: None, .. }
            if *session_id == claude(ALICE) && released.len() == 1)));
}

#[test]
fn a_stop_while_a_subagent_runs_leaves_the_subagents_admissions_open() {
    let f = Fixture::new();
    f.claude_tool(ALICE, None, "PreToolUse", "call-parent", "a.txt");
    f.claude_tool(
        ALICE,
        Some(CAPTURED_AGENT),
        "PreToolUse",
        "call-sub",
        "b.txt",
    );
    // Captured: the parent's Stop arrives while its subagent is still
    // listed in `background_tasks` as running.
    let stop = f.run(
        "claude-code",
        f.captured("stop-with-running-subagent", ALICE),
    );
    assert_eq!(stop.coverage, "stopped", "{}", stop.detail);
    assert_eq!(
        f.leases(&claude(ALICE)),
        [(vec!["a.txt".to_owned()], LeaseStatus::Completed)]
    );
    let subagent = format!("{}.{CAPTURED_AGENT}", claude(ALICE));
    assert_eq!(
        f.leases(&subagent),
        [(vec!["b.txt".to_owned()], LeaseStatus::Active)]
    );
}

#[test]
fn a_subagent_stop_releases_and_ends_that_subagent_only() {
    let f = Fixture::new();
    f.claude_tool(ALICE, None, "PreToolUse", "call-parent", "a.txt");
    f.claude_tool(
        ALICE,
        Some(CAPTURED_AGENT),
        "PreToolUse",
        "call-sub",
        "b.txt",
    );
    let stop = f.run("claude-code", f.captured("subagent-stop", ALICE));
    assert_eq!(stop.coverage, "subagent_ended", "{}", stop.detail);
    assert!(stop.stdout.is_empty(), "{}", stop.stdout);
    let subagent = format!("{}.{CAPTURED_AGENT}", claude(ALICE));
    assert_eq!(
        f.leases(&subagent),
        [(vec!["b.txt".to_owned()], LeaseStatus::Completed)]
    );
    assert_eq!(
        f.leases(&claude(ALICE)),
        [(vec!["a.txt".to_owned()], LeaseStatus::Active)],
        "the parent is still in its reply"
    );
    let state = f.state();
    assert!(state.activity[&subagent].ended);
    assert!(
        state
            .activity
            .get(&claude(ALICE))
            .is_none_or(|activity| !activity.ended)
    );
}

#[test]
fn a_stop_failure_releases_and_records_only_the_error_type() {
    let f = Fixture::new();
    f.claude_tool(ALICE, None, "PreToolUse", "call-a", "a.txt");
    let stop = f.run("claude-code", f.captured("stop-failure", ALICE));
    assert_eq!(stop.coverage, "stopped", "{}", stop.detail);
    assert!(stop.detail.contains("model_not_found"), "{}", stop.detail);
    assert!(f.events().iter().any(|event| matches!(event,
        Event::SessionStopped { failure: Some(failure), released, .. }
            if failure == "model_not_found" && released.len() == 1)));
    // The rendered error text is never recorded.
    let log = serde_json::to_string(&f.events()).unwrap();
    assert!(!log.contains("issue with the selected model"), "{log}");

    // An error type outside the documented grammar is recorded as unknown.
    let mut odd = f.captured("stop-failure", ALICE);
    odd["error"] = json!("Some Free Text: 429");
    assert_eq!(f.run("claude-code", odd).coverage, "stopped");
    assert!(f.events().iter().any(|event| matches!(event,
        Event::SessionStopped { failure: Some(failure), .. } if failure == "unknown")));
}

#[test]
fn a_stop_from_a_session_the_coordinator_never_saw_does_nothing() {
    let f = Fixture::new();
    let stop = f.run("claude-code", f.captured("stop", BOB));
    assert_eq!(stop.coverage, "no_session", "{}", stop.detail);
    assert!(stop.stdout.is_empty());
    assert!(!f.state().sessions.contains_key(&claude(BOB)));
}

#[test]
fn a_tool_batch_is_recorded_and_carries_news_that_arrived_during_it() {
    let f = Fixture::new();
    let start = json!({"session_id": ALICE, "cwd": f.root(), "hook_event_name": "SessionStart",
                       "source": "startup", "transcript_path": "/dev/null"});
    f.run("claude-code", start);
    // Bob edits a file while Alice's batch runs, after its last PostToolUse.
    f.claude_tool(BOB, None, "PreToolUse", "bob-1", "src/lib.rs");
    fs::write(f.root().join("src/lib.rs"), "fn greet() -> u8 { 1 }\n").unwrap();
    assert_eq!(
        f.claude_tool(BOB, None, "PostToolUse", "bob-1", "src/lib.rs")
            .coverage,
        "observed"
    );
    let batch = f.run("claude-code", f.captured("post-tool-batch-edit", ALICE));
    assert_eq!(batch.coverage, "batch_briefed", "{}", batch.detail);
    let (name, context) = batch.context().expect("a briefing");
    assert_eq!(name, "PostToolBatch");
    assert!(context.contains("src/lib.rs"), "{context}");
    let edit =
        langspan::hook::identities_for("claude-code", ALICE, "toolu_01E2qxxgrdkPhuHHkCeKZzQn");
    assert!(f.events().iter().any(|event| matches!(event,
        Event::ToolBatchObserved { session_id, operation_ids, calls: 1, .. }
            if *session_id == claude(ALICE) && *operation_ids == [edit.operation_id.clone()])));

    // Nothing new since: recorded, and nothing to say.
    let again = f.run("claude-code", f.captured("post-tool-batch-reads", ALICE));
    assert_eq!(again.coverage, "batch_recorded", "{}", again.detail);
    assert!(again.stdout.is_empty());
}

#[test]
fn a_subagent_tool_batch_is_the_subagents_own() {
    let f = Fixture::new();
    let batch = f.run("claude-code", f.captured("post-tool-batch-subagent", ALICE));
    assert_eq!(batch.coverage, "batch_recorded", "{}", batch.detail);
    let subagent = format!("{}.{CAPTURED_AGENT}", claude(ALICE));
    let state = f.state();
    assert_eq!(
        state.sessions[&subagent].parent.as_deref(),
        Some(claude(ALICE).as_str())
    );
    assert!(f.events().iter().any(|event| matches!(event,
        Event::ToolBatchObserved { session_id, .. } if *session_id == subagent)));
}

#[test]
fn a_codex_stop_releases_the_turns_admissions_and_session_end_ends_it() {
    let f = Fixture::new();
    assert_eq!(
        f.codex_patch(ALICE, "PreToolUse", "call-1", None).coverage,
        "admitted"
    );
    let stop = f.codex_lifecycle(ALICE, "Stop");
    assert_eq!(stop.coverage, "stopped", "{}", stop.detail);
    assert!(stop.stdout.is_empty(), "{}", stop.stdout);
    assert_eq!(
        f.leases(&format!("codex.{ALICE}")),
        [(vec!["a.txt".to_owned()], LeaseStatus::Completed)]
    );
    let end = f.codex_lifecycle(ALICE, "SessionEnd");
    assert_eq!(end.coverage, "session_ended", "{}", end.detail);
    assert!(f.state().activity[&format!("codex.{ALICE}")].ended);

    // A Codex subagent's identity is not covered, so its stop is not taken
    // for the parent's.
    let mut sub = json!({"session_id": ALICE, "cwd": f.root(), "hook_event_name": "Stop",
                         "turn_id": "t", "agent_id": "agent-1"});
    assert_eq!(f.run("codex", sub.clone()).coverage, "unsupported");
    sub["hook_event_name"] = json!("SubagentStop");
    assert_eq!(f.run("codex", sub).coverage, "unsupported");
}

#[test]
fn a_codex_patch_that_failed_still_makes_its_next_attempt_compared() {
    let f = Fixture::new();
    // First patch of a.txt: no version recorded, so nothing is compared...
    let first = f.codex_patch(ALICE, "PreToolUse", "call-1", None);
    assert_eq!(first.coverage, "admitted");
    assert!(
        first.detail.contains("not compared for 1 file(s)"),
        "{}",
        first.detail
    );
    assert!(
        first.detail.contains("recorded as seen"),
        "{}",
        first.detail
    );
    // ...and it fails (the patch context did not match).
    assert_eq!(
        f.codex_patch(
            ALICE,
            "PostToolUse",
            "call-1",
            Some("Exit code: 1\nWall time: 0 seconds\nOutput:\nerror: no match\n")
        )
        .coverage,
        "observed"
    );
    assert!(f.events().iter().any(|event| matches!(event,
        Event::SourceObserved { origin: langspan_core::SourceOrigin::Snapshot, files, .. }
            if files.len() == 1 && files[0].path == "a.txt")));
    // Bob changes the file; Alice retries the patch she wrote before.
    f.claude_tool(BOB, None, "PreToolUse", "bob-1", "a.txt");
    fs::write(f.root().join("a.txt"), "changed by bob\n").unwrap();
    f.claude_tool(BOB, None, "PostToolUse", "bob-1", "a.txt");
    let retry = f.codex_patch(ALICE, "PreToolUse", "call-2", None);
    assert_eq!(retry.coverage, "denied_stale", "{}", retry.detail);
    assert!(retry.stdout.contains(&claude(BOB)), "{}", retry.stdout);
}

#[test]
fn a_claude_edit_of_an_unread_file_is_not_given_a_snapshot() {
    // Claude Code reports reads; an edit of a file it never read is left
    // unknown rather than recorded as seen.
    let f = Fixture::new();
    let pre = f.claude_tool(ALICE, None, "PreToolUse", "call-1", "a.txt");
    assert_eq!(pre.coverage, "admitted");
    assert!(!pre.detail.contains("recorded as seen"), "{}", pre.detail);
    assert!(!f.events().iter().any(|event| matches!(
        event,
        Event::SourceObserved {
            origin: langspan_core::SourceOrigin::Snapshot,
            ..
        }
    )));
}
