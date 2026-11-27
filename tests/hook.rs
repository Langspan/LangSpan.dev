#![cfg(unix)]
//! The Claude Code hook as a real child process against a real daemon.

use langspan::{
    claude_code::identities,
    client,
    protocol::{Request, Response},
};
use langspan_core::{Command, Event, LeaseStatus, Outcome};
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

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
    ttl_ms: u64,
}

struct HookRun {
    stdout: String,
    coverage: String,
    detail: String,
    code: i32,
}

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "ls-hook-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(base.join("work/src")).unwrap();
        for file in ["a.txt", "b.txt", "src/c.rs"] {
            fs::write(base.join("work").join(file), "hello\n").unwrap();
        }
        Self {
            base,
            child: None,
            ttl_ms: 30_000,
        }
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn root(&self) -> PathBuf {
        fs::canonicalize(self.base.join("work")).unwrap()
    }
    fn start(&mut self) {
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&self.state_dir(), &request("ready", Command::Status)).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn call(&self, id: &str, command: Command) -> Response {
        client::call(&self.state_dir(), &request(id, command)).unwrap()
    }
    fn events(&self) -> Vec<Event> {
        match self
            .call(
                "read",
                Command::Events {
                    after: 0,
                    limit: 1000,
                },
            )
            .outcome
        {
            Outcome::Events { events } => events.into_iter().map(|e| e.event).collect(),
            other => panic!("{other:?}"),
        }
    }
    /// Closed admissions are only counted by `status`; `status --all` lists them.
    fn lease_status(&self, lease_id: &str) -> LeaseStatus {
        client::full_state(&self.state_dir()).unwrap().leases[lease_id]
            .status
            .clone()
    }
    fn event(&self, name: &str, session: &str, tool_use_id: &str, file: &str) -> Value {
        let mut event = json!({
            "session_id": session,
            "transcript_path": "/dev/null",
            "cwd": self.root(),
            "permission_mode": "default",
            "hook_event_name": name,
            "tool_name": "Edit",
            "tool_input": {
                "file_path": self.root().join(file),
                "old_string": "hello",
                "new_string": "hello world",
                "replace_all": false
            },
            "tool_use_id": tool_use_id,
        });
        if name == "PostToolUseFailure" {
            event["error"] = json!("EACCES");
            event["is_interrupt"] = json!(false);
        }
        event
    }
    fn hook(&self, event: &Value) -> HookRun {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "demo", "--root"])
            .arg(self.root())
            .args(["--ttl-ms", &self.ttl_ms.to_string()])
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
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap())
            .unwrap_or_else(|_| panic!("structured diagnostic expected: {stderr}"));
        let run = HookRun {
            stdout: String::from_utf8(output.stdout).unwrap(),
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            code: output.status.code().unwrap(),
        };
        // The adapter never allows and never uses the blocking exit status.
        assert_eq!(run.code, 0, "{stderr}");
        assert!(!run.stdout.contains("\"allow\""), "{}", run.stdout);
        run
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn request(id: &str, command: Command) -> Request {
    Request::new(id, command)
}

fn denial(run: &HookRun) -> Value {
    let value: Value = serde_json::from_str(&run.stdout).expect("deny JSON");
    assert_eq!(value["hookSpecificOutput"]["permissionDecision"], "deny");
    value
}

fn granted(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Event::LeaseGranted { .. }))
        .count()
}

#[test]
fn conflicting_edit_is_denied_and_unrelated_edit_gets_no_decision() {
    let mut f = Fixture::new();
    f.start();
    let a = f.hook(&f.event("PreToolUse", ALICE, "toolu_a1", "a.txt"));
    assert_eq!((a.coverage.as_str(), a.stdout.as_str()), ("admitted", ""));

    let b = f.hook(&f.event("PreToolUse", BOB, "toolu_b1", "a.txt"));
    assert_eq!(b.coverage, "denied");
    let reason = denial(&b)["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(reason.contains(&format!("claude-code.{ALICE}")), "{reason}");
    assert!(reason.contains("a.txt"));

    let other = f.hook(&f.event("PreToolUse", BOB, "toolu_b2", "src/c.rs"));
    assert_eq!(
        (other.coverage.as_str(), other.stdout.as_str()),
        ("admitted", "")
    );

    let done = f.hook(&f.event("PostToolUse", ALICE, "toolu_a1", "a.txt"));
    assert_eq!(done.coverage, "observed", "{}", done.detail);
    let alice = identities(ALICE, "toolu_a1");
    assert_eq!(f.lease_status(&alice.admit), LeaseStatus::Completed);

    let retry = f.hook(&f.event("PreToolUse", BOB, "toolu_b3", "a.txt"));
    assert_eq!(retry.coverage, "admitted", "a fresh attempt proceeds");
}

#[test]
fn duplicate_delivery_replays_and_changed_input_under_one_identity_is_a_mismatch() {
    let mut f = Fixture::new();
    f.start();
    let event = f.event("PreToolUse", ALICE, "toolu_dup", "a.txt");
    assert_eq!(f.hook(&event).coverage, "admitted");
    assert_eq!(f.hook(&event).coverage, "admitted");
    assert_eq!(granted(&f.events()), 1, "a duplicate delivery is a retry");

    let changed = f.hook(&f.event("PreToolUse", ALICE, "toolu_dup", "b.txt"));
    assert_eq!(changed.coverage, "unknown");
    assert!(
        changed.detail.contains("request_id_conflict"),
        "{}",
        changed.detail
    );
    assert_eq!(
        changed.stdout, "",
        "a mismatch is no decision, never a denial"
    );
    assert_eq!(granted(&f.events()), 1);

    let post = f.event("PostToolUse", ALICE, "toolu_dup", "a.txt");
    assert_eq!(f.hook(&post).coverage, "observed");
    assert_eq!(f.hook(&post).coverage, "observed");
    let observed = f
        .events()
        .iter()
        .filter(|e| matches!(e, Event::OperationObserved { .. }))
        .count();
    assert_eq!(observed, 1, "a duplicate post-tool delivery records once");
}

#[test]
fn a_result_reported_after_expiry_is_a_late_observation_that_revives_nothing() {
    let mut f = Fixture::new();
    f.ttl_ms = 200;
    f.start();
    assert_eq!(
        f.hook(&f.event("PreToolUse", ALICE, "toolu_slow", "a.txt"))
            .coverage,
        "admitted"
    );
    // Stands in for a permission prompt held open past the admission.
    std::thread::sleep(Duration::from_millis(400));
    f.ttl_ms = 30_000;
    assert_eq!(
        f.hook(&f.event("PreToolUse", BOB, "toolu_next", "a.txt"))
            .coverage,
        "admitted"
    );
    let late = f.hook(&f.event("PostToolUse", ALICE, "toolu_slow", "a.txt"));
    assert_eq!(late.coverage, "late_observation", "{}", late.detail);
    let alice = identities(ALICE, "toolu_slow");
    let bob = identities(BOB, "toolu_next");
    assert_eq!(f.lease_status(&alice.admit), LeaseStatus::Expired);
    assert_eq!(f.lease_status(&bob.admit), LeaseStatus::Active);
    let holders = f
        .events()
        .into_iter()
        .find_map(|e| match e {
            Event::OperationObserved { observation } => Some(observation.concurrent_holders),
            _ => None,
        })
        .unwrap();
    assert_eq!(holders[0].session_id, format!("claude-code.{BOB}"));
}

#[test]
fn restart_between_pre_and_post_records_an_invalidated_late_observation() {
    let mut f = Fixture::new();
    f.start();
    assert_eq!(
        f.hook(&f.event("PreToolUse", ALICE, "toolu_r", "a.txt"))
            .coverage,
        "admitted"
    );
    f.stop();
    f.start();
    let late = f.hook(&f.event("PostToolUse", ALICE, "toolu_r", "a.txt"));
    assert_eq!(late.coverage, "late_observation", "{}", late.detail);
    assert!(late.detail.contains("invalidated"), "{}", late.detail);
    let alice = identities(ALICE, "toolu_r");
    assert_eq!(f.lease_status(&alice.admit), LeaseStatus::Invalidated);
    // A new tool call reattaches the session in the new generation.
    assert_eq!(
        f.hook(&f.event("PreToolUse", ALICE, "toolu_r2", "a.txt"))
            .coverage,
        "admitted"
    );
}

#[test]
fn failures_and_interruptions_are_observed_and_release_the_admission() {
    let mut f = Fixture::new();
    f.start();
    f.hook(&f.event("PreToolUse", ALICE, "toolu_f", "a.txt"));
    let failed = f.hook(&f.event("PostToolUseFailure", ALICE, "toolu_f", "a.txt"));
    assert_eq!(failed.coverage, "observed");
    assert_eq!(
        f.lease_status(&identities(ALICE, "toolu_f").admit),
        LeaseStatus::Completed
    );

    f.hook(&f.event("PreToolUse", ALICE, "toolu_i", "a.txt"));
    let mut interrupted = f.event("PostToolUseFailure", ALICE, "toolu_i", "a.txt");
    interrupted["is_interrupt"] = json!(true);
    assert_eq!(f.hook(&interrupted).coverage, "observed");
    let results: Vec<_> = f
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Event::OperationObserved { observation } => Some(observation.result),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [
            langspan_core::ToolResult::Failed,
            langspan_core::ToolResult::Interrupted
        ]
    );
}

#[test]
fn a_matching_independently_committed_admission_is_completed_by_the_post_event() {
    let mut f = Fixture::new();
    f.start();
    // This constructs the matching admission directly; it exercises recovery
    // by the post callback, not a forced timeout or a genuinely lost reply.
    f.hook(&f.event("PreToolUse", ALICE, "toolu_warm", "b.txt"));
    let ids = identities(ALICE, "toolu_lost");
    let post_event = f.event("PostToolUse", ALICE, "toolu_lost", "a.txt");
    let committed = f.call(
        &ids.admit,
        Command::AdmitTool {
            session_id: ids.session_id.clone(),
            paths: vec!["a.txt".into()],
            ttl_ms: 30_000,
            operation: langspan_core::ToolIdentity {
                operation_id: ids.operation_id.clone(),
                source: "claude-code".into(),
                tool: "Edit".into(),
                input_digest: langspan::hook::input_fingerprint("Edit", &post_event["tool_input"]),
            },
            base: Vec::new(),
            regions: Vec::new(),
            overwrites: Vec::new(),
        },
    );
    assert!(matches!(committed.outcome, Outcome::Admitted { .. }));
    let post = f.hook(&post_event);
    assert_eq!(post.coverage, "observed", "{}", post.detail);
    assert_eq!(f.lease_status(&ids.admit), LeaseStatus::Completed);
}

#[test]
fn unavailable_coordination_and_unsupported_calls_get_no_decision() {
    let mut f = Fixture::new();
    let down = f.hook(&f.event("PreToolUse", ALICE, "toolu_down", "a.txt"));
    assert_eq!(
        (down.coverage.as_str(), down.stdout.as_str()),
        ("unavailable", "")
    );

    f.start();
    let mut no_identity = f.event("PreToolUse", ALICE, "x", "a.txt");
    no_identity.as_object_mut().unwrap().remove("tool_use_id");
    let mut bash = f.event("PreToolUse", ALICE, "toolu_bash", "a.txt");
    bash["tool_name"] = json!("Bash");
    let mut outside = f.event("PreToolUse", ALICE, "toolu_out", "a.txt");
    outside["tool_input"]["file_path"] = json!(f.base.join("outside.txt"));
    for event in [no_identity, bash, outside] {
        let run = f.hook(&event);
        assert_eq!(
            (run.coverage.as_str(), run.stdout.as_str()),
            ("unsupported", "")
        );
    }
    let mut malformed = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["hook", "claude-code", "--state-dir"])
        .arg(f.state_dir())
        .args(["--workspace", "demo", "--root"])
        .arg(f.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    malformed
        .stdin
        .take()
        .unwrap()
        .write_all(b"{not json")
        .unwrap();
    let output = malformed.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert_eq!(granted(&f.events()), 0);
}
