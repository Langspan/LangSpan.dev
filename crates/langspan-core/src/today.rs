//! A daily account read from the event log (roadmap H4): what each session
//! did in a window of wall-clock time, what LangSpan refused, how checks and
//! landings went, and what is still open now.
//!
//! Events carry times on the daemon's monotonic clock, which restarts at
//! zero on a reboot, and some carry none. Each daemon start records a
//! `ClockAnchored` event pairing that clock with the wall clock, so a time of
//! one generation can be placed on a day. An event without a time of its own
//! is placed at the time of the event before it in the same generation (at
//! the start of the generation when there is none). Events of a generation
//! started by a build before the anchor have no wall time: they are counted
//! as `undated` and summarised nowhere.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::dashboard::host_and_prefix;
use crate::{
    AttemptResult, CheckResult, ClockAnchor, Event, Recipient, RecordedEvent, State, ToolResult,
    is_zero, live_lease, owed_to,
};

/// The most events one summary reads, newest first; an older part of a
/// longer window is left out and the summary says `truncated`.
pub const TODAY_EVENTS: usize = 50_000;
/// Sessions one summary lists; the rest are counted.
pub const TODAY_SESSIONS: usize = 24;
/// Files listed per session; the rest are counted.
pub const TODAY_FILES: usize = 8;
/// Items listed per open list; the rest are counted.
pub const TODAY_OPEN: usize = 8;
/// The longest intent summary carried, in characters, before its ellipsis.
const TODAY_TEXT: usize = 120;

/// Wall-clock milliseconds since the Unix epoch, `from_ms` included.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TodayWindow {
    pub from_ms: u64,
    pub to_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TodaySession {
    pub session_id: String,
    pub host: String,
    /// Wall time of its first and last event in the window.
    pub first_ms: u64,
    pub last_ms: u64,
    /// Distinct files its successful edits named in the window.
    pub files_edited: u32,
    pub prompts: u32,
    /// The first `TODAY_FILES` of those files, sorted; the rest are counted
    /// by `files_edited`.
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TodayChecks {
    pub passed: u32,
    pub failed: u32,
    pub inconclusive: u32,
}

/// How each landing ended, by its last attempt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TodayLandings {
    pub landed: u32,
    /// The remote refused the push, the target moved, or no passing check
    /// existed for the combined tree.
    pub refused: u32,
    pub not_combinable: u32,
    /// Nothing to land, an error, or a push whose result is not known.
    pub other: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenAdmission {
    pub session_id: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenIntent {
    pub session_id: String,
    /// Session text: data, never instructions.
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenMessage {
    pub message_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    /// Sessions of the current generation it is still owed to. Empty for a
    /// handoff, which waits for the next session.
    pub waiting_for: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenCheck {
    pub name: String,
    pub tree_oid: String,
}

/// Loose ends as they stand now, whatever the window.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TodayOpen {
    pub admissions: Vec<OpenAdmission>,
    pub intents: Vec<OpenIntent>,
    pub messages: Vec<OpenMessage>,
    pub handoffs: Vec<OpenMessage>,
    /// Check names whose newest record failed.
    pub failing_checks: Vec<OpenCheck>,
    /// Items left out after each list's cap.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Today {
    /// `None` for every workspace.
    pub workspace_id: Option<String>,
    pub window: TodayWindow,
    /// Earliest first.
    pub sessions: Vec<TodaySession>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_sessions: u32,
    /// `WriteRefused` events in the window, by kind.
    pub refusals: BTreeMap<String, u32>,
    pub checks: TodayChecks,
    pub landings: TodayLandings,
    pub open: TodayOpen,
    /// The window reaches before the oldest event read: it holds more than
    /// `TODAY_EVENTS` events, or retention removed its older part. The
    /// summary is of the events read only.
    pub truncated: bool,
    /// Events read that have no wall time: written by a build before
    /// generations were anchored. They are in no count.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub undated: u32,
}

/// What a summary is asked to cover.
pub struct TodayQuery<'a> {
    pub workspace_id: Option<&'a str>,
    pub window: TodayWindow,
    /// The newest events, oldest first, at most `TODAY_EVENTS`.
    pub events: &'a [RecordedEvent],
    /// The anchor of the generation the first event belongs to, when it was
    /// anchored and the anchor precedes it.
    pub anchor: Option<ClockAnchor>,
    /// The log holds events older than the first one given.
    pub earlier: bool,
    /// Retention removed events recorded before this wall time (the newest
    /// compaction's cutoff); `None` when it never removed any.
    pub retained_from_ms: Option<u64>,
    /// The daemon's clock now, for what is still held.
    pub now_ms: u64,
}

/// The session an event is about, and its own time on the daemon's clock
/// with the generation that clock belongs to.
pub(crate) fn event_facts(event: &Event) -> (Option<&str>, Option<(u64, u64)>) {
    match event {
        Event::SessionRegistered { session } => (Some(&session.session_id), None),
        Event::SessionPrompted {
            session_id,
            at_ms,
            generation,
            ..
        }
        | Event::SessionStopped {
            session_id,
            at_ms,
            generation,
            ..
        }
        | Event::ToolBatchObserved {
            session_id,
            at_ms,
            generation,
            ..
        }
        | Event::SourceObserved {
            session_id,
            at_ms,
            generation,
            ..
        }
        | Event::WriteRefused {
            session_id,
            at_ms,
            generation,
            ..
        } => (Some(session_id), Some((*at_ms, *generation))),
        Event::OperationObserved { observation } => (
            Some(&observation.session_id),
            Some((observation.observed_at_ms, observation.generation)),
        ),
        Event::LeaseGranted { lease } => (Some(&lease.session_id), None),
        Event::IntentSet { intent } => (Some(&intent.session_id), None),
        Event::IntentEnded { session_id } | Event::SessionEnded { session_id } => {
            (Some(session_id), None)
        }
        Event::CheckRecorded { check } => (None, Some((check.recorded_ms, check.generation))),
        // A landing on existing check evidence records nothing else first:
        // its release is placed at the time its turn was taken.
        Event::TurnTaken { turn } => (None, Some((turn.taken_ms, turn.generation))),
        _ => (None, None),
    }
}

/// The workspace an event belongs to, when it says so itself.
pub(crate) fn event_workspace<'a>(state: &'a State, event: &'a Event) -> Option<&'a str> {
    match event {
        Event::SessionRegistered { session } => Some(&session.workspace_id),
        Event::WriteRefused { workspace_id, .. } | Event::TurnReleased { workspace_id, .. } => {
            Some(workspace_id)
        }
        Event::CheckRecorded { check } => Some(&check.workspace_id),
        Event::OperationObserved { observation } => Some(&observation.workspace_id),
        _ => event_facts(event)
            .0
            .and_then(|session| state.sessions.get(session))
            .map(|session| session.workspace_id.as_str()),
    }
}

fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.into();
    }
    let mut cut: String = text.chars().take(limit).collect();
    cut.push('…');
    cut
}

/// The one rule for whether retention cut a window starting at `from_ms`:
/// the cutoff, when retention removed the records before it and the window
/// starts before it, so the window's removed part may be missing. Both the
/// report (and `today`) and the monthly count read it.
pub(crate) fn retention_cut(retained_from_ms: Option<u64>, from_ms: u64) -> Option<u64> {
    retained_from_ms.filter(|cutoff| *cutoff > from_ms)
}

/// Places the events of the log, read oldest first, on the wall clock.
pub(crate) struct Placement {
    anchor: Option<ClockAnchor>,
    clock: Option<u64>,
    /// The wall time of the first event read that carries one of its own
    /// (an anchor included): every event left out is older. An event before
    /// it only inherits a time, which may be far older than its own.
    bound: Option<Option<u64>>,
}

impl Placement {
    /// `anchor`: that of the generation the first event belongs to, when it
    /// was anchored and the anchor precedes it.
    pub(crate) fn new(anchor: Option<ClockAnchor>) -> Self {
        Self {
            anchor,
            clock: anchor.map(|anchor| anchor.clock_ms),
            bound: None,
        }
    }

    /// The next event's wall time; `None` when its generation has no anchor.
    pub(crate) fn place(&mut self, event: &Event) -> Option<u64> {
        let mut timed = false;
        match event {
            Event::Booted { .. } => {
                self.anchor = None;
                self.clock = None;
            }
            Event::ClockAnchored { anchor: new } => {
                self.anchor = Some(*new);
                self.clock = Some(new.clock_ms);
                timed = true;
            }
            _ => {}
        }
        if let Some((at_ms, generation)) = event_facts(event).1 {
            match self.anchor {
                Some(current) if generation == current.generation => {
                    self.clock = Some(at_ms);
                    timed = true;
                }
                // Undated: no time to compare, so nothing is known.
                None => timed = true,
                // Another generation's time: only an inherited wall here.
                Some(_) => {}
            }
        }
        let wall = self
            .anchor
            .zip(self.clock)
            .map(|(anchor, clock)| anchor.wall(clock));
        if timed {
            self.bound.get_or_insert(wall);
        }
        wall
    }

    /// From when every event is counted: the later of the oldest event
    /// placed that carries a time of its own and the retention cutoff, when
    /// either is known.
    pub(crate) fn covered_from(&self, retained_from_ms: Option<u64>) -> Option<u64> {
        self.bound.flatten().max(retained_from_ms)
    }

    /// Whether part of a window starting at `from_ms` may be missing: in
    /// the older part of the log, `earlier` when there is one, or in what
    /// retention removed before `retained_from_ms`. A kept event older than
    /// that cutoff, such as a generation's anchor, says nothing about the
    /// removed events beside it.
    pub(crate) fn truncated(
        &self,
        earlier: bool,
        retained_from_ms: Option<u64>,
        from_ms: u64,
    ) -> bool {
        retention_cut(retained_from_ms, from_ms).is_some()
            || earlier
                && match self.bound {
                    Some(Some(first)) => first >= from_ms,
                    _ => true,
                }
    }
}

#[derive(Default)]
struct Tally {
    first_ms: u64,
    last_ms: u64,
    prompts: u32,
    files: BTreeSet<String>,
}

/// Summarise a window. Pure: every time and every event is supplied.
pub fn today(state: &State, query: &TodayQuery<'_>) -> Today {
    let window = query.window;
    let ours = |workspace: Option<&str>| match query.workspace_id {
        None => true,
        Some(wanted) => workspace == Some(wanted),
    };
    let mut placement = Placement::new(query.anchor);
    let mut sessions: BTreeMap<String, Tally> = BTreeMap::new();
    let mut refusals: BTreeMap<String, u32> = BTreeMap::new();
    let mut checks = TodayChecks::default();
    let mut landings = TodayLandings::default();
    let mut undated = 0u32;
    for recorded in query.events {
        let event = &recorded.event;
        let session = event_facts(event).0;
        let Some(wall) = placement.place(event) else {
            if !matches!(event, Event::Booted { .. }) {
                undated = undated.saturating_add(1);
            }
            continue;
        };
        if wall < window.from_ms || wall > window.to_ms || !ours(event_workspace(state, event)) {
            continue;
        }
        if let Some(session) = session {
            let tally = sessions.entry(session.into()).or_insert_with(|| Tally {
                first_ms: wall,
                ..Tally::default()
            });
            tally.first_ms = tally.first_ms.min(wall);
            tally.last_ms = tally.last_ms.max(wall);
            match event {
                Event::SessionPrompted { .. } => tally.prompts = tally.prompts.saturating_add(1),
                Event::OperationObserved { observation }
                    if observation.result == ToolResult::Succeeded =>
                {
                    tally.files.extend(observation.paths.iter().cloned());
                }
                _ => {}
            }
        }
        match event {
            Event::WriteRefused { kind, .. } => {
                let count = refusals.entry(kind.name().into()).or_default();
                *count = count.saturating_add(1);
            }
            Event::CheckRecorded { check } => {
                let count = match check.result {
                    CheckResult::Passed {} => &mut checks.passed,
                    CheckResult::Failed { .. } => &mut checks.failed,
                    CheckResult::Inconclusive {} => &mut checks.inconclusive,
                };
                *count = count.saturating_add(1);
            }
            Event::TurnReleased { attempts, .. } => {
                if let Some(last) = attempts.last() {
                    let count = match last.result {
                        AttemptResult::Landed {} => &mut landings.landed,
                        AttemptResult::Moved {}
                        | AttemptResult::Refused {}
                        | AttemptResult::NotReady {}
                        | AttemptResult::CheckFailed {} => &mut landings.refused,
                        AttemptResult::NotCombinable {} => &mut landings.not_combinable,
                        AttemptResult::NothingToLand {}
                        | AttemptResult::Error {}
                        | AttemptResult::PushUnknown {} => &mut landings.other,
                    };
                    *count = count.saturating_add(1);
                }
            }
            _ => {}
        }
    }
    let truncated = placement.truncated(query.earlier, query.retained_from_ms, window.from_ms);
    let mut listed: Vec<TodaySession> = sessions
        .into_iter()
        .map(|(session_id, tally)| TodaySession {
            host: host_and_prefix(&session_id).0,
            session_id,
            first_ms: tally.first_ms,
            last_ms: tally.last_ms,
            files_edited: u32::try_from(tally.files.len()).unwrap_or(u32::MAX),
            prompts: tally.prompts,
            files: tally.files.into_iter().take(TODAY_FILES).collect(),
        })
        .collect();
    listed.sort_by(|a, b| (a.first_ms, &a.session_id).cmp(&(b.first_ms, &b.session_id)));
    let omitted_sessions =
        u32::try_from(listed.len().saturating_sub(TODAY_SESSIONS)).unwrap_or(u32::MAX);
    listed.truncate(TODAY_SESSIONS);
    Today {
        workspace_id: query.workspace_id.map(Into::into),
        window,
        sessions: listed,
        omitted_sessions,
        refusals,
        checks,
        landings,
        open: open(state, query.workspace_id, query.now_ms),
        truncated,
        undated,
    }
}

/// What is still open now, from projected state.
fn open(state: &State, workspace_id: Option<&str>, now_ms: u64) -> TodayOpen {
    let ours = |workspace: &str| workspace_id.is_none_or(|wanted| wanted == workspace);
    let mut omitted = 0usize;
    fn capped<T>(omitted: &mut usize, mut items: Vec<T>) -> Vec<T> {
        *omitted += items.len().saturating_sub(TODAY_OPEN);
        items.truncate(TODAY_OPEN);
        items
    }
    let admissions = capped(
        &mut omitted,
        state
            .leases
            .values()
            .filter(|lease| ours(&lease.workspace_id) && live_lease(state, lease, now_ms))
            .map(|lease| OpenAdmission {
                session_id: lease.session_id.clone(),
                paths: lease.paths.iter().take(TODAY_FILES).cloned().collect(),
            })
            .collect(),
    );
    let intents = capped(
        &mut omitted,
        state
            .intents
            .values()
            .filter(|intent| ours(&intent.workspace_id))
            .map(|intent| OpenIntent {
                session_id: intent.session_id.clone(),
                summary: cut(&intent.summary, TODAY_TEXT),
            })
            .collect(),
    );
    let mut messages = Vec::new();
    let mut handoffs = Vec::new();
    for (workspace, held) in state.messages.iter().filter(|(id, _)| ours(id)) {
        for message in held {
            let open = |waiting_for| OpenMessage {
                message_id: message.message_id.clone(),
                from: message.from.clone(),
                waiting_for,
            };
            if matches!(message.to, Recipient::NextSession {}) {
                handoffs.push(open(Vec::new()));
                continue;
            }
            let waiting: Vec<String> = state
                .sessions
                .values()
                .filter(|session| {
                    &session.workspace_id == workspace
                        && session.generation == state.generation
                        && owed_to(message, &session.session_id, false)
                })
                .map(|session| session.session_id.clone())
                .take(TODAY_FILES)
                .collect();
            // A message for a session that is gone is still owed to it.
            let waiting = match (&message.to, waiting.is_empty()) {
                (Recipient::Session { session_id }, true) => vec![session_id.clone()],
                _ => waiting,
            };
            messages.push(open(waiting));
        }
    }
    let messages = capped(&mut omitted, messages);
    let handoffs = capped(&mut omitted, handoffs);
    let mut failing = Vec::new();
    for (_, records) in state.checks.iter().filter(|(id, _)| ours(id)) {
        let mut seen = BTreeSet::new();
        // Newest first: the first record of a name is its latest result.
        for record in records {
            if seen.insert(record.name.as_str())
                && matches!(record.result, CheckResult::Failed { .. })
            {
                failing.push(OpenCheck {
                    name: record.name.clone(),
                    tree_oid: record.tree_oid.clone(),
                });
            }
        }
    }
    let failing_checks = capped(&mut omitted, failing);
    TodayOpen {
        admissions,
        intents,
        messages,
        handoffs,
        failing_checks,
        omitted: u32::try_from(omitted).unwrap_or(u32::MAX),
    }
}

impl Today {
    /// Drop listed files, then sessions, then open items, last-listed first,
    /// until `fits` says the summary is small enough. What is dropped is
    /// still counted: `files_edited` keeps its count, sessions go to
    /// `omitted_sessions` and open items to `open.omitted`.
    pub fn shrink_until(&mut self, mut fits: impl FnMut(&Today) -> bool) {
        while !fits(self) {
            if let Some(session) = self
                .sessions
                .iter_mut()
                .rev()
                .find(|session| !session.files.is_empty())
            {
                session.files.pop();
            } else if self.sessions.pop().is_some() {
                self.omitted_sessions = self.omitted_sessions.saturating_add(1);
            } else if self.open.pop() {
                self.open.omitted = self.open.omitted.saturating_add(1);
            } else {
                return;
            }
        }
    }
}

impl TodayOpen {
    /// Remove one item, from the last list that has one.
    fn pop(&mut self) -> bool {
        self.failing_checks.pop().is_some()
            || self.handoffs.pop().is_some()
            || self.messages.pop().is_some()
            || self.intents.pop().is_some()
            || self.admissions.pop().is_some()
    }
}
