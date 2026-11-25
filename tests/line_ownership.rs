#![cfg(unix)]
//! Lines another session wrote recently, protected from a session that has
//! not read the file (ADR 0028), through the real hook executables and a real
//! daemon, with the file really changing on disk between each pre-tool and
//! post-tool event.

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
const CAROL: &str = "00000000-0000-4000-8000-00000000000c";
const DAVE: &str = "00000000-0000-4000-8000-00000000000d";
const BOB: &str = "01a21050-2d3e-7243-a3ca-bf8517eca23c";
const ERIN: &str = "01a21050-2d3e-7243-a3ca-bf8517eca23e";
const FILE: &str = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\n";

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
    calls: AtomicU64,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    reason: Option<String>,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-lines-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(base.join("work")).unwrap();
        fs::write(base.join("work/a.txt"), FILE).unwrap();
        let child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !base.join("state/daemon.sock").exists() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            base,
            child: Some(child),
            calls: AtomicU64::new(0),
        }
    }
    fn path(&self) -> PathBuf {
        self.base.join("work/a.txt")
    }
    fn content(&self) -> String {
        fs::read_to_string(self.path()).unwrap()
    }
    fn call(&self) -> String {
        format!("call-{}", self.calls.fetch_add(1, Ordering::Relaxed))
    }
    fn run(&self, vendor: &str, event: &Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "work", "--root"])
            .arg(self.base.join("work"))
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
        let reply: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            reason: reply.and_then(|reply| {
                reply["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .map(str::to_owned)
            }),
        }
    }
    /// A complete Claude Code tool call: its pre-tool decision, and when it
    /// is admitted, the host's write and the post-tool report.
    fn claude(
        &self,
        session: &str,
        agent: Option<&str>,
        tool: &str,
        input: Value,
        after: impl FnOnce(&str) -> String,
    ) -> Run {
        let mut event = json!({
            "session_id": session, "cwd": self.base.join("work"),
            "hook_event_name": "PreToolUse", "tool_name": tool,
            "tool_use_id": self.call(), "tool_input": input,
        });
        if let Some(agent) = agent {
            event["agent_id"] = json!(agent);
            event["agent_type"] = json!("general-purpose");
        }
        let pre = self.run("claude-code", &event);
        if pre.coverage != "admitted" {
            return pre;
        }
        fs::write(self.path(), after(&self.content())).unwrap();
        event["hook_event_name"] = json!("PostToolUse");
        let post = self.run("claude-code", &event);
        assert_eq!(post.coverage, "observed", "{post:?}");
        pre
    }
    fn edit(&self, session: &str, old: &str, new: &str) -> Run {
        self.edit_as(session, None, old, new)
    }
    fn edit_as(&self, session: &str, agent: Option<&str>, old: &str, new: &str) -> Run {
        let input = json!({"file_path": self.path(), "old_string": old, "new_string": new});
        self.claude(session, agent, "Edit", input, |text| {
            text.replacen(old, new, 1)
        })
    }
    fn write(&self, session: &str, content: &str) -> Run {
        let input = json!({"file_path": self.path(), "content": content});
        self.claude(session, None, "Write", input, |_| content.to_owned())
    }
    fn read(&self, session: &str) -> Run {
        self.run(
            "claude-code",
            &json!({
                "session_id": session, "cwd": self.base.join("work"),
                "hook_event_name": "PostToolUse", "tool_name": "Read",
                "tool_use_id": self.call(), "tool_input": {"file_path": self.path()},
            }),
        )
    }
    /// A complete Codex `apply_patch` of one hunk: `old` lines replaced by
    /// `new` lines, with the context the host would send.
    fn patch(&self, session: &str, hunk: &str, old: &str, new: &str) -> Run {
        let mut event = json!({
            "session_id": session, "turn_id": "turn", "cwd": self.base.join("work"),
            "hook_event_name": "PreToolUse", "tool_name": "apply_patch",
            "tool_use_id": format!("exec-{}", self.call()),
            "tool_input": {"command": format!(
                "*** Begin Patch\n*** Update File: a.txt\n@@\n{hunk}\n*** End Patch"
            )},
        });
        let pre = self.run("codex", &event);
        if pre.coverage != "admitted" {
            return pre;
        }
        let text = self.content();
        assert!(text.contains(old), "{text:?} lacks {old:?}");
        fs::write(self.path(), text.replacen(old, new, 1)).unwrap();
        event["hook_event_name"] = json!("PostToolUse");
        event["tool_response"] = json!("Success. Updated the following files:\nM a.txt\n");
        assert_eq!(self.run("codex", &event).coverage, "observed");
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

fn claude_session(id: &str) -> String {
    format!("claude-code.{id}")
}

/// Benchmark row 2c: Alice (Claude Code) wrote line 3 seconds ago; Bob
/// (Codex, no reported read) patches that line.
#[test]
fn a_codex_patch_over_a_line_another_session_just_wrote_is_refused_once_naming_the_writer() {
    let f = Fixture::new();
    assert_eq!(f.read(ALICE).coverage, "read_recorded");
    assert_eq!(f.edit(ALICE, "line 3", "alice 3").coverage, "admitted");

    let hunk = " line 2\n-alice 3\n+bob 3\n line 4";
    let refused = f.patch(BOB, hunk, "alice 3", "bob 3");
    assert_eq!(refused.coverage, "denied_unread", "{refused:?}");
    let reason = refused.reason.unwrap();
    assert!(reason.contains(&claude_session(ALICE)), "{reason}");
    assert!(reason.contains("\"a.txt\": line 3,"), "{reason}");
    assert!(reason.contains("Read the file"), "{reason}");
    assert!(f.content().contains("alice 3"), "Alice's line is kept");

    // The agent has been told and, Codex reporting no reads, its retry is
    // what shows it went to look: it is admitted.
    let retried = f.patch(BOB, hunk, "alice 3", "bob 3");
    assert_eq!(retried.coverage, "admitted", "{retried:?}");
    assert!(f.content().contains("bob 3"));
}

/// The same case from Claude Code: after the refusal the agent reads the
/// file, and the edit goes through.
#[test]
fn after_a_refusal_for_unread_lines_a_read_lets_the_edit_through() {
    let f = Fixture::new();
    assert_eq!(f.edit(ALICE, "line 3", "alice 3").coverage, "admitted");
    let refused = f.edit(CAROL, "alice 3", "carol 3");
    assert_eq!(refused.coverage, "denied_unread", "{refused:?}");
    assert!(
        refused.reason.unwrap().contains(&claude_session(ALICE)),
        "the writer is named"
    );
    assert_eq!(f.read(CAROL).coverage, "read_recorded");
    assert_eq!(f.edit(CAROL, "alice 3", "carol 3").coverage, "admitted");
}

#[test]
fn a_session_that_read_the_current_version_is_not_refused_for_unread_lines() {
    let f = Fixture::new();
    assert_eq!(f.edit(ALICE, "line 3", "alice 3").coverage, "admitted");
    assert_eq!(f.read(CAROL).coverage, "read_recorded");
    assert_eq!(f.edit(CAROL, "alice 3", "carol 3").coverage, "admitted");
}

#[test]
fn a_write_to_other_lines_of_the_same_file_is_admitted() {
    let f = Fixture::new();
    assert_eq!(f.edit(ALICE, "line 3", "alice 3").coverage, "admitted");
    // A patch of line 5, with line 4 as context: Alice's line is untouched.
    let other = f.patch(BOB, " line 4\n-line 5\n+bob 5", "line 5", "bob 5");
    assert_eq!(other.coverage, "admitted", "{other:?}");
    // Another session that has not read it either adds a line right below
    // hers: that replaces none of her lines.
    let below = f.patch(
        ERIN,
        " alice 3\n+erin 3.5\n line 4",
        "alice 3\n",
        "alice 3\nerin 3.5\n",
    );
    assert_eq!(below.coverage, "admitted", "{below:?}");
}

#[test]
fn written_lines_move_down_with_an_insertion_above_them() {
    let f = Fixture::new();
    assert_eq!(f.edit(ALICE, "line 3", "alice 3").coverage, "admitted");
    // Dave read the file and adds two lines at the top: Alice's line is now 5.
    assert_eq!(f.read(DAVE).coverage, "read_recorded");
    assert_eq!(
        f.edit(DAVE, "line 1\n", "new a\nnew b\nline 1\n").coverage,
        "admitted"
    );
    assert_eq!(
        f.content(),
        "new a\nnew b\nline 1\nline 2\nalice 3\nline 4\nline 5\nline 6\n"
    );
    // Dave's lines are 1 and 2; line 3 is nobody's, so Bob may replace it.
    let moved_away = f.patch(BOB, " new b\n-line 1\n+bob 1", "line 1", "bob 1");
    assert_eq!(moved_away.coverage, "admitted", "{moved_away:?}");
    // Line 5 is Alice's, numbered in the file as it is now.
    let carol = f.edit(CAROL, "alice 3", "carol 3");
    assert_eq!(carol.coverage, "denied_unread", "{carol:?}");
    let reason = carol.reason.unwrap();
    assert!(reason.contains("\"a.txt\": line 5,"), "{reason}");
    assert!(reason.contains(&claude_session(ALICE)), "{reason}");
    assert!(
        !reason.contains(&claude_session(DAVE)),
        "Dave's lines are not changed: {reason}"
    );
}

#[test]
fn a_whole_file_write_over_fresh_lines_is_refused_once_and_one_keeping_them_is_not() {
    let f = Fixture::new();
    assert_eq!(f.edit(ALICE, "line 3", "alice 3").coverage, "admitted");
    // Keeps Alice's line as she wrote it and changes another: admitted.
    let keeping = f.write(DAVE, &f.content().replace("line 6", "dave 6"));
    assert_eq!(keeping.coverage, "admitted", "{keeping:?}");
    // Replaces it, without having read the file: refused, Alice named.
    let replacing = f.write(CAROL, "carol's file\n");
    assert_eq!(replacing.coverage, "denied_unread", "{replacing:?}");
    let reason = replacing.reason.unwrap();
    assert!(reason.contains(&claude_session(ALICE)), "{reason}");
    assert!(reason.contains("line 3"), "{reason}");
    assert!(
        reason.contains(&claude_session(DAVE)),
        "Dave's line 6 is replaced too: {reason}"
    );
    assert!(f.content().contains("alice 3"));
    assert_eq!(f.write(CAROL, "carol's file\n").coverage, "admitted");
}

/// A Claude Code subagent is a session of its own: its fresh lines are
/// protected from its parent, which has not read the file, and its own
/// next edit of them is not refused.
#[test]
fn a_subagents_lines_are_its_own_sessions() {
    let f = Fixture::new();
    let sub = Some("agent-one");
    assert_eq!(
        f.edit_as(ALICE, sub, "line 3", "sub 3").coverage,
        "admitted"
    );
    assert_eq!(
        f.edit_as(ALICE, sub, "sub 3", "sub 3b").coverage,
        "admitted"
    );
    let parent = f.edit(ALICE, "sub 3b", "parent 3");
    assert_eq!(parent.coverage, "denied_unread", "{parent:?}");
    let reason = parent.reason.unwrap();
    assert!(
        reason.contains(&format!("{}.agent-one", claude_session(ALICE))),
        "the subagent's own session is named: {reason}"
    );
}

/// A host that delivers one PreToolUse twice (same tool-call ID, same
/// input) gets the same answer both times: the retry rule is same request
/// ID and same input, same outcome. The refusal itself records what the
/// session was told, which must not make the second delivery a different
/// request under the same ID.
#[test]
fn a_redelivered_pre_tool_event_after_a_refusal_for_unread_lines_is_refused_again() {
    let f = Fixture::new();
    assert_eq!(f.edit(ALICE, "line 3", "alice 3").coverage, "admitted");
    let event = json!({
        "session_id": CAROL, "cwd": f.base.join("work"),
        "hook_event_name": "PreToolUse", "tool_name": "Edit",
        "tool_use_id": "toolu_redelivered",
        "tool_input": {"file_path": f.path(), "old_string": "alice 3", "new_string": "carol 3"},
    });
    let first = f.run("claude-code", &event);
    assert_eq!(first.coverage, "denied_unread", "{first:?}");
    let again = f.run("claude-code", &event);
    assert_eq!(again.coverage, "denied_unread", "{again:?}");
    assert_eq!(again.reason, first.reason);
    assert!(f.content().contains("alice 3"), "Alice's line is kept");

    // The same for a stale source: Dave read the file, Alice changed it.
    assert_eq!(f.read(DAVE).coverage, "read_recorded");
    assert_eq!(f.edit(ALICE, "line 5", "alice 5").coverage, "admitted");
    let mut stale = event.clone();
    stale["session_id"] = json!(DAVE);
    stale["tool_use_id"] = json!("toolu_redelivered_stale");
    stale["tool_input"] =
        json!({"file_path": f.path(), "old_string": "line 6", "new_string": "dave 6"});
    let first = f.run("claude-code", &stale);
    assert_eq!(first.coverage, "denied_stale", "{first:?}");
    let again = f.run("claude-code", &stale);
    assert_eq!(again.coverage, "denied_stale", "{again:?}");
    assert_eq!(again.reason, first.reason);
}
