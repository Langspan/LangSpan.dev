//! The daemon's contract through a real socket (issue #327): every request
//! type in every protocol version the daemon answers, the answer to a
//! version it does not, the frame limits `serve_connection` reads to, the
//! checks `forward_transfer` and `prepare_move` make before they hand a
//! request on, and the answers `fit_frame` and `fit_blind_spots` cut to one
//! frame. Replies that are the same on every run are pinned in
//! `tests/golden/frames/`; to change one on purpose, see
//! `tests/support/golden.rs`.
#![cfg(unix)]

#[path = "support/golden.rs"]
mod golden;

use langspan::client;
use langspan::protocol::{
    MAX_FRAME_BYTES, OLDEST_PROTOCOL_VERSION, PROTOCOL_VERSION, Request, Response,
};
use langspan_core::{Command, Outcome};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::{fs::PermissionsExt, net::UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{fs, net::Shutdown};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A daemon of its own, on a state directory and a workspace directory of
/// the test's own.
struct Daemon {
    base: PathBuf,
    child: Child,
}

impl Daemon {
    fn start() -> Self {
        let base = std::env::temp_dir().join(format!(
            "ls-contract-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let state = base.join("state");
        fs::create_dir_all(&state).unwrap();
        for dir in [&base, &state] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        // The shortest I/O deadline the daemon accepts, for the read timeout.
        let config = state.join("config.json");
        fs::write(&config, r#"{"io_timeout_ms":100}"#).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["daemon", "--state-dir"])
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut daemon = Self { base, child };
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&state, &Request::new("ready", Command::Status)).is_err() {
            assert!(daemon.child.try_wait().unwrap().is_none(), "daemon exited");
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        daemon
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    /// Write `bytes` to a new connection, close the writing half, and read
    /// what the daemon answers until it closes the connection.
    fn exchange(&self, bytes: &[u8]) -> String {
        let socket = fs::canonicalize(self.state()).unwrap().join("daemon.sock");
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream.write_all(bytes).unwrap();
        // The daemon may have answered and closed already; macOS then
        // refuses the shutdown as not connected.
        let _ = stream.shutdown(Shutdown::Write);
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    fn call(&self, id: &str, command: Command) -> Outcome {
        client::call(&self.state(), &Request::new(id, command))
            .unwrap()
            .outcome
    }

    /// A Git checkout at `base/name` holding `files`, registered as `id`.
    fn checkout(&self, id: &str, name: &str, files: &[(String, String)]) -> PathBuf {
        let root = self.base.join(name);
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let commit = [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "init",
        ];
        for args in [
            &["init", "--quiet", "--initial-branch=main"][..],
            &["add", "."],
            &commit,
        ] {
            let status = Process::new("git").arg("-C").arg(&root).args(args).status();
            assert!(status.unwrap().success(), "git {args:?}");
        }
        let register = Command::WorkspaceRegister {
            workspace_id: id.into(),
            root: root.to_str().unwrap().into(),
        };
        let outcome = self.call(&format!("register-{id}"), register);
        assert!(matches!(outcome, Outcome::Workspace { .. }), "{outcome:?}");
        root
    }

    /// Ask `command` until its coverage says `files` are indexed whole.
    fn indexed(&self, command: &Command, files: u32) -> (String, Value) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let frame = self.exchange(&frame(
                &serde_json::to_value(Request::new("q", command.clone())).unwrap(),
            ));
            let reply: Value = serde_json::from_str(&frame).unwrap();
            let coverage = &reply["outcome"]["coverage"];
            if coverage["current_files"] == files && coverage["indexable_files"] == files {
                return (frame, reply);
            }
            assert!(Instant::now() < deadline, "never indexed: {reply}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn frame(request: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(request).unwrap();
    bytes.push(b'\n');
    bytes
}

/// Pin one reply frame as `tests/golden/frames/NAME.json`, byte for byte
/// but for its generation: that counts what the daemon recorded since it
/// started, such as its first check of the agent hosts, so it is set to 0.
fn pin(name: &str, reply: &str) {
    assert!(
        reply.len() <= MAX_FRAME_BYTES && reply.ends_with('\n'),
        "{reply}"
    );
    let mut value: Value = serde_json::from_str(reply).unwrap();
    assert!(value["generation"].is_u64(), "{reply}");
    value["generation"] = json!(0);
    let pinned = format!("{}\n", serde_json::to_string(&value).unwrap());
    golden::check(&format!("frames/{name}.json"), &pinned);
}

fn error_code(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Error { code, .. } => code,
        _ => "",
    }
}

/// `graph symbol NAME --detail --limit LIMIT` in workspace `app`.
fn symbol(name: &str, limit: u32) -> Command {
    Command::GraphSymbol {
        workspace_id: "app".into(),
        name: name.into(),
        limit,
        detail: true,
        rank: false,
        session_id: None,
        renamed: false,
        blind_in_scope: false,
    }
}

/// The request vectors of `command_vectors.rs`, one per wire request type.
fn vectors() -> Vec<(String, Value)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut vectors: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_str().unwrap().ends_with("-command-v10.json"))
        .map(|path| {
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            (
                name,
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap(),
            )
        })
        .collect();
    vectors.sort_by(|a, b| a.0.cmp(&b.0));
    vectors
}

#[test]
fn every_request_type_is_answered_through_the_socket_in_every_version_the_daemon_reads() {
    let daemon = Daemon::start();
    let vectors = vectors();
    assert!(vectors.len() >= 90, "the request vectors moved");
    // Codes that say the request never reached its handler.
    let unread = [
        "invalid_request",
        "invalid_frame",
        "read_timeout",
        "unsupported_version",
    ];
    for (name, vector) in &vectors {
        for version in OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION {
            let mut request = vector.clone();
            request["version"] = json!(version);
            let id = format!("contract-{version}");
            request["request_id"] = json!(id);
            let reply = daemon.exchange(&frame(&request));
            let case = format!("{name} at version {version}: {reply}");
            assert!(
                reply.len() <= MAX_FRAME_BYTES && reply.ends_with('\n'),
                "{case}"
            );
            assert_eq!(reply.matches('\n').count(), 1, "{case}");
            let response: Response = serde_json::from_str(&reply).expect(&case);
            assert_eq!(response.version, version, "{case}");
            assert_eq!(response.request_id, id, "{case}");
            assert!(!unread.contains(&error_code(&response.outcome)), "{case}");
        }
    }
}

#[test]
fn a_version_the_daemon_does_not_answer_is_refused_in_the_current_version() {
    let daemon = Daemon::start();
    let status = json!({"request_id": "old", "command": {"op": "status"}});
    // An export is checked on its way to the indexer, not by the coordinator.
    let export = json!({"request_id": "old", "command": {"op": "graph_export", "workspace_id": "app", "path": "/x"}});
    for (name, request, version) in [
        ("unsupported-older", &status, OLDEST_PROTOCOL_VERSION - 1),
        ("unsupported-newer", &status, PROTOCOL_VERSION + 1),
        ("unsupported-transfer", &export, OLDEST_PROTOCOL_VERSION - 1),
    ] {
        let mut request = request.clone();
        request["version"] = json!(version);
        pin(name, &daemon.exchange(&frame(&request)));
    }
}

#[test]
fn serve_connection_reads_one_frame_of_at_most_64_kib_and_answers_every_other_shape() {
    let daemon = Daemon::start();
    // The largest frame read: 64 KiB with its newline, padded with the
    // whitespace JSON allows after a value.
    let mut largest =
        br#"{"version":10,"request_id":"largest","command":{"op":"workspace_list"}}"#.to_vec();
    largest.resize(MAX_FRAME_BYTES - 1, b' ');
    largest.push(b'\n');
    let mut over = largest.clone();
    over.insert(MAX_FRAME_BYTES - 2, b' ');
    let cases: [(&str, &[u8]); 5] = [
        ("largest-request", &largest),
        ("request-over-64-kib", &over),
        (
            "request-without-newline",
            br#"{"version":10,"request_id":"r","command":{"op":"status"}}"#,
        ),
        (
            "request-not-read",
            b"{\"version\":7,\"request_id\":\"r\",\"command\":{\"op\":\"nope\"}}\n",
        ),
        ("request-not-json", b"not json\n"),
    ];
    for (name, bytes) in cases {
        pin(name, &daemon.exchange(bytes));
    }
    // Nothing sent before the I/O deadline: the connection stays open.
    let socket = fs::canonicalize(daemon.state())
        .unwrap()
        .join("daemon.sock");
    let mut silent = UnixStream::connect(socket).unwrap();
    silent
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let mut reply = String::new();
    silent.read_to_string(&mut reply).unwrap();
    pin("read-timeout", &reply);
}

#[test]
fn forward_transfer_refuses_a_bad_export_or_import_before_the_indexer() {
    let daemon = Daemon::start();
    daemon.checkout(
        "app",
        "app",
        &[("lib.rs".into(), "pub fn one() {}\n".into())],
    );
    let inside = daemon.base.join("export.bin").to_str().unwrap().to_owned();
    let long = format!("/{}", "a".repeat(4096));
    let export = |workspace: &str, path: &str| Command::GraphExport {
        workspace_id: workspace.into(),
        path: path.into(),
    };
    for (id, command, code) in [
        ("bad id", export("app", &inside), "invalid_request_id"),
        ("relative", export("app", "export.bin"), "invalid_path"),
        ("too-long", export("app", &long), "invalid_path"),
        ("nul", export("app", "/a\0b"), "invalid_path"),
        ("unknown", export("nobody", &inside), "unknown_workspace"),
        (
            "import-relative",
            Command::GraphImport {
                workspace_id: "app".into(),
                path: "x".into(),
                apply: false,
            },
            "invalid_path",
        ),
    ] {
        let outcome = daemon.call(id, command);
        assert_eq!(error_code(&outcome), code, "{id}: {outcome:?}");
    }
    daemon.indexed(&symbol("one", 1), 1);
    let outcome = daemon.call("export", export("app", &inside));
    assert!(
        matches!(outcome, Outcome::GraphExported { .. }),
        "{outcome:?}"
    );
    let outcome = daemon.call("again", export("app", &inside));
    assert_eq!(error_code(&outcome), "index_export_failed", "{outcome:?}");
}

#[test]
fn prepare_move_refuses_each_root_it_cannot_verify_and_changes_nothing() {
    let daemon = Daemon::start();
    let file = vec![("lib.rs".to_owned(), "pub fn one() {}\n".to_owned())];
    let old = daemon.checkout("app", "app", &file);
    // Another commit, so that its history holds none of the old root's.
    let unrelated = vec![("lib.rs".to_owned(), "pub fn two() {}\n".to_owned())];
    let other = daemon.checkout("other", "other", &unrelated);
    fs::create_dir_all(old.join("sub")).unwrap();
    let plain = daemon.base.join("plain");
    fs::create_dir(&plain).unwrap();
    let text = |path: &Path| path.to_str().unwrap().to_owned();
    let long = format!("/{}", "a".repeat(4096));
    let cases = [
        (long.clone(), false, "invalid_root"),
        (text(&daemon.base.join("missing")), false, "invalid_root"),
        (text(&old.join("lib.rs")), false, "invalid_root"),
        (text(&plain), false, "not_checkout_top"),
        (text(&old.join("sub")), false, "not_checkout_top"),
        (text(&other), false, "old_root_exists"),
        (text(&other), true, "different_repository"),
    ];
    for (root, switch, code) in cases {
        let command = Command::WorkspaceMove {
            workspace_id: "app".into(),
            root: root.clone(),
            switch,
        };
        let outcome = daemon.call("move", command);
        assert_eq!(
            error_code(&outcome),
            code,
            "{root} switch={switch}: {outcome:?}"
        );
    }
    // The checkout moved: without --switch it is followed, with it refused.
    let moved = daemon.base.join("moved");
    fs::rename(&old, &moved).unwrap();
    let to = |switch| Command::WorkspaceMove {
        workspace_id: "app".into(),
        root: text(&moved),
        switch,
    };
    assert_eq!(
        error_code(&daemon.call("switch", to(true))),
        "old_root_missing"
    );
    let outcome = daemon.call("follow", to(false));
    assert!(
        matches!(outcome, Outcome::WorkspaceMoved { .. }),
        "{outcome:?}"
    );
    // An unknown ID is the core's to refuse.
    let unknown = Command::WorkspaceMove {
        workspace_id: "nobody".into(),
        root: text(&moved),
        switch: false,
    };
    assert_eq!(
        error_code(&daemon.call("unknown", unknown)),
        "workspace_not_found"
    );
}

/// Long paths, 210 definitions of `target` and 200 files with a syntax
/// error: more than one frame holds of either.
fn long_paths() -> Vec<(String, String)> {
    let deep = format!("{}/{}", "d".repeat(200), "e".repeat(200));
    let mut files: Vec<_> = (0..210)
        .map(|n| {
            (
                format!("{deep}/good_{n:03}.rs"),
                "pub fn target() {}\n".to_owned(),
            )
        })
        .collect();
    files.extend((0..200).map(|n| {
        (
            format!("{deep}/bad_{n:03}.rs"),
            "pub fn broken( {\n".to_owned(),
        )
    }));
    files
}

#[test]
fn answers_longer_than_one_frame_are_cut_to_fit_and_say_so() {
    let daemon = Daemon::start();
    daemon.checkout("app", "app", &long_paths());
    let (frame, reply) = daemon.indexed(&symbol("target", 200), 410);
    assert!(frame.len() <= MAX_FRAME_BYTES, "{}", frame.len());
    let kept = reply["outcome"]["symbols"].as_array().unwrap().len();
    let summary = format!(
        "graph symbol --detail: {} bytes, {kept} of 210 definitions, truncated {}\n",
        frame.len(),
        reply["outcome"]["truncated"]
    );
    let spots = Command::GraphBlindSpots {
        workspace_id: "app".into(),
        limit: 200,
    };
    let (frame, reply) = daemon.indexed(&spots, 410);
    assert!(frame.len() <= MAX_FRAME_BYTES, "{}", frame.len());
    let partial = reply["outcome"]["blind_spots"]["partial"]
        .as_array()
        .unwrap()
        .len();
    let summary = format!(
        "{summary}graph blind-spots: {} bytes, {partial} of 200 partial files, truncated {}\n",
        frame.len(),
        reply["outcome"]["truncated"]
    );
    golden::check("frames/cut-to-one-frame.txt", &summary);
}
