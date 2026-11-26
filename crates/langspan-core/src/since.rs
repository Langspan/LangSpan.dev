//! What happened in one workspace since a point in its event log (roadmap
//! C10): landings, checks, interface changes, ended sessions, messages still
//! owed and teammates heard from. Derived from the log on request; nothing
//! is stored.
//!
//! The point is a sequence number, never a clock time, so a reboot (which
//! restarts the daemon's clock) does not move it. Asked for "since away",
//! the point is the last timed activity of an interactive session of the
//! workspace before its newest gap of at least `after_ms` without any. Times
//! are placed on the wall clock through each generation's `ClockAnchored`
//! event, as `langspan today` places them. Headless sessions (`claude -p`)
//! are agents left running, not the owner at work, so their activity does
//! not end a gap; what they did is reported.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::today::{Placement, event_facts, event_workspace};
use crate::{
    AttemptResult, CheckResult, ClockAnchor, Event, Recipient, RecordedEvent, State, is_false,
    is_zero,
};

/// The most events one answer reads, newest first. A point before them is
/// reported as `truncated`, never guessed at.
pub const SINCE_EVENTS: usize = 10_000;
/// The gap without interactive activity that makes the workspace "away",
/// unless configured otherwise (`LANGSPAN_AWAY_AFTER_MS`): two hours.
pub const AWAY_AFTER_MS: u64 = 2 * 60 * 60 * 1000;
/// The longest gap that can be configured: thirty days.
pub const MAX_AWAY_AFTER_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// Items kept per list; the rest are counted in `omitted`.
pub const SINCE_ITEMS: usize = 32;
/// Symbol names kept per interface change.
const SINCE_NAMES: usize = 8;

/// Where an answer starts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SinceFrom {
    /// After this sequence number.
    Seq { seq: u64 },
    /// After the last activity of an interactive session before the newest
    /// gap of at least `after_ms` without any.
    Away { after_ms: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SinceLanding {
    pub onto: String,
    pub commit_oid: Option<String>,
    /// The agent session that took the landing's turn, when it said.
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SinceFailure {
    pub name: String,
    pub exit_code: i32,
    /// A later record of the same check name passed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub passed_since: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SinceChecks {
    pub passed: u32,
    pub failed: u32,
    /// The newest failed record.
    pub latest_failure: Option<SinceFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SinceInterface {
    pub path: String,
    /// The changed names, at most eight.
    pub names: Vec<String>,
    pub by_session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SinceMessage {
    pub message_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    pub to: Recipient,
}

/// What happened after `from_seq`, up to `to_seq`. Lists are oldest first,
/// each at most `SINCE_ITEMS`; what is left out is counted in `omitted`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Since {
    pub from_seq: u64,
    /// The newest event read.
    pub to_seq: u64,
    /// Asked for "since away": how long the gap was, in milliseconds. While
    /// it lasts (`ongoing`), up to now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away_ms: Option<u64>,
    /// Asked for "since away": no interactive session of the workspace has
    /// been active since `from_seq`. A starting session is told only then.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ongoing: bool,
    /// Part of what happened after `from_seq` was not read: it is older
    /// than the newest `SINCE_EVENTS` events, or retention removed it. The
    /// lists hold only what was read.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    pub landings: Vec<SinceLanding>,
    pub checks: SinceChecks,
    pub interface_changes: Vec<SinceInterface>,
    pub sessions_ended: Vec<String>,
    /// Messages sent since that some recipient has still not acknowledged.
    pub messages_unacknowledged: Vec<SinceMessage>,
    /// Teammates on other machines whose publications arrived since.
    pub teammates_heard: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: u32,
}

/// What an answer is asked to cover.
pub struct SinceQuery<'a> {
    pub workspace_id: &'a str,
    pub from: SinceFrom,
    /// The newest events, oldest first, at most `SINCE_EVENTS`.
    pub events: &'a [RecordedEvent],
    /// The anchor of the generation the first event belongs to, when it was
    /// anchored and the anchor precedes it.
    pub anchor: Option<ClockAnchor>,
    /// The log holds events older than the first one given.
    pub earlier: bool,
    /// Wall-clock milliseconds since the Unix epoch, now.
    pub wall_now_ms: u64,
}

/// Whether an event is the work of an interactive session of
/// `workspace_id`, at a time of its own on `generation`'s clock: a prompt,
/// a tool call, a read, a refusal or the end of a reply (`event_facts`).
/// A hook run's `SessionSeen` is not work: the daemon records one for the
/// starting session's own start, which would end every gap before it was
/// asked about. Liveness is not work either.
fn work_time(state: &State, workspace_id: &str, event: &Event) -> Option<(u64, u64)> {
    let (session_id, time) = event_facts(event);
    state
        .sessions
        .get(session_id?)
        .filter(|session| session.workspace_id == workspace_id && !session.headless)
        .and(time)
}

/// Where the answer starts, how long the gap was and whether it lasts, or
/// `None` when the events read hold no such gap.
struct Mark {
    from_seq: u64,
    away_ms: Option<u64>,
    ongoing: bool,
    /// The point is older than the events read.
    unread: bool,
}

fn away_mark(state: &State, query: &SinceQuery<'_>, after_ms: u64) -> Option<Mark> {
    let mut placement = Placement::new(query.anchor);
    // Each timed activity of an interactive session: its sequence number
    // and wall time. An event without a time of its own inherits an older
    // one, which would invent a gap, so it neither starts nor ends one.
    let mut activity: Vec<(u64, u64)> = Vec::new();
    let mut oldest_wall: Option<u64> = None;
    for recorded in query.events {
        let event = &recorded.event;
        let wall = placement.place(event);
        if oldest_wall.is_none() {
            oldest_wall = wall;
        }
        if let Some(wall) = work_time(state, query.workspace_id, event)
            .and_then(|(at_ms, generation)| placement.wall_of(at_ms, generation))
        {
            activity.push((recorded.seq, wall));
        }
    }
    let now = query.wall_now_ms;
    // Newest first: each activity with the time of the next one (now after
    // the last).
    let newest_gap = (0..activity.len()).rev().find_map(|index| {
        let (seq, from) = activity[index];
        let to = activity.get(index + 1).map_or(now, |next| next.1);
        (to.saturating_sub(from) >= after_ms).then_some((index, seq, to.saturating_sub(from)))
    });
    if let Some((index, seq, gap)) = newest_gap {
        return Some(Mark {
            from_seq: seq,
            away_ms: Some(gap),
            ongoing: index + 1 == activity.len(),
            unread: false,
        });
    }
    // No interactive activity among the events read, and the oldest of them
    // is from before the gap: the workspace is away, from a point older
    // than what was read.
    match (activity.is_empty(), query.earlier, oldest_wall) {
        (true, true, Some(oldest)) if now.saturating_sub(oldest) >= after_ms => Some(Mark {
            from_seq: query
                .events
                .first()
                .map_or(0, |recorded| recorded.seq.saturating_sub(1)),
            away_ms: Some(now.saturating_sub(oldest)),
            ongoing: true,
            unread: true,
        }),
        _ => None,
    }
}

/// What happened since the asked point, or `None` when asked for "since
/// away" and the events read hold no gap that long. Pure: every time and
/// every event is supplied.
pub fn since(state: &State, query: &SinceQuery<'_>) -> Option<Since> {
    let mark = match query.from {
        SinceFrom::Seq { seq } => Mark {
            from_seq: seq,
            away_ms: None,
            ongoing: false,
            unread: false,
        },
        SinceFrom::Away { after_ms } => away_mark(state, query, after_ms)?,
    };
    let from_seq = mark.from_seq;
    let after: Vec<&RecordedEvent> = query
        .events
        .iter()
        .filter(|recorded| recorded.seq > from_seq)
        .collect();
    let to_seq = query
        .events
        .last()
        .map_or(from_seq, |recorded| recorded.seq.max(from_seq));
    // Sequence numbers are assigned without gaps; only retention removes
    // events. Any number after the point that was not read is missing.
    let read = u64::try_from(after.len()).unwrap_or(u64::MAX);
    let truncated = mark.unread || read < to_seq - from_seq;
    let workspace_id = query.workspace_id;
    let mut omitted = 0usize;
    fn push<T>(omitted: &mut usize, list: &mut Vec<T>, item: T) {
        if list.len() < SINCE_ITEMS {
            list.push(item);
        } else {
            *omitted += 1;
        }
    }
    let mut turns: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    let mut landings = Vec::new();
    let mut checks = SinceChecks::default();
    let mut interface_changes = Vec::new();
    let mut sessions_ended = Vec::new();
    let mut messages = Vec::new();
    let mut heard: BTreeSet<&str> = BTreeSet::new();
    let mut teammates_heard = Vec::new();
    let held: BTreeSet<&str> = state
        .messages
        .get(workspace_id)
        .into_iter()
        .flatten()
        .filter(|message| !message.retracted)
        .map(|message| message.message_id.as_str())
        .collect();
    for recorded in after {
        match &recorded.event {
            Event::TurnTaken { turn } if turn.workspace_id == workspace_id => {
                turns.insert(&turn.turn_id, turn.session_id.as_deref());
            }
            Event::TurnReleased {
                workspace_id: id,
                onto,
                turn_id,
                attempts,
            } if id == workspace_id => {
                if let Some(last) = attempts
                    .last()
                    .filter(|last| matches!(last.result, AttemptResult::Landed {}))
                {
                    push(
                        &mut omitted,
                        &mut landings,
                        SinceLanding {
                            onto: onto.clone(),
                            commit_oid: last.commit_oid.clone(),
                            session_id: turns
                                .get(turn_id.as_str())
                                .copied()
                                .flatten()
                                .map(Into::into),
                        },
                    );
                }
            }
            Event::CheckRecorded { check, .. } if check.workspace_id == workspace_id => {
                match check.result {
                    CheckResult::Passed {} => {
                        checks.passed = checks.passed.saturating_add(1);
                        if let Some(failure) = checks
                            .latest_failure
                            .as_mut()
                            .filter(|failure| failure.name == check.name)
                        {
                            failure.passed_since = true;
                        }
                    }
                    CheckResult::Failed { exit_code } => {
                        checks.failed = checks.failed.saturating_add(1);
                        checks.latest_failure = Some(SinceFailure {
                            name: check.name.clone(),
                            exit_code,
                            passed_since: false,
                        });
                    }
                    CheckResult::Inconclusive {} => {}
                }
            }
            Event::InterfaceChanged { notice } if notice.workspace_id == workspace_id => {
                // An added export and a private definition are history only,
                // never news (as in briefings).
                let names: Vec<String> = notice
                    .changes
                    .iter()
                    .filter(|change| change.change != "added" && !change.private)
                    .map(|change| change.name.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .take(SINCE_NAMES)
                    .collect();
                if !names.is_empty() {
                    push(
                        &mut omitted,
                        &mut interface_changes,
                        SinceInterface {
                            path: notice.path.clone(),
                            names,
                            by_session: notice.by_session.clone(),
                        },
                    );
                }
            }
            Event::SessionEnded { session_id }
                if event_workspace(state, &recorded.event) == Some(workspace_id) =>
            {
                push(&mut omitted, &mut sessions_ended, session_id.clone());
            }
            Event::MessageSent { message, .. }
                if message.workspace_id == workspace_id
                    && held.contains(message.message_id.as_str()) =>
            {
                push(
                    &mut omitted,
                    &mut messages,
                    SinceMessage {
                        message_id: message.message_id.clone(),
                        from: message.from.clone(),
                        to: message.to.clone(),
                    },
                );
            }
            Event::RemoteObserved { peer }
                if peer.workspace_id == workspace_id
                    && heard.insert(peer.publication.name.as_str()) =>
            {
                push(
                    &mut omitted,
                    &mut teammates_heard,
                    peer.publication.name.clone(),
                );
            }
            _ => {}
        }
    }
    Some(Since {
        from_seq,
        to_seq,
        away_ms: mark.away_ms,
        ongoing: mark.ongoing,
        truncated,
        landings,
        checks,
        interface_changes,
        sessions_ended,
        messages_unacknowledged: messages,
        teammates_heard,
        omitted: u32::try_from(omitted).unwrap_or(u32::MAX),
    })
}

impl Since {
    /// Remove one listed item, from the last list that has one, and count
    /// it. False when nothing is left to remove.
    pub fn pop(&mut self) -> bool {
        let popped = self.teammates_heard.pop().is_some()
            || self.messages_unacknowledged.pop().is_some()
            || self.sessions_ended.pop().is_some()
            || self.interface_changes.pop().is_some()
            || self.landings.pop().is_some();
        if popped {
            self.omitted = self.omitted.saturating_add(1);
        }
        popped
    }
}
