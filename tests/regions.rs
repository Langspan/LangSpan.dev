#![cfg(unix)]
//! Region admission through the real hook executables and a real daemon:
//! edits of different functions of one file proceed together; edits of one
//! function are serialized, and the denial names it.

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{Command, Event, Outcome, Region};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ALICE: &str = "00000000-0000-4000-8000-00000000000a";
const BOB: &str = "00000000-0000-4000-8000-00000000000b";
const CODEX: &str = "00000000-0000-4000-8000-00000000000c";

const SOURCE: &str = "pub fn alpha() -> u32 {
    let one = 1;
    one
}

pub fn beta() -> u32 {
    2
}
";

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-regions-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("work/src")).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(base.join("work/src/lib.rs"), SOURCE).unwrap();
        let mut fixture = Self { base, daemon: None };
        fixture.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .args(["daemon", "--state-dir"])
                .arg(fixture.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&fixture.state_dir(), &request(Command::Status)).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }

    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }

    fn root(&self) -> PathBuf {
        fs::canonicalize(self.base.join("work")).unwrap()
    }

    /// Runs the hook; returns its diagnostic and its stdout.
    fn hook(&self, adapter: &str, event: &Value) -> (Value, String) {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .args(["hook", adapter, "--workspace", "demo", "--root"])
            .arg(self.root())
            .arg("--state-dir")
            .arg(self.state_dir())
            .args(["--ttl-ms", "30000"])
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
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(!stdout.contains("\"allow\""), "{stdout}");
        let diagnostic: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        (diagnostic, stdout)
    }

    fn claude(&self, session: &str, id: &str, old: &str, new: &str) -> Value {
        json!({
            "hook_event_name": "PreToolUse", "session_id": session,
            "transcript_path": "/dev/null", "permission_mode": "default",
            "tool_use_id": id, "tool_name": "Edit", "cwd": self.root(),
            "tool_input": {
                "file_path": self.root().join("src/lib.rs"),
                "old_string": old, "new_string": new, "replace_all": false
            }
        })
    }

    fn codex(&self, id: &str, patch: &str) -> Value {
        let mut event: Value = serde_json::from_str(include_str!(
            "../fixtures/codex-contract/pre-apply-patch.json"
        ))
        .unwrap();
        event["session_id"] = json!(CODEX);
        event["cwd"] = json!(self.root());
        event["tool_use_id"] = json!(id);
        event["tool_input"]["command"] = json!(patch);
        event
    }

    fn leases(&self) -> Vec<langspan_core::Lease> {
        match client::call(
            &self.state_dir(),
            &request(Command::Events {
                after: 0,
                limit: 1000,
            }),
        )
        .unwrap()
        .outcome
        {
            Outcome::Events { events } => events
                .into_iter()
                .filter_map(|recorded| match recorded.event {
                    Event::LeaseGranted { lease } => Some(lease),
                    _ => None,
                })
                .collect(),
            other => panic!("{other:?}"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn request(command: Command) -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command,
    }
}

fn keys(lease: &langspan_core::Lease) -> Vec<String> {
    lease
        .regions
        .iter()
        .flat_map(|located| &located.regions)
        .map(|region| match region {
            Region::Symbol { key, .. } => key.join("::"),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn reason(stdout: &str) -> String {
    let value: Value = serde_json::from_str(stdout).expect("deny JSON");
    assert_eq!(value["hookSpecificOutput"]["permissionDecision"], "deny");
    value["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn two_claude_sessions_editing_different_functions_of_one_file_are_both_admitted() {
    let f = Fixture::new();
    let (alice, stdout) = f.hook(
        "claude-code",
        &f.claude(ALICE, "toolu_a", "    one\n", "    one + 1\n"),
    );
    assert_eq!(
        (alice["coverage"].as_str(), stdout.as_str()),
        (Some("admitted"), "")
    );
    // Alice's admission is current: her tool has not reported back.
    let (bob, stdout) = f.hook(
        "claude-code",
        &f.claude(BOB, "toolu_b", "    2\n", "    20\n"),
    );
    assert_eq!(
        (bob["coverage"].as_str(), stdout.as_str()),
        (Some("admitted"), ""),
        "{bob}"
    );
    let leases = f.leases();
    assert_eq!(leases.len(), 2);
    assert_eq!(keys(&leases[0]), ["alpha"]);
    assert_eq!(keys(&leases[1]), ["beta"]);
}

#[test]
fn a_second_edit_of_the_same_function_is_denied_with_a_reason_naming_it() {
    let f = Fixture::new();
    assert_eq!(
        f.hook(
            "claude-code",
            &f.claude(ALICE, "toolu_a", "    one\n", "    one + 1\n")
        )
        .0["coverage"],
        "admitted"
    );
    // Another line, the same function.
    let (bob, stdout) = f.hook(
        "claude-code",
        &f.claude(BOB, "toolu_b", "let one = 1;", "let one = 10;"),
    );
    assert_eq!(bob["coverage"], "denied");
    let reason = reason(&stdout);
    assert!(reason.contains("\"alpha\""), "{reason}");
    assert!(reason.contains(&format!("claude-code.{ALICE}")), "{reason}");
    assert!(reason.contains("src/lib.rs"), "{reason}");
    assert_eq!(f.leases().len(), 1);
}

#[test]
fn an_edit_that_cannot_be_placed_is_held_as_the_whole_file() {
    let f = Fixture::new();
    assert_eq!(
        f.hook(
            "claude-code",
            &f.claude(ALICE, "toolu_a", "    one\n", "    one + 1\n")
        )
        .0["coverage"],
        "admitted"
    );
    // `}` occurs twice: the edit's place is ambiguous.
    let (bob, stdout) = f.hook("claude-code", &f.claude(BOB, "toolu_b", "}\n", "};\n"));
    assert_eq!(bob["coverage"], "denied");
    let reason = reason(&stdout);
    assert!(reason.contains("\"alpha\""), "{reason}");
    assert!(reason.contains("whole file"), "{reason}");
}

#[test]
fn a_codex_patch_and_a_claude_edit_in_different_functions_are_both_admitted() {
    let f = Fixture::new();
    assert_eq!(
        f.hook(
            "claude-code",
            &f.claude(ALICE, "toolu_a", "    one\n", "    one + 1\n")
        )
        .0["coverage"],
        "admitted"
    );
    let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@ pub fn beta() -> u32 {\n-    2\n+    20\n*** End Patch";
    let (codex, stdout) = f.hook("codex", &f.codex("call_beta", patch));
    assert_eq!(
        (codex["coverage"].as_str(), stdout.as_str()),
        (Some("admitted"), ""),
        "{codex}"
    );
    // The same function from Codex is denied.
    let same = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n     let one = 1;\n-    one\n+    one * 2\n }\n*** End Patch";
    let (codex, stdout) = f.hook("codex", &f.codex("call_alpha", same));
    assert_eq!(codex["coverage"], "denied");
    assert!(reason(&stdout).contains("\"alpha\""));
    let leases = f.leases();
    assert_eq!(leases.len(), 2);
    assert_eq!(keys(&leases[1]), ["beta"]);
}
