#![cfg(unix)]
//! `langspan statusline` (roadmap H2) against a real daemon and real hook
//! executables, fed the status-line input Claude Code sends on stdin; without
//! a daemon, and with one that never answers; and its `install` entry in a
//! temporary home.

use langspan::{client, protocol::Request};
use langspan_core::{Command, Event, Outcome, RefusalKind};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as Process, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";
const CAROL: &str = "00000000-0000-4000-8000-00000000000c";
const DAVE: &str = "00000000-0000-4000-8000-00000000000d";
/// Claude Code's status-line input, constructed from its documentation.
const INPUT: &str = include_str!("../fixtures/claude-code-statusline-constructed.json");

fn base(name: &str) -> PathBuf {
    let base = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!(
            "ls-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir(&base).unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
    base
}

/// `langspan statusline` with `input` on stdin, and how long it took.
fn status_line(state: &Path, args: &[&str], input: Option<&str>) -> (Output, Duration) {
    let started = Instant::now();
    let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("--state-dir")
        .arg(state)
        .arg("statusline")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(input) = input {
        stdin.write_all(input.as_bytes()).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    (output, started.elapsed())
}

fn text(output: &Output) -> String {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .trim_end()
        .to_owned()
}

/// Every file under `dir` with its size and modification time.
fn listing(dir: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
    let mut found = BTreeMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            found.extend(listing(&path));
        } else {
            found.insert(path, (metadata.len(), metadata.modified().unwrap()));
        }
    }
    found
}

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
    calls: AtomicU64,
}

impl Fixture {
    fn new() -> Self {
        let base = base("statusline");
        fs::create_dir(base.join("work")).unwrap();
        fs::write(base.join("work/a.txt"), "line 1\nline 2\nline 3\n").unwrap();
        let daemon = Process::new(env!("CARGO_BIN_EXE_langspan"))
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
            daemon: Some(daemon),
            calls: AtomicU64::new(0),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fixture.base.join("state/daemon.sock").exists() {
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
    /// The recorded input as Claude Code would send it from this checkout.
    fn input(&self) -> String {
        INPUT.replace("__CWD__", &self.work().to_string_lossy())
    }
    fn line(&self) -> String {
        let (output, _) = status_line(&self.state(), &[], Some(&self.input()));
        text(&output)
    }
    fn hook(&self, event: &Value) -> String {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "work", "--root"])
            .arg(self.work())
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
        diagnostic["coverage"].as_str().unwrap().to_owned()
    }
    fn prompt(&self, session: &str) {
        self.hook(&json!({
            "session_id": session, "cwd": self.work(),
            "hook_event_name": "UserPromptSubmit", "prompt": "work on the api",
        }));
    }
    /// A pre-tool Edit of the second line, with its own tool-use ID.
    fn edit(&self, session: &str) -> (Value, String) {
        self.edit_line(session, "line 2")
    }
    fn edit_line(&self, session: &str, old: &str) -> (Value, String) {
        let event = json!({
            "session_id": session, "cwd": self.work(),
            "hook_event_name": "PreToolUse", "tool_name": "Edit",
            "tool_use_id": format!("call-{}", self.calls.fetch_add(1, Ordering::Relaxed)),
            "tool_input": {
                "file_path": self.work().join("a.txt"),
                "old_string": old, "new_string": format!("{old} {session}"),
            },
        });
        let coverage = self.hook(&event);
        (event, coverage)
    }
    fn refusals(&self) -> Vec<Event> {
        let response = client::call(
            &self.state(),
            &Request::new(
                "read",
                Command::Events {
                    after: 0,
                    limit: 1000,
                },
            ),
        )
        .unwrap();
        let Outcome::Events { events } = response.outcome else {
            panic!("{:?}", response.outcome);
        };
        events
            .into_iter()
            .map(|recorded| recorded.event)
            .filter(|event| matches!(event, Event::WriteRefused { .. }))
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn the_line_counts_current_sessions_and_each_refusal_once() {
    let fixture = Fixture::new();
    // Nothing registered here yet.
    assert_eq!(
        fixture.line(),
        "langspan · 0 sessions here · 0 refusals today"
    );
    for session in [ALICE, BOB, CAROL, DAVE] {
        fixture.prompt(session);
    }
    fixture.hook(&json!({
        "session_id": DAVE, "cwd": fixture.work(),
        "hook_event_name": "SessionEnd", "reason": "exit",
    }));
    // Alice and her two peers; Dave's session ended.
    assert_eq!(
        fixture.line(),
        "langspan · 3 sessions here · 0 refusals today"
    );

    // Bob holds the file; Alice's edit is refused once.
    let (bob_event, bob) = fixture.edit(BOB);
    assert_eq!(bob, "admitted");
    let (alice_event, alice) = fixture.edit(ALICE);
    assert_eq!(alice, "denied");
    let refusals = fixture.refusals();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    match &refusals[0] {
        Event::WriteRefused {
            session_id,
            kind,
            paths,
            peer_session_id,
            ..
        } => {
            assert_eq!(session_id, &format!("claude-code.{ALICE}"));
            assert_eq!(*kind, RefusalKind::Conflict);
            assert_eq!(paths, &vec!["a.txt".to_owned()]);
            assert_eq!(peer_session_id, &Some(format!("claude-code.{BOB}")));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        fixture.line(),
        "langspan · 3 sessions here · 1 refusal today"
    );
    // The host sending the same call again is answered from its receipt
    // (as a historical conflict), and records nothing.
    assert_eq!(fixture.hook(&alice_event), "unknown");
    assert_eq!(fixture.refusals().len(), 1);
    // Bob finishes; Alice's next attempt is let through and records nothing.
    let mut bob_post = bob_event.clone();
    bob_post["hook_event_name"] = json!("PostToolUse");
    fs::write(fixture.work().join("a.txt"), "line 1\nline 2 bob\nline 3\n").unwrap();
    fixture.hook(&bob_post);
    fixture.hook(&json!({
        "session_id": ALICE, "cwd": fixture.work(),
        "hook_event_name": "PostToolUse", "tool_name": "Read",
        "tool_use_id": "read-alice",
        "tool_input": {"file_path": fixture.work().join("a.txt")},
        "tool_response": {"type": "text", "file": {
            "filePath": fixture.work().join("a.txt"),
            "content": "line 1\nline 2 bob\nline 3\n",
            "numLines": 3, "startLine": 1, "totalLines": 3,
        }},
    }));
    let (_, again) = fixture.edit_line(ALICE, "line 2 bob");
    assert_eq!(again, "admitted");
    assert_eq!(fixture.refusals().len(), 1);

    // --json, and --root without stdin, give the same counts.
    let (output, _) = status_line(
        &fixture.state(),
        &["--json", "--root", &fixture.work().to_string_lossy()],
        None,
    );
    let answer: Value = serde_json::from_str(&text(&output)).unwrap();
    assert_eq!(
        answer,
        json!({
            "workspace_id": "work", "sessions_here": 3,
            "refusals_today": 1, "coordinator": "running",
        })
    );
    // The line never names a path.
    assert!(!fixture.line().contains("a.txt"));
}

/// A client may name the folder through a symbolic link (on macOS the
/// temporary directory is one): the daemon resolves it before matching the
/// registered root, so the session there still counts.
#[test]
fn a_session_reached_through_a_symlinked_folder_counts_as_here() {
    let fixture = Fixture::new();
    fixture.prompt(ALICE);
    // The query as any client sends it, the path not resolved first.
    let ask = |root: &Path| {
        let command = Command::StatusLine {
            root: root.to_string_lossy().into_owned(),
            from_ms: 0,
        };
        match client::call(&fixture.state(), &Request::new("read", command))
            .unwrap()
            .outcome
        {
            Outcome::StatusLine { line } => line,
            other => panic!("{other:?}"),
        }
    };
    let link = fixture.base.join("link");
    std::os::unix::fs::symlink(fixture.work(), &link).unwrap();
    fs::create_dir(fixture.work().join("sub")).unwrap();
    for root in [link.clone(), link.join("sub")] {
        let line = ask(&root);
        assert_eq!(line.workspace_id.as_deref(), Some("work"), "{root:?}");
        assert_eq!(line.sessions_here, 1, "{root:?}");
    }
    // A folder that does not exist is matched as given: nothing here.
    assert_eq!(ask(&fixture.base.join("gone")).workspace_id, None);
}

#[test]
fn the_command_writes_nothing_to_the_state_directory() {
    let fixture = Fixture::new();
    fixture.prompt(ALICE);
    // The daemon's own work after the prompt (its first scan of the
    // workspace) writes for a moment; wait until the directory is quiet.
    support::settles(
        "the state directory",
        Duration::from_millis(500),
        Duration::from_secs(20),
        || listing(&fixture.state()),
    );
    let events = fixture.refusals();
    let before = listing(&fixture.state());
    for _ in 0..3 {
        fixture.line();
    }
    assert_eq!(listing(&fixture.state()), before);
    assert_eq!(fixture.refusals(), events);
}

#[test]
fn with_no_coordinator_it_prints_the_unavailable_line_within_its_deadline() {
    let base = base("statusline-down");
    let work = base.join("work");
    fs::create_dir(&work).unwrap();
    let input = INPUT.replace("__CWD__", &work.to_string_lossy());

    // No state directory at all: nothing is created.
    let state = base.join("state");
    let (output, took) = status_line(&state, &[], Some(&input));
    assert_eq!(text(&output), "langspan · coordinator not running");
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert!(
        !state.exists(),
        "the command never creates the state directory"
    );
    let (output, _) = status_line(&state, &["--json"], Some(&input));
    let answer: Value = serde_json::from_str(&text(&output)).unwrap();
    assert_eq!(answer["coordinator"], "unavailable");
    assert!(answer["sessions_here"].is_null());

    // A coordinator socket that accepts and never answers: the command gives
    // up at its deadline instead of the client's 5 s default.
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = state.join("daemon.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let before = listing(&state);
    let (output, took) = status_line(&state, &["--deadline-ms", "200"], Some(&input));
    assert_eq!(text(&output), "langspan · coordinator not running");
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert_eq!(listing(&state), before);
    drop(listener);
    // The deadline stays within the range other coordinator calls use.
    for wrong in ["50", "6000"] {
        let (output, _) = status_line(&state, &["--deadline-ms", wrong], Some(&input));
        assert_ne!(output.status.code(), Some(0));
    }
    let _ = fs::remove_dir_all(&base);
}

struct Home {
    base: PathBuf,
}

impl Home {
    fn new() -> Self {
        let base = base("statusline-home");
        fs::create_dir_all(base.join("home/.claude")).unwrap();
        Self { base }
    }
    fn settings(&self) -> PathBuf {
        self.base.join("home/.claude/settings.json")
    }
    fn langspan(&self, args: &[&str]) -> String {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(args)
            .args(["--hosts", "claude-code", "--home"])
            .arg(self.base.join("home"))
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CLAUDE_PROJECT_DIR")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
    fn read(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.settings()).unwrap()).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn install_leaves_a_status_line_of_the_persons_own_and_says_so() {
    let home = Home::new();
    let mine = "{\n  \"theme\": \"dark\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"~/.claude/my-line.sh\"\n  }\n}\n";
    fs::write(home.settings(), mine).unwrap();
    let dry = home.langspan(&["install", "--statusline", "on"]);
    assert!(
        dry.contains("Claude Code already has a status line"),
        "{dry}"
    );
    assert_eq!(fs::read_to_string(home.settings()).unwrap(), mine);
    let applied = home.langspan(&["install", "--statusline", "on", "--apply"]);
    assert!(applied.contains("already has a status line"), "{applied}");
    let settings = home.read();
    assert_eq!(
        settings["statusLine"],
        json!({"type": "command", "command": "~/.claude/my-line.sh"})
    );
    // Uninstall removes LangSpan's hooks and leaves the person's line.
    home.langspan(&["uninstall", "--apply"]);
    assert_eq!(
        home.read(),
        serde_json::from_str::<Value>(mine).unwrap(),
        "only LangSpan's entries are removed"
    );
}

#[test]
fn install_adds_the_status_line_and_uninstall_removes_only_it() {
    let home = Home::new();
    let before = json!({"theme": "dark", "model": "m"});
    fs::write(home.settings(), before.to_string()).unwrap();
    // A dry run writes nothing.
    let dry = home.langspan(&["install", "--statusline", "on"]);
    assert!(dry.contains("statusline"), "{dry}");
    assert_eq!(home.read(), before);
    home.langspan(&["install", "--statusline", "on", "--apply"]);
    let settings = home.read();
    let command = settings["statusLine"]["command"].as_str().unwrap();
    assert_eq!(settings["statusLine"]["type"], "command");
    assert!(command.contains(" statusline"), "{command}");
    assert!(command.ends_with(" # langspan-managed:claude-code:statusline:v1"));
    assert_eq!(settings["theme"], "dark");
    // The written command runs as the host runs it, and with no daemon
    // prints the unavailable line.
    let output = Process::new("/bin/sh")
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["-c", command])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(text(&output), "langspan · coordinator not running");
    // Installing again without --statusline keeps it; uninstall removes it
    // with LangSpan's hooks, and nothing of the person's.
    home.langspan(&["install", "--apply"]);
    assert_eq!(home.read()["statusLine"], settings["statusLine"]);
    home.langspan(&["uninstall", "--apply"]);
    assert_eq!(home.read(), before);
}
