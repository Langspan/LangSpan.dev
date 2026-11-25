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
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "init"]);
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self { base, daemon: None };
        world.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
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
    /// The index covers all three source files at their current content.
    fn wait_for_index(&self) {
        let deadline = Instant::now() + WAIT;
        loop {
            let (_, text) = self.cli(&["graph", "symbol", "app", "upload", "--json"]);
            let outcome = serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone();
            if outcome["coverage"]["current_files"] == 3 {
                return;
            }
            assert!(Instant::now() < deadline, "never indexed: {outcome:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn hook(&self, vendor: &str, extra: &[&str], event: &Value) -> Run {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
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
    /// A complete reported Claude Code write of a new file.
    fn writes(&self, session: &str, call: &str, file: &str) {
        let mut event = json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file), "content": "x\n"},
        });
        assert_eq!(self.hook("claude-code", &[], &event).coverage, "admitted");
        fs::write(self.work().join(file), "x\n").unwrap();
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
    assert!(
        section.contains(r#"mentioned in "src/main.rs""#),
        "{section}"
    );
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

    // The peers have not changed, so only the code section comes again.
    let again = w.prompt("claude-code", "alice", "Why does upload fail?", &[]);
    assert_eq!(again.coverage, "code_briefed");
    let context = again.context.unwrap();
    assert!(context.starts_with(HEADER), "{context}");
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
