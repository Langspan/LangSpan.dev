#![cfg(unix)]
//! Liveness from recent hook runs (roadmap J12), with a real daemon, hook
//! runs as real child processes with constructed host events, and a real
//! process standing in for a host.

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request, Response},
};
use langspan_core::{
    Briefing, Command, Event, LivenessStatus, Outcome, ProcessIdentity, SESSION_SEEN_INTERVAL_MS,
    State, StatusSummary,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-0000000000a1";
const BOB: &str = "00000000-0000-4000-8000-0000000000b2";

struct Fixture {
    base: PathBuf,
    child: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-seen-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let root = base.join("work");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.txt"), "hello\n").unwrap();
        let mut fixture = Self { base, child: None };
        fixture.start();
        fixture
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }
    fn root(&self) -> PathBuf {
        self.base.join("work")
    }
    fn start(&mut self) {
        self.child = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&self.state_dir(), &request("ready", Command::Status)).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn call(&self, id: &str, command: Command) -> Response {
        client::call(&self.state_dir(), &request(id, command)).unwrap()
    }
    fn status(&self) -> (State, StatusSummary) {
        match self.call("read", Command::Status).outcome {
            Outcome::Status { state, summary } => (state, summary.unwrap()),
            other => panic!("{other:?}"),
        }
    }
    /// What Bob would be told now, without delivering it.
    fn bobs_briefing(&self) -> Briefing {
        // A new request ID each time: a repeated one is answered from its receipt.
        let id = format!("look-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        match self
            .call(
                &id,
                Command::Brief {
                    session_id: format!("claude-code.{BOB}"),
                    max_peers: 8,
                    deliver: false,
                    mid_turn: false,
                    left: Vec::new(),
                },
            )
            .outcome
        {
            Outcome::Briefing { briefing } => briefing,
            other => panic!("{other:?}"),
        }
    }
    /// One Claude Code hook run, as the host would start it.
    fn hook(&self, event: &Value) -> String {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state_dir())
            .args(["--workspace", "work", "--root"])
            .arg(self.root())
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
    fn prompt(&self, session: &str, text: &str) {
        self.hook(&json!({
            "session_id": session, "transcript_path": "/dev/null", "cwd": self.root(),
            "hook_event_name": "UserPromptSubmit", "prompt": text,
        }));
    }
    fn read(&self, session: &str, call: &str) -> String {
        self.hook(&json!({
            "session_id": session, "cwd": self.root(), "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_use_id": call,
            "tool_input": {"file_path": self.root().join("a.txt")},
            "tool_response": {"type": "text"},
        }))
    }
    fn sightings(&self, session: &str) -> usize {
        let mut after = 0;
        let mut count = 0;
        loop {
            let events = match self
                .call("events", Command::Events { after, limit: 100 })
                .outcome
            {
                Outcome::Events { events } => events,
                other => panic!("{other:?}"),
            };
            let Some(last) = events.last() else {
                return count;
            };
            after = last.seq;
            count += events
                .iter()
                .filter(|recorded| {
                    matches!(&recorded.event, Event::SessionSeen { session_id, .. }
                        if session_id == session)
                })
                .count();
        }
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

fn request(id: &str, command: Command) -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: id.into(),
        command,
    }
}

fn lists(briefing: &Briefing, session: &str) -> bool {
    briefing.peers.iter().any(|peer| peer.session_id == session)
}

#[test]
fn a_burst_of_one_hundred_hook_runs_records_at_most_one_sighting_per_interval() {
    let f = Fixture::new();
    let alice = format!("claude-code.{ALICE}");
    let started = Instant::now();
    for call in 0..100 {
        assert_eq!(f.read(ALICE, &format!("read-{call}")), "read_recorded");
    }
    let elapsed = started.elapsed().as_millis() as u64;
    let seen = f.sightings(&alice);
    assert!(seen >= 1, "a hook run that reached the daemon is recorded");
    assert!(
        seen as u64 <= 1 + elapsed / SESSION_SEEN_INTERVAL_MS,
        "{seen} sightings in {elapsed} ms"
    );
    let (_, summary) = f.status();
    let age = summary.last_hook_age_ms[&alice];
    assert!(age <= elapsed + 5_000, "{age}");
}

#[test]
fn a_host_process_that_exits_drops_its_session_from_a_peers_briefing() {
    let f = Fixture::new();
    let alice = format!("claude-code.{ALICE}");
    f.prompt(ALICE, "Trace the upload failure");
    f.prompt(BOB, "Fix the upload handler");
    let mut host = Process::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let attached = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["session", "attach-pid", &alice])
        .arg(host.id().to_string())
        .args(["--request-id", "attach-host", "--state-dir"])
        .arg(f.state_dir())
        .output()
        .unwrap();
    if !attached.status.success() {
        let _ = host.kill();
        let _ = host.wait();
        panic!("{}", String::from_utf8_lossy(&attached.stderr));
    }
    assert!(lists(&f.bobs_briefing(), &alice));
    let leases_before = f.status().0.leases;
    host.kill().unwrap();
    host.wait().unwrap();
    // The daemon probes about once a second.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (state, _) = f.status();
        if state.session_liveness[&alice].status == LivenessStatus::Exited {
            assert_eq!(state.leases, leases_before, "liveness changes no admission");
            break;
        }
        assert!(Instant::now() < deadline, "the exit was not observed");
        std::thread::sleep(Duration::from_millis(100));
    }
    let briefing = f.bobs_briefing();
    assert!(!lists(&briefing, &alice), "{briefing:?}");
    // A later hook run does not bring the exited incarnation back.
    f.read(ALICE, "after-exit");
    let briefing = f.bobs_briefing();
    assert!(!lists(&briefing, &alice), "{briefing:?}");
}

#[test]
fn a_probe_of_another_users_process_is_recorded_as_unknown() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let f = Fixture::new();
    let alice = format!("claude-code.{ALICE}");
    f.prompt(ALICE, "Trace the upload failure");
    // Process 1 is the superuser's; the client's claim of "alive" is not
    // taken, and the daemon's own probe of it is refused.
    let response = f.call(
        "theirs",
        Command::ObserveSessionLiveness {
            session_id: alice.clone(),
            process: ProcessIdentity {
                pid: 1,
                start_token: "0".repeat(64),
            },
            status: LivenessStatus::Alive,
        },
    );
    assert!(
        matches!(&response.outcome, Outcome::SessionLiveness { liveness }
            if liveness.status == LivenessStatus::Unknown),
        "{:?}",
        response.outcome
    );
    let (state, _) = f.status();
    assert_eq!(
        state.session_liveness[&alice].status,
        LivenessStatus::Unknown
    );
    // Unknown is not exited: the session is still in a peer's briefing.
    f.prompt(BOB, "Fix the upload handler");
    assert!(lists(&f.bobs_briefing(), &alice));
}

#[test]
fn status_shows_the_last_hook_age_and_the_probe_per_session() {
    let f = Fixture::new();
    f.prompt(ALICE, "Trace the upload failure");
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["status", "--state-dir"])
        .arg(f.state_dir())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("Last hook run: "), "{text}");
    assert!(!text.contains("none recorded"), "{text}");
    let json = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .args(["status", "--json", "--state-dir"])
        .arg(f.state_dir())
        .output()
        .unwrap();
    let reply: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(
        reply["outcome"]["summary"]["last_hook_age_ms"][format!("claude-code.{ALICE}")].is_u64(),
        "{reply}"
    );
}
