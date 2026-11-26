#![cfg(unix)]
//! Stale-source refusal through the real hook executable and a real daemon,
//! with the files really changing on disk between a read and an edit.

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
    calls: AtomicU64,
}

struct Run {
    coverage: String,
    detail: String,
    reason: Option<String>,
    context: Option<String>,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-stale-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(base.join("work")).unwrap();
        fs::write(base.join("work/a.txt"), "hello\n").unwrap();
        fs::write(base.join("work/b.txt"), "hello\n").unwrap();
        let mut fixture = Self {
            base,
            child: None,
            calls: AtomicU64::new(0),
        };
        fixture.start();
        fixture
    }
    fn start(&mut self) {
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.base.join("state"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.base.join("state/daemon.sock").exists() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// Stop the daemon as a crash or a closed laptop lid would: no shutdown.
    fn stop(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        // The next daemon replaces a stale socket itself; removing it here
        // only lets `start` wait for the new one.
        fs::remove_file(self.base.join("state/daemon.sock")).unwrap();
    }
    fn file(&self) -> PathBuf {
        self.base.join("work/a.txt")
    }
    fn run(&self, vendor: &str, event: Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "work", "--root"])
            .arg(self.base.join("work"))
            // Whoever runs the suite must not decide whether these sessions
            // look headless.
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
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
        let reply: Option<Value> = serde_json::from_str(&stdout).ok();
        let field = |name: &str| {
            reply
                .as_ref()
                .and_then(|reply| reply["hookSpecificOutput"][name].as_str())
                .map(str::to_owned)
        };
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            reason: field("permissionDecisionReason"),
            context: field("additionalContext"),
        }
    }
    fn claude(&self, session: &str, name: &str, tool: &str, call: &str) -> Run {
        self.run(
            "claude-code",
            json!({
                "session_id": session, "cwd": self.base.join("work"), "hook_event_name": name,
                "tool_name": tool, "tool_use_id": call,
                "tool_input": {"file_path": self.file(), "content": "replacement\n"},
            }),
        )
    }
    /// A Claude Code tool event fired inside subagent `agent` of `session`,
    /// constructed in the documented shape: the parent's `session_id` plus
    /// the subagent's `agent_id` and `agent_type`. No real subagent capture
    /// exists yet.
    fn subagent(
        &self,
        session: &str,
        agent: &str,
        name: &str,
        tool: &str,
        call: &str,
        file: &str,
    ) -> Run {
        self.tool(session, Some(agent), name, tool, call, file)
    }
    fn tool(
        &self,
        session: &str,
        agent: Option<&str>,
        name: &str,
        tool: &str,
        call: &str,
        file: &str,
    ) -> Run {
        let mut event = json!({
            "session_id": session, "cwd": self.base.join("work"), "hook_event_name": name,
            "tool_name": tool, "tool_use_id": call,
            "tool_input": {"file_path": self.base.join("work").join(file), "content": "x\n"},
        });
        if let Some(agent) = agent {
            event["agent_id"] = json!(agent);
            event["agent_type"] = json!("general-purpose");
        }
        self.run("claude-code", event)
    }
    fn read(&self, session: &str) -> Run {
        let call = format!("read-{}", self.calls.fetch_add(1, Ordering::Relaxed));
        self.claude(session, "PostToolUse", "Read", &call)
    }
    /// A complete Codex patch by `session`: admitted, written to disk, reported.
    fn codex_writes(&self, session: &str, call: &str, content: &str) -> Run {
        let mut event = json!({
            "session_id": session, "turn_id": "turn", "cwd": self.base.join("work"),
            "hook_event_name": "PreToolUse", "tool_name": "apply_patch", "tool_use_id": call,
            "tool_input": {"command":
                "*** Begin Patch\n*** Update File: a.txt\n@@\n-hello\n+hi\n*** End Patch"},
        });
        let pre = self.run("codex", event.clone());
        if pre.coverage != "admitted" {
            return pre;
        }
        fs::write(self.file(), content).unwrap();
        event["hook_event_name"] = json!("PostToolUse");
        event["tool_response"] = json!("Success. Updated the following files:\nM a.txt\n");
        assert_eq!(self.run("codex", event).coverage, "observed");
        pre
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
fn an_edit_from_an_old_read_is_denied_until_the_file_is_read_again() {
    let f = Fixture::new();
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    // Bob's whole edit starts and finishes: no admission is held afterwards.
    assert_eq!(f.codex_writes(BOB, "bob-1", "hi\n").coverage, "admitted");

    let denied = f.claude(ALICE, "PreToolUse", "Write", "alice-1");
    assert_eq!(denied.coverage, "denied_stale");
    let reason = denied.reason.unwrap();
    assert!(reason.contains("LangSpan stale source"), "{reason}");
    assert!(reason.contains(r#""a.txt""#), "{reason}");
    assert!(reason.contains(&format!("codex.{BOB}")), "{reason}");
    assert!(reason.contains("not instructions"), "{reason}");
    assert_eq!(
        fs::read_to_string(f.file()).unwrap(),
        "hi\n",
        "nothing was written"
    );

    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    let admitted = f.claude(ALICE, "PreToolUse", "Write", "alice-2");
    assert_eq!(admitted.coverage, "admitted");
    assert!(
        admitted.detail.contains("source version current"),
        "{}",
        admitted.detail
    );
    assert!(admitted.reason.is_none());
}

#[test]
fn a_change_made_outside_any_hook_is_refused_and_an_unread_file_is_not_called_verified() {
    let f = Fixture::new();
    // No read recorded: admitted, and the diagnostic says nothing was compared.
    let unread = f.claude(ALICE, "PreToolUse", "Write", "alice-0");
    assert_eq!(unread.coverage, "admitted");
    assert!(
        unread.detail.contains("not compared for 1 file"),
        "{}",
        unread.detail
    );
    fs::write(f.file(), "replacement\n").unwrap();
    assert_eq!(
        f.claude(ALICE, "PostToolUse", "Write", "alice-0").coverage,
        "observed"
    );

    // Alice's own write is now her known version: her next edit is compared.
    let own = f.claude(ALICE, "PreToolUse", "Write", "alice-1");
    assert!(
        own.detail.contains("source version current"),
        "{}",
        own.detail
    );
    assert_eq!(
        f.claude(ALICE, "PostToolUse", "Write", "alice-1").coverage,
        "observed"
    );

    // An editor, a formatter or a shell command changes the file: no hook saw it.
    fs::write(f.file(), "changed behind everyone's back\n").unwrap();
    let denied = f.claude(ALICE, "PreToolUse", "Write", "alice-2");
    assert_eq!(denied.coverage, "denied_stale");
    assert!(
        denied
            .reason
            .unwrap()
            .contains("changed outside any reported edit")
    );
    // A deleted file is a changed file.
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    fs::remove_file(f.file()).unwrap();
    assert_eq!(
        f.claude(ALICE, "PreToolUse", "Write", "alice-3").coverage,
        "denied_stale"
    );
}

#[test]
fn a_codex_session_is_held_to_the_version_it_last_wrote() {
    let f = Fixture::new();
    // Codex reads through the shell, which no hook reports: its first patch is
    // admitted without comparison. Its own write is then its known version.
    assert_eq!(f.codex_writes(BOB, "bob-1", "hi\n").coverage, "admitted");
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    assert_eq!(
        f.claude(ALICE, "PreToolUse", "Write", "alice-1").coverage,
        "admitted"
    );
    fs::write(f.file(), "replacement\n").unwrap();
    assert_eq!(
        f.claude(ALICE, "PostToolUse", "Write", "alice-1").coverage,
        "observed"
    );

    let denied = f.codex_writes(BOB, "bob-2", "never written\n");
    assert_eq!(denied.coverage, "denied_stale");
    assert!(
        denied
            .reason
            .unwrap()
            .contains(&format!("claude-code.{ALICE}"))
    );
    assert_eq!(fs::read_to_string(f.file()).unwrap(), "replacement\n");
    // Codex has no reported read with which to refresh its record. The
    // refusal told it; the same change is not refused a second time.
    assert_eq!(
        f.codex_writes(BOB, "bob-3", "after the notice\n").coverage,
        "admitted"
    );
}

#[test]
fn a_deleted_file_is_refused_once_and_can_then_be_written_again() {
    let f = Fixture::new();
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    fs::remove_file(f.file()).unwrap();
    let denied = f.claude(ALICE, "PreToolUse", "Write", "alice-1");
    assert_eq!(denied.coverage, "denied_stale");
    assert!(denied.reason.unwrap().contains("reported once"));
    // There is nothing left to read; the next attempt must not be refused.
    assert_eq!(
        f.claude(ALICE, "PreToolUse", "Write", "alice-2").coverage,
        "admitted"
    );
}

#[test]
fn the_next_prompt_names_files_read_before_a_peer_edited_them() {
    let f = Fixture::new();
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    assert_eq!(f.codex_writes(BOB, "bob-1", "hi\n").coverage, "admitted");
    let prompt = f.run(
        "claude-code",
        json!({"session_id": ALICE, "cwd": f.base.join("work"),
               "hook_event_name": "UserPromptSubmit", "prompt": "continue"}),
    );
    let context = prompt.context.unwrap();
    assert!(
        context.contains("Files you read have since been edited by another session"),
        "{context}"
    );
    assert!(
        context.contains(r#""a.txt" (edited by session codex."#),
        "{context}"
    );
}

#[test]
fn a_read_that_cannot_be_versioned_records_nothing_and_decides_nothing() {
    let f = Fixture::new();
    fs::remove_file(f.file()).unwrap();
    std::os::unix::fs::symlink("elsewhere", f.file()).unwrap();
    let run = f.read(ALICE);
    assert!(
        matches!(run.coverage.as_str(), "unsupported" | "unknown"),
        "{}",
        run.coverage
    );
    assert!(run.reason.is_none() && run.context.is_none());
    // A Read before it ran is not a version anyone saw.
    let pre = f.claude(ALICE, "PreToolUse", "Read", "read-pre");
    assert_eq!(pre.coverage, "unsupported");
}

#[test]
fn a_post_tool_event_lost_while_the_daemon_is_down_costs_at_most_one_refusal_naming_no_peer() {
    let mut f = Fixture::new();
    // A peer edits the file first and is current in the new generation too.
    assert_eq!(f.codex_writes(BOB, "bob-1", "hi\n").coverage, "admitted");
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    assert_eq!(
        f.claude(ALICE, "PreToolUse", "Write", "alice-1").coverage,
        "admitted"
    );
    fs::write(f.file(), "replacement\n").unwrap();
    f.stop();
    let lost = f.claude(ALICE, "PostToolUse", "Write", "alice-1");
    assert_eq!(lost.coverage, "unavailable", "{}", lost.detail);
    f.start();
    // Bob is still working in this checkout after the restart, on b.txt.
    let bob = f.run(
        "codex",
        json!({"session_id": BOB, "turn_id": "turn-2", "cwd": f.base.join("work"),
               "hook_event_name": "UserPromptSubmit", "prompt": "carry on"}),
    );
    assert!(bob.reason.is_none());

    let mut refusals = Vec::new();
    for call in ["alice-2", "alice-3", "alice-4"] {
        let run = f.claude(ALICE, "PreToolUse", "Write", call);
        if run.coverage == "denied_stale" {
            // A refused edit never runs, so the host reports nothing after it.
            refusals.push(run.reason.unwrap());
            continue;
        }
        assert_eq!(run.coverage, "admitted", "{}", run.detail);
        fs::write(f.file(), format!("{call}\n")).unwrap();
        assert_eq!(
            f.claude(ALICE, "PostToolUse", "Write", call).coverage,
            "observed"
        );
    }
    assert!(refusals.len() <= 1, "{refusals:?}");
    for reason in &refusals {
        assert!(
            !reason.contains(BOB),
            "the change was Alice's own: {reason}"
        );
        assert!(
            reason.contains("changed outside any reported edit"),
            "{reason}"
        );
    }
}

#[test]
fn two_subagents_keep_their_own_read_records_and_a_stale_sibling_write_is_refused_once() {
    // Constructed events: see `Fixture::subagent`.
    let f = Fixture::new();
    assert_eq!(
        f.subagent(ALICE, "agent-a1", "PostToolUse", "Read", "r1", "a.txt")
            .coverage,
        "read_recorded"
    );
    assert_eq!(
        f.subagent(ALICE, "agent-b2", "PostToolUse", "Read", "r2", "a.txt")
            .coverage,
        "read_recorded"
    );
    assert_eq!(
        f.subagent(ALICE, "agent-b2", "PreToolUse", "Write", "w1", "a.txt")
            .coverage,
        "admitted"
    );
    fs::write(f.file(), "x\n").unwrap();
    assert_eq!(
        f.subagent(ALICE, "agent-b2", "PostToolUse", "Write", "w1", "a.txt")
            .coverage,
        "observed"
    );

    let denied = f.subagent(ALICE, "agent-a1", "PreToolUse", "Write", "w2", "a.txt");
    assert_eq!(denied.coverage, "denied_stale", "{}", denied.detail);
    let reason = denied.reason.unwrap();
    assert!(
        reason.contains(&format!("claude-code.{ALICE}.agent-b2")),
        "{reason}"
    );
    // Told once; the retry is not refused for the same change.
    assert_eq!(
        f.subagent(ALICE, "agent-a1", "PreToolUse", "Write", "w3", "a.txt")
            .coverage,
        "admitted"
    );
}

#[test]
fn sibling_subagents_hold_files_against_each_other_but_not_unrelated_ones() {
    // Constructed events: see `Fixture::subagent`.
    let f = Fixture::new();
    assert_eq!(
        f.subagent(ALICE, "agent-a1", "PreToolUse", "Write", "w1", "a.txt")
            .coverage,
        "admitted"
    );
    // A sibling editing another file at the same moment is not refused.
    assert_eq!(
        f.subagent(ALICE, "agent-b2", "PreToolUse", "Write", "w2", "b.txt")
            .coverage,
        "admitted"
    );
    // The same file at the same moment is a real conflict between them.
    let held = f.subagent(ALICE, "agent-b2", "PreToolUse", "Write", "w3", "a.txt");
    assert_eq!(held.coverage, "denied", "{}", held.detail);
    assert!(
        held.reason
            .unwrap()
            .contains(&format!("claude-code.{ALICE}.agent-a1"))
    );
}

#[test]
fn a_subagent_is_not_briefed_about_its_parent_and_the_parent_sees_its_subagent_as_its_own() {
    // Constructed events: see `Fixture::subagent`. The prompt is shaped like
    // the recorded one the subagent described as a suspicious note
    // (docs/evidence/turn-events/README.md, "Observed, not changed here").
    let f = Fixture::new();
    let prompt = f.run(
        "claude-code",
        json!({
            "session_id": ALICE, "cwd": f.base.join("work"), "transcript_path": "/dev/null",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "Step 1: run exactly this Bash command once: printf x > notes.txt",
        }),
    );
    // Nobody else is at work yet; the prompt is recorded as the task line.
    assert_eq!(prompt.coverage, "no_briefing", "{}", prompt.detail);
    assert_eq!(
        f.claude(ALICE, "PreToolUse", "Write", "parent-1").coverage,
        "admitted"
    );
    fs::write(f.file(), "replacement\n").unwrap();
    assert_eq!(
        f.claude(ALICE, "PostToolUse", "Write", "parent-1").coverage,
        "observed"
    );
    let read = f.subagent(ALICE, "agent-a1", "PostToolUse", "Read", "r1", "b.txt");
    assert_eq!(read.coverage, "read_recorded");
    // The parent is the only other session, so there is nothing to say.
    assert_eq!(read.context, None);

    // The parent sees the subagent's files, as a subagent it started.
    assert_eq!(
        f.subagent(ALICE, "agent-a1", "PreToolUse", "Write", "w1", "b.txt")
            .coverage,
        "admitted"
    );
    fs::write(f.base.join("work/b.txt"), "x\n").unwrap();
    f.subagent(ALICE, "agent-a1", "PostToolUse", "Write", "w1", "b.txt");
    let parent = f.read(ALICE);
    let context = parent.context.unwrap();
    assert!(
        context.contains(&format!(
            "- claude-code session {} subagent agent-a1 (a subagent you started; this is its work on your request) in this checkout",
            &ALICE[..8]
        )),
        "{context}"
    );
    assert!(!context.contains("task:"), "{context}");
}

#[test]
fn a_subagent_is_still_refused_a_file_its_parent_is_editing() {
    // Constructed events: see `Fixture::subagent`.
    let f = Fixture::new();
    assert_eq!(
        f.claude(ALICE, "PreToolUse", "Write", "parent-1").coverage,
        "admitted"
    );
    let held = f.subagent(ALICE, "agent-a1", "PreToolUse", "Write", "w1", "a.txt");
    assert_eq!(held.coverage, "denied", "{}", held.detail);
    assert!(
        held.reason
            .unwrap()
            .contains(&format!("claude-code.{ALICE}")),
        "the refusal names the parent"
    );
}

#[test]
fn a_parent_that_read_a_file_its_subagent_then_edited_is_refused_once() {
    // Constructed events: see `Fixture::subagent`.
    let f = Fixture::new();
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    assert_eq!(
        f.subagent(ALICE, "agent-a1", "PreToolUse", "Write", "w1", "a.txt")
            .coverage,
        "admitted"
    );
    fs::write(f.file(), "x\n").unwrap();
    assert_eq!(
        f.subagent(ALICE, "agent-a1", "PostToolUse", "Write", "w1", "a.txt")
            .coverage,
        "observed"
    );
    let denied = f.claude(ALICE, "PreToolUse", "Write", "parent-1");
    assert_eq!(denied.coverage, "denied_stale", "{}", denied.detail);
    assert!(
        denied
            .reason
            .unwrap()
            .contains(&format!("claude-code.{ALICE}.agent-a1"))
    );
}

#[test]
fn a_subagent_is_briefed_about_a_sibling_subagents_work() {
    // Constructed events: see `Fixture::subagent`.
    let f = Fixture::new();
    assert_eq!(
        f.subagent(ALICE, "agent-a1", "PreToolUse", "Write", "w1", "a.txt")
            .coverage,
        "admitted"
    );
    fs::write(f.file(), "x\n").unwrap();
    f.subagent(ALICE, "agent-a1", "PostToolUse", "Write", "w1", "a.txt");
    let read = f.subagent(ALICE, "agent-b2", "PostToolUse", "Read", "r1", "b.txt");
    let context = read.context.unwrap();
    assert!(
        context.contains(&format!(
            "- claude-code session {} subagent agent-a1 (another subagent started by the session that started you, working on its own part) in this checkout",
            &ALICE[..8]
        )),
        "{context}"
    );
}

/// Whether `dir` treats two spellings of one name as one file, asked of the
/// filesystem itself.
fn same_file(dir: &std::path::Path, made: &str, asked: &str) -> bool {
    let probe = dir.join(made);
    fs::write(&probe, "").unwrap();
    let same = dir.join(asked).exists();
    fs::remove_file(probe).unwrap();
    same
}

#[test]
fn one_existing_file_under_two_spellings_is_one_file_key() {
    let f = Fixture::new();
    let work = f.base.join("work");
    // NFC "café.txt" and its NFD spelling (e + combining acute accent).
    let (nfc, nfd) = ("caf\u{e9}.txt", "cafe\u{301}.txt");
    if !same_file(&work, "Probe.txt", "pROBE.TXT") || !same_file(&work, nfc, nfd) {
        eprintln!(
            "skipped: this filesystem is case- or normalisation-sensitive, so two spellings are two files"
        );
        return;
    }
    // Case: Alice holds the file under one spelling, Bob asks under another.
    assert_eq!(
        f.tool(ALICE, None, "PreToolUse", "Write", "c1", "A.TXT")
            .coverage,
        "admitted"
    );
    let held = f.claude(BOB, "PreToolUse", "Write", "c2");
    assert_eq!(held.coverage, "denied", "{}", held.detail);
    assert!(held.reason.unwrap().contains(r#"["a.txt"]"#));

    // Unicode normalisation: the key is the name as stored on disk.
    fs::write(work.join(nfc), "hello\n").unwrap();
    assert_eq!(
        f.tool(ALICE, None, "PreToolUse", "Write", "n1", nfd)
            .coverage,
        "admitted"
    );
    let held = f.tool(BOB, None, "PreToolUse", "Write", "n2", nfc);
    assert_eq!(held.coverage, "denied", "{}", held.detail);

    // Documented limit: a file that does not exist yet has no stored name
    // to resolve against, so its key is the name as spelled.
    assert_eq!(
        f.tool(ALICE, None, "PreToolUse", "Write", "new1", "New.txt")
            .coverage,
        "admitted"
    );
    assert_eq!(
        f.tool(BOB, None, "PreToolUse", "Write", "new2", "new.txt")
            .coverage,
        "admitted"
    );
}

/// LangSpan's stale-source refusal: a file is refused once per change
/// made since the session's version of it, and the next write goes through
/// whatever it is; a new change refuses once again; each session is its own.
/// A file another session is editing at that moment is different: it is a
/// held admission, refused for as long as that edit is in progress (ADR 0002),
/// which this case does not cover.
#[test]
fn a_file_is_refused_once_per_teammate_change_and_the_next_write_goes_through() {
    const CAROL: &str = "00000000-0000-4000-8000-00000000000c";
    let f = Fixture::new();
    let writes = |session: &str, call: &str, content: &str| {
        let pre = f.claude(session, "PreToolUse", "Write", call);
        if pre.coverage == "admitted" {
            fs::write(f.file(), content).unwrap();
            assert_eq!(
                f.claude(session, "PostToolUse", "Write", call).coverage,
                "observed"
            );
        }
        pre.coverage
    };
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    assert_eq!(f.read(CAROL).coverage, "read_recorded");
    assert_eq!(f.codex_writes(BOB, "bob-1", "one\n").coverage, "admitted");
    assert_eq!(
        writes(ALICE, "alice-1", "a1\n"),
        "denied_stale",
        "the first time"
    );
    assert_eq!(
        writes(ALICE, "alice-2", "a2\n"),
        "admitted",
        "the retry goes through"
    );
    assert_eq!(
        writes(ALICE, "alice-3", "a3\n"),
        "admitted",
        "the same change never refuses again"
    );
    // Bob's next write is refused once too: Alice changed the file since his
    // own version of it. Then his change goes through, and it refuses Alice
    // once.
    assert_eq!(
        f.codex_writes(BOB, "bob-2", "two\n").coverage,
        "denied_stale"
    );
    assert_eq!(f.codex_writes(BOB, "bob-3", "two\n").coverage, "admitted");
    assert_eq!(writes(ALICE, "alice-4", "a4\n"), "denied_stale");
    assert_eq!(
        writes(ALICE, "alice-5", "a5\n"),
        "admitted",
        "the next write passes"
    );
    // Carol read before all of it: she is refused once, on her own record.
    assert_eq!(writes(CAROL, "carol-1", "c1\n"), "denied_stale");
    assert_eq!(writes(CAROL, "carol-2", "c2\n"), "admitted");
}
