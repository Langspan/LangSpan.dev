//! The coordination state `apply` folds the event log into, and its stored
//! form. Code outside the core reads it; only `apply` and the constructors
//! here change it.

use serde::{Deserialize, Serialize};

// `BTreeMap` is the crate root's import, which the core's import boundary
// already allows.
use crate::{
    Adoption, AutoCheck, BTreeMap, CheckNotice, CheckRecord, CheckoutChange, CheckoutRun,
    ClockAnchor, Counter, Intent, InterfaceNotice, KeptTests, Landing, Lease, Message, Note,
    PathRefusal, PresenceSetting, RecentRefusal, RemoteLink, RemotePeer, Rename, RetractedMessage,
    ScanCoverage, ScanFailure, Session, SessionActivity, SessionLiveness, TestLearning, Trail,
    Turn, TurnWaiter, UncoordinatedChange, UninstalledHost, UnsettledRefusal, Workspace,
    WorkspaceGit, WriteRun,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub generation: u64,
    pub workspaces: BTreeMap<String, Workspace>,
    pub sessions: BTreeMap<String, Session>,
    pub intents: BTreeMap<String, Intent>,
    pub leases: BTreeMap<String, Lease>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub session_liveness: BTreeMap<String, SessionLiveness>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspace_git: BTreeMap<String, WorkspaceGit>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub activity: BTreeMap<String, SessionActivity>,
    /// Workspaces the owner chose to publish, by workspace ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub remote_links: BTreeMap<String, RemoteLink>,
    /// Latest publication per teammate, keyed `workspace:participant`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub remote_peers: BTreeMap<String, RemotePeer>,
    /// Check records per workspace, newest first, at most `MAX_CHECKS`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checks: BTreeMap<String, Vec<CheckRecord>>,
    /// Landings per workspace, newest first, at most `MAX_LANDINGS`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub landings: BTreeMap<String, Vec<Landing>>,
    /// Push turns held now, keyed `workspace:onto`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub turns: BTreeMap<String, Turn>,
    /// Landings waiting for each push turn, keyed `workspace:onto`, first in
    /// line first, at most `MAX_TURN_QUEUE` each.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub turn_queues: BTreeMap<String, Vec<TurnWaiter>>,
    /// Interface changes per workspace, newest first, at most `MAX_NOTICES`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub interface_notices: BTreeMap<String, Vec<InterfaceNotice>>,
    /// Remembered notes per workspace, newest first, at most `MAX_NOTES`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, Vec<Note>>,
    /// Unacknowledged messages per workspace, newest first, at most
    /// `MAX_MESSAGES`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub messages: BTreeMap<String, Vec<Message>>,
    /// The latest messages sent per workspace, newest first, at most
    /// `MAX_MESSAGES`, with every session that received each one and
    /// whether it was withdrawn: what a withdrawal and the recap read.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub message_history: BTreeMap<String, Vec<Message>>,
    /// IDs of the messages per workspace that newer ones pushed out of
    /// `messages` unread, newest first, at most `MAX_MESSAGES`: what a
    /// status query reads to tell a drop from a completion.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dropped_messages: BTreeMap<String, Vec<String>>,
    /// Withdrawn messages each session received and has not yet been told
    /// to disregard, per session, newest first, at most `MAX_MESSAGES`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub retracted_notices: BTreeMap<String, Vec<RetractedMessage>>,
    /// Workspaces whose last scan attempt failed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scan_failures: BTreeMap<String, ScanFailure>,
    /// Workspaces whose last completed scan left something unversioned.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scan_coverage: BTreeMap<String, ScanCoverage>,
    /// Checkout changes per workspace, newest first, at most `MAX_CHECKOUTS`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checkouts: BTreeMap<String, Vec<CheckoutChange>>,
    /// Sessions' shell runs that moved the checkout, per workspace, newest
    /// first: the newest `MAX_CHECKOUT_RUNS`, and any older run that
    /// explains a held checkout change (at most `MAX_CHECKOUTS` more).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checkout_runs: BTreeMap<String, Vec<CheckoutRun>>,
    /// Uncoordinated changes per workspace, newest first, at most
    /// `MAX_UNCOORDINATED`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub uncoordinated: BTreeMap<String, Vec<UncoordinatedChange>>,
    /// The automatic check per workspace, for the workspaces where the owner
    /// turned one on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub auto_checks: BTreeMap<String, AutoCheck>,
    /// Automatic check results not yet delivered, per session, newest first.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub check_notices: BTreeMap<String, Vec<CheckNotice>>,
    /// Tests that flip on unchanged code, learned per workspace.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub test_learning: BTreeMap<String, TestLearning>,
    /// Agent hosts present on this machine with no LangSpan install that
    /// covers them, by host name (roadmap A7). For the owner; agents are
    /// not told.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub uninstalled_hosts: BTreeMap<String, UninstalledHost>,
    /// Sessions whose presence setting is not the default, by session ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub presence: BTreeMap<String, PresenceSetting>,
    /// Ended sessions whose uncommitted work another session took over, by
    /// the ended session's ID (issue #161). One adoption per ended session.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub adoptions: BTreeMap<String, Adoption>,
    /// Reserved numbers per repository, then key (ADR 0040); not in `status`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) reservations: BTreeMap<String, BTreeMap<String, Counter>>,
    /// Recorded renames per workspace, newest first, at most `MAX_RENAMES`
    /// (roadmap D7): what `graph symbol --renamed` follows. A name defined
    /// again is dropped. Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub(crate) renames: BTreeMap<String, Vec<Rename>>,
    /// Trails of ended sessions per workspace, newest first, at most
    /// `MAX_TRAILS` (roadmap C9). Derived on replay; not part of the status
    /// reply.
    #[serde(skip)]
    pub(crate) trails: BTreeMap<String, Vec<Trail>>,
    /// The current generation's clock anchor, once recorded. Derived from
    /// the events on replay; not part of the status reply.
    #[serde(skip)]
    pub(crate) clock_anchor: Option<ClockAnchor>,
    /// Recent refusals placed in wall time, newest first, for the status
    /// line (roadmap H2). Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub(crate) refusals: Vec<RecentRefusal>,
    /// Refusals of the current run with their paths, newest first, at most
    /// `MAX_PATH_REFUSALS`, for a starting session's recent section
    /// (roadmap C3). Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub(crate) path_refusals: Vec<PathRefusal>,
    /// How many times each workspace ID has been moved or removed: its
    /// registration revision. With the root it names one registration, so
    /// what was derived outside the log during an earlier one (the source
    /// index, a worker's report) is started again or refused, even after a
    /// switch back to an earlier root. Derived on replay; not part of the
    /// status reply.
    #[serde(skip)]
    pub(crate) workspace_revisions: BTreeMap<String, u32>,
    /// Writes recorded per workspace since the event log was replayed: the
    /// write generation a check run takes before its tree and carries with
    /// its test results (`record_check.writes_before`). Derived on replay;
    /// in the status reply, one count per workspace, so that `check run`
    /// can read it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub writes: BTreeMap<String, u64>,
    /// Per workspace, then check ID: the per-test results of the records it
    /// holds, for comparing a later run with them (issue #69). Derived on
    /// replay; not part of the status reply.
    #[serde(skip)]
    pub(crate) kept_tests: BTreeMap<String, BTreeMap<String, KeptTests>>,
    /// Per session, the workspace's write count just before the session's
    /// first write since it registered. Derived on replay; not part of the
    /// status reply.
    #[serde(skip)]
    pub(crate) first_writes: BTreeMap<String, u64>,
    /// Per workspace, its sessions' runs of consecutive writes, oldest
    /// first, at most `MAX_WRITE_RUNS`. Derived on replay; not part of the
    /// status reply.
    #[serde(skip)]
    pub(crate) write_runs: BTreeMap<String, Vec<WriteRun>>,
    /// The wait for a current holder per workspace, for the workspaces
    /// whose conflict policy is `wait` (roadmap B28). Derived on replay;
    /// not part of the status reply.
    #[serde(skip)]
    pub(crate) conflict_waits: BTreeMap<String, u32>,
    /// Each session's write refusals not yet settled by an admitted write or
    /// a stop, oldest first, at most `MAX_UNSETTLED`, of the current
    /// generation only. Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub(crate) unsettled: BTreeMap<String, Vec<UnsettledRefusal>>,
}

/// What code outside the core may read of the derived fields, and the two
/// changes it makes to a state it holds.
impl State {
    /// Set the generation a reply was answered in, on a state a client put
    /// together from that reply's pages.
    pub fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    /// Each session's activity, to reshape for an older client.
    pub fn activities_mut(&mut self) -> impl Iterator<Item = &mut SessionActivity> {
        self.activity.values_mut()
    }

    pub fn renames(&self) -> &BTreeMap<String, Vec<Rename>> {
        &self.renames
    }

    pub fn trails(&self) -> &BTreeMap<String, Vec<Trail>> {
        &self.trails
    }

    pub fn clock_anchor(&self) -> Option<ClockAnchor> {
        self.clock_anchor
    }

    pub fn refusals(&self) -> &[RecentRefusal] {
        &self.refusals
    }

    pub fn path_refusals(&self) -> &[PathRefusal] {
        &self.path_refusals
    }

    pub fn workspace_revisions(&self) -> &BTreeMap<String, u32> {
        &self.workspace_revisions
    }

    pub fn kept_tests(&self) -> &BTreeMap<String, BTreeMap<String, KeptTests>> {
        &self.kept_tests
    }

    pub fn first_writes(&self) -> &BTreeMap<String, u64> {
        &self.first_writes
    }

    pub fn write_runs(&self) -> &BTreeMap<String, Vec<WriteRun>> {
        &self.write_runs
    }

    pub fn unsettled(&self) -> &BTreeMap<String, Vec<UnsettledRefusal>> {
        &self.unsettled
    }
}

/// A snapshot as stored: the state, and the derived fields the state leaves
/// out of every reply (`#[serde(skip)]`), which a start from the snapshot
/// must still have. A field skipped in the state and missing here makes the
/// snapshot check refuse the compaction.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRecord {
    state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clock_anchor: Option<ClockAnchor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refusals: Vec<RecentRefusal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    path_refusals: Vec<PathRefusal>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspace_revisions: BTreeMap<String, u32>,
    /// The test results kept per workspace and check, for classifying a
    /// later run's failures (ADR 0030).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    kept_tests: BTreeMap<String, BTreeMap<String, KeptTests>>,
    /// Each session's write count before its first write (ADR 0030).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    first_writes: BTreeMap<String, u64>,
    /// Each workspace's runs of consecutive writes (ADR 0030).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    write_runs: BTreeMap<String, Vec<WriteRun>>,
    /// Each workspace's wait for a current holder (ADR 0036).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    conflict_waits: BTreeMap<String, u32>,
    /// Each session's refusals not yet settled (ADR 0036).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    unsettled: BTreeMap<String, Vec<UnsettledRefusal>>,
    /// Each workspace's remembered renames (roadmap D7).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    renames: BTreeMap<String, Vec<Rename>>,
    /// Each workspace's trails of ended sessions (roadmap C9).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    trails: BTreeMap<String, Vec<Trail>>,
}

impl SnapshotRecord {
    pub fn of(state: &State) -> Self {
        Self {
            state: state.clone(),
            clock_anchor: state.clock_anchor,
            refusals: state.refusals.clone(),
            path_refusals: state.path_refusals.clone(),
            workspace_revisions: state.workspace_revisions.clone(),
            kept_tests: state.kept_tests.clone(),
            first_writes: state.first_writes.clone(),
            write_runs: state.write_runs.clone(),
            conflict_waits: state.conflict_waits.clone(),
            unsettled: state.unsettled.clone(),
            renames: state.renames.clone(),
            trails: state.trails.clone(),
        }
    }
    pub fn into_state(self) -> State {
        let mut state = self.state;
        state.clock_anchor = self.clock_anchor;
        state.refusals = self.refusals;
        state.path_refusals = self.path_refusals;
        state.workspace_revisions = self.workspace_revisions;
        state.kept_tests = self.kept_tests;
        state.first_writes = self.first_writes;
        state.write_runs = self.write_runs;
        state.conflict_waits = self.conflict_waits;
        state.unsettled = self.unsettled;
        state.renames = self.renames;
        state.trails = self.trails;
        state
    }
}
