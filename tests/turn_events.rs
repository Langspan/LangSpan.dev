#![cfg(unix)]
//! The end of a reply (Stop, StopFailure, SubagentStop), the end of a tool
//! batch (PostToolBatch), Codex's Stop and SessionEnd, and the version a
//! Codex session is given of a file it first patches, through the real hook
//! executable and a real daemon. Claude Code events are the ones captured
//! from Claude Code 2.1.330 (`fixtures/claude-code-2.1.330`), with paths and
//! session IDs rewritten for each test. Codex events are the ones a real
//! Codex sent in the capture of `scripts/capture-codex.sh`
//! (`fixtures/codex-2026-11-26`), replayed in their order with this
//! fixture's checkout put in; the coordinator record of that run is
//! `docs/evidence/codex-turn-events/langspan-events.txt`.

use langspan::{client, protocol::Request};
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
        // The capture's repository held this one file.
        fs::write(base.join("work/notes.txt"), "alpha\nbeta\n").unwrap();
        let child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
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
    /// Every record: these tests read closed admissions, which `status`
    /// only counts.
    fn state(&self) -> State {
        client::full_state(&self.state_dir()).unwrap()
    }
    fn events(&self) -> Vec<Event> {
        let request = Request::new(
            "read",
            Command::Events {
                after: 0,
                limit: 1000,
            },
        );
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
            .env("LANGSPAN_AUTOSTART", "off")
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
    /// Event `number` (from 1) of the real Codex capture, in this fixture's
    /// checkout.
    fn codex_captured(&self, number: usize) -> Value {
        let line = include_str!("../fixtures/codex-2026-11-26/events.jsonl")
            .lines()
            .nth(number - 1)
            .unwrap()
            .replace("/work/repo", self.root().to_str().unwrap());
        serde_json::from_str(&line).unwrap()
    }
    /// Send captured Codex events `numbers` in order; the last one's run.
    fn codex_replay(&self, numbers: std::ops::RangeInclusive<usize>) -> Run {
        let mut last = None;
        for number in numbers {
            last = Some(self.run("codex", self.codex_captured(number)));
        }
        last.unwrap()
    }
    /// The capture's peer: the Claude Code `Write` of `notes.txt` that
    /// `scripts/capture-codex.sh` sends between the two Codex turns, with the
    /// file written between its two events as the script writes it.
    fn capture_peer(&self) {
        let event = |name: &str| {
            json!({
                "session_id": CAPTURE_PEER, "cwd": self.root(), "hook_event_name": name,
                "tool_name": "Write", "tool_use_id": "capture-peer-write",
                "tool_input": {"file_path": self.root().join("notes.txt"),
                               "content": "alpha\nbeta\npeer\n"},
            })
        };
        assert_eq!(
            self.run("claude-code", event("PreToolUse")).coverage,
            "admitted"
        );
        fs::write(self.root().join("notes.txt"), "alpha\nbeta\npeer\n").unwrap();
        let mut post = event("PostToolUse");
        post["tool_response"] = json!({"success": true});
        assert_eq!(self.run("claude-code", post).coverage, "observed");
    }
    /// The capture's CLI holder of `notes.txt`, as the script starts it.
    fn capture_holder(&self) {
        for args in [
            &[
                "session",
                "start",
                CAPTURE_HOLDER,
                "work",
                "--request-id",
                "hold-start",
            ][..],
            &[
                "admit",
                CAPTURE_HOLDER,
                "notes.txt",
                "--ttl-ms",
                "60000",
                "--request-id",
                "hold",
            ],
        ] {
            let run = Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(args)
                .env("LANGSPAN_STATE_DIR", self.state_dir())
                .env("LANGSPAN_AUTOSTART", "off")
                .current_dir(self.root())
                .output()
                .unwrap();
            assert!(
                run.status.success(),
                "{}",
                String::from_utf8_lossy(&run.stderr)
            );
        }
    }
}

/// The capture's two Codex sessions, as LangSpan names them, and the peer
/// and holder sessions the capture script adds.
const CODEX_A: &str = "codex.00000000-0000-7000-8000-00000000000a";
const CODEX_B: &str = "codex.00000000-0000-7000-8000-00000000000b";
const CAPTURE_PEER: &str = "capture-peer-repo-XXXXXX";
const CAPTURE_HOLDER: &str = "work.holder";
/// The real run's digest of `notes.txt` as `alpha\nbeta\n`, and of the
/// identical patch input refused and then admitted in turn 2.
const REAL_FIRST_VERSION: &str = "e49c81e2d2f84e259d40e2fb8192f3bcd198b355184845d76d8f58807d0d78ee";
const REAL_RETRIED_INPUT: &str = "14104956626e1f6dc00a8f26c87bff39503f8042eb39a5b2b719d38256d88004";

/// The coordinating events of a log, in order, with session names reduced
/// to the part a replay shares with the real run: what each admission,
/// version, refusal, stop and end was, not its IDs or times.
fn coordination(events: impl IntoIterator<Item = Value>) -> Vec<String> {
    let who = |session: &Value| {
        let session = session.as_str().unwrap();
        if session.contains("capture-peer") {
            "peer".to_owned()
        } else if session.ends_with(".holder") {
            "holder".to_owned()
        } else {
            session.to_owned()
        }
    };
    events
        .into_iter()
        .filter_map(|event| {
            Some(match event["type"].as_str()? {
                "lease_granted" => {
                    let lease = &event["lease"];
                    format!(
                        "granted {} {} base_unknown={} regions={}",
                        who(&lease["session_id"]),
                        lease["paths"],
                        lease["base_unknown"],
                        lease["regions"].is_array()
                    )
                }
                "lease_completed" => "completed".into(),
                "source_observed" => format!(
                    "version {} {} {}",
                    who(&event["session_id"]),
                    event["origin"],
                    event["files"]
                ),
                "write_refused" => format!(
                    "refused {} {} {} by {}",
                    who(&event["session_id"]),
                    event["kind"],
                    event["paths"],
                    who(&event["peer_session_id"])
                ),
                "operation_observed" => format!(
                    "observed {} {}",
                    who(&event["observation"]["session_id"]),
                    event["observation"]["result"]
                ),
                "session_stopped" => format!(
                    "stopped {} released {}",
                    who(&event["session_id"]),
                    event["released"].as_array()?.len()
                ),
                "session_ended" => format!("ended {}", who(&event["session_id"])),
                "session_prompted" => format!(
                    "prompted {} {}",
                    who(&event["session_id"]),
                    event["line_digests"]
                ),
                _ => return None,
            })
        })
        .collect()
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
    Request::new("read", Command::Status)
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
    // Captured turn 1: session start, prompt, `cat notes.txt`, then a patch
    // whose context does not apply. Codex sent no PostToolUse for that patch.
    assert_eq!(f.codex_replay(1..=5).coverage, "admitted");
    assert_eq!(
        f.leases(CODEX_A),
        [(vec!["notes.txt".to_owned()], LeaseStatus::Active)]
    );
    assert_eq!(
        f.claude_tool(BOB, None, "PreToolUse", "bob-1", "notes.txt")
            .coverage,
        "denied",
        "the open admission still holds the file"
    );
    // Codex's Stop: no `turn_id`, `stop_hook_active` false.
    let stop = f.run("codex", f.codex_captured(6));
    assert_eq!(stop.coverage, "stopped", "{}", stop.detail);
    assert!(stop.detail.contains("1 open admission"), "{}", stop.detail);
    assert!(stop.stdout.is_empty(), "{}", stop.stdout);
    assert_eq!(
        f.leases(CODEX_A),
        [(vec!["notes.txt".to_owned()], LeaseStatus::Completed)]
    );
    assert!(f.events().iter().any(|event| matches!(event,
        Event::SessionStopped { session_id, released, failure: None, .. }
            if session_id == CODEX_A && released.len() == 1)));
    // The file is free at once, not when the 30-second TTL runs out.
    assert_eq!(
        f.claude_tool(BOB, None, "PreToolUse", "bob-2", "notes.txt")
            .coverage,
        "admitted"
    );
    let end = f.run("codex", f.codex_captured(7));
    assert_eq!(end.coverage, "session_ended", "{}", end.detail);
    assert!(end.stdout.is_empty(), "{}", end.stdout);
    assert!(f.state().activity[CODEX_A].ended);
    // `codex exec resume` starts the same session again, and it is told of
    // Bob's work meanwhile, as the real run's was of its peer's.
    assert_eq!(f.codex_replay(8..=8).coverage, "briefed");
    assert!(!f.state().activity[CODEX_A].ended);

    // Constructed: the capture ran no subagent. A Codex subagent's identity
    // is not covered, so its stop is not taken for the parent's.
    let mut sub = json!({"session_id": ALICE, "cwd": f.root(), "hook_event_name": "Stop",
                         "turn_id": "t", "agent_id": "agent-1"});
    assert_eq!(f.run("codex", sub.clone()).coverage, "unsupported");
    sub["hook_event_name"] = json!("SubagentStop");
    assert_eq!(f.run("codex", sub).coverage, "unsupported");
}

#[test]
fn a_codex_patch_that_failed_still_makes_its_next_attempt_compared() {
    let f = Fixture::new();
    // Captured turn 1. The shell read is not reported as a read, so the
    // first patch of notes.txt has no version to be compared with...
    f.codex_replay(1..=4);
    let first = f.run("codex", f.codex_captured(5));
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
    // ...and is given one, the version the real run recorded too.
    assert!(f.events().iter().any(|event| matches!(event,
        Event::SourceObserved { origin: langspan_core::SourceOrigin::Snapshot, files, .. }
            if files.len() == 1 && files[0].path == "notes.txt"
                && serde_json::to_string(&files[0].state).unwrap().contains(REAL_FIRST_VERSION))));
    // The patch did not apply, and Codex sent no PostToolUse for it: the
    // reply ends, and so does `codex exec`.
    f.codex_replay(6..=7);

    // The peer writes notes.txt; the same Codex session, resumed, reads it
    // with `cat` and patches it.
    f.capture_peer();
    f.codex_replay(8..=11);
    let stale = f.run("codex", f.codex_captured(12));
    assert_eq!(stale.coverage, "denied_stale", "{}", stale.detail);
    assert!(
        stale.stdout.contains(&claude(CAPTURE_PEER)),
        "{}",
        stale.stdout
    );
    // Codex reread the file and sent the identical patch again: admitted.
    f.codex_replay(13..=14);
    let retry = f.run("codex", f.codex_captured(15));
    assert_eq!(retry.coverage, "admitted", "{}", retry.detail);
    let refused: Vec<_> = f
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::WriteRefused {
                session_id,
                peer_session_id,
                input_digest,
                ..
            } => Some((session_id, peer_session_id, input_digest)),
            _ => None,
        })
        .collect();
    assert_eq!(
        refused,
        [(
            CODEX_A.to_owned(),
            Some(claude(CAPTURE_PEER)),
            Some(REAL_RETRIED_INPUT.to_owned())
        )],
        "refused once, naming the peer, for the input the real run had"
    );
    fs::write(f.root().join("notes.txt"), "alpha\ngamma\npeer\n").unwrap();
    assert_eq!(f.codex_replay(16..=16).coverage, "observed");
    let stop = f.codex_replay(17..=19);
    assert_eq!(stop.coverage, "stopped", "{}", stop.detail);
    assert!(
        f.leases(CODEX_A)
            .iter()
            .all(|(_, status)| *status == LeaseStatus::Completed)
    );
}

#[test]
fn a_new_codex_session_is_refused_a_held_file_and_its_stop_releases_nothing() {
    let f = Fixture::new();
    f.codex_replay(1..=7);
    f.capture_holder();
    // Captured turn 3: a new session patches notes.txt while the CLI holds it.
    f.codex_replay(21..=24);
    let held = f.run("codex", f.codex_captured(25));
    assert_eq!(held.coverage, "denied", "{}", held.detail);
    assert!(held.stdout.contains(CAPTURE_HOLDER), "{}", held.stdout);
    let stop = f.run("codex", f.codex_captured(26));
    assert_eq!(stop.coverage, "stopped", "{}", stop.detail);
    assert!(f.events().iter().any(|event| matches!(event,
        Event::SessionStopped { session_id, released, .. }
            if session_id == CODEX_B && released.is_empty())));
    // The holder's admission was asked for by hand: a reply's end never
    // releases it.
    assert_eq!(
        f.leases(CAPTURE_HOLDER),
        [(vec!["notes.txt".to_owned()], LeaseStatus::Active)]
    );
    assert_eq!(f.codex_replay(27..=27).coverage, "session_ended");
}

/// The whole capture, replayed in order with the script's peer and holder
/// where the script ran them, coordinates exactly as the real run did.
#[test]
fn the_real_codex_capture_replays_to_the_real_runs_coordination() {
    let f = Fixture::new();
    f.codex_replay(1..=7);
    f.capture_peer();
    f.codex_replay(8..=15);
    fs::write(f.root().join("notes.txt"), "alpha\ngamma\npeer\n").unwrap();
    f.codex_replay(16..=20);
    f.capture_holder();
    f.codex_replay(21..=27);

    let real = include_str!("../docs/evidence/codex-turn-events/langspan-events.txt")
        .lines()
        .map(|line| serde_json::from_str(line.split_once(' ').unwrap().1).unwrap());
    let replayed = f
        .events()
        .into_iter()
        .map(|event| serde_json::to_value(event).unwrap());
    assert_eq!(coordination(replayed), coordination(real));
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
