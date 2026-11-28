//! Show or apply native hook registrations, preserving other settings.
//!
//! Generated commands carry an explicit shell-comment ownership marker. The
//! previous, unmarked format is accepted only in the exact generated shape with
//! an executable named `langspan`; other unmarked commands remain user-owned.
//! Settings-file symlinks are refused rather than silently replaced.

pub mod detect;
mod plan;
mod settings;

use serde_json::{Map, Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub use crate::hosts::port::{PRESENCE_MINUTES, Presence};
pub use crate::vendor::{Vendor, quote};
pub use plan::{plan, plan_for, plan_user_for};
pub use settings::{
    Applied, LeftEmpty, Planner, install_at, json_bytes, json_left_empty, parse_json,
    parse_mcp_json, parse_text, read, read_bytes, read_mcp, read_text, write_or_remove_planned,
    write_planned,
};
#[cfg(test)]
use settings::{MAX_SETTINGS_BYTES, write_or_remove_with, write_planned_with};

pub const EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "PostToolUseFailure"];
/// `Bash` is matched so shell commands are admitted when their write set is
/// literal and compared before and after ([ADR 0011](../docs/adr/0011-shell-writes.md)).
pub const MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Bash";
/// After the tool ran, a completed Read is also reported, so the version a
/// session saw can be compared when it later edits the file, and a Read or
/// Grep shows peers where the session is looking (roadmap A13).
pub const POST_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Bash|Read|Grep";
/// `PreToolUse` with `--outline-reads on`: a `Read` too, so a whole read of a
/// large file can be outlined first (roadmap A11).
pub const OUTLINE_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Bash|Read";
/// The same for Cursor's `preToolUse`.
pub const CURSOR_OUTLINE_MATCHER: &str = "^(Write|Delete|Shell|Read)$";
pub const CODEX_EVENTS: [&str; 2] = ["PreToolUse", "PostToolUse"];
pub const CODEX_MATCHER: &str = "^(apply_patch|Bash)$";
/// The tool events an earlier release registered for Gemini CLI, whose
/// support is removed (issue #246, ADR 0033). Kept only so `uninstall` and
/// `hooks remove` can find and take out what that release wrote.
const GEMINI_EVENTS: [&str; 2] = ["BeforeTool", "AfterTool"];
/// Antigravity's tool hooks (<https://antigravity.google/docs/hooks/>), matched
/// by regular expression on the tool name ([ADR 0035](../docs/adr/0035-antigravity-adapter.md)).
pub const ANTIGRAVITY_EVENTS: [&str; 2] = ["PreToolUse", "PostToolUse"];
/// Antigravity 1.5.14 edits with `edit_file` and reads with `read_file`; the
/// documented names stay in case another version uses them. These name the
/// adapter's `EDIT_TOOLS`, `SHELL_TOOL` and `READ_TOOLS`, which a test checks.
pub const ANTIGRAVITY_MATCHER: &str =
    "^(edit_file|write_to_file|replace_file_content|multi_replace_file_content|run_command)$";
pub const ANTIGRAVITY_POST_MATCHER: &str = "^(edit_file|write_to_file|replace_file_content|multi_replace_file_content|run_command|read_file|view_file)$";
/// Antigravity's `hooks.json` maps hook names to definitions. LangSpan's
/// entries are kept in the definition of this name.
pub const ANTIGRAVITY_HOOK_NAME: &str = "langspan";
/// Cursor's tool hooks, matched by tool type
/// (<https://cursor.com/docs/agent/hooks>, "Matcher"). `afterFileEdit`,
/// `beforeShellExecution` and `afterShellExecution` are not used: per the
/// documentation they carry no tool-call ID to join a call's two halves.
pub const CURSOR_EVENTS: [&str; 3] = ["preToolUse", "postToolUse", "postToolUseFailure"];
pub const CURSOR_MATCHER: &str = "^(Write|Delete|Shell)$";
pub const CURSOR_POST_MATCHER: &str = "^(Write|Delete|Shell|Read|Grep)$";
const CURSOR_PROJECT_MARK: &str = "LANGSPAN_MANAGED=cursor:v1 ";
const CURSOR_USER_MARK: &str = "LANGSPAN_MANAGED=cursor:user:v1 ";
/// The `hooks.json` schema version Cursor documents ("use `1`").
pub const CURSOR_CONFIG_VERSION: u64 = 1;
/// Host hook timeout in seconds; the adapter's own budget is shorter.
pub const HOST_TIMEOUT_SECONDS: u64 = 5;
/// Codex allows a SessionEnd hook at most 3 seconds (its hooks
/// documentation, "Config shape"). The adapter's budget is 1.5 seconds.
pub const CODEX_SESSION_END_TIMEOUT_SECONDS: u64 = 3;

/// What a refused Gemini CLI install says.
pub const GEMINI_REMOVED: &str = "Gemini CLI support was removed: Google replaced Gemini CLI with the Antigravity CLI, which LangSpan supports as `antigravity`. `langspan uninstall --hosts gemini-cli --apply` and `langspan hooks remove gemini-cli --root DIR --apply` remove an earlier install. Nothing was changed";

impl Vendor {
    /// Cursor lists hook entries directly under each event. Claude Code and
    /// Codex nest them in groups that carry the matcher, as Gemini CLI did.
    /// Antigravity nests them for its tool events and lists them directly
    /// under its other events.
    fn flat(self, event: &str) -> bool {
        match self {
            Self::Cursor => true,
            Self::Antigravity => !ANTIGRAVITY_EVENTS.contains(&event),
            _ => false,
        }
    }

    /// The key of the object that maps each event to its hooks: `hooks`, or
    /// for Antigravity, whose file maps hook names to definitions, LangSpan's
    /// own definition.
    fn table(self) -> &'static str {
        match self {
            Self::Antigravity => ANTIGRAVITY_HOOK_NAME,
            _ => "hooks",
        }
    }

    /// Antigravity's events do not name themselves, so each of its commands
    /// carries `--event NAME`.
    pub fn names_event(self) -> bool {
        self == Self::Antigravity
    }

    fn events(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &EVENTS,
            Self::Codex => &CODEX_EVENTS,
            Self::Cursor => &CURSOR_EVENTS,
            Self::Gemini => &GEMINI_EVENTS,
            Self::Antigravity => &ANTIGRAVITY_EVENTS,
        }
    }

    /// Session events with no tool matcher: discovery and peer briefings,
    /// the end of a reply and of a tool batch ([ADR 0027](../docs/adr/0027-turn-and-batch-events.md)).
    /// Codex has no StopFailure or PostToolBatch, and its SubagentStop names
    /// a subagent whose tool events carry the parent's session ID, which this
    /// adapter does not cover. Cursor's end-of-reply events are not
    /// registered until they are checked on a real host. Antigravity
    /// has no session-end event, and its `Stop` requires a decision, so it
    /// is not registered either (ADR 0035).
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
            // What an earlier release registered; never installed now.
            Self::Gemini => &["SessionStart", "BeforeAgent", "SessionEnd"],
            Self::Antigravity => &["SessionStart", "PreInvocation"],
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

    /// The matcher of one event's group. With `--outline-reads on`
    /// (roadmap A11), the pre-tool matcher of Claude Code and Cursor names
    /// their read tool too.
    fn matcher_for(self, event: &str, outline_reads: bool) -> &'static str {
        match (self, event, outline_reads) {
            (Self::ClaudeCode, "PreToolUse", true) => OUTLINE_MATCHER,
            (Self::Cursor, "preToolUse", true) => CURSOR_OUTLINE_MATCHER,
            _ => self.matcher(event),
        }
    }

    fn matcher(self, event: &str) -> &'static str {
        match self {
            Self::ClaudeCode if event == "PostToolUse" => POST_MATCHER,
            Self::ClaudeCode => MATCHER,
            Self::Codex => CODEX_MATCHER,
            Self::Cursor if event == "postToolUse" => CURSOR_POST_MATCHER,
            Self::Cursor => CURSOR_MATCHER,
            // Never installed, so no matcher is written or compared.
            Self::Gemini => "",
            Self::Antigravity if event == "PostToolUse" => ANTIGRAVITY_POST_MATCHER,
            Self::Antigravity => ANTIGRAVITY_MATCHER,
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
            Self::Antigravity => " # langspan-managed:antigravity:v1",
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
            Self::Antigravity => " # langspan-managed:antigravity:user:v1",
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
            Self::Antigravity => &[".agents/hooks.json"],
        }
    }

    /// The project file the host reads MCP servers from, relative to a
    /// project root, where `hooks install` registers `langspan mcp` beside
    /// the hooks. The other hosts' MCP servers are registered at user level.
    pub fn project_mcp_file(self) -> Option<&'static str> {
        match self {
            Self::Cursor => Some(".cursor/mcp.json"),
            Self::Antigravity => Some(".agents/mcp_config.json"),
            _ => None,
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
            Self::Cursor | Self::Gemini | Self::Antigravity => WorktreeHooks::Unconfirmed,
        }
    }

    /// The host and version a `worktree_hooks` rule was observed on.
    pub fn worktree_hooks_observed(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some("Claude Code 2.1.330"),
            Self::Codex => Some("Codex 0.171.0"),
            Self::Cursor | Self::Gemini | Self::Antigravity => None,
        }
    }

    /// One hook entry running `command` for `event`, its timeout in seconds.
    fn hook_entry(self, event: &str, command: String) -> Map<String, Value> {
        let mut entry = Map::new();
        entry.insert("type".into(), "command".into());
        entry.insert("command".into(), command.into());
        entry.insert("timeout".into(), self.timeout(event).into());
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
    /// Where this session looks is shown to peers; false adds `--presence
    /// off`, and a window other than the default adds `--presence-minutes`.
    pub presence: Presence,
    /// True adds `--outline-reads on` (roadmap A11), and the host's read
    /// tool to its pre-tool matcher.
    pub outline_reads: bool,
}

impl Presence {
    fn words(self) -> Vec<String> {
        let mut words = Vec::new();
        if !self.shown {
            words.extend(["--presence".into(), "off".into()]);
        }
        if let Some(minutes) = self.minutes {
            words.extend(["--presence-minutes".into(), minutes.to_string()]);
        }
        words
    }

    /// Take one presence option of an installed command, in the order
    /// `words` writes them; false for anything else.
    fn parse(&mut self, option: &str, value: &str) -> bool {
        match (option, value) {
            ("--presence", "off") if self.shown && self.minutes.is_none() => {
                self.shown = false;
                true
            }
            ("--presence-minutes", value) if self.minutes.is_none() => match Self::minutes(value) {
                Ok(Some(minutes)) => {
                    self.minutes = Some(minutes);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }
}

/// Whether two paths name the same directory: compared in canonical form,
/// so a path through a symbolic link (`/tmp` and `/private/tmp` on macOS)
/// matches the real one. Installs write the state directory as given; this
/// is the one comparison every check of an installed state directory uses,
/// so older installs written either way are judged alike. A directory that
/// does not exist yet (no coordinator has started) is resolved through its
/// nearest existing ancestor.
pub fn same_directory(a: &Path, b: &Path) -> bool {
    resolved(a) == resolved(b)
}

fn resolved(path: &Path) -> PathBuf {
    let Ok(absolute) = std::path::absolute(path) else {
        return path.to_path_buf();
    };
    let mut rest = Vec::new();
    let mut ancestor = absolute.as_path();
    loop {
        if let Ok(canonical) = fs::canonicalize(ancestor) {
            return rest
                .iter()
                .rev()
                .fold(canonical, |path, name| path.join(name));
        }
        match (ancestor.parent(), ancestor.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                ancestor = parent;
            }
            _ => return absolute,
        }
    }
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
        self.command_for_event(vendor, None)
    }

    /// The command for one event. Only a host whose events do not name
    /// themselves (`Vendor::names_event`) carries the event in it.
    pub fn command_for_event(&self, vendor: Vendor, event: Option<&str>) -> String {
        vendor.mark(&self.command_body(vendor, event), false)
    }

    fn command_body(&self, vendor: Vendor, event: Option<&str>) -> String {
        let mut words = head(&self.executable, vendor, event);
        words.extend([
            "--state-dir".into(),
            quote(&self.state_dir.to_string_lossy()),
            "--workspace".into(),
            quote(&self.workspace_id),
            "--root".into(),
            quote(&self.root.to_string_lossy()),
        ]);
        if !self.share_prompt {
            words.extend(["--prompt-summary".into(), "off".into()]);
        }
        if !self.code_briefing {
            words.extend(["--code-briefing".into(), "off".into()]);
        }
        words.extend(self.presence.words());
        words.extend(outline_words(self.outline_reads));
        words.join(" ")
    }
}

/// The words every generated command begins with: the executable, `hook`,
/// the host and, for a host whose events do not name themselves, `--event`.
fn head(executable: &Path, vendor: Vendor, event: Option<&str>) -> Vec<String> {
    let mut words = vec![
        quote(&executable.to_string_lossy()),
        "hook".into(),
        vendor.name().into(),
    ];
    if let Some(event) = event.filter(|_| vendor.names_event()) {
        words.extend(["--event".into(), event.into()]);
    }
    words
}

/// The words of a command without its `--event NAME`, and that event. The
/// event is required, and must be one the host is registered for, exactly
/// for a host whose commands carry it. `None`: the words are not ours.
fn split_event(vendor: Vendor, mut words: Vec<String>) -> Option<(Vec<String>, Option<String>)> {
    if !vendor.names_event() {
        return Some((words, None));
    }
    if words.len() < 5
        || words[3] != "--event"
        || !vendor.hook_events().contains(&words[4].as_str())
    {
        return None;
    }
    let event = words.remove(4);
    words.remove(3);
    Some((words, Some(event)))
}

/// A user-level registration: no workspace and no root. The hook resolves
/// both from each event's working directory.
pub struct UserRegistration {
    pub executable: PathBuf,
    pub state_dir: PathBuf,
    pub share_prompt: bool,
    pub presence: Presence,
    /// As `Registration::outline_reads`.
    pub outline_reads: bool,
}

impl UserRegistration {
    pub fn command_for(&self, vendor: Vendor) -> String {
        self.command_for_event(vendor, None)
    }

    pub fn command_for_event(&self, vendor: Vendor, event: Option<&str>) -> String {
        vendor.mark(&self.command_body(vendor, event), true)
    }

    fn command_body(&self, vendor: Vendor, event: Option<&str>) -> String {
        let mut words = head(&self.executable, vendor, event);
        words.extend([
            "--state-dir".into(),
            quote(&self.state_dir.to_string_lossy()),
        ]);
        if !self.share_prompt {
            words.extend(["--prompt-summary".into(), "off".into()]);
        }
        words.extend(self.presence.words());
        words.extend(outline_words(self.outline_reads));
        words.join(" ")
    }
}

/// The words `--outline-reads on` adds to an installed command, last.
fn outline_words(on: bool) -> Vec<String> {
    if on {
        vec!["--outline-reads".into(), "on".into()]
    } else {
        Vec::new()
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
    let (words, event) = split_event(vendor, words(body)?)?;
    if words.len() < 5 {
        return None;
    }
    // Only the options this module writes, each at most once, in its order;
    // the rebuilt command below must match.
    let (mut share_prompt, mut presence) = (true, Presence::default());
    let mut outline_reads = false;
    let mut options = &words[5..];
    while let [option, value, rest @ ..] = options {
        match (option.as_str(), value.as_str()) {
            ("--prompt-summary", "off") if share_prompt => share_prompt = false,
            ("--outline-reads", "on") if !outline_reads => outline_reads = true,
            (option, value) if presence.parse(option, value) => {}
            _ => return None,
        }
        options = rest;
    }
    if !options.is_empty()
        || words[1] != "hook"
        || words[2] != vendor.name()
        || words[3] != "--state-dir"
    {
        return None;
    }
    let parsed = UserRegistration {
        executable: words[0].clone().into(),
        state_dir: words[4].clone().into(),
        share_prompt,
        presence,
        outline_reads,
    };
    (parsed.command_body(vendor, event.as_deref()) == body).then_some(parsed)
}

pub fn is_user_managed_for(vendor: Vendor, hook: &Value) -> bool {
    parse_user_hook(vendor, hook).is_some()
}

/// Every hook entry in a settings value that `ours` recognises: inside a
/// group's `hooks` (Claude Code, Codex) or listed directly (Cursor), in the
/// `hooks` table or in Antigravity's `langspan` hook definition.
pub fn managed_hooks(settings: &Value, ours: impl Fn(&Value) -> bool) -> Vec<Value> {
    ["hooks", ANTIGRAVITY_HOOK_NAME]
        .into_iter()
        .filter_map(|table| settings[table].as_object())
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

/// Whether the settings switch LangSpan's hooks off as a whole: an
/// Antigravity hook definition runs none of its hooks with `"enabled": false`.
pub fn switched_off(vendor: Vendor, settings: &Value) -> bool {
    vendor == Vendor::Antigravity && settings[vendor.table()]["enabled"] == json!(false)
}

/// The tool events whose group holding one of `ours` matches other tools
/// than this version's install names: written by an earlier version, or
/// changed by hand. Such an install misses tools LangSpan handles, as the
/// first Antigravity install missed `edit_file` and `read_file`.
pub fn stale_matchers(
    vendor: Vendor,
    settings: &Value,
    ours: impl Fn(&Value) -> bool,
) -> Vec<&'static str> {
    vendor
        .events()
        .iter()
        .copied()
        .filter(|event| {
            settings[vendor.table()][*event]
                .as_array()
                .into_iter()
                .flatten()
                // A Cursor entry is listed directly and holds its own
                // matcher; it is its own group here (`entries_of`).
                .filter(|group| entries_of(group).into_iter().any(&ours))
                .any(|group| {
                    let outline = entries_of(group)
                        .into_iter()
                        .filter(|hook| ours(hook))
                        .any(outline_reads_on);
                    group["matcher"] != json!(vendor.matcher_for(event, outline))
                })
        })
        .collect()
}

/// Whether a hook entry LangSpan wrote runs with `--outline-reads on`: the
/// option is two words of the generated command (`outline_words`).
fn outline_reads_on(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| {
            command
                .split(' ')
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| pair == ["--outline-reads", "on"])
        })
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
            !settings[vendor.table()][*event]
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
    let (words, event) = split_event(vendor, words(body)?)?;
    if words.len() < 9 {
        return None;
    }
    // Only the opt-outs this module writes, each at most once. Rebuilding
    // the command below also requires the order it writes them in.
    let (mut share_prompt, mut code_briefing) = (true, true);
    let mut presence = Presence::default();
    let mut outline_reads = false;
    let mut options = &words[9..];
    while let [option, value, rest @ ..] = options {
        match (option.as_str(), value.as_str()) {
            ("--prompt-summary", "off") if share_prompt => share_prompt = false,
            ("--code-briefing", "off") if code_briefing => code_briefing = false,
            ("--outline-reads", "on") if !outline_reads => outline_reads = true,
            (option, value) if presence.parse(option, value) => {}
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
        presence,
        outline_reads,
    };
    let ours = parsed.command_body(vendor, event.as_deref()) == body
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn same_directory_sees_through_links_even_before_the_directory_exists() {
        let base = std::env::temp_dir().join(format!("ls-same-dir-{}", std::process::id()));
        let real = base.join("real");
        fs::create_dir_all(real.join("state")).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        fs::create_dir(base.join("other")).unwrap();
        let checks = [
            (link.join("state"), real.join("state"), true),
            // No coordinator has started: the directory is not there yet.
            (link.join("new/state"), real.join("new/state"), true),
            (link.join("state"), base.join("other"), false),
            (link.join("new/state"), real.join("new/other"), false),
        ];
        let results: Vec<bool> = checks
            .iter()
            .map(|(a, b, _)| same_directory(a, b))
            .collect();
        fs::remove_dir_all(&base).unwrap();
        for ((a, b, want), got) in checks.iter().zip(results) {
            assert_eq!(got, *want, "{} and {}", a.display(), b.display());
        }
    }

    fn registration(root: &str) -> Registration {
        Registration {
            executable: "/opt/bin/langspan".into(),
            state_dir: "/home/u/.local/state/langspan".into(),
            workspace_id: "demo".into(),
            root: root.into(),
            share_prompt: true,
            code_briefing: true,
            presence: Presence::default(),
            outline_reads: false,
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
            presence: Presence::default(),
            outline_reads: false,
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
        let unseen = UserRegistration {
            share_prompt: false,
            presence: Presence {
                shown: false,
                minutes: Some(3),
            },
            ..user("/opt/bin/langspan")
        };
        let window = UserRegistration {
            presence: Presence {
                shown: true,
                minutes: Some(30),
            },
            ..user("/opt/bin/langspan")
        };
        assert!(
            unseen
                .command_for(Vendor::ClaudeCode)
                .contains("--prompt-summary off --presence off --presence-minutes 3 #")
        );
        for registration in [user("/opt/bin/langspan"), private, unseen, window] {
            for vendor in [Vendor::ClaudeCode, Vendor::Codex] {
                let hook = json!({"type": "command", "command": registration.command_for(vendor)});
                let parsed = parse_user_hook(vendor, &hook).unwrap();
                assert_eq!(parsed.executable, registration.executable);
                assert_eq!(parsed.share_prompt, registration.share_prompt);
                assert_eq!(parsed.presence, registration.presence);
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
            // Presence options only in the order and form install writes.
            format!("{body} --presence-minutes 3 --presence off # langspan-managed:claude-code:user:v1"),
            format!("{body} --presence-minutes 10 # langspan-managed:claude-code:user:v1"),
            format!("{body} --presence-minutes 31 # langspan-managed:claude-code:user:v1"),
            format!("{body} --presence on # langspan-managed:claude-code:user:v1"),
            format!("{body} --presence off --presence off # langspan-managed:claude-code:user:v1"),
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
    fn cursor_keeps_its_lifecycle_events_and_gains_no_end_of_reply_event() {
        let vendor = Vendor::Cursor;
        let cursor = plan_for(vendor, None, Some(&registration("/r"))).unwrap();
        let registered: Vec<_> = cursor["hooks"].as_object().unwrap().keys().collect();
        assert_eq!(registered, vendor.hook_events());
        assert!(missing_events(vendor, &cursor, |hook| is_managed_for(vendor, hook)).is_empty());
        for event in [
            "stop",
            "subagentStop",
            "Stop",
            "StopFailure",
            "SubagentStop",
        ] {
            assert!(cursor["hooks"].get(event).is_none(), "cursor {event}");
        }
        // The per-event timeout goes through the host's own entry shape.
        assert_eq!(cursor["hooks"]["sessionEnd"][0]["timeout"], 5);
    }

    #[test]
    fn gemini_cli_is_never_installed_but_an_earlier_install_is_removed() {
        let vendor = Vendor::Gemini;
        assert!(!Vendor::ALL.contains(&vendor));
        assert_eq!(Vendor::from_name("gemini-cli"), Some(vendor));
        assert_eq!(Vendor::from_name("gemini"), Some(vendor));
        assert_eq!(
            plan_for(vendor, Some(json!({})), Some(&registration("/r"))),
            Err(GEMINI_REMOVED.into())
        );
        let user = UserRegistration {
            executable: "/bin/langspan".into(),
            state_dir: "/state".into(),
            share_prompt: true,
            presence: Presence::default(),
            outline_reads: false,
        };
        assert_eq!(
            plan_user_for(vendor, Some(json!({})), Some(&user)),
            Err(GEMINI_REMOVED.into())
        );
        // What the last release with the adapter wrote, beside the owner's hook.
        let owner = json!({"matcher": "write_file", "hooks": [{"type": "command", "command": "/x/guard.sh"}]});
        let mut settings = json!({"hooks": {"BeforeTool": [owner.clone()]}});
        for (event, command) in vendor.hook_events().into_iter().map(|event| {
            (
                event,
                format!(
                    "'/bin/langspan' hook gemini-cli --state-dir '/state'{}",
                    vendor.user_marker()
                ),
            )
        }) {
            let entry = json!({"hooks": [{"name": "langspan", "type": "command", "command": command, "timeout": 5000}]});
            match settings["hooks"][event].as_array_mut() {
                Some(groups) => groups.push(entry),
                None => settings["hooks"][event] = json!([entry]),
            }
        }
        assert_eq!(
            managed_hooks(&settings, |hook| is_user_managed_for(vendor, hook)).len(),
            5
        );
        assert_eq!(
            plan_user_for(vendor, Some(settings), None).unwrap(),
            json!({"hooks": {"BeforeTool": [owner]}})
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
    fn antigravity_matchers_name_exactly_the_tools_its_adapter_handles() {
        use crate::antigravity::{EDIT_TOOLS, READ_TOOLS, SHELL_TOOL};
        let pre: Vec<&str> = EDIT_TOOLS.iter().copied().chain([SHELL_TOOL]).collect();
        let post: Vec<&str> = pre.iter().copied().chain(READ_TOOLS).collect();
        assert_eq!(ANTIGRAVITY_MATCHER, format!("^({})$", pre.join("|")));
        assert_eq!(ANTIGRAVITY_POST_MATCHER, format!("^({})$", post.join("|")));
    }

    #[test]
    fn an_empty_antigravity_mcp_config_is_the_empty_configuration() {
        let path = Path::new("/work/repo/.agents/mcp_config.json");
        for bytes in [&b""[..], b"\n", b"  \r\n\t"] {
            assert_eq!(
                parse_mcp_json(Vendor::Antigravity, path, Some(bytes)).unwrap(),
                Some(json!({}))
            );
            // Only Antigravity leaves it empty; another host's is still JSON.
            let error = parse_mcp_json(Vendor::Cursor, path, Some(bytes)).unwrap_err();
            assert!(error.to_string().contains("not valid JSON"), "{error}");
        }
        assert_eq!(
            parse_mcp_json(Vendor::Antigravity, path, None).unwrap(),
            None
        );
        assert!(parse_mcp_json(Vendor::Antigravity, path, Some(b"{")).is_err());
        assert_eq!(
            parse_mcp_json(Vendor::Antigravity, path, Some(br#"{"mcpServers":{}}"#)).unwrap(),
            Some(json!({"mcpServers": {}}))
        );
    }

    #[test]
    fn a_matcher_other_than_this_versions_is_stale_for_its_event_only() {
        let registration = Registration {
            executable: "/opt/bin/langspan".into(),
            state_dir: "/state".into(),
            workspace_id: "demo".into(),
            root: "/work/repo".into(),
            share_prompt: true,
            code_briefing: true,
            presence: Presence::default(),
            outline_reads: false,
        };
        for vendor in Vendor::ALL {
            let ours = |hook: &Value| is_managed_for(vendor, hook);
            let mut settings = plan_for(vendor, None, Some(&registration)).unwrap();
            assert!(
                stale_matchers(vendor, &settings, ours).is_empty(),
                "{vendor:?}"
            );
            let event = vendor.events()[0];
            let group = &mut settings[vendor.table()][event][0];
            group["matcher"] = json!("^(nothing)$");
            assert_eq!(
                stale_matchers(vendor, &settings, ours),
                [event],
                "{vendor:?}"
            );
        }
    }

    /// Roadmap A11: `--outline-reads on` is in every command, parses back,
    /// and adds the read tool to the pre-tool matcher of the hosts that
    /// outline; the install with it on is not stale, nor the one without.
    #[test]
    fn outline_reads_on_adds_the_read_tool_before_it_runs() {
        let registration = Registration {
            outline_reads: true,
            ..registration("/work/repo")
        };
        let expected = [
            (Vendor::ClaudeCode, "PreToolUse", OUTLINE_MATCHER),
            (Vendor::Cursor, "preToolUse", CURSOR_OUTLINE_MATCHER),
        ];
        for vendor in Vendor::ALL {
            let ours = |hook: &Value| is_managed_for(vendor, hook);
            let settings = plan_for(vendor, None, Some(&registration)).unwrap();
            assert!(
                stale_matchers(vendor, &settings, ours).is_empty(),
                "{vendor:?}"
            );
            for hook in managed_hooks(&settings, ours) {
                assert!(outline_reads_on(&hook), "{hook}");
                assert!(parse_project_hook(vendor, &hook).unwrap().outline_reads);
            }
            for (host, event, matcher) in expected {
                if host == vendor {
                    let group = &settings[vendor.table()][event][0];
                    assert_eq!(group["matcher"], matcher, "{vendor:?}");
                }
            }
            // The same install with it off has the matcher without the read.
            let off = plan_for(vendor, Some(settings), Some(&registration_off())).unwrap();
            assert!(stale_matchers(vendor, &off, ours).is_empty(), "{vendor:?}");
            assert!(!managed_hooks(&off, ours).iter().any(outline_reads_on));
        }
        let user = UserRegistration {
            outline_reads: true,
            ..user("/opt/bin/langspan")
        };
        let command = user.command_for_event(Vendor::ClaudeCode, Some("PreToolUse"));
        let hook = json!({"type": "command", "command": command});
        assert!(
            parse_user_hook(Vendor::ClaudeCode, &hook)
                .unwrap()
                .outline_reads
        );
    }

    fn registration_off() -> Registration {
        registration("/work/repo")
    }

    #[test]
    fn antigravity_entries_live_in_their_own_named_definition_and_name_their_event() {
        // Antigravity's hooks.json maps hook names to definitions; the owner's
        // definition is a sibling of LangSpan's and is never touched.
        let original = json!({
            "format-on-save": {
                "PostToolUse": [{"matcher": "write_to_file", "hooks": [{"command": "./fmt.sh"}]}],
                "Stop": [{"type": "command", "command": "./say.sh"}],
            },
        });
        let vendor = Vendor::Antigravity;
        let project = plan_for(vendor, Some(original.clone()), Some(&registration("/r"))).unwrap();
        let keys: Vec<_> = project.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["format-on-save", ANTIGRAVITY_HOOK_NAME]);
        assert_eq!(project["format-on-save"], original["format-on-save"]);
        assert!(project.get("hooks").is_none(), "no `hooks` table");
        let ours = &project[ANTIGRAVITY_HOOK_NAME];
        let registered: Vec<_> = ours.as_object().unwrap().keys().collect();
        assert_eq!(registered, vendor.hook_events());
        for (event, matcher) in [
            ("PreToolUse", Some(ANTIGRAVITY_MATCHER)),
            ("PostToolUse", Some(ANTIGRAVITY_POST_MATCHER)),
            ("SessionStart", None),
            ("PreInvocation", None),
        ] {
            // Tool events nest their handlers in a matcher group; the others
            // list the handler directly.
            let entry = match matcher {
                Some(matcher) => {
                    assert_eq!(ours[event][0]["matcher"], matcher, "{event}");
                    &ours[event][0]["hooks"][0]
                }
                None => {
                    assert!(ours[event][0].get("hooks").is_none(), "{event}");
                    &ours[event][0]
                }
            };
            assert_eq!(entry["type"], "command", "{event}");
            assert_eq!(entry["timeout"], 5, "{event}: seconds");
            let command = entry["command"].as_str().unwrap();
            assert!(
                command.contains(&format!(" hook antigravity --event {event} --state-dir ")),
                "{command}"
            );
            assert!(
                command.ends_with(" # langspan-managed:antigravity:v1"),
                "{command}"
            );
            assert!(is_managed_for(vendor, entry), "{event}");
            assert!(!is_managed_for(Vendor::Gemini, entry), "{event}");
        }
        for event in ["Stop", "PostInvocation", "SessionEnd"] {
            assert!(ours.get(event).is_none(), "{event} is not registered");
        }
        assert!(missing_events(vendor, &project, |hook| is_managed_for(vendor, hook)).is_empty());
        assert_eq!(
            managed_hooks(&project, |hook| is_managed_for(vendor, hook)).len(),
            4
        );
        let again = plan_for(vendor, Some(project.clone()), Some(&registration("/r"))).unwrap();
        assert_eq!(
            again, project,
            "reinstalling replaces rather than duplicates"
        );

        // Both levels in one file, each removed on its own; nothing of ours
        // is left, not even an empty definition.
        let both = plan_user_for(
            vendor,
            Some(project.clone()),
            Some(&user("/opt/bin/langspan")),
        )
        .unwrap();
        assert_eq!(
            managed_hooks(&both, |hook| is_user_managed_for(vendor, hook)).len(),
            4
        );
        assert_eq!(
            plan_user_for(vendor, Some(both.clone()), None).unwrap(),
            project
        );
        assert_eq!(plan_for(vendor, Some(project), None).unwrap(), original);
        let fresh = plan_for(vendor, None, Some(&registration("/r"))).unwrap();
        assert_eq!(plan_for(vendor, Some(fresh), None).unwrap(), json!({}));
        assert!(plan_for(vendor, Some(json!({"langspan": []})), None).is_err());
    }

    #[test]
    fn an_antigravity_command_is_ours_only_with_an_event_it_is_registered_for() {
        let vendor = Vendor::Antigravity;
        let project = registration("/r").command_for_event(vendor, Some("PreToolUse"));
        let user_level = user("/opt/bin/langspan").command_for_event(vendor, Some("SessionStart"));
        assert!(is_managed_for(
            vendor,
            &json!({"type": "command", "command": project})
        ));
        assert!(is_user_managed_for(
            vendor,
            &json!({"type": "command", "command": user_level})
        ));
        assert_eq!(
            executable_of(vendor, &project),
            Some(PathBuf::from("/opt/bin/langspan"))
        );
        // Other hosts never carry the event.
        assert_eq!(
            registration("/r").command_for_event(Vendor::Cursor, Some("preToolUse")),
            registration("/r").command_for(Vendor::Cursor)
        );
        for command in [
            // No event, an event LangSpan does not register, or one elsewhere.
            registration("/r").command_for(vendor),
            project.replace("--event PreToolUse", "--event Stop"),
            project.replace("--event PreToolUse ", "") + " --event PreToolUse",
            user_level.replace("--event SessionStart", "--event"),
        ] {
            let hook = json!({"type": "command", "command": command});
            assert!(
                !is_managed_for(vendor, &hook) && !is_user_managed_for(vendor, &hook),
                "{command}"
            );
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

    /// The between-step watcher entries (issue #214) in `settings`.
    fn watchers(settings: &Value) -> Vec<Value> {
        managed_hooks(settings, |hook| {
            hook["command"]
                .as_str()
                .is_some_and(|command| command.contains(" --mode between-steps"))
        })
    }

    #[test]
    fn a_claude_code_install_adds_one_background_watcher_beside_its_prompt_hook() {
        let project = plan(None, Some(&registration("/r"))).unwrap();
        let user =
            plan_user_for(Vendor::ClaudeCode, None, Some(&user("/opt/bin/langspan"))).unwrap();
        for (settings, ours) in [
            (
                &project,
                &(|hook: &Value| is_managed_for(Vendor::ClaudeCode, hook))
                    as &dyn Fn(&Value) -> bool,
            ),
            (&user, &|hook: &Value| {
                is_user_managed_for(Vendor::ClaudeCode, hook)
            }),
        ] {
            let found = watchers(settings);
            assert_eq!(found.len(), 1, "{settings:#}");
            let watcher = &found[0];
            // Claude Code's documented background hook that wakes the model
            // on exit 2; it never decides anything.
            assert_eq!(watcher["asyncRewake"], json!(true));
            assert_eq!(
                watcher["timeout"],
                json!(crate::hosts::WATCHER_TIMEOUT_SECONDS)
            );
            let group = &settings["hooks"]["UserPromptSubmit"][0]["hooks"];
            assert_eq!(group.as_array().unwrap().len(), 2, "{group:#}");
            assert!(ours(&group[0]), "the prompt hook comes first");
            assert_eq!(group[1], *watcher);
            // The watcher is the prompt hook's own command with the mode.
            let prompt = group[0]["command"].as_str().unwrap();
            let command = watcher["command"].as_str().unwrap();
            let (body, marker) = prompt.split_at(prompt.find(" # langspan-managed").unwrap());
            assert_eq!(command, format!("{body} --mode between-steps{marker}"));
            // No other event has one.
            let all: usize = settings["hooks"]
                .as_object()
                .unwrap()
                .values()
                .flat_map(|groups| groups.as_array().unwrap())
                .flat_map(|group| group["hooks"].as_array().unwrap())
                .filter(|hook| hook.get("asyncRewake").is_some())
                .count();
            assert_eq!(all, 1);
        }
        // Reinstalling replaces it; removing takes it away with the rest.
        let twice = plan(Some(project.clone()), Some(&registration("/r"))).unwrap();
        assert_eq!(
            twice, project,
            "reinstalling replaces rather than duplicates"
        );
        assert!(watchers(&plan(Some(project), None).unwrap()).is_empty());
        let removed = plan_user_for(Vendor::ClaudeCode, Some(user), None).unwrap();
        assert!(watchers(&removed).is_empty(), "{removed:#}");
        // The other hosts document no between-step delivery.
        for vendor in [Vendor::Codex, Vendor::Cursor, Vendor::Antigravity] {
            let settings = plan_for(vendor, None, Some(&registration("/r"))).unwrap();
            assert!(watchers(&settings).is_empty(), "{}", vendor.name());
        }
    }

    #[test]
    fn a_hand_written_watcher_like_entry_is_not_taken_for_langspan_s() {
        let registered = plan(None, Some(&registration("/r"))).unwrap();
        let watcher = watchers(&registered)[0].clone();
        // Without asyncRewake, or with another mode, it is the owner's.
        let mut sync = watcher.clone();
        sync.as_object_mut().unwrap().remove("asyncRewake");
        let mut other = watcher.clone();
        other["command"] = json!(
            watcher["command"]
                .as_str()
                .unwrap()
                .replace("--mode between-steps", "--mode between-steps-2")
        );
        let owners = json!({"hooks": {"UserPromptSubmit": [{"hooks": [sync, other]}]}});
        assert_eq!(plan(Some(owners.clone()), None).unwrap(), owners);
    }
}
