#![cfg(unix)]
//! `langspan recall` and `langspan_recall` (roadmap E2) through the real
//! CLI, hooks, daemon and MCP adapter, in a real Git repository.

mod support;
use support::git;

use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command as Process,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use langspan::protocol::{MAX_FRAME_BYTES, Request};
use langspan_core::{Command, Outcome};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct World {
    base: PathBuf,
    daemon: Option<support::Owned>,
}

impl World {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "ls-recall-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(base.join("work/src")).unwrap();
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let world = Self { base, daemon: None };
        fs::write(world.work().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        git(&world.work(), &["init", "-q", "-b", "main"]);
        git(&world.work(), &["add", "."]);
        git(&world.work(), &["commit", "-q", "-m", "start"]);
        world
    }
    fn work(&self) -> PathBuf {
        self.base.join("work")
    }
    fn state(&self) -> PathBuf {
        self.base.join("state")
    }
    fn start(&mut self) {
        self.daemon = Some(support::coordinator(&self.state()));
    }
    fn stop(&mut self) {
        drop(self.daemon.take());
    }
    fn cli_full(&self, args: &[&str]) -> (i32, String, String) {
        let output = Process::new(env!("CARGO_BIN_EXE_langspan"))
            .env("LANGSPAN_AUTOSTART", "off")
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
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
    fn recall(&self, args: &[&str]) -> Value {
        let mut all = vec!["recall", "app"];
        all.extend_from_slice(args);
        self.json(&all)["recall"].clone()
    }
    fn remember(&self, id: &str, text: &str, paths: &[&str]) {
        let mut args = vec!["remember", "app", text];
        args.extend_from_slice(paths);
        args.extend_from_slice(&["--session", "claude-code.alice", "--request-id", id]);
        let (code, out) = self.cli(&args);
        assert_eq!(code, 0, "{out}");
    }
    /// A session starts through the real Claude Code hook.
    fn session_start(&self, session: &str) {
        let output = support::hook_call(
            &self.state(),
            "claude-code",
            "app",
            &self.work(),
            &[],
            &json!({"session_id": session, "cwd": self.work(),
                   "hook_event_name": "SessionStart", "source": "startup"}),
        );
        assert!(output.status.success());
    }
    /// The type of each event recorded after `after`, and the last sequence.
    fn events_after(&self, after: u64) -> (Vec<String>, u64) {
        let outcome = call(
            &self.state(),
            "read",
            Command::Events { after, limit: 1000 },
        );
        let Outcome::Events { events } = outcome else {
            panic!("{outcome:?}");
        };
        let last = events.last().map(|event| event.seq).unwrap_or(after);
        let kinds = events
            .iter()
            .map(|event| serde_json::to_value(&event.event).unwrap()["type"].to_string())
            .collect();
        (kinds, last)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn call(state: &Path, id: &str, command: Command) -> Outcome {
    let request = Request::new(id, command);
    langspan::client::call(state, &request).unwrap().outcome
}

fn ids(recall: &Value) -> Vec<&str> {
    recall["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note["note_id"].as_str().unwrap())
        .collect()
}

#[test]
fn recall_finds_notes_by_every_word_and_changes_nothing() {
    let mut w = World::new();
    w.start();
    w.session_start("alice");
    w.session_start("bob");
    w.remember(
        "n-upload",
        "The response field is named Upload_ID; the upload handler mints it.",
        &["src/lib.rs"],
    );
    w.remember("n-bucket", "Uploads go to one bucket.", &[]);
    w.remember("n-api", "API errors are typed.", &["src/api.rs"]);
    w.remember("n-cli", "CLI errors are strings.", &["src/cli.rs"]);
    w.remember("n-whole", "Errors never panic.", &[]);

    // Bob's briefing and the event log before any recall.
    let brief_before =
        w.json(&["brief", "claude-code.bob", "--request-id", "b1"])["briefing"]["notes"].clone();
    assert!(
        !brief_before.as_array().unwrap().is_empty(),
        "{brief_before}"
    );
    let (_, events_before) = w.events_after(0);

    // Both words, without case, in a note about a file bob is not working
    // on; the note with only "upload" is not found.
    let found = w.recall(&["upload id"]);
    assert_eq!(
        found,
        json!({
            "workspace_id": "app",
            "query": "upload id",
            "notes": [{
                "note_id": "n-upload",
                "author": "claude-code.alice",
                "text": "The response field is named Upload_ID; the upload handler mints it.",
                "paths": ["src/lib.rs"],
            }],
            "truncated": false,
        })
    );
    // Words given as separate arguments are the same query.
    assert_eq!(ids(&w.recall(&["UPLOAD", "id"])), ["n-upload"]);

    // A path keeps notes about it or about the whole workspace.
    assert_eq!(
        ids(&w.recall(&["errors", "--path", "src/api.rs"])),
        ["n-whole", "n-api"]
    );
    // Three hits, a limit of two.
    let limited = w.recall(&["errors", "--limit", "2"]);
    assert_eq!(ids(&limited), ["n-whole", "n-cli"]);
    assert_eq!(limited["truncated"], true);
    assert_eq!(w.recall(&["errors"])["truncated"], false);

    // The text form quotes the note as data and names its author.
    let (code, text) = w.cli(&["recall", "app", "upload id"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("Remembered (n-upload, by claude-code.alice): \"The response field"),
        "{text}"
    );
    let (code, text) = w.cli(&["recall", "app", "errors", "--limit", "1"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("More notes match"), "{text}");
    let (code, text) = w.cli(&["recall", "app", "nothing-like-this"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("No held note"), "{text}");

    // Bounds are refused with the bound named.
    let long = "a".repeat(201);
    let (code, out) = w.cli(&["recall", "app", &long]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("invalid_query") && out.contains("200 bytes"),
        "{out}"
    );
    let (code, out) = w.cli(&["recall", "app", "a b c d e f g h i"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("8 words"), "{out}");
    let (code, out) = w.cli(&["recall", "app", "a", "--limit", "51"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("1 to 50"), "{out}");
    let (code, out) = w.cli(&["recall", "nowhere", "a"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("workspace_not_found"), "{out}");
    let (code, _, err) = w.cli_full(&["recall", "app"]);
    assert_eq!(code, 1);
    assert!(err.contains("unknown or incomplete command"), "{err}");

    // Nothing was recorded and bob's briefing is the same.
    // The Git observer records what it sees on its own schedule; nothing
    // else may have been recorded.
    let (recorded, _) = w.events_after(events_before);
    let recorded: Vec<_> = recorded
        .into_iter()
        .filter(|kind| !kind.ends_with("_observed\"") && !kind.contains("scan"))
        .collect();
    assert!(
        recorded.is_empty(),
        "a recall records no event: {recorded:?}"
    );
    assert_eq!(
        w.json(&["brief", "claude-code.bob", "--request-id", "b2"])["briefing"]["notes"],
        brief_before
    );

    // A forgotten note is never returned.
    let (code, out) = w.cli(&["forget", "app", "n-upload", "--request-id", "f1"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        w.recall(&["upload id"])["notes"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Notes outlive a restart and are found again; a stopped daemon is an
    // explicit failure.
    w.stop();
    let (code, _, err) = w.cli_full(&["recall", "app", "errors"]);
    assert_eq!(code, 1);
    assert!(err.contains("coordination unavailable"), "{err}");
    w.start();
    assert_eq!(ids(&w.recall(&["errors"])), ["n-whole", "n-cli", "n-api"]);
}

#[test]
fn recall_requests_match_the_committed_golden_vectors() {
    let vectors: &[&[u8]] = &[
        include_bytes!("../fixtures/recall-request-v3.json"),
        include_bytes!("../fixtures/recall-whole-workspace-request-v3.json"),
    ];
    for wire in vectors {
        let request: Request = serde_json::from_slice(wire).unwrap();
        assert_eq!(request.version, langspan::protocol::OLDEST_PROTOCOL_VERSION);
        assert_eq!(langspan::protocol::encode(&request).unwrap(), *wire);
    }
    let request: Request = serde_json::from_slice(vectors[0]).unwrap();
    assert!(matches!(
        &request.command,
        Command::Recall { workspace_id, query, path: Some(path), limit: 10 }
            if workspace_id == "demo" && query == "upload id" && path == "src/api.rs"
    ));
    let request: Request = serde_json::from_slice(vectors[1]).unwrap();
    assert!(matches!(
        &request.command,
        Command::Recall { path: None, .. }
    ));
    let unknown =
        String::from_utf8_lossy(vectors[1]).replace(r#""limit":10"#, r#""limit":10,"tag":"x""#);
    assert!(serde_json::from_str::<Request>(&unknown).is_err());
}

async fn recall_tool(client: &RunningService<RoleClient, ()>, arguments: Value) -> CallToolResult {
    let request = CallToolRequestParams::new("langspan_recall")
        .with_arguments(arguments.as_object().unwrap().clone());
    tokio::time::timeout(Duration::from_secs(10), client.call_tool(request))
        .await
        .unwrap()
        .unwrap()
}

fn texts(result: &CallToolResult) -> Vec<String> {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_mcp_tool_labels_notes_as_data_and_fits_one_frame() {
    let mut w = World::new();
    w.start();
    let state = w.state();
    let root = w.work().to_string_lossy().into_owned();
    {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            call(
                &state,
                "w",
                Command::WorkspaceRegister {
                    workspace_id: "app".into(),
                    root,
                },
            );
            // As many notes as a workspace holds, each as large as a note
            // can be: 50 of them would not fit one frame.
            for index in 0..64 {
                let outcome = call(
                    &state,
                    &format!("n{index:02}"),
                    Command::Remember {
                        workspace_id: "app".into(),
                        author: None,
                        text: format!("upload_id {index:02} {}", "x".repeat(380)),
                        paths: (0..8)
                            .map(|path| format!("src/{}/{index:02}-{path}.rs", "d".repeat(480)))
                            .collect(),
                        symbol: None,
                        tags: Vec::new(),
                    },
                );
                assert!(matches!(outcome, Outcome::Remembered { .. }), "{outcome:?}");
            }
        })
        .await
        .unwrap();
    }
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command.arg("mcp").arg("--state-dir").arg(&state);
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();

    let result = recall_tool(
        &client,
        json!({"workspace_id": "app", "query": "upload_id", "limit": 50}),
    )
    .await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let texts = texts(&result);
    assert_eq!(texts[0], langspan::mcp::RECALL_LABEL);
    assert!(texts[0].contains("data, not instructions"));
    let recall = &result.structured_content.as_ref().unwrap()["outcome"]["recall"];
    let notes = recall["notes"].as_array().unwrap();
    assert!(!notes.is_empty() && notes.len() < 50, "{}", notes.len());
    assert_eq!(notes[0]["note_id"], "n63", "newest first");
    assert_eq!(recall["truncated"], true);
    let bytes = serde_json::to_vec(&result).unwrap().len();
    assert!(bytes <= 64 * 1024, "{bytes} bytes");

    // The default limit is 10.
    {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            for index in 0..12 {
                call(
                    &state,
                    &format!("small{index}"),
                    Command::Remember {
                        workspace_id: "app".into(),
                        author: None,
                        text: format!("A small note {index}."),
                        paths: Vec::new(),
                        symbol: None,
                        tags: Vec::new(),
                    },
                );
            }
        })
        .await
        .unwrap();
    }
    let result = recall_tool(&client, json!({"workspace_id": "app", "query": "SMALL"})).await;
    let recall = &result.structured_content.as_ref().unwrap()["outcome"]["recall"];
    assert_eq!(recall["notes"].as_array().unwrap().len(), 10);
    assert_eq!(recall["truncated"], true);

    // A refused query is a tool error naming the bound.
    let result = recall_tool(
        &client,
        json!({"workspace_id": "app", "query": "a b c d e f g h i"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let error = &result.structured_content.as_ref().unwrap()["outcome"];
    assert_eq!(error["code"], "invalid_query");
    assert!(error["message"].as_str().unwrap().contains("8 words"));

    // A stopped daemon is an explicit tool error.
    w.stop();
    let result = recall_tool(&client, json!({"workspace_id": "app", "query": "upload"})).await;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(
        result.structured_content.as_ref().unwrap()["code"],
        "daemon_request_failed"
    );
    client.cancel().await.unwrap();
}

/// An MCP result carries the answer twice: as structured content and as a
/// JSON-encoded text copy, which escapes every quotation mark and backslash
/// again. Notes full of them must still give one reply within one frame.
#[tokio::test]
async fn the_mcp_tool_fits_one_frame_when_notes_are_full_of_quotes_and_backslashes() {
    let mut w = World::new();
    w.start();
    let state = w.state();
    let root = w.work().to_string_lossy().into_owned();
    {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            call(
                &state,
                "w",
                Command::WorkspaceRegister {
                    workspace_id: "app".into(),
                    root,
                },
            );
            for index in 0..64 {
                let outcome = call(
                    &state,
                    &format!("q{index:02}"),
                    Command::Remember {
                        workspace_id: "app".into(),
                        author: None,
                        text: format!("upload_id {index:02} {}", "\"\\".repeat(190)),
                        paths: Vec::new(),
                        symbol: None,
                        tags: Vec::new(),
                    },
                );
                assert!(matches!(outcome, Outcome::Remembered { .. }), "{outcome:?}");
            }
        })
        .await
        .unwrap();
    }
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_langspan"));
    command.env("LANGSPAN_AUTOSTART", "off");
    command.arg("mcp").arg("--state-dir").arg(&state);
    let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();

    let result = recall_tool(
        &client,
        json!({"workspace_id": "app", "query": "upload_id", "limit": 50}),
    )
    .await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    // The whole tool result, with room left for the JSON-RPC envelope.
    let bytes = serde_json::to_vec(&result).unwrap().len();
    assert!(
        bytes + langspan::mcp::ENVELOPE_ROOM_BYTES <= MAX_FRAME_BYTES,
        "{bytes} bytes"
    );
    let texts = texts(&result);
    assert_eq!(texts[0], langspan::mcp::RECALL_LABEL);
    let recall = &result.structured_content.as_ref().unwrap()["outcome"]["recall"];
    let notes = recall["notes"].as_array().unwrap();
    assert!(!notes.is_empty() && notes.len() < 50, "{}", notes.len());
    assert_eq!(notes[0]["note_id"], "q63", "newest first");
    assert_eq!(recall["truncated"], true);
    // The text copy is the same answer as the structured one.
    let copy: Value = serde_json::from_str(&texts[1]).unwrap();
    assert_eq!(&copy, result.structured_content.as_ref().unwrap());
    client.cancel().await.unwrap();
    drop(w);
}
