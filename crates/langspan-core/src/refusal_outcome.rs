//! What a session did after a refusal (roadmap H9), derived from the event
//! log by the coordination report. Each edit refusal opens one pending
//! outcome per session, kind and path set; the next relevant event settles
//! it once. Only recorded events decide it, never model text: when the
//! events cannot tell, the outcome says so (`unknown`, `unknown_read`).
//!
//! Nothing here reaches a model, and nothing is held or refused because of
//! an outcome.
//!
//! The report runs in the daemon's single store worker, so an edit
//! admission waits behind it. Every event therefore costs the settler a
//! bounded number of index lookups, never a scan of what is pending: the
//! refusals pending are indexed by session and path, by holder and path,
//! and by deadline.

use std::collections::{BTreeMap, BTreeSet};

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

/// What makes refusals of one session one group: workspace, kind and the
/// refused path set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    workspace_id: String,
    kind: RefusalKind,
    paths: BTreeSet<String>,
}

/// One group's refusals of a session (`GroupKey`), not yet settled.
struct Pending {
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
    /// The outcome of a write of a refused path of a `kind` group: `digest`
    /// is the call's input digest, `None` for a shell command.
    fn written(&self, kind: RefusalKind, digest: Option<&str>) -> RefusalOutcome {
        if kind == RefusalKind::Conflict {
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

    /// A conflict still waiting for its holder to let go: the one state a
    /// lease ending can change.
    fn awaits_holder(&self) -> bool {
        self.holder_known && !self.holder_ended
    }
}

/// A session and a path: the key of the session-and-path indexes.
type SessionPath = (String, String);

/// Settles refusals from the events, in log order. The report feeds it
/// every event it reads, then opens a refusal for each counted one.
#[derive(Default)]
pub(crate) struct Settler {
    window_ms: u64,
    /// Per session, its refusals not yet settled.
    pending: BTreeMap<String, BTreeMap<GroupKey, Pending>>,
    /// Session and refused path to the session's pending groups with it.
    by_path: BTreeMap<SessionPath, BTreeSet<GroupKey>>,
    /// Holder and refused path to the conflicts waiting for that holder
    /// (`Pending::awaits_holder`), by refused session and group.
    by_holder: BTreeMap<SessionPath, BTreeSet<(String, GroupKey)>>,
    /// Pending groups by deadline, earliest first.
    deadlines: BTreeSet<(u64, String, GroupKey)>,
    /// Open admissions: lease ID to its session and paths.
    leases: BTreeMap<String, (String, Vec<String>)>,
    /// Session and path to how many of its open admissions hold the path.
    held: BTreeMap<SessionPath, u32>,
    /// Per session, outcome name to count.
    settled: BTreeMap<String, BTreeMap<&'static str, u32>>,
}

fn key(session: &str, path: &str) -> SessionPath {
    (session.to_owned(), path.to_owned())
}

/// Take `value` out of the set at `index`, dropping the set once empty.
fn unindex<K: Ord, V: Ord>(index: &mut BTreeMap<K, BTreeSet<V>>, at: K, value: &V) {
    if let Some(set) = index.get_mut(&at) {
        set.remove(value);
        if set.is_empty() {
            index.remove(&at);
        }
    }
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

    fn holds(&self, session: &str, path: &str) -> bool {
        self.held.contains_key(&key(session, path))
    }

    /// One admission of `holder` on `paths` is no longer open.
    fn unhold(&mut self, holder: &str, paths: &[String]) {
        for path in paths {
            let at = key(holder, path);
            if let Some(count) = self.held.get_mut(&at) {
                *count -= 1;
                if *count == 0 {
                    self.held.remove(&at);
                }
            }
        }
    }

    /// Index a conflict as waiting for its holder.
    fn await_holder(&mut self, session: &str, group: &GroupKey, peer: &str) {
        for path in &group.paths {
            self.by_holder
                .entry(key(peer, path))
                .or_default()
                .insert((session.to_owned(), group.clone()));
        }
    }

    /// Take a conflict off its holder's index.
    fn stop_awaiting(&mut self, session: &str, group: &GroupKey, peer: &str) {
        let entry = (session.to_owned(), group.clone());
        for path in &group.paths {
            unindex(&mut self.by_holder, key(peer, path), &entry);
        }
    }

    /// Remove one pending group and every index entry for it.
    fn remove(&mut self, session: &str, group: &GroupKey) -> Option<Pending> {
        let groups = self.pending.get_mut(session)?;
        let pending = groups.remove(group)?;
        if groups.is_empty() {
            self.pending.remove(session);
        }
        for path in &group.paths {
            unindex(&mut self.by_path, key(session, path), group);
        }
        if let (true, Some(peer)) = (pending.awaits_holder(), pending.peer.as_deref()) {
            self.stop_awaiting(session, group, peer);
        }
        self.deadlines
            .remove(&(pending.deadline_ms, session.to_owned(), group.clone()));
        Some(pending)
    }

    /// The session's pending groups with any of `paths`.
    fn touching<'a>(
        &self,
        session: &str,
        paths: impl IntoIterator<Item = &'a String>,
    ) -> BTreeSet<GroupKey> {
        paths
            .into_iter()
            .filter_map(|path| self.by_path.get(&key(session, path)))
            .flatten()
            .cloned()
            .collect()
    }

    /// Settle `abandoned` every refusal whose deadline is at or before
    /// `wall_ms`.
    pub(crate) fn expire(&mut self, wall_ms: u64) {
        while let Some((deadline_ms, session, group)) = self.deadlines.first().cloned() {
            if deadline_ms > wall_ms {
                break;
            }
            if self.remove(&session, &group).is_some() {
                self.settle(&session, RefusalOutcome::Abandoned);
            } else {
                self.deadlines.remove(&(deadline_ms, session, group));
            }
        }
    }

    /// Settle the session's refusals that a write of `paths` in
    /// `workspace_id` answers.
    fn write(&mut self, session: &str, workspace_id: &str, paths: &[String], digest: Option<&str>) {
        for group in self.touching(session, paths) {
            if group.workspace_id != workspace_id {
                continue;
            }
            if let Some(pending) = self.remove(session, &group) {
                self.settle(session, pending.written(group.kind, digest));
            }
        }
    }

    /// The admission `lease_id` ended: mark the conflicts waiting for its
    /// holder whose paths that holder no longer holds. Only a conflict with
    /// one of the lease's paths can change.
    fn lease_ended(&mut self, lease_id: &str) {
        let Some((holder, paths)) = self.leases.remove(lease_id) else {
            return;
        };
        self.unhold(&holder, &paths);
        let waiting: BTreeSet<(String, GroupKey)> = paths
            .iter()
            .filter_map(|path| self.by_holder.get(&key(&holder, path)))
            .flatten()
            .cloned()
            .collect();
        for (session, group) in waiting {
            if group.paths.iter().any(|path| self.holds(&holder, path)) {
                continue;
            }
            self.stop_awaiting(&session, &group, &holder);
            if let Some(pending) = self
                .pending
                .get_mut(&session)
                .and_then(|groups| groups.get_mut(&group))
            {
                pending.holder_ended = true;
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
                for path in &lease.paths {
                    *self.held.entry(key(&lease.session_id, path)).or_default() += 1;
                }
                if let Some((holder, replaced)) = self.leases.insert(
                    lease.lease_id.clone(),
                    (lease.session_id.clone(), lease.paths.clone()),
                ) {
                    // Not expected: a lease ID granted twice. Keep the
                    // counts to the paths of the open grant.
                    self.unhold(&holder, &replaced);
                }
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
                let ids: Vec<String> = self.leases.keys().cloned().collect();
                for id in ids {
                    self.lease_ended(&id);
                }
            }
            Event::LeaseCompleted { lease_id } | Event::LeaseExpired { lease_id } => {
                self.lease_ended(lease_id);
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
                let groups = self.touching(session_id, files.iter().map(|file| &file.path));
                if let Some(pending) = self.pending.get_mut(session_id) {
                    for group in groups {
                        if let Some(pending) = pending.get_mut(&group) {
                            pending.read = true;
                        }
                    }
                }
            }
            Event::SessionEnded { session_id } => {
                let ids: Vec<String> = self
                    .leases
                    .iter()
                    .filter(|(_, (session, _))| session == session_id)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    self.lease_ended(&id);
                }
                if wall_ms.is_some() {
                    let groups: Vec<GroupKey> = self
                        .pending
                        .get(session_id)
                        .map(|groups| groups.keys().cloned().collect())
                        .unwrap_or_default();
                    for group in groups {
                        self.remove(session_id, &group);
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
        let group = GroupKey {
            workspace_id: workspace_id.clone(),
            kind,
            paths: paths.iter().cloned().collect(),
        };
        let holder_known =
            peer.is_some_and(|peer| group.paths.iter().any(|path| self.holds(peer, path)));
        let deadline_ms = wall_ms.saturating_add(self.window_ms);
        // Take the group out of every index, update it, and put it back.
        let mut pending = self.remove(session, &group).unwrap_or(Pending {
            digests: BTreeSet::new(),
            digest_missing: false,
            peer: None,
            holder_known: false,
            holder_ended: false,
            read: false,
            reads_reported,
            deadline_ms,
        });
        match digest {
            Some(digest) => {
                pending.digests.insert(digest.into());
            }
            None => pending.digest_missing = true,
        }
        // The latest refusal names the holder now, and restarts the window.
        if kind == RefusalKind::Conflict {
            pending.peer = peer.map(str::to_owned);
            pending.holder_known = holder_known;
            pending.holder_ended = false;
        }
        pending.read = false;
        pending.deadline_ms = deadline_ms;
        for path in &group.paths {
            self.by_path
                .entry(key(session, path))
                .or_default()
                .insert(group.clone());
        }
        if let (true, Some(peer)) = (pending.awaits_holder(), pending.peer.as_deref()) {
            self.await_holder(session, &group, peer);
        }
        self.deadlines
            .insert((deadline_ms, session.clone(), group.clone()));
        self.pending
            .entry(session.clone())
            .or_default()
            .insert(group, pending);
    }

    /// Settle `abandoned` what is past its deadline at `to_ms`, then give,
    /// per session, each outcome's count, with the rest as `PENDING`.
    pub(crate) fn finish(mut self, to_ms: u64) -> BTreeMap<String, BTreeMap<&'static str, u32>> {
        self.expire(to_ms);
        let mut settled = self.settled;
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
