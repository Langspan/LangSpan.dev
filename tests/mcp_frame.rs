#![cfg(unix)]
//! Every MCP tool's reply fits one frame (issue #143). A tool result carries
//! its answer twice, as structured content and as JSON text that escapes
//! every quotation mark and backslash again, so a 48 KiB daemon page full of
//! them is far larger than a frame. These tests fill pages with such text
//! through the real MCP adapter over stdio, against a real daemon, and check
//! that each reply fits, says it was cut and leads to the rest.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use langspan::mcp::{CUT_NOTE, ENVELOPE_ROOM_BYTES};
use langspan::protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION, Request};
use langspan_core::{Command, FileKind, FileState, Outcome};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);
const DEADLINE: Duration = Duration::from_secs(20);

/// Text made only of the two characters JSON escapes, `bytes` long.
fn escaping(bytes: usize) -> String {
    "\"\\".repeat(bytes / 2)
}

struct World {
    base: PathBuf,
    daemon: Option<Child>,
}

impl World {
    fn start() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-frame-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("work")).unwrap();
        fs::create_dir(base.join("state")).unwrap();
        fs::set_permissions(base.join("state"), fs::Permissions::from_mode(0o700)).unwrap();
        let mut world = Self { base, daemon: None };
        world.daemon = Some(
            Process::new(env!("CARGO_BIN_EXE_langspan"))
                .env("LANGSPAN_AUTOSTART", "off")
                .args(["daemon", "--state-dir"])
                .arg(world.state())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + DEADLINE;
        // Ready means what a client checks: an owner-only socket that accepts.
        while !langspan::client::accepts(&world.state(), Instant::now() + Duration::from_secs(1)) {
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        let root = world.base.join("work").to_string_lossy().into_owned();
        world.call(Command::WorkspaceRegister {
            workspace_id: "app".into(),
            root,
        });
        world
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    fn call(&self, command: Command) -> Outcome {
        let request = Request {
            version: PROTOCOL_VERSION,
            request_id: format!("r{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            command,
        };
        let outcome = langspan::client::call(&self.state(), &request)
            .unwrap()
            .outcome;
        assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
        outcome
    }

    /// The CLI's `--json` answer.
    fn cli(&self, args: &[&str]) -> Value {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .arg("--json")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn remember(&self, count: usize) {
        for index in 0..count {
            self.call(Command::Remember {
                workspace_id: "app".into(),
                author: None,
                text: format!("n{index:03} {}", escaping(390)),
                paths: Vec::new(),
                symbol: None,
                tags: Vec::new(),
            });
        }
    }

    /// `count` sessions, each with an intent whose summary is 2,000 bytes
    /// of escaping text.
    fn intents(&self, count: usize) {
        for index in 0..count {
            let session_id = format!("claude-code.s{index:02}");
            self.call(Command::SessionRegister {
                session_id: session_id.clone(),
                workspace_id: "app".into(),
                parent: None,
                headless: false,
            });
            self.call(Command::IntentSet {
                session_id,
                paths: vec![format!("src/{index:02}.rs")],
                summary: escaping(2000),
            });
        }
    }

    async fn mcp(&self) -> RunningService<RoleClient, ()> {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
        command.env("LANGSPAN_AUTOSTART", "off");
        command.arg("mcp").arg("--state-dir").arg(self.state());
        tokio::time::timeout(DEADLINE, ().serve(TokioChildProcess::new(command).unwrap()))
            .await
            .unwrap()
            .unwrap()
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

async fn tool(client: &RunningService<RoleClient, ()>, name: &str, arguments: Value) -> Reply {
    let request = CallToolRequestParams::new(name.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone());
    let result = tokio::time::timeout(DEADLINE, client.call_tool(request))
        .await
        .unwrap()
        .unwrap();
    Reply::checked(result)
}

/// A tool result that fits one frame, with its parts.
struct Reply {
    response: Value,
    notes: Vec<String>,
}

impl Reply {
    /// Check the reply fits one frame with room for the JSON-RPC envelope,
    /// that its text copy is its structured content, and split it.
    fn checked(result: CallToolResult) -> Self {
        let bytes = serde_json::to_vec(&result).unwrap().len();
        assert!(
            bytes + ENVELOPE_ROOM_BYTES <= MAX_FRAME_BYTES,
            "a tool result of {bytes} bytes does not fit one frame"
        );
        assert_eq!(result.is_error, Some(false), "{result:?}");
        let texts: Vec<String> = result
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect();
        let response = result.structured_content.unwrap();
        let copy: Value = serde_json::from_str(&texts[0]).unwrap();
        assert_eq!(copy, response, "the text copy is the structured answer");
        Self {
            response,
            notes: texts[1..].to_vec(),
        }
    }

    fn outcome(&self) -> &Value {
        &self.response["outcome"]
    }

    /// The note saying the page was cut, if it was.
    fn cut(&self) -> Option<&str> {
        self.notes
            .iter()
            .find(|note| note.starts_with(CUT_NOTE))
            .map(String::as_str)
    }
}

fn seqs(events: &Value) -> Vec<u64> {
    events
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["seq"].as_u64().unwrap())
        .collect()
}

/// Read `tool` page by page from the start, continuing after the last
/// sequence number, and return every sequence number read and whether any
/// page was cut.
async fn every_seq(
    client: &RunningService<RoleClient, ()>,
    tool_name: &str,
    arguments: Value,
) -> (Vec<u64>, usize) {
    let mut after = 0;
    let mut read = Vec::new();
    let mut cut = 0;
    loop {
        let mut arguments = arguments.clone();
        arguments["after"] = json!(after);
        arguments["limit"] = json!(1000);
        let reply = tool(client, tool_name, arguments).await;
        let page = seqs(&reply.outcome()["events"]);
        if let Some(note) = reply.cut() {
            cut += 1;
            let last = *page.last().expect("a cut page holds an event");
            assert!(note.contains(&format!("after {last}")), "{note}");
        }
        let Some(&last) = page.last() else {
            return (read, cut);
        };
        read.extend(page);
        after = last;
    }
}

#[tokio::test]
async fn an_events_page_full_of_quotes_fits_one_reply_and_resumes() {
    let world = World::start();
    world.remember(140);
    let client = world.mcp().await;

    let first = tool(
        &client,
        "langspan_events",
        json!({"after": 0, "limit": 1000}),
    )
    .await;
    let shown = seqs(&first.outcome()["events"]);
    assert!(!shown.is_empty());
    let note = first.cut().expect("the first page was cut to fit");
    assert!(note.contains("langspan_events"), "{note}");
    // The CLI's page is unchanged: the daemon's full 48 KiB, longer than
    // the MCP page.
    let cli = world.cli(&["events", "--after", "0", "--limit", "1000"]);
    assert!(seqs(&cli["outcome"]["events"]).len() > shown.len());

    let (read, cut) = every_seq(&client, "langspan_events", json!({})).await;
    assert!(cut >= 2, "{cut} pages cut");
    let expected: Vec<u64> = (1..=*read.last().unwrap()).collect();
    assert_eq!(read, expected, "every event, once, in order");
    assert!(read.len() > 140);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_changes_page_full_of_quotes_fits_one_reply_and_resumes() {
    let world = World::start();
    // Paths cannot hold a backslash; quotation marks alone are escaped twice.
    let quotes = "\"".repeat(480);
    for index in 0..120 {
        world.call(Command::ObserveDisk {
            workspace_id: "app".into(),
            path: format!("src/{index:03}{quotes}"),
            before: None,
            after: FileState::Present {
                digest: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
                kind: FileKind::Regular,
            },
            related_operation_ids: Vec::new(),
        });
    }
    let client = world.mcp().await;
    let arguments = json!({"workspace_id": "app"});
    let first = tool(
        &client,
        "langspan_changes",
        json!({"workspace_id": "app", "after": 0, "limit": 1000}),
    )
    .await;
    assert!(first.cut().expect("cut").contains("langspan_changes"));
    let cli = world.cli(&["changes", "app", "--after", "0", "--limit", "1000"]);
    assert!(
        seqs(&cli["outcome"]["events"]).len() > seqs(&first.outcome()["events"]).len(),
        "the CLI's page is the daemon's whole page"
    );

    let (read, cut) = every_seq(&client, "langspan_changes", arguments).await;
    assert!(cut >= 2, "{cut} pages cut");
    assert_eq!(read.len(), 120, "every observation");
    assert_eq!(
        read.iter().collect::<BTreeSet<_>>().len(),
        120,
        "none read twice"
    );
    client.cancel().await.unwrap();
}

/// The status-records pages the MCP tool gives, from the start, and how
/// many were cut. Each page must fit; a cut one names its cursor.
async fn status_pages(client: &RunningService<RoleClient, ()>) -> (Vec<Value>, usize) {
    let mut after: Option<String> = None;
    let mut records = Vec::new();
    let mut cut = 0;
    for _ in 0..500 {
        let mut arguments = json!({"all": true});
        if let Some(cursor) = &after {
            arguments["after"] = json!(cursor);
        }
        let reply = tool(client, "langspan_status", arguments).await;
        let outcome = reply.outcome();
        assert_eq!(outcome["type"], "status_records", "{outcome}");
        records.extend(outcome["records"].as_array().unwrap().iter().cloned());
        let next = outcome["next"].as_str().map(str::to_owned);
        if let Some(note) = reply.cut() {
            cut += 1;
            let next = next.as_deref().expect("a cut page has a next page");
            assert!(note.contains(next), "{note}");
        }
        match next {
            Some(next) => after = Some(next),
            None => return (records, cut),
        }
    }
    panic!("status pages did not end");
}

/// The records of `kind`, whole or reassembled from parts.
fn of_kind(records: &[Value], kind: &str) -> Vec<Value> {
    let mut found = Vec::new();
    let mut part = String::new();
    for record in records {
        let record = if record["kind"] == "part" {
            part.push_str(record["text"].as_str().unwrap());
            if record["last"] != true {
                continue;
            }
            let whole: Value = serde_json::from_str(&part).unwrap();
            part.clear();
            whole
        } else {
            record.clone()
        };
        if record["kind"] == kind {
            found.push(record);
        }
    }
    found
}

/// The keys of the records of `kind`.
fn keys(records: &[Value], kind: &str) -> BTreeSet<String> {
    of_kind(records, kind)
        .iter()
        .map(|record| record["key"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn the_status_view_and_every_status_page_fit_one_reply() {
    let world = World::start();
    world.intents(40);
    let client = world.mcp().await;

    let view = tool(&client, "langspan_status", json!({})).await;
    let note = view.cut().expect("the bounded view was cut");
    assert!(note.contains("all"), "{note}");
    let mcp_left_out = view.outcome()["summary"]["omitted_sessions"]
        .as_u64()
        .unwrap();
    let cli = world.cli(&["status"]);
    let cli_left_out = cli["outcome"]["summary"]["omitted_sessions"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        mcp_left_out > cli_left_out,
        "MCP left out {mcp_left_out} sessions, the CLI {cli_left_out}"
    );

    let (records, cut) = status_pages(&client).await;
    assert!(cut >= 1, "{cut} pages cut");
    let intents = keys(&records, "intent");
    assert_eq!(intents.len(), 40, "every intent is read: {intents:?}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_recap_full_of_quotes_fits_one_reply_and_status_holds_the_rest() {
    let world = World::start();
    world.remember(16);
    world.intents(16);
    let client = world.mcp().await;

    let recap = tool(&client, "langspan_recap", json!({"workspace_id": "app"})).await;
    let note = recap.cut().expect("the recap was cut");
    assert!(note.contains("langspan_status"), "{note}");
    let recap = &recap.outcome()["recap"];
    let shown = recap["notes"].as_array().unwrap().len();
    assert!(shown < 16, "{shown} notes shown");
    let left_out = recap["omitted_items"].as_u64().unwrap();
    let cli = world.cli(&["recap", "app"]);
    let cli = &cli["outcome"]["recap"];
    assert!(
        left_out > cli["omitted_items"].as_u64().unwrap_or(0),
        "the MCP recap leaves out more than the CLI's"
    );
    assert!(cli["notes"].as_array().unwrap().len() >= shown);

    // What the recap left out are records `status` with `all` gives.
    let (records, _) = status_pages(&client).await;
    assert_eq!(of_kind(&records, "note").len(), 16);
    assert_eq!(keys(&records, "intent").len(), 16);
    client.cancel().await.unwrap();
}

#[test]
fn the_shared_bound_leaves_room_for_the_envelope() {
    // The envelope around a result: `{"jsonrpc":"2.0","id":…,"result":…}`.
    let envelope = json!({"jsonrpc": "2.0", "id": u64::MAX, "result": null});
    assert!(serde_json::to_vec(&envelope).unwrap().len() + 1 < ENVELOPE_ROOM_BYTES);
}
