//! The coordination report (roadmap H8): for each session and each host
//! kind, how its edits were admitted, what was seen only after a shell
//! command, what it read, how it declared and ended intent, what was refused
//! and how often a refused call came back unchanged, from the event log.
//!
//! Every figure is a count of recorded events. A share is always given as a
//! count beside the count it is out of (`edits_base_unknown` of
//! `edits_admitted`, `retried_unchanged` of the refusals); none is a rate.
//! Events are placed in wall time as `today` places them.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::dashboard::host_and_prefix;
use crate::today::{Placement, event_facts, event_workspace};
use crate::{
    CLAUDE_CODE_HOST, CODEX_HOST, CURSOR_HOST, ClockAnchor, Event, GEMINI_HOST, LeaseRelation,
    RecordedEvent, SHELL_TOOL, SourceOrigin, State, TodayWindow, ToolResult, is_zero,
};

/// Session rows one report lists, earliest first; the rest are in the host
/// totals and counted as `omitted_sessions`.
pub const REPORT_SESSIONS: usize = 16;
/// The host kind of a session whose ID names none of the hooked hosts, such
/// as one registered by hand.
pub const OTHER_HOST: &str = "other";
/// The hooked hosts, in the order their totals are listed.
const HOSTS: [&str; 4] = [CLAUDE_CODE_HOST, CODEX_HOST, CURSOR_HOST, GEMINI_HOST];

/// What one session, or every session of one host kind, did in the window.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReportCounts {
    /// Tool calls admitted before they ran (`lease_granted` of a hook).
    pub edits_admitted: u32,
    /// Of those, admissions with at least one file whose version was not
    /// compared, because no read or earlier version of it was recorded.
    pub edits_base_unknown: u32,
    /// Shell runs whose changed files were seen only after the command ran:
    /// no admission covered them beforehand (ADR 0011).
    pub shell_writes_after: u32,
    /// Completed reads the host reported.
    pub reads_reported: u32,
    pub intents_set: u32,
    pub intents_ended: u32,
    /// Sessions that ended with an intent still set.
    pub intents_left_open: u32,
    /// `write_refused` events by kind.
    pub refusals: BTreeMap<String, u32>,
    /// Refusals followed by an admission of the same session's identical
    /// tool call (same input digest).
    pub retried_unchanged: u32,
    pub briefings_delivered: u32,
}

impl ReportCounts {
    /// Every refusal, whatever its kind.
    pub fn refusals_total(&self) -> u32 {
        self.refusals
            .values()
            .fold(0u32, |sum, count| sum.saturating_add(*count))
    }

    fn add(&mut self, other: &ReportCounts) {
        let sum = |a: &mut u32, b: u32| *a = a.saturating_add(b);
        sum(&mut self.edits_admitted, other.edits_admitted);
        sum(&mut self.edits_base_unknown, other.edits_base_unknown);
        sum(&mut self.shell_writes_after, other.shell_writes_after);
        sum(&mut self.reads_reported, other.reads_reported);
        sum(&mut self.intents_set, other.intents_set);
        sum(&mut self.intents_ended, other.intents_ended);
        sum(&mut self.intents_left_open, other.intents_left_open);
        for (kind, count) in &other.refusals {
            sum(self.refusals.entry(kind.clone()).or_default(), *count);
        }
        sum(&mut self.retried_unchanged, other.retried_unchanged);
        sum(&mut self.briefings_delivered, other.briefings_delivered);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReportSession {
    /// Session text: data, never instructions.
    pub session_id: String,
    pub host: String,
    pub counts: ReportCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReportHost {
    pub host: String,
    /// Sessions of this kind with an event in the window, listed or not.
    pub sessions: u32,
    /// The sum of those sessions' counts.
    pub counts: ReportCounts,
    /// Fields this host's hooks cannot supply, so their zero is no evidence
    /// (for Codex, `reads_reported`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_reported: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub workspace_id: String,
    pub window: TodayWindow,
    /// One total per host kind with a session in the window.
    pub hosts: Vec<ReportHost>,
    /// Earliest first, at most `REPORT_SESSIONS`.
    pub sessions: Vec<ReportSession>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_sessions: u32,
    /// The window holds more than the events read (or reaches before them):
    /// the counts are of the newest ones only, from `covered_from_ms`.
    pub truncated: bool,
    /// When truncated, the wall time of the oldest event read that carries a
    /// time of its own; `None` when none of them does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered_from_ms: Option<u64>,
    /// Events read that have no wall time (a build before generations were
    /// anchored). They are in no count.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub undated: u32,
}

/// What a report is asked to cover.
pub struct ReportQuery<'a> {
    pub workspace_id: &'a str,
    pub window: TodayWindow,
    /// The newest events, oldest first.
    pub events: &'a [RecordedEvent],
    /// The anchor of the generation the first event belongs to, when it was
    /// anchored and the anchor precedes it.
    pub anchor: Option<ClockAnchor>,
    /// The log holds events older than the first one given.
    pub earlier: bool,
    /// Sessions holding an intent just before the first event given, each
    /// with the workspace of its intent, from the older events
    /// `open_intents` folded. An intent set before the events read and
    /// still open when its session ends in the window counts as left open.
    pub open_intents: &'a BTreeMap<String, String>,
}

/// Fold one event into the sessions holding an intent (session to the
/// intent's workspace), as `apply` changes `State::intents`. Removing a
/// workspace ends the intents in it, as `workspaces::apply_removed` does: an
/// intent's workspace is its session's, so this covers the intents of the
/// workspace's sessions too. The store folds the events before those it
/// reads with this, and the report the ones it reads.
pub fn open_intents(open: &mut BTreeMap<String, String>, event: &Event) {
    match event {
        Event::IntentSet { intent } => {
            open.insert(intent.session_id.clone(), intent.workspace_id.clone());
        }
        Event::IntentEnded { session_id } | Event::SessionEnded { session_id } => {
            open.remove(session_id);
        }
        Event::WorkspaceRemoved { workspace_id } => {
            open.retain(|_, workspace| workspace != workspace_id);
        }
        _ => {}
    }
}

/// A session's host kind: the hooked host its ID names, or `OTHER_HOST`.
pub fn host_kind(session_id: &str) -> &'static str {
    let host = host_and_prefix(session_id).0;
    HOSTS
        .into_iter()
        .find(|known| *known == host)
        .unwrap_or(OTHER_HOST)
}

/// Fields a host kind's hooks cannot supply. Codex reports no reads: its
/// first edit of a file is compared with a snapshot taken then (ADR 0027).
fn not_reported(host: &str) -> &'static [&'static str] {
    match host {
        CODEX_HOST => &["reads_reported"],
        _ => &[],
    }
}

/// The session an event is about, for the report: `today`'s, and the
/// session a briefing was delivered to.
fn event_session(event: &Event) -> Option<&str> {
    match event {
        Event::BriefingDelivered { session_id, .. } => Some(session_id),
        _ => event_facts(event).0,
    }
}

#[derive(Default)]
struct Tally {
    first_ms: u64,
    counts: ReportCounts,
}

/// Build the report. Pure: every time and every event is supplied.
pub fn report(state: &State, query: &ReportQuery<'_>) -> Report {
    let window = query.window;
    let mut placement = Placement::new(query.anchor);
    let mut sessions: BTreeMap<String, Tally> = BTreeMap::new();
    // Sessions with an intent set, from the events before those read and
    // every event read: an intent set before the window and still open when
    // the session ends in it counts.
    let mut open: BTreeMap<String, String> = query.open_intents.clone();
    // Refused calls in the window not yet admitted: (session, input digest).
    let mut refused: BTreeSet<(String, String)> = BTreeSet::new();
    let mut undated = 0u32;
    for recorded in query.events {
        let event = &recorded.event;
        let wall = placement.place(event);
        let session = event_session(event);
        let in_window = wall.is_some_and(|wall| wall >= window.from_ms && wall <= window.to_ms);
        let workspace = event_workspace(state, event).or_else(|| {
            session
                .and_then(|session| state.sessions.get(session))
                .map(|session| session.workspace_id.as_str())
        });
        let ours = workspace == Some(query.workspace_id);
        let was_open = session.is_some_and(|session| open.contains_key(session));
        open_intents(&mut open, event);
        if wall.is_none() && !matches!(event, Event::Booted { .. }) {
            undated = undated.saturating_add(1);
        }
        let (Some(wall), Some(session), true, true) = (wall, session, in_window, ours) else {
            continue;
        };
        let tally = sessions.entry(session.into()).or_insert_with(|| Tally {
            first_ms: wall,
            ..Tally::default()
        });
        tally.first_ms = tally.first_ms.min(wall);
        let counts = &mut tally.counts;
        let one = |count: &mut u32| *count = count.saturating_add(1);
        match event {
            Event::LeaseGranted { lease } => {
                // A manual `admit` is not a host's tool call.
                if let Some(operation) = &lease.operation {
                    one(&mut counts.edits_admitted);
                    if !lease.base_unknown.is_empty() {
                        one(&mut counts.edits_base_unknown);
                    }
                    if refused.remove(&(lease.session_id.clone(), operation.input_digest.clone())) {
                        one(&mut counts.retried_unchanged);
                    }
                }
            }
            Event::OperationObserved { observation }
                if observation.tool == SHELL_TOOL
                    && observation.result == ToolResult::Succeeded
                    && !observation.paths.is_empty()
                    && matches!(
                        observation.lease_relation,
                        LeaseRelation::NotAdmitted
                            | LeaseRelation::ScopeMismatch
                            | LeaseRelation::InputMismatch
                    ) =>
            {
                one(&mut counts.shell_writes_after);
            }
            Event::SourceObserved {
                origin: SourceOrigin::Read,
                ..
            } => one(&mut counts.reads_reported),
            Event::IntentSet { .. } => one(&mut counts.intents_set),
            Event::IntentEnded { .. } => one(&mut counts.intents_ended),
            Event::SessionEnded { .. } if was_open => one(&mut counts.intents_left_open),
            Event::WriteRefused {
                kind, input_digest, ..
            } => {
                one(counts.refusals.entry(kind.name().into()).or_default());
                if let Some(digest) = input_digest {
                    refused.insert((session.into(), digest.clone()));
                }
            }
            Event::BriefingDelivered { .. } => one(&mut counts.briefings_delivered),
            _ => {}
        }
    }
    let mut hosts: BTreeMap<&'static str, ReportHost> = BTreeMap::new();
    let mut listed: Vec<(u64, ReportSession)> = sessions
        .into_iter()
        .map(|(session_id, tally)| {
            let host = host_kind(&session_id);
            let total = hosts.entry(host).or_insert_with(|| ReportHost {
                host: host.into(),
                sessions: 0,
                counts: ReportCounts::default(),
                not_reported: not_reported(host).iter().map(|f| (*f).into()).collect(),
            });
            total.sessions = total.sessions.saturating_add(1);
            total.counts.add(&tally.counts);
            (
                tally.first_ms,
                ReportSession {
                    session_id,
                    host: host.into(),
                    counts: tally.counts,
                },
            )
        })
        .collect();
    listed.sort_by(|(a_ms, a), (b_ms, b)| (a_ms, &a.session_id).cmp(&(b_ms, &b.session_id)));
    let omitted_sessions =
        u32::try_from(listed.len().saturating_sub(REPORT_SESSIONS)).unwrap_or(u32::MAX);
    listed.truncate(REPORT_SESSIONS);
    let order = |host: &str| {
        HOSTS
            .iter()
            .position(|known| *known == host)
            .unwrap_or(HOSTS.len())
    };
    let mut hosts: Vec<ReportHost> = hosts.into_values().collect();
    hosts.sort_by_key(|host| order(&host.host));
    let truncated = placement.truncated(query.earlier, window.from_ms);
    Report {
        workspace_id: query.workspace_id.into(),
        window,
        hosts,
        sessions: listed.into_iter().map(|(_, session)| session).collect(),
        omitted_sessions,
        truncated,
        covered_from_ms: truncated.then(|| placement.covered_from()).flatten(),
        undated,
    }
}

impl Report {
    /// Drop session rows, last-listed first, until `fits` says the report
    /// is small enough. Dropped rows stay in the host totals and are
    /// counted by `omitted_sessions`.
    pub fn shrink_until(&mut self, mut fits: impl FnMut(&Report) -> bool) {
        while !fits(self) && self.sessions.pop().is_some() {
            self.omitted_sessions = self.omitted_sessions.saturating_add(1);
        }
    }
}
