#![cfg(unix)]
//! `langspan count` (roadmap H5) through the real CLI, hooks, daemon and
//! local page, in a real Git repository. Refusals are made by constructed
//! Claude Code hook events. The month boundary is real history moved in
//! time: the first daemon run's clock anchor is rewritten, with the daemon
//! stopped, so that run's refusals fall on 2027-04-01 at 02:30 UTC, which
//! is April in UTC and still March in New York.

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use langspan::protocol::{PROTOCOL_VERSION, Request};
use langspan_core::{ClockAnchor, Command, Event, Outcome, RefusalKind};

static NEXT: AtomicU64 = AtomicU64::new(0);
/// 2027-04-01T02:30:00Z: April in UTC, 22:30 on March 31 in New York.
const APRIL_1_EARLY: u64 = 1_806_546_600_000;

struct World {
    base: PathBuf,
    daemon: Option<Child>,
    dashboard: Option<Child>,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-count-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("work/src")).unwrap();
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let world = Self {
            base,
            daemon: None,
            dashboard: None,
        };
        for file in ["src/lib.rs", "src/other.rs"] {
            fs::write(world.work().join(file), "pub fn lib() {}\n").unwrap();
        }
        world.git(&["init", "-q", "-b", "main"]);
        world.git(&["add", "."]);
        world.git(&["commit", "-q", "-m", "start"]);
        world
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn state(&self) -> PathBuf {
        self.base.join("state")
    }
    fn start(&mut self) {
        self.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(self.state())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.cli(&["status"], None).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop(&mut self) {
        for child in [self.dashboard.take(), self.daemon.take()]
            .into_iter()
            .flatten()
        {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    /// The CLI, in time zone `tz` when given.
    fn cli(&self, args: &[&str], tz: Option<&str>) -> (i32, String, String) {
        let mut command = Process::new(env!("CARGO_BIN_EXE_langspan"));
        command.arg("--state-dir").arg(self.state()).args(args);
        if let Some(tz) = tz {
            command.env("TZ", tz);
        }
        let output = command.output().unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }
    fn count(&self, month: Option<&str>, tz: Option<&str>) -> Value {
        let mut args = vec!["count", "--json"];
        if let Some(month) = month {
            args.extend(["--month", month]);
        }
        let (code, out, err) = self.cli(&args, tz);
        assert_eq!(code, 0, "{out}{err}");
        serde_json::from_str(&out).unwrap()
    }
    fn git(&self, args: &[&str]) {
        let output = Process::new("git")
            .arg("-C")
            .arg(self.work())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    fn hook(&self, event: Value) -> Value {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.state())
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }
    fn started(&self, session: &str) {
        self.hook(json!({
            "session_id": session, "cwd": self.work(),
            "hook_event_name": "SessionStart", "source": "startup",
        }));
    }
    /// A Write's pre- or post-tool event; whether it was denied.
    fn write(&self, session: &str, call: &str, file: &str, stage: &str) -> bool {
        let reply = self.hook(json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": stage,
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file), "content": "x"},
        }));
        reply["hookSpecificOutput"]["permissionDecision"] == "deny"
    }
    /// Start the local page; its port and token.
    fn dashboard(&mut self) -> (u16, String) {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["dashboard", "--listen", "127.0.0.1:0", "--state-dir"])
            .arg(self.state())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        self.dashboard = Some(child);
        let rest = line.trim().strip_prefix("http://127.0.0.1:").unwrap();
        let (port, query) = rest.split_once('/').unwrap();
        (
            port.parse().unwrap(),
            query.strip_prefix("?t=").unwrap().into(),
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The local page's JSON view.
fn api_state(port: u16, token: &str) -> Value {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-LangSpan-Token: {token}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    let (head, body) = reply.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{reply}");
    serde_json::from_str(body).unwrap()
}

/// Words that would claim what a refusal achieved. The disclaimer's own
/// "avoided" is the one use allowed.
fn assert_no_outcome_claimed(text: &str) {
    let text = text
        .replace("A refusal is not proof of avoided rework.", "")
        .to_lowercase();
    for word in [
        "prevent",
        "saved",
        "saving",
        "saves",
        "avoid",
        "collision",
        "caught",
        "catch",
        "protect",
        "rescue",
        "spared",
        "stopped",
        "averted",
        "blocked",
    ] {
        assert!(!text.contains(word), "{word:?} in {text}");
    }
}

/// Place generation 1 so that its first refusal is at `target` (wall).
fn move_first_run(state: &std::path::Path, target: u64) {
    let connection = rusqlite::Connection::open(state.join("state.sqlite3")).unwrap();
    let rows: Vec<(i64, String)> = connection
        .prepare("SELECT seq, event_json FROM events ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let events: Vec<(i64, Event)> = rows
        .iter()
        .map(|(seq, json)| (*seq, serde_json::from_str(json).unwrap()))
        .collect();
    let first_refusal = events
        .iter()
        .find_map(|(_, event)| match event {
            Event::WriteRefused {
                at_ms,
                generation: 1,
                ..
            } => Some(*at_ms),
            _ => None,
        })
        .unwrap();
    let (seq, anchor) = events
        .iter()
        .find_map(|(seq, event)| match event {
            Event::ClockAnchored { anchor } if anchor.generation == 1 => Some((*seq, *anchor)),
            _ => None,
        })
        .unwrap();
    let moved = Event::ClockAnchored {
        anchor: ClockAnchor {
            wall_ms: target - (first_refusal - anchor.clock_ms),
            ..anchor
        },
    };
    connection
        .execute(
            "UPDATE events SET event_json = ?1 WHERE seq = ?2",
            rusqlite::params![serde_json::to_string(&moved).unwrap(), seq],
        )
        .unwrap();
}

#[test]
fn refusals_are_counted_once_a_day_in_their_own_local_month() {
    let mut w = World::new();
    w.start();
    w.started("alice");
    w.started("bob");
    // Bob holds src/lib.rs; alice is refused three times, three tool calls,
    // for the same file: one refusal.
    assert!(!w.write("bob", "hold", "src/lib.rs", "PreToolUse"));
    for call in ["try-1", "try-2", "try-3"] {
        assert!(w.write("alice", call, "src/lib.rs", "PreToolUse"), "{call}");
    }
    // Bob finishes without changing it; alice's retry of the same file is
    // let through.
    w.write("bob", "hold", "src/lib.rs", "PostToolUse");
    assert!(!w.write("alice", "try-4", "src/lib.rs", "PreToolUse"));
    let first_run = w.count(None, None);
    assert_eq!(first_run["refusals"], 1, "{first_run:#}");
    assert_eq!(first_run["repeats"], 2, "{first_run:#}");
    assert_eq!(first_run["retried_through"], 1, "{first_run:#}");

    // That run moves to 2027-04-01 at 02:30 UTC; a second run refuses now.
    w.stop();
    move_first_run(&w.state(), APRIL_1_EARLY);
    w.start();
    assert!(!w.write("bob", "hold-2", "src/other.rs", "PreToolUse"));
    assert!(w.write("alice", "try-5", "src/other.rs", "PreToolUse"));
    // A held commit, as the commit guard reports it.
    let request = Request {
        version: PROTOCOL_VERSION,
        request_id: "held-commit-1".into(),
        command: Command::RecordRefusal {
            session_id: "claude-code.alice".into(),
            kind: RefusalKind::CommitHeld,
            paths: vec!["src/other.rs".into()],
            peer_session_id: Some("claude-code.bob".into()),
        },
    };
    let outcome = langspan::client::call(&w.state(), &request)
        .unwrap()
        .outcome;
    assert!(
        matches!(outcome, Outcome::RefusalRecorded { .. }),
        "{outcome:?}"
    );

    // Each month by its own local time.
    let april_utc = w.count(Some("2027-04"), Some("UTC"));
    assert_eq!(april_utc["month"], "2027-04");
    assert_eq!(april_utc["refusals"], 1, "{april_utc:#}");
    assert_eq!(april_utc["by_kind"]["conflict"], 1);
    assert_eq!(april_utc["retried_through"], 1, "{april_utc:#}");
    assert_eq!(april_utc["by_workspace"], json!({"app": 1}));
    assert_eq!(w.count(Some("2027-03"), Some("UTC"))["refusals"], 0);
    let new_york = Some("America/New_York");
    assert_eq!(w.count(Some("2027-03"), new_york)["refusals"], 1);
    assert_eq!(w.count(Some("2027-04"), new_york)["refusals"], 0);

    // This month: the second run's two, by kind adding up to the total.
    let now = w.count(None, None);
    assert_eq!(now["refusals"], 2, "{now:#}");
    assert_eq!(now["by_kind"]["conflict"], 1, "{now:#}");
    assert_eq!(now["by_kind"]["commit_held"], 1, "{now:#}");
    let kinds: u64 = now["by_kind"]
        .as_object()
        .unwrap()
        .values()
        .map(|n| n.as_u64().unwrap())
        .sum();
    assert_eq!(kinds, 2);
    assert_eq!(now["retried_through"], 0, "a held commit has no admission");
    assert_eq!(now["truncated"], false);

    // The text says what LangSpan did, never what it saved.
    let (code, text, _) = w.cli(&["count", "--month", "2027-04"], Some("UTC"));
    assert_eq!(code, 0, "{text}");
    assert!(
        text.starts_with(
            "April 2027: 1 refusal (1 conflict, 0 stale_base, 0 unread_lines, 0 commit_held)\n"
        ),
        "{text}"
    );
    assert!(
        text.trim_end()
            .ends_with("A refusal is not proof of avoided rework."),
        "{text}"
    );
    assert_no_outcome_claimed(&text);
    let (code, text, _) = w.cli(&["count"], None);
    assert_eq!(code, 0);
    assert_no_outcome_claimed(&text);
    // `today` shows the month in one line.
    let (code, today, _) = w.cli(&["today", "app"], None);
    assert_eq!(code, 0, "{today}");
    let line: Vec<&str> = today
        .lines()
        .filter(|line| line.starts_with("This month, "))
        .collect();
    assert_eq!(line.len(), 1, "{today}");
    assert!(
        line[0].contains(": 2 refusals (1 conflict, 0 stale_base, 0 unread_lines, 1 commit_held) in every workspace, 2 in app."),
        "{today}"
    );
    assert_no_outcome_claimed(line[0]);

    // The local page carries the same total.
    let (port, token) = w.dashboard();
    let state = api_state(port, &token);
    let month = &state["refusals_this_month"];
    assert_eq!(month["refusals"], now["refusals"], "{month:#}");
    assert_eq!(month["by_kind"], now["by_kind"]);
    assert_eq!(month["month"], now["month"]);
    assert_eq!(month["note"], "A refusal is not proof of avoided rework.");

    // Bad months say what to write.
    let (code, _, err) = w.cli(&["count", "--month", "2027-13"], None);
    assert_eq!(code, 1);
    assert!(err.contains("2027-03"), "{err}");

    // The coordinator down: exit 1 and no figure.
    w.stop();
    let (code, out, err) = w.cli(&["count"], None);
    assert_eq!(code, 1);
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("coordination unavailable"), "{err}");
}

#[test]
fn count_requests_match_the_committed_golden_vector() {
    let wire: &[u8] = include_bytes!("../fixtures/count-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, PROTOCOL_VERSION);
    assert_eq!(langspan::protocol::encode(&request).unwrap(), wire);
    let Command::Count { days } = &request.command else {
        panic!("{request:?}");
    };
    assert_eq!(days.len(), 32);
    let unknown = String::from_utf8_lossy(wire).replace(r#""days":"#, r#""x":1,"days":"#);
    assert!(serde_json::from_str::<Request>(&unknown).is_err());
}

/// Copy the log's first refusal until it holds `extra` more, with the
/// daemon stopped.
fn copy_first_refusal(state: &std::path::Path, extra: u32) {
    let connection = rusqlite::Connection::open(state.join("state.sqlite3")).unwrap();
    let copied = connection
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO events (event_json)
             SELECT (SELECT event_json FROM events
                     WHERE json_extract(event_json, '$.type') = 'write_refused'
                     ORDER BY seq LIMIT 1)
             FROM n",
            [extra],
        )
        .unwrap();
    assert_eq!(copied, extra as usize);
}

#[test]
fn a_truncated_month_says_so_wherever_its_count_is_shown() {
    let mut w = World::new();
    w.start();
    w.started("alice");
    w.started("bob");
    assert!(!w.write("bob", "hold", "src/lib.rs", "PreToolUse"));
    assert!(w.write("alice", "try-1", "src/lib.rs", "PreToolUse"));
    w.stop();
    copy_first_refusal(&w.state(), 20_000);
    w.start();
    assert_eq!(w.count(None, None)["truncated"], true);
    let qualifier = "truncated: counted from the newest 20000 refusal records only";

    // `today`'s one line.
    let (code, today, _) = w.cli(&["today", "app"], None);
    assert_eq!(code, 0, "{today}");
    let line = today
        .lines()
        .find(|line| line.starts_with("This month, "))
        .unwrap_or_else(|| panic!("{today}"));
    assert!(line.contains(qualifier), "{line}");

    // The local page: the line it shows, and the page showing that line.
    let (port, token) = w.dashboard();
    let month = &api_state(port, &token)["refusals_this_month"];
    assert_eq!(month["truncated"], true, "{month:#}");
    assert!(
        month["line"].as_str().unwrap_or("").contains(qualifier),
        "{month:#}"
    );
    let page = include_str!("../src/dashboard.html");
    assert!(
        page.contains("m.line"),
        "the page renders the daemon's line"
    );
}

#[test]
fn a_month_whose_day_starts_cannot_all_be_read_is_not_counted() {
    let mut w = World::new();
    w.start();
    // A `date` that answers once and then fails.
    let bin = w.base.join("bin");
    fs::create_dir(&bin).unwrap();
    let calls = w.base.join("date-calls");
    fs::write(
        bin.join("date"),
        format!(
            "#!/bin/sh\nif [ -e '{0}' ]; then exit 1; fi\n: > '{0}'\necho +0000\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("date"), fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
        .arg("--state-dir")
        .arg(w.state())
        .args(["count", "--month", "2027-03"])
        .env("PATH", path)
        .output()
        .unwrap();
    let (out, err) = (
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(calls.exists(), "the first offset was read");
    assert_eq!(output.status.code(), Some(1), "{out}{err}");
    assert!(out.is_empty(), "no count from a partial month: {out}");
    assert!(err.contains("time zone"), "{err}");
}
