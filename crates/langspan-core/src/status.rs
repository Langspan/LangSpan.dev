//! The status reply. By default it is a bounded view of what matters now:
//! workspaces, live or recent sessions and the admissions still open, with
//! closed admissions only counted. The whole state, history included, is read
//! as a sequence of records in pages, each resumed after an opaque cursor.

use crate::{
    AutoCheck, CheckNotice, CheckRecord, CheckoutChange, CoreError, Intent, InterfaceNotice,
    Landing, Lease, LeaseStatus, LivenessStatus, Message, Note, RemoteLink, RemotePeer,
    ScanCoverage, ScanFailure, Session, SessionActivity, SessionLiveness, State, Turn, TurnWaiter,
    Workspace, WorkspaceGit, error,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::{iter, mem};

/// A session with nothing open is shown while its last activity is at most
/// this old.
pub const STATUS_RECENT_MS: u64 = 60 * 60 * 1000;

/// What the bounded status view left out, so that a reader can tell a quiet
/// coordinator from a trimmed answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatusSummary {
    /// Admissions no longer open (completed, expired or invalidated), per
    /// session. They are not listed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub closed_admissions: BTreeMap<String, u64>,
    /// Closed admissions of sessions left out of `closed_admissions` to keep
    /// the answer within one frame.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub other_closed: u64,
    /// Sessions not shown: nothing open and no activity in the last
    /// `STATUS_RECENT_MS`, or left out to keep the answer within one frame.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_sessions: u64,
    /// Open admissions left out because they alone exceed one frame.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_open: u64,
    /// Parts of the state left out to keep the answer within one frame, by
    /// field name. Every record is still readable with `status --all`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<String>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// The bounded view: every open admission, the sessions that hold one, were
/// active within `STATUS_RECENT_MS`, are seen alive, or registered in this
/// generation and have done nothing yet, and the rest of the current state.
/// Closed admissions are counted per session in the summary.
pub fn status_view(state: &State, now_ms: u64) -> (State, StatusSummary) {
    // Exhaustive, so that a new field must be placed in the view or out of it.
    let State {
        generation,
        workspaces,
        sessions,
        intents,
        leases,
        session_liveness,
        workspace_git,
        activity,
        remote_links,
        remote_peers,
        checks,
        landings,
        turns,
        turn_queues,
        interface_notices,
        notes,
        messages,
        scan_failures,
        scan_coverage,
        checkouts,
        auto_checks,
        check_notices,
    } = state;
    let mut summary = StatusSummary::default();
    let mut open = BTreeMap::new();
    let mut holding = BTreeSet::new();
    for (id, lease) in leases {
        if lease.status == LeaseStatus::Active {
            holding.insert(lease.session_id.as_str());
            open.insert(id.clone(), lease.clone());
        } else {
            *summary
                .closed_admissions
                .entry(lease.session_id.clone())
                .or_default() += 1;
        }
    }
    let shown = |id: &str, session: &Session| {
        holding.contains(id)
            || session_liveness.get(id).is_some_and(|liveness| {
                liveness.status == LivenessStatus::Alive && liveness.generation == *generation
            })
            || match activity.get(id) {
                Some(activity) => activity.last_ms.saturating_add(STATUS_RECENT_MS) >= now_ms,
                None => session.generation == *generation,
            }
    };
    let shown_sessions: BTreeMap<String, Session> = sessions
        .iter()
        .filter(|(id, session)| shown(id, session))
        .map(|(id, session)| (id.clone(), session.clone()))
        .collect();
    summary.omitted_sessions = (sessions.len() - shown_sessions.len()) as u64;
    let of_shown = |id: &String| shown_sessions.contains_key(id);
    let view = State {
        generation: *generation,
        workspaces: workspaces.clone(),
        intents: keep(intents, of_shown),
        leases: open,
        session_liveness: keep(session_liveness, of_shown),
        workspace_git: workspace_git.clone(),
        activity: keep(activity, of_shown),
        remote_links: remote_links.clone(),
        remote_peers: remote_peers.clone(),
        checks: checks.clone(),
        landings: landings.clone(),
        turns: turns.clone(),
        turn_queues: turn_queues.clone(),
        interface_notices: interface_notices.clone(),
        notes: notes.clone(),
        messages: messages.clone(),
        scan_failures: scan_failures.clone(),
        scan_coverage: scan_coverage.clone(),
        checkouts: checkouts.clone(),
        auto_checks: auto_checks.clone(),
        check_notices: keep(check_notices, of_shown),
        sessions: shown_sessions.clone(),
    };
    (view, summary)
}

fn keep<V: Clone>(
    map: &BTreeMap<String, V>,
    wanted: impl Fn(&String) -> bool,
) -> BTreeMap<String, V> {
    map.iter()
        .filter(|(key, _)| wanted(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

type Trim = fn(&mut State) -> bool;

/// Parts of the view left out, in this order, while it does not fit: the
/// bulkiest histories first, current coordination last.
const TRIMS: [(&str, Trim); 12] = [
    ("activity.known", |state| {
        state
            .activity
            .values_mut()
            .fold(false, |any, activity| any | take(&mut activity.known))
    }),
    ("checks", |state| take(&mut state.checks)),
    ("landings", |state| take(&mut state.landings)),
    ("interface_notices", |state| {
        take(&mut state.interface_notices)
    }),
    ("checkouts", |state| take(&mut state.checkouts)),
    ("notes", |state| take(&mut state.notes)),
    ("remote_peers", |state| take(&mut state.remote_peers)),
    ("activity.touched", |state| {
        state
            .activity
            .values_mut()
            .fold(false, |any, activity| any | take(&mut activity.touched))
    }),
    ("activity.alerted", |state| {
        state
            .activity
            .values_mut()
            .fold(false, |any, activity| any | take(&mut activity.alerted))
    }),
    ("check_notices", |state| take(&mut state.check_notices)),
    ("messages", |state| take(&mut state.messages)),
    ("workspace_git", |state| take(&mut state.workspace_git)),
];

fn take<T: Default + PartialEq>(value: &mut T) -> bool {
    mem::take(value) != T::default()
}

/// Leave parts of the view out until `fits` accepts it, each counted or named
/// in the summary: the histories in `TRIMS`, then sessions with nothing open,
/// least recently active first, then per-session counts, and open admissions
/// only if they alone are larger than a frame. Workspaces, links, turns,
/// automatic checks and scan health are never left out.
pub fn shrink_status(
    view: &mut State,
    summary: &mut StatusSummary,
    mut fits: impl FnMut(&State, &StatusSummary) -> bool,
) {
    if fits(view, summary) {
        return;
    }
    for (name, trim) in TRIMS {
        if trim(view) {
            summary.omitted.push(name.into());
            if fits(view, summary) {
                return;
            }
        }
    }
    let holding: BTreeSet<String> = view
        .leases
        .values()
        .map(|lease| lease.session_id.clone())
        .collect();
    let mut idle: Vec<(u64, String)> = view
        .sessions
        .keys()
        .filter(|id| !holding.contains(*id))
        .map(|id| {
            let last = view.activity.get(id).map_or(0, |activity| activity.last_ms);
            (last, id.clone())
        })
        .collect();
    // Newest last, so that the oldest are popped from the front in chunks.
    idle.sort();
    idle.reverse();
    // A chunk is an eighth of what is left, so that a large view needs few
    // measurements and a small one loses no more than it must.
    while !fits(view, summary) && !idle.is_empty() {
        for _ in 0..idle.len().div_ceil(8) {
            let Some((_, id)) = idle.pop() else { break };
            view.sessions.remove(&id);
            view.intents.remove(&id);
            view.session_liveness.remove(&id);
            view.activity.remove(&id);
            view.check_notices.remove(&id);
            summary.omitted_sessions += 1;
        }
    }
    while !fits(view, summary) && !summary.closed_admissions.is_empty() {
        for _ in 0..summary.closed_admissions.len().div_ceil(8) {
            let Some((_, count)) = summary.closed_admissions.pop_last() else {
                break;
            };
            summary.other_closed += count;
        }
    }
    while !fits(view, summary) && !view.leases.is_empty() {
        for _ in 0..view.leases.len().div_ceil(8) {
            if view.leases.pop_last().is_some() {
                summary.omitted_open += 1;
            }
        }
    }
}

/// One entry of the state, as `status --all` pages it. `key` is the entry's
/// key in its state map; list entries are given in their order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StatusRecord {
    Workspace {
        key: String,
        workspace: Workspace,
    },
    Session {
        key: String,
        session: Session,
    },
    Intent {
        key: String,
        intent: Intent,
    },
    Lease {
        key: String,
        lease: Lease,
    },
    SessionLiveness {
        key: String,
        liveness: SessionLiveness,
    },
    WorkspaceGit {
        key: String,
        git: WorkspaceGit,
    },
    Activity {
        key: String,
        activity: SessionActivity,
    },
    RemoteLink {
        key: String,
        link: RemoteLink,
    },
    RemotePeer {
        key: String,
        peer: RemotePeer,
    },
    Check {
        key: String,
        check: CheckRecord,
    },
    Landing {
        key: String,
        landing: Landing,
    },
    Turn {
        key: String,
        turn: Turn,
    },
    TurnWaiter {
        key: String,
        waiter: TurnWaiter,
    },
    InterfaceNotice {
        key: String,
        notice: InterfaceNotice,
    },
    Note {
        key: String,
        note: Note,
    },
    Message {
        key: String,
        message: Message,
    },
    ScanFailure {
        key: String,
        failure: ScanFailure,
    },
    ScanCoverage {
        key: String,
        coverage: ScanCoverage,
    },
    Checkout {
        key: String,
        change: CheckoutChange,
    },
    AutoCheck {
        key: String,
        check: AutoCheck,
    },
    CheckNotice {
        key: String,
        notice: CheckNotice,
    },
}

/// Record kinds in page order. The cursor names one of these.
const KINDS: [&str; 21] = [
    "workspace",
    "session",
    "intent",
    "lease",
    "session_liveness",
    "workspace_git",
    "activity",
    "remote_link",
    "remote_peer",
    "check",
    "landing",
    "turn",
    "turn_waiter",
    "interface_notice",
    "note",
    "message",
    "scan_failure",
    "scan_coverage",
    "checkout",
    "auto_check",
    "check_notice",
];

impl StatusRecord {
    /// Place this record in `state`, as it was in the state it was read from.
    /// Applying every page in order rebuilds the state's maps and lists.
    pub fn apply(self, state: &mut State) {
        match self {
            Self::Workspace { key, workspace } => drop(state.workspaces.insert(key, workspace)),
            Self::Session { key, session } => drop(state.sessions.insert(key, session)),
            Self::Intent { key, intent } => drop(state.intents.insert(key, intent)),
            Self::Lease { key, lease } => drop(state.leases.insert(key, lease)),
            Self::SessionLiveness { key, liveness } => {
                drop(state.session_liveness.insert(key, liveness))
            }
            Self::WorkspaceGit { key, git } => drop(state.workspace_git.insert(key, git)),
            Self::Activity { key, activity } => drop(state.activity.insert(key, activity)),
            Self::RemoteLink { key, link } => drop(state.remote_links.insert(key, link)),
            Self::RemotePeer { key, peer } => drop(state.remote_peers.insert(key, peer)),
            Self::Check { key, check } => state.checks.entry(key).or_default().push(check),
            Self::Landing { key, landing } => state.landings.entry(key).or_default().push(landing),
            Self::Turn { key, turn } => drop(state.turns.insert(key, turn)),
            Self::TurnWaiter { key, waiter } => {
                state.turn_queues.entry(key).or_default().push(waiter)
            }
            Self::InterfaceNotice { key, notice } => {
                state.interface_notices.entry(key).or_default().push(notice)
            }
            Self::Note { key, note } => state.notes.entry(key).or_default().push(note),
            Self::Message { key, message } => state.messages.entry(key).or_default().push(message),
            Self::ScanFailure { key, failure } => drop(state.scan_failures.insert(key, failure)),
            Self::ScanCoverage { key, coverage } => drop(state.scan_coverage.insert(key, coverage)),
            Self::Checkout { key, change } => state.checkouts.entry(key).or_default().push(change),
            Self::AutoCheck { key, check } => drop(state.auto_checks.insert(key, check)),
            Self::CheckNotice { key, notice } => {
                state.check_notices.entry(key).or_default().push(notice)
            }
        }
    }
}

/// Where a page ended: a record kind, the key within it and, for a list, the
/// position within that key's list.
struct Cursor {
    kind: usize,
    index: usize,
    key: String,
}

/// The cursor text is `KIND:INDEX:KEY`. It is opaque to readers, and only
/// meaningful to the coordinator that gave it.
fn cursor(kind: usize, index: usize, key: &str) -> String {
    format!("{}:{index}:{key}", KINDS[kind])
}

fn parse_cursor(text: &str) -> Result<Cursor, CoreError> {
    let invalid = || {
        error(
            "invalid_cursor",
            "the status cursor is not one this coordinator gave; pass --after only the `next` value of the previous page, or leave it out to start again",
        )
    };
    let mut parts = text.splitn(3, ':');
    let (Some(kind), Some(index), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(invalid());
    };
    Ok(Cursor {
        kind: KINDS
            .iter()
            .position(|name| *name == kind)
            .ok_or_else(invalid)?,
        index: index.parse().map_err(|_| invalid())?,
        key: key.into(),
    })
}

type Page<'a> = Box<dyn Iterator<Item = (String, StatusRecord)> + 'a>;

fn entries<'a, V: Clone + 'a>(
    kind: usize,
    map: &'a BTreeMap<String, V>,
    after: Option<&Cursor>,
    record: fn(String, V) -> StatusRecord,
) -> Page<'a> {
    let range = match after {
        Some(after) if after.kind > kind => return Box::new(iter::empty()),
        Some(after) if after.kind == kind => {
            map.range::<str, _>((Bound::Excluded(after.key.as_str()), Bound::Unbounded))
        }
        _ => map.range::<str, _>(..),
    };
    Box::new(
        range.map(move |(key, value)| (cursor(kind, 0, key), record(key.clone(), value.clone()))),
    )
}

fn lists<'a, V: Clone + 'a>(
    kind: usize,
    map: &'a BTreeMap<String, Vec<V>>,
    after: Option<&Cursor>,
    record: fn(String, V) -> StatusRecord,
) -> Page<'a> {
    let (range, skip) = match after {
        Some(after) if after.kind > kind => return Box::new(iter::empty()),
        Some(after) if after.kind == kind => (
            map.range::<str, _>((Bound::Included(after.key.as_str()), Bound::Unbounded)),
            Some((after.key.clone(), after.index)),
        ),
        _ => (map.range::<str, _>(..), None),
    };
    Box::new(range.flat_map(move |(key, values)| {
        let first = match &skip {
            Some((after, index)) if after == key => index + 1,
            _ => 0,
        };
        values
            .iter()
            .enumerate()
            .skip(first)
            .map(move |(index, value)| {
                (cursor(kind, index, key), record(key.clone(), value.clone()))
            })
    }))
}

/// Every record of `state` after the cursor `after` (from the start when
/// `None`), each with the cursor that resumes after it. The order is fixed by
/// kind and key, so a page resumes where the last one ended even when the
/// state changed in between; a record changed meanwhile is read as it is now.
pub fn status_records<'a>(
    state: &'a State,
    after: Option<&str>,
) -> Result<impl Iterator<Item = (String, StatusRecord)> + 'a, CoreError> {
    let after = after.map(parse_cursor).transpose()?;
    let after = after.as_ref();
    let pages: [Page<'a>; 21] = [
        entries(0, &state.workspaces, after, |key, workspace| {
            StatusRecord::Workspace { key, workspace }
        }),
        entries(1, &state.sessions, after, |key, session| {
            StatusRecord::Session { key, session }
        }),
        entries(2, &state.intents, after, |key, intent| {
            StatusRecord::Intent { key, intent }
        }),
        entries(3, &state.leases, after, |key, lease| StatusRecord::Lease {
            key,
            lease,
        }),
        entries(4, &state.session_liveness, after, |key, liveness| {
            StatusRecord::SessionLiveness { key, liveness }
        }),
        entries(5, &state.workspace_git, after, |key, git| {
            StatusRecord::WorkspaceGit { key, git }
        }),
        entries(6, &state.activity, after, |key, activity| {
            StatusRecord::Activity { key, activity }
        }),
        entries(7, &state.remote_links, after, |key, link| {
            StatusRecord::RemoteLink { key, link }
        }),
        entries(8, &state.remote_peers, after, |key, peer| {
            StatusRecord::RemotePeer { key, peer }
        }),
        lists(9, &state.checks, after, |key, check| StatusRecord::Check {
            key,
            check,
        }),
        lists(10, &state.landings, after, |key, landing| {
            StatusRecord::Landing { key, landing }
        }),
        entries(11, &state.turns, after, |key, turn| StatusRecord::Turn {
            key,
            turn,
        }),
        lists(12, &state.turn_queues, after, |key, waiter| {
            StatusRecord::TurnWaiter { key, waiter }
        }),
        lists(13, &state.interface_notices, after, |key, notice| {
            StatusRecord::InterfaceNotice { key, notice }
        }),
        lists(14, &state.notes, after, |key, note| StatusRecord::Note {
            key,
            note,
        }),
        lists(15, &state.messages, after, |key, message| {
            StatusRecord::Message { key, message }
        }),
        entries(16, &state.scan_failures, after, |key, failure| {
            StatusRecord::ScanFailure { key, failure }
        }),
        entries(17, &state.scan_coverage, after, |key, coverage| {
            StatusRecord::ScanCoverage { key, coverage }
        }),
        lists(18, &state.checkouts, after, |key, change| {
            StatusRecord::Checkout { key, change }
        }),
        entries(19, &state.auto_checks, after, |key, check| {
            StatusRecord::AutoCheck { key, check }
        }),
        lists(20, &state.check_notices, after, |key, notice| {
            StatusRecord::CheckNotice { key, notice }
        }),
    ];
    Ok(pages.into_iter().flatten())
}
