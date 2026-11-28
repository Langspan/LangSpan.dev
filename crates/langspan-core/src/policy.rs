//! Coordination policies (roadmap B28,
//! [ADR 0036](../../../docs/adr/0036-coordination-policies.md)): one stated
//! answer for each kind of coordination case, the wait that a held file can
//! get instead of a refusal, and what each refusal was followed by.
//!
//! Only the policy for a current holder (`conflict`) can be set, per
//! workspace, to `refuse` (the default) or `wait`. Every other case is fixed.
//! A refusal is settled by the session's next admitted tool write or its
//! stop: `RefusalSettled` says which, and how long the retry took.

use crate::{
    CoreError, Decision, Event, ObservedFile, Outcome, REFUSAL_PATHS, RefusalKind, SourceOrigin,
    State, error, is_hex_digest, is_zero, known_session, normalized_files, normalized_paths,
    validate_id, validate_known_workspace,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The longest wait a workspace can set. Every wait is cut further to fit
/// the hook's budget.
pub const MAX_WAIT_MS: u32 = 2_000;
/// The wait set by `wait` without a stated bound: as long as the hook's
/// budget allows, up to `MAX_WAIT_MS`.
pub const DEFAULT_WAIT_MS: u32 = MAX_WAIT_MS;
/// A recorded wait is at most this long; a hook's budget is at most 4 s.
pub const MAX_WAITED_MS: u64 = 60_000;
/// Refusals kept per session until they are settled; an older one is
/// dropped unsettled.
pub const MAX_UNSETTLED: usize = 8;

/// A kind of coordination case, as the policy table names it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PolicyCase {
    /// Related work and possible semantic conflicts: briefings, peer
    /// notices, interface notices, break alerts and remote awareness.
    Advisory,
    /// Another session holds the file or an overlapping region now.
    Conflict,
    /// The file changed after this session last read it.
    StaleBase,
    /// The edit would change lines another live session wrote, unread.
    UnreadLines,
    /// A commit would take another live session's changes.
    CommitHeld,
    /// A landing without current integration evidence (`land`, exit code 6).
    LandingNotReady,
}

impl PolicyCase {
    /// Every case, in the order the table lists them.
    pub const ALL: [PolicyCase; 6] = [
        Self::Advisory,
        Self::Conflict,
        Self::StaleBase,
        Self::UnreadLines,
        Self::CommitHeld,
        Self::LandingNotReady,
    ];

    /// The case's wire name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Conflict => "conflict",
            Self::StaleBase => "stale_base",
            Self::UnreadLines => "unread_lines",
            Self::CommitHeld => "commit_held",
            Self::LandingNotReady => "landing_not_ready",
        }
    }

    /// The case its wire name names.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|case| case.name() == name)
    }

    /// The policy a case has when nobody set one, and the only one a fixed
    /// case can have.
    fn fixed(&self) -> Policy {
        match self {
            Self::Advisory => Policy::Advisory,
            _ => Policy::Refuse,
        }
    }
}

/// What LangSpan does in one kind of case.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Inform; never hold a write.
    Advisory,
    /// Wait for the holder, then revalidate the base before admitting.
    Wait,
    /// Refuse at once.
    Refuse,
}

impl Policy {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Wait => "wait",
            Self::Refuse => "refuse",
        }
    }
}

/// One row of the policy table in effect for a workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyRow {
    pub case: PolicyCase,
    pub policy: Policy,
    /// Whether `policy set` can change it. Only `conflict` is.
    pub configurable: bool,
    /// The bound of a wait, before the hook's budget cuts it. Zero unless
    /// the policy is `wait`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wait_ms: u32,
}

/// How a wait for a holder ended.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitResult {
    /// The holder's admission ended with the files unchanged; the write was
    /// admitted.
    Admitted,
    /// The holder still held a path when the wait ended.
    RefusedConflict,
    /// A path changed during the wait, or since this session last read it.
    RefusedStale,
    /// After the wait, the edit would change lines another session wrote,
    /// unread.
    RefusedUnread,
}

/// What a session did after a refusal: the first of its next admitted tool
/// write or its stop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SettledOutcome {
    /// A later write of one of the refused paths was admitted.
    RetriedAdmitted,
    /// The session's next admitted write was of other paths only.
    MovedOn,
    /// The session's turn stopped, or the session ended, with no admitted
    /// write in between.
    Stopped,
}

/// A refusal not yet followed by an admitted write or a stop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnsettledRefusal {
    pub kind: RefusalKind,
    pub paths: Vec<String>,
    pub at_ms: u64,
}

/// The policy table in effect for `workspace_id`.
pub fn table(state: &State, workspace_id: &str) -> Vec<PolicyRow> {
    rows(conflict_wait_ms(state, workspace_id))
}

fn rows(wait_ms: u32) -> Vec<PolicyRow> {
    PolicyCase::ALL
        .into_iter()
        .map(|case| match case {
            PolicyCase::Conflict => PolicyRow {
                case,
                policy: if wait_ms > 0 {
                    Policy::Wait
                } else {
                    Policy::Refuse
                },
                configurable: true,
                wait_ms,
            },
            _ => PolicyRow {
                case,
                policy: case.fixed(),
                configurable: false,
                wait_ms: 0,
            },
        })
        .collect()
}

/// The bound of a wait for a current holder in `workspace_id`; zero when a
/// holder is refused at once.
pub fn conflict_wait_ms(state: &State, workspace_id: &str) -> u32 {
    state.conflict_waits.get(workspace_id).copied().unwrap_or(0)
}

pub(crate) fn decide_set(
    state: &State,
    workspace_id: &str,
    case: PolicyCase,
    policy: Policy,
    wait_ms: Option<u32>,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if case != PolicyCase::Conflict {
        return Err(error(
            "policy_fixed",
            &format!(
                "The {} policy is fixed at {}; only conflict can be set",
                case.name(),
                case.fixed().name()
            ),
        ));
    }
    let wait_ms = match (policy, wait_ms) {
        (Policy::Advisory, _) => {
            return Err(error(
                "invalid_policy",
                "A conflict is refused or waited for; it cannot be advisory",
            ));
        }
        (_, Some(ms)) if ms > MAX_WAIT_MS => {
            return Err(error(
                "invalid_wait",
                &format!("A wait is 0 to {MAX_WAIT_MS} milliseconds"),
            ));
        }
        (Policy::Refuse, Some(ms)) if ms > 0 => {
            return Err(error(
                "invalid_wait",
                "A refusal does not wait; give --wait-ms only with wait",
            ));
        }
        (Policy::Refuse, _) => 0,
        (Policy::Wait, ms) => ms.unwrap_or(DEFAULT_WAIT_MS),
    };
    // A wait of zero is a refusal.
    let policy = if wait_ms == 0 {
        Policy::Refuse
    } else {
        Policy::Wait
    };
    Ok(Decision {
        events: vec![Event::PolicySet {
            workspace_id: workspace_id.into(),
            case,
            policy,
            wait_ms,
        }],
        outcome: Outcome::Policy {
            workspace_id: workspace_id.into(),
            rows: rows(wait_ms),
        },
    })
}

fn set(state: &mut State, workspace_id: &str, wait_ms: u32) {
    if wait_ms == 0 {
        state.conflict_waits.remove(workspace_id);
    } else {
        state.conflict_waits.insert(workspace_id.into(), wait_ms);
    }
}

pub(crate) fn apply_set(
    state: &mut State,
    workspace_id: &str,
    case: PolicyCase,
    policy: Policy,
    wait_ms: u32,
) -> Result<(), CoreError> {
    validate_known_workspace(state, workspace_id)?;
    let valid = case == PolicyCase::Conflict
        && wait_ms <= MAX_WAIT_MS
        && match policy {
            Policy::Wait => wait_ms > 0,
            Policy::Refuse => wait_ms == 0,
            Policy::Advisory => false,
        };
    if !valid {
        return Err(error(
            "invalid_event",
            "A policy record sets conflict to refuse, or to wait for 1 to 2000 milliseconds",
        ));
    }
    set(state, workspace_id, wait_ms);
    Ok(())
}

/// What a hook reports about one wait for a holder.
pub(crate) struct WaitReport<'a> {
    pub session_id: &'a str,
    pub paths: &'a [String],
    pub holder_session_id: Option<&'a String>,
    pub waited_ms: u64,
    pub result: WaitResult,
    pub changed: &'a [ObservedFile],
    pub input_digest: Option<&'a String>,
}

/// A wait the hook ran. With the workspace's policy at `wait`, a conflict
/// refusal is recorded here, by the wait's end, not by the admission that
/// found the holder. A path that changed during the wait (`changed`) is
/// refused here as a stale base, once: what the session was told about
/// becomes its known version, as an admission's stale refusal does.
pub(crate) fn decide_wait(
    state: &State,
    report: WaitReport<'_>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let session = known_session(state, report.session_id)?;
    let paths = normalized_paths(report.paths)?;
    if paths.is_empty() {
        return Err(error("invalid_paths", "A wait names at least one path"));
    }
    if let Some(holder) = report.holder_session_id {
        validate_id(holder, "holder session ID")?;
    }
    if report.waited_ms > MAX_WAITED_MS {
        return Err(error(
            "invalid_wait",
            "A recorded wait is at most 60000 milliseconds",
        ));
    }
    if report
        .input_digest
        .is_some_and(|digest| !is_hex_digest(digest, 64))
    {
        return Err(error(
            "invalid_digest",
            "An input digest is a SHA-256 in lowercase hex",
        ));
    }
    let changed = normalized_files(report.changed)?;
    if changed
        .iter()
        .any(|file| paths.binary_search(&file.path).is_err())
    {
        return Err(error(
            "invalid_base",
            "A changed file must be one of the paths waited for",
        ));
    }
    if !changed.is_empty() && report.result != WaitResult::RefusedStale {
        return Err(error(
            "invalid_wait",
            "Changed files are reported only with a stale refusal",
        ));
    }
    let paths: Vec<String> = paths.into_iter().take(REFUSAL_PATHS).collect();
    let refused = |kind| Event::WriteRefused {
        session_id: report.session_id.into(),
        workspace_id: session.workspace_id.clone(),
        kind,
        paths: paths.clone(),
        peer_session_id: report.holder_session_id.cloned(),
        input_digest: report.input_digest.cloned(),
        at_ms: now_ms,
        generation: state.generation,
    };
    let mut events = vec![Event::WriteWaited {
        session_id: report.session_id.into(),
        workspace_id: session.workspace_id.clone(),
        paths: paths.clone(),
        holder_session_id: report.holder_session_id.cloned(),
        waited_ms: report.waited_ms,
        result: report.result,
        at_ms: now_ms,
        generation: state.generation,
    }];
    match report.result {
        WaitResult::RefusedConflict => events.push(refused(RefusalKind::Conflict)),
        // Found by the hook during the wait. One found by the admission
        // after it was recorded by that admission.
        WaitResult::RefusedStale if !changed.is_empty() => {
            events.push(Event::SourceObserved {
                session_id: report.session_id.into(),
                files: changed,
                origin: SourceOrigin::Refused,
                at_ms: now_ms,
                generation: state.generation,
            });
            events.push(refused(RefusalKind::StaleBase));
        }
        _ => {}
    }
    Ok(Decision {
        events,
        outcome: Outcome::WaitRecorded {
            session_id: report.session_id.into(),
        },
    })
}

pub(crate) fn validate_waited(
    state: &State,
    session_id: &str,
    workspace_id: &str,
    paths: &[String],
    holder_session_id: Option<&String>,
    waited_ms: u64,
) -> Result<(), CoreError> {
    known_session(state, session_id)?;
    validate_known_workspace(state, workspace_id)?;
    normalized_paths(paths)?;
    if paths.is_empty()
        || paths.len() > REFUSAL_PATHS
        || waited_ms > MAX_WAITED_MS
        || holder_session_id.is_some_and(|holder| holder.len() > 200)
    {
        return Err(error(
            "invalid_event",
            "A wait record names 1 to 8 paths, at most one holder and at most 60000 milliseconds",
        ));
    }
    Ok(())
}

/// Keep a refusal of a write until the session's next admitted write or
/// stop settles it. A held commit is not a write and is not kept.
pub(crate) fn remember_refusal(
    state: &mut State,
    session_id: &str,
    kind: RefusalKind,
    paths: &[String],
    at_ms: u64,
    generation: u64,
) {
    if kind == RefusalKind::CommitHeld || generation != state.generation {
        return;
    }
    let kept = state.unsettled.entry(session_id.into()).or_default();
    kept.push(UnsettledRefusal {
        kind,
        paths: paths.to_vec(),
        at_ms,
    });
    if kept.len() > MAX_UNSETTLED {
        kept.remove(0);
    }
}

pub(crate) fn apply_settled(
    state: &mut State,
    session_id: &str,
    kind: RefusalKind,
    paths: &[String],
    refused_at_ms: u64,
) -> Result<(), CoreError> {
    known_session(state, session_id)?;
    if let Some(kept) = state.unsettled.get_mut(session_id) {
        kept.retain(|refusal| {
            !(refusal.kind == kind && refusal.at_ms == refused_at_ms && refusal.paths == paths)
        });
        if kept.is_empty() {
            state.unsettled.remove(session_id);
        }
    }
    Ok(())
}

/// The `RefusalSettled` events `events` call for: a tool write admitted
/// for a session settles each refusal it has kept, as retried when the
/// write names one of the refused paths and as moved on otherwise; a stop
/// or an end of the session settles them as stopped.
pub(crate) fn settle(state: &State, events: &[Event], now_ms: u64) -> Vec<Event> {
    let mut settled = Vec::new();
    let mut done = BTreeSet::new();
    for event in events {
        let (session_id, admitted) = match event {
            Event::LeaseGranted { lease } if lease.operation.is_some() => {
                (&lease.session_id, Some(&lease.paths))
            }
            Event::SessionStopped { session_id, .. } | Event::SessionEnded { session_id } => {
                (session_id, None)
            }
            _ => continue,
        };
        if !done.insert(session_id.clone()) {
            continue;
        }
        let Some(kept) = state.unsettled.get(session_id) else {
            continue;
        };
        let Some(workspace_id) = state
            .sessions
            .get(session_id)
            .map(|session| session.workspace_id.clone())
        else {
            continue;
        };
        for refusal in kept {
            let (outcome, ms_to_next_admitted) = match admitted {
                Some(paths) if refusal.paths.iter().any(|path| paths.contains(path)) => (
                    SettledOutcome::RetriedAdmitted,
                    Some(now_ms.saturating_sub(refusal.at_ms)),
                ),
                Some(_) => (SettledOutcome::MovedOn, None),
                None => (SettledOutcome::Stopped, None),
            };
            settled.push(Event::RefusalSettled {
                session_id: session_id.clone(),
                workspace_id: workspace_id.clone(),
                kind: refusal.kind,
                paths: refusal.paths.clone(),
                refused_at_ms: refusal.at_ms,
                outcome,
                ms_to_next_admitted,
                at_ms: now_ms,
                generation: state.generation,
            });
        }
    }
    settled
}
