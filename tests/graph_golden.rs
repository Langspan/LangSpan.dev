#![cfg(unix)]
//! The source index's on-disk shape, pinned byte for byte in
//! `tests/golden/graph/` (issue #328): the SQLite schema an index file
//! holds, and the export of a fixed repository, which a fresh state
//! directory imports to give the same answers. A change to either breaks
//! indexes and exports users already hold, so it fails here first. To
//! change one on purpose, see `tests/support/golden.rs`.

#[path = "support/golden.rs"]
mod golden;
mod support;
use support::git;

use langspan::graph::Index;

use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(60);

/// A directory of the test's own, removed when it is dropped.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let path = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-graph-golden-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `user_version` and every table and index of the file at `path`, each
/// with the SQL SQLite keeps for it, in type and name order.
fn schema_of(path: &Path) -> String {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let mut out = format!("user_version = {version}\n");
    let mut rows = connection
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .unwrap();
    let rows = rows
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (kind, name, table, sql) = row.unwrap();
        out.push_str(&format!(
            "\n-- {kind} {name} on {table}\n{};\n",
            sql.unwrap_or_default()
        ));
    }
    out
}

#[test]
fn a_new_index_file_has_the_golden_schema_and_opening_it_again_keeps_it() {
    let dir = Dir::new();
    let path = dir.0.join("graph.sqlite3");
    drop(Index::open(&path).unwrap());
    let schema = schema_of(&path);
    golden::check("graph/schema.sql", &schema);
    // The upgrades in `open` run once: a second open, and a reader, change
    // nothing.
    drop(Index::open(&path).unwrap());
    drop(Index::open_reader(&path).unwrap());
    assert_eq!(schema_of(&path), schema);
}

/// The fixed repository: Rust, Python and TypeScript files that define,
/// call and import, one that does not parse cleanly, and one no grammar
/// reads.
const FILES: &[(&str, &str)] = &[
    (
        "src/upload.rs",
        "pub fn upload(path: &str) -> bool {\n    !path.is_empty()\n}\n\npub fn gate(user: &str) -> bool {\n    user == \"admin\"\n}\n",
    ),
    (
        "src/main.rs",
        "mod upload;\nuse upload::gate;\n\nfn main() {\n    if gate(\"admin\") {\n        upload::upload(\"a.txt\");\n    }\n}\n",
    ),
    ("src/broken.rs", "pub fn half( {{{\n"),
    (
        "tools/report.py",
        "import os\n\ndef report(path):\n    return os.path.exists(path)\n",
    ),
    (
        "web/api.ts",
        "import { report } from './report';\n\nexport function send(body: string): number {\n  return report(body);\n}\n",
    ),
    ("README.md", "upload gate\n"),
];

/// One daemon on its own state directory, with workspace `app` at `root`.
struct Daemon {
    state: PathBuf,
    child: Child,
}

impl Daemon {
    /// `files_per_pass` 0 keeps its indexer from parsing anything, so what
    /// it answers can only have come from an import.
    fn start(state: PathBuf, root: &Path, files_per_pass: Option<usize>) -> Self {
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        // Export and import are owner grants; these daemons' owner allows them.
        support::owner_grants(&state, true);
        let mut command = Command::new(env!("CARGO_BIN_EXE_langspan"));
        command
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(files) = files_per_pass {
            command.env("LANGSPAN_TEST_INDEX_FILES_PER_PASS", files.to_string());
        }
        let daemon = Self {
            state,
            child: command.spawn().unwrap(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while daemon.cli(&["status"]).0 != 0 {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        let root = root.to_str().unwrap();
        let (code, text) = daemon.cli(&["workspace", "add", "app", root, "--request-id", "ws"]);
        assert_eq!(code, 0, "{text}");
        daemon
    }

    fn cli(&self, args: &[&str]) -> (i32, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap() + &String::from_utf8_lossy(&output.stderr),
        )
    }

    fn outcome(&self, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        let (_, text) = self.cli(&args);
        let json = text.lines().next().unwrap_or_default();
        serde_json::from_str::<Value>(json).unwrap_or_else(|_| panic!("{text}"))["outcome"].clone()
    }

    /// The same questions of both indexes, coverage included.
    fn answers(&self) -> Vec<Value> {
        let mut asked: Vec<Vec<&str>> = ["upload", "gate", "main", "half", "report", "send"]
            .iter()
            .map(|name| vec!["graph", "symbol", "app", name, "--detail"])
            .collect();
        asked.push(vec![
            "graph",
            "impact",
            "app",
            "src/upload.rs",
            "--depth",
            "2",
        ]);
        asked.push(vec!["graph", "path", "app", "main", "upload"]);
        asked.push(vec!["graph", "map", "app"]);
        asked.iter().map(|args| self.outcome(args)).collect()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The export with its one time-dependent field, the header's
/// `created_at`, set to 0.
fn without_time(export: &str) -> String {
    let (header, files) = export.split_once('\n').unwrap();
    let mut header: Value = serde_json::from_str(header).unwrap();
    assert!(header["created_at"].as_u64().unwrap() > 0, "{header}");
    header["created_at"] = 0.into();
    format!("{header}\n{files}")
}

#[test]
fn the_fixed_repository_exports_the_golden_file_and_imports_to_the_same_answers() {
    let base = Dir::new();
    let work = base.0.join("work");
    for (path, content) in FILES {
        let path = work.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    git(&work, &["init", "--quiet", "--initial-branch=main"]);
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "init"]);

    let first = Daemon::start(base.0.join("state"), &work, None);
    let deadline = Instant::now() + WAIT;
    loop {
        let coverage = first.outcome(&["graph", "symbol", "app", "upload"])["coverage"].clone();
        if coverage["current_files"] == coverage["indexable_files"]
            && coverage["indexable_files"] == 5
        {
            break;
        }
        assert!(Instant::now() < deadline, "never indexed: {coverage:#}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let file = base.0.join("index.jsonl");
    let (code, text) = first.cli(&["graph", "export", "app", "--out", file.to_str().unwrap()]);
    assert_eq!(code, 0, "{text}");
    let exported = fs::read_to_string(&file).unwrap();
    golden::check("graph/export.jsonl", &without_time(&exported));

    // A fresh state directory on the same checkout, parsing nothing:
    // everything it answers comes from the import.
    let second = Daemon::start(base.0.join("state2"), &work, Some(0));
    let deadline = Instant::now() + WAIT;
    let imported = loop {
        let imported = second.outcome(&[
            "graph",
            "import",
            "app",
            "--in",
            file.to_str().unwrap(),
            "--apply",
        ]);
        if imported["message"]
            .as_str()
            .is_none_or(|message| !message.contains("no complete scan yet"))
        {
            break imported;
        }
        assert!(Instant::now() < deadline, "never scanned: {imported:#}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(imported["loaded"], 5, "{imported:#}");
    let answers = first.answers();
    assert!(
        answers[0]["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .any(|symbol| symbol["signature"] == "pub fn upload(path: &str) -> bool"),
        "{:#}",
        answers[0]
    );
    assert_eq!(answers[0]["coverage"]["current_files"], 5);
    assert_eq!(second.answers(), answers);
    // Exported again from the imported index, it is the same file.
    let again = base.0.join("again.jsonl");
    let (code, text) = second.cli(&["graph", "export", "app", "--out", again.to_str().unwrap()]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        without_time(&fs::read_to_string(&again).unwrap()),
        without_time(&exported)
    );
}
