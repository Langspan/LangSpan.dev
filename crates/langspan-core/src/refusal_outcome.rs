//! What a session did after a refusal (roadmap H9), derived from the event
//! log by the coordination report. Each edit refusal opens one pending
//! outcome per session, kind and path set; the next relevant event settles
//! it once. Only recorded events decide it, never model text: when the
//! events cannot tell, the outcome says so (`unknown`, `unknown_read`).
//!
//! Nothing here reaches a model, and nothing is held or refused because of
//! an outcome.

use std::collections::{BTreeMap, BTreeSet};
use std::{iter, mem};

use crate::{Event, LeaseRelation, RefusalKind, SHELL_TOOL, SourceOrigin, ToolResult};

/// How long a refused session has to write one of the refused paths before
/// the refusal is settled `abandoned`: 30 minutes.
pub const REFUSAL_WINDOW_MS: u64 = 30 * 60_000;

/// The key a refusal not yet settled is counted under in
/// `ReportCounts::refusal_outcomes`. It is not an outcome.
pub const PENDING: &str = "pending";

/// How a refusal was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalOutcome {
    /// After the refusal the session reported a read of a refused path and
    /// then wrote one, or wrote one with a call that differs from every
    /// refused one.
    Adapted,
    /// The session wrote a refused path with a call identical to a refused
    /// one (same input digest), or with a shell command, with no read
    /// reported in between.
    RetriedUnchanged,
    /// A conflict: the holder's admissions of the paths ended, then the
    /// session wrote one.
    Waited,
    /// No write of the paths by the session within `REFUSAL_WINDOW_MS`, or
    /// the session ended first.
    Abandoned,
    /// The host reports no reads (Codex) and the write was the identical
    /// call or a shell command: a read may or may not have come first.
    UnknownRead,
    /// The events cannot tell: a refusal recorded without its call's digest,
    /// or a conflict whose holder's admission is not in the events read.
    Unknown,
}

impl RefusalOutcome {
    /// The outcome as its wire name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Adapted => "adapted",
            Self::RetriedUnchanged => "retried_unchanged",
            Self::Waited => "waited",
            Self::Abandoned => "abandoned",
            Self::UnknownRead => "unknown_read",
            Self::Unknown => "unknown",
        }
    }
}

/// One refusal, or several of the same session, kind and path set, not yet
/// settled.
struct Pending {
    workspace_id: String,
    kind: RefusalKind,
    paths: BTreeSet<String>,
    /// The refused calls' input digests.
    digests: BTreeSet<String>,
    /// A refusal of the group was recorded without its call's digest.
    digest_missing: bool,
    /// For a conflict: the session the refusal named.
    peer: Option<String>,
    /// For a conflict: an admission of the peer on a refused path was open
    /// when the refusal was recorded.
    holder_known: bool,
    /// For a conflict: since then, no admission of the peer on a refused
    /// path is open.
    holder_ended: bool,
    /// The session reported a read of a refused path since the refusal.
    read: bool,
    /// The host reports reads (not Codex).
    reads_reported: bool,
    /// Wall time it is settled `abandoned` at.
    deadline_ms: u64,
}

impl Pending {
    fn touches(&self, paths: &[String]) -> bool {
        paths.iter().any(|path| self.paths.contains(path))
    }

    /// The outcome of a write of a refused path: `digest` is the call's
    /// input digest, `None` for a shell command.
    fn written(&self, digest: Option<&str>) -> RefusalOutcome {
        if self.kind == RefusalKind::Conflict {
            if !self.holder_known {
                return RefusalOutcome::Unknown;
            }
            if self.holder_ended {
                return RefusalOutcome::Waited;
            }
        }
        if self.read {
            return RefusalOutcome::Adapted;
        }
        let unchanged = if self.reads_reported {
            RefusalOutcome::RetriedUnchanged
        } else {
            RefusalOutcome::UnknownRead
        };
        match digest {
            None => unchanged,
            Some(digest) if self.digests.contains(digest) => unchanged,
            Some(_) if self.digest_missing => RefusalOutcome::Unknown,
            Some(_) => RefusalOutcome::Adapted,
        }
    }
}

/// Settles refusals from the events, in log order. The report feeds it
/// every event it reads, then opens a refusal for each counted one.
#[derive(Default)]
pub(crate) struct Settler {
    window_ms: u64,
    /// Per session, its refusals not yet settled.
    pending: BTreeMap<String, Vec<Pending>>,
    /// Open admissions: lease ID to its session and paths.
    leases: BTreeMap<String, (String, Vec<String>)>,
    /// Per session, outcome name to count.
    settled: BTreeMap<String, BTreeMap<&'static str, u32>>,
    /// The earliest deadline pending, so most events skip the expiry scan.
    next_deadline_ms: Option<u64>,
}

impl Settler {
    pub(crate) fn new(window_ms: u64) -> Self {
        Self {
            window_ms,
            ..Self::default()
        }
    }

    fn settle(&mut self, session: &str, outcome: RefusalOutcome) {
        let count = self
            .settled
            .entry(session.into())
            .or_default()
            .entry(outcome.name())
            .or_default();
        *count = count.saturating_add(1);
    }

    /// Settle `abandoned` every refusal whose deadline is at or before
    /// `wall_ms`.
    pub(crate) fn expire(&mut self, wall_ms: u64) {
        if self.next_deadline_ms.is_none_or(|next| next > wall_ms) {
            return;
        }
        let mut expired = Vec::new();
        for (session, groups) in &mut self.pending {
            let before = groups.len();
            groups.retain(|group| group.deadline_ms > wall_ms);
            expired.extend(iter::repeat_n(session.clone(), before - groups.len()));
        }
        for session in expired {
            self.settle(&session, RefusalOutcome::Abandoned);
        }
        self.pending.retain(|_, groups| !groups.is_empty());
        self.next_deadline_ms = self
            .pending
            .values()
            .flatten()
            .map(|group| group.deadline_ms)
            .min();
    }

    /// Settle the session's refusals that a write of `paths` in
    /// `workspace_id` answers.
    fn write(&mut self, session: &str, workspace_id: &str, paths: &[String], digest: Option<&str>) {
        let Some(groups) = self.pending.get_mut(session) else {
            return;
        };
        let mut outcomes = Vec::new();
        groups.retain(|group| {
            if group.workspace_id == workspace_id && group.touches(paths) {
                outcomes.push(group.written(digest));
                false
            } else {
                true
            }
        });
        for outcome in outcomes {
            self.settle(session, outcome);
        }
    }

    /// The peer's admissions ended: mark its conflicts whose paths it no
    /// longer holds.
    fn holder_released(&mut self, peer: &str) {
        let held: BTreeSet<&String> = self
            .leases
            .values()
            .filter(|(session, _)| session == peer)
            .flat_map(|(_, paths)| paths)
            .collect();
        for group in self.pending.values_mut().flatten() {
            if group.peer.as_deref() == Some(peer)
                && group.holder_known
                && !group.paths.iter().any(|path| held.contains(path))
            {
                group.holder_ended = true;
            }
        }
    }

    /// Fold one event placed at `wall_ms` (`None` for an event with no wall
    /// time, which settles nothing).
    pub(crate) fn observe(&mut self, event: &Event, wall_ms: Option<u64>) {
        if let Some(wall_ms) = wall_ms {
            self.expire(wall_ms);
        }
        match event {
            Event::LeaseGranted { lease } => {
                self.leases.insert(
                    lease.lease_id.clone(),
                    (lease.session_id.clone(), lease.paths.clone()),
                );
                // A manual `admit` is not a write.
                if let (Some(operation), Some(_)) = (&lease.operation, wall_ms) {
                    self.write(
                        &lease.session_id,
                        &lease.workspace_id,
                        &lease.paths,
                        Some(&operation.input_digest),
                    );
                }
            }
            // A new generation holds none of the earlier one's admissions,
            // and no event ends them one by one.
            Event::Booted { .. } => {
                let holders: BTreeSet<String> = self
                    .leases
                    .values()
                    .map(|(session, _)| session.clone())
                    .collect();
                self.leases.clear();
                for holder in holders {
                    self.holder_released(&holder);
                }
            }
            Event::LeaseCompleted { lease_id } | Event::LeaseExpired { lease_id } => {
                if let Some((session, _)) = self.leases.remove(lease_id) {
                    self.holder_released(&session);
                }
            }
            Event::OperationObserved { observation }
                if wall_ms.is_some()
                    && observation.tool == SHELL_TOOL
                    && observation.result == ToolResult::Succeeded
                    && matches!(
                        observation.lease_relation,
                        LeaseRelation::NotAdmitted
                            | LeaseRelation::ScopeMismatch
                            | LeaseRelation::InputMismatch
                    ) =>
            {
                self.write(
                    &observation.session_id,
                    &observation.workspace_id,
                    &observation.paths,
                    None,
                );
            }
            Event::SourceObserved {
                session_id,
                files,
                origin: SourceOrigin::Read,
                ..
            } if wall_ms.is_some() => {
                for group in self.pending.get_mut(session_id).into_iter().flatten() {
                    if files.iter().any(|file| group.paths.contains(&file.path)) {
                        group.read = true;
                    }
                }
            }
            Event::SessionEnded { session_id } => {
                self.leases.retain(|_, (session, _)| session != session_id);
                self.holder_released(session_id);
                if wall_ms.is_some()
                    && let Some(groups) = self.pending.remove(session_id)
                {
                    for _ in groups {
                        self.settle(session_id, RefusalOutcome::Abandoned);
                    }
                }
            }
            _ => {}
        }
    }

    /// Open (or join) a pending outcome for a counted `write_refused` at
    /// `wall_ms`. A held commit is not an edit refusal and opens none.
    pub(crate) fn open(&mut self, refusal: &Event, reads_reported: bool, wall_ms: u64) {
        let Event::WriteRefused {
            session_id: session,
            workspace_id,
            kind,
            paths,
            peer_session_id: peer,
            input_digest: digest,
            ..
        } = refusal
        else {
            return;
        };
        let (kind, peer, digest) = (*kind, peer.as_deref(), digest.as_deref());
        if kind == RefusalKind::CommitHeld || paths.is_empty() {
            return;
        }
        let paths: BTreeSet<String> = paths.iter().cloned().collect();
        let holder_known = peer.is_some_and(|peer| {
            self.leases.values().any(|(session, held)| {
                session == peer && held.iter().any(|path| paths.contains(path))
            })
        });
        let deadline_ms = wall_ms.saturating_add(self.window_ms);
        self.next_deadline_ms = Some(
            self.next_deadline_ms
                .map_or(deadline_ms, |next| next.min(deadline_ms)),
        );
        let groups = self.pending.entry(session.clone()).or_default();
        let index = match groups.iter().position(|group| {
            group.workspace_id == *workspace_id && group.kind == kind && group.paths == paths
        }) {
            Some(index) => index,
            None => {
                groups.push(Pending {
                    workspace_id: workspace_id.clone(),
                    kind,
                    paths,
                    digests: BTreeSet::new(),
                    digest_missing: false,
                    peer: None,
                    holder_known: false,
                    holder_ended: false,
                    read: false,
                    reads_reported,
                    deadline_ms,
                });
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        match digest {
            Some(digest) => {
                group.digests.insert(digest.into());
            }
            None => group.digest_missing = true,
        }
        // The latest refusal names the holder now, and restarts the window.
        if kind == RefusalKind::Conflict {
            group.peer = peer.map(str::to_owned);
            group.holder_known = holder_known;
            group.holder_ended = false;
        }
        group.read = false;
        group.deadline_ms = deadline_ms;
    }

    /// Settle `abandoned` what is past its deadline at `to_ms`, then give,
    /// per session, each outcome's count, with the rest as `PENDING`.
    pub(crate) fn finish(mut self, to_ms: u64) -> BTreeMap<String, BTreeMap<&'static str, u32>> {
        self.expire(to_ms);
        let mut settled = mem::take(&mut self.settled);
        for (session, groups) in self.pending {
            if groups.is_empty() {
                continue;
            }
            let count = settled
                .entry(session)
                .or_default()
                .entry(PENDING)
                .or_default();
            *count = count.saturating_add(u32::try_from(groups.len()).unwrap_or(u32::MAX));
        }
        settled
    }
}
