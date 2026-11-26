#![cfg(unix)]
//! Desktop notices (roadmap H3) against a real daemon, real hook executables
//! and a stand-in notifier: an executable named as this platform's notifier
//! (`osascript` on macOS, `notify-send` elsewhere), first on the daemon's
//! `PATH`, which records each call's arguments. It is a real child process
//! of the daemon; no LangSpan code is replaced.

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{CheckReport, CheckResult, Command, Outcome};
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

mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);
// Distinct from their first character, as host session IDs are.
const ALICE: &str = "a11ce000-0000-4000-8000-000000000001";
const BOB: &str = "b0b00000-0000-4000-8000-000000000002";
const CAROL: &str = "ca201000-0000-4000-8000-000000000003";
/// Text a notice must never carry.
const PROMPT: &str = "secret plan for the api";
const HANDOFF_TEXT: &str = "private handoff words";

/// The argument that asks a freedesktop notification server for silence.
const SUPPRESS_SOUND: &str = "--hint=boolean:suppress-sound:true";

/// The call asks for silence where it can: `notify-send` passes the
/// `suppress-sound` hint, so a desktop with a default sound stays quiet;
/// `osascript` with no `sound name` plays none, so it carries no sound
/// argument at all.
fn assert_silent(call: &[String]) {
    if cfg!(target_os = "macos") {
        assert!(!call.iter().any(|arg| arg.contains("sound")), "{call:?}");
    } else {
        assert!(call.iter().any(|arg| arg == SUPPRESS_SOUND), "{call:?}");
        assert!(
            !call.iter().any(|arg| arg.contains("sound-name")),
            "{call:?}"
        );
    }
}

fn program() -> &'static str {
    if cfg!(target_os = "macos") {
        "osascript"
    } else {
        "notify-send"
    }
}

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
    calls: AtomicU64,
}

impl Fixture {
    /// A daemon with `config` in its `config.json` (none when `None`) and
    /// the stand-in first on its `PATH`.
    fn new(config: Option<Value>) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-notify-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        for dir in ["", "work", "bin", "calls", "state"] {
            fs::create_dir(base.join(dir)).unwrap();
            fs::set_permissions(base.join(dir), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(base.join("work/a.txt"), "line 1\nline 2\nline 3\n").unwrap();
        // Each call writes its arguments, NUL-separated, to its own file;
        // with `hang` present it then sleeps in place of itself.
        let calls = base.join("calls");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{calls}/call.'$$\nif [ -e '{calls}/hang' ]; then echo $$ > '{calls}/hung.pid'; exec sleep 30; fi\n",
            calls = calls.display()
        );
        let stand_in = base.join("bin").join(program());
        fs::write(&stand_in, script).unwrap();
        fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o700)).unwrap();
        if let Some(config) = config {
            let path = base.join("state/config.json");
            fs::write(&path, config.to_string()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let log = fs::File::create(base.join("daemon.log")).unwrap();
        let daemon = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(base.join("state"))
            .env("PATH", Self::path_with(&base.join("bin")))
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let fixture = Self {
            base,
            daemon: Some(daemon),
            calls: AtomicU64::new(0),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&fixture.state(), &status()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }
    fn path_with(dir: &Path) -> std::ffi::OsString {
        let mut dirs = vec![dir.to_owned()];
        dirs.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(dirs).unwrap()
    }
    fn state(&self) -> PathBuf {
        self.base.join("state")
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn calls_dir(&self) -> PathBuf {
        self.base.join("calls")
    }
    fn log(&self) -> Vec<Value> {
        fs::read_to_string(self.base.join("daemon.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn logged(&self, event: &str) -> Vec<Value> {
        self.log()
            .into_iter()
            .filter(|line| line["event"] == event)
            .collect()
    }
    /// Each recorded call's arguments.
    fn calls(&self) -> Vec<Vec<String>> {
        let mut calls: Vec<Vec<String>> = fs::read_dir(self.calls_dir())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("call."))
            .map(|entry| {
                let bytes = fs::read(entry.path()).unwrap();
                String::from_utf8(bytes)
                    .unwrap()
                    .split_terminator('\0')
                    .map(str::to_owned)
                    .collect()
            })
            .collect();
        calls.sort();
        calls
    }
    /// Wait until `count` calls are recorded, or the log shows `until`.
    fn wait_for(&self, count: usize, until: &str) -> Vec<Vec<String>> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let calls = self.calls();
            if calls.len() >= count || !self.logged(until).is_empty() {
                // A call still writing its file is given a moment.
                std::thread::sleep(Duration::from_millis(100));
                return self.calls();
            }
            assert!(
                Instant::now() < deadline,
                "{count} call(s) not seen; log: {:?}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn cli(&self, args: &[&str], path: &std::ffi::OsStr) -> Output {
        Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .env("PATH", path)
            .output()
            .unwrap()
    }
    fn notify(&self, args: &[&str]) -> String {
        let mut all = vec!["notify"];
        all.extend(args);
        let output = self.cli(&all, &Self::path_with(&self.base.join("bin")));
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
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
            "hook_event_name": "UserPromptSubmit", "prompt": PROMPT,
        }));
    }
    fn edit(&self, session: &str) -> String {
        self.hook(&json!({
            "session_id": session, "cwd": self.work(),
            "hook_event_name": "PreToolUse", "tool_name": "Edit",
            "tool_use_id": format!("call-{}", self.calls.fetch_add(1, Ordering::Relaxed)),
            "tool_input": {
                "file_path": self.work().join("a.txt"),
                "old_string": "line 2", "new_string": format!("line 2 {session}"),
            },
        }))
    }
    /// Bob holds `a.txt`; each of `refused` is refused on it.
    fn refuse(&self, refused: &[&str]) {
        for session in [BOB].iter().chain(refused) {
            self.prompt(session);
        }
        assert_eq!(self.edit(BOB), "admitted");
        for session in refused {
            assert_eq!(self.edit(session), "denied");
        }
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

fn status() -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Status,
    }
}

/// The notice body: the argument after the title.
fn body(call: &[String]) -> &str {
    let title = call.iter().position(|arg| arg == "LangSpan").unwrap();
    assert_eq!(call.len(), title + 2, "{call:?}");
    &call[title + 1]
}

#[test]
fn with_notices_off_no_notifier_is_started() {
    let fixture = Fixture::new(None);
    fixture.refuse(&[ALICE, CAROL]);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(fixture.calls(), Vec::<Vec<String>>::new());
    assert!(fixture.logged("notice_sent").is_empty());
    assert!(fixture.logged("notice_dropped").is_empty());
}

#[test]
fn a_refusal_starts_one_notifier_and_a_second_within_five_minutes_none() {
    let fixture = Fixture::new(None);
    // Switched on while the daemon runs: no restart is needed.
    let status = fixture.notify(&["on"]);
    assert!(status.contains("notices: on"), "{status}");
    assert!(status.contains("sound: off"), "{status}");
    fixture.refuse(&[ALICE]);
    let calls = fixture.wait_for(1, "notifier_failed");
    assert_eq!(calls.len(), 1, "{calls:?}");
    let call = &calls[0];
    let body = body(call);
    assert_eq!(
        body,
        "work: claude-code.a11ce000… refused on a.txt (held by claude-code.b0b00000…)"
    );
    assert!(body.len() <= 200);
    assert_silent(call);
    assert!(!call.iter().any(|arg| arg.contains(PROMPT)), "{call:?}");
    assert_eq!(fixture.logged("notice_sent").len(), 1);

    // Carol is refused too: counted and dropped.
    fixture.prompt(CAROL);
    assert_eq!(fixture.edit(CAROL), "denied");
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture.logged("notice_dropped").is_empty() {
        assert!(Instant::now() < deadline, "{:?}", fixture.log());
        std::thread::sleep(Duration::from_millis(20));
    }
    let dropped = fixture.logged("notice_dropped");
    assert_eq!(
        dropped[0],
        json!({"level":"info","event":"notice_dropped","kind":"refusal","reason":"kind_limit"})
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fixture.calls().len(), 1);

    // Off again: nothing more, not even a drop.
    assert!(fixture.notify(&["off"]).contains("notices: off"));
    let config: Value =
        serde_json::from_slice(&fs::read(fixture.state().join("config.json")).unwrap()).unwrap();
    assert_eq!(config, json!({"notify": false}));
    let mode = fs::metadata(fixture.state().join("config.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn sound_is_an_argument_of_the_one_call_only_when_switched_on() {
    let fixture = Fixture::new(None);
    let status = fixture.notify(&["on", "--sound", "on"]);
    assert!(status.contains("sound: on"), "{status}");
    fixture.refuse(&[ALICE]);
    let calls = fixture.wait_for(1, "notifier_failed");
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].iter().any(|arg| arg.contains("sound")),
        "{calls:?}"
    );
    assert!(
        !calls[0].iter().any(|arg| arg == SUPPRESS_SOUND),
        "{calls:?}"
    );
}

#[test]
fn with_sound_off_a_linux_notice_asks_the_desktop_for_silence() {
    let fixture = Fixture::new(None);
    let status = fixture.notify(&["on", "--sound", "off"]);
    assert!(status.contains("sound: off"), "{status}");
    fixture.refuse(&[ALICE]);
    let calls = fixture.wait_for(1, "notifier_failed");
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_silent(&calls[0]);
}

#[test]
fn a_hung_notifier_is_killed_after_two_seconds_and_the_daemon_keeps_answering() {
    let fixture = Fixture::new(Some(json!({"notify": true})));
    fs::write(fixture.calls_dir().join("hang"), "").unwrap();
    fixture.refuse(&[ALICE]);
    let pid_file = fixture.calls_dir().join("hung.pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() || fs::read_to_string(&pid_file).unwrap().trim().is_empty() {
        assert!(Instant::now() < deadline, "{:?}", fixture.log());
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid: i32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(support::running(pid));
    // While it hangs, requests are answered promptly.
    let asked = Instant::now();
    client::call(&fixture.state(), &status()).unwrap();
    assert!(asked.elapsed() < Duration::from_secs(1));
    let deadline = Instant::now() + Duration::from_secs(10);
    while support::running(pid) {
        assert!(Instant::now() < deadline, "the notifier was never killed");
        std::thread::sleep(Duration::from_millis(20));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.logged("notifier_timed_out").is_empty() {
        assert!(Instant::now() < deadline, "{:?}", fixture.log());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fixture.logged("notice_dropped")[0]["reason"],
        "notifier_timed_out"
    );
    client::call(&fixture.state(), &status()).unwrap();
}

#[test]
fn a_failed_automatic_check_and_a_waiting_handoff_each_get_a_notice() {
    let fixture = Fixture::new(Some(json!({
        "notify": true,
        "notify_kind_interval_ms": 1000,
        "notify_handoff_after_ms": 1000,
    })));
    fixture.prompt(ALICE);
    let session = format!("claude-code.{ALICE}");
    let output = fixture.cli(
        &[
            "handoff",
            &session,
            HANDOFF_TEXT,
            "src/one.rs",
            "src/two.rs",
            "src/three.rs",
            "src/four.rs",
            "--request-id",
            "handoff-1",
        ],
        &std::env::var_os("PATH").unwrap(),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let calls = fixture.wait_for(1, "notifier_failed");
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        body(&calls[0]),
        "work: handoff from claude-code.a11ce000… not taken up for 1 s (about src/four.rs, src/one.rs, src/three.rs, 1 more)"
    );
    assert!(
        !calls[0].iter().any(|arg| arg.contains(HANDOFF_TEXT)),
        "{calls:?}"
    );
    // Told once, however long it then waits.
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(fixture.calls().len(), 1);

    // An automatic check's failure, recorded as the check worker records it.
    let record = |id: &str, result: CheckResult| {
        client::call(
            &fixture.state(),
            &Request {
                version: PROTOCOL_VERSION,
                request_id: id.into(),
                command: Command::RecordCheck {
                    workspace_id: "work".into(),
                    name: "tests".into(),
                    command: "cargo test".into(),
                    tree_oid: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
                    head_oid: None,
                    inputs: Vec::new(),
                    result,
                    duration_ms: 10,
                    report: Some(Box::new(CheckReport {
                        session_ids: vec![session.clone()],
                        tail: "assertion failed".into(),
                        timed_out_s: None,
                        interface: None,
                        index_incomplete: false,
                        dependent_paths: Vec::new(),
                    })),
                    tests: None,
                    rerun: None,
                    writes_before: None,
                    command_id: None,
                },
            },
        )
        .unwrap()
    };
    let recorded = record("check-pass", CheckResult::Passed {});
    assert!(matches!(recorded.outcome, Outcome::CheckRecorded { .. }));
    let recorded = record("check-fail", CheckResult::Failed { exit_code: 101 });
    assert!(matches!(recorded.outcome, Outcome::CheckRecorded { .. }));
    let calls = fixture.wait_for(2, "notifier_failed");
    assert_eq!(calls.len(), 2, "{calls:?}");
    let bodies: Vec<&str> = calls.iter().map(|call| body(call)).collect();
    assert!(
        bodies.contains(&"work: check tests failed (exit 101) after claude-code.a11ce000…"),
        "{bodies:?}"
    );
    assert!(!calls.iter().flatten().any(|arg| arg.contains("assertion")));
    // A retry of the same record is answered from its receipt: no notice.
    record("check-fail", CheckResult::Failed { exit_code: 101 });
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(fixture.calls().len(), 2);
}

#[test]
fn status_names_the_notifier_found_or_says_none_was() {
    let fixture = Fixture::new(None);
    let found = fixture.notify(&["status"]);
    let expected = fixture.base.join("bin").join(program());
    assert!(
        found.contains(&format!("notifier: {} ({})", program(), expected.display())),
        "{found}"
    );
    assert!(found.contains("notices: off"), "{found}");
    let empty = fixture.base.join("calls");
    let output = fixture.cli(&["notify", "status", "--json"], empty.as_os_str());
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        answer,
        json!({"notify": false, "notify_sound": false, "notifier": null})
    );
    let output = fixture.cli(&["notify", "status"], empty.as_os_str());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("notifier: none found"), "{text}");
    // Unknown words and misplaced options are refused.
    for args in [
        &["notify", "maybe"][..],
        &["notify", "status", "--sound", "on"],
        &["notify", "on", "--sound", "loud"],
    ] {
        let output = fixture.cli(args, empty.as_os_str());
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
    }
    // Other settings are kept.
    fs::write(
        fixture.state().join("config.json"),
        json!({"max_clients": 8}).to_string(),
    )
    .unwrap();
    // A configuration others can read is refused, not rewritten.
    let output = fixture.cli(&["notify", "on"], empty.as_os_str());
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    fs::set_permissions(
        fixture.state().join("config.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fixture.notify(&["on", "--sound", "on"]);
    let config: Value =
        serde_json::from_slice(&fs::read(fixture.state().join("config.json")).unwrap()).unwrap();
    assert_eq!(
        config,
        json!({"max_clients": 8, "notify": true, "notify_sound": true})
    );
}
