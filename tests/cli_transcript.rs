//! A fixed script of `langspan` commands against a real coordinator on a
//! temporary repository, each run for its human output and with `--json`,
//! compared with golden transcripts in `tests/golden/transcript/` (issues
//! #324 and #325). Only what must vary is normalized: temporary paths,
//! generated identifiers, process IDs and times. To change a transcript on
//! purpose, see `tests/support/golden.rs`.

#[path = "support/golden.rs"]
mod golden;
mod support;

use langspan_core::Outcome;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// A coordinator, its state directory and a repository, all under one
/// short directory of the test's own (a Unix socket path must fit), and the
/// transcript so far.
struct Transcript {
    base: PathBuf,
    daemon: support::Owned,
    text: String,
    /// Generated values seen so far and what the transcript writes instead,
    /// in the order first seen.
    names: Vec<(String, String)>,
}

impl Transcript {
    fn new(name: &str) -> Self {
        let base = PathBuf::from(format!("/tmp/lst-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("work/src")).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir(base.join("state")).unwrap();
        std::fs::set_permissions(
            base.join("state"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(base.join("work/src/lib.rs"), LIB).unwrap();
        // No remote pass during the script: one would push and record
        // publications at a time of its own.
        let config = base.join("state/config.json");
        std::fs::write(&config, "{\"remote_pass_ms\":600000}\n").unwrap();
        std::fs::set_permissions(&config, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_langspan"));
        support::in_home(&mut daemon, &base.join("home"))
            .env("LANGSPAN_AUTOSTART", "off")
            .env("TZ", "UTC")
            .arg("daemon")
            .arg("--state-dir")
            .arg(base.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let daemon = support::Owned::spawn(&mut daemon);
        let names = vec![
            (daemon.id().to_string(), "<pid>".into()),
            (env!("CARGO_PKG_VERSION").into(), "<version>".into()),
        ];
        let transcript = Self {
            daemon,
            base,
            text: String::new(),
            names,
        };
        transcript.git(&["init", "-q", "-b", "main"]);
        transcript.git(&["add", "."]);
        transcript.git(&["commit", "-q", "-m", "start"]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while transcript.run(&["status"]).0 != Some(0) {
            assert!(Instant::now() < deadline, "the coordinator did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        transcript
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.work())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            // Fixed dates, so that commit IDs are the same on every run.
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    /// Runs the binary with `args`; its exit code, stdout and stderr.
    fn run(&self, args: &[&str]) -> (Option<i32>, String, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_langspan"));
        let output = support::in_home(&mut command, &self.base.join("home"))
            .env("LANGSPAN_AUTOSTART", "off")
            .env("TZ", "UTC")
            .env("LANGSPAN_STATE_DIR", self.base.join("state"))
            .current_dir(&self.base)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }

    /// `text` with temporary paths, the generated values seen so far and
    /// times written as placeholders.
    fn normalized(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for form in [
            std::fs::canonicalize(&self.base).unwrap(),
            self.base.clone(),
        ] {
            text = text.replace(form.to_str().unwrap(), "$TMP");
        }
        for (value, name) in &self.names {
            text = text.replace(value, name);
        }
        human_times(&text)
    }

    /// Writes each clock value in `value` (a number under a key ending in
    /// `_ms`, or in an object under one) as `<ms>`, except the durations the
    /// script chose. A large one is also replaced in the human output,
    /// which prints the same values.
    fn scrub(&mut self, value: &mut Value, timed: bool) {
        match value {
            Value::Object(map) => {
                // An export's size counts the digits of the clock reading
                // in its header.
                if map.get("type").and_then(Value::as_str) == Some("graph_exported")
                    && let Some(bytes) = map.get_mut("bytes")
                {
                    *bytes = Value::String("<bytes>".into());
                }
                for (key, value) in map.iter_mut() {
                    let timed =
                        timed || (key.ends_with("_ms") && !CHOSEN_MS.contains(&key.as_str()));
                    if key == "month" {
                        *value = Value::String("<month>".into());
                    }
                    self.scrub(value, timed);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.scrub(item, false)),
            Value::Number(number) if timed => {
                let text = number.to_string();
                if text.len() >= 7 && !self.names.iter().any(|(seen, _)| *seen == text) {
                    self.names.push((text, "<ms>".into()));
                }
                *value = Value::String("<ms>".into());
            }
            _ => {}
        }
    }

    /// Learns the generated identifiers in `value`: every string of a key
    /// that names an ID, long enough not to be one the script chose.
    fn learn(&mut self, value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if let Value::String(text) = value
                        && (key == "id"
                            || key == "group"
                            || key.ends_with("_id")
                            || key.ends_with("_ids")
                            || key.ends_with("_token"))
                        && text.len() >= 12
                        && !self.names.iter().any(|(seen, _)| seen == text)
                    {
                        let ids = self.names.iter().filter(|(_, name)| name != "<ms>").count();
                        let name = format!("<{key}-{}>", ids - 1);
                        self.names.push((text.clone(), name));
                    }
                    self.learn(value);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| self.learn(item)),
            _ => {}
        }
    }

    /// One case: `args` for the human output, then with `--json`. Returns
    /// the JSON answer's outcome.
    fn case(&mut self, title: &str, args: &[&str]) -> Value {
        self.case_twice(title, args, args)
    }

    /// A case whose `--json` run takes other arguments, for a command that
    /// cannot run twice the same way (`graph export` makes a new file).
    fn case_twice(&mut self, title: &str, args: &[&str], json_args: &[&str]) -> Value {
        let (code, stdout, stderr) = self.run(args);
        let json_args: Vec<&str> = json_args.iter().copied().chain(["--json"]).collect();
        let (json_code, json_stdout, json_stderr) = self.run(&json_args);
        let response: Value = serde_json::from_str(&json_stdout).unwrap_or_else(|error| {
            panic!(
                "`langspan {args:?} --json` printed no JSON ({error}): {json_stdout}{json_stderr}"
            )
        });
        self.learn(&response);
        // The case that shows an outcome produces that outcome.
        if let Some(outcome) = response.get("outcome") {
            let outcome: Outcome = serde_json::from_value(outcome.clone())
                .unwrap_or_else(|error| panic!("{title}: not an outcome ({error}): {outcome}"));
            if CASES.contains(&title) {
                assert_eq!(
                    case_of(&outcome),
                    title,
                    "the case {title:?} gave {outcome:?}"
                );
            }
        }
        let mut scrubbed = response.clone();
        self.scrub(&mut scrubbed, false);
        let json_stdout = format!("{scrubbed}\n");
        let (code, json_code) = (code.unwrap_or(-1), json_code.unwrap_or(-1));
        let mut text = format!("## {title}\n$ langspan {}\nexit {code}\n", args.join(" "));
        // A line of human output that is a JSON object (`status --all`,
        // the dashboard) is normalized as the JSON answer is.
        for line in stdout.lines() {
            match serde_json::from_str::<Value>(line) {
                Ok(mut value) if value.is_object() => {
                    self.learn(&value);
                    self.scrub(&mut value, false);
                    text.push_str(&format!("{value}\n"));
                }
                // Trailing spaces are shown, as `cat -e` does.
                _ if line.ends_with(char::is_whitespace) => {
                    text.push_str(&format!("{line}$\n"));
                }
                _ => text.push_str(&format!("{line}\n")),
            }
        }
        for line in stderr.lines() {
            text.push_str(&format!("stderr: {line}\n"));
        }
        text.push_str(&format!(
            "$ langspan {}\nexit {json_code}\n{json_stdout}",
            json_args.join(" ")
        ));
        for line in json_stderr.lines() {
            text.push_str(&format!("stderr: {line}\n"));
        }
        text.push('\n');
        let text = self.normalized(&text);
        self.text.push_str(&text);
        response["outcome"].clone()
    }

    /// Waits until the coordinator has scanned `workspace` once and indexed
    /// its source, so that the scan's events come at the same place in the
    /// log on every run and answers that use the index are complete.
    fn wait_for_scan(&self, workspace: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (_, stdout, _) = self.run(&["changes", workspace, "--json"]);
            let answer: Value = serde_json::from_str(&stdout).unwrap_or(Value::Null);
            let (_, symbols, _) = self.run(&["graph", "symbol", workspace, "parse", "--json"]);
            let symbols: Value = serde_json::from_str(&symbols).unwrap_or(Value::Null);
            if !answer["outcome"]["scan"].is_null()
                && symbols["outcome"]["symbols"]
                    .as_array()
                    .is_some_and(|found| !found.is_empty())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no scan of {workspace}: {stdout}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A case for a command only hooks and MCP send: `langspan request`
    /// with the whole envelope.
    fn request(&mut self, title: &str, request_id: &str, command: Value) -> Value {
        let body = serde_json::json!({
            "version": langspan::protocol::PROTOCOL_VERSION,
            "request_id": request_id,
            "command": command,
        })
        .to_string();
        self.case(title, &["request", &body])
    }
}

impl Drop for Transcript {
    fn drop(&mut self) {
        self.daemon.stop();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// `case_of`, an exhaustive match from each `Outcome` variant to the title
/// of the transcript case that shows it, and `CASES`, every such title. A
/// new variant fails to compile until it is given a case here;
/// `every_outcome_has_a_transcript_case` then fails until a transcript has
/// a case of that title, and the case itself fails unless it produces the
/// variant.
macro_rules! cases {
    ($($variant:ident => $title:literal,)*) => {
        fn case_of(outcome: &Outcome) -> &'static str {
            match outcome {
                $(Outcome::$variant { .. } => $title,)*
            }
        }
        const CASES: &[&str] = &[$($title,)*];
    };
}

cases! {
    Workspace => "workspace add",
    Workspaces => "workspace list",
    WorkspaceMoved => "workspace move",
    WorkspaceRemoval => "workspace remove",
    Session => "session start",
    SessionLiveness => "session attach-pid",
    SessionSeen => "session seen",
    ContextRecorded => "record context",
    ToldForgotten => "forget told",
    Intent => "intent set",
    IntentEnded => "intent end",
    Admitted => "admit",
    Conflict => "admit, conflict",
    Completed => "complete",
    Observed => "observe",
    DiskObserved => "observe disk",
    BranchObserved => "observe branch",
    Status => "status",
    StatusRecords => "status --all",
    Events => "events",
    Changes => "changes",
    WorkspaceGit => "observe workspace git",
    SessionPrompted => "session prompt",
    PromptTargets => "set prompt targets",
    PresenceObserved => "observe presence",
    Presence => "set presence",
    SessionEnded => "session end",
    Adopted => "adopt",
    SessionStopped => "session stop",
    ToolBatchObserved => "observe tool batch",
    AutoCheck => "auto check enable",
    AutoCheckDisabled => "auto check disable",
    Briefing => "brief",
    StaleBase => "admit tool, stale base",
    UnreadLines => "admit tool, unread lines",
    SourceObserved => "observe source",
    RemoteLink => "remote enable",
    RemoteDisabled => "remote disable",
    RemoteOutbox => "remote outbox",
    RemoteSent => "remote sent",
    Compaction => "compact",
    RemoteObserved => "observe remote",
    CheckRecorded => "record check",
    Checks => "check status",
    FlakyTests => "flaky tests",
    FlakyTestForgotten => "forget flaky test",
    Readiness => "landing readiness",
    Landed => "record landing",
    Turn => "take turn",
    TurnBusy => "take turn, busy",
    TurnWaiting => "take turn, waiting",
    TurnUnavailable => "take turn, unavailable",
    TurnQueue => "turn queue",
    TurnQueueLeft => "leave turn queue",
    TurnReleased => "release turn",
    InterfaceObserved => "observe interface",
    InterfaceDependentsObserved => "observe interface dependents",
    Remembered => "remember",
    NoteAnchorsObserved => "observe note anchors",
    Forgotten => "forget",
    MessageSent => "message",
    MessageAcknowledged => "ack",
    MessageShown => "messages show",
    MessageStatus => "messages status",
    MessageRetracted => "messages retract",
    ScanObserved => "observe scan",
    HostsObserved => "observe hosts",
    CheckoutObserved => "observe checkout",
    CheckoutRunObserved => "observe checkout run",
    UncoordinatedObserved => "observe uncoordinated",
    Recap => "recap",
    Trails => "trail",
    Recall => "recall",
    History => "graph history",
    Today => "today",
    Report => "report",
    Count => "count request",
    StatusLine => "status line",
    RefusalRecorded => "record refusal",
    Policy => "policy",
    Holders => "holders",
    WaitRecorded => "record wait",
    Dashboard => "dashboard",
    Version => "version",
    Graph => "graph symbol",
    GraphContext => "graph context",
    GraphInflight => "graph inflight",
    GraphPath => "graph path",
    GraphMap => "graph map",
    GraphExported => "graph export",
    GraphImported => "graph import",
    CallBreaks => "check calls",
    WriteCheck => "check-write",
    GraphBlindSpots => "graph blind-spots",
    InterfaceAcknowledged => "acknowledge interface",
    ReadOutline => "outline read",
    BetweenSteps => "between steps",
    Error => "complete, an unknown admission",
}

/// Every outcome's case is in a golden transcript.
#[test]
fn every_outcome_has_a_transcript_case() {
    let titles: Vec<String> = ["coordination.txt", "remaining.txt"]
        .iter()
        .flat_map(|file| {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden/transcript")
                .join(file);
            std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
                .lines()
                .filter_map(|line| line.strip_prefix("## ").map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect();
    for title in CASES {
        assert!(
            titles.iter().any(|seen| seen == title),
            "no transcript case {title:?}: add one that produces its outcome"
        );
    }
}

/// The SHA-256 of `file` in hex, from the system's tool (`sha256sum` on
/// Linux, `shasum` on macOS).
fn sha256_hex(file: &std::path::Path) -> String {
    let output = Command::new("sha256sum")
        .arg(file)
        .output()
        .or_else(|_| {
            Command::new("shasum")
                .args(["-a", "256"])
                .arg(file)
                .output()
        })
        .expect("run sha256sum or shasum");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()[..64].to_owned()
}

/// The repository's one source file.
const LIB: &str = "pub fn parse(input: &str) -> usize {\n    input.len()\n}\n";

/// `_ms` fields the script sets, which do not vary.
const CHOSEN_MS: &[&str] = &["wait_ms", "ttl_ms"];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Human `text` with the times that vary written as placeholders: RFC 3339
/// times, clock times (`04:03`), ages (`3s ago`, `3 s ago`), coordinator
/// clock readings and durations (`at 374471 ms`), an export's size and the
/// month and year (`November 2026`).
fn human_times(text: &str) -> String {
    let bytes = text.as_bytes();
    let digits = |at: usize, n: usize| {
        at + n <= bytes.len() && bytes[at..at + n].iter().all(u8::is_ascii_digit)
    };
    let mut out = String::new();
    let mut at = 0;
    while at < bytes.len() {
        // 2026-08-29T04:03:03Z or with an offset.
        if digits(at, 4)
            && bytes.get(at + 4) == Some(&b'-')
            && digits(at + 5, 2)
            && bytes.get(at + 10) == Some(&b'T')
        {
            let mut end = at + 19;
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b')' {
                end += 1;
            }
            out.push_str("<time>");
            at = end;
            continue;
        }
        // 04:03, not inside a longer number.
        if digits(at, 2)
            && bytes.get(at + 2) == Some(&b':')
            && digits(at + 3, 2)
            && !digits(at + 5, 1)
            && (at == 0 || !bytes[at - 1].is_ascii_digit())
        {
            out.push_str("<hh:mm>");
            at += 5;
            continue;
        }
        // A clock reading or how long something has held, of any length:
        // "for 4 ms", "Expires at 374471 ms", "until 9 ms", "(…, 12 ms)".
        // And the size of an export, whose header holds a clock reading:
        // "(0 partial, 583 bytes)".
        let measured = [
            ("for ", " ms"),
            ("at ", " ms"),
            ("until ", " ms"),
            (", ", " ms"),
            ("partial, ", " bytes"),
        ]
        .iter()
        .find_map(|&(before, unit)| {
            let start = at + before.len();
            if !text[at..].starts_with(before) || !digits(start, 1) {
                return None;
            }
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            text[end..].starts_with(unit).then_some((before, unit, end))
        });
        if let Some((before, unit, end)) = measured {
            let name = if unit == " ms" { "<ms>" } else { "<bytes>" };
            out.push_str(&format!("{before}{name}"));
            at = end;
            continue;
        }
        // An age: digits, an optional space, a unit, " ago".
        if bytes[at].is_ascii_digit() && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric()) {
            let mut end = at;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
                end += 1;
            }
            let rest = &text[end..];
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if let Some(unit) = ["ms", "s", "min", "m", "h", "d"]
                .iter()
                .find(|unit| rest.starts_with(&format!("{unit} ago")))
            {
                out.push_str("<age> ago");
                at = text.len() - rest.len() + unit.len() + " ago".len();
                continue;
            }
        }
        let ch = text[at..].chars().next().unwrap();
        out.push(ch);
        at += ch.len_utf8();
    }
    for month in MONTHS {
        for year in 2026..2100 {
            out = out.replace(&format!("{month} {year}"), "<month>");
        }
    }
    out
}

/// The clock reads differently on each platform: Linux CI printed a 6-digit
/// expiry and a 579-byte export where macOS printed 583 bytes.
#[test]
fn clock_readings_and_export_sizes_are_placeholders_whatever_their_length() {
    for (text, want) in [
        ("Expires at 374471 ms of", "Expires at <ms> ms of"),
        ("Expires at 1234567890 ms of", "Expires at <ms> ms of"),
        ("held until 9 ms.", "held until <ms> ms."),
        (
            "(observation generation 1, 37 ms)",
            "(observation generation 1, <ms> ms)",
        ),
        ("(0 partial, 579 bytes)", "(0 partial, <bytes> bytes)"),
        ("wait up to 100 ms", "wait up to 100 ms"),
        ("Added context: 120 bytes", "Added context: 120 bytes"),
    ] {
        assert_eq!(human_times(text), want, "{text}");
    }
}

fn string<'a>(value: &'a Value, path: &[&str]) -> &'a str {
    let mut value = value;
    for key in path {
        value = &value[key];
    }
    value
        .as_str()
        .unwrap_or_else(|| panic!("no string at {path:?} in {value}"))
}

/// Workspaces and sessions, admission, observation, messages and notes,
/// and the projections (issue #324).
#[test]
#[ignore = "machine-dependent values on Linux: #397"]
fn transcript_of_the_coordination_commands() {
    let mut t = Transcript::new("one");
    let work = t.work().display().to_string();
    t.case(
        "workspace add",
        &["workspace", "add", "w", &work, "--request-id", "r-ws"],
    );
    t.wait_for_scan("w");
    t.case("workspace list", &["workspace", "list"]);
    t.case(
        "session start",
        &["session", "start", "s1", "w", "--request-id", "r-s1"],
    );
    t.case(
        "session start, second",
        &["session", "start", "s2", "w", "--request-id", "r-s2"],
    );
    t.case(
        "intent set",
        &[
            "intent",
            "set",
            "s1",
            "src/lib.rs",
            "--summary",
            "parser work",
            "--request-id",
            "r-i1",
        ],
    );
    t.case(
        "intent end",
        &["intent", "end", "s1", "--request-id", "r-i2"],
    );
    let admitted = t.case(
        "admit",
        &[
            "admit",
            "s1",
            "src/lib.rs",
            "--ttl-ms",
            "60000",
            "--request-id",
            "r-a1",
        ],
    );
    t.case(
        "admit, conflict",
        &["admit", "s2", "src/lib.rs", "--request-id", "r-a2"],
    );
    t.case("check-write", &["check-write", "s2", "src/lib.rs"]);
    let lease = string(&admitted, &["lease", "lease_id"]).to_owned();
    t.case(
        "complete",
        &["complete", "s1", &lease, "--request-id", "r-c1"],
    );
    t.case(
        "complete, an unknown admission",
        &["complete", "s1", "nothing", "--request-id", "r-c2"],
    );
    let pid = t.daemon.id().to_string();
    t.case(
        "session attach-pid",
        &["session", "attach-pid", "s1", &pid, "--request-id", "r-l1"],
    );
    t.request(
        "session seen",
        "r-h1",
        json!({"op": "session_seen", "session_id": "s1"}),
    );
    t.request(
        "record context",
        "r-h2",
        json!({"op": "record_context", "session_id": "s1", "bytes": 120, "turn": true}),
    );
    t.request(
        "forget told",
        "r-h3",
        json!({"op": "forget_told", "session_id": "s1", "keys": [1]}),
    );
    t.request(
        "session prompt",
        "r-h4",
        json!({"op": "session_prompt", "session_id": "s2", "summary": "fix the parser"}),
    );
    t.request(
        "set prompt targets",
        "r-h5",
        json!({"op": "set_prompt_targets", "session_id": "s2", "paths": ["src/lib.rs"]}),
    );
    t.request(
        "observe presence",
        "r-h6",
        json!({"op": "observe_presence", "session_id": "s2", "kind": "search", "paths": ["src"]}),
    );
    t.request(
        "set presence",
        "r-h7",
        json!({"op": "set_presence", "session_id": "s2", "shown": true, "window_ms": 600000}),
    );
    t.request(
        "holders",
        "read",
        json!({"op": "holders", "session_id": "s2", "paths": ["src/lib.rs"]}),
    );
    t.request(
        "record refusal",
        "r-h8",
        json!({"op": "record_refusal", "session_id": "s2", "kind": "commit_held", "paths": ["src/lib.rs"], "peer_session_id": "s1"}),
    );
    t.case("policy", &["policy", "w"]);
    t.case(
        "policy set",
        &[
            "policy",
            "set",
            "w",
            "conflict",
            "wait",
            "--wait-ms",
            "100",
            "--request-id",
            "r-p1",
        ],
    );
    t.case("changes", &["changes", "w", "--limit", "3"]);
    t.case("events", &["events", "--limit", "3"]);
    t.request(
        "observe",
        "r-o1",
        json!({"op": "observe", "session_id": "s1", "operation_id": "op-1", "source": "cli",
            "tool": "Edit", "paths": ["src/lib.rs"], "result": "succeeded", "lease_id": null}),
    );
    t.request(
        "observe tool batch",
        "r-h9",
        json!({"op": "observe_tool_batch", "session_id": "s1", "operation_ids": ["op-1"], "calls": 1}),
    );
    t.request(
        "session stop",
        "r-h10",
        json!({"op": "session_stop", "session_id": "s1"}),
    );
    t.request(
        "observe disk",
        "r-o2",
        json!({"op": "observe_disk", "workspace_id": "w", "path": "src/new.rs", "before": null,
            "after": {"type": "present", "digest": "a".repeat(64)}, "related_operation_ids": []}),
    );
    t.request(
        "observe branch",
        "r-o3",
        json!({"op": "observe_branch", "workspace_id": "w", "branch_ref": "refs/heads/topic",
            "head_oid": null, "tree_oid": null}),
    );
    t.request(
        "observe workspace git",
        "r-o4",
        json!({"op": "observe_workspace_git", "workspace_id": "w", "repository_id": "repository-one",
            "branch_ref": "refs/heads/main"}),
    );
    let note = t.case(
        "remember",
        &[
            "remember",
            "w",
            "the parser counts bytes",
            "src/lib.rs",
            "--tag",
            "parser",
            "--request-id",
            "r-n1",
        ],
    );
    t.case("recall", &["recall", "w", "parser"]);
    let note = string(&note, &["note", "note_id"]).to_owned();
    t.case("forget", &["forget", "w", &note, "--request-id", "r-n2"]);
    let message = t.case(
        "message",
        &[
            "message",
            "w",
            "please review the parser",
            "--to",
            "s2",
            "--from",
            "s1",
            "--request-id",
            "r-m1",
        ],
    );
    let message = string(&message, &["message", "message_id"]).to_owned();
    t.case("messages show", &["messages", "show", "w", &message]);
    t.case("messages status", &["messages", "status", "w", &message]);
    t.case("brief", &["brief", "s2", "--request-id", "r-b1"]);
    t.case("ack", &["ack", "s2", &message, "--request-id", "r-m2"]);
    t.case(
        "handoff",
        &[
            "handoff",
            "s1",
            "take over the parser",
            "--request-id",
            "r-m3",
        ],
    );
    t.case(
        "messages retract",
        &[
            "messages",
            "retract",
            "w",
            "last",
            "--from",
            "s1",
            "--request-id",
            "r-m4",
        ],
    );
    t.request(
        "session end",
        "r-e1",
        json!({"op": "session_end", "session_id": "s2"}),
    );
    t.request(
        "adopt",
        "r-e2",
        json!({"op": "adopt", "session_id": "s1", "from": "s2"}),
    );
    t.request("version", "read", json!({"op": "version"}));
    t.request(
        "status line",
        "read",
        json!({"op": "status_line", "root": t.work(), "from_ms": 0}),
    );
    t.case("status", &["status"]);
    t.case("status --all", &["status", "--all"]);
    t.case("recap", &["recap", "w"]);
    t.case("trail", &["trail", "w"]);
    t.case("today", &["today", "w"]);
    t.case("report", &["report", "w"]);
    t.case("count", &["count"]);
    t.case("graph history", &["graph", "history", "w", "parse"]);
    t.case("compact", &["compact"]);
    t.case("workspace remove", &["workspace", "remove", "w"]);
    std::fs::rename(t.work(), t.base.join("moved")).unwrap();
    let moved = t.base.join("moved").display().to_string();
    t.case(
        "workspace move",
        &["workspace", "move", "w", &moved, "--request-id", "r-w2"],
    );
    t.case(
        "workspace remove --apply",
        &[
            "workspace",
            "remove",
            "w",
            "--apply",
            "--request-id",
            "r-w3",
        ],
    );
    let text = std::mem::take(&mut t.text);
    golden::check("transcript/coordination.txt", &text);
}

/// Graph queries, checks and landing turns, interface notices, the scan's
/// and hosts' observations, remote awareness and the dashboard (issue #325).
#[test]
#[ignore = "machine-dependent values on Linux: #397"]
fn transcript_of_the_remaining_outcomes() {
    let mut t = Transcript::new("two");
    let work = t.work().display().to_string();
    let tree = "e2dd98466274a2f79e9aaf75ddcb0a996b15fe76";
    let head = "3151388a147d1a4c8b1a5a46910ac04c7cceb909";
    t.run(&["workspace", "add", "w", &work, "--request-id", "r-ws"]);
    t.wait_for_scan("w");
    t.run(&["session", "start", "s1", "w", "--request-id", "r-s1"]);
    t.run(&["session", "start", "s2", "w", "--request-id", "r-s2"]);

    t.case("graph symbol", &["graph", "symbol", "w", "parse"]);
    t.case("graph path", &["graph", "path", "w", "parse", "parse"]);
    t.case("graph map", &["graph", "map", "w"]);
    t.case("graph blind-spots", &["graph", "blind-spots", "w"]);
    t.case("graph inflight", &["graph", "inflight", "w"]);
    let index = t.base.join("index.jsonl").display().to_string();
    let index_json = t.base.join("index-json.jsonl").display().to_string();
    t.case_twice(
        "graph export",
        &["graph", "export", "w", "--out", &index],
        &["graph", "export", "w", "--out", &index_json],
    );
    t.case("graph import", &["graph", "import", "w", "--in", &index]);
    t.request(
        "graph context",
        "read",
        json!({"op": "graph_context", "workspace_id": "w", "names": ["parse"], "limit": 3}),
    );
    t.request(
        "check calls",
        "read",
        json!({"op": "check_calls", "session_id": "s1",
            "calls": [{"path": "src/lib.rs", "line": 2, "name": "parse", "args": 1, "member": false}]}),
    );
    t.request(
        "outline read",
        "r-g1",
        json!({"op": "outline_read", "session_id": "s1", "path": "src/lib.rs",
            "digest": "a".repeat(64), "lines": 3, "bytes": 60}),
    );

    let check = t.request(
        "record check",
        "r-k1",
        json!({"op": "record_check", "workspace_id": "w", "name": "unit", "command": "cargo test",
            "tree_oid": tree, "head_oid": head, "inputs": [], "result": {"type": "passed"},
            "duration_ms": 1200}),
    );
    t.request(
        "check status",
        "read",
        json!({"op": "check_status", "workspace_id": "w", "tree_oid": tree}),
    );
    t.request(
        "landing readiness",
        "read",
        json!({"op": "landing_readiness", "workspace_id": "w", "name": "unit", "tree_oid": tree}),
    );
    t.request(
        "flaky tests",
        "read",
        json!({"op": "flaky_tests", "workspace_id": "w"}),
    );
    t.request(
        "forget flaky test",
        "r-k2",
        json!({"op": "forget_flaky_test", "workspace_id": "w", "test": "parses"}),
    );
    t.request(
        "auto check enable",
        "r-k3",
        json!({"op": "auto_check_enable", "workspace_id": "w", "name": "auto", "argv": ["true"],
            "timeout_s": 30}),
    );
    t.request(
        "auto check disable",
        "r-k4",
        json!({"op": "auto_check_disable", "workspace_id": "w"}),
    );
    let check_id = check["check"]["check_id"]
        .as_str()
        .unwrap_or("check")
        .to_owned();
    t.request(
        "record landing",
        "r-k5",
        json!({"op": "record_landing", "workspace_id": "w", "onto": "main", "commit_oid": head,
            "tree_oid": tree, "inputs": [], "check_id": check_id}),
    );

    let take = |session: &str, queue: Value| {
        let mut command = json!({"op": "take_turn", "workspace_id": "w", "onto": "main",
            "session_id": session, "ttl_ms": 60000});
        if !queue.is_null() {
            command["queue"] = queue;
        }
        command
    };
    t.request("take turn", "r-t1", take("s1", Value::Null));
    t.request("take turn, busy", "r-t2", take("s2", Value::Null));
    t.request(
        "take turn, waiting",
        "r-t3",
        take(
            "s2",
            json!({"ticket": "r-t3", "wait_ms": 60000, "joined": false}),
        ),
    );
    t.request(
        "turn queue",
        "read",
        json!({"op": "turn_queue", "workspace_id": "w", "onto": "main"}),
    );
    t.request(
        "release turn",
        "r-t6",
        json!({"op": "release_turn", "workspace_id": "w", "onto": "main", "turn_id": "r-t1",
            "attempts": []}),
    );
    // Free, but r-t3 is next in line.
    t.request("take turn, unavailable", "r-t4", take("s1", Value::Null));
    t.request(
        "leave turn queue",
        "r-t5",
        json!({"op": "leave_turn_queue", "workspace_id": "w", "onto": "main", "ticket": "r-t3",
            "reason": "cancelled"}),
    );

    let notice = t.request(
        "observe interface",
        "r-f1",
        json!({"op": "observe_interface", "workspace_id": "w", "path": "src/lib.rs",
            "changes": [{"name": "parse", "kind": "function", "change": "signature_changed",
                "before": "fn parse(input: &str) -> usize", "after": "fn parse(input: &[u8]) -> usize"}],
            "dependents": []}),
    );
    let notice_id = notice["notice"]["notice_id"]
        .as_str()
        .unwrap_or("notice")
        .to_owned();
    // Another worktree of the same repository.
    let other = t.base.join("other").display().to_string();
    t.git(&["worktree", "add", "-q", "-b", "topic", &other]);
    t.run(&["workspace", "add", "w2", &other, "--request-id", "r-ws2"]);
    t.wait_for_scan("w2");
    t.request(
        "observe interface dependents",
        "r-f2",
        json!({"op": "observe_interface_dependents", "notice_workspace_id": "w",
            "notice_id": notice_id, "workspace_id": "w2", "paths": ["src/lib.rs"]}),
    );
    t.request(
        "acknowledge interface",
        "r-f3",
        json!({"op": "acknowledge_interface", "session_id": "s2", "notice_ids": [notice_id]}),
    );
    t.request(
        "between steps",
        "r-f4",
        json!({"op": "between_steps", "session_id": "s2", "stops": 0}),
    );
    t.request(
        "observe note anchors",
        "r-f5",
        json!({"op": "observe_note_anchors", "workspace_id": "w"}),
    );

    let digest = |c: &str| c.repeat(64);
    t.request(
        "observe source",
        "r-v1",
        json!({"op": "observe_source", "session_id": "s2", "origin": "read",
            "files": [{"path": "src/lib.rs", "state": {"type": "present", "digest": digest("a")}}]}),
    );
    let admit = |id: &str, base: &str, overwrites: Value| {
        json!({"op": "admit_tool", "session_id": "s2", "paths": ["src/lib.rs"], "ttl_ms": 30000,
            "operation": {"operation_id": id, "source": "cli", "tool": "Write",
                "input_digest": digest("c")},
            "base": [{"path": "src/lib.rs", "state": {"type": "present", "digest": digest(base)}}],
            "overwrites": overwrites})
    };
    t.request(
        "admit tool, stale base",
        "r-v2",
        admit("op-2", "b", json!([])),
    );
    // A session with no recorded version of the file, at its real content.
    t.run(&["session", "start", "s3", "w", "--request-id", "r-s3"]);
    let real = sha256_hex(&t.work().join("src/lib.rs"));
    let mut unread = admit(
        "op-3",
        "a",
        json!([{"path": "src/lib.rs", "session_id": "s1",
            "lines": [{"start_line": 2, "end_line": 2}], "age_ms": 2000}]),
    );
    unread["session_id"] = json!("s3");
    unread["base"][0]["state"]["digest"] = json!(real);
    t.request("admit tool, unread lines", "r-v3", unread);
    t.request(
        "record wait",
        "r-v4",
        json!({"op": "record_wait", "session_id": "s2", "paths": ["src/lib.rs"],
            "holder_session_id": "s1", "waited_ms": 100, "result": "refused_conflict"}),
    );
    t.request(
        "observe scan",
        "r-v5",
        json!({"op": "observe_scan", "workspace_id": "w", "failure": "the disk is unreadable"}),
    );
    t.request(
        "observe hosts",
        "r-v6",
        json!({"op": "observe_hosts", "not_installed": ["codex"]}),
    );
    t.request(
        "observe checkout",
        "r-v7",
        json!({"op": "observe_checkout", "workspace_id": "w", "from_branch": "refs/heads/main",
            "to_branch": "refs/heads/topic", "from_head": head, "to_head": head, "files": 2}),
    );
    t.request(
        "observe checkout run",
        "r-v8",
        json!({"op": "observe_checkout_run", "session_id": "s1", "command": "git switch topic",
            "to_branch": "refs/heads/topic", "to_head": head, "ran_ms": 50}),
    );
    t.request(
        "observe uncoordinated",
        "r-v9",
        json!({"op": "observe_uncoordinated", "workspace_id": "w",
            "files": [{"path": "src/lib.rs", "state": {"type": "present", "digest": digest("d")}}]}),
    );

    t.request(
        "remote enable",
        "r-r1",
        json!({"op": "remote_enable", "workspace_id": "w", "remote": "origin",
            "participant": "0123456789abcdef0123456789abcdef", "name": "alice", "share_task": false}),
    );
    t.request(
        "remote published",
        "r-r4",
        json!({"op": "remote_published", "workspace_id": "w", "sequence": 1,
            "signature": "signature-one", "payload": "{}", "published_at_ms": 1_800_000_000_000_u64}),
    );
    t.request(
        "remote outbox",
        "read",
        json!({"op": "remote_outbox", "workspace_id": "w"}),
    );
    t.request(
        "remote sent",
        "read",
        json!({"op": "remote_sent", "workspace_id": "w", "limit": 10}),
    );
    t.request(
        "observe remote",
        "r-r2",
        json!({"op": "observe_remote", "workspace_id": "w",
            "publication": {"participant": "fedcba9876543210fedcba9876543210", "name": "bob",
                "sequence": 1, "work": []}}),
    );
    t.request(
        "remote disable",
        "r-r3",
        json!({"op": "remote_disable", "workspace_id": "w"}),
    );
    t.request(
        "count request",
        "read",
        json!({"op": "count", "days": [0, 86_400_000]}),
    );
    t.request("dashboard", "read", json!({"op": "dashboard"}));
    let text = std::mem::take(&mut t.text);
    golden::check("transcript/remaining.txt", &text);
}
