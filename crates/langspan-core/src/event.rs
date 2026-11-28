//! The event log's record: every state change is one `Event`, and the state
//! is what `apply` derives from them. One flat enum, because the store and
//! the wire depend on its serde shape; the banners group its variants by
//! domain, in their order.

use serde::{Deserialize, Serialize};

use crate::{
    Adoption, AnchorState, AutoCheck, BranchObservation, CheckNotice, CheckRecord, CheckoutChange,
    CheckoutRun, ClockAnchor, Delivery, DiskObservation, Intent, InterfaceNotice, KeptTests,
    Landing, LandingAttempt, Lease, LeftQueue, Message, Note, NoteAnchor, NoticeVia, Observation,
    ObservedFile, Policy, PolicyCase, PresenceKind, PresenceSetting, RefusalKind, RemoteLink,
    RemotePeer, ScanCoverage, ScanFailure, Session, SessionLiveness, SettledOutcome, SourceOrigin,
    Stamp, TestResults, Trail, Turn, TurnWaiter, UncoordinatedChange, UninstalledHost, WaitResult,
    Workspace, WorkspaceGit, WorktreeFile, is_false,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    // -----------------------------------------------------------------------
    // The daemon, write refusals and the conflict policy
    // -----------------------------------------------------------------------
    Booted {
        generation: u64,
    },
    /// The daemon's clock read with the wall clock as a generation started,
    /// so its times can be placed on a day (the status line, `langspan today`).
    ClockAnchored {
        anchor: ClockAnchor,
    },
    /// A write or commit was refused. Recorded with the refusal's receipt,
    /// so a retry of the same request is not counted again.
    WriteRefused {
        session_id: String,
        workspace_id: String,
        kind: RefusalKind,
        /// At most `REFUSAL_PATHS`, sorted.
        paths: Vec<String>,
        /// The other session the refusal names, when there is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_session_id: Option<String>,
        /// The refused tool call's input digest (`ToolIdentity`), so the
        /// coordination report can tell an identical retry from a changed
        /// call. Absent for a held commit and from records written before it
        /// was kept.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_digest: Option<String>,
        at_ms: u64,
        generation: u64,
    },
    /// A hook waited for a current holder instead of refusing at once
    /// (roadmap B28), and how the wait ended.
    WriteWaited {
        session_id: String,
        workspace_id: String,
        /// At most `REFUSAL_PATHS`, sorted.
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holder_session_id: Option<String>,
        waited_ms: u64,
        result: WaitResult,
        at_ms: u64,
        generation: u64,
    },
    /// What the session did after a write refusal: its next admitted tool
    /// write, or its stop (roadmap B28). `ms_to_next_admitted` is the time
    /// from the refusal to an admitted write of one of its paths.
    RefusalSettled {
        session_id: String,
        workspace_id: String,
        kind: RefusalKind,
        paths: Vec<String>,
        refused_at_ms: u64,
        outcome: SettledOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms_to_next_admitted: Option<u64>,
        at_ms: u64,
        generation: u64,
    },
    /// The owner set a workspace's policy for a current holder (roadmap
    /// B28). `wait_ms` is zero for `refuse`.
    PolicySet {
        workspace_id: String,
        case: PolicyCase,
        policy: Policy,
        wait_ms: u32,
    },
    // -----------------------------------------------------------------------
    // Workspaces and sessions
    // -----------------------------------------------------------------------
    WorkspaceRegistered {
        workspace: Workspace,
    },
    /// The owner rebound the workspace to another checkout root (roadmap
    /// G6). Everything kept under its ID stays; file versions recorded
    /// under `from_root` are dropped. Local only.
    WorkspaceMoved {
        workspace: Workspace,
        from_root: String,
    },
    /// The owner took the workspace out of coordination (roadmap G6): it,
    /// its sessions and everything derived for them leave the state. The
    /// events before this one stay in the log. Local only.
    WorkspaceRemoved {
        workspace_id: String,
    },
    SessionRegistered {
        session: Session,
    },
    SessionLivenessObserved {
        liveness: SessionLiveness,
    },
    /// A hook run of the session reached the daemon (roadmap J12). Evidence
    /// that it is working; it grants, renews and releases nothing.
    SessionSeen {
        session_id: String,
        at_ms: u64,
        generation: u64,
    },
    // -----------------------------------------------------------------------
    // Intents and admissions
    // -----------------------------------------------------------------------
    IntentSet {
        intent: Intent,
    },
    IntentEnded {
        session_id: String,
    },
    LeaseGranted {
        lease: Lease,
    },
    LeaseCompleted {
        lease_id: String,
    },
    LeaseExpired {
        lease_id: String,
    },
    // -----------------------------------------------------------------------
    // Observed operations, files, branches and Git state
    // -----------------------------------------------------------------------
    OperationObserved {
        observation: Observation,
    },
    DiskObserved {
        observation: DiskObservation,
    },
    BranchObserved {
        observation: BranchObservation,
    },
    WorkspaceGitObserved {
        git: WorkspaceGit,
    },
    // -----------------------------------------------------------------------
    // Prompts, presence and a session's turns
    // -----------------------------------------------------------------------
    SessionPrompted {
        session_id: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        line_digests: Vec<String>,
        at_ms: u64,
        generation: u64,
    },
    /// The definition paths the session's prompt named, replacing earlier
    /// ones. Local only: never published.
    PromptTargetsSet {
        session_id: String,
        paths: Vec<String>,
        at_ms: u64,
        generation: u64,
    },
    /// Paths a session's search or shell command looked at (roadmap A13).
    /// Paths only, never a pattern or command text. Local only: never
    /// published. Reads are presence through `SourceObserved`.
    PresenceObserved {
        session_id: String,
        kind: PresenceKind,
        paths: Vec<String>,
        at_ms: u64,
        generation: u64,
    },
    /// The session's presence setting; the default removes its record.
    PresenceSet {
        session_id: String,
        setting: PresenceSetting,
    },
    SessionEnded {
        session_id: String,
    },
    /// A session took over an ended session's uncommitted work (issue
    /// #161).
    SessionAdopted {
        adoption: Adoption,
    },
    /// What a session left as it ended (roadmap C9). Replaces an earlier
    /// trail of the same session.
    TrailRecorded {
        trail: Trail,
    },
    /// `session_id` was told the trail `from` left.
    TrailTaken {
        workspace_id: String,
        from: String,
        session_id: String,
    },
    /// The host said the session finished its reply (Stop, StopFailure or
    /// SubagentStop). `released` are the tool admissions still open then,
    /// completed by this event's preceding `LeaseCompleted` events.
    /// `failure` is the host's error type when the reply ended on an error.
    SessionStopped {
        session_id: String,
        released: Vec<String>,
        failure: Option<String>,
        at_ms: u64,
        generation: u64,
        /// The host's ID of the prompt whose reply finished, when it gave
        /// one (issue #214). Absent from older records.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_id: Option<String>,
    },
    /// The host finished a batch of tool calls and is about to ask the model
    /// again (Claude Code's PostToolBatch). `operation_ids` name at most
    /// `MAX_BATCH_OPERATIONS` of the `calls`.
    ToolBatchObserved {
        session_id: String,
        operation_ids: Vec<String>,
        calls: u32,
        at_ms: u64,
        generation: u64,
    },
    // -----------------------------------------------------------------------
    // Briefings and what a session was told
    // -----------------------------------------------------------------------
    BriefingDelivered {
        session_id: String,
        signature: String,
        /// The keys of the items the session has now been told, at most
        /// `MAX_TOLD` (issue #222). Absent from a delivery recorded before
        /// them: the next briefing is then the full view.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        told: Vec<u64>,
        /// When this delivery placed items in the session's context; absent
        /// when it only forgot items that no longer apply.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        told_at_ms: Option<u64>,
    },
    /// A hook placed `bytes` of text in the session's context; `turn` when
    /// it did so at the start of a turn (a session start or a prompt), with
    /// zero bytes when it had nothing to add (issue #222).
    ContextAdded {
        session_id: String,
        bytes: u64,
        turn: bool,
        generation: u64,
    },
    /// These told items were not placed in the session's context: they are
    /// told again (review comment 4711375989). At most `MAX_TOLD` keys.
    ToldForgotten {
        session_id: String,
        keys: Vec<u64>,
    },
    /// These in-flight signatures were placed in the session's context with
    /// a briefing (roadmap C7). Each is told once.
    InflightDelivered {
        session_id: String,
        keys: Vec<String>,
    },
    /// A write was told about these interface notices (ADR 0029).
    InterfaceAlerted {
        session_id: String,
        notice_ids: Vec<String>,
    },
    /// These interface notices were placed in the session's context, by a
    /// briefing or between its steps (issue #214). Each is told once
    /// between steps, and one told there is left out of later briefings.
    InterfaceNoticesTold {
        session_id: String,
        notice_ids: Vec<String>,
        via: NoticeVia,
    },
    /// The session's whole read of this file version was refused once with
    /// an outline (roadmap A11). No read version is recorded: stale checks
    /// are unaffected.
    ReadOutlined {
        session_id: String,
        path: String,
        digest: String,
    },
    // -----------------------------------------------------------------------
    // Source versions a session read or wrote
    // -----------------------------------------------------------------------
    SourceObserved {
        session_id: String,
        files: Vec<ObservedFile>,
        origin: SourceOrigin,
        at_ms: u64,
        generation: u64,
    },
    // -----------------------------------------------------------------------
    // Remote publication
    // -----------------------------------------------------------------------
    RemoteEnabled {
        link: RemoteLink,
    },
    RemoteDisabled {
        workspace_id: String,
    },
    /// The outbound record: what left this machine for a workspace. The
    /// payload and time are absent from records written before they were
    /// kept.
    RemotePublished {
        workspace_id: String,
        sequence: u64,
        signature: String,
        payload: Option<String>,
        published_at_ms: Option<u64>,
    },
    RemoteObserved {
        peer: RemotePeer,
    },
    RemoteForgotten {
        workspace_id: String,
        participant: String,
    },
    RemoteWithdrawn {
        workspace_id: String,
    },
    /// A teammate's message from another machine was placed in a session's
    /// context (ADR 0041). `text` is what was delivered, at most
    /// `MESSAGE_BYTES`, so the owner can read what entered the context.
    RemoteMessageDelivered {
        workspace_id: String,
        participant: String,
        message_id: String,
        session_id: String,
        text: String,
    },
    // -----------------------------------------------------------------------
    // Checks and test results
    // -----------------------------------------------------------------------
    CheckRecorded {
        check: CheckRecord,
        /// The per-test results of a working-tree record, kept to compare a
        /// later run with (issue #69). Absent from older logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tests: Option<KeptTests>,
    },
    AutoCheckEnabled {
        check: AutoCheck,
    },
    AutoCheckDisabled {
        workspace_id: String,
    },
    /// An automatic check's result, to be delivered once to each of
    /// `session_ids`. Follows the `CheckRecorded` of the same record.
    CheckReported {
        session_ids: Vec<String>,
        notice: CheckNotice,
    },
    /// The result was placed in the session's context.
    CheckDelivered {
        session_id: String,
        check_id: String,
    },
    /// Which tests one run of check `name` failed and passed on `tree_oid`:
    /// names only, no output. Follows the `CheckRecorded` of record
    /// `check_id`; `rerun` for the run made again on the same tree, which is
    /// no record of its own. Compared with the check's last such result of
    /// the same `command_id`, unless a write was recorded in the workspace
    /// since.
    TestsRecorded {
        workspace_id: String,
        check_id: String,
        name: String,
        tree_oid: String,
        command: String,
        command_id: String,
        results: TestResults,
        #[serde(default, skip_serializing_if = "is_false")]
        rerun: bool,
        at_ms: u64,
    },
    /// The owner cleared what was learned about `test`, in check `name` or
    /// in every check.
    FlakyTestForgotten {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        test: String,
    },
    // -----------------------------------------------------------------------
    // Landings and landing turns
    // -----------------------------------------------------------------------
    Landed {
        landing: Landing,
    },
    TurnTaken {
        turn: Turn,
    },
    /// The landing ended; `attempts` is how each try went.
    TurnReleased {
        workspace_id: String,
        onto: String,
        turn_id: String,
        attempts: Vec<LandingAttempt>,
    },
    TurnRenewed {
        workspace_id: String,
        onto: String,
        turn_id: String,
        expires_at_ms: u64,
    },
    TurnExpired {
        workspace_id: String,
        onto: String,
        turn_id: String,
    },
    TurnQueueJoined {
        waiter: TurnWaiter,
    },
    /// A waiting landing asked again: it stays in line until `seen_until_ms`.
    TurnQueueSeen {
        workspace_id: String,
        onto: String,
        ticket: String,
        seen_until_ms: u64,
    },
    TurnQueueLeft {
        workspace_id: String,
        onto: String,
        ticket: String,
        reason: LeftQueue,
    },
    // -----------------------------------------------------------------------
    // Interface changes and notes
    // -----------------------------------------------------------------------
    InterfaceChanged {
        notice: InterfaceNotice,
    },
    /// Files of another worktree found to mention a held notice's changed
    /// names (issue #194).
    InterfaceDependentsFound {
        notice_workspace_id: String,
        notice_id: String,
        files: Vec<WorktreeFile>,
    },
    Remembered {
        note: Note,
        /// When it was remembered. Absent from notes recorded before it was
        /// kept; a definition's history (roadmap D4) places notes near
        /// interface changes by it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<Stamp>,
    },
    Forgotten {
        workspace_id: String,
        note_id: String,
    },
    /// An anchored note's definition, compared with a complete scan, is in
    /// a new state (roadmap E1). `anchor` is filled in when the note held
    /// only a name.
    NoteAnchorChanged {
        workspace_id: String,
        note_id: String,
        anchor: NoteAnchor,
        state: AnchorState,
    },
    // -----------------------------------------------------------------------
    // Messages
    // -----------------------------------------------------------------------
    /// `at_ms` and `generation` say when it was sent, on the daemon's
    /// clock; absent in records written before roadmap E12.
    MessageSent {
        message: Message,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        generation: Option<u64>,
    },
    /// The session was given the message. `via` is the path that placed it
    /// in the session's context, and `at_ms` and `generation` when; all
    /// three are absent in records written before roadmap E12.
    MessageAcknowledged {
        workspace_id: String,
        message_id: String,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<Delivery>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        generation: Option<u64>,
    },
    /// The message was withdrawn by `by` (its sender; `None` for the
    /// owner). The `MessageSent` event stays in the log.
    MessageRetracted {
        workspace_id: String,
        message_id: String,
        by: Option<String>,
    },
    /// The session was told to disregard a withdrawn message.
    RetractionDelivered {
        session_id: String,
        message_id: String,
    },
    // -----------------------------------------------------------------------
    // Scans, checkouts and uncoordinated changes
    // -----------------------------------------------------------------------
    ScanFailed {
        failure: ScanFailure,
    },
    ScanRecovered {
        workspace_id: String,
    },
    /// `None` when the last completed scan versioned every file.
    ScanCoverageObserved {
        workspace_id: String,
        coverage: Option<ScanCoverage>,
    },
    CheckoutChanged {
        change: CheckoutChange,
    },
    CheckoutRunObserved {
        run: CheckoutRun,
    },
    /// A scan saw files change that no session reported, with Codex
    /// sessions registered in the checkout (issue #129).
    UncoordinatedChanged {
        change: UncoordinatedChange,
    },
    /// A briefing carried this uncoordinated change to this session.
    UncoordinatedDelivered {
        workspace_id: String,
        change_id: String,
        session_id: String,
    },
    // -----------------------------------------------------------------------
    // Hosts without an install
    // -----------------------------------------------------------------------
    /// A host check found this host present and not covered by any
    /// LangSpan install. Recorded once; repeated only after a
    /// `HostCleared`.
    HostNotInstalled {
        host: UninstalledHost,
    },
    /// A host recorded as not installed is covered now, or its
    /// configuration directory is gone.
    HostCleared {
        host: String,
    },
}
