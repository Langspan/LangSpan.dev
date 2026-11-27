//! The reply to a command: one `Outcome` per request. One flat enum,
//! because the wire depends on its serde shape; the banners group its
//! variants by domain, in their order.

use serde::{Deserialize, Serialize};

use crate::{
    Adoption, AutoCheck, BlindSpots, BranchObservation, Briefing, CallBreak, CheckRecord,
    CheckView, CheckoutChange, CheckoutRun, CompactionReport, Conflict, ContextUse, Dashboard,
    DiskObservation, FlakyTest, GraphCoverage, History, ImpactLevel, InflightNotice, Intent,
    InterfaceImpact, InterfaceNotice, Landing, Lease, MapArea, MapEdge, Message, MessageStatus,
    NameContext, NameRef, Note, Observation, OutlineEntry, PeerCallers, PolicyRow, PresenceSetting,
    Publication, QueuedLanding, Recall, Recap, RecordedEvent, RefusalCount, RemoteLink, Rename,
    Report, Reservation, ScanFailure, ScanSummary, SentPublication, Session, SessionLiveness,
    ShownMessage, StaleFile, State, StatusLine, StatusRecord, StatusSummary, SymbolPath, SymbolRef,
    Today, TrailView, Turn, Workspace, WorkspaceEnding, WorkspaceGit, WorkspaceKept,
    WorkspaceSummary, WriteCheck, WrittenLines, is_false, is_zero,
};

// One reply per request, built and sent once: its size does not matter, and
// boxing `Status`'s state would only add an allocation to every status read.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    // -----------------------------------------------------------------------
    // Workspaces and sessions
    // -----------------------------------------------------------------------
    Workspace {
        workspace: Workspace,
    },
    Workspaces {
        workspaces: Vec<WorkspaceSummary>,
        /// The cursor for the next page (`after`), when more remain.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<String>,
    },
    WorkspaceMoved {
        workspace: Workspace,
        from_root: String,
        kept: WorkspaceKept,
    },
    /// `applied` is false for a dry run: nothing ended.
    WorkspaceRemoval {
        workspace_id: String,
        applied: bool,
        ending: WorkspaceEnding,
    },
    Session {
        session: Session,
    },
    SessionLiveness {
        liveness: SessionLiveness,
    },
    /// The session's last recorded hook run, this one or one less than
    /// `SESSION_SEEN_INTERVAL_MS` earlier.
    SessionSeen {
        session_id: String,
        last_hook_ms: u64,
    },
    /// What the session's hooks have added to its context so far.
    ContextRecorded {
        session_id: String,
        context: ContextUse,
    },
    /// How many of the named items the session had been told and now has
    /// not; zero when none was, and nothing is recorded.
    ToldForgotten {
        session_id: String,
        forgotten: u32,
    },
    // -----------------------------------------------------------------------
    // Intents and admissions
    // -----------------------------------------------------------------------
    Intent {
        intent: Intent,
    },
    IntentEnded {
        session_id: String,
    },
    /// The number taken, and the other running sessions' claims of the
    /// same counter, newest first.
    Reserved {
        reservation: Reservation,
        held: Vec<Reservation>,
    },
    Admitted {
        lease: Lease,
    },
    Conflict {
        holders: Vec<Conflict>,
    },
    Completed {
        lease_id: String,
    },
    // -----------------------------------------------------------------------
    // Observed operations, files and branches
    // -----------------------------------------------------------------------
    Observed {
        observation: Observation,
    },
    DiskObserved {
        observation: DiskObservation,
    },
    BranchObserved {
        observation: BranchObservation,
    },
    // -----------------------------------------------------------------------
    // State, the event log and Git state
    // -----------------------------------------------------------------------
    /// `state` is the bounded view of what matters now; `summary` says what
    /// it counts instead of listing and what it left out to fit one frame.
    /// An answer from a build before the summary has none.
    Status {
        state: State,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<StatusSummary>,
    },
    /// One page of state records in a fixed order. `next` resumes after the
    /// last of them; it is absent on the last page.
    StatusRecords {
        records: Vec<StatusRecord>,
        next: Option<String>,
    },
    Events {
        events: Vec<RecordedEvent>,
    },
    Changes {
        events: Vec<RecordedEvent>,
        scan: Option<ScanSummary>,
    },
    WorkspaceGit {
        git: WorkspaceGit,
    },
    // -----------------------------------------------------------------------
    // Prompts, presence and a session's turns
    // -----------------------------------------------------------------------
    SessionPrompted {
        session_id: String,
        /// Leading prompt lines a live peer's latest prompt in the same
        /// repository shares with this one, by digest.
        #[serde(default, skip_serializing_if = "is_zero")]
        shared_lines: u32,
    },
    PromptTargets {
        session_id: String,
        paths: Vec<String>,
    },
    PresenceObserved {
        session_id: String,
        paths: u32,
    },
    Presence {
        session_id: String,
        setting: PresenceSetting,
    },
    SessionEnded {
        session_id: String,
    },
    /// `recorded` is false when this session had already adopted `from`:
    /// nothing new was recorded, and the adapter may finish crediting the
    /// lines. `root` is the workspace's, whose line journal holds them.
    /// `live` names `from`'s subagents that have not ended: their lines
    /// (and their own subagents') stay theirs. `earlier` names the sessions
    /// whose adoptions lead to `from`: an adopter that ended before its
    /// lines were credited left them there, and they are credited too.
    /// `kept` names subagents of `from` (or of an `earlier` session) that
    /// another session adopted: their lines (and their own subagents') are
    /// that session's to credit, so they stay where they are.
    Adopted {
        adoption: Adoption,
        root: String,
        recorded: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        live: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        earlier: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kept: Vec<String>,
    },
    SessionStopped {
        session_id: String,
        released: Vec<String>,
        /// The session wrote a file since its last batch or stop.
        #[serde(default, skip_serializing_if = "is_false")]
        wrote: bool,
    },
    ToolBatchObserved {
        session_id: String,
        calls: u32,
        /// The session wrote a file since its last batch or stop.
        #[serde(default, skip_serializing_if = "is_false")]
        wrote: bool,
    },
    // -----------------------------------------------------------------------
    // Automatic checks
    // -----------------------------------------------------------------------
    AutoCheck {
        check: AutoCheck,
    },
    AutoCheckDisabled {
        workspace_id: String,
    },
    // -----------------------------------------------------------------------
    // Briefings, refused edits and source versions
    // -----------------------------------------------------------------------
    Briefing {
        briefing: Briefing,
    },
    /// Nothing was admitted: the session's view of these files is out of date.
    StaleBase {
        stale: Vec<StaleFile>,
    },
    /// Nothing was admitted: the edit would change lines other live
    /// sessions wrote recently, and this session has not read the file.
    UnreadLines {
        lines: Vec<WrittenLines>,
    },
    SourceObserved {
        session_id: String,
        files: u32,
    },
    // -----------------------------------------------------------------------
    // Remote publication and retention
    // -----------------------------------------------------------------------
    RemoteLink {
        link: RemoteLink,
    },
    RemoteDisabled {
        workspace_id: String,
    },
    RemoteOutbox {
        publication: Publication,
        signature: String,
        /// False when the content matches the last recorded publication.
        changed: bool,
    },
    /// Newest first. `more` is true when older records were left out to
    /// keep the answer within one frame.
    RemoteSent {
        sent: Vec<SentPublication>,
        more: bool,
        /// Wall-clock Unix milliseconds from which the list is complete:
        /// publications recorded before it may have been removed by
        /// retention. Absent when nothing has ever been removed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_ms: Option<u64>,
    },
    /// What a compaction removed, or with `applied` false would remove.
    Compaction {
        report: CompactionReport,
    },
    /// `accepted` is false for a publication that is not newer than the one
    /// already held: a replay, or one that arrived out of order.
    RemoteObserved {
        participant: String,
        accepted: bool,
    },
    // -----------------------------------------------------------------------
    // Checks and test results
    // -----------------------------------------------------------------------
    CheckRecorded {
        check: CheckRecord,
    },
    Checks {
        checks: Vec<CheckView>,
    },
    /// Known-flaky tests, newest flip first; `more` when some were left out.
    FlakyTests {
        workspace_id: String,
        tests: Vec<FlakyTest>,
        #[serde(default, skip_serializing_if = "is_false")]
        more: bool,
    },
    /// `forgotten` is false when nothing was learned about the test.
    FlakyTestForgotten {
        workspace_id: String,
        test: String,
        forgotten: bool,
    },
    /// `ready` only when a passing check of that name exists for exactly
    /// the tree asked about. `reason` says what is missing otherwise.
    Readiness {
        ready: bool,
        reason: String,
        check: Option<CheckRecord>,
    },
    // -----------------------------------------------------------------------
    // Landings and landing turns
    // -----------------------------------------------------------------------
    Landed {
        landing: Landing,
    },
    /// The caller holds the turn.
    Turn {
        turn: Turn,
    },
    /// Another landing holds the turn; nothing was taken. `held_ms` is
    /// how long it has held it.
    TurnBusy {
        holder: Turn,
        held_ms: u64,
    },
    /// The caller is waiting for the turn, at `position` (1 is next).
    /// `waiting` is the whole line, first first.
    TurnWaiting {
        position: u32,
        holder: Option<Turn>,
        held_ms: Option<u64>,
        waiting: Vec<QueuedLanding>,
    },
    /// The caller neither has the turn nor is waiting for it: the queue is
    /// full, or the caller did not ask to wait and others are in line.
    TurnUnavailable {
        holder: Option<Turn>,
        held_ms: Option<u64>,
        waiting: Vec<QueuedLanding>,
        queue_full: bool,
    },
    /// Who holds a target's push turn and who waits for it, first first.
    TurnQueue {
        holder: Option<Turn>,
        held_ms: Option<u64>,
        waiting: Vec<QueuedLanding>,
    },
    /// `left` is false when the landing was not waiting.
    TurnQueueLeft {
        ticket: String,
        left: bool,
    },
    /// `released` is false when the turn was not held by `turn_id`: it had
    /// already ended.
    TurnReleased {
        turn_id: String,
        released: bool,
    },
    // -----------------------------------------------------------------------
    // Interface changes and notes
    // -----------------------------------------------------------------------
    InterfaceObserved {
        notice: InterfaceNotice,
    },
    /// How many files of another worktree were added to the notice's
    /// `elsewhere`; zero when every one was already listed or it is full.
    InterfaceDependentsObserved {
        notice_id: String,
        added: u32,
    },
    Remembered {
        note: Note,
    },
    /// How many anchored notes an index pass moved to a new state.
    NoteAnchorsObserved {
        changed: u32,
    },
    Forgotten {
        note_id: String,
        /// False when no such note existed.
        removed: bool,
    },
    // -----------------------------------------------------------------------
    // Messages
    // -----------------------------------------------------------------------
    MessageSent {
        message: Message,
    },
    /// `acknowledged` is false when the message was not owed to the
    /// session: already acknowledged, dropped, or never addressed to it.
    MessageAcknowledged {
        message_id: String,
        acknowledged: bool,
    },
    MessageShown {
        message: ShownMessage,
    },
    MessageStatus {
        status: MessageStatus,
    },
    /// Sessions that were still owed it and now are not, and sessions that
    /// had received it and will be told to disregard it.
    MessageRetracted {
        message_id: String,
        withdrawn_unread: Vec<String>,
        told_to_disregard: Vec<String>,
    },
    // -----------------------------------------------------------------------
    // Scans, hosts, checkouts and uncoordinated changes
    // -----------------------------------------------------------------------
    ScanObserved {
        workspace_id: String,
        failed: bool,
    },
    /// The hosts held as not installed after a host check.
    HostsObserved {
        not_installed: Vec<String>,
    },
    CheckoutObserved {
        change: CheckoutChange,
    },
    CheckoutRunObserved {
        run: CheckoutRun,
    },
    /// The Codex sessions named and the files kept; both empty when every
    /// change was explained or nothing new was recorded.
    UncoordinatedObserved {
        sessions: Vec<String>,
        paths: Vec<String>,
    },
    // -----------------------------------------------------------------------
    // Recaps, trails and reports
    // -----------------------------------------------------------------------
    Recap {
        recap: Recap,
    },
    Trails {
        workspace_id: String,
        trails: Vec<TrailView>,
        /// Trails left out so that the answer fits one frame.
        #[serde(default, skip_serializing_if = "is_zero")]
        omitted: u32,
    },
    Recall {
        recall: Recall,
    },
    History {
        history: History,
    },
    Today {
        today: Today,
    },
    Report {
        report: Report,
    },
    Count {
        count: RefusalCount,
    },
    StatusLine {
        line: StatusLine,
    },
    // -----------------------------------------------------------------------
    // Refusals, the conflict policy and waits
    // -----------------------------------------------------------------------
    RefusalRecorded {
        session_id: String,
    },
    Policy {
        workspace_id: String,
        rows: Vec<PolicyRow>,
    },
    Holders {
        holders: Vec<Conflict>,
    },
    WaitRecorded {
        session_id: String,
    },
    // -----------------------------------------------------------------------
    // The dashboard and the daemon's version
    // -----------------------------------------------------------------------
    Dashboard {
        dashboard: Dashboard,
    },
    /// The answer to `Command::Version`.
    Version {
        version: String,
        pid: u32,
    },
    // -----------------------------------------------------------------------
    // The source graph and write checks
    // -----------------------------------------------------------------------
    /// Every list is a name match bounded by the request's limit. `coverage`
    /// says how much of the workspace was indexed when it was answered.
    Graph {
        symbols: Vec<SymbolRef>,
        references: Vec<NameRef>,
        /// `name` on a detailed symbol lookup: callers are found by name,
        /// not resolved. Absent otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution: Option<String>,
        coverage: GraphCoverage,
        truncated: bool,
        /// `graph impact` with a depth above 1: each level's files, from
        /// the same index snapshot as `coverage`. Absent otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        levels: Option<Vec<ImpactLevel>>,
        /// `graph symbol --renamed` of a name with a recorded rename: the
        /// renames followed, in order; `symbols` are the last new name's.
        /// Absent otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        renamed: Option<Vec<Rename>>,
    },
    /// Only names with at least one definition, in the order asked.
    GraphContext {
        matches: Vec<NameContext>,
        coverage: GraphCoverage,
    },
    /// What other worktrees have in flight that the workspace mentions, in
    /// worktree, path and name order. `coverage` is the asking workspace's;
    /// a worktree without a complete scan contributes nothing.
    GraphInflight {
        inflight: Vec<InflightNotice>,
        coverage: GraphCoverage,
        truncated: bool,
    },
    /// The shortest chains of calls between two names (roadmap D3), at
    /// most `MAX_PATHS`, in path and line order of their steps. Empty when
    /// none is within the hops asked for. `edges_visited` counts what the
    /// search read; `truncated` says a bound, the deadline or the chain
    /// limit cut it, so a missing or longer chain may still exist.
    GraphPath {
        /// The query, as asked.
        from: String,
        to: String,
        max_hops: u32,
        paths: Vec<SymbolPath>,
        edges_visited: u32,
        coverage: GraphCoverage,
        truncated: bool,
    },
    /// The workspace's areas and the edges between them (roadmap D3), in
    /// area order and then by count. `truncated`: some areas or edges are
    /// not listed.
    GraphMap {
        depth: u32,
        areas: Vec<MapArea>,
        edges: Vec<MapEdge>,
        coverage: GraphCoverage,
        truncated: bool,
    },
    /// The source index was written to a file (roadmap D5).
    GraphExported {
        workspace_id: String,
        files: u32,
        partial_files: u32,
        bytes: u64,
    },
    /// What an index import found, by file line (roadmap D5). Only
    /// `loaded` files were written, and only when `applied`.
    GraphImported {
        workspace_id: String,
        applied: bool,
        /// Same content here, and not yet indexed here: loaded, or would be.
        loaded: u32,
        /// Content here differs, or the file is missing or not in the last
        /// scan: left to the indexer.
        different: u32,
        /// Already indexed here: the local rows are kept.
        kept: u32,
        /// Over a parse bound (symbols, references, imports, calls, line size).
        over_limits: u32,
        /// Not a valid file record, or a path seen earlier in the file.
        malformed: u32,
    },
    /// `ready` is false when the index has no complete scan of the
    /// workspace: nothing was checked.
    CallBreaks {
        breaks: Vec<CallBreak>,
        coverage: GraphCoverage,
        ready: bool,
        /// The request's `renamed` names that other worktrees' uncommitted
        /// files mention (capability `rename_holds`, issue #213). Asked
        /// about renamed names alone, `ready` says only that the index
        /// was read, and `coverage` is empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        callers: Vec<PeerCallers>,
    },
    /// The answer to `CheckWrite` (roadmap B26). Nothing was recorded.
    WriteCheck {
        check: WriteCheck,
    },
    /// Where the index cannot answer for the workspace's source (roadmap
    /// D8). Each list is in path order and holds at most the request's
    /// limit; `totals` has the full counts. Paths only, never content.
    GraphBlindSpots {
        blind_spots: BlindSpots,
        coverage: GraphCoverage,
        /// The last scan attempt failed: the lists are as of the last
        /// complete one, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scan_failure: Option<ScanFailure>,
        truncated: bool,
    },
    // -----------------------------------------------------------------------
    // Interface notices, outlines and what is told between steps
    // -----------------------------------------------------------------------
    InterfaceAcknowledged {
        session_id: String,
        notice_ids: Vec<String>,
    },
    /// The answer to `OutlineRead` (roadmap A11). `outline` is present only
    /// when the read is to be refused once with it; `skipped` says why it
    /// is not. `definitions` is how many the index holds for the file: more
    /// than `outline` lists when the daemon kept only the largest.
    ReadOutline {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outline: Option<Vec<OutlineEntry>>,
        #[serde(default, skip_serializing_if = "is_zero")]
        definitions: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skipped: Option<String>,
    },
    /// What a session is told between its steps (issue #214). `open` is
    /// false once the turn the asking watcher started in is over, or the
    /// session ended; `stops` is the reference for its next ask.
    BetweenSteps {
        session_id: String,
        open: bool,
        stops: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        interface_changes: Vec<InterfaceImpact>,
    },
    // -----------------------------------------------------------------------
    // Errors
    // -----------------------------------------------------------------------
    Error {
        code: String,
        message: String,
    },
}
