//! Antigravity hook adapter: `PreToolUse` admission, `PostToolUse`
//! observation and reads, and briefings at `SessionStart` and
//! `PreInvocation` ([ADR 0035](../docs/adr/0035-antigravity-adapter.md)).
//!
//! Built from Antigravity's documentation
//! (<https://antigravity.google/docs/hooks/>, read 25 November 2026) and
//! corrected against the events of a real Antigravity 1.5.14 run, kept in
//! `fixtures/antigravity-1.5.14` (issue #187). The fixtures in
//! `fixtures/antigravity-constructed` follow the documented names.
//!
//! The installed command names its event (`--event NAME`) and the caller
//! passes it in: the documentation lists no event name in the payload, though
//! 1.5.14 sends `hookEventName`.
//!
//! It never answers `auto_approve` or `ask` and never exits 2. On
//! `PreToolUse` it answers `{"decision":"deny",...}` for a known conflict or
//! a stale source and `{"decision":"allow"}` otherwise, which delegates to
//! Antigravity's own policy: 1.5.14 reads `{}` there as a refusal. Context
//! goes out as a system message in `injectSteps`, on the two events that
//! take it. Every other outcome is `{}`.

use crate::hook::{self, Config, LifecycleKind, Operation, Phase, Reply};
use langspan_core::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

/// The session namespace: a conversation is `antigravity.<conversationId>`.
pub const SOURCE: &str = "antigravity";
/// Tools admitted before they run. `edit_file` is what 1.5.14 sends; the
/// other three are the documented names, kept in case another version uses
/// them. `write_to_file` states the whole file; `edit_file` and the replace
/// tools state what they change.
pub const EDIT_TOOLS: [&str; 4] = [
    "edit_file",
    "write_to_file",
    "replace_file_content",
    "multi_replace_file_content",
];
pub const SHELL_TOOL: &str = "run_command";
/// A completed read is recorded as the version the session saw: `read_file`
/// in 1.5.14, `view_file` in the documentation.
pub const READ_TOOLS: [&str; 2] = ["read_file", "view_file"];
/// The answer on an event that takes no decision, or whose event is unknown.
pub const NO_DECISION: &str = "{}";
/// `PreToolUse`'s answer when LangSpan has no objection: proceed, delegating
/// to the next hook or Antigravity's own policy. 1.5.14 reads `{}` there as a
/// refusal with an empty reason.
pub const ALLOW: &str = r#"{"decision":"allow"}"#;

/// The answer for `event` when the hook has nothing to say, or cannot run:
/// an explicit allow on `PreToolUse`, else `{}`.
pub fn fallback(event: Option<&str>) -> &'static str {
    if event == Some("PreToolUse") {
        ALLOW
    } else {
        NO_DECISION
    }
}

/// The argument holding a file tool's path. 1.5.14's `edit_file` and
/// `read_file` take `Path`; the documented tools take `TargetFile` or
/// `AbsolutePath`.
fn path_key(tool: &str) -> &'static str {
    match tool {
        "edit_file" | "read_file" => "Path",
        "view_file" => "AbsolutePath",
        _ => "TargetFile",
    }
}

/// The fields Antigravity documents, and `cwd`, which 1.5.14 sends, in its
/// camelCase. Others are ignored.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostEvent {
    conversation_id: Option<String>,
    workspace_paths: Option<Vec<String>>,
    /// The workspace the conversation runs in. The hook process's own
    /// working directory is `<workspace>/.agents` in 1.5.14 and never used.
    cwd: Option<String>,
    last_user_input: Option<String>,
    tool_call: Option<ToolCall>,
    /// The index of the step in the conversation's trajectory: the only
    /// field that can tell one tool call from another.
    step_idx: Option<u64>,
    /// `PostToolUse`: the runtime error message, empty when the call worked.
    error: Option<Value>,
    /// `PreInvocation`: which model call of the execution this is, from 0.
    invocation_num: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ToolCall {
    name: Option<String>,
    args: Option<Value>,
}

impl HostEvent {
    /// The conversation's workspace folders that are absolute, in order;
    /// the event's `cwd` when it lists none.
    fn folders(&self) -> impl Iterator<Item = &str> + Clone {
        let absolute = |dir: &&str| Path::new(dir).is_absolute();
        let listed = self
            .workspace_paths
            .iter()
            .flatten()
            .map(String::as_str)
            .filter(absolute);
        let cwd = self
            .cwd
            .as_deref()
            .filter(absolute)
            .filter(|_| listed.clone().next().is_none());
        listed.chain(cwd)
    }

    /// The workspace folder the event is about: the most specific one
    /// holding `target`, else the first. Each must be absolute. A linked
    /// worktree listed inside its repository is its own folder, whatever
    /// the order; `Path::starts_with` compares whole components, so
    /// `/work/repo2` is not inside `/work/repo`.
    fn directory(&self, target: Option<&str>) -> Option<&str> {
        let folders = self.folders();
        let first = folders.clone().next();
        target
            .and_then(|target| {
                folders
                    .filter(|dir| Path::new(target).starts_with(dir))
                    .max_by_key(|dir| Path::new(dir).components().count())
            })
            .or(first)
    }

    fn failed(&self) -> bool {
        match &self.error {
            None | Some(Value::Null) => false,
            Some(Value::String(text)) => !text.trim().is_empty(),
            Some(_) => true,
        }
    }
}

pub fn run(input: impl Read, config: &Config, event: &str) -> Reply {
    let Some(deadline) = Instant::now().checked_add(config.budget) else {
        return answer(event, Reply::none("unknown", "invalid hook deadline"));
    };
    run_until(input, config, event, deadline)
}

/// Handle one event, named by the installed command's `--event`.
pub fn run_until(input: impl Read, config: &Config, event: &str, deadline: Instant) -> Reply {
    let started = Instant::now();
    let reply = answer(event, handle(input, config, event, deadline));
    Reply {
        detail: format!("{} ({} ms)", reply.detail, started.elapsed().as_millis()),
        ..reply
    }
}

fn handle(input: impl Read, config: &Config, event_name: &str, deadline: Instant) -> Reply {
    if Instant::now() >= deadline {
        return Reply::none("timeout", "hook deadline elapsed before parsing");
    }
    let mut bytes = Vec::new();
    if let Err(error) = input
        .take(hook::MAX_EVENT_BYTES + 1)
        .read_to_end(&mut bytes)
    {
        return Reply::none(
            "unknown",
            format!("could not read Antigravity event: {error}"),
        );
    }
    if bytes.len() as u64 > hook::MAX_EVENT_BYTES {
        return Reply::none("unsupported", "Antigravity event exceeds 8 MiB");
    }
    let event: HostEvent = match serde_json::from_slice(&bytes) {
        Ok(event) => event,
        Err(_) => return Reply::none("unknown", "malformed Antigravity hook JSON"),
    };
    let Some(conversation) = event
        .conversation_id
        .clone()
        .filter(|id| hook::valid_host_id(id))
    else {
        return Reply::none(
            "unsupported",
            "Antigravity event lacks a usable conversation identity",
        );
    };
    let lifecycle =
        |kind: LifecycleKind| lifecycle(&event, &conversation, kind, event_name, config, deadline);
    let pre = match event_name {
        "SessionStart" => return lifecycle(LifecycleKind::Start),
        // Before every model call. The first of an execution starts from
        // the user's input: that is the prompt. A later one gets peer news
        // that changed meanwhile, and nothing about the prompt again.
        "PreInvocation" => {
            return lifecycle(LifecycleKind::Prompt {
                prompt: event
                    .last_user_input
                    .clone()
                    .filter(|_| event.invocation_num == Some(0)),
            });
        }
        "PreToolUse" => true,
        "PostToolUse" => false,
        _ => return Reply::none("unsupported", "unsupported Antigravity hook event"),
    };
    match tool_call(&event, &conversation, pre, config, deadline) {
        Ok((operation, resolved)) => hook::execute(operation, &resolved, deadline),
        Err(reply) => reply,
    }
}

/// At most this many workspace folders are registered and briefed at one
/// lifecycle event. A folder past them gets its session at its first tool
/// call, and no briefing.
const LIFECYCLE_FOLDERS: usize = 8;

/// The conversation's session in one workspace. A conversation can hold
/// several workspace folders, each possibly a repository of its own, while a
/// LangSpan session belongs to one workspace. So every event names the
/// session of the workspace it resolved to: the conversation ID and a short
/// digest of that workspace's ID. The same pair always gives the same ID; a
/// conversation ID too long to take the suffix is replaced by its digest.
fn scoped_session(conversation: &str, workspace_id: &str) -> String {
    let digest = |text: &str| crate::sha256::hex(&crate::sha256::digest(text.as_bytes()));
    let scope = &digest(workspace_id)[..12];
    let head = if conversation.len() + scope.len() + 2 <= 100 {
        conversation.to_owned()
    } else {
        format!("h{}", &digest(conversation)[..32])
    };
    format!("{head}.w{scope}")
}

/// A lifecycle event in each of the conversation's workspace folders (the
/// installed root when it names none), once per workspace: each workspace's
/// session is registered and briefed there. The briefings are joined, within
/// the one briefing limit.
fn lifecycle(
    event: &HostEvent,
    conversation: &str,
    kind: LifecycleKind,
    event_name: &str,
    config: &Config,
    deadline: Instant,
) -> Reply {
    let mut folders: Vec<Option<&str>> =
        event.folders().take(LIFECYCLE_FOLDERS).map(Some).collect();
    if folders.is_empty() {
        folders.push(None);
    }
    let mut workspaces = Vec::new();
    let mut replies = Vec::new();
    // What the contexts so far take of the one briefing limit, separators
    // included. Each workspace is briefed within the rest, so nothing is
    // delivered that the join would then cut.
    let mut used = 0_usize;
    for folder in folders {
        let resolved = match hook::resolve_workspace(config, folder, deadline) {
            Ok(resolved) => resolved,
            Err(reply) => {
                replies.push(reply);
                continue;
            }
        };
        if workspaces.contains(&resolved.workspace_id) {
            continue;
        }
        let separator = if used == 0 { 0 } else { 2 };
        let reply = hook::lifecycle_resolved(
            SOURCE,
            &scoped_session(conversation, &resolved.workspace_id),
            kind.clone(),
            (event_name, None),
            &resolved,
            deadline,
            hook::BRIEFING_LIMIT.saturating_sub(used + separator),
        );
        if let Some(text) = context(&reply) {
            used += separator + text.len();
        }
        replies.push(reply);
        workspaces.push(resolved.workspace_id);
    }
    joined(replies)
}

/// The context a reply in the shape `hook::lifecycle_resolved` writes holds.
fn context(reply: &Reply) -> Option<String> {
    serde_json::from_str::<Value>(&reply.stdout)
        .ok()?
        .get("hookSpecificOutput")?
        .get("additionalContext")?
        .as_str()
        .map(str::to_owned)
}

/// One reply for a lifecycle event handled in several workspaces: their
/// contexts in folder order, under the coverage of the first that briefed,
/// else the first that did not fail. Each was briefed within what the ones
/// before it left of the briefing limit, so the join is never cut: what was
/// delivered is shown.
fn joined(replies: Vec<Reply>) -> Reply {
    if replies.len() == 1 {
        return replies
            .into_iter()
            .next()
            .unwrap_or_else(|| Reply::none("unknown", ""));
    }
    let contexts: Vec<String> = replies.iter().filter_map(context).collect();
    let failed = |reply: &&Reply| {
        matches!(reply.coverage, "unsupported" | "unknown" | "timeout")
            || reply.coverage.starts_with("deferred")
    };
    let coverage = replies
        .iter()
        .find(|reply| context(reply).is_some())
        .or_else(|| replies.iter().find(|reply| !failed(reply)))
        .or(replies.first())
        .map_or("unknown", |reply| reply.coverage);
    let detail = replies
        .iter()
        .map(|reply| reply.detail.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let stdout = if contexts.is_empty() {
        String::new()
    } else {
        json!({ "hookSpecificOutput": {
            "additionalContext": contexts.join("\n\n")
        }})
        .to_string()
    };
    Reply {
        stdout,
        coverage,
        detail,
    }
}

fn tool_call(
    event: &HostEvent,
    conversation: &str,
    pre: bool,
    config: &Config,
    deadline: Instant,
) -> Result<(Operation, Config), Reply> {
    let unsupported = |detail: &str| Reply::none("unsupported", detail.to_owned());
    let call = event
        .tool_call
        .as_ref()
        .ok_or_else(|| unsupported("Antigravity tool event has no toolCall"))?;
    let tool = call.name.as_deref().unwrap_or_default();
    let reads = READ_TOOLS.contains(&tool);
    if !(EDIT_TOOLS.contains(&tool) || tool == SHELL_TOOL || reads) {
        return Err(unsupported(
            "supported Antigravity tools are edit_file, write_to_file, replace_file_content, multi_replace_file_content, run_command, read_file and view_file",
        ));
    }
    if reads && (pre || event.failed()) {
        return Err(unsupported("only a completed read is recorded"));
    }
    // The step index is the call's place in the conversation, and 1.5.14
    // gives both events of a call the step of the call in its own output; a
    // call's two events were not both captured (ADR 0035). `toolCall.id`,
    // which 1.5.14 also sends, is not documented.
    let step = event
        .step_idx
        .ok_or_else(|| unsupported("Antigravity tool event has no stepIdx; no ID generated"))?;
    let tool_use_id = format!("step-{step}");
    let args = call
        .args
        .as_ref()
        .filter(|args| args.is_object())
        .ok_or_else(|| unsupported("Antigravity tool call has no args"))?;
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    if tool == SHELL_TOOL {
        let command = text("CommandLine")
            .ok_or_else(|| unsupported("run_command args have no CommandLine"))?;
        let folder = event.directory(text("Cwd"));
        // The command runs in `Cwd`; a relative one is taken from the
        // workspace folder, an empty one is that folder.
        let cwd: Option<PathBuf> = match text("Cwd").filter(|cwd| !cwd.is_empty()) {
            Some(cwd) if Path::new(cwd).is_absolute() => Some(cwd.into()),
            Some(cwd) => folder.map(|folder| Path::new(folder).join(cwd)),
            None => folder.map(PathBuf::from),
        };
        let cwd = cwd.as_deref().and_then(Path::to_str);
        // The workspace is the folder's, as for a file tool: Antigravity
        // reads `.agents/hooks.json` there, not in the command's directory.
        let config = hook::resolve_workspace(config, folder, deadline)?;
        let operation = hook::shell_operation(
            SOURCE,
            &scoped_session(conversation, &config.workspace_id),
            // A subagent is a conversation of its own; its events do not
            // name the conversation that started it.
            None,
            &tool_use_id,
            command,
            cwd,
            pre,
            // A persistent command is meant to outlive its call.
            args.get("RunPersistent").and_then(Value::as_bool) == Some(true),
            &config.root,
            // `PostToolUse` takes no context (ADR 0035).
            false,
            // Nor does `PreToolUse`: its answer is a decision or `{}`.
            false,
        );
        return Ok((operation, config));
    }
    let file_path =
        text(path_key(tool)).ok_or_else(|| unsupported("Antigravity tool call names no file"))?;
    let cwd = event.directory(Some(file_path));
    let config = hook::resolve_workspace(config, cwd, deadline)?;
    let key = hook::file_key(&config.root, cwd, file_path)
        .map_err(|error| Reply::none("unsupported", error))?;
    let phase = match (pre, reads, event.failed()) {
        (true, _, _) => Phase::Pre,
        (false, true, _) => Phase::Read,
        (false, false, false) => Phase::Post(ToolResult::Succeeded),
        (false, false, true) => Phase::Post(ToolResult::Failed),
    };
    Ok((
        Operation {
            source: SOURCE.into(),
            host_session: scoped_session(conversation, &config.workspace_id),
            tool_use_id,
            tool: tool.into(),
            paths: vec![key.clone()],
            input_fingerprint: hook::input_fingerprint(tool, args),
            edits: replacement(tool, args)
                .map(|edit| vec![(key, edit)])
                .unwrap_or_default(),
            phase,
            // `PostToolUse` takes no context; news reaches the model at the
            // next `PreInvocation` instead.
            mid_turn_context: false,
            parent: None,
            // It reports its reads, so it gets no snapshot.
            snapshot_unknown: false,
            shell: None,
            contents: Vec::new(),
            // `PreToolUse` takes no context either, so a break alert there
            // would never reach the model (ADR 0029).
            pre_tool_context: false,
            // Nor does `PostToolUse`, so no calls are kept for after it.
            breaks_after: false,
        },
        config,
    ))
}

/// The string replacement an `edit_file` (`Target`, `Replacement`) or a
/// `replace_file_content` (`TargetContent`, `ReplacementContent`) call
/// states. `None` for the other tools, whose file is held whole:
/// `multi_replace_file_content`'s chunk fields are not documented.
fn replacement(tool: &str, args: &Value) -> Option<crate::regions::Edit> {
    let (old, new) = match tool {
        "edit_file" => ("Target", "Replacement"),
        "replace_file_content" => ("TargetContent", "ReplacementContent"),
        _ => return None,
    };
    Some(crate::regions::Edit::Replace(vec![
        crate::regions::Replacement {
            old: args.get(old)?.as_str()?.to_owned(),
            new: args.get(new)?.as_str()?.to_owned(),
            replace_all: match args.get("AllowMultiple") {
                None | Some(Value::Null) => false,
                Some(value) => value.as_bool()?,
            },
        },
    ]))
}

/// The shared lifecycle's reply in the fields Antigravity documents for
/// `event`: a denial as `decision` and `reason` on `PreToolUse`, context as
/// one system message in `injectSteps` on `SessionStart` and
/// `PreInvocation`. Anything else becomes the event's `fallback`, and what
/// was dropped is noted in the detail.
fn answer(event: &str, reply: Reply) -> Reply {
    let output = serde_json::from_str::<Value>(&reply.stdout)
        .ok()
        .and_then(|value| value.get("hookSpecificOutput").cloned());
    let text = |field: &str| {
        output
            .as_ref()
            .and_then(|output| output.get(field))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let denied = output
        .as_ref()
        .and_then(|output| output.get("permissionDecision"))
        .and_then(Value::as_str)
        == Some("deny");
    let stdout = match event {
        "PreToolUse" if denied => text("permissionDecisionReason")
            .map(|reason| json!({ "decision": "deny", "reason": reason })),
        "SessionStart" | "PreInvocation" if !denied => text("additionalContext").map(
            |context| json!({ "injectSteps": [{ "systemMessage": { "systemMessage": context } }] }),
        ),
        _ => None,
    };
    match stdout {
        Some(stdout) => Reply {
            stdout: stdout.to_string(),
            ..reply
        },
        None if reply.stdout.is_empty() => Reply {
            stdout: fallback(Some(event)).into(),
            ..reply
        },
        None => Reply {
            stdout: fallback(Some(event)).into(),
            detail: format!(
                "{}; a reply Antigravity has no field for on {event} was dropped",
                reply.detail
            ),
            ..reply
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const FIXTURE_ROOT: &str = "/work/demo";

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/antigravity-constructed")
            .join(name);
        fs::read_to_string(path).unwrap()
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "lsag-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            fs::write(root.join("a.txt"), "hello\n").unwrap();
            Self(root)
        }

        fn config(&self) -> Config {
            Config {
                state_dir: self.0.join("state"),
                workspace_id: "demo".into(),
                root: self.0.clone(),
                ttl_ms: 30_000,
                budget: Duration::from_millis(1500),
                share_prompt: true,
                code_briefing: true,
                presence: true,
                presence_window_ms: langspan_core::PRESENCE_WINDOW_MS,
                outline_reads: false,
                vendor: crate::install::Vendor::Antigravity,
                user_level: false,
            }
        }

        fn event(&self, name: &str) -> Value {
            serde_json::from_str(&fixture(name).replace(FIXTURE_ROOT, self.0.to_str().unwrap()))
                .unwrap()
        }

        fn parse(&self, event: &Value, pre: bool) -> Result<Operation, Reply> {
            let config = self.config();
            let parsed: HostEvent = serde_json::from_value(event.clone()).unwrap();
            let session = parsed.conversation_id.clone().unwrap();
            tool_call(
                &parsed,
                &session,
                pre,
                &config,
                Instant::now() + config.budget,
            )
            .map(|(operation, _)| operation)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn constructed_events_parse_into_the_fields_the_adapter_reads() {
        for name in [
            "pre-write-to-file.json",
            "pre-replace.json",
            "post-replace.json",
            "pre-multi-replace.json",
            "post-view-file.json",
            "pre-run-command.json",
            "post-run-command.json",
        ] {
            let event: HostEvent = serde_json::from_str(&fixture(name)).expect(name);
            assert!(
                hook::valid_host_id(event.conversation_id.as_deref().unwrap()),
                "{name}"
            );
            assert!(event.step_idx.is_some(), "{name}");
            assert_eq!(event.directory(None), Some(FIXTURE_ROOT), "{name}");
            let tool = event.tool_call.as_ref().unwrap().name.as_deref().unwrap();
            assert!(
                EDIT_TOOLS.contains(&tool) || tool == SHELL_TOOL || READ_TOOLS.contains(&tool),
                "{name}"
            );
        }
        for name in ["session-start.json", "pre-invocation.json"] {
            let event: HostEvent = serde_json::from_str(&fixture(name)).expect(name);
            assert!(event.conversation_id.is_some(), "{name}");
            assert_eq!(event.directory(None), Some(FIXTURE_ROOT), "{name}");
        }
        let invocation: HostEvent = serde_json::from_str(&fixture("pre-invocation.json")).unwrap();
        assert_eq!(invocation.invocation_num, Some(0));
        assert!(invocation.last_user_input.is_some());
    }

    #[test]
    fn before_and_after_one_call_share_its_identity_and_a_replace_states_its_region() {
        let s = Scratch::new();
        let pre = s.parse(&s.event("pre-replace.json"), true).unwrap();
        let post = s.parse(&s.event("post-replace.json"), false).unwrap();
        assert_eq!(pre.source, "antigravity");
        assert_eq!(pre.tool, "replace_file_content");
        assert_eq!(pre.paths, ["a.txt"]);
        assert_eq!(pre.tool_use_id, post.tool_use_id);
        assert_eq!(pre.input_fingerprint, post.input_fingerprint);
        assert!(matches!(pre.phase, Phase::Pre));
        assert!(matches!(post.phase, Phase::Post(ToolResult::Succeeded)));
        assert_eq!(pre.edits.len(), 1, "a replace states what it changes");
        assert!(!pre.mid_turn_context && !pre.pre_tool_context);
        let ids = hook::identities_for(SOURCE, &pre.host_session, &pre.tool_use_id);
        assert_eq!(ids.session_id, format!("antigravity.{}", pre.host_session));
        langspan_core::validate_request_id(&ids.session_id).unwrap();
        // A whole-file write, and a multi-chunk replace whose chunk fields
        // are not documented, hold the file whole.
        for name in ["pre-write-to-file.json", "pre-multi-replace.json"] {
            let whole = s.parse(&s.event(name), true).unwrap();
            assert!(whole.edits.is_empty(), "{name}");
            assert_eq!(whole.paths, ["a.txt"], "{name}");
        }
    }

    #[test]
    fn a_failed_edit_is_a_failure_and_a_failed_or_early_read_records_nothing() {
        let s = Scratch::new();
        let mut failed = s.event("post-replace.json");
        failed["error"] = json!("target content not found in file");
        assert!(matches!(
            s.parse(&failed, false).unwrap().phase,
            Phase::Post(ToolResult::Failed)
        ));
        // An empty error is success, as the documentation says.
        failed["error"] = json!("");
        assert!(matches!(
            s.parse(&failed, false).unwrap().phase,
            Phase::Post(ToolResult::Succeeded)
        ));
        let read = s.parse(&s.event("post-view-file.json"), false).unwrap();
        assert!(matches!(read.phase, Phase::Read));
        assert_eq!(read.paths, ["a.txt"]);
        let mut unread = s.event("post-view-file.json");
        unread["error"] = json!("file not found");
        assert_eq!(
            s.parse(&unread, false).err().unwrap().coverage,
            "unsupported"
        );
        assert_eq!(
            s.parse(&s.event("post-view-file.json"), true)
                .err()
                .unwrap()
                .coverage,
            "unsupported"
        );
    }

    #[test]
    fn a_shell_call_runs_in_its_cwd() {
        let s = Scratch::new();
        fs::create_dir(s.0.join("sub")).unwrap();
        let mut event = s.event("pre-run-command.json");
        event["toolCall"]["args"]["CommandLine"] = json!("echo x > out.txt");
        event["toolCall"]["args"]["Cwd"] = json!(s.0.join("sub"));
        let operation = s.parse(&event, true).unwrap();
        assert_eq!(operation.paths, ["sub/out.txt"]);
        assert_eq!(operation.tool, hook::SHELL_TOOL);
        event["toolCall"]["args"]["Cwd"] = json!("sub");
        assert_eq!(s.parse(&event, true).unwrap().paths, ["sub/out.txt"]);
    }

    #[test]
    fn unsupported_tools_paths_and_events_get_the_empty_answer() {
        let s = Scratch::new();
        let mut other_tool = s.event("pre-replace.json");
        other_tool["toolCall"]["name"] = json!("grep_search");
        let mut outside = s.event("pre-replace.json");
        outside["toolCall"]["args"]["TargetFile"] = json!("/elsewhere/a.txt");
        let mut no_session = s.event("pre-replace.json");
        no_session["conversationId"] = json!("not a valid id");
        let mut no_step = s.event("pre-replace.json");
        no_step.as_object_mut().unwrap().remove("stepIdx");
        let mut relative = s.event("pre-replace.json");
        relative["workspacePaths"] = json!(["relative"]);
        relative["toolCall"]["args"]["TargetFile"] = json!("a.txt");
        for (event, name) in [
            (other_tool, "PreToolUse"),
            (outside, "PreToolUse"),
            (no_session, "PreToolUse"),
            (no_step, "PreToolUse"),
            (relative, "PreToolUse"),
            (s.event("pre-replace.json"), "Stop"),
            (s.event("pre-replace.json"), "PostInvocation"),
        ] {
            let reply = run(
                serde_json::to_vec(&event).unwrap().as_slice(),
                &s.config(),
                name,
            );
            assert_eq!(reply.coverage, "unsupported", "{event}: {}", reply.detail);
            // `PreToolUse` must allow explicitly: `{}` there is a refusal.
            let expected = if name == "PreToolUse" {
                ALLOW
            } else {
                NO_DECISION
            };
            assert_eq!(reply.stdout, expected, "{name}");
        }
        let malformed = run(&b"{broken"[..], &s.config(), "PreToolUse");
        assert_eq!(
            (malformed.coverage, malformed.stdout.as_str()),
            ("unknown", ALLOW)
        );
        let expired = run_until(&b"{}"[..], &s.config(), "PreToolUse", Instant::now());
        assert_eq!(
            (expired.coverage, expired.stdout.as_str()),
            ("timeout", ALLOW)
        );
        let expired = run_until(&b"{}"[..], &s.config(), "PostToolUse", Instant::now());
        assert_eq!(expired.stdout, NO_DECISION);
    }

    #[test]
    fn the_fallback_allows_only_on_pre_tool_use() {
        assert_eq!(fallback(Some("PreToolUse")), ALLOW);
        for event in [
            None,
            Some("PostToolUse"),
            Some("SessionStart"),
            Some("PreInvocation"),
            Some("preToolUse"),
        ] {
            assert_eq!(fallback(event), NO_DECISION, "{event:?}");
        }
        let value: Value = serde_json::from_str(ALLOW).unwrap();
        assert_eq!(value, json!({"decision": "allow"}));
    }

    #[test]
    fn captured_1_5_14_events_parse_with_their_tool_names_and_paths() {
        let s = Scratch::new();
        fs::write(s.0.join("main.py"), "    print(add(2, 3))\n").unwrap();
        fs::write(s.0.join("calc.py"), "x\n").unwrap();
        let captured = |name: &str| -> Value {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/antigravity-1.5.14")
                .join(name);
            let text = fs::read_to_string(path).unwrap();
            serde_json::from_str(&text.replace("/work/repo", s.0.to_str().unwrap())).unwrap()
        };
        let post = captured("post-edit-file.json");
        let mut pre = post.clone();
        pre.as_object_mut().unwrap().remove("error");
        let admitted = s.parse(&pre, true).unwrap();
        assert_eq!(admitted.tool, "edit_file");
        assert_eq!(admitted.paths, ["main.py"]);
        assert_eq!(admitted.tool_use_id, "step-6");
        assert_eq!(admitted.edits.len(), 1, "an edit_file states its region");
        let completed = s.parse(&post, false).unwrap();
        assert!(matches!(
            completed.phase,
            Phase::Post(ToolResult::Succeeded)
        ));
        assert_eq!(completed.tool_use_id, admitted.tool_use_id);
        assert_eq!(completed.input_fingerprint, admitted.input_fingerprint);
        let failed = s
            .parse(&captured("post-edit-file-failed.json"), false)
            .unwrap();
        assert!(matches!(failed.phase, Phase::Post(ToolResult::Failed)));
        let read = s.parse(&captured("post-read-file.json"), false).unwrap();
        assert!(matches!(read.phase, Phase::Read));
        assert_eq!(read.paths, ["calc.py"]);
        // `run_command` sends no `Cwd`: the command runs in the workspace.
        let shell = s
            .parse(&captured("pre-run-command-append.json"), true)
            .unwrap();
        assert_eq!(shell.paths, ["main.py"]);
    }

    #[test]
    fn the_events_cwd_is_its_folder_when_it_lists_none() {
        let only_cwd: HostEvent =
            serde_json::from_value(json!({"cwd": "/work/repo", "workspacePaths": []})).unwrap();
        assert_eq!(only_cwd.directory(None), Some("/work/repo"));
        let both: HostEvent = serde_json::from_value(json!({
            "cwd": "/work/other", "workspacePaths": ["/work/repo"],
        }))
        .unwrap();
        assert_eq!(both.folders().collect::<Vec<_>>(), ["/work/repo"]);
        let relative: HostEvent = serde_json::from_value(json!({"cwd": "repo"})).unwrap();
        assert_eq!(relative.directory(None), None);
    }

    #[test]
    fn a_denial_and_a_briefing_take_antigravitys_fields_and_nothing_else_passes() {
        let deny = hook::deny(
            &[langspan_core::Conflict {
                lease_id: "lease".into(),
                session_id: "claude-code.other".into(),
                paths: vec!["a.txt".into()],
                regions: Vec::new(),
            }],
            &[],
        );
        let value: Value =
            serde_json::from_str(&answer("PreToolUse", deny.clone()).stdout).unwrap();
        assert_eq!(value["decision"], "deny");
        assert!(
            value["reason"]
                .as_str()
                .unwrap()
                .contains("not instructions")
        );
        assert_eq!(value.as_object().unwrap().len(), 2, "{value}");
        // A denial is never sent on an event that takes no decision.
        for event in ["PostToolUse", "SessionStart", "PreInvocation"] {
            assert_eq!(answer(event, deny.clone()).stdout, NO_DECISION, "{event}");
        }

        let context = Reply {
            stdout: json!({"hookSpecificOutput": {"hookEventName": "PreInvocation",
                "additionalContext": "peer work"}})
            .to_string(),
            coverage: "briefed",
            detail: String::new(),
        };
        for event in ["SessionStart", "PreInvocation"] {
            let value: Value =
                serde_json::from_str(&answer(event, context.clone()).stdout).unwrap();
            assert_eq!(
                value,
                json!({"injectSteps": [{"systemMessage": {"systemMessage": "peer work"}}]}),
                "{event}"
            );
        }
        for event in ["PreToolUse", "PostToolUse"] {
            let dropped = answer(event, context.clone());
            assert_eq!(dropped.stdout, fallback(Some(event)), "{event}");
            assert!(dropped.detail.contains("dropped"), "{event}");
        }
        // Anything that is not a deny is the explicit allow, never another
        // permission.
        for stdout in [
            r#"{"hookSpecificOutput":{"permissionDecision":"allow"}}"#,
            r#"{"hookSpecificOutput":{"permissionDecision":"deny"}}"#,
            "not json",
        ] {
            let reply = answer(
                "PreToolUse",
                Reply {
                    stdout: stdout.into(),
                    coverage: "admitted",
                    detail: String::new(),
                },
            );
            assert_eq!(reply.stdout, ALLOW, "{stdout}");
            for word in ["auto_approve", "ask", "overwrite", "permissionOverrides"] {
                assert!(!reply.stdout.contains(word), "{stdout}");
            }
        }
    }

    #[test]
    fn a_conversation_has_one_stable_session_per_workspace() {
        let conversation = "00000000-0000-4000-8000-0000000000a1";
        let one = scoped_session(conversation, "root-one");
        assert_eq!(one, scoped_session(conversation, "root-one"));
        assert_ne!(one, scoped_session(conversation, "root-two"));
        assert_ne!(one, scoped_session("other", "root-one"));
        assert!(one.starts_with(conversation), "{one}");
        // The longest valid conversation ID still gives a valid, distinct one.
        let long = "a".repeat(100);
        let scoped = scoped_session(&long, "root-one");
        assert!(hook::valid_host_id(&scoped), "{scoped}");
        assert_ne!(scoped, scoped_session(&"b".repeat(100), "root-one"));
        for id in [one, scoped] {
            langspan_core::validate_request_id(&format!("{SOURCE}.{id}")).unwrap();
        }
    }

    #[test]
    fn briefings_from_several_workspaces_are_joined_whole() {
        let briefed = |context: &str| Reply {
            stdout: json!({"hookSpecificOutput": {"additionalContext": context}}).to_string(),
            coverage: "briefed",
            detail: "one".into(),
        };
        let both = joined(vec![
            Reply::none("deferred_to_project", "deferred"),
            briefed("first"),
            briefed("second"),
        ]);
        assert_eq!(both.coverage, "briefed");
        let value: Value = serde_json::from_str(&both.stdout).unwrap();
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"],
            "first\n\nsecond"
        );
        assert!(both.detail.contains("deferred"), "{}", both.detail);
        // Each part was briefed within the room the ones before it left, and
        // what it delivered is acknowledged: the join never cuts it.
        let long = joined(vec![
            briefed(&"x".repeat(hook::BRIEFING_LIMIT)),
            briefed("tail"),
        ]);
        let value: Value = serde_json::from_str(&long.stdout).unwrap();
        let text = value["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(text.ends_with("\n\ntail"), "{}", text.len());
        // Nothing to say: the coverage of the workspace that did not fail.
        let quiet = joined(vec![
            Reply::none("unsupported", "not a repository"),
            Reply::none("no_briefing", "nothing new"),
        ]);
        assert_eq!((quiet.coverage, quiet.stdout.as_str()), ("no_briefing", ""));
    }

    #[test]
    fn the_folder_holding_the_target_is_the_events_directory() {
        let event: HostEvent = serde_json::from_value(json!({
            "conversationId": "c",
            "workspacePaths": ["relative", "/work/one", "/work/two"],
        }))
        .unwrap();
        assert_eq!(event.directory(None), Some("/work/one"));
        assert_eq!(event.directory(Some("/work/two/a.txt")), Some("/work/two"));
        assert_eq!(event.directory(Some("/elsewhere/a.txt")), Some("/work/one"));
        // The most specific folder holding the target, whatever the order,
        // compared by whole path components.
        let nested: HostEvent = serde_json::from_value(json!({
            "workspacePaths": ["/work/repo", "/work/repo/.worktrees/topic", "/work/repo2"],
        }))
        .unwrap();
        for (target, folder) in [
            (
                "/work/repo/.worktrees/topic/a.txt",
                "/work/repo/.worktrees/topic",
            ),
            ("/work/repo/.worktrees/topic", "/work/repo/.worktrees/topic"),
            ("/work/repo/.worktrees/topical/a.txt", "/work/repo"),
            ("/work/repo/a.txt", "/work/repo"),
            ("/work/repo2/a.txt", "/work/repo2"),
        ] {
            assert_eq!(nested.directory(Some(target)), Some(folder), "{target}");
        }
        let none: HostEvent = serde_json::from_value(json!({"workspacePaths": []})).unwrap();
        assert_eq!(none.directory(None), None);
    }
}
