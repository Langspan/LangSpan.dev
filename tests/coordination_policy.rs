#![cfg(unix)]
//! Coordination policies (roadmap B28, ADR 0036) through the real hook
//! executable, a real daemon and a real Git checkout: a wait for a current
//! holder, and the refusals no setting makes anything else.

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

#[allow(dead_code)] // Only the wait helpers are used here.
mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";
const LINES: &str = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\n";

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
    calls: AtomicU64,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    detail: String,
    reason: Option<String>,
    elapsed: Duration,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-policy-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let hub = base.join("hub.git");
        git(
            &base,
            &["init", "-q", "--bare", "-b", "main", hub.to_str().unwrap()],
        );
        let work = base.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("a.txt"), "hello\n").unwrap();
        fs::write(work.join("b.txt"), "beta\n").unwrap();
        fs::write(work.join("lines.txt"), LINES).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["config", "user.name", "Owner"]);
        git(&work, &["config", "user.email", "owner@example.invalid"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "start"]);
        git(&work, &["remote", "add", "origin", hub.to_str().unwrap()]);
        git(&work, &["push", "-q", "origin", "main"]);
        let child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Owned before anything can panic, so `Drop` stops it (issue #239).
        let fixture = Self {
            base,
            child: Some(child),
            calls: AtomicU64::new(0),
        };
        let socket = fixture.base.join("state/daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        // As the first hook run would.
        let root = fixture.work().to_string_lossy().into_owned();
        let (code, text) = fixture.cli(&["workspace", "add", "work", &root, "--request-id", "w"]);
        assert_eq!(code, 0, "{text}");
        // Setting a policy is an owner grant; this fixture's owner allows it.
        support::owner_grants(&fixture.base.join("state"), true);
        fixture
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn call(&self) -> String {
        format!("call-{}", self.calls.fetch_add(1, Ordering::Relaxed))
    }
    fn hook(&self, vendor: &str, budget_ms: Option<u64>, event: &Value) -> Run {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        command
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "work", "--root"])
            .arg(self.work())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(budget) = budget_ms {
            command.args(["--budget-ms", &budget.to_string()]);
        }
        let started = Instant::now();
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let elapsed = started.elapsed();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let reply: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            reason: reply.and_then(|reply| {
                reply["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .map(str::to_owned)
            }),
            elapsed,
        }
    }
    fn claude_event(
        &self,
        session: &str,
        call: &str,
        phase: &str,
        tool: &str,
        input: Value,
    ) -> Value {
        json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": phase,
            "tool_name": tool, "tool_use_id": call, "tool_input": input,
            "tool_response": {"filePath": input["file_path"]},
        })
    }
    fn write_input(&self, file: &str, content: &str) -> Value {
        json!({"file_path": self.work().join(file), "content": content})
    }
    /// Alice's Claude Code write of `a.txt`, admitted and still open, as at
    /// a permission prompt.
    fn alice_holds(&self, call: &str) {
        let input = self.write_input("a.txt", "alice\n");
        let pre = self.hook(
            "claude-code",
            None,
            &self.claude_event(ALICE, call, "PreToolUse", "Write", input),
        );
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
    }
    /// Alice's held write ends: written and reported, or failed and reported.
    fn alice_ends(&self, call: &str, written: bool) {
        let input = self.write_input("a.txt", "alice\n");
        let phase = if written {
            fs::write(self.work().join("a.txt"), "alice\n").unwrap();
            "PostToolUse"
        } else {
            "PostToolUseFailure"
        };
        let post = self.hook(
            "claude-code",
            None,
            &self.claude_event(ALICE, call, phase, "Write", input),
        );
        assert_eq!(post.coverage, "observed", "{post:?}");
    }
    fn codex(&self, session: &str, call: &str, phase: &str, budget: Option<u64>) -> Run {
        let mut event = json!({
            "session_id": session, "turn_id": "turn", "cwd": self.work(),
            "hook_event_name": phase, "tool_name": "apply_patch", "tool_use_id": call,
            "tool_input": {"command":
                "*** Begin Patch\n*** Update File: a.txt\n@@\n-hello\n+hi\n*** End Patch"},
        });
        if phase == "PostToolUse" {
            event["tool_response"] = json!("Success. Updated the following files:\nM a.txt\n");
        }
        self.hook("codex", budget, &event)
    }
    fn claude_write(
        &self,
        session: &str,
        call: &str,
        file: &str,
        content: &str,
        budget: Option<u64>,
    ) -> Run {
        let input = self.write_input(file, content);
        self.hook(
            "claude-code",
            budget,
            &self.claude_event(session, call, "PreToolUse", "Write", input),
        )
    }
    /// A complete Claude Code edit: admitted, written, reported.
    fn claude_edit(&self, session: &str, file: &str, old: &str, new: &str) -> Run {
        let call = self.call();
        let path = self.work().join(file);
        let input = json!({"file_path": path, "old_string": old, "new_string": new});
        let pre = self.hook(
            "claude-code",
            None,
            &self.claude_event(session, &call, "PreToolUse", "Edit", input.clone()),
        );
        if pre.coverage != "admitted" {
            return pre;
        }
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replacen(old, new, 1)).unwrap();
        let post = self.hook(
            "claude-code",
            None,
            &self.claude_event(session, &call, "PostToolUse", "Edit", input),
        );
        assert_eq!(post.coverage, "observed", "{post:?}");
        pre
    }
    fn read(&self, session: &str, file: &str) {
        let call = self.call();
        let run = self.hook(
            "claude-code",
            None,
            &json!({
                "session_id": session, "cwd": self.work(), "hook_event_name": "PostToolUse",
                "tool_name": "Read", "tool_use_id": call,
                "tool_input": {"file_path": self.work().join(file)},
            }),
        );
        assert_eq!(run.coverage, "read_recorded", "{run:?}");
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .current_dir(self.work())
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }
    /// Returns once the hook run `run` of `session` (`vendor.id`) has
    /// reached the coordinator and then gone on waiting for 400 ms, twice
    /// the 200 ms a test finds recorded as waited. Its session is registered
    /// before it asks for the admission it waits on.
    fn waits(&self, session: &str, run: &std::thread::ScopedJoinHandle<'_, Run>) {
        support::event_recorded(
            &self.base.join("state"),
            &format!("{session} registered"),
            |event| {
                matches!(event, langspan_core::Event::SessionRegistered { session: s }
                    if s.session_id == session)
            },
        );
        support::holds_for(
            &format!("{session}'s edit waiting"),
            Duration::from_millis(400),
            || !run.is_finished(),
        );
    }
    fn wait_policy(&self, extra: &[&str]) {
        let (code, text) = self.cli(
            &[
                &["policy", "set", "work", "conflict", "wait"][..],
                extra,
                &["--request-id", &self.call()],
            ]
            .concat(),
        );
        assert_eq!(code, 0, "{text}");
    }
    /// The conflicts the dashboard lists as refusals.
    fn dashboard_conflicts(&self) -> usize {
        let request = langspan::protocol::Request::new("read", langspan_core::Command::Dashboard);
        match langspan::client::call(&self.base.join("state"), &request)
            .unwrap()
            .outcome
        {
            langspan_core::Outcome::Dashboard { dashboard } => {
                dashboard.denials.unwrap().conflicts.len()
            }
            other => panic!("{other:?}"),
        }
    }
    /// Every recorded event of the given types, as `langspan events --json`
    /// prints them.
    fn events(&self, types: &[&str]) -> Vec<Value> {
        let (code, text) = self.cli(&["events", "--limit", "1000", "--json"]);
        assert_eq!(code, 0, "{text}");
        let reply: Value = serde_json::from_str(&text).unwrap();
        reply["outcome"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|recorded| recorded["event"].clone())
            .filter(|event| types.contains(&event["type"].as_str().unwrap()))
            .collect()
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

/// The issue's example: Bob's Codex edit waits while Alice's held write sits
/// at a permission prompt; her write fails without changing the file, so his
/// edit is admitted and his post-tool run completes that admission.
#[test]
fn a_wait_admits_the_edit_once_the_holder_ends_its_admission_with_the_file_unchanged() {
    let f = Fixture::new();
    f.wait_policy(&["--wait-ms", "2000"]);
    f.alice_holds("alice-1");
    let bob = std::thread::scope(|scope| {
        let bob = scope.spawn(|| f.codex(BOB, "bob-1", "PreToolUse", Some(4000)));
        f.waits(&format!("codex.{BOB}"), &bob);
        f.alice_ends("alice-1", false);
        bob.join().unwrap()
    });
    assert_eq!(bob.coverage, "admitted", "{bob:?}");
    assert!(bob.detail.contains("after waiting"), "{}", bob.detail);
    assert!(bob.reason.is_none());
    // The host writes, and the post-tool run finds the admission it waited
    // for: observed and completed, not an unadmitted write.
    fs::write(f.work().join("a.txt"), "hi\n").unwrap();
    let post = f.codex(BOB, "bob-1", "PostToolUse", None);
    assert_eq!(post.coverage, "observed", "{post:?}");
    assert!(post.detail.contains("admission completed"), "{post:?}");

    let waited = f.events(&["write_waited"]);
    assert_eq!(waited.len(), 1, "{waited:?}");
    assert_eq!(waited[0]["session_id"], format!("codex.{BOB}"));
    assert_eq!(waited[0]["paths"], json!(["a.txt"]));
    assert_eq!(
        waited[0]["holder_session_id"],
        format!("claude-code.{ALICE}")
    );
    assert_eq!(waited[0]["result"], "admitted");
    assert!(
        waited[0]["waited_ms"].as_u64().unwrap() >= 200,
        "{waited:?}"
    );
    // No refusal was given, so none is counted or shown.
    assert!(f.events(&["write_refused"]).is_empty());
    assert_eq!(f.dashboard_conflicts(), 0);
}

/// Review follow-up #244: the host delivers Bob's pre-tool event again
/// after his wait admitted it, and gets the same admission, not a
/// historical conflict.
#[test]
fn a_pre_tool_event_delivered_again_after_a_wait_keeps_its_admission() {
    let f = Fixture::new();
    f.wait_policy(&["--wait-ms", "2000"]);
    f.alice_holds("alice-1");
    let bob = std::thread::scope(|scope| {
        let bob = scope.spawn(|| f.codex(BOB, "bob-1", "PreToolUse", Some(4000)));
        f.waits(&format!("codex.{BOB}"), &bob);
        f.alice_ends("alice-1", false);
        bob.join().unwrap()
    });
    assert_eq!(bob.coverage, "admitted", "{bob:?}");
    let again = f.codex(BOB, "bob-1", "PreToolUse", None);
    assert_eq!(again.coverage, "admitted", "{again:?}");
    assert_eq!(f.events(&["write_waited"]).len(), 1);
}

/// Review follow-up #244: while Bob waits, `a.txt` becomes a symlink, which
/// has no version to compare. The edit was planned against a version that
/// can no longer be read, so the wait decides nothing (`unknown`) instead of
/// admitting it as unchanged. A real daemon and hook: what is verified is
/// the file read after a concurrent holder lets go.
#[test]
fn a_wait_whose_file_can_no_longer_be_read_decides_nothing() {
    let f = Fixture::new();
    f.wait_policy(&["--wait-ms", "2000"]);
    f.alice_holds("alice-1");
    let bob = std::thread::scope(|scope| {
        let bob = scope.spawn(|| f.codex(BOB, "bob-1", "PreToolUse", Some(4000)));
        f.waits(&format!("codex.{BOB}"), &bob);
        let file = f.work().join("a.txt");
        fs::write(f.work().join("elsewhere.txt"), "other\n").unwrap();
        fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink("elsewhere.txt", &file).unwrap();
        // Alice's write fails; her report sees the symlink too, so it is not
        // `observed`, but it ends her admission all the same.
        let input = f.write_input("a.txt", "alice\n");
        let event = f.claude_event(ALICE, "alice-1", "PostToolUseFailure", "Write", input);
        f.hook("claude-code", None, &event);
        bob.join().unwrap()
    });
    assert_eq!(bob.coverage, "unknown", "{bob:?}");
    assert!(bob.detail.contains("a.txt"), "{}", bob.detail);
    assert!(f.events(&["write_waited"]).is_empty());
}

/// A holder that never lets go: the wait ends with today's conflict
/// refusal, saying how long it waited, before the hook's deadline.
#[test]
fn a_wait_for_a_holder_that_never_releases_ends_in_a_conflict_before_the_deadline() {
    let f = Fixture::new();
    f.wait_policy(&[]);
    f.alice_holds("alice-1");
    for (call, budget, limit) in [
        ("bob-1", None, Duration::from_millis(1500)),
        ("bob-2", Some(4000), Duration::from_millis(4000)),
    ] {
        let bob = f.claude_write(BOB, call, "a.txt", "bob\n", budget);
        assert_eq!(bob.coverage, "denied", "{bob:?}");
        assert!(bob.elapsed < limit, "{budget:?}: {:?}", bob.elapsed);
        assert!(bob.detail.contains("after waiting"), "{}", bob.detail);
        let reason = bob.reason.unwrap();
        assert!(
            reason.contains("LangSpan coordination conflict."),
            "{reason}"
        );
        assert!(reason.contains("This edit waited"), "{reason}");
        assert!(reason.contains(&format!("claude-code.{ALICE}")), "{reason}");
    }
    let waited = f.events(&["write_waited"]);
    assert_eq!(waited.len(), 2, "{waited:?}");
    for wait in &waited {
        assert_eq!(wait["result"], "refused_conflict");
    }
    // The longer budget waited longer: the whole 2,000 ms bound fits.
    let first = waited[0]["waited_ms"].as_u64().unwrap();
    let second = waited[1]["waited_ms"].as_u64().unwrap();
    assert!(first < 1000 && second >= 1500, "{first} {second}");
    let refused = f.events(&["write_refused"]);
    assert_eq!(refused.len(), 2, "one refusal per wait: {refused:?}");
    assert_eq!(f.dashboard_conflicts(), 2);
    assert!(refused.iter().all(|event| event["kind"] == "conflict"));
}

/// The holder writes the file during the wait: the edit was planned
/// against content that is gone, so it is refused as stale, naming the
/// path. The retry is admitted, and the refusal is settled with the time
/// it took.
#[test]
fn a_wait_whose_holder_writes_the_file_ends_in_a_stale_refusal_naming_it() {
    let f = Fixture::new();
    f.wait_policy(&[]);
    f.alice_holds("alice-1");
    let bob = std::thread::scope(|scope| {
        let bob = scope.spawn(|| f.claude_write(BOB, "bob-1", "a.txt", "bob\n", Some(4000)));
        f.waits(&format!("claude-code.{BOB}"), &bob);
        f.alice_ends("alice-1", true);
        bob.join().unwrap()
    });
    assert_eq!(bob.coverage, "denied_stale", "{bob:?}");
    let reason = bob.reason.unwrap();
    assert!(reason.contains("LangSpan stale source"), "{reason}");
    assert!(
        reason.contains("changed while this edit waited"),
        "{reason}"
    );
    assert!(reason.contains(r#""a.txt""#), "{reason}");
    assert!(reason.contains(&format!("claude-code.{ALICE}")), "{reason}");
    assert_eq!(
        fs::read_to_string(f.work().join("a.txt")).unwrap(),
        "alice\n"
    );

    // Told once: the same edit again is admitted.
    let retry = f.claude_write(BOB, "bob-2", "a.txt", "bob\n", None);
    assert_eq!(retry.coverage, "admitted", "{retry:?}");

    let waited = f.events(&["write_waited"]);
    assert_eq!(waited.len(), 1, "{waited:?}");
    assert_eq!(waited[0]["result"], "refused_stale");
    let refused = f.events(&["write_refused"]);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0]["kind"], "stale_base");
    // One refusal, the stale one: the conflict that started the wait is
    // not listed as another (review follow-up #244).
    assert_eq!(f.dashboard_conflicts(), 0);
    assert_eq!(
        refused[0]["peer_session_id"],
        format!("claude-code.{ALICE}")
    );
    let settled = f.events(&["refusal_settled"]);
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["session_id"], format!("claude-code.{BOB}"));
    assert_eq!(settled[0]["kind"], "stale_base");
    assert_eq!(settled[0]["outcome"], "retried_admitted");
    assert!(
        settled[0]["ms_to_next_admitted"].as_u64().is_some(),
        "{settled:?}"
    );
}

/// `refuse` restores today's behavior; every other kind is fixed.
#[test]
fn a_configured_refuse_restores_todays_conflict_and_every_other_kind_is_fixed() {
    let f = Fixture::new();
    // The workspace exists once a hook has run in it.
    f.alice_holds("alice-1");
    let (code, text) = f.cli(&["policy", "work"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("conflict           refuse\n"), "{text}");
    assert!(text.contains("stale_base         refuse (fixed)"), "{text}");
    assert!(
        text.contains("advisory           advisory (fixed)"),
        "{text}"
    );

    f.wait_policy(&["--wait-ms", "300"]);
    let (_, text) = f.cli(&["policy", "work", "--json"]);
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        reply["outcome"]["rows"][1],
        json!({"case": "conflict", "policy": "wait", "configurable": true, "wait_ms": 300})
    );
    let (code, text) = f.cli(&[
        "policy",
        "set",
        "work",
        "conflict",
        "refuse",
        "--request-id",
        "r1",
    ]);
    assert_eq!(code, 0, "{text}");
    let bob = f.claude_write(BOB, "bob-1", "a.txt", "bob\n", None);
    assert_eq!(bob.coverage, "denied", "{bob:?}");
    assert!(
        bob.detail.starts_with("conflict with a current holder") && !bob.detail.contains("wait"),
        "{}",
        bob.detail
    );
    assert!(!bob.reason.unwrap().contains("waited"));
    assert!(f.events(&["write_waited"]).is_empty());
    assert_eq!(f.events(&["write_refused"]).len(), 1);

    for kind in [
        "advisory",
        "stale_base",
        "unread_lines",
        "commit_held",
        "landing_not_ready",
    ] {
        let (code, text) = f.cli(&[
            "policy",
            "set",
            "work",
            kind,
            "wait",
            "--request-id",
            &f.call(),
        ]);
        assert_eq!(code, 1, "{kind}: {text}");
        assert!(text.contains(kind) && text.contains("fixed"), "{text}");
    }
    let (code, text) = f.cli(&[
        "policy",
        "set",
        "work",
        "conflict",
        "wait",
        "--wait-ms",
        "2001",
        "--request-id",
        "r2",
    ]);
    assert_eq!(code, 1, "{text}");
    let (code, text) = f.cli(&[
        "policy",
        "set",
        "work",
        "landing",
        "wait",
        "--request-id",
        "r3",
    ]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("unknown case"), "{text}");
}

/// With the wait policy on, a file another session does not hold is
/// admitted at once.
#[test]
fn a_write_of_a_file_nobody_holds_never_waits() {
    let f = Fixture::new();
    f.wait_policy(&[]);
    // Alice and Bob are both at work; Bob then writes a file Alice is not
    // editing while she holds hers.
    f.alice_holds("alice-1");
    let bob = f.claude_write(BOB, "bob-1", "b.txt", "bob\n", None);
    assert_eq!(bob.coverage, "admitted", "{bob:?}");
    assert!(!bob.detail.contains("waiting"), "{}", bob.detail);
    assert!(f.events(&["write_waited"]).is_empty());
}

/// The refusals no setting changes, with the conflict policy at `wait`:
/// a stale source, unread lines, a commit hold and a landing that is not
/// ready.
#[test]
fn stale_source_unread_lines_commit_holds_and_landings_stay_refusals_with_a_wait_set() {
    let f = Fixture::new();
    // Stale source: Alice read a.txt, Bob's whole patch changed it.
    f.read(ALICE, "a.txt");
    f.wait_policy(&[]);
    assert_eq!(
        f.codex(BOB, "bob-1", "PreToolUse", None).coverage,
        "admitted"
    );
    fs::write(f.work().join("a.txt"), "hi\n").unwrap();
    assert_eq!(
        f.codex(BOB, "bob-1", "PostToolUse", None).coverage,
        "observed"
    );
    let stale = f.claude_write(ALICE, "alice-1", "a.txt", "alice\n", None);
    assert_eq!(stale.coverage, "denied_stale", "{stale:?}");

    // Unread lines: Alice wrote line 3; Bob's patch over it read nothing.
    assert_eq!(
        f.claude_edit(ALICE, "lines.txt", "line 3", "alice 3")
            .coverage,
        "admitted"
    );
    let patch = f.hook(
        "codex",
        None,
        &json!({
            "session_id": BOB, "turn_id": "turn", "cwd": f.work(),
            "hook_event_name": "PreToolUse", "tool_name": "apply_patch",
            "tool_use_id": "bob-2",
            "tool_input": {"command":
                "*** Begin Patch\n*** Update File: lines.txt\n@@\n line 2\n-alice 3\n+bob 3\n line 4\n*** End Patch"},
        }),
    );
    assert_eq!(patch.coverage, "denied_unread", "{patch:?}");

    // Commit hold: Alice's commit of everything would take Bob's a.txt.
    let commit = f.hook(
        "claude-code",
        None,
        &json!({
            "session_id": ALICE, "cwd": f.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Bash", "tool_use_id": "alice-2",
            "tool_input": {"command": "git commit -am 'Alice: lines'", "description": "commit"},
        }),
    );
    assert_eq!(commit.coverage, "denied_commit", "{commit:?}");

    // Landing without a check of that name: not ready, exit code 6.
    git(&f.work(), &["checkout", "-q", "-b", "feature"]);
    fs::write(f.work().join("feature.txt"), "feature\n").unwrap();
    git(&f.work(), &["add", "feature.txt"]);
    git(&f.work(), &["commit", "-q", "-m", "feature"]);
    git(&f.work(), &["checkout", "-q", "main"]);
    let (code, text) = f.cli(&[
        "land", "work", "--onto", "main", "--name", "unit", "feature",
    ]);
    assert_eq!(code, 6, "{text}");

    assert!(f.events(&["write_waited"]).is_empty(), "nothing waited");
    let kinds: Vec<Value> = f
        .events(&["write_refused"])
        .into_iter()
        .map(|event| event["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        vec![
            json!("stale_base"),
            json!("unread_lines"),
            json!("commit_held")
        ]
    );
}
