//! A bounded, read-only account of everything this coordinator knows: every
//! local session, every teammate's published session, what is held, what
//! overlaps, and the evidence and health around them. It is the dashboard's
//! one source. Built from projected state only; the daemon adds the facts
//! that only its stores hold (recorded denials, index coverage, uptime).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    Applicability, BRIEF_HORIZON_MS, CheckResult, GraphCoverage, LivenessStatus, QueuedLanding,
    REMOTE_HORIZON_MS, Recipient, State, live_lease,
};

/// Agent rows in one view, at most.
pub const DASHBOARD_AGENTS: usize = 50;
/// Rows in every other list, at most.
pub const DASHBOARD_ITEMS: usize = 20;
/// Files listed per row, at most, for each kind of file list.
pub const DASHBOARD_PATHS: usize = 8;
/// Characters of any one string shown, at most, before an ellipsis.
pub const DASHBOARD_TEXT: usize = 240;
/// Recorded events and receipts searched for denials, counting back from
/// the newest. Older denials are not shown.
pub const DENIAL_WINDOW: u32 = 5000;

/// Encoded bytes each section may take. Together, with the daemon's
/// additions, they stay well inside one 64 KiB transport frame.
const AGENT_BYTES: usize = 20 * 1024;
const SECTION_BYTES: usize = 3 * 1024;
const NOTICE_BYTES: usize = 4 * 1024;
const MESSAGE_BYTES: usize = 4 * 1024;
const WORKSPACE_BYTES: usize = 4 * 1024;
const DENIAL_BYTES: usize = 3 * 1024;

/// What an agent is doing, in the order the dashboard lists them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Holds a current admission (local), or publishes files being edited
    /// now (a teammate heard within the horizon).
    Editing,
    /// Local, with recorded activity within the briefing horizon.
    Active,
    /// A teammate's session in a publication heard within the horizon.
    Published,
    /// Local and registered in this daemon run, with no recent activity.
    Idle,
    /// A teammate's session whose publication is older than the horizon or
    /// was received before this daemon started.
    Unheard,
    /// The host reported the session over.
    Ended,
    /// The session's process was observed to have exited.
    Exited,
    /// Registered in an earlier daemon run and not seen since.
    EarlierRun,
}

impl AgentState {
    fn is_active(self) -> bool {
        matches!(self, Self::Editing | Self::Active | Self::Published)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecentFile {
    pub path: String,
    /// Absent where no comparable time exists: a teammate's file, or one
    /// edited in an earlier daemon run.
    pub age_ms: Option<u64>,
}

/// One agent session, local or a teammate's.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentRow {
    pub local: bool,
    /// The teammate's published name; absent for this machine.
    pub machine: Option<String>,
    /// `claude-code`, `codex`, or what the session ID names.
    pub host: String,
    /// The first characters of the host's session ID.
    pub session: String,
    /// The full logical session ID, for a local session.
    pub session_id: Option<String>,
    /// The local workspace: the session's, or the one that received the
    /// teammate's publication.
    pub workspace_id: String,
    pub branch_ref: Option<String>,
    pub task: Option<String>,
    pub intent: Option<String>,
    pub editing: Vec<String>,
    pub recent: Vec<RecentFile>,
    /// Files left out of `editing` and `recent`.
    pub more_files: u32,
    pub state: AgentState,
    pub liveness: Option<LivenessStatus>,
    /// Local: time since the session's last recorded activity in this run.
    pub active_age_ms: Option<u64>,
    /// A teammate: time since this daemon last received their publication.
    pub heard_age_ms: Option<u64>,
}

/// A file a local session holds a current admission for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EditRow {
    pub workspace_id: String,
    pub path: String,
    pub session_id: String,
    pub expires_in_ms: u64,
    /// Admitted with no recorded read to compare its version with.
    pub base_unknown: bool,
}

/// A file that two or more agents, local or remote, touched recently.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OverlapRow {
    pub workspace_id: String,
    pub path: String,
    /// Labels of the agents: `host prefix`, or `name: host prefix`.
    pub agents: Vec<String>,
    /// At least one of them is editing it now.
    pub editing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChangeRow {
    pub name: String,
    pub kind: String,
    pub change: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NoticeRow {
    pub workspace_id: String,
    pub path: String,
    pub by_session: Option<String>,
    pub changes: Vec<ChangeRow>,
    pub more_changes: u32,
    pub dependents: Vec<String>,
    pub more_dependents: u32,
    pub age_ms: Option<u64>,
}

/// One recorded refusal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DenialRow {
    pub session_id: String,
    pub paths: Vec<String>,
    pub more_paths: u32,
    /// A conflict: the sessions holding the files.
    pub holders: Vec<String>,
    /// Absent when the record carries no time (conflicts) or one from an
    /// earlier daemon run.
    pub age_ms: Option<u64>,
}

/// A denial as the store recovered it, before bounding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenialRecord {
    pub session_id: String,
    pub paths: Vec<String>,
    pub holders: Vec<String>,
    pub at_ms: Option<u64>,
    pub generation: Option<u64>,
}

/// Recent refusals, newest first, from the last `window` recorded events
/// (stale-source refusals) and receipts (conflicts).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Denials {
    pub conflicts: Vec<DenialRow>,
    pub more_conflicts: bool,
    pub stale_refusals: Vec<DenialRow>,
    pub more_stale_refusals: bool,
    pub window: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckRow {
    pub workspace_id: String,
    pub name: String,
    pub result: CheckResult,
    pub tree_oid: String,
    /// A check of combined branches, not of a working tree.
    pub combined: bool,
    /// `stale` once a file was observed to change after it; otherwise
    /// `unknown`: no change was seen, and no tree was compared.
    pub applies: Applicability,
    pub duration_ms: u64,
    pub age_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LandingRow {
    pub workspace_id: String,
    pub onto: String,
    pub commit_oid: String,
    pub tree_oid: String,
    pub check_id: String,
    pub inputs: u32,
}

/// One target's push turn: who holds it and who waits for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PushTurnRow {
    pub workspace_id: String,
    pub onto: String,
    /// The holding landing's request ID; absent when the turn is free.
    pub holder: Option<String>,
    /// Its session; absent for the owner's terminal.
    pub holder_session: Option<String>,
    pub held_ms: Option<u64>,
    /// Landings in line, first first, at most `DASHBOARD_PATHS`.
    pub waiting: Vec<QueuedLanding>,
    pub more_waiting: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MessageRow {
    pub workspace_id: String,
    /// Absent for the owner.
    pub from: Option<String>,
    pub to: Recipient,
    pub text: String,
    pub paths: Vec<String>,
    pub acknowledged: u32,
    /// Withdrawn by its sender or the owner; no longer delivered.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub retracted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NoteRow {
    pub workspace_id: String,
    pub author: Option<String>,
    pub text: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScanRow {
    pub detail: String,
    pub age_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublishRow {
    pub remote: String,
    pub name: String,
    pub enabled: bool,
    pub withdrawn: bool,
    pub share_task: bool,
    /// The last sequence recorded as pushed; zero when none was.
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRow {
    pub workspace_id: String,
    pub root: String,
    pub branch_ref: Option<String>,
    /// Sessions registered in this daemon run.
    pub sessions: u32,
    pub scan_failure: Option<ScanRow>,
    pub publishing: Option<PublishRow>,
    /// Teammates whose publication this workspace holds.
    pub teammates: u32,
    /// Filled by the daemon from the source index; absent when the index
    /// could not be read.
    pub coverage: Option<GraphCoverage>,
}

/// The whole view. Every list is bounded, with the number left out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Dashboard {
    pub generation: u64,
    /// Filled by the daemon.
    pub uptime_ms: Option<u64>,
    /// Agents editing, active or heard from recently, on every machine.
    pub active_agents: u32,
    /// Teammates heard from within the horizon in this daemon run.
    pub teammates_heard: u32,
    /// Teammates whose publication is held, however old.
    pub teammates_known: u32,
    pub agents: Vec<AgentRow>,
    pub more_agents: u32,
    pub editing: Vec<EditRow>,
    pub more_editing: u32,
    pub overlaps: Vec<OverlapRow>,
    pub more_overlaps: u32,
    pub interface_changes: Vec<NoticeRow>,
    pub more_interface_changes: u32,
    /// Filled by the store; absent if it was not read.
    pub denials: Option<Denials>,
    pub checks: Vec<CheckRow>,
    pub more_checks: u32,
    pub landings: Vec<LandingRow>,
    pub more_landings: u32,
    /// Push turns held now or waited for.
    pub push_turns: Vec<PushTurnRow>,
    pub more_push_turns: u32,
    pub messages: Vec<MessageRow>,
    pub more_messages: u32,
    pub notes: Vec<NoteRow>,
    pub more_notes: u32,
    pub workspaces: Vec<WorkspaceRow>,
    pub more_workspaces: u32,
    /// Agent hosts on this machine with no LangSpan install covering them,
    /// by name; at most `MAX_HOSTS` (roadmap A7).
    #[serde(default)]
    pub uninstalled_hosts: Vec<String>,
}

/// Bytes a string takes encoded as JSON, an upper bound.
fn encoded(text: &str) -> usize {
    2 + text
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' => 2,
            0..=0x1f => 6,
            _ => 1,
        })
        .sum::<usize>()
}

fn encoded_all<'a>(texts: impl IntoIterator<Item = &'a String>) -> usize {
    texts.into_iter().map(|text| encoded(text) + 1).sum()
}

fn encoded_opt(text: &Option<String>) -> usize {
    text.as_deref().map_or(4, encoded)
}

/// At most `DASHBOARD_TEXT` characters, cut with an ellipsis.
fn clip(text: &str) -> String {
    if text.chars().count() <= DASHBOARD_TEXT {
        return text.into();
    }
    let mut cut: String = text.chars().take(DASHBOARD_TEXT - 1).collect();
    cut.push('…');
    cut
}

fn clip_opt(text: Option<&String>) -> Option<String> {
    text.map(|text| clip(text))
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The first `limit` rows that fit in `bytes`, and how many were left out.
/// A row that does not fit ends the list, so the order is kept.
fn bounded<T>(
    rows: Vec<T>,
    limit: usize,
    bytes: usize,
    cost: impl Fn(&T) -> usize,
) -> (Vec<T>, u32) {
    let total = rows.len();
    let mut used = 0;
    let kept: Vec<T> = rows
        .into_iter()
        .take(limit)
        .take_while(|row| {
            used += cost(row);
            used <= bytes
        })
        .collect();
    let omitted = count(total - kept.len());
    (kept, omitted)
}

/// `host` and the first characters of the host's session ID, the way a
/// publication names a session.
pub(crate) fn host_and_prefix(session_id: &str) -> (String, String) {
    let (host, id) = session_id.split_once('.').unwrap_or(("agent", session_id));
    (
        host.chars().take(32).collect(),
        id.chars().take(8).collect(),
    )
}

fn paths(items: impl IntoIterator<Item = String>, limit: usize) -> (Vec<String>, usize) {
    let mut omitted = 0;
    let mut kept = Vec::new();
    for item in items {
        if kept.len() < limit {
            kept.push(clip(&item));
        } else {
            omitted += 1;
        }
    }
    (kept, omitted)
}

fn agent_cost(row: &AgentRow) -> usize {
    320 + encoded_opt(&row.machine)
        + encoded(&row.host)
        + encoded(&row.session)
        + encoded_opt(&row.session_id)
        + encoded(&row.workspace_id)
        + encoded_opt(&row.branch_ref)
        + encoded_opt(&row.task)
        + encoded_opt(&row.intent)
        + encoded_all(&row.editing)
        + row
            .recent
            .iter()
            .map(|file| encoded(&file.path) + 32)
            .sum::<usize>()
}

/// One agent's recent contact with one file, a candidate overlap.
struct Touch {
    /// The repository, or the workspace when its repository is unknown.
    group: String,
    path: String,
    /// The full session identity, local or published: what makes two
    /// touches two sessions. Labels are shortened and can collide.
    session: String,
    label: String,
    editing: bool,
}

/// Every local session, then every teammate's published one, current
/// activity first. Also returns the overlap candidates each contributes.
fn agents(state: &State, now_ms: u64) -> (Vec<AgentRow>, Vec<Touch>) {
    let current = |generation: u64| generation == state.generation;
    let age =
        |at_ms: u64, generation: u64| current(generation).then(|| now_ms.saturating_sub(at_ms));
    let mut rows = Vec::new();
    let mut touches = Vec::new();
    for session in state.sessions.values() {
        let id = &session.session_id;
        let activity = state.activity.get(id);
        let in_run = activity.filter(|activity| current(activity.generation));
        let liveness = state
            .session_liveness
            .get(id)
            .map(|liveness| liveness.status);
        let editing: BTreeSet<String> = state
            .leases
            .values()
            .filter(|lease| &lease.session_id == id && live_lease(state, lease, now_ms))
            .flat_map(|lease| lease.paths.iter().cloned())
            .collect();
        let recent: Vec<(String, Option<u64>)> = activity
            .map(|activity| {
                activity
                    .touched
                    .iter()
                    .filter(|touch| !editing.contains(&touch.path))
                    .map(|touch| (touch.path.clone(), age(touch.at_ms, activity.generation)))
                    .collect()
            })
            .unwrap_or_default();
        let active_age_ms = in_run.map(|activity| now_ms.saturating_sub(activity.last_ms));
        let ended = activity.is_some_and(|activity| activity.ended);
        let state_now = if !current(session.generation) {
            AgentState::EarlierRun
        } else if liveness == Some(LivenessStatus::Exited) {
            AgentState::Exited
        } else if ended {
            AgentState::Ended
        } else if !editing.is_empty() {
            AgentState::Editing
        } else if active_age_ms.is_some_and(|age| age <= BRIEF_HORIZON_MS) {
            AgentState::Active
        } else {
            AgentState::Idle
        };
        let (host, prefix) = host_and_prefix(id);
        if matches!(
            state_now,
            AgentState::Editing | AgentState::Active | AgentState::Idle | AgentState::Ended
        ) {
            let group = overlap_group(state, &session.workspace_id);
            // A subagent shares its parent's host session ID; its own part
            // keeps it apart in the label.
            let label = match session
                .parent
                .as_deref()
                .and_then(|parent| id.strip_prefix(parent))
                .and_then(|rest| rest.strip_prefix('.'))
            {
                Some(agent) => format!(
                    "{host} {prefix} subagent {}",
                    agent.chars().take(8).collect::<String>()
                ),
                None => format!("{host} {prefix}"),
            };
            for path in &editing {
                touches.push(Touch {
                    group: group.clone(),
                    path: path.clone(),
                    session: id.clone(),
                    label: label.clone(),
                    editing: true,
                });
            }
            for (path, age) in &recent {
                if age.is_some_and(|age| age <= BRIEF_HORIZON_MS) {
                    touches.push(Touch {
                        group: group.clone(),
                        path: path.clone(),
                        session: id.clone(),
                        label: label.clone(),
                        editing: false,
                    });
                }
            }
        }
        let total_files = editing.len() + recent.len();
        let (editing, _) = paths(editing, DASHBOARD_PATHS);
        let recent: Vec<RecentFile> = recent
            .into_iter()
            .take(DASHBOARD_PATHS)
            .map(|(path, age_ms)| RecentFile {
                path: clip(&path),
                age_ms,
            })
            .collect();
        rows.push(AgentRow {
            local: true,
            machine: None,
            more_files: count(total_files - editing.len() - recent.len()),
            host,
            session: prefix,
            session_id: Some(clip(id)),
            workspace_id: clip(&session.workspace_id),
            branch_ref: clip_opt(
                state
                    .workspace_git
                    .get(&session.workspace_id)
                    .and_then(|git| git.branch_ref.as_ref()),
            ),
            task: clip_opt(activity.and_then(|activity| activity.summary.as_ref())),
            intent: clip_opt(
                state
                    .intents
                    .get(id)
                    .filter(|intent| current(intent.generation))
                    .map(|intent| &intent.summary),
            ),
            editing,
            recent,
            state: state_now,
            liveness,
            active_age_ms,
            heard_age_ms: None,
        });
    }
    for peer in state.remote_peers.values() {
        let heard_age_ms = age(peer.received_ms, peer.generation);
        let fresh = heard_age_ms.is_some_and(|age| age <= REMOTE_HORIZON_MS);
        let group = overlap_group(state, &peer.workspace_id);
        for work in &peer.publication.work {
            let label = format!("{}: {} {}", peer.publication.name, work.host, work.session);
            let identity = format!(
                "remote\n{}\n{}\n{}",
                peer.publication.participant, work.host, work.session
            );
            if fresh {
                for path in &work.editing {
                    touches.push(Touch {
                        group: group.clone(),
                        path: path.clone(),
                        session: identity.clone(),
                        label: label.clone(),
                        editing: true,
                    });
                }
                for path in &work.recent {
                    touches.push(Touch {
                        group: group.clone(),
                        path: path.clone(),
                        session: identity.clone(),
                        label: label.clone(),
                        editing: false,
                    });
                }
            }
            let recent_paths: Vec<&String> = work
                .recent
                .iter()
                .filter(|path| !work.editing.contains(path))
                .collect();
            let (editing, _) = paths(work.editing.iter().cloned(), DASHBOARD_PATHS);
            let recent: Vec<RecentFile> = recent_paths
                .iter()
                .take(DASHBOARD_PATHS)
                .map(|path| RecentFile {
                    path: clip(path),
                    age_ms: None,
                })
                .collect();
            rows.push(AgentRow {
                local: false,
                machine: Some(clip(&peer.publication.name)),
                host: clip(&work.host),
                session: clip(&work.session),
                session_id: None,
                workspace_id: clip(&peer.workspace_id),
                branch_ref: clip_opt(work.branch_ref.as_ref()),
                task: clip_opt(work.summary.as_ref()),
                intent: None,
                more_files: count(
                    work.editing.len() + recent_paths.len() - editing.len() - recent.len(),
                ),
                editing,
                recent,
                state: match (fresh, work.editing.is_empty()) {
                    (true, false) => AgentState::Editing,
                    (true, true) => AgentState::Published,
                    (false, _) => AgentState::Unheard,
                },
                liveness: None,
                active_age_ms: None,
                heard_age_ms,
            });
        }
    }
    rows.sort_by(|a, b| {
        let key = |row: &AgentRow| {
            (
                row.state,
                row.active_age_ms.or(row.heard_age_ms).unwrap_or(u64::MAX),
                !row.local,
                row.machine.clone(),
                row.session.clone(),
                row.session_id.clone(),
            )
        };
        key(a).cmp(&key(b))
    });
    (rows, touches)
}

/// Linked worktrees of one repository share their files' relative paths;
/// a teammate's paths are relative to the same repository.
fn overlap_group(state: &State, workspace_id: &str) -> String {
    match state.workspace_git.get(workspace_id) {
        Some(git) => format!("repository:{}", git.repository_id),
        None => format!("workspace:{workspace_id}"),
    }
}

fn overlaps(state: &State, touches: Vec<Touch>) -> Vec<OverlapRow> {
    // The workspace shown for a group is the first registered one in it.
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for id in state.workspaces.keys().rev() {
        names.insert(overlap_group(state, id), id.clone());
    }
    for peer in state.remote_peers.values() {
        names
            .entry(overlap_group(state, &peer.workspace_id))
            .or_insert_with(|| peer.workspace_id.clone());
    }
    // Sessions are counted by identity, and each shown by its label.
    type Sessions = BTreeMap<String, String>;
    let mut files: BTreeMap<(String, String), (Sessions, bool)> = BTreeMap::new();
    for touch in touches {
        let entry = files.entry((touch.group, touch.path)).or_default();
        entry.0.insert(touch.session, touch.label);
        entry.1 |= touch.editing;
    }
    let mut rows: Vec<OverlapRow> = files
        .into_iter()
        .filter(|(_, (agents, _))| agents.len() >= 2)
        .map(|((group, path), (agents, editing))| OverlapRow {
            workspace_id: clip(names.get(&group).map_or("", String::as_str)),
            path: clip(&path),
            agents: {
                let mut labels: Vec<&String> = agents.values().collect();
                labels.sort();
                labels
                    .into_iter()
                    .take(DASHBOARD_PATHS)
                    .map(|a| clip(a))
                    .collect()
            },
            editing,
        })
        .collect();
    rows.sort_by(|a, b| {
        (!a.editing, usize::MAX - a.agents.len(), &a.path).cmp(&(
            !b.editing,
            usize::MAX - b.agents.len(),
            &b.path,
        ))
    });
    rows
}

pub(crate) fn dashboard(state: &State, now_ms: u64) -> Dashboard {
    let current = |generation: u64| generation == state.generation;
    let age =
        |at_ms: u64, generation: u64| current(generation).then(|| now_ms.saturating_sub(at_ms));

    let (all_agents, touches) = agents(state, now_ms);
    let active_agents = count(
        all_agents
            .iter()
            .filter(|row| row.state.is_active())
            .count(),
    );
    let (agents, more_agents) = bounded(all_agents, DASHBOARD_AGENTS, AGENT_BYTES, agent_cost);

    let mut editing: Vec<EditRow> = state
        .leases
        .values()
        .filter(|lease| live_lease(state, lease, now_ms))
        .flat_map(|lease| {
            lease.paths.iter().map(move |path| EditRow {
                workspace_id: clip(&lease.workspace_id),
                path: clip(path),
                session_id: clip(&lease.session_id),
                expires_in_ms: lease.expires_at_ms.saturating_sub(now_ms),
                base_unknown: lease.base_unknown.contains(path),
            })
        })
        .collect();
    editing.sort_by(|a, b| (&a.workspace_id, &a.path).cmp(&(&b.workspace_id, &b.path)));
    let (editing, more_editing) = bounded(editing, DASHBOARD_ITEMS, SECTION_BYTES, |row| {
        120 + encoded(&row.workspace_id) + encoded(&row.path) + encoded(&row.session_id)
    });

    let (overlaps, more_overlaps) = bounded(
        overlaps(state, touches),
        DASHBOARD_ITEMS,
        SECTION_BYTES,
        |row| 80 + encoded(&row.workspace_id) + encoded(&row.path) + encoded_all(&row.agents),
    );

    let mut notices: Vec<_> = state.interface_notices.values().flatten().collect();
    notices.sort_by_key(|notice| (!current(notice.generation), u64::MAX - notice.at_ms));
    let notices = notices
        .into_iter()
        .map(|notice| {
            let (dependents, more) = paths(notice.dependents.iter().cloned(), DASHBOARD_PATHS);
            NoticeRow {
                workspace_id: clip(&notice.workspace_id),
                path: clip(&notice.path),
                by_session: clip_opt(notice.by_session.as_ref()),
                changes: notice
                    .changes
                    .iter()
                    .take(DASHBOARD_PATHS)
                    .map(|change| ChangeRow {
                        name: clip(&change.name),
                        kind: clip(&change.kind),
                        change: clip(&change.change),
                    })
                    .collect(),
                more_changes: count(notice.changes.len().saturating_sub(DASHBOARD_PATHS)),
                dependents,
                more_dependents: count(more),
                age_ms: age(notice.at_ms, notice.generation),
            }
        })
        .collect();
    let (interface_changes, more_interface_changes) =
        bounded(notices, DASHBOARD_ITEMS, NOTICE_BYTES, |row| {
            160 + encoded(&row.workspace_id)
                + encoded(&row.path)
                + encoded_opt(&row.by_session)
                + row
                    .changes
                    .iter()
                    .map(|change| {
                        40 + encoded(&change.name) + encoded(&change.kind) + encoded(&change.change)
                    })
                    .sum::<usize>()
                + encoded_all(&row.dependents)
        });

    let mut checks: Vec<_> = state.checks.values().flatten().collect();
    checks.sort_by_key(|check| (!current(check.generation), u64::MAX - check.recorded_ms));
    let checks = checks
        .into_iter()
        .map(|check| CheckRow {
            workspace_id: clip(&check.workspace_id),
            name: clip(&check.name),
            result: check.result.clone(),
            tree_oid: clip(&check.tree_oid),
            combined: !check.inputs.is_empty(),
            applies: if check.changed_since {
                Applicability::Stale
            } else {
                Applicability::Unknown
            },
            duration_ms: check.duration_ms,
            age_ms: age(check.recorded_ms, check.generation),
        })
        .collect();
    let (checks, more_checks) = bounded(checks, DASHBOARD_ITEMS, SECTION_BYTES, |row| {
        200 + encoded(&row.workspace_id) + encoded(&row.name) + encoded(&row.tree_oid)
    });

    let landings = state
        .landings
        .values()
        .flatten()
        .map(|landing| LandingRow {
            workspace_id: clip(&landing.workspace_id),
            onto: clip(&landing.onto),
            commit_oid: clip(&landing.commit_oid),
            tree_oid: clip(&landing.tree_oid),
            check_id: clip(&landing.check_id),
            inputs: count(landing.inputs.len()),
        })
        .collect();
    let (landings, more_landings) = bounded(landings, DASHBOARD_ITEMS, SECTION_BYTES, |row| {
        120 + encoded(&row.workspace_id)
            + encoded(&row.onto)
            + encoded(&row.commit_oid)
            + encoded(&row.tree_oid)
            + encoded(&row.check_id)
    });

    let keys: BTreeSet<&String> = state
        .turns
        .iter()
        .filter(|(_, turn)| turn.expires_at_ms > now_ms)
        .map(|(key, _)| key)
        .chain(state.turn_queues.keys())
        .collect();
    let push_turns = keys
        .into_iter()
        .filter_map(|key| {
            let holder = state
                .turns
                .get(key)
                .filter(|turn| turn.expires_at_ms > now_ms);
            let waiting = crate::line(&crate::waiting_for(state, key, now_ms), now_ms);
            let (workspace_id, onto) = holder
                .map(|turn| (&turn.workspace_id, &turn.onto))
                .or_else(|| {
                    state
                        .turn_queues
                        .get(key)
                        .and_then(|queue| queue.first())
                        .map(|waiter| (&waiter.workspace_id, &waiter.onto))
                })?;
            if holder.is_none() && waiting.is_empty() {
                return None;
            }
            let more_waiting = count(waiting.len().saturating_sub(DASHBOARD_PATHS));
            Some(PushTurnRow {
                workspace_id: clip(workspace_id),
                onto: clip(onto),
                holder: holder.map(|turn| clip(&turn.turn_id)),
                holder_session: holder.and_then(|turn| clip_opt(turn.session_id.as_ref())),
                held_ms: holder.map(|turn| now_ms.saturating_sub(turn.taken_ms)),
                waiting: waiting
                    .into_iter()
                    .take(DASHBOARD_PATHS)
                    .map(|waiter| QueuedLanding {
                        ticket: clip(&waiter.ticket),
                        session_id: clip_opt(waiter.session_id.as_ref()),
                        ..waiter
                    })
                    .collect(),
                more_waiting,
            })
        })
        .collect();
    let (push_turns, more_push_turns) = bounded(
        push_turns,
        DASHBOARD_ITEMS,
        SECTION_BYTES,
        |row: &PushTurnRow| {
            120 + encoded(&row.workspace_id)
                + encoded(&row.onto)
                + row.holder.as_deref().map_or(0, encoded)
                + row.holder_session.as_deref().map_or(0, encoded)
                + row
                    .waiting
                    .iter()
                    .map(|waiter| {
                        100 + encoded(&waiter.ticket)
                            + waiter.session_id.as_deref().map_or(0, encoded)
                    })
                    .sum::<usize>()
        },
    );

    // Held messages, then those withdrawn while still in the history.
    let withdrawn = state
        .message_history
        .values()
        .flatten()
        .filter(|message| message.retracted);
    let messages = state
        .messages
        .values()
        .flatten()
        .chain(withdrawn)
        .map(|message| MessageRow {
            workspace_id: clip(&message.workspace_id),
            from: clip_opt(message.from.as_ref()),
            to: match &message.to {
                Recipient::Session { session_id } => Recipient::Session {
                    session_id: clip(session_id),
                },
                other => other.clone(),
            },
            text: clip(&message.text),
            paths: paths(message.paths.iter().cloned(), DASHBOARD_PATHS).0,
            acknowledged: count(message.acknowledged.len()),
            retracted: message.retracted,
        })
        .collect();
    let (messages, more_messages) = bounded(messages, DASHBOARD_ITEMS, MESSAGE_BYTES, |row| {
        160 + encoded(&row.workspace_id)
            + encoded_opt(&row.from)
            + match &row.to {
                Recipient::Session { session_id } => encoded(session_id),
                _ => 0,
            }
            + encoded(&row.text)
            + encoded_all(&row.paths)
    });

    let notes = state
        .notes
        .values()
        .flatten()
        .map(|note| NoteRow {
            workspace_id: clip(&note.workspace_id),
            author: clip_opt(note.author.as_ref()),
            text: clip(&note.text),
            paths: paths(note.paths.iter().cloned(), DASHBOARD_PATHS).0,
        })
        .collect();
    let (notes, more_notes) = bounded(notes, DASHBOARD_ITEMS, SECTION_BYTES, |row| {
        80 + encoded(&row.workspace_id)
            + encoded_opt(&row.author)
            + encoded(&row.text)
            + encoded_all(&row.paths)
    });

    let workspaces = state
        .workspaces
        .values()
        .map(|workspace| {
            let id = &workspace.workspace_id;
            WorkspaceRow {
                workspace_id: clip(id),
                root: clip(&workspace.root),
                branch_ref: clip_opt(
                    state
                        .workspace_git
                        .get(id)
                        .and_then(|git| git.branch_ref.as_ref()),
                ),
                sessions: count(
                    state
                        .sessions
                        .values()
                        .filter(|session| {
                            &session.workspace_id == id && current(session.generation)
                        })
                        .count(),
                ),
                scan_failure: state.scan_failures.get(id).map(|failure| ScanRow {
                    detail: clip(&failure.detail),
                    age_ms: age(failure.at_ms, failure.generation),
                }),
                publishing: state.remote_links.get(id).map(|link| PublishRow {
                    remote: clip(&link.remote),
                    name: clip(&link.name),
                    enabled: link.enabled,
                    withdrawn: link.withdrawn,
                    share_task: link.share_task,
                    sequence: link.sequence,
                }),
                teammates: count(
                    state
                        .remote_peers
                        .values()
                        .filter(|peer| &peer.workspace_id == id)
                        .count(),
                ),
                coverage: None,
            }
        })
        .collect();
    let (workspaces, more_workspaces) =
        bounded(workspaces, DASHBOARD_ITEMS, WORKSPACE_BYTES, |row| {
            360 + encoded(&row.workspace_id)
                + encoded(&row.root)
                + encoded_opt(&row.branch_ref)
                + row
                    .scan_failure
                    .as_ref()
                    .map_or(0, |scan| encoded(&scan.detail))
                + row.publishing.as_ref().map_or(0, |publishing| {
                    encoded(&publishing.remote) + encoded(&publishing.name)
                })
        });

    let participants = |fresh: bool| {
        count(
            state
                .remote_peers
                .values()
                .filter(|peer| {
                    !fresh
                        || age(peer.received_ms, peer.generation)
                            .is_some_and(|age| age <= REMOTE_HORIZON_MS)
                })
                .map(|peer| &peer.publication.participant)
                .collect::<BTreeSet<_>>()
                .len(),
        )
    };

    Dashboard {
        generation: state.generation,
        uptime_ms: None,
        active_agents,
        teammates_heard: participants(true),
        teammates_known: participants(false),
        agents,
        more_agents,
        editing,
        more_editing,
        overlaps,
        more_overlaps,
        interface_changes,
        more_interface_changes,
        denials: None,
        checks,
        more_checks,
        landings,
        more_landings,
        push_turns,
        more_push_turns,
        messages,
        more_messages,
        notes,
        more_notes,
        workspaces,
        more_workspaces,
        uninstalled_hosts: state.uninstalled_hosts.keys().cloned().collect(),
    }
}

/// Bound the denials a store recovered, newest first, for a dashboard.
/// `more_*` says the store found more than it passed.
pub fn dashboard_denials(
    state: &State,
    now_ms: u64,
    conflicts: Vec<DenialRecord>,
    more_conflicts: bool,
    stale_refusals: Vec<DenialRecord>,
    more_stale_refusals: bool,
) -> Denials {
    let rows = |records: Vec<DenialRecord>| -> (Vec<DenialRow>, bool) {
        let rows: Vec<DenialRow> = records
            .into_iter()
            .map(|record| {
                let (paths, more) = paths(record.paths, 4);
                DenialRow {
                    session_id: clip(&record.session_id),
                    paths,
                    more_paths: count(more),
                    holders: record.holders.iter().take(4).map(|h| clip(h)).collect(),
                    age_ms: match (record.at_ms, record.generation) {
                        (Some(at_ms), Some(generation)) if generation == state.generation => {
                            Some(now_ms.saturating_sub(at_ms))
                        }
                        _ => None,
                    },
                }
            })
            .collect();
        let (rows, omitted) = bounded(rows, DASHBOARD_ITEMS / 2, DENIAL_BYTES, |row| {
            100 + encoded(&row.session_id) + encoded_all(&row.paths) + encoded_all(&row.holders)
        });
        (rows, omitted > 0)
    };
    let (conflicts, cut_conflicts) = rows(conflicts);
    let (stale_refusals, cut_refusals) = rows(stale_refusals);
    Denials {
        conflicts,
        more_conflicts: more_conflicts || cut_conflicts,
        stale_refusals,
        more_stale_refusals: more_stale_refusals || cut_refusals,
        window: DENIAL_WINDOW,
    }
}
