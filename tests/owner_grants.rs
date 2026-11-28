#![cfg(unix)]
//! Owner-grant commands need the owner's own action (issue #335, ADR 0039).
//! A plain client writes each command's golden request frame to a real
//! coordinator's socket. This starts a daemon because what is verified is the
//! socket path and the coordinator reading the owner's `config.json` again
//! for each request. The commands come from the vectors every wire command
//! has (`tests/command_vectors.rs`) and the class table, so a new owner grant
//! is tried here through its vector, and fails the list below until ADR
//! 0039's table and the usage page name it.

#[path = "support/golden.rs"]
mod golden;
mod support;

use langspan::protocol::Request;
use langspan_core::{Authority, authority};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// A coordinator of the test's own, on a state directory of its own.
struct Coordinator {
    base: PathBuf,
    _daemon: support::Owned,
}

impl Coordinator {
    fn start(name: &str) -> Self {
        // A Unix socket path is limited to about 100 bytes: keep it short.
        let base = PathBuf::from(format!("/tmp/lsog-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("state")).unwrap();
        fs::set_permissions(base.join("state"), fs::Permissions::from_mode(0o700)).unwrap();
        // The coordinator names its state directory by its canonical path.
        let base = fs::canonicalize(base).unwrap();
        let daemon = support::Owned::spawn(
            Command::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(base.join("state"))
                .stdout(Stdio::null())
                .stderr(fs::File::create(base.join("daemon.log")).unwrap()),
        );
        support::daemon_ready(&base.join("state"));
        Self {
            base,
            _daemon: daemon,
        }
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    /// Writes `frame` to a new connection, as any client of the socket can,
    /// and reads the reply.
    fn send(&self, frame: &[u8]) -> Value {
        let mut stream = UnixStream::connect(self.state().join("daemon.sock")).unwrap();
        stream.write_all(frame).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{error}: {line}"))
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Every command vector whose command is an owner grant: its op and frame.
fn owner_grant_vectors() -> Vec<(String, Vec<u8>)> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut vectors: Vec<(String, Vec<u8>)> = fs::read_dir(&fixtures)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with("-command-v10.json"))
        .filter_map(|path| {
            let frame = fs::read(&path).unwrap();
            let request: Request = serde_json::from_slice(&frame)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let op = serde_json::to_value(&request.command).unwrap()["op"]
                .as_str()
                .unwrap()
                .to_owned();
            (authority(&request.command) == Authority::OwnerGrant).then_some((op, frame))
        })
        .collect();
    vectors.sort();
    vectors
}

#[track_caller]
fn assert_refused(reply: &Value, op: &str, config: &Path) {
    let outcome = &reply["outcome"];
    assert_eq!(outcome["type"], "error", "{op}: {reply}");
    assert_eq!(outcome["code"], "owner_grant_off", "{op}: {reply}");
    let message = outcome["message"].as_str().unwrap();
    let how = format!("set \"owner_grants\": true in {}", config.display());
    assert!(message.contains(&how), "{op}: {message}");
    assert!(message.contains("run the command again"), "{op}: {message}");
}

#[test]
fn each_owner_grant_is_refused_until_the_owner_allows_it_and_again_once_off() {
    let vectors = owner_grant_vectors();
    let ops: Vec<&str> = vectors.iter().map(|(op, _)| op.as_str()).collect();
    assert_eq!(
        ops,
        [
            "auto_check_enable",
            "compact",
            "graph_export",
            "graph_import",
            "remote_enable",
            "set_policy",
            "workspace_remove",
        ],
        "ADR 0039's table and docs/usage name every owner grant"
    );
    let c = Coordinator::start("each");
    let config = c.state().join("config.json");

    // No config.json at all: refused.
    for (op, frame) in &vectors {
        assert_refused(&c.send(frame), op, &config);
    }
    // Each refusal is logged with its request ID, for the owner to see.
    let log = fs::read_to_string(c.base.join("daemon.log")).unwrap();
    let refused: Vec<Value> = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["event"] == "owner_grant_refused")
        .collect();
    assert_eq!(refused.len(), vectors.len(), "{log}");
    assert!(
        refused.iter().any(|line| line["request_id"] == "compact-1"),
        "{log}"
    );
    // A coordination command is unaffected.
    let status =
        c.send(b"{\"version\":10,\"request_id\":\"read\",\"command\":{\"op\":\"status\"}}\n");
    assert_eq!(status["outcome"]["type"], "status", "{status}");

    // The owner allows owner grants: each one reaches the coordinator, with
    // the same request ID, because the refusal recorded nothing.
    support::owner_grants(&c.state(), true);
    for (op, frame) in &vectors {
        let reply = c.send(frame);
        assert_ne!(reply["outcome"]["code"], "owner_grant_off", "{op}: {reply}");
        assert_eq!(reply["replayed"], false, "{op}: {reply}");
    }

    // Turned off again, without a restart: refused again, even a request
    // ID the coordinator has already answered.
    support::owner_grants(&c.state(), false);
    for (op, frame) in &vectors {
        assert_refused(&c.send(frame), op, &config);
    }
}

#[test]
fn a_config_json_that_cannot_be_read_refuses_and_says_why() {
    let c = Coordinator::start("bad");
    let config = c.state().join("config.json");
    // Not a setting the coordinator accepts: a safety control denies when
    // it fails, so this is not taken as on.
    fs::write(&config, r#"{"owner_grants":"yes"}"#).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    for (op, frame) in owner_grant_vectors() {
        let reply = c.send(&frame);
        assert_eq!(reply["outcome"]["code"], "owner_grant_off", "{op}: {reply}");
        let message = reply["outcome"]["message"].as_str().unwrap();
        assert!(message.contains("could not be read"), "{op}: {message}");
        assert!(
            message.contains("\"owner_grants\": true in"),
            "{op}: {message}"
        );
    }
    // A file others can read is refused too, though it says `true`.
    support::owner_grants(&c.state(), true);
    fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
    let (op, frame) = &owner_grant_vectors()[0];
    let reply = c.send(frame);
    assert_eq!(reply["outcome"]["code"], "owner_grant_off", "{op}: {reply}");
}

/// The refusal's wire frame, pinned in `tests/golden/frames/`.
#[test]
fn the_refusal_frame_is_pinned() {
    let c = Coordinator::start("frame");
    let frame =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/compact-command-v10.json"))
            .unwrap();
    let mut reply = c.send(&frame);
    let message = reply["outcome"]["message"]
        .as_str()
        .unwrap()
        .replace(c.state().to_str().unwrap(), "$STATE");
    reply["outcome"]["message"] = Value::String(message);
    golden::check(
        "frames/owner-grant-off.json",
        &format!("{}\n", serde_json::to_string_pretty(&reply).unwrap()),
    );
}
