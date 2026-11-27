//! User-level installation: where each host keeps its user configuration,
//! and LangSpan's MCP server entry in it.
//!
//! Hooks reuse the planning in `install`. The MCP entry is recognized by its
//! exact generated shape in JSON (Claude Code's `~/.claude.json`, whose
//! tolerance of unknown keys is not documented, Cursor's and Antigravity's
//! MCP files, and the `~/.gemini/settings.json` of an earlier Gemini CLI
//! install, which `uninstall` cleans) and by marker comments
//! around a LangSpan-owned table in TOML (Codex's `config.toml`). Any other
//! entry named `langspan` belongs to the user: install refuses rather than
//! replace it, and uninstall leaves it alone.

use crate::install::Vendor;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub const SERVER_NAME: &str = "langspan";
const TOML_BEGIN: &str = "# langspan-managed:mcp:v1 begin";
const TOML_END: &str = "# langspan-managed:mcp:v1 end";
const TOML_NOTE: &str = "# Written by `langspan install`; `langspan uninstall` removes this table.";

/// The files one host reads for its user-level hooks and MCP servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFiles {
    /// The directory whose existence means the host is in use.
    pub config_dir: PathBuf,
    pub hooks: PathBuf,
    pub mcp: PathBuf,
}

/// Locate a host's user files under `home`. `override_dir` is the host's own
/// relocation variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`)
/// when the caller honours it; each host documents that it moves every user
/// file. `GEMINI_CLI_HOME` names a home: the files are in its `.gemini`. It
/// is read only to remove an earlier Gemini CLI install.
pub fn host_files(vendor: Vendor, home: &Path, override_dir: Option<PathBuf>) -> HostFiles {
    match vendor {
        Vendor::ClaudeCode => {
            let (config_dir, mcp) = match override_dir {
                Some(dir) => (dir.clone(), dir.join(".claude.json")),
                None => (home.join(".claude"), home.join(".claude.json")),
            };
            HostFiles {
                hooks: config_dir.join("settings.json"),
                config_dir,
                mcp,
            }
        }
        Vendor::Codex => {
            let config_dir = override_dir.unwrap_or_else(|| home.join(".codex"));
            HostFiles {
                hooks: config_dir.join("hooks.json"),
                mcp: config_dir.join("config.toml"),
                config_dir,
            }
        }
        // <https://cursor.com/docs/agent/hooks> ("User") and
        // <https://cursor.com/docs/context/mcp> ("Global"). Cursor documents
        // no variable that relocates them.
        Vendor::Cursor => {
            let config_dir = home.join(".cursor");
            HostFiles {
                hooks: config_dir.join("hooks.json"),
                mcp: config_dir.join("mcp.json"),
                config_dir,
            }
        }
        Vendor::Gemini => {
            let config_dir = override_dir.as_deref().unwrap_or(home).join(".gemini");
            HostFiles {
                hooks: config_dir.join("settings.json"),
                mcp: config_dir.join("settings.json"),
                config_dir,
            }
        }
        // <https://antigravity.google/docs/hooks/> and
        // <https://antigravity.google/docs/mcp/> ("Global"). Antigravity
        // documents no variable that relocates them; `GEMINI_CLI_HOME` is
        // Gemini CLI's.
        Vendor::Antigravity => {
            let config_dir = home.join(".gemini").join("config");
            HostFiles {
                hooks: config_dir.join("hooks.json"),
                mcp: config_dir.join("mcp_config.json"),
                config_dir,
            }
        }
    }
}

/// The `langspan mcp` server as a host launches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub executable: PathBuf,
    pub state_dir: PathBuf,
    /// `install --mcp-tools hooked` (roadmap I11): the host whose hooks
    /// shorten the tool list, written as `--tools hooked --host NAME`.
    /// `None` lists every tool.
    pub hooked_host: Option<String>,
}

impl McpServer {
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "mcp".into(),
            "--state-dir".into(),
            self.state_dir.to_string_lossy().into_owned(),
        ];
        if let Some(host) = &self.hooked_host {
            args.extend([
                "--tools".into(),
                "hooked".into(),
                "--host".into(),
                host.clone(),
            ]);
        }
        args
    }

    /// The server `args` describe, if they are exactly what `args` writes.
    fn from_args(executable: PathBuf, args: &[String]) -> Option<Self> {
        let (state_dir, hooked_host) = match args {
            [mcp, option, state_dir] if mcp == "mcp" && option == "--state-dir" => {
                (state_dir, None)
            }
            [mcp, option, state_dir, tools, hooked, host_option, host]
                if mcp == "mcp"
                    && option == "--state-dir"
                    && tools == "--tools"
                    && hooked == "hooked"
                    && host_option == "--host"
                    && crate::mcp::HOOKED_HOSTS.contains(&host.as_str()) =>
            {
                (state_dir, Some(host.clone()))
            }
            _ => return None,
        };
        Some(Self {
            executable,
            state_dir: state_dir.into(),
            hooked_host,
        })
    }

    /// Claude Code's stdio entry, in the shape its documentation shows.
    pub fn claude_entry(&self) -> Value {
        json!({
            "type": "stdio",
            "command": self.executable.to_string_lossy(),
            "args": self.args(),
        })
    }

    /// Antigravity's stdio entry: `command` and `args`
    /// (<https://antigravity.google/docs/mcp/>).
    pub fn antigravity_entry(&self) -> Value {
        json!({
            "command": self.executable.to_string_lossy(),
            "args": self.args(),
        })
    }

    fn toml_block(&self) -> String {
        let args: Vec<String> = self.args().iter().map(|a| toml_string(a)).collect();
        format!(
            "{TOML_BEGIN}\n{TOML_NOTE}\n[mcp_servers.{SERVER_NAME}]\ncommand = {}\nargs = [{}]\n{TOML_END}\n",
            toml_string(&self.executable.to_string_lossy()),
            args.join(", ")
        )
    }
}

fn is_langspan_executable(command: &str) -> bool {
    let path = Path::new(command);
    path.is_absolute() && path.file_name().and_then(|n| n.to_str()) == Some("langspan")
}

/// The server a Claude Code entry runs, if it is exactly what LangSpan writes:
/// `type` stdio (or absent), an absolute `langspan` executable, the three
/// generated arguments and at most an empty `env`. Anything else is the user's.
pub fn parse_claude_entry(entry: &Value) -> Option<McpServer> {
    let entry = entry.as_object()?;
    for (key, value) in entry {
        let fits = match key.as_str() {
            "type" => value == "stdio",
            "env" => value.as_object().is_some_and(Map::is_empty),
            "command" | "args" => true,
            _ => false,
        };
        if !fits {
            return None;
        }
    }
    let command = entry.get("command")?.as_str()?;
    if !is_langspan_executable(command) {
        return None;
    }
    let args = entry
        .get("args")?
        .as_array()?
        .iter()
        .map(|arg| arg.as_str().map(str::to_owned))
        .collect::<Option<Vec<String>>>()?;
    McpServer::from_args(command.into(), &args)
}

/// Return an `mcpServers` configuration with LangSpan's server set
/// (install) or removed: Claude Code's `~/.claude.json`, or Cursor's
/// `mcp.json`, which documents the same stdio entry shape
/// (<https://cursor.com/docs/context/mcp>). Other servers and every other
/// key keep their place.
pub fn plan_claude(existing: Value, server: Option<&McpServer>) -> Result<Value, String> {
    plan_json(Vendor::ClaudeCode, existing, server)
}

/// Gemini CLI's user settings with the MCP entry an earlier release wrote
/// removed. Gemini CLI support is removed (issue #246), so nothing writes
/// one; `server` must be `None`. The entry is recognized as LangSpan's by
/// the same exact shape as Claude Code's.
pub fn plan_gemini(existing: Value, server: Option<&McpServer>) -> Result<Value, String> {
    if server.is_some() {
        return Err(crate::install::GEMINI_REMOVED.into());
    }
    plan_json(Vendor::Gemini, existing, None)
}

/// The same for Antigravity's `mcp_config.json`, which holds only MCP
/// servers: a stdio entry is `command` and `args`
/// (<https://antigravity.google/docs/mcp/>). A file LangSpan creates holds
/// `{}` once its entry is removed.
pub fn plan_antigravity(
    existing: Option<Value>,
    server: Option<&McpServer>,
) -> Result<Value, String> {
    plan_json(
        Vendor::Antigravity,
        existing.unwrap_or_else(|| json!({})),
        server,
    )
}

fn plan_json(vendor: Vendor, existing: Value, server: Option<&McpServer>) -> Result<Value, String> {
    let Value::Object(mut config) = existing else {
        return Err("the MCP configuration must be a JSON object; nothing was changed".into());
    };
    let servers = match config.get_mut("mcpServers") {
        None => None,
        Some(Value::Object(servers)) => Some(servers),
        Some(_) => {
            return Err("`mcpServers` must be a JSON object; nothing was changed".into());
        }
    };
    let ours = servers
        .as_ref()
        .and_then(|servers| servers.get(SERVER_NAME))
        .map(|entry| parse_claude_entry(entry).is_some());
    match (server, ours) {
        (Some(_), Some(false)) => Err(format!(
            "`mcpServers.{SERVER_NAME}` exists and was not written by LangSpan; nothing was changed. Rename or remove that entry, then run this again"
        )),
        (Some(server), _) => {
            let entry = if vendor == Vendor::Antigravity {
                server.antigravity_entry()
            } else {
                server.claude_entry()
            };
            if parse_claude_entry(&entry).is_none() {
                return Err(format!(
                    "the executable {} is not named `langspan`, so its entry could not be recognised later; install it under that name",
                    server.executable.display()
                ));
            }
            let servers = config
                .entry("mcpServers")
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or("`mcpServers` must be a JSON object")?;
            // Replacing an existing key keeps its position.
            servers.insert(SERVER_NAME.into(), entry);
            Ok(Value::Object(config))
        }
        (None, Some(true)) => {
            let servers = config
                .get_mut("mcpServers")
                .and_then(Value::as_object_mut)
                .ok_or("`mcpServers` must be a JSON object")?;
            servers.shift_remove(SERVER_NAME);
            if servers.is_empty() {
                config.shift_remove("mcpServers");
            }
            Ok(Value::Object(config))
        }
        (None, _) => Ok(Value::Object(config)),
    }
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parse one basic string in the form `toml_string` writes, returning it and
/// the rest of the line.
fn parse_toml_string(text: &str) -> Option<(String, &str)> {
    let mut chars = text.strip_prefix('"')?.char_indices();
    let mut out = String::new();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return Some((out, &text[index + 2..])),
            '\\' => match chars.next()?.1 {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'u' => {
                    let hex: String = (0..4)
                        .filter_map(|_| chars.next().map(|(_, c)| c))
                        .collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// Where LangSpan's block sits in a TOML file, by line index (end inclusive).
struct Block {
    begin: usize,
    end: usize,
}

/// Find LangSpan's marked block and refuse a `mcp_servers.langspan` defined
/// anywhere else. The scan is line-oriented: a multi-line string that happens
/// to contain such a header causes a refusal, never a silent edit.
fn scan_toml(lines: &[&str]) -> Result<Option<Block>, String> {
    let mut block: Option<Block> = None;
    let mut open: Option<usize> = None;
    let mut table = String::new();
    let normal = |text: &str| -> String {
        text.chars()
            .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
            .collect()
    };
    let names_ours = |name: &str| {
        let prefix = format!("mcp_servers.{SERVER_NAME}");
        name == prefix || name.starts_with(&format!("{prefix}."))
    };
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line == TOML_BEGIN {
            if open.is_some() || block.is_some() {
                return Err("config.toml has more than one LangSpan block; nothing was changed. Remove the extra one by hand".into());
            }
            open = Some(index);
            continue;
        }
        if line == TOML_END {
            let Some(begin) = open.take() else {
                return Err(
                    "config.toml has a LangSpan end marker without its start; nothing was changed"
                        .into(),
                );
            };
            block = Some(Block { begin, end: index });
            // A bare key after our block would extend our table.
            table = format!("mcp_servers.{SERVER_NAME}");
            continue;
        }
        if open.is_some() || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let header = line.split('#').next().unwrap_or(line);
            let name = normal(header.trim_matches(|c| c == '[' || c == ']' || c == ' '));
            if names_ours(&name) {
                return Err(format!(
                    "config.toml already has a `[mcp_servers.{SERVER_NAME}]` table that LangSpan did not write; nothing was changed. Rename or remove it, then run this again"
                ));
            }
            table = name;
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = normal(key);
            let full = if table.is_empty() {
                key
            } else {
                format!("{table}.{key}")
            };
            if names_ours(&full) || (full == "mcp_servers" && value.contains(SERVER_NAME)) {
                return Err(format!(
                    "config.toml already defines `mcp_servers.{SERVER_NAME}` outside a LangSpan block; nothing was changed. Rename or remove it, then run this again"
                ));
            }
        }
    }
    if open.is_some() {
        return Err(
            "config.toml has a LangSpan start marker without its end; nothing was changed".into(),
        );
    }
    Ok(block)
}

/// The server in LangSpan's block of a Codex `config.toml`, if there is one.
pub fn parse_codex(text: &str) -> Result<Option<McpServer>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(block) = scan_toml(&lines)? else {
        return Ok(None);
    };
    let mut command = None;
    let mut args = None;
    for line in &lines[block.begin..=block.end] {
        if let Some(rest) = line.strip_prefix("command = ") {
            command = parse_toml_string(rest).map(|(value, _)| value);
        } else if let Some(rest) = line.strip_prefix("args = [") {
            let mut values = Vec::new();
            let mut rest = rest;
            while let Some((value, tail)) = parse_toml_string(rest) {
                values.push(value);
                rest = tail.trim_start_matches([',', ' ']);
            }
            args = Some(values);
        }
    }
    match command
        .zip(args)
        .and_then(|(command, args)| McpServer::from_args(command.into(), &args))
    {
        Some(server) => Ok(Some(server)),
        None => Err("LangSpan's block in config.toml was edited and is no longer recognised; nothing was changed. Remove the block by hand, then run this again".into()),
    }
}

/// Return Codex's `config.toml` text with LangSpan's block set (install) or
/// removed. Every other line is kept byte for byte.
pub fn plan_codex(existing: Option<&str>, server: Option<&McpServer>) -> Result<String, String> {
    let text = existing.unwrap_or("");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let trimmed: Vec<&str> = lines
        .iter()
        .map(|l| l.trim_end_matches(['\n', '\r']))
        .collect();
    let block = scan_toml(&trimmed)?;
    if block.is_some() {
        parse_codex(text)?;
    }
    match (block, server) {
        (Some(block), Some(server)) => Ok(format!(
            "{}{}{}",
            lines[..block.begin].concat(),
            server.toml_block(),
            lines[block.end + 1..].concat()
        )),
        (Some(block), None) => {
            let mut before = lines[..block.begin].concat();
            let after = lines[block.end + 1..].concat();
            // Undo the blank separator install adds before a block at the end.
            if after.is_empty() && before.ends_with("\n\n") {
                before.pop();
            }
            Ok(before + &after)
        }
        (None, Some(server)) => {
            let mut out = text.to_owned();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(&server.toml_block());
            Ok(out)
        }
        (None, None) => Ok(text.to_owned()),
    }
}

/// What a whole `uninstall` leaves (issue #292): the binary, and the state
/// directory unless `--purge` removes it.
pub struct Leftover {
    /// The state directory, when it exists.
    state_dir: Option<PathBuf>,
    purge: bool,
}

impl Leftover {
    /// Checked before anything is written. `whole` is an uninstall without
    /// `--hosts`; only that names what is left, since the other hosts'
    /// hooks still use the binary and the state directory. `--purge` needs
    /// one, and removes only an empty directory or one a coordinator used.
    pub fn check(state_dir: &Path, purge: bool, whole: bool) -> Result<Option<Self>, crate::Error> {
        if !whole && purge {
            return Err("--purge removes the state directory every host uses: run `langspan uninstall --purge` without --hosts".into());
        }
        if !whole {
            return Ok(None);
        }
        let state_dir = match std::fs::symlink_metadata(state_dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("{}: {error}", state_dir.display()).into()),
            Ok(_) => Some(state_dir.to_path_buf()),
        };
        if purge && let Some(dir) = &state_dir {
            let ours = dir.is_dir()
                && !dir.is_symlink()
                && (dir.join("daemon.lock").is_file() || std::fs::read_dir(dir)?.next().is_none());
            if !ours {
                return Err(format!("{} is not a LangSpan state directory (it has no daemon.lock), so --purge changed nothing. Remove it yourself, or run uninstall without --purge", dir.display()).into());
            }
        }
        Ok(Some(Self { state_dir, purge }))
    }

    /// With `--purge`, removes the state directory once `apply`. Returns
    /// how many removals there are and the lines to print: after `apply`,
    /// what is left and the commands that remove it.
    pub fn finish(
        &self,
        executable: &Path,
        apply: bool,
    ) -> Result<(usize, Vec<String>), crate::Error> {
        use crate::install::quote;
        let mut left = self.state_dir.as_deref();
        let removing = usize::from(self.purge && left.is_some());
        let mut lines = Vec::new();
        if let (1, Some(dir)) = (removing, left) {
            if !apply {
                return Ok((1, vec![format!("{} would be removed.", dir.display())]));
            }
            std::fs::remove_dir_all(dir)
                .map_err(|error| format!("remove {}: {error}", dir.display()))?;
            lines.push(format!("Removed {}.", dir.display()));
            left = None;
        }
        if apply {
            lines.push(match left {
                Some(_) => {
                    "Left in place: the langspan binary and its state directory. To remove them:"
                        .into()
                }
                None => "Left in place: the langspan binary. To remove it:".into(),
            });
            lines.push(format!("  rm {}", quote(&executable.to_string_lossy())));
            lines.extend(left.map(|dir| format!("  rm -r {}", quote(&dir.to_string_lossy()))));
        }
        Ok((removing, lines))
    }
}

/// Marks the `statusLine` LangSpan writes in Claude Code's user settings. The
/// host runs the command through a shell, which ignores the comment.
const STATUS_LINE_MARKER: &str = " # langspan-managed:claude-code:statusline:v1";

/// What `install --statusline` asks for Claude Code's `statusLine`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusLineChoice {
    /// No `--statusline`: whatever is there stays.
    Unchanged,
    /// `--statusline on`: LangSpan's line, only where none is set.
    On {
        executable: PathBuf,
        state_dir: PathBuf,
    },
    /// `--statusline off`, and every `uninstall`: LangSpan's line removed.
    Off,
}

/// The command LangSpan's status line runs.
pub fn status_line_command(executable: &Path, state_dir: &Path) -> String {
    format!(
        "{} --state-dir {} statusline{STATUS_LINE_MARKER}",
        crate::install::quote(&executable.to_string_lossy()),
        crate::install::quote(&state_dir.to_string_lossy())
    )
}

/// Whether a `statusLine` value is the one LangSpan writes.
pub fn is_langspan_status_line(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("command")
        && value
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.ends_with(STATUS_LINE_MARKER))
}

/// Plan Claude Code's `statusLine` in its user settings, and what to tell
/// the person. A status line that is not LangSpan's is never replaced or
/// removed; every other key keeps its place.
pub fn plan_status_line(
    existing: Value,
    choice: &StatusLineChoice,
) -> Result<(Value, Option<String>), String> {
    let Value::Object(mut settings) = existing else {
        return Err("the settings must be a JSON object; nothing was changed".into());
    };
    let current = settings.get("statusLine");
    let ours = current.is_some_and(is_langspan_status_line);
    let note = match choice {
        StatusLineChoice::Unchanged => None,
        StatusLineChoice::Off => {
            if ours {
                settings.remove("statusLine");
            }
            None
        }
        StatusLineChoice::On { .. } if current.is_some() && !ours => Some(
            "Claude Code already has a status line (`statusLine` in its settings); it was left as it is. To add LangSpan's counts to it, call `langspan statusline` from that command.".to_owned(),
        ),
        StatusLineChoice::On {
            executable,
            state_dir,
        } => {
            settings.insert(
                "statusLine".into(),
                json!({
                    "type": "command",
                    "command": status_line_command(executable, state_dir),
                }),
            );
            None
        }
    };
    Ok((Value::Object(settings), note))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(state: &str) -> McpServer {
        McpServer {
            executable: "/opt/bin/langspan".into(),
            state_dir: state.into(),
            hooked_host: None,
        }
    }

    fn hooked(state: &str, host: &str) -> McpServer {
        McpServer {
            hooked_host: Some(host.into()),
            ..server(state)
        }
    }

    /// `--mcp-tools hooked` entries round-trip, are replaced in place by a
    /// plain one and removed exactly; an edited flag is not LangSpan's.
    #[test]
    fn a_hooked_entry_is_recognised_replaced_and_removed() {
        let original = json!({"mcpServers":{"other":{"command":"npx"}}});
        let installed = plan_claude(original.clone(), Some(&hooked("/s", "claude-code"))).unwrap();
        let entry = &installed["mcpServers"]["langspan"];
        assert_eq!(
            entry["args"],
            json!([
                "mcp",
                "--state-dir",
                "/s",
                "--tools",
                "hooked",
                "--host",
                "claude-code"
            ])
        );
        assert_eq!(parse_claude_entry(entry), Some(hooked("/s", "claude-code")));
        let plain = plan_claude(installed.clone(), Some(&server("/s"))).unwrap();
        assert_eq!(
            plain["mcpServers"]["langspan"]["args"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(plan_claude(installed, None).unwrap(), original);
        for edited in [
            json!(["mcp", "--state-dir", "/s", "--tools", "hooked"]),
            json!([
                "mcp",
                "--state-dir",
                "/s",
                "--tools",
                "hooked",
                "--host",
                "cursor"
            ]),
            json!([
                "mcp",
                "--state-dir",
                "/s",
                "--tools",
                "all",
                "--host",
                "codex"
            ]),
            json!([
                "mcp",
                "--state-dir",
                "/s",
                "--host",
                "codex",
                "--tools",
                "hooked"
            ]),
        ] {
            let entry = json!({"command": "/opt/bin/langspan", "args": edited});
            assert_eq!(parse_claude_entry(&entry), None, "{entry}");
        }

        let text = plan_codex(None, Some(&hooked("/s", "codex"))).unwrap();
        assert!(
            text.contains("args = [\"mcp\", \"--state-dir\", \"/s\", \"--tools\", \"hooked\", \"--host\", \"codex\"]"),
            "{text}"
        );
        assert_eq!(parse_codex(&text).unwrap(), Some(hooked("/s", "codex")));
        let plain = plan_codex(Some(&text), Some(&server("/s"))).unwrap();
        assert_eq!(parse_codex(&plain).unwrap(), Some(server("/s")));
        assert_eq!(plan_codex(Some(&text), None).unwrap(), "");
    }

    #[test]
    fn claude_entry_is_added_beside_other_servers_and_removed_exactly() {
        let original: Value = serde_json::from_str(
            r#"{"numStartups":3,"mcpServers":{"other":{"type":"stdio","command":"npx","args":["x"]}},"projects":{}}"#,
        )
        .unwrap();
        let installed = plan_claude(original.clone(), Some(&server("/s"))).unwrap();
        let keys: Vec<_> = installed.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["numStartups", "mcpServers", "projects"]);
        let names: Vec<_> = installed["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(names, ["other", "langspan"]);
        assert_eq!(
            installed["mcpServers"]["langspan"],
            json!({"type":"stdio","command":"/opt/bin/langspan","args":["mcp","--state-dir","/s"]})
        );
        assert_eq!(
            plan_claude(installed.clone(), Some(&server("/s"))).unwrap(),
            installed
        );
        let moved = plan_claude(installed.clone(), Some(&server("/t"))).unwrap();
        assert_eq!(moved["mcpServers"]["langspan"]["args"][2], "/t");
        assert_eq!(plan_claude(installed, None).unwrap(), original);
        let fresh = plan_claude(json!({"a":1}), Some(&server("/s"))).unwrap();
        assert_eq!(plan_claude(fresh, None).unwrap(), json!({"a":1}));
    }

    #[test]
    fn a_foreign_claude_langspan_entry_is_refused_and_kept() {
        for foreign in [
            json!({"command":"/usr/bin/other","args":["mcp","--state-dir","/s"]}),
            json!({"command":"/opt/bin/langspan","args":["mcp"]}),
            json!({"command":"/opt/bin/langspan","args":["mcp","--state-dir","/s"],"env":{"K":"v"}}),
            json!({"type":"http","url":"https://example.invalid"}),
            json!({"command":"langspan","args":["mcp","--state-dir","/s"]}),
        ] {
            let existing = json!({"mcpServers":{"langspan": foreign}});
            let error = plan_claude(existing.clone(), Some(&server("/s"))).unwrap_err();
            assert!(error.contains("not written by LangSpan"), "{error}");
            assert_eq!(plan_claude(existing.clone(), None).unwrap(), existing);
        }
        assert!(plan_claude(json!([]), None).is_err());
        assert!(plan_claude(json!({"mcpServers": []}), Some(&server("/s"))).is_err());
        let renamed = McpServer {
            executable: "/opt/bin/langspan-0.2".into(),
            state_dir: "/s".into(),
            hooked_host: None,
        };
        assert!(plan_claude(json!({}), Some(&renamed)).is_err());
    }

    #[test]
    fn codex_block_is_appended_replaced_in_place_and_removed_byte_for_byte() {
        let original = "model = \"o3\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs\"]\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n";
        let installed = plan_codex(Some(original), Some(&server("/s"))).unwrap();
        assert!(installed.starts_with(original));
        assert!(installed.contains("[mcp_servers.langspan]\ncommand = \"/opt/bin/langspan\"\nargs = [\"mcp\", \"--state-dir\", \"/s\"]\n"));
        assert_eq!(
            plan_codex(Some(&installed), Some(&server("/s"))).unwrap(),
            installed
        );
        assert_eq!(parse_codex(&installed).unwrap(), Some(server("/s")));
        // A block the user moved stays where they put it when replaced.
        let moved_block = format!(
            "{}\n[later]\nx = 1\n",
            installed.replace(original, "").trim_start()
        ) + original;
        let replaced = plan_codex(Some(&moved_block), Some(&server("/t"))).unwrap();
        assert!(replaced.starts_with(TOML_BEGIN));
        assert!(replaced.contains("\"/t\"]"));
        assert_eq!(plan_codex(Some(&installed), None).unwrap(), original);
        let fresh = plan_codex(None, Some(&server("/s"))).unwrap();
        assert_eq!(plan_codex(Some(&fresh), None).unwrap(), "");
        assert_eq!(plan_codex(Some(original), None).unwrap(), original);
    }

    #[test]
    fn codex_strings_round_trip_through_escaping() {
        let odd = server("/tmp/it's \"quoted\" \\ here\u{1}");
        let text = plan_codex(None, Some(&odd)).unwrap();
        assert_eq!(parse_codex(&text).unwrap(), Some(odd));
    }

    #[test]
    fn a_foreign_codex_langspan_table_is_refused_without_changes() {
        for foreign in [
            "[mcp_servers.langspan]\ncommand = \"x\"\n",
            "[ mcp_servers . \"langspan\" ]\ncommand = \"x\"\n",
            "[mcp_servers.langspan.env]\nK = \"v\"\n",
            "[mcp_servers]\nlangspan = { command = \"x\" }\n",
            "mcp_servers.langspan.command = \"x\"\n",
            "mcp_servers = { langspan = { command = \"x\" } }\n",
        ] {
            let error = plan_codex(Some(foreign), Some(&server("/s"))).unwrap_err();
            assert!(error.contains("nothing was changed"), "{foreign}: {error}");
            assert!(plan_codex(Some(foreign), None).is_err(), "{foreign}");
        }
        for broken in [
            format!("{TOML_BEGIN}\n[mcp_servers.langspan]\n"),
            format!("{TOML_END}\n"),
            format!("{TOML_BEGIN}\ncommand = 1\n{TOML_END}\n"),
        ] {
            assert!(
                plan_codex(Some(&broken), Some(&server("/s"))).is_err(),
                "{broken}"
            );
        }
        // A bare key after the block would belong to LangSpan's table.
        let extended = plan_codex(None, Some(&server("/s"))).unwrap() + "extra = 1\n";
        assert!(plan_codex(Some(&extended), Some(&server("/s"))).is_err());
        // Another server whose name merely starts the same way is not ours.
        let other = "[mcp_servers.langspan-dev]\ncommand = \"x\"\n";
        assert!(plan_codex(Some(other), Some(&server("/s"))).is_ok());
    }

    #[test]
    fn host_files_follow_each_hosts_documented_layout() {
        let home = Path::new("/h");
        assert_eq!(
            host_files(Vendor::ClaudeCode, home, None),
            HostFiles {
                config_dir: "/h/.claude".into(),
                hooks: "/h/.claude/settings.json".into(),
                mcp: "/h/.claude.json".into(),
            }
        );
        assert_eq!(
            host_files(Vendor::Codex, home, None),
            HostFiles {
                config_dir: "/h/.codex".into(),
                hooks: "/h/.codex/hooks.json".into(),
                mcp: "/h/.codex/config.toml".into(),
            }
        );
        assert_eq!(
            host_files(Vendor::ClaudeCode, home, Some("/c".into())).mcp,
            PathBuf::from("/c/.claude.json")
        );
    }
}
