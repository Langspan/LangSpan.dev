//! Gemini CLI hook adapter: BeforeTool admission, AfterTool observation and
//! reads, and the session lifecycle with briefings
//! ([ADR 0033](../docs/adr/0033-gemini-cli-adapter.md)).
//!
//! The event shapes are taken from Gemini CLI 0.70.0: its hooks reference
//! (`docs/hooks/reference.md` at tag `v0.70.0`) and the code that builds the
//! payloads (`packages/core/src/hooks/hookEventHandler.ts`,
//! `packages/core/src/core/coreToolHookTriggers.ts`). The fixtures in
//! `fixtures/gemini-cli-contract` follow them; no live Gemini session has been
//! captured yet (see that directory's README).
//!
//! The adapter never answers `allow` and never exits 2. It answers
//! `{"decision":"deny"}` only for a known conflict or a stale source; every
//! other outcome is the empty object `{}`, so Gemini's own approval flow
//! governs. An empty object is printed rather than nothing because Gemini
//! parses a hook's stderr as its answer when stdout is empty, and reads
//! plain text there as `allow`.

use crate::hook::{self, Config, Operation, Phase, Reply};
use langspan_core::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    time::Instant,
};

/// The session namespace: a Gemini session is `gemini.<session_id>`.
pub const SOURCE: &str = langspan_core::GEMINI_HOST;
pub use crate::hook::MAX_EVENT_BYTES;
/// Tools admitted before they run.
pub const EDIT_TOOLS: [&str; 2] = ["write_file", "replace"];
pub const SHELL_TOOL: &str = "run_shell_command";
/// A completed `read_file` is recorded as the version the session saw.
pub const READ_TOOL: &str = "read_file";
/// How many calls on one target a session may make: see `call_id`.
const MAX_REPEATS: u32 = 4096;

#[derive(Debug, Deserialize)]
struct HostEvent {
    hook_event_name: String,
    session_id: Option<String>,
    cwd: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<Value>,
    tool_response: Option<Value>,
}

/// In-memory parser entry point. CLI stdin is bounded by hook::read_stdin_until.
pub fn run(input: impl Read, config: &Config) -> Reply {
    run_until(input, config, Instant::now() + config.budget)
}

pub fn run_until(input: impl Read, config: &Config, deadline: Instant) -> Reply {
    answer(handle(input, config, deadline))
}

fn handle(input: impl Read, config: &Config, deadline: Instant) -> Reply {
    if Instant::now() >= deadline {
        return Reply::none("timeout", "hook deadline elapsed before parsing");
    }
    let started = Instant::now();
    let mut body = Vec::new();
    if let Err(error) = input.take(MAX_EVENT_BYTES + 1).read_to_end(&mut body) {
        return Reply::none("unknown", format!("could not read host event: {error}"));
    }
    if body.len() as u64 > MAX_EVENT_BYTES {
        return Reply::none("unsupported", "host event exceeds 8 MiB");
    }
    let event: HostEvent = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(_) => return Reply::none("unknown", "malformed Gemini CLI hook JSON"),
    };
    if event.hook_event_name == "SessionEnd"
        && let Some(session) = event
            .session_id
            .as_deref()
            .filter(|s| hook::valid_host_id(s))
    {
        forget_calls(&config.state_dir, session);
    }
    if let Some(reply) = hook::lifecycle_event(&body, SOURCE, config, deadline) {
        return reply;
    }
    let (operation, resolved) = match parse(event, config, deadline) {
        Ok(parsed) => parsed,
        Err(reply) => return reply,
    };
    let reply = hook::execute(operation, &resolved, deadline);
    Reply {
        detail: format!("{} ({} ms)", reply.detail, started.elapsed().as_millis()),
        ..reply
    }
}

/// The reply in the shape Gemini CLI reads. A deny becomes `decision` and
/// `reason` (sent to the model as the tool's error); context keeps the
/// `hookSpecificOutput.additionalContext` field, which Gemini reads on
/// SessionStart, BeforeAgent and AfterTool; nothing becomes `{}`.
fn answer(reply: Reply) -> Reply {
    let parsed: Option<Value> = serde_json::from_str(&reply.stdout).ok();
    let stdout = match parsed {
        None => "{}".to_owned(),
        Some(value) if value["hookSpecificOutput"]["permissionDecision"] == "deny" => json!({
            "decision": "deny",
            "reason": value["hookSpecificOutput"]["permissionDecisionReason"],
        })
        .to_string(),
        Some(_) => reply.stdout,
    };
    Reply { stdout, ..reply }
}

fn parse(
    event: HostEvent,
    config: &Config,
    deadline: Instant,
) -> Result<(Operation, Config), Reply> {
    let pre = match event.hook_event_name.as_str() {
        "BeforeTool" => true,
        "AfterTool" => false,
        _ => return Err(Reply::none("unsupported", "unsupported hook event")),
    };
    let tool = event.tool_name.as_deref().unwrap_or_default();
    let reads = tool == READ_TOOL;
    if !(EDIT_TOOLS.contains(&tool) || tool == SHELL_TOOL || reads) {
        return Err(Reply::none(
            "unsupported",
            "supported tools are write_file, replace, read_file and run_shell_command",
        ));
    }
    if reads && pre && !config.outline_reads {
        return Err(Reply::none(
            "unsupported",
            "only a completed read_file is recorded",
        ));
    }
    let Some(host_session) = event.session_id.filter(|id| hook::valid_host_id(id)) else {
        return Err(Reply::none(
            "unsupported",
            "Gemini CLI event lacks a usable session identity",
        ));
    };
    let Some(input) = event.tool_input.filter(Value::is_object) else {
        return Err(Reply::none("unsupported", "tool input missing"));
    };
    let cwd = event
        .cwd
        .filter(|cwd| Path::new(cwd).is_absolute())
        .ok_or_else(|| Reply::none("unsupported", "event has no absolute working directory"))?;
    let failed = event
        .tool_response
        .as_ref()
        .and_then(|response| response.get("error"))
        .is_some_and(|error| !error.is_null());
    let config = hook::resolve_workspace(config, Some(&cwd), deadline)?;
    // With `--outline-reads on`, a whole read of a large indexed file is
    // refused once with its outline (roadmap A11). It is a call of its own,
    // whose AfterTool, if it runs, records the read as before.
    if reads && pre {
        let file_path = input
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| Reply::none("unsupported", "tool input names no file"))?;
        let key = hook::file_key(&config.root, Some(&cwd), file_path)
            .map_err(|error| Reply::none("unsupported", error))?;
        return Err(hook::outline_read(
            SOURCE,
            &host_session,
            None,
            &format!("gr{}", new_nonce()),
            &key,
            &input,
            &config,
            deadline,
        ));
    }
    // A read has no BeforeTool, so each completed one is a call of its own.
    let tool_use_id = call_id(&config.state_dir, &host_session, tool, &input, pre || reads)
        .map_err(|error| {
            Reply::none(
                "unknown",
                format!("could not keep this call's identity: {error}"),
            )
        })?;
    if tool == SHELL_TOOL {
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| Reply::none("unsupported", "run_shell_command input has no command"))?;
        // The command runs in `dir_path`, absolute or relative to the cwd.
        let dir = match input.get("dir_path").and_then(Value::as_str) {
            Some(dir) if !dir.is_empty() => Path::new(&cwd).join(dir),
            _ => PathBuf::from(&cwd),
        };
        let background = input
            .get("is_background")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let operation = hook::shell_operation(
            SOURCE,
            &host_session,
            // Gemini CLI's events do not say which subagent made a call.
            None,
            &tool_use_id,
            command,
            dir.to_str(),
            pre,
            background,
            &config.root,
            true,
            // Gemini CLI reads `additionalContext` on AfterTool, not BeforeTool.
            false,
        );
        return Ok((operation, config));
    }
    let file_path = input
        .get("file_path")
        .and_then(Value::as_str)
        .ok_or_else(|| Reply::none("unsupported", "tool input names no file"))?;
    let key = hook::file_key(&config.root, Some(&cwd), file_path)
        .map_err(|error| Reply::none("unsupported", error))?;
    let phase = match (pre, reads, failed) {
        (true, _, _) => Phase::Pre,
        (false, true, false) => Phase::Read,
        (false, true, true) => {
            return Err(Reply::none(
                "unsupported",
                "only a completed read_file is recorded",
            ));
        }
        (false, false, false) => Phase::Post(ToolResult::Succeeded),
        (false, false, true) => Phase::Post(ToolResult::Failed),
    };
    Ok((
        Operation {
            source: SOURCE.into(),
            host_session,
            tool_use_id,
            tool: tool.into(),
            paths: vec![key.clone()],
            input_fingerprint: hook::input_fingerprint(tool, &input),
            edits: replacement(tool, &input)
                .map(|edit| vec![(key.clone(), edit)])
                .unwrap_or_default(),
            phase,
            // Gemini appends AfterTool context to the tool's result.
            mid_turn_context: true,
            parent: None,
            // It reports its reads, so it gets no snapshot.
            snapshot_unknown: false,
            shell: None,
            // A `write_file`'s whole new content, for the break check.
            contents: match (tool, input.get("content").and_then(Value::as_str)) {
                ("write_file", Some(content)) => vec![(key, content.to_owned())],
                _ => Vec::new(),
            },
            // Gemini reads no context on BeforeTool (ADR 0033), so a break
            // alert there would never reach the model. Its AfterTool context
            // is added to the tool's result: the write's calls are found at
            // BeforeTool, kept, and checked and told at AfterTool (ADR 0029).
            pre_tool_context: false,
            breaks_after: true,
        },
        config,
    ))
}

/// The string replacement a `replace` call states. `None` for `write_file`
/// or an input of another shape: the file is held whole.
fn replacement(tool: &str, input: &Value) -> Option<crate::regions::Edit> {
    if tool != "replace" {
        return None;
    }
    Some(crate::regions::Edit::Replace(vec![
        crate::regions::Replacement {
            old: input.get("old_string")?.as_str()?.to_owned(),
            new: input.get("new_string")?.as_str()?.to_owned(),
            replace_all: match input.get("allow_multiple") {
                None | Some(Value::Null) => false,
                Some(value) => value.as_bool()?,
            },
        },
    ]))
}

/// A session's marker directory, named by a digest of its ID: the ID
/// grammar admits `.` and `..`, which must never become path components.
fn calls_dir(state_dir: &Path, session: &str) -> PathBuf {
    let name = crate::sha256::hex(&crate::sha256::digest(session.as_bytes()));
    state_dir.join("gemini-calls").join(&name[..32])
}

/// Gemini CLI gives a tool call no ID: BeforeTool and AfterTool carry the
/// session, the tool name and the call's `tool_input`, and nothing else that
/// identifies the call. The ID is the digest of the tool and what it acts on
/// (the file, or the command and its directory), numbered by how many such
/// calls the session has started, and a nonce fresh at each BeforeTool
/// (`claim`). BeforeTool takes the next number by linking a marker holding
/// the nonce into place exclusively; AfterTool takes the latest marker's.
/// The nonce keeps a resumed session (Gemini keeps the session ID on
/// `--resume` and `--session-id`) from reusing an ID once its markers are
/// gone. Two such calls in flight at once can share a marker, and the second
/// one's report is then answered as the first's. A call whose AfterTool never
/// comes (denied, or refused by the user) leaves its marker behind,
/// harmlessly. The rest of the input is left out of the ID, so an edit the
/// user changes in Gemini's confirmation dialog keeps its ID; its digest
/// then differs, so its admission is not completed and lapses at its TTL.
fn call_id(
    state_dir: &Path,
    session: &str,
    tool: &str,
    input: &Value,
    claim: bool,
) -> Result<String, String> {
    let target = json!([
        input.get("file_path"),
        input.get("command"),
        input.get("dir_path")
    ]);
    let digest = &hook::input_fingerprint(tool, &target)[..24];
    let dir = calls_dir(state_dir, session);
    let marker = |n: u32| dir.join(format!("{digest}.{n}"));
    let latest = (1..=MAX_REPEATS).take_while(|n| marker(*n).exists()).last();
    if let Some(n) = latest.filter(|_| !claim)
        && let Some(nonce) = read_nonce(&marker(n))
    {
        return Ok(format!("g{digest}-{n}-{nonce}"));
    }
    #[cfg(unix)]
    if !dir.exists() {
        prune(&state_dir.join("gemini-calls"));
        crate::local::private_directory(&dir).map_err(|e| e.to_string())?;
    }
    let nonce = new_nonce();
    let staged = dir.join(format!(".{digest}.{nonce}"));
    create_marker(&staged, &nonce).map_err(|e| format!("{}: {e}", staged.display()))?;
    let mut n = latest.unwrap_or(0) + 1;
    let result = loop {
        if n > MAX_REPEATS {
            break Err(format!("more than {MAX_REPEATS} calls on the same target"));
        }
        // A hard link is created whole or not at all: a reader never sees a
        // marker without its nonce.
        match std::fs::hard_link(&staged, marker(n)) {
            Ok(()) => break Ok(format!("g{digest}-{n}-{nonce}")),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(error) => break Err(format!("{}: {error}", marker(n).display())),
        }
    };
    let _ = std::fs::remove_file(&staged);
    result
}

/// 16 hexadecimal digits from the wall clock and the process ID.
fn new_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    crate::sha256::hex(&crate::sha256::digest(
        format!("{nanos}-{}", std::process::id()).as_bytes(),
    ))[..16]
        .to_owned()
}

fn read_nonce(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(64)
        .read_to_string(&mut text)
        .ok()?;
    let nonce = text.trim();
    (nonce.len() == 16 && nonce.bytes().all(|b| b.is_ascii_hexdigit())).then(|| nonce.to_owned())
}

#[cfg(unix)]
fn create_marker(path: &Path, nonce: &str) -> io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?
        .write_all(nonce.as_bytes())
}

#[cfg(not(unix))]
fn create_marker(_: &Path, _: &str) -> io::Result<()> {
    Err(io::Error::other(
        "Gemini CLI hooks are implemented for Unix",
    ))
}

/// Session call numbers older than a week: a session whose SessionEnd never
/// arrived (Gemini does not wait for that hook).
fn prune(dir: &Path) {
    const WEEK: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(10_000) {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > WEEK);
        if old
            && entry
                .file_type()
                .is_ok_and(|t| t.is_dir() && !t.is_symlink())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// At SessionEnd the session's call numbers are no longer needed.
fn forget_calls(state_dir: &Path, session: &str) {
    let dir = calls_dir(state_dir, session);
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        let _ = std::fs::remove_dir_all(dir);
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

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "lsgm-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            fs::write(root.join("notes.txt"), "alpha\nbeta\n").unwrap();
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
                vendor: crate::install::Vendor::Gemini,
                user_level: false,
            }
        }

        fn event(&self, fixture: &str) -> Value {
            let mut event: Value = serde_json::from_str(fixture).unwrap();
            event["cwd"] = json!(self.0);
            if let Some(path) = event["tool_input"]["file_path"].as_str() {
                let name = Path::new(path).file_name().unwrap().to_owned();
                event["tool_input"]["file_path"] = json!(self.0.join(name));
            }
            event
        }

        fn parse(&self, event: &Value) -> Result<Operation, Reply> {
            let config = self.config();
            parse(
                serde_json::from_value(event.clone()).unwrap(),
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

    const BEFORE_REPLACE: &str =
        include_str!("../fixtures/gemini-cli-contract/before-replace.json");
    const AFTER_REPLACE: &str = include_str!("../fixtures/gemini-cli-contract/after-replace.json");
    const BEFORE_SHELL: &str = include_str!("../fixtures/gemini-cli-contract/before-shell.json");
    const AFTER_READ: &str = include_str!("../fixtures/gemini-cli-contract/after-read-file.json");

    #[test]
    fn contract_events_parse_into_what_the_adapter_reads() {
        for (name, body) in [
            (
                "session-start",
                include_str!("../fixtures/gemini-cli-contract/session-start.json"),
            ),
            (
                "before-agent",
                include_str!("../fixtures/gemini-cli-contract/before-agent.json"),
            ),
            ("before-replace", BEFORE_REPLACE),
            ("after-replace", AFTER_REPLACE),
            (
                "before-write",
                include_str!("../fixtures/gemini-cli-contract/before-write-file.json"),
            ),
            ("after-read", AFTER_READ),
            ("before-shell", BEFORE_SHELL),
            (
                "after-shell",
                include_str!("../fixtures/gemini-cli-contract/after-shell.json"),
            ),
            (
                "session-end",
                include_str!("../fixtures/gemini-cli-contract/session-end.json"),
            ),
        ] {
            let event: HostEvent = serde_json::from_str(body).expect(name);
            assert!(
                hook::valid_host_id(event.session_id.as_deref().unwrap()),
                "{name}"
            );
            assert!(
                Path::new(event.cwd.as_deref().unwrap()).is_absolute(),
                "{name}"
            );
        }
    }

    #[test]
    fn before_and_after_one_call_share_its_identity_and_scope() {
        let s = Scratch::new();
        let pre = s.parse(&s.event(BEFORE_REPLACE)).unwrap();
        let post = s.parse(&s.event(AFTER_REPLACE)).unwrap();
        assert_eq!(pre.source, "gemini");
        assert_eq!(pre.tool, "replace");
        assert_eq!(pre.paths, ["notes.txt"]);
        assert_eq!(pre.tool_use_id, post.tool_use_id);
        assert_eq!(pre.input_fingerprint, post.input_fingerprint);
        assert!(matches!(pre.phase, Phase::Pre));
        assert!(matches!(post.phase, Phase::Post(ToolResult::Succeeded)));
        assert_eq!(pre.edits.len(), 1, "a replace states what it changes");
        let ids = hook::identities_for(SOURCE, &pre.host_session, &pre.tool_use_id);
        assert_eq!(ids.session_id, format!("gemini.{}", pre.host_session));
        langspan_core::validate_request_id(&ids.session_id).unwrap();
    }

    #[test]
    fn an_identical_call_made_again_is_a_new_call() {
        let s = Scratch::new();
        let first = s.parse(&s.event(BEFORE_SHELL)).unwrap();
        let second = s.parse(&s.event(BEFORE_SHELL)).unwrap();
        assert_ne!(first.tool_use_id, second.tool_use_id);
        let mut after = s.event(BEFORE_SHELL);
        after["hook_event_name"] = json!("AfterTool");
        assert_eq!(s.parse(&after).unwrap().tool_use_id, second.tool_use_id);
        // Each completed read is its own call: there is no BeforeTool for it.
        let read = s.parse(&s.event(AFTER_READ)).unwrap();
        let again = s.parse(&s.event(AFTER_READ)).unwrap();
        assert!(matches!(read.phase, Phase::Read));
        assert_ne!(read.tool_use_id, again.tool_use_id);
        // After SessionEnd the session can be resumed under the same ID
        // (`gemini --resume`): its calls must not reuse earlier IDs.
        forget_calls(&s.config().state_dir, &first.host_session);
        let resumed = s.parse(&s.event(BEFORE_SHELL)).unwrap().tool_use_id;
        assert_ne!(resumed, first.tool_use_id);
        assert_ne!(resumed, second.tool_use_id);
        assert!(hook::valid_host_id(&resumed), "{resumed}");
    }

    #[test]
    fn a_dot_session_id_never_names_a_directory_outside_its_own() {
        let s = Scratch::new();
        let state = s.config().state_dir;
        fs::create_dir_all(state.join("gemini-calls")).unwrap();
        fs::write(state.join("store.sqlite"), "keep").unwrap();
        for session in [".", ".."] {
            let mut event = s.event(BEFORE_SHELL);
            event["session_id"] = json!(session);
            let _ = run(serde_json::to_vec(&event).unwrap().as_slice(), &s.config());
            event["hook_event_name"] = json!("SessionEnd");
            let _ = run(serde_json::to_vec(&event).unwrap().as_slice(), &s.config());
        }
        assert_eq!(
            fs::read_to_string(state.join("store.sqlite")).unwrap(),
            "keep"
        );
        assert!(state.join("gemini-calls").is_dir());
    }

    #[test]
    fn a_failed_edit_is_a_failure_and_a_failed_read_records_nothing() {
        let s = Scratch::new();
        s.parse(&s.event(BEFORE_REPLACE)).unwrap();
        let mut failed = s.event(AFTER_REPLACE);
        failed["tool_response"]["error"] = json!({"message": "Failed to edit, could not find the string to replace.", "type": "edit_no_occurrence_found"});
        assert!(matches!(
            s.parse(&failed).unwrap().phase,
            Phase::Post(ToolResult::Failed)
        ));
        let mut unread = s.event(AFTER_READ);
        unread["tool_response"]["error"] =
            json!({"message": "File not found.", "type": "file_not_found"});
        assert_eq!(s.parse(&unread).err().unwrap().coverage, "unsupported");
    }

    #[test]
    fn unsupported_tools_paths_and_events_get_an_empty_answer() {
        let s = Scratch::new();
        let mut other_tool = s.event(BEFORE_REPLACE);
        other_tool["tool_name"] = json!("read_many_files");
        let mut outside = s.event(BEFORE_REPLACE);
        outside["tool_input"]["file_path"] = json!("/elsewhere/notes.txt");
        let mut no_session = s.event(BEFORE_REPLACE);
        no_session["session_id"] = json!("not a valid id");
        let mut relative = s.event(BEFORE_REPLACE);
        relative["cwd"] = json!("relative");
        let mut before_read = s.event(AFTER_READ);
        before_read["hook_event_name"] = json!("BeforeTool");
        let mut model = s.event(BEFORE_REPLACE);
        model["hook_event_name"] = json!("BeforeModel");
        for event in [
            other_tool,
            outside,
            no_session,
            relative,
            before_read,
            model,
        ] {
            let reply = run(serde_json::to_vec(&event).unwrap().as_slice(), &s.config());
            assert_eq!(reply.coverage, "unsupported", "{event}: {}", reply.detail);
            assert_eq!(reply.stdout, "{}");
        }
        let malformed = run(&b"{broken"[..], &s.config());
        assert_eq!(
            (malformed.coverage, malformed.stdout.as_str()),
            ("unknown", "{}")
        );
        let expired = run_until(&b"{}"[..], &s.config(), Instant::now());
        assert_eq!(
            (expired.coverage, expired.stdout.as_str()),
            ("timeout", "{}")
        );
    }

    #[test]
    fn a_deny_is_gemini_shaped_and_context_keeps_its_field() {
        let holders = vec![langspan_core::Conflict {
            lease_id: "lease".into(),
            session_id: "claude-code.other".into(),
            paths: vec!["notes.txt".into()],
            regions: Vec::new(),
        }];
        let denied = answer(hook::deny(&holders, &[]));
        let value: Value = serde_json::from_str(&denied.stdout).unwrap();
        assert_eq!(value["decision"], "deny");
        assert!(
            value["reason"]
                .as_str()
                .unwrap()
                .contains("not instructions")
        );
        assert!(value.get("hookSpecificOutput").is_none());
        let context =
            r#"{"hookSpecificOutput":{"hookEventName":"BeforeAgent","additionalContext":"x"}}"#;
        let briefed = answer(Reply {
            stdout: context.into(),
            coverage: "briefed",
            detail: String::new(),
        });
        assert_eq!(briefed.stdout, context);
        assert!(!denied.stdout.contains("\"allow\""));
    }

    #[test]
    fn a_shell_call_runs_in_its_dir_path() {
        let s = Scratch::new();
        fs::create_dir(s.0.join("sub")).unwrap();
        let mut event = s.event(BEFORE_SHELL);
        event["tool_input"]["command"] = json!("echo x > out.txt");
        event["tool_input"]["dir_path"] = json!("sub");
        let operation = s.parse(&event).unwrap();
        assert_eq!(operation.paths, ["sub/out.txt"]);
        assert_eq!(operation.tool, hook::SHELL_TOOL);
    }
}
