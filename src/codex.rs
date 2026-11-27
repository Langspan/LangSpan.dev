//! Native Codex apply_patch hook adapter. Protocol fixtures are source-derived;
//! see fixtures/codex-contract/README.md for the live-host evidence boundary.

use crate::hook::{self, Config, Operation, Phase, Reply};
use langspan_core::ToolResult;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, io::Read, time::Instant};

/// The core names a Codex session by this prefix of its ID (issue #129).
pub const SOURCE: &str = langspan_core::CODEX_HOST;
pub use crate::hook::MAX_EVENT_BYTES;

const MAX_PATCH_BYTES: usize = 4 * 1024 * 1024;
const MAX_PATCH_PATHS: usize = 64;

#[derive(Deserialize)]
struct HostEvent {
    hook_event_name: String,
    session_id: Option<String>,
    tool_use_id: Option<String>,
    tool_name: Option<String>,
    agent_id: Option<String>,
    cwd: Option<String>,
    tool_input: Option<Value>,
    tool_response: Option<Value>,
}

fn host_identity(value: Option<String>) -> Result<String, Reply> {
    match value {
        Some(value) if hook::valid_host_id(&value) => Ok(value),
        _ => Err(Reply::none(
            "unsupported",
            "Codex event lacks a usable session or tool-call identity",
        )),
    }
}

fn parse_event(
    body: &[u8],
    config: &Config,
    deadline: Instant,
) -> Result<(Operation, Config), Reply> {
    let event: HostEvent = serde_json::from_slice(body)
        .map_err(|_| Reply::none("unknown", "malformed Codex hook JSON"))?;
    if event.tool_name.as_deref() == Some(hook::SHELL_TOOL) {
        return shell(event, config, deadline);
    }
    if event.tool_name.as_deref() != Some("apply_patch") {
        return Err(Reply::none(
            "unsupported",
            "Codex adapter supports only native apply_patch and Bash calls",
        ));
    }
    if event.agent_id.is_some() {
        return Err(Reply::none(
            "unsupported",
            "Codex subagent identity is not covered by this adapter",
        ));
    }
    let host_session = host_identity(event.session_id)?;
    let tool_use_id = host_identity(event.tool_use_id)?;
    let phase = match event.hook_event_name.as_str() {
        "PreToolUse" => Phase::Pre,
        "PostToolUse" => match event
            .tool_response
            .as_ref()
            .and_then(Value::as_str)
            .and_then(patch_result)
        {
            Some(result) => Phase::Post(result),
            // Do not infer success merely from a callback or an unfamiliar shape.
            None => {
                return Err(Reply::none(
                    "unknown",
                    "Codex post-tool result does not match a supported patch response",
                ));
            }
        },
        _ => {
            return Err(Reply::none(
                "unsupported",
                "Codex adapter supports PreToolUse and PostToolUse",
            ));
        }
    };
    let input = event
        .tool_input
        .ok_or_else(|| Reply::none("unsupported", "Codex event has no tool_input"))?;
    if !input
        .as_object()
        .is_some_and(|object| object.len() == 1 && object.contains_key("command"))
    {
        return Err(Reply::none(
            "unsupported",
            "Codex apply_patch input has fields outside the supported command contract",
        ));
    }
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Reply::none(
                "unsupported",
                "Codex apply_patch input has no string command",
            )
        })?;
    let raw_paths = patch_paths(command).map_err(|detail| Reply::none("unsupported", detail))?;
    let cwd = event
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .ok_or_else(|| Reply::none("unsupported", "Codex event has no working directory"))?;
    if !std::path::Path::new(cwd).is_absolute() {
        return Err(Reply::none(
            "unsupported",
            "Codex working directory must be absolute",
        ));
    }
    let config = hook::resolve_workspace(config, Some(cwd), deadline)?;
    let paths: Result<BTreeSet<_>, _> = raw_paths
        .iter()
        .map(|path| hook::file_key(&config.root, Some(cwd), path))
        .collect();
    let paths = paths.map_err(|detail| Reply::none("unsupported", detail))?;
    // A hunk set whose file cannot be keyed is simply held whole.
    let edits = patch_edits(command)
        .into_iter()
        .filter_map(|(path, edit)| {
            Some((hook::file_key(&config.root, Some(cwd), &path).ok()?, edit))
        })
        .collect();
    Ok((
        Operation {
            source: SOURCE.into(),
            host_session,
            tool_use_id,
            tool: "apply_patch".into(),
            paths: paths.into_iter().collect(),
            input_fingerprint: hook::input_fingerprint("apply_patch", &input),
            edits,
            phase,
            // Not sent to Codex until its handling of context on a completed
            // tool call has been observed on the host.
            mid_turn_context: false,
            // Codex subagent tool events are refused above: its documentation
            // gives a subagent the parent's session ID and does not say that
            // tool events carry the subagent's.
            parent: None,
            // Codex reads through shell commands, which no hook reports.
            snapshot_unknown: true,
            shell: None,
            contents: added_files(command)
                .into_iter()
                .filter_map(|(path, content)| {
                    Some((
                        hook::file_key(&config.root, Some(cwd), &path).ok()?,
                        content,
                    ))
                })
                .collect(),
            pre_tool_context: true,
        },
        config,
    ))
}

/// Codex names its shell tools' hook events `Bash`, with the command as
/// `tool_input.command`, and sends PostToolUse once the process has exited,
/// under the tool-use ID of the call that started it (`exec_command` and
/// `write_stdin` in `codex-rs/core/src/tools/handlers/unified_exec*`, tag
/// `rust-v0.171.0`). The response text is not read: what changed is found by
/// comparing the workspace.
fn shell(
    event: HostEvent,
    config: &Config,
    deadline: Instant,
) -> Result<(Operation, Config), Reply> {
    if event.agent_id.is_some() {
        return Err(Reply::none(
            "unsupported",
            "Codex subagent identity is not covered by this adapter",
        ));
    }
    let host_session = host_identity(event.session_id)?;
    let tool_use_id = host_identity(event.tool_use_id)?;
    let pre = match event.hook_event_name.as_str() {
        "PreToolUse" => true,
        "PostToolUse" => false,
        _ => {
            return Err(Reply::none(
                "unsupported",
                "Codex adapter supports PreToolUse and PostToolUse",
            ));
        }
    };
    let command = event
        .tool_input
        .as_ref()
        .and_then(|input| input.get("command"))
        .and_then(Value::as_str)
        .ok_or_else(|| Reply::none("unsupported", "Codex Bash input has no string command"))?;
    let cwd = event
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty() && std::path::Path::new(cwd).is_absolute())
        .ok_or_else(|| {
            Reply::none(
                "unsupported",
                "Codex event has no absolute working directory",
            )
        })?;
    let config = hook::resolve_workspace(config, Some(cwd), deadline)?;
    let operation = hook::shell_operation(
        SOURCE,
        &host_session,
        // Codex subagent events are refused above.
        None,
        &tool_use_id,
        command,
        Some(cwd),
        pre,
        // Codex reports a shell call's PostToolUse once its process exits.
        false,
        &config.root,
        false,
        // Its PreToolUse reply's `additionalContext` reaches the model, as
        // for `apply_patch`.
        true,
    );
    Ok((operation, config))
}

const PATCH_SUCCESS: &str = "Success. Updated the following files:\n";

/// Read the outcome of an `apply_patch` call from its post-tool response.
/// Two shapes are known: the bare success text, and the wrapped form real
/// Codex 0.171.0 sends (`Exit code: N`, `Wall time: ...`, `Output:`, then the
/// tool's output; see `fixtures/codex-0.171.0`). Anything else is unknown.
fn patch_result(text: &str) -> Option<ToolResult> {
    if text.starts_with(PATCH_SUCCESS) {
        return Some(ToolResult::Succeeded);
    }
    let code: i64 = text
        .strip_prefix("Exit code: ")?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()?;
    let output = text.split_once("\nOutput:\n").map(|(_, output)| output)?;
    match code {
        0 if output.starts_with(PATCH_SUCCESS) => Some(ToolResult::Succeeded),
        0 => None,
        _ => Some(ToolResult::Failed),
    }
}

/// Parse a bounded event supplied by a caller. The CLI bounds its actual stdin
/// read separately, then retains that original deadline through run_until.
pub fn run(input: impl Read, config: &Config) -> Reply {
    run_until(input, config, Instant::now() + config.budget)
}

pub fn run_until(input: impl Read, config: &Config, deadline: Instant) -> Reply {
    if Instant::now() >= deadline {
        return Reply::none("timeout", "Codex hook deadline elapsed before parsing");
    }
    let mut body = Vec::new();
    if input
        .take(MAX_EVENT_BYTES + 1)
        .read_to_end(&mut body)
        .is_err()
    {
        return Reply::none("unknown", "could not read Codex hook input");
    }
    if body.len() as u64 > MAX_EVENT_BYTES {
        return Reply::none("unsupported", "Codex event exceeds the 8 MiB input limit");
    }
    if let Some(reply) = hook::lifecycle_event(&body, SOURCE, config, deadline) {
        return reply;
    }
    let (operation, resolved) = match parse_event(&body, config, deadline) {
        Ok(parsed) => parsed,
        Err(reply) => return reply,
    };
    if Instant::now() >= deadline {
        return Reply::none("timeout", "Codex hook deadline elapsed while parsing");
    }
    hook::execute(operation, &resolved, deadline)
}

#[derive(Clone, Copy)]
enum Section {
    Add,
    Delete,
    Update {
        moved: bool,
        body: bool,
        ended: bool,
    },
}

fn finish_section(section: Option<Section>) -> Result<(), &'static str> {
    if matches!(section, Some(Section::Update { body: false, .. })) {
        return Err("empty apply_patch update is unsupported");
    }
    Ok(())
}

fn insert_path(paths: &mut BTreeSet<String>, path: &str) -> Result<(), &'static str> {
    if path.trim().is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
        return Err("apply_patch path is empty, too long, or contains control characters");
    }
    paths.insert(path.to_owned());
    if paths.len() > MAX_PATCH_PATHS {
        return Err("apply_patch names more than 64 paths; no partial admission");
    }
    Ok(())
}

/// Extract the complete resource set from the supported local patch grammar.
/// Unknown directives reject the whole patch; shell and remote forms are not
/// inferred. This parser does not read file content or attempt to apply a patch.
pub fn patch_paths(patch: &str) -> Result<Vec<String>, &'static str> {
    if patch.len() > MAX_PATCH_BYTES {
        return Err("apply_patch exceeds the 4 MiB supported patch limit");
    }
    let mut lines = patch.trim().lines().peekable();
    if lines.next().map(str::trim) != Some("*** Begin Patch") {
        return Err("apply_patch requires a literal Begin Patch envelope");
    }
    let mut section = None;
    let mut paths = BTreeSet::new();
    let mut ended = false;
    while let Some(line) = lines.next() {
        if lines.peek().is_none() && line.trim() == "*** End Patch" {
            finish_section(section)?;
            ended = true;
            break;
        }
        let header = [
            ("*** Add File: ", Section::Add),
            ("*** Delete File: ", Section::Delete),
            (
                "*** Update File: ",
                Section::Update {
                    moved: false,
                    body: false,
                    ended: false,
                },
            ),
        ]
        .into_iter()
        .find_map(|(prefix, next)| line.strip_prefix(prefix).map(|path| (path, next)));
        if let Some((path, next)) = header {
            finish_section(section)?;
            insert_path(&mut paths, path)?;
            section = Some(next);
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Move to: ") {
            match section.as_mut() {
                Some(Section::Update {
                    moved,
                    body: false,
                    ended: false,
                }) if !*moved => {
                    insert_path(&mut paths, path)?;
                    *moved = true;
                    continue;
                }
                _ => return Err("apply_patch move must precede the update body"),
            }
        }
        if line == "*** End of File" {
            match section.as_mut() {
                Some(Section::Update {
                    body: true, ended, ..
                }) if !*ended => {
                    *ended = true;
                    continue;
                }
                _ => return Err("apply_patch End of File is outside an update body"),
            }
        }
        if line.is_empty() && section.is_some() {
            continue;
        }
        match section.as_mut() {
            Some(Section::Add) if line.starts_with('+') => {}
            Some(Section::Update {
                body, ended: false, ..
            }) => {
                if line == "@@" || line.starts_with("@@ ") {
                    continue;
                }
                if line.starts_with(['+', '-', ' ']) {
                    *body = true;
                } else {
                    return Err("unrecognized apply_patch update body; no partial admission");
                }
            }
            _ => return Err("unrecognized apply_patch directive; no partial admission"),
        }
    }
    if !ended || paths.is_empty() {
        return Err("apply_patch requires a complete, nonempty patch");
    }
    Ok(paths.into_iter().collect())
}

/// The hunks of each `Update File` section of a patch that `patch_paths`
/// accepted. A file that is added, deleted, moved or named in more than one
/// section has no entry and is held whole.
pub fn patch_edits(patch: &str) -> Vec<(String, crate::regions::Edit)> {
    use crate::regions::{Edit, Hunk, HunkLine};
    let mut sections: Vec<(String, Option<Vec<Hunk>>)> = Vec::new();
    for line in patch.trim().lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            sections.push((path.to_owned(), Some(Vec::new())));
            continue;
        }
        if let Some(path) = ["*** Add File: ", "*** Delete File: "]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
        {
            sections.push((path.to_owned(), None));
            continue;
        }
        let Some((_, current @ Some(_))) = sections.last_mut() else {
            continue;
        };
        if line.starts_with("*** Move to: ") {
            *current = None;
            continue;
        }
        let Some(hunks) = current else {
            continue;
        };
        if line.starts_with("*** ") {
            continue;
        }
        if line == "@@" || line.starts_with("@@ ") || hunks.is_empty() {
            hunks.push(Hunk::default());
            if line.starts_with("@@") {
                continue;
            }
        }
        let Some(hunk) = hunks.last_mut() else {
            continue;
        };
        let (kind, text) = match line.chars().next() {
            Some('+') => (HunkLine::Added, &line[1..]),
            Some('-') => (HunkLine::Removed, &line[1..]),
            Some(' ') => (HunkLine::Context, &line[1..]),
            _ => (HunkLine::Context, line),
        };
        hunk.lines.push((kind, text.to_owned()));
    }
    let named = |path: &str| sections.iter().filter(|(other, _)| other == path).count();
    sections
        .iter()
        .filter(|(path, _)| named(path) == 1)
        .filter_map(|(path, hunks)| Some((path.clone(), Edit::Hunks(hunks.clone()?))))
        .collect()
}

/// The content of each `Add File` section of a patch that `patch_paths`
/// accepted: its `+` lines. A path named in more than one section has none.
fn added_files(patch: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, Option<String>)> = Vec::new();
    for line in patch.trim().lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            sections.push((path.to_owned(), Some(String::new())));
            continue;
        }
        if let Some(path) = ["*** Update File: ", "*** Delete File: "]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
        {
            sections.push((path.to_owned(), None));
            continue;
        }
        if line.starts_with("*** End Patch") {
            break;
        }
        // `*** Begin Patch`, and `*** Move to:` of an update.
        if line.starts_with("*** ") {
            continue;
        }
        if let Some((_, Some(content))) = sections.last_mut()
            && let Some(text) = line.strip_prefix('+')
        {
            content.push_str(text);
            content.push('\n');
        }
    }
    let named = |path: &str| sections.iter().filter(|(other, _)| other == path).count();
    sections
        .iter()
        .filter(|(path, _)| named(path) == 1)
        .filter_map(|(path, content)| Some((path.clone(), content.clone()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct ParseFixture(PathBuf);

    impl ParseFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "lscx-parser-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(fs::canonicalize(root).unwrap())
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
                vendor: crate::install::Vendor::Codex,
                user_level: false,
            }
        }

        fn event(&self, post: bool) -> Value {
            let fixture = if post {
                include_str!("../fixtures/codex-contract/post-apply-patch.json")
            } else {
                include_str!("../fixtures/codex-contract/pre-apply-patch.json")
            };
            let mut event: Value = serde_json::from_str(fixture).unwrap();
            event["cwd"] = serde_json::json!(self.0);
            event
        }
    }

    impl Drop for ParseFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn codex_retry_ids_match_independent_vectors_and_do_not_collide_with_claude() {
        // Independently calculated with Python hashlib and big-endian u64 lengths.
        let session = "00000000-0000-4000-8000-000000000001";
        let ids = hook::identities_for(SOURCE, session, "call_example");
        assert_eq!(ids.operation_id, "lsh1-op-f499381dc1577d650e005317bd5d5604");
        assert_eq!(
            ids.register,
            "lsh1-register-803f6e2e16f18067d8661ddfb883b96d"
        );
        assert_eq!(ids.admit, "lsh1-admit-d0a315ddc01821242d510eb916a32e34");
        assert_eq!(
            ids.complete,
            "lsh1-complete-6713294b883bf67b1150745f2070132a"
        );
        assert_eq!(
            hook::wire_id(
                "observe-post_tool_use",
                &[SOURCE, &ids.session_id, &ids.operation_id]
            ),
            "lsh1-observe-post_tool_use-71954e3eae43849f1d1d2186ba3bf67d"
        );
        assert_ne!(
            ids.admit,
            hook::identities_for("claude-code", session, "call_example").admit
        );
    }

    #[test]
    fn source_derived_pre_and_post_events_have_matching_scope_and_input_identity() {
        let f = ParseFixture::new();
        let config = f.config();
        let (pre, _) = parse_event(
            &serde_json::to_vec(&f.event(false)).unwrap(),
            &config,
            Instant::now() + config.budget,
        )
        .unwrap();
        let (post, _) = parse_event(
            &serde_json::to_vec(&f.event(true)).unwrap(),
            &config,
            Instant::now() + config.budget,
        )
        .unwrap();
        assert_eq!(pre.source, "codex");
        assert_eq!(pre.tool, "apply_patch");
        assert_eq!(pre.paths, ["a.txt", "new.txt"]);
        assert_eq!(pre.host_session, post.host_session);
        assert_eq!(pre.tool_use_id, post.tool_use_id);
        assert_eq!(pre.paths, post.paths);
        assert_eq!(pre.input_fingerprint, post.input_fingerprint);
        assert!(matches!(pre.phase, Phase::Pre));
        assert!(matches!(post.phase, Phase::Post(ToolResult::Succeeded)));
    }

    #[test]
    fn same_file_with_changed_patch_content_changes_the_command_fingerprint() {
        let f = ParseFixture::new();
        let config = f.config();
        let original = f.event(false);
        let mut changed = original.clone();
        changed["tool_input"]["command"] = serde_json::json!(
            original["tool_input"]["command"]
                .as_str()
                .unwrap()
                .replace("+new", "+different")
        );
        let (original, _) = parse_event(
            &serde_json::to_vec(&original).unwrap(),
            &config,
            Instant::now() + config.budget,
        )
        .unwrap();
        let (changed, _) = parse_event(
            &serde_json::to_vec(&changed).unwrap(),
            &config,
            Instant::now() + config.budget,
        )
        .unwrap();
        assert_eq!(original.paths, changed.paths);
        assert_ne!(original.input_fingerprint, changed.input_fingerprint);
    }

    #[test]
    fn missing_identity_unsupported_tools_and_any_outside_path_get_no_decision() {
        let f = ParseFixture::new();
        let mut no_id = f.event(false);
        no_id.as_object_mut().unwrap().remove("tool_use_id");
        let mut no_session = f.event(false);
        no_session["session_id"] = serde_json::json!("");
        // Bash is covered (tests/shell_writes.rs); other tools are not.
        let mut shell = f.event(false);
        shell["tool_name"] = serde_json::json!("spawn_agent");
        let mut outside = f.event(false);
        outside["tool_input"]["command"] = serde_json::json!(
            "*** Begin Patch\n*** Delete File: inside.txt\n*** Delete File: ../outside.txt\n*** End Patch"
        );
        let mut subagent = f.event(false);
        subagent["agent_id"] = serde_json::json!("agent-example");
        let mut relative_cwd = f.event(false);
        relative_cwd["cwd"] = serde_json::json!("relative");
        let mut unknown_input = f.event(false);
        unknown_input["tool_input"]["workdir"] = serde_json::json!("elsewhere");
        for event in [
            no_id,
            no_session,
            shell,
            outside,
            subagent,
            relative_cwd,
            unknown_input,
        ] {
            let reply = run(serde_json::to_vec(&event).unwrap().as_slice(), &f.config());
            assert_eq!(reply.coverage, "unsupported", "{}", reply.detail);
            assert!(reply.stdout.is_empty());
        }
    }

    #[test]
    fn unknown_post_results_do_not_become_successful_observations() {
        let f = ParseFixture::new();
        for response in [
            Value::Null,
            serde_json::json!({"success":true}),
            serde_json::json!("permission denied"),
            serde_json::json!(""),
        ] {
            let mut event = f.event(true);
            event["tool_response"] = response;
            let reply = run(serde_json::to_vec(&event).unwrap().as_slice(), &f.config());
            assert_eq!(reply.coverage, "unknown");
            assert!(reply.stdout.is_empty());
        }
    }

    #[test]
    fn malformed_oversized_and_expired_inputs_get_no_decision() {
        let f = ParseFixture::new();
        let malformed = run(&b"{broken"[..], &f.config());
        assert_eq!(malformed.coverage, "unknown");
        assert!(malformed.stdout.is_empty());
        let oversized = run(std::io::repeat(b'x').take(MAX_EVENT_BYTES + 1), &f.config());
        assert_eq!(oversized.coverage, "unsupported");
        assert!(oversized.stdout.is_empty());
        let expired = run_until(&b"{}"[..], &f.config(), Instant::now());
        assert_eq!(expired.coverage, "timeout");
        assert!(expired.stdout.is_empty());
    }

    #[test]
    fn an_inherited_hook_resolves_the_linked_worktree_from_the_event() {
        let f = ParseFixture::new();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .arg("-C")
                .arg(&f.0)
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
        fs::write(f.0.join("a.txt"), "old\n").unwrap();
        git(&["add", "a.txt"]);
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
        let linked = f.0.join("linked-parent").join("worktree");
        fs::create_dir(linked.parent().unwrap()).unwrap();
        git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().unwrap(),
        ]);
        // Cover both sibling worktrees and the common nested .worktrees layout.
        let install = f.0.join("main-anchor");
        git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            install.to_str().unwrap(),
        ]);
        let mut event = f.event(false);
        event["cwd"] = serde_json::json!(linked);
        for anchor in [install, f.0.clone()] {
            let mut config = f.config();
            config.root = anchor;
            let (operation, resolved) = parse_event(
                &serde_json::to_vec(&event).unwrap(),
                &config,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
            assert_eq!(resolved.root, linked);
            assert_ne!(resolved.workspace_id, config.workspace_id);
            assert_eq!(operation.paths, ["a.txt", "new.txt"]);
            assert_eq!(operation.source, "codex");
        }
    }

    #[test]
    fn patch_scope_includes_creations_deletions_and_both_sides_of_moves() {
        let patch = "*** Begin Patch\n*** Add File: new file.txt\n+created\n*** Delete File: old.txt\n*** Update File: before.rs\n*** Move to: after.rs\n@@\n-before\n+after\n*** End Patch";
        assert_eq!(
            patch_paths(patch).unwrap(),
            ["after.rs", "before.rs", "new file.txt", "old.txt"]
        );
    }

    #[test]
    fn update_sections_yield_hunks_and_other_sections_are_held_whole() {
        use crate::regions::{Edit, Hunk, HunkLine::*};
        let patch = "*** Begin Patch\n*** Update File: a.rs\n@@ fn a()\n x\n-y\n+z\n@@\n-p\n+q\n*** Add File: new.rs\n+n\n*** Update File: m.rs\n*** Move to: n.rs\n@@\n-o\n+p\n*** Delete File: d.rs\n*** Update File: twice.rs\n@@\n-a\n+b\n*** Update File: twice.rs\n@@\n-c\n+d\n*** End Patch";
        assert!(patch_paths(patch).is_ok());
        let hunk = |lines: &[(crate::regions::HunkLine, &str)]| Hunk {
            lines: lines
                .iter()
                .map(|(kind, text)| (*kind, (*text).into()))
                .collect(),
        };
        assert_eq!(
            patch_edits(patch),
            [(
                "a.rs".to_owned(),
                Edit::Hunks(vec![
                    hunk(&[(Context, "x"), (Removed, "y"), (Added, "z")]),
                    hunk(&[(Removed, "p"), (Added, "q")]),
                ])
            )]
        );
        // A first hunk without a header, an empty context line, CRLF.
        let bare = "*** Begin Patch\r\n*** Update File: b.rs\r\n-old\r\n\r\n+new\r\n*** End of File\r\n*** End Patch\r\n";
        assert_eq!(
            patch_edits(bare),
            [(
                "b.rs".to_owned(),
                Edit::Hunks(vec![hunk(&[
                    (Removed, "old"),
                    (Context, ""),
                    (Added, "new")
                ])])
            )]
        );
    }

    #[test]
    fn body_lines_that_look_like_headers_never_expand_scope() {
        for prefix in [" ", "+", "-"] {
            for marker in [
                "*** Add File: hidden.txt",
                "*** Delete File: hidden.txt",
                "*** Move to: hidden.txt",
                "*** End Patch",
            ] {
                let patch = format!(
                    "*** Begin Patch\n*** Update File: actual.txt\n@@\n{prefix}{marker}\n*** End Patch"
                );
                assert_eq!(patch_paths(&patch).unwrap(), ["actual.txt"], "{patch}");
            }
        }
    }

    #[test]
    fn unfamiliar_or_ambiguous_patch_shapes_are_unsupported_as_a_whole() {
        for patch in [
            "*** Begin Patch\n*** End Patch",
            "*** Begin Patch\n*** Update File: a.txt\n*** End Patch",
            "*** Begin Patch\n*** Delete File: \n*** End Patch",
            "*** Begin Patch\n*** Environment ID: remote\n*** Delete File: a.txt\n*** End Patch",
            "*** Begin Patch\n*** Workdir: elsewhere\n*** Delete File: a.txt\n*** End Patch",
            "*** Begin Patch\n*** Add File: a.txt\n*** Move to: b.txt\n*** End Patch",
            "*** Begin Patch\n*** Update File: a.txt\n+x\n*** Move to: b.txt\n*** End Patch",
            "*** Begin Patch\n*** Delete File: a.txt\nunparsed\n*** End Patch",
            "*** Begin Patch\n*** Add File: a.txt\n+x\n*** Rename File: b.txt\n*** End Patch",
            "*** Begin Patch\n*** Delete File: a.txt\n*** End Patch\ntrailer",
            "<<EOF\n*** Begin Patch\n*** Delete File: a.txt\n*** End Patch\nEOF",
        ] {
            assert!(patch_paths(patch).is_err(), "{patch}");
        }
    }

    #[test]
    fn scope_is_deduplicated_bounded_and_independent_of_file_order() {
        let mut patch = String::from("*** Begin Patch\n");
        for i in (0..64).rev() {
            patch.push_str(&format!("*** Delete File: f{i:02}.txt\n"));
        }
        patch.push_str("*** Delete File: f00.txt\n*** End Patch");
        let paths = patch_paths(&patch).unwrap();
        assert_eq!(paths.len(), 64);
        assert_eq!(paths.first().unwrap(), "f00.txt");
        assert_eq!(paths.last().unwrap(), "f63.txt");
        let oversized = patch.replace("*** End Patch", "*** Delete File: extra.txt\n*** End Patch");
        assert!(patch_paths(&oversized).is_err());
    }

    #[test]
    fn unicode_and_crlf_preserve_the_exact_named_resources() {
        let patch = "*** Begin Patch\r\n*** Update File: src/日本語.rs\r\n@@ fn sample()\r\n-old\r\n+new\r\n*** End of File\r\n*** End Patch\r\n";
        assert_eq!(patch_paths(patch).unwrap(), ["src/日本語.rs"]);
    }
}

#[cfg(test)]
mod captured_host_events {
    use super::*;

    #[test]
    fn real_codex_0_171_0_events_parse_into_what_the_adapter_reads() {
        for (name, body) in [
            (
                "pre",
                include_str!("../fixtures/codex-0.171.0/pre-apply-patch.json"),
            ),
            (
                "post",
                include_str!("../fixtures/codex-0.171.0/post-apply-patch.json"),
            ),
        ] {
            let event: HostEvent = serde_json::from_str(body).expect(name);
            assert_eq!(event.tool_name.as_deref(), Some("apply_patch"), "{name}");
            assert!(
                event.session_id.is_some() && event.tool_use_id.is_some(),
                "{name}"
            );
            let input = event.tool_input.unwrap();
            assert_eq!(
                patch_paths(input["command"].as_str().unwrap()).unwrap(),
                ["README.md"]
            );
        }
        let post: HostEvent = serde_json::from_str(include_str!(
            "../fixtures/codex-0.171.0/post-apply-patch.json"
        ))
        .unwrap();
        assert_eq!(
            patch_result(post.tool_response.unwrap().as_str().unwrap()),
            Some(ToolResult::Succeeded),
            "the wrapped response real Codex sends is a success"
        );
    }

    #[test]
    fn patch_results_are_read_strictly() {
        assert_eq!(
            patch_result("Success. Updated the following files:\nM a\n"),
            Some(ToolResult::Succeeded)
        );
        assert_eq!(
            patch_result("Exit code: 1\nWall time: 0 seconds\nOutput:\nerror: no match\n"),
            Some(ToolResult::Failed)
        );
        for unknown in [
            "",
            "Exit code: 0\nWall time: 0 seconds\nOutput:\nsomething else\n",
            "Exit code: zero\nOutput:\nSuccess. Updated the following files:\n",
            "Exit code: 0\nno output marker",
            "Done.",
        ] {
            assert_eq!(patch_result(unknown), None, "{unknown:?}");
        }
    }
}
