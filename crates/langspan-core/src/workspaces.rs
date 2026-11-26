//! A workspace's lifecycle after registration (roadmap G6): list the
//! workspaces, rebind one to a moved or replacement checkout, and remove one
//! the owner no longer uses. The event log keeps every earlier record; only
//! the derived state changes.

use serde::{Deserialize, Serialize};

use crate::{
    CoreError, Decision, Event, LeaseRelation, Outcome, State, Workspace, error, lease_relation,
    normalized_workspace_links, validate_id, validate_workspace, waiting_for,
};

/// One registered workspace as `workspace list` shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSummary {
    pub workspace_id: String,
    pub root: String,
    /// Whether the root is a directory now. Filled in by the daemon, which
    /// can look; the pure core always says false.
    pub root_exists: bool,
    /// Sessions of the current daemon generation that have not ended.
    pub sessions_current: u32,
    pub notes: u32,
    pub messages_pending: u32,
    pub checks: u32,
    pub auto_check: bool,
    pub remote_sharing: bool,
}

/// What a move kept: everything the workspace knew, by count.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceKept {
    pub notes: u32,
    pub messages_pending: u32,
    pub checks: u32,
    pub landings: u32,
}

/// What a removal ends. Sessions are listed by ID, at most
/// `MAX_ENDING_SESSIONS`; `more_sessions` counts the rest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEnding {
    pub sessions: Vec<String>,
    #[serde(default, skip_serializing_if = "crate::is_zero")]
    pub more_sessions: u32,
    pub intents: u32,
    pub notes: u32,
    pub messages_pending: u32,
    /// The name of the automatic check that stops, if one is on.
    pub auto_check: Option<String>,
}

pub const MAX_ENDING_SESSIONS: usize = 32;

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn sessions_of<'a>(state: &'a State, workspace_id: &'a str) -> impl Iterator<Item = &'a str> {
    state
        .sessions
        .values()
        .filter(move |session| session.workspace_id == workspace_id)
        .map(|session| session.session_id.as_str())
}

fn current(state: &State, session_id: &str) -> bool {
    state
        .sessions
        .get(session_id)
        .is_some_and(|session| session.generation == state.generation)
        && !state
            .activity
            .get(session_id)
            .is_some_and(|activity| activity.ended)
}

/// Workspaces in one `workspace list` page at most. The daemon's store may
/// end a page sooner to keep it within a frame (a root can be 4 KiB).
pub const MAX_WORKSPACE_PAGE: usize = 100;

/// One page of workspaces in ID order after the workspace ID `after`, and
/// the cursor for the next page when more remain. A cursor need not name a
/// workspace still registered: the page starts after it in ID order.
pub(crate) fn list(
    state: &State,
    after: Option<&str>,
) -> Result<(Vec<WorkspaceSummary>, Option<String>), CoreError> {
    use std::ops::Bound;
    let start = match after {
        Some(after) => {
            validate_id(after, "workspace list cursor")?;
            Bound::Excluded(after)
        }
        None => Bound::Unbounded,
    };
    let mut rest = state
        .workspaces
        .range::<str, _>((start, Bound::Unbounded))
        .map(|(_, workspace)| workspace);
    let page: Vec<WorkspaceSummary> = rest
        .by_ref()
        .take(MAX_WORKSPACE_PAGE)
        .map(|workspace| summary(state, workspace))
        .collect();
    let next = rest
        .next()
        .and(page.last())
        .map(|last| last.workspace_id.clone());
    Ok((page, next))
}

fn summary(state: &State, workspace: &Workspace) -> WorkspaceSummary {
    let id = workspace.workspace_id.as_str();
    WorkspaceSummary {
        workspace_id: id.to_owned(),
        root: workspace.root.clone(),
        root_exists: false,
        sessions_current: count(
            sessions_of(state, id)
                .filter(|session| current(state, session))
                .count(),
        ),
        notes: count(state.notes.get(id).map_or(0, Vec::len)),
        messages_pending: count(state.messages.get(id).map_or(0, Vec::len)),
        checks: count(state.checks.get(id).map_or(0, Vec::len)),
        auto_check: state.auto_checks.contains_key(id),
        remote_sharing: state.remote_links.get(id).is_some_and(|link| link.enabled),
    }
}

fn registered<'a>(state: &'a State, workspace_id: &str) -> Result<&'a Workspace, CoreError> {
    validate_id(workspace_id, "workspace ID")?;
    state.workspaces.get(workspace_id).ok_or_else(|| {
        error(
            "workspace_not_found",
            "No workspace is registered under this ID; `langspan workspace list` shows them",
        )
    })
}

/// Why the workspace cannot be moved or removed now, if it cannot: a
/// session holds an admission there, a landing holds or waits for a push
/// turn, or its work is published. A disabled publication's ref must be gone
/// too: the daemon withdraws it through the root's configured remote, so a
/// move before then would send the withdrawal to the new root's remote,
/// which need not be the one the ref was published to.
fn refuse_while_in_use(state: &State, workspace_id: &str, now_ms: u64) -> Result<(), CoreError> {
    if let Some(lease) = state.leases.values().find(|lease| {
        lease.workspace_id == workspace_id
            && lease_relation(state, lease, now_ms) == LeaseRelation::Current
    }) {
        return Err(error(
            "workspace_busy",
            &format!(
                "Session {} holds an admission in workspace {workspace_id} until it completes or expires; wait for it, then retry with a new request ID",
                lease.session_id
            ),
        ));
    }
    if let Some(turn) = state
        .turns
        .values()
        .find(|turn| turn.workspace_id == workspace_id)
    {
        return Err(error(
            "workspace_busy",
            &format!(
                "A landing holds the push turn for {} in workspace {workspace_id}; wait for it to finish (`langspan land-queue {workspace_id} --onto {}`), then retry with a new request ID",
                turn.onto, turn.onto
            ),
        ));
    }
    if let Some(waiter) = state
        .turn_queues
        .keys()
        .flat_map(|key| waiting_for(state, key, now_ms))
        .find(|waiter| waiter.workspace_id == workspace_id)
    {
        return Err(error(
            "workspace_busy",
            &format!(
                "A landing waits for the push turn for {} in workspace {workspace_id}; wait for it or stop it (`langspan land-cancel`), then retry with a new request ID",
                waiter.onto
            ),
        ));
    }
    if let Some(link) = state.remote_links.get(workspace_id) {
        if link.enabled {
            return Err(error(
                "remote_sharing_on",
                &format!(
                    "Workspace {workspace_id} publishes its work to teammates; run `langspan remote disable {workspace_id}` first, so the published ref is removed, then retry with a new request ID"
                ),
            ));
        }
        if !link.withdrawn {
            return Err(error(
                "remote_sharing_on",
                &format!(
                    "Publication for workspace {workspace_id} is off, but its ref is not yet removed from the remote; `langspan doctor` shows its progress. Retry with a new request ID once it is gone"
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn decide_move(
    state: &State,
    workspace_id: &str,
    root: &str,
    verified_overlaps: &[String],
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let old = registered(state, workspace_id)?;
    if old.root == root {
        return Err(error(
            "invalid_move",
            "The workspace is already registered at this root; nothing to move",
        ));
    }
    refuse_while_in_use(state, workspace_id, now_ms)?;
    let links = normalized_workspace_links(verified_overlaps)?;
    let workspace = Workspace {
        workspace_id: workspace_id.to_owned(),
        root: root.to_owned(),
        verified_overlaps: links,
    };
    validate_rebinding(state, &workspace)?;
    let kept = WorkspaceKept {
        notes: count(state.notes.get(workspace_id).map_or(0, Vec::len)),
        messages_pending: count(state.messages.get(workspace_id).map_or(0, Vec::len)),
        checks: count(state.checks.get(workspace_id).map_or(0, Vec::len)),
        landings: count(state.landings.get(workspace_id).map_or(0, Vec::len)),
    };
    Ok(Decision {
        events: vec![Event::WorkspaceMoved {
            workspace: workspace.clone(),
            from_root: old.root.clone(),
        }],
        outcome: Outcome::WorkspaceMoved {
            workspace,
            from_root: old.root.clone(),
            kept,
        },
    })
}

/// The new root is checked as a new registration would be, as though the
/// workspace were not registered yet: another workspace at or around it is
/// refused unless its distinct worktree boundary was verified.
fn validate_rebinding(state: &State, workspace: &Workspace) -> Result<(), CoreError> {
    let mut without = state.clone();
    without.workspaces.remove(&workspace.workspace_id);
    validate_workspace(&without, workspace)
}

pub(crate) fn apply_moved(
    state: &mut State,
    workspace: &Workspace,
    from_root: &str,
) -> Result<(), CoreError> {
    let old = registered(state, &workspace.workspace_id)?;
    if old.root != from_root || old.root == workspace.root {
        return Err(error(
            "invalid_event",
            "A move names the workspace's current root and a different new one",
        ));
    }
    validate_rebinding(state, workspace)?;
    let id = workspace.workspace_id.as_str();
    state.workspaces.insert(id.to_owned(), workspace.clone());
    // Other workspaces verified their boundary against the old root.
    for other in state.workspaces.values_mut() {
        if other.workspace_id != id {
            other.verified_overlaps.retain(|linked| linked != id);
        }
    }
    // File versions read or written under the old root say nothing about
    // the new one: a session's next edit of an existing file has an unknown
    // base until it reads it again.
    let sessions: Vec<String> = sessions_of(state, id).map(str::to_owned).collect();
    for session in &sessions {
        if let Some(activity) = state.activity.get_mut(session) {
            activity.known.clear();
        }
    }
    // What the old checkout's scans said; the next scan of the new root says
    // it again.
    state.workspace_git.remove(id);
    state.scan_failures.remove(id);
    state.scan_coverage.remove(id);
    state.checkouts.remove(id);
    state.checkout_runs.remove(id);
    state.uncoordinated.remove(id);
    // A switch back to an earlier root is still a new registration.
    next_revision(state, id);
    Ok(())
}

pub(crate) fn decide_remove(
    state: &State,
    workspace_id: &str,
    apply: bool,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    registered(state, workspace_id)?;
    refuse_while_in_use(state, workspace_id, now_ms)?;
    let sessions: Vec<&str> = sessions_of(state, workspace_id)
        .filter(|session| current(state, session))
        .collect();
    let ending = WorkspaceEnding {
        sessions: sessions
            .iter()
            .take(MAX_ENDING_SESSIONS)
            .map(|session| (*session).to_owned())
            .collect(),
        more_sessions: count(sessions.len().saturating_sub(MAX_ENDING_SESSIONS)),
        intents: count(
            state
                .intents
                .values()
                .filter(|intent| intent.workspace_id == workspace_id)
                .count(),
        ),
        notes: count(state.notes.get(workspace_id).map_or(0, Vec::len)),
        messages_pending: count(state.messages.get(workspace_id).map_or(0, Vec::len)),
        auto_check: state
            .auto_checks
            .get(workspace_id)
            .map(|check| check.name.clone()),
    };
    Ok(Decision {
        events: if apply {
            vec![Event::WorkspaceRemoved {
                workspace_id: workspace_id.to_owned(),
            }]
        } else {
            Vec::new()
        },
        outcome: Outcome::WorkspaceRemoval {
            workspace_id: workspace_id.to_owned(),
            applied: apply,
            ending,
        },
    })
}

/// Everything derived for the workspace and its sessions leaves the state.
/// A disabled publication's link stays, already withdrawn, so that the ID
/// registered and published again continues its sequence for teammates.
/// The removal advances the registration revision, so a later registration
/// under the same ID and root is still a new lifetime to the source index.
pub(crate) fn apply_removed(state: &mut State, workspace_id: &str) -> Result<(), CoreError> {
    registered(state, workspace_id)?;
    let id = workspace_id;
    let sessions: Vec<String> = sessions_of(state, id).map(str::to_owned).collect();
    state.workspaces.remove(id);
    for other in state.workspaces.values_mut() {
        other.verified_overlaps.retain(|linked| linked != id);
    }
    for session in &sessions {
        state.sessions.remove(session);
        state.intents.remove(session);
        state.session_liveness.remove(session);
        state.activity.remove(session);
        state.check_notices.remove(session);
        state.retracted_notices.remove(session);
        state.presence.remove(session);
        state.first_writes.remove(session);
        state.unsettled.remove(session);
    }
    // A withdrawal names its workspace; one owed to a session of another
    // workspace goes too, so it cannot outlive the messages it withdraws.
    state.retracted_notices.retain(|_, notices| {
        notices.retain(|notice| notice.workspace_id != id);
        !notices.is_empty()
    });
    state.intents.retain(|_, intent| intent.workspace_id != id);
    state
        .adoptions
        .retain(|_, adoption| adoption.workspace_id != id);
    state.leases.retain(|_, lease| lease.workspace_id != id);
    state.turns.retain(|_, turn| turn.workspace_id != id);
    state
        .turn_queues
        .retain(|_, waiters| waiters.iter().all(|waiter| waiter.workspace_id != id));
    state.remote_peers.retain(|_, peer| peer.workspace_id != id);
    state.refusals.retain(|refusal| refusal.workspace_id != id);
    state.workspace_git.remove(id);
    state.conflict_waits.remove(id);
    state.checks.remove(id);
    state.landings.remove(id);
    state.interface_notices.remove(id);
    state.renames.remove(id);
    state.notes.remove(id);
    state.messages.remove(id);
    state.message_history.remove(id);
    state.dropped_messages.remove(id);
    state.scan_failures.remove(id);
    state.scan_coverage.remove(id);
    state.checkouts.remove(id);
    state.checkout_runs.remove(id);
    state.uncoordinated.remove(id);
    state.auto_checks.remove(id);
    state.test_learning.remove(id);
    state.kept_tests.remove(id);
    state.write_runs.remove(id);
    // `writes` is kept: it is a generation that never goes back, so a run
    // taken before the removal cannot match a count after it.
    next_revision(state, id);
    Ok(())
}

/// A move or a removal starts a new registration revision for the ID.
fn next_revision(state: &mut State, workspace_id: &str) {
    let revision = state
        .workspace_revisions
        .entry(workspace_id.to_owned())
        .or_default();
    *revision = revision.saturating_add(1);
}
