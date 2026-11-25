#![cfg(unix)]
//! Shell commands through the real hook executable, a real daemon and a real
//! Git repository, with each command really run between its two hook calls
//! (ADR 0011).

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

#[derive(Debug)]
struct Run {
    coverage: String,
    detail: String,
    reason: Option<String>,
    context: Option<String>,
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-shell-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let mut fixture = Self { base, child: None };
        fixture.repository("work");
        fixture.start_daemon();
        fixture
    }

    /// A Git repository at `BASE/name` with one commit of `a.txt`.
    fn repository(&self, name: &str) -> PathBuf {
        let work = self.base.join(name);
        fs::create_dir(&work).unwrap();
        fs::write(work.join("a.txt"), "a line\n").unwrap();
        git(&work, &["init", "-q"]);
        git(&work, &["add", "a.txt"]);
        git(
            &work,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "-m",
                "start",
            ],
        );
        work
    }

    fn start_daemon(&mut self) {
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(self.base.join("state"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let socket = self.base.join("state/daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Send the daemon a signal, such as `STOP` or `CONT`.
    fn signal_daemon(&self, signal: &str) {
        let pid = self.child.as_ref().unwrap().id().to_string();
        let status = Process::new("kill")
            .args([&format!("-{signal}"), &pid])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn run(&self, vendor: &str, event: Value) -> Run {
        self.run_in("work", vendor, event)
    }

    /// A hook call for the repository `BASE/name`, registered as workspace
    /// `name`, with the same state directory and daemon as every other.
    fn run_in(&self, name: &str, vendor: &str, event: Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", name, "--root"])
            .arg(self.base.join(name))
            // The snapshot runs Git; give it the time a cold start can need.
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

    /// A Claude Code `Bash` hook call.
    fn bash(&self, session: &str, phase: &str, call: &str, command: &str) -> Run {
        self.bash_in("work", session, phase, call, command)
    }

    /// A Claude Code `Bash` hook call in the repository `BASE/name`.
    fn bash_in(&self, name: &str, session: &str, phase: &str, call: &str, command: &str) -> Run {
        self.run_in(
            name,
            "claude-code",
            json!({
                "session_id": session, "cwd": self.base.join(name), "hook_event_name": phase,
                "tool_name": "Bash", "tool_use_id": call,
                "tool_input": {"command": command, "description": "test command"},
                "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
            }),
        )
    }

    /// Run `command` in the workspace, as the host would between its hooks.
    fn shell(&self, command: &str) {
        self.shell_in("work", command);
    }

    fn shell_in(&self, name: &str, command: &str) {
        let status = Process::new("/bin/sh")
            .args(["-c", command])
            .current_dir(self.base.join(name))
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{command}");
    }

    /// A whole Bash call: the pre hook, the command if it was not denied, and
    /// the post hook.
    fn bash_call(&self, session: &str, call: &str, command: &str) -> (Run, Option<Run>) {
        let pre = self.bash(session, "PreToolUse", call, command);
        if pre.coverage.starts_with("denied") {
            return (pre, None);
        }
        self.shell(command);
        let post = self.bash(session, "PostToolUse", call, command);
        (pre, Some(post))
    }

    /// A Claude Code `Write` of `file` that is admitted and not finished.
    fn hold(&self, session: &str, call: &str, file: &str) -> Run {
        self.run(
            "claude-code",
            json!({
                "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
                "tool_name": "Write", "tool_use_id": call,
                "tool_input": {"file_path": self.work().join(file), "content": "held\n"},
            }),
        )
    }

    fn prompt(&self, session: &str) -> Option<String> {
        self.run(
            "claude-code",
            json!({"session_id": session, "cwd": self.work(),
                   "hook_event_name": "UserPromptSubmit", "prompt": "continue"}),
        )
        .context
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
fn a_literal_sed_on_a_held_file_is_denied_before_it_runs() {
    let f = Fixture::new();
    assert_eq!(f.hold(BOB, "bob-1", "a.txt").coverage, "admitted");
    let (denied, post) = f.bash_call(ALICE, "alice-1", "sed -i 's/a/b/' a.txt");
    assert_eq!(denied.coverage, "denied", "{denied:?}");
    assert!(post.is_none());
    let reason = denied.reason.unwrap();
    assert!(
        reason.contains("LangSpan coordination conflict"),
        "{reason}"
    );
    assert!(reason.contains(&format!("claude-code.{BOB}")), "{reason}");
    assert!(reason.contains("not instructions"), "{reason}");
    assert_eq!(
        fs::read_to_string(f.work().join("a.txt")).unwrap(),
        "a line\n"
    );

    // An unrelated literal write is not held up.
    let (admitted, _) = f.bash_call(ALICE, "alice-2", "printf x > other.txt");
    assert_eq!(admitted.coverage, "admitted", "{admitted:?}");
}

#[test]
fn a_literal_write_is_admitted_then_attributed_and_peers_are_told() {
    let f = Fixture::new();
    let (pre, post) = f.bash_call(ALICE, "alice-1", "printf x > b.txt");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    assert!(pre.reason.is_none());
    let post = post.unwrap();
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert!(
        post.detail.contains("1 changed file(s) attributed"),
        "{}",
        post.detail
    );
    assert!(post.detail.contains("admission current"), "{}", post.detail);
    assert_eq!(fs::read_to_string(f.work().join("b.txt")).unwrap(), "x");

    // The admission was completed, so a peer is not held off b.txt...
    assert_eq!(f.hold(BOB, "bob-1", "b.txt").coverage, "admitted");
    // ...and is told who changed it.
    let context = f.prompt(BOB).unwrap();
    assert!(context.contains(&ALICE[..8]), "{context}");
    assert!(context.contains(r#"edited: "b.txt" ("#), "{context}");
}

#[test]
fn the_sessions_own_shell_write_is_its_known_version() {
    let f = Fixture::new();
    let read = f.run(
        "claude-code",
        json!({"session_id": ALICE, "cwd": f.work(), "hook_event_name": "PostToolUse",
               "tool_name": "Read", "tool_use_id": "read-1",
               "tool_input": {"file_path": f.work().join("a.txt")}}),
    );
    assert_eq!(read.coverage, "read_recorded");
    // An unparsed command changes the file Alice read.
    let (pre, post) = f.bash_call(ALICE, "alice-1", "for f in a.txt; do printf y >> $f; done");
    assert_eq!(pre.coverage, "shell_observing");
    assert_eq!(post.unwrap().coverage, "shell_observed");
    // Her next edit is compared against what her command wrote, not refused.
    let edit = f.hold(ALICE, "alice-2", "a.txt");
    assert_eq!(edit.coverage, "admitted", "{edit:?}");
    assert!(
        edit.detail.contains("source version current"),
        "{}",
        edit.detail
    );
}

#[test]
fn a_complex_command_gets_no_decision_but_its_real_changes_are_attributed() {
    let f = Fixture::new();
    fs::write(f.work().join("c.txt"), "untracked\n").unwrap();
    // Bob holds a.txt: a command whose writes cannot be known is not denied.
    assert_eq!(f.hold(BOB, "bob-1", "a.txt").coverage, "admitted");
    let command = r#"for f in *.txt; do printf 'more\n' >> "$f"; done"#;
    let (pre, post) = f.bash_call(ALICE, "alice-1", command);
    assert_eq!(pre.coverage, "shell_observing", "{pre:?}");
    assert!(pre.reason.is_none() && pre.context.is_none());
    assert!(pre.detail.contains("write set unknown"), "{}", pre.detail);
    let post = post.unwrap();
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert!(
        post.detail.contains("2 changed file(s) attributed"),
        "{}",
        post.detail
    );
    let context = f.prompt(BOB).unwrap();
    assert!(context.contains(r#""a.txt""#), "{context}");
    assert!(context.contains(r#""c.txt""#), "{context}");
}

#[test]
fn a_clean_tracked_file_deleted_or_moved_is_seen() {
    let f = Fixture::new();
    let (pre, post) = f.bash_call(ALICE, "alice-1", "mv a.txt renamed.txt");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    let post = post.unwrap();
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert!(
        post.detail.contains("2 changed file(s) attributed"),
        "{}",
        post.detail
    );
}

#[test]
fn a_command_that_changes_nothing_records_nothing() {
    let f = Fixture::new();
    let (pre, post) = f.bash_call(ALICE, "alice-1", "cat a.txt");
    assert_eq!(pre.coverage, "shell_observing", "{pre:?}");
    assert_eq!(post.unwrap().coverage, "shell_unchanged");
    // A command run outside any hook, after Alice's, is not hers.
    f.shell("printf z > later.txt");
    assert!(
        f.prompt(BOB)
            .is_none_or(|context| !context.contains("later.txt")),
    );
}

#[test]
fn an_incomplete_snapshot_records_nothing_and_decides_nothing() {
    let f = Fixture::new();
    // A changed file larger than a version can be: the snapshot is incomplete.
    fs::write(f.work().join("big.bin"), vec![b'x'; 5 * 1024 * 1024]).unwrap();
    assert_eq!(f.hold(BOB, "bob-1", "a.txt").coverage, "admitted");
    let (pre, post) = f.bash_call(ALICE, "alice-1", "sed -i 's/a/b/' a.txt; printf x > b.txt");
    assert_eq!(pre.coverage, "shell_unobserved", "{pre:?}");
    assert!(
        pre.reason.is_none(),
        "never denied on an incomplete snapshot"
    );
    assert!(pre.detail.contains("cannot be versioned"), "{}", pre.detail);
    let post = post.unwrap();
    assert_eq!(post.coverage, "shell_unobserved", "{post:?}");
    assert!(post.detail.contains("nothing recorded"), "{}", post.detail);
    let context = f.prompt(BOB).unwrap_or_default();
    assert!(!context.contains("b.txt"), "{context}");
}

#[test]
fn a_command_that_moves_head_is_not_attributed() {
    let f = Fixture::new();
    let command = "printf x > b.txt && git add b.txt && git -c user.name=T -c user.email=t@example.invalid commit -q -m b";
    let (pre, post) = f.bash_call(ALICE, "alice-1", command);
    assert_eq!(pre.coverage, "shell_observing", "{pre:?}");
    let post = post.unwrap();
    assert_eq!(post.coverage, "shell_unobserved", "{post:?}");
    assert!(post.detail.contains("HEAD moved"), "{}", post.detail);
}

#[test]
fn nothing_is_denied_while_a_merge_is_in_progress() {
    let f = Fixture::new();
    assert_eq!(f.hold(BOB, "bob-1", "a.txt").coverage, "admitted");
    fs::write(f.work().join(".git/MERGE_HEAD"), "0".repeat(40)).unwrap();
    let pre = f.bash(ALICE, "PreToolUse", "alice-1", "sed -i 's/a/b/' a.txt");
    assert_eq!(pre.coverage, "shell_observing", "{pre:?}");
    assert!(pre.reason.is_none());
    assert!(pre.detail.contains("merge, rebase"), "{}", pre.detail);
}

#[test]
fn overlapping_runs_of_two_sessions_attribute_a_shared_change_to_neither() {
    let f = Fixture::new();
    let command = "for f in x.txt; do printf 1 > $f; done";
    assert_eq!(
        f.bash(ALICE, "PreToolUse", "alice-1", command).coverage,
        "shell_observing"
    );
    assert_eq!(
        f.bash(BOB, "PreToolUse", "bob-1", "for f in y.txt; do true; done")
            .coverage,
        "shell_observing"
    );
    f.shell(command);
    // Bob's run is still going and could have written x.txt.
    let alice = f.bash(ALICE, "PostToolUse", "alice-1", command);
    assert_eq!(alice.coverage, "contested", "{alice:?}");
    assert!(
        alice.detail.contains("0 changed file(s) attributed"),
        "{}",
        alice.detail
    );
    // Bob sees the same change, which Alice's finished run also saw.
    let bob = f.bash(BOB, "PostToolUse", "bob-1", "for f in y.txt; do true; done");
    assert_eq!(bob.coverage, "contested", "{bob:?}");
    let context = f.prompt(BOB).unwrap_or_default();
    assert!(!context.contains("x.txt"), "{context}");

    // A later run of Alice's alone is hers again.
    let (_, post) = f.bash_call(ALICE, "alice-2", "for f in z.txt; do printf 2 > $f; done");
    assert_eq!(post.unwrap().coverage, "shell_observed");
}

#[test]
fn a_codex_shell_call_is_admitted_or_denied_and_observed() {
    let f = Fixture::new();
    let event = |phase: &str, call: &str, command: &str| {
        json!({
            "session_id": BOB, "turn_id": "turn", "cwd": f.work(),
            "hook_event_name": phase, "model": "example", "permission_mode": "default",
            "tool_name": "Bash", "tool_use_id": call,
            "tool_input": {"command": command},
            "tool_response": "",
        })
    };
    assert_eq!(f.hold(ALICE, "alice-1", "a.txt").coverage, "admitted");
    let denied = f.run("codex", event("PreToolUse", "bob-1", "printf x >> a.txt"));
    assert_eq!(denied.coverage, "denied", "{denied:?}");
    assert!(
        denied
            .reason
            .unwrap()
            .contains(&format!("claude-code.{ALICE}"))
    );

    let pre = f.run("codex", event("PreToolUse", "bob-2", "printf x > b.txt"));
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    f.shell("printf x > b.txt");
    let post = f.run("codex", event("PostToolUse", "bob-2", "printf x > b.txt"));
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    let context = f.prompt(ALICE).unwrap();
    assert!(context.contains(r#"edited: "b.txt" ("#), "{context}");
}

#[test]
fn a_post_without_a_snapshot_or_outside_git_records_nothing() {
    let f = Fixture::new();
    let post = f.bash(ALICE, "PostToolUse", "never-pre", "printf x > b.txt");
    assert_eq!(post.coverage, "shell_unobserved");
    fs::remove_dir_all(f.work().join(".git")).unwrap();
    let pre = f.bash(ALICE, "PreToolUse", "alice-1", "printf x > b.txt");
    assert_eq!(pre.coverage, "shell_unobserved", "{pre:?}");
    assert!(
        pre.detail.contains("not a Git working tree"),
        "{}",
        pre.detail
    );
}

#[test]
fn shell_runs_in_another_repository_sharing_the_state_dir_do_not_contest() {
    let f = Fixture::new();
    f.repository("other");
    let unknown = "for f in y.txt; do true; done";
    let alice = "printf x > a.txt";
    assert_eq!(
        f.bash(ALICE, "PreToolUse", "alice-1", alice).coverage,
        "admitted"
    );
    // In the other repository, one run is still going and another, which
    // overlapped Alice's, changed a file of the same name.
    assert_eq!(
        f.bash_in("other", BOB, "PreToolUse", "bob-1", unknown)
            .coverage,
        "shell_observing"
    );
    let theirs = "printf y > a.txt";
    assert_eq!(
        f.bash_in("other", BOB, "PreToolUse", "bob-2", theirs)
            .coverage,
        "admitted"
    );
    f.shell_in("other", theirs);
    let bob = f.bash_in("other", BOB, "PostToolUse", "bob-2", theirs);
    assert_eq!(bob.coverage, "shell_observed", "{bob:?}");
    f.shell(alice);
    let post = f.bash(ALICE, "PostToolUse", "alice-1", alice);
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert!(
        post.detail.contains("1 changed file(s) attributed"),
        "{}",
        post.detail
    );
    f.bash_in("other", BOB, "PostToolUse", "bob-1", unknown);

    // The same overlap inside one repository still contests a change that
    // Alice's command does not name (one it names is hers, see
    // `concurrent_commands_credit_a_change_to_the_one_that_names_the_file`).
    let unnamed = "for f in a.txt; do printf z > $f; done";
    assert_eq!(
        f.bash(ALICE, "PreToolUse", "alice-2", unnamed).coverage,
        "shell_observing"
    );
    assert_eq!(
        f.bash(CAROL, "PreToolUse", "carol-1", unknown).coverage,
        "shell_observing"
    );
    f.shell(unnamed);
    let post = f.bash(ALICE, "PostToolUse", "alice-2", unnamed);
    assert_eq!(post.coverage, "contested", "{post:?}");
}

#[test]
fn a_post_hook_redelivered_after_the_daemon_was_down_records_the_change_once() {
    let f = Fixture::new();
    let command = "printf x > b.txt";
    assert_eq!(
        f.bash(ALICE, "PreToolUse", "alice-1", command).coverage,
        "admitted"
    );
    f.shell(command);
    // The daemon stops answering (stopped, not restarted, so the admission
    // is still the one made before the command).
    f.signal_daemon("STOP");
    let down = f.bash(ALICE, "PostToolUse", "alice-1", command);
    f.signal_daemon("CONT");
    assert!(
        matches!(down.coverage.as_str(), "timeout" | "unavailable"),
        "{down:?}"
    );
    assert!(down.detail.contains("changes are kept"), "{down:?}");
    let retry = f.bash(ALICE, "PostToolUse", "alice-1", command);
    assert_eq!(retry.coverage, "shell_observed", "{retry:?}");
    assert!(
        retry.detail.contains("1 changed file(s) attributed"),
        "{}",
        retry.detail
    );
    assert!(
        retry.detail.contains("admission current"),
        "{}",
        retry.detail
    );
    // Once recorded, a further delivery records nothing more.
    let again = f.bash(ALICE, "PostToolUse", "alice-1", command);
    assert!(again.detail.contains("already handled"), "{again:?}");
    // The admission was completed, and a peer is told about the change once.
    assert_eq!(f.hold(BOB, "bob-1", "b.txt").coverage, "admitted");
    let context = f.prompt(BOB).unwrap();
    assert_eq!(
        context.matches(r#"edited: "b.txt" ("#).count(),
        1,
        "{context}"
    );
}

/// A Claude Code subagent's `Bash` call is its own session, like its edits:
/// a file its parent holds is held from it too, and the holder is named.
#[test]
fn a_subagents_shell_write_is_its_own_sessions_not_its_parents() {
    let f = Fixture::new();
    assert_eq!(f.hold(ALICE, "alice-1", "a.txt").coverage, "admitted");
    let command = "sed -i 's/a/b/' a.txt";
    let denied = f.run(
        "claude-code",
        json!({
            "session_id": ALICE, "agent_id": "agent-one", "agent_type": "general-purpose",
            "cwd": f.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Bash", "tool_use_id": "sub-1",
            "tool_input": {"command": command},
        }),
    );
    assert_eq!(denied.coverage, "denied", "{denied:?}");
    let reason = denied.reason.unwrap();
    assert!(reason.contains(&format!("claude-code.{ALICE}")), "{reason}");
}

/// The attributed paths of `session` recorded so far, from the event log.
fn credited(f: &Fixture, session: &str) -> Vec<String> {
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(f.base.join("state"))
        .args(["events", "--limit", "1000", "--json"])
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut paths: Vec<String> = value["outcome"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| &event["event"])
        .filter(|event| {
            event["type"] == "operation_observed"
                && event["observation"]["tool"] == "Bash"
                && event["observation"]["session_id"] == format!("claude-code.{session}")
        })
        .flat_map(|event| event["observation"]["paths"].as_array().unwrap().clone())
        .map(|path| path.as_str().unwrap().to_owned())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Two sessions' commands run at once and a file changes: the one whose
/// command names it as a literal write target wrote it, and the other is
/// credited nothing. A command that names no target names nothing.
#[test]
fn concurrent_commands_credit_a_change_to_the_one_that_names_the_file() {
    let f = Fixture::new();
    let unnamed = "for f in nothing; do true; done";
    assert_eq!(
        f.bash(BOB, "PreToolUse", "bob-1", unnamed).coverage,
        "shell_observing"
    );
    // Alice's whole call happens while Bob's is still running.
    let (pre, post) = f.bash_call(ALICE, "alice-1", "printf 1 > named.txt");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    let post = post.unwrap();
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert!(
        post.detail.contains("1 changed file(s) attributed"),
        "{post:?}"
    );
    let bob = f.bash(BOB, "PostToolUse", "bob-1", unnamed);
    assert!(
        bob.detail.contains("0 changed file(s) attributed"),
        "{bob:?}"
    );
    assert_eq!(credited(&f, ALICE), ["named.txt"]);
    assert!(credited(&f, BOB).is_empty());

    // The other way round: Alice's starts first and Bob's whole call runs
    // inside it, before her command writes.
    let named = "printf 2 > second.txt";
    assert_eq!(
        f.bash(ALICE, "PreToolUse", "alice-2", named).coverage,
        "admitted"
    );
    f.bash_call(BOB, "bob-2", unnamed);
    f.shell(named);
    let alice = f.bash(ALICE, "PostToolUse", "alice-2", named);
    assert_eq!(alice.coverage, "shell_observed", "{alice:?}");
    assert_eq!(credited(&f, ALICE), ["named.txt", "second.txt"]);
    assert!(credited(&f, BOB).is_empty());
}

/// Two commands at once and a file neither names: nobody can say whose it is.
#[test]
fn a_change_no_concurrent_command_names_is_credited_to_no_session() {
    let f = Fixture::new();
    let first = "for f in x.txt; do printf 1 > $f; done";
    let second = "for f in y.txt; do true; done";
    f.bash(ALICE, "PreToolUse", "alice-1", first);
    f.bash(BOB, "PreToolUse", "bob-1", second);
    f.shell(first);
    let alice = f.bash(ALICE, "PostToolUse", "alice-1", first);
    let bob = f.bash(BOB, "PostToolUse", "bob-1", second);
    assert_eq!(alice.coverage, "contested", "{alice:?}");
    assert_eq!(bob.coverage, "contested", "{bob:?}");
    assert!(credited(&f, ALICE).is_empty() && credited(&f, BOB).is_empty());
}

/// A command started in the background (a dev server, a watcher) keeps
/// running after its call returns. Until it can no longer be running, what
/// changes and no command names is nobody's; what a command names is still
/// its own.
#[test]
fn a_running_background_command_contests_changes_no_command_names() {
    let f = Fixture::new();
    let background = |phase: &str| {
        f.run(
            "claude-code",
            json!({
                "session_id": ALICE, "cwd": f.work(), "hook_event_name": phase,
                "tool_name": "Bash", "tool_use_id": "alice-dev",
                "tool_input": {"command": "npm run dev", "run_in_background": true},
                "tool_response": {"stdout": "", "stderr": "", "interrupted": false,
                                  "backgroundTaskId": "b1"},
            }),
        )
    };
    assert_eq!(background("PreToolUse").coverage, "shell_observing");
    let started = background("PostToolUse");
    assert_eq!(started.coverage, "shell_unchanged", "{started:?}");
    // Carol's command changes a file it does not name, while the server runs.
    let (_, carol) = f.bash_call(CAROL, "carol-1", "for f in gen.txt; do printf g > $f; done");
    let carol = carol.unwrap();
    assert_eq!(carol.coverage, "contested", "{carol:?}");
    assert!(credited(&f, CAROL).is_empty(), "{:?}", credited(&f, CAROL));
    // Bob's command names its target: that one is his.
    let (_, bob) = f.bash_call(BOB, "bob-1", "printf b > b.txt");
    assert_eq!(bob.unwrap().coverage, "shell_observed");
    assert_eq!(credited(&f, BOB), ["b.txt"]);
}
