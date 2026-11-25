//! Which agent hosts are on this machine and whether a LangSpan install
//! covers each (roadmap A7). Detection only: it reads a host's
//! configuration directory entry and LangSpan's own entries in the host's
//! hook and MCP files, bounded as `install` reads them, and never writes.
//! `langspan hosts`, `doctor` and the daemon's hourly check all use it.

use crate::{install, install::Vendor, user_install};
use serde::Serialize;
use std::{
    io,
    path::{Path, PathBuf},
};

/// The home a user-level install targets. An explicit `--home` ignores the
/// environment entirely (HOME, CLAUDE_CONFIG_DIR, CODEX_HOME,
/// GEMINI_CLI_HOME, LANGSPAN_STATE_DIR, XDG_STATE_HOME), so a run against a
/// test or a spare home can never reach the real one.
#[derive(Debug, Clone)]
pub struct UserHome {
    pub home: PathBuf,
    explicit: bool,
}

impl UserHome {
    pub fn new(option: Option<PathBuf>) -> Result<Self, crate::Error> {
        let explicit = option.is_some();
        let home = option
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .ok_or("choose --home; HOME is not set")?;
        Ok(Self {
            home: std::path::absolute(home)?,
            explicit,
        })
    }

    /// Whether it was named with `--home`.
    pub fn explicit(&self) -> bool {
        self.explicit
    }

    fn env(&self, name: &str) -> Option<PathBuf> {
        (!self.explicit)
            .then(|| std::env::var_os(name))
            .flatten()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }

    /// Where `vendor` keeps its user files in this home.
    pub fn files(&self, vendor: Vendor) -> user_install::HostFiles {
        let variable = match vendor {
            Vendor::ClaudeCode => Some("CLAUDE_CONFIG_DIR"),
            Vendor::Codex => Some("CODEX_HOME"),
            Vendor::Cursor => None,
            Vendor::Gemini => Some("GEMINI_CLI_HOME"),
        };
        user_install::host_files(
            vendor,
            &self.home,
            variable.and_then(|variable| self.env(variable)),
        )
    }

    /// The state directory `install` uses with `option` (`--state-dir`).
    pub fn state_dir(&self, option: Option<String>) -> PathBuf {
        option
            .map(PathBuf::from)
            .or_else(|| self.env("LANGSPAN_STATE_DIR"))
            .or_else(|| self.env("XDG_STATE_HOME").map(|p| p.join("langspan")))
            .unwrap_or_else(|| self.home.join(".local/state/langspan"))
    }
}

/// Whether the host's configuration directory exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    Present,
    Absent,
    /// It could not be looked at: never taken for absent.
    Unknown(String),
}

/// What user-level LangSpan install the host has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInstall {
    None,
    /// Hooks for every event and the MCP server, for this state directory,
    /// with an executable that exists.
    Current,
    /// Partly installed, from an earlier version, or its executable is gone.
    Outdated,
    /// Installed for another state directory: another coordinator's.
    OtherStateDir,
    /// Its files could not be read.
    Unknown,
}

/// One host, as `langspan hosts --json` gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostReport {
    pub name: &'static str,
    pub config_dir: PathBuf,
    /// `null` when the directory could not be looked at.
    pub present: Option<bool>,
    pub user_install: UserInstall,
    /// The command that installs it, where one is proposed.
    pub next_step: Option<String>,
    /// Why `present` or `user_install` is unknown.
    pub error: Option<String>,
}

impl HostReport {
    /// Present with no user-level install at all.
    pub fn not_installed(&self) -> bool {
        self.present == Some(true) && self.user_install == UserInstall::None
    }
}

/// Hosts proposed for install without the owner naming them: those
/// `install` chooses by itself. Gemini CLI is chosen only by name, since
/// `~/.gemini` is also made by other tools.
pub fn proposed(vendor: Vendor) -> bool {
    vendor != Vendor::Gemini
}

fn presence(dir: &Path) -> Presence {
    match std::fs::metadata(dir) {
        Ok(metadata) if metadata.is_dir() => Presence::Present,
        Ok(_) => Presence::Absent,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Presence::Absent,
        Err(error) => Presence::Unknown(format!("{}: {error}", dir.display())),
    }
}

/// The user-level install of a present host, read from LangSpan's own
/// entries in its hook and MCP files.
fn user_install_of(
    vendor: Vendor,
    files: &user_install::HostFiles,
    state_dir: &Path,
) -> Result<UserInstall, String> {
    let hooks: Vec<install::UserRegistration>;
    let missing: Vec<&str>;
    match install::read(&files.hooks).map_err(|e| e.to_string())? {
        None => {
            hooks = Vec::new();
            missing = Vec::new();
        }
        Some(settings) => {
            let ours = |hook: &serde_json::Value| install::is_user_managed_for(vendor, hook);
            hooks = install::managed_hooks(&settings, ours)
                .iter()
                .filter_map(|hook| install::parse_user_hook(vendor, hook))
                .collect();
            missing = install::missing_events(vendor, &settings, ours);
        }
    }
    let server = match vendor {
        Vendor::ClaudeCode | Vendor::Cursor | Vendor::Gemini => {
            match install::read(&files.mcp).map_err(|e| e.to_string())? {
                None => None,
                Some(config) => config
                    .get("mcpServers")
                    .and_then(|servers| servers.get(user_install::SERVER_NAME))
                    .and_then(user_install::parse_claude_entry),
            }
        }
        Vendor::Codex => install::read_text(&files.mcp)
            .map_err(|e| e.to_string())
            .and_then(|text| user_install::parse_codex(text.as_deref().unwrap_or("")))?,
    };
    let Some(first) = hooks.first() else {
        return Ok(if server.is_some() {
            UserInstall::Outdated
        } else {
            UserInstall::None
        });
    };
    let same = install::same_directory;
    if !same(&first.state_dir, state_dir) {
        return Ok(UserInstall::OtherStateDir);
    }
    let server_current = server
        .as_ref()
        .is_some_and(|server| same(&server.state_dir, state_dir) && server.executable.is_file());
    Ok(
        if missing.is_empty() && first.executable.is_file() && server_current {
            UserInstall::Current
        } else {
            UserInstall::Outdated
        },
    )
}

/// The install command for `vendor` in `home` for `state_dir`, naming
/// `--home` and `--state-dir` only where `install` would not choose them.
pub fn install_command(vendor: Vendor, home: &UserHome, state_dir: &Path) -> String {
    let mut command = format!("langspan install --hosts {} --apply", vendor.name());
    if home.explicit {
        command.push_str(" --home ");
        command.push_str(&install::quote(&home.home.to_string_lossy()));
    }
    if !install::same_directory(state_dir, &home.state_dir(None)) {
        command.push_str(" --state-dir ");
        command.push_str(&install::quote(&state_dir.to_string_lossy()));
    }
    command
}

/// One host's report.
pub fn inspect(vendor: Vendor, home: &UserHome, state_dir: &Path) -> HostReport {
    let files = home.files(vendor);
    let mut report = HostReport {
        name: vendor.name(),
        config_dir: files.config_dir.clone(),
        present: None,
        user_install: UserInstall::Unknown,
        next_step: None,
        error: None,
    };
    match presence(&files.config_dir) {
        Presence::Unknown(error) => report.error = Some(error),
        Presence::Absent => {
            report.present = Some(false);
            report.user_install = UserInstall::None;
        }
        Presence::Present => {
            report.present = Some(true);
            match user_install_of(vendor, &files, state_dir) {
                Ok(state) => report.user_install = state,
                Err(error) => report.error = Some(error),
            }
        }
    }
    let wants = matches!(
        report.user_install,
        UserInstall::None | UserInstall::Outdated
    );
    if report.present == Some(true) && wants && proposed(vendor) {
        report.next_step = Some(install_command(vendor, home, state_dir));
    }
    report
}

/// Every known host's report.
pub fn report(home: &UserHome, state_dir: &Path) -> Vec<HostReport> {
    Vendor::ALL
        .into_iter()
        .map(|vendor| inspect(vendor, home, state_dir))
        .collect()
}

/// The hosts to tell the owner about: proposed, present with no user-level
/// install, and with no project install in any of `roots` (the registered
/// workspaces) for this state directory.
pub fn not_installed(reports: &[HostReport], roots: &[PathBuf], state_dir: &Path) -> Vec<Vendor> {
    reports
        .iter()
        .filter(|report| report.not_installed())
        .filter_map(|report| Vendor::from_name(report.name))
        .filter(|vendor| proposed(*vendor))
        .filter(|vendor| {
            !roots
                .iter()
                .any(|root| crate::hook::install_at(*vendor, state_dir, root).is_some())
        })
        .collect()
}

/// One daemon check: the hosts to hold as not installed, and why any host
/// could not be inspected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub not_installed: Vec<String>,
    pub errors: Vec<String>,
}

/// What the daemon's check reports, given the hosts already `held` as not
/// installed. A held host whose inspection is unknown stays held: neither
/// an install nor a removal is established, so its notice is not cleared.
pub fn observe(
    reports: &[HostReport],
    roots: &[PathBuf],
    state_dir: &Path,
    held: &[String],
) -> Observation {
    let mut not_installed: Vec<String> = not_installed(reports, roots, state_dir)
        .into_iter()
        .map(|vendor| vendor.name().to_owned())
        .collect();
    let mut errors = Vec::new();
    for report in reports {
        let unknown = report.present.is_none() || report.user_install == UserInstall::Unknown;
        if !unknown {
            continue;
        }
        errors.push(format!(
            "{}: {}",
            report.name,
            report.error.as_deref().unwrap_or("could not be inspected")
        ));
        if held.iter().any(|host| host == report.name)
            && !not_installed.iter().any(|host| host == report.name)
        {
            not_installed.push(report.name.to_owned());
        }
    }
    Observation {
        not_installed,
        errors,
    }
}

/// The command that previews an install of `host` for the coordinator at
/// `state_dir`. The state directory is always named: the shell it is run
/// in may choose another by default, and hooks installed for another
/// coordinator do not coordinate with this one. `host` is a name the core
/// accepted: lower-case letters and hyphens, so it needs no quoting.
pub fn preview_command(host: &str, state_dir: &Path) -> String {
    let state_dir = std::path::absolute(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    format!(
        "langspan install --hosts {host} --state-dir {}",
        install::quote(&state_dir.to_string_lossy())
    )
}

/// The line `status` shows for a host held as not installed by the
/// coordinator at `state_dir`.
pub fn status_line(host: &str, state_dir: &Path) -> String {
    format!(
        "hosts: {host} has a configuration directory but no LangSpan hooks; run `{}` to preview, then add --apply",
        preview_command(host, state_dir)
    )
}
