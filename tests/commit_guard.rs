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

    // The same command again is the session's informed choice: not held.
    let (again, ran) = f.bash_call(ALICE, "alice-3", "git commit -am 'Alice: alpha'");
    assert!(!again.coverage.starts_with("denied"), "{again:?}");
    assert!(again.detail.contains("told so once"), "{}", again.detail);
    assert!(ran.unwrap().status.success());
    assert!(f.head_files().contains("b.txt"));
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
fn a_hunk_that_mixes_another_live_sessions_lines_with_its_own_is_held_and_not_committed() {
    let f = Fixture::new();
    f.edit(BOB, "bob-1", "shared.txt", "line 10\n", "line 10, by Bob\n");
    f.edit(
        ALICE,
        "alice-1",
        "shared.txt",
        "line 11\n",
        "line 11, by Alice\n",
    );
    // Adjacent lines: one hunk, part Bob's and part Alice's.
    let run = f.bash(ALICE, "PreToolUse", "alice-2", "git commit -am 'Alice'");
    assert_eq!(run.coverage, "denied_commit", "{run:?}");
    assert!(run.reason.unwrap().contains(&format!("claude-code.{BOB}")));
    let (_, ran) = f.bash_call(ALICE, "alice-3", &f.commit_mine_command("Alice"));
    let ran = ran.unwrap();
    assert!(!ran.status.success());
    assert!(String::from_utf8_lossy(&ran.stdout).contains("nothing was committed"));
    assert!(f.head_files().contains("start"));
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
