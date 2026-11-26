#![cfg(unix)]
//! Observations the coordinator never got, kept and sent again at the next
//! hook run that reaches it (ADR 0014). A real daemon that is really stopped
//! or stopped answering, the real hook executable and a real Git repository.

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
    child: Option<Child>,
}

#[derive(Debug)]
struct Run {
    coverage: String,
    detail: String,
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
                "ls-spool-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let work = base.join("work");
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
        let mut fixture = Self { base, child: None };
        fixture.start();
        fixture
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn start(&mut self) {
        assert!(self.child.is_none());
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(self.state())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.cli(&["status", "--json"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The daemon really stops: the process has exited and its socket is gone.
    fn stop(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let _ = fs::remove_file(self.state().join("daemon.sock"));
    }

    /// Send the daemon a signal, such as `STOP` or `CONT`.
    fn signal(&self, signal: &str) {
        let pid = self.child.as_ref().unwrap().id().to_string();
        let status = Process::new("kill")
            .args([&format!("-{signal}"), &pid])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }

    fn events(&self) -> Vec<Value> {
        let (code, text) = self.cli(&["events", "--limit", "1000", "--json"]);
        assert_eq!(code, 0, "{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        value["outcome"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["event"].clone())
            .collect()
    }

    /// The recorded observations of tool calls by `session` of `path`.
    fn observations(&self, session: &str, path: &str) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|event| event["type"] == "operation_observed")
            .map(|event| event["observation"].clone())
            .filter(|observation| {
                observation["session_id"] == format!("claude-code.{session}")
                    && observation["paths"] == json!([path])
            })
            .collect()
    }

    fn spool(&self) -> Vec<Value> {
        let Ok(entries) = fs::read_dir(self.state().join("spool")) else {
            return Vec::new();
        };
        entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
            .collect()
    }

    fn run(&self, event: Value, budget_ms: u64) -> Run {
        self.run_with(&[], event, budget_ms)
    }

    /// A hook run with the install's own options, such as `--presence off`.
    fn run_with(&self, extra: &[&str], event: Value, budget_ms: u64) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "work", "--root"])
            .arg(self.work())
            .args(["--budget-ms", &budget_ms.to_string()])
            .args(extra)
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
        let reply: Option<Value> = serde_json::from_str(&stdout).ok();
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            detail: diagnostic["detail"].as_str().unwrap_or_default().into(),
            context: reply
                .as_ref()
                .and_then(|reply| reply["hookSpecificOutput"]["additionalContext"].as_str())
                .map(str::to_owned),
        }
    }

    /// A Claude Code `Write` hook event of `file`.
    fn write(&self, session: &str, phase: &str, call: &str, file: &str) -> Run {
        self.run(
            json!({
                "session_id": session, "cwd": self.work(), "hook_event_name": phase,
                "tool_name": "Write", "tool_use_id": call,
                "tool_input": {"file_path": self.work().join(file), "content": "x\n"},
            }),
            2_000,
        )
    }

    fn prompt(&self, session: &str) -> Run {
        self.run(
            json!({"session_id": session, "cwd": self.work(),
                   "hook_event_name": "UserPromptSubmit", "prompt": "continue"}),
            2_000,
        )
    }

    /// Alice's whole `Write` of `file`, whose PostToolUse finds the daemon
    /// really stopped. The daemon is started again afterwards.
    fn write_while_stopped(&mut self, call: &str, file: &str) {
        assert_eq!(
            self.write(ALICE, "PreToolUse", call, file).coverage,
            "admitted"
        );
        fs::write(self.work().join(file), format!("{call}\n")).unwrap();
        self.stop();
        let down = self.write(ALICE, "PostToolUse", call, file);
        assert_eq!(down.coverage, "unavailable", "{down:?}");
        assert!(down.detail.contains("kept to send again"), "{down:?}");
        self.start();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = Process::new("kill")
                .args(["-CONT", &child.id().to_string()])
                .status();
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn an_edit_finished_while_the_coordinator_was_down_is_recorded_once_at_the_next_hook_run() {
    let mut f = Fixture::new();
    f.write_while_stopped("alice-1", "b.txt");
    assert_eq!(f.spool().len(), 1, "the observation is kept");
    let mode = fs::metadata(f.state().join("spool"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    assert!(f.observations(ALICE, "b.txt").is_empty());

    // The daemon restarted, so Alice's session and admission are from an
    // earlier generation. Bob's next hook run, about another file, sends
    // Alice's observation first.
    let next = f.write(BOB, "PreToolUse", "bob-1", "c.txt");
    assert_eq!(next.coverage, "admitted", "{next:?}");
    assert!(
        next.detail.contains("1 earlier observation(s) sent again"),
        "{next:?}"
    );
    assert!(f.spool().is_empty(), "nothing is owed any more");
    let recorded = f.observations(ALICE, "b.txt");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    // Recorded as history: the restart had already ended the admission.
    assert_eq!(recorded[0]["lease_relation"], "invalidated");

    // Further hook runs send nothing again, and nothing is recorded twice.
    let again = f.write(BOB, "PreToolUse", "bob-2", "d.txt");
    assert!(!again.detail.contains("sent again"), "{again:?}");
    assert_eq!(f.observations(ALICE, "b.txt").len(), 1);
    // History, not presence: a briefing names only what sessions did since
    // the restart, so Bob is not told about b.txt.
    let context = f.prompt(BOB).context.unwrap_or_default();
    assert!(!context.contains(r#""b.txt""#), "{context}");
    // Her written version was not recorded (her session was from before the
    // restart), so her own next edit of b.txt is admitted uncompared rather
    // than refused as stale.
    let own = f.write(ALICE, "PreToolUse", "alice-2", "b.txt");
    assert_eq!(own.coverage, "admitted", "{own:?}");
    assert!(own.detail.contains("not compared for 1 file"), "{own:?}");
}

#[test]
fn an_observation_older_than_an_hour_is_dropped_unsent() {
    let mut f = Fixture::new();
    f.write_while_stopped("alice-1", "b.txt");
    let spool = f.state().join("spool");
    let path = fs::read_dir(&spool)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut entry: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let hour_and_a_bit = 3_600_000 + 60_000;
    for clock in ["created_ms", "created_wall_ms"] {
        let at = entry[clock].as_u64().unwrap();
        entry[clock] = json!(at.saturating_sub(hour_and_a_bit));
    }
    fs::write(&path, entry.to_string()).unwrap();

    let next = f.write(BOB, "PreToolUse", "bob-1", "c.txt");
    assert_eq!(next.coverage, "admitted", "{next:?}");
    assert!(next.detail.contains("1 too old to send"), "{next:?}");
    assert!(f.spool().is_empty(), "the old entry is dropped");
    assert!(f.observations(ALICE, "b.txt").is_empty(), "and never sent");
}

#[test]
fn an_entry_is_tried_three_times_at_most_and_each_try_is_counted_before_sending() {
    let mut f = Fixture::new();
    f.write_while_stopped("alice-1", "b.txt");
    // The daemon accepts connections but stops answering: each hook run
    // spends one try on the entry and stops there.
    f.signal("STOP");
    for tries in 1..=3 {
        let run = f.run(
            json!({
                "session_id": BOB, "cwd": f.work(), "hook_event_name": "PreToolUse",
                "tool_name": "Write", "tool_use_id": format!("bob-{tries}"),
                "tool_input": {"file_path": f.work().join("c.txt"), "content": "x\n"},
            }),
            400,
        );
        assert_eq!(run.coverage, "timeout", "{run:?}");
        let spool = f.spool();
        if tries < 3 {
            assert_eq!(spool.len(), 1, "after try {tries}");
            assert_eq!(spool[0]["tries"], tries, "after try {tries}");
        } else {
            assert!(spool.is_empty(), "its last try spent, it is owed no longer");
        }
    }
    f.signal("CONT");
    // Whatever the resumed daemon makes of the requests that waited for it,
    // the observation is never recorded twice.
    let (code, _) = f.cli(&["status", "--json"]);
    assert_eq!(code, 0);
    assert!(f.observations(ALICE, "b.txt").len() <= 1);
    assert!(f.spool().is_empty(), "and it is not sent again");
}

#[test]
fn a_spooled_report_superseded_by_a_live_one_is_dropped_and_not_sent_again() {
    let mut f = Fixture::new();
    assert_eq!(
        f.write(ALICE, "PreToolUse", "alice-1", "b.txt").coverage,
        "admitted"
    );
    fs::write(f.work().join("b.txt"), "one\n").unwrap();
    f.stop();
    assert_eq!(
        f.write(ALICE, "PostToolUse", "alice-1", "b.txt").coverage,
        "unavailable"
    );
    // A second write of b.txt and its report, both while the daemon is down:
    // the newer observation of the same file replaces the older one.
    f.write(ALICE, "PreToolUse", "alice-2", "b.txt");
    fs::write(f.work().join("b.txt"), "two\n").unwrap();
    f.write(ALICE, "PostToolUse", "alice-2", "b.txt");
    assert_eq!(f.spool().len(), 1, "{:?}", f.spool());
    // A third write; its report is the first hook run that reaches the
    // daemon, and it reports b.txt live.
    f.write(ALICE, "PreToolUse", "alice-3", "b.txt");
    fs::write(f.work().join("b.txt"), "three\n").unwrap();
    f.start();
    let live = f.write(ALICE, "PostToolUse", "alice-3", "b.txt");
    assert!(
        live.coverage.starts_with("observed") || live.coverage == "late_observation",
        "{live:?}"
    );
    assert!(live.detail.contains("1 superseded"), "{live:?}");
    assert!(f.spool().is_empty());
    let recorded = f.observations(ALICE, "b.txt");
    assert_eq!(recorded.len(), 1, "only the live report: {recorded:?}");
}

#[test]
fn a_shell_commands_changes_found_while_the_coordinator_was_down_are_recorded_once() {
    let mut f = Fixture::new();
    let bash = |f: &Fixture, phase: &str| {
        f.run(
            json!({
                "session_id": ALICE, "cwd": f.work(), "hook_event_name": phase,
                "tool_name": "Bash", "tool_use_id": "alice-sh",
                "tool_input": {"command": "for f in e.txt; do printf 1 > $f; done"},
                "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
            }),
            4_000,
        )
    };
    assert_eq!(bash(&f, "PreToolUse").coverage, "shell_observing");
    fs::write(f.work().join("e.txt"), "1").unwrap();
    f.stop();
    let down = bash(&f, "PostToolUse");
    assert_eq!(down.coverage, "unavailable", "{down:?}");
    assert!(down.detail.contains("kept to send again"), "{down:?}");
    f.start();
    let next = f.write(BOB, "PreToolUse", "bob-1", "c.txt");
    assert!(
        next.detail.contains("1 earlier observation(s) sent again"),
        "{next:?}"
    );
    assert_eq!(f.observations(ALICE, "e.txt").len(), 1);
    f.write(BOB, "PreToolUse", "bob-2", "d.txt");
    assert_eq!(f.observations(ALICE, "e.txt").len(), 1, "recorded once");
}

/// A read kept while the coordinator was down keeps its own session's
/// presence setting: a session with presence off whose report is sent again
/// by another session's hook run, with the default setting, is not shown as
/// looking at what it read, and the version it read is still recorded.
#[test]
fn a_kept_read_is_sent_again_with_its_own_sessions_presence_setting() {
    let mut f = Fixture::new();
    f.stop();
    let down = f.run_with(
        &["--presence", "off"],
        json!({
            "session_id": ALICE, "cwd": f.work(), "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_use_id": "alice-read",
            "tool_input": {"file_path": f.work().join("a.txt")},
            "tool_response": {"type": "text", "file": {"filePath": f.work().join("a.txt"), "content": "a line\n"}},
        }),
        2_000,
    );
    assert_eq!(down.coverage, "unavailable", "{down:?}");
    assert!(down.detail.contains("kept to send again"), "{down:?}");
    f.start();
    let next = f.write(BOB, "PreToolUse", "bob-1", "c.txt");
    assert!(
        next.detail.contains("1 earlier observation(s) sent again"),
        "{next:?}"
    );
    let (code, text) = f.cli(&["status", "--json"]);
    assert_eq!(code, 0, "{text}");
    let status: Value = serde_json::from_str(&text).unwrap();
    let state = &status["outcome"]["state"];
    let alice = format!("claude-code.{ALICE}");
    assert_eq!(state["presence"][&alice]["shown"], json!(false), "{state}");
    let activity = &state["activity"][&alice];
    assert!(
        activity["looking"]
            .as_array()
            .is_none_or(|looking| looking.is_empty()),
        "{activity}"
    );
    assert!(
        activity["known"]
            .as_array()
            .unwrap()
            .iter()
            .any(|known| known["path"] == "a.txt"),
        "{activity}"
    );
    let context = f.prompt(BOB).context.unwrap_or_default();
    assert!(!context.contains("looking at"), "{context}");
}
