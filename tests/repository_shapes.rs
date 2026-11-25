#![cfg(unix)]
//! Repository shapes real teams have, through a real daemon, the real
//! observer and the real hook executable: a repository over the scan's
//! bounds, a merge stopped on a conflict, and a checkout that changes under
//! the sessions working in it.

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{Command, Event, Outcome, ScanSummary, State};
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
const WAIT: Duration = Duration::from_secs(60);

fn git(root: &Path, args: &[&str]) -> (bool, String) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Dev",
            "-c",
            "user.email=dev@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    )
}

fn ok(root: &Path, args: &[&str]) -> String {
    let (success, text) = git(root, args);
    assert!(success, "git {args:?} failed");
    text
}

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    reason: Option<String>,
    context: Option<String>,
}

impl World {
    /// A Git checkout at `work`, filled by `setup`, then a daemon.
    fn new(setup: impl FnOnce(&Path)) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-shapes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let work = base.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        ok(&work, &["init", "--quiet", "--initial-branch=main"]);
        setup(&work);
        let mut world = Self { base, daemon: None };
        world.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(world.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&world.state_dir(), &request(Command::Status)).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        world
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn state(&self) -> State {
        match client::call(&self.state_dir(), &request(Command::Status))
            .unwrap()
            .outcome
        {
            Outcome::Status { state } => state,
            other => panic!("{other:?}"),
        }
    }
    fn changes(&self) -> (Vec<Event>, Option<ScanSummary>) {
        let command = Command::Changes {
            workspace_id: "work".into(),
            after: 0,
            limit: 1000,
        };
        match client::call(&self.state_dir(), &request(command))
            .unwrap()
            .outcome
        {
            Outcome::Changes { events, scan } => (
                events.into_iter().map(|record| record.event).collect(),
                scan,
            ),
            other => panic!("{other:?}"),
        }
    }
    fn wait<T>(&self, what: &str, mut done: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(value) = done() {
                return value;
            }
            assert!(Instant::now() < deadline, "never observed: {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    /// Wait until a completed scan has seen HEAD at the current commit.
    fn wait_for_scan_of_head(&self) {
        let head = ok(&self.work(), &["rev-parse", "HEAD"]);
        self.wait("a scan of the current HEAD", || {
            let (_, scan) = self.changes();
            (scan?.head_oid? == head).then_some(())
        });
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(self.state_dir())
            .args(args)
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
    fn hook(&self, event: Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "work", "--root"])
            .arg(self.work())
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
        let field = |name: &str| {
            reply
                .as_ref()
                .and_then(|reply| reply["hookSpecificOutput"][name].as_str())
                .map(str::to_owned)
        };
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            reason: field("permissionDecisionReason"),
            context: field("additionalContext"),
        }
    }
    fn tool(&self, session: &str, name: &str, tool: &str, call: &str, file: &str) -> Run {
        self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": name,
            "tool_name": tool, "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file), "content": "replacement\n"},
        }))
    }
    fn read(&self, session: &str, call: &str, file: &str) {
        let run = self.tool(session, "PostToolUse", "Read", call, file);
        assert_eq!(run.coverage, "read_recorded");
    }
    /// A complete Write by `session`: admitted, written to disk, reported.
    fn writes(&self, session: &str, call: &str, file: &str, content: &str) {
        let pre = self.tool(session, "PreToolUse", "Write", call, file);
        assert_eq!(pre.coverage, "admitted", "{:?}", pre.reason);
        fs::write(self.work().join(file), content).unwrap();
        let post = self.tool(session, "PostToolUse", "Write", call, file);
        assert_eq!(post.coverage, "observed");
    }
    fn prompt(&self, session: &str) -> Run {
        self.hook(json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": self.work(),
            "hook_event_name": "UserPromptSubmit", "prompt": "carry on",
        }))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn request(command: Command) -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command,
    }
}

fn disk_paths(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::DiskObserved { observation } => Some(observation.path.clone()),
            _ => None,
        })
        .collect()
}

/// Item 1: more files than the scan's 4,096-file bound and one over 4 MiB,
/// with the daemon's own bounds and its two-second scan deadline.
#[test]
fn a_repository_over_the_scan_bounds_is_scanned_partially_and_says_so() {
    let w = World::new(|work| {
        fs::create_dir_all(work.join("a")).unwrap();
        fs::write(work.join("a/lib.rs"), "pub fn coverage_marker() {}\n").unwrap();
        for index in 0..4200 {
            let directory = work.join(format!("d{:02}", index / 100));
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join(format!("f{index:04}.txt")),
                format!("{index}\n"),
            )
            .unwrap();
        }
        fs::write(work.join("big.bin"), vec![b'x'; 5 * 1024 * 1024]).unwrap();
        ok(work, &["add", "."]);
        ok(work, &["commit", "--quiet", "-m", "large"]);
    });
    let (code, text) = w.cli(&[
        "workspace",
        "add",
        "work",
        w.work().to_str().unwrap(),
        "--request-id",
        "ws",
    ]);
    assert_eq!(code, 0, "{text}");
    w.wait_for_scan_of_head();
    let state = w.state();
    assert!(
        !state.scan_failures.contains_key("work"),
        "{:?}",
        state.scan_failures
    );
    let coverage = w.wait("the coverage in state", || {
        w.state().scan_coverage.get("work").cloned()
    });
    assert!(coverage.is_partial());
    assert_eq!(coverage.too_large_paths, ["big.bin"]);
    // 4,202 listed: 4,096 versioned, 1 too large, the rest left out.
    assert_eq!(coverage.omitted, 4202 - 4096 - 1);
    let (_, scan) = w.changes();
    assert_eq!(scan.unwrap().file_count, 4096);

    let (_, recap) = w.cli(&["recap", "work"]);
    assert!(recap.contains("The last scan was partial"), "{recap}");
    assert!(recap.contains("\"big.bin\""), "{recap}");
    let (_, doctor) = w.cli(&["doctor"]);
    assert!(doctor.contains("The last scan was partial"), "{doctor}");

    // The index works on the files the scan has.
    w.wait("the index to hold a/lib.rs", || {
        let (_, text) = w.cli(&["graph", "symbol", "work", "coverage_marker", "--json"]);
        let value: Value = serde_json::from_str(&text).ok()?;
        (!value["outcome"]["symbols"].as_array()?.is_empty()).then_some(())
    });

    // A change to a file that sorts last, past the bound, is still seen.
    fs::write(w.work().join("d41/f4199.txt"), "changed\n").unwrap();
    w.wait("the change to the last file", || {
        let (events, _) = w.changes();
        disk_paths(&events)
            .contains(&"d41/f4199.txt".to_owned())
            .then_some(())
    });
    let (events, _) = w.changes();
    assert_eq!(
        disk_paths(&events),
        ["d41/f4199.txt"],
        "nothing left out is reported changed or deleted"
    );
}

/// Item 7: a merge stopped on a conflict. The scan completes and marks the
/// file, and an edit from the session's earlier read is refused once.
#[test]
fn a_merge_conflict_is_scanned_and_an_edit_from_an_old_read_is_refused_once() {
    let w = World::new(|work| {
        fs::write(work.join("shared.txt"), "base\n").unwrap();
        ok(work, &["add", "."]);
        ok(work, &["commit", "--quiet", "-m", "base"]);
        ok(work, &["checkout", "--quiet", "-b", "theirs"]);
        fs::write(work.join("shared.txt"), "theirs\n").unwrap();
        ok(work, &["commit", "--quiet", "-am", "theirs"]);
        ok(work, &["checkout", "--quiet", "main"]);
        fs::write(work.join("shared.txt"), "ours\n").unwrap();
        ok(work, &["commit", "--quiet", "-am", "ours"]);
    });
    w.read(ALICE, "read-1", "shared.txt");
    w.wait_for_scan_of_head();
    let (merged, _) = git(&w.work(), &["merge", "--quiet", "theirs"]);
    assert!(!merged, "the merge stops on the conflict");
    w.wait("the conflicted file to be observed", || {
        let (events, _) = w.changes();
        disk_paths(&events)
            .contains(&"shared.txt".to_owned())
            .then_some(())
    });
    assert!(
        !w.state().scan_failures.contains_key("work"),
        "a scan during a merge completes"
    );

    let denied = w.tool(ALICE, "PreToolUse", "Write", "alice-1", "shared.txt");
    assert_eq!(denied.coverage, "denied_stale");
    assert!(
        denied.reason.unwrap().contains("\"shared.txt\""),
        "the refusal names the file"
    );
    let again = w.tool(ALICE, "PreToolUse", "Write", "alice-2", "shared.txt");
    assert_eq!(again.coverage, "admitted", "refused once per change");
}

/// Item 14: the checkout changes under two sessions. Each is told, with the
/// old and new branch; each file is refused once; nobody is named for it.
#[test]
fn a_checkout_switch_is_briefed_to_the_sessions_in_it_and_refused_once_per_file() {
    let w = World::new(|work| {
        for file in ["a.txt", "b.txt", "c.txt"] {
            fs::write(work.join(file), format!("main {file}\n")).unwrap();
        }
        ok(work, &["add", "."]);
        ok(work, &["commit", "--quiet", "-m", "main"]);
        ok(work, &["checkout", "--quiet", "-b", "other"]);
        for file in ["a.txt", "b.txt"] {
            fs::write(work.join(file), format!("other {file}\n")).unwrap();
        }
        ok(work, &["commit", "--quiet", "-am", "other"]);
        ok(work, &["checkout", "--quiet", "main"]);
    });
    w.read(ALICE, "read-a", "a.txt");
    w.read(ALICE, "read-b", "b.txt");
    // Bob edits a.txt after Alice read it, and commits it.
    w.writes(BOB, "bob-1", "a.txt", "bob a.txt\n");
    ok(&w.work(), &["commit", "--quiet", "-am", "bob"]);
    w.wait_for_scan_of_head();
    let from = ok(&w.work(), &["rev-parse", "HEAD"]);
    let (switched, _) = git(&w.work(), &["checkout", "--quiet", "other"]);
    assert!(switched);
    let change = w.wait("the checkout change", || {
        w.state().checkouts.get("work")?.first().cloned()
    });
    assert_eq!(change.from_branch.as_deref(), Some("refs/heads/main"));
    assert_eq!(change.to_branch.as_deref(), Some("refs/heads/other"));
    assert_eq!(change.from_head.as_deref(), Some(from.as_str()));
    assert_eq!(change.files, 2);

    for session in [ALICE, BOB] {
        let context = w.prompt(session).context.unwrap_or_default();
        assert!(
            context.contains("The checkout changed under you"),
            "{session}: {context}"
        );
        assert!(
            context.contains(r#"branch "main" became branch "other""#),
            "{context}"
        );
        assert!(context.contains(&from[..12]), "{context}");
    }

    for file in ["a.txt", "b.txt"] {
        let denied = w.tool(ALICE, "PreToolUse", "Write", &format!("{file}-1"), file);
        assert_eq!(denied.coverage, "denied_stale", "{file}");
        let reason = denied.reason.unwrap();
        assert!(
            !reason.contains(BOB),
            "Bob did not make the checkout's version: {reason}"
        );
        let again = w.tool(ALICE, "PreToolUse", "Write", &format!("{file}-2"), file);
        assert_eq!(
            again.coverage, "admitted",
            "{file}: refused once per change"
        );
    }
    // A file the checkout did not change is not refused.
    w.read(ALICE, "read-c", "c.txt");
    let untouched = w.tool(ALICE, "PreToolUse", "Write", "c-1", "c.txt");
    assert_eq!(untouched.coverage, "admitted");
}

/// A file in conflict is never held, with one deliberate exception: a session that read the file before the merge is
/// refused once, because what it read is not what is on disk and the
/// refusal is what makes it read the conflict markers it must resolve. A
/// session with no earlier read of the file, and every later write, goes
/// through: resolving the conflict is never held up by the version record.
#[test]
fn a_file_in_conflict_is_refused_at_most_once_and_never_without_an_earlier_read() {
    let w = World::new(|work| {
        fs::write(work.join("shared.txt"), "base\n").unwrap();
        ok(work, &["add", "."]);
        ok(work, &["commit", "--quiet", "-m", "base"]);
        ok(work, &["checkout", "--quiet", "-b", "theirs"]);
        fs::write(work.join("shared.txt"), "theirs\n").unwrap();
        ok(work, &["commit", "--quiet", "-am", "theirs"]);
        ok(work, &["checkout", "--quiet", "main"]);
        fs::write(work.join("shared.txt"), "ours\n").unwrap();
        ok(work, &["commit", "--quiet", "-am", "ours"]);
    });
    w.read(ALICE, "read-1", "shared.txt");
    w.wait_for_scan_of_head();
    let (merged, _) = git(&w.work(), &["merge", "--quiet", "theirs"]);
    assert!(!merged, "the merge stops on the conflict");
    // Bob never read it: resolving it is not held.
    let bob = w.tool(BOB, "PreToolUse", "Write", "bob-1", "shared.txt");
    assert_eq!(bob.coverage, "admitted", "{:?}", bob.reason);
    w.tool(BOB, "PostToolUse", "Write", "bob-1", "shared.txt");
    // Alice read the version from before the merge: refused once...
    let alice = w.tool(ALICE, "PreToolUse", "Write", "alice-1", "shared.txt");
    assert_eq!(alice.coverage, "denied_stale", "{alice:?}");
    // ...and her next write goes through, whatever it is.
    let again = w.tool(ALICE, "PreToolUse", "Write", "alice-2", "shared.txt");
    assert_eq!(again.coverage, "admitted", "{again:?}");
    w.tool(ALICE, "PostToolUse", "Write", "alice-2", "shared.txt");
    let third = w.tool(ALICE, "PreToolUse", "Write", "alice-3", "shared.txt");
    assert_eq!(third.coverage, "admitted", "{third:?}");
}
