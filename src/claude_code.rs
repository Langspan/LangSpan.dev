//! Claude Code hook adapter: PreToolUse admission and post-tool observation.
//!
//! Verified against Claude Code 2.1.331 event shapes (see
//! `fixtures/claude-code-2.1.331`). The adapter never answers `allow` and
//! never exits 2. It returns a JSON `deny` only for a known conflict with a
//! current holder; every other outcome, including failures, is "no decision",
//! so the host's own permission flow governs. Admission is cooperative and does
//! not fence the write that the host performs afterwards.

pub use crate::hook::{Config, Identities, MAX_EVENT_BYTES, Reply, wire_id, workspace_request_id};
use crate::hook::{file_key, valid_host_id};
use langspan_core::ToolResult;
use serde::Deserialize;
use std::{io::Read, time::Instant};
pub const SOURCE: &str = langspan_core::CLAUDE_CODE_HOST;
pub const SUPPORTED_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
/// A completed `Read` is recorded as the version the session saw. It is
/// never admitted or denied.
pub const READ_TOOL: &str = "Read";
/// A completed `Grep` shows peers the path it searched (roadmap A13). Its
/// pattern is never read. It is never admitted or denied.
pub const SEARCH_TOOL: &str = "Grep";
pub fn identities(host_session: &str, tool_use_id: &str) -> Identities {
    crate::hook::identities_for(SOURCE, host_session, tool_use_id)
}
#[derive(Debug, Deserialize)]
struct HostEvent {
    hook_event_name: String,
    session_id: Option<String>,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    tool_input: Option<serde_json::Value>,
    cwd: Option<String>,
    is_interrupt: Option<bool>,
    /// Present when the event fired inside a subagent or an in-process
    /// teammate (Claude Code hooks reference, "Common input fields").
    agent_id: Option<String>,
}
/// In-memory parser entry point. CLI stdin is bounded by hook::read_stdin_until.
pub fn run(input: impl Read, config: &Config) -> Reply {
    let Some(deadline) = Instant::now().checked_add(config.budget) else {
        return Reply::none("unknown", "invalid hook deadline");
    };
    run_until(input, config, deadline)
}
pub fn run_until(input: impl Read, config: &Config, deadline: Instant) -> Reply {
    let started = Instant::now();
    let mut bytes = Vec::new();
    if let Err(error) = input.take(MAX_EVENT_BYTES + 1).read_to_end(&mut bytes) {
        return Reply::none("unknown", format!("could not read host event: {error}"));
    }
    if bytes.len() as u64 > MAX_EVENT_BYTES {
        return Reply::none("unsupported", "host event exceeds 8 MiB");
    }
    if from_cursor(&bytes) {
        return Reply::none(
            "unsupported",
            "a Cursor event reached the Claude Code hook (Cursor also runs Claude Code's hooks); nothing was done. For Cursor, run `langspan hooks install cursor`",
        );
    }
    if let Some(reply) = crate::hook::lifecycle_event(&bytes, SOURCE, config, deadline) {
        return reply;
    }
    let event: HostEvent = match serde_json::from_slice(&bytes) {
        Ok(event) => event,
        Err(_) => return Reply::none("unknown", "malformed host event"),
    };
    let phase = match event.hook_event_name.as_str() {
        "PreToolUse" => crate::hook::Phase::Pre,
        "PostToolUse" => crate::hook::Phase::Post(ToolResult::Succeeded),
        "PostToolUseFailure" => crate::hook::Phase::Post(if event.is_interrupt == Some(true) {
            ToolResult::Interrupted
        } else {
            ToolResult::Failed
        }),
        _ => return Reply::none("unsupported", "unsupported hook phase"),
    };
    if event.tool_name.as_deref() == Some(crate::hook::SHELL_TOOL) {
        return shell(event, phase, config, deadline, started);
    }
    if event.tool_name.as_deref() == Some(SEARCH_TOOL) {
        if !matches!(phase, crate::hook::Phase::Post(ToolResult::Succeeded)) {
            return Reply::none("unsupported", "only a completed Grep is recorded");
        }
        let (Some(host_session), Some(tool_use_id)) =
            (event.session_id.as_deref(), event.tool_use_id.as_deref())
        else {
            return Reply::none(
                "unsupported",
                "missing host tool-call identity; no ID generated",
            );
        };
        if !valid_host_id(host_session) {
            return Reply::none(
                "unsupported",
                "host session or tool-call identity is outside supported grammar",
            );
        }
        let (host_session, parent) = match identity(host_session, event.agent_id.as_deref()) {
            Ok(identity) => identity,
            Err(reply) => return reply,
        };
        let scope = event
            .tool_input
            .as_ref()
            .and_then(|input| input.get("path"))
            .and_then(serde_json::Value::as_str);
        let reply = crate::hook::search(
            SOURCE,
            &host_session,
            parent.as_deref(),
            tool_use_id,
            scope,
            event.cwd.as_deref(),
            config,
            deadline,
        );
        return Reply {
            detail: format!("{} ({} ms)", reply.detail, started.elapsed().as_millis()),
            ..reply
        };
    }
    let phase = match (event.tool_name.as_deref(), phase) {
        (Some(READ_TOOL), crate::hook::Phase::Post(ToolResult::Succeeded)) => {
            crate::hook::Phase::Read
        }
        (Some(READ_TOOL), crate::hook::Phase::Pre) => {
            let reply = pre_read(event, config, deadline);
            return Reply {
                detail: format!("{} ({} ms)", reply.detail, started.elapsed().as_millis()),
                ..reply
            };
        }
        (Some(READ_TOOL), _) => {
            return Reply::none("unsupported", "only a completed Read is recorded");
        }
        (_, phase) => phase,
    };
    let Some(tool) = event
        .tool_name
        .as_deref()
        .filter(|tool| SUPPORTED_TOOLS.contains(tool) || *tool == READ_TOOL)
    else {
        return Reply::none(
            "unsupported",
            "supported tools are Edit, Write, MultiEdit, NotebookEdit, Read, Grep and Bash",
        );
    };
    let (Some(host_session), Some(tool_use_id)) =
        (event.session_id.as_deref(), event.tool_use_id.as_deref())
    else {
        return Reply::none(
            "unsupported",
            "missing host tool-call identity; no ID generated",
        );
    };
    if !valid_host_id(host_session) || !valid_host_id(tool_use_id) {
        return Reply::none(
            "unsupported",
            "host session or tool-call identity is outside supported grammar",
        );
    }
    let (host_session, parent) = match identity(host_session, event.agent_id.as_deref()) {
        Ok(identity) => identity,
        Err(reply) => return reply,
    };
    let Some(input) = event.tool_input else {
        return Reply::none("unsupported", "tool input missing");
    };
    // NotebookEdit names its file `notebook_path`; the others `file_path`.
    let Some(file_path) = ["file_path", "notebook_path"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
    else {
        return Reply::none("unsupported", "tool input names no file");
    };
    let config = match crate::hook::resolve_workspace(config, event.cwd.as_deref(), deadline) {
        Ok(config) => config,
        Err(reply) => return reply,
    };
    let key = match file_key(&config.root, event.cwd.as_deref(), file_path) {
        Ok(key) => key,
        Err(error) => return Reply::none("unsupported", error),
    };
    let reply = crate::hook::execute(
        crate::hook::Operation {
            source: SOURCE.into(),
            host_session,
            tool_use_id: tool_use_id.into(),
            tool: tool.into(),
            paths: vec![key.clone()],
            input_fingerprint: crate::hook::input_fingerprint(tool, &input),
            edits: replacements(tool, &input)
                .map(|edit| vec![(key.clone(), edit)])
                .unwrap_or_default(),
            phase,
            mid_turn_context: true,
            parent,
            // Claude Code reports its reads.
            snapshot_unknown: false,
            shell: None,
            // A `Write`'s content is in `edits`.
            contents: Vec::new(),
            pre_tool_context: true,
        },
        &config,
        deadline,
    );
    Reply {
        detail: format!("{} ({} ms)", reply.detail, started.elapsed().as_millis()),
        ..reply
    }
}

/// A `Read` about to run: with `--outline-reads on`, a whole read of a
/// large indexed file is refused once with its outline (roadmap A11).
/// Anything else is no decision.
fn pre_read(event: HostEvent, config: &Config, deadline: Instant) -> Reply {
    if !config.outline_reads {
        return Reply::none("unsupported", "only a completed Read is recorded");
    }
    let (Some(host_session), Some(tool_use_id)) =
        (event.session_id.as_deref(), event.tool_use_id.as_deref())
    else {
        return Reply::none(
            "unsupported",
            "missing host tool-call identity; no ID generated",
        );
    };
    if !valid_host_id(host_session) {
        return Reply::none(
            "unsupported",
            "host session or tool-call identity is outside supported grammar",
        );
    }
    let (host_session, parent) = match identity(host_session, event.agent_id.as_deref()) {
        Ok(identity) => identity,
        Err(reply) => return reply,
    };
    let Some(input) = event.tool_input.as_ref() else {
        return Reply::none("unsupported", "tool input missing");
    };
    let Some(file_path) = input.get("file_path").and_then(serde_json::Value::as_str) else {
        return Reply::none("unsupported", "tool input names no file");
    };
    let config = match crate::hook::resolve_workspace(config, event.cwd.as_deref(), deadline) {
        Ok(config) => config,
        Err(reply) => return reply,
    };
    let key = match file_key(&config.root, event.cwd.as_deref(), file_path) {
        Ok(key) => key,
        Err(error) => return Reply::none("unsupported", error),
    };
    crate::hook::outline_read(
        SOURCE,
        &host_session,
        parent.as_deref(),
        tool_use_id,
        &key,
        input,
        &config,
        deadline,
    )
}

/// Cursor runs the hooks in Claude Code's settings files with its own
/// payload, which always carries `cursor_version`
/// (<https://cursor.com/docs/agent/hooks>, "Input (all hooks)";
/// <https://cursor.com/docs/reference/third-party-hooks>). Claude Code's
/// payload never does. Acting on such an event would coordinate one Cursor
/// tool call twice, under two session identities.
fn from_cursor(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Probe {
        cursor_version: Option<serde_json::Value>,
    }
    serde_json::from_slice::<Probe>(bytes).is_ok_and(|probe| probe.cursor_version.is_some())
}

/// The session a tool event belongs to, and its parent's LangSpan session
/// ID for a subagent. A subagent's tool events carry the parent's
/// `session_id` and the subagent's own `agent_id`; each subagent is a
/// session of its own, for edits and shell commands alike.
fn identity(host_session: &str, agent: Option<&str>) -> Result<(String, Option<String>), Reply> {
    crate::hook::own_session(SOURCE, host_session, agent).ok_or_else(|| {
        Reply::none(
            "unsupported",
            "subagent identity is outside supported grammar",
        )
    })
}

/// The string replacements an `Edit` or `MultiEdit` states, in order, or the
/// content a `Write` states. `None` for any other tool or an input of
/// another shape: the file is held whole and may change on any line.
fn replacements(tool: &str, input: &serde_json::Value) -> Option<crate::regions::Edit> {
    let one = |edit: &serde_json::Value| {
        Some(crate::regions::Replacement {
            old: edit.get("old_string")?.as_str()?.to_owned(),
            new: edit.get("new_string")?.as_str()?.to_owned(),
            replace_all: match edit.get("replace_all") {
                None | Some(serde_json::Value::Null) => false,
                Some(value) => value.as_bool()?,
            },
        })
    };
    let list = match tool {
        "Write" => {
            return Some(crate::regions::Edit::Write(
                input.get("content")?.as_str()?.to_owned(),
            ));
        }
        "Edit" => vec![one(input)?],
        "MultiEdit" => input
            .get("edits")?
            .as_array()?
            .iter()
            .map(one)
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    Some(crate::regions::Edit::Replace(list))
}

/// A `Bash` call: before it, admission when its write set is literal; after
/// it, success or failure, what it changed ([ADR 0011](../docs/adr/0011-shell-writes.md)).
fn shell(
    event: HostEvent,
    phase: crate::hook::Phase,
    config: &Config,
    deadline: Instant,
    started: Instant,
) -> Reply {
    let (Some(host_session), Some(tool_use_id)) =
        (event.session_id.as_deref(), event.tool_use_id.as_deref())
    else {
        return Reply::none(
            "unsupported",
            "missing host tool-call identity; no ID generated",
        );
    };
    let Some(command) = event
        .tool_input
        .as_ref()
        .and_then(|input| input.get("command"))
        .and_then(serde_json::Value::as_str)
    else {
        return Reply::none("unsupported", "Bash input has no command");
    };
    if !valid_host_id(host_session) || !valid_host_id(tool_use_id) {
        return Reply::none(
            "unsupported",
            "host session or tool-call identity is outside supported grammar",
        );
    }
    let (host_session, parent) = match identity(host_session, event.agent_id.as_deref()) {
        Ok(identity) => identity,
        Err(reply) => return reply,
    };
    let config = match crate::hook::resolve_workspace(config, event.cwd.as_deref(), deadline) {
        Ok(config) => config,
        Err(reply) => return reply,
    };
    let operation = crate::hook::shell_operation(
        SOURCE,
        &host_session,
        parent,
        tool_use_id,
        command,
        event.cwd.as_deref(),
        matches!(phase, crate::hook::Phase::Pre),
        // The command keeps running after the call returns.
        event
            .tool_input
            .as_ref()
            .and_then(|input| input.get("run_in_background"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        &config.root,
        true,
        true,
    );
    let reply = crate::hook::execute(operation, &config, deadline);
    Reply {
        detail: format!("{} ({} ms)", reply.detail, started.elapsed().as_millis()),
        ..reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::{REASON_LIMIT, deny};
    use langspan_core::Conflict;
    use std::fs;

    #[test]
    fn wire_ids_are_deterministic_bounded_and_accepted_by_the_validator() {
        let ids = identities(
            "00000000-0000-4000-8000-000000000001",
            "toolu_018A5huJynCN32GAjA9oC2mA",
        );
        // Golden vectors computed independently (Python hashlib); changing the
        // derivation changes retry identity.
        assert_eq!(ids.admit, "lsh1-admit-887b5271ce369e6b398b1f0163a28fe9");
        assert_eq!(ids.operation_id, "lsh1-op-7b1f4e085dccc9999494f3c8a32e1792");
        for id in [&ids.admit, &ids.complete, &ids.register, &ids.operation_id] {
            langspan_core::validate_request_id(id).unwrap();
        }
        langspan_core::validate_request_id(&ids.session_id).unwrap();
        assert_eq!(identities("s", "t"), identities("s", "t"));
        let distinct = [ids.admit, ids.complete, ids.register, ids.operation_id];
        assert_eq!(
            distinct
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            4
        );
    }

    #[test]
    fn length_prefixing_keeps_field_boundaries_unambiguous() {
        assert_ne!(
            wire_id("admit", &["ab", "c"]),
            wire_id("admit", &["a", "bc"])
        );
        assert_ne!(identities("s1", "t").admit, identities("s", "1t").admit);
    }

    #[test]
    fn captured_host_events_parse_into_the_fields_the_adapter_reads() {
        for (name, body) in [
            (
                "pre-edit",
                include_str!("../fixtures/claude-code-2.1.331/pre-edit.json"),
            ),
            (
                "post-edit",
                include_str!("../fixtures/claude-code-2.1.331/post-edit.json"),
            ),
            (
                "pre-write",
                include_str!("../fixtures/claude-code-2.1.331/pre-write.json"),
            ),
            (
                "post-write",
                include_str!("../fixtures/claude-code-2.1.331/post-write.json"),
            ),
            (
                "post-failure-write",
                include_str!("../fixtures/claude-code-2.1.331/post-failure-write.json"),
            ),
        ] {
            let event: HostEvent = serde_json::from_str(body).expect(name);
            assert!(event.session_id.is_some(), "{name}");
            assert!(event.tool_use_id.is_some(), "{name}");
            assert!(SUPPORTED_TOOLS.contains(&event.tool_name.as_deref().unwrap()));
            assert!(
                event
                    .tool_input
                    .unwrap()
                    .get("file_path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap()
                    .starts_with("/work/demo/")
            );
        }
        let failure: HostEvent = serde_json::from_str(include_str!(
            "../fixtures/claude-code-2.1.331/post-failure-write.json"
        ))
        .unwrap();
        assert_eq!(failure.hook_event_name, "PostToolUseFailure");
        assert_eq!(failure.is_interrupt, Some(false));
    }

    #[test]
    fn deny_reply_is_the_documented_shape_labelled_and_capped() {
        let holders = vec![Conflict {
            lease_id: "lease".into(),
            session_id: "claude-code.other".into(),
            paths: vec!["x".repeat(2000)],
            regions: Vec::new(),
        }];
        let reply = deny(&holders, &[]);
        let value: serde_json::Value = serde_json::from_str(&reply.stdout).unwrap();
        let output = &value["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "deny");
        assert_eq!(output["hookEventName"], "PreToolUse");
        let reason = output["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("not instructions"));
        assert!(reason.len() < REASON_LIMIT + 300);
        assert!(!reply.stdout.contains("\"allow\""));
    }

    #[test]
    fn each_subagent_gets_its_own_identity_within_the_id_grammar() {
        use crate::hook::subagent_session;
        let parent = "00000000-0000-4000-8000-000000000001";
        assert_eq!(
            subagent_session(parent, "agent-abc123").as_deref(),
            Some("00000000-0000-4000-8000-000000000001.agent-abc123")
        );
        // Outside the grammar, or long: a digest, distinct per agent ID.
        let odd = subagent_session(parent, "agent/with spaces").unwrap();
        let other = subagent_session(parent, "agent/with  spaces").unwrap();
        let long = subagent_session(parent, &"a".repeat(300)).unwrap();
        for id in [&odd, &other, &long] {
            assert!(valid_host_id(id), "{id}");
            langspan_core::validate_request_id(&format!("{SOURCE}.{id}")).unwrap();
        }
        assert_ne!(odd, other);
        assert!(subagent_session(parent, "").is_none());
        assert!(subagent_session(&"s".repeat(100), "agent").is_none());
    }

    #[test]
    fn a_region_denial_names_the_overlapping_symbol_or_lines() {
        use langspan_core::{PathRegions, Region};
        let digest = |n: char| n.to_string().repeat(64);
        let symbol = |key: &[&str]| Region::Symbol {
            key: key.iter().map(|name| (*name).into()).collect(),
            digest: digest('3'),
            start_line: 1,
            end_line: 5,
        };
        let located = |version: char, regions: Vec<Region>| PathRegions {
            path: "src/lib.rs".into(),
            digest: digest(version),
            regions,
        };
        let reason = |theirs: Vec<PathRegions>, ours: &[PathRegions]| {
            let holders = vec![Conflict {
                lease_id: "lease".into(),
                session_id: "claude-code.other".into(),
                paths: vec!["src/lib.rs".into()],
                regions: theirs,
            }];
            let value: serde_json::Value =
                serde_json::from_str(&deny(&holders, ours).stdout).unwrap();
            value["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let held = vec![located(
            '1',
            vec![symbol(&["Store", "open"]), symbol(&["beta"])],
        )];
        let text = reason(held.clone(), &[located('2', vec![symbol(&["Store"])])]);
        assert!(
            text.contains(r#""src/lib.rs" symbol "Store::open""#),
            "{text}"
        );
        assert!(!text.contains("beta"), "only what overlaps: {text}");
        let whole = reason(held, &[]);
        assert!(whole.contains("symbol \"beta\""), "{whole}");
        assert!(whole.contains("counts as the whole file"), "{whole}");
        let lines = Region::Lines {
            start_line: 4,
            end_line: 9,
        };
        let text = reason(
            vec![located('1', vec![lines.clone()])],
            &[located('2', vec![lines])],
        );
        assert!(
            text.contains("lines 4-9 of another version of the file"),
            "{text}"
        );
        let text = reason(Vec::new(), &[located('1', vec![symbol(&["beta"])])]);
        assert!(
            text.contains(r#""src/lib.rs" is held as the whole file"#),
            "{text}"
        );
        // Repository text in a key stays on its line.
        let text = reason(vec![located('1', vec![symbol(&["evil\u{202e}name"])])], &[]);
        assert!(!text.contains('\u{202e}'), "{text}");
    }

    #[test]
    fn file_keys_stay_inside_the_root() {
        let base = std::env::temp_dir().join(format!("ls-key-{}", std::process::id()));
        let root = base.join("root");
        fs::create_dir_all(root.join("src")).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let inside = root.join("src/new.rs");
        assert_eq!(
            file_key(&root, None, inside.to_str().unwrap()).unwrap(),
            "src/new.rs"
        );
        assert_eq!(
            file_key(&root, Some(root.to_str().unwrap()), "src/a.rs").unwrap(),
            "src/a.rs"
        );
        assert!(file_key(&root, None, base.join("other.rs").to_str().unwrap()).is_err());
        assert!(file_key(&root, None, &format!("{}/../x", root.display())).is_err());
        assert!(file_key(&root, None, "relative.rs").is_err());
        assert!(file_key(&root, None, root.to_str().unwrap()).is_err());
        fs::remove_dir_all(base).unwrap();
    }
}
