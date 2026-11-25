#![cfg(unix)]
//! The derived index and interface-change notices, through a real daemon,
//! the real observer and indexer, and files that really change on disk.

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

struct World {
    base: PathBuf,
    daemon: Option<Child>,
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
                "ls-graph-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let work = base.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::write(
            work.join("src/upload.rs"),
            "pub fn upload(path: &str) -> bool {\n    !path.is_empty()\n}\n\npub fn gate(user: &str) -> bool {\n    user == \"admin\"\n}\n",
        )
        .unwrap();
        fs::write(
            work.join("src/main.rs"),
            "mod upload;\nfn main() {\n    if upload::gate(\"admin\") {\n        upload::upload(\"a.txt\");\n    }\n}\n",
        )
        .unwrap();
        fs::write(
            work.join("src/other.py"),
            "def unrelated():\n    return 1\n",
        )
        .unwrap();
        fs::write(work.join("README.md"), "upload gate\n").unwrap();
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "init"]);
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self { base, daemon: None };
        world.start_daemon();
        let root = world.work();
        assert_eq!(
            world
                .cli(&[
                    "workspace",
                    "add",
                    "app",
                    root.to_str().unwrap(),
                    "--request-id",
                    "ws"
                ])
                .0,
            0
        );
        world
    }
    fn start_daemon(&mut self) {
        self.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(self.base.join("state"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    /// `graph symbol --detail`, waiting until `done` holds.
    fn detail(&self, what: &str, name: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let (_, text) = self.cli(&["graph", "symbol", "app", name, "--detail", "--json"]);
            let outcome = serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone();
            if done(&outcome) {
                return outcome;
            }
            assert!(Instant::now() < deadline, "never saw {what}: {outcome:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
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
    /// One request written to the daemon's socket as it is, as any client
    /// build would send it.
    fn raw(&self, request: &Value) -> Value {
        use std::io::{BufRead, BufReader};
        let mut stream =
            std::os::unix::net::UnixStream::connect(self.base.join("state/daemon.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    fn graph(&self, query: &str, subject: &str) -> Value {
        let (_, text) = self.cli(&["graph", query, "app", subject, "--json"]);
        serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone()
    }
    fn wait_for(
        &self,
        what: &str,
        query: &str,
        subject: &str,
        done: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let outcome = self.graph(query, subject);
            if done(&outcome) {
                return outcome;
            }
            assert!(Instant::now() < deadline, "never saw {what}: {outcome:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// The hook's coverage, its additional context, and for a failure
    /// message: its detail (which ends with the hook's own elapsed ms) and
    /// the process's wall time.
    fn hook(&self, event: Value) -> (String, Option<String>, String) {
        self.hook_with(event, &[])
    }
    /// The same, with more hook options (`--prompt-summary off`).
    fn hook_with(&self, event: Value, options: &[&str]) -> (String, Option<String>, String) {
        let started = Instant::now();
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
            .args(options)
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
        let wall = started.elapsed();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        let reply: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        (
            diagnostic["coverage"].as_str().unwrap().into(),
            reply.and_then(|reply| {
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_owned)
            }),
            format!(
                "detail {:?}; hook process took {} ms",
                diagnostic["detail"].as_str().unwrap_or_default(),
                wall.as_millis()
            ),
        )
    }
    /// A complete reported edit that really rewrites the file.
    fn edits(&self, session: &str, call: &str, file: &str, content: &str) {
        let mut event = json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file), "content": content},
        });
        let (coverage, _, detail) = self.hook(event.clone());
        assert_eq!(coverage, "admitted", "PreToolUse {call}: {detail}");
        fs::write(self.work().join(file), content).unwrap();
        event["hook_event_name"] = json!("PostToolUse");
        let (coverage, _, detail) = self.hook(event);
        assert_eq!(coverage, "observed", "PostToolUse {call}: {detail}");
    }
    fn prompt(&self, session: &str) -> Option<String> {
        self.hook(json!({"session_id": session, "cwd": self.work(),
                         "hook_event_name": "UserPromptSubmit", "prompt": "continue"}))
            .1
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop_daemon();
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn paths(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| format!("{}:{}", item["path"].as_str().unwrap(), item["line"]))
        .collect()
}

#[test]
fn definitions_mentions_and_impact_are_answered_with_coverage_and_follow_the_files() {
    let w = World::new();
    // Before the first scan nothing is known, and the answer says so.
    let empty = w.graph("symbol", "no_such_name");
    assert!(
        empty["type"] == "error"
            || empty["coverage"]["scanned"] == false
            || empty["symbols"] == json!([]),
        "{empty:#}"
    );
    let found = w.wait_for("the definition of upload", "symbol", "upload", |outcome| {
        outcome["symbols"]
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty())
            && outcome["coverage"]["current_files"] == 3
    });
    // `mod upload;` in main.rs is also a symbol named upload: a name match
    // returns both, each with its kind.
    let kinds: Vec<&str> = found["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["module", "function"]);
    let symbol = &found["symbols"][1];
    assert_eq!(symbol["path"], "src/upload.rs");
    assert_eq!(symbol["signature"], "pub fn upload(path: &str) -> bool");
    assert_eq!(symbol["exported"], true);
    assert_eq!(
        symbol["digest"].as_str().unwrap().len(),
        64,
        "tied to exact content"
    );
    assert_eq!(
        found["coverage"]["indexable_files"], 3,
        "README.md is not source"
    );

    let mentions = w.graph("references", "upload");
    assert_eq!(
        paths(&mentions["references"]),
        ["src/main.rs:3", "src/main.rs:4"]
    );
    let impact = w.graph("impact", "src/upload.rs");
    assert_eq!(
        impact["symbols"].as_array().unwrap().len(),
        2,
        "upload and gate"
    );
    assert!(
        paths(&impact["references"])
            .iter()
            .all(|path| path.starts_with("src/main.rs"))
    );
    let bounded = w
        .cli(&[
            "graph",
            "references",
            "app",
            "upload",
            "--limit",
            "1",
            "--json",
        ])
        .1;
    let bounded: Value = serde_json::from_str(&bounded).unwrap();
    assert_eq!(
        bounded["outcome"]["references"].as_array().unwrap().len(),
        1
    );
    assert_eq!(bounded["outcome"]["truncated"], true);
    let (code, text) = w.cli(&["graph", "symbol", "app", "upload", "--limit", "0"]);
    assert_eq!(code, 1, "{text}");

    // A new file, a changed file and a deleted file are followed.
    fs::write(
        w.work().join("src/new.go"),
        "package app\nfunc Fresh() int { return upload() }\n",
    )
    .unwrap();
    fs::remove_file(w.work().join("src/other.py")).unwrap();
    w.wait_for("the new file", "symbol", "Fresh", |outcome| {
        outcome["symbols"]
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty())
    });
    w.wait_for("the deletion", "symbol", "unrelated", |outcome| {
        outcome["symbols"] == json!([]) && outcome["coverage"]["indexable_files"] == 3
    });
    // The index is derived: delete it, restart, and it is rebuilt.
    let human = w.cli(&["graph", "impact", "app", "src/upload.rs"]).1;
    assert!(human.contains("by name, not resolved uses"), "{human}");
}

/// The `upload` function of `src/upload.rs` in the detail test: it calls
/// `open` twice and `hash` once.
const UPLOAD: &str = "pub fn upload(path: &str) -> bool {\n    let file = open(path);\n    let again = open(path);\n    hash(file, again)\n}\n\npub fn gate(user: &str) -> bool {\n    user == \"admin\"\n}\n";

fn upload_function(outcome: &Value) -> Option<&Value> {
    outcome["symbols"]
        .as_array()?
        .iter()
        .find(|symbol| symbol["kind"] == "function")
}

#[test]
fn a_detailed_lookup_adds_calls_callers_digest_notes_and_check_and_is_rebuilt() {
    let mut w = World::new();
    fs::write(w.work().join("src/upload.rs"), UPLOAD).unwrap();
    fs::write(
        w.work().join("src/api.rs"),
        "fn serve() {\n    upload::upload(\"b\");\n}\n",
    )
    .unwrap();
    let ready = |outcome: &Value| {
        outcome["coverage"]["current_files"] == 4
            && upload_function(outcome).is_some_and(|symbol| symbol["calls"] != json!([]))
    };
    let found = w.detail("upload with its calls", "upload", ready);
    assert_eq!(found["resolution"], "name", "{found:#}");
    let upload = upload_function(&found).unwrap();
    assert_eq!(
        upload["calls"],
        json!([{"name": "open", "count": 2, "line": 2}, {"name": "hash", "count": 1, "line": 4}])
    );
    assert_eq!(
        upload["callers"],
        json!({"count": 2, "sites": ["src/api.rs:2", "src/main.rs:4"]})
    );
    assert_eq!(upload["notes"], json!([]));
    assert!(
        upload.get("last_check").is_none(),
        "no check yet: {upload:#}"
    );
    assert!(
        !upload.to_string().contains("let file"),
        "no source body is returned"
    );

    // A note on the file appears; a workspace-wide one does not.
    for (id, text, paths) in [
        (
            "note-file",
            "The response field is named upload_id.",
            &["src/upload.rs"][..],
        ),
        ("note-wide", "Release freeze until Friday", &[][..]),
    ] {
        let mut args = vec!["remember", "app", text];
        args.extend(paths);
        args.extend(["--request-id", id]);
        let (code, text) = w.cli(&args);
        assert_eq!(code, 0, "{text}");
    }
    // A check on the working tree as it is.
    let (code, text) = w.cli(&["check", "run", "app", "--name", "unit", "--", "true"]);
    assert_eq!(code, 0, "{text}");
    let tree = text
        .split("passed on tree ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_hexdigit()).next())
        .unwrap_or_else(|| panic!("no tree in: {text}"))
        .to_owned();
    let found = w.detail("the note and the check", "upload", |outcome| {
        upload_function(outcome).is_some_and(|symbol| symbol.get("last_check").is_some())
    });
    let upload = upload_function(&found).unwrap().clone();
    assert_eq!(
        upload["notes"],
        json!([{"id": "note-file", "text": "The response field is named upload_id."}])
    );
    assert_eq!(
        upload["last_check"],
        json!({"name": "unit", "result": "passed", "tree": tree})
    );
    let human = w.cli(&["graph", "symbol", "app", "upload", "--detail"]).1;
    assert!(human.contains("open ×2 (line 2)"), "{human}");
    assert!(
        human.contains(r#"note note-file: "The response field is named upload_id.""#),
        "quoted: {human}"
    );

    // The index is derived: delete it, restart, and the same answer is
    // rebuilt, calls included.
    w.stop_daemon();
    let state = w.base.join("state");
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(state.join(format!("graph.sqlite3{suffix}")));
    }
    w.start_daemon();
    let rebuilt = w.detail("the rebuilt index", "upload", |outcome| {
        outcome["coverage"]["current_files"] == 4
            && upload_function(outcome).is_some_and(|symbol| symbol.get("last_check").is_some())
    });
    assert_eq!(upload_function(&rebuilt).unwrap(), &upload);

    // Editing another function in the file changes the file's digest but
    // not upload's own; the check no longer holds this content.
    fs::write(
        w.work().join("src/upload.rs"),
        UPLOAD.replace("\"admin\"", "\"root\""),
    )
    .unwrap();
    let edited = w.detail("the edit to gate", "upload", |outcome| {
        upload_function(outcome).is_some_and(|symbol| symbol["digest"] != upload["digest"])
    });
    let after = upload_function(&edited).unwrap();
    assert_eq!(after["body_digest"], upload["body_digest"]);
    assert_eq!(after["body_digest"].as_str().unwrap().len(), 64);
    assert!(after.get("last_check").is_none(), "{after:#}");

    // Without --detail the answer is today's, field for field.
    let plain = w.graph("symbol", "upload");
    let keys =
        |value: &Value| -> Vec<String> { value.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys(&plain),
        ["type", "symbols", "references", "coverage", "truncated"]
    );
    assert_eq!(
        keys(upload_function(&plain).unwrap()),
        [
            "path",
            "name",
            "kind",
            "line",
            "end_line",
            "signature",
            "exported",
            "digest"
        ]
    );
}

#[test]
fn a_changed_exported_signature_is_reported_to_the_session_whose_files_mention_it() {
    let w = World::new();
    w.wait_for("the first index", "symbol", "upload", |outcome| {
        outcome["coverage"]["current_files"] == 3
    });
    // Alice works on main.rs, which calls upload. Carol works on other.py.
    let main = fs::read_to_string(w.work().join("src/main.rs")).unwrap();
    w.edits("alice", "a1", "src/main.rs", &format!("{main}// touched\n"));
    w.edits(
        "carol",
        "c1",
        "src/other.py",
        "def unrelated():\n    return 2\n",
    );
    w.wait_for("alice's edit indexed", "references", "upload", |outcome| {
        outcome["coverage"]["current_files"] == 3
    });
    assert!(
        w.prompt("alice")
            .is_none_or(|context| !context.contains("Interface change"))
    );

    // Bob changes only a body. That is not an interface change.
    let upload = fs::read_to_string(w.work().join("src/upload.rs")).unwrap();
    w.edits(
        "bob",
        "b1",
        "src/upload.rs",
        &upload.replace("!path.is_empty()", "path.len() > 0"),
    );
    w.wait_for("the body edit indexed", "symbol", "upload", |outcome| {
        outcome["coverage"]["current_files"] == 3
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        w.prompt("alice")
            .is_none_or(|context| !context.contains("Interface change"))
    );

    // Bob changes upload's signature and removes gate.
    w.edits(
        "bob",
        "b2",
        "src/upload.rs",
        "pub fn upload(path: &str, retries: u32) -> bool {\n    retries > 0 && path.len() > 0\n}\n",
    );
    let deadline = Instant::now() + WAIT;
    let context = loop {
        if let Some(context) = w
            .prompt("alice")
            .filter(|context| context.contains("Interface change"))
        {
            break context;
        }
        assert!(Instant::now() < deadline, "alice was never told");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(
        context.contains(r#"Interface change in "src/upload.rs""#),
        "{context}"
    );
    assert!(context.contains("claude-code.bob"), "{context}");
    assert!(
        context.contains(r#"function "upload" changed from "pub fn upload(path: &str) -> bool" to "pub fn upload(path: &str, retries: u32) -> bool""#),
        "{context}"
    );
    assert!(
        context.contains(r#"function "gate" was removed or renamed"#),
        "{context}"
    );
    assert!(context.contains(r#"Your files that mention these names (a name match, not a checked use): "src/main.rs""#), "{context}");
    // Carol's file mentions neither name, and Bob made the change himself.
    for session in ["carol", "bob"] {
        assert!(
            w.prompt(session)
                .is_none_or(|context| !context.contains("Interface change")),
            "{session} has nothing at stake"
        );
    }
    // Carol and Bob were just prompted, which is news about them. After
    // that is delivered, nothing has changed and nothing is repeated.
    let _ = w.prompt("alice");
    assert!(w.prompt("alice").is_none());
}

/// Wait until `session`'s next prompt carries an interface notice holding
/// `text`.
fn told(w: &World, session: &str, text: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(context) = w
            .prompt(session)
            .filter(|context| context.contains("Interface change") && context.contains(text))
        {
            return context;
        }
        assert!(Instant::now() < deadline, "{session} was never told {text}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn a_second_same_named_definition_is_told_as_added_and_losing_it_as_removed() {
    let w = World::new();
    w.wait_for("the first index", "symbol", "upload", |outcome| {
        outcome["coverage"]["current_files"] == 3
    });
    let upload = fs::read_to_string(w.work().join("src/upload.rs")).unwrap();
    let one = format!(
        "{upload}\npub struct A;\npub struct B;\n\nimpl A {{\n    pub fn text(&self) -> String {{\n        String::new()\n    }}\n}}\n"
    );
    let two = format!(
        "{one}\nimpl B {{\n    pub fn text(&self) -> String {{\n        \"b\".into()\n    }}\n}}\n"
    );
    // A new name is not news to anyone: there is no notice for it.
    w.edits("bob", "b1", "src/upload.rs", &one);
    let main = fs::read_to_string(w.work().join("src/main.rs")).unwrap();
    w.edits(
        "alice",
        "a1",
        "src/main.rs",
        &main.replace("fn main() {", "fn main() {\n    let _ = upload::A.text();"),
    );
    w.wait_for("one text method", "symbol", "text", |outcome| {
        outcome["symbols"].as_array().is_some_and(|d| d.len() == 1)
            && outcome["coverage"]["current_files"] == 3
    });
    w.wait_for("alice's mention", "references", "text", |outcome| {
        paths(&outcome["references"])
            .iter()
            .any(|path| path.starts_with("src/main.rs"))
    });

    // Bob adds a second `text`, on another type, with the same signature.
    w.edits("bob", "b2", "src/upload.rs", &two);
    let added = r#"a second method "text" was added: "pub fn text(&self) -> String" (now 2 definitions with that name)"#;
    let context = told(&w, "alice", added);
    assert!(!context.contains("changed from"), "{context}");

    // Bob takes it away again.
    w.edits("bob", "b3", "src/upload.rs", &one);
    let removed = r#"one of 2 methods "text" was removed: "pub fn text(&self) -> String" (now 1 definition with that name)"#;
    let context = told(&w, "alice", removed);
    assert!(!context.contains("changed from"), "{context}");

    // A real change to the one left is still a changed signature.
    w.edits(
        "bob",
        "b4",
        "src/upload.rs",
        &one.replace("text(&self)", "text(&self, limit: usize)"),
    );
    told(
        &w,
        "alice",
        r#"method "text" changed from "pub fn text(&self) -> String" to "pub fn text(&self, limit: usize) -> String""#,
    );
}

/// `graph history app NAME --json`, waiting until it holds `count` entries.
fn history(w: &World, name: &str, count: usize) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, text) = w.cli(&["graph", "history", "app", name, "--json"]);
        let outcome = serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone();
        if outcome["history"]["entries"]
            .as_array()
            .is_some_and(|entries| entries.len() >= count)
        {
            return outcome["history"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "never saw {count} entries for {name}: {outcome:#}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_definitions_history_shows_who_changed_it_why_and_that_it_was_reverted() {
    let w = World::new();
    w.wait_for("the first index", "symbol", "upload", |outcome| {
        outcome["coverage"]["current_files"] == 3
    });
    let original = fs::read_to_string(w.work().join("src/upload.rs")).unwrap();
    let before = "pub fn upload(path: &str) -> bool";
    let after = "pub fn upload(path: &str, retries: u32) -> bool";

    // Alice, with prompt summaries on, adds a parameter.
    w.hook(
        json!({"session_id": "alice", "cwd": w.work(), "hook_event_name": "UserPromptSubmit",
                  "prompt": "add retries to upload"}),
    );
    w.edits(
        "alice",
        "a1",
        "src/upload.rs",
        &original.replace(before, after),
    );
    history(&w, "upload", 1);
    // Her reason, remembered just after the change.
    let (code, _) = w.cli(&[
        "remember",
        "app",
        "retries make the uploader deadlock under load",
        "src/upload.rs",
        "--request-id",
        "note-1",
    ]);
    assert_eq!(code, 0);

    // Bob, with prompt summaries off, puts the old signature back.
    w.hook_with(
        json!({"session_id": "bob", "cwd": w.work(), "hook_event_name": "UserPromptSubmit",
               "prompt": "put upload back the way it was"}),
        &["--prompt-summary", "off"],
    );
    // He reads Alice's lines first, as LangSpan asks before changing them.
    let (coverage, _, detail) = w.hook(json!({
        "session_id": "bob", "cwd": w.work(), "hook_event_name": "PostToolUse",
        "tool_name": "Read", "tool_use_id": "b0",
        "tool_input": {"file_path": w.work().join("src/upload.rs")},
    }));
    assert_eq!(coverage, "read_recorded", "Read b0: {detail}");
    w.edits("bob", "b1", "src/upload.rs", &original);
    let found = history(&w, "upload", 2);
    let entries = found["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{found:#}");
    let (newest, oldest) = (&entries[0], &entries[1]);
    assert!(newest["seq"].as_u64() > oldest["seq"].as_u64());

    assert_eq!(newest["change"], "signature_changed");
    assert_eq!(newest["before"], after);
    assert_eq!(newest["after"], before);
    assert!(
        newest["by_session"].as_str().unwrap().ends_with("bob"),
        "{newest:#}"
    );
    assert_eq!(newest["reverted"], true);
    assert_eq!(newest["task"], Value::Null, "Bob recorded no task line");

    assert_eq!(oldest["before"], before);
    assert_eq!(oldest["after"], after);
    assert!(
        oldest["by_session"].as_str().unwrap().ends_with("alice"),
        "{oldest:#}"
    );
    assert_eq!(oldest["reverted"], false);
    assert_eq!(oldest["task"], "add retries to upload");
    assert_eq!(
        oldest["notes"][0]["text"],
        "retries make the uploader deadlock under load"
    );
    assert!(oldest["age_ms"].as_u64().is_some());
    assert!(
        found["attribution"]
            .as_str()
            .unwrap()
            .contains("not proof of authorship")
    );

    // The person's view says the same, quoted, and names what a session is.
    let (code, text) = w.cli(&["graph", "history", "app", "upload"]);
    assert_eq!(code, 0, "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].contains(&format!(
            r#""src/upload.rs" function signature changed from {after:?} to {before:?} by session "#
        )) && lines[0].contains("(reverted: back to an earlier signature)")
            && !lines[0].contains("task:"),
        "{text}"
    );
    // The note was made within 30 minutes of both changes, so both show it.
    let note = r#"  note (note-1): "retries make the uploader deadlock under load""#;
    assert_eq!(lines[1], note, "{text}");
    assert!(
        lines[2].contains(&format!(
            r#""src/upload.rs" function signature changed from {before:?} to {after:?} by session "#
        )) && lines[2].contains(r#"; task: "add retries to upload""#),
        "{text}"
    );
    assert_eq!(lines[3], note, "{text}");
    assert_eq!(
        lines.last().copied(),
        Some("Sessions are the last reported editors of the file, not proof of authorship.")
    );
    // A page of one says where to read on, and reading on gives the rest.
    let (_, text) = w.cli(&[
        "graph", "history", "app", "upload", "--limit", "1", "--json",
    ]);
    let page = serde_json::from_str::<Value>(&text).unwrap()["outcome"]["history"].clone();
    assert_eq!(page["truncated"], true);
    assert_eq!(page["next_seq"], newest["seq"]);
    let next = newest["seq"].to_string();
    let (_, text) = w.cli(&[
        "graph", "history", "app", "upload", "--before", &next, "--json",
    ]);
    let rest = serde_json::from_str::<Value>(&text).unwrap()["outcome"]["history"].clone();
    assert_eq!(rest["entries"].as_array().unwrap().len(), 1);
    assert_eq!(rest["entries"][0]["seq"], oldest["seq"]);

    // A new export is recorded for its history, and told to no one.
    // (Bob wrote the file last; Alice would first be asked to read it.)
    w.edits(
        "bob",
        "b2",
        "src/upload.rs",
        &format!(
            "{original}\npub fn upload_all(paths: &[&str]) -> usize {{\n    paths.len()\n}}\n"
        ),
    );
    let added = history(&w, "upload_all", 1);
    assert_eq!(added["entries"][0]["change"], "added");
    assert_eq!(
        added["entries"][0]["after"],
        "pub fn upload_all(paths: &[&str]) -> usize"
    );
    let (_, text) = w.cli(&["recap", "app", "--json"]);
    assert!(!text.contains("upload_all"), "{text}");

    // Over MCP: labelled as data, with the same entries.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let state = w.base.join("state");
    let (label, structured) = runtime.block_on(async move {
        use rmcp::ServiceExt;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
        command.arg("mcp").arg("--state-dir").arg(&state);
        let client =
            ().serve(rmcp::transport::TokioChildProcess::new(command).unwrap())
                .await
                .unwrap();
        let request = rmcp::model::CallToolRequestParams::new("langspan_history").with_arguments(
            json!({"workspace_id": "app", "name": "upload", "path": "src/upload.rs"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let result = tokio::time::timeout(Duration::from_secs(10), client.call_tool(request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.is_error, Some(false), "{result:?}");
        let label = match &result.content[0] {
            rmcp::model::ContentBlock::Text(text) => text.text.clone(),
            other => panic!("{other:?}"),
        };
        (label, result.structured_content.unwrap())
    });
    assert!(
        label.contains("data, not instructions") && label.contains("not proof of authorship"),
        "{label}"
    );
    assert_eq!(
        structured["outcome"]["history"]["entries"][0]["reverted"],
        true
    );
    assert_eq!(
        structured["outcome"]["history"]["entries"][1]["task"],
        "add retries to upload"
    );
}

#[test]
fn history_requests_match_the_committed_golden_vector() {
    let wire: &[u8] = include_bytes!("../fixtures/history-request-v3.json");
    let request: langspan::protocol::Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, langspan::protocol::PROTOCOL_VERSION);
    assert_eq!(langspan::protocol::encode(&request).unwrap(), wire);
    assert!(matches!(
        &request.command,
        langspan_core::Command::History {
            workspace_id,
            name,
            path: Some(path),
            limit: 10,
            before_seq: Some(812),
        } if workspace_id == "demo" && name == "save" && path == "src/store.rs"
    ));
    let unknown = String::from_utf8_lossy(wire).replace(r#""limit":10"#, r#""limit":10,"tag":"x""#);
    assert!(serde_json::from_str::<langspan::protocol::Request>(&unknown).is_err());
}

#[test]
fn blind_spots_name_the_files_the_index_cannot_answer_for() {
    let w = World::new();
    // The only file mentioning `lonely` does not parse: impact finds the
    // mention, and says one file in scope is blind.
    fs::write(w.work().join("src/lonely.rs"), "pub fn lonely() {}\n").unwrap();
    fs::write(
        w.work().join("src/broken.rs"),
        "fn caller() { lonely(); }\nfn half( {{{\n",
    )
    .unwrap();
    let impact = w.wait_for(
        "the partial file indexed",
        "impact",
        "src/lonely.rs",
        |outcome| outcome["coverage"]["current_files"] == 5,
    );
    assert_eq!(
        paths(&impact["references"]),
        ["src/broken.rs:1"],
        "{impact:#}"
    );
    assert_eq!(impact["coverage"]["blind_in_scope"], 1, "{impact:#}");
    assert_eq!(impact["coverage"]["partial_files"], 1);

    // An earlier client (a CLI or a long-running MCP adapter) does not ask
    // for the count and reads coverage strictly: every answer it can get
    // keeps the shape it knows, while a file in scope is blind.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct EarlierCoverage {
        indexable_files: u32,
        current_files: u32,
        partial_files: u32,
        scanned: bool,
    }
    for command in [
        json!({"op": "graph_impact", "workspace_id": "app", "path": "src/lonely.rs", "limit": 10}),
        json!({"op": "graph_symbol", "workspace_id": "app", "name": "lonely", "limit": 10}),
        json!({"op": "graph_symbol", "workspace_id": "app", "name": "lonely", "limit": 10, "detail": true}),
        json!({"op": "graph_references", "workspace_id": "app", "name": "lonely", "limit": 10}),
        json!({"op": "graph_context", "workspace_id": "app", "names": ["lonely"], "limit": 3}),
        json!({"op": "dashboard"}),
    ] {
        let response = w.raw(&json!({"version": 3, "request_id": "earlier", "command": command}));
        let outcome = &response["outcome"];
        let coverages = match outcome.get("dashboard") {
            Some(dashboard) => dashboard["workspaces"]
                .as_array()
                .unwrap()
                .iter()
                .map(|workspace| workspace["coverage"].clone())
                .collect(),
            None => vec![outcome["coverage"].clone()],
        };
        assert!(!coverages.is_empty(), "{response:#}");
        for coverage in coverages {
            serde_json::from_value::<EarlierCoverage>(coverage.clone())
                .unwrap_or_else(|error| panic!("{command}: {error}: {coverage}"));
        }
    }
    // A client that asks gets the count.
    let asked = w.raw(&json!({"version": 3, "request_id": "asked", "command": {
        "op": "graph_impact", "workspace_id": "app", "path": "src/lonely.rs", "limit": 10,
        "blind_in_scope": true}}));
    assert_eq!(
        asked["outcome"]["coverage"]["blind_in_scope"], 1,
        "{asked:#}"
    );

    // Over the index's 1 MiB bound, over the scan's 4 MiB bound, and ten
    // files in a language the index has no grammar for.
    fs::write(
        w.work().join("src/big.rs"),
        format!("// {}\n", "x".repeat(1536 * 1024)),
    )
    .unwrap();
    fs::write(
        w.work().join("src/huge.rs"),
        format!("// {}\n", "x".repeat(5 * 1024 * 1024)),
    )
    .unwrap();
    fs::create_dir_all(w.work().join("java")).unwrap();
    for n in 0..10 {
        fs::write(
            w.work().join(format!("java/C{n}.java")),
            "class C { void lonely() {} }\n",
        )
        .unwrap();
    }
    let blind = |limit: &str| {
        let (code, text) = w.cli(&["graph", "blind-spots", "app", "--limit", limit, "--json"]);
        assert_eq!(code, 0, "{text}");
        serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone()
    };
    let deadline = Instant::now() + WAIT;
    let spots = loop {
        let outcome = blind("50");
        if outcome["blind_spots"]["too_large"].as_array().map(Vec::len) == Some(2)
            && outcome["blind_spots"]["unsupported_languages"] != json!([])
        {
            break outcome;
        }
        assert!(Instant::now() < deadline, "never saw them: {outcome:#}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let lists = &spots["blind_spots"];
    assert_eq!(lists["partial"][0]["path"], "src/broken.rs", "{spots:#}");
    assert!(lists["partial"][0]["since_ms"].as_u64().unwrap() > 0);
    assert_eq!(lists["never_clean"], json!(["src/broken.rs"]));
    assert_eq!(lists["too_large"], json!(["src/big.rs", "src/huge.rs"]));
    assert_eq!(lists["waiting"], json!([]), "too large is not waiting");
    assert_eq!(
        lists["unsupported_languages"],
        json!([{"extension": ".java", "files": 10}])
    );
    assert_eq!(spots["coverage"]["blind_in_scope"], 3);
    assert_eq!(spots["truncated"], false);
    assert!(
        !spots.to_string().contains("class C"),
        "no file content is returned"
    );

    // A capped list is the same on every call, and says it is capped.
    let first = blind("1");
    assert_eq!(first["blind_spots"]["too_large"], json!(["src/big.rs"]));
    assert_eq!(first["blind_spots"]["totals"]["too_large"], 2);
    assert_eq!(first["truncated"], true);
    assert_eq!(blind("1")["blind_spots"], first["blind_spots"]);

    let human = w.cli(&["graph", "blind-spots", "app"]).1;
    assert!(
        human.contains(r#"partial (1): "src/broken.rs" (since "#),
        "{human}"
    );
    assert!(
        human.contains(r#"too large (2): "src/big.rs", "src/huge.rs""#),
        "{human}"
    );
    assert!(
        human.contains("not indexed, other languages: .java 10"),
        "{human}"
    );
    for args in [
        &["graph", "blind-spots", "app", "--limit", "0"][..],
        &["graph", "blind-spots", "app", "--limit", "201"][..],
        &["graph", "blind-spots", "nowhere"][..],
    ] {
        let (code, text) = w.cli(args);
        assert_eq!(code, 1, "{args:?}: {text}");
    }

    // A workspace no scan can complete in: nothing is known, and the
    // answer carries the scan failure.
    let plain = w.base.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let (code, text) = w.cli(&[
        "workspace",
        "add",
        "plain",
        plain.to_str().unwrap(),
        "--request-id",
        "ws-plain",
    ]);
    assert_eq!(code, 0, "{text}");
    let deadline = Instant::now() + WAIT;
    let failed = loop {
        let (_, text) = w.cli(&["graph", "blind-spots", "plain", "--json"]);
        let outcome = serde_json::from_str::<Value>(&text).unwrap()["outcome"].clone();
        if outcome.get("scan_failure").is_some() {
            break outcome;
        }
        assert!(
            Instant::now() < deadline,
            "never saw the failure: {outcome:#}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(failed["coverage"]["scanned"], false, "{failed:#}");
    assert_eq!(failed["blind_spots"]["partial"], json!([]));
    let human = w.cli(&["graph", "blind-spots", "plain"]).1;
    assert!(human.contains("The last scan failed"), "{human}");
    assert!(human.contains("nothing is known"), "{human}");
}

#[test]
fn a_workspace_where_a_session_acts_is_indexed_promptly_among_many() {
    // Found by the scenario benchmark: with about 35 workspaces on one
    // coordinator, a new workspace waited its round-robin turn (two seconds
    // each) and its interface notice never arrived in time.
    let w = World::new();
    for n in 0..30 {
        let other = w.base.join(format!("other-{n:02}"));
        fs::create_dir_all(other.join("src")).unwrap();
        fs::write(other.join("src/lib.rs"), "pub fn other() {}\n").unwrap();
        git(&other, &["init", "--quiet", "--initial-branch=main"]);
        git(&other, &["add", "."]);
        git(&other, &["commit", "--quiet", "-m", "init"]);
        let (code, text) = w.cli(&[
            "workspace",
            "add",
            &format!("other-{n:02}"),
            other.to_str().unwrap(),
            "--request-id",
            &format!("ws-other-{n:02}"),
        ]);
        assert_eq!(code, 0, "{text}");
    }
    // A session edits the main workspace; it is scanned next, not 30th.
    // Let the first scan of every workspace pass first, so the touched one
    // is behind all thirty in round-robin order.
    let main = fs::read_to_string(w.work().join("src/main.rs")).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let started = Instant::now();
    w.edits(
        "alice",
        "a1",
        "src/main.rs",
        &format!("{main}pub fn fresh_name() {{}}\n"),
    );
    w.wait_for(
        "the new function indexed",
        "symbol",
        "fresh_name",
        |outcome| {
            outcome["symbols"]
                .as_array()
                .is_some_and(|symbols| !symbols.is_empty())
        },
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "took {:?}; round-robin alone needs about a minute",
        started.elapsed()
    );
}
