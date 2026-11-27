//! A fixed script of `langspan` commands against a real coordinator on a
//! temporary repository, each run for its human output and with `--json`,
//! compared with golden transcripts in `tests/golden/transcript/` (issues
//! #324 and #325). Only what must vary is normalized: temporary paths,
//! generated identifiers, process IDs and times. To change a transcript on
//! purpose, see `tests/support/golden.rs`.

#[path = "support/golden.rs"]
mod golden;
mod support;

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
        std::fs::write(
            base.join("work/src/lib.rs"),
            "pub fn parse(input: &str) -> usize {\n    input.len()\n}\n",
        )
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
        let (code, stdout, stderr) = self.run(args);
        let json_args: Vec<&str> = args.iter().copied().chain(["--json"]).collect();
        let (json_code, json_stdout, json_stderr) = self.run(&json_args);
        let response: Value = serde_json::from_str(&json_stdout).unwrap_or_else(|error| {
            panic!(
                "`langspan {args:?} --json` printed no JSON ({error}): {json_stdout}{json_stderr}"
            )
        });
        self.learn(&response);
        let mut scrubbed = response.clone();
        self.scrub(&mut scrubbed, false);
        let json_stdout = format!("{scrubbed}\n");
        let (code, json_code) = (code.unwrap_or(-1), json_code.unwrap_or(-1));
        let mut text = format!("## {title}\n$ langspan {}\nexit {code}\n", args.join(" "));
        text.push_str(&stdout);
        for line in stderr.lines() {
            text.push_str(&format!("stderr: {line}\n"));
        }
        text.push_str(&format!(
            "$ langspan {} --json\nexit {json_code}\n{json_stdout}",
            args.join(" ")
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
/// times, clock times (`04:03`), ages (`3s ago`, `3 s ago`) and the month
/// and year (`November 2026`).
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
