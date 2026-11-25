//! The status reply. By default it is a bounded view of what matters now:
//! workspaces, live or recent sessions and the admissions still open, with
//! closed admissions only counted. The whole state, history included, is read
//! as a sequence of records in pages, each resumed after an opaque cursor.

use crate::{
    AutoCheck, CheckNotice, CheckRecord, CheckoutChange, CoreError, Intent, InterfaceNotice,
    Landing, Lease, LeaseStatus, LivenessStatus, Message, Note, PresenceSetting, RemoteLink,
    RemotePeer, ScanCoverage, ScanFailure, Session, SessionActivity, SessionLiveness, State, Turn,
    TurnWaiter, UninstalledHost, Workspace, WorkspaceGit, error,
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
        uninstalled_hosts,
        presence,
        // Derived for the status line; never part of the status reply.
        clock_anchor: _,
        refusals: _,
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
        uninstalled_hosts: uninstalled_hosts.clone(),
        presence: keep(presence, of_shown),
        sessions: shown_sessions.clone(),
        clock_anchor: None,
        refusals: Vec::new(),
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

/// One part of the view that may be left out: `trim` empties it in a view
/// and says whether anything was there; `restore` puts it back into a view
/// from the complete state.
struct Trim {
    name: &'static str,
    trim: fn(&mut State) -> bool,
    restore: fn(&mut State, &State),
}

/// A whole field of the state.
macro_rules! field {
    ($name:literal, $field:ident) => {
        Trim {
            name: $name,
            trim: |state| take(&mut state.$field),
            restore: |view, full| view.$field = full.$field.clone(),
        }
    };
}

/// One list of every session's activity: only the sessions in the view are
/// given theirs back.
macro_rules! activity {
    ($name:literal, $field:ident) => {
        Trim {
            name: $name,
            trim: |state| {
                state
                    .activity
                    .values_mut()
                    .fold(false, |any, activity| any | take(&mut activity.$field))
            },
            restore: |view, full| {
                for (id, activity) in &mut view.activity {
                    if let Some(complete) = full.activity.get(id) {
                        activity.$field = complete.$field.clone();
                    }
                }
            },
        }
    };
}

/// Parts of the view left out, in this order, while it does not fit: the
/// bulkiest histories first, current coordination last. Each name is the one
/// `StatusSummary::omitted` gives.
const TRIMS: [Trim; 15] = [
    activity!("activity.known", known),
    field!("checks", checks),
    field!("landings", landings),
    field!("interface_notices", interface_notices),
    field!("checkouts", checkouts),
    field!("notes", notes),
    field!("remote_peers", remote_peers),
    activity!("activity.touched", touched),
    activity!("activity.alerted", alerted),
    activity!("activity.prompt_targets", prompt_targets),
    activity!("activity.looking", looking),
    // Only the shown sessions' notices are in the view.
    Trim {
        name: "check_notices",
        trim: |state| take(&mut state.check_notices),
        restore: |view, full| {
            view.check_notices = full
                .check_notices
                .iter()
                .filter(|(id, _)| view.sessions.contains_key(*id))
                .map(|(id, notices)| (id.clone(), notices.clone()))
                .collect();
        },
    },
    field!("messages", messages),
    field!("workspace_git", workspace_git),
    // Only one at the limits of its command is larger than a frame.
    field!("auto_checks", auto_checks),
];

fn take<T: Default + PartialEq>(value: &mut T) -> bool {
    mem::take(value) != T::default()
}

/// Put back into `view` each part named in `needed` that the bounded view
/// left out, from `full`, the complete state read with `status --all`, and
/// take its name off `summary.omitted`. A caller that decides from one of
/// these parts reads it complete this way, and only when it was left out.
pub fn restore_status(
    view: &mut State,
    summary: &mut StatusSummary,
    full: &State,
    needed: &[&str],
) {
    for part in &TRIMS {
        if needed.contains(&part.name) && summary.omitted.iter().any(|name| name == part.name) {
            (part.restore)(view, full);
            summary.omitted.retain(|name| name != part.name);
        }
    }
}

/// Leave parts of the view out until `fits` accepts it, each counted or named
/// in the summary: the histories in `TRIMS`, then sessions with nothing open,
/// least recently active first, then per-session counts, and open admissions
/// only if they alone are larger than a frame. Workspaces, links, turns and
/// scan health are never left out.
pub fn shrink_status(
    view: &mut State,
    summary: &mut StatusSummary,
    mut fits: impl FnMut(&State, &StatusSummary) -> bool,
) {
    if fits(view, summary) {
        return;
    }
    for part in &TRIMS {
        if (part.trim)(view) {
            summary.omitted.push(part.name.into());
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
    UninstalledHost {
        key: String,
        host: UninstalledHost,
    },
    Presence {
        key: String,
        setting: PresenceSetting,
    },
    /// Part of one record too large for a page: `text` is its JSON from
    /// byte `offset`. The parts of a record come in order, the last with
    /// `last`, and joined they are that record's JSON. A part at offset 0
    /// starts the record again: it changed while it was being read.
    Part {
        text: String,
        offset: u64,
        last: bool,
    },
}

/// Record kinds in page order. The cursor names one of these.
const KINDS: [&str; 23] = [
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
    "uninstalled_host",
    "presence",
];

impl StatusRecord {
    /// Place this record in `state`, as it was in the state it was read from.
    /// Applying every page in order rebuilds the state's maps and lists. A
    /// part is refused: its record is applied once its parts are joined.
    pub fn apply(self, state: &mut State) -> Result<(), CoreError> {
        match self {
            Self::Part { .. } => {
                return Err(error(
                    "status_part",
                    "a record part is applied only as the whole record its parts make",
                ));
            }
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
            Self::UninstalledHost { key, host } => drop(state.uninstalled_hosts.insert(key, host)),
            Self::Presence { key, setting } => drop(state.presence.insert(key, setting)),
        }
        Ok(())
    }
}

/// Where one record is in the page order: its kind, its key and, for a
/// list, its position in that key's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPosition {
    kind: usize,
    index: usize,
    key: String,
}

impl StatusPosition {
    /// The cursor that resumes after this record: `KIND:INDEX:KEY`. Cursors
    /// are opaque to readers, and only meaningful to the coordinator that
    /// gave them.
    pub fn after(&self) -> String {
        format!("{}:{}:{}", KINDS[self.kind], self.index, self.key)
    }

    /// The cursor that resumes inside this record, at byte `offset` of its
    /// JSON, if the record's JSON still has the `digest` it had:
    /// `KIND:INDEX+OFFSET+DIGEST:KEY`.
    pub fn within(&self, offset: usize, digest: u64) -> String {
        format!(
            "{}:{}+{offset}+{digest}:{}",
            KINDS[self.kind], self.index, self.key
        )
    }
}

/// Where a page that ended inside a record resumes: at `offset` bytes into
/// its JSON, if that JSON still has `digest`; otherwise at its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusResume {
    pub offset: usize,
    pub digest: u64,
}

/// A parsed cursor: the position it names, and whether the page resumes
/// inside that record rather than after it.
struct Cursor {
    position: StatusPosition,
    within: Option<StatusResume>,
    /// The first list position to give for the cursor's key.
    first: usize,
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
    let number = |text: &str| text.parse::<usize>().map_err(|_| invalid());
    let (index, within) = match index.split('+').collect::<Vec<_>>()[..] {
        [index] => (number(index)?, None),
        [index, offset, digest] => (
            number(index)?,
            Some(StatusResume {
                offset: number(offset)?,
                digest: digest.parse().map_err(|_| invalid())?,
            }),
        ),
        _ => return Err(invalid()),
    };
    let first = match within {
        Some(_) => index,
        // A position after the last one there can be is not one we gave.
        None => index.checked_add(1).ok_or_else(invalid)?,
    };
    Ok(Cursor {
        position: StatusPosition {
            kind: KINDS
                .iter()
                .position(|name| *name == kind)
                .ok_or_else(invalid)?,
            index,
            key: key.into(),
        },
        within,
        first,
    })
}

type Page<'a> = Box<dyn Iterator<Item = (StatusPosition, StatusRecord)> + 'a>;

fn entries<'a, V: Clone + 'a>(
    kind: usize,
    map: &'a BTreeMap<String, V>,
    after: Option<&Cursor>,
    record: fn(String, V) -> StatusRecord,
) -> Page<'a> {
    let range = match after {
        Some(after) if after.position.kind > kind => return Box::new(iter::empty()),
        Some(after) if after.position.kind == kind => {
            let key = after.position.key.as_str();
            // Resuming inside a record gives that record again.
            let from = match after.within {
                Some(_) => Bound::Included(key),
                None => Bound::Excluded(key),
            };
            map.range::<str, _>((from, Bound::Unbounded))
        }
        _ => map.range::<str, _>(..),
    };
    Box::new(range.map(move |(key, value)| {
        (
            StatusPosition {
                kind,
                index: 0,
                key: key.clone(),
            },
            record(key.clone(), value.clone()),
        )
    }))
}

fn lists<'a, V: Clone + 'a>(
    kind: usize,
    map: &'a BTreeMap<String, Vec<V>>,
    after: Option<&Cursor>,
    record: fn(String, V) -> StatusRecord,
) -> Page<'a> {
    let (range, skip) = match after {
        Some(after) if after.position.kind > kind => return Box::new(iter::empty()),
        Some(after) if after.position.kind == kind => (
            map.range::<str, _>((
                Bound::Included(after.position.key.as_str()),
                Bound::Unbounded,
            )),
            Some((after.position.key.clone(), after.first)),
        ),
        _ => (map.range::<str, _>(..), None),
    };
    Box::new(range.flat_map(move |(key, values)| {
        let first = match &skip {
            Some((after, first)) if after == key => *first,
            _ => 0,
        };
        values
            .iter()
            .enumerate()
            .skip(first)
            .map(move |(index, value)| {
                (
                    StatusPosition {
                        kind,
                        index,
                        key: key.clone(),
                    },
                    record(key.clone(), value.clone()),
                )
            })
    }))
}

/// Records of `state` from the cursor `after` (from the start when `None`),
/// each with its position. The order is fixed by kind and key, so a page
/// resumes where the last one ended even when the state changed in between;
/// a record changed meanwhile is read as it is now. When the cursor resumes
/// inside a record, that record's position and the resume point are
/// returned, and the first record given is that one if it is still there.
#[allow(clippy::type_complexity)]
pub fn status_records<'a>(
    state: &'a State,
    after: Option<&str>,
) -> Result<
    (
        Option<(StatusPosition, StatusResume)>,
        impl Iterator<Item = (StatusPosition, StatusRecord)> + 'a,
    ),
    CoreError,
> {
    let after = after.map(parse_cursor).transpose()?;
    let resume = after
        .as_ref()
        .and_then(|after| Some((after.position.clone(), after.within?)));
    let after = after.as_ref();
    let pages: [Page<'a>; 23] = [
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
        entries(21, &state.uninstalled_hosts, after, |key, host| {
            StatusRecord::UninstalledHost { key, host }
        }),
        entries(22, &state.presence, after, |key, setting| {
            StatusRecord::Presence { key, setting }
        }),
    ];
    Ok((resume, pages.into_iter().flatten()))
}
