//! Shared native-hook lifecycle. Vendor adapters only parse host envelopes.
use crate::{
    client,
    protocol::{Request, Response},
    sha256,
};
use langspan_core::{Command, Conflict, Outcome, ToolResult};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
mod admit;
mod deny;
mod lifecycle;
mod observe;
mod prompt;
mod reads;
mod rename_hold;
mod shell_hooks;
mod tool_call;
mod workspace;
pub use crate::briefing::BRIEFING_LIMIT;
use crate::briefing::render;
pub use crate::file_state::{MAX_VERSION_BYTES, file_state};
use crate::file_state::{file_content, version_of};
pub use crate::ids::{file_key, user_workspace_id, valid_host_id, wire_id, workspace_request_id};
pub use crate::input::{MAX_EVENT_BYTES, read_stdin_until};
pub use crate::text::age;
pub(crate) use crate::text::{SUMMARY_LIMIT, clean, quoted, truncate};
pub(crate) use admit::*;
pub(crate) use deny::*;
pub use lifecycle::*;
use observe::*;
#[cfg(test)]
use prompt::PROMPT_SCAN_BYTES;
use prompt::PromptLines;
pub use prompt::{
    prompt_identifiers, prompt_line, prompt_line_digests, prompt_qualifiers, prompt_summary,
    prompt_target_paths,
};
pub use reads::*;
pub use render::{
    BRIEFING, CHECK_RESULT, CONFLICT, FLAKY_LABEL, STALE_SOURCE, UNCOORDINATED, UNREAD_LINES,
    about_note, checkout_text, compose_context, has_content, note_about, render_briefing,
    render_code_briefing, uncoordinated_text, unplaced_keys,
};
#[cfg(test)]
use render::{
    CHECKS_LIMIT, CODE_SHARE, CODE_SKIPPED, COMPARISON_LIMIT, INFLIGHT_PARTIAL, MORE_FAILURES,
    PEER_LINE_LIMIT, PEERS_MINIMUM, PEERS_NOT_SHOWN, definitions_text, has_peer_content,
    inflight_line, render_checks, render_comparison, render_peers_placed,
};
use render::{
    compose_context_placed, compose_context_within, reply_context_bytes, shows_all_inflight,
};
pub(crate) use render::{
    interface_change_text, interface_impact_line, render_briefing_placed, session_label,
    shown_notice_ids,
};
use shell_hooks::shell_looked_at;
pub use shell_hooks::{SHELL_TOOL, shell_operation};
pub use tool_call::*;
pub use workspace::*;
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
    pub vendor: crate::install::Vendor,
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
        vendor: crate::install::Vendor,
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
            presence_window_ms: crate::install::Presence {
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

pub fn identities_for(source: &str, host_session: &str, tool_use_id: &str) -> Identities {
    let fields = [source, host_session, tool_use_id];
    Identities {
        session_id: format!("{source}.{host_session}"),
        operation_id: wire_id("op", &fields),
        register: wire_id("register", &fields),
        admit: wire_id("admit", &fields),
        complete: wire_id("complete", &fields),
    }
}

/// A host's prompt or turn ID that LangSpan uses: a valid host ID of at
/// most 64 bytes. It keys a redelivered prompt's requests, and binds a
/// between-step watcher and a stop to their prompt (issue #214).
pub(crate) fn prompt_event_id(id: Option<&str>) -> Option<&str> {
    id.filter(|id| valid_host_id(id) && id.len() <= 64)
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
/// What a write states about each file: its edits, and whole contents.
type Writes<'a> = (&'a [(String, crate::regions::Edit)], &'a [(String, String)]);

pub struct Operation {
    pub source: String,
    pub host_session: String,
    pub tool_use_id: String,
    pub tool: String,
    pub paths: Vec<String>,
    pub input_fingerprint: String,
    /// What the call changes in each file, where the host input says. A
    /// file with no entry is held whole.
    pub edits: Vec<(String, crate::regions::Edit)>,
    pub phase: Phase,
    /// The host accepts context on a completed tool call, so news that
    /// arrived since the last prompt can reach the model mid-turn.
    pub mid_turn_context: bool,
    /// For a host subagent, the LangSpan session ID of the conversation that
    /// started it. `host_session` is then the subagent's own identity.
    pub parent: Option<String>,
    /// The host reports no reads (Codex): a file admitted with no recorded
    /// version is recorded as seen, as it is on disk now, so the session's
    /// later edits of it are compared ([ADR 0027](../docs/adr/0027-turn-and-batch-events.md)).
    pub snapshot_unknown: bool,
    /// For a shell call, the command and the directory it runs in.
    pub shell: Option<ShellCall>,
    /// The whole new content of a file the call creates or deletes outside
    /// `edits` (a Codex `Add File`, or empty for a `Delete File`), for the
    /// break check and the rename hold before the write
    /// ([ADR 0029](../docs/adr/0029-break-alerts-at-write-time.md)).
    pub contents: Vec<(String, String)>,
    /// The host gives the model `additionalContext` from a pre-tool reply
    /// (Claude Code, Codex). The break check runs before the write only
    /// then: an alert the model never reads would still use up its notice.
    pub pre_tool_context: bool,
}

/// What the pre-tool run of a write does about breaks (ADR 0029).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakCheck {
    Off,
    /// Check now and tell in the pre-tool reply.
    Now,
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

struct Session<'a> {
    config: &'a Config,
    deadline: Instant,
    root: &'a Path,
    source: &'a str,
    parent: Option<&'a str>,
    headless: bool,
    /// A call did not reach the daemon, or its answer did not arrive in
    /// time: what this run reported may not be recorded.
    unreachable: std::cell::Cell<bool>,
    /// A call of this run was answered: the daemon has noted the session's
    /// hook run from it (roadmap J12).
    reached: std::cell::Cell<bool>,
}

enum Failure {
    Transport(&'static str, String),
    Rejected(String),
}

impl Session<'_> {
    fn call(&self, request_id: String, command: Command) -> Result<Response, Failure> {
        self.call_by(request_id, command, self.deadline)
    }

    /// `call`, answered `by` then: a step before the admission leaves it time.
    fn call_by(&self, id: String, command: Command, by: Instant) -> Result<Response, Failure> {
        let request = Request::new(id, command);
        let response =
            client::call_until(&self.config.state_dir, &request, by).map_err(|error| {
                let text = error.to_string();
                let coverage = if text.contains("deadline") || text.contains("timed out") {
                    "timeout"
                } else {
                    "unavailable"
                };
                self.unreachable.set(true);
                Failure::Transport(coverage, text)
            })?;
        match &response.outcome {
            Outcome::Error { code, message } if code == "coordination_unavailable" => {
                self.unreachable.set(true);
                Err(Failure::Transport("unavailable", message.clone()))
            }
            Outcome::Error { code, message } => {
                Err(Failure::Rejected(format!("{code}: {message}")))
            }
            _ => {
                self.reached.set(true);
                Ok(response)
            }
        }
    }

    fn attach(&self, ids: &Identities) -> Result<(), Failure> {
        let root = self.root.to_string_lossy().into_owned();
        self.call(
            wire_id(
                "workspace-op",
                &[&self.config.workspace_id, &root, &ids.operation_id],
            ),
            Command::WorkspaceRegister {
                workspace_id: self.config.workspace_id.clone(),
                root,
            },
        )?;
        self.call(
            ids.register.clone(),
            Command::SessionRegister {
                session_id: ids.session_id.clone(),
                workspace_id: self.config.workspace_id.clone(),
                parent: self.parent.map(str::to_owned),
                headless: self.headless,
            },
        )?;
        // A setting other than the default is sent before anything the
        // session looked at, so that nothing is shown while it is off. A
        // failure fails the attachment: what follows is not recorded.
        if !self.default_presence() {
            self.set_presence(ids)?;
        }
        Ok(())
    }

    fn default_presence(&self) -> bool {
        self.config.presence && self.config.presence_window_ms == langspan_core::PRESENCE_WINDOW_MS
    }

    /// This session's presence setting, from its hook's options. The request
    /// ID is the call's own: a setting changed back is recorded again.
    fn set_presence(&self, ids: &Identities) -> Result<(), Failure> {
        self.call(
            wire_id("presence", &[self.source, &ids.operation_id]),
            Command::SetPresence {
                session_id: ids.session_id.clone(),
                shown: self.config.presence,
                window_ms: self.config.presence_window_ms,
            },
        )?;
        Ok(())
    }

    /// Paths a search or a shell command looked at, as presence. Nothing is
    /// sent with presence off.
    fn look(
        &self,
        ids: &Identities,
        kind: langspan_core::PresenceKind,
        paths: Vec<String>,
    ) -> Result<usize, Failure> {
        if !self.config.presence || paths.is_empty() {
            return Ok(0);
        }
        let count = paths.len();
        self.call(
            wire_id("look", &[self.source, &ids.session_id, &ids.operation_id]),
            Command::ObservePresence {
                session_id: ids.session_id.clone(),
                kind,
                paths,
            },
        )?;
        Ok(count)
    }

    fn report_liveness(&self, ids: &Identities) {
        if Instant::now() >= self.deadline {
            return;
        }
        let reported = match crate::process::host_parent(self.source) {
            Some(process) => self.call(
                wire_id("liveness", &[self.source, &ids.operation_id]),
                Command::ObserveSessionLiveness {
                    session_id: ids.session_id.clone(),
                    process,
                    status: langspan_core::LivenessStatus::Alive,
                },
            ),
            // A run that told the daemon nothing else (a shell command that
            // writes no file by its parse, say) is still a sign of work. A
            // session the daemon does not know is not registered for it.
            None if !self.reached.get() => match self.call(
                wire_id("seen", &[self.source, &ids.operation_id]),
                Command::SessionSeen {
                    session_id: ids.session_id.clone(),
                },
            ) {
                Err(Failure::Rejected(message))
                    if message.starts_with("session_not_found")
                        || message.starts_with("stale_session") =>
                {
                    return;
                }
                other => other,
            },
            None => return,
        };
        if let Err(failure) = reported {
            let diagnostic = failure_reply(failure);
            eprintln!(
                "{}",
                json!({"event":"liveness_unavailable", "coverage":diagnostic.coverage, "detail":diagnostic.detail})
            );
        }
    }
}

#[cfg(test)]
mod briefing_text;

#[cfg(test)]
mod code_briefing_text;

/// Review comment 4711375989: the items a briefing's text placed in full,
/// for each way the renderer leaves one out: a peer line that does not
/// fit, a path past a line's cut, a task cut short, presence without room,
/// a notice without room, a stale-read list cut short, and peer lines left
/// out for a check result. Each placed item is in the text; each other one
/// is among the keys the hook has forgotten.
#[cfg(test)]
mod placed_briefing;
