#![cfg(unix)]
//! Golden hook replies (issue #326). Every host event recorded under
//! `fixtures/` goes through the real `langspan hook` to a coordinator in one
//! fixed state (a new daemon, no other session, and a repository whose files
//! and commit are the same on every run), and its exit code, stdout and
//! stderr are pinned in `tests/golden/hook/`. A new fixture fails here until
//! its reply is pinned; `tests/support/golden.rs` says how to refresh one.
//! Also pinned: the briefing text for a fixed input
//! (`tests/golden/briefing/`), how the hook reads stdin, and how it turns a
//! shell command into the files it holds.

#[path = "support/golden.rs"]
mod golden;
mod support;

use langspan::hook::{render_briefing, render_code_briefing};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
/// The peer session of the fixed state; no recorded event uses this ID.
const PEER: &str = "00000000-0000-4000-8000-0000000000ee";
/// The placeholder roots the recorded events name (each folder's README).
const PLACEHOLDER_ROOTS: [&str; 2] = ["/work/demo", "/work/repo"];
/// The fixed repository: its files and one commit.
const FILES: [(&str, &str); 6] = [
    ("a.txt", "hello\n"),
    ("README.md", "# demo\n"),
    ("calc.py", "def add(a, b):\n    return a + b\n"),
    ("main.py", "from calc import add\n\nprint(add(1, 2))\n"),
    ("notes.txt", "first\n"),
    (
        "src/lib.rs",
        "pub fn greet() -> &'static str {\n    \"hello\"\n}\n",
    ),
];

/// A coordinator in the fixed state, with its repository and home.
struct Fixture {
    daemon: Option<support::Owned>,
    base: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-replies-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        // Owned before anything can panic, so `Drop` removes it.
        let mut fixture = Self { daemon: None, base };
        let root = fixture.root();
        fs::create_dir_all(root.join("src")).unwrap();
        for (path, text) in FILES {
            fs::write(root.join(path), text).unwrap();
        }
        git(&root, &["init", "--quiet", "-b", "main"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "--quiet", "-m", "init"]);
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_langspan"));
        support::in_home(&mut daemon, &fixture.base.join("home"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(fixture.state())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        fixture.daemon = Some(support::Owned::spawn(&mut daemon));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fixture.state().join("daemon.sock").exists() {
            assert!(Instant::now() < deadline, "the daemon did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture.peer();
        // The daemon observes git in the background; the fixed state has
        // the branch recorded, so a reply does not depend on that timing.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = langspan::client::full_state(&fixture.state()).unwrap();
            if state
                .workspace_git
                .values()
                .any(|git| git.branch_ref.is_some())
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon did not record the branch"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }
    /// The one other session of the fixed state: a Claude Code session
    /// that said its task and edited `a.txt`, through the hook as a host
    /// would.
    fn peer(&self) {
        let event = |name: &str, extra: Value| {
            let mut event = serde_json::json!({
                "session_id": PEER,
                "transcript_path": "/dev/null",
                "cwd": self.root(),
                "permission_mode": "default",
                "hook_event_name": name,
            });
            event
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            event.to_string()
        };
        let edit = serde_json::json!({
            "tool_name": "Edit",
            "tool_use_id": "toolu_peer_edit",
            "tool_input": {
                "file_path": self.root().join("a.txt"),
                "old_string": "hello",
                "new_string": "hello world",
                "replace_all": false
            },
        });
        let prompt = serde_json::json!({"prompt": "task: greet the world from a.txt"});
        for (name, extra) in [("UserPromptSubmit", prompt), ("PreToolUse", edit.clone())] {
            self.reply("claude-code", None, &event(name, extra));
        }
        fs::write(self.root().join("a.txt"), "hello world\n").unwrap();
        self.reply("claude-code", None, &event("PostToolUse", edit));
    }
    fn root(&self) -> PathBuf {
        self.base.join("work")
    }
    fn state(&self) -> PathBuf {
        self.base.join("state")
    }
    /// The hook for `host`, its stdin given to `feed`, as a transcript with
    /// this run's paths replaced by fixed names.
    fn hook(
        &self,
        host: &str,
        args: &[&str],
        feed: impl FnOnce(std::process::ChildStdin),
    ) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_langspan"));
        support::in_home(&mut command, &self.base.join("home"))
            .env("LANGSPAN_AUTOSTART", "off")
            .env_remove("LANGSPAN_CAPTURE_DIR")
            .env_remove("CLAUDE_CODE_SESSION_ATTENDED")
            .args(["hook", host])
            .args(args)
            .arg("--state-dir")
            .arg(self.state())
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = support::Owned::spawn(&mut command);
        feed(child.child().stdin.take().unwrap());
        let output = child.wait_with_output();
        let text = format!(
            "exit: {:?}\n--- stdout\n{}\n--- stderr\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let text = text
            .replace(self.root().to_str().unwrap(), "<root>")
            .replace(self.state().to_str().unwrap(), "<state>")
            .replace(self.base.to_str().unwrap(), "<base>");
        without_timings(&text)
    }
    /// The hook's reply to one recorded event, sent whole.
    fn reply(&self, host: &str, event: Option<&str>, body: &str) -> String {
        let mut body = body.to_owned();
        for placeholder in PLACEHOLDER_ROOTS {
            body = body.replace(placeholder, self.root().to_str().unwrap());
        }
        let args: Vec<&str> = event.map(|e| vec!["--event", e]).unwrap_or_default();
        self.hook(host, &args, |mut stdin| {
            stdin.write_all(body.as_bytes()).unwrap();
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            daemon.stop();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// `text` with each time the run measured, `(21 ms)` or `3s ago`, written
/// with `N` for the number.
fn without_timings(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(|c: char| c.is_ascii_digit()) {
        out.push_str(&rest[..at]);
        let digits = rest[at..].len()
            - rest[at..]
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .len();
        let after = &rest[at + digits..];
        let measured = [" ms)", "s ago", "m ago"]
            .iter()
            .any(|unit| after.starts_with(unit));
        let word = rest[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '-');
        out.push_str(if measured && !word {
            "N"
        } else {
            &rest[at..at + digits]
        });
        rest = after;
    }
    out.push_str(rest);
    out
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// The host whose events a folder of `fixtures/` holds.
fn host(folder: &str) -> &'static str {
    ["claude-code", "codex", "cursor", "antigravity"]
        .into_iter()
        .find(|host| folder.starts_with(&format!("{host}-")))
        .unwrap_or_else(|| panic!("fixtures/{folder}: name its host here"))
}

/// The event name the installed Antigravity command passes: the event's own
/// `hookEventName`, or for a constructed file the one its README's table
/// gives by file name.
fn antigravity_event(stem: &str, body: &Value) -> String {
    if let Some(name) = body["hookEventName"].as_str() {
        return name.into();
    }
    match stem.split('-').next().unwrap() {
        "session" => "SessionStart",
        "pre" if stem == "pre-invocation" => "PreInvocation",
        "pre" => "PreToolUse",
        "post" => "PostToolUse",
        other => panic!("antigravity-constructed/{stem}: no event for {other}"),
    }
    .into()
}

/// Every recorded event of `fixtures/<folder>`: its golden name and body.
/// A `.jsonl` file holds one event per line, numbered from 1.
fn events(folder: &str) -> Vec<(String, String)> {
    let mut files: Vec<PathBuf> = fs::read_dir(fixtures().join(folder))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    let mut events = Vec::new();
    for path in files {
        let stem = path.file_stem().unwrap().to_str().unwrap().to_owned();
        let text = fs::read_to_string(&path).unwrap();
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => events.push((stem, text)),
            Some("jsonl") => events.extend(
                text.lines()
                    .enumerate()
                    .map(|(i, line)| (format!("{stem}-{:02}", i + 1), line.to_owned())),
            ),
            Some("md") => {}
            _ => panic!("{}: not an event file this test knows", path.display()),
        }
    }
    events
}

/// The golden replies of every recorded event of one folder, each against
/// a new coordinator in the fixed state, in `tests/golden/hook/<folder>.txt`
/// under a heading per event.
fn pin_folder(folder: &str) {
    let host = host(folder);
    let events = events(folder);
    assert!(!events.is_empty(), "fixtures/{folder} holds no event");
    let mut transcript = String::new();
    for (name, body) in &events {
        let parsed: Value = serde_json::from_str(body).unwrap();
        let event = (host == "antigravity").then(|| antigravity_event(name, &parsed));
        let reply = Fixture::new().reply(host, event.as_deref(), body);
        let under = event.map(|e| format!(" as {e}")).unwrap_or_default();
        transcript.push_str(&format!("=== {name}{under}\n{reply}"));
    }
    golden::check(&format!("hook/{folder}.txt"), &transcript);
}

macro_rules! folders {
    ($($test:ident => $folder:literal,)*) => {
        $(#[test] fn $test() { pin_folder($folder); })*
        const FOLDERS: &[&str] = &[$($folder),*];
    };
}

folders! {
    antigravity_1_5_14 => "antigravity-1.5.14",
    antigravity_constructed => "antigravity-constructed",
    claude_code_2_1_330 => "claude-code-2.1.330",
    claude_code_2_1_331 => "claude-code-2.1.331",
    codex_0_171_0 => "codex-0.171.0",
    codex_2026_11_26 => "codex-2026-11-26",
    codex_contract => "codex-contract",
    cursor_constructed => "cursor-constructed",
}

/// The folders of `fixtures/` that hold no host's hook events.
const OTHER_FOLDERS: [&str; 1] = [
    // Protocol capability vectors, pinned in `tests/protocol.rs`.
    "capabilities",
];

/// The goldens in `tests/golden/hook/` that are not a fixtures folder's.
const OTHER_GOLDENS: [&str; 3] = [
    "shell-write-targets.txt",
    "stdin-left-open.txt",
    "stdin-over-8-mib.txt",
];

/// Every folder of recorded events has a test above, and every golden
/// reply file still has its folder.
#[test]
fn every_recorded_event_has_a_golden_reply_and_no_reply_outlives_its_event() {
    let names = |dir: PathBuf, keep: fn(&Path) -> bool| {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| keep(path))
            .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    };
    let mut folders: Vec<&str> = FOLDERS.iter().copied().chain(OTHER_FOLDERS).collect();
    folders.sort();
    assert_eq!(
        names(fixtures(), Path::is_dir),
        folders,
        "add each new fixtures folder of hook events to `folders!`, any other to `OTHER_FOLDERS`"
    );
    let goldens = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/hook");
    let mut expected: Vec<String> = FOLDERS
        .iter()
        .map(|folder| format!("{folder}.txt"))
        .collect();
    expected.extend(OTHER_GOLDENS.map(String::from));
    expected.sort();
    assert_eq!(
        names(goldens, Path::is_file),
        expected,
        "a golden reply with no test; delete it"
    );
}

/// `render_briefing` and `render_code_briefing` on a fixed input. The input
/// carries peer text shaped like instructions, which stays quoted data.
#[test]
fn briefing_text_for_a_fixed_input() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/briefing");
    let briefing: langspan_core::Briefing =
        serde_json::from_str(&fs::read_to_string(dir.join("briefing.input.json")).unwrap())
            .unwrap();
    golden::check(
        "briefing/briefing.txt",
        &format!("{}\n", render_briefing(&briefing)),
    );
    let code: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("code.input.json")).unwrap()).unwrap();
    let matches: Vec<langspan_core::NameContext> =
        serde_json::from_value(code["matches"].clone()).unwrap();
    let coverage: langspan_core::GraphCoverage =
        serde_json::from_value(code["coverage"].clone()).unwrap();
    let whole = render_code_briefing(&matches, &coverage, 2400).expect("a code briefing");
    // A limit with room for one name counts the other.
    let cut = render_code_briefing(&matches, &coverage, whole.len() - 40).expect("one name");
    golden::check(
        "briefing/code.txt",
        &format!(
            "=== within 2400 bytes\n{whole}\n=== within {} bytes\n{cut}\n",
            whole.len() - 40
        ),
    );
}

const PRE_EDIT: &str = include_str!("../fixtures/claude-code-2.1.331/pre-edit.json");

/// `read_stdin_until`: an event written in pieces, with pauses, is read
/// whole, and gets the reply the same event gets sent at once.
#[test]
fn an_event_sent_in_pieces_is_read_whole() {
    let fixture = Fixture::new();
    let body = PRE_EDIT.replace("/work/demo", fixture.root().to_str().unwrap());
    let in_pieces = fixture.hook("claude-code", &[], |mut stdin| {
        for piece in body.as_bytes().chunks(body.len() / 3 + 1) {
            stdin.write_all(piece).unwrap();
            stdin.flush().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    assert!(in_pieces.contains("hookSpecificOutput"), "{in_pieces}");
    assert_eq!(
        in_pieces,
        Fixture::new().reply("claude-code", None, PRE_EDIT)
    );
}

/// `read_stdin_until`: a host that never closes stdin gets an answer when
/// the hook's budget runs out, not a hook that waits with it.
#[test]
fn stdin_left_open_is_answered_when_the_budget_runs_out() {
    let fixture = Fixture::new();
    let started = Instant::now();
    let mut held = None;
    let reply = fixture.hook("claude-code", &["--budget-ms", "300"], |stdin| {
        held = Some(stdin)
    });
    // Far from the 300 ms budget, and far below a hook that never returns.
    assert!(started.elapsed() < Duration::from_secs(20), "{reply}");
    drop(held);
    golden::check("hook/stdin-left-open.txt", &reply);
}

/// `read_stdin_until`: an event over 8 MiB is refused without being read
/// to its end.
#[test]
fn an_event_over_eight_mib_is_refused() {
    let fixture = Fixture::new();
    let reply = fixture.hook("claude-code", &[], |mut stdin| {
        let chunk = vec![b' '; 1024 * 1024];
        // The hook stops reading at the limit and closes the pipe, so a
        // write past it may fail; the reply is what is checked.
        for _ in 0..9 {
            if stdin.write_all(&chunk).is_err() {
                break;
            }
        }
    });
    golden::check("hook/stdin-over-8-mib.txt", &reply);
}

/// `shell_keys`: the files a shell command writes, as the reply and the
/// lease show them. Targets are distinct and sorted; one outside the
/// workspace, a directory or a command it cannot parse holds nothing, and
/// the command's writes are compared after it runs instead.
#[test]
fn a_shell_commands_write_targets_are_the_files_it_holds() {
    let cases: [(&str, &str, &[&str]); 4] = [
        (
            "targets",
            "printf x > notes.txt; printf y > main.py; printf z >> notes.txt",
            &["main.py", "notes.txt"],
        ),
        ("outside", "printf x > /tmp/langspan-outside.txt", &[]),
        ("directory", "printf x > src", &[]),
        ("unsupported", "git status", &[]),
    ];
    let mut transcript = String::new();
    for (name, command, held) in cases {
        let fixture = Fixture::new();
        let event = serde_json::json!({
            "session_id": "00000000-0000-4000-8000-00000000000a",
            "transcript_path": "/dev/null",
            "cwd": fixture.root(),
            "permission_mode": "default",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_use_id": format!("toolu_{name}"),
        });
        let reply = fixture.reply("claude-code", None, &event.to_string());
        transcript.push_str(&format!("=== {name}: {command}\n{reply}"));
        let state = langspan::client::full_state(&fixture.state()).unwrap();
        let leases = state
            .leases
            .values()
            .filter(|lease| !lease.session_id.ends_with(PEER));
        let mut paths: Vec<&str> = leases
            .flat_map(|lease| lease.paths.iter().map(String::as_str))
            .collect();
        paths.sort();
        assert_eq!(paths, held, "{name}: {reply}");
    }
    golden::check("hook/shell-write-targets.txt", &transcript);
}
