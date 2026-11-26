//! Cursor hook adapter: `preToolUse` admission, post-tool observation and
//! session lifecycle ([ADR 0032](../docs/adr/0032-cursor-adapter.md)).
//!
//! Built from Cursor's documentation (<https://cursor.com/docs/agent/hooks>,
//! read 24 November 2026) and the bundled JavaScript of the installed
//! `cursor-agent` 2026.11.22-e9042b2. It has **not** been verified against a
//! real Cursor run: the fixtures in `fixtures/cursor-constructed` are
//! constructed, not captured.
//!
//! Like the other adapters it never answers `allow` and never exits 2. It
//! answers `deny` only for a known conflict or a stale source, in Cursor's
//! `permission` shape. Every other outcome prints nothing: Cursor treats an
//! empty reply from a hook without `failClosed` as a failed hook and lets the
//! action proceed under its own permission flow.

use crate::hook::{self, Config, LifecycleKind, Operation, Phase, Reply};
use langspan_core::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io::Read, time::Instant};

pub const SOURCE: &str = langspan_core::CURSOR_HOST;
/// Tools whose calls are admitted before they run. `Write` creates or
/// replaces a file, `Delete` removes one, `Shell` runs a command.
pub const ADMITTED_TOOLS: [&str; 3] = ["Write", "Delete", "Shell"];
/// A completed `Read` is recorded as the version the session saw.
pub const READ_TOOL: &str = "Read";
pub const SHELL_TOOL: &str = "Shell";
/// The captured `preToolUse` event of a real Cursor `Write`, relative to the
/// repository root, that the write-time break check is enabled by
/// ([ADR 0029](../docs/adr/0029-break-alerts-at-write-time.md)). None yet:
/// the adapter is built from documentation, and a check whose alert might
/// never reach the model would still use up its notice. A test fails when a
/// captured `pre-write.json` appears and this does not name it.
pub const CAPTURED_WRITE: Option<&str> = None;
/// A completed `Grep` shows peers the path it searched (roadmap A13), read
/// from its `path` input; its pattern is never read. The input's shape is
/// taken from Cursor's documentation and not yet seen from a real Cursor.
pub const SEARCH_TOOL: &str = "Grep";

#[derive(Debug, Deserialize)]
struct HostEvent {
    hook_event_name: String,
    /// Stable across a conversation's turns. The CLI also sends a
    /// `session_id` equal to it.
    conversation_id: Option<String>,
    session_id: Option<String>,
    /// Changes with every user message: a prompt's request identity.
    generation_id: Option<String>,
    /// Set on a tool call made inside a subagent: the parent's call that
    /// started it (CLI source; not in the documentation).
    parent_tool_call_id: Option<String>,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    tool_input: Option<Value>,
    cwd: Option<String>,
    workspace_roots: Option<Vec<String>>,
    prompt: Option<String>,
    is_interrupt: Option<bool>,
}

impl HostEvent {
    /// The working directory: the event's own, else its first workspace
    /// root. User-level hooks run from `~/.cursor`, so the process's own
    /// directory says nothing about the project.
    fn directory(&self) -> Option<&str> {
        self.cwd
            .as_deref()
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| self.workspace_roots.as_ref()?.first().map(String::as_str))
            .filter(|dir| !dir.is_empty())
    }

    fn conversation(&self) -> Option<&str> {
        self.conversation_id
            .as_deref()
            .or(self.session_id.as_deref())
            .filter(|id| !id.is_empty())
    }
}

/// A host identity inside the hook ID grammar. Cursor does not document the
/// format of its IDs, so one outside the grammar is replaced by a digest
/// rather than dropped; distinct IDs keep distinct digests.
pub fn host_id(id: &str) -> Option<String> {
    if hook::valid_host_id(id) {
        return Some(id.to_owned());
    }
    if id.is_empty() || id.len() > 4096 {
        return None;
    }
    Some(format!(
        "h{}",
        &crate::sha256::hex(&crate::sha256::digest(id.as_bytes()))[..32]
    ))
}

pub fn run(input: impl Read, config: &Config) -> Reply {
    let Some(deadline) = Instant::now().checked_add(config.budget) else {
        return Reply::none("unknown", "invalid hook deadline");
    };
    run_until(input, config, deadline)
}

pub fn run_until(input: impl Read, config: &Config, deadline: Instant) -> Reply {
    let started = Instant::now();
    let mut bytes = Vec::new();
    if let Err(error) = input
        .take(hook::MAX_EVENT_BYTES + 1)
        .read_to_end(&mut bytes)
    {
        return Reply::none("unknown", format!("could not read Cursor event: {error}"));
    }
    if bytes.len() as u64 > hook::MAX_EVENT_BYTES {
        return Reply::none("unsupported", "Cursor event exceeds 8 MiB");
    }
    let event: HostEvent = match serde_json::from_slice(&bytes) {
        Ok(event) => event,
        Err(_) => return Reply::none("unknown", "malformed Cursor hook JSON"),
    };
    let reply = translate(dispatch(event, config, deadline));
    Reply {
        detail: format!("{} ({} ms)", reply.detail, started.elapsed().as_millis()),
        ..reply
    }
}

fn dispatch(event: HostEvent, config: &Config, deadline: Instant) -> Reply {
    let Some(conversation) = event.conversation().and_then(host_id) else {
        return Reply::none(
            "unsupported",
            "Cursor event lacks a usable conversation identity",
        );
    };
    let lifecycle = |kind: LifecycleKind| {
        hook::lifecycle_parsed(
            SOURCE,
            &conversation,
            kind,
            (&event.hook_event_name, event.generation_id.as_deref()),
            event.directory(),
            config,
            deadline,
        )
    };
    let phase = match event.hook_event_name.as_str() {
        "sessionStart" => return lifecycle(LifecycleKind::Start),
        "beforeSubmitPrompt" => {
            return lifecycle(LifecycleKind::Prompt {
                prompt: event.prompt.clone(),
            });
        }
        "sessionEnd" => return lifecycle(LifecycleKind::End),
        "preToolUse" => Phase::Pre,
        "postToolUse" => Phase::Post(ToolResult::Succeeded),
        "postToolUseFailure" => Phase::Post(if event.is_interrupt == Some(true) {
            ToolResult::Interrupted
        } else {
            ToolResult::Failed
        }),
        _ => return Reply::none("unsupported", "unsupported Cursor hook event"),
    };
    let Some(tool_use_id) = event.tool_use_id.as_deref().and_then(host_id) else {
        return Reply::none(
            "unsupported",
            "missing Cursor tool-call identity; no ID generated",
        );
    };
    // A subagent's calls are a session of their own, under the parent call
    // that started it, whichever conversation ID they carry.
    let (host_session, parent) = match event.parent_tool_call_id.as_deref() {
        None => (conversation.clone(), None),
        Some(call) => match hook::subagent_session(&conversation, call) {
            Some(own) => (own, Some(format!("{SOURCE}.{conversation}"))),
            None => {
                return Reply::none(
                    "unsupported",
                    "Cursor subagent identity is outside supported grammar",
                );
            }
        },
    };
    let tool = event.tool_name.as_deref().unwrap_or_default();
    let config = match hook::resolve_workspace(config, event.directory(), deadline) {
        Ok(config) => config,
        Err(reply) => return reply,
    };
    let Some(input) = event.tool_input.as_ref() else {
        return Reply::none("unsupported", "Cursor tool input missing");
    };
    if tool == SHELL_TOOL {
        let Some(command) = input.get("command").and_then(Value::as_str) else {
            return Reply::none("unsupported", "Cursor Shell input has no command");
        };
        // Shell's own `cwd` is where the command runs; empty means the
        // event's directory.
        let cwd = input
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
            .or(event.directory());
        let operation = hook::shell_operation(
            SOURCE,
            &host_session,
            parent,
            &tool_use_id,
            command,
            cwd,
            matches!(phase, Phase::Pre),
            // Cursor does not mark a background command in its Shell input.
            false,
            &config.root,
            true,
            // Not yet seen to reach the model from a real Cursor (ADR 0029).
            false,
        );
        return hook::execute(operation, &config, deadline);
    }
    if tool == SEARCH_TOOL {
        if !matches!(phase, Phase::Post(ToolResult::Succeeded)) {
            return Reply::none("unsupported", "only a completed Grep is recorded");
        }
        return hook::search(
            SOURCE,
            &host_session,
            parent.as_deref(),
            &tool_use_id,
            input.get("path").and_then(Value::as_str),
            event.directory(),
            &config,
            deadline,
        );
    }
    let phase = match (tool, phase) {
        (READ_TOOL, Phase::Post(ToolResult::Succeeded)) => Phase::Read,
        (READ_TOOL, _) => {
            return Reply::none("unsupported", "only a completed Read is recorded");
        }
        (tool, phase) if ADMITTED_TOOLS.contains(&tool) => phase,
        _ => {
            return Reply::none(
                "unsupported",
                "supported Cursor tools are Write, Delete, Shell, Read and Grep",
            );
        }
    };
    let Some(file_path) = input.get("file_path").and_then(Value::as_str) else {
        return Reply::none("unsupported", "Cursor tool input names no file");
    };
    let key = match hook::file_key(&config.root, event.directory(), file_path) {
        Ok(key) => key,
        Err(error) => return Reply::none("unsupported", error),
    };
    hook::execute(
        Operation {
            source: SOURCE.into(),
            host_session,
            tool_use_id,
            tool: tool.into(),
            paths: vec![key.clone()],
            input_fingerprint: hook::input_fingerprint(tool, input),
            // A Write states the whole new content and a Delete none: the
            // file is held whole.
            edits: Vec::new(),
            phase,
            mid_turn_context: true,
            parent,
            // It reports its reads, so it gets no snapshot.
            snapshot_unknown: false,
            shell: None,
            // A `Write`'s whole new content, for the break check.
            contents: match (tool, input.get("content").and_then(Value::as_str)) {
                ("Write", Some(content)) => vec![(key, content.to_owned())],
                _ => Vec::new(),
            },
            // Cursor documents `additional_context` on `preToolUse`, but its
            // `Write` input has not been seen from a real Cursor: its writes
            // are checked for breaks only once a capture is in the
            // repository (ADR 0029).
            pre_tool_context: CAPTURED_WRITE.is_some(),
        },
        &config,
        deadline,
    )
}

/// Rewrite the shared lifecycle's reply in Cursor's documented output
/// fields: a denial as `permission`, `user_message` and `agent_message`, and
/// context as `additional_context`. Anything else is dropped, never passed
/// on: for a permission hook, Cursor blocks on a reply it cannot read.
fn translate(reply: Reply) -> Reply {
    if reply.stdout.is_empty() {
        return reply;
    }
    let output = serde_json::from_str::<Value>(&reply.stdout)
        .ok()
        .and_then(|value| value.get("hookSpecificOutput").cloned());
    let stdout = output.as_ref().and_then(|output| {
        if output.get("permissionDecision").and_then(Value::as_str) == Some("deny") {
            let reason = output.get("permissionDecisionReason")?.as_str()?;
            return Some(json!({
                "permission": "deny",
                "user_message": reason,
                "agent_message": reason,
            }));
        }
        let context = output.get("additionalContext")?.as_str()?;
        Some(json!({ "additional_context": context }))
    });
    match stdout {
        Some(stdout) => Reply {
            stdout: stdout.to_string(),
            ..reply
        },
        None => Reply {
            stdout: String::new(),
            detail: format!(
                "{}; a reply Cursor has no field for was dropped",
                reply.detail
            ),
            ..reply
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> HostEvent {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/cursor-constructed")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_break_check_is_on_exactly_when_a_captured_write_fixture_is_named() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // Every `pre-write.json` outside the constructed set is a capture.
        let captured: Vec<String> = std::fs::read_dir(root.join("fixtures"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("cursor-") && name != "cursor-constructed")
            .map(|name| format!("fixtures/{name}/pre-write.json"))
            .filter(|path| root.join(path).is_file())
            .collect();
        match CAPTURED_WRITE {
            None => assert!(
                captured.is_empty(),
                "a captured Cursor Write is present ({captured:?}): name it in CAPTURED_WRITE once it shows `file_path` and `content`"
            ),
            Some(path) => {
                assert!(captured.iter().any(|found| found == path), "{path}");
                let event: Value =
                    serde_json::from_str(&std::fs::read_to_string(root.join(path)).unwrap())
                        .unwrap();
                assert_eq!(event["tool_name"], "Write", "{path}");
                assert!(event["tool_input"]["file_path"].is_string(), "{path}");
                assert!(event["tool_input"]["content"].is_string(), "{path}");
            }
        }
    }

    #[test]
    fn constructed_events_parse_into_the_fields_the_adapter_reads() {
        for name in [
            "pre-write.json",
            "post-write.json",
            "post-failure-write.json",
            "pre-delete.json",
            "post-read.json",
            "pre-shell.json",
            "post-shell.json",
        ] {
            let event = fixture(name);
            assert!(event.conversation().is_some(), "{name}");
            assert!(event.tool_use_id.is_some(), "{name}");
            assert_eq!(event.directory(), Some("/work/demo"), "{name}");
            let tool = event.tool_name.as_deref().unwrap();
            assert!(
                ADMITTED_TOOLS.contains(&tool) || tool == READ_TOOL,
                "{name}"
            );
        }
        let failure = fixture("post-failure-write.json");
        assert!(failure.cwd.is_none(), "the CLI sends no cwd on a failure");
        for name in [
            "session-start.json",
            "before-submit-prompt.json",
            "session-end.json",
        ] {
            let event = fixture(name);
            assert!(event.conversation().is_some(), "{name}");
            assert_eq!(event.directory(), Some("/work/demo"), "{name}");
        }
        assert!(fixture("before-submit-prompt.json").generation_id.is_some());
    }

    #[test]
    fn a_denial_and_a_briefing_are_rewritten_in_cursors_fields_and_nothing_else_passes() {
        let deny = hook::deny(
            &[langspan_core::Conflict {
                lease_id: "lease".into(),
                session_id: "claude-code.other".into(),
                paths: vec!["a.txt".into()],
                regions: Vec::new(),
            }],
            &[],
        );
        let value: Value = serde_json::from_str(&translate(deny).stdout).unwrap();
        assert_eq!(value["permission"], "deny");
        assert_eq!(value["user_message"], value["agent_message"]);
        assert!(
            value["agent_message"]
                .as_str()
                .unwrap()
                .contains("not instructions")
        );
        assert_eq!(value.as_object().unwrap().len(), 3);

        let context = Reply {
            stdout: json!({"hookSpecificOutput": {"hookEventName": "PostToolUse",
                "additionalContext": "peer work"}})
            .to_string(),
            coverage: "observed",
            detail: String::new(),
        };
        assert_eq!(
            translate(context).stdout,
            json!({"additional_context": "peer work"}).to_string()
        );

        for stdout in [
            r#"{"hookSpecificOutput":{"permissionDecision":"allow"}}"#,
            r#"{"hookSpecificOutput":{"permissionDecision":"deny"}}"#,
            "not json",
        ] {
            let reply = translate(Reply {
                stdout: stdout.into(),
                coverage: "admitted",
                detail: String::new(),
            });
            assert!(reply.stdout.is_empty(), "{stdout}");
            assert!(reply.detail.contains("dropped"));
        }
    }

    #[test]
    fn host_ids_outside_the_grammar_become_distinct_digests_within_it() {
        assert_eq!(host_id("tool_0001").as_deref(), Some("tool_0001"));
        let a = host_id("call_abc|fc_def").unwrap();
        let b = host_id("call_abc|fc_deg").unwrap();
        let long = host_id(&"x".repeat(300)).unwrap();
        for id in [&a, &b, &long] {
            assert!(hook::valid_host_id(id), "{id}");
            langspan_core::validate_request_id(&format!("{SOURCE}.{id}")).unwrap();
        }
        assert_ne!(a, b);
        assert!(host_id("").is_none());
        assert!(host_id(&"x".repeat(5000)).is_none());
    }
}
