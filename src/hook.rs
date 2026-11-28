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
use crate::file_state::{file_content, file_state, version_of};
pub(crate) use crate::hosts::port::REASON_LIMIT;
pub use crate::hosts::port::{Config, Identities, LifecycleKind, Phase, Reply, ShellCall};
use crate::ids::{file_key, valid_host_id};
pub use crate::ids::{user_workspace_id, wire_id, workspace_request_id};
pub use crate::input::read_stdin_until;
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
#[cfg(test)]
use render::interface_change_text;
pub use render::{BRIEFING, UNCOORDINATED, compose_context, render_briefing, render_code_briefing};
#[cfg(test)]
use render::{
    CHECK_RESULT, CHECKS_LIMIT, CODE_SHARE, CODE_SKIPPED, COMPARISON_LIMIT, INFLIGHT_PARTIAL,
    MORE_FAILURES, PEER_LINE_LIMIT, PEERS_MINIMUM, PEERS_NOT_SHOWN, definitions_text,
    has_peer_content, inflight_line, render_checks, render_comparison, render_peers_placed,
};
use render::{CONFLICT, STALE_SOURCE, UNREAD_LINES, has_content, unplaced_keys};
use render::{
    compose_context_placed, compose_context_within, reply_context_bytes, shows_all_inflight,
};
pub(crate) use render::{
    interface_impact_line, render_briefing_placed, session_label, shown_notice_ids,
};
use shell_hooks::shell_looked_at;
pub use shell_hooks::{SHELL_TOOL, shell_operation};
pub use tool_call::*;
pub use workspace::*;

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
                self.unreachable.set(true);
                Failure::Transport(coverage(&error), error.to_string())
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

/// The coverage a hook reports for a call that did not reach the daemon.
fn coverage(error: &client::ClientError) -> &'static str {
    match error {
        client::ClientError::TimedOut(_) => "timeout",
        _ => "unavailable",
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::{client::ClientError, coverage};

    #[test]
    fn only_a_timed_out_call_is_reported_as_a_timeout() {
        // A path that names a deadline no longer reads as one.
        let path = "coordination unavailable at /tmp/deadline/daemon.sock: refused".to_owned();
        assert_eq!(coverage(&ClientError::TimedOut("late".into())), "timeout");
        assert_eq!(coverage(&ClientError::Unavailable(path)), "unavailable");
        assert_eq!(coverage(&ClientError::Refused("x".into())), "unavailable");
        assert_eq!(coverage(&ClientError::Corrupt("x".into())), "unavailable");
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
