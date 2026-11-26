#![cfg(unix)]
//! Commits in a checkout shared by several sessions, through the real hook
//! executable, a real daemon and a real Git repository (ADR 0026): a commit
//! that would carry another live session's uncommitted lines is held once,
//! and `langspan commit --mine` commits only the session's own.

use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    detail: String,
    reason: Option<String>,
}

fn git_command(root: &Path) -> Process {
    let mut command = Process::new("git");
    command
        .arg("-C")
        .arg(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = git_command(root).args(arguments).output().unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Twenty numbered lines, so that edits near the top and near the bottom
/// are separate hunks.
fn numbered() -> String {
    (1..=20).map(|n| format!("line {n}\n")).collect()
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-commit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("a.txt"), "alpha\n").unwrap();
        fs::write(work.join("b.txt"), "beta\n").unwrap();
        fs::write(work.join("shared.txt"), numbered()).unwrap();
        git(&work, &["init", "-q"]);
        // The repository's own identity stands for the owner's.
        git(&work, &["config", "user.name", "Owner"]);
        git(&work, &["config", "user.email", "owner@example.invalid"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "start"]);
        let mut fixture = Self { base, child: None };
        fixture.start_daemon();
        fixture
    }

    fn start_daemon(&mut self) {
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.state())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let socket = self.state().join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn read(&self, file: &str) -> String {
        fs::read_to_string(self.work().join(file)).unwrap()
    }

    fn hook(&self, event: Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "work", "--root"])
            .arg(self.work())
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
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            reason: reply.as_ref().and_then(|reply| {
                reply["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .map(str::to_owned)
            }),
        }
    }

    /// A whole Claude Code `Edit` of `file`: its pre hook, the edit as the
    /// host would make it, and its post hook.
    fn edit(&self, session: &str, call: &str, file: &str, old: &str, new: &str) {
        let path = self.work().join(file);
        let input = json!({"file_path": path, "old_string": old, "new_string": new});
        let pre = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Edit", "tool_use_id": call, "tool_input": input,
        }));
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(old), "{file} lacks {old:?}");
        fs::write(&path, content.replacen(old, new, 1)).unwrap();
        let post = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PostToolUse",
            "tool_name": "Edit", "tool_use_id": call, "tool_input": input,
            "tool_response": {"filePath": path},
        }));
        assert_eq!(post.coverage, "observed", "{post:?}");
    }

    /// A whole Claude Code `Write` of a new or replaced file.
    fn write(&self, session: &str, call: &str, file: &str, content: &str) {
        let path = self.work().join(file);
        let input = json!({"file_path": path, "content": content});
        let pre = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": call, "tool_input": input,
        }));
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
        fs::write(&path, content).unwrap();
        let post = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PostToolUse",
            "tool_name": "Write", "tool_use_id": call, "tool_input": input,
            "tool_response": {"filePath": path},
        }));
        assert_eq!(post.coverage, "observed", "{post:?}");
    }

    fn bash(&self, session: &str, phase: &str, call: &str, command: &str) -> Run {
        self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": phase,
            "tool_name": "Bash", "tool_use_id": call,
            "tool_input": {"command": command, "description": "test command"},
            "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
        }))
    }

    /// A whole Bash call: the pre hook, the command if it was not held, and
    /// the post hook. The command's output is returned when it ran.
    fn bash_call(&self, session: &str, call: &str, command: &str) -> (Run, Option<Output>) {
        let pre = self.bash(session, "PreToolUse", call, command);
        if pre.coverage.starts_with("denied") {
            return (pre, None);
        }
        let output = Process::new("/bin/sh")
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["-c", command])
            .current_dir(self.work())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        self.bash(session, "PostToolUse", call, command);
        (pre, Some(output))
    }

    fn end(&self, session: &str) {
        let run = self.hook(json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": self.work(),
            "hook_event_name": "SessionEnd", "reason": "other",
        }));
        assert_eq!(run.coverage, "session_ended", "{run:?}");
    }

    /// `langspan commit --mine` as a session's Bash call would run it.
    fn commit_mine_command(&self, message: &str) -> String {
        format!(
            "'{}' --state-dir '{}' commit --mine -m '{message}'",
            env!("CARGO_BIN_EXE_langspan"),
            self.state().display()
        )
    }

    /// The `WriteRefused` events the daemon recorded.
    fn refusals(&self) -> Vec<langspan_core::Event> {
        let request = langspan::protocol::Request {
            version: langspan::protocol::PROTOCOL_VERSION,
            request_id: "read".into(),
            command: langspan_core::Command::Events {
                after: 0,
                limit: 1000,
            },
        };
        match langspan::client::call(&self.state(), &request)
            .unwrap()
            .outcome
        {
            langspan_core::Outcome::Events { events } => events
                .into_iter()
                .map(|recorded| recorded.event)
                .filter(|event| matches!(event, langspan_core::Event::WriteRefused { .. }))
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    fn head_files(&self) -> String {
        git(
            &self.work(),
            &["show", "--name-only", "--format=%an <%ae>%n%s", "HEAD"],
        )
    }

    fn status(&self) -> String {
        git(&self.work(), &["status", "--porcelain=v1"])
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
fn a_commit_all_that_would_carry_another_live_sessions_file_is_held_once_naming_it() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");

    let (held, ran) = f.bash_call(ALICE, "alice-2", "git commit -am 'Alice: alpha'");
    assert_eq!(held.coverage, "denied_commit", "{held:?}");
    assert!(ran.is_none());
    let reason = held.reason.unwrap();
    assert!(reason.contains(&format!("claude-code.{BOB}")), "{reason}");
    assert!(reason.contains("b.txt"), "{reason}");
    assert!(!reason.contains("a.txt"), "{reason}");
    assert!(reason.contains("commit --mine"), "{reason}");
    assert!(reason.contains("not instructions"), "{reason}");
    // Nothing was committed or staged.
    assert!(f.head_files().contains("start"));
    assert_eq!(f.status(), " M a.txt\n M b.txt\n");
    // The hold is recorded once, for the status line (roadmap H2).
    let held = f.refusals();
    assert_eq!(held.len(), 1, "{held:?}");
    match &held[0] {
        langspan_core::Event::WriteRefused {
            kind,
            paths,
            peer_session_id,
            ..
        } => {
            assert_eq!(*kind, langspan_core::RefusalKind::CommitHeld);
            assert_eq!(paths, &vec!["b.txt".to_owned()]);
            assert_eq!(peer_session_id, &Some(format!("claude-code.{BOB}")));
        }
        other => panic!("{other:?}"),
    }

    // The same command again is the session's informed choice: not held.
    let (again, ran) = f.bash_call(ALICE, "alice-3", "git commit -am 'Alice: alpha'");
    assert!(!again.coverage.starts_with("denied"), "{again:?}");
    assert!(again.detail.contains("told so once"), "{}", again.detail);
    assert!(ran.unwrap().status.success());
    assert!(f.head_files().contains("b.txt"));
    assert_eq!(
        f.refusals().len(),
        1,
        "a commit let through records nothing"
    );
}

#[test]
fn every_broad_form_of_staging_or_committing_is_held_and_named_paths_of_its_own_are_not() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.write(BOB, "bob-1", "new.txt", "Bob's new file\n");
    f.edit(BOB, "bob-2", "b.txt", "beta", "beta, by Bob");
    for (call, command) in [
        ("c1", "git add -A"),
        ("c2", "git add ."),
        ("c3", "git add --all && git commit -m x"),
        ("c4", "git add -u"),
        ("c5", "git commit -m x -- a.txt b.txt"),
        ("c6", "cd /tmp && cd '{work}' && git commit -a -m x"),
        (
            "c7",
            "git commit -a -m \"$(cat <<'EOF'\nA message (with \"quotes\")\n\nEOF\n)\"",
        ),
    ] {
        let command = command.replace("{work}", &f.work().display().to_string());
        let run = f.bash(ALICE, "PreToolUse", call, &command);
        assert_eq!(run.coverage, "denied_commit", "{command}: {run:?}");
        assert!(run.reason.unwrap().contains("b.txt"), "{command}");
    }
    // Paths of its own, named explicitly, are committed as usual.
    let (named, ran) = f.bash_call(ALICE, "c8", "git commit -m 'Alice' -- a.txt");
    assert!(!named.coverage.starts_with("denied"), "{named:?}");
    assert!(ran.unwrap().status.success());
    assert!(f.head_files().ends_with("a.txt\n"), "{}", f.head_files());
    assert_eq!(f.status(), " M b.txt\n?? new.txt\n");

    // A plain commit is held when the index already holds Bob's change.
    git(&f.work(), &["add", "b.txt"]);
    let run = f.bash(ALICE, "PreToolUse", "c9", "git commit -m 'Alice again'");
    assert_eq!(run.coverage, "denied_commit", "{run:?}");
}

#[test]
fn commit_mine_commits_only_this_sessions_file_and_leaves_the_others_edit_unstaged_and_intact() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.write(ALICE, "alice-2", "alice-new.txt", "Alice's new file\n");
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    f.write(BOB, "bob-2", "bob-new.txt", "Bob's new file\n");
    // Bob staged his edit: it stays staged, and out of Alice's commit.
    git(&f.work(), &["add", "b.txt"]);

    let command = f.commit_mine_command("Alice: alpha");
    let (pre, ran) = f.bash_call(ALICE, "alice-3", &command);
    assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
    let ran = ran.unwrap();
    let stdout = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(stdout.contains("2 file(s)"), "{stdout}");

    let head = f.head_files();
    assert!(
        head.starts_with("Owner <owner@example.invalid>\nAlice: alpha\n"),
        "{head}"
    );
    assert!(
        head.contains("a.txt") && head.contains("alice-new.txt"),
        "{head}"
    );
    assert!(
        !head.contains("b.txt") && !head.contains("bob-new.txt"),
        "{head}"
    );
    assert_eq!(f.status(), "M  b.txt\n?? bob-new.txt\n");
    assert_eq!(f.read("b.txt"), "beta, by Bob\n");
    assert_eq!(f.read("bob-new.txt"), "Bob's new file\n");
    assert_eq!(f.read("a.txt"), "alpha, by Alice\n");
}

#[test]
fn of_two_sessions_hunks_in_one_file_only_this_sessions_are_committed() {
    let f = Fixture::new();
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 2\n",
        "line 2, by Alice\n",
    );
    f.edit(BOB, "bob-1", "shared.txt", "line 18\n", "line 18, by Bob\n");
    let both = f.read("shared.txt");

    let (held, _) = f.bash_call(ALICE, "alice-2", "git commit -am 'Alice'");
    assert_eq!(held.coverage, "denied_commit", "{held:?}");
    let reason = held.reason.unwrap();
    assert!(reason.contains("shared.txt"), "{reason}");
    assert!(reason.contains(&format!("claude-code.{BOB}")), "{reason}");

    let (_, ran) = f.bash_call(ALICE, "alice-3", &f.commit_mine_command("Alice: line 2"));
    let ran = ran.unwrap();
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let committed = git(&f.work(), &["show", "HEAD:shared.txt"]);
    assert_eq!(
        committed,
        numbered().replace("line 2\n", "line 2, by Alice\n")
    );
    // The working file keeps both; only Bob's hunk is left, unstaged.
    assert_eq!(f.read("shared.txt"), both);
    assert_eq!(f.status(), " M shared.txt\n");
    let left = git(&f.work(), &["diff", "-U0"]);
    assert!(left.contains("+line 18, by Bob"), "{left}");
    assert!(!left.contains("Alice"), "{left}");

    // Bob's own commit takes the rest, and nothing of Alice's is held.
    let (_, ran) = f.bash_call(BOB, "bob-2", &f.commit_mine_command("Bob: line 18"));
    assert!(ran.unwrap().status.success());
    assert_eq!(git(&f.work(), &["show", "HEAD:shared.txt"]), both);
    assert_eq!(f.status(), "");
}

#[test]
fn its_later_hunks_are_committed_past_another_sessions_earlier_insertions_and_deletions() {
    let f = Fixture::new();
    f.edit(
        BOB,
        "bob-1",
        "shared.txt",
        "line 2\n",
        "line 2\nBob a\nBob b\n",
    );
    f.edit(BOB, "bob-2", "shared.txt", "line 5\n", "");
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 18\n",
        "line 18, by Alice\n",
    );
    f.edit(
        ALICE,
        "alice-2",
        "shared.txt",
        "line 20\n",
        "line 20\nline 21, no newline",
    );
    let all = f.read("shared.txt");

    let (_, ran) = f.bash_call(ALICE, "alice-3", &f.commit_mine_command("Alice"));
    let ran = ran.unwrap();
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let alice = numbered()
        .replace("line 18\n", "line 18, by Alice\n")
        .replace("line 20\n", "line 20\nline 21, no newline");
    assert_eq!(git(&f.work(), &["show", "HEAD:shared.txt"]), alice);
    assert_eq!(f.read("shared.txt"), all);
    // The shared index holds Alice's commit, not its reversal.
    assert_eq!(git(&f.work(), &["diff", "--cached"]), "");
    let (_, ran) = f.bash_call(BOB, "bob-3", &f.commit_mine_command("Bob"));
    assert!(ran.unwrap().status.success());
    assert_eq!(git(&f.work(), &["show", "HEAD:shared.txt"]), all);
    assert_eq!(f.status(), "");
}

#[test]
fn a_hunk_that_mixes_another_live_sessions_lines_with_its_own_is_held_and_split_by_commit_mine() {
    let f = Fixture::new();
    f.edit(BOB, "bob-1", "shared.txt", "line 10\n", "line 10, by Bob\n");
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 11\n",
        "line 11, by Alice\n",
    );
    let both = f.read("shared.txt");
    // Adjacent lines: one hunk, part Bob's and part Alice's. A plain commit
    // would take Bob's line too: held.
    let run = f.bash(ALICE, "PreToolUse", "alice-2", "git commit -am 'Alice'");
    assert_eq!(run.coverage, "denied_commit", "{run:?}");
    let reason = run.reason.unwrap();
    assert!(reason.contains(&format!("claude-code.{BOB}")), "{reason}");
    assert!(reason.contains("line 10"), "{reason}");
    // commit --mine splits the hunk between them (issue #106).
    let (_, ran) = f.bash_call(ALICE, "alice-3", &f.commit_mine_command("Alice"));
    let ran = ran.unwrap();
    assert!(ran.status.success(), "{}", said(&ran));
    assert_eq!(
        git(&f.work(), &["show", "HEAD:shared.txt"]),
        numbered().replace("line 11\n", "line 11, by Alice\n")
    );
    assert_eq!(f.read("shared.txt"), both);
    assert_eq!(git(&f.work(), &["diff", "--cached"]), "");
    let (_, ran) = f.bash_call(BOB, "bob-2", &f.commit_mine_command("Bob"));
    let ran = ran.unwrap();
    assert!(ran.status.success(), "{}", said(&ran));
    assert_eq!(git(&f.work(), &["show", "HEAD:shared.txt"]), both);
    assert_eq!(f.status(), "");
}

/// Two sessions each add a block to one file with the edit tool, each set
/// off by a blank line and closed by the same line. Those two lines are both
/// sessions' by the journal; at the edge of the other session's hunk they
/// are taken as that session's, so each `commit --mine` commits its own
/// block and refuses nothing (issue #141's acceptance run).
#[test]
fn lines_both_sessions_wrote_go_with_the_hunk_around_them() {
    let f = Fixture::new();
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 2\n",
        "line 2\n\nalice {\n  one\n}\n",
    );
    f.edit(
        BOB,
        "bob-1",
        "shared.txt",
        "line 18\n",
        "line 18\n\nbob {\n  two\n}\n",
    );
    let both = f.read("shared.txt");

    let (_, ran) = f.bash_call(ALICE, "alice-2", &f.commit_mine_command("Alice"));
    let ran = ran.unwrap();
    assert!(ran.status.success(), "{}", said(&ran));
    assert_eq!(
        git(&f.work(), &["show", "HEAD:shared.txt"]),
        numbered().replace("line 2\n", "line 2\n\nalice {\n  one\n}\n")
    );
    assert_eq!(f.read("shared.txt"), both);
    let (_, ran) = f.bash_call(BOB, "bob-2", &f.commit_mine_command("Bob"));
    let ran = ran.unwrap();
    assert!(ran.status.success(), "{}", said(&ran));
    assert_eq!(git(&f.work(), &["show", "HEAD:shared.txt"]), both);
    assert_eq!(f.status(), "");
}

/// A line both sessions wrote is only given to the other session's hunk
/// when this session's own copy is elsewhere in the diff: here Alice's only
/// `}` is the one at the edge of Bob's lines, so her `commit --mine` refuses
/// that hunk rather than commit her block without it.
#[test]
fn a_line_both_wrote_stays_this_sessions_when_its_only_copy_is_by_the_others() {
    let f = Fixture::new();
    f.edit(
        BOB,
        "bob-1",
        "shared.txt",
        "line 18\n",
        "line 18\nbob {\n  two\n",
    );
    f.reread_and_edit(ALICE, "alice-1", "shared.txt", "  two\n", "  two\n}\n");
    // Bob also wrote a `}`, elsewhere and since removed.
    f.reread_and_edit(BOB, "bob-2", "shared.txt", "line 2\n", "line 2\n}\n");
    f.reread_and_edit(BOB, "bob-3", "shared.txt", "line 2\n}\n", "line 2\n");

    let (_, ran) = f.bash_call(ALICE, "alice-2", &f.commit_mine_command("Alice"));
    let ran = ran.unwrap();
    assert!(!ran.status.success(), "{}", said(&ran));
    assert!(said(&ran).contains("shared.txt"), "{}", said(&ran));
    assert_eq!(git(&f.work(), &["log", "--format=%s"]), "start\n");
}

impl Fixture {
    /// Install `hooks` (name, script body) in a hooks directory outside the
    /// checkout, named by `core.hooksPath`.
    fn hooks(&self, hooks: &[(&str, &str)]) {
        let dir = self.base.join("hooks");
        fs::create_dir_all(&dir).unwrap();
        for (name, body) in hooks {
            let path = dir.join(name);
            fs::write(&path, format!("#!/bin/sh\nset -e\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(
            &self.work(),
            &["config", "core.hooksPath", dir.to_str().unwrap()],
        );
    }

    /// `langspan commit --mine` for `session`, from a terminal.
    fn commit_mine(&self, session: &str, message: &str) -> Output {
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.state())
            .args([
                "commit",
                "--mine",
                "--session",
                &format!("claude-code.{session}"),
            ])
            .arg("--root")
            .arg(self.work())
            .args(["-m", message])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    }

    fn subjects(&self) -> String {
        git(&self.work(), &["log", "--format=%s"])
    }
}

/// A pre-commit hook that, the first time it runs, commits on the branch
/// itself: HEAD moves between `commit --mine`'s check and its update.
const MOVE_HEAD_ONCE: &str = r#"mark="$(git rev-parse --git-dir)/moved-once"
[ -e "$mark" ] && exit 0
: > "$mark"
unset GIT_INDEX_FILE
"#;

#[test]
fn a_commit_made_on_the_branch_while_commit_mine_runs_survives_under_it() {
    let f = Fixture::new();
    let other = format!(
        "{MOVE_HEAD_ONCE}echo other > other.txt\ngit add other.txt\ngit commit -q --no-verify -m other"
    );
    f.hooks(&[
        ("pre-commit", other.as_str()),
        ("commit-msg", r#"printf '\nHooked: commit-msg\n' >> "$1""#),
        (
            "post-commit",
            r#"echo post >> "$(git rev-parse --git-dir)/post-commit-ran""#,
        ),
    ]);
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");

    let output = f.commit_mine(ALICE, "Alice");
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The other commit is kept, under this session's.
    assert_eq!(f.subjects(), "Alice\nother\nstart\n");
    assert_eq!(git(&f.work(), &["show", "HEAD:other.txt"]), "other\n");
    assert_eq!(git(&f.work(), &["show", "HEAD:a.txt"]), "alpha, by Alice\n");
    assert_eq!(git(&f.work(), &["show", "HEAD:b.txt"]), "beta\n");
    // The branch moved, not a detached HEAD; the owner's hooks ran.
    assert!(git(&f.work(), &["symbolic-ref", "HEAD"]).starts_with("refs/heads/"));
    let message = git(&f.work(), &["log", "-1", "--format=%B"]);
    assert!(message.contains("Hooked: commit-msg"), "{message}");
    assert!(f.work().join(".git/post-commit-ran").exists());
    assert_eq!(f.status(), " M b.txt\n");
}

#[test]
fn commit_mine_refuses_and_changes_nothing_when_its_hunks_no_longer_apply_on_a_moved_head() {
    let f = Fixture::new();
    let conflicting = format!(
        "{MOVE_HEAD_ONCE}oid=$(printf 'conflict\\n' | git hash-object -w --stdin)\ngit update-index --cacheinfo 100644,$oid,a.txt\ngit commit -q --no-verify -m other"
    );
    f.hooks(&[("pre-commit", conflicting.as_str())]);
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");

    let output = f.commit_mine(ALICE, "Alice");
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("nothing was committed"), "{said}");
    assert!(said.contains("HEAD moved"), "{said}");
    assert_eq!(f.subjects(), "other\nstart\n");
    assert_eq!(git(&f.work(), &["show", "HEAD:a.txt"]), "conflict\n");
    assert_eq!(f.read("a.txt"), "alpha, by Alice\n");
}

#[test]
fn a_failing_pre_commit_hook_stops_commit_mine_and_nothing_is_committed() {
    let f = Fixture::new();
    f.hooks(&[("pre-commit", "echo refused >&2; exit 1")]);
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    let output = f.commit_mine(ALICE, "Alice");
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(said.contains("pre-commit"), "{said}");
    assert_eq!(f.subjects(), "start\n");
    assert_eq!(f.status(), " M a.txt\n");
}

#[test]
fn a_read_only_git_directory_is_named_with_gits_own_words_and_the_fix() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    let git_dir = f.work().join(".git");
    fs::set_permissions(&git_dir, fs::Permissions::from_mode(0o555)).unwrap();
    // Permission bits do not bind a superuser: then there is nothing to show.
    let writable = fs::write(git_dir.join("probe"), "").is_ok();
    let output = (!writable).then(|| f.commit_mine(ALICE, "Alice"));
    fs::set_permissions(&git_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let Some(output) = output else {
        eprintln!("skipped: the Git directory stays writable for this user");
        return;
    };
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("commit --mine said: {said}");
    assert!(!output.status.success(), "{said}");
    // The step, its status, and Git's own message.
    assert!(said.contains("git read-tree"), "{said}");
    assert!(said.contains("exit status: 128"), "{said}");
    assert!(said.contains("Git said: fatal:"), "{said}");
    assert!(said.contains("Permission denied"), "{said}");
    // The cause and the usual fix.
    assert!(said.contains("Git directory is read-only"), "{said}");
    assert!(said.contains("outside the agent's sandbox"), "{said}");
    assert!(said.contains("make the Git directory writable"), "{said}");
    assert!(said.contains("nothing was committed"), "{said}");
    assert_eq!(f.subjects(), "start\n");
    assert_eq!(f.status(), " M a.txt\n");
}

#[test]
fn a_failed_git_read_in_the_commit_guard_says_what_git_said() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    // A pathspec Git refuses: the guard's own read of it fails.
    let run = f.bash(BOB, "PreToolUse", "bob-2", "git add ':(nonsense)b.txt'");
    assert!(run.detail.contains("commit guard: no decision"), "{run:?}");
    assert!(run.detail.contains("exit status: 128"), "{run:?}");
    assert!(run.detail.contains("Git said: fatal:"), "{run:?}");
    assert!(run.detail.contains("nonsense"), "{run:?}");
}

#[test]
fn a_mode_change_no_session_made_is_not_committed_with_the_sessions_hunks() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(
        ALICE,
        "alice-2",
        "shared.txt",
        "line 2\n",
        "line 2, by Alice\n",
    );
    // The owner makes both files executable, outside any session.
    for file in ["a.txt", "shared.txt"] {
        fs::set_permissions(f.work().join(file), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = f.commit_mine(ALICE, "Alice");
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{said}");
    assert_eq!(git(&f.work(), &["show", "HEAD:a.txt"]), "alpha, by Alice\n");
    let modes = git(&f.work(), &["ls-tree", "HEAD", "a.txt", "shared.txt"]);
    assert!(!modes.contains("100755"), "{modes}");
    assert!(said.contains("a.txt (no session's)"), "{said}");
}

#[test]
fn a_session_whose_changes_are_all_its_own_commits_normally() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.write(ALICE, "alice-2", "c.txt", "Alice's\n");
    // Bob is live and has edited nothing of what is uncommitted; the owner's
    // own unreported change belongs to no session.
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    let (_, ran) = f.bash_call(BOB, "bob-2", &f.commit_mine_command("Bob"));
    assert!(ran.unwrap().status.success());
    fs::write(f.work().join("owner.txt"), "the owner's\n").unwrap();

    let (run, ran) = f.bash_call(ALICE, "alice-3", "git add -A && git commit -m 'Alice'");
    assert!(!run.coverage.starts_with("denied"), "{run:?}");
    assert!(ran.unwrap().status.success());
    assert_eq!(f.status(), "");
}

#[test]
fn an_ended_sessions_uncommitted_changes_do_not_hold_a_commit() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    f.end(BOB);
    let (run, ran) = f.bash_call(ALICE, "alice-2", "git commit -am 'Alice and what Bob left'");
    assert!(!run.coverage.starts_with("denied"), "{run:?}");
    assert!(ran.unwrap().status.success());
}

#[test]
fn commit_mine_refuses_a_hunk_whose_lines_it_cannot_attribute_and_changes_nothing() {
    let f = Fixture::new();
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 2\n",
        "line 2, by Alice\n",
    );
    // The owner changes the next line outside any session: one hunk, part
    // Alice's and part nobody's.
    let content = f
        .read("shared.txt")
        .replace("line 3\n", "line 3, by hand\n");
    fs::write(f.work().join("shared.txt"), &content).unwrap();
    let index = fs::read(f.work().join(".git/index")).unwrap();

    let (_, ran) = f.bash_call(ALICE, "alice-2", &f.commit_mine_command("Alice"));
    let ran = ran.unwrap();
    assert!(!ran.status.success());
    let said = String::from_utf8_lossy(&ran.stdout).into_owned();
    assert!(said.contains("shared.txt"), "{said}");
    assert!(said.contains("nothing was committed"), "{said}");
    assert!(f.head_files().contains("start"));
    assert_eq!(fs::read(f.work().join(".git/index")).unwrap(), index);
    assert_eq!(f.read("shared.txt"), content);
}

#[test]
fn commit_mine_with_an_explicit_session_needs_no_hook_and_says_when_nothing_is_its_own() {
    let f = Fixture::new();
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("--state-dir")
        .arg(f.state())
        .args([
            "commit",
            "--mine",
            "--session",
            &format!("claude-code.{ALICE}"),
        ])
        .args(["--root"])
        .arg(f.work())
        .args(["-m", "Alice"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{said}");
    assert!(
        said.contains("Nothing of this session's is uncommitted"),
        "{said}"
    );
    assert_eq!(f.status(), " M b.txt\n");
}

#[tokio::test]
async fn the_mcp_tool_commits_only_the_sessions_own_changes() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(f.state())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let request = CallToolRequestParams::new("langspan_commit_mine").with_arguments(
        json!({
            "request_id": "alice-commit-1", "workspace_id": "work",
            "session_id": format!("claude-code.{ALICE}"), "message": "Alice via MCP",
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let value = result.structured_content.unwrap();
    assert_eq!(value["result"], "committed", "{value}");
    assert_eq!(value["paths"], json!(["a.txt"]), "{value}");
    client.cancel().await.unwrap();
    let head = f.head_files();
    assert!(
        head.contains("Alice via MCP") && head.contains("a.txt"),
        "{head}"
    );
    assert_eq!(f.status(), " M b.txt\n");
}

impl Fixture {
    /// The end of a session's reply.
    fn stop(&self, session: &str) -> Run {
        self.hook(json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": self.work(),
            "hook_event_name": "Stop", "stop_hook_active": false,
        }))
    }
}

/// A `commit --mine` Bash call the host refused after LangSpan's pre-tool
/// hook (its own permission check) gets no post event. The end of that
/// session's reply, or of the session, closes the run, so another session's
/// `commit --mine` is not told two sessions are running it.
#[test]
fn a_refused_commit_mine_call_stops_counting_once_its_reply_or_session_ends() {
    let f = Fixture::new();
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    for (ender, call) in [("Stop", "bob-refused-1"), ("SessionEnd", "bob-refused-2")] {
        let command = f.commit_mine_command("Bob");
        let pre = f.bash(BOB, "PreToolUse", call, &command);
        assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
        let ended = if ender == "Stop" {
            f.stop(BOB)
        } else {
            let run = f.hook(json!({
                "session_id": BOB, "transcript_path": "/dev/null", "cwd": f.work(),
                "hook_event_name": "SessionEnd", "reason": "other",
            }));
            assert_eq!(run.coverage, "session_ended", "{run:?}");
            run
        };
        let file = format!("alice-{ender}.txt");
        f.write(ALICE, &format!("alice-write-{ender}"), &file, "Alice's\n");
        let (_, ran) = f.bash_call(
            ALICE,
            &format!("alice-commit-{ender}"),
            &f.commit_mine_command(ender),
        );
        let ran = ran.unwrap();
        assert!(
            ran.status.success(),
            "{ender}: {}{}",
            String::from_utf8_lossy(&ran.stdout),
            String::from_utf8_lossy(&ran.stderr)
        );
        assert!(f.head_files().contains(&file), "{}", f.head_files());
        assert!(
            ended
                .detail
                .contains("1 shell run(s) that got no result closed"),
            "{ender}: {ended:?}"
        );
    }
}

impl Fixture {
    /// `langspan commit --mine` run in the checkout without --state-dir, as
    /// an agent's shell would run it, with the default state directory
    /// somewhere else.
    fn commit_mine_default_state(&self) -> String {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["commit", "--mine", "-m", "default"])
            .current_dir(self.work())
            .env_remove("LANGSPAN_STATE_DIR")
            .env("XDG_STATE_HOME", self.base.join("default-state"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(!output.status.success());
        String::from_utf8(output.stderr).unwrap()
    }
}

/// From an agent's shell, `commit --mine` without --state-dir looks in the
/// default state directory. When the hooks were installed with another one,
/// the error names both and the command to run, not a missing session.
#[test]
fn commit_mine_in_the_wrong_state_directory_names_it_and_the_installed_one() {
    let f = Fixture::new();
    let default = f.base.join("default-state").join("langspan");
    let said = f.commit_mine_default_state();
    assert!(said.contains(&format!("'{}'", default.display())), "{said}");
    assert!(said.contains("pass the same --state-dir"), "{said}");

    let installed = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["hooks", "install", "claude-code", "--workspace", "work"])
        .arg("--root")
        .arg(f.work())
        .arg("--state-dir")
        .arg(f.state())
        .arg("--apply")
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    let command = f.commit_mine_command("Alice");
    let pre = f.bash(ALICE, "PreToolUse", "alice-2", &command);
    assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
    let said = f.commit_mine_default_state();
    assert!(said.contains(&format!("'{}'", default.display())), "{said}");
    assert!(
        said.contains(&format!(
            "uses the state directory '{}'",
            f.state().display()
        )),
        "{said}"
    );
    assert!(
        said.contains(&format!(
            "langspan --state-dir '{}' commit --mine",
            f.state().display()
        )),
        "{said}"
    );
    assert!(f.head_files().contains("start"));
}

// Issue #99: two sessions editing different parts of one file, in turn.

impl Fixture {
    /// An `Edit` whose pre-tool hook never ran (it timed out, or the hooks
    /// were installed mid-call): the journal records the write with its
    /// lines unknown.
    fn edit_unseen(&self, session: &str, call: &str, file: &str, old: &str, new: &str) {
        let path = self.work().join(file);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(old), "{file} lacks {old:?}");
        fs::write(&path, content.replacen(old, new, 1)).unwrap();
        let input = json!({"file_path": path, "old_string": old, "new_string": new});
        let post = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PostToolUse",
            "tool_name": "Edit", "tool_use_id": call, "tool_input": input,
            "tool_response": {"filePath": path},
        }));
        assert!(post.coverage.starts_with("observed"), "{post:?}");
    }

    /// A `Read` of `file`, then an `Edit` of it: what a session does in
    /// turn with another one editing the same file.
    fn reread_and_edit(&self, session: &str, call: &str, file: &str, old: &str, new: &str) {
        let read = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_use_id": format!("{call}-read"),
            "tool_input": {"file_path": self.work().join(file)},
        }));
        assert!(!read.coverage.starts_with("denied"), "{read:?}");
        self.edit(session, call, file, old, new);
    }
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Forty numbered lines, `long.txt`, committed.
fn long_file(f: &Fixture) -> String {
    let content: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    fs::write(f.work().join("long.txt"), &content).unwrap();
    git(&f.work(), &["add", "long.txt"]);
    git(&f.work(), &["commit", "-q", "-m", "long"]);
    content
}

#[test]
fn sessions_editing_one_file_in_turn_each_commit_their_own_hunks_past_shifted_lines() {
    let f = Fixture::new();
    let start = long_file(&f);
    // In alternation, each inserting lines above the other's earlier edits.
    let edit = |session, call, old, new| f.reread_and_edit(session, call, "long.txt", old, new);
    edit(ALICE, "a1", "line 30\n", "line 30, by Alice\n");
    edit(BOB, "b1", "line 25\n", "line 25, by Bob\n");
    edit(BOB, "b2", "line 5\n", "line 5\nBob 1\nBob 2\nBob 3\n");
    edit(ALICE, "a2", "line 2\n", "line 2\nAlice 1\nAlice 2\n");
    edit(BOB, "b3", "line 1\n", "Bob 0\nline 1\n");
    edit(ALICE, "a3", "line 36\n", "line 36, by Alice\n");
    edit(BOB, "b4", "line 38\n", "");
    let all = f.read("long.txt");

    let output = f.commit_mine(ALICE, "Alice");
    let text = said(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(&format!("long.txt (claude-code.{BOB})")),
        "{text}"
    );
    let alice = start
        .replace("line 30\n", "line 30, by Alice\n")
        .replace("line 2\n", "line 2\nAlice 1\nAlice 2\n")
        .replace("line 36\n", "line 36, by Alice\n");
    assert_eq!(git(&f.work(), &["show", "HEAD:long.txt"]), alice);
    assert_eq!(f.read("long.txt"), all);

    let output = f.commit_mine(BOB, "Bob");
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(git(&f.work(), &["show", "HEAD:long.txt"]), all);
    assert_eq!(f.status(), "");
}

#[test]
fn a_file_is_not_left_as_another_sessions_when_some_hunks_are_this_sessions() {
    let f = Fixture::new();
    let start = long_file(&f);
    f.edit(ALICE, "a1", "long.txt", "line 30\n", "line 30, by Alice\n");
    // Bob's writes whose lines the journal does not know, above Alice's.
    f.edit_unseen(BOB, "b1", "long.txt", "line 5\n", "line 5\nBob 1\nBob 2\n");
    f.reread_and_edit(ALICE, "a2", "long.txt", "line 2\n", "line 2\nAlice 1\n");
    f.edit_unseen(BOB, "b2", "long.txt", "line 20\n", "line 20, by Bob\n");
    let all = f.read("long.txt");

    let output = f.commit_mine(ALICE, "Alice");
    let text = said(&output);
    assert!(output.status.success(), "{text}");
    let alice = start
        .replace("line 30\n", "line 30, by Alice\n")
        .replace("line 2\n", "line 2\nAlice 1\n");
    assert_eq!(git(&f.work(), &["show", "HEAD:long.txt"]), alice);
    assert_eq!(f.read("long.txt"), all);
    // What is left is Bob's, and only Bob's.
    let left = git(&f.work(), &["diff", "-U0"]);
    assert!(!left.contains("Alice"), "{left}");
    assert!(
        text.contains(&format!("long.txt (claude-code.{BOB})")),
        "{text}"
    );
}

#[test]
fn a_hunk_another_sessions_unknown_write_could_share_is_refused_saying_why() {
    let f = Fixture::new();
    long_file(&f);
    f.edit(ALICE, "a1", "long.txt", "line 30\n", "line 30\nsame\n");
    // Bob adds a line with the same text in a write whose lines are not
    // known: which of the two is Alice's cannot be told.
    f.edit_unseen(BOB, "b1", "long.txt", "line 5\n", "line 5\nsame\n");
    let content = f.read("long.txt");
    let index = fs::read(f.work().join(".git/index")).unwrap();

    let output = f.commit_mine(ALICE, "Alice");
    let text = said(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("nothing was committed"), "{text}");
    assert!(text.contains("long.txt (line"), "{text}");
    assert!(text.contains("not known"), "{text}");
    assert!(f.head_files().contains("long"));
    assert_eq!(fs::read(f.work().join(".git/index")).unwrap(), index);
    assert_eq!(f.read("long.txt"), content);
}

#[test]
fn a_file_only_another_session_changed_in_unknown_lines_is_left_as_its() {
    let f = Fixture::new();
    long_file(&f);
    f.edit(ALICE, "a1", "a.txt", "alpha", "alpha, by Alice");
    f.edit_unseen(BOB, "b1", "long.txt", "line 5\n", "line 5, by Bob\n");
    let output = f.commit_mine(ALICE, "Alice");
    let text = said(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(&format!("long.txt (claude-code.{BOB})")),
        "{text}"
    );
    assert_eq!(f.status(), " M long.txt\n");
}

// Review of #99: copies are counted in the whole file's changes since HEAD,
// and only this session's writes not yet committed account for them.

/// Stage `content` as `file` in the shared index, the working tree as it is.
fn stage_version(f: &Fixture, file: &str, content: &str) {
    let staged = f.base.join("staged");
    fs::write(&staged, content).unwrap();
    let staged = staged.to_string_lossy().into_owned();
    let oid = git(&f.work(), &["hash-object", "-w", "--path", file, &staged]);
    let info = format!("100644,{},{file}", oid.trim());
    git(&f.work(), &["update-index", "--cacheinfo", &info]);
}

#[test]
fn a_plain_commit_of_only_another_sessions_staged_copy_of_this_sessions_line_is_held() {
    let f = Fixture::new();
    let start = long_file(&f);
    f.edit(ALICE, "a1", "long.txt", "line 30\n", "line 30\nsame\n");
    // Bob adds a line with the same text, in a write whose lines are not
    // known, and stages only his own change.
    f.edit_unseen(BOB, "b1", "long.txt", "line 5\n", "line 5\nsame\n");
    stage_version(&f, "long.txt", &start.replace("line 5\n", "line 5\nsame\n"));

    let run = f.bash(ALICE, "PreToolUse", "a2", "git commit -m 'Alice'");
    assert_eq!(run.coverage, "denied_commit", "{run:?}");
    let reason = run.reason.unwrap();
    assert!(reason.contains(&format!("claude-code.{BOB}")), "{reason}");
    assert!(reason.contains("long.txt"), "{reason}");
}

#[test]
fn a_line_this_session_committed_through_git_does_not_make_another_sessions_copy_its() {
    let f = Fixture::new();
    long_file(&f);
    f.edit(ALICE, "a1", "long.txt", "line 30\n", "line 30\nsame\n");
    // Committed through ordinary Git: Alice's journal entry stays.
    git(&f.work(), &["commit", "-q", "-am", "Alice, through Git"]);
    f.edit_unseen(BOB, "b1", "long.txt", "line 5\n", "line 5\nsame\n");
    let head = git(&f.work(), &["rev-parse", "HEAD"]);
    let content = f.read("long.txt");

    let output = f.commit_mine(ALICE, "Alice");
    let text = said(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("nothing was committed"), "{text}");
    assert!(text.contains("long.txt (line 6)"), "{text}");
    assert!(text.contains("may already be committed"), "{text}");
    assert_eq!(git(&f.work(), &["rev-parse", "HEAD"]), head);
    assert_eq!(f.read("long.txt"), content);
}

#[test]
fn after_a_commit_through_git_this_sessions_new_hunks_are_still_its_own() {
    let f = Fixture::new();
    let start = long_file(&f);
    f.edit(ALICE, "a1", "long.txt", "line 30\n", "line 30\nAlice 1\n");
    git(&f.work(), &["commit", "-q", "-am", "Alice, through Git"]);
    f.reread_and_edit(ALICE, "a2", "long.txt", "line 10\n", "line 10, by Alice\n");
    f.edit_unseen(BOB, "b1", "long.txt", "line 5\n", "line 5, by Bob\n");

    let output = f.commit_mine(ALICE, "Alice");
    let text = said(&output);
    assert!(output.status.success(), "{text}");
    let alice = start
        .replace("line 30\n", "line 30\nAlice 1\n")
        .replace("line 10\n", "line 10, by Alice\n");
    assert_eq!(git(&f.work(), &["show", "HEAD:long.txt"]), alice);
    let left = git(&f.work(), &["diff", "-U0", "HEAD"]);
    assert!(
        left.contains("+line 5, by Bob") && !left.contains("Alice"),
        "{left}"
    );
}

// ---------------------------------------------------------------------------
// status --mine (issue #40)
// ---------------------------------------------------------------------------

impl Fixture {
    /// `langspan status --mine` as a session's Bash call would run it.
    fn status_mine_command(&self, json: bool) -> String {
        format!(
            "'{}' --state-dir '{}' status --mine{}",
            env!("CARGO_BIN_EXE_langspan"),
            self.state().display(),
            if json { " --json" } else { "" }
        )
    }

    /// `status --mine --json` through a whole Bash call of `session`: its
    /// session is found from the running call, as `commit --mine`'s is.
    fn status_mine(&self, session: &str, call: &str) -> Value {
        let (pre, ran) = self.bash_call(session, call, &self.status_mine_command(true));
        assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
        let ran = ran.unwrap();
        let text = said(&ran);
        assert!(ran.status.success(), "{text}");
        serde_json::from_slice(&ran.stdout).unwrap_or_else(|e| panic!("{e}: {text}"))
    }

    /// Everything the command could change: the index file, `git status`,
    /// and each file's content.
    fn snapshot(&self) -> (Vec<u8>, String, Vec<(String, String)>) {
        let mut files: Vec<(String, String)> = fs::read_dir(self.work())
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_file())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (name.clone(), self.read(&name))
            })
            .collect();
        files.sort();
        (
            fs::read(self.work().join(".git/index")).unwrap(),
            self.status(),
            files,
        )
    }
}

#[test]
fn status_mine_lists_this_sessions_files_and_hunks_and_changes_nothing() {
    let f = Fixture::new();
    let alice = format!("claude-code.{ALICE}");
    let bob = format!("claude-code.{BOB}");
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(
        ALICE,
        "alice-2",
        "shared.txt",
        "line 2\n",
        "line 2, by Alice\n",
    );
    f.edit(BOB, "bob-1", "shared.txt", "line 18\n", "line 18, by Bob\n");
    f.edit(BOB, "bob-2", "b.txt", "beta", "beta, by Bob");
    f.write(ALICE, "alice-3", "alice-new.txt", "one\ntwo\nthree\n");
    // Changes no session made: a line of shared.txt and a new file.
    let shared = f
        .read("shared.txt")
        .replace("line 10\n", "line 10, by nobody\n");
    fs::write(f.work().join("shared.txt"), shared).unwrap();
    fs::write(f.work().join("stray.txt"), "nobody's\n").unwrap();
    // Alice staged her edit of a.txt.
    git(&f.work(), &["add", "a.txt"]);
    let before = f.snapshot();

    let value = f.status_mine(ALICE, "alice-4");
    assert_eq!(f.snapshot(), before, "status --mine changed something");
    assert_eq!(value["session_id"], alice, "{value}");
    assert_eq!(value["truncated"], false, "{value}");
    // b.txt is Bob's, shared.txt holds Bob's lines too.
    assert_eq!(value["others"], 2, "{value}");
    let paths: Vec<&str> = value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["a.txt", "alice-new.txt", "shared.txt"], "{value}");
    let file = |path: &str| {
        value["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == path)
            .unwrap()
            .clone()
    };
    assert_eq!(
        file("a.txt"),
        json!({
            "path": "a.txt", "owner": "mine", "staged": true, "whole": false,
            "hunks": [{"start_line": 1, "line_count": 1, "owner": "mine"}],
        })
    );
    assert_eq!(
        file("alice-new.txt"),
        json!({
            "path": "alice-new.txt", "owner": "mine", "staged": false, "whole": false,
            "hunks": [{"start_line": 1, "line_count": 3, "owner": "mine"}],
        })
    );
    assert_eq!(
        file("shared.txt"),
        json!({
            "path": "shared.txt", "owner": "mixed", "staged": false, "whole": false,
            "hunks": [
                {"start_line": 2, "line_count": 1, "owner": "mine"},
                {"start_line": 10, "line_count": 1, "owner": "none"},
                {"start_line": 18, "line_count": 1, "owner": "others", "sessions": [bob]},
            ],
        })
    );

    // The same, for a person.
    let (_, ran) = f.bash_call(ALICE, "alice-5", &f.status_mine_command(false));
    let text = said(&ran.unwrap());
    assert!(text.contains("shared.txt"), "{text}");
    assert!(
        text.contains(&format!("line 2 mine, line 10 no session's, line 18 {bob}")),
        "{text}"
    );
    assert!(text.contains("a.txt") && text.contains("staged"), "{text}");
    assert!(
        !text.contains("stray.txt") && !text.contains("b.txt"),
        "{text}"
    );
    assert_eq!(f.snapshot(), before, "status --mine changed something");

    // Bob's view is his own.
    let value = f.status_mine(BOB, "bob-3");
    let paths: Vec<&str> = value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["b.txt", "shared.txt"], "{value}");

    // After commit --mine, what it committed is no longer listed.
    let (_, ran) = f.bash_call(ALICE, "alice-6", &f.commit_mine_command("Alice"));
    let ran = ran.unwrap();
    assert!(ran.status.success(), "{}", said(&ran));
    let value = f.status_mine(ALICE, "alice-7");
    assert_eq!(value["files"], json!([]), "{value}");
    assert_eq!(value["others"], 2, "{value}");
    let (_, ran) = f.bash_call(ALICE, "alice-8", &f.status_mine_command(false));
    let text = said(&ran.unwrap());
    assert!(
        text.contains("Nothing of this session's is uncommitted"),
        "{text}"
    );
}

#[test]
fn status_mine_marks_a_file_unknown_where_commit_mine_would_refuse() {
    let f = Fixture::new();
    // Bob replaced a line of Alice's next to another of hers: one hunk that
    // cannot be split between them (issue #106).
    replace_alices_line(&f);
    let value = f.status_mine(ALICE, "alice-2");
    let files = value["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{value}");
    assert_eq!(files[0]["owner"], "unknown", "{value}");
    assert_eq!(files[0]["hunks"][0]["owner"], "unknown", "{value}");
    assert!(files[0]["hunks"][0]["reason"].is_string(), "{value}");
}

/// Alice changes line 2 and adds a line below it; Bob replaces her line 2.
fn replace_alices_line(f: &Fixture) {
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 2\n",
        "line 2, Alice\nAlice\n",
    );
    f.reread_and_edit(
        BOB,
        "bob-1",
        "shared.txt",
        "line 2, Alice\n",
        "line 2, Bob\n",
    );
}

#[test]
fn status_mine_counts_another_sessions_lines_in_an_unknown_hunk() {
    let f = Fixture::new();
    // A hunk of Alice's and Bob's lines that cannot be split: it is not
    // known whose, but Bob's line is in it.
    replace_alices_line(&f);
    let value = f.status_mine(ALICE, "alice-2");
    assert_eq!(value["files"][0]["owner"], "unknown", "{value}");
    assert_eq!(value["others"], 1, "{value}");
    let (_, ran) = f.bash_call(ALICE, "alice-3", &f.status_mine_command(false));
    let text = said(&ran.unwrap());
    assert!(
        text.contains("1 changed file(s) hold other sessions' lines."),
        "{text}"
    );
}

#[test]
fn status_mine_without_a_running_call_names_the_state_directory() {
    let f = Fixture::new();
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("--state-dir")
        .arg(f.state())
        .args(["status", "--mine", "--root"])
        .arg(f.work())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let text = said(&output);
    assert!(!output.status.success(), "{text}");
    assert!(
        text.contains(&format!("'{}'", f.state().display())),
        "{text}"
    );
    assert!(text.contains("--session ID"), "{text}");

    // With --session, it runs from anywhere.
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("--state-dir")
        .arg(f.state())
        .args(["status", "--mine", "--json", "--session"])
        .arg(format!("claude-code.{ALICE}"))
        .arg("--root")
        .arg(f.work())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", said(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["files"][0]["path"], "a.txt", "{value}");
}

#[tokio::test]
async fn the_mcp_tool_lists_the_sessions_own_changes() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    f.edit(BOB, "bob-1", "b.txt", "beta", "beta, by Bob");
    let before = f.snapshot();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command
        .arg("mcp")
        .arg("--state-dir")
        .arg(f.state())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let request = CallToolRequestParams::new("langspan_my_changes").with_arguments(
        json!({
            "request_id": "alice-mine-1", "workspace_id": "work",
            "session_id": format!("claude-code.{ALICE}"),
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let value = result.structured_content.unwrap();
    assert_eq!(value["files"][0]["path"], "a.txt", "{value}");
    assert_eq!(value["files"][0]["owner"], "mine", "{value}");
    assert_eq!(value["files"].as_array().unwrap().len(), 1, "{value}");
    assert_eq!(value["others"], 1, "{value}");
    client.cancel().await.unwrap();
    assert_eq!(f.snapshot(), before);
}

// ---------------------------------------------------------------------------
// A hunk split between sessions (issue #106)
// ---------------------------------------------------------------------------

const LIST: &str = "# Done\n- one\n- two\n- three\n";

/// `list.md`, committed.
fn list_file(f: &Fixture) {
    fs::write(f.work().join("list.md"), LIST).unwrap();
    git(&f.work(), &["add", "list.md"]);
    git(&f.work(), &["commit", "-q", "-m", "list"]);
}

fn entry_of(session: &str) -> String {
    let name = if session == ALICE { "Alice" } else { "Bob" };
    format!("- {name}'s entry\n")
}

/// Each of `sessions` in turn adds its entry directly below the one before:
/// Git's diff joins them into one hunk.
fn add_adjacent_entries(f: &Fixture, sessions: &[&str]) {
    let mut above = "- two\n".to_owned();
    for (n, session) in sessions.iter().enumerate() {
        let entry = entry_of(session);
        f.reread_and_edit(
            session,
            &format!("entry-{n}"),
            "list.md",
            &above,
            &format!("{above}{entry}"),
        );
        above = entry;
    }
}

/// `LIST` with `entries` below "- two".
fn list_with(entries: &[&str]) -> String {
    let added: String = entries.iter().map(|s| entry_of(s)).collect();
    LIST.replace("- two\n", &format!("- two\n{added}"))
}

#[test]
fn two_sessions_adding_adjacent_lines_to_one_list_each_commit_only_their_own_in_either_order() {
    for order in [[ALICE, BOB], [BOB, ALICE]] {
        let f = Fixture::new();
        list_file(&f);
        add_adjacent_entries(&f, &order);
        let all = f.read("list.md");
        assert_eq!(all, list_with(&order));
        let one_hunk = git(&f.work(), &["diff", "-U0"]);
        assert_eq!(one_hunk.matches("@@ -").count(), 1, "{one_hunk}");

        let output = f.commit_mine(ALICE, "Alice");
        let text = said(&output);
        assert!(output.status.success(), "{order:?}: {text}");
        assert!(
            text.contains(&format!("list.md (claude-code.{BOB})")),
            "{text}"
        );
        assert_eq!(
            git(&f.work(), &["show", "HEAD:list.md"]),
            list_with(&[ALICE]),
            "{order:?}"
        );
        // Bob's line stays in the working tree, uncommitted and unstaged.
        assert_eq!(f.read("list.md"), all);
        assert_eq!(git(&f.work(), &["diff", "--cached"]), "");
        let left = git(&f.work(), &["diff", "-U0"]);
        assert!(left.contains("+- Bob's entry"), "{left}");
        assert!(!left.contains("Alice"), "{left}");

        let output = f.commit_mine(BOB, "Bob");
        assert!(output.status.success(), "{order:?}: {}", said(&output));
        assert_eq!(git(&f.work(), &["show", "HEAD:list.md"]), all);
        assert_eq!(f.status(), "");
    }
}

#[test]
fn this_sessions_lines_on_both_sides_of_anothers_are_committed_together() {
    let f = Fixture::new();
    list_file(&f);
    // Alice, Bob, then Alice again below Bob's line: one hunk of three runs.
    add_adjacent_entries(&f, &[ALICE, BOB]);
    f.reread_and_edit(
        ALICE,
        "entry-2",
        "list.md",
        "- Bob's entry\n",
        "- Bob's entry\n- Alice's second\n",
    );
    let all = f.read("list.md");

    let output = f.commit_mine(ALICE, "Alice");
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(
        git(&f.work(), &["show", "HEAD:list.md"]),
        LIST.replace("- two\n", "- two\n- Alice's entry\n- Alice's second\n")
    );
    assert_eq!(f.read("list.md"), all);
    assert_eq!(git(&f.work(), &["diff", "--cached"]), "");

    // status --mine, before Bob commits, shows his line as his alone.
    let status = f.status_mine(BOB, "bob-status");
    let hunks = &status["files"][0]["hunks"];
    assert_eq!(status["files"][0]["owner"], "mine", "{status}");
    assert_eq!(hunks.as_array().unwrap().len(), 1, "{status}");

    let output = f.commit_mine(BOB, "Bob");
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(git(&f.work(), &["show", "HEAD:list.md"]), all);
    assert_eq!(f.status(), "");
}

#[test]
fn status_mine_lists_each_sessions_part_of_a_hunk_as_its_own() {
    let f = Fixture::new();
    list_file(&f);
    add_adjacent_entries(&f, &[BOB, ALICE]);
    let status = f.status_mine(ALICE, "alice-status");
    let file = &status["files"][0];
    assert_eq!(file["owner"], "mixed", "{status}");
    let hunks: Vec<(u64, u64, &str)> = file["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["start_line"].as_u64().unwrap(),
                h["line_count"].as_u64().unwrap(),
                h["owner"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(hunks, [(4, 1, "others"), (5, 1, "mine")], "{status}");
}

#[test]
fn a_line_that_replaced_another_sessions_line_next_to_its_own_is_refused_saying_so() {
    for (first, second) in [(BOB, ALICE), (ALICE, BOB)] {
        let f = Fixture::new();
        // The first session changes line 10; the second replaces that line
        // with its own and changes line 11 too: one hunk, whose removed
        // line 10 is the first's and whose added line is the second's.
        f.edit(
            first,
            "first-1",
            "shared.txt",
            "line 10\n",
            "line 10, first\n",
        );
        f.reread_and_edit(
            second,
            "second-1",
            "shared.txt",
            "line 10, first\nline 11\n",
            "line 10, second\nline 11, second\n",
        );
        let content = f.read("shared.txt");
        let index = fs::read(f.work().join(".git/index")).unwrap();

        for session in [ALICE, BOB] {
            let output = f.commit_mine(session, "mine");
            let text = said(&output);
            assert!(!output.status.success(), "{first} then {second}: {text}");
            assert!(text.contains("nothing was committed"), "{text}");
            assert!(text.contains("shared.txt (lines 10-11)"), "{text}");
            assert!(
                text.contains("replaced another session's"),
                "{first} then {second}, committing {session}: {text}"
            );
        }
        assert!(f.head_files().contains("start"));
        assert_eq!(fs::read(f.work().join(".git/index")).unwrap(), index);
        assert_eq!(f.read("shared.txt"), content);
    }
}

/// The three lines one session changes and the other deletes one of, with
/// lines around them that no session touches.
const ABC: &str = "A\nB\nC\n";

/// Where in `abc.txt` the three lines are: the whole file, its start, its
/// end, and its middle.
fn around_abc() -> [(&'static str, &'static str); 4] {
    [
        ("", ""),
        ("", "keep 1\nkeep 2\n"),
        ("keep 1\nkeep 2\n", ""),
        ("keep 1\n", "keep 2\n"),
    ]
}

#[test]
fn a_replaced_line_that_could_pair_with_either_of_two_removed_runs_is_refused_not_moved() {
    // The first session changes "A B C" to "B X" (it replaced C with X and
    // deleted A); the second then deletes B. Git's one hunk removes A
    // (first's), B (second's), C (first's) and adds X (first's): X could
    // have taken A's place or C's, so its place next to the second
    // session's uncommitted B is not known. Splitting it as if X replaced A
    // committed "X B" for the first session, moving X across B.
    for (first, second) in [(ALICE, BOB), (BOB, ALICE)] {
        for (before, after) in around_abc() {
            let f = Fixture::new();
            let start = format!("{before}{ABC}{after}");
            fs::write(f.work().join("abc.txt"), &start).unwrap();
            git(&f.work(), &["add", "abc.txt"]);
            git(&f.work(), &["commit", "-q", "-m", "abc"]);
            f.reread_and_edit(first, "first-1", "abc.txt", ABC, "B\nX\n");
            f.reread_and_edit(second, "second-1", "abc.txt", "B\nX\n", "X\n");
            let content = f.read("abc.txt");
            assert_eq!(content, format!("{before}X\n{after}"));
            let one_hunk = git(&f.work(), &["diff", "-U0"]);
            assert_eq!(one_hunk.matches("@@ -").count(), 1, "{one_hunk}");
            let index = fs::read(f.work().join(".git/index")).unwrap();
            let head = git(&f.work(), &["rev-parse", "HEAD"]);

            for session in [first, second] {
                let output = f.commit_mine(session, "mine");
                let text = said(&output);
                let case = format!("{first} first, {before:?}/{after:?}, committing {session}");
                assert!(!output.status.success(), "{case}: {text}");
                assert!(text.contains("nothing was committed"), "{case}: {text}");
                assert!(text.contains("more than one order"), "{case}: {text}");
            }
            assert_eq!(git(&f.work(), &["rev-parse", "HEAD"]), head);
            assert_eq!(git(&f.work(), &["show", "HEAD:abc.txt"]), start);
            assert_eq!(fs::read(f.work().join(".git/index")).unwrap(), index);
            assert_eq!(f.read("abc.txt"), content);
        }
    }
}

#[test]
fn a_replaced_line_with_one_removed_run_of_its_session_is_still_split_in_place() {
    // The first session replaces C with X; the second deletes B above it.
    // X can only have taken C's place: the first session's commit is
    // "A B X", B still in it, and the second's then removes B.
    for (first, second) in [(ALICE, BOB), (BOB, ALICE)] {
        for (before, after) in around_abc() {
            let f = Fixture::new();
            let start = format!("{before}{ABC}{after}");
            fs::write(f.work().join("abc.txt"), &start).unwrap();
            git(&f.work(), &["add", "abc.txt"]);
            git(&f.work(), &["commit", "-q", "-m", "abc"]);
            f.reread_and_edit(first, "first-1", "abc.txt", "C\n", "X\n");
            f.reread_and_edit(second, "second-1", "abc.txt", "B\nX\n", "X\n");
            let content = f.read("abc.txt");
            let case = format!("{first} first, {before:?}/{after:?}");

            let output = f.commit_mine(first, "first");
            assert!(output.status.success(), "{case}: {}", said(&output));
            assert_eq!(
                git(&f.work(), &["show", "HEAD:abc.txt"]),
                format!("{before}A\nB\nX\n{after}"),
                "{case}"
            );
            assert_eq!(f.read("abc.txt"), content);
            assert_eq!(git(&f.work(), &["diff", "--cached"]), "");
        }
    }
}

#[test]
fn a_hunk_with_a_line_no_session_wrote_next_to_another_sessions_is_still_refused() {
    let f = Fixture::new();
    list_file(&f);
    add_adjacent_entries(&f, &[ALICE, BOB]);
    // The owner adds a line below Bob's outside any session.
    let content = f
        .read("list.md")
        .replace("- Bob's entry\n", "- Bob's entry\n- by hand\n");
    fs::write(f.work().join("list.md"), &content).unwrap();

    let output = f.commit_mine(ALICE, "Alice");
    let text = said(&output);
    assert!(!output.status.success(), "{text}");
    assert!(
        text.contains("part by another session or by none"),
        "{text}"
    );
    assert!(f.head_files().contains("list"));
    assert_eq!(f.read("list.md"), content);
}

// ---------------------------------------------------------------------------
// Other hosts (issue #26): each host's own edit and shell events, through the
// real hook executable, mark the running `commit --mine` call, so the
// session is found without --session.
// ---------------------------------------------------------------------------

const CODEX: &str = "00000000-0000-4000-8000-000000000001";
const CURSOR: &str = "00000000-0000-4000-8000-0000000000c1";

/// A host's run of the hook: its coverage, its detail and, for a deny, the
/// reason in the host's own field.
#[derive(Debug)]
struct HostRun {
    coverage: String,
    detail: String,
    reason: Option<String>,
}

impl Fixture {
    /// The hook as `vendor`'s configuration runs it, with `event`.
    fn host(&self, vendor: &str, event: &Value) -> HostRun {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "work", "--root"])
            .arg(self.work())
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
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        // Codex reads Claude Code's field, and Cursor `agent_message`.
        let reason = [
            &reply["hookSpecificOutput"]["permissionDecisionReason"],
            &reply["agent_message"],
        ]
        .into_iter()
        .find_map(|field| field.as_str().map(str::to_owned));
        HostRun {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            reason,
        }
    }

    /// Run `command` in the checkout as a host's shell tool would, between
    /// the host's pre and post events, which `event(pre)` builds.
    fn host_shell(&self, vendor: &str, event: impl Fn(bool) -> Value, command: &str) -> Output {
        let pre = self.host(vendor, &event(true));
        assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
        let output = Process::new("/bin/sh")
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["-c", command])
            .current_dir(self.work())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        let post = self.host(vendor, &event(false));
        assert!(!post.coverage.starts_with("denied"), "{post:?}");
        output
    }

    /// `commit --mine --json`, as an agent's shell runs it: no --session.
    fn commit_mine_json(&self, message: &str) -> String {
        format!("{} --json", self.commit_mine_command(message))
    }

    /// A Codex `apply_patch` replacing line `old` of shared.txt with `new`.
    fn codex_patch(&self, call: &str, old: &str, new: &str) {
        let patch = format!(
            "*** Begin Patch\n*** Update File: shared.txt\n@@\n-{old}\n+{new}\n*** End Patch"
        );
        let event = |phase: &str| {
            let mut event = json!({
                "session_id": CODEX, "turn_id": "turn-1", "transcript_path": null,
                "cwd": self.work(), "hook_event_name": phase, "model": "example-model",
                "permission_mode": "default", "tool_name": "apply_patch",
                "tool_use_id": call, "tool_input": {"command": patch},
            });
            if phase == "PostToolUse" {
                event["tool_response"] =
                    json!("Success. Updated the following files:\nM shared.txt\n");
            }
            event
        };
        let pre = self.host("codex", &event("PreToolUse"));
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
        let content = self.read("shared.txt");
        let replaced = content.replacen(&format!("{old}\n"), &format!("{new}\n"), 1);
        assert_ne!(content, replaced, "shared.txt lacks {old:?}");
        fs::write(self.work().join("shared.txt"), replaced).unwrap();
        let post = self.host("codex", &event("PostToolUse"));
        assert_eq!(post.coverage, "observed", "{post:?}");
    }

    /// A Codex shell call (`exec_command`, whose hook events say `Bash`).
    fn codex_shell(&self, call: &str, command: &str) -> Output {
        let event = |pre: bool| {
            json!({
                "session_id": CODEX, "turn_id": "turn-1", "transcript_path": null,
                "cwd": self.work(),
                "hook_event_name": if pre { "PreToolUse" } else { "PostToolUse" },
                "model": "example-model", "permission_mode": "default",
                "tool_name": "Bash", "tool_use_id": call,
                "tool_input": {"command": command},
            })
        };
        self.host_shell("codex", event, command)
    }
}

/// shared.txt with line 18 changed by `by_18`, and with line 2 changed by
/// `by_2` as well.
fn line_2_and_18(by_2: &str, by_18: &str) -> (String, String) {
    let only_18 = numbered().replace("line 18\n", &format!("line 18, by {by_18}\n"));
    let both = only_18.replace("line 2\n", &format!("line 2, by {by_2}\n"));
    (only_18, both)
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Acceptance: a Codex session runs `langspan commit --mine -m "x"` in its
/// shell and commits only its hunks in a file both sessions changed; the
/// other session's hunk is still in the working tree and not staged.
#[test]
fn a_codex_shell_commit_mine_commits_only_its_hunk_of_a_file_both_changed() {
    let f = Fixture::new();
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 2\n",
        "line 2, by Alice\n",
    );
    f.codex_patch("codex-1", "line 18", "line 18, by Codex");
    let (only_codex, both) = line_2_and_18("Alice", "Codex");
    assert_eq!(f.read("shared.txt"), both);

    let ran = f.codex_shell("codex-2", &f.commit_mine_json("x"));
    assert!(ran.status.success(), "{}", said(&ran));
    let report: Value = serde_json::from_str(&stdout_of(&ran)).unwrap();
    assert_eq!(report["result"], "committed", "{report}");
    assert_eq!(report["session_id"], format!("codex.{CODEX}"), "{report}");
    assert_eq!(report["paths"], json!(["shared.txt"]), "{report}");
    assert_eq!(
        report["left"],
        json!([{"path": "shared.txt", "sessions": [format!("claude-code.{ALICE}")]}]),
        "{report}"
    );
    assert_eq!(report["refused"], json!([]), "{report}");
    assert_eq!(report["moved"], false, "{report}");

    assert_eq!(git(&f.work(), &["show", "HEAD:shared.txt"]), only_codex);
    assert_eq!(git(&f.work(), &["log", "-1", "--format=%s"]), "x\n");
    // Alice's hunk: still in the working tree, and not staged.
    assert_eq!(f.read("shared.txt"), both);
    assert_eq!(f.status(), " M shared.txt\n");
    assert_eq!(git(&f.work(), &["diff", "--cached"]), "");
    let left = git(&f.work(), &["diff", "-U0"]);
    assert!(left.contains("+line 2, by Alice"), "{left}");
    assert!(!left.contains("Codex"), "{left}");
}

/// The text output lists what was left and points to `status --mine` for
/// the lines still uncommitted.
#[test]
fn commit_mine_text_lists_what_was_left_and_points_to_status_mine() {
    let f = Fixture::new();
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 2\n",
        "line 2, by Alice\n",
    );
    f.codex_patch("codex-1", "line 18", "line 18, by Codex");
    let ran = f.codex_shell("codex-2", &f.commit_mine_command("x"));
    let stdout = stdout_of(&ran);
    assert!(ran.status.success(), "{}", said(&ran));
    assert!(
        stdout.contains(&format!(
            "Left as they are, uncommitted: shared.txt (claude-code.{ALICE})"
        )),
        "{stdout}"
    );
    assert!(stdout.contains("langspan status --mine"), "{stdout}");
}

impl Fixture {
    /// A Cursor event of `tool` with `input`.
    fn cursor_event(&self, phase: &str, tool: &str, call: &str, input: Value) -> Value {
        json!({
            "conversation_id": CURSOR, "generation_id": "00000000-0000-4000-8000-0000000000e1",
            "model": "default", "session_id": CURSOR, "hook_event_name": phase,
            "cursor_version": "1.7.2", "workspace_roots": [self.work()],
            "user_email": null, "transcript_path": null, "tool_name": tool,
            "tool_input": input, "tool_use_id": call, "cwd": self.work(),
        })
    }

    /// A Cursor `Write` of `file` with `content`.
    fn cursor_write(&self, call: &str, file: &str, content: &str) {
        let path = self.work().join(file);
        let input = json!({"file_path": path, "content": content});
        let pre = self.host(
            "cursor",
            &self.cursor_event("preToolUse", "Write", call, input.clone()),
        );
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
        fs::write(&path, content).unwrap();
        let post = self.host(
            "cursor",
            &self.cursor_event("postToolUse", "Write", call, input),
        );
        assert_eq!(post.coverage, "observed", "{post:?}");
    }

    /// A Cursor `Shell` call of `command`.
    fn cursor_shell(&self, call: &str, command: &str) -> Output {
        let event = |pre: bool| {
            let input = json!({"command": command, "cwd": self.work()});
            if pre {
                return self.cursor_event("preToolUse", "Shell", call, input);
            }
            let mut event = self.cursor_event("postToolUse", "Shell", call, input);
            event["tool_output"] = json!("{\"output\":\"\",\"exitCode\":0}");
            event["duration"] = json!(20);
            event
        };
        self.host_shell("cursor", event, command)
    }
}

/// Acceptance: a Cursor MCP call to `langspan_commit_mine` returns the same
/// result fields as the CLI `--json`, here the CLI from a Cursor `Shell`
/// call that names no session.
#[tokio::test]
async fn a_cursor_mcp_commit_mine_returns_the_cli_json_fields() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
    let f = Fixture::new();
    // Alice changes line 2; the Cursor session changes line 18 and writes a
    // new file.
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 2\n",
        "line 2, by Alice\n",
    );
    let (only_cursor, both) = line_2_and_18("Alice", "Cursor");
    // Cursor reads the file Alice changed before it writes it.
    let mut read = f.cursor_event(
        "postToolUse",
        "Read",
        "cursor-0",
        json!({"file_path": f.work().join("shared.txt")}),
    );
    read["tool_output"] = json!("{}");
    assert_eq!(f.host("cursor", &read).coverage, "read_recorded");
    f.cursor_write("cursor-1", "shared.txt", &both);
    f.cursor_write("cursor-2", "cursor-1.txt", "Cursor's first\n");

    // The CLI, from Cursor's shell, without --session.
    let ran = f.cursor_shell("cursor-3", &f.commit_mine_json("Cursor: first"));
    assert!(ran.status.success(), "{}", said(&ran));
    let cli: Value = serde_json::from_str(&stdout_of(&ran)).unwrap();
    assert_eq!(cli["result"], "committed", "{cli}");
    assert_eq!(cli["session_id"], format!("cursor.{CURSOR}"), "{cli}");
    assert_eq!(cli["paths"], json!(["shared.txt", "cursor-1.txt"]), "{cli}");
    assert_eq!(git(&f.work(), &["show", "HEAD:shared.txt"]), only_cursor);

    // Cursor writes again, and commits through its MCP registration.
    f.cursor_write("cursor-4", "cursor-2.txt", "Cursor's second\n");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("mcp")
        .arg("--state-dir")
        .arg(f.state())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
    let request = CallToolRequestParams::new("langspan_commit_mine").with_arguments(
        json!({
            "request_id": "cursor-commit-1", "workspace_id": "work",
            "session_id": format!("cursor.{CURSOR}"), "message": "Cursor: second",
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let result = tokio::time::timeout(Duration::from_secs(60), client.call_tool(request))
        .await
        .unwrap()
        .unwrap();
    client.cancel().await.unwrap();
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let mcp = result.structured_content.unwrap();
    assert_eq!(mcp["result"], "committed", "{mcp}");
    assert_eq!(mcp["paths"], json!(["cursor-2.txt"]), "{mcp}");

    // Every field of the CLI's JSON, each with the same type; the MCP
    // result adds only its exit code and request ID.
    let cli = cli.as_object().unwrap();
    let mcp = mcp.as_object().unwrap();
    for (key, value) in cli {
        let other = &mcp[key];
        assert_eq!(
            std::mem::discriminant(value),
            std::mem::discriminant(other),
            "{key}: CLI {value}, MCP {other}"
        );
    }
    let extra: Vec<&String> = mcp.keys().filter(|key| !cli.contains_key(*key)).collect();
    assert_eq!(extra, ["exit_code", "request_id"]);
    for key in ["session_id", "left", "refused", "moved"] {
        assert_eq!(cli[key], mcp[key], "{key}");
    }
    assert_eq!(mcp["exit_code"], 0);
    assert_eq!(f.status(), " M shared.txt\n");
}

/// Acceptance: a failing owner hook commits nothing, and the reply says
/// which hook refused.
#[test]
fn each_failing_commit_hook_is_named_and_nothing_is_committed() {
    for hook in ["pre-commit", "prepare-commit-msg", "commit-msg"] {
        let f = Fixture::new();
        f.hooks(&[(hook, "echo 'hook said no' >&2; exit 1")]);
        f.edit(
            ALICE,
            "alice-1",
            "shared.txt",
            "line 2\n",
            "line 2, by Alice\n",
        );
        f.codex_patch("codex-1", "line 18", "line 18, by Codex");
        let index = fs::read(f.work().join(".git/index")).unwrap();
        let ran = f.codex_shell("codex-2", &f.commit_mine_command("x"));
        assert!(!ran.status.success(), "{hook}");
        let text = String::from_utf8_lossy(&ran.stderr).into_owned();
        assert!(
            text.contains(&format!("the {hook} hook refused the commit")),
            "{hook}: {text}"
        );
        assert!(text.contains("nothing was committed"), "{hook}: {text}");
        // The hook's own words reach the caller.
        assert!(text.contains("hook said no"), "{hook}: {text}");
        assert_eq!(f.subjects(), "start\n", "{hook}");
        assert_eq!(
            fs::read(f.work().join(".git/index")).unwrap(),
            index,
            "{hook}"
        );
        assert_eq!(f.status(), " M shared.txt\n", "{hook}");
    }
}

impl Fixture {
    /// A directory holding a `git` that answers `git hook` as Git before
    /// 2.36 does, logging each such call to `old-git.log`, and passes
    /// everything else to the real Git.
    fn old_git(&self) -> PathBuf {
        let real = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("git"))
                    .find(|git| git.is_file())
            })
            .expect("git on PATH");
        let dir = self.base.join("old-git");
        fs::create_dir_all(&dir).unwrap();
        let log = self.base.join("old-git.log");
        let script = format!(
            r#"#!/bin/sh
skip=
for arg in "$@"; do
  if [ -n "$skip" ]; then skip=; continue; fi
  case "$arg" in
    -C|-c) skip=1 ;;
    -*) ;;
    hook) echo "$*" >> '{log}'
          echo "git: 'hook' is not a git command. See 'git --help'." >&2
          exit 1 ;;
    *) break ;;
  esac
done
exec '{real}' "$@"
"#,
            log = log.display(),
            real = real.display()
        );
        let shim = dir.join("git");
        fs::write(&shim, script).unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    /// `commit --mine` for `session` with `first` ahead of PATH.
    fn commit_mine_with_path(&self, session: &str, message: &str, first: &Path) -> Output {
        let path = std::env::join_paths(
            std::iter::once(first.to_path_buf())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .env("PATH", path)
            .arg("--state-dir")
            .arg(self.state())
            .args([
                "commit",
                "--mine",
                "--session",
                &format!("claude-code.{session}"),
            ])
            .arg("--root")
            .arg(self.work())
            .args(["-m", message])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    }

    /// Install `hooks` in the Git directory's own `hooks`.
    fn git_dir_hooks(&self, hooks: &[(&str, &str)]) {
        let dir = self.work().join(".git/hooks");
        fs::create_dir_all(&dir).unwrap();
        for (name, body) in hooks {
            let path = dir.join(name);
            fs::write(&path, format!("#!/bin/sh\nset -e\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

/// Acceptance: the Git hook fallback for Git before 2.36. With no `git hook
/// run`, the hooks run from the Git directory's `hooks` or from
/// `core.hooksPath`, with the private index and the message file, and a
/// refusal there commits nothing.
#[test]
fn without_git_hook_run_the_owners_hooks_run_from_the_hooks_directory() {
    for configured in [false, true] {
        let f = Fixture::new();
        let seen = f.base.join("seen");
        // pre-commit records that its index holds the session's change;
        // the message hooks get the message file and may rewrite it.
        let pre_commit = format!(
            "test -n \"$GIT_INDEX_FILE\"\ngit show :shared.txt | grep -c 'by Alice' > '{}'",
            seen.display()
        );
        let hooks = [
            ("pre-commit", pre_commit.as_str()),
            (
                "prepare-commit-msg",
                "test \"$2\" = message\nprintf 'prepared\\n' >> \"$1\"",
            ),
            (
                "commit-msg",
                "printf '\\nChecked-by: commit-msg\\n' >> \"$1\"",
            ),
        ];
        if configured {
            f.hooks(&hooks);
        } else {
            f.git_dir_hooks(&hooks);
        }
        f.edit(
            ALICE,
            "alice-1",
            "shared.txt",
            "line 2\n",
            "line 2, by Alice\n",
        );
        f.edit(BOB, "bob-1", "shared.txt", "line 18\n", "line 18, by Bob\n");

        let output = f.commit_mine_with_path(ALICE, "Alice", &f.old_git());
        let case = format!("core.hooksPath set: {configured}");
        assert!(output.status.success(), "{case}: {}", said(&output));
        let log = fs::read_to_string(f.base.join("old-git.log")).unwrap_or_default();
        assert!(
            log.contains("hook run"),
            "{case}: no git hook call: {log:?}"
        );
        assert_eq!(fs::read_to_string(&seen).unwrap(), "1\n", "{case}");
        assert_eq!(
            git(&f.work(), &["log", "-1", "--format=%B"]),
            "Alice\nprepared\n\nChecked-by: commit-msg\n\n",
            "{case}"
        );
        assert_eq!(
            git(&f.work(), &["show", "HEAD:shared.txt"]),
            numbered().replace("line 2\n", "line 2, by Alice\n"),
            "{case}"
        );
        assert_eq!(f.status(), " M shared.txt\n", "{case}");
    }

    // A refusal from the hooks directory stops the commit.
    let f = Fixture::new();
    f.git_dir_hooks(&[("pre-commit", "echo 'old hook said no' >&2\nexit 1")]);
    f.edit(ALICE, "alice-1", "a.txt", "alpha", "alpha, by Alice");
    let output = f.commit_mine_with_path(ALICE, "Alice", &f.old_git());
    let text = said(&output);
    assert!(!output.status.success(), "{text}");
    assert!(
        text.contains("the pre-commit hook refused the commit"),
        "{text}"
    );
    assert!(text.contains("old hook said no"), "{text}");
    assert_eq!(f.subjects(), "start\n");
    assert_eq!(f.status(), " M a.txt\n");
}

/// A formatter one session runs over a file both changed reflows both
/// sessions' lines. Each reformatted line stays with the session that wrote
/// it: `commit --mine` commits the runner's own reformatted hunk and leaves
/// the other session's, reformatted, unstaged.
#[test]
fn a_formatter_reflowing_both_sessions_lines_in_one_file_commits_only_the_runners() {
    let f = Fixture::new();
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 2\n",
        "fn alice(a,b) {}\n",
    );
    f.edit(BOB, "bob-1", "shared.txt", "line 18\n", "fn bob(c,d) {}\n");
    // A space after every comma, as a code formatter puts one.
    let scratch = f.base.join("formatted");
    let command = format!(
        "sed 's/,\\([^ ]\\)/, \\1/g' shared.txt > '{0}' && cat '{0}' > shared.txt",
        scratch.display()
    );
    let (_, ran) = f.bash_call(ALICE, "alice-2", &command);
    assert!(ran.unwrap().status.success());
    let formatted = f.read("shared.txt");
    assert!(
        formatted.contains("fn alice(a, b) {}\nline 3"),
        "{formatted}"
    );
    assert!(
        formatted.contains("fn bob(c, d) {}\nline 19"),
        "{formatted}"
    );

    let output = f.commit_mine(ALICE, "Alice: formatted");
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(
        git(&f.work(), &["show", "HEAD:shared.txt"]),
        numbered().replace("line 2\n", "fn alice(a, b) {}\n")
    );
    assert_eq!(f.read("shared.txt"), formatted);
    assert_eq!(f.status(), " M shared.txt\n");
    let left = git(&f.work(), &["diff", "-U0"]);
    assert!(left.contains("+fn bob(c, d) {}"), "{left}");
    assert!(!left.contains("alice"), "{left}");

    // The reformatted hunk left is Bob's, not nobody's: his own commit
    // takes it.
    let output = f.commit_mine(BOB, "Bob: formatted");
    assert!(output.status.success(), "{}", said(&output));
    assert!(
        stdout_of(&output).starts_with("Committed this session's changes of 1 file(s)"),
        "{}",
        said(&output)
    );
    assert_eq!(git(&f.work(), &["show", "HEAD:shared.txt"]), formatted);
    assert_eq!(f.status(), "");
}

/// A formatter that joins one session's line with another's makes a line
/// neither wrote. `commit --mine` cannot tell whose it is: it refuses,
/// naming the file, and commits and stages nothing.
#[test]
fn a_formatter_joining_two_sessions_lines_into_one_is_refused_not_guessed() {
    let f = Fixture::new();
    f.edit(ALICE, "alice-1", "shared.txt", "line 10\n", "alice(a,\n");
    f.edit(BOB, "bob-1", "shared.txt", "line 11\n", "b) // Bob\n");
    // Joins a line that ends in a comma with the next one.
    let command = "sed -e ':a' -e '/,$/N; s/,\\n/, /; ta' shared.txt > joined.tmp && cat joined.tmp > shared.txt && rm joined.tmp";
    let (_, ran) = f.bash_call(ALICE, "alice-2", command);
    assert!(ran.unwrap().status.success());
    let joined = f.read("shared.txt");
    assert!(
        joined.contains("line 9\nalice(a, b) // Bob\nline 12"),
        "{joined}"
    );
    let index = fs::read(f.work().join(".git/index")).unwrap();

    let output = f.commit_mine(ALICE, "Alice");
    let text = said(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("nothing was committed"), "{text}");
    assert!(text.contains("shared.txt"), "{text}");
    assert_eq!(f.subjects(), "start\n");
    assert_eq!(fs::read(f.work().join(".git/index")).unwrap(), index);
    assert_eq!(f.read("shared.txt"), joined);
}

// ---------------------------------------------------------------------------
// The guard on every host's shell tool (issue #25)
// ---------------------------------------------------------------------------

impl Fixture {
    /// Commit `files`, each twenty numbered lines, so that a session's later
    /// edit of one is an uncommitted change.
    fn commit_files(&self, files: &[&str]) {
        for file in files {
            let path = self.work().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, numbered()).unwrap();
        }
        git(&self.work(), &["add", "--", "."]);
        git(&self.work(), &["commit", "-q", "-m", "files"]);
    }

    /// A Claude Code hook run with `budget_ms` as its budget.
    fn hook_with_budget(&self, event: Value, budget_ms: u64) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "work", "--root"])
            .arg(self.work())
            .args(["--budget-ms", &budget_ms.to_string()])
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
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            reason: None,
        }
    }
}

/// The hold names the Claude Code session, its file and its lines, in the
/// host's own reason field.
fn assert_names_alices_lines(run: &HostRun) {
    assert_eq!(run.coverage, "denied_commit", "{run:?}");
    let reason = run.reason.as_deref().unwrap_or_default();
    assert!(reason.contains(&format!("claude-code.{ALICE}")), "{reason}");
    assert!(reason.contains("\"src/a.rs\" (line 2)"), "{reason}");
    assert!(reason.contains("commit --mine"), "{reason}");
    assert!(reason.contains("not instructions"), "{reason}");
}

/// Acceptance: on Codex, `git commit -am "x"` while a Claude Code session
/// has uncommitted lines in `src/a.rs` is held once, naming that session,
/// the file and the lines; the same command again commits.
#[test]
fn a_codex_commit_all_is_held_once_naming_the_claude_sessions_lines_and_then_commits() {
    let f = Fixture::new();
    f.commit_files(&["src/a.rs"]);
    f.edit(
        ALICE,
        "alice-1",
        "src/a.rs",
        "line 2\n",
        "line 2, by Alice\n",
    );
    f.codex_patch("codex-1", "line 18", "line 18, by Codex");
    let command = "git commit -am \"x\"";
    let event = json!({
        "session_id": CODEX, "turn_id": "turn-1", "transcript_path": null,
        "cwd": f.work(), "hook_event_name": "PreToolUse",
        "model": "example-model", "permission_mode": "default",
        "tool_name": "Bash", "tool_use_id": "codex-2",
        "tool_input": {"command": command},
    });

    let held = f.host("codex", &event);
    assert_names_alices_lines(&held);
    // Nothing was staged or committed.
    assert_eq!(f.subjects(), "files\nstart\n");
    assert_eq!(f.status(), " M shared.txt\n M src/a.rs\n");

    // The same command, run again by that session, commits: held once.
    let mut again = event.clone();
    again["tool_use_id"] = json!("codex-3");
    let repeat = f.host("codex", &again);
    assert!(!repeat.coverage.starts_with("denied"), "{repeat:?}");
    assert!(repeat.detail.contains("told so once"), "{repeat:?}");
    let ran = Process::new("/bin/sh")
        .args(["-c", command])
        .current_dir(f.work())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    again["hook_event_name"] = json!("PostToolUse");
    f.host("codex", &again);
    assert!(ran.status.success(), "{}", said(&ran));
    assert_eq!(f.subjects(), "x\nfiles\nstart\n");
    assert_eq!(f.status(), "");
    assert_eq!(
        f.refusals().len(),
        1,
        "a commit let through records nothing"
    );
}

/// Acceptance: a Cursor `Shell` event running `git add -A` is held the same
/// way.
#[test]
fn a_cursor_shell_add_all_is_held_naming_the_claude_sessions_lines() {
    let f = Fixture::new();
    f.commit_files(&["src/a.rs"]);
    f.edit(
        ALICE,
        "alice-1",
        "src/a.rs",
        "line 2\n",
        "line 2, by Alice\n",
    );
    f.cursor_write("cursor-1", "cursor.txt", "Cursor's file\n");
    let input = json!({"command": "git add -A", "cwd": f.work()});
    let held = f.host(
        "cursor",
        &f.cursor_event("preToolUse", "Shell", "cursor-2", input),
    );
    assert_names_alices_lines(&held);
    assert_eq!(git(&f.work(), &["diff", "--cached", "--name-only"]), "");
}

/// Acceptance: `git add src/b.rs`, where `src/b.rs` holds only the
/// session's own lines, is not held while another session has uncommitted
/// lines in `src/a.rs`.
#[test]
fn adding_a_path_that_holds_only_the_sessions_own_lines_is_not_held() {
    let f = Fixture::new();
    f.commit_files(&["src/a.rs", "src/b.rs"]);
    f.edit(BOB, "bob-1", "src/a.rs", "line 2\n", "line 2, by Bob\n");
    f.edit(
        ALICE,
        "alice-1",
        "src/b.rs",
        "line 5\n",
        "line 5, by Alice\n",
    );
    let (run, ran) = f.bash_call(ALICE, "alice-2", "git add src/b.rs");
    assert!(!run.coverage.starts_with("denied"), "{run:?}");
    // Only src/b.rs's entries are read, and they are all Alice's.
    assert!(
        run.detail
            .contains("no other session's uncommitted lines are journaled in what it would take"),
        "{}",
        run.detail
    );
    assert!(ran.unwrap().status.success());
    assert_eq!(f.status(), " M src/a.rs\nM  src/b.rs\n");

    // Bob's line added to src/b.rs as well: his entry is read, and the
    // command is held.
    f.edit(BOB, "bob-2", "src/b.rs", "line 18\n", "line 18, by Bob\n");
    let run = f.bash(ALICE, "PreToolUse", "alice-3", "git add src/b.rs");
    assert_eq!(run.coverage, "denied_commit", "{run:?}");
    assert!(
        run.reason.unwrap().contains("\"src/b.rs\" (line 18)"),
        "held for Bob's line"
    );
}

/// Acceptance: with 4,000 journal entries over 50 files, the guard for a
/// commit of 2 files stays within the default hook budget (1,500 ms), and
/// makes the whole decision each time (held once, then told so once). Each
/// run's time is printed, one row per run; ADR 0026 records the figure.
#[test]
fn the_guard_for_a_two_file_commit_stays_within_the_hook_budget_over_a_large_journal() {
    let f = Fixture::new();
    let files: Vec<String> = (0..50).map(|n| format!("src/f{n:02}.rs")).collect();
    let names: Vec<&str> = files.iter().map(String::as_str).collect();
    f.commit_files(&names);
    f.edit(
        ALICE,
        "alice-1",
        "src/f00.rs",
        "line 2\n",
        "line 2, by Alice\n",
    );
    f.edit(BOB, "bob-1", "src/f01.rs", "line 5\n", "line 5, by Bob\n");
    // Bob's other writes: 3,998 entries over the other 48 files.
    let bob = format!("claude-code.{BOB}");
    for n in 0..3998 {
        let path = vec![files[2 + n % 48].clone()];
        let operation = format!("bob-noise-{n}");
        langspan::journal::before_edit(&f.state(), &operation, &f.work(), &path);
        langspan::journal::record(
            &f.state(),
            &f.work(),
            &bob,
            &path,
            langspan::journal::Origin::Edit {
                operation_id: &operation,
            },
        )
        .unwrap();
    }
    assert_eq!(
        langspan::journal::entries(&f.state(), &f.work()).len(),
        4000
    );

    let command = "git commit -m x -- src/f00.rs src/f01.rs";
    let mut times = Vec::new();
    for run in 0..20 {
        let started = Instant::now();
        let guarded = f.hook_with_budget(
            json!({
                "session_id": ALICE, "cwd": f.work(), "hook_event_name": "PreToolUse",
                "tool_name": "Bash", "tool_use_id": format!("alice-timed-{run}"),
                "tool_input": {"command": command, "description": "test command"},
            }),
            1500,
        );
        let elapsed = started.elapsed();
        if run == 0 {
            assert_eq!(guarded.coverage, "denied_commit", "{guarded:?}");
        } else {
            assert!(guarded.detail.contains("told so once"), "{guarded:?}");
        }
        eprintln!("guard run {run}: {} ms", elapsed.as_millis());
        times.push(elapsed);
    }
    times.sort();
    let p95 = times[times.len() * 95 / 100 - 1];
    eprintln!(
        "guard p95 over {} runs: {} ms",
        times.len(),
        p95.as_millis()
    );
    assert!(p95 < Duration::from_millis(1500), "p95 {p95:?}: {times:?}");
}
