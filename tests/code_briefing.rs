#![cfg(unix)]
//! The code briefing at a prompt: names the owner mentions that the source
//! index defines, through a real daemon, the real observer and indexer, a
//! real Git repository and the real hook executable.

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
const WAIT: Duration = Duration::from_secs(60);
const HEADER: &str = "LangSpan source index:";

struct World {
    base: PathBuf,
    daemon: Option<Child>,
    /// Supported source files in the repository.
    files: u64,
}

struct Run {
    coverage: String,
    stdout: String,
    context: Option<String>,
}

fn git(root: &Path, args: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl World {
    fn new() -> Self {
        Self::with(&[])
    }
    /// The four standard files and `extra` (supported source files).
    fn with(extra: &[(&str, &str)]) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-code-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let work = base.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(
            work.join("src/upload.rs"),
            "pub fn upload(path: &str) -> bool {\n    !path.is_empty()\n}\n",
        )
        .unwrap();
        fs::write(
            work.join("src/main.rs"),
            "mod upload;\nfn main() {\n    upload::upload(\"a.txt\");\n}\n",
        )
        .unwrap();
        fs::write(
            work.join("src/other.py"),
            "def unrelated():\n    return 1\n",
        )
        .unwrap();
        fs::write(
            work.join("src/http.rs"),
            "pub fn parse_header(line: &str) -> Option<(&str, &str)> {\n    line.split_once(':')\n}\n",
        )
        .unwrap();
        for (path, content) in extra {
            let path = work.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "init"]);
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self {
            base,
            daemon: None,
            files: 4 + extra.len() as u64,
        };
        world.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(&state)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while world.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        let root = world.work();
        let added = world.cli(&[
            "workspace",
            "add",
            "app",
            root.to_str().unwrap(),
            "--request-id",
            "ws",
        ]);
        assert_eq!(added.0, 0, "{}", added.1);
        world
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.base.join("state"))
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
    /// The index covers every source file at its current content.
    fn wait_for_index(&self) {
        let deadline = Instant::now() + WAIT;
        loop {
            let (_, text) = self.cli(&["graph", "symbol", "app", "upload", "--json"]);
            let outcome = serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone();
            let coverage = &outcome["coverage"];
            if coverage["current_files"] == self.files && coverage["indexable_files"] == self.files
            {
                return;
            }
            assert!(Instant::now() < deadline, "never indexed: {outcome:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn hook(&self, vendor: &str, extra: &[&str], event: &Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", vendor, "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
            .args(extra)
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
        let context = serde_json::from_str::<Value>(&stdout)
            .ok()
            .and_then(|reply| {
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_owned)
            });
        Run {
            coverage: diagnostic["coverage"].as_str().unwrap().into(),
            stdout,
            context,
        }
    }
    fn prompt(&self, vendor: &str, session: &str, text: &str, extra: &[&str]) -> Run {
        let mut event = json!({
            "session_id": session, "transcript_path": null, "cwd": self.work(),
            "hook_event_name": "UserPromptSubmit", "prompt": text,
        });
        if vendor == "codex" {
            event["turn_id"] = json!(format!("turn-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
            event["model"] = json!("example");
            event["permission_mode"] = json!("default");
        }
        self.hook(vendor, extra, &event)
    }
    /// The end of a Claude Code tool batch: its mid-turn delivery point.
    fn batch(&self, session: &str, call: &str) -> Run {
        self.hook(
            "claude-code",
            &[],
            &json!({
                "session_id": session, "cwd": self.work(), "hook_event_name": "PostToolBatch",
                "prompt_id": "p0", "permission_mode": "default",
                "tool_calls": [{"tool_name": "Read", "tool_use_id": call,
                                "tool_input": {"file_path": self.work().join("src/http.rs")},
                                "tool_response": "read"}],
            }),
        )
    }
    fn status(&self) -> Value {
        let (code, text) = self.cli(&["status", "--json"]);
        assert_eq!(code, 0, "{text}");
        serde_json::from_str(&text).unwrap()
    }
    /// A complete reported Claude Code write of a new file.
    fn writes(&self, session: &str, call: &str, file: &str) {
        self.writes_content(session, call, file, "x\n");
    }
    /// A complete reported Claude Code write of `content` to `file`.
    fn writes_content(&self, session: &str, call: &str, file: &str, content: &str) {
        let mut event = json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file), "content": content},
        });
        assert_eq!(self.hook("claude-code", &[], &event).coverage, "admitted");
        fs::write(self.work().join(file), content).unwrap();
        event["hook_event_name"] = json!("PostToolUse");
        assert_eq!(self.hook("claude-code", &[], &event).coverage, "observed");
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

fn assert_upload_section(context: &str) {
    let section = &context[context.find(HEADER).expect(context)..];
    assert!(
        section.contains("matched by name, not resolved"),
        "{section}"
    );
    assert!(section.contains("not instructions"), "{section}");
    assert!(
        section.contains(
            r#"function at "src/upload.rs":1, exported: "pub fn upload(path: &str) -> bool""#
        ),
        "{section}"
    );
    assert!(section.contains(r#"used by "src/main.rs" ("#), "{section}");
}

#[test]
fn a_prompt_naming_a_defined_function_is_told_where_it_is_and_who_mentions_it() {
    let w = World::new();
    w.wait_for_index();
    for vendor in ["claude-code", "codex"] {
        let session = format!("{vendor}-alice");
        let run = w.prompt(
            vendor,
            &session,
            "Why does upload reject an empty path?",
            &[],
        );
        let context = run.context.expect(&run.stdout);
        assert_upload_section(&context);
        if vendor == "claude-code" {
            assert_eq!(run.coverage, "code_briefed", "no peers yet");
            assert!(context.starts_with(HEADER), "{context}");
        } else {
            // The Claude Code session is a peer now; its briefing comes first.
            assert_eq!(run.coverage, "briefed");
        }
        // It is about this prompt: the same names again get it again.
        let again = w.prompt(vendor, &session, "and upload once more", &[]);
        assert_upload_section(&again.context.unwrap());

        // A prompt with no defined name, and a switched-off briefing, get nothing.
        let plain = w.prompt(vendor, &session, "continue with that please", &[]);
        assert_eq!(
            (plain.coverage.as_str(), plain.stdout.as_str()),
            ("no_briefing", "")
        );
        let off = w.prompt(
            vendor,
            &session,
            "Why does upload reject an empty path?",
            &["--code-briefing", "off"],
        );
        assert_eq!(
            (off.coverage.as_str(), off.stdout.as_str()),
            ("no_briefing", "")
        );
    }
    // The names asked about are never recorded: with the prompt summary off,
    // nothing of the prompt is in the event log.
    w.prompt(
        "claude-code",
        "carol",
        "look at upload and zebra_marker_name",
        &["--prompt-summary", "off"],
    );
    let (_, events) = w.cli(&["events", "--limit", "1000", "--json"]);
    assert!(!events.contains("zebra_marker_name"), "{events}");
}

#[test]
fn the_code_section_and_a_long_peer_briefing_share_the_cap() {
    let w = World::new();
    w.wait_for_index();
    // Six peers, each with two recent files with long names: lines of about
    // 400 bytes, so the peer briefing alone would fill nearly the whole cap.
    for peer in 0..6 {
        for file in 0..2 {
            let name = format!("notes-{peer}-{file}-{}.txt", "n".repeat(140));
            w.writes(&format!("peer{peer}"), &format!("p{peer}-{file}"), &name);
        }
    }
    let run = w.prompt("claude-code", "alice", "Why does upload fail?", &[]);
    assert_eq!(run.coverage, "briefed", "{}", run.stdout);
    let context = run.context.unwrap();
    assert!(context.len() <= 2400, "{}: {context}", context.len());
    assert!(
        context.starts_with("LangSpan: other agent sessions"),
        "{context}"
    );
    assert!(context.contains("claude-code session peer"), "{context}");
    assert!(context.contains("more session(s) not shown"), "{context}");
    assert_upload_section(&context);

    let shown = peers_in(&context);
    assert!(!shown.is_empty() && shown.len() < 6, "{context}");

    // The peers the cap left out were never told (issue #222): the next turn
    // tells them, as a briefing of changes only, within the same cap.
    let again = w.prompt("claude-code", "alice", "Why does upload fail?", &[]);
    assert_eq!(again.coverage, "briefed", "{}", again.stdout);
    let context = again.context.unwrap();
    assert!(context.len() <= 2400, "{}: {context}", context.len());
    assert!(context.contains("What changed since your last briefing"));
    assert!(context.contains("unchanged item(s) from earlier briefings"));
    assert_upload_section(&context);
    let retold = peers_in(&context);
    assert!(retold.iter().all(|peer| !shown.contains(peer)), "{context}");
    assert_eq!(shown.len() + retold.len(), 6, "{context}");

    // Now nothing about the peers is new, so only the code section comes again.
    let third = w.prompt("claude-code", "alice", "Why does upload fail?", &[]);
    assert_eq!(third.coverage, "code_briefed");
    let context = third.context.unwrap();
    assert!(context.starts_with(HEADER), "{context}");
}

#[test]
fn a_changes_only_briefing_and_the_code_section_share_the_cap() {
    let w = World::new();
    w.wait_for_index();
    w.writes("peer0", "first", "first.txt");
    let run = w.prompt("claude-code", "alice", "Why does upload fail?", &[]);
    assert_eq!(run.coverage, "briefed", "{}", run.stdout);
    // Then six peers do new work with long file names: every line is news,
    // and the changes alone would fill nearly the whole cap.
    for peer in 0..6 {
        for file in 0..2 {
            let name = format!("later-{peer}-{file}-{}.txt", "n".repeat(140));
            w.writes(&format!("peer{peer}"), &format!("q{peer}-{file}"), &name);
        }
    }
    let again = w.prompt("claude-code", "alice", "Why does upload fail?", &[]);
    assert_eq!(again.coverage, "briefed", "{}", again.stdout);
    let context = again.context.unwrap();
    assert!(context.len() <= 2400, "{}: {context}", context.len());
    assert!(
        context.contains("What changed since your last briefing"),
        "{context}"
    );
    assert!(context.contains("more session(s) not shown"), "{context}");
    assert_upload_section(&context);
}

/// The peer session IDs a briefing's lines name.
fn peers_in(context: &str) -> Vec<String> {
    (0..6)
        .map(|n| format!("peer{n}"))
        .filter(|peer| context.contains(&format!("claude-code session {peer} ")))
        .collect()
}

#[test]
fn the_daemon_refuses_an_unbounded_code_context_query_and_records_none() {
    let w = World::new();
    let ask = |names: Value, limit: u32| {
        let request = json!({"version": 3, "request_id": "read", "command":
            {"op": "graph_context", "workspace_id": "app", "names": names, "limit": limit}});
        let (_, text) = w.cli(&["request", &request.to_string(), "--json"]);
        serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone()
    };
    let too_many: Vec<String> = (0..13).map(|n| format!("name{n}")).collect();
    for (names, limit) in [
        (json!([]), 3),
        (json!(too_many), 3),
        (json!(["has space"]), 3),
        (json!(["upload", "upload"]), 3),
        (json!(["upload"]), 0),
        (json!(["upload"]), 4),
    ] {
        let outcome = ask(names.clone(), limit);
        assert_eq!(
            outcome["code"], "invalid_graph_context",
            "{names} {limit}: {outcome:#}"
        );
    }
    w.wait_for_index();
    let outcome = ask(json!(["upload", "unrelated", "missing_name"]), 3);
    assert_eq!(outcome["type"], "graph_context", "{outcome:#}");
    let found: Vec<&str> = outcome["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        found,
        ["upload", "unrelated"],
        "only defined names, in order"
    );
    assert_eq!(outcome["coverage"]["scanned"], true);
    // A read is answered, never replayed: the same request ID asks again.
    assert_eq!(ask(json!(["upload"]), 3)["matches"][0]["name"], "upload");
    let (_, events) = w.cli(&["events", "--limit", "1000", "--json"]);
    assert!(!events.contains("missing_name"), "{events}");
}

const POINTS: &str = r#"task points at your files: "src/http.rs";"#;

/// The peer line about `who` in a briefing, which must be there.
fn line_about<'a>(context: &'a str, who: &str) -> &'a str {
    context
        .lines()
        .find(|line| line.starts_with(&format!("- {who} ")))
        .unwrap_or_else(|| panic!("no line about {who}: {context}"))
}

fn assert_bounded(context: &str) {
    assert!(context.len() <= 2400, "{}: {context}", context.len());
    for line in context.lines() {
        assert!(line.len() <= 700, "{}: {line}", line.len());
    }
}

/// Roadmap C8: two sessions whose prompts name `parse_header` are each told
/// of the other before either writes. The second is told in its prompt's
/// briefing, the first at its next delivery point: mid-turn on Claude Code,
/// its next prompt on Codex.
#[test]
fn two_prompts_naming_one_function_tell_each_session_of_the_other_before_either_writes() {
    for (first, second) in [("claude-code", "codex"), ("codex", "claude-code")] {
        let w = World::new();
        w.wait_for_index();
        let alice = w.prompt(
            first,
            "alice",
            "make `parse_header` accept folded lines",
            &[],
        );
        assert_eq!(alice.coverage, "code_briefed", "{first}: no peers yet");
        // The second line of a prompt never leaves the session.
        let bob = w.prompt(
            second,
            "bob",
            "fix the 400 from parse_header on empty values\nzebra_private_detail",
            &[],
        );
        assert_eq!(bob.coverage, "briefed", "{first} then {second}");
        let context = bob.context.unwrap();
        assert_bounded(&context);
        let line = line_about(&context, &format!("{first} session alice"));
        assert!(
            line.contains(&format!(r#"in this checkout on branch "main": {POINTS}"#)),
            "{line}"
        );
        assert!(
            line.contains(r#"task: "make `parse_header` accept folded lines";"#),
            "{line}"
        );
        assert!(!line.contains("editing now") && !line.contains("edited:"));

        // The first session's next delivery point, once.
        let told = if first == "claude-code" {
            let told = w.batch("alice", "call-1");
            assert_eq!(told.coverage, "batch_briefed", "{}", told.stdout);
            let again = w.batch("alice", "call-2");
            assert_eq!(again.coverage, "batch_recorded", "{}", again.stdout);
            told
        } else {
            let told = w.prompt(first, "alice", "continue", &[]);
            assert_eq!(told.coverage, "briefed", "{}", told.stdout);
            let again = w.prompt(first, "alice", "continue", &[]);
            assert_eq!(again.coverage, "no_briefing", "{}", again.stdout);
            told
        };
        let context = told.context.unwrap();
        assert_bounded(&context);
        assert!(
            line_about(&context, &format!("{second} session bob")).contains(POINTS),
            "{context}"
        );

        // `status --json` shows each session's targets: paths only.
        let status = w.status();
        for session in [format!("{first}.alice"), format!("{second}.bob")] {
            let targets = &status["outcome"]["state"]["activity"][&session]["prompt_targets"];
            assert_eq!(targets["paths"], json!(["src/http.rs"]), "{status:#}");
        }
        let (_, events) = w.cli(&["events", "--limit", "1000", "--json"]);
        let events: Value = serde_json::from_str(&events).unwrap();
        let recorded: Vec<&Value> = events["outcome"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| &record["event"])
            .filter(|event| event["type"] == "prompt_targets_set")
            .collect();
        assert_eq!(recorded.len(), 2, "{events:#}");
        for event in recorded {
            let mut keys: Vec<&str> = event
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["at_ms", "generation", "paths", "session_id", "type"]);
            assert_eq!(event["paths"], json!(["src/http.rs"]));
        }
        let text = events.to_string();
        assert!(!text.contains("zebra_private_detail"), "{text}");
    }
}

#[test]
fn with_the_prompt_summary_or_code_briefing_off_no_targets_are_recorded() {
    let w = World::new();
    w.wait_for_index();
    let alice = w.prompt("claude-code", "alice", "make parse_header strict", &[]);
    assert_eq!(alice.coverage, "code_briefed");
    for (session, flag) in [("carol", "--prompt-summary"), ("dave", "--code-briefing")] {
        let run = w.prompt(
            "codex",
            session,
            "fix parse_header on empty values",
            &[flag, "off"],
        );
        // Carol is still told about alice's task line, as before; neither
        // is told that alice's task points at its files, which are none.
        if let Some(context) = &run.context {
            assert!(!context.contains("task points at"), "{context}");
        }
        let status = w.status();
        let activity = &status["outcome"]["state"]["activity"][format!("codex.{session}")];
        assert!(activity["prompt_targets"].is_null(), "{status:#}");
    }
    // Alice is told nothing new about them.
    let batch = w.batch("alice", "call-1");
    assert!(
        batch
            .context
            .as_deref()
            .is_none_or(|context| !context.contains("task points at")),
        "{}",
        batch.stdout
    );
    let (_, events) = w.cli(&["events", "--limit", "1000", "--json"]);
    let count = events.matches("prompt_targets_set").count();
    assert_eq!(count, 1, "only alice's: {events}");
}

// Roadmap C2: the ranked code briefing.

/// One `upload` in each of `a/`, `b/` and `c/`, after `src/upload.rs`'s.
const THREE_UPLOADS: [(&str, &str); 3] = [
    ("a/upload.rs", "pub fn upload(a: u8) -> u8 {\n    a\n}\n"),
    ("b/upload.rs", "fn upload(b: u16) -> u16 {\n    b\n}\n"),
    ("c/upload.rs", "pub fn upload(c: u32) -> u32 {\n    c\n}\n"),
];

impl World {
    /// The answer to one raw protocol request.
    fn ask(&self, command: Value) -> Value {
        let request = json!({"version": 3, "request_id": "read", "command": command});
        let (_, text) = self.cli(&["request", &request.to_string(), "--json"]);
        serde_json::from_str::<Value>(&text).unwrap()
    }
}

/// The code section's line about `name`, which must be there.
fn name_line(context: &str, name: &str) -> String {
    let section = &context[context.find(HEADER).expect(context)..];
    section
        .lines()
        .find(|line| line.starts_with(&format!("- \"{name}\": ")))
        .unwrap_or_else(|| panic!("no line about {name}: {section}"))
        .to_owned()
}

const ASK: &str = "make `upload` return the stored size";

#[test]
fn a_definition_in_a_file_the_session_edited_is_listed_first_with_why() {
    let w = World::with(&THREE_UPLOADS);
    w.wait_for_index();
    let before = w.prompt("claude-code", "alice", ASK, &[]);
    let line = name_line(&before.context.unwrap(), "upload");
    assert!(
        line.starts_with(r#"- "upload": function at "a/upload.rs":1, exported: "#),
        "path order before any work: {line}"
    );

    w.writes_content("alice", "edit-c", "c/upload.rs", THREE_UPLOADS[2].1);
    let run = w.prompt("claude-code", "alice", ASK, &[]);
    let line = name_line(&run.context.unwrap(), "upload");
    assert!(
        line.starts_with(
            r#"- "upload": function at "c/upload.rs":1, exported (you edited this file): "pub fn upload(c: u32) -> u32""#
        ),
        "{line}"
    );
    // The exported ones follow in path order. The unexported one in `b/`
    // and the module `src/main.rs` declares rank lower and are counted.
    let a = line.find(r#"also function at "a/upload.rs":1, exported"#);
    let src = line.find(r#"also function at "src/upload.rs":1, exported"#);
    assert!(a.is_some() && a < src, "{line}");
    assert!(!line.contains(r#""b/upload.rs""#), "{line}");
    assert!(line.ends_with("; 2 more definition(s) not shown"), "{line}");
}

#[test]
fn a_definition_whose_path_the_prompt_qualifies_ranks_above_path_order() {
    let w = World::with(&[
        ("x/api/upload.rs", "fn upload() {}\n"),
        ("x/store/upload.rs", "fn upload() {}\n"),
    ]);
    w.wait_for_index();
    for (vendor, session) in [("claude-code", "alice"), ("codex", "bob")] {
        let run = w.prompt(vendor, session, "make store::upload faster", &[]);
        let line = name_line(&run.context.unwrap(), "upload");
        // The exported one first, then the qualified one; `x/api` is counted.
        assert!(
            line.starts_with(r#"- "upload": function at "src/upload.rs":1, exported: "#),
            "{line}"
        );
        assert!(
            line.contains(
                r#"; also function at "x/store/upload.rs":1 (your prompt names its path): "fn upload()""#
            ),
            "{vendor}: {line}"
        );
        assert!(!line.contains("x/api"), "{line}");
    }
}

#[test]
fn a_definition_a_live_peer_is_editing_ranks_above_an_exported_one_and_names_the_peer() {
    let w = World::with(&THREE_UPLOADS);
    w.wait_for_index();
    // Bob (Claude Code) edits the unexported `upload` in `b/`.
    w.writes_content(
        "bob",
        "edit-b",
        "b/upload.rs",
        "fn upload(b: u16) -> u32 {\n    b.into()\n}\n",
    );
    w.wait_for_index();
    // Alice (Codex) is told of it first, with the peer named.
    let run = w.prompt("codex", "alice", "why does upload truncate?", &[]);
    let line = name_line(&run.context.unwrap(), "upload");
    assert!(
        line.starts_with(
            r#"- "upload": function at "b/upload.rs":1 (claude-code session bob is editing it): "#
        ),
        "{line}"
    );
    assert!(
        line.contains(r#"also function at "a/upload.rs":1, exported: "#),
        "{line}"
    );

    // The same order outside a prompt: `graph symbol --rank`.
    let ranked = [
        "graph",
        "symbol",
        "app",
        "upload",
        "--rank",
        "--session",
        "codex.alice",
    ];
    let (code, text) = w.cli(&[&ranked[..], &["--json"]].concat());
    assert_eq!(code, 0, "{text}");
    let outcome = &serde_json::from_str::<Value>(&text).unwrap()["outcome"];
    assert_eq!(outcome["symbols"][0]["path"], "b/upload.rs", "{outcome:#}");
    assert_eq!(
        outcome["symbols"][0]["rank_reasons"],
        json!([{"reason": "peer_editing", "session_id": "claude-code.bob"}])
    );
    let (code, text) = w.cli(&ranked);
    assert_eq!(code, 0, "{text}");
    assert!(text.starts_with("b/upload.rs:1 function upload"), "{text}");
    assert!(
        text.contains("ranked by: session claude-code.bob is editing it"),
        "{text}"
    );
    // A session without `--rank` is refused, not ignored.
    let (code, _) = w.cli(&[
        "graph",
        "symbol",
        "app",
        "upload",
        "--session",
        "codex.alice",
    ]);
    assert_ne!(code, 0);
}

#[test]
fn each_definition_is_used_by_at_most_five_ranked_files_then_counted() {
    // Seven files call `upload`: `m1` once, `m2` twice ... `m7` seven times.
    let callers: Vec<(String, String)> = (1..=7)
        .map(|n| {
            let calls = "    upload(1);\n".repeat(n);
            (format!("m/m{n}.rs"), format!("fn m{n}() {{\n{calls}}}\n"))
        })
        .collect();
    let extra: Vec<(&str, &str)> = callers
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    let w = World::with(&extra);
    w.wait_for_index();
    // Alice edited `m1`, the least-mentioning; peer bob edits `m2`.
    w.writes_content(
        "alice",
        "edit-m1",
        "m/m1.rs",
        "fn m1() {\n    upload(2);\n}\n",
    );
    let m2 = callers[1].1.replace('1', "3");
    w.writes_content("bob", "edit-m2", "m/m2.rs", &m2);
    w.wait_for_index();

    let outcome = w.ask(json!({"op": "graph_context", "workspace_id": "app",
        "names": ["upload"], "limit": 3, "rank": true, "session_id": "claude-code.alice"}));
    let definition = &outcome["outcome"]["matches"][0]["definitions"][0];
    let dependents: Vec<&str> = definition["dependents"]
        .as_array()
        .unwrap_or_else(|| panic!("{outcome:#}"))
        .iter()
        .map(|dependent| dependent["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        dependents,
        ["m/m1.rs", "m/m2.rs", "m/m7.rs", "m/m6.rs", "m/m5.rs"],
        "own, peer's, then by mention count: {outcome:#}"
    );
    // Seven callers and `src/main.rs`; the definition's own file is not one.
    assert_eq!(definition["dependent_count"], 8, "{outcome:#}");
    assert_eq!(definition["dependents"][2]["mentions"], 7);

    let run = w.prompt("claude-code", "alice", ASK, &[]);
    let line = name_line(&run.context.unwrap(), "upload");
    assert!(
        line.contains(
            r#"; used by "m/m1.rs" (1, you edited it), "m/m2.rs" (2, claude-code session bob is editing it), "m/m7.rs" (7), "m/m6.rs" (6), "m/m5.rs" (5) and 3 more file(s)"#
        ),
        "{line}"
    );
}

#[test]
fn without_rank_the_code_context_answer_is_unchanged() {
    let w = World::with(&THREE_UPLOADS);
    w.wait_for_index();
    w.writes_content(
        "alice",
        "edit-c",
        "c/upload.rs",
        "pub fn upload(c: u32) -> u64 {\n    c as u64\n}\n",
    );
    w.wait_for_index();
    let outcome = w.ask(json!({"op": "graph_context", "workspace_id": "app",
        "names": ["upload"], "limit": 3}));
    let matched = &outcome["outcome"]["matches"][0];
    let keys: Vec<&String> = matched.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["name", "definitions", "mentioned_in", "truncated"],
        "{outcome:#}"
    );
    let paths: Vec<&str> = matched["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|definition| {
            let fields = definition.as_object().unwrap();
            for added in ["rank_reasons", "dependents", "dependent_count"] {
                assert!(!fields.contains_key(added), "{definition:#}");
            }
            definition["path"].as_str().unwrap()
        })
        .collect();
    assert_eq!(paths, ["a/upload.rs", "b/upload.rs", "c/upload.rs"]);
    assert_eq!(matched["truncated"], true, "src/upload.rs is left out");
    // Ranking fields without `rank` are refused.
    let refused = w.ask(json!({"op": "graph_context", "workspace_id": "app",
        "names": ["upload"], "limit": 3, "session_id": "claude-code.alice"}));
    assert_eq!(
        refused["outcome"]["code"], "invalid_graph_context",
        "{refused:#}"
    );
}

#[test]
fn a_ranked_code_context_query_records_nothing() {
    let w = World::with(&THREE_UPLOADS);
    w.wait_for_index();
    w.writes_content("alice", "edit-c", "c/upload.rs", THREE_UPLOADS[2].1);
    w.wait_for_index();
    let (_, before) = w.cli(&["events", "--limit", "1000", "--json"]);
    let generation = w.status()["generation"].clone();
    let ranked = json!({"op": "graph_context", "workspace_id": "app",
        "names": ["upload", "zebra_marker_name"], "limit": 3, "rank": true,
        "session_id": "claude-code.alice", "qualifiers": ["zebra_qualifier"]});
    for _ in 0..2 {
        let answer = w.ask(ranked.clone());
        assert_eq!(answer["replayed"], false, "{answer:#}");
        assert_eq!(answer["outcome"]["type"], "graph_context", "{answer:#}");
        let first = &answer["outcome"]["matches"][0]["definitions"][0];
        assert_eq!(first["path"], "c/upload.rs", "{answer:#}");
    }
    let (_, after) = w.cli(&["events", "--limit", "1000", "--json"]);
    assert_eq!(before, after, "no event");
    assert_eq!(w.status()["generation"], generation);
    assert!(!after.contains("zebra"), "{after}");
}

#[test]
fn a_ranked_code_section_stays_within_its_share_and_counts_what_it_left_out() {
    // Twelve names, each defined in three files, and each called from seven
    // files: more than one name's line or the section can hold.
    let long = "d".repeat(30);
    let mut files: Vec<(String, String)> = Vec::new();
    for n in 0..12 {
        for d in 0..3 {
            files.push((
                format!("defs/{long}/n{n}_{d}.rs"),
                format!("pub fn name_{n}(argument: {}) {{}}\n", "u".repeat(60)),
            ));
        }
    }
    for c in 0..7 {
        let calls: String = (0..12).map(|n| format!("    name_{n}();\n")).collect();
        files.push((
            format!("callers/{long}/c{c}.rs"),
            format!("fn c{c}() {{\n{calls}}}\n"),
        ));
    }
    let extra: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    let w = World::with(&extra);
    w.wait_for_index();
    let names: String = (0..12).map(|n| format!("name_{n} ")).collect();
    // Alone, the section has the whole cap.
    let alone = w.prompt("claude-code", "alice", &names, &[]);
    let context = alone.context.unwrap();
    assert!(context.len() <= 2400, "{}: {context}", context.len());
    assert!(context.starts_with(HEADER), "{context}");
    // Peers with long file names fill most of the cap.
    for peer in 0..6 {
        for file in 0..2 {
            let name = format!("notes-{peer}-{file}-{}.txt", "n".repeat(140));
            w.writes(&format!("peer{peer}"), &format!("p{peer}-{file}"), &name);
        }
    }
    let run = w.prompt("claude-code", "alice", &names, &[]);
    let context = run.context.unwrap();
    assert!(context.len() <= 2400, "{}: {context}", context.len());
    assert!(
        context.starts_with("LangSpan: other agent sessions"),
        "{context}"
    );
    let section = &context[context.find(HEADER).expect(&context)..];
    assert!(section.len() <= 1000, "{}: {section}", section.len());
    // Whole lines and whole parts only: nothing cut, everything counted.
    assert!(!section.contains('…'), "{section}");
    assert!(section.contains("more name(s) not shown"), "{section}");
    let line = name_line(&context, "name_0");
    assert!(line.contains("more definition(s) not shown"), "{line}");
    assert!(line.contains("more file(s)"), "{line}");
    for line in section.lines() {
        assert!(line.len() <= 500, "{}: {line}", line.len());
    }
}

#[test]
fn the_sessions_own_definition_ranks_first_past_a_peers_file_of_many_same_name_definitions() {
    // Review comment 4709803063: peer bob's early-sorting generated file
    // holds more `upload` definitions than the ranking reads in path order.
    let generated = "def upload():\n    pass\n".repeat(300);
    let own = "pub fn upload(z: u64) -> u64 {\n    z\n}\n";
    let w = World::with(&[("a/gen.py", &generated), ("z/upload.rs", own)]);
    w.wait_for_index();
    w.writes_content("bob", "edit-gen", "a/gen.py", &generated);
    w.writes_content("alice", "edit-z", "z/upload.rs", own);
    w.wait_for_index();
    let outcome = w.ask(json!({"op": "graph_context", "workspace_id": "app",
        "names": ["upload"], "limit": 3, "rank": true, "session_id": "claude-code.alice"}));
    let matched = &outcome["outcome"]["matches"][0];
    let paths: Vec<&str> = matched["definitions"]
        .as_array()
        .unwrap_or_else(|| panic!("{outcome:#}"))
        .iter()
        .map(|definition| definition["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["z/upload.rs", "a/gen.py", "a/gen.py"],
        "own, then the peer's: {outcome:#}"
    );
    assert_eq!(matched["definition_count"], 303, "{outcome:#}");
    // The same for `graph symbol --rank`, and for the prompt's code section.
    let (code, text) = w.cli(&[
        "graph",
        "symbol",
        "app",
        "upload",
        "--rank",
        "--session",
        "claude-code.alice",
        "--json",
    ]);
    assert_eq!(code, 0, "{text}");
    let outcome = &serde_json::from_str::<Value>(&text).unwrap()["outcome"];
    assert_eq!(outcome["symbols"][0]["path"], "z/upload.rs", "{outcome:#}");
    let run = w.prompt("claude-code", "alice", ASK, &[]);
    let line = name_line(&run.context.unwrap(), "upload");
    assert!(
        line.starts_with(
            r#"- "upload": function at "z/upload.rs":1, exported (you edited this file)"#
        ),
        "{line}"
    );
}

#[test]
fn an_exported_definition_past_many_private_ones_in_the_sessions_own_file_ranks_first() {
    // The same cutoff within one file: 300 private `upload`s, then the
    // exported one, all in a file alice edited.
    let own = format!("{}pub fn upload() {{}}\n", "fn upload() {}\n".repeat(300));
    let w = World::with(&[("a/gen.rs", &own)]);
    w.wait_for_index();
    w.writes_content("alice", "edit-gen", "a/gen.rs", &own);
    w.wait_for_index();
    let outcome = w.ask(json!({"op": "graph_context", "workspace_id": "app",
        "names": ["upload"], "limit": 1, "rank": true, "session_id": "claude-code.alice"}));
    let first = &outcome["outcome"]["matches"][0]["definitions"][0];
    assert_eq!(first["path"], "a/gen.rs", "{outcome:#}");
    assert_eq!(first["line"], 301, "{outcome:#}");
    assert_eq!(
        first["rank_reasons"],
        json!([{"reason": "own_file", "edited": true}, {"reason": "exported"}])
    );
}

#[test]
fn a_code_context_with_long_dependent_paths_fits_one_frame_and_counts_the_rest() {
    // Review comment 4709803050: twelve names, three definitions each, five
    // callers each, every path about 400 bytes. Five dependents under every
    // definition exceed one 64 KiB frame.
    let deep = format!("{}/{}", "d".repeat(190), "e".repeat(190));
    let mut files: Vec<(String, String)> = Vec::new();
    for n in 0..12 {
        for d in 0..3 {
            files.push((
                format!("defs/{deep}/n{n}_{d}.rs"),
                format!("pub fn name_{n}() {{}}\n"),
            ));
        }
    }
    for c in 0..5 {
        let calls: String = (0..12).map(|n| format!("    name_{n}();\n")).collect();
        files.push((
            format!("callers/{deep}/c{c}.rs"),
            format!("fn c{c}() {{\n{calls}}}\n"),
        ));
    }
    let extra: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    let w = World::with(&extra);
    w.wait_for_index();
    let names: Vec<String> = (0..12).map(|n| format!("name_{n}")).collect();
    for rank in [true, false] {
        let outcome = w.ask(json!({"op": "graph_context", "workspace_id": "app",
            "names": names, "limit": 3, "rank": rank}));
        assert_eq!(outcome["outcome"]["type"], "graph_context", "{outcome:#}");
        let matches = outcome["outcome"]["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 12, "every name kept: {outcome:#}");
        for matched in matches {
            let definitions = matched["definitions"].as_array().unwrap();
            assert!(!definitions.is_empty(), "{matched:#}");
            if rank {
                assert_eq!(matched["definition_count"], 3, "{matched:#}");
                for definition in definitions {
                    assert_eq!(definition["dependent_count"], 5, "{definition:#}");
                }
            }
        }
    }
    let run = w.prompt("claude-code", "alice", &names.join(" "), &[]);
    let context = run.context.expect("a code section");
    name_line(&context, "name_0");
}
