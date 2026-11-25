//! Show or apply native hook registrations, preserving other settings.
//!
//! Generated commands carry an explicit shell-comment ownership marker. The
//! previous, unmarked format is accepted only in the exact generated shape with
//! an executable named `langspan`; other unmarked commands remain user-owned.
//! Settings-file symlinks are refused rather than silently replaced.

use serde_json::{Map, Value, json};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub const EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "PostToolUseFailure"];
/// `Bash` is matched so shell commands are admitted when their write set is
/// literal and compared before and after ([ADR 0011](../docs/adr/0011-shell-writes.md)).
pub const MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Bash";
/// After the tool ran, a completed Read is also reported, so the version a
/// session saw can be compared when it later edits the file.
pub const POST_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Bash|Read";
pub const CODEX_EVENTS: [&str; 2] = ["PreToolUse", "PostToolUse"];
pub const CODEX_MATCHER: &str = "^(apply_patch|Bash)$";
pub const GEMINI_EVENTS: [&str; 2] = ["BeforeTool", "AfterTool"];
/// Gemini CLI matches tool hooks by regular expression on the tool name.
pub const GEMINI_MATCHER: &str = "^(write_file|replace|run_shell_command)$";
pub const GEMINI_POST_MATCHER: &str = "^(write_file|replace|run_shell_command|read_file)$";
/// Gemini CLI names each hook; the name is shown in `/hooks` and is what
/// `hooksConfig.disabled` lists.
pub const GEMINI_HOOK_NAME: &str = "langspan";
/// Cursor's tool hooks, matched by tool type
/// (<https://cursor.com/docs/agent/hooks>, "Matcher"). `afterFileEdit`,
/// `beforeShellExecution` and `afterShellExecution` are not used: per the
/// documentation they carry no tool-call ID to join a call's two halves.
pub const CURSOR_EVENTS: [&str; 3] = ["preToolUse", "postToolUse", "postToolUseFailure"];
pub const CURSOR_MATCHER: &str = "^(Write|Delete|Shell)$";
pub const CURSOR_POST_MATCHER: &str = "^(Write|Delete|Shell|Read)$";
const CURSOR_PROJECT_MARK: &str = "LANGSPAN_MANAGED=cursor:v1 ";
const CURSOR_USER_MARK: &str = "LANGSPAN_MANAGED=cursor:user:v1 ";
/// The `hooks.json` schema version Cursor documents ("use `1`").
pub const CURSOR_CONFIG_VERSION: u64 = 1;
/// Host hook timeout in seconds; the adapter's own budget is shorter.
pub const HOST_TIMEOUT_SECONDS: u64 = 5;
/// Codex allows a SessionEnd hook at most 3 seconds (its hooks
/// documentation, "Config shape"). The adapter's budget is 1.5 seconds.
pub const CODEX_SESSION_END_TIMEOUT_SECONDS: u64 = 3;
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    ClaudeCode,
    Codex,
    Cursor,
    Gemini,
}

impl Vendor {
    /// Every host, in the order help text and checks list them.
    pub const ALL: [Self; 4] = [Self::ClaudeCode, Self::Codex, Self::Cursor, Self::Gemini];

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini-cli",
        }
    }

    /// The host a command-line name stands for.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|vendor| vendor.name() == name)
    }

    /// Cursor lists hook entries directly under each event. Claude Code,
    /// Codex and Gemini CLI nest them in groups that carry the matcher.
    fn flat(self) -> bool {
        self == Self::Cursor
    }

    fn events(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &EVENTS,
            Self::Codex => &CODEX_EVENTS,
            Self::Cursor => &CURSOR_EVENTS,
            Self::Gemini => &GEMINI_EVENTS,
        }
    }

    /// Session events with no tool matcher: discovery and peer briefings,
    /// the end of a reply and of a tool batch ([ADR 0027](../docs/adr/0027-turn-and-batch-events.md)).
    /// Codex has no StopFailure or PostToolBatch, and its SubagentStop names
    /// a subagent whose tool events carry the parent's session ID, which this
    /// adapter does not cover. Cursor's and Gemini CLI's end-of-reply events
    /// are not registered until they are checked on a real host.
    fn lifecycle_events(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &[
                "SessionStart",
                "UserPromptSubmit",
                "SessionEnd",
                "Stop",
                "StopFailure",
                "SubagentStop",
                "PostToolBatch",
            ],
            Self::Codex => &["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"],
            Self::Cursor => &["sessionStart", "beforeSubmitPrompt", "sessionEnd"],
            Self::Gemini => &["SessionStart", "BeforeAgent", "SessionEnd"],
        }
    }

    /// Every event an install registers, in the order it writes them.
    pub fn hook_events(self) -> Vec<&'static str> {
        self.events()
            .iter()
            .chain(self.lifecycle_events())
            .copied()
            .collect()
    }

    /// The host's timeout for one event's hook, in seconds.
    fn timeout(self, event: &str) -> u64 {
        match self {
            Self::Codex if event == "SessionEnd" => CODEX_SESSION_END_TIMEOUT_SECONDS,
            _ => HOST_TIMEOUT_SECONDS,
        }
    }

    fn matcher(self, event: &str) -> &'static str {
        match self {
            Self::ClaudeCode if event == "PostToolUse" => POST_MATCHER,
            Self::ClaudeCode => MATCHER,
            Self::Codex => CODEX_MATCHER,
            Self::Cursor if event == "postToolUse" => CURSOR_POST_MATCHER,
            Self::Cursor => CURSOR_MATCHER,
            Self::Gemini if event == "AfterTool" => GEMINI_POST_MATCHER,
            Self::Gemini => GEMINI_MATCHER,
        }
    }

    // A shell comment stays inside the host's supported command string schema
    // and does not become an argument. It survives executable relocations.
    // Not for Cursor: see `mark`.
    fn marker(self) -> &'static str {
        match self {
            Self::ClaudeCode => " # langspan-managed:claude-code:v1",
            Self::Codex => " # langspan-managed:codex:v1",
            Self::Cursor => " # langspan-managed:cursor:v1",
            Self::Gemini => " # langspan-managed:gemini-cli:v1",
        }
    }

    /// The user-level form has its own marker, so a project install and a
    /// user install are never mistaken for one another.
    fn user_marker(self) -> &'static str {
        match self {
            Self::ClaudeCode => " # langspan-managed:claude-code:user:v1",
            Self::Codex => " # langspan-managed:codex:user:v1",
            Self::Cursor => " # langspan-managed:cursor:user:v1",
            Self::Gemini => " # langspan-managed:gemini-cli:user:v1",
        }
    }

    /// The generated command with its ownership marker. Cursor's CLI appends
    /// the event to the command as a heredoc (`COMMAND <<'CURSOR_HOOK_EOF'`),
    /// so a trailing comment would swallow the redirection and leave the
    /// event to run as shell code. Its marker is a leading variable
    /// assignment instead, which the shell sets and nothing reads.
    fn mark(self, body: &str, user: bool) -> String {
        match (self, user) {
            (Self::Cursor, false) => format!("{CURSOR_PROJECT_MARK}{body}"),
            (Self::Cursor, true) => format!("{CURSOR_USER_MARK}{body}"),
            (_, false) => format!("{body}{}", self.marker()),
            (_, true) => format!("{body}{}", self.user_marker()),
        }
    }

    /// The generated body of a marked command, if it carries this marker.
    fn unmark(self, command: &str, user: bool) -> Option<&str> {
        match (self, user) {
            (Self::Cursor, false) => command.strip_prefix(CURSOR_PROJECT_MARK),
            (Self::Cursor, true) => command.strip_prefix(CURSOR_USER_MARK),
            (_, false) => command.strip_suffix(self.marker()),
            (_, true) => command.strip_suffix(self.user_marker()),
        }
    }

    /// Project files the host reads hooks from, relative to a project root.
    pub fn project_hook_files(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &[".claude/settings.local.json", ".claude/settings.json"],
            Self::Codex => &[".codex/hooks.json"],
            Self::Cursor => &[".cursor/hooks.json"],
            Self::Gemini => &[".gemini/settings.json"],
        }
    }

    /// Which project hooks the host runs for a session started in a linked
    /// worktree that has a LangSpan install of its own, as far as it has
    /// been observed. ADR 0003 records each rule and its evidence.
    pub fn worktree_hooks(self) -> WorktreeHooks {
        match self {
            // Claude Code 2.1.330 on macOS (launch validation): the
            // worktree's own project hooks and the main checkout's local
            // settings both run.
            Self::ClaudeCode => WorktreeHooks::Both,
            // Codex 0.171.0 on macOS (issue #2): only the main checkout's
            // project hooks run; the worktree's own never do.
            Self::Codex => WorktreeHooks::MainOnly,
            // No run in a linked worktree has been retained.
            Self::Cursor | Self::Gemini => WorktreeHooks::Unconfirmed,
        }
    }

    /// The host and version a `worktree_hooks` rule was observed on.
    pub fn worktree_hooks_observed(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some("Claude Code 2.1.330"),
            Self::Codex => Some("Codex 0.171.0"),
            Self::Cursor | Self::Gemini => None,
        }
    }

    /// One hook entry running `command` for `event`. Gemini CLI takes its
    /// timeout in milliseconds and names each hook; the other hosts take
    /// seconds.
    fn hook_entry(self, event: &str, command: String) -> Map<String, Value> {
        let mut entry = Map::new();
        if self == Self::Gemini {
            entry.insert("name".into(), GEMINI_HOOK_NAME.into());
        }
        entry.insert("type".into(), "command".into());
        entry.insert("command".into(), command.into());
        let timeout = match self {
            Self::Gemini => self.timeout(event) * 1000,
            _ => self.timeout(event),
        };
        entry.insert("timeout".into(), timeout.into());
        entry
    }
}

/// What a host runs for a session started in a linked worktree that has its
/// own LangSpan project install. The main checkout's hook steps aside only
/// for `Both`; otherwise it decides, so no event goes undecided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeHooks {
    /// The worktree's own hooks and the main checkout's both run.
    Both,
    /// Only the main checkout's hooks run.
    MainOnly,
    /// Not observed on a real host.
    Unconfirmed,
}

pub struct Registration {
    pub executable: PathBuf,
    pub state_dir: PathBuf,
    pub workspace_id: String,
    pub root: PathBuf,
    /// False adds `--prompt-summary off` to the installed command.
    pub share_prompt: bool,
    /// False adds `--code-briefing off` to the installed command.
    pub code_briefing: bool,
}

pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The executable path written into hooks and MCP entries: the running
/// binary's canonical path, unless that is a versioned Homebrew keg
/// (`PREFIX/Cellar/FORMULA/VERSION/bin/NAME`), which `brew upgrade` deletes.
/// Then the first of `PREFIX/opt/FORMULA/bin/NAME` and `PREFIX/bin/NAME`
/// that exists and resolves to this same binary is written instead, so the
/// entries keep working across upgrades. Neither qualifying keeps the
/// canonical path.
pub fn installed_executable(current: &Path) -> io::Result<PathBuf> {
    let canonical = fs::canonicalize(current)?;
    Ok(homebrew_stable_path(&canonical).unwrap_or(canonical))
}

fn homebrew_stable_path(canonical: &Path) -> Option<PathBuf> {
    let name = canonical.file_name()?;
    let bin = canonical.parent()?;
    let formula = bin.parent()?.parent()?;
    let cellar = formula.parent()?;
    if bin.file_name()? != "bin" || cellar.file_name()? != "Cellar" {
        return None;
    }
    let prefix = cellar.parent()?;
    [
        prefix.join("opt").join(formula.file_name()?).join("bin"),
        prefix.join("bin"),
    ]
    .into_iter()
    .map(|dir| dir.join(name))
    .find(|candidate| fs::canonicalize(candidate).is_ok_and(|resolved| resolved == canonical))
}

impl Registration {
    pub fn command(&self) -> String {
        self.command_for(Vendor::ClaudeCode)
    }

    pub fn command_for(&self, vendor: Vendor) -> String {
        vendor.mark(&self.command_body(vendor), false)
    }

    fn command_body(&self, vendor: Vendor) -> String {
        let mut words = vec![
            quote(&self.executable.to_string_lossy()),
            "hook".into(),
            vendor.name().into(),
            "--state-dir".into(),
            quote(&self.state_dir.to_string_lossy()),
            "--workspace".into(),
            quote(&self.workspace_id),
            "--root".into(),
            quote(&self.root.to_string_lossy()),
        ];
        if !self.share_prompt {
            words.extend(["--prompt-summary".into(), "off".into()]);
        }
        if !self.code_briefing {
            words.extend(["--code-briefing".into(), "off".into()]);
        }
        words.join(" ")
    }
}

/// A user-level registration: no workspace and no root. The hook resolves
/// both from each event's working directory.
pub struct UserRegistration {
    pub executable: PathBuf,
    pub state_dir: PathBuf,
    pub share_prompt: bool,
}

impl UserRegistration {
    pub fn command_for(&self, vendor: Vendor) -> String {
        vendor.mark(&self.command_body(vendor), true)
    }

    fn command_body(&self, vendor: Vendor) -> String {
        let mut words = vec![
            quote(&self.executable.to_string_lossy()),
            "hook".into(),
            vendor.name().into(),
            "--state-dir".into(),
            quote(&self.state_dir.to_string_lossy()),
        ];
        if !self.share_prompt {
            words.extend(["--prompt-summary".into(), "off".into()]);
        }
        words.join(" ")
    }
}

/// The user-level registration a hook entry carries, if LangSpan wrote it.
/// Only the marked, exactly generated form is ours.
pub fn parse_user_hook(vendor: Vendor, hook: &Value) -> Option<UserRegistration> {
    if hook.get("type").and_then(Value::as_str) != Some("command") {
        return None;
    }
    let command = hook.get("command").and_then(Value::as_str)?;
    let body = vendor.unmark(command, true)?;
    let words = words(body)?;
    let share_prompt = match &words[..] {
        [_, _, _, _, _] => true,
        [_, _, _, _, _, option, value] if option == "--prompt-summary" && value == "off" => false,
        _ => return None,
    };
    if words[1] != "hook" || words[2] != vendor.name() || words[3] != "--state-dir" {
        return None;
    }
    let parsed = UserRegistration {
        executable: words[0].clone().into(),
        state_dir: words[4].clone().into(),
        share_prompt,
    };
    (parsed.command_body(vendor) == body).then_some(parsed)
}

pub fn is_user_managed_for(vendor: Vendor, hook: &Value) -> bool {
    parse_user_hook(vendor, hook).is_some()
}

/// Every hook entry in a settings value that `ours` recognises: inside a
/// group's `hooks` (Claude Code, Codex) or listed directly (Cursor).
pub fn managed_hooks(settings: &Value, ours: impl Fn(&Value) -> bool) -> Vec<Value> {
    settings["hooks"]
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(_, groups)| groups.as_array().into_iter().flatten())
        .flat_map(entries_of)
        .filter(|hook| ours(hook))
        .cloned()
        .collect()
}

/// The hook entries a group holds, or the entry itself when it is listed
/// directly under its event (Cursor).
fn entries_of(group: &Value) -> Vec<&Value> {
    match group.get("hooks").and_then(Value::as_array) {
        Some(entries) => entries.iter().collect(),
        None => vec![group],
    }
}

/// The events an install registers that hold none of `ours` in `settings`:
/// left out by an install from an earlier version, or removed by hand.
pub fn missing_events(
    vendor: Vendor,
    settings: &Value,
    ours: impl Fn(&Value) -> bool,
) -> Vec<&'static str> {
    vendor
        .hook_events()
        .into_iter()
        .filter(|event| {
            !settings["hooks"][*event]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(entries_of)
                .any(&ours)
        })
        .collect()
}

/// Split the single-quoted form this module writes. Other forms are not ours.
fn words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut in_word = false;
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => current.push(c),
                    }
                }
            }
            '\\' if in_word || chars.peek() == Some(&'\'') => {
                in_word = true;
                current.push(chars.next()?);
            }
            ' ' => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Some(words)
}

/// The executable a project-level command LangSpan wrote runs.
pub fn executable_of(vendor: Vendor, command: &str) -> Option<PathBuf> {
    let body = vendor.unmark(command, false).unwrap_or(command);
    words(body)?.into_iter().next().map(PathBuf::from)
}

pub fn is_managed(hook: &Value) -> bool {
    is_managed_for(Vendor::ClaudeCode, hook)
}

pub fn is_managed_for(vendor: Vendor, hook: &Value) -> bool {
    parse_project_hook(vendor, hook).is_some()
}

/// The project-level registration a hook entry carries, if LangSpan wrote
/// it. Only the exactly generated form is ours.
pub fn parse_project_hook(vendor: Vendor, hook: &Value) -> Option<Registration> {
    if hook.get("type").and_then(Value::as_str) != Some("command") {
        return None;
    }
    let command = hook.get("command").and_then(Value::as_str)?;
    let (body, marked) = match vendor.unmark(command, false) {
        Some(body) => (body, true),
        None => (command, false),
    };
    let words = words(body)?;
    if words.len() < 9 {
        return None;
    }
    // Only the opt-outs this module writes, each at most once. Rebuilding
    // the command below also requires the order it writes them in.
    let (mut share_prompt, mut code_briefing) = (true, true);
    let mut options = &words[9..];
    while let [option, value, rest @ ..] = options {
        match (option.as_str(), value.as_str()) {
            ("--prompt-summary", "off") if share_prompt => share_prompt = false,
            ("--code-briefing", "off") if code_briefing => code_briefing = false,
            _ => return None,
        }
        options = rest;
    }
    if !options.is_empty()
        || words[1] != "hook"
        || words[2] != vendor.name()
        || words[3] != "--state-dir"
        || words[5] != "--workspace"
        || words[7] != "--root"
    {
        return None;
    }
    let parsed = Registration {
        executable: words[0].clone().into(),
        state_dir: words[4].clone().into(),
        workspace_id: words[6].clone(),
        root: words[8].clone().into(),
        share_prompt,
        code_briefing,
    };
    let ours = parsed.command_body(vendor) == body
        && (marked
            || vendor == Vendor::ClaudeCode
                && Path::new(&words[0]).file_name().and_then(|n| n.to_str()) == Some("langspan"));
    ours.then_some(parsed)
}

/// The project-level registrations for `vendor` in the hook files at `dir`.
/// A file that cannot be read safely is an error, never a guess.
pub fn project_registrations(
    vendor: Vendor,
    dir: &Path,
) -> Result<Vec<Registration>, crate::Error> {
    let mut found = Vec::new();
    for file in vendor.project_hook_files() {
        let Some(settings) = read(&dir.join(file))? else {
            continue;
        };
        found.extend(
            managed_hooks(&settings, |hook| is_managed_for(vendor, hook))
                .iter()
                .filter_map(|hook| parse_project_hook(vendor, hook)),
        );
    }
    Ok(found)
}

fn remove_managed(
    vendor: Vendor,
    settings: &mut Map<String, Value>,
    prune_empty: bool,
    ours: &dyn Fn(&Value) -> bool,
) -> usize {
    let mut removed = 0;
    let Some(Value::Object(hooks)) = settings.get_mut("hooks") else {
        return 0;
    };
    for event in vendor.hook_events() {
        let Some(Value::Array(groups)) = hooks.get_mut(event) else {
            continue;
        };
        let before_event = removed;
        if vendor.flat() {
            let before = groups.len();
            groups.retain(|hook| !ours(hook));
            removed += before - groups.len();
        }
        groups.retain_mut(|group| {
            if let Some(Value::Array(entries)) = group.get_mut("hooks") {
                let before = entries.len();
                entries.retain(|hook| !ours(hook));
                let changed = before - entries.len();
                removed += changed;
                // An unrelated empty group belongs to the user. Only remove a
                // group made empty by removing one of our own entries.
                return changed == 0 || !entries.is_empty();
            }
            true
        });
        if prune_empty && removed > before_event && groups.is_empty() {
            hooks.shift_remove(event);
        }
    }
    if prune_empty && removed > 0 && hooks.is_empty() {
        // Cursor's `version` stays: LangSpan cannot tell whether it added it.
        // A user-level file left holding only it is removed by `uninstall`
        // (`json_left_empty`).
        settings.shift_remove("hooks");
    }
    removed
}

/// Return the settings with LangSpan's entries replaced (install) or removed.
pub fn plan(existing: Option<Value>, registration: Option<&Registration>) -> Result<Value, String> {
    plan_for(Vendor::ClaudeCode, existing, registration)
}

/// Plan one host's registration without changing another host's entries or
/// trust configuration. Codex inherits project trust from the native host.
pub fn plan_for(
    vendor: Vendor,
    existing: Option<Value>,
    registration: Option<&Registration>,
) -> Result<Value, String> {
    plan_hooks(
        vendor,
        existing,
        registration.map(|r| r.command_for(vendor)),
        &|hook| is_managed_for(vendor, hook),
    )
}

/// The same plan for the host's user-level file. Only user-level entries
/// are replaced or removed; anything else in the file is left as it is.
pub fn plan_user_for(
    vendor: Vendor,
    existing: Option<Value>,
    registration: Option<&UserRegistration>,
) -> Result<Value, String> {
    plan_hooks(
        vendor,
        existing,
        registration.map(|r| r.command_for(vendor)),
        &|hook| is_user_managed_for(vendor, hook),
    )
}

fn plan_hooks(
    vendor: Vendor,
    existing: Option<Value>,
    command: Option<String>,
    ours: &dyn Fn(&Value) -> bool,
) -> Result<Value, String> {
    let mut settings = match existing {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("settings file must contain a JSON object".into()),
    };
    if let Some(hooks) = settings.get("hooks")
        && !hooks.is_object()
    {
        return Err("settings `hooks` must be a JSON object; nothing was changed".into());
    }
    if vendor == Vendor::Gemini
        && let Some(command) = &command
        && command.contains('$')
    {
        return Err(unexpandable(command));
    }
    // Retain existing key positions when replacing a registration. Removal
    // prunes only containers emptied by our own entries, using ordered removal.
    remove_managed(vendor, &mut settings, command.is_none(), ours);
    if let Some(command) = command {
        if vendor.flat() && !settings.contains_key("version") {
            settings.insert("version".into(), json!(CURSOR_CONFIG_VERSION));
        }
        let hooks = settings
            .entry("hooks")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or("settings `hooks` must be a JSON object")?;
        let tool_events = vendor.events().len();
        for (index, event) in vendor.hook_events().into_iter().enumerate() {
            let groups = hooks
                .entry(event)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| format!("settings `hooks.{event}` must be an array"))?;
            let mut entry = vendor.hook_entry(event, command.clone());
            // Cursor puts the matcher on the entry; the others on its group.
            let mut group = Map::new();
            let holder = if vendor.flat() {
                &mut entry
            } else {
                &mut group
            };
            if index < tool_events {
                holder.insert("matcher".into(), vendor.matcher(event).into());
            }
            if vendor.flat() {
                groups.push(Value::Object(entry));
            } else {
                group.insert("hooks".into(), Value::Array(vec![Value::Object(entry)]));
                groups.push(Value::Object(group));
            }
        }
    }
    Ok(Value::Object(settings))
}

/// Gemini CLI replaces `$NAME` and `${NAME}` in every string of its
/// settings, and `$GEMINI_PROJECT_DIR` and its like again in a hook command,
/// before it runs anything: a path with `$` in it would name another file.
pub fn unexpandable(text: &str) -> String {
    format!(
        "Gemini CLI would expand the `$` in {text:?} as a variable; install LangSpan and its state directory at paths without `$`. Nothing was changed"
    )
}

fn settings_metadata(path: &Path) -> Result<Option<fs::Metadata>, crate::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => Err(format!(
            "refusing settings symlink {}; choose its intended target explicitly with --settings; nothing was changed",
            path.display()
        )
        .into()),
        Ok(metadata) if !metadata.is_file() => Err(format!(
            "settings path {} is not a regular file; nothing was changed",
            path.display()
        )
        .into()),
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect {}: {error}", path.display()).into()),
    }
}

pub fn read(path: &Path) -> Result<Option<Value>, crate::Error> {
    parse_json(path, read_bytes(path)?.as_deref())
}

/// `bytes` read from `path` as JSON.
pub fn parse_json(path: &Path, bytes: Option<&[u8]>) -> Result<Option<Value>, crate::Error> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_slice(bytes).map_err(|error| {
        format!(
            "{} is not valid JSON ({error}); nothing was changed",
            path.display()
        )
    })?))
}

/// Read a configuration file that is not JSON, with the same refusals.
pub fn read_text(path: &Path) -> Result<Option<String>, crate::Error> {
    parse_text(path, read_bytes(path)?.as_deref())
}

/// `bytes` read from `path` as UTF-8 text.
pub fn parse_text(path: &Path, bytes: Option<&[u8]>) -> Result<Option<String>, crate::Error> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    Ok(Some(String::from_utf8(bytes.to_vec()).map_err(|_| {
        format!("{} is not UTF-8; nothing was changed", path.display())
    })?))
}

/// The file's exact bytes, with the symlink, type and size refusals.
pub fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, crate::Error> {
    if settings_metadata(path)?.is_none() {
        return Ok(None);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display()).into()),
    };
    if !file.metadata()?.is_file() {
        return Err(format!("settings path {} is not a regular file", path.display()).into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(format!("{} exceeds 1 MiB; nothing was changed", path.display()).into());
    }
    Ok(Some(bytes))
}

/// The bytes LangSpan writes for a planned JSON file.
pub fn json_bytes(value: &Value) -> Result<Vec<u8>, crate::Error> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// A file's new content, planned from what it holds now (`None`: absent).
pub type Planner<'a> = dyn Fn(Option<&[u8]>) -> Result<Vec<u8>, crate::Error> + 'a;

/// Replace the file atomically with `plan(from)`, keeping an existing file's
/// mode; new files are 0600. `from` is what the file held when the plan was
/// shown. Just before the rename the file is read again: a host such as
/// Claude Code may have rewritten it meanwhile, and that change is never
/// overwritten. The plan is made once more against the new content; if the
/// file changes again, nothing is written and an error says so. Returns
/// false when the new content already matched, so nothing was written.
///
/// What is left is the moment between that last read and the rename. With
/// no lock that the hosts honour, it can be made short but not closed.
pub fn write_planned(
    path: &Path,
    from: Option<&[u8]>,
    plan: &Planner,
) -> Result<bool, crate::Error> {
    write_planned_with(path, from, plan, &mut || {})
}

fn write_planned_with(
    path: &Path,
    from: Option<&[u8]>,
    plan: &Planner,
    before_rename: &mut dyn FnMut(),
) -> Result<bool, crate::Error> {
    let bytes = plan(from)?;
    let now = match write_bytes(path, from, &bytes, before_rename)? {
        Written::Replaced => return Ok(true),
        Written::Changed(now) => now,
    };
    let bytes = plan(now.as_deref())?;
    if now.as_deref() == Some(bytes.as_slice()) {
        return Ok(false);
    }
    match write_bytes(path, now.as_deref(), &bytes, before_rename)? {
        Written::Replaced => Ok(true),
        Written::Changed(_) => Err(format!(
            "{} changed while LangSpan was writing it, twice; nothing was written. Close the program that keeps changing it (for example Claude Code) and run this again",
            path.display()
        )
        .into()),
    }
}

/// Whether a JSON file LangSpan may create holds nothing once LangSpan's
/// entries are removed: an empty object, or for Cursor's `hooks.json` only
/// the `version` install puts in a new one. Such a file changes nothing for
/// its host, so `uninstall` removes it rather than leave `{}` behind.
pub fn json_left_empty(vendor: Vendor, bytes: &[u8]) -> bool {
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    map.is_empty()
        || vendor.flat()
            && map.len() == 1
            && map.get("version") == Some(&json!(CURSOR_CONFIG_VERSION))
}

/// Which planned bytes leave a file holding nothing, so it is removed.
pub type LeftEmpty<'a> = dyn Fn(&[u8]) -> bool + 'a;

/// What `write_or_remove_planned` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    Written,
    Removed,
    /// The file already held the planned content (or was already absent).
    Unchanged,
}

/// `write_planned`, except that a planned content for which `empty` holds
/// removes the file instead of writing it. The file is removed only while
/// it still holds the bytes the plan was made from; if it changed, the plan
/// is made again against the change, and a second change refuses with
/// nothing removed. As with the rename, the moment between the last read
/// and the removal can be made short but not closed.
pub fn write_or_remove_planned(
    path: &Path,
    from: Option<&[u8]>,
    plan: &Planner,
    empty: &LeftEmpty,
) -> Result<Applied, crate::Error> {
    write_or_remove_with(path, from, plan, empty, &mut || {})
}

fn write_or_remove_with(
    path: &Path,
    from: Option<&[u8]>,
    plan: &Planner,
    empty: &LeftEmpty,
    before_change: &mut dyn FnMut(),
) -> Result<Applied, crate::Error> {
    let mut from = from.map(<[u8]>::to_vec);
    for _ in 0..2 {
        if !empty(&plan(from.as_deref())?) {
            return Ok(
                match write_planned_with(path, from.as_deref(), plan, before_change)? {
                    true => Applied::Written,
                    false => Applied::Unchanged,
                },
            );
        }
        before_change();
        let now = read_bytes(path)?;
        if now == from {
            if now.is_none() {
                return Ok(Applied::Unchanged);
            }
            fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
            return Ok(Applied::Removed);
        }
        from = now;
    }
    Err(format!(
        "{} changed while LangSpan was removing it, twice; nothing was removed. Close the program that keeps changing it and run this again",
        path.display()
    )
    .into())
}

enum Written {
    Replaced,
    /// The file no longer held what was expected; this is what it holds.
    Changed(Option<Vec<u8>>),
}

#[cfg(unix)]
fn write_bytes(
    path: &Path,
    from: Option<&[u8]>,
    bytes: &[u8],
    before_rename: &mut dyn FnMut(),
) -> Result<Written, crate::Error> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let parent = path.parent().ok_or("settings path has no parent")?;
    let mode = settings_metadata(path)?
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .unwrap_or(0o600);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let temporary = parent.join(format!(
        ".{}.langspan-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("settings"),
        std::process::id()
    ));
    // Creation errors must not enter cleanup: the preexisting entry is not ours.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<Written, crate::Error> {
        file.write_all(bytes)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        before_rename();
        let now = read_bytes(path)?;
        if now.as_deref() != from {
            return Ok(Written::Changed(now));
        }
        fs::rename(&temporary, path)?;
        Ok(Written::Replaced)
    })();
    if !matches!(result, Ok(Written::Replaced)) {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(unix))]
fn write_bytes(
    _path: &Path,
    _from: Option<&[u8]>,
    _bytes: &[u8],
    _before_rename: &mut dyn FnMut(),
) -> Result<Written, crate::Error> {
    Err("hook installation is implemented for Unix in this slice".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(root: &str) -> Registration {
        Registration {
            executable: "/opt/bin/langspan".into(),
            state_dir: "/home/u/.local/state/langspan".into(),
            workspace_id: "demo".into(),
            root: root.into(),
            share_prompt: true,
            code_briefing: true,
        }
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("langspan-guarded-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Adds `"ours": true` to whatever the file holds, as a planner would.
    fn add_ours(now: Option<&[u8]>) -> Result<Vec<u8>, crate::Error> {
        let mut value: Value = serde_json::from_slice(now.ok_or("missing")?)?;
        value["ours"] = json!(true);
        json_bytes(&value)
    }

    #[test]
    fn a_file_changed_between_plan_and_write_is_planned_again_against_the_change() {
        let scratch = Scratch::new("replan");
        let path = scratch.0.join(".claude.json");
        fs::write(&path, b"{\"projects\":{}}\n").unwrap();
        let from = read_bytes(&path).unwrap();
        let mut changes = 0;
        let written = write_planned_with(&path, from.as_deref(), &add_ours, &mut || {
            // The host rewrites its file once, after LangSpan planned.
            if changes == 0 {
                fs::write(&path, b"{\"projects\":{\"/new\":{}}}\n").unwrap();
            }
            changes += 1;
        })
        .unwrap();
        assert!(written);
        let result: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(result, json!({"projects":{"/new":{}},"ours":true}));
    }

    #[test]
    fn a_file_that_keeps_changing_is_refused_and_the_concurrent_change_kept() {
        let scratch = Scratch::new("refuse");
        let path = scratch.0.join(".claude.json");
        fs::write(&path, b"{\"n\":0}\n").unwrap();
        let from = read_bytes(&path).unwrap();
        let mut n = 0;
        let error = write_planned_with(&path, from.as_deref(), &add_ours, &mut || {
            n += 1;
            fs::write(&path, format!("{{\"n\":{n}}}\n")).unwrap();
        })
        .expect_err("a file that changes under every attempt is not overwritten");
        assert!(error.to_string().contains("changed while"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"{\"n\":2}\n");
        let leftovers: Vec<_> = fs::read_dir(&scratch.0).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file is left behind");
    }

    /// Removes `"ours"` from whatever the file holds, as uninstall would.
    fn remove_ours(now: Option<&[u8]>) -> Result<Vec<u8>, crate::Error> {
        let mut value: Value = serde_json::from_slice(now.ok_or("missing")?)?;
        value.as_object_mut().ok_or("object")?.shift_remove("ours");
        json_bytes(&value)
    }

    fn left_empty(bytes: &[u8]) -> bool {
        json_left_empty(Vendor::Codex, bytes)
    }

    #[test]
    fn a_file_left_holding_nothing_is_removed_and_one_with_more_is_written() {
        let scratch = Scratch::new("remove");
        let path = scratch.0.join("hooks.json");
        fs::write(&path, b"{\"ours\":true}\n").unwrap();
        let from = read_bytes(&path).unwrap();
        let applied =
            write_or_remove_planned(&path, from.as_deref(), &remove_ours, &left_empty).unwrap();
        assert_eq!(applied, Applied::Removed);
        assert!(!path.exists());

        fs::write(&path, b"{\"ours\":true,\"mine\":1}\n").unwrap();
        let from = read_bytes(&path).unwrap();
        let applied =
            write_or_remove_planned(&path, from.as_deref(), &remove_ours, &left_empty).unwrap();
        assert_eq!(applied, Applied::Written);
        assert_eq!(fs::read(&path).unwrap(), b"{\n  \"mine\": 1\n}\n");
    }

    #[test]
    fn a_file_changed_before_removal_is_planned_again_and_the_change_kept() {
        let scratch = Scratch::new("remove-replan");
        let path = scratch.0.join("hooks.json");
        fs::write(&path, b"{\"ours\":true}\n").unwrap();
        let from = read_bytes(&path).unwrap();
        let mut changes = 0;
        let applied = write_or_remove_with(
            &path,
            from.as_deref(),
            &remove_ours,
            &left_empty,
            &mut || {
                // The host writes a setting of its own after LangSpan planned.
                if changes == 0 {
                    fs::write(&path, b"{\"ours\":true,\"theirs\":1}\n").unwrap();
                }
                changes += 1;
            },
        )
        .unwrap();
        assert_eq!(applied, Applied::Written);
        let result: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(result, json!({"theirs": 1}));
    }

    #[test]
    fn a_file_that_keeps_changing_is_not_removed() {
        let scratch = Scratch::new("remove-refuse");
        let path = scratch.0.join("hooks.json");
        fs::write(&path, b"{\"ours\":true}\n").unwrap();
        let from = read_bytes(&path).unwrap();
        let mut n = 0;
        let error = write_or_remove_with(
            &path,
            from.as_deref(),
            &remove_ours,
            &left_empty,
            &mut || {
                // Still nothing but LangSpan's entry, yet different bytes.
                n += 1;
                fs::write(&path, format!("{{\"ours\":true}}{}\n", " ".repeat(n))).unwrap();
            },
        )
        .expect_err("a file that changes under every attempt is not removed");
        assert!(error.to_string().contains("changed while"), "{error}");
        assert!(path.exists());
    }

    #[test]
    fn only_an_empty_object_or_cursors_bare_version_counts_as_left_empty() {
        assert!(json_left_empty(Vendor::Codex, b"{}\n"));
        assert!(json_left_empty(Vendor::Cursor, b"{\n  \"version\": 1\n}\n"));
        assert!(!json_left_empty(Vendor::ClaudeCode, b"{\"version\":1}"));
        assert!(!json_left_empty(Vendor::Cursor, b"{\"version\":2}"));
        assert!(!json_left_empty(
            Vendor::Cursor,
            b"{\"version\":1,\"hooks\":{}}"
        ));
        assert!(!json_left_empty(Vendor::Gemini, b"{\"hooks\":{}}"));
        assert!(!json_left_empty(Vendor::Codex, b"[]"));
        assert!(!json_left_empty(Vendor::Codex, b""));
    }

    #[test]
    fn install_preserves_other_settings_and_order_and_is_idempotent() {
        let existing: Value = serde_json::from_str(
            r#"{"zeta":1,"permissions":{"allow":["Bash(ls)"]},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/x/guard.sh"}]}],"Stop":[]},"alpha":true}"#,
        )
        .unwrap();
        let once = plan(Some(existing.clone()), Some(&registration("/r"))).unwrap();
        let keys: Vec<_> = once.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["zeta", "permissions", "hooks", "alpha"]);
        assert_eq!(
            once["hooks"]["PreToolUse"][0],
            existing["hooks"]["PreToolUse"][0]
        );
        // The owner's (empty) Stop list now holds LangSpan's entry alone.
        assert_eq!(once["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert!(is_managed(&once["hooks"]["Stop"][0]["hooks"][0]));
        for event in EVENTS {
            let groups = once["hooks"][event].as_array().unwrap();
            let ours = groups.last().unwrap();
            let expected = if event == "PostToolUse" {
                POST_MATCHER
            } else {
                MATCHER
            };
            assert_eq!(ours["matcher"], expected);
            // Shell commands are matched in every phase (ADR 0011).
            assert!(
                expected.split('|').any(|tool| tool == "Bash"),
                "{event}: {expected}"
            );
            assert!(is_managed(&ours["hooks"][0]));
            assert!(ours["hooks"][0].get("onFailure").is_none());
        }
        let twice = plan(Some(once.clone()), Some(&registration("/r"))).unwrap();
        assert_eq!(once, twice, "reinstalling replaces rather than duplicates");
        let moved = plan(Some(once), Some(&registration("/other root"))).unwrap();
        assert_eq!(moved["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert!(
            moved["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("--root '/other root'")
        );
    }

    #[test]
    fn remove_restores_the_original_settings() {
        let original: Value = serde_json::from_str(
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/x/guard.sh"}]}]},"model":"x"}"#,
        )
        .unwrap();
        let installed = plan(Some(original.clone()), Some(&registration("/r"))).unwrap();
        assert_eq!(plan(Some(installed), None).unwrap(), original);
        let fresh = plan(None, Some(&registration("/r"))).unwrap();
        assert_eq!(plan(Some(fresh), None).unwrap(), json!({}));
    }

    #[test]
    fn only_langspan_hook_commands_are_recognised() {
        let ours = json!({"type": "command", "command": registration("/it's here").command()});
        assert!(is_managed(&ours));
        assert_eq!(
            words(
                ours["command"]
                    .as_str()
                    .unwrap()
                    .strip_suffix(Vendor::ClaudeCode.marker())
                    .unwrap()
            )
            .unwrap()
            .last()
            .unwrap(),
            "/it's here"
        );
        for other in [
            "/opt/bin/langspan-helper hook claude-code",
            "/opt/bin/langspan hook codex",
            "echo langspan hook claude-code",
            "'/opt/bin/langspan",
        ] {
            assert!(
                !is_managed(&json!({ "type": "command", "command": other })),
                "{other}"
            );
        }
    }

    fn user(executable: &str) -> UserRegistration {
        UserRegistration {
            executable: executable.into(),
            state_dir: "/home/u/.local/state/langspan".into(),
            share_prompt: true,
        }
    }

    #[test]
    fn user_install_keeps_unrelated_hooks_and_order_and_round_trips() {
        let original: Value = serde_json::from_str(
            r#"{"theme":"dark","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/x/guard.sh"}]}],"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]},"model":"m"}"#,
        )
        .unwrap();
        for vendor in [Vendor::ClaudeCode, Vendor::Codex] {
            let once = plan_user_for(
                vendor,
                Some(original.clone()),
                Some(&user("/opt/bin/langspan")),
            )
            .unwrap();
            let keys: Vec<_> = once.as_object().unwrap().keys().cloned().collect();
            assert_eq!(keys, ["theme", "hooks", "model"]);
            assert_eq!(
                once["hooks"]["PreToolUse"][0],
                original["hooks"]["PreToolUse"][0]
            );
            // The owner's own Stop hook stays first and unchanged.
            assert_eq!(once["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
            let ours = managed_hooks(&once, |hook| is_user_managed_for(vendor, hook));
            assert_eq!(ours.len(), vendor.hook_events().len());
            let command = ours[0]["command"].as_str().unwrap();
            assert!(
                !command.contains("--workspace") && !command.contains("--root"),
                "{command}"
            );
            // A user-level entry is never taken for a project-level one.
            assert!(managed_hooks(&once, |hook| is_managed_for(vendor, hook)).is_empty());
            let twice = plan_user_for(vendor, Some(once.clone()), Some(&user("/opt/bin/langspan")))
                .unwrap();
            assert_eq!(once, twice, "reinstalling replaces rather than duplicates");
            let moved =
                plan_user_for(vendor, Some(once.clone()), Some(&user("/new/langspan"))).unwrap();
            assert_eq!(
                managed_hooks(&moved, |hook| is_user_managed_for(vendor, hook)).len(),
                ours.len()
            );
            assert_eq!(plan_user_for(vendor, Some(moved), None).unwrap(), original);
        }
        let fresh = plan_user_for(Vendor::Codex, None, Some(&user("/opt/bin/langspan"))).unwrap();
        assert_eq!(
            plan_user_for(Vendor::Codex, Some(fresh), None).unwrap(),
            json!({})
        );
    }

    #[test]
    fn project_and_user_installs_in_one_file_are_removed_independently() {
        let project = plan(None, Some(&registration("/r"))).unwrap();
        let both = plan_user_for(
            Vendor::ClaudeCode,
            Some(project.clone()),
            Some(&user("/opt/bin/langspan")),
        )
        .unwrap();
        assert_eq!(
            plan_user_for(Vendor::ClaudeCode, Some(both.clone()), None).unwrap(),
            project
        );
        let user_only = plan(Some(both), None).unwrap();
        assert!(managed_hooks(&user_only, is_managed).is_empty());
        assert!(
            !managed_hooks(&user_only, |hook| is_user_managed_for(
                Vendor::ClaudeCode,
                hook
            ))
            .is_empty()
        );
    }

    #[test]
    fn only_the_exact_user_level_form_is_recognised() {
        let private = UserRegistration {
            share_prompt: false,
            ..user("/opt/it's/langspan")
        };
        for registration in [user("/opt/bin/langspan"), private] {
            for vendor in [Vendor::ClaudeCode, Vendor::Codex] {
                let hook = json!({"type": "command", "command": registration.command_for(vendor)});
                let parsed = parse_user_hook(vendor, &hook).unwrap();
                assert_eq!(parsed.executable, registration.executable);
                assert_eq!(parsed.share_prompt, registration.share_prompt);
                let other = if vendor == Vendor::Codex {
                    Vendor::ClaudeCode
                } else {
                    Vendor::Codex
                };
                assert!(!is_user_managed_for(other, &hook));
            }
        }
        let body = "'/opt/bin/langspan' hook claude-code --state-dir '/s'";
        for command in [
            body.to_owned(),
            format!("{body} # langspan-managed:claude-code:v1"),
            format!("{body} --ttl-ms 1 # langspan-managed:claude-code:user:v1"),
            format!("{body} && rm -rf / # langspan-managed:claude-code:user:v1"),
            "'/opt/bin/langspan' hook codex --state-dir '/s' # langspan-managed:claude-code:user:v1".into(),
        ] {
            assert!(
                !is_user_managed_for(Vendor::ClaudeCode, &json!({"type": "command", "command": command})),
                "{command}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn text_files_share_the_symlink_and_size_refusals() {
        let base = std::env::temp_dir().join(format!("ls-text-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let target = base.join("target.toml");
        fs::write(&target, "a = 1\n").unwrap();
        let link = base.join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            read_text(&link)
                .unwrap_err()
                .to_string()
                .contains("symlink")
        );
        assert!(write_planned(&link, None, &|_| Ok(b"b = 2\n".to_vec())).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "a = 1\n");
        let big = base.join("big.toml");
        fs::write(&big, vec![b'#'; MAX_SETTINGS_BYTES as usize + 1]).unwrap();
        assert!(read_text(&big).unwrap_err().to_string().contains("1 MiB"));
        assert_eq!(read_text(&base.join("missing.toml")).unwrap(), None);
        fs::write(&big, [0xff, 0xfe]).unwrap();
        assert!(read_text(&big).unwrap_err().to_string().contains("UTF-8"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn every_end_of_reply_and_batch_event_is_registered_for_its_host() {
        let claude = plan(None, Some(&registration("/r"))).unwrap();
        for event in ["Stop", "StopFailure", "SubagentStop", "PostToolBatch"] {
            let group = &claude["hooks"][event][0];
            assert!(is_managed(&group["hooks"][0]), "{event}");
            assert!(group.get("matcher").is_none(), "{event} takes no matcher");
        }
        let codex = plan_for(Vendor::Codex, None, Some(&registration("/r"))).unwrap();
        for event in ["Stop", "SessionEnd"] {
            assert!(
                is_managed_for(Vendor::Codex, &codex["hooks"][event][0]["hooks"][0]),
                "{event}"
            );
        }
        // Codex allows SessionEnd at most 3 seconds.
        assert_eq!(codex["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
        assert_eq!(codex["hooks"]["Stop"][0]["hooks"][0]["timeout"], 5);
        // Events Codex does not send, or whose subagent identity it does not
        // give, are not registered for it.
        for event in ["StopFailure", "SubagentStop", "PostToolBatch"] {
            assert!(codex["hooks"].get(event).is_none(), "{event}");
        }
        assert!(missing_events(Vendor::ClaudeCode, &claude, is_managed).is_empty());
        assert!(
            missing_events(Vendor::Codex, &codex, |hook| is_managed_for(
                Vendor::Codex,
                hook
            ))
            .is_empty()
        );
    }

    #[test]
    fn an_install_from_before_the_stop_events_upgrades_in_place() {
        let command = registration("/r").command();
        let ours = |matcher: Option<&str>| {
            let mut group =
                json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]});
            if let Some(matcher) = matcher {
                group["matcher"] = json!(matcher);
            }
            group
        };
        // What `hooks install claude-code` wrote before this change, next to
        // the owner's own Stop hook.
        let old = json!({"hooks": {
            "PreToolUse": [ours(Some(MATCHER))],
            "PostToolUse": [ours(Some(POST_MATCHER))],
            "PostToolUseFailure": [ours(Some(MATCHER))],
            "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}],
            "SessionStart": [ours(None)],
            "UserPromptSubmit": [ours(None)],
            "SessionEnd": [ours(None)],
        }});
        assert_eq!(
            missing_events(Vendor::ClaudeCode, &old, is_managed),
            ["Stop", "StopFailure", "SubagentStop", "PostToolBatch"]
        );
        let upgraded = plan(Some(old.clone()), Some(&registration("/r"))).unwrap();
        assert!(missing_events(Vendor::ClaudeCode, &upgraded, is_managed).is_empty());
        for (event, groups) in upgraded["hooks"].as_object().unwrap() {
            let count = groups
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|group| group["hooks"].as_array().unwrap())
                .filter(|hook| is_managed(hook))
                .count();
            assert_eq!(count, 1, "{event}: one LangSpan entry, never two");
        }
        assert_eq!(
            upgraded["hooks"]["Stop"][0], old["hooks"]["Stop"][0],
            "the owner's own Stop hook stays first"
        );
        assert_eq!(
            plan(Some(upgraded), None).unwrap(),
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}})
        );
    }

    #[test]
    fn cursor_and_gemini_keep_their_lifecycle_events_and_gain_no_end_of_reply_event() {
        let cursor = plan_for(Vendor::Cursor, None, Some(&registration("/r"))).unwrap();
        let gemini = plan_for(Vendor::Gemini, None, Some(&registration("/r"))).unwrap();
        for (vendor, settings) in [(Vendor::Cursor, &cursor), (Vendor::Gemini, &gemini)] {
            let registered: Vec<_> = settings["hooks"].as_object().unwrap().keys().collect();
            assert_eq!(registered, vendor.hook_events(), "{}", vendor.name());
            assert!(
                missing_events(vendor, settings, |hook| is_managed_for(vendor, hook)).is_empty(),
                "{}",
                vendor.name()
            );
        }
        for event in [
            "stop",
            "subagentStop",
            "Stop",
            "StopFailure",
            "SubagentStop",
        ] {
            assert!(cursor["hooks"].get(event).is_none(), "cursor {event}");
        }
        for event in ["AfterAgent", "Stop", "StopFailure", "SubagentStop"] {
            assert!(gemini["hooks"].get(event).is_none(), "gemini {event}");
        }
        // The per-event timeout goes through each host's own entry shape.
        assert_eq!(cursor["hooks"]["sessionEnd"][0]["timeout"], 5);
        assert_eq!(
            gemini["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"],
            5000
        );
        assert_eq!(
            gemini["hooks"]["SessionEnd"][0]["hooks"][0]["name"],
            GEMINI_HOOK_NAME
        );
    }

    #[test]
    fn cursor_entries_are_flat_marked_at_the_front_and_round_trip_at_both_levels() {
        let original = json!({"version": 1, "hooks": {"stop": [{"command": "./say.sh"}]}});
        let project = plan_for(
            Vendor::Cursor,
            Some(original.clone()),
            Some(&registration("/r")),
        )
        .unwrap();
        let both = plan_user_for(
            Vendor::Cursor,
            Some(project.clone()),
            Some(&user("/opt/bin/langspan")),
        )
        .unwrap();
        let entry = &both["hooks"]["preToolUse"][0];
        assert_eq!(entry["matcher"], CURSOR_MATCHER);
        assert!(entry.get("hooks").is_none(), "entries are not grouped");
        let command = entry["command"].as_str().unwrap();
        assert!(command.starts_with("LANGSPAN_MANAGED=cursor:v1 '/opt/bin/langspan' hook cursor "));
        assert!(!command.contains('#'));
        assert!(is_managed_for(Vendor::Cursor, entry));
        assert!(!is_managed_for(Vendor::ClaudeCode, entry));
        assert!(!is_user_managed_for(Vendor::Cursor, entry));
        let users = managed_hooks(&both, |hook| is_user_managed_for(Vendor::Cursor, hook));
        assert_eq!(users.len(), 6);
        assert_eq!(
            parse_user_hook(Vendor::Cursor, &users[0])
                .unwrap()
                .executable,
            PathBuf::from("/opt/bin/langspan")
        );
        assert_eq!(
            executable_of(Vendor::Cursor, command),
            Some(PathBuf::from("/opt/bin/langspan"))
        );
        // Each level is removed on its own, and the user's entry stays.
        assert_eq!(
            plan_user_for(Vendor::Cursor, Some(both.clone()), None).unwrap(),
            project
        );
        let user_only = plan_for(Vendor::Cursor, Some(both), None).unwrap();
        assert!(managed_hooks(&user_only, |hook| is_managed_for(Vendor::Cursor, hook)).is_empty());
        assert_eq!(
            plan_for(Vendor::Cursor, Some(project), None).unwrap(),
            original
        );
        // `version` is required by Cursor: added when missing, and kept on
        // removal, so a file that held only it round-trips exactly.
        let bare = json!({"version": 1});
        let installed = plan_for(
            Vendor::Cursor,
            Some(bare.clone()),
            Some(&registration("/r")),
        )
        .unwrap();
        assert_eq!(
            plan_for(Vendor::Cursor, Some(installed), None).unwrap(),
            bare
        );
        let fresh = plan_for(Vendor::Cursor, None, Some(&registration("/r"))).unwrap();
        assert_eq!(plan_for(Vendor::Cursor, Some(fresh), None).unwrap(), bare);
        // An unmarked or altered command is the user's.
        for other in [
            "'/opt/bin/langspan' hook cursor --state-dir '/s' --workspace 'demo' --root '/r'",
            "LANGSPAN_MANAGED=cursor:v1 '/opt/bin/langspan' hook cursor --state-dir '/s' --workspace 'demo' --root '/r' ; rm -rf /",
        ] {
            assert!(!is_managed_for(
                Vendor::Cursor,
                &json!({"type": "command", "command": other})
            ));
        }
    }

    #[test]
    fn malformed_settings_are_refused_without_changes() {
        assert!(plan(Some(json!([])), Some(&registration("/r"))).is_err());
        assert!(plan(Some(json!({"hooks": []})), None).is_err());
    }

    #[test]
    fn a_homebrew_keg_is_named_by_a_link_that_resolves_to_it_or_kept() {
        use std::os::unix::fs::symlink;
        let scratch = Scratch::new("brew");
        let base = fs::canonicalize(&scratch.0).unwrap();
        let keg = |version: &str| {
            let bin = base.join("Cellar/langspan").join(version).join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("langspan"), version).unwrap();
            bin.join("langspan")
        };
        let old = keg("0.1.0");
        let new = keg("0.2.0");
        fs::create_dir_all(base.join("opt")).unwrap();
        fs::create_dir_all(base.join("bin")).unwrap();

        // No link resolves to the running keg: today's path is kept.
        assert_eq!(installed_executable(&old).unwrap(), old);
        // `bin` alone resolves to it.
        symlink(&old, base.join("bin/langspan")).unwrap();
        assert_eq!(
            installed_executable(&old).unwrap(),
            base.join("bin/langspan")
        );
        // `opt` is preferred once it resolves to it too.
        symlink(
            base.join("Cellar/langspan/0.1.0"),
            base.join("opt/langspan"),
        )
        .unwrap();
        let opt = base.join("opt/langspan/bin/langspan");
        assert_eq!(installed_executable(&old).unwrap(), opt);
        // Called through a link, the answer is the same.
        assert_eq!(
            installed_executable(&base.join("bin/langspan")).unwrap(),
            opt
        );
        // Links that point at another version are not this binary.
        assert_eq!(installed_executable(&new).unwrap(), new);

        // Not a keg: the canonical path, unchanged.
        let plain = base.join("plain/bin/langspan");
        fs::create_dir_all(plain.parent().unwrap()).unwrap();
        fs::write(&plain, "x").unwrap();
        assert_eq!(installed_executable(&plain).unwrap(), plain);
        let missing = base.join("Cellar/langspan/9.9.9/bin/langspan");
        assert!(installed_executable(&missing).is_err());
    }
}
