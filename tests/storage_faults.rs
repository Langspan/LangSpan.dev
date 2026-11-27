//! Storage faults under a real coordinator (issue #339): a file-size limit,
//! a state directory made read-only while it runs, and SIGKILL during a
//! burst of writes. Each test starts a coordinator process because the
//! subject is how the operating system, SQLite's write-ahead log and the
//! process behave when storage fails, which no in-process test can show.
#![cfg(unix)]

use langspan::protocol::Request;
use langspan::store::Store;
use langspan_core::{Command, Outcome};
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod support;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The hook's budget in these tests, and the bound it must answer within:
/// far above the budget, far below a hang.
const BUDGET_MS: u64 = 1000;
const HOOK_LIMIT: Duration = Duration::from_secs(10);

struct World {
    base: PathBuf,
    daemon: Option<support::Owned>,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-storage-faults-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("work")).unwrap();
        fs::create_dir(base.join("state")).unwrap();
        fs::set_permissions(base.join("state"), fs::Permissions::from_mode(0o700)).unwrap();
        Self { base, daemon: None }
    }
    fn state(&self) -> PathBuf {
        self.base.join("state")
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn database(&self) -> PathBuf {
        self.state().join("state.sqlite3")
    }
    /// Starts the coordinator through `sh`, with `limits` run first (such as
    /// `ulimit -f`), and waits until it answers.
    fn start(&mut self, limits: &str) {
        let script = format!("{limits}\nexec \"$0\" daemon --state-dir \"$1\"");
        self.daemon = Some(support::Owned::spawn(
            Process::new("sh")
                .arg("-c")
                .arg(script)
                .arg(env!("CARGO_BIN_EXE_langspan"))
                .arg(self.state())
                .env("LANGSPAN_AUTOSTART", "off")
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        ));
        support::daemon_ready(&self.state());
    }
    /// SIGKILL: the coordinator gets no chance to close the store.
    fn kill(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            daemon.stop();
        }
    }
    fn call(&self, id: &str, command: Command) -> Result<Outcome, String> {
        langspan::client::call(&self.state(), &Request::new(id, command))
            .map(|response| response.outcome)
            .map_err(|error| error.to_string())
    }
    fn register(&self) {
        let outcome = self
            .call(
                "register-work",
                Command::WorkspaceRegister {
                    workspace_id: "work".into(),
                    root: self.work().to_str().unwrap().into(),
                },
            )
            .unwrap();
        assert!(matches!(outcome, Outcome::Workspace { .. }), "{outcome:?}");
    }
    fn remember(&self, id: &str) -> Result<Outcome, String> {
        self.call(id, note(id))
    }
    /// A Claude `PreToolUse` hook for a write in the workspace: its output
    /// and how long it took.
    fn hook(&self, tool_use_id: &str) -> (Output, Duration) {
        let event = serde_json::json!({
            "session_id": "00000000-0000-4000-8000-000000000339",
            "cwd": self.work(),
            "hook_event_name": "PreToolUse", "tool_name": "Write", "tool_use_id": tool_use_id,
            "tool_input": {"file_path": self.work().join("a.txt"), "content": "x"},
        });
        let started = Instant::now();
        let mut child = support::Owned::spawn(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["hook", "claude-code", "--workspace", "work", "--root"])
                .arg(self.work())
                .arg("--state-dir")
                .arg(self.state())
                .args(["--budget-ms", &BUDGET_MS.to_string()])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        );
        child
            .child()
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        support::wait_until("the hook to answer", HOOK_LIMIT, || {
            child.child().try_wait().unwrap().is_some()
        });
        (child.wait_with_output(), started.elapsed())
    }
    /// The note IDs the store's events record.
    fn remembered(&self) -> BTreeSet<String> {
        let connection = Connection::open(self.database()).unwrap();
        let mut statement = connection
            .prepare("SELECT json_extract(event_json, '$.note.note_id') FROM events WHERE json_extract(event_json, '$.note.note_id') IS NOT NULL")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
    /// The note IDs of the store's receipts for a note.
    fn receipted(&self) -> BTreeSet<String> {
        let connection = Connection::open(self.database()).unwrap();
        let mut statement = connection
            .prepare("SELECT json_extract(outcome_json, '$.note.note_id') FROM receipts WHERE json_extract(outcome_json, '$.note.note_id') IS NOT NULL")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
    /// Reopens the stopped store as a coordinator would: the file passes
    /// SQLite's integrity check, replays, and holds every acknowledged note
    /// with its receipt, and no receipt without its event or the reverse.
    fn reopens_with(&self, acknowledged: &BTreeSet<String>) {
        let integrity: String = Connection::open(self.database())
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        Store::open(&self.database()).unwrap();
        let remembered = self.remembered();
        assert_eq!(remembered, self.receipted(), "an event without its receipt");
        let lost: Vec<_> = acknowledged.difference(&remembered).collect();
        assert!(lost.is_empty(), "acknowledged notes lost: {lost:?}");
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.kill();
        let _ = chmod_all(&self.state(), 0o700, 0o600);
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// One note of about 4 KiB, so a few hundred writes grow the store.
fn note(id: &str) -> Command {
    Command::Remember {
        workspace_id: "work".into(),
        author: None,
        text: format!("{id} {}", "x".repeat(360)),
        paths: (0..8)
            .map(|path| format!("src/{}/{id}-{path}.rs", "d".repeat(400)))
            .collect(),
        symbol: None,
        tags: Vec::new(),
    }
}

fn note_id(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Remembered { note } => note.note_id.clone(),
        other => panic!("not a note: {other:?}"),
    }
}

fn chmod_all(dir: &Path, dir_mode: u32, file_mode: u32) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() {
            fs::set_permissions(&path, fs::Permissions::from_mode(file_mode))?;
        }
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(dir_mode))
}

/// A refused write names the store at the state directory and what to do.
#[track_caller]
fn names_the_store_and_the_fix(message: &str, state: &Path) {
    assert!(
        message.contains(&state.display().to_string()),
        "the state directory is not named: {message}"
    );
    assert!(
        message.contains("free disk space") && message.contains("writable"),
        "no remedy: {message}"
    );
}

/// The hook answered within its budget, with no decision, and said why.
#[track_caller]
fn hook_answered(output: &Output, took: Duration) -> String {
    assert!(output.status.success(), "{output:?}");
    assert!(
        took < Duration::from_millis(BUDGET_MS) + Duration::from_secs(5),
        "the hook took {took:?} on a budget of {BUDGET_MS} ms"
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_coordinator_over_its_file_size_limit_answers_the_hook_and_reopens_intact() {
    let mut world = World::new();
    world.start("");
    world.register();
    world.kill();
    // Just above the largest file now: in 512-byte blocks (POSIX `sh`), or
    // 1024 where a shell counts in KiB, which only takes more writes. The
    // signal a write past the limit raises is ignored, as it is across
    // `exec`, so the write fails with EFBIG, as on a full disk.
    let largest = fs::read_dir(world.state())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .max()
        .unwrap();
    let blocks = largest / 512 + 256;
    world.start(&format!("ulimit -f {blocks} || exit 97\ntrap '' XFSZ"));

    let mut acknowledged = BTreeSet::new();
    let refusal = (0..4000)
        .find_map(|index| match world.remember(&format!("note-{index:04}")) {
            Ok(Outcome::Error { code, message }) => Some((code, message)),
            Ok(outcome) => {
                acknowledged.insert(note_id(&outcome));
                None
            }
            Err(error) => panic!("the coordinator stopped answering: {error}"),
        })
        .expect("no write was refused over the file-size limit");
    assert_eq!(refusal.0, "storage_error", "{}", refusal.1);
    names_the_store_and_the_fix(&refusal.1, &world.state());
    assert!(
        !acknowledged.is_empty(),
        "no write succeeded under the limit"
    );

    // A hook for a write still answers within its budget, without a decision,
    // and its diagnostic says why.
    let (output, took) = world.hook("over-limit");
    let diagnostic = hook_answered(&output, took);
    assert!(output.stdout.is_empty(), "{output:?}");
    names_the_store_and_the_fix(&diagnostic, &world.state());

    world.kill();
    world.reopens_with(&acknowledged);
}

#[test]
fn a_state_directory_made_read_only_while_running_answers_the_hook_and_reopens_intact() {
    if rustix::process::geteuid().is_root() {
        println!("skipped: running as root, which file permissions do not stop");
        return;
    }
    let mut world = World::new();
    world.start("");
    world.register();
    let mut acknowledged = BTreeSet::new();
    acknowledged.insert(note_id(&world.remember("before").unwrap()));

    chmod_all(&world.state(), 0o500, 0o400).unwrap();
    // The coordinator keeps the files it has open; whatever it can no longer
    // write is refused with a reason and a remedy.
    for index in 0..20 {
        match world.remember(&format!("read-only-{index:02}")).unwrap() {
            Outcome::Error { code, message } => {
                assert_eq!(code, "storage_error", "{message}");
                names_the_store_and_the_fix(&message, &world.state());
            }
            outcome => {
                acknowledged.insert(note_id(&outcome));
            }
        }
    }
    let (output, took) = world.hook("read-only");
    hook_answered(&output, took);

    // A coordinator started in the read-only directory refuses to start and
    // says which directory and what to do.
    world.kill();
    let started = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["daemon", "--state-dir"])
        .arg(world.state())
        .env("LANGSPAN_AUTOSTART", "off")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&started.stderr);
    assert!(!started.status.success(), "started read-only: {stderr}");
    assert!(
        stderr.contains(&world.state().display().to_string()),
        "the state directory is not named: {stderr}"
    );
    assert!(stderr.contains("writable"), "no remedy: {stderr}");

    chmod_all(&world.state(), 0o700, 0o600).unwrap();
    world.reopens_with(&acknowledged);
    world.start("");
    assert!(matches!(
        world.call("status", Command::Status),
        Ok(Outcome::Status { .. })
    ));
}

#[test]
fn sigkill_during_a_burst_of_writes_loses_no_acknowledged_event() {
    let mut world = World::new();
    world.start("");
    world.register();
    let state = world.state();
    let acknowledged = Arc::new(Mutex::new(BTreeSet::new()));
    // Writers in parallel, each with its own request IDs, until the
    // coordinator is gone.
    let writers: Vec<_> = (0..4)
        .map(|writer| {
            let acknowledged = Arc::clone(&acknowledged);
            let state = state.clone();
            std::thread::spawn(move || {
                for index in 0.. {
                    let id = format!("burst-{writer}-{index:05}");
                    let reply = langspan::client::call(&state, &Request::new(&id, note(&id)));
                    match reply.map(|response| response.outcome) {
                        Ok(outcome @ Outcome::Remembered { .. }) => {
                            acknowledged.lock().unwrap().insert(note_id(&outcome));
                        }
                        Ok(other) => panic!("a write was refused: {other:?}"),
                        Err(_) => return,
                    }
                }
            })
        })
        .collect();
    support::wait_until(
        "some writes to be acknowledged",
        Duration::from_secs(30),
        || acknowledged.lock().unwrap().len() >= 40,
    );
    world.kill();
    for writer in writers {
        writer.join().unwrap();
    }
    let acknowledged = acknowledged.lock().unwrap().clone();
    world.reopens_with(&acknowledged);
    world.start("");
    assert!(matches!(
        world.call("status", Command::Status),
        Ok(Outcome::Status { .. })
    ));
}
