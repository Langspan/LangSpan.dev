#![cfg(unix)]
//! Repository shapes real teams have, through a real daemon, the real
//! observer and the real hook executable: a repository over the scan's
//! bounds, a merge stopped on a conflict, and a checkout that changes under
//! the sessions working in it.

use langspan::{client, protocol::Request};
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

mod support;

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
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(world.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        // Bounded by WAIT, far above a loaded machine's start-up time; a
        // daemon that exits fails at once.
        let started = Instant::now();
        while let Err(error) = client::call(&world.state_dir(), &request(Command::Status)) {
            if let Some(status) = world.daemon.as_mut().unwrap().try_wait().unwrap() {
                panic!("the daemon exited before it answered ({status}): {error:?}");
            }
            assert!(
                started.elapsed() < WAIT,
                "the daemon did not answer within {WAIT:?}: {error:?}"
            );
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
            Outcome::Status { state, .. } => state,
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
        // `doctor` reads the user-level install: this world's own home,
        // never the machine's.
        let output = support::in_home(
            &mut Process::new(env!("CARGO_BIN_EXE_langspan")),
            &self.base.join("home"),
        )
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
            .env("LANGSPAN_AUTOSTART", "off")
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
    /// A shell call by `session` through its hooks: `run` stands for the
    /// command running between the pre- and post-tool hooks.
    fn shell(&self, session: &str, call: &str, command: &str, run: impl FnOnce()) -> Run {
        let pre = self.shell_hook(session, call, command, "PreToolUse");
        assert!(
            !matches!(pre.coverage.as_str(), "denied" | "denied_stale"),
            "{pre:?}"
        );
        run();
        self.shell_hook(session, call, command, "PostToolUse")
    }
    /// One hook event of `session`'s shell call `call`.
    fn shell_hook(&self, session: &str, call: &str, command: &str, name: &str) -> Run {
        self.hook(json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": self.work(),
            "hook_event_name": name, "tool_name": "Bash", "tool_use_id": call,
            "tool_input": {"command": command, "description": "test command"},
            "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
        }))
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
    Request::new("read", command)
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
/// with the daemon's own bounds and its two-second scan deadline, whichever
/// of them stops the scan.
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
    w.wait("the coverage in state", || {
        w.state().scan_coverage.get("work").cloned()
    });
    // The counts of one scan. The daemon's scan stops at the file-count
    // bound, or, on a slow or loaded machine, at its time bound: clean
    // files are versioned only in the first half of the two-second
    // deadline. Either way it is partial, says so and accounts for every
    // file. The exact count at the file bound is held by
    // `the_default_file_bound_versions_exactly_4096_files` in
    // tests/git_observer.rs, with a deadline that cannot stop it first.
    let scan = w.changes().1.unwrap();
    let coverage = scan.coverage.clone().unwrap_or_default();
    let bound = if scan.file_count == 4096 {
        "the file-count bound"
    } else {
        "the time bound"
    };
    let seen = format!(
        "{bound} stopped the scan: {} files versioned, coverage {coverage:?}",
        scan.file_count
    );
    eprintln!("{seen}");
    assert!(coverage.is_partial(), "{seen}");
    assert_eq!(coverage.too_large_paths, ["big.bin"], "{seen}");
    // 4,202 listed: each one versioned, too large or left out.
    assert_eq!(
        scan.file_count + coverage.too_large + coverage.omitted,
        4202,
        "{seen}"
    );
    assert!(scan.file_count <= 4096, "{seen}");

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

/// Issue #139: a session that merges through its own shell tool is not told
/// the checkout changed under it; another session in the checkout is, with
/// the session that ran the merge named. A change no session's command
/// explains (the owner's terminal) is still told to both.
#[test]
fn a_sessions_own_merge_is_not_briefed_to_it_and_is_named_to_the_others() {
    let w = World::new(upstream_to_merge);
    w.read(ALICE, "read-a", "a.txt");
    w.read(BOB, "read-b", "b.txt");
    w.wait_for_scan_of_head();

    // Alice merges through her shell tool. The scan records the change
    // before her post-tool hook reports the command: the harder order.
    let mut seen = None;
    let post = w.shell(ALICE, "alice-merge", "git merge origin/main", || {
        ok(&w.work(), &["merge", "--quiet", "origin/main"]);
        seen = Some(w.wait("the merge's checkout change", || {
            w.state().checkouts.get("work")?.first().cloned()
        }));
    });
    let merged = seen.unwrap();
    assert_eq!(merged.files, 2);
    let own = post.context.unwrap_or_default();
    assert!(
        !own.contains("The checkout changed"),
        "Alice's own merge is not news to her: {own}"
    );
    let own = w.prompt(ALICE).context.unwrap_or_default();
    assert!(!own.contains("The checkout changed"), "{own}");

    let theirs = w.prompt(BOB).context.unwrap_or_default();
    assert!(
        theirs.contains("The checkout changed under you"),
        "{theirs}"
    );
    assert!(
        theirs.contains(&format!("session claude-code.{ALICE}")),
        "Bob is told who ran it: {theirs}"
    );
    assert!(theirs.contains("git merge origin/main"), "{theirs}");

    // The owner switches branches in a terminal: no session's command.
    w.wait_for_scan_of_head();
    ok(&w.work(), &["checkout", "--quiet", "other"]);
    w.wait("the owner's checkout change", || {
        let change = w.state().checkouts.get("work")?.first().cloned()?;
        (change.change_id != merged.change_id).then_some(())
    });
    for session in [ALICE, BOB] {
        let context = w.prompt(session).context.unwrap_or_default();
        assert!(
            context.contains(r#"branch "main" became branch "other""#),
            "{session}: {context}"
        );
        let line = context
            .lines()
            .find(|line| line.contains(r#"became branch "other""#))
            .unwrap();
        assert!(line.contains("The checkout changed under you"), "{line}");
        assert!(
            !line.contains("session claude-code."),
            "nobody is named: {line}"
        );
    }
}

/// The paged status records carry checkout runs, so a reader that pages
/// through `status --all` keeps the attribution that suppresses a session's
/// own merge, as the single-frame status does.
#[test]
fn paged_status_records_carry_the_checkout_runs() {
    let w = World::new(upstream_to_merge);
    w.read(ALICE, "read-a", "a.txt");
    w.wait_for_scan_of_head();
    w.shell(ALICE, "alice-merge", "git merge origin/main", || {
        ok(&w.work(), &["merge", "--quiet", "origin/main"]);
    });
    let runs = w.state().checkout_runs;
    assert_eq!(runs.get("work").map(Vec::len), Some(1), "{runs:?}");
    let paged =
        client::read_all(|command| Ok(client::call(&w.state_dir(), &request(command))?.outcome))
            .unwrap();
    assert_eq!(paged.checkout_runs, runs);
}

/// Issue #139: Alice commits through her shell tool, which moves HEAD and
/// changes no file, so the scan records no checkout change for it. The owner
/// then restores two dirty files in a terminal, within the grace period, at
/// the branch and HEAD the commit left. That restore is the owner's, not
/// Alice's commit: she is told of it, with nobody named.
#[test]
fn a_restore_after_a_sessions_own_commit_is_still_news_to_it() {
    let w = World::new(upstream_to_merge);
    w.writes(ALICE, "alice-a", "a.txt", "alice a.txt\n");
    fs::write(w.work().join("b.txt"), "owner b.txt\n").unwrap();
    fs::write(w.work().join("c.txt"), "owner c.txt\n").unwrap();
    w.wait("the owner's dirty files", || {
        let (events, _) = w.changes();
        let paths = disk_paths(&events);
        (paths.contains(&"b.txt".to_owned()) && paths.contains(&"c.txt".to_owned())).then_some(())
    });

    let command = "git commit --quiet -m alice a.txt";
    w.shell(ALICE, "alice-commit", command, || {
        ok(&w.work(), &["commit", "--quiet", "-m", "alice", "a.txt"]);
    });
    w.wait_for_scan_of_head();
    assert!(
        w.state().checkouts.get("work").is_none_or(Vec::is_empty),
        "a commit is not a checkout change"
    );

    ok(&w.work(), &["checkout", "--", "b.txt", "c.txt"]);
    let restore = w.wait("the owner's restore", || {
        w.state().checkouts.get("work")?.first().cloned()
    });
    assert_eq!(restore.from_head, restore.to_head);
    let context = w.prompt(ALICE).context.unwrap_or_default();
    let line = context
        .lines()
        .find(|line| line.contains("The checkout changed under you"))
        .unwrap_or_else(|| panic!("Alice is told of the owner's restore: {context}"));
    assert!(
        !line.contains("session claude-code."),
        "nobody is named: {line}"
    );
}

/// `main` with three files, `origin/main` a commit ahead changing two of
/// them, and a branch `other` at `main`.
fn upstream_to_merge(work: &Path) {
    for file in ["a.txt", "b.txt", "c.txt"] {
        fs::write(work.join(file), format!("main {file}\n")).unwrap();
    }
    ok(work, &["add", "."]);
    ok(work, &["commit", "--quiet", "-m", "main"]);
    // What the remote's main has: a commit on top, as a remote-tracking ref.
    ok(work, &["checkout", "--quiet", "-b", "upstream"]);
    for file in ["a.txt", "b.txt"] {
        fs::write(work.join(file), format!("upstream {file}\n")).unwrap();
    }
    ok(work, &["commit", "--quiet", "-am", "upstream"]);
    let upstream = ok(work, &["rev-parse", "HEAD"]);
    ok(work, &["update-ref", "refs/remotes/origin/main", &upstream]);
    ok(work, &["checkout", "--quiet", "main"]);
    ok(work, &["branch", "--quiet", "-D", "upstream"]);
    ok(work, &["branch", "--quiet", "other"]);
}

/// Issue #139, redelivery: Alice's post-tool hook cannot reach the daemon
/// when her merge finishes, and the host delivers it again later, after
/// more time than the merge took. The run keeps the merge's own window on
/// the daemon's clock, so her own merge is still not news to her; Bob is
/// told, with her named. The hook and the daemon read different clocks
/// (wall time and time since boot), which only the real binaries show.
#[test]
fn a_redelivered_merge_report_keeps_the_merges_own_window() {
    let w = World::new(upstream_to_merge);
    w.read(ALICE, "read-a", "a.txt");
    w.read(BOB, "read-b", "b.txt");
    w.wait_for_scan_of_head();

    let command = "git merge origin/main";
    let started = Instant::now();
    let pre = w.shell_hook(ALICE, "alice-merge", command, "PreToolUse");
    assert!(!pre.coverage.starts_with("denied"), "{pre:?}");
    ok(&w.work(), &["merge", "--quiet", "origin/main"]);
    let merged = w.wait("the merge's checkout change", || {
        w.state().checkouts.get("work")?.first().cloned()
    });
    assert_eq!(merged.files, 2);

    // The daemon is unreachable when the command finishes: its socket is
    // moved aside, so the hook keeps the outcome for a redelivery.
    let socket = w.state_dir().join("daemon.sock");
    let aside = w.state_dir().join("daemon.sock.aside");
    fs::rename(&socket, &aside).unwrap();
    let first = w.shell_hook(ALICE, "alice-merge", command, "PostToolUse");
    fs::rename(&aside, &socket).unwrap();
    assert_eq!(first.coverage, "unavailable", "{first:?}");
    let ran = started.elapsed();

    // Redelivered after longer than the whole run took, so a window placed
    // back from the redelivery's arrival would start after the scan saw
    // the merge.
    std::thread::sleep(ran + Duration::from_secs(2));
    let post = w.shell_hook(ALICE, "alice-merge", command, "PostToolUse");
    assert!(!post.coverage.starts_with("unavailable"), "{post:?}");
    let own = post.context.unwrap_or_default();
    assert!(!own.contains("The checkout changed"), "{own}");
    let own = w.prompt(ALICE).context.unwrap_or_default();
    assert!(
        !own.contains("The checkout changed"),
        "Alice's own merge is not news to her after a redelivery: {own}"
    );
    let run = w.state().checkout_runs["work"][0].clone();
    assert!(
        run.started_ms <= merged.at_ms && merged.at_ms <= run.finished_ms,
        "the run's window holds the scan's observation: {run:?} {merged:?}"
    );
    // A report the daemon recorded but whose reply was lost is sent again
    // with a larger age: answered from its receipt, not refused.
    let report = |ago| {
        Request::new(
            "checkout-run-retried",
            Command::ObserveCheckoutRun {
                session_id: run.session_id.clone(),
                command: run.command.clone(),
                to_branch: run.to_branch.clone(),
                to_head: run.to_head.clone(),
                ran_ms: 10,
                finished_ago_ms: ago,
                files_unchanged: run.files_unchanged,
            },
        )
    };
    let first = client::call(&w.state_dir(), &report(0)).unwrap();
    let again = client::call(&w.state_dir(), &report(5_000)).unwrap();
    assert_eq!(first.outcome, again.outcome);

    let theirs = w.prompt(BOB).context.unwrap_or_default();
    assert!(
        theirs.contains("The checkout changed under you"),
        "{theirs}"
    );
    assert!(
        theirs.contains(&format!("session claude-code.{ALICE}")),
        "{theirs}"
    );
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
