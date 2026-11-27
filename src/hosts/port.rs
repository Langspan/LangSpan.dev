//! The hook port: the configuration a host adapter hands the shared hook
//! lifecycle, and the reply and call shapes that cross between them.
use super::vendor::Vendor;
use langspan_core::ToolResult;
use std::{path::PathBuf, time::Duration};

pub(crate) const REASON_LIMIT: usize = 600;
#[derive(Debug, Clone)]
pub struct Config {
    pub state_dir: PathBuf,
    pub workspace_id: String,
    pub root: PathBuf,
    pub ttl_ms: u64,
    pub budget: Duration,
    /// Record the first line of the owner's prompt as this session's task,
    /// so peers can be told what it is working on.
    pub share_prompt: bool,
    /// At each prompt, tell the model where names the prompt mentions are
    /// defined. The prompt text is used for one local index query only.
    pub code_briefing: bool,
    /// Show peers where this session looks: its reads, the path scope of
    /// its searches and the paths its shell commands read (roadmap A13).
    /// Off (`--presence off`), none of them is shown; reads are still
    /// recorded as versions for the session's own stale checks.
    pub presence: bool,
    /// How old a peer's presence this session is shown, in milliseconds
    /// (`--presence-minutes`).
    pub presence_window_ms: u64,
    /// A whole read of a large indexed file is refused once with the
    /// file's outline (`--outline-reads on`, roadmap A11). Off by default.
    pub outline_reads: bool,
    /// The host whose hook this is.
    pub vendor: Vendor,
    /// Installed once per user rather than per project: `workspace_id` and
    /// `root` are unset and come from each event's working directory.
    pub user_level: bool,
}

impl Config {
    /// A project-level hook's configuration with the `langspan hook`
    /// command's defaults for every option. Callers that set an option
    /// override it with `Config { field: value, ..Config::new(..) }`, so a
    /// new option does not break them.
    pub fn new(
        state_dir: PathBuf,
        workspace_id: impl Into<String>,
        root: PathBuf,
        vendor: Vendor,
    ) -> Self {
        Self {
            state_dir,
            workspace_id: workspace_id.into(),
            root,
            ttl_ms: 30_000,
            budget: Duration::from_millis(1500),
            share_prompt: true,
            code_briefing: true,
            presence: true,
            presence_window_ms: Presence {
                shown: true,
                minutes: None,
            }
            .window_ms(),
            outline_reads: false,
            vendor,
            user_level: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub stdout: String,
    pub coverage: &'static str,
    pub detail: String,
}

impl Reply {
    pub fn none(coverage: &'static str, detail: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            coverage,
            detail: detail.into(),
        }
    }
}

/// Logical identities for one host tool call, encoded for the request-ID grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identities {
    pub session_id: String,
    pub operation_id: String,
    pub register: String,
    pub admit: String,
    pub complete: String,
}

#[derive(Debug, Clone, Copy)]
pub enum Phase {
    Pre,
    Post(ToolResult),
    /// The host reported that the session read these files. Nothing is
    /// admitted; the versions become what the session is known to have seen.
    Read,
    /// A shell command is about to run. `paths` are the files it will write
    /// when the command parsed; `unknown` says why it did not.
    ShellPre {
        unknown: Option<&'static str>,
        /// Started in the background: it may outlive its call.
        background: bool,
    },
    /// A shell command has finished: what it changed is found by comparison.
    ShellPost,
}

/// The command of a shell call, as the host sent it.
#[derive(Debug, Clone)]
pub struct ShellCall {
    pub command: String,
    pub cwd: Option<String>,
}

/// Session lifecycle events shared by the hosts: the points where a session
/// is discovered and where peer context can reach the model before it acts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleKind {
    Start,
    /// The prompt as the host submitted it. What of it is recorded (its
    /// summary) or looked up in the source index (its names, never
    /// recorded) is decided in `lifecycle` from the settings of the
    /// workspace the event resolves to, so a linked worktree's own opt-outs
    /// hold (`resolve_workspace`). It never leaves this process otherwise.
    Prompt {
        prompt: Option<String>,
    },
    End,
}

/// The presence options of an installed hook command (roadmap A13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presence {
    /// False: the session's reads, searches and shell paths are never shown.
    pub shown: bool,
    /// How old a peer's presence the session is shown; `None` is the
    /// default, `PRESENCE_MINUTES`.
    pub minutes: Option<u64>,
}

/// The presence window, in minutes, when none is given.
pub const PRESENCE_MINUTES: u64 = langspan_core::PRESENCE_WINDOW_MS / 60_000;

impl Default for Presence {
    fn default() -> Self {
        Self {
            shown: true,
            minutes: None,
        }
    }
}

impl Presence {
    /// How old a peer's presence the session is shown, in milliseconds.
    pub fn window_ms(self) -> u64 {
        self.minutes.unwrap_or(PRESENCE_MINUTES) * 60_000
    }

    /// `--presence-minutes`: 1 to 30, the briefing horizon. The default
    /// is written as no option at all.
    pub fn minutes(value: &str) -> Result<Option<u64>, String> {
        match value.parse::<u64>() {
            Ok(PRESENCE_MINUTES) => Ok(None),
            Ok(minutes @ 1..=30) => Ok(Some(minutes)),
            _ => Err("--presence-minutes must be a whole number of minutes from 1 to 30".into()),
        }
    }
}
