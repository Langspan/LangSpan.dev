//! A request to the core: one `Command` per request, and `CommandWire`, its
//! strict decoding. Each is one flat enum, because the wire and the stored
//! receipts depend on their serde shape; the banners group their variants
//! by domain, in their order.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    AnchorIndex, CallSite, CheckReport, CheckResult, FileState, InflightSymbol, InterfaceChange,
    LandingAttempt, LeftQueue, LivenessStatus, ObservedFile, PathRegions, Policy, PolicyCase,
    PresenceKind, ProcessIdentity, Publication, RefusalKind, Rewritten, ScanCoverage, SinceFrom,
    SourceOrigin, SymbolFindings, TestResults, ToolIdentity, ToolResult, TrailFacts, TurnQueueing,
    WaitResult, WiderCheck, WrittenLines, default_path_hops, is_false, is_one, is_zero_u64, one,
};
// For the link in `RecordCheck`'s documentation only.
#[cfg(doc)]
use crate::State;

impl Command {
    /// Whether `other`, sent under this command's request ID, is the same
    /// request: an identical retry, answered with the recorded outcome.
    /// What the sender measures anew on every send is not compared: the age
    /// of each written-lines entry of an admission (`WrittenLines::age_ms`)
    /// and a checkout run's `finished_ago_ms`, which change between two
    /// deliveries of one tool call.
    pub fn same_request(&self, other: &Command) -> bool {
        fn ageless(command: &Command) -> Option<Command> {
            let mut command = command.clone();
            match &mut command {
                Command::AdmitTool { overwrites, .. } => {
                    for entry in overwrites.iter_mut() {
                        entry.age_ms = 0;
                    }
                }
                Command::ObserveCheckoutRun {
                    finished_ago_ms, ..
                } => *finished_ago_ms = 0,
                _ => return None,
            }
            Some(command)
        }
        match (ageless(self), ageless(other)) {
            (Some(ours), Some(theirs)) => ours == theirs,
            _ => self == other,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    // -----------------------------------------------------------------------
    // Workspaces and sessions
    // -----------------------------------------------------------------------
    WorkspaceRegister {
        workspace_id: String,
        root: String,
    },
    /// Prepared adapter input only. It is absent from CommandWire and cannot be
    /// serialized as a receipt: only the original WorkspaceRegister is stored.
    #[serde(skip_serializing)]
    WorkspaceRegisterVerified {
        workspace_id: String,
        root: String,
        verified_overlaps: Vec<String>,
    },
    /// Read-only: one page of the registered workspaces with what each
    /// holds, in ID order, after the workspace ID `after` (from the first
    /// when absent). The daemon's store also bounds each page's bytes.
    WorkspaceList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    /// Rebind the workspace to `root`: a checkout the owner moved (the old
    /// root is gone), or with `switch` another checkout of the same
    /// repository. The daemon checks the filesystem and Git before the core
    /// decides; an owner command only, never an MCP tool.
    WorkspaceMove {
        workspace_id: String,
        root: String,
        #[serde(default, skip_serializing_if = "is_false")]
        switch: bool,
    },
    /// Prepared adapter input only, as `WorkspaceRegisterVerified`: the
    /// receipt stores the original `WorkspaceMove`.
    #[serde(skip_serializing)]
    WorkspaceMoveVerified {
        workspace_id: String,
        root: String,
        verified_overlaps: Vec<String>,
    },
    /// Take the workspace out of coordination. Without `apply`, a read that
    /// says what would end and changes nothing.
    WorkspaceRemove {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "is_false")]
        apply: bool,
    },
    SessionRegister {
        session_id: String,
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        headless: bool,
    },
    ObserveSessionLiveness {
        session_id: String,
        process: ProcessIdentity,
        status: LivenessStatus,
    },
    /// A hook run of the session reached the daemon. Recorded only when the
    /// last record is `SESSION_SEEN_INTERVAL_MS` old (roadmap J12).
    SessionSeen {
        session_id: String,
    },
    /// The hook placed `bytes` of text in the session's context, at a
    /// turn's start (`turn`, zero bytes allowed) or after a tool call
    /// (issue #222). A measurement only.
    RecordContext {
        session_id: String,
        bytes: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        turn: bool,
    },
    /// The hook could not place these briefing items in the session's
    /// context (its text budget was full): they are no longer told, so the
    /// next briefing of changes only tells them (review comment 4711375989).
    /// At most `MAX_TOLD` keys, from `briefing_keys`.
    ForgetTold {
        session_id: String,
        keys: Vec<u64>,
    },
    // -----------------------------------------------------------------------
    // Intents, admissions and source versions
    // -----------------------------------------------------------------------
    IntentSet {
        session_id: String,
        paths: Vec<String>,
        summary: String,
    },
    IntentEnd {
        session_id: String,
    },
    Admit {
        session_id: String,
        paths: Vec<String>,
        ttl_ms: u64,
    },
    AdmitTool {
        session_id: String,
        paths: Vec<String>,
        ttl_ms: u64,
        operation: ToolIdentity,
        /// The admitted files as the adapter saw them on disk just before
        /// asking. A path left out is admitted with an unknown base.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        base: Vec<ObservedFile>,
        /// Where in each file the edit was located. A path left out, or
        /// whose base is not the version its regions name, is held whole.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regions: Vec<PathRegions>,
        /// Lines other sessions wrote recently that this edit would change,
        /// in the base version, from the hook's line journal.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overwrites: Vec<WrittenLines>,
    },
    ObserveSource {
        session_id: String,
        files: Vec<ObservedFile>,
        origin: SourceOrigin,
    },
    // -----------------------------------------------------------------------
    // Remote publication
    // -----------------------------------------------------------------------
    RemoteEnable {
        workspace_id: String,
        remote: String,
        participant: String,
        name: String,
        share_task: bool,
        /// Publish under `refs/heads/langspan/peers/` instead of
        /// `refs/langspan/peers/`. ADR 0006.
        #[serde(default, skip_serializing_if = "is_false")]
        publish_as_branch: bool,
    },
    RemoteDisable {
        workspace_id: String,
    },
    /// Read-only: what this workspace would publish next.
    RemoteOutbox {
        workspace_id: String,
    },
    /// The publication was pushed. `payload` is the exact `state.json` the
    /// remote now holds (at most `PUBLICATION_BYTES`) and `published_at_ms`
    /// the wall-clock time of the push in Unix milliseconds, for people.
    RemotePublished {
        workspace_id: String,
        sequence: u64,
        signature: String,
        /// Absent from records written before payloads were kept.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        payload: String,
        #[serde(default, skip_serializing_if = "is_zero_u64")]
        published_at_ms: u64,
        /// The namespace the push went to (`true`: the branch namespace).
        /// Absent from records of earlier builds, which are taken as current.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        as_branch: Option<bool>,
    },
    /// Read-only: the latest publications that left for a workspace, newest
    /// first, with their exact payloads.
    RemoteSent {
        workspace_id: String,
        limit: u32,
    },
    ObserveRemote {
        workspace_id: String,
        publication: Publication,
    },
    /// The teammate's ref is gone from the remote: they withdrew.
    ForgetRemote {
        workspace_id: String,
        participant: String,
    },
    /// The daemon removed a disabled workspace's ref from the remote.
    RemoteWithdrawn {
        workspace_id: String,
    },
    // -----------------------------------------------------------------------
    // Checks and test results
    // -----------------------------------------------------------------------
    RecordCheck {
        workspace_id: String,
        name: String,
        command: String,
        tree_oid: String,
        head_oid: Option<String>,
        inputs: Vec<String>,
        result: CheckResult,
        duration_ms: u64,
        /// For an automatic check: the sessions to tell, and its output's
        /// end.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<Box<CheckReport>>,
        /// The tests its output named, for a working-tree check that passed
        /// or failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tests: Option<TestResults>,
        /// The tests of a second run on the same tree, made to learn which
        /// tests flip; its result is not recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rerun: Option<TestResults>,
        /// With `rerun`: the workspace's [`State::writes`] when the first
        /// run's tree was taken. The rerun is compared only if no write was
        /// recorded since, even one the tree no longer shows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        writes_before: Option<u64>,
        /// With `tests`: the command's identity, 64 lowercase hex digits
        /// that differ for any two argument vectors. `command` is a cut and
        /// redacted summary, which two commands can share; results are
        /// compared only when this is equal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        /// For a combined check asked for them: what comparing the
        /// branches' definitions found, kept with the record.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        symbol_findings: Option<Box<SymbolFindings>>,
    },
    /// Read-only: the tests known to flip on unchanged code, of check `name`
    /// or of every check.
    FlakyTests {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Clear what was learned about `test`, in check `name` or in every
    /// check.
    ForgetFlakyTest {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        test: String,
    },
    /// Run `argv` automatically after a session's writes in the workspace,
    /// recorded as check `name`. Replaces any earlier automatic check.
    AutoCheckEnable {
        workspace_id: String,
        name: String,
        argv: Vec<String>,
        timeout_s: u32,
        /// The wider check to run after it when the session's writes
        /// changed an indexed interface that other files mention.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_interface_change: Option<Box<WiderCheck>>,
    },
    AutoCheckDisable {
        workspace_id: String,
    },
    /// Read-only: every record for the workspace and whether it applies to
    /// `tree_oid`.
    CheckStatus {
        workspace_id: String,
        tree_oid: Option<String>,
    },
    /// Read-only: may `tree_oid` be landed on the strength of check `name`?
    LandingReadiness {
        workspace_id: String,
        name: String,
        tree_oid: String,
    },
    // -----------------------------------------------------------------------
    // Landings and landing turns
    // -----------------------------------------------------------------------
    RecordLanding {
        workspace_id: String,
        onto: String,
        commit_oid: String,
        tree_oid: String,
        inputs: Vec<String>,
        check_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rebased: Vec<Rewritten>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        adapted: Vec<String>,
    },
    /// Take the push turn for `onto`, or be told who holds it. With
    /// `queue`, wait in line for it instead.
    TakeTurn {
        workspace_id: String,
        onto: String,
        session_id: Option<String>,
        ttl_ms: u64,
        /// The turn this landing already holds: extend it to `ttl_ms` from
        /// now instead of taking a new one. Refused if it has ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue: Option<TurnQueueing>,
    },
    /// End a turn this landing holds, recording how each attempt went.
    ReleaseTurn {
        workspace_id: String,
        onto: String,
        turn_id: String,
        attempts: Vec<LandingAttempt>,
    },
    /// Stop a landing waiting for the push turn. One that holds it already
    /// is not stopped.
    LeaveTurnQueue {
        workspace_id: String,
        onto: String,
        ticket: String,
        reason: LeftQueue,
    },
    /// Who holds `onto`'s push turn and who waits for it. A read.
    TurnQueue {
        workspace_id: String,
        onto: String,
    },
    // -----------------------------------------------------------------------
    // Interface changes and notes
    // -----------------------------------------------------------------------
    ObserveInterface {
        workspace_id: String,
        path: String,
        changes: Vec<InterfaceChange>,
        dependents: Vec<String>,
    },
    /// Files of `workspace_id` that the index finds mention a name changed
    /// by the notice `notice_id` of `notice_workspace_id`, another worktree
    /// of the same repository (issue #194). Internal to the daemon's scan.
    ObserveInterfaceDependents {
        notice_workspace_id: String,
        notice_id: String,
        workspace_id: String,
        paths: Vec<String>,
    },
    /// `symbol` anchors the note to the one indexed definition of that name
    /// in `paths` (or in the workspace when empty), roadmap E1. The daemon
    /// looks it up (`RememberPrepared`); decided without its index, the
    /// anchor is `unknown` until the next complete scan.
    Remember {
        workspace_id: String,
        author: Option<String>,
        text: String,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        symbol: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
    },
    /// Compare the workspace's anchored notes with its index (roadmap E1),
    /// asked by the indexer after each pass. Records only state changes; an
    /// ask that changes nothing writes nothing, not even a receipt.
    ObserveNoteAnchors {
        workspace_id: String,
    },
    Forget {
        workspace_id: String,
        note_id: String,
    },
    // -----------------------------------------------------------------------
    // Messages
    // -----------------------------------------------------------------------
    /// Send one line to a session of the workspace, or with `to` absent to
    /// every other session of it. `from` absent means the owner.
    SendMessage {
        workspace_id: String,
        from: Option<String>,
        to: Option<String>,
        text: String,
        paths: Vec<String>,
    },
    /// The session has seen the message; it is not delivered to it again.
    AckMessage {
        session_id: String,
        message_id: String,
    },
    /// Read-only: one message as it was sent, from its `message_sent`
    /// event, so also after every recipient has acknowledged it.
    ShowMessage {
        workspace_id: String,
        message_id: String,
    },
    /// Read-only: who was given a sent message (roadmap E12).
    /// `message_id` absent means the newest message `from` sent, or the
    /// owner's newest when `from` is absent too.
    MessageStatus {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    /// Leave one line for the next other session that starts or is
    /// prompted in this session's workspace. Leases, intents and activity
    /// are unchanged.
    Handoff {
        session_id: String,
        text: String,
        paths: Vec<String>,
    },
    /// Withdraw a sent message or handoff: `message_id` absent means the
    /// newest message `from` sent. `from` is the sender, or absent for the
    /// owner, who may withdraw any message.
    RetractMessage {
        workspace_id: String,
        from: Option<String>,
        message_id: Option<String>,
    },
    // -----------------------------------------------------------------------
    // Scans, checkouts, uncoordinated changes and hosts
    // -----------------------------------------------------------------------
    /// The observer's latest attempt: a failure with its reason, or `None`
    /// for a completed scan.
    ObserveScan {
        workspace_id: String,
        failure: Option<String>,
        /// What a completed scan did not version; ignored for a failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        coverage: Option<ScanCoverage>,
    },
    /// The observer saw the checkout change under the sessions in it.
    /// Internal to the daemon's scan, like `ObserveDisk`.
    ObserveCheckout {
        workspace_id: String,
        from_branch: Option<String>,
        to_branch: Option<String>,
        from_head: Option<String>,
        to_head: Option<String>,
        files: u32,
    },
    /// A session's shell command ran a Git operation that changed its
    /// checkout (`CheckoutRun`). Sent by the session's post-tool hook.
    ObserveCheckoutRun {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        to_branch: Option<String>,
        to_head: Option<String>,
        /// How long the command ran, as its hook measured it.
        ran_ms: u64,
        /// How long ago the command finished, as its hook measured it when
        /// sending: a report redelivered later keeps the command's own
        /// window. A duration, not a time: the daemon places the finish
        /// this long before its own now, never after it. Absent from an
        /// earlier hook, which reported as the command finished.
        #[serde(default, skip_serializing_if = "is_zero_u64")]
        finished_ago_ms: u64,
        /// The command changed no file's content (`CheckoutRun`). Absent
        /// from an earlier hook.
        #[serde(default, skip_serializing_if = "is_false")]
        files_unchanged: bool,
    },
    /// Files a scan saw change outside a checkout change, at their new
    /// versions. The core keeps those no session of the workspace reported
    /// and names its Codex sessions (issue #129). Internal to the daemon's
    /// scan, like `ObserveDisk`.
    ObserveUncoordinated {
        workspace_id: String,
        files: Vec<ObservedFile>,
        /// When the scan began reading the checkout, on the clock the core is
        /// told. A session that reported a file since then is newer than the
        /// version the scan saw. Absent: not known, and no file is excused.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scanned_from_ms: Option<u64>,
    },
    /// The daemon's host check: every host present now with no LangSpan
    /// install covering it, by name (roadmap A7). Hosts held from an
    /// earlier check and absent here are cleared.
    ObserveHosts {
        not_installed: Vec<String>,
    },
    // -----------------------------------------------------------------------
    // Recaps, trails, recall and reports
    // -----------------------------------------------------------------------
    /// Read-only: a bounded account of one workspace. With `tag`, only
    /// notes carrying it are listed (roadmap E1). With `since`, the answer
    /// also says what happened after that point of the event log, read by
    /// the daemon's store (roadmap C10, capability `recap_since`).
    Recap {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<SinceFrom>,
    },
    /// Read-only: the newest trails ended sessions left in one workspace
    /// (roadmap C9), at most `limit` (1 to `MAX_TRAILS`, default 4).
    Trails {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read-only: the held notes of one workspace that hold every word of
    /// `query`, about `path` or the whole workspace when a path is given
    /// (roadmap E2). Never recorded, never counted as delivered.
    Recall {
        workspace_id: String,
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        limit: u32,
    },
    /// Read-only, answered from the event log: the recorded interface
    /// changes of definitions named `name`, newest first, in `path` or in
    /// any file, older than `before_seq` when given (roadmap D4). Never
    /// recorded.
    History {
        workspace_id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        limit: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_seq: Option<u64>,
    },
    /// Read-only, answered from the event log: what happened since
    /// `from_ms` (wall clock, Unix milliseconds) in one workspace, or in
    /// every workspace when none is named, and what is still open.
    Today {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        from_ms: u64,
    },
    /// Read-only, answered from the event log: the coordination report of
    /// one workspace since `from_ms` (wall clock, Unix milliseconds), per
    /// session and per host kind.
    Report {
        workspace_id: String,
        from_ms: u64,
    },
    /// Read-only, answered from the event log: refusals in the local days
    /// starting at each of `days` (wall clock, Unix milliseconds), the last
    /// being the end, once per session, kind and path set per day (H5).
    Count {
        days: Vec<u64>,
    },
    /// Read-only: the person's status line for the directory `root`, with
    /// refusals counted from `from_ms` (wall clock, Unix milliseconds).
    StatusLine {
        root: String,
        from_ms: u64,
    },
    // -----------------------------------------------------------------------
    // Refusals, the conflict policy and waits
    // -----------------------------------------------------------------------
    /// A refusal the hook decided itself (a held commit), recorded as a
    /// `WriteRefused` event. Other kinds are recorded by the admission
    /// that refuses them.
    RecordRefusal {
        session_id: String,
        kind: RefusalKind,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_session_id: Option<String>,
    },
    /// Read-only: the policy table in effect for a workspace (roadmap B28).
    Policy {
        workspace_id: String,
    },
    /// The owner sets the policy for a current holder. Every other case is
    /// fixed and refused with `policy_fixed`.
    SetPolicy {
        workspace_id: String,
        case: PolicyCase,
        policy: Policy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u32>,
    },
    /// Read-only: the current holders an admission of these paths would
    /// meet now, polled by a hook that waits for them.
    Holders {
        session_id: String,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        base: Vec<ObservedFile>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regions: Vec<PathRegions>,
    },
    /// A hook's wait for a current holder and how it ended, recorded as
    /// `WriteWaited`, with the refusal it ended in. `changed` are the paths
    /// whose version changed during the wait, as they are now.
    RecordWait {
        session_id: String,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holder_session_id: Option<String>,
        waited_ms: u64,
        result: WaitResult,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<ObservedFile>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_digest: Option<String>,
    },
    // -----------------------------------------------------------------------
    // The source graph
    // -----------------------------------------------------------------------
    /// Read-only, answered from the derived index: where `name` is defined.
    /// With `detail`, each definition also carries its body digest, calls,
    /// callers, notes and last check (roadmap D1).
    GraphSymbol {
        workspace_id: String,
        name: String,
        limit: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        detail: bool,
        /// The client reads `GraphCoverage::blind_in_scope`. A request
        /// without it is one an earlier build sends, and its answer keeps
        /// the coverage shape that build knows.
        #[serde(default, skip_serializing_if = "is_false")]
        blind_in_scope: bool,
        /// Rank the definitions as a code briefing does and give each the
        /// files that mention its name (roadmap C2).
        #[serde(default, skip_serializing_if = "is_false")]
        rank: bool,
        /// The asking session, whose own files rank first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Follow recorded renames of `name` to its newest name and look
        /// that up instead (roadmap D7); the answer's `renamed` says which.
        #[serde(default, skip_serializing_if = "is_false")]
        renamed: bool,
    },
    /// Read-only: where `name` is mentioned.
    GraphReferences {
        workspace_id: String,
        name: String,
        limit: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        blind_in_scope: bool,
    },
    /// Read-only: what `path` exports and which files mention those names.
    GraphImpact {
        workspace_id: String,
        path: String,
        limit: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        blind_in_scope: bool,
        /// Levels to follow, 1 to `MAX_IMPACT_DEPTH` (roadmap D2). Above 1
        /// the answer adds `levels`; at 1 the field is absent from the
        /// request, which is then the one earlier builds send, and the
        /// answer is theirs too.
        #[serde(default = "one", skip_serializing_if = "is_one")]
        depth: u32,
    },
    /// Read-only: the files the index cannot answer for, by reason, at
    /// most `limit` paths per list (roadmap D8).
    GraphBlindSpots {
        workspace_id: String,
        limit: u32,
    },
    /// Read-only: the shortest chains of calls from a definition named
    /// `from` to one named `to`, at most `max_hops` calls long (1 to
    /// `MAX_PATH_HOPS`), found by name and by import path (roadmap D3).
    GraphPath {
        workspace_id: String,
        from: String,
        to: String,
        #[serde(default = "default_path_hops")]
        max_hops: u32,
    },
    /// Read-only: the workspace's areas at `depth` (1 to `MAX_MAP_DEPTH`)
    /// with their files, exports and languages, and the edges between them
    /// (roadmap D3).
    GraphMap {
        workspace_id: String,
        #[serde(default = "one")]
        depth: u32,
    },
    /// Read-only: for each name that is defined in the workspace, up to
    /// `limit` definitions and the files that mention it. Never recorded.
    GraphContext {
        workspace_id: String,
        names: Vec<String>,
        limit: u32,
        /// Rank each name's definitions and give each the files that
        /// mention its name (roadmap C2). Without it the answer is the one
        /// earlier builds give, in path order.
        #[serde(default, skip_serializing_if = "is_false")]
        rank: bool,
        /// The asking session, whose own files rank first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Paths and qualifiers the prompt names (`store` in `store::upload`).
        /// Used for this query only: never logged, recorded or kept.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        qualifiers: Vec<String>,
    },
    /// Read-only: what other worktrees of the workspace's repository have
    /// in flight that any of its indexed files mention (roadmap C7). Never
    /// recorded.
    GraphInflight {
        workspace_id: String,
    },
    /// Owner-only, CLI-only: write the workspace's source index to the
    /// absolute path `path` as JSON Lines, created owner-only and never over
    /// an existing file (roadmap D5). Not recorded.
    GraphExport {
        workspace_id: String,
        path: String,
    },
    /// Owner-only, CLI-only: read an exported index from the absolute path
    /// `path`. Only files whose content here has the exported digest, and
    /// that the index does not hold already, are loaded, and only with
    /// `apply`; without it nothing changes. Not recorded (roadmap D5).
    GraphImport {
        workspace_id: String,
        path: String,
        #[serde(default, skip_serializing_if = "is_false")]
        apply: bool,
    },
    // -----------------------------------------------------------------------
    // Write checks, interface alerts and outlines
    // -----------------------------------------------------------------------
    /// Answered by the daemon from its records, the line journal and its
    /// source index (roadmap B26): whether `paths` are held, which of their
    /// lines peers wrote recently and whether they are stale for the
    /// session, and what a peer just changed in `symbols` (or, without
    /// any, in the definitions of `paths`). A read: never recorded.
    CheckWrite {
        session_id: String,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        symbols: Vec<String>,
    },
    /// Answered by the daemon with its source index: the calls among
    /// `calls` whose argument count does not fit a signature another live
    /// session changed, from notices not yet alerted to this session. The
    /// daemon records the notices it alerts to (`AcknowledgeInterface`);
    /// the calls themselves are never recorded. `renamed` (`rename_holds`,
    /// issue #213): names the write renames or removes; `calls` may then be empty.
    CheckCalls {
        session_id: String,
        calls: Vec<CallSite>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        renamed: Vec<String>,
    },
    /// The session was alerted to these interface notices at a write; it
    /// is not alerted to them again.
    AcknowledgeInterface {
        session_id: String,
        notice_ids: Vec<String>,
    },
    /// The interface notices owed to a running session that no briefing or
    /// earlier delivery told it, for a host's between-step mechanism
    /// (issue #214). Records only what it tells. `stops` is the count an
    /// earlier answer gave: once the session finished a reply since, its
    /// turn is over and nothing is told. Never the session at work: a
    /// waiting watcher does not count as activity. `prompt_id` is the
    /// host's ID of the prompt the watcher started at: once that prompt's
    /// reply finished, even before the watcher's first answer, its turn is
    /// over.
    BetweenSteps {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stops: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_id: Option<String>,
    },
    /// The session's hook placed these interface notices of a briefing in
    /// its context (`Briefing::interface_notice_ids`), so no between-step
    /// watcher tells them again (issue #214). A hook reports only the ones
    /// its rendered text showed.
    InterfaceShown {
        session_id: String,
        notice_ids: Vec<String>,
    },
    /// A whole read by the session of `path`, whose content on disk has
    /// SHA-256 `digest` and is `lines` lines and `bytes` bytes long
    /// (roadmap A11). The daemon answers with the file's outline when the
    /// file is large, indexed whole at that digest, and the session was not
    /// given this version's outline before; otherwise with none.
    OutlineRead {
        session_id: String,
        path: String,
        digest: String,
        lines: u64,
        bytes: u64,
    },
    // -----------------------------------------------------------------------
    // Completions and observed operations, files, branches and Git state
    // -----------------------------------------------------------------------
    Complete {
        session_id: String,
        lease_id: String,
    },
    Observe {
        session_id: String,
        operation_id: String,
        source: String,
        tool: String,
        paths: Vec<String>,
        result: ToolResult,
        lease_id: Option<String>,
    },
    ObserveTool {
        session_id: String,
        operation: ToolIdentity,
        paths: Vec<String>,
        result: ToolResult,
        lease_id: Option<String>,
    },
    ObserveDisk {
        workspace_id: String,
        path: String,
        before: Option<FileState>,
        after: FileState,
        related_operation_ids: Vec<String>,
    },
    ObserveBranch {
        workspace_id: String,
        branch_ref: String,
        head_oid: Option<String>,
        tree_oid: Option<String>,
    },
    ObserveWorkspaceGit {
        workspace_id: String,
        repository_id: String,
        branch_ref: Option<String>,
    },
    // -----------------------------------------------------------------------
    // Prompts, presence and a session's turns
    // -----------------------------------------------------------------------
    SessionPrompt {
        session_id: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        line_digests: Vec<String>,
    },
    /// Where the definitions the session's prompt named are, from the
    /// source index: replaces the session's earlier prompt targets. Paths
    /// only, at most `MAX_PROMPT_TARGETS`.
    SetPromptTargets {
        session_id: String,
        paths: Vec<String>,
    },
    /// Paths a search or shell command looked at: 1 to `PRESENCE_PATHS`
    /// relative paths, never the pattern or the command. `kind` is `search`
    /// or `shell`; reads arrive as `observe_source`.
    ObservePresence {
        session_id: String,
        kind: PresenceKind,
        paths: Vec<String>,
    },
    /// Whether peers are shown where this session looks, and how old a
    /// peer's presence this session is shown (`window_ms`,
    /// `MIN_PRESENCE_WINDOW_MS` to `BRIEF_HORIZON_MS`).
    SetPresence {
        session_id: String,
        shown: bool,
        window_ms: u64,
    },
    SessionEnd {
        session_id: String,
        /// What the end hook read from the line journal for the trail
        /// (roadmap C9). Absent from an older client: the trail has no line
        /// ranges and no uncommitted files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trail: Option<TrailFacts>,
    },
    /// The session finished a reply: complete what its tool calls still
    /// hold. `failure` is the host's error type (lower-case letters and
    /// underscores) when the reply ended on an error.
    SessionStop {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<String>,
        /// The host's ID of the prompt whose reply finished (issue #214).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_id: Option<String>,
    },
    /// A batch of the session's tool calls has finished.
    ObserveToolBatch {
        session_id: String,
        operation_ids: Vec<String>,
        calls: u32,
    },
    // -----------------------------------------------------------------------
    // Briefings, adoption and prepared adapter input
    // -----------------------------------------------------------------------
    Brief {
        session_id: String,
        max_peers: u32,
        /// True when the result is being placed in the session's context.
        /// Only then is it remembered as delivered; a look from outside
        /// neither suppresses the session's next briefing nor counts as
        /// activity.
        deliver: bool,
        /// True for a briefing placed in context during a turn, after a
        /// tool call. A handoff waits for a session's start or next prompt.
        #[serde(default, skip_serializing_if = "is_false")]
        mid_turn: bool,
        /// Add what changed recently in the repository (roadmap C3). Set
        /// by session-start hooks and `langspan brief --recent` only; never
        /// with `mid_turn`.
        #[serde(default, skip_serializing_if = "is_false")]
        recent: bool,
        /// How far back `recent` looks. The daemon sets it from its
        /// configuration; absent means `RECENT_WINDOW_MS`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recent_window_ms: Option<u64>,
        /// True when the session asked through the MCP tool
        /// `langspan_brief`, so a message it carries is recorded as
        /// delivered that way (roadmap E12).
        #[serde(default, skip_serializing_if = "is_false")]
        mcp: bool,
        /// Sessions whose journaled lines are still uncommitted in this
        /// session's checkout, as its adapter found them at the session's
        /// start. The ended ones nobody adopted are named in the briefing.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        left: Vec<String>,
        /// Leave out what the session was already told and is unchanged,
        /// and count it in `Briefing::unchanged` (issue #222). Set by
        /// prompt and mid-turn hooks; a session start and an explicit
        /// request get the full view.
        #[serde(default, skip_serializing_if = "is_false")]
        changes_only: bool,
    },
    /// Take over an ended session's journaled, uncommitted lines in the
    /// same workspace (issue #161). Refused for a live session and for one
    /// of another workspace. The line journal is the adapter's: once the
    /// event is recorded, the adapter credits the lines to `session_id`.
    Adopt {
        session_id: String,
        from: String,
    },
    /// Prepared adapter input only, as `WorkspaceRegisterVerified`: the
    /// receipt stores the original `Brief`. `inflight` is what the daemon's
    /// source index found other worktrees have in flight that the session's
    /// workspace mentions (roadmap C7), and `inflight_partial` that the
    /// index had not finished a first scan of a worktree it reads.
    #[serde(skip_serializing)]
    BriefPrepared {
        session_id: String,
        max_peers: u32,
        deliver: bool,
        mid_turn: bool,
        recent: bool,
        recent_window_ms: Option<u64>,
        mcp: bool,
        left: Vec<String>,
        changes_only: bool,
        inflight: Vec<InflightSymbol>,
        #[serde(default)]
        inflight_partial: bool,
        /// Names of anchored notes that the session's files mention, as
        /// the index found them (roadmap E1).
        mentioned: BTreeSet<String>,
    },
    /// Prepared adapter input only: the receipt stores the original
    /// `Remember`. `index` is what the daemon's source index holds of
    /// `symbol`, absent without a complete scan.
    #[serde(skip_serializing)]
    RememberPrepared {
        workspace_id: String,
        author: Option<String>,
        text: String,
        paths: Vec<String>,
        symbol: Option<String>,
        tags: Vec<String>,
        index: Option<AnchorIndex>,
    },
    /// Prepared adapter input only: `ObserveNoteAnchors` with one snapshot
    /// of the index. The daemon sends none without a complete scan.
    #[serde(skip_serializing)]
    NoteAnchorsPrepared {
        workspace_id: String,
        index: AnchorIndex,
    },
    // -----------------------------------------------------------------------
    // Reads of the state and the event log, the version and retention
    // -----------------------------------------------------------------------
    /// Read-only: the bounded view of what matters now. Closed admissions
    /// are counted, not listed; the answer always fits one frame.
    Status,
    /// Read-only: one page of every state record, history included, after
    /// the cursor `after` (from the start when absent). Answered by the
    /// daemon's store, which bounds each page's bytes.
    StatusRecords {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        /// A smaller page than the store's own bound, in bytes; absent for
        /// the store's bound. The MCP adapter asks for one when a full page
        /// does not fit its reply, which carries the page twice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_bytes: Option<u32>,
    },
    /// Read-only: a bounded view of every session on every machine heard
    /// from, with what is held, what overlaps, evidence and health.
    Dashboard,
    /// Read-only: the version the running coordinator was built as and its
    /// process ID, for `doctor` (roadmap I5). Answered by the daemon, never
    /// recorded. A daemon from before it answers with an error.
    Version,
    Events {
        after: u64,
        limit: u32,
    },
    Changes {
        workspace_id: String,
        after: u64,
        limit: u32,
    },
    /// Retention (ADR 0025, J7): snapshot the derived state and delete the
    /// events and receipts recorded more than `keep_days` ago. A dry run
    /// (`apply` false) deletes nothing and reports what would go. Answered
    /// by the daemon's store; never recorded and never replayed.
    Compact {
        keep_days: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        apply: bool,
    },
}

// Serde's internally tagged unit variants ignore extra fields. The empty struct
// variant here preserves Command::Status while enforcing the wire contract.
#[derive(Deserialize)]
#[serde(
    remote = "Command",
    tag = "op",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum CommandWire {
    // -----------------------------------------------------------------------
    // Workspaces and sessions
    // -----------------------------------------------------------------------
    WorkspaceRegister {
        workspace_id: String,
        root: String,
    },
    WorkspaceList {
        #[serde(default)]
        after: Option<String>,
    },
    WorkspaceMove {
        workspace_id: String,
        root: String,
        #[serde(default)]
        switch: bool,
    },
    WorkspaceRemove {
        workspace_id: String,
        #[serde(default)]
        apply: bool,
    },
    SessionRegister {
        session_id: String,
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        headless: bool,
    },
    ObserveSessionLiveness {
        session_id: String,
        process: ProcessIdentity,
        status: LivenessStatus,
    },
    /// A hook run of the session reached the daemon. Recorded only when the
    /// last record is `SESSION_SEEN_INTERVAL_MS` old (roadmap J12).
    SessionSeen {
        session_id: String,
    },
    /// The hook placed `bytes` of text in the session's context, at a
    /// turn's start (`turn`, zero bytes allowed) or after a tool call
    /// (issue #222). A measurement only.
    RecordContext {
        session_id: String,
        bytes: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        turn: bool,
    },
    /// The hook could not place these briefing items in the session's
    /// context (its text budget was full): they are no longer told, so the
    /// next briefing of changes only tells them (review comment 4711375989).
    /// At most `MAX_TOLD` keys, from `briefing_keys`.
    ForgetTold {
        session_id: String,
        keys: Vec<u64>,
    },
    // -----------------------------------------------------------------------
    // Intents, admissions and source versions
    // -----------------------------------------------------------------------
    IntentSet {
        session_id: String,
        paths: Vec<String>,
        summary: String,
    },
    IntentEnd {
        session_id: String,
    },
    Admit {
        session_id: String,
        paths: Vec<String>,
        ttl_ms: u64,
    },
    AdmitTool {
        session_id: String,
        paths: Vec<String>,
        ttl_ms: u64,
        operation: ToolIdentity,
        /// The admitted files as the adapter saw them on disk just before
        /// asking. A path left out is admitted with an unknown base.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        base: Vec<ObservedFile>,
        /// Where in each file the edit was located. A path left out, or
        /// whose base is not the version its regions name, is held whole.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regions: Vec<PathRegions>,
        /// Lines other sessions wrote recently that this edit would change,
        /// in the base version, from the hook's line journal.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overwrites: Vec<WrittenLines>,
    },
    ObserveSource {
        session_id: String,
        files: Vec<ObservedFile>,
        origin: SourceOrigin,
    },
    // -----------------------------------------------------------------------
    // Remote publication
    // -----------------------------------------------------------------------
    RemoteEnable {
        workspace_id: String,
        remote: String,
        participant: String,
        name: String,
        share_task: bool,
        #[serde(default)]
        publish_as_branch: bool,
    },
    RemoteDisable {
        workspace_id: String,
    },
    /// Read-only: what this workspace would publish next.
    RemoteOutbox {
        workspace_id: String,
    },
    /// The publication was pushed. `payload` is the exact `state.json` the
    /// remote now holds (at most `PUBLICATION_BYTES`) and `published_at_ms`
    /// the wall-clock time of the push in Unix milliseconds, for people.
    RemotePublished {
        workspace_id: String,
        sequence: u64,
        signature: String,
        /// Absent from records written before payloads were kept.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        payload: String,
        #[serde(default, skip_serializing_if = "is_zero_u64")]
        published_at_ms: u64,
        /// The namespace the push went to (`true`: the branch namespace).
        /// Absent from records of earlier builds, which are taken as current.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        as_branch: Option<bool>,
    },
    /// Read-only: the latest publications that left for a workspace, newest
    /// first, with their exact payloads.
    RemoteSent {
        workspace_id: String,
        limit: u32,
    },
    ObserveRemote {
        workspace_id: String,
        publication: Publication,
    },
    /// The teammate's ref is gone from the remote: they withdrew.
    ForgetRemote {
        workspace_id: String,
        participant: String,
    },
    /// The daemon removed a disabled workspace's ref from the remote.
    RemoteWithdrawn {
        workspace_id: String,
    },
    // -----------------------------------------------------------------------
    // Checks and test results
    // -----------------------------------------------------------------------
    RecordCheck {
        workspace_id: String,
        name: String,
        command: String,
        tree_oid: String,
        head_oid: Option<String>,
        inputs: Vec<String>,
        result: CheckResult,
        duration_ms: u64,
        /// For an automatic check: the sessions to tell, and its output's
        /// end.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<Box<CheckReport>>,
        /// The tests its output named, for a working-tree check that passed
        /// or failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tests: Option<TestResults>,
        /// The tests of a second run on the same tree, made to learn which
        /// tests flip; its result is not recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rerun: Option<TestResults>,
        /// With `rerun`: the workspace's [`State::writes`] when the first
        /// run's tree was taken. The rerun is compared only if no write was
        /// recorded since, even one the tree no longer shows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        writes_before: Option<u64>,
        /// With `tests`: the command's identity, 64 lowercase hex digits
        /// that differ for any two argument vectors. `command` is a cut and
        /// redacted summary, which two commands can share; results are
        /// compared only when this is equal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        /// For a combined check asked for them: what comparing the
        /// branches' definitions found, kept with the record.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        symbol_findings: Option<Box<SymbolFindings>>,
    },
    /// Read-only: the tests known to flip on unchanged code, of check `name`
    /// or of every check.
    FlakyTests {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Clear what was learned about `test`, in check `name` or in every
    /// check.
    ForgetFlakyTest {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        test: String,
    },
    /// Run `argv` automatically after a session's writes in the workspace,
    /// recorded as check `name`. Replaces any earlier automatic check.
    AutoCheckEnable {
        workspace_id: String,
        name: String,
        argv: Vec<String>,
        timeout_s: u32,
        /// The wider check to run after it when the session's writes
        /// changed an indexed interface that other files mention.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_interface_change: Option<Box<WiderCheck>>,
    },
    AutoCheckDisable {
        workspace_id: String,
    },
    /// Read-only: every record for the workspace and whether it applies to
    /// `tree_oid`.
    CheckStatus {
        workspace_id: String,
        tree_oid: Option<String>,
    },
    /// Read-only: may `tree_oid` be landed on the strength of check `name`?
    LandingReadiness {
        workspace_id: String,
        name: String,
        tree_oid: String,
    },
    // -----------------------------------------------------------------------
    // Landings and landing turns
    // -----------------------------------------------------------------------
    RecordLanding {
        workspace_id: String,
        onto: String,
        commit_oid: String,
        tree_oid: String,
        inputs: Vec<String>,
        check_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rebased: Vec<Rewritten>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        adapted: Vec<String>,
    },
    /// Take the push turn for `onto`, or be told who holds it. With
    /// `queue`, wait in line for it instead.
    TakeTurn {
        workspace_id: String,
        onto: String,
        session_id: Option<String>,
        ttl_ms: u64,
        /// The turn this landing already holds: extend it to `ttl_ms` from
        /// now instead of taking a new one. Refused if it has ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue: Option<TurnQueueing>,
    },
    /// End a turn this landing holds, recording how each attempt went.
    ReleaseTurn {
        workspace_id: String,
        onto: String,
        turn_id: String,
        attempts: Vec<LandingAttempt>,
    },
    /// Stop a landing waiting for the push turn. One that holds it already
    /// is not stopped.
    LeaveTurnQueue {
        workspace_id: String,
        onto: String,
        ticket: String,
        reason: LeftQueue,
    },
    /// Who holds `onto`'s push turn and who waits for it. A read.
    TurnQueue {
        workspace_id: String,
        onto: String,
    },
    // -----------------------------------------------------------------------
    // Interface changes and notes
    // -----------------------------------------------------------------------
    ObserveInterface {
        workspace_id: String,
        path: String,
        changes: Vec<InterfaceChange>,
        dependents: Vec<String>,
    },
    /// Files of `workspace_id` that the index finds mention a name changed
    /// by the notice `notice_id` of `notice_workspace_id`, another worktree
    /// of the same repository (issue #194). Internal to the daemon's scan.
    ObserveInterfaceDependents {
        notice_workspace_id: String,
        notice_id: String,
        workspace_id: String,
        paths: Vec<String>,
    },
    /// `symbol` anchors the note to the one indexed definition of that name
    /// in `paths` (or in the workspace when empty), roadmap E1. The daemon
    /// looks it up (`RememberPrepared`); decided without its index, the
    /// anchor is `unknown` until the next complete scan.
    Remember {
        workspace_id: String,
        author: Option<String>,
        text: String,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        symbol: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
    },
    /// Compare the workspace's anchored notes with its index (roadmap E1),
    /// asked by the indexer after each pass. Records only state changes; an
    /// ask that changes nothing writes nothing, not even a receipt.
    ObserveNoteAnchors {
        workspace_id: String,
    },
    Forget {
        workspace_id: String,
        note_id: String,
    },
    // -----------------------------------------------------------------------
    // Messages
    // -----------------------------------------------------------------------
    /// Send one line to a session of the workspace, or with `to` absent to
    /// every other session of it. `from` absent means the owner.
    SendMessage {
        workspace_id: String,
        from: Option<String>,
        to: Option<String>,
        text: String,
        paths: Vec<String>,
    },
    /// The session has seen the message; it is not delivered to it again.
    AckMessage {
        session_id: String,
        message_id: String,
    },
    /// Read-only: one message as it was sent, from its `message_sent`
    /// event, so also after every recipient has acknowledged it.
    ShowMessage {
        workspace_id: String,
        message_id: String,
    },
    /// Read-only: who was given a sent message (roadmap E12).
    /// `message_id` absent means the newest message `from` sent, or the
    /// owner's newest when `from` is absent too.
    MessageStatus {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    /// Leave one line for the next other session that starts or is
    /// prompted in this session's workspace. Leases, intents and activity
    /// are unchanged.
    Handoff {
        session_id: String,
        text: String,
        paths: Vec<String>,
    },
    /// Withdraw a sent message or handoff: `message_id` absent means the
    /// newest message `from` sent. `from` is the sender, or absent for the
    /// owner, who may withdraw any message.
    RetractMessage {
        workspace_id: String,
        from: Option<String>,
        message_id: Option<String>,
    },
    // -----------------------------------------------------------------------
    // Scans, checkouts, uncoordinated changes and hosts
    // -----------------------------------------------------------------------
    /// The observer's latest attempt: a failure with its reason, or `None`
    /// for a completed scan.
    ObserveScan {
        workspace_id: String,
        failure: Option<String>,
        /// What a completed scan did not version; ignored for a failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        coverage: Option<ScanCoverage>,
    },
    /// The observer saw the checkout change under the sessions in it.
    /// Internal to the daemon's scan, like `ObserveDisk`.
    ObserveCheckout {
        workspace_id: String,
        from_branch: Option<String>,
        to_branch: Option<String>,
        from_head: Option<String>,
        to_head: Option<String>,
        files: u32,
    },
    /// A session's shell command ran a Git operation that changed its
    /// checkout (`CheckoutRun`). Sent by the session's post-tool hook.
    ObserveCheckoutRun {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        to_branch: Option<String>,
        to_head: Option<String>,
        /// How long the command ran, as its hook measured it.
        ran_ms: u64,
        /// How long ago the command finished, as its hook measured it when
        /// sending: a report redelivered later keeps the command's own
        /// window. A duration, not a time: the daemon places the finish
        /// this long before its own now, never after it. Absent from an
        /// earlier hook, which reported as the command finished.
        #[serde(default, skip_serializing_if = "is_zero_u64")]
        finished_ago_ms: u64,
        /// The command changed no file's content (`CheckoutRun`). Absent
        /// from an earlier hook.
        #[serde(default, skip_serializing_if = "is_false")]
        files_unchanged: bool,
    },
    /// Files a scan saw change outside a checkout change, at their new
    /// versions. The core keeps those no session of the workspace reported
    /// and names its Codex sessions (issue #129). Internal to the daemon's
    /// scan, like `ObserveDisk`.
    ObserveUncoordinated {
        workspace_id: String,
        files: Vec<ObservedFile>,
        /// When the scan began reading the checkout, on the clock the core is
        /// told. A session that reported a file since then is newer than the
        /// version the scan saw. Absent: not known, and no file is excused.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scanned_from_ms: Option<u64>,
    },
    /// The daemon's host check: every host present now with no LangSpan
    /// install covering it, by name (roadmap A7). Hosts held from an
    /// earlier check and absent here are cleared.
    ObserveHosts {
        not_installed: Vec<String>,
    },
    // -----------------------------------------------------------------------
    // Recaps, trails, recall and reports
    // -----------------------------------------------------------------------
    /// Read-only: a bounded account of one workspace. With `tag`, only
    /// notes carrying it are listed (roadmap E1). With `since`, the answer
    /// also says what happened after that point of the event log, read by
    /// the daemon's store (roadmap C10, capability `recap_since`).
    Recap {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<SinceFrom>,
    },
    /// Read-only: the newest trails ended sessions left in one workspace
    /// (roadmap C9), at most `limit` (1 to `MAX_TRAILS`, default 4).
    Trails {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read-only: the held notes of one workspace that hold every word of
    /// `query`, about `path` or the whole workspace when a path is given
    /// (roadmap E2). Never recorded, never counted as delivered.
    Recall {
        workspace_id: String,
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        limit: u32,
    },
    /// Read-only, answered from the event log: the recorded interface
    /// changes of definitions named `name`, newest first, in `path` or in
    /// any file, older than `before_seq` when given (roadmap D4). Never
    /// recorded.
    History {
        workspace_id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        limit: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_seq: Option<u64>,
    },
    /// Read-only, answered from the event log: what happened since
    /// `from_ms` (wall clock, Unix milliseconds) in one workspace, or in
    /// every workspace when none is named, and what is still open.
    Today {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        from_ms: u64,
    },
    /// Read-only, answered from the event log: the coordination report of
    /// one workspace since `from_ms` (wall clock, Unix milliseconds), per
    /// session and per host kind.
    Report {
        workspace_id: String,
        from_ms: u64,
    },
    /// Read-only, answered from the event log: refusals in the local days
    /// starting at each of `days` (wall clock, Unix milliseconds), the last
    /// being the end, once per session, kind and path set per day (H5).
    Count {
        days: Vec<u64>,
    },
    /// Read-only: the person's status line for the directory `root`, with
    /// refusals counted from `from_ms` (wall clock, Unix milliseconds).
    StatusLine {
        root: String,
        from_ms: u64,
    },
    // -----------------------------------------------------------------------
    // Refusals, the conflict policy and waits
    // -----------------------------------------------------------------------
    /// A refusal the hook decided itself (a held commit), recorded as a
    /// `WriteRefused` event. Other kinds are recorded by the admission
    /// that refuses them.
    RecordRefusal {
        session_id: String,
        kind: RefusalKind,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_session_id: Option<String>,
    },
    /// Read-only: the policy table in effect for a workspace (roadmap B28).
    Policy {
        workspace_id: String,
    },
    /// The owner sets the policy for a current holder. Every other case is
    /// fixed and refused with `policy_fixed`.
    SetPolicy {
        workspace_id: String,
        case: PolicyCase,
        policy: Policy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u32>,
    },
    /// Read-only: the current holders an admission of these paths would
    /// meet now, polled by a hook that waits for them.
    Holders {
        session_id: String,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        base: Vec<ObservedFile>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regions: Vec<PathRegions>,
    },
    /// A hook's wait for a current holder and how it ended, recorded as
    /// `WriteWaited`, with the refusal it ended in. `changed` are the paths
    /// whose version changed during the wait, as they are now.
    RecordWait {
        session_id: String,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holder_session_id: Option<String>,
        waited_ms: u64,
        result: WaitResult,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<ObservedFile>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_digest: Option<String>,
    },
    // -----------------------------------------------------------------------
    // The source graph
    // -----------------------------------------------------------------------
    /// Read-only, answered from the derived index: where `name` is defined.
    /// With `detail`, each definition also carries its body digest, calls,
    /// callers, notes and last check (roadmap D1).
    GraphSymbol {
        workspace_id: String,
        name: String,
        limit: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        detail: bool,
        /// The client reads `GraphCoverage::blind_in_scope`. A request
        /// without it is one an earlier build sends, and its answer keeps
        /// the coverage shape that build knows.
        #[serde(default, skip_serializing_if = "is_false")]
        blind_in_scope: bool,
        /// Rank the definitions as a code briefing does and give each the
        /// files that mention its name (roadmap C2).
        #[serde(default, skip_serializing_if = "is_false")]
        rank: bool,
        /// The asking session, whose own files rank first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Follow recorded renames of `name` to its newest name and look
        /// that up instead (roadmap D7); the answer's `renamed` says which.
        #[serde(default, skip_serializing_if = "is_false")]
        renamed: bool,
    },
    /// Read-only: where `name` is mentioned.
    GraphReferences {
        workspace_id: String,
        name: String,
        limit: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        blind_in_scope: bool,
    },
    /// Read-only: what `path` exports and which files mention those names.
    GraphImpact {
        workspace_id: String,
        path: String,
        limit: u32,
        #[serde(default, skip_serializing_if = "is_false")]
        blind_in_scope: bool,
        /// Levels to follow, 1 to `MAX_IMPACT_DEPTH` (roadmap D2). Above 1
        /// the answer adds `levels`; at 1 the field is absent from the
        /// request, which is then the one earlier builds send, and the
        /// answer is theirs too.
        #[serde(default = "one", skip_serializing_if = "is_one")]
        depth: u32,
    },
    /// Read-only: the files the index cannot answer for, by reason, at
    /// most `limit` paths per list (roadmap D8).
    GraphBlindSpots {
        workspace_id: String,
        limit: u32,
    },
    GraphPath {
        workspace_id: String,
        from: String,
        to: String,
        #[serde(default = "default_path_hops")]
        max_hops: u32,
    },
    GraphMap {
        workspace_id: String,
        #[serde(default = "one")]
        depth: u32,
    },
    /// Read-only: for each name that is defined in the workspace, up to
    /// `limit` definitions and the files that mention it. Never recorded.
    GraphContext {
        workspace_id: String,
        names: Vec<String>,
        limit: u32,
        /// Rank each name's definitions and give each the files that
        /// mention its name (roadmap C2). Without it the answer is the one
        /// earlier builds give, in path order.
        #[serde(default, skip_serializing_if = "is_false")]
        rank: bool,
        /// The asking session, whose own files rank first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Paths and qualifiers the prompt names (`store` in `store::upload`).
        /// Used for this query only: never logged, recorded or kept.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        qualifiers: Vec<String>,
    },
    /// Read-only: what other worktrees of the workspace's repository have
    /// in flight that any of its indexed files mention (roadmap C7). Never
    /// recorded.
    GraphInflight {
        workspace_id: String,
    },
    GraphExport {
        workspace_id: String,
        path: String,
    },
    GraphImport {
        workspace_id: String,
        path: String,
        #[serde(default)]
        apply: bool,
    },
    // -----------------------------------------------------------------------
    // Write checks, interface alerts and outlines
    // -----------------------------------------------------------------------
    CheckWrite {
        session_id: String,
        paths: Vec<String>,
        #[serde(default)]
        symbols: Vec<String>,
    },
    /// Answered by the daemon with its source index: the calls among
    /// `calls` whose argument count does not fit a signature another live
    /// session changed, from notices not yet alerted to this session. The
    /// daemon records the notices it alerts to (`AcknowledgeInterface`);
    /// the calls themselves are never recorded. `renamed` (`rename_holds`,
    /// issue #213): names the write renames or removes; `calls` may then be empty.
    CheckCalls {
        session_id: String,
        calls: Vec<CallSite>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        renamed: Vec<String>,
    },
    /// The session was alerted to these interface notices at a write; it
    /// is not alerted to them again.
    AcknowledgeInterface {
        session_id: String,
        notice_ids: Vec<String>,
    },
    /// The interface notices owed to a running session that no briefing or
    /// earlier delivery told it, for a host's between-step mechanism
    /// (issue #214). Records only what it tells. `stops` is the count an
    /// earlier answer gave: once the session finished a reply since, its
    /// turn is over and nothing is told. Never the session at work: a
    /// waiting watcher does not count as activity. `prompt_id` is the
    /// host's ID of the prompt the watcher started at: once that prompt's
    /// reply finished, even before the watcher's first answer, its turn is
    /// over.
    BetweenSteps {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stops: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_id: Option<String>,
    },
    /// The session's hook placed these interface notices of a briefing in
    /// its context (`Briefing::interface_notice_ids`), so no between-step
    /// watcher tells them again (issue #214). A hook reports only the ones
    /// its rendered text showed.
    InterfaceShown {
        session_id: String,
        notice_ids: Vec<String>,
    },
    /// A whole read by the session of `path`, whose content on disk has
    /// SHA-256 `digest` and is `lines` lines and `bytes` bytes long
    /// (roadmap A11). The daemon answers with the file's outline when the
    /// file is large, indexed whole at that digest, and the session was not
    /// given this version's outline before; otherwise with none.
    OutlineRead {
        session_id: String,
        path: String,
        digest: String,
        lines: u64,
        bytes: u64,
    },
    // -----------------------------------------------------------------------
    // Completions and observed operations, files, branches and Git state
    // -----------------------------------------------------------------------
    Complete {
        session_id: String,
        lease_id: String,
    },
    Observe {
        session_id: String,
        operation_id: String,
        source: String,
        tool: String,
        paths: Vec<String>,
        result: ToolResult,
        lease_id: Option<String>,
    },
    ObserveTool {
        session_id: String,
        operation: ToolIdentity,
        paths: Vec<String>,
        result: ToolResult,
        lease_id: Option<String>,
    },
    ObserveDisk {
        workspace_id: String,
        path: String,
        before: Option<FileState>,
        after: FileState,
        related_operation_ids: Vec<String>,
    },
    ObserveBranch {
        workspace_id: String,
        branch_ref: String,
        head_oid: Option<String>,
        tree_oid: Option<String>,
    },
    ObserveWorkspaceGit {
        workspace_id: String,
        repository_id: String,
        branch_ref: Option<String>,
    },
    // -----------------------------------------------------------------------
    // Prompts, presence and a session's turns
    // -----------------------------------------------------------------------
    SessionPrompt {
        session_id: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        line_digests: Vec<String>,
    },
    /// Where the definitions the session's prompt named are, from the
    /// source index: replaces the session's earlier prompt targets. Paths
    /// only, at most `MAX_PROMPT_TARGETS`.
    SetPromptTargets {
        session_id: String,
        paths: Vec<String>,
    },
    /// Paths a search or shell command looked at: 1 to `PRESENCE_PATHS`
    /// relative paths, never the pattern or the command. `kind` is `search`
    /// or `shell`; reads arrive as `observe_source`.
    ObservePresence {
        session_id: String,
        kind: PresenceKind,
        paths: Vec<String>,
    },
    /// Whether peers are shown where this session looks, and how old a
    /// peer's presence this session is shown (`window_ms`,
    /// `MIN_PRESENCE_WINDOW_MS` to `BRIEF_HORIZON_MS`).
    SetPresence {
        session_id: String,
        shown: bool,
        window_ms: u64,
    },
    SessionEnd {
        session_id: String,
        /// What the end hook read from the line journal for the trail
        /// (roadmap C9). Absent from an older client: the trail has no line
        /// ranges and no uncommitted files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trail: Option<TrailFacts>,
    },
    /// The session finished a reply: complete what its tool calls still
    /// hold. `failure` is the host's error type (lower-case letters and
    /// underscores) when the reply ended on an error.
    SessionStop {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<String>,
        /// The host's ID of the prompt whose reply finished (issue #214).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_id: Option<String>,
    },
    /// A batch of the session's tool calls has finished.
    ObserveToolBatch {
        session_id: String,
        operation_ids: Vec<String>,
        calls: u32,
    },
    // -----------------------------------------------------------------------
    // Briefings and adoption
    // -----------------------------------------------------------------------
    Brief {
        session_id: String,
        max_peers: u32,
        /// True when the result is being placed in the session's context.
        /// Only then is it remembered as delivered; a look from outside
        /// neither suppresses the session's next briefing nor counts as
        /// activity.
        deliver: bool,
        /// True for a briefing placed in context during a turn, after a
        /// tool call. A handoff waits for a session's start or next prompt.
        #[serde(default, skip_serializing_if = "is_false")]
        mid_turn: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        recent: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recent_window_ms: Option<u64>,
        /// True when the session asked through the MCP tool
        /// `langspan_brief`, so a message it carries is recorded as
        /// delivered that way (roadmap E12).
        #[serde(default, skip_serializing_if = "is_false")]
        mcp: bool,
        #[serde(default)]
        left: Vec<String>,
        #[serde(default)]
        changes_only: bool,
    },
    Adopt {
        session_id: String,
        from: String,
    },
    // -----------------------------------------------------------------------
    // Reads of the state and the event log, the version and retention
    // -----------------------------------------------------------------------
    Status {},
    StatusRecords {
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        page_bytes: Option<u32>,
    },
    Dashboard {},
    Version {},
    Events {
        after: u64,
        limit: u32,
    },
    Changes {
        workspace_id: String,
        after: u64,
        limit: u32,
    },
    Compact {
        keep_days: u32,
        #[serde(default)]
        apply: bool,
    },
}

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        CommandWire::deserialize(deserializer)
    }
}
