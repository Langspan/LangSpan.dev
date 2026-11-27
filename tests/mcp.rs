#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use langspan::protocol::{MAX_FRAME_BYTES, Request, Response};
use langspan_core::{Command as Operation, Event, FileKind, FileState, LeaseStatus, Outcome};
use rmcp::model::{CallToolRequestParams, CallToolResult, ProtocolVersion};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientLifecycleMode, ClientServiceExt, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::{Instant, sleep, timeout};

const DEADLINE: Duration = Duration::from_secs(10);
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "langspan-mcp-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create isolated MCP test directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("restrict MCP test directory permissions");
        Self(path)
    }

    fn state_dir(&self) -> PathBuf {
        self.0.join("state")
    }

    fn workspace(&self) -> PathBuf {
        let workspace = self.0.join("workspace");
        fs::create_dir_all(workspace.join("src")).expect("create real workspace");
        fs::write(workspace.join("src/shared.rs"), "// shared test file\n")
            .expect("create real file for admission");
        workspace
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("MCP test directory cleanup failed: {error}");
        }
    }
}

struct Daemon(Child);

impl Daemon {
    async fn start(state_dir: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("daemon")
            .arg("--state-dir")
            .arg(state_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start real LangSpan daemon");
        let mut daemon = Self(child);
        let deadline = Instant::now() + DEADLINE;
        while !state_dir.join("daemon.sock").exists() {
            assert!(
                daemon.0.try_wait().expect("check daemon process").is_none(),
                "daemon exited before binding its socket"
            );
            assert!(Instant::now() < deadline, "daemon startup timed out");
            sleep(Duration::from_millis(10)).await;
        }
        daemon
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // A process that already exited needs only to be reaped.
        if matches!(self.0.try_wait(), Ok(None))
            && let Err(error) = self.0.kill()
        {
            eprintln!("MCP test daemon termination failed: {error}");
        }
        if let Err(error) = self.0.wait() {
            eprintln!("MCP test daemon reap failed: {error}");
        }
    }
}

fn child_transport(state_dir: &Path) -> TokioChildProcess {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command.arg("mcp").arg("--state-dir").arg(state_dir);
    TokioChildProcess::new(command).expect("spawn real MCP stdio adapter")
}

async fn connect(state_dir: &Path) -> RunningService<RoleClient, ()> {
    // ServiceExt::serve performs initialize and notifications/initialized;
    // tools are never called on an uninitialized hand-written connection.
    timeout(DEADLINE, ().serve(child_transport(state_dir)))
        .await
        .expect("MCP initialization timed out")
        .expect("MCP initialization failed")
}

async fn call_tool(
    client: &RunningService<RoleClient, ()>,
    name: &'static str,
    arguments: Value,
) -> CallToolResult {
    let request = CallToolRequestParams::new(name)
        .with_arguments(arguments.as_object().expect("object arguments").clone());
    timeout(DEADLINE, client.call_tool(request))
        .await
        .expect("MCP tool request timed out")
        .expect("MCP tool protocol request failed")
}

fn response(result: CallToolResult) -> Response {
    assert_eq!(result.is_error, Some(false), "unexpected tool failure");
    serde_json::from_value(
        result
            .structured_content
            .expect("structured daemon response"),
    )
    .expect("tool result uses the shared response envelope")
}

async fn disconnect(client: RunningService<RoleClient, ()>) {
    timeout(DEADLINE, client.cancel())
        .await
        .expect("MCP shutdown timed out")
        .expect("MCP shutdown failed");
}

async fn call_ipc(state_dir: &Path, request_id: &str, command: Operation) -> Response {
    let state_dir = state_dir.to_path_buf();
    let request = Request::new(request_id, command);
    tokio::task::spawn_blocking(move || langspan::client::call(&state_dir, &request))
        .await
        .expect("independent IPC task")
        .expect("independent IPC request")
}

#[tokio::test]
async fn stdio_lists_typed_tools_and_reports_an_unavailable_daemon() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let client = connect(&state_dir).await;
    let tools = timeout(DEADLINE, client.list_all_tools())
        .await
        .expect("tools/list timed out")
        .expect("tools/list failed");
    let names: BTreeSet<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    // The server lists exactly the tools in the one list, `TOOL_NAMES`.
    if let Err(problem) = langspan::mcp::compare_tool_names(names.iter().copied()) {
        panic!("{problem}");
    }
    // Every tool the LangSpan skill and the guidance block name is one this
    // server lists.
    for (what, named) in [
        ("skill", langspan::skill::tools()),
        ("guidance block", langspan::guidance::tools()),
    ] {
        assert!(!named.is_empty(), "{what}");
        for tool in named {
            assert!(
                names.contains(tool),
                "the {what} names {tool}, which is not listed"
            );
        }
    }
    for tool in &tools {
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&json!(false)),
            "tool inputs reject unknown fields: {}",
            tool.name
        );
        if !matches!(
            tool.name.as_ref(),
            "langspan_status"
                | "langspan_events"
                | "langspan_changes"
                | "langspan_symbol"
                | "langspan_references"
                | "langspan_impact"
                | "langspan_path"
                | "langspan_map"
                | "langspan_blind_spots"
                | "langspan_recap"
                | "langspan_show_message"
                | "langspan_message_status"
                | "langspan_recall"
                | "langspan_history"
                | "langspan_today"
                | "langspan_report"
                | "langspan_land_queue"
                | "langspan_flaky_tests"
                | "langspan_policy"
                | "langspan_check_write"
        ) {
            assert!(
                tool.input_schema["required"]
                    .as_array()
                    .expect("required argument list")
                    .contains(&json!("request_id")),
                "mutations require a caller-owned retry identity: {}",
                tool.name
            );
        }
    }

    let unavailable = call_tool(&client, "langspan_status", json!({})).await;
    assert_eq!(unavailable.is_error, Some(true));
    let error = unavailable
        .structured_content
        .expect("structured adapter error");
    assert_eq!(error["code"], "daemon_request_failed");
    assert!(
        !state_dir.exists(),
        "MCP must not create its own coordination store"
    );

    let changes = call_tool(
        &client,
        "langspan_changes",
        json!({
            "workspace_id":"workspace", "after":0, "limit":25,
        }),
    )
    .await;
    assert_eq!(changes.is_error, Some(true));
    assert_eq!(
        changes.structured_content.unwrap()["code"],
        "daemon_request_failed"
    );

    let invalid = CallToolRequestParams::new("langspan_admit").with_arguments(
        json!({"request_id":"bad-args", "session_id":"a", "paths":["src/shared.rs"],
               "ttl_ms":5000, "unexpected":true})
        .as_object()
        .expect("object")
        .clone(),
    );
    let rejected = timeout(DEADLINE, client.call_tool(invalid))
        .await
        .expect("invalid-argument response timed out")
        .expect("SDK returns typed argument errors as tool results");
    assert_eq!(rejected.is_error, Some(true));
    let rejected = serde_json::to_value(rejected).expect("serialize typed argument error");
    let diagnostic = rejected["content"][0]["text"]
        .as_str()
        .expect("typed argument diagnostic");
    assert!(
        diagnostic.contains("unknown field") && diagnostic.contains("unexpected"),
        "reject the extra field before daemon forwarding: {diagnostic}"
    );
    disconnect(client).await;
}

#[tokio::test]
async fn stdio_supports_current_discovery_lifecycle() {
    let directory = TestDirectory::new();
    let client = timeout(
        DEADLINE,
        ().serve_with_lifecycle(
            child_transport(&directory.state_dir()),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        ),
    )
    .await
    .expect("MCP discovery timed out")
    .expect("MCP discovery failed");
    let tools = timeout(DEADLINE, client.list_all_tools())
        .await
        .expect("discovery-lifecycle tools/list timed out")
        .expect("discovery-lifecycle tools/list failed");
    if let Err(problem) =
        langspan::mcp::compare_tool_names(tools.iter().map(|tool| tool.name.as_ref()))
    {
        panic!("tools/list over stdio: {problem}");
    }
    let result = call_tool(&client, "langspan_status", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("adapter error")["code"],
        "daemon_request_failed"
    );
    disconnect(client).await;
}

#[tokio::test]
async fn stdio_blind_spots_names_a_partial_file_through_the_daemon() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = fs::canonicalize(directory.workspace()).expect("resolve workspace");
    fs::write(workspace.join("src/broken.rs"), "pub fn half( {{{\n").expect("write file");
    let git = Command::new("git")
        .arg("-C")
        .arg(&workspace)
        .args(["init", "--quiet"])
        .status()
        .expect("run git");
    assert!(git.success());
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"workspace-register", "workspace_id":"workspace", "root":workspace}),
        )
        .await,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let outcome = loop {
        let answer = response(
            call_tool(
                &client,
                "langspan_blind_spots",
                json!({"workspace_id":"workspace", "limit":10}),
            )
            .await,
        );
        let Outcome::GraphBlindSpots {
            blind_spots,
            coverage,
            ..
        } = answer.outcome
        else {
            panic!("unexpected outcome: {:?}", answer.outcome);
        };
        // Both files indexed: until then the other one is waiting.
        if !blind_spots.partial.is_empty() && coverage.current_files == 2 {
            break (blind_spots, coverage);
        }
        assert!(Instant::now() < deadline, "the partial file never appeared");
        sleep(Duration::from_millis(100)).await;
    };
    let (blind_spots, coverage) = outcome;
    assert_eq!(blind_spots.partial[0].path, "src/broken.rs");
    assert_eq!(blind_spots.never_clean, ["src/broken.rs"]);
    assert_eq!(coverage.blind_in_scope, 1);
    let bad = call_tool(
        &client,
        "langspan_blind_spots",
        json!({"workspace_id":"workspace", "limit":0}),
    )
    .await;
    let bad = serde_json::to_string(&bad).expect("serialize");
    assert!(bad.contains("limit must be between 1 and 200"), "{bad}");
    disconnect(client).await;
}

#[tokio::test]
async fn stdio_changes_reads_uncached_pages_and_status_forwards_liveness() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = directory.workspace();
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({
                "request_id":"workspace-register", "workspace_id":"workspace", "root":workspace,
            }),
        )
        .await,
    );
    response(
        call_tool(
            &client,
            "langspan_session_register",
            json!({
                "request_id":"session-register", "session_id":"agent-a", "workspace_id":"workspace",
            }),
        )
        .await,
    );

    let empty = response(
        call_tool(
            &client,
            "langspan_changes",
            json!({
                "workspace_id":"workspace", "after":0, "limit":1,
            }),
        )
        .await,
    );
    assert!(
        matches!(empty.outcome, Outcome::Changes { ref events, scan: None } if events.is_empty())
    );
    assert!(!empty.replayed);

    let empty_digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let a_digest = "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb";
    fs::write(workspace.join("src/shared.rs"), b"").unwrap();
    let first = call_ipc(
        &state_dir,
        "disk-first",
        Operation::ObserveDisk {
            workspace_id: "workspace".into(),
            path: "src/shared.rs".into(),
            before: None,
            after: FileState::Present {
                digest: empty_digest.into(),
                kind: FileKind::Regular,
            },
            related_operation_ids: vec![],
        },
    )
    .await;
    assert!(matches!(first.outcome, Outcome::DiskObserved { .. }));
    fs::write(workspace.join("src/shared.rs"), b"a").unwrap();
    let second = call_ipc(
        &state_dir,
        "disk-second",
        Operation::ObserveDisk {
            workspace_id: "workspace".into(),
            path: "src/shared.rs".into(),
            before: Some(FileState::Present {
                digest: empty_digest.into(),
                kind: FileKind::Regular,
            }),
            after: FileState::Present {
                digest: a_digest.into(),
                kind: FileKind::Regular,
            },
            related_operation_ids: vec![],
        },
    )
    .await;
    assert!(matches!(second.outcome, Outcome::DiskObserved { .. }));

    let first_page = response(
        call_tool(
            &client,
            "langspan_changes",
            json!({
                "workspace_id":"workspace", "after":0, "limit":1,
            }),
        )
        .await,
    );
    assert!(
        !first_page.replayed,
        "the same read ID must not cache the original empty result"
    );
    let Outcome::Changes { events, .. } = first_page.outcome else {
        panic!("expected changes")
    };
    assert_eq!(events.len(), 1);
    let first_sequence = events[0].seq;
    assert!(
        matches!(&events[0].event, Event::DiskObserved { observation } if observation.before.is_none())
    );
    let next_page = response(
        call_tool(
            &client,
            "langspan_changes",
            json!({
                "workspace_id":"workspace", "after":first_sequence, "limit":1,
            }),
        )
        .await,
    );
    assert!(!next_page.replayed);
    let Outcome::Changes { events, .. } = next_page.outcome else {
        panic!("expected changes")
    };
    assert_eq!(events.len(), 1);
    assert!(events[0].seq > first_sequence);
    assert!(
        matches!(&events[0].event, Event::DiskObserved { observation }
        if observation.before.as_ref() == Some(&FileState::Present { digest: empty_digest.into(), kind: FileKind::Regular })
        && observation.after == FileState::Present { digest: a_digest.into(), kind: FileKind::Regular })
    );

    #[cfg(target_os = "linux")]
    {
        let attached = Command::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .args([
                "session",
                "attach-pid",
                "agent-a",
                &std::process::id().to_string(),
                "--request-id",
                "attach-test-process",
                "--state-dir",
            ])
            .arg(&state_dir)
            .output()
            .unwrap();
        assert!(
            attached.status.success(),
            "{}",
            String::from_utf8_lossy(&attached.stderr)
        );
        let status = response(call_tool(&client, "langspan_status", json!({})).await);
        let Outcome::Status { state, .. } = status.outcome else {
            panic!("expected status")
        };
        let observed = &state.session_liveness["agent-a"];
        assert_eq!(observed.process.pid, std::process::id());
        assert_eq!(observed.status, langspan_core::LivenessStatus::Alive);
        assert_eq!(observed.generation, state.generation);
    }
    disconnect(client).await;
}

#[tokio::test]
async fn stdio_operations_share_daemon_state_and_preserve_retry_and_conflict_results() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = directory.workspace();
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;

    let registered = response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({
                "request_id":"workspace-register", "workspace_id":"workspace", "root":workspace,
            }),
        )
        .await,
    );
    assert!(matches!(registered.outcome, Outcome::Workspace { .. }));
    for session in ["agent-a", "agent-b"] {
        let registered = response(
            call_tool(
                &client,
                "langspan_session_register",
                json!({
                    "request_id":format!("register-{session}"), "session_id":session,
                    "workspace_id":"workspace",
                }),
            )
            .await,
        );
        assert!(matches!(registered.outcome, Outcome::Session { .. }));
    }
    let intent = response(
        call_tool(
            &client,
            "langspan_intent_set",
            json!({
                "request_id":"intent-a", "session_id":"agent-a", "paths":["src/shared.rs"],
                "summary":"Edit the shared file",
            }),
        )
        .await,
    );
    assert!(matches!(intent.outcome, Outcome::Intent { .. }));

    let admission_arguments = json!({
        "request_id":"admit-a", "session_id":"agent-a", "paths":["src/shared.rs"],
        "ttl_ms":60000,
    });
    let admitted =
        response(call_tool(&client, "langspan_admit", admission_arguments.clone()).await);
    assert_eq!(admitted.lease_valid, Some(true));
    assert!(
        matches!(admitted.outcome, Outcome::Admitted { ref lease } if lease.lease_id == "admit-a")
    );
    let replayed = response(call_tool(&client, "langspan_admit", admission_arguments).await);
    assert!(replayed.replayed);
    assert_eq!(replayed.outcome, admitted.outcome);

    let conflict = response(
        call_tool(
            &client,
            "langspan_admit",
            json!({
                "request_id":"admit-b-conflict", "session_id":"agent-b", "paths":["src/shared.rs"],
                "ttl_ms":60000,
            }),
        )
        .await,
    );
    assert!(matches!(conflict.outcome, Outcome::Conflict { ref holders }
        if holders.len() == 1 && holders[0].session_id == "agent-a"));

    let independent_dir = state_dir.clone();
    let observed = tokio::task::spawn_blocking(move || {
        langspan::client::call(
            &independent_dir,
            &Request::new("independent-status", Operation::Status),
        )
    })
    .await
    .expect("independent client task")
    .expect("independent IPC status");
    assert!(matches!(observed.outcome, Outcome::Status { ref state, .. }
        if state.leases["admit-a"].status == LeaseStatus::Active
        && state.intents.contains_key("agent-a")));

    let denied = call_tool(
        &client,
        "langspan_complete",
        json!({
            "request_id":"wrong-holder", "session_id":"agent-b", "lease_id":"admit-a",
        }),
    )
    .await;
    assert_eq!(
        denied.is_error,
        Some(true),
        "domain errors remain tool errors"
    );
    let denied: Response =
        serde_json::from_value(denied.structured_content.expect("domain error envelope"))
            .expect("shared domain response");
    assert!(matches!(denied.outcome, Outcome::Error { .. }));

    let completed = response(
        call_tool(
            &client,
            "langspan_complete",
            json!({
                "request_id":"complete-a", "session_id":"agent-a", "lease_id":"admit-a",
            }),
        )
        .await,
    );
    assert!(matches!(completed.outcome, Outcome::Completed { .. }));
    let admitted_b = response(
        call_tool(
            &client,
            "langspan_admit",
            json!({
                "request_id":"admit-b-fresh", "session_id":"agent-b", "paths":["src/shared.rs"],
                "ttl_ms":60000,
            }),
        )
        .await,
    );
    assert!(matches!(admitted_b.outcome, Outcome::Admitted { .. }));

    let ended = response(
        call_tool(
            &client,
            "langspan_intent_end",
            json!({
                "request_id":"end-intent-a", "session_id":"agent-a",
            }),
        )
        .await,
    );
    assert!(matches!(ended.outcome, Outcome::IntentEnded { .. }));
    // The bounded view lists what is open and counts what closed.
    let status = response(call_tool(&client, "langspan_status", json!({})).await);
    assert!(
        matches!(status.outcome, Outcome::Status { ref state, ref summary }
        if !state.intents.contains_key("agent-a")
        && !state.leases.contains_key("admit-a")
        && state.leases["admit-b-fresh"].status == LeaseStatus::Active
        && summary.as_ref().is_some_and(|summary| summary.closed_admissions["agent-a"] == 1))
    );
    // `all` pages through every record, the closed admission included.
    let page = response(call_tool(&client, "langspan_status", json!({"all": true})).await);
    let Outcome::StatusRecords { records, next } = page.outcome else {
        panic!("expected a status page, got {:?}", page.outcome)
    };
    assert_eq!(next, None, "a small state fits one page");
    let mut whole = langspan_core::State::default();
    for record in records {
        record.apply(&mut whole).unwrap();
    }
    assert_eq!(whole.leases["admit-a"].status, LeaseStatus::Completed);
    assert_eq!(whole.leases["admit-b-fresh"].status, LeaseStatus::Active);
    let events =
        response(call_tool(&client, "langspan_events", json!({"after":0,"limit":100})).await);
    let Outcome::Events { events } = events.outcome else {
        panic!("events tool must return the daemon event log");
    };
    assert!(!events.is_empty());
    assert!(events.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    disconnect(client).await;
}

#[tokio::test]
async fn stdio_rejects_oversized_frames_and_recovers_for_the_next_request() {
    let directory = TestDirectory::new();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("mcp")
        .arg("--state-dir")
        .arg(directory.state_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("start stdio framing test subprocess");
    let mut input = child.stdin.take().expect("child stdin");
    let mut output = BufReader::new(child.stdout.take().expect("child stdout")).lines();

    // Raw frames are used only to probe the transport boundary. They still
    // complete the supported initialize lifecycle before application traffic.
    let mut oversized = json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{"protocolVersion":"2025-11-25", "capabilities":{},
            "clientInfo":{"name":"langspan-framing-test", "version":"0.1.0"}},
    });
    oversized["params"]["_meta"] = json!({"padding":"x".repeat(MAX_FRAME_BYTES)});
    timeout(
        DEADLINE,
        input.write_all(format!("{oversized}\n").as_bytes()),
    )
    .await
    .expect("oversized frame write timed out")
    .expect("oversized frame write");

    // The SDK drops an oversized line without a reply. Its startup lifecycle
    // is sequential: accepting the oversized initialize would answer id 1,
    // while discarding it must make this next initialize the first response.
    // This avoids relying on a timing-based absence-of-response assertion.
    let initialize = json!({
        "jsonrpc":"2.0", "id":2, "method":"initialize",
        "params":{"protocolVersion":"2025-11-25", "capabilities":{},
            "clientInfo":{"name":"langspan-framing-test", "version":"0.1.0"}},
    });
    timeout(
        DEADLINE,
        input.write_all(format!("{initialize}\n").as_bytes()),
    )
    .await
    .expect("initialize write timed out")
    .expect("initialize write");
    let initialized: Value = serde_json::from_str(
        &timeout(DEADLINE, output.next_line())
            .await
            .expect("initialize read timed out")
            .expect("initialize read")
            .expect("initialize response"),
    )
    .expect("initialize JSON");
    assert_eq!(
        initialized["id"], 2,
        "the oversized opener must be discarded"
    );
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    let initialized_notification =
        b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
    timeout(DEADLINE, input.write_all(initialized_notification))
        .await
        .expect("initialized notification timed out")
        .expect("initialized notification");

    timeout(
        DEADLINE,
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n"),
    )
    .await
    .expect("recovery ping write timed out")
    .expect("recovery ping write");
    let recovered: Value = serde_json::from_str(
        &timeout(DEADLINE, output.next_line())
            .await
            .expect("recovery ping read timed out")
            .expect("recovery ping read")
            .expect("recovery ping response"),
    )
    .expect("recovery ping JSON");
    assert_eq!(recovered["id"], 3);
    assert!(recovered.get("result").is_some());
    drop(input);
    assert!(
        timeout(DEADLINE, child.wait())
            .await
            .expect("MCP exit after EOF timed out")
            .expect("MCP exit after EOF")
            .success()
    );
}

/// `langspan_symbol` with `detail` through a real MCP client, daemon,
/// indexer and Git repository (roadmap D1).
#[tokio::test]
async fn stdio_symbol_with_detail_returns_calls_and_callers_by_name() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = directory.workspace();
    for (path, text) in [
        (
            "src/upload.rs",
            "pub fn upload(path: &str) -> bool {\n    let file = open(path);\n    let again = open(path);\n    hash(file, again)\n}\n",
        ),
        ("src/cli.rs", "fn main() {\n    upload(\"a\");\n}\n"),
        ("src/api.rs", "fn serve() {\n\n    upload(\"b\");\n}\n"),
    ] {
        fs::write(workspace.join(path), text).expect("write source");
    }
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["add", "."],
        &["commit", "--quiet", "-m", "init"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    }
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"workspace-register", "workspace_id":"workspace", "root":workspace}),
        )
        .await,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let outcome = loop {
        let answer = call_tool(
            &client,
            "langspan_symbol",
            json!({"workspace_id":"workspace", "name":"upload", "limit":10, "detail":true}),
        )
        .await;
        let outcome = answer.structured_content.unwrap_or_default()["outcome"].clone();
        if outcome["symbols"][0]["callers"]["count"] == 2 {
            break outcome;
        }
        assert!(Instant::now() < deadline, "never indexed: {outcome:#}");
        sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(outcome["resolution"], "name");
    let upload = &outcome["symbols"][0];
    assert_eq!(
        upload["calls"],
        json!([{"name":"open","count":2,"line":2},{"name":"hash","count":1,"line":4}])
    );
    assert_eq!(
        upload["callers"]["sites"],
        json!(["src/api.rs:3", "src/cli.rs:2"])
    );
    assert_eq!(upload["body_digest"].as_str().map(str::len), Some(64));

    let plain = response(
        call_tool(
            &client,
            "langspan_symbol",
            json!({"workspace_id":"workspace", "name":"upload", "limit":10}),
        )
        .await,
    );
    let Outcome::Graph {
        symbols,
        resolution,
        ..
    } = plain.outcome
    else {
        panic!("{:?}", plain.outcome);
    };
    assert_eq!(resolution, None);
    assert_eq!((&symbols[0].body_digest, &symbols[0].calls), (&None, &None));
    disconnect(client).await;
}

#[tokio::test]
async fn stdio_show_message_returns_the_whole_text_after_acknowledgement() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = directory.workspace();
    let _daemon = Daemon::start(&state_dir).await;
    let registered = call_ipc(
        &state_dir,
        "workspace",
        Operation::WorkspaceRegister {
            workspace_id: "workspace".into(),
            root: workspace.to_string_lossy().into_owned(),
        },
    )
    .await;
    assert!(matches!(registered.outcome, Outcome::Workspace { .. }));
    let session = call_ipc(
        &state_dir,
        "session",
        Operation::SessionRegister {
            session_id: "agent-b".into(),
            workspace_id: "workspace".into(),
            parent: None,
            headless: false,
        },
    )
    .await;
    assert!(matches!(session.outcome, Outcome::Session { .. }));
    let text = "\"é".repeat(133);
    let paths: Vec<String> = (0..8).map(|n| format!("src/{n}.rs")).collect();
    let sent = call_ipc(
        &state_dir,
        "m-9",
        Operation::SendMessage {
            workspace_id: "workspace".into(),
            from: None,
            to: Some("agent-b".into()),
            text: text.clone(),
            paths: paths.clone(),
        },
    )
    .await;
    assert!(matches!(sent.outcome, Outcome::MessageSent { .. }));
    let acknowledged = call_ipc(
        &state_dir,
        "ack",
        Operation::AckMessage {
            session_id: "agent-b".into(),
            message_id: "m-9".into(),
        },
    )
    .await;
    assert!(matches!(
        acknowledged.outcome,
        Outcome::MessageAcknowledged {
            acknowledged: true,
            ..
        }
    ));

    let client = connect(&state_dir).await;
    let shown = response(
        call_tool(
            &client,
            "langspan_show_message",
            json!({"workspace_id": "workspace", "message_id": "m-9"}),
        )
        .await,
    );
    let Outcome::MessageShown { message } = shown.outcome else {
        panic!("{shown:?}");
    };
    assert_eq!(
        (message.text, message.paths, message.from, message.retracted),
        (text, paths, None, false)
    );
    let unknown = call_tool(
        &client,
        "langspan_show_message",
        json!({"workspace_id": "workspace", "message_id": "m-404"}),
    )
    .await;
    assert_eq!(unknown.is_error, Some(true));
    assert_eq!(
        unknown.structured_content.unwrap()["outcome"]["code"],
        "unknown_message"
    );
    // A message is found only in the workspace it was sent in.
    let elsewhere = directory.0.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let registered = call_ipc(
        &state_dir,
        "elsewhere",
        Operation::WorkspaceRegister {
            workspace_id: "elsewhere".into(),
            root: elsewhere.to_string_lossy().into_owned(),
        },
    )
    .await;
    assert!(matches!(registered.outcome, Outcome::Workspace { .. }));
    let other = call_tool(
        &client,
        "langspan_show_message",
        json!({"workspace_id": "elsewhere", "message_id": "m-9"}),
    )
    .await;
    assert_eq!(other.is_error, Some(true));
    assert_eq!(
        other.structured_content.unwrap()["outcome"]["code"],
        "unknown_message"
    );
    disconnect(client).await;
}

/// Roadmap E12: a sender asks who received a message. Delivery through
/// `langspan_brief` is recorded `via: "mcp_brief"`.
#[tokio::test]
async fn stdio_message_status_reports_pending_then_delivered_through_the_brief_tool() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = directory.workspace();
    let _daemon = Daemon::start(&state_dir).await;
    let registered = call_ipc(
        &state_dir,
        "workspace",
        Operation::WorkspaceRegister {
            workspace_id: "workspace".into(),
            root: workspace.to_string_lossy().into_owned(),
        },
    )
    .await;
    assert!(matches!(registered.outcome, Outcome::Workspace { .. }));
    for session in ["agent-a", "agent-b"] {
        let registered = call_ipc(
            &state_dir,
            session,
            Operation::SessionRegister {
                session_id: session.into(),
                workspace_id: "workspace".into(),
                parent: None,
                headless: false,
            },
        )
        .await;
        assert!(matches!(registered.outcome, Outcome::Session { .. }));
    }
    let client = connect(&state_dir).await;
    let sent = response(
        call_tool(
            &client,
            "langspan_send_message",
            json!({"request_id": "m-1", "workspace_id": "workspace", "session_id": "agent-a",
                   "to": "agent-b", "text": "stop editing src/db.rs", "paths": ["src/db.rs"]}),
        )
        .await,
    );
    assert!(
        matches!(sent.outcome, Outcome::MessageSent { .. }),
        "{sent:?}"
    );
    let ask = || {
        call_tool(
            &client,
            "langspan_message_status",
            json!({"workspace_id": "workspace", "message_id": "last", "session_id": "agent-a"}),
        )
    };
    let pending = ask().await;
    let text = format!("{:?}", pending.content);
    assert!(text.contains("not that it acted on it"), "{text}");
    let Outcome::MessageStatus { status } = response(pending).outcome else {
        panic!("not a status");
    };
    assert_eq!(status.message_id, "m-1");
    assert_eq!(status.state, langspan_core::MessageState::Pending);
    assert_eq!(status.recipients.len(), 1);
    assert_eq!(status.recipients[0].session_id, "agent-b");
    assert_eq!(
        status.recipients[0].status,
        langspan_core::RecipientState::Pending
    );

    let briefed = response(
        call_tool(
            &client,
            "langspan_brief",
            json!({"request_id": "b-1", "session_id": "agent-b", "max_peers": 4}),
        )
        .await,
    );
    assert!(matches!(briefed.outcome, Outcome::Briefing { .. }));
    let Outcome::MessageStatus { status } = response(ask().await).outcome else {
        panic!("not a status");
    };
    assert_eq!(status.state, langspan_core::MessageState::Done);
    let recipient = &status.recipients[0];
    assert_eq!(
        (recipient.status, recipient.via),
        (
            langspan_core::RecipientState::Delivered,
            Some(langspan_core::Delivery::McpBrief)
        )
    );
    assert!(recipient.age_ms.is_some());

    // Only the sender's own newest message is `last`.
    let none = call_tool(
        &client,
        "langspan_message_status",
        json!({"workspace_id": "workspace", "message_id": "last", "session_id": "agent-b"}),
    )
    .await;
    assert_eq!(none.is_error, Some(true));
    disconnect(client).await;
}

/// `langspan_impact` with `depth` through a real MCP client, daemon,
/// indexer and Git repository (roadmap D2).
#[tokio::test]
async fn stdio_impact_with_depth_returns_levels_by_name() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = directory.workspace();
    for (path, text) in [
        ("src/a.rs", "pub fn alpha() {}\n"),
        ("src/b.rs", "pub fn beta() { alpha(); }\n"),
        ("src/c.rs", "fn main() { beta(); }\n"),
    ] {
        fs::write(workspace.join(path), text).expect("write source");
    }
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["add", "."],
        &["commit", "--quiet", "-m", "init"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    }
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"workspace-register", "workspace_id":"workspace", "root":workspace}),
        )
        .await,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let outcome = loop {
        let answer = call_tool(
            &client,
            "langspan_impact",
            json!({"workspace_id":"workspace", "path":"src/a.rs", "limit":10, "depth":2}),
        )
        .await;
        let outcome = answer.structured_content.unwrap_or_default()["outcome"].clone();
        // The test workspace holds a source file of its own besides these.
        let coverage = &outcome["coverage"];
        if coverage["scanned"] == true
            && coverage["indexable_files"].as_u64() >= Some(3)
            && coverage["current_files"] == coverage["indexable_files"]
        {
            break outcome;
        }
        assert!(Instant::now() < deadline, "never indexed: {outcome:#}");
        sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        outcome["levels"],
        json!([
            {"level": 1, "truncated": false,
             "files": [{"path": "src/b.rs", "via": "alpha", "from": "src/a.rs"}]},
            {"level": 2, "truncated": false,
             "files": [{"path": "src/c.rs", "via": "beta", "from": "src/b.rs"}]},
        ]),
        "{outcome:#}"
    );
    assert_eq!(outcome["resolution"], "name");

    // Without `depth`, the answer earlier builds give: no levels.
    let plain = response(
        call_tool(
            &client,
            "langspan_impact",
            json!({"workspace_id":"workspace", "path":"src/a.rs", "limit":10}),
        )
        .await,
    );
    let Outcome::Graph { levels, .. } = plain.outcome else {
        panic!("{:?}", plain.outcome);
    };
    assert_eq!(levels, None);

    let refused = call_tool(
        &client,
        "langspan_impact",
        json!({"workspace_id":"workspace", "path":"src/a.rs", "limit":10, "depth":4}),
    )
    .await;
    let text = serde_json::to_string(&refused.structured_content).unwrap();
    assert!(text.contains("depth must be between 1 and 3"), "{text}");
    disconnect(client).await;
}

/// `langspan_path` and `langspan_map` through a real MCP client, daemon,
/// indexer and Git repository (roadmap D3): the chain names each step, the
/// import of `./store` is resolved by path, and the map counts the edge.
#[tokio::test]
async fn stdio_path_and_map_follow_calls_and_imports() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = directory.workspace();
    fs::create_dir_all(workspace.join("src/api")).expect("create directory");
    for (path, text) in [
        (
            "src/api/handler.ts",
            "import { service } from \"../service\";\nexport function handler() {\n  return service();\n}\n",
        ),
        (
            "src/service.ts",
            "import { save } from \"./store\";\nexport function service() {\n  return save();\n}\n",
        ),
        ("src/store.ts", "export function save() {\n  return 1;\n}\n"),
    ] {
        fs::write(workspace.join(path), text).expect("write source");
    }
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["add", "."],
        &["commit", "--quiet", "-m", "init"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    }
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"workspace-register", "workspace_id":"workspace", "root":workspace}),
        )
        .await,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let outcome = loop {
        let answer = call_tool(
            &client,
            "langspan_path",
            json!({"workspace_id":"workspace", "from":"handler", "to":"save"}),
        )
        .await;
        let outcome = answer.structured_content.unwrap_or_default()["outcome"].clone();
        let coverage = &outcome["coverage"];
        if coverage["scanned"] == true
            && coverage["current_files"] == coverage["indexable_files"]
            && outcome["paths"]
                .as_array()
                .is_some_and(|paths| !paths.is_empty())
        {
            break outcome;
        }
        assert!(Instant::now() < deadline, "never found: {outcome:#}");
        sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        outcome["paths"],
        json!([{"ambiguous": false, "steps": [
            {"path": "src/api/handler.ts", "line": 3, "name": "handler", "edge": "calls",
             "callee": "service", "resolution": "name", "ambiguous": false},
            {"path": "src/service.ts", "line": 3, "name": "service", "edge": "calls",
             "callee": "save", "resolution": "path", "ambiguous": false},
            {"path": "src/store.ts", "line": 1, "name": "save", "edge": "defines",
             "resolution": "path", "ambiguous": false},
        ]}]),
        "{outcome:#}"
    );
    assert_eq!(outcome["max_hops"], 4, "the default");
    assert_eq!(outcome["truncated"], false);

    let map = response(
        call_tool(
            &client,
            "langspan_map",
            json!({"workspace_id":"workspace", "depth":1}),
        )
        .await,
    );
    let Outcome::GraphMap { areas, edges, .. } = map.outcome else {
        panic!("{:?}", map.outcome);
    };
    assert!(
        areas
            .iter()
            .any(|area| area.path == "src/api" && area.files == 1),
        "{areas:?}"
    );
    let imports: Vec<(&str, &str, u32)> = edges
        .iter()
        .filter(|edge| edge.kind == "imports")
        .map(|edge| (edge.from.as_str(), edge.to.as_str(), edge.count))
        .collect();
    assert_eq!(
        imports,
        [
            ("src/api", "src/service", 1),
            ("src/service", "src/store", 1)
        ],
        "{edges:?}"
    );

    let refused = call_tool(
        &client,
        "langspan_path",
        json!({"workspace_id":"workspace", "from":"handler", "to":"save", "max_hops":7}),
    )
    .await;
    let text = serde_json::to_string(&refused.structured_content).unwrap();
    assert!(text.contains("max_hops must be between 1 and 6"), "{text}");
    disconnect(client).await;
}

#[tokio::test]
async fn stdio_retract_message_withdraws_the_callers_newest_message() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let workspace = directory.workspace();
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"w", "workspace_id":"workspace", "root":workspace}),
        )
        .await,
    );
    for session in ["agent-a", "agent-b"] {
        response(
            call_tool(
                &client,
                "langspan_session_register",
                json!({"request_id":format!("r-{session}"), "session_id":session,
                       "workspace_id":"workspace"}),
            )
            .await,
        );
    }
    for id in ["m-1", "m-2"] {
        response(
            call_tool(
                &client,
                "langspan_send_message",
                json!({"request_id":id, "workspace_id":"workspace", "session_id":"agent-a",
                       "to":"agent-b", "text":id, "paths":[]}),
            )
            .await,
        );
    }
    let refused = call_tool(
        &client,
        "langspan_retract_message",
        json!({"request_id":"x-1", "workspace_id":"workspace", "session_id":"agent-b",
               "message_id":"m-1"}),
    )
    .await;
    assert_eq!(refused.is_error, Some(true));
    let refused: Response =
        serde_json::from_value(refused.structured_content.expect("domain error envelope"))
            .expect("shared domain response");
    assert!(
        matches!(&refused.outcome, Outcome::Error { code, .. } if code == "not_sender"),
        "{refused:?}"
    );
    let withdrawn = response(
        call_tool(
            &client,
            "langspan_retract_message",
            json!({"request_id":"x-2", "workspace_id":"workspace", "session_id":"agent-a",
                   "message_id":"last"}),
        )
        .await,
    );
    let Outcome::MessageRetracted {
        message_id,
        withdrawn_unread,
        told_to_disregard,
    } = &withdrawn.outcome
    else {
        panic!("{withdrawn:?}");
    };
    assert_eq!(message_id, "m-2");
    assert_eq!(withdrawn_unread, &["agent-b"]);
    assert!(told_to_disregard.is_empty());
    disconnect(client).await;
}

/// Roadmap C9: the recap tool answers with the trail an ended session left,
/// read-only, and asking for it neither delivers nor takes it.
#[tokio::test]
async fn stdio_recap_lists_an_ended_sessions_trail_without_taking_it() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let root = directory.workspace();
    let _daemon = Daemon::start(&state_dir).await;
    for phase in ["PreToolUse", "PostToolUse"] {
        claude_write_hook(&state_dir, &root, "alice", "src/shared.rs", "x\n", phase);
    }
    call_ipc(
        &state_dir,
        "end-alice",
        Operation::SessionEnd {
            session_id: "claude-code.alice".into(),
            trail: None,
        },
    )
    .await;
    let client = connect(&state_dir).await;
    let recap =
        response(call_tool(&client, "langspan_recap", json!({"workspace_id": "app"})).await);
    let Outcome::Recap { recap } = &recap.outcome else {
        panic!("{recap:?}");
    };
    let trail = &recap.trails[0].trail;
    assert_eq!(trail.session_id, "claude-code.alice");
    assert_eq!(trail.edited[0].path, "src/shared.rs");
    // Without the end hook's journal facts, the trail says so.
    assert!(trail.partial);
    assert_eq!(trail.taken_by, None);
    disconnect(client).await;
}

/// One Claude Code `Write` hook event, through the real hook executable,
/// for a session in workspace `app` at `root`.
fn claude_write_hook(
    state_dir: &Path,
    root: &Path,
    session: &str,
    file: &str,
    content: &str,
    phase: &str,
) {
    use std::io::Write as _;
    let event = json!({
        "session_id": session, "cwd": root, "hook_event_name": phase,
        "tool_name": "Write", "tool_use_id": format!("call-{session}-{}", file.replace(['/', '.'], "-")),
        "tool_input": {"file_path": root.join(file), "content": content},
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .args(["hook", "claude-code", "--state-dir"])
        .arg(state_dir)
        .args(["--workspace", "app", "--root"])
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the real hook");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(event.to_string().as_bytes())
        .expect("write hook event");
    let output = child.wait_with_output().expect("hook output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let coverage = stderr
        .lines()
        .last()
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .map(|diagnostic| diagnostic["coverage"].clone())
        .unwrap_or_default();
    let expected = if phase == "PreToolUse" {
        "admitted"
    } else {
        "observed"
    };
    assert_eq!(coverage, expected, "{phase} {file}: {stderr}");
}

fn cli(state_dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_langspan"))
        .env("LANGSPAN_AUTOSTART", "off")
        .arg("--state-dir")
        .arg(state_dir)
        .args(args)
        .output()
        .expect("run the real CLI");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn git_repository(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let full = root.join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("create directory");
        fs::write(full, text).expect("write source");
    }
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["add", "."],
        &["commit", "--quiet", "-m", "init"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    }
}

const NET_BEFORE: &str = "pub fn upload(path: &str) -> bool {\n    path.is_empty()\n}\n\npub fn download(path: &str) -> bool {\n    path.starts_with(\"http\")\n}\n";
const NET_AFTER: &str = "pub fn upload(path: &str, timeout_ms: u64) -> bool {\n    path.is_empty() && timeout_ms > 0\n}\n\npub fn download(path: &str) -> bool {\n    path.starts_with(\"https://example.invalid\")\n}\n";
const MAIN_RS: &str = "mod held;\nmod net;\nfn main() {\n    net::upload(\"a\");\n    net::download(\"b\");\n    held::hold();\n}\n";

/// B26: a hookless MCP client asks, before it writes, what changed in the
/// files and symbols it is about to change. Real daemon, observer and
/// indexer, a real Git repository and real hooks for the peers.
#[tokio::test]
async fn stdio_check_write_reports_a_peers_signature_body_lines_and_hold_and_records_nothing() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let root = directory.0.join("app");
    git_repository(
        &root,
        &[
            ("src/net.rs", NET_BEFORE),
            ("src/main.rs", MAIN_RS),
            ("src/held.rs", "pub fn hold() {}\n"),
        ],
    );
    let root = fs::canonicalize(&root).expect("canonical root");
    let mut daemon = Some(Daemon::start(&state_dir).await);
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"ws", "workspace_id":"app", "root":root}),
        )
        .await,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let found = call_tool(
            &client,
            "langspan_symbol",
            json!({"workspace_id":"app", "name":"upload", "limit":10}),
        )
        .await
        .structured_content
        .unwrap_or_default();
        let coverage = &found["outcome"]["coverage"];
        if coverage["scanned"] == true && coverage["current_files"] == 3 {
            break;
        }
        assert!(Instant::now() < deadline, "never indexed: {found:#}");
        sleep(Duration::from_millis(100)).await;
    }

    // Alice changes upload's signature and download's body; Carol starts a
    // write of src/held.rs that has not finished.
    claude_write_hook(
        &state_dir,
        &root,
        "alice",
        "src/net.rs",
        NET_AFTER,
        "PreToolUse",
    );
    fs::write(root.join("src/net.rs"), NET_AFTER).expect("alice writes");
    claude_write_hook(
        &state_dir,
        &root,
        "alice",
        "src/net.rs",
        NET_AFTER,
        "PostToolUse",
    );
    claude_write_hook(
        &state_dir,
        &root,
        "carol",
        "src/held.rs",
        "pub fn hold() { }\n",
        "PreToolUse",
    );
    response(
        call_tool(
            &client,
            "langspan_intent_set",
            json!({"request_id":"intent-alice", "session_id":"claude-code.alice",
                   "paths":["src/net.rs"], "summary":"make upload take a timeout"}),
        )
        .await,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let history = call_tool(
            &client,
            "langspan_history",
            json!({"workspace_id":"app", "name":"upload"}),
        )
        .await
        .structured_content
        .unwrap_or_default();
        if history["outcome"]["history"]["entries"][0]["after"]
            .as_str()
            .is_some_and(|after| after.contains("timeout_ms"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "no notice: {history:#}");
        sleep(Duration::from_millis(100)).await;
    }
    // Bob has no hooks: he registers and asks before he writes.
    response(
        call_tool(
            &client,
            "langspan_session_register",
            json!({"request_id":"bob", "session_id":"mcp-bob", "workspace_id":"app"}),
        )
        .await,
    );
    let (_, before) = cli(&state_dir, &["events", "--limit", "1000", "--json"]);
    let last_seq =
        serde_json::from_str::<Value>(&before).expect("events json")["outcome"]["events"]
            .as_array()
            .and_then(|events| events.last())
            .and_then(|event| event["seq"].as_u64())
            .unwrap_or(0);

    let answer = call_tool(
        &client,
        "langspan_check_write",
        json!({"session_id":"mcp-bob", "paths":["src/net.rs", "src/held.rs"],
               "symbols":["upload", "download", "hold"]}),
    )
    .await;
    assert_eq!(answer.is_error, Some(false), "{answer:?}");
    let text = format!("{:?}", answer.content);
    assert!(
        text.contains("not instructions"),
        "labelled as data: {text}"
    );
    let check = answer.structured_content.expect("structured")["outcome"]["check"].clone();
    assert_eq!(check["index_ready"], true, "{check:#}");
    let net = &check["paths"][0];
    assert_eq!(net["path"], "src/net.rs", "{check:#}");
    assert_eq!(net["held_by"], Value::Null, "{check:#}");
    assert_eq!(net["stale"], Value::Null, "Bob never read it: {check:#}");
    assert_eq!(
        net["unread_lines"][0]["session_id"], "claude-code.alice",
        "{check:#}"
    );
    assert_eq!(
        net["unread_lines"][0]["lines"],
        json!([{"start_line":1,"end_line":2},{"start_line":6,"end_line":6}]),
        "{check:#}"
    );
    assert!(
        net["unread_lines"][0]["age_ms"].as_u64().is_some(),
        "{check:#}"
    );
    let held = &check["paths"][1];
    assert_eq!(held["path"], "src/held.rs", "{check:#}");
    assert_eq!(held["held_by"], "claude-code.carol", "{check:#}");

    let symbols = check["symbols"].as_array().expect("symbols");
    let upload = symbols
        .iter()
        .find(|symbol| symbol["name"] == "upload")
        .unwrap_or_else(|| panic!("upload: {check:#}"));
    assert_eq!(upload["change"], "signature", "{check:#}");
    assert_eq!(upload["path"], "src/net.rs", "{check:#}");
    assert_eq!(upload["by_session"], "claude-code.alice", "{check:#}");
    assert!(
        upload["age_ms"].as_u64().is_some_and(|age| age < 120_000),
        "{check:#}"
    );
    assert_eq!(upload["confidence"], "name_match", "{check:#}");
    assert_eq!(upload["intents"], json!(["claude-code.alice"]), "{check:#}");
    assert!(
        upload["after"]
            .as_str()
            .unwrap_or("")
            .contains("timeout_ms"),
        "{check:#}"
    );
    let download = symbols
        .iter()
        .find(|symbol| symbol["name"] == "download")
        .unwrap_or_else(|| panic!("download: {check:#}"));
    assert_eq!(download["change"], "body", "{check:#}");
    assert_eq!(download["by_session"], "claude-code.alice", "{check:#}");
    assert_eq!(
        download["lines"],
        json!([{"start_line":6,"end_line":6}]),
        "{check:#}"
    );
    assert_eq!(download["intents"], json!([]), "{check:#}");
    assert!(
        !symbols.iter().any(|symbol| symbol["name"] == "hold"),
        "nobody changed hold: {check:#}"
    );

    // Without names, the definitions in the paths asked about are checked.
    let by_path = call_tool(
        &client,
        "langspan_check_write",
        json!({"session_id":"mcp-bob", "paths":["src/net.rs"]}),
    )
    .await
    .structured_content
    .expect("structured")["outcome"]["check"]
        .clone();
    let names: BTreeSet<&str> = by_path["symbols"]
        .as_array()
        .expect("symbols")
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert_eq!(names, BTreeSet::from(["download", "upload"]), "{by_path:#}");

    // Alice wrote the current version herself: nothing is unread or stale
    // for her, and her own change is not news to her.
    let own = call_tool(
        &client,
        "langspan_check_write",
        json!({"session_id":"claude-code.alice", "paths":["src/net.rs"], "symbols":["upload"]}),
    )
    .await
    .structured_content
    .expect("structured")["outcome"]["check"]
        .clone();
    assert_eq!(own["paths"][0]["stale"], false, "{own:#}");
    assert_eq!(own["paths"][0]["unread_lines"], json!([]), "{own:#}");
    assert_eq!(own["symbols"], json!([]), "{own:#}");

    // The same answer through the CLI.
    let (code, cli_json) = cli(
        &state_dir,
        &[
            "check-write",
            "mcp-bob",
            "src/net.rs",
            "--symbol",
            "upload",
            "--json",
        ],
    );
    assert_eq!(code, Some(0), "{cli_json}");
    let cli_check =
        serde_json::from_str::<Value>(&cli_json).expect("cli json")["outcome"]["check"].clone();
    assert_eq!(cli_check["symbols"][0]["name"], "upload", "{cli_check:#}");
    assert_eq!(cli_check["symbols"][0]["by_session"], "claude-code.alice");
    let (code, human) = cli(
        &state_dir,
        &[
            "check-write",
            "mcp-bob",
            "src/held.rs",
            "src/net.rs",
            "--symbol",
            "upload",
        ],
    );
    assert_eq!(code, Some(0), "{human}");
    assert!(human.contains("not instructions"), "{human}");
    assert!(human.contains("held by claude-code.carol"), "{human}");
    assert!(human.contains("\"upload\""), "{human}");
    assert!(human.contains("signature"), "{human}");

    // The checks recorded nothing: no event names Bob after he registered,
    // and no receipt holds a check.
    let after_seq = last_seq.to_string();
    let (_, after) = cli(
        &state_dir,
        &["events", "--after", &after_seq, "--limit", "1000", "--json"],
    );
    assert!(!after.contains("mcp-bob"), "{after}");
    let receipts: String = rusqlite::Connection::open(state_dir.join("state.sqlite3"))
        .expect("open the state database")
        .query_row(
            "SELECT coalesce(group_concat(command_json), '') FROM receipts",
            [],
            |row| row.get(0),
        )
        .expect("read receipts");
    assert!(!receipts.contains("check_write"), "{receipts}");

    // Without a daemon the check is a tool error, not an empty answer.
    drop(daemon.take());
    let down = call_tool(
        &client,
        "langspan_check_write",
        json!({"session_id":"mcp-bob", "paths":["src/net.rs"]}),
    )
    .await;
    assert_eq!(down.is_error, Some(true), "{down:?}");
    disconnect(client).await;
}

const NET_CS_BEFORE: &str = "namespace App;\n\npublic class Net\n{\n    public bool Upload(string path)\n    {\n        return path.Length == 0;\n    }\n\n    public bool Download(string path)\n    {\n        return path.StartsWith(\"http\");\n    }\n}\n";
const NET_CS_AFTER: &str = "namespace App;\n\npublic class Net\n{\n    public bool Upload(string path, int timeoutMs)\n    {\n        return path.Length == 0;\n    }\n\n    public bool Download(string path)\n    {\n        return path.StartsWith(\"https://example.invalid\");\n    }\n}\n";

/// B26 with roadmap D6, in C#: a peer's signature change and body change
/// to C# methods are what a hookless client is told before it writes.
#[tokio::test]
async fn stdio_check_write_reports_a_peers_csharp_signature_and_body_changes() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let root = directory.0.join("app");
    git_repository(
        &root,
        &[
            ("src/Net.cs", NET_CS_BEFORE),
            (
                "src/Program.cs",
                "namespace App;\n\nstatic class Program\n{\n    static void Main() => new Net().Upload(\"a\");\n}\n",
            ),
        ],
    );
    let root = fs::canonicalize(&root).expect("canonical root");
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"ws", "workspace_id":"app", "root":root}),
        )
        .await,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let found = call_tool(
            &client,
            "langspan_symbol",
            json!({"workspace_id":"app", "name":"Upload", "limit":10}),
        )
        .await
        .structured_content
        .unwrap_or_default();
        let coverage = &found["outcome"]["coverage"];
        if coverage["scanned"] == true && coverage["current_files"] == 2 {
            break;
        }
        assert!(Instant::now() < deadline, "never indexed: {found:#}");
        sleep(Duration::from_millis(100)).await;
    }
    for phase in ["PreToolUse", "PostToolUse"] {
        if phase == "PostToolUse" {
            fs::write(root.join("src/Net.cs"), NET_CS_AFTER).expect("alice writes");
        }
        claude_write_hook(
            &state_dir,
            &root,
            "alice",
            "src/Net.cs",
            NET_CS_AFTER,
            phase,
        );
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let history = call_tool(
            &client,
            "langspan_history",
            json!({"workspace_id":"app", "name":"Upload"}),
        )
        .await
        .structured_content
        .unwrap_or_default();
        if history["outcome"]["history"]["entries"][0]["after"]
            .as_str()
            .is_some_and(|after| after.contains("timeoutMs"))
        {
            break;
        }
        assert!(Instant::now() < deadline, "no notice: {history:#}");
        sleep(Duration::from_millis(100)).await;
    }
    response(
        call_tool(
            &client,
            "langspan_session_register",
            json!({"request_id":"bob", "session_id":"mcp-bob", "workspace_id":"app"}),
        )
        .await,
    );
    let check = call_tool(
        &client,
        "langspan_check_write",
        json!({"session_id":"mcp-bob", "paths":["src/Net.cs"]}),
    )
    .await
    .structured_content
    .expect("structured")["outcome"]["check"]
        .clone();
    assert_eq!(check["index_ready"], true, "{check:#}");
    let symbol = |name: &str| {
        check["symbols"]
            .as_array()
            .and_then(|symbols| symbols.iter().find(|symbol| symbol["name"] == name))
            .unwrap_or_else(|| panic!("{name}: {check:#}"))
            .clone()
    };
    let upload = symbol("Upload");
    assert_eq!(
        (&upload["change"], &upload["by_session"], &upload["after"]),
        (
            &json!("signature"),
            &json!("claude-code.alice"),
            &json!("public bool Upload(string path, int timeoutMs)")
        ),
        "{check:#}"
    );
    let download = symbol("Download");
    assert_eq!(
        (&download["change"], &download["lines"]),
        (&json!("body"), &json!([{"start_line":12,"end_line":12}])),
        "{check:#}"
    );
    disconnect(client).await;
}

/// B26 with more definitions in a file than one check reads: a removal a
/// peer has since undone is not reported for a definition past the bound,
/// and the answer says that definitions were left out.
#[tokio::test]
async fn stdio_check_write_past_the_definition_bound_checks_removals_against_the_index() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let root = directory.0.join("app");
    let many = |without: Option<usize>| -> String {
        (0..70)
            .filter(|i| Some(*i) != without)
            .map(|i| format!("pub fn f{i:02}() {{}}\n"))
            .collect()
    };
    git_repository(&root, &[("src/many.rs", &many(None))]);
    let root = fs::canonicalize(&root).expect("canonical root");
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"ws", "workspace_id":"app", "root":root}),
        )
        .await,
    );
    let defined = |found: &Value| found["outcome"]["symbols"].as_array().map(Vec::len);
    let wait_for = async |want: Option<usize>| {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let found = call_tool(
                &client,
                "langspan_symbol",
                json!({"workspace_id":"app", "name":"f69", "limit":10}),
            )
            .await
            .structured_content
            .unwrap_or_default();
            if found["outcome"]["coverage"]["scanned"] == true && defined(&found) == want {
                return;
            }
            assert!(Instant::now() < deadline, "never indexed: {found:#}");
            sleep(Duration::from_millis(100)).await;
        }
    };
    wait_for(Some(1)).await;

    // Alice removes f69, the 70th definition; Dave puts it back.
    for (session, content) in [("alice", many(Some(69))), ("dave", many(None))] {
        claude_write_hook(
            &state_dir,
            &root,
            session,
            "src/many.rs",
            &content,
            "PreToolUse",
        );
        fs::write(root.join("src/many.rs"), &content).expect("write");
        claude_write_hook(
            &state_dir,
            &root,
            session,
            "src/many.rs",
            &content,
            "PostToolUse",
        );
        if session == "alice" {
            wait_for(Some(0)).await;
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let history = call_tool(
                    &client,
                    "langspan_history",
                    json!({"workspace_id":"app", "name":"f69"}),
                )
                .await
                .structured_content
                .unwrap_or_default();
                if history["outcome"]["history"]["entries"][0]["change"] == "removed" {
                    break;
                }
                assert!(Instant::now() < deadline, "no notice: {history:#}");
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
    wait_for(Some(1)).await;
    response(
        call_tool(
            &client,
            "langspan_session_register",
            json!({"request_id":"bob", "session_id":"mcp-bob", "workspace_id":"app"}),
        )
        .await,
    );
    let check = call_tool(
        &client,
        "langspan_check_write",
        json!({"session_id":"mcp-bob", "paths":["src/many.rs"]}),
    )
    .await
    .structured_content
    .expect("structured")["outcome"]["check"]
        .clone();
    assert_eq!(check["index_ready"], true, "{check:#}");
    assert!(
        !check["symbols"]
            .as_array()
            .expect("symbols")
            .iter()
            .any(|symbol| symbol["name"] == "f69" && symbol["change"] == "removed"),
        "f69 is defined again: {check:#}"
    );
    assert_eq!(check["definitions_truncated"], true, "{check:#}");
    disconnect(client).await;
}

/// B26 without a complete scan: a plain directory is never indexed, so the
/// check answers for its paths only and says the index is not ready.
#[tokio::test]
async fn stdio_check_write_without_an_index_gives_path_results_only() {
    let directory = TestDirectory::new();
    let state_dir = directory.state_dir();
    let root = directory.0.join("app");
    fs::create_dir_all(root.join("src")).expect("create workspace");
    fs::write(root.join("src/net.rs"), NET_BEFORE).expect("write source");
    let root = fs::canonicalize(&root).expect("canonical root");
    let _daemon = Daemon::start(&state_dir).await;
    let client = connect(&state_dir).await;
    response(
        call_tool(
            &client,
            "langspan_workspace_register",
            json!({"request_id":"ws", "workspace_id":"app", "root":root}),
        )
        .await,
    );
    claude_write_hook(
        &state_dir,
        &root,
        "carol",
        "src/net.rs",
        NET_AFTER,
        "PreToolUse",
    );
    response(
        call_tool(
            &client,
            "langspan_session_register",
            json!({"request_id":"bob", "session_id":"mcp-bob", "workspace_id":"app"}),
        )
        .await,
    );
    let check = call_tool(
        &client,
        "langspan_check_write",
        json!({"session_id":"mcp-bob", "paths":["src/net.rs"], "symbols":["upload"]}),
    )
    .await
    .structured_content
    .expect("structured")["outcome"]["check"]
        .clone();
    assert_eq!(check["index_ready"], false, "{check:#}");
    assert_eq!(check["symbols"], json!([]), "{check:#}");
    assert_eq!(
        check["paths"][0]["held_by"], "claude-code.carol",
        "{check:#}"
    );
    disconnect(client).await;
}
