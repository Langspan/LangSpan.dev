#![cfg(unix)]
//! Notes anchored to a definition (roadmap E1) through a real daemon, the
//! real observer and indexer, a real Git repository and the real Claude
//! Code hook: a Rust and a Python definition change under their notes.

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

const API: &str = "pub fn parse_upload(body: &str) -> usize {\n    body.len()\n}\n\npub fn send_upload(body: &str) -> usize {\n    parse_upload(body)\n}\n";
const UPLOAD_PY: &str =
    "def check_size(size):\n    return size < 10\n\n\ndef parse_upload(body):\n    return body\n";
const CALLER: &str = "pub fn caller() -> usize {\n    parse_upload(\"x\")\n}\n";

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
        Self::with(&[])
    }
    /// The three source files and `extra` (path, content), committed.
    fn with(extra: &[(&str, &str)]) -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-anchor-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let work = base.join("work");
        fs::create_dir_all(work.join("src")).unwrap();
        fs::create_dir_all(work.join("app")).unwrap();
        fs::write(work.join("src/api.rs"), API).unwrap();
        fs::write(work.join("app/upload.py"), UPLOAD_PY).unwrap();
        fs::write(work.join("src/caller.rs"), CALLER).unwrap();
        for (path, content) in extra {
            let file = work.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
        git(&work, &["init", "--quiet", "--initial-branch=main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "init"]);
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self { base, daemon: None };
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
    fn cli_full(&self, args: &[&str]) -> (i32, String, String) {
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
            String::from_utf8(output.stderr).unwrap(),
        )
    }
    /// A refused command's whole output.
    fn refused(&self, args: &[&str]) -> String {
        let (code, out, err) = self.cli_full(args);
        assert_ne!(code, 0, "{out}{err}");
        format!("{out}{err}")
    }
    fn cli(&self, args: &[&str]) -> (i32, String) {
        let (code, out, _) = self.cli_full(args);
        (code, out)
    }
    fn json(&self, args: &[&str]) -> Value {
        let mut all = args.to_vec();
        all.push("--json");
        let (code, out, err) = self.cli_full(&all);
        assert_eq!(code, 0, "{out}{err}");
        serde_json::from_str::<Value>(&out).unwrap()["outcome"].clone()
    }
    /// The index covers the three source files at their current content.
    fn wait_for_index(&self) {
        self.wait_for_files(3);
    }
    /// The index covers `files` source files at their current content.
    fn wait_for_files(&self, files: u32) {
        let deadline = Instant::now() + WAIT;
        loop {
            let outcome = self.json(&["graph", "symbol", "app", "parse_upload"]);
            let coverage = &outcome["coverage"];
            if coverage["current_files"] == files && coverage["indexable_files"] == files {
                return;
            }
            assert!(Instant::now() < deadline, "never indexed: {outcome:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn notes(&self, tag: Option<&str>) -> Vec<Value> {
        let mut args = vec!["recap", "app"];
        if let Some(tag) = tag {
            args.extend(["--tag", tag]);
        }
        self.json(&args)["recap"]["notes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
    fn state_of(&self, note: &str) -> Option<String> {
        self.notes(None)
            .into_iter()
            .find(|held| held["note_id"] == note)
            .and_then(|held| held["anchor_state"].as_str().map(str::to_owned))
    }
    fn wait_for_state(&self, note: &str, state: &str) {
        let deadline = Instant::now() + WAIT;
        while self.state_of(note).as_deref() != Some(state) {
            assert!(
                Instant::now() < deadline,
                "{note} never {state}: {:#?}",
                self.notes(None)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn hook(&self, event: &Value) -> Option<String> {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", "claude-code", "--state-dir"])
            .arg(self.base.join("state"))
            .args(["--workspace", "app", "--root"])
            .arg(self.work())
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
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout)
            .ok()
            .and_then(|reply| {
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_owned)
            })
    }
    fn prompt(&self, session: &str, text: &str) -> String {
        self.hook(&json!({
            "session_id": session, "transcript_path": null, "cwd": self.work(),
            "hook_event_name": "UserPromptSubmit", "prompt": text,
        }))
        .unwrap_or_default()
    }
    /// A complete reported Claude Code write of `content` to `file`.
    fn writes(&self, session: &str, call: &str, file: &str, content: &str) {
        let mut event = json!({
            "session_id": session, "cwd": self.work(), "hook_event_name": "PreToolUse",
            "tool_name": "Write", "tool_use_id": call,
            "tool_input": {"file_path": self.work().join(file), "content": content},
        });
        self.hook(&event);
        fs::write(self.work().join(file), content).unwrap();
        event["hook_event_name"] = json!("PostToolUse");
        self.hook(&event);
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

fn note_line(context: &str) -> &str {
    context
        .lines()
        .find(|line| line.starts_with("Remembered for this repository"))
        .unwrap_or_else(|| panic!("no note line in {context}"))
}

#[test]
fn an_anchored_note_reports_its_definitions_changes_in_rust_and_python() {
    let w = World::new();
    w.wait_for_index();
    w.prompt("alice", "keep the parser pure");
    // Carol works on a caller: her file mentions parse_upload.
    w.prompt("carol", "add a caller");
    w.writes(
        "carol",
        "c1",
        "src/caller.rs",
        &format!("{CALLER}// calls parse_upload\n"),
    );

    // Defined in a Rust and a Python file: ambiguous without a path, and
    // nothing is recorded.
    let err = w.refused(&[
        "remember",
        "app",
        "no network",
        "--symbol",
        "parse_upload",
        "--request-id",
        "n0",
    ]);
    assert!(
        // The message is printed quoted, so its own quotes are escaped.
        err.contains("symbol_ambiguous")
            && err.contains(r#"\"app/upload.py\" line 5"#)
            && err.contains(r#"\"src/api.rs\" line 1"#),
        "{err}"
    );
    assert!(w.notes(None).is_empty());
    // A name the file does not define is not found.
    let err = w.refused(&[
        "remember",
        "app",
        "x",
        "src/api.rs",
        "--symbol",
        "check_size",
        "--request-id",
        "n1",
    ]);
    assert!(
        err.contains("symbol_not_found") && err.contains(r#"\"app/upload.py\" line 1"#),
        "{err}"
    );
    // A tag with an upper-case letter or over 32 bytes is refused before
    // anything is recorded.
    for (id, tag) in [("n2", "Api"), ("n3", &"a".repeat(33))] {
        let err = w.refused(&[
            "remember",
            "app",
            "x",
            "src/api.rs",
            "--symbol",
            "parse_upload",
            "--tag",
            tag,
            "--request-id",
            id,
        ]);
        assert!(err.contains("invalid_tag"), "{err}");
    }
    assert!(w.notes(None).is_empty());

    let (code, out) = w.cli(&[
        "remember",
        "app",
        "parse_upload must not touch the network.",
        "src/api.rs",
        "--symbol",
        "parse_upload",
        "--tag",
        "api",
        "--session",
        "claude-code.alice",
        "--request-id",
        "note-7",
    ]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = w.cli(&[
        "remember",
        "app",
        "check_size stays pure.",
        "app/upload.py",
        "--symbol",
        "check_size",
        "--tag",
        "py",
        "--request-id",
        "note-py",
    ]);
    assert_eq!(code, 0, "{out}");
    let held = w.notes(None);
    let rust = held.iter().find(|n| n["note_id"] == "note-7").unwrap();
    assert_eq!(rust["anchor"]["path"], "src/api.rs");
    assert_eq!(rust["anchor"]["name"], "parse_upload");
    assert_eq!(rust["anchor"]["kind"], "function");
    assert_eq!(rust["anchor"]["line"], 1);
    assert_eq!(
        rust["anchor"]["body_digest"].as_str().map(str::len),
        Some(64)
    );
    assert_eq!(rust["anchor_state"], "current");
    assert_eq!(rust["tags"], json!(["api"]));

    // Carol's next prompt carries the note: her file mentions the name.
    let context = w.prompt("carol", "go on");
    assert_eq!(
        note_line(&context),
        r#"Remembered for this repository (about parse_upload in "src/api.rs") [api]: "parse_upload must not touch the network.""#
    );

    // Another function of the same file changes, then the Python
    // definition's body: once the later file's pass is checked, the earlier
    // one's has been, and the Rust note is still current.
    fs::write(
        w.work().join("src/api.rs"),
        API.replace("    parse_upload(body)\n", "    parse_upload(body) + 1\n"),
    )
    .unwrap();
    fs::write(
        w.work().join("app/upload.py"),
        UPLOAD_PY.replace("size < 10", "size < 20"),
    )
    .unwrap();
    w.wait_for_state("note-py", "changed");
    assert_eq!(w.state_of("note-7").as_deref(), Some("current"));

    // Only the body of parse_upload changes: the note reads changed.
    let changed = API
        .replace("    parse_upload(body)\n", "    parse_upload(body) + 1\n")
        .replace("    body.len()\n", "    body.trim().len()\n");
    fs::write(w.work().join("src/api.rs"), &changed).unwrap();
    w.wait_for_state("note-7", "changed");
    let context = w.prompt("carol", "and now");
    assert_eq!(
        note_line(&context),
        r#"Remembered for this repository (about parse_upload in "src/api.rs"; the code changed since this note was written) [api]: "parse_upload must not touch the network.""#
    );

    // Only the notes tagged api.
    let tagged = w.notes(Some("api"));
    assert_eq!(tagged.len(), 1, "{tagged:#?}");
    assert_eq!(tagged[0]["note_id"], "note-7");

    // parse_upload is deleted: gone, and still listed until forgotten.
    fs::write(
        w.work().join("src/api.rs"),
        "pub fn send_upload(body: &str) -> usize {\n    body.len()\n}\n",
    )
    .unwrap();
    w.wait_for_state("note-7", "gone");
    let (code, text) = w.cli(&["recap", "app"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains(
            r#"Remembered (note-7) (about parse_upload in "src/api.rs"; that definition is no longer there) [api]: "parse_upload must not touch the network.""#
        ),
        "{text}"
    );
    let (code, _) = w.cli(&["forget", "app", "note-7", "--request-id", "f1"]);
    assert_eq!(code, 0);
    assert_eq!(w.state_of("note-7"), None);
}

/// Review comment 4713922957: a deleted or moved file has no row in the
/// index, and the note on its definition reads gone, not unknown forever.
#[test]
fn a_note_on_a_deleted_or_moved_file_reads_gone() {
    let w = World::new();
    w.wait_for_index();
    for (id, path, name) in [
        ("note-del", "src/api.rs", "parse_upload"),
        ("note-mv", "app/upload.py", "check_size"),
    ] {
        let (code, out) = w.cli(&[
            "remember",
            "app",
            "x",
            path,
            "--symbol",
            name,
            "--request-id",
            id,
        ]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(w.state_of(id).as_deref(), Some("current"));
    }
    fs::remove_file(w.work().join("src/api.rs")).unwrap();
    w.wait_for_state("note-del", "gone");
    fs::rename(
        w.work().join("app/upload.py"),
        w.work().join("app/moved.py"),
    )
    .unwrap();
    w.wait_for_state("note-mv", "gone");
}

/// Review comment 4713922965: more definitions of a name than the
/// workspace lookup's cap, all in a file that sorts earlier, hide neither
/// a file's own definition from `remember` nor an anchored one from the
/// recheck.
#[test]
fn many_definitions_elsewhere_hide_no_anchored_definition() {
    let w = World::new();
    w.wait_for_index();
    let remember = |text: &str, id: &str| {
        w.cli(&[
            "remember",
            "app",
            text,
            "src/api.rs",
            "--symbol",
            "parse_upload",
            "--request-id",
            id,
        ])
    };
    let (code, out) = remember("x", "note-7");
    assert_eq!(code, 0, "{out}");
    let many: String = (0..205)
        .map(|n| format!("fn parse_upload() -> u32 {{\n    {n}\n}}\n"))
        .collect();
    fs::create_dir_all(w.work().join("a")).unwrap();
    fs::write(w.work().join("a/many.rs"), many).unwrap();
    w.wait_for_files(4);
    // A new note on the file's own definition resolves.
    let (code, out) = remember("y", "note-8");
    assert_eq!(code, 0, "{out}");
    assert_eq!(w.state_of("note-8").as_deref(), Some("current"));
    // Without a path the name is ambiguous, counted as a lower bound.
    let err = w.refused(&[
        "remember",
        "app",
        "z",
        "--symbol",
        "parse_upload",
        "--request-id",
        "note-9",
    ]);
    assert!(
        err.contains("symbol_ambiguous") && err.contains("at least 200 times"),
        "{err}"
    );
    // The held note's definition is still seen: it changes, not goes.
    fs::write(
        w.work().join("src/api.rs"),
        API.replace("    body.len()\n", "    body.trim().len()\n"),
    )
    .unwrap();
    w.wait_for_state("note-7", "changed");
}

const PAIR: &str = "pub struct A;\nimpl A {\n    pub fn new() -> Self {\n        A\n    }\n}\n";
const PAIR_B: &str = "pub struct B;\nimpl B {\n    pub fn new() -> Self {\n        B\n    }\n}\n";

/// The note IDs `graph symbol NAME --detail` lists with each definition,
/// by its line.
fn detail_notes(w: &World, name: &str) -> Vec<(u64, Vec<String>)> {
    w.json(&["graph", "symbol", "app", name, "--detail"])["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| {
            (
                symbol["line"].as_u64().unwrap(),
                symbol["notes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|note| note["id"].as_str().unwrap().to_owned())
                    .collect(),
            )
        })
        .collect()
}

/// Review comment 4713922971: an anchored note made without paths is
/// listed with its definition, found by the anchor's file and the
/// definition's own bytes; a second definition of the name in that file
/// does not carry it, and a plain note still goes by its paths.
#[test]
fn graph_symbol_detail_lists_an_anchored_note_with_only_its_own_definition() {
    let w = World::with(&[("src/pair.rs", PAIR)]);
    w.wait_for_files(4);
    for args in [
        &[
            "remember",
            "app",
            "sync",
            "--symbol",
            "send_upload",
            "--request-id",
            "n-send",
        ][..],
        &[
            "remember",
            "app",
            "a only",
            "--symbol",
            "new",
            "--request-id",
            "n-new",
        ],
        &[
            "remember",
            "app",
            "plain",
            "src/pair.rs",
            "--request-id",
            "n-plain",
        ],
    ] {
        let (code, out) = w.cli(args);
        assert_eq!(code, 0, "{out}");
    }
    assert_eq!(
        detail_notes(&w, "send_upload"),
        [(5, vec!["n-send".to_owned()])]
    );
    fs::write(w.work().join("src/pair.rs"), format!("{PAIR}{PAIR_B}")).unwrap();
    let deadline = Instant::now() + WAIT;
    while detail_notes(&w, "new").len() != 2 {
        assert!(Instant::now() < deadline, "B::new never indexed");
        std::thread::sleep(Duration::from_millis(100));
    }
    w.wait_for_state("n-new", "current");
    assert_eq!(
        detail_notes(&w, "new"),
        [
            (3, vec!["n-plain".to_owned(), "n-new".to_owned()]),
            (9, vec!["n-plain".to_owned()]),
        ]
    );
}
