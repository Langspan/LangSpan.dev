#![cfg(unix)]
//! Synthetic, source-derived Codex hook payloads through real child processes.
//! These tests do not stand in for the separately required live-host acceptance.

#[allow(dead_code)] // Only `output_once_not_busy` is used here.
mod support;

use langspan::{
    client,
    protocol::{PROTOCOL_VERSION, Request, Response},
};
use langspan_core::{Command, Event, LeaseStatus, Outcome};
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
const CODEX_SESSION: &str = "00000000-0000-4000-8000-000000000001";
const CLAUDE_SESSION: &str = "00000000-0000-4000-8000-000000000002";

struct Fixture {
    base: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "lscx-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(base.join("work/src")).unwrap();
        for file in ["a.txt", "b.txt", "src/c.txt"] {
            fs::write(base.join("work").join(file), "old\n").unwrap();
        }
        Self { base, daemon: None }
    }

    fn root(&self) -> PathBuf {
        fs::canonicalize(self.base.join("work")).unwrap()
    }

    fn state_dir(&self) -> PathBuf {
        self.base.join("state")
    }

    fn start(&mut self) {
        self.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(self.state_dir())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client::call(&self.state_dir(), &request("ready", Command::Status)).is_err() {
            if self.daemon.as_mut().unwrap().try_wait().unwrap().is_some()
                || Instant::now() >= deadline
            {
                let mut child = self.daemon.take().unwrap();
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "daemon did not start: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn call(&self, command: Command) -> Response {
        client::call(&self.state_dir(), &request("read", command)).unwrap()
    }

    fn events(&self) -> Vec<Event> {
        match self
            .call(Command::Events {
                after: 0,
                limit: 1000,
            })
            .outcome
        {
            Outcome::Events { events } => events.into_iter().map(|e| e.event).collect(),
            other => panic!("unexpected event response {other:?}"),
        }
    }

    fn hook(&self, adapter: &str, event: &Value) -> (Value, String) {
        self.hook_at(adapter, event, &self.root(), 30_000)
    }

    fn hook_at(&self, adapter: &str, event: &Value, root: &Path, ttl: u64) -> (Value, String) {
        let mut child = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args(["hook", adapter, "--workspace", "demo", "--root"])
            .arg(root)
            .arg("--state-dir")
            .arg(self.state_dir())
            .args(["--ttl-ms", &ttl.to_string()])
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

    fn codex(&self, phase: &str, id: &str, patch: &str) -> Value {
        let mut event: Value = serde_json::from_str(include_str!(
            "../fixtures/codex-contract/pre-apply-patch.json"
        ))
        .unwrap();
        event["cwd"] = json!(self.root());
        event["hook_event_name"] = json!(phase);
        event["tool_use_id"] = json!(id);
        event["tool_input"]["command"] = json!(patch);
        if phase == "PostToolUse" {
            event["tool_response"] = json!("Success. Updated the following files:\nM a.txt\n");
        }
        event
    }

    fn claude(&self, id: &str, path: &str) -> Value {
        json!({
            "hook_event_name": "PreToolUse", "session_id": CLAUDE_SESSION,
            "tool_use_id": id, "tool_name": "Edit", "cwd": self.root(),
            "tool_input": {"file_path": self.root().join(path), "old_string": "old", "new_string": "new"}
        })
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

fn request(id: &str, command: Command) -> Request {
    Request {
        version: PROTOCOL_VERSION,
        request_id: id.into(),
        command,
    }
}

fn patch(path: &str) -> String {
    format!("*** Begin Patch\n*** Update File: {path}\n@@\n-old\n+new\n*** End Patch")
}

#[test]
fn a_native_patch_admits_the_entire_scope_and_duplicate_post_records_once() {
    let mut f = Fixture::new();
    f.start();
    let mut pre: Value = serde_json::from_str(include_str!(
        "../fixtures/codex-contract/pre-apply-patch.json"
    ))
    .unwrap();
    pre["cwd"] = json!(f.root());
    let (result, stdout) = f.hook("codex", &pre);
    assert_eq!(result["coverage"], "admitted");
    assert!(stdout.is_empty());
    assert_eq!(f.hook("codex", &pre).0["coverage"], "admitted");
    let leases: Vec<_> = f
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::LeaseGranted { lease } => Some(lease),
            _ => None,
        })
        .collect();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].paths, ["a.txt", "new.txt"]);
    assert_eq!(leases[0].session_id, format!("codex.{CODEX_SESSION}"));

    let mut post: Value = serde_json::from_str(include_str!(
        "../fixtures/codex-contract/post-apply-patch.json"
    ))
    .unwrap();
    post["cwd"] = json!(f.root());
    assert_eq!(f.hook("codex", &post).0["coverage"], "observed");
    assert_eq!(f.hook("codex", &post).0["coverage"], "observed");
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| matches!(e, Event::OperationObserved { .. }))
            .count(),
        1
    );
    // A completed admission is only counted by `status`; `--all` lists it.
    assert_eq!(
        client::full_state(&f.state_dir()).unwrap().leases[&leases[0].lease_id].status,
        LeaseStatus::Completed
    );
}

#[test]
fn claude_conflict_denies_the_whole_codex_patch_and_unrelated_work_proceeds() {
    let mut f = Fixture::new();
    f.start();
    assert_eq!(
        f.hook("claude-code", &f.claude("holder", "a.txt")).0["coverage"],
        "admitted"
    );
    let body = "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** Update File: b.txt\n@@\n-old\n+new\n*** End Patch";
    let (result, stdout) = f.hook("codex", &f.codex("PreToolUse", "conflict", body));
    assert_eq!(result["coverage"], "denied");
    let denial: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(denial["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        denial["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("claude-code.")
    );
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| matches!(e, Event::LeaseGranted { .. }))
            .count(),
        1
    );
    assert_eq!(
        f.hook(
            "codex",
            &f.codex("PreToolUse", "other", &patch("src/c.txt"))
        )
        .0["coverage"],
        "admitted"
    );
}

#[test]
fn changed_patch_content_under_the_same_identity_is_rejected() {
    let mut f = Fixture::new();
    f.start();
    let mut event = f.codex("PreToolUse", "reuse", &patch("a.txt"));
    assert_eq!(f.hook("codex", &event).0["coverage"], "admitted");
    event["tool_input"]["command"] = json!(patch("a.txt").replace("+new", "+different"));
    let (result, stdout) = f.hook("codex", &event);
    assert_eq!(result["coverage"], "unknown");
    assert!(stdout.is_empty());
    assert_eq!(
        f.events()
            .iter()
            .filter(|e| matches!(e, Event::LeaseGranted { .. }))
            .count(),
        1
    );
}

#[test]
fn a_move_reserves_its_destination_and_cannot_skip_a_claude_holder() {
    let mut f = Fixture::new();
    f.start();
    assert_eq!(
        f.hook("claude-code", &f.claude("destination-holder", "b.txt"))
            .0["coverage"],
        "admitted"
    );
    let command = "*** Begin Patch\n*** Update File: a.txt\n*** Move to: b.txt\n@@\n-old\n+new\n*** End Patch";
    let (result, stdout) = f.hook("codex", &f.codex("PreToolUse", "move", command));
    assert_eq!(result["coverage"], "denied");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
    assert_eq!(
        f.events()
            .iter()
            .filter(|event| matches!(event, Event::LeaseGranted { .. }))
            .count(),
        1
    );
}

#[test]
fn a_late_codex_post_cannot_complete_another_current_operation() {
    let mut f = Fixture::new();
    f.start();
    let pre = f.codex("PreToolUse", "slow", &patch("a.txt"));
    assert_eq!(
        f.hook_at("codex", &pre, &f.root(), 100).0["coverage"],
        "admitted"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        f.hook("claude-code", &f.claude("later", "a.txt")).0["coverage"],
        "admitted"
    );
    let (result, stdout) = f.hook("codex", &f.codex("PostToolUse", "slow", &patch("a.txt")));
    assert_eq!(result["coverage"], "late_observation");
    assert!(stdout.is_empty());
    let observations: Vec<_> = f
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::OperationObserved { observation } => Some(observation),
            _ => None,
        })
        .collect();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].lease_relation,
        langspan_core::LeaseRelation::Expired
    );
    assert_eq!(observations[0].concurrent_holders.len(), 1);
    assert_eq!(
        observations[0].concurrent_holders[0].session_id,
        format!("claude-code.{CLAUDE_SESSION}")
    );
    match f.call(Command::Status).outcome {
        Outcome::Status { state, .. } => {
            assert!(state.leases.values().any(|lease| lease.session_id
                == format!("claude-code.{CLAUDE_SESSION}")
                && lease.status == LeaseStatus::Active))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn inherited_installation_tracks_the_event_worktree_without_false_file_conflicts() {
    check_inherited_worktree(false);
}

#[test]
fn inherited_installation_tracks_a_nested_worktree_without_false_file_conflicts() {
    check_inherited_worktree(true);
}

fn check_inherited_worktree(nested: bool) {
    let mut f = Fixture::new();
    let root = f.root();
    let git = |args: &[&str]| {
        let output = Process::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["add", "a.txt", "b.txt", "src/c.txt"]);
    git(&[
        "-c",
        "user.name=LangSpan test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "fixture",
    ]);
    let worktree = if nested {
        root.join(".worktrees/second")
    } else {
        f.base.join("second")
    };
    git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        worktree.to_str().unwrap(),
    ]);
    f.start();
    assert_eq!(
        f.hook("claude-code", &f.claude("main-holder", "a.txt")).0["coverage"],
        "admitted"
    );
    let mut event = f.codex("PreToolUse", "worktree-edit", &patch("a.txt"));
    event["cwd"] = json!(worktree);
    // The command inherited from the main checkout still names its install
    // anchor. The event cwd identifies the physically distinct worktree.
    assert_eq!(
        f.hook_at("codex", &event, &root, 30_000).0["coverage"],
        "admitted"
    );
    match f.call(Command::Status).outcome {
        Outcome::Status { state, .. } => {
            assert_eq!(state.workspaces.len(), 2);
            assert_eq!(
                state
                    .leases
                    .values()
                    .filter(|lease| lease.status == LeaseStatus::Active)
                    .count(),
                2
            );
            assert!(
                state
                    .workspaces
                    .values()
                    .any(|workspace| workspace.root == worktree.to_string_lossy())
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn post_without_pre_registers_a_session_and_records_an_unadmitted_result() {
    let mut f = Fixture::new();
    f.start();
    let (result, stdout) = f.hook(
        "codex",
        &f.codex("PostToolUse", "missing-pre", &patch("a.txt")),
    );
    assert_eq!(result["coverage"], "observed_unadmitted", "{result}");
    assert!(stdout.is_empty());
    let events = f.events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::LeaseGranted { .. }))
    );
    let observations: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            Event::OperationObserved { observation } => Some(observation),
            _ => None,
        })
        .collect();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].lease_relation,
        langspan_core::LeaseRelation::NotAdmitted
    );
}

#[test]
fn a_replayed_conflict_never_denies_using_an_outdated_holder() {
    let mut f = Fixture::new();
    f.start();
    let mut holder = f.claude("holder-before-replay", "a.txt");
    assert_eq!(f.hook("claude-code", &holder).0["coverage"], "admitted");
    let attempt = f.codex("PreToolUse", "denied-before-replay", &patch("a.txt"));
    assert_eq!(f.hook("codex", &attempt).0["coverage"], "denied");
    holder["hook_event_name"] = json!("PostToolUse");
    assert_eq!(f.hook("claude-code", &holder).0["coverage"], "observed");
    let (replayed, stdout) = f.hook("codex", &attempt);
    assert_eq!(replayed["coverage"], "unknown");
    assert!(stdout.is_empty());
    assert_eq!(
        f.hook(
            "codex",
            &f.codex("PreToolUse", "fresh-after-replay", &patch("a.txt"))
        )
        .0["coverage"],
        "admitted"
    );
}

#[test]
fn a_changed_post_scope_or_input_is_observed_without_completing_its_lease() {
    for (id, changed) in [
        (
            "post-input-change",
            patch("a.txt").replace("+new", "+different"),
        ),
        ("post-scope-change", patch("b.txt")),
    ] {
        let mut f = Fixture::new();
        f.start();
        assert_eq!(
            f.hook("codex", &f.codex("PreToolUse", id, &patch("a.txt")))
                .0["coverage"],
            "admitted"
        );
        let (result, stdout) = f.hook("codex", &f.codex("PostToolUse", id, &changed));
        assert_eq!(result["coverage"], "observed_mismatch");
        assert!(stdout.is_empty());
        let ids = langspan::hook::identities_for("codex", CODEX_SESSION, id);
        match f.call(Command::Status).outcome {
            Outcome::Status { state, .. } => {
                assert_eq!(state.leases[&ids.admit].status, LeaseStatus::Active)
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_new_tool_attempt_can_register_after_a_parent_lease_delayed_its_worktree() {
    let mut f = Fixture::new();
    let root = f.root();
    let git = |args: &[&str]| {
        let output = Process::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet", "--initial-branch=main"]);
    git(&["add", "a.txt"]);
    git(&[
        "-c",
        "user.name=LangSpan fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "commit",
        "--quiet",
        "-m",
        "fixture",
    ]);
    f.start();
    for (id, command) in [
        (
            "parent-workspace",
            Command::WorkspaceRegister {
                workspace_id: "demo".into(),
                root: root.to_str().unwrap().into(),
            },
        ),
        (
            "parent-session",
            Command::SessionRegister {
                session_id: "parent-session".into(),
                workspace_id: "demo".into(),
                parent: None,
                headless: false,
            },
        ),
        (
            "parent-grant",
            Command::Admit {
                session_id: "parent-session".into(),
                paths: vec!["nested/a.txt".into()],
                ttl_ms: 60_000,
            },
        ),
    ] {
        let response = client::call(&f.state_dir(), &request(id, command)).unwrap();
        assert!(!matches!(response.outcome, Outcome::Error { .. }));
    }
    let child = root.join("nested");
    git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        child.to_str().unwrap(),
    ]);
    let mut event = f.codex("PreToolUse", "blocked-registration", &patch("a.txt"));
    event["cwd"] = json!(child);
    let first = f.hook("codex", &event).0;
    assert_eq!(first["coverage"], "unknown");
    assert!(
        first["detail"]
            .as_str()
            .unwrap()
            .contains("workspace_in_use")
    );
    let completed = client::call(
        &f.state_dir(),
        &request(
            "finish-parent",
            Command::Complete {
                session_id: "parent-session".into(),
                lease_id: "parent-grant".into(),
            },
        ),
    )
    .unwrap();
    assert!(matches!(completed.outcome, Outcome::Completed { .. }));
    assert_eq!(
        f.hook("codex", &event).0["coverage"],
        "unknown",
        "the original operation retains its refused receipt"
    );
    event["tool_use_id"] = json!("fresh-registration");
    assert_eq!(
        f.hook("codex", &event).0["coverage"],
        "admitted",
        "a new tool attempt needs a new registration request identity"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn advisory_liveness_rejection_does_not_suppress_admission_or_observation() {
    let f = Fixture::new();
    let executable = f.base.join("codex");
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    // The owned subprocess is a process-identity fixture, not a model host.
    let output = support::output_once_not_busy(|| {
        let mut command = Process::new(&executable);
        command
            .args(["--ignored", "--exact", "advisory_liveness_process_fixture"])
            .env("LANGSPAN_LIVENESS_PROCESS_FIXTURE", "1");
        command
    });
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "owned subprocess fixture for advisory process-binding rejection"]
fn advisory_liveness_process_fixture() {
    if std::env::var_os("LANGSPAN_LIVENESS_PROCESS_FIXTURE").is_none() {
        return;
    }
    let mut f = Fixture::new();
    f.start();
    let mut other = Process::new("sleep").arg("30").spawn().unwrap();
    let process = langspan::process::identify(other.id()).unwrap();
    for (id, command) in [
        (
            "fixture-workspace",
            Command::WorkspaceRegister {
                workspace_id: "demo".into(),
                root: f.root().to_str().unwrap().into(),
            },
        ),
        (
            "fixture-session",
            Command::SessionRegister {
                session_id: format!("codex.{CODEX_SESSION}"),
                workspace_id: "demo".into(),
                parent: None,
                headless: false,
            },
        ),
        (
            "fixture-liveness",
            Command::ObserveSessionLiveness {
                session_id: format!("codex.{CODEX_SESSION}"),
                process,
                status: langspan_core::LivenessStatus::Alive,
            },
        ),
    ] {
        let response = client::call(&f.state_dir(), &request(id, command)).unwrap();
        assert!(!matches!(response.outcome, Outcome::Error { .. }));
    }
    let pre = f
        .hook(
            "codex",
            &f.codex("PreToolUse", "liveness-mismatch", &patch("a.txt")),
        )
        .0;
    let post = f
        .hook(
            "codex",
            &f.codex("PostToolUse", "liveness-mismatch", &patch("a.txt")),
        )
        .0;
    other.kill().unwrap();
    other.wait().unwrap();
    assert_eq!(pre["coverage"], "admitted");
    assert_eq!(post["coverage"], "observed");
    let request_id = langspan::hook::wire_id(
        "liveness",
        &[
            "codex",
            &langspan::hook::identities_for("codex", CODEX_SESSION, "liveness-mismatch")
                .operation_id,
        ],
    );
    let database = rusqlite::Connection::open(f.state_dir().join("state.sqlite3")).unwrap();
    let outcome: String = database
        .query_row(
            "SELECT outcome_json FROM receipts WHERE request_id=?1",
            [request_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        outcome.contains("process_identity_mismatch"),
        "fixture must exercise the advisory refusal: {outcome}"
    );
}
