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
        fs::write(self.work().join(file), content).unwrap();
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
