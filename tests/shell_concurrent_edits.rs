#![cfg(unix)]
//! A long shell command and another session's hooked edits at the same time,
//! through the real hook executable, a real daemon, a real Git repository and
//! a real command held open while the other session edits (ADR 0011).
//!
//! The command waits for a signal file outside the working tree, so the
//! other session's edit happens while it is running, whatever the load.

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

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    detail: String,
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// A command started by Alice's shell call that is still running: it waits
/// for `release`, then runs `then`.
struct Held {
    child: Child,
    signal: PathBuf,
}

impl Held {
    fn release(mut self) {
        fs::write(&self.signal, "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "the held command did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A test that fails before releasing its command does not leave it running.
impl Drop for Held {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-shell-edit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join("a.txt"), "a line\n").unwrap();
        fs::write(work.join("b.txt"), "b line\n").unwrap();
        git(&work, &["init", "-q"]);
        git(&work, &["config", "user.name", "Owner"]);
        git(&work, &["config", "user.email", "owner@example.invalid"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "start"]);
        let daemon = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let fixture = Self {
            base,
            daemon: Some(daemon),
        };
        let socket = fixture.state().join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
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
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
        }
    }

    fn read(&self, session: &str, call: &str, file: &str) {
        let run = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file)},
        }));
        assert_eq!(run.coverage, "read_recorded", "{run:?}");
    }

    fn write_event(
        &self,
        session: &str,
        phase: &str,
        call: &str,
        file: &str,
        content: &str,
    ) -> Run {
        let path = self.work().join(file);
        self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": phase,
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": path, "content": content},
            "tool_response": {"filePath": path},
        }))
    }

    /// A whole Claude Code `Write`: its pre hook, the write as the host would
    /// make it, and its post hook.
    fn write(&self, session: &str, call: &str, file: &str, content: &str) {
        let pre = self.write_event(session, "PreToolUse", call, file, content);
        assert_eq!(pre.coverage, "admitted", "{pre:?}");
        // The host makes a new file's directories, as Claude Code's does.
        let path = self.work().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
        let post = self.write_event(session, "PostToolUse", call, file, content);
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

    /// The command Alice's call runs: wait for the signal, then `then`. The
    /// loop makes its write set unknown, so it gets no admission decision
    /// and its changes are found by comparing the workspace.
    fn held_command(&self, call: &str, then: &str) -> (String, PathBuf) {
        let signal = self.base.join(format!("go-{call}"));
        (
            format!(
                "while [ ! -f '{}' ]; do sleep 0.02; done; {then}",
                signal.display()
            ),
            signal,
        )
    }

    /// Alice's shell call starts (its pre hook) and its command runs until
    /// released.
    fn start_held(&self, session: &str, call: &str, then: &str) -> (Held, String) {
        let (command, signal) = self.held_command(call, then);
        let pre = self.bash(session, "PreToolUse", call, &command);
        assert_eq!(pre.coverage, "shell_observing", "{pre:?}");
        let child = Process::new("/bin/sh")
            .args(["-c", &command])
            .current_dir(self.work())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        (Held { child, signal }, command)
    }

    /// The paths `session` was credited with, by tool, from the event log.
    fn credited(&self, session: &str, tool: &str) -> Vec<String> {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(self.state())
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
                    && event["observation"]["tool"] == tool
                    && event["observation"]["session_id"] == format!("claude-code.{session}")
            })
            .flat_map(|event| event["observation"]["paths"].as_array().unwrap().clone())
            .map(|path| path.as_str().unwrap().to_owned())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// Run `session`'s `langspan commit --mine` as a hooked shell call and
    /// return its output.
    fn commit_mine(&self, session: &str, call: &str, message: &str) -> std::process::Output {
        let commit = format!(
            "'{}' --state-dir '{}' commit --mine -m '{message}'",
            env!("CARGO_BIN_EXE_langspan"),
            self.state().display()
        );
        let pre = self.bash(session, "PreToolUse", call, &commit);
        assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
        let ran = Process::new("/bin/sh")
            .args(["-c", &commit])
            .current_dir(self.work())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        self.bash(session, "PostToolUse", call, &commit);
        ran
    }

    /// A formatter-like command: it rewrites each named file in place,
    /// putting a space after every comma, as `cargo fmt --all` rewrites every
    /// file of a workspace. Its write set does not parse, as a formatter's
    /// does not, so its changes are found by comparing the workspace.
    fn formatter(&self, files: &[&str]) -> String {
        let scratch = self.base.join("formatted");
        files
            .iter()
            .map(|file| {
                format!(
                    "sed 's/,\\([^ ]\\)/, \\1/g' {file} > '{scratch}' && cat '{scratch}' > {file}",
                    scratch = scratch.display()
                )
            })
            .collect::<Vec<_>>()
            .join(" && ")
    }

    /// `session` runs `command` as a whole hooked shell call.
    fn run_shell(&self, session: &str, call: &str, command: &str) -> Run {
        let pre = self.bash(session, "PreToolUse", call, command);
        assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
        let status = Process::new("/bin/sh")
            .args(["-c", command])
            .current_dir(self.work())
            .status()
            .unwrap();
        assert!(status.success(), "{command}");
        self.bash(session, "PostToolUse", call, command)
    }

    /// A whole Claude Code `Edit` of `file`, returning its pre hook's run.
    /// The edit is made, and its post hook sent, only when it was admitted.
    fn edit(&self, session: &str, call: &str, file: &str, old: &str, new: &str) -> Run {
        let path = self.work().join(file);
        let input = json!({"file_path": path, "old_string": old, "new_string": new});
        let pre = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Edit", "tool_use_id": call, "tool_input": input,
        }));
        if pre.coverage != "admitted" {
            return pre;
        }
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(old), "{file} lacks {old:?}");
        fs::write(&path, content.replacen(old, new, 1)).unwrap();
        let post = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PostToolUse",
            "tool_name": "Edit", "tool_use_id": call, "tool_input": input,
            "tool_response": {"filePath": path},
        }));
        assert_eq!(post.coverage, "observed", "{post:?}");
        pre
    }

    fn head_files(&self) -> String {
        git(
            &self.work(),
            &["show", "--name-only", "--format=%s", "HEAD"],
        )
    }

    fn status(&self) -> String {
        git(&self.work(), &["status", "--porcelain=v1"])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The issue's scenario: Bob edits a file through his hooked tool while
/// Alice's long command runs. The edit is Bob's; what the command itself
/// wrote is Alice's; and Alice's later edit from her old read is refused once.
#[test]
fn an_edit_another_session_makes_during_a_long_command_is_the_editors() {
    let f = Fixture::new();
    f.read(ALICE, "alice-read", "a.txt");
    let (held, command) = f.start_held(ALICE, "alice-1", "printf mine > mine.txt");
    f.write(BOB, "bob-1", "a.txt", "Bob's line\n");
    held.release();

    let post = f.bash(ALICE, "PostToolUse", "alice-1", &command);
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert!(
        post.detail
            .contains("1 changed file(s) attributed to this session"),
        "{post:?}"
    );
    assert!(
        post.detail.contains("1 to another session's edit"),
        "{post:?}"
    );
    assert_eq!(f.credited(ALICE, "Bash"), ["mine.txt"]);
    assert_eq!(f.credited(BOB, "Write"), ["a.txt"]);

    // Alice read a.txt before Bob changed it: her edit from that read is
    // refused once, as for any other session's write...
    let stale = f.write_event(ALICE, "PreToolUse", "alice-2", "a.txt", "Alice's\n");
    assert_eq!(stale.coverage, "denied_stale", "{stale:?}");
    // ...and her own file is the version she knows.
    let own = f.write_event(ALICE, "PreToolUse", "alice-3", "mine.txt", "more\n");
    assert_eq!(own.coverage, "admitted", "{own:?}");
    assert!(own.detail.contains("source version current"), "{own:?}");
}

/// A file both the command and the other session changed is nobody's: the
/// command's session is not credited, and the editor's known version is not
/// what is on disk now, so each is refused once from what it last knew.
#[test]
fn a_file_changed_by_both_the_command_and_another_sessions_edit_is_unknown() {
    let f = Fixture::new();
    f.read(ALICE, "alice-read", "a.txt");
    let (held, command) = f.start_held(ALICE, "alice-1", "printf 'more\\n' >> a.txt");
    f.write(BOB, "bob-1", "a.txt", "Bob's line\n");
    held.release();
    assert_eq!(
        fs::read_to_string(f.work().join("a.txt")).unwrap(),
        "Bob's line\nmore\n"
    );

    let post = f.bash(ALICE, "PostToolUse", "alice-1", &command);
    assert_eq!(post.coverage, "contested", "{post:?}");
    assert!(
        post.detail
            .contains("0 changed file(s) attributed to this session"),
        "{post:?}"
    );
    assert!(f.credited(ALICE, "Bash").is_empty());

    let alice = f.write_event(ALICE, "PreToolUse", "alice-2", "a.txt", "x\n");
    assert_eq!(alice.coverage, "denied_stale", "{alice:?}");
    let bob = f.write_event(BOB, "PreToolUse", "bob-2", "a.txt", "y\n");
    assert_eq!(bob.coverage, "denied_stale", "{bob:?}");
}

/// An edit still under way when the command finishes: whether it or the
/// command wrote the file first cannot be known, so neither is credited by
/// the command's observation.
#[test]
fn a_file_another_session_is_still_editing_when_the_command_ends_is_unknown() {
    let f = Fixture::new();
    let (held, command) = f.start_held(ALICE, "alice-1", "true");
    let pre = f.write_event(BOB, "PreToolUse", "bob-1", "b.txt", "Bob's\n");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    fs::write(f.work().join("b.txt"), "Bob's\n").unwrap();
    held.release();

    let post = f.bash(ALICE, "PostToolUse", "alice-1", &command);
    assert_eq!(post.coverage, "contested", "{post:?}");
    assert!(f.credited(ALICE, "Bash").is_empty());
    let done = f.write_event(BOB, "PostToolUse", "bob-1", "b.txt", "Bob's\n");
    assert_eq!(done.coverage, "observed", "{done:?}");
    assert_eq!(f.credited(BOB, "Write"), ["b.txt"]);
}

/// After such a run, `commit --mine` takes the command's own file and
/// leaves the other session's edit in the working tree.
#[test]
fn commit_mine_after_a_long_command_commits_its_own_lines_and_leaves_the_editors() {
    let f = Fixture::new();
    let (held, command) = f.start_held(ALICE, "alice-1", "printf 'mine\\n' >> a.txt");
    f.write(BOB, "bob-1", "b.txt", "Bob's line\n");
    held.release();
    let post = f.bash(ALICE, "PostToolUse", "alice-1", &command);
    assert_eq!(post.coverage, "shell_observed", "{post:?}");

    let commit = format!(
        "'{}' --state-dir '{}' commit --mine -m 'Alice: mine'",
        env!("CARGO_BIN_EXE_langspan"),
        f.state().display()
    );
    let pre = f.bash(ALICE, "PreToolUse", "alice-2", &commit);
    assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
    let ran = Process::new("/bin/sh")
        .args(["-c", &commit])
        .current_dir(f.work())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    f.bash(ALICE, "PostToolUse", "alice-2", &commit);
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let head = f.head_files();
    assert!(head.starts_with("Alice: mine\n"), "{head}");
    assert!(head.contains("a.txt") && !head.contains("b.txt"), "{head}");
    assert_eq!(f.status(), " M b.txt\n");
    assert_eq!(
        fs::read_to_string(f.work().join("b.txt")).unwrap(),
        "Bob's line\n"
    );
}

/// Issue #98: Alice edits a.txt, Bob edits b.txt while Alice's formatter
/// runs, and the formatter rewrites both. a.txt's reformatted lines stay
/// Alice's, b.txt (changed by both) is credited to no one, and Alice's
/// `commit --mine` commits her formatted lines and leaves Bob's file.
#[test]
fn a_formatter_keeps_its_sessions_own_file_when_another_file_it_rewrote_is_contested() {
    let f = Fixture::new();
    f.write(ALICE, "alice-1", "a.txt", "a line\nfn alice(x,y) {}\n");
    let (held, command) = f.start_held(ALICE, "alice-2", &f.formatter(&["a.txt", "b.txt"]));
    f.write(BOB, "bob-1", "b.txt", "b line\nfn bob(x,y) {}\n");
    held.release();
    assert_eq!(
        fs::read_to_string(f.work().join("b.txt")).unwrap(),
        "b line\nfn bob(x, y) {}\n"
    );

    let post = f.bash(ALICE, "PostToolUse", "alice-2", &command);
    assert_eq!(post.coverage, "contested", "{post:?}");
    assert!(
        post.detail
            .contains("1 changed file(s) attributed to this session")
            && post.detail.contains("1 attributed to no session"),
        "{post:?}"
    );
    assert_eq!(f.credited(ALICE, "Bash"), ["a.txt"]);

    let ran = f.commit_mine(ALICE, "alice-3", "Alice: formatted");
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let head = f.head_files();
    assert!(head.starts_with("Alice: formatted\n"), "{head}");
    assert!(head.contains("a.txt") && !head.contains("b.txt"), "{head}");
    assert_eq!(
        git(&f.work(), &["show", "HEAD:a.txt"]),
        "a line\nfn alice(x, y) {}\n"
    );
    assert_eq!(f.status(), " M b.txt\n");
}

/// Issue #98: Bob's edit of b.txt finished before Alice's formatter started,
/// and the formatter rewrote Bob's line. The rewritten line was changed by
/// both sessions, so it is credited to neither: the file is contested, not
/// Alice's, and her `commit --mine` takes only her own formatted file.
#[test]
fn a_formatter_rewriting_another_sessions_uncommitted_lines_does_not_take_them() {
    let f = Fixture::new();
    f.write(ALICE, "alice-1", "a.txt", "a line\nfn alice(x,y) {}\n");
    f.write(BOB, "bob-1", "b.txt", "b line\nfn bob(x,y) {}\n");

    let post = f.run_shell(ALICE, "alice-2", &f.formatter(&["a.txt", "b.txt"]));
    assert_eq!(post.coverage, "contested", "{post:?}");
    assert_eq!(f.credited(ALICE, "Bash"), ["a.txt"]);

    let ran = f.commit_mine(ALICE, "alice-3", "Alice: formatted");
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let head = f.head_files();
    assert!(head.contains("a.txt") && !head.contains("b.txt"), "{head}");
    assert_eq!(f.status(), " M b.txt\n");
    assert_eq!(
        fs::read_to_string(f.work().join("b.txt")).unwrap(),
        "b line\nfn bob(x, y) {}\n"
    );
}

/// Once another session's lines are committed, rewriting them is a change
/// of committed lines like any other: the command's session is credited.
#[test]
fn a_formatter_rewriting_another_sessions_committed_lines_is_credited() {
    let f = Fixture::new();
    f.write(BOB, "bob-1", "b.txt", "b line\nfn bob(x,y) {}\n");
    git(&f.work(), &["commit", "-q", "-am", "Bob: committed"]);
    let post = f.run_shell(ALICE, "alice-1", &f.formatter(&["b.txt"]));
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert_eq!(f.credited(ALICE, "Bash"), ["b.txt"]);
}

/// A command that only adds lines to a file another session has changed,
/// rewriting none of that session's lines, is credited with what it added.
#[test]
fn a_command_adding_to_another_sessions_file_without_rewriting_its_lines_is_credited() {
    let f = Fixture::new();
    f.write(BOB, "bob-1", "b.txt", "b line\nBob's line\n");
    let post = f.run_shell(
        ALICE,
        "alice-1",
        "for f in b.txt; do printf 'mine\\n' >> $f; done",
    );
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert_eq!(f.credited(ALICE, "Bash"), ["b.txt"]);
}

/// Set `path`'s modification time to `age` ago.
fn age(path: &Path, age: Duration) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - age)
        .unwrap();
}

/// The files in `directory` with extension `extension`.
fn files_with(directory: &Path, extension: &str) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == extension))
        .collect()
}

/// Backdate the start of the one shell run still recorded as running.
fn backdate_running_command(state: &Path, by: Duration) {
    let [run] = files_with(&state.join("shell"), "run").try_into().unwrap();
    let mut record: Value = serde_json::from_slice(&fs::read(&run).unwrap()).unwrap();
    record["started_ms"] = json!(record["started_ms"].as_u64().unwrap() - by.as_millis() as u64);
    fs::write(&run, record.to_string()).unwrap();
    age(&run, by);
}

const CAROL: &str = "00000000-0000-4000-8000-00000000000c";

/// A shell call can run for longer than ten minutes (a background command
/// for up to two hours): another session's later edit must not prune the
/// finished edit marks the still-running command needs, or its result
/// credits that session's file to the command's session.
#[test]
fn a_command_running_longer_than_ten_minutes_keeps_the_edit_marks_it_needs() {
    let f = Fixture::new();
    let (held, command) = f.start_held(ALICE, "alice-1", "printf mine > mine.txt");
    f.write(BOB, "bob-1", "a.txt", "Bob's line\n");

    // Alice's command started eleven minutes ago, Bob's edit five.
    backdate_running_command(&f.state(), Duration::from_secs(11 * 60));
    let [bob] = files_with(&f.state().join("shell").join("edits"), "json")
        .try_into()
        .unwrap();
    age(&bob, Duration::from_secs(5 * 60));

    // Carol's edit starts while the command is still running.
    f.write(CAROL, "carol-1", "b.txt", "Carol's line\n");
    held.release();

    let post = f.bash(ALICE, "PostToolUse", "alice-1", &command);
    assert_eq!(post.coverage, "shell_observed", "{post:?}");
    assert_eq!(f.credited(ALICE, "Bash"), ["mine.txt"]);
    assert_eq!(f.credited(BOB, "Write"), ["a.txt"]);
}

/// A run older than the longest a shell call can run may have lost the
/// edit marks it needed: none of what it changed is credited to it.
#[test]
fn a_command_older_than_a_shell_calls_lifetime_is_credited_with_nothing() {
    let f = Fixture::new();
    let (held, command) = f.start_held(ALICE, "alice-1", "printf mine > mine.txt");
    backdate_running_command(&f.state(), Duration::from_secs(3 * 60 * 60));
    held.release();

    let post = f.bash(ALICE, "PostToolUse", "alice-1", &command);
    assert_eq!(post.coverage, "contested", "{post:?}");
    assert!(f.credited(ALICE, "Bash").is_empty());
}

/// A shell write that lands while an edit's post hook waits on the daemon
/// is not part of that edit: the edit's mark keeps the version the post hook
/// found on entry, so the file is not taken for the editor's alone.
#[test]
fn a_shell_write_during_an_edits_post_hook_is_not_taken_for_the_edits() {
    let mut f = Fixture::new();
    let (held, command) = f.start_held(ALICE, "alice-1", "true");
    let pre = f.write_event(BOB, "PreToolUse", "bob-1", "a.txt", "Bob's line\n");
    assert_eq!(pre.coverage, "admitted", "{pre:?}");
    fs::write(f.work().join("a.txt"), "Bob's line\n").unwrap();

    // Hold Bob's post hook at its first daemon call, and write as Alice's
    // command would in that moment.
    let mut daemon = f.daemon.take().unwrap();
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let socket = f.state().join("daemon.sock");
    let _ = fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let work = f.work();
    let holder = std::thread::spawn(move || {
        let (connection, _) = listener.accept().unwrap();
        fs::write(work.join("a.txt"), "Bob's line\nshell\n").unwrap();
        drop(connection);
    });
    f.write_event(BOB, "PostToolUse", "bob-1", "a.txt", "Bob's line\n");
    holder.join().unwrap();
    let _ = fs::remove_file(&socket);
    f.daemon = Some(
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["daemon", "--state-dir"])
            .arg(f.state())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "daemon did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    held.release();

    let post = f.bash(ALICE, "PostToolUse", "alice-1", &command);
    assert!(
        !post.detail.contains("to another session's edit"),
        "{post:?}"
    );
    assert_eq!(post.coverage, "contested", "{post:?}");
}

/// The most edit marks kept at once (`MAX_RECORDS` in `src/shell.rs`).
const MAX_MARKS: usize = 4096;

/// Fill the edit-mark directory to its cap with edits still under way in
/// this workspace, as 4,096 concurrent edit calls would leave it.
fn fill_edit_marks(state: &Path, work: &Path) {
    let directory = state.join("shell").join("edits");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let mark = json!({
        "session_id": "filler", "workspace": work, "started_ms": now,
        "finished_ms": null, "files": {},
    })
    .to_string();
    let existing = files_with(&directory, "json").len();
    for n in existing..MAX_MARKS {
        let path = directory.join(format!("filler-{n}.json"));
        fs::write(&path, &mark).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// While the edit-mark directory is full, an edit whose start could not be
/// marked must not be marked by its post hook either (the directory would
/// outgrow what a shell observation reads), and a shell run going at the
/// same time credits nothing it changed: it cannot tell its own changes from
/// that unmarked edit's.
#[test]
fn a_full_edit_mark_directory_stays_bounded_and_credits_the_command_nothing() {
    let f = Fixture::new();
    let (held, command) = f.start_held(ALICE, "alice-1", "printf mine > mine.txt");
    fill_edit_marks(&f.state(), &f.work());
    f.write(BOB, "bob-1", "b.txt", "Bob's line\n");
    let marks = files_with(&f.state().join("shell").join("edits"), "json").len();
    assert!(marks <= MAX_MARKS, "{marks} edit marks kept");
    held.release();

    let post = f.bash(ALICE, "PostToolUse", "alice-1", &command);
    assert_eq!(post.coverage, "contested", "{post:?}");
    assert!(f.credited(ALICE, "Bash").is_empty());
    assert_eq!(f.credited(BOB, "Write"), ["b.txt"]);
}

/// Issue #101: a file a session creates with a whole-file write is that
/// session's version of it. With another session editing in the same
/// checkout, a later edit of it from that version is admitted, not refused
/// as changed outside any reported edit, and `commit --mine` commits the new
/// file as the session's own.
#[test]
fn a_file_a_session_creates_is_its_own_to_edit_again_and_commit() {
    let f = Fixture::new();
    f.write(ALICE, "alice-1", "tests/created.rs", "fn alice() {}\n");
    f.write(BOB, "bob-1", "b.txt", "b line\nBob's line\n");

    let edit = f.edit(
        ALICE,
        "alice-2",
        "tests/created.rs",
        "fn alice() {}",
        "fn alice() { 1 }",
    );
    assert_eq!(edit.coverage, "admitted", "{edit:?}");
    // Compared with the version the session wrote, not admitted as unknown.
    assert!(edit.detail.contains("source version current"), "{edit:?}");

    let ran = f.commit_mine(ALICE, "alice-3", "Alice: created");
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let head = f.head_files();
    assert!(head.starts_with("Alice: created\n"), "{head}");
    assert!(
        head.contains("tests/created.rs") && !head.contains("b.txt"),
        "{head}"
    );
    assert_eq!(
        git(&f.work(), &["show", "HEAD:tests/created.rs"]),
        "fn alice() { 1 }\n"
    );
    assert_eq!(f.status(), " M b.txt\n");
}

/// Issue #101, with a formatter: the session creates a file, reformats it
/// from its shell with a command that also rewrites the other session's
/// file, edits it again and commits it with `commit --mine`. The created
/// file stays the session's throughout; the other session's file is left.
#[test]
fn a_file_a_session_creates_and_reformats_is_its_own_to_edit_again_and_commit() {
    let f = Fixture::new();
    f.write(ALICE, "alice-1", "tests/created.rs", "fn alice(x,y) {}\n");
    f.write(BOB, "bob-1", "b.txt", "b line\nfn bob(x,y) {}\n");

    let post = f.run_shell(
        ALICE,
        "alice-2",
        &f.formatter(&["tests/created.rs", "b.txt"]),
    );
    assert_eq!(post.coverage, "contested", "{post:?}");
    assert_eq!(f.credited(ALICE, "Bash"), ["tests/created.rs"]);

    let edit = f.edit(
        ALICE,
        "alice-3",
        "tests/created.rs",
        "fn alice(x, y) {}",
        "fn alice(x, y) { x }",
    );
    assert_eq!(edit.coverage, "admitted", "{edit:?}");
    // Compared with the version the session wrote, not admitted as unknown.
    assert!(edit.detail.contains("source version current"), "{edit:?}");

    let ran = f.commit_mine(ALICE, "alice-4", "Alice: created");
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let head = f.head_files();
    assert!(
        head.contains("tests/created.rs") && !head.contains("b.txt"),
        "{head}"
    );
    assert_eq!(
        git(&f.work(), &["show", "HEAD:tests/created.rs"]),
        "fn alice(x, y) { x }\n"
    );
    assert_eq!(f.status(), " M b.txt\n");
}

/// Issue #101, with the other session editing while the formatter runs: its
/// file is contested, and the file this session created stays its own.
#[test]
fn a_file_a_session_creates_stays_its_own_when_its_formatter_overlaps_another_edit() {
    let f = Fixture::new();
    f.write(ALICE, "alice-1", "tests/created.rs", "fn alice(x,y) {}\n");
    let (held, command) = f.start_held(
        ALICE,
        "alice-2",
        &f.formatter(&["tests/created.rs", "b.txt"]),
    );
    f.write(BOB, "bob-1", "b.txt", "b line\nfn bob(x,y) {}\n");
    held.release();
    let post = f.bash(ALICE, "PostToolUse", "alice-2", &command);
    assert_eq!(post.coverage, "contested", "{post:?}");
    assert_eq!(f.credited(ALICE, "Bash"), ["tests/created.rs"]);

    let edit = f.edit(
        ALICE,
        "alice-3",
        "tests/created.rs",
        "fn alice(x, y) {}",
        "fn alice(x, y) { x }",
    );
    assert_eq!(edit.coverage, "admitted", "{edit:?}");
    // Compared with the version the session wrote, not admitted as unknown.
    assert!(edit.detail.contains("source version current"), "{edit:?}");

    let ran = f.commit_mine(ALICE, "alice-4", "Alice: created");
    assert!(
        ran.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let head = f.head_files();
    assert!(
        head.contains("tests/created.rs") && !head.contains("b.txt"),
        "{head}"
    );
    assert_eq!(
        git(&f.work(), &["show", "HEAD:tests/created.rs"]),
        "fn alice(x, y) { x }\n"
    );
    assert_eq!(f.status(), " M b.txt\n");
}
