//! The person's status line (roadmap H2): how many sessions are working in
//! this repository now, and how many writes LangSpan refused today.
//!
//! Refusals are `WriteRefused` events. Their times are on the daemon's
//! monotonic clock, so each daemon start records a `ClockAnchored` event
//! pairing that clock with the wall clock; a refusal is placed on a day by its
//! generation's anchor as it is applied. A refusal of a generation that has no
//! anchor (history written by a build before it) is never counted.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{BRIEF_HORIZON_MS, LivenessStatus, State};

/// Refusals kept for the line, newest first. One more than this many in a
/// day is still shown as this many.
pub const MAX_REFUSALS: usize = 1024;
/// Refusals older than this, before the newest one, are dropped: no day is
/// longer.
pub const REFUSAL_RETENTION_MS: u64 = 48 * 60 * 60 * 1000;
/// Paths one `WriteRefused` event keeps.
pub const REFUSAL_PATHS: usize = 8;

/// The daemon's monotonic clock read together with the wall clock when a
/// generation started.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClockAnchor {
    pub generation: u64,
    /// The daemon's clock (`now_ms` as the core is told it).
    pub clock_ms: u64,
    /// Milliseconds since the Unix epoch.
    pub wall_ms: u64,
}

impl ClockAnchor {
    /// Where a time on this generation's clock falls in wall time.
    pub fn wall(&self, clock_ms: u64) -> u64 {
        if clock_ms >= self.clock_ms {
            self.wall_ms.saturating_add(clock_ms - self.clock_ms)
        } else {
            self.wall_ms.saturating_sub(self.clock_ms - clock_ms)
        }
    }
}

/// Why a write was refused: the kind a `WriteRefused` event records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// Another session holds one of the paths (`denied`).
    Conflict,
    /// A file changed after this session last read it (`denied_stale`).
    StaleBase,
    /// The edit would change lines another session wrote, unread
    /// (`denied_unread`).
    UnreadLines,
    /// A commit would take another live session's changes
    /// (`denied_commit`). Decided by the hook and reported to the daemon.
    CommitHeld,
}

impl RefusalKind {
    /// The kind as its wire name, the key `langspan today` counts it under.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Conflict => "conflict",
            Self::StaleBase => "stale_base",
            Self::UnreadLines => "unread_lines",
            Self::CommitHeld => "commit_held",
        }
    }
}

/// A refusal placed in wall time, for the status line's count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentRefusal {
    pub workspace_id: String,
    pub wall_ms: u64,
}

/// What the status line shows. Counts only, never a path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatusLine {
    /// The registered workspace holding the asked directory, if any.
    pub workspace_id: Option<String>,
    /// Current sessions of that workspace's repository, any worktree.
    pub sessions_here: u32,
    /// Refusals in that repository since the asked wall time.
    pub refusals_today: u32,
}

impl StatusLine {
    /// The one line a person sees.
    pub fn text(&self) -> String {
        let plural =
            |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        format!(
            "langspan · {} here · {} today",
            plural(self.sessions_here, "session", "sessions"),
            plural(self.refusals_today, "refusal", "refusals")
        )
    }
}

/// Record a refusal when its generation is anchored: newest first, those
/// older than `REFUSAL_RETENTION_MS` before it dropped, at most
/// `MAX_REFUSALS`.
pub(crate) fn remember(state: &mut State, workspace_id: &str, at_ms: u64, generation: u64) {
    let Some(anchor) = state
        .clock_anchor
        .filter(|anchor| anchor.generation == generation)
    else {
        return;
    };
    let wall_ms = anchor.wall(at_ms);
    state
        .refusals
        .retain(|kept| kept.wall_ms.saturating_add(REFUSAL_RETENTION_MS) >= wall_ms);
    state.refusals.insert(
        0,
        RecentRefusal {
            workspace_id: workspace_id.to_owned(),
            wall_ms,
        },
    );
    state.refusals.truncate(MAX_REFUSALS);
}

/// The registered workspace holding `root`: the one with the longest root
/// that is `root` or one of its ancestors.
fn workspace_at<'a>(state: &'a State, root: &str) -> Option<&'a str> {
    state
        .workspaces
        .values()
        .filter(|workspace| {
            let base = workspace.root.trim_end_matches('/');
            root == workspace.root
                || root
                    .strip_prefix(base)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|workspace| workspace.root.len())
        .map(|workspace| workspace.workspace_id.as_str())
}

/// The workspaces of the same repository as `workspace_id` (its linked
/// worktrees), or only itself when its repository is not known.
fn repository_of<'a>(state: &'a State, workspace_id: &'a str) -> BTreeSet<&'a str> {
    match state.workspace_git.get(workspace_id) {
        Some(git) => state
            .workspace_git
            .iter()
            .filter(|(_, other)| other.repository_id == git.repository_id)
            .map(|(id, _)| id.as_str())
            .chain([workspace_id])
            .collect(),
        None => BTreeSet::from([workspace_id]),
    }
}

/// The status line for the directory `root`, counting refusals at or after
/// `from_ms` (wall clock). A session counts when it is of this daemon run,
/// a conversation rather than a host subagent, not ended or known to have
/// exited, and active within `BRIEF_HORIZON_MS`.
pub fn status_line(state: &State, root: &str, from_ms: u64, now_ms: u64) -> StatusLine {
    let Some(workspace_id) = workspace_at(state, root) else {
        return StatusLine {
            workspace_id: None,
            sessions_here: 0,
            refusals_today: 0,
        };
    };
    let here = repository_of(state, workspace_id);
    let sessions = state
        .sessions
        .values()
        .filter(|session| {
            here.contains(session.workspace_id.as_str())
                && session.generation == state.generation
                && session.parent.is_none()
                && state
                    .session_liveness
                    .get(&session.session_id)
                    .is_none_or(|liveness| liveness.status != LivenessStatus::Exited)
                && state
                    .activity
                    .get(&session.session_id)
                    .is_some_and(|activity| {
                        activity.generation == state.generation
                            && !activity.ended
                            && now_ms.saturating_sub(activity.last_ms) <= BRIEF_HORIZON_MS
                    })
        })
        .count();
    let refusals = state
        .refusals
        .iter()
        .filter(|refusal| {
            refusal.wall_ms >= from_ms && here.contains(refusal.workspace_id.as_str())
        })
        .count();
    StatusLine {
        workspace_id: Some(workspace_id.to_owned()),
        sessions_here: u32::try_from(sessions).unwrap_or(u32::MAX),
        refusals_today: u32::try_from(refusals).unwrap_or(u32::MAX),
    }
}
