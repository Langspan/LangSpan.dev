#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use langspan::protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION, Request, Response};
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
    let request = Request {
        version: PROTOCOL_VERSION,
        request_id: request_id.into(),
        command,
    };
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
    // Every tool the LangSpan skill names is one this server lists.
    let named = langspan::skill::tools();
    assert!(!named.is_empty());
    for tool in named {
        assert!(
            names.contains(tool),
            "the skill names {tool}, which is not listed"
        );
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
                | "langspan_blind_spots"
                | "langspan_recap"
                | "langspan_recall"
                | "langspan_history"
                | "langspan_today"
                | "langspan_land_queue"
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
            &Request {
                version: PROTOCOL_VERSION,
                request_id: "independent-status".into(),
                command: Operation::Status,
            },
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
