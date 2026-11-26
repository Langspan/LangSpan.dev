//! Pure coordination decisions and event replay. Time and identifiers are supplied
//! by the caller; this crate performs no I/O and cannot fence filesystem writers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

mod anchors;
mod classes;
mod count;
mod dashboard;
mod flaky;
mod history;
mod lines;
mod policy;
mod rank;
mod recall;
mod recent;
mod refusal_outcome;
mod report;
mod status;
mod status_line;
mod today;
mod trail;
mod workspaces;
mod write_check;
pub use anchors::{
    ANCHOR_CANDIDATES, AnchorIndex, AnchorLookup, AnchorState, MAX_TAGS, NoteAnchor, SYMBOL_BYTES,
    TAG_BYTES, anchored, remember_lookups, validate_tag, validate_tags,
};
pub use classes::{
    FailureClass, KeptTests, MAX_FIXED_NAMED, MAX_WRITE_RUNS, MAX_WRITERS_NAMED, WriteRun,
};
pub use count::{
    COUNT_DAYS, COUNT_KINDS, COUNT_REFUSALS, COUNT_WORKSPACES, CountQuery, RefusalCount,
    count_refusals, validate_count_days,
};
pub use dashboard::{
    AgentRow, AgentState, ChangeRow, CheckRow, DASHBOARD_AGENTS, DASHBOARD_ITEMS, DASHBOARD_PATHS,
    DASHBOARD_TEXT, DENIAL_WINDOW, Dashboard, DenialRecord, DenialRow, Denials, EditRow,
    LandingRow, MessageRow, NoteRow, NoticeRow, OverlapRow, PublishRow, RecentFile, ScanRow,
    WorkspaceRow, dashboard_denials,
};
pub use flaky::{
    FLAKY_DECAY_RUNS, FLAKY_MIN_FLIPS, FLAKY_WINDOW_RUNS, FlakyCount, FlakyTest, LastTests,
    MAX_FLAKY_LISTED, MAX_LEARNED_CHECKS, MAX_LEARNED_TESTS, MAX_TEST_NAME_BYTES, MAX_TEST_NAMES,
    MAX_TEST_RESULT_BYTES, TestFailure, TestHistory, TestLearning, TestResults,
};
pub use history::{
    HISTORY_ATTRIBUTION, HISTORY_EVENTS, HISTORY_LIMIT, HISTORY_MOVES, HISTORY_NAME_BYTES,
    HISTORY_RELATED, HISTORY_RELATED_MS, History, HistoryEntry, HistoryMessage, HistoryNote,
    HistoryQuery, history as definition_history,
};
pub use policy::{
    DEFAULT_WAIT_MS, MAX_UNSETTLED, MAX_WAIT_MS, Policy, PolicyCase, PolicyRow, SettledOutcome,
    UnsettledRefusal, WaitResult,
};
pub use rank::{
    Dependent, QUALIFIER_BYTES, RANK_CANDIDATES, RankInputs, RankReason, is_qualifier,
    rank_definitions, rank_dependents, rank_inputs, validate_rank,
};
pub use recall::{
    RECALL_LIMIT, RECALL_QUERY_BYTES, RECALL_WORDS, Recall, RecalledNote, recall as recall_notes,
};
pub use recent::{
    ChangedPath, ContestedPath, LeftUncommitted, MAX_PATH_REFUSALS, MAX_RECENT_WINDOW_MS,
    MIN_RECENT_WINDOW_MS, PathRefusal, RECENT_BYTES, RECENT_CHANGED, RECENT_CONTESTED,
    RECENT_INTERFACES, RECENT_LEFT, RECENT_LEFT_CANDIDATES, RECENT_NOTICE_CHANGES,
    RECENT_WINDOW_MS, RecentChanges, RecentInterfaceChange,
};
pub use refusal_outcome::{PENDING, REFUSAL_WINDOW_MS, RefusalOutcome};
pub use report::{
    OTHER_HOST, REPORT_HISTORY_EVENTS, REPORT_SESSIONS, Report, ReportCounts, ReportHost,
    ReportKnown, ReportQuery, ReportSession, host_kind, known_sessions, open_intents, report,
    session_workspaces,
};
pub use status::{
    STATUS_RECENT_MS, StatusPosition, StatusRecord, StatusResume, StatusSummary, restore_status,
    shrink_status, status_records, status_view,
};
pub use status_line::{
    ClockAnchor, MAX_REFUSALS, REFUSAL_PATHS, REFUSAL_RETENTION_MS, RecentRefusal, RefusalKind,
    StatusLine, status_line,
};
pub use today::{
    OpenAdmission, OpenCheck, OpenIntent, OpenMessage, TODAY_EVENTS, TODAY_FILES, TODAY_OPEN,
    TODAY_SESSIONS, Today, TodayChecks, TodayLandings, TodayOpen, TodayQuery, TodaySession,
    TodayWindow, today,
};
pub use trail::{
    MAX_TRAIL_FACTS, MAX_TRAILS, TRAIL_BYTES, TRAIL_CHECKS, TRAIL_FILES, TRAIL_JSON_BYTES,
    TRAIL_RANGES, TRAIL_WAITING, Trail, TrailCheck, TrailFacts, TrailFile, TrailView,
};
pub use workspaces::{
    MAX_ENDING_SESSIONS, MAX_WORKSPACE_PAGE, WorkspaceEnding, WorkspaceKept, WorkspaceSummary,
};
pub use write_check::{
    DefinitionFacts, IndexFacts, PathFacts, UnreadLines, WRITE_CHECK_DEFINITIONS,
    WRITE_CHECK_LABEL, WRITE_CHECK_PATHS, WRITE_CHECK_SYMBOLS, WRITE_CHECK_WRITERS, WriteCheck,
    WriteCheckPath, WriteCheckRequest, WriteCheckSymbol, write_check, write_check_noticed,
    write_check_request,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub workspace_id: String,
    pub root: String,
    /// Existing workspace IDs whose distinct nested worktree boundary was
    /// verified by the filesystem adapter when this workspace was registered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified_overlaps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub session_id: String,
    pub workspace_id: String,
    pub generation: u64,
    /// The session that started this one, for a host subagent: it shares the
    /// parent's conversation, so the two are related, not strangers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Started without a person at the terminal (`claude -p`). A headless
    /// session takes a handoff only under the rule in `takes_handoff`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub headless: bool,
}

/// An OS process incarnation, not merely a recyclable process number. The
/// adapter's start token includes both OS boot and process start identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_token: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LivenessStatus {
    Alive,
    Exited,
    Unknown,
}

/// Point-in-time process evidence. It neither reattaches a session nor changes
/// its admission authority. The timestamp is meaningful within its generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLiveness {
    pub session_id: String,
    pub process: ProcessIdentity,
    pub status: LivenessStatus,
    pub observed_at_ms: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub session_id: String,
    pub workspace_id: String,
    pub paths: Vec<String>,
    pub summary: String,
    pub generation: u64,
}

/// Immutable identity of one native tool call. The adapter hashes canonical
/// semantic input so receipts can reject changed content without storing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolIdentity {
    pub operation_id: String,
    pub source: String,
    pub tool: String,
    pub input_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub lease_id: String,
    pub session_id: String,
    pub workspace_id: String,
    pub paths: Vec<String>,
    pub generation: u64,
    pub expires_at_ms: u64,
    pub status: LeaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<ToolIdentity>,
    /// Admitted paths whose source version could not be compared with what
    /// the session last read, because no read of them was recorded. The
    /// admission is not evidence that the session's view of them is current.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_unknown: Vec<String>,
    /// Where in each file the edit was located. A path with no entry is held
    /// whole, which is also how a lease recorded before regions replays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<PathRegions>,
}

/// Part of one file an edit was located in, by the hook, from the file as it
/// was on disk. Lines are one-based and inclusive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Region {
    /// A definition found by the source parser. `key` is its name and the
    /// names of the definitions enclosing it, outermost first. `digest` is
    /// the SHA-256 of the definition's text with line endings and trailing
    /// whitespace normalized: the symbol's own version. The lines say where it
    /// was, for people; admission compares keys, not lines.
    Symbol {
        key: Vec<String>,
        digest: String,
        start_line: u32,
        end_line: u32,
    },
    /// A line range, for a file whose language is not parsed. Comparable only
    /// with ranges located in the same version of the file.
    Lines { start_line: u32, end_line: u32 },
}

/// The regions of one admitted path and the version of the file (`digest`,
/// SHA-256 of its bytes) they were located in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathRegions {
    pub path: String,
    pub digest: String,
    pub regions: Vec<Region>,
}

/// One-based, inclusive.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    pub start_line: u32,
    pub end_line: u32,
}

/// Lines of `path` that session `session_id` wrote `age_ms` ago and that an
/// edit would change, numbered in the file as it is now; `lines` is empty
/// when they could not be placed in it. The hook finds them in the line
/// journal ([ADR 0028](../../../docs/adr/0028-line-ownership.md)).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WrittenLines {
    pub path: String,
    pub session_id: String,
    pub lines: Vec<LineRange>,
    pub age_ms: u64,
}

/// At most this many written-lines entries in one admission.
pub const MAX_WRITTEN: usize = 64;
/// Line ranges per entry, at most.
pub const MAX_WRITTEN_RANGES: usize = 32;
/// Another session's lines count this long after it wrote them.
pub const LINE_HORIZON_MS: u64 = 30 * 60 * 1000;

/// At most this many regions in one admission, over all its paths.
pub const MAX_REGIONS: usize = 64;
/// No region line beyond this: far past the line count of a 1 MiB file.
pub const MAX_REGION_LINE: u32 = 2_000_000;
/// A symbol key names at most this many nested definitions.
pub const MAX_KEY_DEPTH: usize = 8;
const MAX_KEY_NAME_BYTES: usize = 128;

/// A file version as an adapter saw it on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservedFile {
    pub path: String,
    pub state: FileState,
}

/// How a session came to know a file version.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceOrigin {
    /// The host reported that the session read the file.
    Read,
    /// The host reported that the session's own write succeeded.
    Write,
    /// The session was refused an edit and told the file had changed. It has
    /// been informed of this version; it has not necessarily read it.
    Refused,
    /// The session was about to edit a file it had no recorded version of,
    /// on a host that reports no reads (Codex). This is the file as it was on
    /// disk at that moment, so later edits are compared with it. The session
    /// has not necessarily read it ([ADR 0027](../../../docs/adr/0027-turn-and-batch-events.md)).
    Snapshot,
}

/// The version of a file a session last read or wrote. `at_ms` is zero for a
/// version carried over from an earlier daemon generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnownVersion {
    pub path: String,
    pub state: FileState,
    pub at_ms: u64,
}

/// A file whose version on disk differs from the one the session last read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaleFile {
    pub path: String,
    pub read: FileState,
    pub current: FileState,
    /// The other session in this workspace that most recently reported
    /// editing the path, when there is one. A correlation, not proof.
    pub changed_by: Option<String>,
}

/// A file the asking session read that a peer has since reported editing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaleRead {
    pub path: String,
    pub session_id: String,
    pub age_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStatus {
    Active,
    Completed,
    Expired,
    Invalidated,
}

/// What the host reported about a tool call after it ran.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResult {
    Succeeded,
    Failed,
    Interrupted,
}

/// How an observation relates to the admission it names, judged when the
/// observation is recorded. `Expired` and `Invalidated` are late observations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseRelation {
    Current,
    Completed,
    Expired,
    Invalidated,
    NotAdmitted,
    /// The reported paths include a resource that the named admission did not
    /// cover. This classification takes precedence over the lease's age.
    ScopeMismatch,
    /// The reported tool identity does not match the named admission. A legacy
    /// observation cannot bypass the input check on a typed admission.
    InputMismatch,
}

/// Evidence of a reported tool result. It is never authority: recording it does
/// not grant, renew, complete or reactivate any admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub observation_id: String,
    pub session_id: String,
    pub workspace_id: String,
    pub operation_id: String,
    pub source: String,
    pub tool: String,
    pub paths: Vec<String>,
    pub result: ToolResult,
    pub lease_id: Option<String>,
    pub lease_relation: LeaseRelation,
    pub concurrent_holders: Vec<Conflict>,
    pub observed_at_ms: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    #[default]
    Regular,
    Symlink,
}

impl FileKind {
    fn is_regular(&self) -> bool {
        *self == Self::Regular
    }
}

/// A successfully observed file state. Read failures are not `Missing`; adapters
/// report them as incomplete scans without inventing a deletion observation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileState {
    Missing,
    Present {
        digest: String,
        #[serde(default, skip_serializing_if = "FileKind::is_regular")]
        kind: FileKind,
    },
}

// As with Command::Status, use an empty struct for strict unit-variant decoding.
#[derive(Deserialize)]
#[serde(
    remote = "FileState",
    tag = "type",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum FileStateWire {
    Missing {},
    Present {
        digest: String,
        #[serde(default)]
        kind: FileKind,
    },
}

impl<'de> Deserialize<'de> for FileState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        FileStateWire::deserialize(deserializer)
    }
}

/// One disk fact. `before: None` means no prior baseline, not a created file.
/// Related operation IDs are unverified correlations, never causal attribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiskObservation {
    pub observation_id: String,
    pub workspace_id: String,
    pub path: String,
    pub before: Option<FileState>,
    pub after: FileState,
    pub related_operation_ids: Vec<String>,
    pub observed_at_ms: u64,
    pub generation: u64,
}

/// A local ref fact. Both OIDs are absent for an unborn HEAD or missing branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchObservation {
    pub observation_id: String,
    pub workspace_id: String,
    pub branch_ref: String,
    pub head_oid: Option<String>,
    pub tree_oid: Option<String>,
    pub observed_at_ms: u64,
    pub generation: u64,
}

/// Bounded metadata for the store's most recent completed workspace scan. A
/// missing summary means no baseline, distinct from a scan with zero files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScanSummary {
    pub workspace_id: String,
    pub observed_at_ms: u64,
    pub generation: u64,
    pub file_count: u64,
    pub branch_count: u64,
    pub head_oid: Option<String>,
    pub branch_ref: Option<String>,
    /// What the scan did not version. Absent when it versioned every file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<ScanCoverage>,
}

/// Paths listed in a workspace that its last completed scan did not
/// version, by reason. A scan is partial when `omitted` or `too_large` is
/// above zero: some working files of the checkout itself were not read.
/// Submodules and skip-worktree entries are outside the working tree the
/// scan reads, by design; they are counted, never read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ScanCoverage {
    /// Left out because the file-count, byte or time bound was reached.
    /// Files Git reports as changed are versioned before any clean file.
    pub omitted: u64,
    /// Over the per-file bound: listed, not versioned.
    pub too_large: u64,
    /// The first `COVERAGE_PATHS` of them, in path order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub too_large_paths: Vec<String>,
    /// Gitlinks: recorded as present, never read into.
    pub submodules: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub submodule_paths: Vec<String>,
    /// Entries outside a sparse checkout: not in the working tree, and not
    /// reported as deleted.
    pub skip_worktree: u64,
}

/// Paths a coverage record names per reason, at most.
pub const COVERAGE_PATHS: usize = 8;

impl ScanCoverage {
    /// True when some working files of the checkout were not versioned.
    pub fn is_partial(&self) -> bool {
        self.omitted > 0 || self.too_large > 0
    }
}

/// The repository and branch a physical workspace was last observed on. The
/// repository identity groups linked worktrees; it never merges their
/// admission domains.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGit {
    pub workspace_id: String,
    pub repository_id: String,
    pub branch_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TouchedPath {
    pub path: String,
    pub at_ms: u64,
}

/// What one session was last seen doing. Derived from recorded events only;
/// times are meaningful within `generation`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionActivity {
    pub session_id: String,
    pub generation: u64,
    pub last_ms: u64,
    /// The last recorded hook run of any kind that reached the daemon for
    /// this session (a read, a failed call, a batch, a stop): at most one
    /// `SessionSeen` per `SESSION_SEEN_INTERVAL_MS`. Zero when none was.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub last_hook_ms: u64,
    /// The owner's own words for the current task, capped by the adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Most recent successful edits, newest first, at most `MAX_TOUCHED`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched: Vec<TouchedPath>,
    /// File versions this session last read or wrote, newest first, at most
    /// `MAX_KNOWN`. A path that falls off the end is unknown again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known: Vec<KnownVersion>,
    /// Signature of the last briefing delivered to this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub briefed: Option<String>,
    /// Keys of the briefing items this session was last told, in briefing
    /// order, at most `MAX_TOLD` (issue #222). A briefing asked for with
    /// `changes_only` leaves out an item whose key is here. An item past
    /// the bound is not remembered, so it is told again: never dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub told: Vec<u64>,
    /// When a briefing last placed items in this session's context: a
    /// safety item is told again when the session read or edited one of
    /// its files after this.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub told_at_ms: u64,
    /// The context LangSpan's hooks added to this session in this
    /// generation, as they measured it (issue #222).
    #[serde(default, skip_serializing_if = "ContextUse::is_empty")]
    pub context: ContextUse,
    /// Interface notices this session was alerted to at a write, newest
    /// first, at most `MAX_ALERTED`. Each is alerted once (ADR 0029).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alerted: Vec<String>,
    /// Interface notices told to this session by a briefing or between
    /// steps, newest first, at most `MAX_TOLD_NOTICES` (issue #214).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices_told: Vec<ToldNotice>,
    /// Replies the session finished in this generation: a between-step
    /// watcher started in one turn stops once this moves (issue #214).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub stops: u64,
    /// The host's IDs of the prompts whose replies finished in this
    /// generation, newest first, at most `MAX_STOPPED_PROMPTS`: a watcher
    /// started for one of them has no turn left (issue #214).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped_prompts: Vec<String>,
    /// In-flight signatures told to this session, newest first, at most
    /// `MAX_INFLIGHT_TOLD` (roadmap C7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inflight_told: Vec<String>,
    /// The host reported the session over. Its recent edits stay relevant to
    /// peers until they age out; they are labelled as a finished session's.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ended: bool,
    /// The session wrote a file since its last tool batch or stop: the next
    /// one is a point to run the workspace's automatic check.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wrote_since_boundary: bool,
    /// Where the definitions named in the session's last prompt are, from
    /// the source index (roadmap C8). Paths only; never the prompt's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_targets: Option<PromptTargets>,
    /// Where the session was last seen looking: files it read, the path
    /// scope of its searches, and paths its shell commands named. Newest
    /// first, one entry per path, at most `MAX_LOOKING` (roadmap A13). Paths
    /// only: never a search pattern or a command's text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub looking: Vec<LookedPath>,
    /// Digests of the leading non-empty lines of the session's last prompt,
    /// at most `MAX_PROMPT_LINES`: compared with live peers' to tell prompts
    /// started from one template apart (issue #114). Never the words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_digests: Vec<String>,
}

/// How a session came to look at a path, for presence (roadmap A13).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PresenceKind {
    /// The host reported a completed read of the file.
    Read,
    /// A search (Claude Code's or Cursor's `Grep`) scoped to this path.
    Search,
    /// A shell command named this existing path as an operand.
    Shell,
}

/// One path a session looked at, and when.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LookedPath {
    pub path: String,
    pub kind: PresenceKind,
    pub at_ms: u64,
}

/// One path a peer was looking at, as a briefing shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PresencePath {
    pub path: String,
    pub kind: PresenceKind,
    pub age_s: u64,
}

/// A session's own presence setting, kept only when it differs from the
/// default (shown, `PRESENCE_WINDOW_MS`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PresenceSetting {
    /// False: this session's reads, searches and shell paths are never
    /// shown to peers (`--presence off`).
    pub shown: bool,
    /// How old a peer's presence this session is still shown.
    pub window_ms: u64,
}

/// Looked-at paths kept per session.
pub const MAX_LOOKING: usize = 16;
/// Looked-at paths shown per peer, and named by one presence observation.
pub const PRESENCE_PATHS: usize = 4;
/// Presence older than this is not shown, unless the asking session chose
/// another window (at most `BRIEF_HORIZON_MS`).
pub const PRESENCE_WINDOW_MS: u64 = 10 * 60 * 1000;
/// The shortest presence window a session can choose.
pub const MIN_PRESENCE_WINDOW_MS: u64 = 60 * 1000;

/// Briefing item keys remembered per session, at most.
pub const MAX_TOLD: usize = 256;

/// The context a session's hooks added to it: every briefing, code section
/// and mid-turn note, in bytes of text placed in its context.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextUse {
    /// Turns: a session start or resume, and each prompt.
    pub turns: u64,
    /// Bytes added over those turns, mid-turn notes included.
    pub bytes: u64,
    /// The most one turn added, its own context and the notes after it.
    #[serde(default)]
    pub largest_turn_bytes: u64,
    /// Bytes added since the current turn began.
    #[serde(default)]
    pub turn_bytes: u64,
}

/// Bytes of text per token, for an estimate only: English prose and code
/// average about four bytes per token in current models' tokenizers.
pub const BYTES_PER_TOKEN: u64 = 4;

/// The most one context record may name: far above any hook's cap.
pub const MAX_CONTEXT_RECORD_BYTES: u64 = 1024 * 1024;

impl ContextUse {
    pub fn is_empty(&self) -> bool {
        self.turns == 0 && self.bytes == 0
    }
    /// Count `bytes` added, at the start of a new turn when `turn`.
    fn add(&mut self, bytes: u64, turn: bool) {
        if turn {
            self.turns = self.turns.saturating_add(1);
            self.turn_bytes = 0;
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.turn_bytes = self.turn_bytes.saturating_add(bytes);
        self.largest_turn_bytes = self.largest_turn_bytes.max(self.turn_bytes);
    }
    /// Bytes per turn on average, rounded down; zero before the first turn.
    pub fn bytes_per_turn(&self) -> u64 {
        self.bytes.checked_div(self.turns).unwrap_or(0)
    }
    /// An estimate of `bytes` in tokens (`BYTES_PER_TOKEN`), rounded up.
    pub fn estimated_tokens(bytes: u64) -> u64 {
        bytes.div_ceil(BYTES_PER_TOKEN)
    }
}

impl SessionActivity {
    /// When the session was last known to be working: its newest edit,
    /// prompt or recorded hook run. Whether it is current compares this, not
    /// `last_ms` alone, with `BRIEF_HORIZON_MS` (roadmap J12).
    pub fn last_seen_ms(&self) -> u64 {
        self.last_ms.max(self.last_hook_ms)
    }
}

/// A hook run is recorded as `SessionSeen` at most this often per session,
/// so a burst of tool calls writes one event, not one per call.
pub const SESSION_SEEN_INTERVAL_MS: u64 = 30 * 1000;

/// The session a request is made on behalf of, when it is that session at
/// work: every request a hook run sends for it, and the same requests from
/// the CLI or MCP. A look from outside (`Brief` without `deliver`, a status
/// read) and the end of a session are not. The daemon records a
/// `SessionSeen` after each such request (roadmap J12).
pub fn acting_session(command: &Command) -> Option<&str> {
    match command {
        Command::SessionRegister { session_id, .. }
        | Command::ObserveSessionLiveness { session_id, .. }
        | Command::IntentSet { session_id, .. }
        | Command::IntentEnd { session_id }
        | Command::Admit { session_id, .. }
        | Command::AdmitTool { session_id, .. }
        | Command::ObserveSource { session_id, .. }
        | Command::ObservePresence { session_id, .. }
        | Command::SetPresence { session_id, .. }
        | Command::AckMessage { session_id, .. }
        | Command::Handoff { session_id, .. }
        | Command::RecordRefusal { session_id, .. }
        | Command::RecordWait { session_id, .. }
        | Command::CheckCalls { session_id, .. }
        | Command::Complete { session_id, .. }
        | Command::Observe { session_id, .. }
        | Command::ObserveTool { session_id, .. }
        | Command::SessionPrompt { session_id, .. }
        | Command::SetPromptTargets { session_id, .. }
        | Command::SessionStop { session_id, .. }
        | Command::ObserveToolBatch { session_id, .. }
        | Command::Adopt { session_id, .. }
        | Command::Brief {
            session_id,
            deliver: true,
            ..
        } => Some(session_id),
        _ => None,
    }
}

/// Whether a hook run of `session_id` at `now_ms` is recorded: the session
/// is current and its last record is at least `SESSION_SEEN_INTERVAL_MS`
/// old, or there is none in this generation.
pub fn session_seen_due(state: &State, session_id: &str, now_ms: u64) -> bool {
    if current_session(state, session_id).is_err() {
        return false;
    }
    let last = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .map_or(0, |activity| activity.last_hook_ms);
    last == 0 || now_ms.saturating_sub(last) >= SESSION_SEEN_INTERVAL_MS
}

/// At most this many leading prompt lines are compared by digest.
pub const MAX_PROMPT_LINES: usize = 32;
/// Hex characters in one prompt line digest, at most.
pub const MAX_PROMPT_LINE_DIGEST: usize = 64;

/// The definition paths a prompt's code briefing found, and when. Compared
/// with other sessions' targets and work while within `BRIEF_HORIZON_MS`;
/// never a hold, an admission or a publication.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromptTargets {
    pub paths: Vec<String>,
    pub at_ms: u64,
}

/// At most this many prompt target paths: every definition a code briefing
/// can find, `CONTEXT_DEFINITIONS` for each of `CONTEXT_NAMES` names.
pub const MAX_PROMPT_TARGETS: usize = CONTEXT_NAMES * CONTEXT_DEFINITIONS as usize;

fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

fn one() -> u32 {
    1
}

fn default_path_hops() -> u32 {
    DEFAULT_PATH_HOPS
}

fn is_one(value: &u32) -> bool {
    *value == 1
}

/// The signature of a briefing with nothing in it.
const EMPTY_SIGNATURE: &str = "cbf29ce484222325";
/// A briefing outcome stays well inside one 64 KiB transport frame.
const BRIEF_BYTES: usize = 40 * 1024;

/// FNV-1a, 64 bits. It only tells one briefing from the next for the same
/// session; a collision would withhold one repeat briefing and nothing else.
fn fingerprint(text: &str) -> String {
    format!("{:016x}", fingerprint_u64(text))
}

fn fingerprint_u64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub const MAX_TOUCHED: usize = 32;
pub const MAX_KNOWN: usize = 128;
pub const MAX_BRIEF_PEERS: u32 = 16;
pub const BRIEF_PATHS: usize = 8;
/// Activity older than this, within one generation, is left out of briefings.
pub const BRIEF_HORIZON_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecentPath {
    pub path: String,
    pub age_ms: u64,
}

/// One other session's relevant work, as shown to the asking session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PeerWork {
    pub session_id: String,
    pub workspace_id: String,
    pub workspace_root: String,
    /// False for a linked worktree of the same repository: overlap there is
    /// logical, and no admission in one blocks the other.
    pub same_workspace: bool,
    pub branch_ref: Option<String>,
    pub liveness: Option<LivenessStatus>,
    /// The host reported this session over; `recent` is what it left behind.
    pub ended: bool,
    pub summary: Option<String>,
    pub intent: Option<String>,
    pub editing: Vec<String>,
    pub recent: Vec<RecentPath>,
    /// Paths this peer shares with the asking session's own work. The
    /// asking session's prompt targets count as its work here.
    pub overlap: Vec<String>,
    /// Paths this peer's last prompt points at (its prompt targets) that the
    /// asking session works on or its own prompt points at, and that are not
    /// already in `overlap`. Advisory: it never holds a write (roadmap C8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_overlap: Vec<String>,
    /// How this peer is related to the asking session through a host
    /// subagent; absent for an unrelated session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<PeerRelation>,
    /// Where this peer was recently looking (presence, roadmap A13): at most
    /// `PRESENCE_PATHS`, newest first, none it is editing or edited, within
    /// the asking session's presence window. Advisory: it never holds,
    /// refuses or delays anything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub looking: Vec<PresencePath>,
}

/// A peer that belongs to the same host conversation as the asking session.
/// A subagent's own parent is never a peer in its briefing (ADR 0004).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeerRelation {
    /// The session that started the asking subagent. No new briefing names
    /// it; it stays so briefing receipts stored before that change still read.
    Parent,
    /// A subagent the asking session started.
    Subagent,
    /// Another subagent of the asking subagent's parent.
    Sibling,
}

/// How `peer` is related to `me` through a host subagent, if at all.
fn relation(me: &Session, peer: &Session) -> Option<PeerRelation> {
    if peer.parent.as_deref() == Some(me.session_id.as_str()) {
        Some(PeerRelation::Subagent)
    } else if me.parent.is_some() && me.parent == peer.parent {
        Some(PeerRelation::Sibling)
    } else {
        None
    }
}

/// One session's work as its owner's daemon publishes it to teammates.
/// Paths are repository-relative. Everything here crosses a machine boundary
/// and is validated as untrusted input on receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteWork {
    pub host: String,
    /// The first characters of the host session ID: enough to tell two apart.
    pub session: String,
    pub branch_ref: Option<String>,
    pub summary: Option<String>,
    pub editing: Vec<String>,
    pub recent: Vec<String>,
}

/// What one participant publishes for one repository. `sequence` rises by
/// one with each publication and is the only ordering used across machines.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    pub participant: String,
    pub name: String,
    pub sequence: u64,
    pub work: Vec<RemoteWork>,
    /// The sender stopped publishing but its host refused to delete its ref,
    /// so this final publication says so in its place. It lists no work, and
    /// a receiver treats the sender as gone.
    #[serde(default, skip_serializing_if = "is_false")]
    pub withdrawn: bool,
}

/// The largest publication payload, in bytes, that is pushed or recorded.
pub const PUBLICATION_BYTES: usize = 16 * 1024;

/// One publication that left this machine, as recorded when it was pushed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SentPublication {
    /// Position in the local event log.
    pub seq: u64,
    pub sequence: u64,
    pub published_at_ms: Option<u64>,
    /// The exact bytes pushed, or `None` for a record from before payloads
    /// were kept.
    pub payload: Option<String>,
}

/// The retention windows a compaction accepts, in days, inclusive. The lower bound
/// stays well above any client's retry time, since a retry whose receipt
/// was removed is a new request.
pub const MIN_RETENTION_DAYS: u32 = 7;
pub const MAX_RETENTION_DAYS: u32 = 3650;

/// One compaction of the event log (ADR 0025, J7). Times are wall-clock
/// Unix milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactionReport {
    /// False for a dry run, which deleted nothing.
    pub applied: bool,
    pub keep_days: u32,
    /// Records written before this time are older than the window.
    pub cutoff_ms: u64,
    /// The sequence the derived state is snapshotted at: events at or below
    /// it are no longer replayed at start. `None` when none is kept.
    pub snapshot_seq: Option<u64>,
    /// Events removed (or to be removed), and their sequence range.
    pub events: u64,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub receipts: u64,
    /// Bytes of the removed records. SQLite reuses the space for new records
    /// rather than shrinking the file.
    pub bytes: u64,
    /// Why the snapshot stops before the newest event older than the
    /// window: something still open was started there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<String>,
}

/// The owner's standing choice to publish a workspace's work to a Git remote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteLink {
    pub workspace_id: String,
    pub remote: String,
    pub participant: String,
    pub name: String,
    /// Whether task summaries leave the machine. Off unless the owner asks.
    pub share_task: bool,
    /// Sequence and content fingerprint of the last recorded publication.
    /// Kept when publication is disabled, so that enabling it again never
    /// publishes a sequence teammates have already seen.
    pub sequence: u64,
    pub signature: String,
    /// False once the owner disabled publication.
    pub enabled: bool,
    /// True once a disabled workspace's ref is known to be gone from the
    /// remote. Until then the daemon keeps trying to remove it.
    pub withdrawn: bool,
    /// The owner chose to publish under `refs/heads/langspan/peers/`, for
    /// hosts that accept pushes only to branches. Absent in links recorded
    /// before the choice existed, which publish under `refs/langspan/peers/`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub publish_as_branch: bool,
}

/// A teammate's latest publication as this daemon received it. Its age is
/// measured from local receipt, never from the sender's clock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemotePeer {
    pub workspace_id: String,
    pub publication: Publication,
    pub received_ms: u64,
    pub generation: u64,
}

/// A teammate's session on another machine, as shown to the asking session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemotePeerWork {
    pub name: String,
    pub work: RemoteWork,
    pub overlap: Vec<String>,
    /// Time since this daemon last received a new publication from them.
    pub received_age_ms: u64,
}

/// What a check command reported, bound to the inputs it ran on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckResult {
    Passed {},
    Failed {
        exit_code: i32,
    },
    /// The inputs changed while the command ran, or it did not finish. It
    /// says nothing about any tree.
    Inconclusive {},
}

/// Evidence that one command gave one result on one exact tree. `tree_oid`
/// is the Git tree of everything the command could read. With `inputs`
/// empty that is a working tree as it stood; otherwise it is the merge of
/// the listed commits, which no working tree may ever have held.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckRecord {
    pub check_id: String,
    pub workspace_id: String,
    pub name: String,
    pub command: String,
    pub tree_oid: String,
    pub head_oid: Option<String>,
    pub inputs: Vec<String>,
    pub result: CheckResult,
    pub duration_ms: u64,
    pub recorded_ms: u64,
    pub generation: u64,
    /// A file in the workspace was observed to change after this record.
    /// Set only for working-tree checks; never cleared.
    #[serde(default, skip_serializing_if = "is_false")]
    pub changed_since: bool,
    /// For a combined check asked for them: definitions the combined
    /// branches changed in ways that disagree. Advisory; they never change
    /// `result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_findings: Option<Box<SymbolFindings>>,
}

/// The most definition findings one combined check keeps.
pub const MAX_SYMBOL_FINDINGS: usize = 32;
/// The most bytes of a finding's name, branch, path or signature.
pub const MAX_SYMBOL_FINDING_TEXT: usize = 256;
/// The longest note on why findings are missing or incomplete.
pub const MAX_SYMBOL_FINDINGS_NOTE: usize = 512;

/// What comparing the combined branches' definitions found (ADR 0020, D9):
/// name matches across the files each branch changed, never a resolved
/// reference. Repository text, data only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolFindings {
    /// At most `MAX_SYMBOL_FINDINGS`, in branch, path and line order.
    pub findings: Vec<SymbolFinding>,
    /// Findings found beyond those kept.
    pub more: u32,
    /// Changed files in a supported language that were indexed.
    pub files_indexed: u32,
    /// Changed files in a supported language that were not: larger than
    /// the parser's bound, or not text.
    pub files_skipped: u32,
    /// Why findings were skipped, when they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SymbolFindingKind {
    /// Two branches changed one definition's signature differently.
    BothChanged,
    /// One branch changed a signature; another added a call whose argument
    /// count fits the old signature and not the new one.
    ChangedAndCalled,
    /// One branch removed or renamed an exported definition; another added
    /// a line that mentions its name.
    RemovedAndUsed,
}

/// One disagreement between two combined branches. `branch`, `path` and
/// `line` are where the first branch has the definition (where it was, for
/// a removal); `other_*` where the second branch changed it, calls it or
/// mentions it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolFinding {
    pub finding: SymbolFindingKind,
    pub name: String,
    pub branch: String,
    pub path: String,
    pub line: u32,
    /// The signature where the branches started.
    pub before: String,
    /// The first branch's signature; unset for a removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// For a rename, the new name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_to: Option<String>,
    pub other_branch: String,
    pub other_path: String,
    pub other_line: u32,
    /// For `both_changed`, the second branch's signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_after: Option<String>,
    /// For `changed_and_called`, the call's argument count, and how many
    /// the first branch's signature takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takes: Option<Arity>,
}

fn validate_symbol_findings(check: &CheckRecord) -> Result<(), CoreError> {
    let Some(found) = &check.symbol_findings else {
        return Ok(());
    };
    let text = |value: &str| {
        !value.is_empty()
            && value.len() <= MAX_SYMBOL_FINDING_TEXT
            && !value.chars().any(char::is_control)
    };
    let valid = !check.inputs.is_empty()
        && found.findings.len() <= MAX_SYMBOL_FINDINGS
        && found.note.as_ref().is_none_or(|note| {
            note.len() <= MAX_SYMBOL_FINDINGS_NOTE && !note.chars().any(char::is_control)
        })
        && found.findings.iter().all(|finding| {
            [
                &finding.name,
                &finding.branch,
                &finding.path,
                &finding.before,
                &finding.other_branch,
                &finding.other_path,
            ]
            .into_iter()
            .chain(&finding.after)
            .chain(&finding.renamed_to)
            .chain(&finding.other_after)
            .all(|value| text(value))
                && finding.line > 0
                && finding.other_line > 0
                && finding.args.is_none_or(|args| args <= MAX_ARITY)
        });
    if valid {
        Ok(())
    } else {
        Err(error(
            "invalid_check",
            &format!(
                "Definition findings belong to a combined check: at most {MAX_SYMBOL_FINDINGS}, each with one-line texts of 1 to {MAX_SYMBOL_FINDING_TEXT} bytes and lines from 1"
            ),
        ))
    }
}

/// Whether a record says anything about the tree being asked about.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Applicability {
    /// The record's tree is the tree asked about.
    Current,
    /// The tree asked about differs, or the workspace changed since.
    Stale,
    /// No tree was supplied and no change was observed: not compared.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckView {
    pub record: CheckRecord,
    pub applicability: Applicability,
}

/// The check the owner chose to run automatically after a session's writes
/// in one workspace ([ADR 0030](../../../docs/adr/0030-automatic-checks-after-write-batches.md)).
/// `argv` is run without a shell, in the workspace root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoCheck {
    pub workspace_id: String,
    pub name: String,
    pub argv: Vec<String>,
    pub timeout_s: u32,
    /// The wider check run after this one, on the same tree, when the
    /// session's writes changed an indexed export that other files mention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_interface_change: Option<WiderCheck>,
}

/// The owner's second, wider command (for example the whole workspace's
/// tests), run whole: LangSpan never narrows it to the files the index names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WiderCheck {
    pub name: String,
    pub argv: Vec<String>,
    pub timeout_s: u32,
}

/// What an interface change did to the wider check.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceGrade {
    /// The wider check passed.
    Kept,
    /// The wider check failed.
    Broke,
}

/// The interface change a wider check ran for: one changed export, and the
/// files that mention its name, from the index. A name lookup, not resolved
/// uses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InterfaceCause {
    /// The file whose export changed.
    pub path: String,
    pub change: InterfaceChange,
    /// Other changed exports the same run was for.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_changes: u32,
    /// At most [`MAX_CAUSE_DEPENDENTS`] files that mention a changed name.
    pub dependents: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_dependents: u32,
    /// Supported-language files the index could not read in full when the
    /// change was reported: a mention in one of them is missing here.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blind_files: u32,
}

/// Mentioning files a wider check's report names at most.
pub const MAX_CAUSE_DEPENDENTS: usize = 8;

/// Bounds of an automatic check's command and of what is kept of its output.
pub const MAX_AUTO_CHECK_ARGS: usize = 64;
pub const MAX_AUTO_CHECK_ARG_BYTES: usize = 1024;
pub const MAX_AUTO_CHECK_TIMEOUT_S: u32 = 3600;
pub const MAX_CHECK_TAIL_BYTES: usize = 2048;
/// Bytes in a check record's one-line command.
pub const MAX_CHECK_COMMAND_BYTES: usize = 512;
/// Bytes in a session's prompt summary.
pub const MAX_PROMPT_SUMMARY_BYTES: usize = 512;
/// Bytes in an intent's summary.
pub const MAX_INTENT_SUMMARY_BYTES: usize = 2048;
pub const MAX_CHECK_REPORT_SESSIONS: usize = 16;
/// Undelivered results kept per session.
pub const MAX_CHECK_NOTICES: usize = 4;

/// Who an automatic check's result is for, and the end of its output. Sent
/// with the record itself, so the evidence and the notice are one decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckReport {
    /// The sessions whose writes the check ran after.
    pub session_ids: Vec<String>,
    /// The last lines of the command's output, already redacted and bounded
    /// by the daemon. The command's own text: data, never instructions.
    pub tail: String,
    /// The command was stopped at its time limit, of this many seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timed_out_s: Option<u32>,
    /// For a wider check: the interface change it ran for. The core grades
    /// it from the result and tells the sessions working on the mentioning
    /// files too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<Box<InterfaceCause>>,
    /// A wider check is configured, but the index had no complete scan, so
    /// no interface change could be found and the wider check did not run.
    #[serde(default, skip_serializing_if = "is_false")]
    pub index_incomplete: bool,
    /// For a wider check: every file that mentions a changed name, at most
    /// [`MAX_REPORT_DEPENDENT_PATHS`]. The sessions working on any of them
    /// are told, even past the [`MAX_CAUSE_DEPENDENTS`] the notice shows.
    /// Used to choose recipients and not kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependent_paths: Vec<String>,
}

/// Mentioning files a wider check's report may name to choose its
/// recipients: the daemon's 16 waiting interface notices, each with at
/// most [`NOTICE_DEPENDENTS`] files.
pub const MAX_REPORT_DEPENDENT_PATHS: usize = 16 * NOTICE_DEPENDENTS;

/// An automatic check's result waiting to be placed in one session's
/// context, once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckNotice {
    pub check_id: String,
    pub name: String,
    pub tree_oid: String,
    pub result: CheckResult,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timed_out_s: Option<u32>,
    pub tail: String,
    /// The failing tests its output named, each labelled when it is known
    /// to flip on unchanged code. Empty when no test was named.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<TestFailure>,
    /// Its output named more failing tests than `failures` holds.
    #[serde(default, skip_serializing_if = "is_false")]
    pub more_failures: bool,
    /// The record its failures were compared with: the session's baseline
    /// (issue #69). `None` when nothing was compared, and then no failure
    /// has a class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_check_id: Option<String>,
    /// Tests that failed in the baseline and pass now, at most
    /// `MAX_FIXED_NAMED`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed: Vec<String>,
    /// Other sessions that wrote between the baseline's tree and this one,
    /// at most `MAX_WRITERS_NAMED`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writers: Vec<String>,
    /// A wider check's grade: `kept` when it passed, `broke` when it failed,
    /// absent when it was inconclusive or is no wider check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_grade: Option<InterfaceGrade>,
    /// The interface change a wider check ran for. Boxed: it holds a whole
    /// `InterfaceChange`, and most notices have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<Box<InterfaceCause>>,
    /// See [`CheckReport::index_incomplete`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub index_incomplete: bool,
}

/// A push of an integrated result that LangSpan made at the owner's command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Landing {
    pub landing_id: String,
    pub workspace_id: String,
    pub onto: String,
    pub commit_oid: String,
    pub tree_oid: String,
    pub inputs: Vec<String>,
    /// The passing check on exactly `tree_oid` that the landing relied on.
    pub check_id: String,
    /// With a rebase landing: each branch commit and the commit that
    /// replays it onto the target, oldest first (roadmap F5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rebased: Vec<Rewritten>,
    /// With `--adapt-renames`: the commits LangSpan added on top of a
    /// branch to use the target's new names (roadmap F5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapted: Vec<String>,
}

/// A branch commit and the commit a rebase landing made from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Rewritten {
    pub from: String,
    pub to: String,
}

/// The right to push to one target branch of one workspace's remote. One
/// landing holds it at a time; it ends when released, at its deadline in the
/// daemon's clock, or when the daemon restarts.
/// A landing waiting for a target's push turn, first come first served.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TurnWaiter {
    /// The landing's request ID; it becomes the turn's ID when it is taken.
    pub ticket: String,
    pub workspace_id: String,
    pub onto: String,
    pub session_id: Option<String>,
    pub joined_ms: u64,
    /// It leaves the queue, timed out, at this time.
    pub deadline_ms: u64,
    /// It leaves the queue, stopped, unless it asks again before this time.
    pub seen_until_ms: u64,
    pub generation: u64,
}

/// A landing in line for the push turn, as reported to callers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueuedLanding {
    pub ticket: String,
    pub session_id: Option<String>,
    /// 1 is next.
    pub position: u32,
    pub waited_ms: u64,
    /// Until it gives up, at most.
    pub wait_left_ms: u64,
}

/// Why a landing stopped waiting for the push turn without taking it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeftQueue {
    /// Someone asked for it to stop waiting.
    Cancelled,
    /// Its wait limit ran out.
    TimedOut,
    /// It did not ask again within `TURN_WAITER_SILENCE_MS`.
    Stopped,
}

/// Wait for the push turn instead of being told it is busy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TurnQueueing {
    /// The landing's request ID, the same on every ask.
    pub ticket: String,
    /// How long it waits at most, 1 ms to `MAX_TURN_WAIT_MS`.
    pub wait_ms: u64,
    /// True once it has been told it is waiting: if it is no longer in the
    /// queue, it was cancelled or dropped, and does not join again.
    pub joined: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    /// The request that took it.
    pub turn_id: String,
    pub workspace_id: String,
    pub onto: String,
    /// The agent session landing, if it said; none for the owner's terminal.
    pub session_id: Option<String>,
    pub taken_ms: u64,
    pub expires_at_ms: u64,
    pub generation: u64,
}

/// How one try at pushing a combined result ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttemptResult {
    Landed {},
    /// The remote refused the push because the target moved.
    Moved {},
    /// The remote refused the push and the target had not moved: a hook,
    /// a permission or a protection rule.
    Refused {},
    /// No passing check exists for exactly this combined tree.
    NotReady {},
    /// The check ran on this attempt's tree and did not pass.
    CheckFailed {},
    NotCombinable {},
    NothingToLand {},
    /// Git or the coordinator failed; nothing was pushed in this attempt.
    Error {},
    /// The push did not finish (it timed out or could not be waited for):
    /// whether the remote branch changed is not known.
    PushUnknown {},
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LandingAttempt {
    /// The target's tip as fetched for this attempt.
    pub target_oid: Option<String>,
    /// The combined commit it would push.
    pub commit_oid: Option<String>,
    pub result: AttemptResult,
}

/// A symbol definition found by structural indexing. `digest` is the content
/// of the file it was read from; a match is by name and kind only.
///
/// The fields after `digest` are present only when a lookup asked for
/// detail (`GraphSymbol { detail: true }`); otherwise the record is the one
/// every earlier version sent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolRef {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub end_line: u32,
    pub signature: String,
    pub exported: bool,
    pub digest: String,
    /// SHA-256 of the definition's own bytes as indexed: a change elsewhere
    /// in the file leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_digest: Option<String>,
    /// Distinct names called within the definition's lines, first call
    /// first, at most `MAX_SYMBOL_CALLS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<Vec<CallCount>>,
    /// Call sites outside this definition whose callee has this name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callers: Option<Callers>,
    /// Notes whose paths include this file, newest first, at most
    /// `MAX_SYMBOL_NOTES`. Their text is data, never instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Vec<SymbolNote>>,
    /// The newest conclusive check record whose tree holds this file's
    /// indexed content. Absent when none was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<LastCheck>,
    /// In a ranked answer, why this definition ranks where it does, in
    /// rank order; empty for path order alone (roadmap C2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_reasons: Option<Vec<RankReason>>,
    /// In a ranked answer, the files other than its own that mention this
    /// definition's name, ranked, at most `CONTEXT_FILES`. Found by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependents: Option<Vec<Dependent>>,
    /// In a ranked answer, how many files other than its own mention the
    /// name: `dependents` and those left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependent_count: Option<u32>,
}

/// Distinct callees listed for one definition.
pub const MAX_SYMBOL_CALLS: u32 = 32;
/// Caller locations listed for one definition; all of them are counted.
pub const MAX_CALLER_SITES: u32 = 8;
/// Notes listed for one definition.
pub const MAX_SYMBOL_NOTES: usize = 4;

/// One name called within a definition: how often, and the line of the
/// first call. A name, not a resolved target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallCount {
    pub name: String,
    pub count: u32,
    pub line: u32,
}

/// Call sites of a name, found by name only. `sites` are `path:line`, in
/// path and line order, at most `MAX_CALLER_SITES`; `count` is all of them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Callers {
    pub count: u32,
    pub sites: Vec<String>,
}

/// A remembered note, quoted for a symbol lookup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolNote {
    pub id: String,
    pub text: String,
}

/// A check record named by a symbol lookup: its name, `passed` or
/// `failed`, and the tree it ran on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LastCheck {
    pub name: String,
    pub result: String,
    pub tree: String,
}

/// A place a name is mentioned. Not a resolved use: any symbol of that name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NameRef {
    pub path: String,
    pub name: String,
    pub line: u32,
    pub digest: String,
}

/// The deepest `graph impact` level a client may ask for (roadmap D2).
pub const MAX_IMPACT_DEPTH: u32 = 3;

/// One level of a multi-level impact answer (roadmap D2): the files that
/// mention a name the previous level's files export. Found by name, not
/// resolved, so each level compounds the false positives of the one before.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImpactLevel {
    /// From 1: level 1 mentions an export of the queried file.
    pub level: u32,
    pub files: Vec<ImpactFile>,
    /// A bound, the frame size or the request deadline cut this level.
    pub truncated: bool,
}

/// A file reached at its first level, and how.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImpactFile {
    pub path: String,
    /// The exported name of `from` that this file mentions.
    pub via: String,
    /// The file at the previous level (the queried file for level 1).
    pub from: String,
}

/// The most `calls` steps a `graph path` chain may have, and the number
/// asked for when none is given (roadmap D3).
pub const MAX_PATH_HOPS: u32 = 6;
pub const DEFAULT_PATH_HOPS: u32 = 4;
/// Chains one `graph path` answer lists at most.
pub const MAX_PATHS: usize = 5;
/// Edges one `graph path` search visits at most: one per definition a call
/// may reach, and one per called name with no definition.
pub const MAX_PATH_EDGES: u32 = 10_000;
/// The deepest `graph map` area, and the areas and edges it lists at most.
pub const MAX_MAP_DEPTH: u32 = 3;
pub const MAX_MAP_AREAS: usize = 64;
pub const MAX_MAP_EDGES: usize = 256;

/// One step of a `graph path` chain (roadmap D3): a definition, and the
/// call in it that the next step's definition answers to, or, for the last
/// step, the definition alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathStep {
    pub path: String,
    /// The call's line, or for the last step the definition's first line.
    pub line: u32,
    /// The definition's name.
    pub name: String,
    /// `calls`, or `defines` for the last step.
    pub edge: String,
    /// The called name, which the next step defines. Absent on the last step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callee: Option<String>,
    /// How this step's definition was found: `name` (every function,
    /// method or type of the name; the first step is always one) or `path`
    /// (only those in files the calling file imports, resolved by path).
    pub resolution: String,
    /// The name had more than one such definition, so this step may be one
    /// the program never takes.
    pub ambiguous: bool,
}

/// One chain of calls from `graph path`'s first name to its second.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolPath {
    pub steps: Vec<PathStep>,
    /// Any step is `ambiguous`.
    pub ambiguous: bool,
}

/// One area of a `graph map` (roadmap D3): the files whose module path
/// starts with `path`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MapArea {
    pub path: String,
    pub files: u32,
    /// Definitions visible outside their file.
    pub exported: u32,
    /// Indexed languages of its files, in name order.
    pub languages: Vec<String>,
}

/// Edges of one kind from one `graph map` area to another, counted.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MapEdge {
    pub from: String,
    pub to: String,
    /// `imports` (resolved by path) or `calls` (to a name with exactly one
    /// function, method or type definition, found by name).
    pub kind: String,
    pub resolution: String,
    pub count: u32,
}

/// How much of a workspace the index covers, so a short answer is not read
/// as a complete one.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphCoverage {
    /// Files in the last complete scan in a supported language.
    pub indexable_files: u32,
    /// Of those, indexed at exactly their scanned content.
    pub current_files: u32,
    /// Indexed with a syntax error or a bound reached.
    pub partial_files: u32,
    /// False when no complete scan exists: nothing is known.
    pub scanned: bool,
    /// Supported-language files the index cannot read in full at their
    /// current content (partial, waiting, too large, left out of the scan).
    /// Every query matches names across the whole workspace, so any of them
    /// may hold a match the answer lacks; above zero, an empty or short
    /// answer is weak. `graph blind-spots` names them. Absent when zero,
    /// and sent only to a client that asks for it (`blind_in_scope` on a
    /// lookup, or `graph_blind_spots`): earlier builds read coverage
    /// strictly and reject a field they do not know.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blind_in_scope: u32,
}

/// A file indexed with a syntax error or a bound reached.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PartialFile {
    pub path: String,
    /// Wall-clock milliseconds since which its indexed versions have been
    /// partial, on the coordinator's machine.
    pub since_ms: u64,
}

/// Other source files the index has no grammar for, by extension.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageCount {
    pub extension: String,
    pub files: u32,
}

/// Full counts of each blind-spot list, however many paths are returned.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BlindTotals {
    pub partial: u32,
    pub never_clean: u32,
    pub waiting: u32,
    pub too_large: u32,
    pub unscanned: u32,
}

/// Where the index is blind, by reason. A file can be in more than one
/// list (a partial file can also be waiting for its newer version).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BlindSpots {
    /// Indexed with a syntax error or a bound reached.
    pub partial: Vec<PartialFile>,
    /// Indexed, but never parsed cleanly: no interface baseline, so no
    /// interface notice can come from it.
    pub never_clean: Vec<String>,
    /// The scanned content is not indexed yet.
    pub waiting: Vec<String>,
    /// Over the index's 1 MiB bound (or the scan's 4 MiB one): never read.
    pub too_large: Vec<String>,
    /// Left out by the scan's file or byte bound.
    pub unscanned: Vec<String>,
    /// Other source files, most first, then by extension.
    pub unsupported_languages: Vec<LanguageCount>,
    pub totals: BlindTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InterfaceChange {
    pub name: String,
    pub kind: String,
    /// `added`, `removed`, `signature_changed`, `renamed`, `definition_added`
    /// or `definition_removed`. `added` is a new exported name with its
    /// signature in `after`: kept in the log for a definition's history
    /// (roadmap D4), never held for briefings. A removal recorded since
    /// then carries the removed signature in `before`. `renamed` means the
    /// only definition of `name` went and one with the same signature but
    /// for the name came as `after_name`, with the evidence `evidence`
    /// names; with `file_moved`, the definition kept its name and moved with
    /// its whole file from `before_path` to the notice's path. The last two
    /// mean only the number of definitions sharing the name and kind
    /// changed: `after` (added) or `before` (removed) holds the signatures
    /// that came or went, and the counts say how many there were and are.
    pub change: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// The new name, for `renamed` only (roadmap B10). Absent from notices
    /// recorded before renames were told apart from removals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_name: Option<String>,
    /// The old name, for `renamed` only: always `name` (roadmap D7). Absent
    /// from notices recorded before it was kept. This field, `evidence`,
    /// `before_path` and `private` are left out of replies to protocol
    /// version 3 clients, and so is a change with `private` set
    /// (`protocol::encode_for` in the daemon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_name: Option<String>,
    /// What links the two, for `renamed` only (roadmap D7): `same_body`
    /// (the same body digest), `same_signature` (a changed body at the same
    /// first and last line) or `file_moved` (the whole file moved, with
    /// exactly the same content). Absent from renames recorded before it
    /// was kept, which were all `same_body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Where the file was before, for `file_moved` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_path: Option<String>,
    /// A definition that is not exported, for `file_moved` only: what the
    /// notice of a file with no exports lists, so that the file's notes and
    /// history follow it (roadmap D7). History only, like `added`: never
    /// held for briefings, recaps, break alerts or the dashboard.
    #[serde(default, skip_serializing_if = "is_false")]
    pub private: bool,
    /// Parameter counts of `before` and `after`, when each is exactly one
    /// function's signature (ADR 0029). Absent from notices recorded
    /// before they were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_arity: Option<Arity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_arity: Option<Arity>,
    /// Definitions with this name and kind before and after, for
    /// `definition_added` and `definition_removed` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_count: Option<u32>,
}

/// One recorded rename of an exported definition (roadmap D7): `name` went
/// from `path` and `after_name` came, linked by `evidence` (absent for a
/// rename recorded before it was kept).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Rename {
    pub name: String,
    pub after_name: String,
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// The rename's new signature.
    pub after: String,
}

/// Renames remembered per workspace for `graph symbol --renamed`.
pub const MAX_RENAMES: usize = 256;
/// Renames one lookup follows at most, old name to newest.
pub const MAX_RENAME_HOPS: usize = 8;

/// The renames that took `name` to its newest name in `workspace_id`, in
/// order, at most `MAX_RENAME_HOPS`; `None` when no rename of it is
/// recorded. Each hop after the first is of the first one's kind, so an
/// unrelated definition's rename never continues the chain. A hop back to a
/// name already passed ends the chain.
pub fn renamed_to(state: &State, workspace_id: &str, name: &str) -> Option<Vec<Rename>> {
    let renames = state.renames.get(workspace_id)?;
    let mut hops: Vec<Rename> = Vec::new();
    let mut current = name;
    while hops.len() < MAX_RENAME_HOPS {
        let Some(hop) = renames.iter().find(|rename| {
            rename.name == current && hops.first().is_none_or(|first| first.kind == rename.kind)
        }) else {
            break;
        };
        if hop.after_name == name || hops.iter().any(|seen| seen.name == hop.after_name) {
            break;
        }
        hops.push(hop.clone());
        current = &hop.after_name;
    }
    (!hops.is_empty()).then_some(hops)
}

/// Definitions sharing one name and kind that a notice counts at most.
pub const MAX_DEFINITION_COUNT: u32 = 65_535;

/// How many arguments a function takes, read from its signature text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Arity {
    /// Parameters without a default, not variadic, not the receiver.
    pub min: u32,
    /// All parameters but the receiver; `None` when one is variadic.
    pub max: Option<u32>,
    /// The first parameter is an explicit receiver (Rust `self`, Python
    /// `self` or `cls`): passed by `a.f(...)`, written out by `T::f(a, ...)`.
    pub receiver: bool,
}

/// The largest parameter count an arity may state.
pub const MAX_ARITY: u32 = 255;

impl Arity {
    /// Whether a call with `args` arguments can be a call of this function.
    /// A call through a receiver (`member`) does not pass it; any other call
    /// may or may not, so both readings are accepted.
    pub fn fits(&self, args: u32, member: bool) -> bool {
        let within =
            |extra: u32| args >= self.min + extra && self.max.is_none_or(|max| args <= max + extra);
        within(0) || (self.receiver && !member && within(1))
    }
}

/// One call written by a tool call that has not landed yet: where, which
/// name, and how many arguments, from the hook's parse of the new text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallSite {
    pub path: String,
    pub line: u32,
    pub name: String,
    pub args: u32,
    /// Written after a receiver or module (`a.f(x)`).
    pub member: bool,
}

/// A written call that another live session's recent change makes wrong: its
/// argument count does not fit the new signature, or the function it names
/// was renamed or removed. A name match, not a resolved call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallBreak {
    pub notice_id: String,
    pub call: CallSite,
    /// The interface change: `signature_changed`, `renamed` or `removed`.
    pub change: String,
    /// The file whose exported signature changed.
    pub defined_in: String,
    /// The root of the other worktree of the repository whose index holds
    /// the change, when it is not the asking session's own workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    pub by_session: String,
    pub before: String,
    /// Empty for `removed`.
    pub after: String,
    /// The new name, for `renamed` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_name: Option<String>,
    pub before_arity: Option<Arity>,
    /// Always present for `signature_changed`.
    pub after_arity: Option<Arity>,
    pub age_ms: u64,
}

/// Calls one write check names at most.
pub const MAX_CALL_SITES: usize = 64;
/// Breaks one answer carries at most.
pub const MAX_CALL_BREAKS: usize = 8;
/// Notices remembered as alerted per session.
pub const MAX_ALERTED: usize = 64;
/// Notices remembered as told per session, by a briefing or between steps
/// (issue #214). A briefing tells at most `BRIEF_INTERFACE_NOTICES` at once.
pub const MAX_TOLD_NOTICES: usize = 64;
/// Interface notices one briefing or one between-step delivery tells.
pub const BRIEF_INTERFACE_NOTICES: usize = 4;
/// Prompts remembered per session as finished, for between-step watchers
/// (issue #214). A watcher lives within one turn, so a few suffice.
pub const MAX_STOPPED_PROMPTS: usize = 8;

/// How an interface notice reached a session's context (issue #214).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeVia {
    /// In a briefing at a session's start, a prompt, after a tool call or
    /// from `langspan_brief`.
    Briefing,
    /// While the session's turn ran, through a host's documented
    /// between-step mechanism, with no hook call of the session's own.
    BetweenSteps,
}

/// An interface notice a session was told, and how.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToldNotice {
    pub notice_id: String,
    pub via: NoticeVia,
}

/// An exported symbol of a file changed between two indexed versions, with
/// the files that mention the name. A rename shows as a removal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InterfaceNotice {
    pub notice_id: String,
    pub workspace_id: String,
    pub path: String,
    pub changes: Vec<InterfaceChange>,
    /// Other files in the workspace that mention a changed name.
    pub dependents: Vec<String>,
    /// Files of other worktrees of the repository that mention a changed
    /// name, as their indexes find them after the notice, at most
    /// `NOTICE_DEPENDENTS` (issue #194). Absent from notices recorded
    /// before it was kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elsewhere: Vec<WorktreeFile>,
    /// The session that most recently reported editing `path`. A
    /// correlation, not proof of who made the change.
    pub by_session: Option<String>,
    pub at_ms: u64,
    pub generation: u64,
}

/// A file of one workspace, named by its path relative to that workspace's
/// root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct WorktreeFile {
    pub workspace_id: String,
    pub path: String,
}

/// An interface change that touches the asking session's own files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InterfaceImpact {
    pub path: String,
    pub by_session: Option<String>,
    pub changes: Vec<InterfaceChange>,
    /// The asking session's files that mention a changed name.
    pub affected: Vec<String>,
    pub age_ms: u64,
}

/// In-flight lines one briefing carries at most (roadmap C7).
pub const INFLIGHT_LINES: usize = 3;
/// Rendered bytes the in-flight lines of one briefing take together, at
/// most, by `inflight_cost`: their share of the briefing cap.
pub const INFLIGHT_BYTES: usize = 600;
/// In-flight symbols the daemon offers one briefing, at most.
pub const INFLIGHT_CANDIDATES: usize = 32;
/// The asking side's files named for one in-flight symbol, at most.
pub const INFLIGHT_MENTIONS: usize = 4;
/// In-flight signatures remembered as told per session. The daemon's
/// briefing window reads every in-flight row in turn, so this bounds how
/// many signatures still in flight a session hears once each; past it the
/// oldest is forgotten and told again.
pub const MAX_INFLIGHT_TOLD: usize = 1024;

/// An exported symbol that another worktree of the repository added, or
/// whose signature it changed, in a file that differs from that worktree's
/// `HEAD`: work not yet committed there (roadmap C7). Found by the daemon
/// in its source index by name, with the asking workspace's files that
/// mention the name or import a path ending in it or in the file's module.
/// The signature is repository data, never instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InflightSymbol {
    /// The worktree's workspace, never the asking one.
    pub workspace_id: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub mentioned_in: Vec<String>,
    /// The name `HEAD` gave this symbol, when the worktree renamed it
    /// (roadmap D7): files that mention either name are told. Left out of
    /// replies to protocol version 3 clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
}

/// An in-flight symbol as a briefing or `graph inflight` tells it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InflightNotice {
    pub path: String,
    /// The worktree's root.
    pub worktree: String,
    /// The session in that worktree that most recently reported editing
    /// `path`. A correlation, not proof of who made the change.
    pub session: Option<String>,
    pub name: String,
    pub kind: String,
    pub signature: String,
    /// The asking side's files that mention it.
    pub mentioned_in: Vec<String>,
    /// See [`InflightSymbol::renamed_from`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
}

/// What a symbol's in-flight line costs of `INFLIGHT_BYTES`: at least what
/// an adapter renders for it, quoting included, and never more than the
/// whole share, so the first line always fits.
pub fn inflight_cost(notice: &InflightNotice) -> usize {
    let quoted = |text: &str| text.len() + text.matches(['"', '\\']).count() + 4;
    let raw = [
        &notice.path,
        &notice.worktree,
        &notice.name,
        &notice.kind,
        &notice.signature,
    ]
    .into_iter()
    .map(|text| quoted(text))
    .sum::<usize>()
        + notice.session.as_deref().map_or(0, quoted)
        // ` (renamed from "…")`.
        + notice.renamed_from.as_deref().map_or(0, |from| quoted(from) + 16)
        + notice
            .mentioned_in
            .iter()
            .map(|path| quoted(path))
            .sum::<usize>();
    (200 + raw).min(INFLIGHT_BYTES)
}

/// One in-flight signature, as remembered once told to a session: a change
/// of signature is a new key, so it is told again.
fn inflight_key(item: &InflightSymbol) -> String {
    fingerprint(&format!(
        "{}\n{}\n{}\n{}\n{}",
        item.workspace_id, item.path, item.kind, item.name, item.signature
    ))
}

/// Whether this signature of `item` was already told to `session_id`.
pub fn inflight_told(state: &State, session_id: &str, item: &InflightSymbol) -> bool {
    state
        .activity
        .get(session_id)
        .is_some_and(|activity| activity.inflight_told.contains(&inflight_key(item)))
}

/// The worktree root and reporting session of an in-flight symbol, from
/// projected state; `None` when its workspace is not registered.
pub fn inflight_notice(state: &State, item: &InflightSymbol) -> Option<InflightNotice> {
    let workspace = state.workspaces.get(&item.workspace_id)?;
    let session = state
        .activity
        .values()
        .filter(|activity| activity.generation == state.generation)
        .filter(|activity| {
            state
                .sessions
                .get(&activity.session_id)
                .is_some_and(|session| session.workspace_id == item.workspace_id)
        })
        .filter_map(|activity| {
            let touch = activity
                .touched
                .iter()
                .find(|touch| touch.path == item.path)?;
            Some((touch.at_ms, &activity.session_id))
        })
        .max()
        .map(|(_, session)| session.clone());
    Some(InflightNotice {
        path: item.path.clone(),
        worktree: workspace.root.clone(),
        session,
        name: item.name.clone(),
        kind: item.kind.clone(),
        signature: item.signature.clone(),
        mentioned_in: item.mentioned_in.clone(),
        renamed_from: item.renamed_from.clone(),
    })
}

/// What a briefing of `session_id` compares in-flight work with: the
/// session's workspace, and the files it works on or has read. `None` for
/// a session that is not current.
pub fn briefing_concern(
    state: &State,
    session_id: &str,
    now_ms: u64,
) -> Option<(String, BTreeSet<String>)> {
    let me = current_session(state, session_id).ok()?;
    Some((
        me.workspace_id.clone(),
        concerned_paths(state, session_id, now_ms),
    ))
}

/// When an event that has no time of its own was decided: the daemon's
/// clock and the generation it belongs to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub at_ms: u64,
    pub generation: u64,
}

/// Something a person or an agent asked LangSpan to remember about a
/// repository. It outlives sessions and daemon restarts until forgotten.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub note_id: String,
    pub workspace_id: String,
    /// The session that recorded it, when one did.
    pub author: Option<String>,
    pub text: String,
    /// Files it is about. Empty means the whole workspace.
    pub paths: Vec<String>,
    /// The definition it is about (roadmap E1). Absent for a note about
    /// files or the workspace, and for every note recorded before anchors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<NoteAnchor>,
    /// Whether that definition is still as it was; present exactly when
    /// `anchor` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_state: Option<AnchorState>,
    /// Up to `MAX_TAGS`, each once, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Who a message is for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Recipient {
    /// One session of the workspace.
    Session { session_id: String },
    /// Every session of the workspace except the sender, each once.
    Everyone {},
    /// A handoff: the first other session of the workspace that starts or
    /// is prompted after it was sent.
    NextSession {},
}

/// A short line from one session, or from the owner, to other sessions of
/// the same workspace. Held until each recipient has acknowledged it; the
/// event log keeps the history. Its text is data, never instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub message_id: String,
    pub workspace_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    pub to: Recipient,
    pub text: String,
    /// Files it is about. May be empty.
    pub paths: Vec<String>,
    /// Sessions that have acknowledged it. A held message lists them only
    /// for everyone; the history kept for withdrawal lists every reader.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acknowledged: Vec<String>,
    /// Withdrawn by its sender or the owner: no longer delivered, and each
    /// session that had received it is told once to disregard it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub retracted: bool,
}

/// Why a session took over an ended session's uncommitted work, when
/// LangSpan can tell (issue #161). Shown, never required: the person or
/// agent decides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdoptReason {
    /// The ended session sent the adopter this message: addressed to it,
    /// or a handoff the adopter took.
    Handoff { message_id: String },
    /// Both sessions have the same task line.
    SameTask {},
}

/// One session took over an ended session's journaled, uncommitted lines
/// in the same workspace (issue #161). The line journal credits them to
/// `session_id` from then on; this record names the transfer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Adoption {
    /// The ended session whose lines were taken over.
    pub from: String,
    /// The session that took them over.
    pub session_id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<AdoptReason>,
    /// The daemon's clock, within `generation`.
    pub at_ms: u64,
    pub generation: u64,
}

/// An ended session that left uncommitted work in the briefed session's
/// checkout and that nobody has adopted: the briefing suggests `langspan
/// adopt` (issue #161). Its task line is data from that session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeftWork {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Ended sessions one briefing names as having left work, at most.
pub const BRIEF_LEFT_WORK: usize = 4;

/// A message a session received and that was then withdrawn: the session
/// is told once, at its next briefing, to disregard it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetractedMessage {
    pub message_id: String,
    pub workspace_id: String,
    /// The message's sender; `None` for the owner.
    pub from: Option<String>,
    /// The first `RETRACTED_TEXT_BYTES` of its text, cut at a character
    /// boundary.
    pub text: String,
    /// True when `text` is shorter than the message was.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cut: bool,
}

/// One message in full, as it was sent: what `messages show` returns for a
/// line a briefing cut. The text and paths are data from the sender, never
/// instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShownMessage {
    pub message_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    pub to: Recipient,
    pub text: String,
    pub paths: Vec<String>,
    /// Withdrawn by its sender or the owner since it was sent: its readers
    /// have been or will be told to disregard it.
    pub retracted: bool,
}

impl From<Message> for ShownMessage {
    fn from(message: Message) -> Self {
        Self {
            message_id: message.message_id,
            from: message.from,
            to: message.to,
            text: message.text,
            paths: message.paths,
            retracted: message.retracted,
        }
    }
}

/// How a message reached a session's context (roadmap E12).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// A briefing a hook placed at session start or at a prompt.
    Briefing,
    /// A briefing a hook placed after a completed tool call, mid-turn.
    MidTurn,
    /// The session's own call to the MCP tool `langspan_brief`.
    McpBrief,
    /// `langspan ack` or its operation: marked seen without a briefing.
    Ack,
}

/// Where a sent message stands as a whole.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    /// Still held: some recipient may yet be given it.
    Pending,
    /// Every session it was owed to was given it; nothing more is sent.
    Done,
    /// Withdrawn by its sender or the owner.
    Retracted,
    /// Pushed out, unread, by newer messages past the `MAX_MESSAGES` held.
    Dropped,
}

/// Where a message stands for one recipient.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecipientState {
    /// Placed in the session's context. Not a sign the model acted on it.
    Delivered,
    /// Not yet placed in the session's context.
    Pending,
    /// The session ended before it was given the message.
    Ended,
}

/// One recipient of a message and whether it was given it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecipientStatus {
    pub session_id: String,
    pub status: RecipientState,
    /// Since delivery, on this daemon's clock; `None` before delivery or
    /// when it was recorded before the daemon last started or before
    /// roadmap E12.
    pub age_ms: Option<u64>,
    /// `None` before delivery or in a record from before roadmap E12.
    pub via: Option<Delivery>,
}

/// Recipients a status lists at most; the rest are counted.
pub const STATUS_RECIPIENTS: usize = 64;

/// What `delivered` means, carried in every status reply.
pub const DELIVERED_MEANS: &str =
    "Delivered means placed in the session's context, not that it acted on it.";

/// Who was given a sent message: what `messages status` answers its sender
/// (roadmap E12). Read-only: asking records nothing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MessageStatus {
    pub message_id: String,
    /// The sending session; `None` for the owner.
    pub from: Option<String>,
    pub to: Recipient,
    /// Since it was sent, on this daemon's clock; `None` when that is not
    /// known (sent before the daemon last started, or the record is gone).
    pub sent_age_ms: Option<u64>,
    pub state: MessageState,
    /// Delivered sessions first, in delivery order, then the rest by ID; at
    /// most `STATUS_RECIPIENTS`. A pending handoff has none: no session has
    /// taken it.
    pub recipients: Vec<RecipientStatus>,
    /// Recipients past `STATUS_RECIPIENTS`, counted and not listed.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub more_recipients: u64,
    /// Part of the message's record is no longer in the event log
    /// (retention removed it): the answer is what remains.
    #[serde(default, skip_serializing_if = "is_false")]
    pub partial: bool,
    /// `DELIVERED_MEANS`.
    pub delivered_means: String,
}

/// A message's sent record as the event log holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentRecord {
    pub message: Message,
    pub at_ms: Option<u64>,
    pub generation: Option<u64>,
}

/// One `message_acknowledged` record from the event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckRecord {
    pub session_id: String,
    pub via: Option<Delivery>,
    pub at_ms: Option<u64>,
    pub generation: Option<u64>,
}

/// The message `message_id` of the workspace, or with no ID the newest one
/// `from` sent (the owner's when `from` is `None`), for a status query. The
/// workspace and IDs are validated here; the message's own records are
/// read from the event log by the caller.
pub fn status_message_id(
    state: &State,
    workspace_id: &str,
    from: Option<&str>,
    message_id: Option<&str>,
) -> Result<String, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if let Some(from) = from {
        validate_id(from, "session ID")?;
    }
    match message_id {
        Some(id) => {
            validate_request_id(id)?;
            Ok(id.to_owned())
        }
        None => sent_message(state, workspace_id, from, None)
            .map(|message| message.message_id.clone())
            .ok_or_else(|| {
                error(
                    "unknown_message",
                    "This sender has none of the latest 64 messages sent in this workspace; ask about an older one by its ID",
                )
            }),
    }
}

/// Who was given message `message_id`: from the state's held messages and
/// history, and from its records in the event log, `sent` (absent when not
/// kept) and `acks` (in log order, after `sent`). `retracted` says a later
/// `message_retracted` record exists. `None` when neither the state nor
/// the log knows the message.
pub fn message_status(
    state: &State,
    workspace_id: &str,
    message_id: &str,
    sent: Option<&SentRecord>,
    acks: &[AckRecord],
    retracted: bool,
    now_ms: u64,
) -> Option<MessageStatus> {
    fn find<'a>(
        lists: &'a BTreeMap<String, Vec<Message>>,
        workspace_id: &str,
        message_id: &str,
    ) -> Option<&'a Message> {
        lists
            .get(workspace_id)
            .and_then(|list| list.iter().find(|message| message.message_id == message_id))
    }
    let held = find(&state.messages, workspace_id, message_id);
    let record = find(&state.message_history, workspace_id, message_id);
    let message = sent.map(|sent| &sent.message).or(record).or(held)?.clone();
    let age = |at_ms: Option<u64>, generation: Option<u64>| match (at_ms, generation) {
        (Some(at_ms), Some(generation)) if generation == state.generation => {
            Some(now_ms.saturating_sub(at_ms))
        }
        _ => None,
    };
    // Every reader the log and the state know, first delivery first.
    let mut delivered: Vec<RecipientStatus> = Vec::new();
    for ack in acks {
        if !delivered
            .iter()
            .any(|seen| seen.session_id == ack.session_id)
        {
            delivered.push(RecipientStatus {
                session_id: ack.session_id.clone(),
                status: RecipientState::Delivered,
                age_ms: age(ack.at_ms, ack.generation),
                via: ack.via,
            });
        }
    }
    for reader in record
        .into_iter()
        .chain(held)
        .flat_map(|message| &message.acknowledged)
    {
        if !delivered.iter().any(|seen| &seen.session_id == reader) {
            delivered.push(RecipientStatus {
                session_id: reader.clone(),
                status: RecipientState::Delivered,
                age_ms: None,
                via: None,
            });
        }
    }
    let dropped = state
        .dropped_messages
        .get(workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut partial = sent.is_none();
    let message_state = if retracted || record.is_some_and(|record| record.retracted) {
        MessageState::Retracted
    } else if held.is_some() {
        MessageState::Pending
    } else if record.is_some() {
        // The history outlasts every drop: a message the cap pushed out of
        // the held list was already out of the history.
        MessageState::Done
    } else if dropped.iter().any(|id| id == message_id) {
        MessageState::Dropped
    } else {
        match message.to {
            // These leave the held list only when taken, withdrawn or dropped.
            Recipient::Session { .. } | Recipient::NextSession {} => {
                if delivered.is_empty() {
                    MessageState::Dropped
                } else {
                    MessageState::Done
                }
            }
            Recipient::Everyone {} => {
                // The drop list keeps the latest `MAX_MESSAGES` drops; once
                // full, an older drop cannot be told from a completion.
                if dropped.len() >= MAX_MESSAGES {
                    partial = true;
                }
                MessageState::Done
            }
        }
    };
    let ended = |session_id: &str| {
        state
            .activity
            .get(session_id)
            .is_some_and(|activity| activity.ended)
    };
    let mut waiting: Vec<RecipientStatus> = Vec::new();
    let mut not_given = |session_id: &str| {
        if !delivered.iter().any(|seen| seen.session_id == session_id) {
            waiting.push(RecipientStatus {
                session_id: session_id.to_owned(),
                status: if ended(session_id) {
                    RecipientState::Ended
                } else {
                    RecipientState::Pending
                },
                age_ms: None,
                via: None,
            });
        }
    };
    match &message.to {
        Recipient::Session { session_id } => not_given(session_id),
        // Each current session it is still owed to. Once done, a session
        // that joined later was never owed it.
        Recipient::Everyone {} if message_state != MessageState::Done => {
            for session in state.sessions.values() {
                if session.workspace_id == workspace_id
                    && session.generation == state.generation
                    && message.from.as_deref() != Some(session.session_id.as_str())
                {
                    not_given(&session.session_id);
                }
            }
        }
        Recipient::Everyone {} | Recipient::NextSession {} => {}
    }
    waiting.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    let mut recipients = delivered;
    recipients.append(&mut waiting);
    let more_recipients = recipients.len().saturating_sub(STATUS_RECIPIENTS) as u64;
    recipients.truncate(STATUS_RECIPIENTS);
    Some(MessageStatus {
        message_id: message.message_id,
        from: message.from,
        to: message.to,
        sent_age_ms: sent.and_then(|sent| age(sent.at_ms, sent.generation)),
        state: message_state,
        recipients,
        more_recipients,
        partial,
        delivered_means: DELIVERED_MEANS.into(),
    })
}

/// The observer could not complete a scan of a workspace. Until one
/// completes, disk facts, check staleness marks and the index are as of the
/// last complete scan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScanFailure {
    pub workspace_id: String,
    pub detail: String,
    pub at_ms: u64,
    pub generation: u64,
}

/// One session in a recap: what it is and what it last did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecapSession {
    pub session_id: String,
    /// Registered since the daemon last started.
    pub current: bool,
    pub ended: bool,
    pub liveness: Option<LivenessStatus>,
    pub summary: Option<String>,
    pub intent: Option<String>,
    pub editing: Vec<String>,
    pub recent: Vec<RecentPath>,
    /// The session that took over this ended session's uncommitted work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopted_by: Option<String>,
    /// Ended sessions whose uncommitted work this session took over, at
    /// most `RECAP_ITEMS`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adopted: Vec<String>,
}

/// A bounded account of one workspace: who is here, what changed, what is
/// remembered, what evidence exists and what LangSpan cannot currently see.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recap {
    pub workspace_id: String,
    pub root: String,
    pub branch_ref: Option<String>,
    pub sessions: Vec<RecapSession>,
    pub omitted_sessions: u32,
    /// Items left out after the per-list caps so the whole recap fits one
    /// transport frame. Counted, never silently dropped.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_items: u32,
    pub interface_changes: Vec<InterfaceNotice>,
    pub checks: Vec<CheckRecord>,
    pub landings: Vec<Landing>,
    pub notes: Vec<Note>,
    /// Messages still owed to at least one session, newest first, then
    /// recently withdrawn ones (`retracted`), newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// The newest trails ended sessions left here, at most `RECAP_TRAILS`
    /// (roadmap C9), whether or not a session was told them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trails: Vec<TrailView>,
    pub teammates: Vec<RemotePeer>,
    pub publishing: Option<RemoteLink>,
    /// Present when the last scan attempt failed.
    pub scan_failure: Option<ScanFailure>,
    /// Present when the last completed scan left something unversioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_coverage: Option<ScanCoverage>,
}

pub const MAX_NOTES: usize = 64;
pub const NOTE_BYTES: usize = 400;
pub const RECAP_SESSIONS: usize = 16;
const RECAP_ITEMS: usize = 8;
/// Trails in a recap; `langspan trail` lists up to `MAX_TRAILS`.
pub const RECAP_TRAILS: usize = 4;

/// Unacknowledged messages held per workspace; the oldest go first.
pub const MAX_MESSAGES: usize = 64;
pub const MESSAGE_BYTES: usize = 400;
/// Messages one briefing carries at most.
pub const BRIEF_MESSAGES: usize = 4;
/// The longest line an adapter renders for one message, before its
/// ellipsis. Selection below assumes adapters cut there.
pub const MESSAGE_LINE_LIMIT: usize = 700;
/// Rendered bytes the messages of one briefing may take together, so that
/// every message selected for a briefing fits its text and none is
/// acknowledged without being shown.
pub const MESSAGE_BRIEF_BYTES: usize = 1500;
/// The longest line an adapter renders for an uncoordinated change, before
/// its ellipsis. A briefing that carries one takes this line, plus its
/// ellipsis and newline, out of `MESSAGE_BRIEF_BYTES`: both are delivered
/// once, so every message and withdrawal selected beside it must still be
/// shown.
pub const UNCOORDINATED_LINE_LIMIT: usize = 700;
/// Bytes of a withdrawn message's text quoted when its readers are told.
pub const RETRACTED_TEXT_BYTES: usize = 120;

pub const MAX_NOTICES: usize = 32;
pub const NOTICE_CHANGES: usize = 16;
pub const NOTICE_DEPENDENTS: usize = 32;

/// A code-context query names at most this many identifiers.
pub const CONTEXT_NAMES: usize = 12;
/// Definitions returned per name, at most.
pub const CONTEXT_DEFINITIONS: u32 = 3;
/// Distinct files that mention a name, returned per name, at most.
pub const CONTEXT_FILES: u32 = 5;

/// One name asked about in a code-context query, with where it is defined
/// and which files mention it. A name match, not a resolved symbol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NameContext {
    pub name: String,
    pub definitions: Vec<SymbolRef>,
    /// Distinct paths, in path order.
    pub mentioned_in: Vec<String>,
    /// True when more definitions or mentioning files exist than are listed.
    pub truncated: bool,
    /// In a ranked answer, how many definitions the name has: those listed
    /// and those left out (roadmap C2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_count: Option<u32>,
}

pub const MAX_CHECKS: usize = 64;
pub const MAX_LANDINGS: usize = 32;
/// The longest a push turn may be held without being released.
pub const MAX_TURN_TTL_MS: u64 = 4 * 60 * 60 * 1000;
/// Tries at landing within one turn, when the target keeps moving.
pub const MAX_LANDING_ATTEMPTS: usize = 3;
/// Commits one landing may rebase, and adapt, at most (roadmap F5).
pub const MAX_REWRITTEN: usize = 64;
/// Landings waiting for one target's push turn, at most.
pub const MAX_TURN_QUEUE: usize = 8;
/// The longest a landing may wait for the push turn.
pub const MAX_TURN_WAIT_MS: u64 = 60 * 60 * 1000;
/// A waiting landing that has not asked again for this long has stopped
/// waiting, and leaves the queue.
pub const TURN_WAITER_SILENCE_MS: u64 = 15_000;

pub const REMOTE_WORK: usize = 8;
/// A teammate not heard from for this long is treated as disconnected.
pub const REMOTE_HORIZON_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Briefing {
    pub session_id: String,
    /// False when the same peers and paths were already delivered.
    pub changed: bool,
    pub peers: Vec<PeerWork>,
    pub omitted_peers: u32,
    /// Items left out because this session was already told them and they
    /// are unchanged: only in a briefing asked for with `changes_only`
    /// (issue #222). Zero means the briefing is the full view.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unchanged: u32,
    /// Files this session read that a peer in the same checkout has since
    /// reported editing. Detection after the fact, from reported edits only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_reads: Vec<StaleRead>,
    /// Teammates' sessions on other machines, from their publications.
    /// Always advisory: nothing here holds or releases a file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote: Vec<RemotePeerWork>,
    /// Exported symbols a peer changed that this session's files mention by
    /// name. A name match, not a resolved dependency.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interface_changes: Vec<InterfaceImpact>,
    /// The notice ID of each of `interface_changes`, in the same order. A
    /// hook reports the ones its rendered text showed (`InterfaceShown`,
    /// issue #214).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interface_notice_ids: Vec<String>,
    /// Exported symbols another worktree of the repository has added or
    /// changed and not committed, that this session's files mention by name
    /// (roadmap C7). Each signature is told once, at most `INFLIGHT_LINES`
    /// within `INFLIGHT_BYTES`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inflight: Vec<InflightNotice>,
    /// In-flight symbols owed to this session but left for a later
    /// briefing.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_inflight: u32,
    /// Remembered notes about the workspace or about files this session
    /// works on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    /// Messages addressed to this session or to everyone, and a pending
    /// handoff, that it has not yet acknowledged. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// Messages this session received that were since withdrawn: each is
    /// delivered once, so the session disregards it. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retracted: Vec<RetractedMessage>,
    /// The checkout this session works in changed under it since it last
    /// read or edited a file there, other than by its own shell command.
    /// Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkout_changes: Vec<CheckoutNotice>,
    /// Changes on disk in this checkout that no session reported, with the
    /// Codex sessions that may have run without LangSpan's hooks. Each is
    /// delivered once. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncoordinated: Vec<UncoordinatedNotice>,
    /// Results of the automatic check run after this session's writes, not
    /// yet delivered. Each is delivered once. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckNotice>,
    /// Ended sessions that left uncommitted work in this checkout, from the
    /// sessions the request named (`Brief::left`). At most
    /// `BRIEF_LEFT_WORK`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_work: Vec<LeftWork>,
    /// The trail an ended session of this checkout left, at a start or a
    /// prompt, taken once (roadmap C9). Shown first, after that session's
    /// own handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<TrailView>,
    /// What changed recently in the repository, only in a briefing asked
    /// for at session start (roadmap C3). Never part of the signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent: Option<RecentChanges>,
}

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
    /// Recorded renames per workspace, newest first, at most `MAX_RENAMES`
    /// (roadmap D7): what `graph symbol --renamed` follows. A name defined
    /// again is dropped. Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub renames: BTreeMap<String, Vec<Rename>>,
    /// Trails of ended sessions per workspace, newest first, at most
    /// `MAX_TRAILS` (roadmap C9). Derived on replay; not part of the status
    /// reply.
    #[serde(skip)]
    pub trails: BTreeMap<String, Vec<Trail>>,
    /// The current generation's clock anchor, once recorded. Derived from
    /// the events on replay; not part of the status reply.
    #[serde(skip)]
    pub clock_anchor: Option<ClockAnchor>,
    /// Recent refusals placed in wall time, newest first, for the status
    /// line (roadmap H2). Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub refusals: Vec<RecentRefusal>,
    /// Refusals of the current run with their paths, newest first, at most
    /// `MAX_PATH_REFUSALS`, for a starting session's recent section
    /// (roadmap C3). Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub path_refusals: Vec<PathRefusal>,
    /// How many times each workspace ID has been moved or removed: its
    /// registration revision. With the root it names one registration, so
    /// what was derived outside the log during an earlier one (the source
    /// index, a worker's report) is started again or refused, even after a
    /// switch back to an earlier root. Derived on replay; not part of the
    /// status reply.
    #[serde(skip)]
    pub workspace_revisions: BTreeMap<String, u32>,
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
    pub kept_tests: BTreeMap<String, BTreeMap<String, KeptTests>>,
    /// Per session, the workspace's write count just before the session's
    /// first write since it registered. Derived on replay; not part of the
    /// status reply.
    #[serde(skip)]
    pub first_writes: BTreeMap<String, u64>,
    /// Per workspace, its sessions' runs of consecutive writes, oldest
    /// first, at most `MAX_WRITE_RUNS`. Derived on replay; not part of the
    /// status reply.
    #[serde(skip)]
    pub write_runs: BTreeMap<String, Vec<WriteRun>>,
    /// The wait for a current holder per workspace, for the workspaces
    /// whose conflict policy is `wait` (roadmap B28). Derived on replay;
    /// not part of the status reply.
    #[serde(skip)]
    pub conflict_waits: BTreeMap<String, u32>,
    /// Each session's write refusals not yet settled by an admitted write or
    /// a stop, oldest first, at most `MAX_UNSETTLED`, of the current
    /// generation only. Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub unsettled: BTreeMap<String, Vec<UnsettledRefusal>>,
}

/// Checkout changes kept per workspace.
pub const MAX_CHECKOUTS: usize = 4;

/// Host names one host check may report.
pub const MAX_HOSTS: usize = 16;

/// An agent host whose configuration directory exists on this machine but
/// which no LangSpan install covers: no user-level install, and no project
/// install in a registered workspace. Only its name is kept; what the owner
/// can run is derived from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UninstalledHost {
    pub host: String,
    /// When the daemon first saw it so, by its own clock.
    pub at_ms: u64,
    pub generation: u64,
}

/// The checkout of one physical workspace changed under the sessions in it:
/// its branch changed, its HEAD moved and files changed with it, or several
/// changed files went back to their committed version at once (`git
/// checkout`, `switch`, `reset --hard`, `stash`, `pull`). Built from the
/// scan's branch observation. Who did it is not known, unless a session's
/// shell command explains it (`CheckoutRun`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckoutChange {
    pub change_id: String,
    pub workspace_id: String,
    pub from_branch: Option<String>,
    pub to_branch: Option<String>,
    pub from_head: Option<String>,
    pub to_head: Option<String>,
    /// Files whose content changed in the same scan.
    pub files: u32,
    pub at_ms: u64,
    pub generation: u64,
}

/// Uncoordinated changes kept per workspace.
pub const MAX_UNCOORDINATED: usize = 4;
/// Changed files one uncoordinated change keeps, at most.
pub const MAX_UNCOORDINATED_FILES: usize = 8;
/// Codex sessions one uncoordinated change names, at most.
pub const MAX_UNCOORDINATED_SESSIONS: usize = 2;
/// Changed files one scan reports for comparison, at most.
pub const MAX_UNCOORDINATED_OBSERVED: usize = 256;
/// The host name that starts a Codex session's ID (`codex.<host session>`).
pub const CODEX_HOST: &str = "codex";
/// The host names that start the other hooked hosts' session IDs.
pub const CLAUDE_CODE_HOST: &str = "claude-code";
pub const CURSOR_HOST: &str = "cursor";
pub const ANTIGRAVITY_HOST: &str = "antigravity";
/// The tool name of a shell call on every host, as the hooks record it.
pub const SHELL_TOOL: &str = "Bash";

/// Files a scan saw change in one physical workspace that no session of it
/// reported, while Codex sessions are registered there (issue #129). Codex
/// trusts project hooks by the exact path of their file, so a session resumed
/// under another path for the same directory runs none of them. The sessions
/// named are the likely cause, not a proven author: a hand edit looks the same.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UncoordinatedChange {
    pub change_id: String,
    pub workspace_id: String,
    /// The Codex sessions registered here, most recently active first.
    pub sessions: Vec<String>,
    /// The versions the scan saw, sorted by path.
    pub files: Vec<ObservedFile>,
    pub at_ms: u64,
    pub generation: u64,
    /// Sessions this change was delivered to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivered: Vec<String>,
}

/// An uncoordinated change as one session's briefing carries it: the files
/// still at a version no session has reported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UncoordinatedNotice {
    pub sessions: Vec<String>,
    pub paths: Vec<String>,
    pub age_ms: u64,
}

/// A checkout change as one session's briefing carries it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckoutNotice {
    pub from_branch: Option<String>,
    pub to_branch: Option<String>,
    pub from_head: Option<String>,
    pub to_head: Option<String>,
    pub files: u32,
    pub age_ms: u64,
    /// The other session whose shell command explains the change, when one
    /// does (`CheckoutRun`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_session: Option<String>,
    /// That command's line, redacted and capped by its hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Shell runs that moved a checkout, kept per workspace.
pub const MAX_CHECKOUT_RUNS: usize = 8;
/// A checkout change first observed up to this long after a run finished,
/// at the state the run left, is the run's: the observer scans every few
/// seconds and may see the change only after the run's hook reported it.
pub const CHECKOUT_RUN_GRACE_MS: u64 = 60_000;
/// The longest a checkout run is taken to have run: a shell call's
/// supported lifetime, two hours.
pub const MAX_CHECKOUT_RUN_MS: u64 = 2 * 60 * 60 * 1000;
/// The command text a checkout run keeps, at most, in bytes.
pub const MAX_CHECKOUT_COMMAND_BYTES: usize = 256;

/// One session's shell command ran a Git operation that changes the
/// checkout (`git merge`, `pull`, `rebase`, `reset`, `checkout`, `switch`,
/// `stash` and the like) and the checkout changed while it ran: reported by
/// that session's post-tool hook. A checkout change it explains is not news
/// to that session, and other sessions are told whose command it was.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckoutRun {
    pub run_id: String,
    pub workspace_id: String,
    pub session_id: String,
    /// The command line, redacted and capped by the hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The checkout the command left.
    pub to_branch: Option<String>,
    pub to_head: Option<String>,
    /// When the command started and finished on the daemon's clock, placed
    /// back from the report's arrival by the durations its hook measured
    /// (never after the arrival), whenever it is reported.
    pub started_ms: u64,
    pub finished_ms: u64,
    pub generation: u64,
    /// The command changed no file's content: it moved HEAD or the branch
    /// only, as a commit does. Absent from an earlier record.
    #[serde(default, skip_serializing_if = "is_false")]
    pub files_unchanged: bool,
}

impl CheckoutRun {
    /// Whether this run explains `change`: observed while the command ran,
    /// or the first change observed after it finished, within
    /// `CHECKOUT_RUN_GRACE_MS`, at the checkout the command left, unless a
    /// change while it ran already showed that checkout or the command
    /// changed no file and the change left branch and HEAD in place.
    /// `changes` are the workspace's changes, to find the first.
    pub fn explains(&self, change: &CheckoutChange, changes: &[CheckoutChange]) -> bool {
        if change.workspace_id != self.workspace_id
            || change.generation != self.generation
            || change.at_ms < self.started_ms
        {
            return false;
        }
        if change.at_ms <= self.finished_ms {
            return true;
        }
        let left = |other: &CheckoutChange| {
            other.to_head == self.to_head && other.to_branch == self.to_branch
        };
        // A run that changed no file (a commit) explains no later change
        // with branch and HEAD in place (a restore): the scan records no
        // change for such a run, so that one came after it.
        let moved = change.from_head != change.to_head || change.from_branch != change.to_branch;
        change.at_ms <= self.finished_ms.saturating_add(CHECKOUT_RUN_GRACE_MS)
            && left(change)
            && (moved || !self.files_unchanged)
            && !changes.iter().any(|other| {
                other.change_id != change.change_id
                    && other.workspace_id == self.workspace_id
                    && other.generation == self.generation
                    && other.at_ms < change.at_ms
                    // Another change after the run came first; one while it
                    // ran at the state it left already showed its result.
                    && (other.at_ms > self.finished_ms
                        || (other.at_ms >= self.started_ms && left(other)))
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
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
    SourceObserved {
        session_id: String,
        files: Vec<ObservedFile>,
        origin: SourceOrigin,
        at_ms: u64,
        generation: u64,
    },
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordedEvent {
    pub seq: u64,
    pub event: Event,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    pub lease_id: String,
    pub session_id: String,
    pub paths: Vec<String>,
    /// The holder's regions in the conflicting paths it holds by region. A
    /// conflicting path missing here is held whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<PathRegions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreError {
    pub code: String,
    pub message: String,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CoreError {}

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
    /// Read-only: a bounded account of one workspace. With `tag`, only
    /// notes carrying it are listed (roadmap E1).
    Recap {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
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
    /// the calls themselves are never recorded.
    CheckCalls {
        session_id: String,
        calls: Vec<CallSite>,
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
    /// workspace mentions (roadmap C7).
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
    /// Read-only: a bounded account of one workspace. With `tag`, only
    /// notes carrying it are listed (roadmap E1).
    Recap {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
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
    /// the calls themselves are never recorded.
    CheckCalls {
        session_id: String,
        calls: Vec<CallSite>,
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

// One reply per request, built and sent once: its size does not matter, and
// boxing `Status`'s state would only add an allocation to every status read.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
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
    Intent {
        intent: Intent,
    },
    IntentEnded {
        session_id: String,
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
    Observed {
        observation: Observation,
    },
    DiskObserved {
        observation: DiskObservation,
    },
    BranchObserved {
        observation: BranchObservation,
    },
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
    AutoCheck {
        check: AutoCheck,
    },
    AutoCheckDisabled {
        workspace_id: String,
    },
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
    Dashboard {
        dashboard: Dashboard,
    },
    /// The answer to `Command::Version`.
    Version {
        version: String,
        pid: u32,
    },
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
    InterfaceAcknowledged {
        session_id: String,
        notice_ids: Vec<String>,
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
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub events: Vec<Event>,
    pub outcome: Outcome,
}

/// Validate a command and describe its complete transition without changing state.
/// The caller persists the returned events and receipt atomically before applying
/// them. Receipt replay and request-id ownership are storage responsibilities.
pub fn decide(state: &State, command: &Command, request_id: &str, now_ms: u64) -> Decision {
    match decide_checked(state, command, request_id, now_ms) {
        Ok(mut decision) => {
            let settled = policy::settle(state, &decision.events, now_ms);
            decision.events.extend(settled);
            decision
        }
        Err(error) => Decision {
            events: Vec::new(),
            outcome: Outcome::Error {
                code: error.code,
                message: error.message,
            },
        },
    }
}

fn decide_checked(
    state: &State,
    command: &Command,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    validate_request_id(request_id)?;
    match command {
        Command::WorkspaceRegister { workspace_id, root }
        | Command::WorkspaceRegisterVerified {
            workspace_id, root, ..
        } => {
            let verified_overlaps = if let Command::WorkspaceRegisterVerified {
                verified_overlaps,
                ..
            } = command
            {
                let links = normalized_workspace_links(verified_overlaps)?;
                validate_workspace_link_targets(state, workspace_id, root, &links)?;
                links
            } else {
                Vec::new()
            };
            let workspace = Workspace {
                workspace_id: workspace_id.clone(),
                root: root.clone(),
                verified_overlaps: state
                    .workspaces
                    .get(workspace_id)
                    .map(|old| old.verified_overlaps.clone())
                    .unwrap_or(verified_overlaps),
            };
            validate_workspace(state, &workspace)?;
            let events = if state.workspaces.contains_key(workspace_id) {
                Vec::new()
            } else {
                vec![Event::WorkspaceRegistered {
                    workspace: workspace.clone(),
                }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::Workspace { workspace },
            })
        }
        Command::WorkspaceList { after } => {
            let (workspaces, next) = workspaces::list(state, after.as_deref())?;
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Workspaces { workspaces, next },
            })
        }
        Command::WorkspaceMove {
            workspace_id, root, ..
        } => workspaces::decide_move(state, workspace_id, root, &[], now_ms),
        Command::WorkspaceMoveVerified {
            workspace_id,
            root,
            verified_overlaps,
        } => workspaces::decide_move(state, workspace_id, root, verified_overlaps, now_ms),
        Command::WorkspaceRemove {
            workspace_id,
            apply,
        } => workspaces::decide_remove(state, workspace_id, *apply, now_ms),
        Command::SessionRegister {
            session_id,
            workspace_id,
            parent,
            headless,
        } => {
            let session = Session {
                session_id: session_id.clone(),
                workspace_id: workspace_id.clone(),
                generation: state.generation,
                parent: parent.clone(),
                headless: *headless,
            };
            validate_session_registration(state, &session)?;
            let events = if state.sessions.get(session_id) == Some(&session) {
                Vec::new()
            } else {
                vec![Event::SessionRegistered {
                    session: session.clone(),
                }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::Session { session },
            })
        }
        Command::ObserveSessionLiveness {
            session_id,
            process,
            status,
        } => {
            let liveness = SessionLiveness {
                session_id: session_id.clone(),
                process: process.clone(),
                status: *status,
                observed_at_ms: now_ms,
                generation: state.generation,
            };
            validate_session_liveness(state, &liveness)?;
            let events = if state.session_liveness.get(session_id) == Some(&liveness) {
                Vec::new()
            } else {
                vec![Event::SessionLivenessObserved {
                    liveness: liveness.clone(),
                }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::SessionLiveness { liveness },
            })
        }
        Command::SessionSeen { session_id } => {
            current_session(state, session_id)?;
            let due = session_seen_due(state, session_id, now_ms);
            let last = state
                .activity
                .get(session_id)
                .filter(|activity| activity.generation == state.generation)
                .map_or(0, |activity| activity.last_hook_ms);
            let events = if due {
                vec![Event::SessionSeen {
                    session_id: session_id.clone(),
                    at_ms: now_ms,
                    generation: state.generation,
                }]
            } else {
                Vec::new()
            };
            Ok(Decision {
                events,
                outcome: Outcome::SessionSeen {
                    session_id: session_id.clone(),
                    last_hook_ms: if due { now_ms } else { last },
                },
            })
        }
        Command::RecordContext {
            session_id,
            bytes,
            turn,
        } => {
            current_session(state, session_id)?;
            if *bytes > MAX_CONTEXT_RECORD_BYTES || (*bytes == 0 && !*turn) {
                return Err(error(
                    "invalid_request",
                    "A context record names at most 1 MiB, and zero bytes only at a turn's start",
                ));
            }
            let event = Event::ContextAdded {
                session_id: session_id.clone(),
                bytes: *bytes,
                turn: *turn,
                generation: state.generation,
            };
            let mut after = state
                .activity
                .get(session_id)
                .filter(|activity| activity.generation == state.generation)
                .map(|activity| activity.context)
                .unwrap_or_default();
            after.add(*bytes, *turn);
            Ok(Decision {
                events: vec![event],
                outcome: Outcome::ContextRecorded {
                    session_id: session_id.clone(),
                    context: after,
                },
            })
        }
        Command::ForgetTold { session_id, keys } => {
            current_session(state, session_id)?;
            if keys.len() > MAX_TOLD {
                return Err(error(
                    "invalid_request",
                    "Forget at most 256 briefing item keys at once",
                ));
            }
            let named: BTreeSet<u64> = keys.iter().copied().collect();
            let told: Vec<u64> = state
                .activity
                .get(session_id)
                .filter(|activity| activity.generation == state.generation)
                .map(|activity| {
                    activity
                        .told
                        .iter()
                        .copied()
                        .filter(|key| named.contains(key))
                        .collect()
                })
                .unwrap_or_default();
            let forgotten = u32::try_from(told.len()).unwrap_or(u32::MAX);
            Ok(Decision {
                // A retry, or keys never told, record nothing.
                events: if told.is_empty() {
                    Vec::new()
                } else {
                    vec![Event::ToldForgotten {
                        session_id: session_id.clone(),
                        keys: told,
                    }]
                },
                outcome: Outcome::ToldForgotten {
                    session_id: session_id.clone(),
                    forgotten,
                },
            })
        }
        Command::IntentSet {
            session_id,
            paths,
            summary,
        } => {
            let session = current_session(state, session_id)?;
            validate_summary(summary)?;
            let intent = Intent {
                session_id: session_id.clone(),
                workspace_id: session.workspace_id.clone(),
                paths: normalized_paths(paths)?,
                summary: summary.clone(),
                generation: state.generation,
            };
            let events = if state.intents.get(session_id) == Some(&intent) {
                Vec::new()
            } else {
                vec![Event::IntentSet {
                    intent: intent.clone(),
                }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::Intent { intent },
            })
        }
        Command::IntentEnd { session_id } => {
            current_session(state, session_id)?;
            let events = if state.intents.contains_key(session_id) {
                vec![Event::IntentEnded {
                    session_id: session_id.clone(),
                }]
            } else {
                Vec::new()
            };
            Ok(Decision {
                events,
                outcome: Outcome::IntentEnded {
                    session_id: session_id.clone(),
                },
            })
        }
        Command::Admit {
            session_id,
            paths,
            ttl_ms,
        }
        | Command::AdmitTool {
            session_id,
            paths,
            ttl_ms,
            ..
        } => {
            let (operation, base, regions, overwrites) = if let Command::AdmitTool {
                operation,
                base,
                regions,
                overwrites,
                ..
            } = command
            {
                validate_tool_identity(operation)?;
                (
                    Some(operation.clone()),
                    Some(normalized_files(base)?),
                    regions.as_slice(),
                    overwrites.as_slice(),
                )
            } else {
                (None, None, &[][..], &[][..])
            };
            let session = current_session(state, session_id)?;
            let paths = normalized_paths(paths)?;
            lines::validate_overwrites(overwrites, &paths)?;
            let regions = trusted_regions(regions, &paths, base.iter().flatten())?;
            if !(1..=60_000).contains(ttl_ms) {
                return Err(error(
                    "invalid_ttl",
                    "Lease TTL must be between 1 and 60000 milliseconds",
                ));
            }
            let expires_at_ms = now_ms.checked_add(*ttl_ms).ok_or_else(|| {
                error(
                    "invalid_deadline",
                    "Lease deadline overflows the supplied clock; use a valid monotonic timestamp",
                )
            })?;
            if state.leases.contains_key(request_id) {
                return Err(error(
                    "lease_id_reused",
                    "This lease ID already exists; replay the original request through storage or use a new request ID",
                ));
            }
            // A host's tool call is never held back by an earlier tool call
            // of the same session. That earlier call may still sit at a
            // permission prompt the owner refused, which sends no post-tool
            // event; denying the session its own file until the admission
            // expires helps nobody. A manual admission stays exclusive, as
            // ADR 0001 defines.
            let tool_call = operation.is_some();
            let holders = holders_of(
                state,
                session_id,
                &session.workspace_id,
                &paths,
                &regions,
                tool_call,
                now_ms,
            );
            // Each refusal is one event, stored with the refusal's receipt:
            // the same request again is answered from it and adds none.
            let refused = |kind, peer: Option<&String>| Event::WriteRefused {
                session_id: session_id.clone(),
                workspace_id: session.workspace_id.clone(),
                kind,
                paths: paths.iter().take(REFUSAL_PATHS).cloned().collect(),
                peer_session_id: peer.cloned(),
                input_digest: operation
                    .as_ref()
                    .map(|operation| operation.input_digest.clone()),
                at_ms: now_ms,
                generation: state.generation,
            };
            if !holders.is_empty() {
                // A hook's tool call; a manual `admit` refused is not a write.
                // Where the workspace waits for a holder, the hook records
                // the refusal when its wait ends without an admission.
                let waits = policy::conflict_wait_ms(state, &session.workspace_id) > 0;
                return Ok(Decision {
                    events: (tool_call && !waits)
                        .then(|| {
                            refused(
                                RefusalKind::Conflict,
                                holders.first().map(|holder| &holder.session_id),
                            )
                        })
                        .into_iter()
                        .collect(),
                    outcome: Outcome::Conflict { holders },
                });
            }
            let mut base_unknown = Vec::new();
            if let Some(base) = &base {
                if let Some(extra) = base
                    .iter()
                    .find(|file| paths.binary_search(&file.path).is_err())
                {
                    return Err(error(
                        "invalid_base",
                        &format!(
                            "Base version names {:?}, which this admission does not request",
                            extra.path
                        ),
                    ));
                }
                let stale = stale_files(state, session, base);
                if !stale.is_empty() {
                    // One refusal per change. The session is told; what it was
                    // told about becomes its known version, so a host with no
                    // reported read, or a file that no longer exists to be
                    // read, cannot be refused for the same change forever.
                    return Ok(Decision {
                        events: vec![
                            Event::SourceObserved {
                                session_id: session_id.clone(),
                                files: stale
                                    .iter()
                                    .map(|file| ObservedFile {
                                        path: file.path.clone(),
                                        state: file.current.clone(),
                                    })
                                    .collect(),
                                origin: SourceOrigin::Refused,
                                at_ms: now_ms,
                                generation: state.generation,
                            },
                            refused(
                                RefusalKind::StaleBase,
                                stale.iter().find_map(|file| file.changed_by.as_ref()),
                            ),
                        ],
                        outcome: Outcome::StaleBase { stale },
                    });
                }
                let unread = lines::unread_lines(state, session, base, overwrites);
                if !unread.is_empty() {
                    // Refused once, like a stale base: what the session was
                    // told about becomes its known version, so the retry, or
                    // any edit after it reads the file, is admitted.
                    let told: Vec<ObservedFile> = base
                        .iter()
                        .filter(|file| unread.iter().any(|lines| lines.path == file.path))
                        .cloned()
                        .collect();
                    return Ok(Decision {
                        events: vec![
                            Event::SourceObserved {
                                session_id: session_id.clone(),
                                files: told,
                                origin: SourceOrigin::Refused,
                                at_ms: now_ms,
                                generation: state.generation,
                            },
                            refused(
                                RefusalKind::UnreadLines,
                                unread.first().map(|lines| &lines.session_id),
                            ),
                        ],
                        outcome: Outcome::UnreadLines { lines: unread },
                    });
                }
                let known = known_versions(state, session_id);
                base_unknown = paths
                    .iter()
                    .filter(|path| {
                        let supplied = base.iter().find(|file| &file.path == *path);
                        // A file that does not exist yet has no earlier version to miss.
                        !matches!(supplied.map(|file| &file.state), Some(FileState::Missing))
                            && !(supplied.is_some()
                                && known.iter().any(|version| &version.path == *path))
                    })
                    .cloned()
                    .collect();
            }
            let lease = Lease {
                lease_id: request_id.into(),
                session_id: session_id.clone(),
                workspace_id: session.workspace_id.clone(),
                paths,
                generation: state.generation,
                expires_at_ms,
                status: LeaseStatus::Active,
                operation,
                base_unknown,
                regions,
            };
            Ok(Decision {
                events: vec![Event::LeaseGranted {
                    lease: lease.clone(),
                }],
                outcome: Outcome::Admitted { lease },
            })
        }
        Command::Complete {
            session_id,
            lease_id,
        } => {
            current_session(state, session_id)?;
            validate_id(lease_id, "lease ID")?;
            let lease = state.leases.get(lease_id).ok_or_else(|| {
                error(
                    "lease_not_found",
                    "Lease does not exist; inspect status for the active lease ID",
                )
            })?;
            if lease.session_id != *session_id {
                return Err(error(
                    "not_holder",
                    "Only the session holding this lease can complete it",
                ));
            }
            if lease.generation != state.generation || lease.status == LeaseStatus::Invalidated {
                return Err(error(
                    "stale_lease",
                    "The lease belongs to a previous daemon generation; request a new admission",
                ));
            }
            if lease.status == LeaseStatus::Expired
                || (lease.status == LeaseStatus::Active && lease.expires_at_ms <= now_ms)
            {
                return Err(error(
                    "lease_expired",
                    "The lease has expired; request a new admission before continuing work",
                ));
            }
            if lease.status != LeaseStatus::Active {
                return Err(error(
                    "lease_not_active",
                    "The lease is no longer active; replay completion through storage or request a new admission",
                ));
            }
            Ok(Decision {
                events: vec![Event::LeaseCompleted {
                    lease_id: lease_id.clone(),
                }],
                outcome: Outcome::Completed {
                    lease_id: lease_id.clone(),
                },
            })
        }
        Command::Observe {
            session_id,
            operation_id,
            source,
            tool,
            paths,
            result,
            lease_id,
        } => decide_observation(
            state,
            ObservationInput {
                session_id,
                operation_id,
                source,
                tool,
                paths,
                result: *result,
                lease_id: lease_id.as_deref(),
                operation: None,
            },
            request_id,
            now_ms,
        ),
        Command::ObserveTool {
            session_id,
            operation,
            paths,
            result,
            lease_id,
        } => decide_observation(
            state,
            ObservationInput {
                session_id,
                operation_id: &operation.operation_id,
                source: &operation.source,
                tool: &operation.tool,
                paths,
                result: *result,
                lease_id: lease_id.as_deref(),
                operation: Some(operation),
            },
            request_id,
            now_ms,
        ),
        Command::ObserveDisk {
            workspace_id,
            path,
            before,
            after,
            related_operation_ids,
        } => {
            let observation = DiskObservation {
                observation_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                path: path.clone(),
                before: before.clone(),
                after: after.clone(),
                related_operation_ids: normalized_related_ids(related_operation_ids)?,
                observed_at_ms: now_ms,
                generation: state.generation,
            };
            validate_disk_observation(state, &observation)?;
            Ok(Decision {
                events: vec![Event::DiskObserved {
                    observation: observation.clone(),
                }],
                outcome: Outcome::DiskObserved { observation },
            })
        }
        Command::ObserveBranch {
            workspace_id,
            branch_ref,
            head_oid,
            tree_oid,
        } => {
            let observation = BranchObservation {
                observation_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                branch_ref: branch_ref.clone(),
                head_oid: head_oid.clone(),
                tree_oid: tree_oid.clone(),
                observed_at_ms: now_ms,
                generation: state.generation,
            };
            validate_branch_observation(state, &observation)?;
            Ok(Decision {
                events: vec![Event::BranchObserved {
                    observation: observation.clone(),
                }],
                outcome: Outcome::BranchObserved { observation },
            })
        }
        Command::ObserveWorkspaceGit {
            workspace_id,
            repository_id,
            branch_ref,
        } => {
            let git = WorkspaceGit {
                workspace_id: workspace_id.clone(),
                repository_id: repository_id.clone(),
                branch_ref: branch_ref.clone(),
            };
            validate_workspace_git(state, &git)?;
            let events = if state.workspace_git.get(workspace_id) == Some(&git) {
                Vec::new()
            } else {
                vec![Event::WorkspaceGitObserved { git: git.clone() }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::WorkspaceGit { git },
            })
        }
        // The summary is the adapter's one line; the digests let it learn,
        // without any peer's words, whether a live peer's prompt began the
        // same way, so that it can name this prompt by where it differs.
        Command::SessionPrompt {
            session_id,
            summary,
            line_digests,
        } => {
            current_session(state, session_id)?;
            validate_prompt_summary(summary)?;
            validate_line_digests(line_digests)?;
            Ok(Decision {
                events: vec![Event::SessionPrompted {
                    session_id: session_id.clone(),
                    summary: summary.clone(),
                    line_digests: line_digests.clone(),
                    at_ms: now_ms,
                    generation: state.generation,
                }],
                outcome: Outcome::SessionPrompted {
                    session_id: session_id.clone(),
                    shared_lines: shared_prompt_lines(state, session_id, line_digests),
                },
            })
        }
        Command::SetPromptTargets { session_id, paths } => {
            current_session(state, session_id)?;
            let paths = prompt_target_paths(paths)?;
            // Recorded again when unchanged: naming the same code again
            // renews how recent the targets are.
            Ok(Decision {
                events: vec![Event::PromptTargetsSet {
                    session_id: session_id.clone(),
                    paths: paths.clone(),
                    at_ms: now_ms,
                    generation: state.generation,
                }],
                outcome: Outcome::PromptTargets {
                    session_id: session_id.clone(),
                    paths,
                },
            })
        }
        Command::ObservePresence {
            session_id,
            kind,
            paths,
        } => {
            current_session(state, session_id)?;
            let paths = presence_paths(*kind, paths)?;
            if !presence_setting(state, session_id).shown {
                return Err(error(
                    "presence_off",
                    "This session's presence is off; nothing it looks at is recorded",
                ));
            }
            Ok(Decision {
                events: vec![Event::PresenceObserved {
                    session_id: session_id.clone(),
                    kind: *kind,
                    paths: paths.clone(),
                    at_ms: now_ms,
                    generation: state.generation,
                }],
                outcome: Outcome::PresenceObserved {
                    session_id: session_id.clone(),
                    paths: u32::try_from(paths.len()).unwrap_or(u32::MAX),
                },
            })
        }
        Command::SetPresence {
            session_id,
            shown,
            window_ms,
        } => {
            known_session(state, session_id)?;
            let setting = PresenceSetting {
                shown: *shown,
                window_ms: *window_ms,
            };
            validate_presence_setting(&setting)?;
            let events = if presence_setting(state, session_id) == setting {
                Vec::new()
            } else {
                vec![Event::PresenceSet {
                    session_id: session_id.clone(),
                    setting,
                }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::Presence {
                    session_id: session_id.clone(),
                    setting,
                },
            })
        }
        Command::SessionEnd { session_id, trail } => {
            let session = known_session(state, session_id)?;
            if let Some(facts) = trail {
                trail::validate_facts(facts)?;
            }
            let ending = state
                .activity
                .get(session_id)
                .is_none_or(|activity| !activity.ended);
            let mut events = if ending || state.intents.contains_key(session_id) {
                vec![Event::SessionEnded {
                    session_id: session_id.clone(),
                }]
            } else {
                Vec::new()
            };
            // A trail is built once per end, from the state before it.
            if ending && let Some(trail) = trail::build(state, session, trail.as_ref(), now_ms) {
                events.push(Event::TrailRecorded { trail });
            }
            Ok(Decision {
                events,
                outcome: Outcome::SessionEnded {
                    session_id: session_id.clone(),
                },
            })
        }
        Command::SessionStop {
            session_id,
            failure,
            prompt_id,
        } => {
            known_session(state, session_id)?;
            if let Some(failure) = failure {
                validate_failure(failure)?;
            }
            if let Some(prompt_id) = prompt_id {
                validate_request_id(prompt_id)?;
            }
            // Only what the session's own tool calls hold, in this
            // generation, and not already past its deadline (that is the
            // expiry sweep's). An admission asked for by hand is not part of
            // a reply and stays until it is completed or expires.
            let released: Vec<String> = state
                .leases
                .values()
                .filter(|lease| {
                    lease.session_id == *session_id
                        && lease.status == LeaseStatus::Active
                        && lease.generation == state.generation
                        && lease.operation.is_some()
                        && lease.expires_at_ms > now_ms
                })
                .map(|lease| lease.lease_id.clone())
                .collect();
            let mut events: Vec<Event> = released
                .iter()
                .map(|lease_id| Event::LeaseCompleted {
                    lease_id: lease_id.clone(),
                })
                .collect();
            events.push(Event::SessionStopped {
                session_id: session_id.clone(),
                released: released.clone(),
                failure: failure.clone(),
                at_ms: now_ms,
                generation: state.generation,
                prompt_id: prompt_id.clone(),
            });
            Ok(Decision {
                events,
                outcome: Outcome::SessionStopped {
                    session_id: session_id.clone(),
                    released,
                    wrote: wrote_since_boundary(state, session_id),
                },
            })
        }
        Command::AcknowledgeInterface {
            session_id,
            notice_ids,
        } => {
            current_session(state, session_id)?;
            validate_alerted(notice_ids)?;
            Ok(Decision {
                events: vec![Event::InterfaceAlerted {
                    session_id: session_id.clone(),
                    notice_ids: notice_ids.clone(),
                }],
                outcome: Outcome::InterfaceAcknowledged {
                    session_id: session_id.clone(),
                    notice_ids: notice_ids.clone(),
                },
            })
        }
        Command::InterfaceShown {
            session_id,
            notice_ids,
        } => {
            current_session(state, session_id)?;
            validate_told(notice_ids)?;
            // A notice told before, either way, is not recorded twice.
            let newly_told: Vec<String> = notice_ids
                .iter()
                .filter(|id| !told_notices(state, session_id).any(|told| told.notice_id == **id))
                .cloned()
                .collect();
            let events = if newly_told.is_empty() {
                Vec::new()
            } else {
                vec![Event::InterfaceNoticesTold {
                    session_id: session_id.clone(),
                    notice_ids: newly_told,
                    via: NoticeVia::Briefing,
                }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::InterfaceAcknowledged {
                    session_id: session_id.clone(),
                    notice_ids: notice_ids.clone(),
                },
            })
        }
        Command::BetweenSteps {
            session_id,
            stops,
            prompt_id,
        } => {
            let me = known_session(state, session_id)?;
            if let Some(prompt_id) = prompt_id {
                validate_request_id(prompt_id)?;
            }
            let activity = state
                .activity
                .get(session_id)
                .filter(|activity| activity.generation == state.generation);
            let now_stops = activity.map_or(0, |activity| activity.stops);
            // The watcher's turn is over once its own prompt's reply
            // finished, whenever its first answer came, or once any reply
            // finished after its first ask; an ended or earlier-generation
            // session has none.
            let open = me.generation == state.generation
                && !activity.is_some_and(|activity| activity.ended)
                && !prompt_id.as_ref().is_some_and(|prompt| {
                    activity.is_some_and(|activity| activity.stopped_prompts.contains(prompt))
                })
                && stops.is_none_or(|stops| stops == now_stops);
            let (notice_ids, interface_changes): (Vec<String>, Vec<InterfaceImpact>) = if open {
                let alerted = activity.map_or(&[][..], |activity| activity.alerted.as_slice());
                let concerned = concerned_paths(state, session_id, now_ms);
                interface_impacts(state, me, &concerned, now_ms)
                    .filter(|(id, _)| {
                        !alerted.contains(id)
                            && !told_notices(state, session_id).any(|told| told.notice_id == *id)
                    })
                    .take(BRIEF_INTERFACE_NOTICES)
                    .unzip()
            } else {
                (Vec::new(), Vec::new())
            };
            let events = if notice_ids.is_empty() {
                Vec::new()
            } else {
                vec![Event::InterfaceNoticesTold {
                    session_id: session_id.clone(),
                    notice_ids,
                    via: NoticeVia::BetweenSteps,
                }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::BetweenSteps {
                    session_id: session_id.clone(),
                    open,
                    stops: now_stops,
                    interface_changes,
                },
            })
        }
        Command::ObserveToolBatch {
            session_id,
            operation_ids,
            calls,
        } => {
            current_session(state, session_id)?;
            validate_batch(operation_ids, *calls)?;
            Ok(Decision {
                events: vec![Event::ToolBatchObserved {
                    session_id: session_id.clone(),
                    operation_ids: operation_ids.clone(),
                    calls: *calls,
                    at_ms: now_ms,
                    generation: state.generation,
                }],
                outcome: Outcome::ToolBatchObserved {
                    session_id: session_id.clone(),
                    calls: *calls,
                    wrote: wrote_since_boundary(state, session_id),
                },
            })
        }
        Command::ObserveSource {
            session_id,
            files,
            origin,
        } => {
            current_session(state, session_id)?;
            let files = normalized_files(files)?;
            if files.is_empty() {
                return Err(error(
                    "invalid_path",
                    "Name at least one observed file version",
                ));
            }
            let count = u32::try_from(files.len()).unwrap_or(u32::MAX);
            Ok(Decision {
                events: vec![Event::SourceObserved {
                    session_id: session_id.clone(),
                    files,
                    origin: *origin,
                    at_ms: now_ms,
                    generation: state.generation,
                }],
                outcome: Outcome::SourceObserved {
                    session_id: session_id.clone(),
                    files: count,
                },
            })
        }
        Command::RemoteEnable {
            workspace_id,
            remote,
            participant,
            name,
            share_task,
            publish_as_branch,
        } => {
            let previous = state.remote_links.get(workspace_id);
            // Re-enabling as the same participant continues its sequence, so
            // teammates never see it go backwards.
            let (sequence, signature) = previous
                .filter(|link| &link.participant == participant)
                .map(|link| (link.sequence, link.signature.clone()))
                .unwrap_or_default();
            let link = RemoteLink {
                workspace_id: workspace_id.clone(),
                remote: remote.clone(),
                participant: participant.clone(),
                name: name.clone(),
                share_task: *share_task,
                sequence,
                // An empty fingerprint makes the first pass after enabling
                // publish, whatever was last recorded, and so does a move to
                // the other namespace: the new ref must be written at once.
                signature: if previous.is_some_and(|link| {
                    link.enabled && link.publish_as_branch == *publish_as_branch
                }) {
                    signature
                } else {
                    String::new()
                },
                enabled: true,
                withdrawn: false,
                publish_as_branch: *publish_as_branch,
            };
            validate_remote_link(state, &link)?;
            // One ref per participant per repository: two worktrees of one
            // repository publishing separately would overwrite each other.
            if let Some(other) = state.remote_links.values().find(|other| {
                other.enabled
                    && other.workspace_id != link.workspace_id
                    && related_workspace(state, &link.workspace_id, &other.workspace_id)
            }) {
                return Err(error(
                    "already_published",
                    &format!(
                        "This repository already publishes through workspace {:?}, which covers its worktrees",
                        other.workspace_id
                    ),
                ));
            }
            let events = if previous == Some(&link) {
                Vec::new()
            } else {
                vec![Event::RemoteEnabled { link: link.clone() }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::RemoteLink { link },
            })
        }
        Command::RemoteDisable { workspace_id } => {
            validate_id(workspace_id, "workspace ID")?;
            let events = if state
                .remote_links
                .get(workspace_id)
                .is_some_and(|link| link.enabled)
            {
                vec![Event::RemoteDisabled {
                    workspace_id: workspace_id.clone(),
                }]
            } else {
                Vec::new()
            };
            Ok(Decision {
                events,
                outcome: Outcome::RemoteDisabled {
                    workspace_id: workspace_id.clone(),
                },
            })
        }
        Command::RemoteOutbox { workspace_id } => {
            let link = remote_link(state, workspace_id)?;
            let (publication, signature) = publication(state, link, now_ms);
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::RemoteOutbox {
                    changed: signature != link.signature,
                    publication,
                    signature,
                },
            })
        }
        Command::RemotePublished {
            workspace_id,
            sequence,
            signature,
            payload,
            published_at_ms,
            as_branch,
        } => {
            // A disabled workspace records too: its final "withdrawn"
            // publication, sent where the host refused to delete its ref.
            validate_id(workspace_id, "workspace ID")?;
            let link = state
                .remote_links
                .get(workspace_id)
                .ok_or_else(|| error("remote_not_enabled", "This workspace has never published"))?;
            if *sequence != link.sequence + 1 || signature.len() > 64 {
                return Err(error(
                    "invalid_sequence",
                    "A publication record must carry the next sequence for its workspace",
                ));
            }
            if payload.len() > PUBLICATION_BYTES {
                return Err(error(
                    "invalid_payload",
                    "A publication record holds at most 16 KiB of payload",
                ));
            }
            // A push to a destination the owner has since left still uses up
            // its sequence, but does not stand for the current one: the
            // signature stays unset so the next pass publishes there.
            let signature = match as_branch {
                Some(as_branch) if *as_branch != link.publish_as_branch => String::new(),
                _ => signature.clone(),
            };
            let mut link = link.clone();
            link.sequence = *sequence;
            link.signature = signature.clone();
            Ok(Decision {
                events: vec![Event::RemotePublished {
                    workspace_id: workspace_id.clone(),
                    sequence: *sequence,
                    signature,
                    payload: (!payload.is_empty()).then(|| payload.clone()),
                    published_at_ms: (*published_at_ms != 0).then_some(*published_at_ms),
                }],
                outcome: Outcome::RemoteLink { link },
            })
        }
        Command::ObserveRemote {
            workspace_id,
            publication,
        } => {
            let link = remote_link(state, workspace_id)?;
            validate_publication(publication)?;
            if publication.participant == link.participant {
                return Err(error(
                    "own_publication",
                    "A workspace does not observe its own publication",
                ));
            }
            let key = remote_key(workspace_id, &publication.participant);
            // A sender's sequence only rises. Within one generation an equal
            // or lower one is a replay or a reordering and changes nothing.
            let newer = state.remote_peers.get(&key).is_none_or(|held| {
                held.generation != state.generation
                    || held.publication.sequence < publication.sequence
            });
            // A teammate who left, whose host kept the ref: shown as gone.
            if publication.withdrawn {
                let held = state.remote_peers.contains_key(&key);
                return Ok(Decision {
                    events: if held && newer {
                        vec![Event::RemoteForgotten {
                            workspace_id: workspace_id.clone(),
                            participant: publication.participant.clone(),
                        }]
                    } else {
                        Vec::new()
                    },
                    outcome: Outcome::RemoteObserved {
                        participant: publication.participant.clone(),
                        accepted: held && newer,
                    },
                });
            }
            let events = if newer {
                vec![Event::RemoteObserved {
                    peer: RemotePeer {
                        workspace_id: workspace_id.clone(),
                        publication: publication.clone(),
                        received_ms: now_ms,
                        generation: state.generation,
                    },
                }]
            } else {
                Vec::new()
            };
            Ok(Decision {
                events,
                outcome: Outcome::RemoteObserved {
                    participant: publication.participant.clone(),
                    accepted: newer,
                },
            })
        }
        Command::RemoteWithdrawn { workspace_id } => {
            validate_id(workspace_id, "workspace ID")?;
            let link = state
                .remote_links
                .get(workspace_id)
                .ok_or_else(|| error("remote_not_enabled", "This workspace has never published"))?;
            let pending = !link.enabled && !link.withdrawn;
            Ok(Decision {
                events: if pending {
                    vec![Event::RemoteWithdrawn {
                        workspace_id: workspace_id.clone(),
                    }]
                } else {
                    Vec::new()
                },
                outcome: Outcome::RemoteDisabled {
                    workspace_id: workspace_id.clone(),
                },
            })
        }
        Command::RecordCheck {
            workspace_id,
            name,
            command,
            tree_oid,
            head_oid,
            inputs,
            result,
            duration_ms,
            report,
            tests,
            rerun,
            writes_before,
            command_id,
            symbol_findings,
        } => {
            let check = CheckRecord {
                check_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                name: name.clone(),
                command: command.clone(),
                tree_oid: tree_oid.clone(),
                head_oid: head_oid.clone(),
                inputs: inputs.clone(),
                result: result.clone(),
                duration_ms: *duration_ms,
                recorded_ms: now_ms,
                generation: state.generation,
                changed_since: false,
                symbol_findings: symbol_findings.clone(),
            };
            validate_check(state, &check)?;
            // Kept with the record so that a later run can be compared with
            // it; validated with the test results below.
            let kept = match (tests, *writes_before, command_id) {
                (Some(results), Some(writes_before), Some(command_id)) => Some(KeptTests {
                    results: results.clone(),
                    writes_before,
                    command_id: command_id.clone(),
                }),
                _ => None,
            };
            // The write generation the run took before its tree. A write
            // recorded since, even one undone before the run (or its rerun)
            // ended, means its results are not of unchanged code: they are
            // checked but learned from by neither this record nor a later
            // one, and are no baseline (a write counted between the count
            // and the tree would be in the tree it tested).
            let writes = state.writes.get(workspace_id).copied().unwrap_or(0);
            let unchanged = *writes_before == Some(writes);
            let mut events = vec![Event::CheckRecorded {
                check: check.clone(),
                tests: kept.clone().filter(|_| unchanged),
            }];
            // What this record teaches about tests that flip, learned on a
            // copy so that its notice is labelled with it.
            let mut learning = state
                .test_learning
                .get(workspace_id)
                .cloned()
                .unwrap_or_default();
            if rerun.is_some() && tests.is_none() {
                return Err(error(
                    "invalid_test_results",
                    "A rerun's test results come with the record's own",
                ));
            }
            if tests.is_some() && writes_before.is_none() {
                return Err(error(
                    "invalid_test_results",
                    "Test results come with the workspace's write generation taken before the run's tree (writes_before)",
                ));
            }
            let command_id = match (tests, command_id) {
                (None, None) => "",
                (Some(_), Some(id)) if flaky::is_command_id(id) => id.as_str(),
                _ => {
                    return Err(error(
                        "invalid_test_results",
                        "Test results come with the command's identity (command_id: 64 lowercase hex digits), and only they do",
                    ));
                }
            };
            let runs = tests
                .iter()
                .map(|results| (results, false))
                .chain(rerun.iter().map(|results| (results, true)));
            for (results, again) in runs {
                if !inputs.is_empty() || matches!(result, CheckResult::Inconclusive {}) {
                    return Err(error(
                        "invalid_test_results",
                        "Test results belong to a working-tree check that passed or failed",
                    ));
                }
                let passed = if again {
                    results.all_passed
                } else {
                    matches!(result, CheckResult::Passed {})
                };
                flaky::validate_test_results(results, passed)?;
                if !unchanged {
                    continue;
                }
                learning.learn(name, tree_oid, command_id, writes, results, now_ms);
                events.push(Event::TestsRecorded {
                    workspace_id: workspace_id.clone(),
                    check_id: check.check_id.clone(),
                    name: name.clone(),
                    tree_oid: tree_oid.clone(),
                    command: command.clone(),
                    command_id: command_id.to_owned(),
                    results: results.clone(),
                    rerun: again,
                    at_ms: now_ms,
                });
            }
            let notice = |comparison: Option<&classes::Comparison>| CheckNotice {
                check_id: check.check_id.clone(),
                name: check.name.clone(),
                tree_oid: check.tree_oid.clone(),
                result: check.result.clone(),
                duration_ms: check.duration_ms,
                timed_out_s: report.as_ref().and_then(|report| report.timed_out_s),
                tail: report
                    .as_ref()
                    .map(|report| report.tail.clone())
                    .unwrap_or_default(),
                failures: tests
                    .iter()
                    .flat_map(|results| &results.failed)
                    .enumerate()
                    .map(|(at, test)| TestFailure {
                        test: test.clone(),
                        flaky: learning.label(name, test),
                        class: comparison
                            .and_then(|comparison| comparison.classes.get(at).copied()),
                    })
                    .collect(),
                more_failures: tests.as_ref().is_some_and(|results| results.more_failed),
                baseline_check_id: comparison
                    .map(|comparison| comparison.baseline_check_id.clone()),
                fixed: comparison.map_or_else(Vec::new, |comparison| comparison.fixed.clone()),
                writers: comparison.map_or_else(Vec::new, |comparison| comparison.writers.clone()),
                interface_grade: interface_grade(
                    report
                        .as_ref()
                        .and_then(|report| report.interface.as_deref()),
                    &check.result,
                ),
                interface: report.as_ref().and_then(|report| report.interface.clone()),
                index_incomplete: report
                    .as_ref()
                    .is_some_and(|report| report.index_incomplete),
            };
            if let Some(report) = report {
                let mut session_ids = report.session_ids.clone();
                validate_dependent_paths(report)?;
                if let Some(cause) = &report.interface {
                    validate_interface_cause(cause)?;
                    // The sessions working on any mentioning file are told
                    // too, not only those on the files the notice shows,
                    // decided here so that replay gives the same set.
                    let mut paths = cause.dependents.clone();
                    paths.extend(report.dependent_paths.iter().cloned());
                    for peer in concerned_sessions(state, &check.workspace_id, &paths, now_ms) {
                        if session_ids.len() >= MAX_CHECK_REPORT_SESSIONS {
                            break;
                        }
                        if !session_ids.contains(&peer) {
                            session_ids.push(peer);
                        }
                    }
                }
                validate_check_report(
                    state,
                    &check.workspace_id,
                    &Event::CheckReported {
                        session_ids: session_ids.clone(),
                        notice: notice(None),
                    },
                )?;
                // Each session's failures are compared with its own baseline:
                // one report per group of sessions told the same.
                let groups = classes::group(&session_ids, |session| {
                    classes::compare(state, workspace_id, name, kept.as_ref()?, session)
                });
                for (comparison, session_ids) in groups {
                    let reported = Event::CheckReported {
                        session_ids,
                        notice: notice(comparison.as_ref()),
                    };
                    validate_check_report(state, &check.workspace_id, &reported)?;
                    events.push(reported);
                }
            }
            Ok(Decision {
                events,
                outcome: Outcome::CheckRecorded { check },
            })
        }
        Command::FlakyTests { workspace_id, name } => {
            validate_known_workspace(state, workspace_id)?;
            let (tests, more) = state
                .test_learning
                .get(workspace_id)
                .map(|learning| learning.flaky(name.as_deref(), now_ms))
                .unwrap_or_default();
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::FlakyTests {
                    workspace_id: workspace_id.clone(),
                    tests,
                    more,
                },
            })
        }
        Command::ForgetFlakyTest {
            workspace_id,
            name,
            test,
        } => {
            validate_known_workspace(state, workspace_id)?;
            validate_forget_flaky(name.as_deref(), test)?;
            let forgotten = state
                .test_learning
                .get(workspace_id)
                .is_some_and(|learning| learning.follows(name.as_deref(), test));
            Ok(Decision {
                events: if forgotten {
                    vec![Event::FlakyTestForgotten {
                        workspace_id: workspace_id.clone(),
                        name: name.clone(),
                        test: test.clone(),
                    }]
                } else {
                    Vec::new()
                },
                outcome: Outcome::FlakyTestForgotten {
                    workspace_id: workspace_id.clone(),
                    test: test.clone(),
                    forgotten,
                },
            })
        }
        Command::AutoCheckEnable {
            workspace_id,
            name,
            argv,
            timeout_s,
            on_interface_change,
        } => {
            let check = AutoCheck {
                workspace_id: workspace_id.clone(),
                name: name.clone(),
                argv: argv.clone(),
                timeout_s: *timeout_s,
                on_interface_change: on_interface_change.as_deref().cloned(),
            };
            validate_auto_check(state, &check)?;
            let events = if state.auto_checks.get(workspace_id) == Some(&check) {
                Vec::new()
            } else {
                vec![Event::AutoCheckEnabled {
                    check: check.clone(),
                }]
            };
            Ok(Decision {
                events,
                outcome: Outcome::AutoCheck { check },
            })
        }
        Command::AutoCheckDisable { workspace_id } => {
            validate_known_workspace(state, workspace_id)?;
            let events = if state.auto_checks.contains_key(workspace_id) {
                vec![Event::AutoCheckDisabled {
                    workspace_id: workspace_id.clone(),
                }]
            } else {
                Vec::new()
            };
            Ok(Decision {
                events,
                outcome: Outcome::AutoCheckDisabled {
                    workspace_id: workspace_id.clone(),
                },
            })
        }
        Command::CheckStatus {
            workspace_id,
            tree_oid,
        } => {
            validate_known_workspace(state, workspace_id)?;
            if let Some(tree) = tree_oid {
                validate_oid(tree)?;
            }
            let checks = state
                .checks
                .get(workspace_id)
                .map(|records| {
                    records
                        .iter()
                        .map(|record| CheckView {
                            applicability: match tree_oid {
                                Some(tree) if *tree == record.tree_oid => Applicability::Current,
                                Some(_) => Applicability::Stale,
                                None if record.changed_since => Applicability::Stale,
                                None => Applicability::Unknown,
                            },
                            record: record.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Checks { checks },
            })
        }
        Command::LandingReadiness {
            workspace_id,
            name,
            tree_oid,
        } => {
            validate_known_workspace(state, workspace_id)?;
            validate_id(name, "check name")?;
            validate_oid(tree_oid)?;
            let (ready, reason, check) = readiness(state, workspace_id, name, tree_oid);
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Readiness {
                    ready,
                    reason,
                    check,
                },
            })
        }
        Command::RecordLanding {
            workspace_id,
            onto,
            commit_oid,
            tree_oid,
            inputs,
            check_id,
            rebased,
            adapted,
        } => {
            let landing = Landing {
                landing_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                onto: onto.clone(),
                commit_oid: commit_oid.clone(),
                tree_oid: tree_oid.clone(),
                inputs: inputs.clone(),
                check_id: check_id.clone(),
                rebased: rebased.clone(),
                adapted: adapted.clone(),
            };
            validate_landing(state, &landing)?;
            Ok(Decision {
                events: vec![Event::Landed {
                    landing: landing.clone(),
                }],
                outcome: Outcome::Landed { landing },
            })
        }
        Command::TakeTurn {
            workspace_id,
            onto,
            session_id,
            ttl_ms,
            turn_id,
            queue,
        } => {
            validate_known_workspace(state, workspace_id)?;
            validate_branch_name(onto)?;
            if let Some(session) = session_id {
                validate_id(session, "session ID")?;
            }
            if !(1..=MAX_TURN_TTL_MS).contains(ttl_ms) {
                return Err(error(
                    "invalid_ttl",
                    "A push turn lasts 1 ms to 4 hours; a landing that needs longer must be split",
                ));
            }
            let key = turn_key(workspace_id, onto);
            let expires_at_ms = now_ms
                .checked_add(*ttl_ms)
                .ok_or_else(|| error("invalid_ttl", "The turn's deadline is beyond the clock"))?;
            let live = state
                .turns
                .get(&key)
                .filter(|held| held.expires_at_ms > now_ms);
            if let Some(queueing) = queue {
                if turn_id.is_some() {
                    return Err(error(
                        "invalid_turn",
                        "A landing renews the turn it holds or waits for one, not both",
                    ));
                }
                return queue_for_turn(
                    state,
                    workspace_id,
                    onto,
                    session_id,
                    expires_at_ms,
                    queueing,
                    now_ms,
                );
            }
            if let Some(renewing) = turn_id {
                validate_request_id(renewing)?;
                let Some(held) = live.filter(|held| held.turn_id == *renewing) else {
                    return Err(error(
                        "turn_lost",
                        "This landing's push turn has ended (it expired or the daemon restarted); nothing more may be pushed under it. Start the landing again",
                    ));
                };
                let mut turn = held.clone();
                turn.expires_at_ms = turn.expires_at_ms.max(expires_at_ms);
                return Ok(Decision {
                    events: vec![Event::TurnRenewed {
                        workspace_id: workspace_id.clone(),
                        onto: onto.clone(),
                        turn_id: renewing.clone(),
                        expires_at_ms: turn.expires_at_ms,
                    }],
                    outcome: Outcome::Turn { turn },
                });
            }
            let mut events = Vec::new();
            // Nobody jumps the line: with landings waiting, only the first
            // of them takes a free turn.
            if live.is_none() {
                let waiting = waiting_for(state, &key, now_ms);
                if !waiting.is_empty() {
                    return Ok(Decision {
                        events,
                        outcome: Outcome::TurnUnavailable {
                            holder: None,
                            held_ms: None,
                            waiting: line(&waiting, now_ms),
                            queue_full: false,
                        },
                    });
                }
            }
            if let Some(held) = state.turns.get(&key) {
                if held.expires_at_ms > now_ms {
                    return Ok(Decision {
                        events,
                        outcome: if held.turn_id == request_id {
                            Outcome::Turn { turn: held.clone() }
                        } else {
                            Outcome::TurnBusy {
                                holder: held.clone(),
                                held_ms: now_ms.saturating_sub(held.taken_ms),
                            }
                        },
                    });
                }
                // Past its deadline: it ends before the next one begins.
                events.push(Event::TurnExpired {
                    workspace_id: workspace_id.clone(),
                    onto: onto.clone(),
                    turn_id: held.turn_id.clone(),
                });
            }
            let turn = Turn {
                turn_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                onto: onto.clone(),
                session_id: session_id.clone(),
                taken_ms: now_ms,
                expires_at_ms,
                generation: state.generation,
            };
            events.push(Event::TurnTaken { turn: turn.clone() });
            Ok(Decision {
                events,
                outcome: Outcome::Turn { turn },
            })
        }
        Command::LeaveTurnQueue {
            workspace_id,
            onto,
            ticket,
            reason,
        } => {
            validate_known_workspace(state, workspace_id)?;
            validate_branch_name(onto)?;
            validate_request_id(ticket)?;
            if *reason == LeftQueue::Stopped {
                return Err(error(
                    "invalid_reason",
                    "A landing is taken out of the queue as cancelled or timed out; the coordinator decides when one has stopped",
                ));
            }
            let left = state
                .turn_queues
                .get(&turn_key(workspace_id, onto))
                .is_some_and(|queue| queue.iter().any(|waiter| waiter.ticket == *ticket));
            Ok(Decision {
                events: if left {
                    vec![Event::TurnQueueLeft {
                        workspace_id: workspace_id.clone(),
                        onto: onto.clone(),
                        ticket: ticket.clone(),
                        reason: *reason,
                    }]
                } else {
                    Vec::new()
                },
                outcome: Outcome::TurnQueueLeft {
                    ticket: ticket.clone(),
                    left,
                },
            })
        }
        Command::TurnQueue { workspace_id, onto } => {
            validate_known_workspace(state, workspace_id)?;
            validate_branch_name(onto)?;
            let key = turn_key(workspace_id, onto);
            let holder = state
                .turns
                .get(&key)
                .filter(|held| held.expires_at_ms > now_ms)
                .cloned();
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::TurnQueue {
                    held_ms: holder
                        .as_ref()
                        .map(|held| now_ms.saturating_sub(held.taken_ms)),
                    holder,
                    waiting: line(&waiting_for(state, &key, now_ms), now_ms),
                },
            })
        }
        Command::ReleaseTurn {
            workspace_id,
            onto,
            turn_id,
            attempts,
        } => {
            validate_known_workspace(state, workspace_id)?;
            validate_branch_name(onto)?;
            validate_request_id(turn_id)?;
            validate_attempts(attempts)?;
            let released = state
                .turns
                .get(&turn_key(workspace_id, onto))
                .is_some_and(|held| held.turn_id == *turn_id);
            Ok(Decision {
                events: vec![Event::TurnReleased {
                    workspace_id: workspace_id.clone(),
                    onto: onto.clone(),
                    turn_id: turn_id.clone(),
                    attempts: attempts.clone(),
                }],
                outcome: Outcome::TurnReleased {
                    turn_id: turn_id.clone(),
                    released,
                },
            })
        }
        Command::ForgetRemote {
            workspace_id,
            participant,
        } => {
            remote_link(state, workspace_id)?;
            validate_participant(participant, "-")?;
            let held = state
                .remote_peers
                .contains_key(&remote_key(workspace_id, participant));
            Ok(Decision {
                events: if held {
                    vec![Event::RemoteForgotten {
                        workspace_id: workspace_id.clone(),
                        participant: participant.clone(),
                    }]
                } else {
                    Vec::new()
                },
                outcome: Outcome::RemoteObserved {
                    participant: participant.clone(),
                    accepted: held,
                },
            })
        }
        Command::Adopt { session_id, from } => adopt(state, session_id, from, now_ms),
        Command::Brief {
            session_id,
            max_peers,
            deliver,
            mid_turn,
            left,
            recent,
            recent_window_ms,
            mcp,
            changes_only,
        }
        | Command::BriefPrepared {
            session_id,
            max_peers,
            deliver,
            mid_turn,
            left,
            recent,
            recent_window_ms,
            mcp,
            changes_only,
            ..
        } => {
            if !(1..=MAX_BRIEF_PEERS).contains(max_peers) {
                return Err(error(
                    "invalid_limit",
                    "A briefing names between 1 and 16 peers",
                ));
            }
            if *recent && *mid_turn {
                return Err(error(
                    "invalid_request",
                    "What changed recently is told at a session's start, never mid-turn; leave out recent",
                ));
            }
            let window = recent::window(*recent_window_ms)?;
            let none = BTreeSet::new();
            let (inflight, mentioned) = match command {
                Command::BriefPrepared {
                    inflight,
                    mentioned,
                    ..
                } => {
                    if inflight.len() > INFLIGHT_CANDIDATES {
                        return Err(error(
                            "invalid_limit",
                            "A briefing is offered at most 32 in-flight symbols",
                        ));
                    }
                    (inflight.as_slice(), mentioned)
                }
                _ => (&[][..], &none),
            };
            let (mut briefing, signature, inflight_keys) = brief(
                state,
                session_id,
                *max_peers,
                !*mid_turn,
                now_ms,
                (inflight, mentioned),
            )?;
            briefing.left_work = left_work(state, session_id, left)?;
            // Not part of the signature: a prompt's briefing never carries
            // it, so it must not make the next one look new.
            if *recent {
                let me = current_session(state, session_id)?;
                briefing.recent = Some(recent::recent(state, me, window, now_ms));
            }
            let previous = state
                .activity
                .get(session_id)
                .and_then(|activity| activity.briefed.as_deref())
                .unwrap_or(EMPTY_SIGNATURE);
            // Delivered messages are acknowledged below and so are absent
            // from the next briefing: the remembered signature leaves them
            // out, and any message at all makes this briefing new.
            // Presence is news only at a session start or prompt, and only
            // for a path not shown before. Mid-turn, or when some or all of
            // it merely aged out, it is not (see `brief`).
            let differs = without_presence(&signature) != without_presence(previous)
                || (!*mid_turn && new_presence(&signature, previous));
            let once = !briefing.messages.is_empty()
                || !briefing.uncoordinated.is_empty()
                || !briefing.retracted.is_empty()
                || !briefing.checks.is_empty()
                || !briefing.left_work.is_empty()
                || briefing.trail.is_some();
            // What the session was told, and when: from this generation
            // only, since times from an earlier one do not compare.
            let activity = state
                .activity
                .get(session_id)
                .filter(|activity| activity.generation == state.generation);
            let told: BTreeSet<u64> = activity
                .map(|activity| activity.told.iter().copied().collect())
                .unwrap_or_default();
            let keys = briefing_keys(&briefing);
            let (told_next, shown) = if *changes_only {
                let since = activity.map_or(0, |activity| activity.told_at_ms);
                // The session read or edited `path` after it was last told:
                // a safety item about the file is news again. A read in the
                // same millisecond as that briefing counts as told with it:
                // the clock cannot order the two, and usually the read is
                // the hook run that carried the briefing.
                let acted = |path: &str| {
                    activity.is_some_and(|activity| {
                        activity
                            .touched
                            .iter()
                            .any(|touch| touch.path == path && touch.at_ms > since)
                            || activity
                                .known
                                .iter()
                                .any(|known| known.path == path && known.at_ms > since)
                    })
                };
                let shown_keys = keep_changes(&mut briefing, &told, &acted, *mid_turn, once);
                briefing.changed = once || shown_anything(&briefing);
                let next: Vec<u64> = keys
                    .iter()
                    .copied()
                    .filter(|key| told.contains(key) || shown_keys.contains(key))
                    .take(MAX_TOLD)
                    .collect();
                (next, briefing.changed)
            } else {
                // The whole view: once delivered, every item in it is told.
                briefing.changed = differs || once;
                (keys.into_iter().take(MAX_TOLD).collect(), true)
            };
            let told_before: Vec<u64> = activity
                .map(|activity| activity.told.clone())
                .unwrap_or_default();
            let mut events = Vec::new();
            if *deliver {
                let records = if *changes_only {
                    shown || told_next != told_before
                } else {
                    // Mid-turn, a full view not delivered as news (where a
                    // peer looks, alone) is not recorded: it stays news for
                    // the next prompt.
                    (signature != previous && briefing.changed)
                        || (told_next != told_before && (briefing.changed || !*mid_turn))
                };
                if records {
                    events.push(Event::BriefingDelivered {
                        session_id: session_id.clone(),
                        signature,
                        told: told_next,
                        told_at_ms: shown.then_some(now_ms),
                    });
                }
                events.extend(
                    briefing
                        .messages
                        .iter()
                        .map(|message| Event::MessageAcknowledged {
                            workspace_id: message.workspace_id.clone(),
                            message_id: message.message_id.clone(),
                            session_id: session_id.clone(),
                            via: Some(if *mid_turn {
                                Delivery::MidTurn
                            } else if *mcp {
                                Delivery::McpBrief
                            } else {
                                Delivery::Briefing
                            }),
                            at_ms: Some(now_ms),
                            generation: Some(state.generation),
                        }),
                );
                events.extend(briefing.retracted.iter().map(|retracted| {
                    Event::RetractionDelivered {
                        session_id: session_id.clone(),
                        message_id: retracted.message_id.clone(),
                    }
                }));
                events.extend(briefing.trail.iter().map(|view| Event::TrailTaken {
                    workspace_id: view.trail.workspace_id.clone(),
                    from: view.trail.session_id.clone(),
                    session_id: session_id.clone(),
                }));
                events.extend(briefing.checks.iter().map(|notice| Event::CheckDelivered {
                    session_id: session_id.clone(),
                    check_id: notice.check_id.clone(),
                }));
                if !inflight_keys.is_empty() {
                    events.push(Event::InflightDelivered {
                        session_id: session_id.clone(),
                        keys: inflight_keys,
                    });
                }
                // The MCP tool returns every notice, so asking there tells
                // them all. A hook renders within a byte cap that can leave
                // some out, so it reports the ones it showed itself
                // (`InterfaceShown`). A notice told before is not recorded
                // twice.
                let newly_told: Vec<String> = briefing
                    .interface_notice_ids
                    .iter()
                    .filter(|id| {
                        !told_notices(state, session_id).any(|told| told.notice_id == **id)
                    })
                    .cloned()
                    .collect();
                if *mcp && !newly_told.is_empty() {
                    events.push(Event::InterfaceNoticesTold {
                        session_id: session_id.clone(),
                        notice_ids: newly_told,
                        via: NoticeVia::Briefing,
                    });
                }
                events.extend(
                    uncoordinated_owed(state, session_id, now_ms).map(|(change, _)| {
                        Event::UncoordinatedDelivered {
                            workspace_id: change.workspace_id.clone(),
                            change_id: change.change_id.clone(),
                            session_id: session_id.clone(),
                        }
                    }),
                );
            }
            Ok(Decision {
                events,
                outcome: Outcome::Briefing { briefing },
            })
        }
        Command::SendMessage {
            workspace_id,
            from,
            to,
            text,
            paths,
        } => {
            validate_known_workspace(state, workspace_id)?;
            if let Some(from) = from
                && known_session(state, from)?.workspace_id != *workspace_id
            {
                return Err(error(
                    "sender_not_in_workspace",
                    "The sending session belongs to another workspace; send from a session of this one, or leave out the sender to send as the owner",
                ));
            }
            let to = match to {
                Some(to) => {
                    validate_id(to, "session ID")?;
                    if state
                        .sessions
                        .get(to)
                        .is_none_or(|session| session.workspace_id != *workspace_id)
                    {
                        return Err(error(
                            "recipient_not_found",
                            "The recipient is not a session of this workspace; `langspan recap WORKSPACE` lists them",
                        ));
                    }
                    if from.as_ref() == Some(to) {
                        return Err(error(
                            "invalid_message",
                            "A session cannot send a message to itself; use `remember` for a note",
                        ));
                    }
                    Recipient::Session {
                        session_id: to.clone(),
                    }
                }
                None => Recipient::Everyone {},
            };
            sent(
                request_id,
                now_ms,
                state.generation,
                workspace_id,
                from.clone(),
                to,
                text,
                paths,
            )
        }
        Command::Handoff {
            session_id,
            text,
            paths,
        } => {
            let session = known_session(state, session_id)?;
            sent(
                request_id,
                now_ms,
                state.generation,
                &session.workspace_id,
                Some(session_id.clone()),
                Recipient::NextSession {},
                text,
                paths,
            )
        }
        Command::AckMessage {
            session_id,
            message_id,
        } => {
            let session = known_session(state, session_id)?;
            validate_request_id(message_id)?;
            let owed = state
                .messages
                .get(&session.workspace_id)
                .and_then(|messages| {
                    messages
                        .iter()
                        .find(|message| &message.message_id == message_id)
                })
                .is_some_and(|message| owed_to(message, session_id, true));
            Ok(Decision {
                events: if owed {
                    vec![Event::MessageAcknowledged {
                        workspace_id: session.workspace_id.clone(),
                        message_id: message_id.clone(),
                        session_id: session_id.clone(),
                        via: Some(Delivery::Ack),
                        at_ms: Some(now_ms),
                        generation: Some(state.generation),
                    }]
                } else {
                    Vec::new()
                },
                outcome: Outcome::MessageAcknowledged {
                    message_id: message_id.clone(),
                    acknowledged: owed,
                },
            })
        }
        Command::RetractMessage {
            workspace_id,
            from,
            message_id,
        } => retract(state, workspace_id, from.as_deref(), message_id.as_deref()),
        Command::Dashboard => Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::Dashboard {
                dashboard: dashboard::dashboard(state, now_ms),
            },
        }),
        Command::Status => {
            let (view, summary) = status_view(state, now_ms);
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Status {
                    state: view,
                    summary: Some(summary),
                },
            })
        }
        Command::Remember {
            workspace_id,
            author,
            text,
            paths,
            symbol,
            tags,
        } => decide_checked(
            state,
            &Command::RememberPrepared {
                workspace_id: workspace_id.clone(),
                author: author.clone(),
                text: text.clone(),
                paths: paths.clone(),
                symbol: symbol.clone(),
                tags: tags.clone(),
                index: None,
            },
            request_id,
            now_ms,
        ),
        Command::ObserveNoteAnchors { workspace_id } => {
            validate_known_workspace(state, workspace_id)?;
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::NoteAnchorsObserved { changed: 0 },
            })
        }
        Command::NoteAnchorsPrepared {
            workspace_id,
            index,
        } => {
            validate_known_workspace(state, workspace_id)?;
            let events: Vec<Event> = state
                .notes
                .get(workspace_id)
                .into_iter()
                .flatten()
                .filter_map(|note| {
                    let (anchor, held) = note.anchor.as_ref().zip(note.anchor_state)?;
                    let (anchor_now, state_now) = anchors::observe(anchor, &note.paths, index);
                    (anchor_now != *anchor || state_now != held).then(|| Event::NoteAnchorChanged {
                        workspace_id: workspace_id.clone(),
                        note_id: note.note_id.clone(),
                        anchor: anchor_now,
                        state: state_now,
                    })
                })
                .collect();
            Ok(Decision {
                outcome: Outcome::NoteAnchorsObserved {
                    changed: u32::try_from(events.len()).unwrap_or(u32::MAX),
                },
                events,
            })
        }
        Command::RememberPrepared {
            workspace_id,
            author,
            text,
            paths,
            symbol,
            tags,
            index,
        } => {
            validate_known_workspace(state, workspace_id)?;
            if let Some(author) = author {
                known_session(state, author)?;
            }
            let paths = if paths.is_empty() {
                Vec::new()
            } else {
                normalized_paths(paths)?
            };
            let tags = anchors::validate_tags(tags)?;
            let (anchor, anchor_state) = match symbol {
                Some(name) => {
                    let (anchor, held) = anchors::resolve(name, &paths, index.as_ref())?;
                    (Some(anchor), Some(held))
                }
                None => (None, None),
            };
            let note = Note {
                note_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                author: author.clone(),
                text: text.clone(),
                paths,
                anchor,
                anchor_state,
                tags,
            };
            validate_note(&note)?;
            Ok(Decision {
                events: vec![Event::Remembered {
                    note: note.clone(),
                    at: Some(Stamp {
                        at_ms: now_ms,
                        generation: state.generation,
                    }),
                }],
                outcome: Outcome::Remembered { note },
            })
        }
        Command::Forget {
            workspace_id,
            note_id,
        } => {
            validate_known_workspace(state, workspace_id)?;
            validate_request_id(note_id)?;
            let removed = state
                .notes
                .get(workspace_id)
                .is_some_and(|notes| notes.iter().any(|note| &note.note_id == note_id));
            Ok(Decision {
                events: if removed {
                    vec![Event::Forgotten {
                        workspace_id: workspace_id.clone(),
                        note_id: note_id.clone(),
                    }]
                } else {
                    Vec::new()
                },
                outcome: Outcome::Forgotten {
                    note_id: note_id.clone(),
                    removed,
                },
            })
        }
        Command::ObserveScan {
            workspace_id,
            failure,
            coverage,
        } => {
            validate_known_workspace(state, workspace_id)?;
            let held = state.scan_failures.get(workspace_id);
            let mut events = match failure {
                Some(detail) => {
                    let detail: String = detail
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .take(200)
                        .collect();
                    // The same failure repeated is one fact, not many.
                    if held.is_some_and(|held| {
                        held.detail == detail && held.generation == state.generation
                    }) {
                        Vec::new()
                    } else {
                        vec![Event::ScanFailed {
                            failure: ScanFailure {
                                workspace_id: workspace_id.clone(),
                                detail,
                                at_ms: now_ms,
                                generation: state.generation,
                            },
                        }]
                    }
                }
                None if held.is_some() => vec![Event::ScanRecovered {
                    workspace_id: workspace_id.clone(),
                }],
                None => Vec::new(),
            };
            // Coverage is of the last completed scan: a failed attempt
            // leaves it as it was, and an unchanged one is not a new fact.
            if failure.is_none() {
                let coverage = coverage
                    .clone()
                    .filter(|coverage| coverage != &ScanCoverage::default());
                if let Some(coverage) = &coverage {
                    validate_coverage(coverage)?;
                }
                if state.scan_coverage.get(workspace_id) != coverage.as_ref() {
                    events.push(Event::ScanCoverageObserved {
                        workspace_id: workspace_id.clone(),
                        coverage,
                    });
                }
            }
            Ok(Decision {
                events,
                outcome: Outcome::ScanObserved {
                    workspace_id: workspace_id.clone(),
                    failed: failure.is_some(),
                },
            })
        }
        Command::ObserveHosts { not_installed } => {
            if not_installed.len() > MAX_HOSTS {
                return Err(error(
                    "invalid_hosts",
                    &format!("a host check names at most {MAX_HOSTS} hosts"),
                ));
            }
            let now_missing: BTreeSet<&str> = not_installed.iter().map(String::as_str).collect();
            if now_missing.len() != not_installed.len() {
                return Err(error("invalid_hosts", "a host is named more than once"));
            }
            for host in &now_missing {
                validate_host_name(host)?;
            }
            // Each change of a host's state is one fact; an unchanged
            // host is not recorded again.
            let mut events: Vec<Event> = state
                .uninstalled_hosts
                .keys()
                .filter(|host| !now_missing.contains(host.as_str()))
                .map(|host| Event::HostCleared { host: host.clone() })
                .collect();
            events.extend(
                now_missing
                    .iter()
                    .filter(|host| !state.uninstalled_hosts.contains_key(**host))
                    .map(|host| Event::HostNotInstalled {
                        host: UninstalledHost {
                            host: (*host).into(),
                            at_ms: now_ms,
                            generation: state.generation,
                        },
                    }),
            );
            Ok(Decision {
                events,
                outcome: Outcome::HostsObserved {
                    not_installed: now_missing.iter().map(|host| (*host).into()).collect(),
                },
            })
        }
        Command::ObserveCheckout {
            workspace_id,
            from_branch,
            to_branch,
            from_head,
            to_head,
            files,
        } => {
            let change = CheckoutChange {
                change_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                from_branch: from_branch.clone(),
                to_branch: to_branch.clone(),
                from_head: from_head.clone(),
                to_head: to_head.clone(),
                files: *files,
                at_ms: now_ms,
                generation: state.generation,
            };
            validate_checkout(state, &change)?;
            Ok(Decision {
                events: vec![Event::CheckoutChanged {
                    change: change.clone(),
                }],
                outcome: Outcome::CheckoutObserved { change },
            })
        }
        Command::ObserveCheckoutRun {
            session_id,
            command,
            to_branch,
            to_head,
            ran_ms,
            finished_ago_ms,
            files_unchanged,
        } => {
            let Some(session) = state.sessions.get(session_id) else {
                return Err(error(
                    "unknown_session",
                    "A checkout run names a registered session; register it first",
                ));
            };
            // When the command ran, not when its report arrived: a
            // redelivered report credits no later change. Placed back from
            // the daemon's own now by durations, so never after it; a run
            // two hours at most, a shell call's lifetime.
            let finished_ms = now_ms.saturating_sub(*finished_ago_ms);
            let run = CheckoutRun {
                run_id: request_id.into(),
                workspace_id: session.workspace_id.clone(),
                session_id: session_id.clone(),
                command: command.clone(),
                to_branch: to_branch.clone(),
                to_head: to_head.clone(),
                started_ms: finished_ms.saturating_sub((*ran_ms).min(MAX_CHECKOUT_RUN_MS)),
                finished_ms,
                generation: state.generation,
                files_unchanged: *files_unchanged,
            };
            validate_checkout_run(state, &run)?;
            Ok(Decision {
                events: vec![Event::CheckoutRunObserved { run: run.clone() }],
                outcome: Outcome::CheckoutRunObserved { run },
            })
        }
        Command::ObserveUncoordinated {
            workspace_id,
            files,
            scanned_from_ms,
        } => {
            validate_known_workspace(state, workspace_id)?;
            if files.is_empty() || files.len() > MAX_UNCOORDINATED_OBSERVED {
                return Err(error(
                    "invalid_path",
                    &format!(
                        "An uncoordinated change names 1 to {MAX_UNCOORDINATED_OBSERVED} changed files"
                    ),
                ));
            }
            let files = normalized_files(files)?;
            // A file under an admitted edit is that edit's, whose report has
            // not arrived yet; a version a session reported is explained; and
            // a file a session reported after the scan began reading is newer
            // than what the scan saw: the version before that session's edit.
            let unexplained: Vec<ObservedFile> = files
                .into_iter()
                .filter(|file| {
                    !leased(state, workspace_id, &file.path, now_ms)
                        && !explained(state, workspace_id, file)
                        && !scanned_from_ms.is_some_and(|from| {
                            reported_since(state, workspace_id, &file.path, from)
                        })
                })
                .take(MAX_UNCOORDINATED_FILES)
                .collect();
            // A Codex session already named in a notice still held is not
            // named again: one notice per session until it ages out.
            let named: BTreeSet<&str> = held_uncoordinated(state, workspace_id, now_ms)
                .flat_map(|change| change.sessions.iter().map(String::as_str))
                .collect();
            let sessions = if unexplained.is_empty() {
                Vec::new()
            } else {
                codex_sessions(state, workspace_id)
                    .into_iter()
                    .filter(|session| !named.contains(session.as_str()))
                    .take(MAX_UNCOORDINATED_SESSIONS)
                    .collect()
            };
            if sessions.is_empty() {
                return Ok(Decision {
                    events: Vec::new(),
                    outcome: Outcome::UncoordinatedObserved {
                        sessions: Vec::new(),
                        paths: Vec::new(),
                    },
                });
            }
            let change = UncoordinatedChange {
                change_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                sessions: sessions.clone(),
                files: unexplained,
                at_ms: now_ms,
                generation: state.generation,
                delivered: Vec::new(),
            };
            validate_uncoordinated(state, &change)?;
            Ok(Decision {
                outcome: Outcome::UncoordinatedObserved {
                    sessions,
                    paths: change.files.iter().map(|file| file.path.clone()).collect(),
                },
                events: vec![Event::UncoordinatedChanged { change }],
            })
        }
        Command::Recap { workspace_id, tag } => {
            validate_known_workspace(state, workspace_id)?;
            if let Some(tag) = tag {
                anchors::validate_tag(tag)?;
            }
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Recap {
                    recap: recap(state, workspace_id, tag.as_deref(), now_ms),
                },
            })
        }
        Command::Trails {
            workspace_id,
            limit,
        } => {
            validate_known_workspace(state, workspace_id)?;
            let limit = limit.unwrap_or(4);
            if !(1..=MAX_TRAILS as u32).contains(&limit) {
                return Err(error(
                    "invalid_limit",
                    "A trail listing names between 1 and 16 trails",
                ));
            }
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Trails {
                    workspace_id: workspace_id.clone(),
                    trails: trail::listed(state, workspace_id, limit as usize, now_ms),
                    omitted: 0,
                },
            })
        }
        Command::Recall {
            workspace_id,
            query,
            path,
            limit,
        } => Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::Recall {
                recall: recall_notes(state, workspace_id, query, path.as_deref(), *limit)?,
            },
        }),
        Command::StatusLine { root, from_ms } => {
            if !root.starts_with('/') || root.len() > 4096 {
                return Err(error(
                    "invalid_root",
                    "A status line names an absolute directory of at most 4096 bytes",
                ));
            }
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::StatusLine {
                    line: status_line(state, root, *from_ms, now_ms),
                },
            })
        }
        Command::Policy { workspace_id } => {
            validate_known_workspace(state, workspace_id)?;
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Policy {
                    workspace_id: workspace_id.clone(),
                    rows: policy::table(state, workspace_id),
                },
            })
        }
        Command::SetPolicy {
            workspace_id,
            case,
            policy,
            wait_ms,
        } => policy::decide_set(state, workspace_id, *case, *policy, *wait_ms),
        Command::Holders {
            session_id,
            paths,
            base,
            regions,
        } => {
            let session = current_session(state, session_id)?;
            let paths = normalized_paths(paths)?;
            let base = normalized_files(base)?;
            let regions = trusted_regions(regions, &paths, base.iter())?;
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Holders {
                    holders: holders_of(
                        state,
                        session_id,
                        &session.workspace_id,
                        &paths,
                        &regions,
                        true,
                        now_ms,
                    ),
                },
            })
        }
        Command::RecordWait {
            session_id,
            paths,
            holder_session_id,
            waited_ms,
            result,
            changed,
            input_digest,
        } => policy::decide_wait(
            state,
            policy::WaitReport {
                session_id,
                paths,
                holder_session_id: holder_session_id.as_ref(),
                waited_ms: *waited_ms,
                result: *result,
                changed,
                input_digest: input_digest.as_ref(),
            },
            now_ms,
        ),
        Command::RecordRefusal {
            session_id,
            kind,
            paths,
            peer_session_id,
        } => {
            if *kind != RefusalKind::CommitHeld {
                return Err(error(
                    "invalid_refusal",
                    "Only a held commit is reported; the coordinator records the other refusals itself",
                ));
            }
            let session = known_session(state, session_id)?;
            let paths = normalized_paths(paths)?;
            if let Some(peer) = peer_session_id {
                known_session(state, peer)?;
            }
            Ok(Decision {
                events: vec![Event::WriteRefused {
                    session_id: session_id.clone(),
                    workspace_id: session.workspace_id.clone(),
                    kind: *kind,
                    paths: paths.into_iter().take(REFUSAL_PATHS).collect(),
                    peer_session_id: peer_session_id.clone(),
                    input_digest: None,
                    at_ms: now_ms,
                    generation: state.generation,
                }],
                outcome: Outcome::RefusalRecorded {
                    session_id: session_id.clone(),
                },
            })
        }
        Command::ObserveInterface {
            workspace_id,
            path,
            changes,
            dependents,
        } => {
            validate_known_workspace(state, workspace_id)?;
            let notice = InterfaceNotice {
                notice_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                path: path.clone(),
                changes: changes.clone(),
                dependents: dependents.clone(),
                elsewhere: Vec::new(),
                by_session: last_peer_edit(state, "", workspace_id, path, 0)
                    .map(|(session, _)| session.to_owned()),
                at_ms: now_ms,
                generation: state.generation,
            };
            validate_notice(&notice)?;
            Ok(Decision {
                events: vec![Event::InterfaceChanged {
                    notice: notice.clone(),
                }],
                outcome: Outcome::InterfaceObserved { notice },
            })
        }
        Command::ObserveInterfaceDependents {
            notice_workspace_id,
            notice_id,
            workspace_id,
            paths,
        } => {
            validate_known_workspace(state, notice_workspace_id)?;
            validate_known_workspace(state, workspace_id)?;
            if workspace_id == notice_workspace_id
                || !related_workspace(state, notice_workspace_id, workspace_id)
            {
                return Err(error(
                    "invalid_notice",
                    "Dependents found elsewhere are files of another worktree of the notice's repository",
                ));
            }
            if paths.len() > NOTICE_DEPENDENTS {
                return Err(error(
                    "invalid_notice",
                    "At most 32 dependent files are reported at once",
                ));
            }
            for path in paths {
                validate_relative_path(path)?;
            }
            let notice = state
                .interface_notices
                .get(notice_workspace_id)
                .and_then(|notices| notices.iter().find(|n| n.notice_id == *notice_id))
                .ok_or_else(|| {
                    error(
                        "unknown_notice",
                        "The interface notice is not held: a newer one for its file replaced it, or it was never recorded",
                    )
                })?;
            let room = NOTICE_DEPENDENTS.saturating_sub(notice.elsewhere.len());
            let files: Vec<WorktreeFile> = paths
                .iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|path| WorktreeFile {
                    workspace_id: workspace_id.clone(),
                    path: path.clone(),
                })
                .filter(|file| !notice.elsewhere.contains(file))
                .take(room)
                .collect();
            Ok(Decision {
                outcome: Outcome::InterfaceDependentsObserved {
                    notice_id: notice_id.clone(),
                    added: files.len() as u32,
                },
                events: if files.is_empty() {
                    Vec::new()
                } else {
                    vec![Event::InterfaceDependentsFound {
                        notice_workspace_id: notice_workspace_id.clone(),
                        notice_id: notice_id.clone(),
                        files,
                    }]
                },
            })
        }
        Command::Events { .. }
        | Command::Today { .. }
        | Command::Report { .. }
        | Command::Count { .. }
        | Command::History { .. }
        | Command::StatusRecords { .. }
        | Command::Changes { .. }
        | Command::RemoteSent { .. }
        | Command::Compact { .. }
        | Command::Version
        | Command::ShowMessage { .. }
        | Command::MessageStatus { .. }
        | Command::GraphSymbol { .. }
        | Command::GraphReferences { .. }
        | Command::GraphImpact { .. }
        | Command::GraphBlindSpots { .. }
        | Command::GraphPath { .. }
        | Command::GraphMap { .. }
        | Command::GraphContext { .. }
        | Command::GraphInflight { .. }
        | Command::GraphExport { .. }
        | Command::GraphImport { .. }
        | Command::CheckWrite { .. }
        | Command::CheckCalls { .. } => Err(error(
            "storage_operation",
            "Event history, status pages, change projections, sent messages and index queries are answered by the daemon's stores",
        )),
    }
}

fn validate_note(note: &Note) -> Result<(), CoreError> {
    validate_request_id(&note.note_id)?;
    if note.text.trim().is_empty()
        || note.text.len() > NOTE_BYTES
        || note.text.chars().any(char::is_control)
        || note.paths.len() > BRIEF_PATHS
        || !anchors::valid_note_anchor(note)
    {
        return Err(error(
            "invalid_note",
            "A note is one line of 1 to 400 bytes about at most 8 files",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sent(
    request_id: &str,
    now_ms: u64,
    generation: u64,
    workspace_id: &str,
    from: Option<String>,
    to: Recipient,
    text: &str,
    paths: &[String],
) -> Result<Decision, CoreError> {
    let message = Message {
        message_id: request_id.into(),
        workspace_id: workspace_id.into(),
        from,
        to,
        text: text.into(),
        paths: if paths.is_empty() {
            Vec::new()
        } else {
            normalized_paths(paths)?
        },
        acknowledged: Vec::new(),
        retracted: false,
    };
    validate_message(&message)?;
    Ok(Decision {
        events: vec![Event::MessageSent {
            message: message.clone(),
            at_ms: Some(now_ms),
            generation: Some(generation),
        }],
        outcome: Outcome::MessageSent { message },
    })
}

/// The sent message `message_id` of the workspace, or with no ID the newest
/// one `from` sent. The history is the latest messages sent, newest first,
/// and lists every reader, so it answers first. A message older than the
/// history but still held is found by its ID only: the sender's newest
/// message is in the history if any of its messages is, so `last` never
/// reaches past a newer message that was dropped.
fn sent_message<'a>(
    state: &'a State,
    workspace_id: &str,
    from: Option<&str>,
    message_id: Option<&str>,
) -> Option<&'a Message> {
    let list = |lists: &'a BTreeMap<String, Vec<Message>>| {
        lists.get(workspace_id).map(Vec::as_slice).unwrap_or(&[])
    };
    match message_id {
        Some(id) => list(&state.message_history)
            .iter()
            .chain(list(&state.messages))
            .find(|message| message.message_id == id),
        None => list(&state.message_history)
            .iter()
            .find(|message| message.from.as_deref() == from),
    }
}

fn retract(
    state: &State,
    workspace_id: &str,
    from: Option<&str>,
    message_id: Option<&str>,
) -> Result<Decision, CoreError> {
    validate_known_workspace(state, workspace_id)?;
    if let Some(id) = message_id {
        validate_request_id(id)?;
    }
    if let Some(from) = from
        && known_session(state, from)?.workspace_id != workspace_id
    {
        return Err(error(
            "sender_not_in_workspace",
            "The withdrawing session belongs to another workspace; withdraw from the session that sent it, or leave out the sender to withdraw as the owner",
        ));
    }
    let Some(message) = sent_message(state, workspace_id, from, message_id) else {
        return Err(error(
            "unknown_message",
            match message_id {
                Some(_) => {
                    "No such message in this workspace: unknown, or dropped as the oldest of more than 64; `langspan recap WORKSPACE` lists those held"
                }
                None => {
                    "This sender has none of the latest 64 messages sent in this workspace; withdraw an older one still held by its ID"
                }
            },
        ));
    };
    if from.is_some() && message.from.as_deref() != from {
        return Err(error(
            "not_sender",
            "Only the session that sent a message, or the owner (without a session), may withdraw it",
        ));
    }
    if message.retracted {
        return Err(error(
            "already_retracted",
            "That message was already withdrawn; its readers have been or will be told",
        ));
    }
    let held = state.messages.get(workspace_id).and_then(|messages| {
        messages
            .iter()
            .find(|held| held.message_id == message.message_id)
    });
    let withdrawn_unread: Vec<String> = match held.map(|held| (held, &held.to)) {
        Some((_, Recipient::Session { session_id })) => vec![session_id.clone()],
        Some((held, Recipient::Everyone {})) => state
            .sessions
            .values()
            .filter(|session| {
                session.workspace_id == workspace_id
                    && session.generation == state.generation
                    && owed_to(held, &session.session_id, false)
            })
            .map(|session| session.session_id.clone())
            .collect(),
        Some((_, Recipient::NextSession {})) | None => Vec::new(),
    };
    Ok(Decision {
        events: vec![Event::MessageRetracted {
            workspace_id: workspace_id.into(),
            message_id: message.message_id.clone(),
            by: from.map(str::to_owned),
        }],
        outcome: Outcome::MessageRetracted {
            message_id: message.message_id.clone(),
            withdrawn_unread,
            told_to_disregard: readers(state, workspace_id, &message.message_id),
        },
    })
}

/// Every session that received the message, in ID order.
fn readers(state: &State, workspace_id: &str, message_id: &str) -> Vec<String> {
    let mut readers: Vec<String> = sent_message(state, workspace_id, None, Some(message_id))
        .map(|message| message.acknowledged.clone())
        .unwrap_or_default();
    readers.sort();
    readers.dedup();
    readers
}

/// The first `RETRACTED_TEXT_BYTES` of `text`, at a character boundary, and
/// whether anything was left out.
fn retracted_text(text: &str) -> (String, bool) {
    let mut end = text.len().min(RETRACTED_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), end < text.len())
}

/// An upper bound on the bytes an adapter renders to tell a session to
/// disregard a withdrawn message, counted like `message_cost`.
fn retracted_cost(retracted: &RetractedMessage) -> usize {
    (192 + 4 * retracted.text.len()).min(MESSAGE_LINE_LIMIT + 4)
}

fn validate_message(message: &Message) -> Result<(), CoreError> {
    validate_request_id(&message.message_id)?;
    if message.text.trim().is_empty()
        || message.text.len() > MESSAGE_BYTES
        || message.text.chars().any(char::is_control)
        || message.paths.len() > BRIEF_PATHS
    {
        return Err(error(
            "invalid_message",
            "A message is one line of 1 to 400 bytes about at most 8 files",
        ));
    }
    for path in &message.paths {
        validate_relative_path(path)?;
    }
    if let Some(from) = &message.from {
        validate_id(from, "session ID")?;
    }
    if let Recipient::Session { session_id } = &message.to {
        validate_id(session_id, "session ID")?;
    }
    Ok(())
}

/// Whether `session_id` is still owed `message`. A handoff is owed to any
/// other session until one takes it; `handoffs` is false mid-turn.
fn owed_to(message: &Message, session_id: &str, handoffs: bool) -> bool {
    message.from.as_deref() != Some(session_id)
        && !message.acknowledged.iter().any(|held| held == session_id)
        && match &message.to {
            Recipient::Session { session_id: to } => to == session_id,
            Recipient::Everyone {} => true,
            Recipient::NextSession {} => handoffs,
        }
}

/// Whether `me` may take `message` if it is a handoff. A handoff is meant
/// for the next session a person works in, so a headless session (`claude
/// -p`) takes one only when a headless session wrote it: then the next
/// headless run is the kind of session that will carry the work on, as in a
/// scripted pipeline. A person's handoff waits for an interactive session,
/// however many scripted runs start first. A subagent never takes one: it
/// works for its parent, which takes it if it is owed.
fn takes_handoff(state: &State, me: &Session, message: &Message) -> bool {
    if !matches!(message.to, Recipient::NextSession {}) {
        return true;
    }
    if me.parent.is_some() {
        return false;
    }
    !me.headless
        || message
            .from
            .as_deref()
            .and_then(|from| state.sessions.get(from))
            .is_some_and(|sender| sender.headless)
}

/// An upper bound on the bytes an adapter renders for one message: a label
/// of under 192 bytes (a session ID is at most 128), then text and paths
/// that quoting can grow at most fourfold, cut within `MESSAGE_LINE_LIMIT`
/// (ellipsis and `messages show` pointer included) plus its newline.
fn message_cost(message: &Message) -> usize {
    let raw = message.text.len()
        + message
            .paths
            .iter()
            .map(|path| path.len() + 4)
            .sum::<usize>();
    (192 + 4 * raw).min(MESSAGE_LINE_LIMIT + 4)
}

/// A bounded account of one workspace, from projected state only.
fn recap(state: &State, workspace_id: &str, tag: Option<&str>, now_ms: u64) -> Recap {
    let mut sessions: Vec<(u64, RecapSession)> = state
        .sessions
        .values()
        .filter(|session| session.workspace_id == workspace_id)
        .map(|session| {
            let activity = state.activity.get(&session.session_id);
            let in_generation = activity.filter(|activity| activity.generation == state.generation);
            let editing: Vec<String> = state
                .leases
                .values()
                .filter(|lease| {
                    lease.session_id == session.session_id && live_lease(state, lease, now_ms)
                })
                .flat_map(|lease| lease.paths.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(RECAP_ITEMS)
                .collect();
            (
                in_generation
                    .map(SessionActivity::last_seen_ms)
                    .unwrap_or(0),
                RecapSession {
                    session_id: session.session_id.clone(),
                    current: session.generation == state.generation,
                    ended: activity.is_some_and(|activity| activity.ended),
                    liveness: state
                        .session_liveness
                        .get(&session.session_id)
                        .map(|liveness| liveness.status),
                    summary: activity.and_then(|activity| activity.summary.clone()),
                    intent: state
                        .intents
                        .get(&session.session_id)
                        .map(|intent| intent.summary.clone()),
                    editing,
                    recent: in_generation
                        .map(|activity| {
                            activity
                                .touched
                                .iter()
                                .take(RECAP_ITEMS)
                                .map(|touch| RecentPath {
                                    path: touch.path.clone(),
                                    age_ms: now_ms.saturating_sub(touch.at_ms),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    adopted_by: state
                        .adoptions
                        .get(&session.session_id)
                        .map(|adoption| adoption.session_id.clone()),
                    adopted: state
                        .adoptions
                        .values()
                        .filter(|adoption| adoption.session_id == session.session_id)
                        .take(RECAP_ITEMS)
                        .map(|adoption| adoption.from.clone())
                        .collect(),
                },
            )
        })
        .collect();
    // Sessions that are here now first, then by most recent activity.
    sessions.sort_by(|a, b| {
        (!a.1.current, a.1.ended, u64::MAX - a.0, &a.1.session_id).cmp(&(
            !b.1.current,
            b.1.ended,
            u64::MAX - b.0,
            &b.1.session_id,
        ))
    });
    let total = sessions.len();
    let first = |items: Option<&Vec<_>>| -> Vec<_> {
        items
            .map(|items| items.iter().take(RECAP_ITEMS).cloned().collect())
            .unwrap_or_default()
    };
    Recap {
        workspace_id: workspace_id.into(),
        root: state
            .workspaces
            .get(workspace_id)
            .map(|workspace| workspace.root.clone())
            .unwrap_or_default(),
        branch_ref: state
            .workspace_git
            .get(workspace_id)
            .and_then(|git| git.branch_ref.clone()),
        omitted_sessions: u32::try_from(total.saturating_sub(RECAP_SESSIONS)).unwrap_or(u32::MAX),
        omitted_items: 0,
        sessions: sessions
            .into_iter()
            .take(RECAP_SESSIONS)
            .map(|(_, session)| session)
            .collect(),
        interface_changes: state
            .interface_notices
            .get(workspace_id)
            .map(|notices| notices.iter().take(RECAP_ITEMS).cloned().collect())
            .unwrap_or_default(),
        checks: first(state.checks.get(workspace_id)),
        landings: state
            .landings
            .get(workspace_id)
            .map(|landings| landings.iter().take(RECAP_ITEMS).cloned().collect())
            .unwrap_or_default(),
        notes: state
            .notes
            .get(workspace_id)
            .map(|notes| {
                notes
                    .iter()
                    .filter(|note| tag.is_none_or(|tag| note.tags.iter().any(|held| held == tag)))
                    .take(RECAP_SESSIONS)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
        messages: state
            .messages
            .get(workspace_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .take(RECAP_SESSIONS)
            .chain(
                state
                    .message_history
                    .get(workspace_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|message| message.retracted)
                    .take(RECAP_ITEMS),
            )
            .cloned()
            .collect(),
        trails: trail::listed(state, workspace_id, RECAP_TRAILS, now_ms),
        teammates: state
            .remote_peers
            .values()
            .filter(|peer| peer.workspace_id == workspace_id)
            .take(RECAP_ITEMS)
            .cloned()
            .collect(),
        publishing: state.remote_links.get(workspace_id).cloned(),
        scan_failure: state.scan_failures.get(workspace_id).cloned(),
        scan_coverage: state.scan_coverage.get(workspace_id).cloned(),
    }
}

fn validate_notice(notice: &InterfaceNotice) -> Result<(), CoreError> {
    validate_request_id(&notice.notice_id)?;
    validate_relative_path(&notice.path)?;
    if notice.changes.is_empty()
        || notice.changes.len() > NOTICE_CHANGES
        || notice.dependents.len() > NOTICE_DEPENDENTS
        || notice.elsewhere.len() > NOTICE_DEPENDENTS
    {
        return Err(error(
            "invalid_notice",
            "An interface notice names 1 to 16 changes and at most 32 dependent files in its own worktree and 32 in others",
        ));
    }
    for path in notice
        .dependents
        .iter()
        .chain(notice.elsewhere.iter().map(|file| &file.path))
    {
        validate_relative_path(path)?;
    }
    for change in &notice.changes {
        change
            .before_arity
            .iter()
            .chain(&change.after_arity)
            .try_for_each(validate_arity)?;
        let texts = [
            Some(&change.name),
            change.before.as_ref(),
            change.after.as_ref(),
            change.after_name.as_ref(),
        ];
        validate_rename_record(notice, change)?;
        let counts = match (change.before_count, change.after_count) {
            (Some(before), Some(after)) => Some((before, after)),
            (None, None) => None,
            _ => Some((0, 0)),
        };
        // Only a rename has a new name, other than the old one, and both
        // signatures. A count change names its counts, in the direction it
        // says, and carries the signatures that came or went; nothing else
        // has counts.
        // A definition moved with its file keeps its name (roadmap D7).
        let renamed = change.change == "renamed";
        let moved = change.evidence.as_deref() == Some(FILE_MOVED);
        let named = match &change.after_name {
            Some(to) => {
                renamed && !to.is_empty() && to.len() <= 128 && ((*to == change.name) == moved)
            }
            None => !renamed,
        };
        let shaped = named
            && match (change.change.as_str(), counts) {
                ("removed" | "signature_changed", None) => true,
                ("renamed", None) => change.before.is_some() && change.after.is_some(),
                // A new export: its signature, and no parameter counts.
                ("added", None) => {
                    change.before.is_none()
                        && change.after.is_some()
                        && change.before_arity.is_none()
                        && change.after_arity.is_none()
                }
                ("definition_added", Some((before, after))) => {
                    before >= 1
                        && before < after
                        && after <= MAX_DEFINITION_COUNT
                        && change.before.is_none()
                        && change.after.is_some()
                }
                ("definition_removed", Some((before, after))) => {
                    after >= 1
                        && after < before
                        && before <= MAX_DEFINITION_COUNT
                        && change.before.is_some()
                        && change.after.is_none()
                }
                _ => false,
            };
        if change.name.is_empty()
            || change.name.len() > 128
            || !shaped
            || change.kind.len() > 16
            || texts
                .into_iter()
                .flatten()
                .any(|text| text.len() > 256 || text.chars().any(char::is_control))
        {
            return Err(error(
                "invalid_notice",
                "An interface change has a name, a kind, `added`, `removed`, `signature_changed`, `renamed` with a different `after_name` of at most 128 bytes, or `definition_added` or `definition_removed` with both counts, and single-line signatures of at most 256 bytes",
            ));
        }
    }
    Ok(())
}

/// What a notice's renames and moves change in the state (roadmap D7). A
/// rename is remembered under its old name and kind, replacing an earlier
/// rename of that name and kind; a name that comes back as the
/// same kind (added, or the new name of another rename) is forgotten as
/// renamed. A file moved
/// whole takes its notes along: each note about the old path is about the
/// new one instead.
fn follow_renames(state: &mut State, notice: &InterfaceNotice) {
    for change in &notice.changes {
        let moved = change.evidence.as_deref() == Some(FILE_MOVED);
        let renames = state
            .renames
            .entry(notice.workspace_id.clone())
            .or_default();
        let back = match (change.change.as_str(), &change.after_name) {
            ("added", _) => Some(&change.name),
            ("renamed", Some(to)) if !moved => Some(to),
            _ => None,
        };
        if let Some(back) = back {
            // Only a definition of the renamed one's kind takes its name
            // back, as the lookup's own check does.
            renames.retain(|rename| rename.name != *back || rename.kind != change.kind);
        }
        if let ("renamed", Some(to), false) = (change.change.as_str(), &change.after_name, moved) {
            renames.retain(|rename| rename.name != change.name || rename.kind != change.kind);
            renames.insert(
                0,
                Rename {
                    name: change.name.clone(),
                    after_name: to.clone(),
                    kind: change.kind.clone(),
                    path: notice.path.clone(),
                    evidence: change.evidence.clone(),
                    after: change.after.clone().unwrap_or_default(),
                },
            );
            renames.truncate(MAX_RENAMES);
        }
        if renames.is_empty() {
            state.renames.remove(&notice.workspace_id);
        }
        let Some(from) = change.before_path.as_ref().filter(|_| moved) else {
            continue;
        };
        for note in state
            .notes
            .get_mut(&notice.workspace_id)
            .into_iter()
            .flatten()
        {
            // Sorted and once each, as a note's paths are recorded.
            if let Some(at) = note.paths.iter().position(|path| path == from) {
                note.paths[at].clone_from(&notice.path);
                note.paths.sort();
                note.paths.dedup();
            }
        }
    }
}

/// What links a rename's two names (roadmap D7).
pub const RENAME_EVIDENCE: [&str; 3] = ["same_body", "same_signature", FILE_MOVED];
/// The evidence of a definition that moved with its whole file.
pub const FILE_MOVED: &str = "file_moved";

/// A rename's evidence in words, as a notice ends its rename: ` (same
/// body)`, ` (same signature)`, or nothing for a rename recorded before its
/// evidence was kept and for a moved file, whose text says so itself.
pub fn rename_evidence_words(evidence: Option<&str>) -> &'static str {
    match evidence {
        Some("same_body") => " (same body)",
        Some("same_signature") => " (same signature)",
        _ => "",
    }
}

/// A rename's old name, evidence and old path (roadmap D7): each only on a
/// rename, the old name always `name`, and an old path exactly when the
/// file moved, other than the notice's path. A rename without them was
/// recorded before they were kept and is read as it was.
fn validate_rename_record(
    notice: &InterfaceNotice,
    change: &InterfaceChange,
) -> Result<(), CoreError> {
    let renamed = change.change == "renamed";
    let moved = change.evidence.as_deref() == Some(FILE_MOVED);
    let shaped = change
        .before_name
        .as_ref()
        .is_none_or(|name| renamed && *name == change.name)
        && change
            .evidence
            .as_deref()
            .is_none_or(|evidence| renamed && RENAME_EVIDENCE.contains(&evidence))
        && match &change.before_path {
            Some(path) => moved && *path != notice.path,
            None => !moved,
        }
        && (!change.private || moved);
    if !shaped {
        return Err(error(
            "invalid_notice",
            "A rename's `before_name` is its name, its `evidence` is `same_body`, `same_signature` or `file_moved`, and only `file_moved` has a `before_path`, another file than the notice's, or is `private`",
        ));
    }
    if let Some(path) = &change.before_path {
        validate_relative_path(path)?;
    }
    Ok(())
}

fn validate_known_workspace(state: &State, workspace_id: &str) -> Result<(), CoreError> {
    validate_id(workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Workspace is not registered; register it first",
        ));
    }
    Ok(())
}

fn validate_oid(oid: &str) -> Result<(), CoreError> {
    if !is_hex_digest(oid, 40) && !is_hex_digest(oid, 64) {
        return Err(error(
            "invalid_git_oid",
            "A Git object ID is 40 or 64 hexadecimal characters",
        ));
    }
    Ok(())
}

fn validate_inputs(inputs: &[String]) -> Result<(), CoreError> {
    if inputs.len() > 16 {
        return Err(error(
            "too_many_inputs",
            "A check or landing combines at most 16 commits",
        ));
    }
    inputs.iter().try_for_each(|oid| validate_oid(oid))
}

fn validate_check(state: &State, check: &CheckRecord) -> Result<(), CoreError> {
    validate_request_id(&check.check_id)?;
    validate_known_workspace(state, &check.workspace_id)?;
    validate_id(&check.name, "check name")?;
    if check.name.len() > 64
        || check.command.is_empty()
        || check.command.len() > MAX_CHECK_COMMAND_BYTES
        || check.command.chars().any(char::is_control)
    {
        return Err(error(
            "invalid_check",
            &format!(
                "A check has a name of at most 64 characters and a one-line command of 1 to {MAX_CHECK_COMMAND_BYTES} bytes"
            ),
        ));
    }
    validate_oid(&check.tree_oid)?;
    if let Some(head) = &check.head_oid {
        validate_oid(head)?;
    }
    validate_inputs(&check.inputs)?;
    validate_symbol_findings(check)?;
    if check.generation != state.generation {
        return Err(error(
            "invalid_event",
            "A check record belongs to the current daemon generation",
        ));
    }
    Ok(())
}

/// The newest record named `name` for exactly `tree_oid` decides. A pass
/// on any other tree is not evidence about this one.
fn readiness(
    state: &State,
    workspace_id: &str,
    name: &str,
    tree_oid: &str,
) -> (bool, String, Option<CheckRecord>) {
    let records = state
        .checks
        .get(workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    match records
        .iter()
        .find(|record| record.name == name && record.tree_oid == tree_oid)
    {
        Some(record) => match &record.result {
            CheckResult::Passed {} => (
                true,
                "a passing check exists for exactly this tree".into(),
                Some(record.clone()),
            ),
            CheckResult::Failed { exit_code } => (
                false,
                format!("the check failed on exactly this tree (exit {exit_code})"),
                Some(record.clone()),
            ),
            CheckResult::Inconclusive {} => (
                false,
                "the check on this tree was inconclusive; run it again".into(),
                Some(record.clone()),
            ),
        },
        None if records.iter().any(|record| record.name == name) => (
            false,
            "the check has only run on other trees; its results do not apply to this one".into(),
            None,
        ),
        None => (
            false,
            "no check of that name has been recorded".into(),
            None,
        ),
    }
}

fn validate_landing(state: &State, landing: &Landing) -> Result<(), CoreError> {
    validate_request_id(&landing.landing_id)?;
    validate_known_workspace(state, &landing.workspace_id)?;
    validate_oid(&landing.commit_oid)?;
    validate_oid(&landing.tree_oid)?;
    validate_inputs(&landing.inputs)?;
    if landing.rebased.len() > MAX_REWRITTEN || landing.adapted.len() > MAX_REWRITTEN {
        return Err(error(
            "invalid_landing",
            "A landing records at most 64 rebased and 64 adapted commits",
        ));
    }
    for oid in landing
        .rebased
        .iter()
        .flat_map(|rewritten| [&rewritten.from, &rewritten.to])
        .chain(&landing.adapted)
    {
        validate_oid(oid)?;
    }
    validate_request_id(&landing.check_id)?;
    validate_branch_name(&landing.onto)?;
    // A landing is recorded only on the evidence the core itself holds.
    let evidence = state.checks.get(&landing.workspace_id).and_then(|records| {
        records
            .iter()
            .find(|record| record.check_id == landing.check_id)
    });
    match evidence {
        Some(record)
            if record.tree_oid == landing.tree_oid && record.result == (CheckResult::Passed {}) =>
        {
            Ok(())
        }
        _ => Err(error(
            "no_current_evidence",
            "A landing must name a passing check recorded for exactly the landed tree",
        )),
    }
}

fn validate_branch_name(onto: &str) -> Result<(), CoreError> {
    if onto.is_empty() || onto.len() > 255 || onto.chars().any(|c| c.is_control() || c == ' ') {
        return Err(error(
            "invalid_branch_ref",
            "A landing names its target branch",
        ));
    }
    Ok(())
}

/// Whether `waiter` is still in line at `now_ms`.
fn still_waiting(waiter: &TurnWaiter, generation: u64, now_ms: u64) -> bool {
    waiter.generation == generation && waiter.deadline_ms > now_ms && waiter.seen_until_ms > now_ms
}

/// The landings in line for `key` at `now_ms`, first first.
pub(crate) fn waiting_for(state: &State, key: &str, now_ms: u64) -> Vec<TurnWaiter> {
    state
        .turn_queues
        .get(key)
        .into_iter()
        .flatten()
        .filter(|waiter| still_waiting(waiter, state.generation, now_ms))
        .cloned()
        .collect()
}

/// The line as callers see it.
pub(crate) fn line(waiting: &[TurnWaiter], now_ms: u64) -> Vec<QueuedLanding> {
    waiting
        .iter()
        .enumerate()
        .map(|(index, waiter)| QueuedLanding {
            ticket: waiter.ticket.clone(),
            session_id: waiter.session_id.clone(),
            position: u32::try_from(index + 1).unwrap_or(u32::MAX),
            waited_ms: now_ms.saturating_sub(waiter.joined_ms),
            wait_left_ms: waiter.deadline_ms.saturating_sub(now_ms),
        })
        .collect()
}

/// `TakeTurn` with a queue: take the turn if it is free and this landing is
/// first in line (or nobody is), otherwise join the line or stay in it.
fn queue_for_turn(
    state: &State,
    workspace_id: &str,
    onto: &str,
    session_id: &Option<String>,
    expires_at_ms: u64,
    queueing: &TurnQueueing,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let TurnQueueing {
        ticket,
        wait_ms,
        joined,
    } = queueing;
    validate_request_id(ticket)?;
    if !(1..=MAX_TURN_WAIT_MS).contains(wait_ms) {
        return Err(error(
            "invalid_wait",
            "A landing waits 1 ms to 1 hour for the push turn",
        ));
    }
    let key = turn_key(workspace_id, onto);
    let held = state
        .turns
        .get(&key)
        .filter(|held| held.expires_at_ms > now_ms);
    if let Some(held) = held.filter(|held| held.turn_id == *ticket) {
        return Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::Turn { turn: held.clone() },
        });
    }
    let mut events = Vec::new();
    // Waiters past their time leave before anyone is placed.
    for waiter in state.turn_queues.get(&key).into_iter().flatten() {
        if !still_waiting(waiter, state.generation, now_ms) {
            events.push(Event::TurnQueueLeft {
                workspace_id: workspace_id.into(),
                onto: onto.into(),
                ticket: waiter.ticket.clone(),
                reason: if waiter.deadline_ms <= now_ms {
                    LeftQueue::TimedOut
                } else {
                    LeftQueue::Stopped
                },
            });
        }
    }
    let mut waiting = waiting_for(state, &key, now_ms);
    let place = waiting.iter().position(|waiter| waiter.ticket == *ticket);
    // A landing taken out of line (cancelled, silent too long, or the
    // coordinator restarted) is told so, even when the turn is free: a
    // cancelled landing never goes on to push.
    if *joined && place.is_none() {
        return Err(error(
            "not_queued",
            "This landing is no longer waiting for the push turn: it was cancelled, it did not ask again within 15 s, or the coordinator restarted. Nothing was fetched or pushed; start the landing again to wait again",
        ));
    }
    if held.is_none() && waiting.first().is_none_or(|first| first.ticket == *ticket) {
        if let Some(expired) = state.turns.get(&key) {
            events.push(Event::TurnExpired {
                workspace_id: workspace_id.into(),
                onto: onto.into(),
                turn_id: expired.turn_id.clone(),
            });
        }
        let turn = Turn {
            turn_id: ticket.clone(),
            workspace_id: workspace_id.into(),
            onto: onto.into(),
            session_id: session_id.clone(),
            taken_ms: now_ms,
            expires_at_ms,
            generation: state.generation,
        };
        events.push(Event::TurnTaken { turn: turn.clone() });
        return Ok(Decision {
            events,
            outcome: Outcome::Turn { turn },
        });
    }
    let holder = held.cloned();
    let held_ms = held.map(|held| now_ms.saturating_sub(held.taken_ms));
    let seen_until_ms = now_ms.saturating_add(TURN_WAITER_SILENCE_MS);
    let position = match place {
        Some(index) => {
            // Asked again: refreshed once half its silence allowance is used,
            // so a waiting landing writes an event every few seconds, not on
            // every ask.
            let waiter = &mut waiting[index];
            if waiter.seen_until_ms.saturating_sub(now_ms) < TURN_WAITER_SILENCE_MS / 2 {
                waiter.seen_until_ms = seen_until_ms;
                events.push(Event::TurnQueueSeen {
                    workspace_id: workspace_id.into(),
                    onto: onto.into(),
                    ticket: ticket.clone(),
                    seen_until_ms,
                });
            }
            index + 1
        }
        None if waiting.len() >= MAX_TURN_QUEUE => {
            return Ok(Decision {
                events,
                outcome: Outcome::TurnUnavailable {
                    holder,
                    held_ms,
                    waiting: line(&waiting, now_ms),
                    queue_full: true,
                },
            });
        }
        None => {
            let waiter = TurnWaiter {
                ticket: ticket.clone(),
                workspace_id: workspace_id.into(),
                onto: onto.into(),
                session_id: session_id.clone(),
                joined_ms: now_ms,
                deadline_ms: now_ms.saturating_add(*wait_ms),
                seen_until_ms,
                generation: state.generation,
            };
            events.push(Event::TurnQueueJoined {
                waiter: waiter.clone(),
            });
            waiting.push(waiter);
            waiting.len()
        }
    };
    Ok(Decision {
        events,
        outcome: Outcome::TurnWaiting {
            position: u32::try_from(position).unwrap_or(u32::MAX),
            holder,
            held_ms,
            waiting: line(&waiting, now_ms),
        },
    })
}

/// Workspace IDs contain no colon, so the key is unambiguous.
fn turn_key(workspace_id: &str, onto: &str) -> String {
    format!("{workspace_id}:{onto}")
}

fn validate_attempts(attempts: &[LandingAttempt]) -> Result<(), CoreError> {
    if attempts.len() > MAX_LANDING_ATTEMPTS {
        return Err(error(
            "too_many_attempts",
            "A turn records at most 3 landing attempts",
        ));
    }
    attempts
        .iter()
        .flat_map(|attempt| [&attempt.target_oid, &attempt.commit_oid])
        .flatten()
        .try_for_each(|oid| validate_oid(oid))
}

fn remote_key(workspace_id: &str, participant: &str) -> String {
    format!("{workspace_id}:{participant}")
}

fn remote_link<'a>(state: &'a State, workspace_id: &str) -> Result<&'a RemoteLink, CoreError> {
    validate_id(workspace_id, "workspace ID")?;
    state
        .remote_links
        .get(workspace_id)
        .filter(|link| link.enabled)
        .ok_or_else(|| {
            error(
                "remote_not_enabled",
                "This workspace does not publish; enable it with `langspan remote enable`",
            )
        })
}

fn validate_remote_link(state: &State, link: &RemoteLink) -> Result<(), CoreError> {
    validate_id(&link.workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(&link.workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Register the workspace before enabling publication",
        ));
    }
    validate_id(&link.remote, "Git remote name")?;
    if link.remote.starts_with(['-', '.']) {
        return Err(error(
            "invalid_remote",
            "A Git remote name starts with a letter, digit or underscore",
        ));
    }
    validate_participant(&link.participant, &link.name)?;
    if link.remote.len() > 64 || link.signature.len() > 64 {
        return Err(error(
            "invalid_remote",
            "A Git remote name is at most 64 characters",
        ));
    }
    Ok(())
}

fn validate_participant(participant: &str, name: &str) -> Result<(), CoreError> {
    if !is_hex_digest(participant, 32) || participant.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(error(
            "invalid_participant",
            "A participant ID is 32 lowercase hexadecimal characters",
        ));
    }
    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
        return Err(error(
            "invalid_participant",
            "A participant name is 1 to 64 bytes without control characters",
        ));
    }
    Ok(())
}

/// Everything in a publication came from another machine. Reject the whole
/// publication rather than keep part of one.
fn validate_publication(publication: &Publication) -> Result<(), CoreError> {
    validate_participant(&publication.participant, &publication.name)?;
    if publication.withdrawn && !publication.work.is_empty() {
        return Err(error(
            "invalid_publication",
            "A withdrawn publication lists no sessions",
        ));
    }
    if publication.work.len() > REMOTE_WORK {
        return Err(error(
            "invalid_publication",
            "A publication lists at most 8 sessions",
        ));
    }
    for work in &publication.work {
        validate_id(&work.host, "host name")?;
        validate_id(&work.session, "session label")?;
        if work.host.len() > 32
            || work.session.len() > 16
            || work.editing.len() > BRIEF_PATHS
            || work.recent.len() > BRIEF_PATHS
        {
            return Err(error(
                "invalid_publication",
                "A published session exceeds its size bounds",
            ));
        }
        for path in work.editing.iter().chain(&work.recent) {
            validate_relative_path(path)?;
        }
        if let Some(summary) = &work.summary {
            validate_prompt_summary(summary)?;
        }
        if let Some(reference) = &work.branch_ref
            && (reference.len() > 512
                || reference.chars().any(char::is_control)
                || !reference
                    .strip_prefix("refs/heads/")
                    .is_some_and(|name| !name.is_empty()))
        {
            return Err(error(
                "invalid_branch_ref",
                "A published branch is a refs/heads/ name without control characters",
            ));
        }
    }
    Ok(())
}

/// Workspaces whose sessions belong to the same repository as `workspace_id`:
/// itself, and any the observer placed in the same repository.
fn related_workspace(state: &State, workspace_id: &str, other: &str) -> bool {
    if workspace_id == other {
        return true;
    }
    let repository = |id: &str| state.workspace_git.get(id).map(|git| &git.repository_id);
    repository(workspace_id).is_some() && repository(workspace_id) == repository(other)
}

/// What a workspace's owner publishes now: the current work of every local
/// session in the repository, bounded, with task lines only if the owner
/// chose to share them. Returns it with a fingerprint of its content.
fn publication(state: &State, link: &RemoteLink, now_ms: u64) -> (Publication, String) {
    let mut work = Vec::new();
    for session in state.sessions.values() {
        if session.generation != state.generation
            || !related_workspace(state, &link.workspace_id, &session.workspace_id)
            || state
                .session_liveness
                .get(&session.session_id)
                .is_some_and(|liveness| liveness.status == LivenessStatus::Exited)
        {
            continue;
        }
        let editing: Vec<String> = state
            .leases
            .values()
            .filter(|lease| {
                lease.session_id == session.session_id && live_lease(state, lease, now_ms)
            })
            .flat_map(|lease| lease.paths.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(BRIEF_PATHS)
            .collect();
        let activity = state
            .activity
            .get(&session.session_id)
            .filter(|activity| activity.generation == state.generation);
        let recent: Vec<String> = activity
            .map(|activity| {
                activity
                    .touched
                    .iter()
                    .filter(|touch| now_ms.saturating_sub(touch.at_ms) <= BRIEF_HORIZON_MS)
                    .map(|touch| touch.path.clone())
                    .take(BRIEF_PATHS)
                    .collect()
            })
            .unwrap_or_default();
        let ended = activity.is_some_and(|activity| activity.ended);
        let summary = activity
            .filter(|activity| {
                link.share_task
                    && !activity.ended
                    && now_ms.saturating_sub(activity.last_seen_ms()) <= BRIEF_HORIZON_MS
            })
            .and_then(|activity| activity.summary.clone());
        if editing.is_empty() && recent.is_empty() && (summary.is_none() || ended) {
            continue;
        }
        let (host, id) = session
            .session_id
            .split_once('.')
            .unwrap_or(("agent", &session.session_id));
        work.push(RemoteWork {
            host: host.chars().take(32).collect(),
            session: id.chars().take(8).collect(),
            branch_ref: state
                .workspace_git
                .get(&session.workspace_id)
                .and_then(|git| git.branch_ref.clone()),
            summary,
            editing,
            recent,
        });
        if work.len() == REMOTE_WORK {
            break;
        }
    }
    let mut text = String::new();
    for item in &work {
        text.push_str(&format!(
            "{}|{}|{}|{}|{}|{}\n",
            item.host,
            item.session,
            item.branch_ref.as_deref().unwrap_or(""),
            item.summary.as_deref().unwrap_or(""),
            item.editing.join(","),
            item.recent.join(",")
        ));
    }
    (
        Publication {
            participant: link.participant.clone(),
            name: link.name.clone(),
            sequence: link.sequence + 1,
            work,
            withdrawn: false,
        },
        fingerprint(&text),
    )
}

fn known_versions<'a>(state: &'a State, session_id: &str) -> &'a [KnownVersion] {
    state
        .activity
        .get(session_id)
        .map(|activity| activity.known.as_slice())
        .unwrap_or(&[])
}

/// The other session in `workspace_id` that most recently reported a
/// successful edit of `path` after `after_ms`, with the time it did.
fn last_peer_edit<'a>(
    state: &'a State,
    session_id: &str,
    workspace_id: &str,
    path: &str,
    after_ms: u64,
) -> Option<(&'a str, u64)> {
    state
        .activity
        .values()
        .filter(|activity| {
            activity.session_id != session_id
                && activity.generation == state.generation
                && state
                    .sessions
                    .get(&activity.session_id)
                    .is_some_and(|peer| peer.workspace_id == workspace_id)
        })
        .filter_map(|activity| {
            activity
                .touched
                .iter()
                .find(|touch| touch.path == path && touch.at_ms >= after_ms)
                .map(|touch| (activity.session_id.as_str(), touch.at_ms))
        })
        .max_by_key(|(id, at)| (*at, smallest_id_first(id)))
}

// Ties go to the lexically smallest session, so the result is deterministic.
fn smallest_id_first(id: &str) -> core::cmp::Reverse<&str> {
    core::cmp::Reverse(id)
}

/// Supplied files whose version differs from the one the session last read
/// or wrote. A file the session has no recorded version of is not stale: it
/// is unknown, and reported as such on the admission.
fn stale_files(state: &State, session: &Session, base: &[ObservedFile]) -> Vec<StaleFile> {
    let known = known_versions(state, &session.session_id);
    base.iter()
        .filter_map(|file| {
            let version = known.iter().find(|version| version.path == file.path)?;
            (version.state != file.state).then(|| StaleFile {
                path: file.path.clone(),
                read: version.state.clone(),
                current: file.state.clone(),
                // A peer whose own last version of the file is not what is
                // on disk now did not make it so: after a checkout, reset or
                // any other unreported change, it is not named.
                changed_by: last_peer_edit(
                    state,
                    &session.session_id,
                    &session.workspace_id,
                    &file.path,
                    version.at_ms,
                )
                .filter(|(peer, _)| {
                    known_versions(state, peer)
                        .iter()
                        .find(|known| known.path == file.path)
                        .is_none_or(|known| known.state == file.state)
                })
                .map(|(id, _)| id.to_owned()),
            })
        })
        .collect()
}

fn normalized_files(files: &[ObservedFile]) -> Result<Vec<ObservedFile>, CoreError> {
    let mut unique = BTreeMap::new();
    for file in files {
        validate_observed_file(&file.path, &file.state)?;
        if unique
            .insert(file.path.clone(), file.state.clone())
            .is_some()
        {
            return Err(error(
                "invalid_path",
                "Each observed file version may be named once",
            ));
        }
        if unique.len() > 64 {
            return Err(error(
                "too_many_paths",
                "A command can name at most 64 file versions",
            ));
        }
    }
    Ok(unique
        .into_iter()
        .map(|(path, state)| ObservedFile { path, state })
        .collect())
}

fn live_lease(state: &State, lease: &Lease, now_ms: u64) -> bool {
    lease.status == LeaseStatus::Active
        && lease.generation == state.generation
        && lease.expires_at_ms > now_ms
}

/// Select the other sessions whose current work is relevant to `session_id`:
/// same physical workspace or a worktree of the same repository, registered in
/// this generation, not known to have exited, and active within the horizon.
/// Returns the briefing and a signature that ignores ages, so an unchanged
/// situation is not delivered twice.
/// The signature leaves out messages: a delivered message is acknowledged,
/// so its absence afterwards is not a change. Also returns the keys of the
/// in-flight signatures it carries, to be remembered once delivered.
fn brief(
    state: &State,
    session_id: &str,
    max_peers: u32,
    handoffs: bool,
    now_ms: u64,
    (inflight, mentioned): (&[InflightSymbol], &BTreeSet<String>),
) -> Result<(Briefing, String, Vec<String>), CoreError> {
    let me = current_session(state, session_id)?;
    let repository = state
        .workspace_git
        .get(&me.workspace_id)
        .map(|git| git.repository_id.as_str());
    let work_paths = |id: &str| work_paths(state, id, now_ms);
    // Where a running session's last prompt pointed, while it is recent.
    let prompt_targets = |id: &str| -> BTreeSet<String> {
        state
            .activity
            .get(id)
            .filter(|activity| activity.generation == state.generation && !activity.ended)
            .and_then(|activity| activity.prompt_targets.as_ref())
            .filter(|targets| now_ms.saturating_sub(targets.at_ms) <= BRIEF_HORIZON_MS)
            .map(|targets| targets.paths.iter().cloned().collect())
            .unwrap_or_default()
    };
    // Where a running peer that shows its presence was recently looking,
    // within this session's window: newest first, none it edits or edited.
    let window_ms = presence_setting(state, session_id).window_ms;
    let looking = |id: &str, edited: &BTreeSet<String>| -> Vec<PresencePath> {
        if !presence_setting(state, id).shown {
            return Vec::new();
        }
        state
            .activity
            .get(id)
            .filter(|activity| activity.generation == state.generation && !activity.ended)
            .map(|activity| {
                activity
                    .looking
                    .iter()
                    .filter(|looked| {
                        now_ms.saturating_sub(looked.at_ms) <= window_ms
                            && !edited.contains(&looked.path)
                    })
                    .take(PRESENCE_PATHS)
                    .map(|looked| PresencePath {
                        path: looked.path.clone(),
                        kind: looked.kind,
                        age_s: now_ms.saturating_sub(looked.at_ms) / 1000,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let (_, _, mine) = work_paths(session_id);
    // What a peer's work or task is compared with: this session's work and
    // the files its own task points at.
    let mut mine_or_targeted = mine.clone();
    mine_or_targeted.extend(prompt_targets(session_id));
    let mut peers = Vec::new();
    for peer in state.sessions.values() {
        if peer.session_id == session_id || peer.generation != state.generation {
            continue;
        }
        // A subagent's parent started it and knows its own task and files;
        // told back to the subagent, they read as another session's. The
        // parent's held files are still refused to it at admission, and its
        // edits of files the subagent read are still stale reads.
        if me.parent.as_deref() == Some(peer.session_id.as_str()) {
            continue;
        }
        let same_workspace = peer.workspace_id == me.workspace_id;
        let peer_git = state.workspace_git.get(&peer.workspace_id);
        if !same_workspace
            && !(repository.is_some()
                && peer_git.map(|git| git.repository_id.as_str()) == repository)
        {
            continue;
        }
        let liveness = state
            .session_liveness
            .get(&peer.session_id)
            .map(|liveness| liveness.status);
        if liveness == Some(LivenessStatus::Exited) {
            continue;
        }
        let (mut editing, mut recent, all) = work_paths(&peer.session_id);
        let activity = state
            .activity
            .get(&peer.session_id)
            .filter(|activity| activity.generation == state.generation);
        // A session that only received briefings has shown no work of its own.
        let fresh = activity.is_some_and(|activity| {
            (activity.summary.is_some() || !activity.touched.is_empty())
                && now_ms.saturating_sub(activity.last_seen_ms()) <= BRIEF_HORIZON_MS
        });
        let intent = state
            .intents
            .get(&peer.session_id)
            .filter(|intent| intent.generation == state.generation);
        let targets = prompt_targets(&peer.session_id);
        // Every file the peer holds or edited within the horizon, not only
        // those listed: such a file is shown as edited, never as looked at.
        let mut edited: BTreeSet<String> = editing.iter().cloned().collect();
        edited.extend(activity.into_iter().flat_map(|activity| {
            activity
                .touched
                .iter()
                .filter(|touch| now_ms.saturating_sub(touch.at_ms) <= BRIEF_HORIZON_MS)
                .map(|touch| touch.path.clone())
        }));
        let looking = looking(&peer.session_id, &edited);
        if editing.is_empty()
            && intent.is_none()
            && !fresh
            && targets.is_empty()
            && looking.is_empty()
        {
            continue;
        }
        // A finished session matters only for the edits it left behind.
        if activity.is_some_and(|activity| activity.ended)
            && editing.is_empty()
            && recent.is_empty()
        {
            continue;
        }
        let Some(workspace) = state.workspaces.get(&peer.workspace_id) else {
            continue;
        };
        let overlap: Vec<String> = all
            .intersection(&mine_or_targeted)
            .take(BRIEF_PATHS)
            .cloned()
            .collect();
        editing.truncate(BRIEF_PATHS);
        recent.truncate(BRIEF_PATHS);
        let last_ms = activity.map(SessionActivity::last_seen_ms).unwrap_or(0);
        let relation = relation(me, peer);
        // A subagent's task is the asking parent's own request: its files
        // are news to the parent, its task line and declared intent are not.
        let task_known = relation == Some(PeerRelation::Subagent);
        if task_known && editing.is_empty() && recent.is_empty() {
            continue;
        }
        let target_overlap: Vec<String> = if task_known {
            Vec::new()
        } else {
            targets
                .intersection(&mine_or_targeted)
                .filter(|path| !overlap.contains(*path))
                .take(BRIEF_PATHS)
                .cloned()
                .collect()
        };
        // A peer without admissions, edits, intent or targets (one known by
        // its task line or by where it looked) ranks after every peer with
        // them, so presence never takes such a peer's place.
        let no_work =
            editing.is_empty() && recent.is_empty() && intent.is_none() && targets.is_empty();
        peers.push((
            (
                overlap.is_empty() && target_overlap.is_empty(),
                editing.is_empty(),
                no_work,
                u64::MAX - last_ms,
            ),
            PeerWork {
                session_id: peer.session_id.clone(),
                workspace_id: peer.workspace_id.clone(),
                workspace_root: workspace.root.clone(),
                same_workspace,
                branch_ref: peer_git.and_then(|git| git.branch_ref.clone()),
                liveness,
                ended: activity.is_some_and(|activity| activity.ended),
                // A declared intent is the session's own account of its
                // task: it replaces the line taken from the prompt.
                summary: activity
                    .filter(|activity| fresh && !activity.ended && !task_known)
                    .filter(|_| intent.is_none_or(|intent| intent.summary.is_empty()))
                    .and_then(|activity| activity.summary.clone()),
                intent: intent
                    .filter(|_| !task_known)
                    .map(|intent| intent.summary.clone()),
                editing,
                recent,
                overlap,
                target_overlap,
                relation,
                looking,
            },
        ));
    }
    // A peer known only by where it looked ranks after every other, so it
    // never takes a slot from a peer with work or a task.
    let rank = |(key, peer): &((bool, bool, bool, u64), PeerWork)| {
        (key.0, key.1, key.2, presence_only(peer), key.3)
    };
    peers.sort_by(|a, b| (rank(a), &a.1.session_id).cmp(&(rank(b), &b.1.session_id)));
    let total = peers.len();
    // Peers are chosen by their work within the byte budget first; where
    // they look is added after, in the space left, first peers first, so
    // presence never takes the place of another peer's work. A trail this
    // briefing may tell takes its encoding out of that budget, so the
    // reply holds both within its frame.
    let peer_bytes = BRIEF_BYTES
        - trail::owed(state, me, handoffs).map_or(0, |trail| {
            trail::json_cost(trail).min(trail::TRAIL_JSON_BYTES)
        });
    let mut bytes = 0;
    let mut peers: Vec<PeerWork> = peers
        .into_iter()
        .map(|(_, peer)| peer)
        .take(max_peers as usize)
        .take_while(|peer| {
            // Counted as encoded, escapes included, so quoted or control
            // characters cannot push the reply past its frame.
            let cost = 256
                + recent::json_len(&peer.workspace_root)
                + [&peer.summary, &peer.intent, &peer.branch_ref]
                    .into_iter()
                    .flatten()
                    .map(|text| recent::json_len(text))
                    .sum::<usize>()
                + peer
                    .editing
                    .iter()
                    .chain(&peer.overlap)
                    .chain(&peer.target_overlap)
                    .chain(peer.recent.iter().map(|recent| &recent.path))
                    .map(|path| recent::json_len(path) + 32)
                    .sum::<usize>();
            bytes += cost;
            if bytes > peer_bytes {
                bytes -= cost;
                return false;
            }
            true
        })
        .collect();
    // A peer there only for where it looked, without room for that, is left
    // out and counted.
    peers.retain_mut(|peer| {
        let presence: usize = peer
            .looking
            .iter()
            .map(|looked| recent::json_len(&looked.path) + 32)
            .sum();
        if bytes + presence <= peer_bytes {
            bytes += presence;
            return true;
        }
        let only = presence_only(peer);
        peer.looking.clear();
        !only
    });
    let mut stale_reads: Vec<StaleRead> = known_versions(state, session_id)
        .iter()
        .filter_map(|version| {
            let (peer, at_ms) = last_peer_edit(
                state,
                session_id,
                &me.workspace_id,
                &version.path,
                version.at_ms,
            )?;
            Some(StaleRead {
                path: version.path.clone(),
                session_id: peer.to_owned(),
                age_ms: now_ms.saturating_sub(at_ms),
            })
        })
        .collect();
    stale_reads.sort_by(|a, b| (a.age_ms, &a.path).cmp(&(b.age_ms, &b.path)));
    stale_reads.truncate(BRIEF_PATHS);
    let mut remote: Vec<RemotePeerWork> = state
        .remote_peers
        .values()
        .filter(|peer| {
            peer.generation == state.generation
                && now_ms.saturating_sub(peer.received_ms) <= REMOTE_HORIZON_MS
                && related_workspace(state, &me.workspace_id, &peer.workspace_id)
        })
        .flat_map(|peer| {
            let mine = &mine;
            peer.publication
                .work
                .iter()
                .map(move |work| RemotePeerWork {
                    name: peer.publication.name.clone(),
                    overlap: work
                        .editing
                        .iter()
                        .chain(&work.recent)
                        .filter(|path| mine.contains(*path))
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                    work: work.clone(),
                    received_age_ms: now_ms.saturating_sub(peer.received_ms),
                })
        })
        .collect();
    remote.sort_by(|a, b| {
        (
            a.overlap.is_empty(),
            a.work.editing.is_empty(),
            &a.name,
            &a.work.session,
        )
            .cmp(&(
                b.overlap.is_empty(),
                b.work.editing.is_empty(),
                &b.name,
                &b.work.session,
            ))
    });
    remote.truncate(BRIEF_PATHS);
    let concerned = concerned_paths(state, session_id, now_ms);
    // A notice already told between steps is not told again (issue #214).
    let (notice_ids, interface_changes): (Vec<String>, Vec<InterfaceImpact>) =
        interface_impacts(state, me, &concerned, now_ms)
            .filter(|(id, _)| {
                !told_notices(state, session_id)
                    .any(|told| told.notice_id == *id && told.via == NoticeVia::BetweenSteps)
            })
            .take(BRIEF_INTERFACE_NOTICES)
            .unzip();
    // Notes about the whole workspace, or about a file this session works
    // on; an anchored note, about its definition's file or a file that
    // mentions its name (roadmap E1). What still holds is chosen first,
    // each kind newest first.
    let mut notes: Vec<&Note> = state
        .notes
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|note| {
            note.author.as_deref() != Some(session_id)
                && anchors::note_reaches(note, &concerned, mentioned)
        })
        .collect();
    notes.sort_by_key(|note| AnchorState::rank(note.anchor_state));
    let notes: Vec<Note> = notes.into_iter().take(4).cloned().collect();
    // Withdrawals first, then messages owed to this session, each newest
    // first, as many as the text of one briefing is sure to hold beside an
    // owed uncoordinated change, whose line is reserved; the rest wait for
    // the next one. The first always fits.
    let uncoordinated: Vec<UncoordinatedNotice> = uncoordinated_owed(state, session_id, now_ms)
        .map(|(_, notice)| notice)
        .collect();
    let message_bytes = if uncoordinated.is_empty() {
        MESSAGE_BRIEF_BYTES
    } else {
        MESSAGE_BRIEF_BYTES - (UNCOORDINATED_LINE_LIMIT + 4)
    };
    let mut bytes = 0;
    let retracted: Vec<RetractedMessage> = state
        .retracted_notices
        .get(session_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(BRIEF_MESSAGES)
        .take_while(|retracted| {
            bytes += retracted_cost(retracted);
            bytes <= message_bytes
        })
        .cloned()
        .collect();
    let messages: Vec<Message> = state
        .messages
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|message| {
            owed_to(
                message,
                session_id,
                handoffs && takes_handoff(state, me, message),
            )
        })
        .take(BRIEF_MESSAGES)
        .take_while(|message| {
            bytes += message_cost(message);
            bytes <= message_bytes
        })
        .map(|message| Message {
            acknowledged: Vec::new(),
            ..message.clone()
        })
        .collect();
    // An ended session's trail (roadmap C9) shares the messages' room and
    // follows that session's own handoff: while the handoff is owed here
    // and waits for room, so does the trail.
    let used = retracted.iter().map(retracted_cost).sum::<usize>()
        + messages.iter().map(message_cost).sum::<usize>();
    let trail = trail::owed(state, me, handoffs)
        .filter(|_| used + TRAIL_BYTES < message_bytes)
        .filter(|trail| {
            state
                .messages
                .get(&me.workspace_id)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter(|message| {
                    message.from.as_deref() == Some(trail.session_id.as_str())
                        && matches!(message.to, Recipient::NextSession {})
                        && owed_to(message, session_id, takes_handoff(state, me, message))
                })
                .all(|handoff| {
                    messages
                        .iter()
                        .any(|shown| shown.message_id == handoff.message_id)
                })
        })
        .map(|trail| trail::view(state, trail, now_ms));
    // A checkout change matters to a session that read or edited files
    // here before it: what it knows may no longer be on disk.
    // A change a session's own shell command explains is not news to it;
    // others are told whose command it was.
    let mut checkout_ids = Vec::new();
    let changes = state
        .checkouts
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let runs = state
        .checkout_runs
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let explained_by =
        |change: &CheckoutChange| runs.iter().find(|run| run.explains(change, changes));
    let checkout_changes: Vec<CheckoutNotice> = changes
        .iter()
        .filter(|change| explained_by(change).is_none_or(|run| run.session_id != session_id))
        .filter(|change| {
            change.generation == state.generation
                && now_ms.saturating_sub(change.at_ms) <= BRIEF_HORIZON_MS
                && state
                    .activity
                    .get(session_id)
                    .filter(|activity| activity.generation == state.generation)
                    .is_some_and(|activity| {
                        activity
                            .known
                            .iter()
                            .any(|known| known.at_ms <= change.at_ms)
                            || activity
                                .touched
                                .iter()
                                .any(|touch| touch.at_ms <= change.at_ms)
                    })
        })
        .take(2)
        .map(|change| {
            checkout_ids.push(change.change_id.clone());
            let run = explained_by(change);
            CheckoutNotice {
                by_session: run.map(|run| run.session_id.clone()),
                command: run.and_then(|run| run.command.clone()),
                from_branch: change.from_branch.clone(),
                to_branch: change.to_branch.clone(),
                from_head: change.from_head.clone(),
                to_head: change.to_head.clone(),
                files: change.files,
                age_ms: now_ms.saturating_sub(change.at_ms),
            }
        })
        .collect();
    // In-flight signatures from other worktrees of the repository (roadmap
    // C7), each told once. Each line is delivered once, so it is chosen
    // only for a briefing with nothing else used up on delivery, whose
    // text is sure to hold it; otherwise every one is counted and waits.
    let quiet = messages.is_empty()
        && trail.is_none()
        && retracted.is_empty()
        && uncoordinated.is_empty()
        && !state.check_notices.contains_key(session_id);
    let owed: Vec<(String, InflightNotice)> = inflight
        .iter()
        .filter(|item| {
            item.workspace_id != me.workspace_id
                && related_workspace(state, &me.workspace_id, &item.workspace_id)
                && !inflight_told(state, session_id, item)
        })
        .filter_map(|item| {
            let key = inflight_key(item);
            let mut notice = inflight_notice(state, item)?;
            notice.mentioned_in.retain(|path| concerned.contains(path));
            notice.mentioned_in.truncate(INFLIGHT_MENTIONS);
            (!notice.mentioned_in.is_empty()).then_some((key, notice))
        })
        .collect();
    let mut inflight_keys = Vec::new();
    let mut inflight_notices = Vec::new();
    let mut bytes = 0;
    for (key, notice) in &owed {
        bytes += inflight_cost(notice);
        if !quiet || inflight_notices.len() == INFLIGHT_LINES || bytes > INFLIGHT_BYTES {
            break;
        }
        inflight_keys.push(key.clone());
        inflight_notices.push(notice.clone());
    }
    let mut signature = String::new();
    for key in &inflight_keys {
        signature.push_str(&format!("inflight|{key}\n"));
    }
    for id in &checkout_ids {
        signature.push_str(&format!("checkout|{id}\n"));
    }
    // A note whose definition changed state is news again.
    for note in &notes {
        match note.anchor_state {
            Some(held) => {
                signature.push_str(&format!("note|{}|{}\n", note.note_id, held.as_str()));
            }
            None => signature.push_str(&format!("note|{}\n", note.note_id)),
        }
    }
    for impact in &interface_changes {
        signature.push_str(&format!("interface|{}|", impact.path));
        for change in &impact.changes {
            signature.push_str(&format!(
                "{}:{}:{}|",
                change.name,
                change.change,
                change.after.as_deref().unwrap_or("")
            ));
        }
        signature.push_str(&impact.affected.join(","));
        signature.push('\n');
    }
    for item in &remote {
        signature.push_str(&format!(
            "remote|{}|{}|{}|{}|{}|{}\n",
            item.name,
            item.work.session,
            item.work.branch_ref.as_deref().unwrap_or(""),
            item.work.summary.as_deref().unwrap_or(""),
            item.work.editing.join(","),
            item.work.recent.join(",")
        ));
    }
    for stale in &stale_reads {
        signature.push_str(&format!("stale|{}|{}\n", stale.path, stale.session_id));
    }
    // A peer listed only for where it looked is presence, not work.
    for peer in peers.iter().filter(|peer| !presence_only(peer)) {
        signature.push_str(&peer.session_id);
        for part in [&peer.editing, &peer.overlap] {
            signature.push('|');
            signature.push_str(&part.join(","));
        }
        signature.push('|');
        for recent in &peer.recent {
            signature.push_str(&recent.path);
            signature.push(',');
        }
        // A peer in this checkout is on this session's own branch, learned
        // by the Git observer on its own thread, perhaps only after a
        // briefing: not news. A change of that branch is a checkout change.
        let branch = (!peer.same_workspace).then_some(&peer.branch_ref);
        for text in [Some(&peer.summary), Some(&peer.intent), branch] {
            signature.push('|');
            signature.push_str(text.and_then(Option::as_deref).unwrap_or(""));
        }
        // Only when present, so a briefing without one keeps the signature
        // it had before prompt targets existed and is not delivered again.
        if !peer.target_overlap.is_empty() {
            signature.push_str("|targets:");
            signature.push_str(&peer.target_overlap.join(","));
        }
        signature.push_str(if peer.ended { "|ended\n" } else { "\n" });
    }
    // Presence has a signature part of its own: alone it makes a briefing
    // new only at a session start or prompt, never mid-turn, so a peer's
    // reads do not interrupt a turn. The part is a set of bits, one pair per
    // peer and path (`presence_bits`), so that a briefing is news only for
    // a path not shown before: a path that ages out, or is read again, or
    // the same files read in another order, is not.
    let presence = presence_bits(&peers);
    // Without presence the signature is as it was before presence existed,
    // so a briefing stored then is not delivered again.
    let mut signature = fingerprint(&signature);
    if let Some(bits) = presence {
        signature.push('+');
        for byte in bits {
            signature.push_str(&format!("{byte:02x}"));
        }
    }
    Ok((
        Briefing {
            session_id: session_id.into(),
            changed: false,
            omitted_peers: u32::try_from(total - peers.len()).unwrap_or(u32::MAX),
            // Counted by `keep_changes` for a briefing of changes only.
            unchanged: 0,
            peers,
            stale_reads,
            remote,
            interface_changes,
            interface_notice_ids: notice_ids,
            omitted_inflight: u32::try_from(owed.len() - inflight_notices.len())
                .unwrap_or(u32::MAX),
            inflight: inflight_notices,
            notes,
            messages,
            retracted,
            checkout_changes,
            uncoordinated,
            // Filled in by the request that names the sessions (`Brief::left`).
            left_work: Vec::new(),
            trail,
            // One result per briefing, the newest: it always fits the
            // rendered text, and each delivered one is removed, so the rest
            // follow at the next briefings.
            checks: state
                .check_notices
                .get(session_id)
                .and_then(|notices| notices.first())
                .cloned()
                .into_iter()
                .collect(),
            recent: None,
        },
        signature,
        inflight_keys,
    ))
}

/// The key of one briefing item: what it says, without ages, so that the
/// same item told again has the same key (issue #222).
fn item_key(parts: &[&str]) -> u64 {
    let mut text = String::new();
    for part in parts {
        text.push_str(part);
        text.push('\u{1f}');
    }
    fingerprint_u64(&text)
}

/// A peer's own line without its files: who it is, where, its task and
/// whether it is still running. A peer in this checkout is on this
/// session's own branch, which the Git observer may learn only after a
/// briefing (as in `brief`'s signature): not part of its key.
fn peer_key(peer: &PeerWork) -> u64 {
    let branch = (!peer.same_workspace)
        .then_some(peer.branch_ref.as_deref())
        .flatten();
    let relation = peer.relation.map(|relation| format!("{relation:?}"));
    item_key(&[
        "peer",
        &peer.session_id,
        &peer.workspace_root,
        branch.unwrap_or(""),
        peer.summary.as_deref().unwrap_or(""),
        peer.intent.as_deref().unwrap_or(""),
        if peer.ended { "ended" } else { "" },
        if peer.liveness == Some(LivenessStatus::Unknown) {
            "unknown"
        } else {
            ""
        },
        relation.as_deref().unwrap_or(""),
    ])
}

fn peer_path_key(peer: &PeerWork, kind: &str, path: &str) -> u64 {
    item_key(&["peer", &peer.session_id, kind, path])
}

fn stale_key(stale: &StaleRead) -> u64 {
    item_key(&["stale", &stale.path, &stale.session_id])
}

fn remote_work_key(remote: &RemotePeerWork) -> u64 {
    let work = &remote.work;
    item_key(&[
        "remote",
        &remote.name,
        &work.session,
        work.branch_ref.as_deref().unwrap_or(""),
        work.summary.as_deref().unwrap_or(""),
        &work.editing.join(","),
        &work.recent.join(","),
        &remote.overlap.join(","),
    ])
}

fn interface_key(impact: &InterfaceImpact) -> u64 {
    let changes: Vec<String> = impact
        .changes
        .iter()
        .map(|change| {
            format!(
                "{}:{}:{}",
                change.name,
                change.change,
                change.after.as_deref().unwrap_or("")
            )
        })
        .collect();
    item_key(&[
        "interface",
        &impact.path,
        &changes.join("|"),
        &impact.affected.join(","),
    ])
}

/// A note and its anchor's state: an anchored note whose state changes is
/// news again (roadmap E1).
fn note_key(note: &Note) -> u64 {
    item_key(&[
        "note",
        &note.note_id,
        note.anchor_state.map_or("", AnchorState::as_str),
    ])
}

fn checkout_key(change: &CheckoutNotice) -> u64 {
    item_key(&[
        "checkout",
        change.from_branch.as_deref().unwrap_or(""),
        change.to_branch.as_deref().unwrap_or(""),
        change.from_head.as_deref().unwrap_or(""),
        change.to_head.as_deref().unwrap_or(""),
        change.by_session.as_deref().unwrap_or(""),
        &change.files.to_string(),
    ])
}

/// The kinds of a peer's listed paths, each with its own key.
const PEER_LISTS: [&str; 5] = ["overlap", "target", "editing", "recent", "looking"];

fn peer_paths<'a>(peer: &'a PeerWork, kind: &str) -> Vec<&'a str> {
    match kind {
        "overlap" => peer.overlap.iter().map(String::as_str).collect(),
        "target" => peer.target_overlap.iter().map(String::as_str).collect(),
        "editing" => peer.editing.iter().map(String::as_str).collect(),
        "recent" => peer
            .recent
            .iter()
            .map(|recent| recent.path.as_str())
            .collect(),
        _ => peer
            .looking
            .iter()
            .map(|looked| looked.path.as_str())
            .collect(),
    }
}

/// Every item key of `briefing`, in the order a reader meets them. Messages,
/// withdrawals, check results, in-flight signatures, uncoordinated changes
/// and left work are not keyed: each is delivered once already. A hook
/// names the keys of the items it could not place to `Command::ForgetTold`.
pub fn briefing_keys(briefing: &Briefing) -> Vec<u64> {
    let mut keys = Vec::new();
    keys.extend(briefing.stale_reads.iter().map(stale_key));
    keys.extend(briefing.checkout_changes.iter().map(checkout_key));
    keys.extend(briefing.notes.iter().map(note_key));
    keys.extend(briefing.interface_changes.iter().map(interface_key));
    for peer in &briefing.peers {
        keys.push(peer_key(peer));
        for kind in PEER_LISTS {
            keys.extend(
                peer_paths(peer, kind)
                    .into_iter()
                    .map(|path| peer_path_key(peer, kind, path)),
            );
        }
    }
    keys.extend(briefing.remote.iter().map(remote_work_key));
    keys
}

/// Leave in `briefing` only what `told` does not hold, count the rest in
/// `unchanged`, and return the keys left in. A safety item stays in when
/// `acted` says the session read or edited one of its files since it was
/// last told: a peer's hold on a file this session works on, an interface
/// change to names its files use, a file it read that a peer has since
/// edited. Mid-turn, where a peer is looking is not news by itself
/// (`once`: something delivered once is in the briefing); it waits, untold,
/// for a briefing that has other news.
fn keep_changes(
    briefing: &mut Briefing,
    told: &BTreeSet<u64>,
    acted: &dyn Fn(&str) -> bool,
    mid_turn: bool,
    once: bool,
) -> BTreeSet<u64> {
    let mut shown = BTreeSet::new();
    let mut unchanged = 0_usize;
    let mut keep = |key: u64, safety: bool| {
        let fresh = !told.contains(&key) || safety;
        if fresh {
            shown.insert(key);
        } else {
            unchanged += 1;
        }
        fresh
    };
    briefing
        .stale_reads
        .retain(|stale| keep(stale_key(stale), acted(&stale.path)));
    briefing
        .checkout_changes
        .retain(|change| keep(checkout_key(change), false));
    briefing.notes.retain(|note| keep(note_key(note), false));
    briefing.interface_changes.retain(|impact| {
        keep(
            interface_key(impact),
            impact.affected.iter().any(|path| acted(path)),
        )
    });
    // Presence counted apart: alone, mid-turn, it is held back.
    let mut presence = Vec::new();
    let mut work_news = false;
    briefing.peers.retain_mut(|peer| {
        let only_presence = presence_only(peer);
        // A file this peer holds in this checkout that this session works
        // on: refused to this session while held.
        let held: BTreeSet<String> = peer
            .overlap
            .iter()
            .filter(|path| peer.same_workspace && peer.editing.contains(path))
            .cloned()
            .collect();
        let header = peer_key(peer);
        let header_fresh = keep(header, false);
        if header_fresh {
            if only_presence {
                presence.push(header);
            } else {
                work_news = true;
            }
        }
        let mut any = false;
        let id = peer.session_id.clone();
        let mut list = |kind: &str, path: &str| {
            let key = item_key(&["peer", &id, kind, path]);
            let safety =
                matches!(kind, "overlap" | "editing") && held.contains(path) && acted(path);
            let fresh = keep(key, safety);
            if fresh {
                any = true;
                if kind == "looking" {
                    presence.push(key);
                } else {
                    work_news = true;
                }
            }
            fresh
        };
        peer.overlap.retain(|path| list("overlap", path));
        peer.target_overlap.retain(|path| list("target", path));
        peer.editing.retain(|path| list("editing", path));
        peer.recent.retain(|recent| list("recent", &recent.path));
        peer.looking.retain(|looked| list("looking", &looked.path));
        if !header_fresh {
            // Its task was told: the line names the peer and what is new.
            peer.summary = None;
            peer.intent = None;
        }
        header_fresh || any
    });
    briefing
        .remote
        .retain(|remote| keep(remote_work_key(remote), false));
    let other_news = work_news
        || once
        || !briefing.stale_reads.is_empty()
        || !briefing.checkout_changes.is_empty()
        || !briefing.notes.is_empty()
        || !briefing.interface_changes.is_empty()
        || !briefing.remote.is_empty()
        || !briefing.inflight.is_empty();
    if mid_turn && !other_news && !presence.is_empty() {
        for key in &presence {
            shown.remove(key);
        }
        // A peer is left in only for its own line, told here for the first
        // time (and not only for where it looked), or for a file still in.
        // A told line had its task cleared, so its key no longer matches.
        briefing.peers.retain_mut(|peer| {
            peer.looking.clear();
            shown.contains(&peer_key(peer))
                || !(peer.overlap.is_empty()
                    && peer.target_overlap.is_empty()
                    && peer.editing.is_empty()
                    && peer.recent.is_empty())
        });
    }
    briefing.unchanged = u32::try_from(unchanged).unwrap_or(u32::MAX);
    shown
}

/// Whether a `changes_only` briefing still has an item to place in context.
fn shown_anything(briefing: &Briefing) -> bool {
    !briefing.peers.is_empty()
        || !briefing.stale_reads.is_empty()
        || !briefing.remote.is_empty()
        || !briefing.interface_changes.is_empty()
        || !briefing.inflight.is_empty()
        || !briefing.notes.is_empty()
        || !briefing.checkout_changes.is_empty()
}

/// Bytes in the presence part of a briefing signature: with the work
/// fingerprint and its `+`, the signature stays within the 64 bytes an
/// event holds.
const PRESENCE_SIGNATURE_BYTES: usize = 23;

/// The presence part of a briefing signature: for each peer and path it
/// shows as presence, two bits set from that pair's fingerprint. None when
/// it shows none. A path whose bits another pair already set is not news on
/// its own; it is shown with the next briefing that is.
fn presence_bits(peers: &[PeerWork]) -> Option<[u8; PRESENCE_SIGNATURE_BYTES]> {
    const BITS: u64 = (PRESENCE_SIGNATURE_BYTES * 8) as u64;
    let mut bits = [0u8; PRESENCE_SIGNATURE_BYTES];
    let mut any = false;
    for peer in peers {
        for looked in &peer.looking {
            let hash = fingerprint_u64(&format!("{}|{}", peer.session_id, looked.path));
            for bit in [hash % BITS, (hash >> 32) % BITS] {
                bits[(bit / 8) as usize] |= 1 << (bit % 8);
            }
            any = true;
        }
    }
    any.then_some(bits)
}

/// Whether a briefing signature shows presence that `previous` did not:
/// a bit set in its presence part that is clear in the previous one. A
/// previous signature without a presence part, or from before this form,
/// has shown none.
fn new_presence(signature: &str, previous: &str) -> bool {
    let bits = |signature: &str| -> Option<Vec<u8>> {
        let (_, hex) = signature.split_once('+')?;
        if hex.len() != PRESENCE_SIGNATURE_BYTES * 2 {
            return None;
        }
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
            .collect()
    };
    let Some(now) = bits(signature) else {
        return false;
    };
    match bits(previous) {
        Some(before) => now
            .iter()
            .zip(&before)
            .any(|(now, before)| now & !before != 0),
        None => true,
    }
}

/// A briefing signature without its presence part.
fn without_presence(signature: &str) -> &str {
    signature
        .split_once('+')
        .map_or(signature, |(work, _)| work)
}

/// A peer in a briefing only for where it looked.
fn presence_only(peer: &PeerWork) -> bool {
    !peer.looking.is_empty()
        && peer.editing.is_empty()
        && peer.recent.is_empty()
        && peer.overlap.is_empty()
        && peer.target_overlap.is_empty()
        && peer.summary.is_none()
        && peer.intent.is_none()
}

/// Whether an active, unexpired lease of this workspace holds `path`.
fn leased(state: &State, workspace_id: &str, path: &str, now_ms: u64) -> bool {
    state.leases.values().any(|lease| {
        lease.workspace_id == workspace_id
            && lease.status == LeaseStatus::Active
            && lease.generation == state.generation
            && lease.expires_at_ms > now_ms
            && lease.paths.iter().any(|held| held == path)
    })
}

/// Whether a session of this workspace reported reading or writing the file
/// at this version.
fn explained(state: &State, workspace_id: &str, file: &ObservedFile) -> bool {
    state
        .sessions
        .values()
        .filter(|session| session.workspace_id == workspace_id)
        .filter_map(|session| state.activity.get(&session.session_id))
        .any(|activity| {
            activity
                .known
                .iter()
                .any(|known| known.path == file.path && known.state == file.state)
        })
}

/// Whether a session of this workspace reported reading or writing the file
/// at or after `from_ms`, at any version.
fn reported_since(state: &State, workspace_id: &str, path: &str, from_ms: u64) -> bool {
    state
        .sessions
        .values()
        .filter(|session| session.workspace_id == workspace_id)
        .filter_map(|session| state.activity.get(&session.session_id))
        .any(|activity| {
            activity
                .known
                .iter()
                .any(|known| known.path == path && known.at_ms >= from_ms)
        })
}

/// The Codex sessions registered in this workspace in this generation, most
/// recently active first. An ended or exited one is kept: Codex reports no
/// session end, and a resumed session is a new process.
fn codex_sessions(state: &State, workspace_id: &str) -> Vec<String> {
    let mut sessions: Vec<(u64, &str)> = state
        .sessions
        .values()
        .filter(|session| {
            session.workspace_id == workspace_id
                && session.generation == state.generation
                && session
                    .session_id
                    .split_once('.')
                    .is_some_and(|(host, _)| host == CODEX_HOST)
        })
        .map(|session| {
            let last = state
                .activity
                .get(&session.session_id)
                .filter(|activity| activity.generation == state.generation)
                .map_or(0, |activity| activity.last_ms);
            (last, session.session_id.as_str())
        })
        .collect();
    sessions.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    sessions.into_iter().map(|(_, id)| id.to_owned()).collect()
}

/// The files of an uncoordinated change that are still at a version no
/// session of its workspace reported: a report arriving after the scan
/// explains the rest.
fn still_unexplained(state: &State, change: &UncoordinatedChange) -> Vec<String> {
    change
        .files
        .iter()
        .filter(|file| !explained(state, &change.workspace_id, file))
        .map(|file| file.path.clone())
        .collect()
}

/// This workspace's uncoordinated changes of this generation, within the
/// briefing horizon, with a file still unexplained. Newest first.
fn held_uncoordinated<'a>(
    state: &'a State,
    workspace_id: &str,
    now_ms: u64,
) -> impl Iterator<Item = &'a UncoordinatedChange> {
    state
        .uncoordinated
        .get(workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(move |change| {
            change.generation == state.generation
                && now_ms.saturating_sub(change.at_ms) <= BRIEF_HORIZON_MS
                && !still_unexplained(state, change).is_empty()
        })
}

/// The uncoordinated change owed to this session's next briefing, the
/// newest, with the notice it carries: one always fits the rendered text, and
/// each is delivered once, so the rest follow at the next briefings.
fn uncoordinated_owed<'a>(
    state: &'a State,
    session_id: &'a str,
    now_ms: u64,
) -> impl Iterator<Item = (&'a UncoordinatedChange, UncoordinatedNotice)> {
    let workspace = state
        .sessions
        .get(session_id)
        .map_or("", |session| session.workspace_id.as_str());
    held_uncoordinated(state, workspace, now_ms)
        .filter(move |change| !change.delivered.iter().any(|held| held == session_id))
        .take(1)
        .map(move |change| {
            (
                change,
                UncoordinatedNotice {
                    sessions: change.sessions.clone(),
                    paths: still_unexplained(state, change),
                    age_ms: now_ms.saturating_sub(change.at_ms),
                },
            )
        })
}

fn validate_uncoordinated(state: &State, change: &UncoordinatedChange) -> Result<(), CoreError> {
    validate_fact_header(
        state,
        &change.change_id,
        &change.workspace_id,
        change.generation,
    )?;
    if change.files.is_empty()
        || change.files.len() > MAX_UNCOORDINATED_FILES
        || normalized_files(&change.files)? != change.files
        || change.sessions.is_empty()
        || change.sessions.len() > MAX_UNCOORDINATED_SESSIONS
        || change.sessions.iter().any(|session| {
            state
                .sessions
                .get(session)
                .is_none_or(|known| known.workspace_id != change.workspace_id)
        })
    {
        return Err(error(
            "invalid_event",
            &format!(
                "An uncoordinated change names 1 to {MAX_UNCOORDINATED_FILES} sorted, unique files and 1 to {MAX_UNCOORDINATED_SESSIONS} sessions of its workspace"
            ),
        ));
    }
    Ok(())
}

/// Whether the session wrote a file since its last tool batch or stop.
fn wrote_since_boundary(state: &State, session_id: &str) -> bool {
    state
        .activity
        .get(session_id)
        .is_some_and(|activity| activity.wrote_since_boundary)
}

fn validate_auto_check(state: &State, check: &AutoCheck) -> Result<(), CoreError> {
    validate_known_workspace(state, &check.workspace_id)?;
    validate_check_command(&check.name, &check.argv, check.timeout_s)?;
    if let Some(wider) = &check.on_interface_change {
        validate_check_command(&wider.name, &wider.argv, wider.timeout_s)?;
        if wider.name == check.name {
            return Err(error(
                "invalid_check",
                "The wider check has a name of its own: its records are told apart from the regular check's by name",
            ));
        }
    }
    Ok(())
}

/// One automatic command's name, arguments and time limit.
fn validate_check_command(name: &str, argv: &[String], timeout_s: u32) -> Result<(), CoreError> {
    validate_id(name, "check name")?;
    if name.len() > 64
        || argv.is_empty()
        || argv.len() > MAX_AUTO_CHECK_ARGS
        || argv[0].is_empty()
        || argv.iter().any(|arg| {
            arg.is_empty()
                || arg.len() > MAX_AUTO_CHECK_ARG_BYTES
                || arg.chars().any(char::is_control)
        })
    {
        return Err(error(
            "invalid_check",
            &format!(
                "An automatic check has a name of at most 64 characters and a command of 1 to {MAX_AUTO_CHECK_ARGS} arguments, each 1 to {MAX_AUTO_CHECK_ARG_BYTES} bytes without control characters"
            ),
        ));
    }
    if !(1..=MAX_AUTO_CHECK_TIMEOUT_S).contains(&timeout_s) {
        return Err(error(
            "invalid_limit",
            &format!("An automatic check's time limit is 1 to {MAX_AUTO_CHECK_TIMEOUT_S} seconds"),
        ));
    }
    Ok(())
}

/// A report's mentioning files come only with an interface change, are
/// valid paths and number at most [`MAX_REPORT_DEPENDENT_PATHS`].
fn validate_dependent_paths(report: &CheckReport) -> Result<(), CoreError> {
    if report.dependent_paths.is_empty() {
        return Ok(());
    }
    if report.interface.is_none() || report.dependent_paths.len() > MAX_REPORT_DEPENDENT_PATHS {
        return Err(error(
            "invalid_check_report",
            &format!(
                "A check report names mentioning files only for an interface change, at most {MAX_REPORT_DEPENDENT_PATHS}"
            ),
        ));
    }
    for path in &report.dependent_paths {
        validate_relative_path(path)?;
    }
    Ok(())
}

/// A wider check's cause names one valid change, a valid path and at most
/// [`MAX_CAUSE_DEPENDENTS`] valid mentioning files.
fn validate_interface_cause(cause: &InterfaceCause) -> Result<(), CoreError> {
    validate_relative_path(&cause.path)?;
    if cause.dependents.is_empty() || cause.dependents.len() > MAX_CAUSE_DEPENDENTS {
        return Err(error(
            "invalid_check_report",
            &format!(
                "A wider check's interface change names 1 to {MAX_CAUSE_DEPENDENTS} files that mention it"
            ),
        ));
    }
    for path in &cause.dependents {
        validate_relative_path(path)?;
    }
    // The change itself is shaped as in an interface notice.
    validate_notice(&InterfaceNotice {
        notice_id: "cause".into(),
        workspace_id: String::new(),
        path: cause.path.clone(),
        changes: vec![cause.change.clone()],
        dependents: Vec::new(),
        elsewhere: Vec::new(),
        by_session: None,
        at_ms: 0,
        generation: 0,
    })
}

/// What session `id` works on: the paths its live leases hold, the files it
/// touched within the briefing horizon (with their ages), and those with the
/// paths of its declared intent.
fn work_paths(
    state: &State,
    id: &str,
    now_ms: u64,
) -> (Vec<String>, Vec<RecentPath>, BTreeSet<String>) {
    let editing: Vec<String> = state
        .leases
        .values()
        .filter(|lease| lease.session_id == id && live_lease(state, lease, now_ms))
        .flat_map(|lease| lease.paths.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let recent: Vec<RecentPath> = state
        .activity
        .get(id)
        .filter(|activity| activity.generation == state.generation)
        .map(|activity| {
            activity
                .touched
                .iter()
                .filter(|touch| now_ms.saturating_sub(touch.at_ms) <= BRIEF_HORIZON_MS)
                .map(|touch| RecentPath {
                    path: touch.path.clone(),
                    age_ms: now_ms.saturating_sub(touch.at_ms),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut all: BTreeSet<String> = editing.iter().cloned().collect();
    all.extend(recent.iter().map(|recent| recent.path.clone()));
    if let Some(intent) = state
        .intents
        .get(id)
        .filter(|intent| intent.generation == state.generation)
    {
        all.extend(intent.paths.iter().cloned());
    }
    (editing, recent, all)
}

/// Files session `id` works on or has read: what a changed interface could
/// break for it.
fn concerned_paths(state: &State, id: &str, now_ms: u64) -> BTreeSet<String> {
    let (_, _, mut concerned) = work_paths(state, id, now_ms);
    concerned.extend(
        known_versions(state, id)
            .iter()
            .map(|version| version.path.clone()),
    );
    concerned
}

/// The interface notices of `me`'s workspace, in the order they are held, that touch
/// `concerned` (the files `me` works on or has read), within the briefing
/// horizon and not of `me`'s own making: each notice's ID and what a
/// briefing shows of it. One rule for the briefing and for between-step
/// delivery (issue #214).
fn interface_impacts<'a>(
    state: &'a State,
    me: &'a Session,
    concerned: &'a BTreeSet<String>,
    now_ms: u64,
) -> impl Iterator<Item = (String, InterfaceImpact)> + 'a {
    state
        .interface_notices
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(move |notice| {
            notice.generation == state.generation
                && now_ms.saturating_sub(notice.at_ms) <= BRIEF_HORIZON_MS
                && notice.by_session.as_deref() != Some(me.session_id.as_str())
        })
        .filter_map(move |notice| {
            let affected: Vec<String> = notice
                .dependents
                .iter()
                .filter(|path| concerned.contains(*path))
                .take(BRIEF_PATHS)
                .cloned()
                .collect();
            (!affected.is_empty()).then(|| {
                (
                    notice.notice_id.clone(),
                    InterfaceImpact {
                        path: notice.path.clone(),
                        by_session: notice.by_session.clone(),
                        changes: notice.changes.clone(),
                        affected,
                        age_ms: now_ms.saturating_sub(notice.at_ms),
                    },
                )
            })
        })
}

/// The interface notices told to `session_id` in this generation (issue
/// #214), newest first.
fn told_notices<'a>(state: &'a State, session_id: &str) -> impl Iterator<Item = &'a ToldNotice> {
    state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .into_iter()
        .flat_map(|activity| &activity.notices_told)
}

/// The running sessions of the workspace, in this daemon's lifetime, that
/// work on or have read one of `paths`, as a briefing counts them.
fn concerned_sessions(
    state: &State,
    workspace_id: &str,
    paths: &[String],
    now_ms: u64,
) -> Vec<String> {
    state
        .sessions
        .values()
        .filter(|session| {
            session.workspace_id == workspace_id && session.generation == state.generation
        })
        .filter(|session| {
            state
                .activity
                .get(&session.session_id)
                .is_some_and(|activity| !activity.ended)
        })
        .filter(|session| {
            let concerned = concerned_paths(state, &session.session_id, now_ms);
            paths.iter().any(|path| concerned.contains(path))
        })
        .map(|session| session.session_id.clone())
        .collect()
}

/// The sessions are this workspace's, the tail is bounded, and the notice
/// names a record of that workspace.
fn validate_check_report(
    state: &State,
    workspace_id: &str,
    event: &Event,
) -> Result<(), CoreError> {
    let Event::CheckReported {
        session_ids,
        notice,
    } = event
    else {
        return Err(error("invalid_event", "Not a check report"));
    };
    if session_ids.is_empty() || session_ids.len() > MAX_CHECK_REPORT_SESSIONS {
        return Err(error(
            "invalid_check_report",
            &format!("A check report names 1 to {MAX_CHECK_REPORT_SESSIONS} sessions"),
        ));
    }
    for session_id in session_ids {
        if known_session(state, session_id)?.workspace_id != workspace_id {
            return Err(error(
                "invalid_check_report",
                "A check report names only sessions of the checked workspace",
            ));
        }
    }
    if notice.tail.len() > MAX_CHECK_TAIL_BYTES || notice.tail.contains('\0') {
        return Err(error(
            "invalid_check_report",
            &format!("A check's output tail is at most {MAX_CHECK_TAIL_BYTES} bytes without NUL"),
        ));
    }
    validate_request_id(&notice.check_id)?;
    validate_oid(&notice.tree_oid)?;
    // A comparison (issue #69) is whole: a baseline, and a class for every
    // failure, or neither.
    let compared = notice.baseline_check_id.is_some();
    if let Some(baseline) = &notice.baseline_check_id {
        validate_request_id(baseline)?;
    }
    let names_ok = notice.fixed.len() <= MAX_FIXED_NAMED
        && notice.fixed.iter().all(|test| {
            !test.is_empty()
                && test.len() <= MAX_TEST_NAME_BYTES
                && !test.chars().any(char::is_control)
        });
    if !names_ok
        || notice.writers.len() > MAX_WRITERS_NAMED
        || (!compared && !(notice.fixed.is_empty() && notice.writers.is_empty()))
        || notice
            .failures
            .iter()
            .any(|failure| failure.class.is_some() != compared)
    {
        return Err(error(
            "invalid_check_report",
            &format!(
                "A check report compared with a baseline classes every failure and names at most {MAX_FIXED_NAMED} fixed tests and {MAX_WRITERS_NAMED} writers; one not compared has none of them"
            ),
        ));
    }
    for writer in &notice.writers {
        validate_id(writer, "session ID")?;
    }
    if let Some(cause) = &notice.interface {
        validate_interface_cause(cause)?;
    }
    // A grade is the wider check's result, and only a wider check has one.
    if notice.interface_grade != interface_grade(notice.interface.as_deref(), &notice.result) {
        return Err(error(
            "invalid_check_report",
            "An interface grade is kept for a passed wider check and broke for a failed one",
        ));
    }
    Ok(())
}

/// The grade of a wider check with `interface`, from its result.
fn interface_grade(
    interface: Option<&InterfaceCause>,
    result: &CheckResult,
) -> Option<InterfaceGrade> {
    match (interface, result) {
        (Some(_), CheckResult::Passed {}) => Some(InterfaceGrade::Kept),
        (Some(_), CheckResult::Failed { .. }) => Some(InterfaceGrade::Broke),
        _ => None,
    }
}

/// A write was recorded in the workspace: its write generation moves on, so
/// no check result is compared with one from before it to learn which tests
/// flip.
fn wrote_in(state: &mut State, workspace_id: &str, session_id: Option<&str>) {
    classes::wrote(state, workspace_id, session_id);
}

fn validate_forget_flaky(name: Option<&str>, test: &str) -> Result<(), CoreError> {
    if let Some(name) = name {
        validate_id(name, "check name")?;
    }
    if test.is_empty()
        || test.len() > MAX_TEST_NAME_BYTES
        || test.trim() != test
        || test.chars().any(char::is_control)
    {
        return Err(error(
            "invalid_test_name",
            &format!(
                "A test name is 1 to {MAX_TEST_NAME_BYTES} bytes on one line without surrounding spaces"
            ),
        ));
    }
    Ok(())
}

fn validate_prompt_summary(summary: &str) -> Result<(), CoreError> {
    if summary.is_empty()
        || summary.len() > MAX_PROMPT_SUMMARY_BYTES
        || summary.chars().any(char::is_control)
    {
        return Err(error(
            "invalid_summary",
            &format!(
                "A prompt summary is one line of 1 to {MAX_PROMPT_SUMMARY_BYTES} UTF-8 bytes without control characters"
            ),
        ));
    }
    Ok(())
}

fn validate_line_digests(digests: &[String]) -> Result<(), CoreError> {
    if digests.len() > MAX_PROMPT_LINES
        || digests.iter().any(|digest| {
            digest.is_empty()
                || digest.len() > MAX_PROMPT_LINE_DIGEST
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(error(
            "invalid_line_digests",
            &format!(
                "Prompt line digests are at most {MAX_PROMPT_LINES} lower-case hex strings of 1 to {MAX_PROMPT_LINE_DIGEST} characters"
            ),
        ));
    }
    Ok(())
}

/// How many leading lines of `digests` the latest prompt of another live
/// session in `session_id`'s checkout or repository begins with, at most:
/// a session registered in this generation, not ended and not known to have
/// exited. Zero when the session is unknown or nobody shares a line.
fn shared_prompt_lines(state: &State, session_id: &str, digests: &[String]) -> u32 {
    let Some(me) = state.sessions.get(session_id) else {
        return 0;
    };
    let repository = |workspace_id: &str| {
        state
            .workspace_git
            .get(workspace_id)
            .map(|git| git.repository_id.as_str())
    };
    let mine = repository(&me.workspace_id);
    state
        .sessions
        .values()
        .filter(|peer| peer.session_id != session_id && peer.generation == state.generation)
        .filter(|peer| {
            peer.workspace_id == me.workspace_id
                || (mine.is_some() && repository(&peer.workspace_id) == mine)
        })
        .filter(|peer| {
            state
                .session_liveness
                .get(&peer.session_id)
                .is_none_or(|liveness| liveness.status != LivenessStatus::Exited)
        })
        .filter_map(|peer| state.activity.get(&peer.session_id))
        .filter(|activity| activity.generation == state.generation && !activity.ended)
        .map(|activity| {
            digests
                .iter()
                .zip(&activity.line_digests)
                .take_while(|(mine, theirs)| mine == theirs)
                .count()
        })
        .max()
        .unwrap_or(0) as u32
}

/// Whether a registered session is over: the host reported it ended, or its
/// process is known to have exited. A session LangSpan cannot tell is over
/// (killed while its liveness was unknown) counts as live.
pub(crate) fn finished(state: &State, session_id: &str) -> bool {
    state
        .activity
        .get(session_id)
        .is_some_and(|activity| activity.ended)
        || state
            .session_liveness
            .get(session_id)
            .is_some_and(|liveness| liveness.status == LivenessStatus::Exited)
}

/// Sessions a briefing's `left` may name, at most.
pub const MAX_LEFT: usize = 64;

/// `session_id` takes over `from`'s uncommitted work (issue #161).
fn adopt(state: &State, session_id: &str, from: &str, now_ms: u64) -> Result<Decision, CoreError> {
    // The adopter is the caller, at work: registered, perhaps not yet again
    // since the coordinator restarted.
    let me = known_session(state, session_id)?;
    let theirs = known_session(state, from)?;
    if session_id == from {
        return Err(error(
            "invalid_adoption",
            "A session cannot adopt its own work; name the ended session whose work to take over",
        ));
    }
    if finished(state, session_id) {
        return Err(error(
            "session_ended",
            "This session has ended; adopt from the session that continues the task",
        ));
    }
    if theirs.workspace_id != me.workspace_id {
        return Err(error(
            "other_workspace",
            &format!(
                "That session worked in workspace {:?}, not this session's {:?}; only an ended session's work in the same checkout can be adopted",
                theirs.workspace_id, me.workspace_id
            ),
        ));
    }
    if !finished(state, from) {
        return Err(error(
            "session_live",
            "That session has not ended: LangSpan has neither its end nor its process's exit. Ask it to commit its own work, or adopt once it has ended",
        ));
    }
    // An adopter that ended after its adoption was recorded, before its
    // adapter credited the lines, leaves them under the session it adopted;
    // that session cannot be adopted again. Passing the adoption on credits
    // them too: every session whose adoptions lead to `from`. A session that
    // has resumed since it was adopted owns what it writes again, so it is
    // left out, and so is what its own adoptions lead to.
    let mut earlier: Vec<String> = Vec::new();
    let mut pending = vec![from.to_owned()];
    while let Some(adopter) = pending.pop() {
        for adoption in state.adoptions.values() {
            if adoption.session_id != adopter
                || adoption.workspace_id != me.workspace_id
                || adoption.from == from
                || earlier.contains(&adoption.from)
                || !finished(state, &adoption.from)
            {
                continue;
            }
            if earlier.len() >= MAX_LEFT {
                return Err(error(
                    "adoption_chain",
                    &format!(
                        "More than {MAX_LEFT} earlier adoptions lead to that session; commit part of the work, then adopt again"
                    ),
                ));
            }
            earlier.push(adoption.from.clone());
            pending.push(adoption.from.clone());
        }
    }
    earlier.sort();
    // Ending a session does not end its subagents: one still running keeps
    // its lines.
    let live: Vec<String> = state
        .sessions
        .values()
        .filter(|session| {
            session.workspace_id == me.workspace_id
                && [from]
                    .into_iter()
                    .chain(earlier.iter().map(String::as_str))
                    .any(|source| {
                        session.parent.as_deref() == Some(source)
                            || session
                                .session_id
                                .strip_prefix(source)
                                .is_some_and(|rest| rest.starts_with('.'))
                    })
                && !finished(state, &session.session_id)
        })
        .map(|session| session.session_id.clone())
        .collect();
    if live.len() > MAX_LEFT {
        return Err(error(
            "session_live",
            &format!(
                "More than {MAX_LEFT} subagents of that session have not ended; adopt once they have"
            ),
        ));
    }
    // A subagent another session adopted is that session's to credit, even
    // when its adapter has not yet rewritten the journal.
    let mut kept: Vec<String> = state
        .adoptions
        .values()
        .filter(|adoption| {
            adoption.workspace_id == me.workspace_id
                && adoption.session_id != session_id
                && !earlier.contains(&adoption.from)
                && [from]
                    .into_iter()
                    .chain(earlier.iter().map(String::as_str))
                    .any(|source| {
                        adoption
                            .from
                            .strip_prefix(source)
                            .is_some_and(|rest| rest.starts_with('.'))
                    })
        })
        .map(|adoption| adoption.from.clone())
        .collect();
    kept.sort();
    if kept.len() > MAX_LEFT {
        return Err(error(
            "adoption_chain",
            &format!(
                "Other sessions adopted more than {MAX_LEFT} subagents of that session; commit part of the work, then adopt again"
            ),
        ));
    }
    let root = state
        .workspaces
        .get(&me.workspace_id)
        .map(|workspace| workspace.root.clone())
        .unwrap_or_default();
    if let Some(adoption) = state.adoptions.get(from) {
        if adoption.session_id != session_id {
            return Err(error(
                "already_adopted",
                &format!(
                    "Session {} already adopted that session's work; adopt from {} once it has ended",
                    adoption.session_id, adoption.session_id
                ),
            ));
        }
        return Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::Adopted {
                adoption: adoption.clone(),
                root,
                recorded: false,
                live,
                earlier,
                kept,
            },
        });
    }
    let adoption = Adoption {
        from: from.to_owned(),
        session_id: session_id.to_owned(),
        workspace_id: me.workspace_id.clone(),
        reason: adopt_reason(state, &me.workspace_id, session_id, from),
        at_ms: now_ms,
        generation: state.generation,
    };
    Ok(Decision {
        events: vec![Event::SessionAdopted {
            adoption: adoption.clone(),
        }],
        outcome: Outcome::Adopted {
            adoption,
            root,
            recorded: true,
            live,
            earlier,
            kept,
        },
    })
}

/// Why `session_id` may be continuing `from`'s task, newest evidence
/// first: a message `from` sent it (addressed to it, or a handoff it took),
/// then the same task line.
fn adopt_reason(
    state: &State,
    workspace_id: &str,
    session_id: &str,
    from: &str,
) -> Option<AdoptReason> {
    let held = state.messages.get(workspace_id).into_iter().flatten();
    let history = state
        .message_history
        .get(workspace_id)
        .into_iter()
        .flatten();
    let handoff = held.chain(history).find(|message| {
        message.from.as_deref() == Some(from)
            && !message.retracted
            && match &message.to {
                Recipient::Session { session_id: to } => to == session_id,
                Recipient::NextSession {} => {
                    message.acknowledged.iter().any(|taker| taker == session_id)
                }
                Recipient::Everyone {} => false,
            }
    });
    if let Some(message) = handoff {
        return Some(AdoptReason::Handoff {
            message_id: message.message_id.clone(),
        });
    }
    let task = |id: &str| {
        state
            .activity
            .get(id)
            .and_then(|activity| activity.summary.as_deref())
    };
    (task(from).is_some() && task(from) == task(session_id)).then_some(AdoptReason::SameTask {})
}

/// Of `left`, the sessions a briefing names as having left uncommitted
/// work: of `session_id`'s workspace, over, not adopted, and neither the
/// session nor one of its own subagents.
fn left_work(state: &State, session_id: &str, left: &[String]) -> Result<Vec<LeftWork>, CoreError> {
    if left.len() > MAX_LEFT {
        return Err(error(
            "invalid_left",
            &format!("A briefing names at most {MAX_LEFT} sessions that left work"),
        ));
    }
    for id in left {
        validate_id(id, "session ID")?;
    }
    if left.is_empty() {
        return Ok(Vec::new());
    }
    let me = known_session(state, session_id)?;
    let mut found: Vec<LeftWork> = Vec::new();
    for id in left {
        let Some(session) = state.sessions.get(id) else {
            continue;
        };
        if id == session_id
            || session.parent.as_deref() == Some(session_id)
            || session.workspace_id != me.workspace_id
            || !finished(state, id)
            || state.adoptions.contains_key(id)
            || found.iter().any(|work| &work.session_id == id)
        {
            continue;
        }
        found.push(LeftWork {
            session_id: id.clone(),
            summary: state
                .activity
                .get(id)
                .and_then(|activity| activity.summary.clone()),
        });
        if found.len() == BRIEF_LEFT_WORK {
            break;
        }
    }
    Ok(found)
}

fn validate_workspace_git(state: &State, git: &WorkspaceGit) -> Result<(), CoreError> {
    validate_id(&git.workspace_id, "workspace ID")?;
    validate_id(&git.repository_id, "repository ID")?;
    if !state.workspaces.contains_key(&git.workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Register the workspace before recording its repository",
        ));
    }
    if let Some(reference) = &git.branch_ref
        && (reference.len() > 512
            || reference.chars().any(char::is_control)
            || !reference
                .strip_prefix("refs/heads/")
                .is_some_and(|name| !name.is_empty()))
    {
        return Err(error(
            "invalid_branch_ref",
            "A workspace branch is a nonempty refs/heads/ name, at most 512 bytes without control characters",
        ));
    }
    Ok(())
}

fn activity_mut<'a>(state: &'a mut State, session_id: &str) -> &'a mut SessionActivity {
    let generation = state.generation;
    let activity = state
        .activity
        .entry(session_id.to_owned())
        .or_insert_with(|| SessionActivity {
            session_id: session_id.to_owned(),
            generation,
            ..SessionActivity::default()
        });
    if activity.generation != generation {
        // Times from an earlier generation are not comparable; start again.
        // A version the session read is still what it read: keep it, undated.
        let mut known = take_all(&mut activity.known);
        for version in &mut known {
            version.at_ms = 0;
        }
        *activity = SessionActivity {
            session_id: session_id.to_owned(),
            generation,
            known,
            ..SessionActivity::default()
        };
    }
    // Any new record from the session means it is running again.
    activity.ended = false;
    activity
}

fn take_all<T: Default>(value: &mut T) -> T {
    core::mem::take(value)
}

struct ObservationInput<'a> {
    session_id: &'a str,
    operation_id: &'a str,
    source: &'a str,
    tool: &'a str,
    paths: &'a [String],
    result: ToolResult,
    lease_id: Option<&'a str>,
    operation: Option<&'a ToolIdentity>,
}

fn decide_observation(
    state: &State,
    input: ObservationInput<'_>,
    request_id: &str,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    // A stale session may still report what happened before a restart.
    let session = known_session(state, input.session_id)?;
    if let Some(operation) = input.operation {
        validate_tool_identity(operation)?;
    } else {
        validate_id(input.operation_id, "operation ID")?;
        validate_id(input.source, "observation source")?;
        validate_id(input.tool, "tool name")?;
    }
    let paths = normalized_paths(input.paths)?;
    let mut scope: BTreeSet<String> = paths.iter().cloned().collect();
    let lease_relation = match input.lease_id {
        None => LeaseRelation::NotAdmitted,
        Some(id) => {
            validate_id(id, "lease ID")?;
            match state.leases.get(id) {
                None => LeaseRelation::NotAdmitted,
                Some(lease) => {
                    if lease.session_id != input.session_id {
                        return Err(error(
                            "not_holder",
                            "An observation can name only its own session's admission",
                        ));
                    }
                    scope.extend(lease.paths.iter().cloned());
                    if lease.operation.as_ref() != input.operation {
                        LeaseRelation::InputMismatch
                    } else if paths
                        .iter()
                        .any(|path| lease.paths.binary_search(path).is_err())
                    {
                        LeaseRelation::ScopeMismatch
                    } else {
                        lease_relation(state, lease, now_ms)
                    }
                }
            }
        }
    };
    let scope: Vec<String> = scope.into_iter().collect();
    // Judged as the named admission was: by its regions, if it had any.
    let regions = input
        .lease_id
        .and_then(|id| state.leases.get(id))
        .map(|lease| lease.regions.as_slice())
        .unwrap_or_default();
    let concurrent_holders = conflicts(state, &session.workspace_id, &scope, regions, now_ms)
        .into_iter()
        .filter(|holder| input.lease_id != Some(holder.lease_id.as_str()))
        .collect();
    let observation = Observation {
        observation_id: request_id.into(),
        session_id: input.session_id.into(),
        workspace_id: session.workspace_id.clone(),
        operation_id: input.operation_id.into(),
        source: input.source.into(),
        tool: input.tool.into(),
        paths,
        result: input.result,
        lease_id: input.lease_id.map(str::to_owned),
        lease_relation,
        concurrent_holders,
        observed_at_ms: now_ms,
        generation: state.generation,
    };
    Ok(Decision {
        events: vec![Event::OperationObserved {
            observation: observation.clone(),
        }],
        outcome: Outcome::Observed { observation },
    })
}

/// The regions of `regions` that name the version `base` gives for their
/// path. Regions are trusted only for the version the hook saw on disk when
/// it asked; any other path is held whole.
fn trusted_regions<'a>(
    regions: &[PathRegions],
    paths: &[String],
    base: impl Iterator<Item = &'a ObservedFile> + Clone,
) -> Result<Vec<PathRegions>, CoreError> {
    Ok(normalized_regions(regions, paths)?
        .into_iter()
        .filter(|located| {
            base.clone().any(|file| {
                file.path == located.path
                    && matches!(&file.state, FileState::Present { digest, .. } if *digest == located.digest)
            })
        })
        .collect())
}

/// The current admissions an admission of `paths` by `session_id` would
/// meet. A host's tool call is never held back by an earlier tool call of
/// the same session.
fn holders_of(
    state: &State,
    session_id: &str,
    workspace_id: &str,
    paths: &[String],
    regions: &[PathRegions],
    tool_call: bool,
    now_ms: u64,
) -> Vec<Conflict> {
    conflicts(state, workspace_id, paths, regions, now_ms)
        .into_iter()
        .filter(|holder| {
            !(tool_call
                && holder.session_id == session_id
                && state
                    .leases
                    .get(&holder.lease_id)
                    .is_some_and(|lease| lease.operation.is_some()))
        })
        .collect()
}

fn conflicts(
    state: &State,
    workspace_id: &str,
    paths: &[String],
    regions: &[PathRegions],
    now_ms: u64,
) -> Vec<Conflict> {
    let located = |list: &'_ [PathRegions], path: &str| -> Option<PathRegions> {
        list.iter().find(|entry| entry.path == path).cloned()
    };
    state
        .leases
        .values()
        .filter_map(|lease| {
            if lease.status != LeaseStatus::Active
                || lease.generation != state.generation
                || lease.expires_at_ms <= now_ms
                || lease.workspace_id != workspace_id
            {
                return None;
            }
            let intersection: Vec<_> = paths
                .iter()
                .filter(|path| lease.paths.binary_search(path).is_ok())
                .filter(|path| {
                    !separate(
                        located(regions, path).as_ref(),
                        located(&lease.regions, path).as_ref(),
                    )
                })
                .cloned()
                .collect();
            if intersection.is_empty() {
                None
            } else {
                Some(Conflict {
                    lease_id: lease.lease_id.clone(),
                    session_id: lease.session_id.clone(),
                    regions: intersection
                        .iter()
                        .filter_map(|path| located(&lease.regions, path))
                        .collect(),
                    paths: intersection,
                })
            }
        })
        .collect()
}

/// True only when two edits of one path cannot touch the same code: both were
/// located, and either they name different symbols, neither inside the other,
/// or they name disjoint line ranges of the same version of the file. Every
/// other case, including a whole-file edit on either side and a mix of
/// symbols and lines, is a conflict as before regions existed.
fn separate(ours: Option<&PathRegions>, theirs: Option<&PathRegions>) -> bool {
    let (Some(ours), Some(theirs)) = (ours, theirs) else {
        return false;
    };
    ours.regions.iter().all(|a| {
        theirs.regions.iter().all(|b| match (a, b) {
            (Region::Symbol { key: a, .. }, Region::Symbol { key: b, .. }) => {
                !(a.starts_with(b) || b.starts_with(a))
            }
            (
                Region::Lines {
                    start_line: a_start,
                    end_line: a_end,
                },
                Region::Lines {
                    start_line: b_start,
                    end_line: b_end,
                },
            ) => ours.digest == theirs.digest && (a_end < b_start || b_end < a_start),
            _ => false,
        })
    })
}

/// Validate regions against the admitted (normalized) paths and return them
/// with each path's regions sorted and deduplicated.
fn normalized_regions(
    regions: &[PathRegions],
    paths: &[String],
) -> Result<Vec<PathRegions>, CoreError> {
    let invalid = |message: &str| Err(error("invalid_regions", message));
    let mut seen = BTreeSet::new();
    let mut total = 0;
    let mut normalized = Vec::with_capacity(regions.len());
    for located in regions {
        if paths.binary_search(&located.path).is_err() {
            return invalid("Regions name a path this admission does not request");
        }
        if !seen.insert(located.path.as_str()) {
            return invalid("Each path's regions may be given once");
        }
        if !is_hex_digest(&located.digest, 64) {
            return invalid("A file version digest must contain exactly 64 hexadecimal characters");
        }
        if located.regions.is_empty() {
            return invalid("Give at least one region, or leave the path out to hold it whole");
        }
        let symbols = located
            .regions
            .iter()
            .filter(|region| matches!(region, Region::Symbol { .. }))
            .count();
        if symbols != 0 && symbols != located.regions.len() {
            return invalid("One path's regions are all symbols or all line ranges");
        }
        for region in &located.regions {
            let (start_line, end_line) = match region {
                Region::Symbol {
                    key,
                    digest,
                    start_line,
                    end_line,
                } => {
                    if key.is_empty()
                        || key.len() > MAX_KEY_DEPTH
                        || key.iter().any(|name| {
                            name.is_empty()
                                || name.len() > MAX_KEY_NAME_BYTES
                                || name.chars().any(char::is_control)
                        })
                    {
                        return invalid(
                            "A symbol key is 1 to 8 names of 1 to 128 bytes without control characters",
                        );
                    }
                    if !is_hex_digest(digest, 64) {
                        return invalid(
                            "A symbol digest must contain exactly 64 hexadecimal characters",
                        );
                    }
                    (*start_line, *end_line)
                }
                Region::Lines {
                    start_line,
                    end_line,
                } => (*start_line, *end_line),
            };
            if start_line == 0 || start_line > end_line || end_line > MAX_REGION_LINE {
                return invalid("Region lines are one-based, ordered and at most 2000000");
            }
        }
        let mut sorted = located.regions.clone();
        sorted.sort();
        sorted.dedup();
        total += sorted.len();
        if total > MAX_REGIONS {
            return invalid(
                "An admission can name at most 64 distinct regions; leave paths out to hold them whole",
            );
        }
        normalized.push(PathRegions {
            path: located.path.clone(),
            digest: located.digest.clone(),
            regions: sorted,
        });
    }
    normalized.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(normalized)
}

/// Replay an event, rejecting invalid references or transitions before mutation.
/// Events carry decisions rather than a clock: expiry ordering is established by
/// the writer calling `expiry_events` before command admission and persistence.
pub fn apply(state: &mut State, event: &Event) -> Result<(), CoreError> {
    match event {
        Event::Booted { generation } => {
            if *generation <= state.generation {
                return Err(error(
                    "invalid_event",
                    "A boot event must advance the daemon generation",
                ));
            }
            state.generation = *generation;
            // The new generation's anchor, if any, is the next event.
            state.clock_anchor = None;
            // A refusal is settled within the run it was given in.
            state.unsettled.clear();
            for lease in state.leases.values_mut() {
                if lease.status == LeaseStatus::Active {
                    lease.status = LeaseStatus::Invalidated;
                }
            }
            for liveness in state.session_liveness.values_mut() {
                liveness.status = LivenessStatus::Unknown;
            }
            // Times do not carry across generations: any edit a peer reports
            // from now on is after every read made before the restart.
            for activity in state.activity.values_mut() {
                for version in &mut activity.known {
                    version.at_ms = 0;
                }
            }
            // What teammates published is current only once received again.
            state.remote_peers.clear();
            // A landing that was under way is no longer known to be alive,
            // nor is one that was waiting.
            state.turns.clear();
            state.turn_queues.clear();
        }
        Event::ClockAnchored { anchor } => {
            if anchor.generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A clock anchor names the generation that is starting",
                ));
            }
            state.clock_anchor = Some(*anchor);
        }
        Event::WriteRefused {
            session_id,
            workspace_id,
            kind,
            paths,
            peer_session_id,
            input_digest,
            at_ms,
            generation,
        } => {
            known_session(state, session_id)?;
            validate_known_workspace(state, workspace_id)?;
            normalized_paths(paths)?;
            if paths.len() > REFUSAL_PATHS
                || peer_session_id
                    .as_ref()
                    .is_some_and(|peer| peer.len() > 200)
                || input_digest
                    .as_ref()
                    .is_some_and(|digest| !is_hex_digest(digest, 64))
            {
                return Err(error(
                    "invalid_event",
                    "A refusal keeps at most 8 paths, one peer session ID and a SHA-256 input digest",
                ));
            }
            status_line::remember(state, workspace_id, *at_ms, *generation);
            recent::remember_refusal(state, workspace_id, paths, *at_ms, *generation);
            policy::remember_refusal(state, session_id, *kind, paths, *at_ms, *generation);
        }
        Event::WriteWaited {
            session_id,
            workspace_id,
            paths,
            holder_session_id,
            waited_ms,
            ..
        } => policy::validate_waited(
            state,
            session_id,
            workspace_id,
            paths,
            holder_session_id.as_ref(),
            *waited_ms,
        )?,
        Event::RefusalSettled {
            session_id,
            kind,
            paths,
            refused_at_ms,
            ..
        } => policy::apply_settled(state, session_id, *kind, paths, *refused_at_ms)?,
        Event::PolicySet {
            workspace_id,
            case,
            policy,
            wait_ms,
        } => policy::apply_set(state, workspace_id, *case, *policy, *wait_ms)?,
        Event::WorkspaceRegistered { workspace } => {
            validate_workspace(state, workspace)?;
            state
                .workspaces
                .insert(workspace.workspace_id.clone(), workspace.clone());
        }
        Event::WorkspaceMoved {
            workspace,
            from_root,
        } => workspaces::apply_moved(state, workspace, from_root)?,
        Event::WorkspaceRemoved { workspace_id } => {
            workspaces::apply_removed(state, workspace_id)?;
        }
        Event::SessionRegistered { session } => {
            validate_session_registration(state, session)?;
            // A new registration, or one in another workspace, has written
            // nothing yet; a daemon restart's registers again in place.
            if state
                .sessions
                .get(&session.session_id)
                .is_none_or(|old| old.workspace_id != session.workspace_id)
            {
                state.first_writes.remove(&session.session_id);
            }
            state
                .sessions
                .insert(session.session_id.clone(), session.clone());
        }
        Event::SessionLivenessObserved { liveness } => {
            validate_session_liveness(state, liveness)?;
            state
                .session_liveness
                .insert(liveness.session_id.clone(), liveness.clone());
        }
        Event::SessionSeen {
            session_id,
            at_ms,
            generation,
        } => {
            current_session(state, session_id)?;
            if *generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A hook-run record must belong to the current daemon generation",
                ));
            }
            let activity = activity_mut(state, session_id);
            activity.last_hook_ms = activity.last_hook_ms.max(*at_ms);
        }
        Event::IntentSet { intent } => {
            let session = current_session(state, &intent.session_id)?;
            if intent.workspace_id != session.workspace_id || intent.generation != state.generation
            {
                return Err(error(
                    "invalid_event",
                    "Intent must match its session's workspace and current generation",
                ));
            }
            validate_canonical_paths(&intent.paths)?;
            validate_summary(&intent.summary)?;
            state
                .intents
                .insert(intent.session_id.clone(), intent.clone());
        }
        Event::IntentEnded { session_id } => {
            current_session(state, session_id)?;
            if !state.intents.contains_key(session_id) {
                return Err(error(
                    "invalid_event",
                    "Intent-ended event references no existing intent",
                ));
            }
            state.intents.remove(session_id);
        }
        Event::LeaseGranted { lease } => {
            validate_id(&lease.lease_id, "lease ID")?;
            if let Some(operation) = &lease.operation {
                validate_tool_identity(operation)?;
            }
            let session = current_session(state, &lease.session_id)?;
            if lease.workspace_id != session.workspace_id
                || lease.generation != state.generation
                || lease.status != LeaseStatus::Active
                || lease.expires_at_ms == 0
            {
                return Err(error(
                    "invalid_event",
                    "Granted lease must be active in its session's current workspace and generation with a positive deadline",
                ));
            }
            validate_canonical_paths(&lease.paths)?;
            if normalized_regions(&lease.regions, &lease.paths)? != lease.regions {
                return Err(error(
                    "invalid_event",
                    "Granted lease regions must be in their normalized order",
                ));
            }
            if state.leases.contains_key(&lease.lease_id) {
                return Err(error(
                    "invalid_event",
                    "A lease-granted event cannot reuse an existing lease ID",
                ));
            }
            state.leases.insert(lease.lease_id.clone(), lease.clone());
        }
        Event::OperationObserved { observation } => {
            validate_request_id(&observation.observation_id)?;
            let session = known_session(state, &observation.session_id)?;
            for (id, label) in [
                (&observation.operation_id, "operation ID"),
                (&observation.source, "observation source"),
                (&observation.tool, "tool name"),
            ] {
                validate_id(id, label)?;
            }
            validate_canonical_paths(&observation.paths)?;
            if observation.workspace_id != session.workspace_id
                || observation.generation != state.generation
            {
                return Err(error(
                    "invalid_event",
                    "Observation must match its session's workspace and the current generation",
                ));
            }
            if let Some(lease) = observation
                .lease_id
                .as_ref()
                .and_then(|id| state.leases.get(id))
                && lease.session_id != observation.session_id
            {
                return Err(error(
                    "invalid_event",
                    "Observation names another session's admission",
                ));
            }
            // Observations grant nothing. Older versions classified scope and
            // same-session holders differently; preserve those historical
            // decisions rather than rejecting an existing event log.
            if observation.result == ToolResult::Succeeded {
                if !observation.paths.is_empty() {
                    wrote_in(
                        state,
                        &observation.workspace_id,
                        Some(&observation.session_id),
                    );
                }
                let activity = activity_mut(state, &observation.session_id);
                activity.wrote_since_boundary |= !observation.paths.is_empty();
                // The file is no longer the version the session knew. Until
                // the adapter reports what it became, its base is unknown,
                // never the old version.
                activity
                    .known
                    .retain(|version| !observation.paths.contains(&version.path));
                activity.last_ms = activity.last_ms.max(observation.observed_at_ms);
                for path in &observation.paths {
                    activity.touched.retain(|touch| &touch.path != path);
                    activity.touched.insert(
                        0,
                        TouchedPath {
                            path: path.clone(),
                            at_ms: observation.observed_at_ms,
                        },
                    );
                }
                activity.touched.truncate(MAX_TOUCHED);
            }
        }
        Event::WorkspaceGitObserved { git } => {
            validate_workspace_git(state, git)?;
            state
                .workspace_git
                .insert(git.workspace_id.clone(), git.clone());
        }
        Event::SessionPrompted {
            session_id,
            summary,
            line_digests,
            at_ms,
            generation,
        } => {
            current_session(state, session_id)?;
            validate_prompt_summary(summary)?;
            validate_line_digests(line_digests)?;
            if *generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A prompt record must belong to the current daemon generation",
                ));
            }
            let activity = activity_mut(state, session_id);
            activity.summary = Some(summary.clone());
            activity.line_digests = line_digests.clone();
            activity.last_ms = activity.last_ms.max(*at_ms);
        }
        Event::PromptTargetsSet {
            session_id,
            paths,
            at_ms,
            generation,
        } => {
            current_session(state, session_id)?;
            if prompt_target_paths(paths)? != *paths {
                return Err(error(
                    "invalid_event",
                    "Prompt targets must be recorded sorted and without repeats",
                ));
            }
            if *generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A prompt targets record must belong to the current daemon generation",
                ));
            }
            let activity = activity_mut(state, session_id);
            activity.prompt_targets = Some(PromptTargets {
                paths: paths.clone(),
                at_ms: *at_ms,
            });
            activity.last_ms = activity.last_ms.max(*at_ms);
        }
        Event::PresenceObserved {
            session_id,
            kind,
            paths,
            at_ms,
            generation,
        } => {
            current_session(state, session_id)?;
            if presence_paths(*kind, paths)? != *paths || *generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A presence record names sorted, unique paths in the current generation",
                ));
            }
            // Recorded only while shown; a record kept from before the
            // session turned presence off adds nothing.
            if presence_setting(state, session_id).shown {
                look(activity_mut(state, session_id), *kind, paths, *at_ms);
            }
        }
        Event::PresenceSet {
            session_id,
            setting,
        } => {
            known_session(state, session_id)?;
            validate_presence_setting(setting)?;
            if !setting.shown {
                // Off forgets what was recorded: turning it on again later
                // never shows a path looked at while it was off or before.
                if let Some(activity) = state.activity.get_mut(session_id) {
                    activity.looking.clear();
                }
            }
            if *setting == PresenceSetting::default() {
                state.presence.remove(session_id);
            } else {
                state.presence.insert(session_id.clone(), *setting);
            }
        }
        Event::SessionEnded { session_id } => {
            known_session(state, session_id)?;
            activity_mut(state, session_id).ended = true;
            state.intents.remove(session_id);
        }
        Event::SessionAdopted { adoption } => {
            let adopter = known_session(state, &adoption.session_id)?;
            let ended = known_session(state, &adoption.from)?;
            if adoption.from == adoption.session_id
                || adopter.workspace_id != adoption.workspace_id
                || ended.workspace_id != adoption.workspace_id
            {
                return Err(error(
                    "invalid_event",
                    "An adoption names two different sessions of its workspace",
                ));
            }
            if let Some(AdoptReason::Handoff { message_id }) = &adoption.reason {
                validate_request_id(message_id)?;
            }
            state
                .adoptions
                .insert(adoption.from.clone(), adoption.clone());
        }
        Event::TrailRecorded { trail } => {
            trail::validate_trail(state, trail)?;
            let trails = state.trails.entry(trail.workspace_id.clone()).or_default();
            trails.retain(|kept| kept.session_id != trail.session_id);
            trails.insert(0, trail.clone());
            trails.truncate(MAX_TRAILS);
        }
        Event::TrailTaken {
            workspace_id,
            from,
            session_id,
        } => {
            known_session(state, session_id)?;
            // A trail the cap already dropped has nothing left to mark.
            if let Some(trail) = state
                .trails
                .get_mut(workspace_id)
                .and_then(|trails| trails.iter_mut().find(|trail| &trail.session_id == from))
                && trail.taken_by.is_none()
            {
                trail.taken_by = Some(session_id.clone());
            }
        }
        // Boundaries in the log. The admissions a stop released ended in
        // their own `LeaseCompleted` events.
        Event::SessionStopped {
            session_id,
            failure,
            generation,
            prompt_id,
            ..
        } => {
            known_session(state, session_id)?;
            if let Some(failure) = failure {
                validate_failure(failure)?;
            }
            if let Some(prompt_id) = prompt_id {
                validate_request_id(prompt_id)?;
            }
            if *generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A stop record must belong to the current daemon generation",
                ));
            }
            // Only the mark and the count: a boundary does not bring an
            // ended session back or start an activity record.
            if let Some(activity) = state.activity.get_mut(session_id) {
                activity.wrote_since_boundary = false;
                activity.stops = activity.stops.saturating_add(1);
                if let Some(prompt_id) = prompt_id {
                    let stopped = &mut activity.stopped_prompts;
                    stopped.retain(|held| held != prompt_id);
                    stopped.insert(0, prompt_id.clone());
                    stopped.truncate(MAX_STOPPED_PROMPTS);
                }
            }
        }
        Event::ToolBatchObserved {
            session_id,
            operation_ids,
            calls,
            generation,
            ..
        } => {
            current_session(state, session_id)?;
            validate_batch(operation_ids, *calls)?;
            if *generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A tool batch record must belong to the current daemon generation",
                ));
            }
            // Only the mark: a boundary does not bring an ended session back
            // or start an activity record.
            if let Some(activity) = state.activity.get_mut(session_id) {
                activity.wrote_since_boundary = false;
            }
        }
        Event::SourceObserved {
            session_id,
            files,
            origin,
            at_ms,
            generation,
        } => {
            current_session(state, session_id)?;
            if *generation != state.generation
                || files.is_empty()
                || normalized_files(files)? != *files
            {
                return Err(error(
                    "invalid_event",
                    "A source observation names sorted, unique files in the current generation",
                ));
            }
            let activity = activity_mut(state, session_id);
            for file in files.iter().rev() {
                activity.known.retain(|version| version.path != file.path);
                activity.known.insert(
                    0,
                    KnownVersion {
                        path: file.path.clone(),
                        state: file.state.clone(),
                        at_ms: *at_ms,
                    },
                );
            }
            activity.known.truncate(MAX_KNOWN);
            // A reported read is also where the session is looking
            // (roadmap A13), unless its presence is off.
            if *origin == SourceOrigin::Read && presence_setting(state, session_id).shown {
                let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
                look(
                    activity_mut(state, session_id),
                    PresenceKind::Read,
                    &paths,
                    *at_ms,
                );
            }
        }
        Event::RemoteEnabled { link } => {
            validate_remote_link(state, link)?;
            state
                .remote_links
                .insert(link.workspace_id.clone(), link.clone());
        }
        Event::RemoteDisabled { workspace_id } => {
            let Some(link) = state.remote_links.get_mut(workspace_id) else {
                return Err(error(
                    "invalid_event",
                    "Publication was not enabled for this workspace",
                ));
            };
            link.enabled = false;
            link.withdrawn = false;
            // What teammates published stays theirs, but this workspace no
            // longer takes part, so it stops showing it.
            state
                .remote_peers
                .retain(|_, peer| &peer.workspace_id != workspace_id);
        }
        Event::RemotePublished {
            workspace_id,
            sequence,
            signature,
            ..
        } => {
            let link = state.remote_links.get_mut(workspace_id).ok_or_else(|| {
                error(
                    "invalid_event",
                    "A publication record names a workspace that does not publish",
                )
            })?;
            if *sequence != link.sequence + 1 || signature.len() > 64 {
                return Err(error(
                    "invalid_event",
                    "A publication record must carry the next sequence",
                ));
            }
            link.sequence = *sequence;
            link.signature = signature.clone();
        }
        Event::RemoteObserved { peer } => {
            validate_publication(&peer.publication)?;
            if peer.generation != state.generation
                || !state
                    .remote_links
                    .get(&peer.workspace_id)
                    .is_some_and(|link| link.enabled)
            {
                return Err(error(
                    "invalid_event",
                    "A received publication belongs to a publishing workspace in the current generation",
                ));
            }
            state.remote_peers.insert(
                remote_key(&peer.workspace_id, &peer.publication.participant),
                peer.clone(),
            );
        }
        Event::RemoteWithdrawn { workspace_id } => {
            if let Some(link) = state.remote_links.get_mut(workspace_id) {
                link.withdrawn = true;
            }
        }
        Event::Remembered { note, .. } => {
            validate_note(note)?;
            validate_known_workspace(state, &note.workspace_id)?;
            let notes = state.notes.entry(note.workspace_id.clone()).or_default();
            notes.retain(|held| held.note_id != note.note_id);
            notes.insert(0, note.clone());
            notes.truncate(MAX_NOTES);
        }
        Event::Forgotten {
            workspace_id,
            note_id,
        } => {
            if let Some(notes) = state.notes.get_mut(workspace_id) {
                notes.retain(|note| &note.note_id != note_id);
            }
        }
        Event::NoteAnchorChanged {
            workspace_id,
            note_id,
            anchor,
            state: anchor_state,
        } => {
            // A note forgotten or dropped since is left alone.
            if let Some(note) = state
                .notes
                .get_mut(workspace_id)
                .and_then(|notes| notes.iter_mut().find(|note| &note.note_id == note_id))
                .filter(|note| note.anchor.is_some())
            {
                let mut changed = note.clone();
                changed.anchor = Some(anchor.clone());
                changed.anchor_state = Some(*anchor_state);
                validate_note(&changed)?;
                *note = changed;
            }
        }
        Event::MessageSent { message, .. } => {
            validate_message(message)?;
            validate_known_workspace(state, &message.workspace_id)?;
            if !message.acknowledged.is_empty() || message.retracted {
                return Err(error(
                    "invalid_event",
                    "A message is sent unacknowledged and not withdrawn",
                ));
            }
            let history = state
                .message_history
                .entry(message.workspace_id.clone())
                .or_default();
            history.retain(|held| held.message_id != message.message_id);
            history.insert(0, message.clone());
            history.truncate(MAX_MESSAGES);
            let held = state
                .messages
                .entry(message.workspace_id.clone())
                .or_default();
            held.retain(|held| held.message_id != message.message_id);
            held.insert(0, message.clone());
            // The oldest held past the cap are dropped unread.
            let pushed_out: Vec<String> = held
                .split_off(held.len().min(MAX_MESSAGES))
                .into_iter()
                .map(|dropped| dropped.message_id)
                .collect();
            let dropped = state
                .dropped_messages
                .entry(message.workspace_id.clone())
                .or_default();
            dropped.retain(|id| *id != message.message_id);
            for id in pushed_out.into_iter().rev() {
                dropped.retain(|held| *held != id);
                dropped.insert(0, id);
            }
            dropped.truncate(MAX_MESSAGES);
            if dropped.is_empty() {
                state.dropped_messages.remove(&message.workspace_id);
            }
        }
        Event::MessageAcknowledged {
            workspace_id,
            message_id,
            session_id,
            ..
        } => {
            known_session(state, session_id)?;
            // Sessions of this generation that a message for everyone is
            // still owed to; once none is left it is done.
            let current: Vec<String> = state
                .sessions
                .values()
                .filter(|session| {
                    &session.workspace_id == workspace_id && session.generation == state.generation
                })
                .map(|session| session.session_id.clone())
                .collect();
            if let Some(messages) = state.messages.get_mut(workspace_id) {
                // A message the cap already dropped has nothing left to mark.
                if let Some(index) = messages
                    .iter()
                    .position(|message| &message.message_id == message_id)
                {
                    let message = &mut messages[index];
                    // The history keeps every reader, for a withdrawal.
                    if owed_to(message, session_id, true)
                        && let Some(record) =
                            state
                                .message_history
                                .get_mut(workspace_id)
                                .and_then(|history| {
                                    history
                                        .iter_mut()
                                        .find(|record| &record.message_id == message_id)
                                })
                        && !record.acknowledged.contains(session_id)
                    {
                        record.acknowledged.push(session_id.clone());
                    }
                    let done = match message.to {
                        Recipient::Everyone {} => {
                            if !message.acknowledged.contains(session_id) {
                                message.acknowledged.push(session_id.clone());
                            }
                            current
                                .iter()
                                .all(|session| !owed_to(message, session, false))
                        }
                        Recipient::Session { .. } | Recipient::NextSession {} => true,
                    };
                    if done {
                        messages.remove(index);
                    }
                }
            }
        }
        Event::MessageRetracted {
            workspace_id,
            message_id,
            by,
        } => {
            validate_known_workspace(state, workspace_id)?;
            validate_request_id(message_id)?;
            if let Some(by) = by {
                validate_id(by, "session ID")?;
            }
            let readers = readers(state, workspace_id, message_id);
            let held = state.messages.get_mut(workspace_id).and_then(|messages| {
                let index = messages
                    .iter()
                    .position(|message| &message.message_id == message_id)?;
                Some(messages.remove(index))
            });
            if state.messages.get(workspace_id).is_some_and(Vec::is_empty) {
                state.messages.remove(workspace_id);
            }
            // The history stays the latest messages sent, in send order: a
            // held message older than it is withdrawn without joining it,
            // so the recap does not list that one.
            let record = state
                .message_history
                .get_mut(workspace_id)
                .and_then(|history| {
                    history
                        .iter_mut()
                        .find(|record| &record.message_id == message_id)
                });
            let withdrawn = match record {
                Some(record) => {
                    record.retracted = true;
                    Some(record.clone())
                }
                None => held,
            };
            // A message no longer known has nothing left to withdraw.
            if let Some(withdrawn) = withdrawn {
                let (text, cut) = retracted_text(&withdrawn.text);
                let notice = RetractedMessage {
                    message_id: message_id.clone(),
                    workspace_id: workspace_id.clone(),
                    from: withdrawn.from.clone(),
                    text,
                    cut,
                };
                for reader in readers {
                    let notices = state.retracted_notices.entry(reader).or_default();
                    notices.retain(|held| held.message_id != *message_id);
                    notices.insert(0, notice.clone());
                    notices.truncate(MAX_MESSAGES);
                }
            }
        }
        Event::RetractionDelivered {
            session_id,
            message_id,
        } => {
            validate_id(session_id, "session ID")?;
            validate_request_id(message_id)?;
            if let Some(notices) = state.retracted_notices.get_mut(session_id) {
                notices.retain(|notice| notice.message_id != *message_id);
                if notices.is_empty() {
                    state.retracted_notices.remove(session_id);
                }
            }
        }
        Event::ScanFailed { failure } => {
            validate_known_workspace(state, &failure.workspace_id)?;
            state
                .scan_failures
                .insert(failure.workspace_id.clone(), failure.clone());
        }
        Event::ScanRecovered { workspace_id } => {
            state.scan_failures.remove(workspace_id);
        }
        Event::ScanCoverageObserved {
            workspace_id,
            coverage,
        } => {
            validate_known_workspace(state, workspace_id)?;
            match coverage {
                Some(coverage) => {
                    validate_coverage(coverage)?;
                    state
                        .scan_coverage
                        .insert(workspace_id.clone(), coverage.clone());
                }
                None => {
                    state.scan_coverage.remove(workspace_id);
                }
            }
        }
        Event::HostNotInstalled { host } => {
            validate_host_name(&host.host)?;
            if !state.uninstalled_hosts.contains_key(&host.host)
                && state.uninstalled_hosts.len() >= MAX_HOSTS
            {
                return Err(error(
                    "invalid_hosts",
                    &format!("at most {MAX_HOSTS} hosts are held as not installed"),
                ));
            }
            state
                .uninstalled_hosts
                .insert(host.host.clone(), host.clone());
        }
        Event::HostCleared { host } => {
            state.uninstalled_hosts.remove(host);
        }
        Event::CheckoutChanged { change } => {
            validate_checkout(state, change)?;
            let changes = state
                .checkouts
                .entry(change.workspace_id.clone())
                .or_default();
            changes.insert(0, change.clone());
            changes.truncate(MAX_CHECKOUTS);
        }
        Event::CheckoutRunObserved { run } => {
            validate_checkout_run(state, run)?;
            let runs = state
                .checkout_runs
                .entry(run.workspace_id.clone())
                .or_default();
            runs.insert(0, run.clone());
            // Commits add runs and no change, so the two lists move apart.
            // An older run stays while it explains a change still held, so
            // that change keeps its attribution.
            let changes = state
                .checkouts
                .get(&run.workspace_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let explaining: BTreeSet<usize> = changes
                .iter()
                .filter_map(|change| runs.iter().position(|run| run.explains(change, changes)))
                .collect();
            let mut index = 0;
            runs.retain(|_| {
                index += 1;
                index <= MAX_CHECKOUT_RUNS || explaining.contains(&(index - 1))
            });
        }
        Event::UncoordinatedChanged { change } => {
            validate_uncoordinated(state, change)?;
            if !change.delivered.is_empty() {
                return Err(error(
                    "invalid_event",
                    "A new uncoordinated change has not been delivered to any session",
                ));
            }
            let changes = state
                .uncoordinated
                .entry(change.workspace_id.clone())
                .or_default();
            changes.insert(0, change.clone());
            changes.truncate(MAX_UNCOORDINATED);
        }
        Event::UncoordinatedDelivered {
            workspace_id,
            change_id,
            session_id,
        } => {
            if known_session(state, session_id)?.workspace_id != *workspace_id {
                return Err(error(
                    "invalid_event",
                    "An uncoordinated change is delivered to a session of its workspace",
                ));
            }
            // A change already truncated away has nothing left to mark.
            if let Some(change) = state
                .uncoordinated
                .get_mut(workspace_id)
                .and_then(|changes| changes.iter_mut().find(|c| c.change_id == *change_id))
                && !change.delivered.contains(session_id)
            {
                change.delivered.push(session_id.clone());
            }
        }
        Event::InterfaceChanged { notice } => {
            validate_notice(notice)?;
            validate_known_workspace(state, &notice.workspace_id)?;
            // An added export is history only (roadmap D4): a new name is
            // news to no dependent, so it is not held for briefings, recaps,
            // break alerts or the dashboard, and an addition alone does not
            // replace the file's held notice or open a workspace's entry.
            // A private definition moved with its file is history only too.
            follow_renames(state, notice);
            let mut held = notice.clone();
            held.changes
                .retain(|change| change.change != "added" && !change.private);
            if !held.changes.is_empty() {
                let notices = state
                    .interface_notices
                    .entry(notice.workspace_id.clone())
                    .or_default();
                // A newer notice for the same file replaces the older one.
                notices.retain(|older| older.path != held.path);
                notices.insert(0, held);
                notices.truncate(MAX_NOTICES);
            }
        }
        Event::InterfaceDependentsFound {
            notice_workspace_id,
            notice_id,
            files,
        } => {
            if files.len() > NOTICE_DEPENDENTS {
                return Err(error(
                    "invalid_event",
                    "At most 32 dependent files are found at once",
                ));
            }
            for file in files {
                validate_relative_path(&file.path)?;
            }
            // A notice replaced or dropped since then has nothing to add to.
            if let Some(notice) = state
                .interface_notices
                .get_mut(notice_workspace_id)
                .and_then(|notices| notices.iter_mut().find(|n| n.notice_id == *notice_id))
            {
                for file in files {
                    if notice.elsewhere.len() < NOTICE_DEPENDENTS
                        && !notice.elsewhere.contains(file)
                    {
                        notice.elsewhere.push(file.clone());
                    }
                }
            }
        }
        Event::CheckRecorded { check, tests } => {
            validate_check(state, check)?;
            if let Some(kept) = tests {
                let passed = match check.result {
                    CheckResult::Passed {} => true,
                    CheckResult::Failed { .. } => false,
                    CheckResult::Inconclusive {} => {
                        return Err(error(
                            "invalid_event",
                            "An inconclusive record has no test results",
                        ));
                    }
                };
                if !check.inputs.is_empty() || !flaky::is_command_id(&kept.command_id) {
                    return Err(error(
                        "invalid_event",
                        "Test results belong to a working-tree record and name their command's identity",
                    ));
                }
                flaky::validate_test_results(&kept.results, passed)?;
            }
            let records = state.checks.entry(check.workspace_id.clone()).or_default();
            // One record per name and tree: a rerun replaces the earlier result.
            records
                .retain(|record| !(record.name == check.name && record.tree_oid == check.tree_oid));
            records.insert(0, check.clone());
            records.truncate(MAX_CHECKS);
            // Kept only when no write was counted after the run took its
            // count; an earlier version also stored results whose count had
            // moved, and on replay they are no baseline either.
            let writes = state.writes.get(&check.workspace_id).copied().unwrap_or(0);
            let kept = tests.as_ref().filter(|tests| tests.writes_before == writes);
            classes::keep(state, &check.workspace_id, &check.check_id, kept);
        }
        Event::AutoCheckEnabled { check } => {
            validate_auto_check(state, check)?;
            state
                .auto_checks
                .insert(check.workspace_id.clone(), check.clone());
        }
        Event::AutoCheckDisabled { workspace_id } => {
            if state.auto_checks.remove(workspace_id).is_none() {
                return Err(error(
                    "invalid_event",
                    "No automatic check was enabled for this workspace",
                ));
            }
        }
        Event::CheckReported {
            session_ids,
            notice,
        } => {
            let Some(record) = state
                .checks
                .values()
                .flatten()
                .find(|record| record.check_id == notice.check_id)
            else {
                return Err(error(
                    "invalid_event",
                    "A check report follows the record it reports",
                ));
            };
            if record.name != notice.name
                || record.tree_oid != notice.tree_oid
                || record.result != notice.result
                || record.duration_ms != notice.duration_ms
            {
                return Err(error(
                    "invalid_event",
                    "A check report must match the record it reports",
                ));
            }
            let workspace_id = record.workspace_id.clone();
            validate_check_report(state, &workspace_id, event)?;
            for session_id in session_ids {
                let notices = state.check_notices.entry(session_id.clone()).or_default();
                // Only the newest result of a check is news.
                notices.retain(|held| held.name != notice.name);
                notices.insert(0, notice.clone());
                notices.truncate(MAX_CHECK_NOTICES);
            }
        }
        Event::CheckDelivered {
            session_id,
            check_id,
        } => {
            validate_id(session_id, "session ID")?;
            validate_request_id(check_id)?;
            if let Some(notices) = state.check_notices.get_mut(session_id) {
                notices.retain(|notice| notice.check_id != *check_id);
                if notices.is_empty() {
                    state.check_notices.remove(session_id);
                }
            }
        }
        Event::TestsRecorded {
            workspace_id,
            check_id,
            name,
            tree_oid,
            command,
            command_id,
            results,
            rerun,
            at_ms,
        } => {
            if !flaky::is_command_id(command_id) {
                return Err(error(
                    "invalid_event",
                    "Test results name their command's identity",
                ));
            }
            let Some(record) = state
                .checks
                .get(workspace_id)
                .and_then(|records| records.iter().find(|record| record.check_id == *check_id))
            else {
                return Err(error(
                    "invalid_event",
                    "Test results follow the record they belong to",
                ));
            };
            if record.name != *name
                || record.tree_oid != *tree_oid
                || record.command != *command
                || !record.inputs.is_empty()
            {
                return Err(error(
                    "invalid_event",
                    "Test results must match the working-tree record they belong to",
                ));
            }
            let passed = match (&record.result, rerun) {
                (_, true) => results.all_passed,
                (CheckResult::Passed {}, false) => true,
                (CheckResult::Failed { .. }, false) => false,
                (CheckResult::Inconclusive {}, _) => {
                    return Err(error(
                        "invalid_event",
                        "An inconclusive record has no test results",
                    ));
                }
            };
            flaky::validate_test_results(results, passed)?;
            // Recorded only when no write came during the run: the write
            // generation now is the one the run took.
            let writes = state.writes.get(workspace_id).copied().unwrap_or(0);
            state
                .test_learning
                .entry(workspace_id.clone())
                .or_default()
                .learn(name, tree_oid, command_id, writes, results, *at_ms);
        }
        Event::FlakyTestForgotten {
            workspace_id,
            name,
            test,
        } => {
            validate_forget_flaky(name.as_deref(), test)?;
            let Some(learning) = state.test_learning.get_mut(workspace_id) else {
                return Err(error(
                    "invalid_event",
                    "Nothing was learned about tests in this workspace",
                ));
            };
            learning.forget(name.as_deref(), test);
            if learning.is_empty() {
                state.test_learning.remove(workspace_id);
            }
        }
        Event::Landed { landing } => {
            validate_request_id(&landing.landing_id)?;
            validate_known_workspace(state, &landing.workspace_id)?;
            let landings = state
                .landings
                .entry(landing.workspace_id.clone())
                .or_default();
            landings.insert(0, landing.clone());
            landings.truncate(MAX_LANDINGS);
        }
        Event::TurnTaken { turn } => {
            validate_request_id(&turn.turn_id)?;
            validate_known_workspace(state, &turn.workspace_id)?;
            validate_branch_name(&turn.onto)?;
            let key = turn_key(&turn.workspace_id, &turn.onto);
            if turn.generation != state.generation
                || turn.expires_at_ms <= turn.taken_ms
                || state.turns.contains_key(&key)
            {
                return Err(error(
                    "invalid_event",
                    "A turn is taken in the current generation, with a later deadline, for a target nobody holds",
                ));
            }
            if let Some(queue) = state.turn_queues.get_mut(&key) {
                queue.retain(|waiter| waiter.ticket != turn.turn_id);
                if queue.is_empty() {
                    state.turn_queues.remove(&key);
                }
            }
            state.turns.insert(key, turn.clone());
        }
        Event::TurnQueueJoined { waiter } => {
            validate_request_id(&waiter.ticket)?;
            validate_known_workspace(state, &waiter.workspace_id)?;
            validate_branch_name(&waiter.onto)?;
            if let Some(session) = &waiter.session_id {
                validate_id(session, "session ID")?;
            }
            let queue = state
                .turn_queues
                .entry(turn_key(&waiter.workspace_id, &waiter.onto))
                .or_default();
            if waiter.generation != state.generation
                || waiter.deadline_ms <= waiter.joined_ms
                || waiter.seen_until_ms <= waiter.joined_ms
                || queue.len() >= MAX_TURN_QUEUE
                || queue.iter().any(|queued| queued.ticket == waiter.ticket)
            {
                return Err(error(
                    "invalid_event",
                    "A landing joins the line once, in the current generation, with later deadlines, while there is room",
                ));
            }
            queue.push(waiter.clone());
        }
        Event::TurnQueueSeen {
            workspace_id,
            onto,
            ticket,
            seen_until_ms,
        } => {
            let Some(waiter) = state
                .turn_queues
                .get_mut(&turn_key(workspace_id, onto))
                .and_then(|queue| queue.iter_mut().find(|waiter| waiter.ticket == *ticket))
                .filter(|waiter| waiter.seen_until_ms <= *seen_until_ms)
            else {
                return Err(error(
                    "invalid_event",
                    "Only a waiting landing is seen again, and never to an earlier time",
                ));
            };
            waiter.seen_until_ms = *seen_until_ms;
        }
        Event::TurnQueueLeft {
            workspace_id,
            onto,
            ticket,
            ..
        } => {
            let key = turn_key(workspace_id, onto);
            let Some(queue) = state
                .turn_queues
                .get_mut(&key)
                .filter(|queue| queue.iter().any(|waiter| waiter.ticket == *ticket))
            else {
                return Err(error(
                    "invalid_event",
                    "Only a waiting landing can leave the line",
                ));
            };
            queue.retain(|waiter| waiter.ticket != *ticket);
            if queue.is_empty() {
                state.turn_queues.remove(&key);
            }
        }
        Event::TurnReleased {
            workspace_id,
            onto,
            turn_id,
            attempts,
        } => {
            validate_attempts(attempts)?;
            let key = turn_key(workspace_id, onto);
            if state
                .turns
                .get(&key)
                .is_some_and(|held| held.turn_id == *turn_id)
            {
                state.turns.remove(&key);
            }
        }
        Event::TurnRenewed {
            workspace_id,
            onto,
            turn_id,
            expires_at_ms,
        } => {
            let Some(held) = state
                .turns
                .get_mut(&turn_key(workspace_id, onto))
                .filter(|held| held.turn_id == *turn_id && held.expires_at_ms <= *expires_at_ms)
            else {
                return Err(error(
                    "invalid_event",
                    "Only a held turn can be renewed, and never to an earlier deadline",
                ));
            };
            held.expires_at_ms = *expires_at_ms;
        }
        Event::TurnExpired {
            workspace_id,
            onto,
            turn_id,
        } => {
            let key = turn_key(workspace_id, onto);
            if !state
                .turns
                .get(&key)
                .is_some_and(|held| held.turn_id == *turn_id)
            {
                return Err(error(
                    "invalid_event",
                    "Only a turn that is held can expire",
                ));
            }
            state.turns.remove(&key);
        }
        Event::RemoteForgotten {
            workspace_id,
            participant,
        } => {
            state
                .remote_peers
                .remove(&remote_key(workspace_id, participant));
        }
        Event::InterfaceAlerted {
            session_id,
            notice_ids,
        } => {
            current_session(state, session_id)?;
            validate_alerted(notice_ids)?;
            let alerted = &mut activity_mut(state, session_id).alerted;
            alerted.retain(|held| !notice_ids.contains(held));
            for id in notice_ids.iter().rev() {
                alerted.insert(0, id.clone());
            }
            alerted.truncate(MAX_ALERTED);
        }
        Event::InterfaceNoticesTold {
            session_id,
            notice_ids,
            via,
        } => {
            current_session(state, session_id)?;
            validate_told(notice_ids)
                .map_err(|failure| error("invalid_event", &failure.message))?;
            let told = &mut activity_mut(state, session_id).notices_told;
            told.retain(|held| !notice_ids.contains(&held.notice_id));
            for id in notice_ids.iter().rev() {
                told.insert(
                    0,
                    ToldNotice {
                        notice_id: id.clone(),
                        via: *via,
                    },
                );
            }
            told.truncate(MAX_TOLD_NOTICES);
        }
        Event::BriefingDelivered {
            session_id,
            signature,
            told,
            told_at_ms,
        } => {
            current_session(state, session_id)?;
            if signature.len() > 64 || told.len() > MAX_TOLD {
                return Err(error(
                    "invalid_event",
                    "A briefing signature is at most 64 bytes, and a delivery names at most 256 item keys",
                ));
            }
            let activity = activity_mut(state, session_id);
            activity.briefed = Some(signature.clone());
            activity.told = told.clone();
            if let Some(at_ms) = told_at_ms {
                activity.told_at_ms = *at_ms;
            }
        }
        Event::ContextAdded {
            session_id,
            bytes,
            turn,
            generation,
        } => {
            current_session(state, session_id)?;
            if *generation != state.generation || *bytes > MAX_CONTEXT_RECORD_BYTES {
                return Err(error(
                    "invalid_event",
                    "A context record belongs to the current daemon generation and names at most 1 MiB",
                ));
            }
            activity_mut(state, session_id).context.add(*bytes, *turn);
        }
        Event::ToldForgotten { session_id, keys } => {
            current_session(state, session_id)?;
            if keys.len() > MAX_TOLD {
                return Err(error(
                    "invalid_event",
                    "A forget names at most 256 briefing item keys",
                ));
            }
            activity_mut(state, session_id)
                .told
                .retain(|key| !keys.contains(key));
        }
        Event::InflightDelivered { session_id, keys } => {
            current_session(state, session_id)?;
            if keys.is_empty()
                || keys.len() > INFLIGHT_LINES
                || keys
                    .iter()
                    .any(|key| key.is_empty() || key.len() > 64 || !key.is_ascii())
            {
                return Err(error(
                    "invalid_event",
                    "A delivery names 1 to 3 in-flight keys of at most 64 ASCII bytes",
                ));
            }
            let told = &mut activity_mut(state, session_id).inflight_told;
            told.retain(|held| !keys.contains(held));
            for key in keys.iter().rev() {
                told.insert(0, key.clone());
            }
            told.truncate(MAX_INFLIGHT_TOLD);
        }
        Event::DiskObserved { observation } => {
            validate_disk_observation(state, observation)?;
            // A first baseline is not a change. Anything else means the
            // working tree a check ran on is no longer the working tree.
            let changed = observation
                .before
                .as_ref()
                .is_some_and(|before| before != &observation.after);
            if changed {
                wrote_in(state, &observation.workspace_id, None);
            }
            if changed && let Some(records) = state.checks.get_mut(&observation.workspace_id) {
                for record in records.iter_mut().filter(|record| record.inputs.is_empty()) {
                    record.changed_since = true;
                }
            }
        }
        Event::BranchObserved { observation } => {
            validate_branch_observation(state, observation)?;
        }
        Event::LeaseCompleted { lease_id } | Event::LeaseExpired { lease_id } => {
            validate_id(lease_id, "lease ID")?;
            let lease = state
                .leases
                .get_mut(lease_id)
                .ok_or_else(|| error("invalid_event", "Lease event references an unknown lease"))?;
            if lease.status != LeaseStatus::Active || lease.generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "Only an active lease in the current generation can end",
                ));
            }
            lease.status = if matches!(event, Event::LeaseCompleted { .. }) {
                LeaseStatus::Completed
            } else {
                LeaseStatus::Expired
            };
        }
    }
    Ok(())
}

/// Return stable, lease-ID ordered expiration events; do not change state.
pub fn expiry_events(state: &State, now_ms: u64) -> Vec<Event> {
    state
        .leases
        .values()
        .filter(|lease| {
            lease.status == LeaseStatus::Active
                && lease.generation == state.generation
                && lease.expires_at_ms <= now_ms
        })
        .map(|lease| Event::LeaseExpired {
            lease_id: lease.lease_id.clone(),
        })
        .chain(
            state
                .turns
                .values()
                .filter(|turn| turn.expires_at_ms <= now_ms)
                .map(|turn| Event::TurnExpired {
                    workspace_id: turn.workspace_id.clone(),
                    onto: turn.onto.clone(),
                    turn_id: turn.turn_id.clone(),
                }),
        )
        .chain(
            state
                .turn_queues
                .values()
                .flatten()
                .filter(|waiter| !still_waiting(waiter, state.generation, now_ms))
                .map(|waiter| Event::TurnQueueLeft {
                    workspace_id: waiter.workspace_id.clone(),
                    onto: waiter.onto.clone(),
                    ticket: waiter.ticket.clone(),
                    reason: if waiter.deadline_ms <= now_ms {
                        LeftQueue::TimedOut
                    } else {
                        LeftQueue::Stopped
                    },
                }),
        )
        .collect()
}

/// The identifier grammar the index is asked about: an ASCII letter or
/// underscore, then 2 to 63 ASCII letters, digits or underscores.
pub fn is_graph_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (3..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

/// Bounds of a code-context query: 1 to `CONTEXT_NAMES` distinct names in
/// the identifier grammar, and 1 to `CONTEXT_DEFINITIONS` definitions each.
pub fn validate_graph_context(names: &[String], limit: u32) -> Result<(), CoreError> {
    let distinct: BTreeSet<&String> = names.iter().collect();
    if names.is_empty()
        || names.len() > CONTEXT_NAMES
        || distinct.len() != names.len()
        || !names.iter().all(|name| is_graph_name(name))
        || !(1..=CONTEXT_DEFINITIONS).contains(&limit)
    {
        return Err(error(
            "invalid_graph_context",
            "Ask about 1 to 12 distinct names, each 3 to 64 ASCII letters, digits or underscores not starting with a digit, with a limit of 1 to 3 definitions per name",
        ));
    }
    Ok(())
}

fn validate_alerted(notice_ids: &[String]) -> Result<(), CoreError> {
    if notice_ids.is_empty() || notice_ids.len() > MAX_CALL_BREAKS {
        return Err(error(
            "invalid_alert",
            "An interface alert names 1 to 8 notices",
        ));
    }
    notice_ids.iter().try_for_each(|id| validate_request_id(id))
}

/// The notices one briefing or one between-step delivery told: 1 to
/// `BRIEF_INTERFACE_NOTICES` valid IDs (issue #214).
fn validate_told(notice_ids: &[String]) -> Result<(), CoreError> {
    if notice_ids.is_empty() || notice_ids.len() > BRIEF_INTERFACE_NOTICES {
        return Err(error(
            "invalid_told",
            "A delivery tells 1 to 4 interface notices",
        ));
    }
    notice_ids.iter().try_for_each(|id| validate_request_id(id))
}

fn validate_arity(arity: &Arity) -> Result<(), CoreError> {
    if arity.min > MAX_ARITY
        || arity
            .max
            .is_some_and(|max| max < arity.min || max > MAX_ARITY)
    {
        return Err(error(
            "invalid_notice",
            "A parameter count is at most 255, and its minimum is at most its maximum",
        ));
    }
    Ok(())
}

/// The calls of one write check: 1 to `MAX_CALL_SITES`, each in a relative
/// path, on a line from 1 to 2,000,000, naming 1 to 128 bytes without
/// control characters, with at most `MAX_ARITY` arguments.
pub fn validate_calls(calls: &[CallSite]) -> Result<(), CoreError> {
    let invalid = || {
        error(
            "invalid_calls",
            "A write check names 1 to 64 calls, each in a workspace-relative path, on a line from 1 to 2000000, a name of 1 to 128 bytes without control characters, and at most 255 arguments",
        )
    };
    if calls.is_empty() || calls.len() > MAX_CALL_SITES {
        return Err(invalid());
    }
    for call in calls {
        validate_relative_path(&call.path).map_err(|_| invalid())?;
        if !(1..=2_000_000).contains(&call.line)
            || call.name.is_empty()
            || call.name.len() > 128
            || call.name.chars().any(char::is_control)
            || call.args > MAX_ARITY
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Where a notice of another worktree stands against the asking session's
/// checkout, as Git answers it (issue #217).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The notice's file differs from its worktree's `HEAD`: not committed.
    Uncommitted,
    /// Committed on its worktree's branch, in a commit the asking
    /// checkout's `HEAD` does not contain.
    Unmerged,
    /// The asking checkout's `HEAD` contains the commit.
    Merged,
}

/// Standings by `(workspace ID, notice ID)`. A notice missing from it, such
/// as one Git could not answer for in time, keeps the uncommitted rules.
pub type Standings = BTreeMap<(String, String), Standing>;

/// Notices of other worktrees whose standing one write check asks Git for,
/// at most.
pub const MAX_STANDING_QUESTIONS: usize = 8;

/// A notice of another worktree whose standing a write check asks Git for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingQuestion {
    pub workspace_id: String,
    pub notice_id: String,
    /// The notice's file, relative to that worktree's root.
    pub path: String,
}

/// The notices of other worktrees that `calls` could be alerted to if they
/// were committed and not merged: those the uncommitted rules would also
/// keep, and those they leave out only because the change is older than the
/// briefing horizon or its session has ended. At most
/// `MAX_STANDING_QUESTIONS`, each workspace's newest first. Usually none, and
/// then Git is not asked.
pub fn standing_questions(
    state: &State,
    session_id: &str,
    calls: &[CallSite],
    now_ms: u64,
) -> Result<Vec<StandingQuestion>, CoreError> {
    validate_calls(calls)?;
    let own = &current_session(state, session_id)?.workspace_id;
    let mut questions: Vec<StandingQuestion> = Vec::new();
    for (notice, change, _) in alertable(state, session_id, now_ms, None)? {
        let asked = questions.iter().any(|question| {
            question.workspace_id == notice.workspace_id && question.notice_id == notice.notice_id
        });
        if notice.workspace_id == *own
            || asked
            || !calls.iter().any(|call| call.name == change.name)
        {
            continue;
        }
        if questions.len() == MAX_STANDING_QUESTIONS {
            break;
        }
        questions.push(StandingQuestion {
            workspace_id: notice.workspace_id.clone(),
            notice_id: notice.notice_id.clone(),
            path: notice.path.clone(),
        });
    }
    Ok(questions)
}

/// Changes to functions that another session made in this session's
/// workspace or in another worktree of its repository (issue #194), in this
/// generation, that this session has not been alerted to: `(notice, change,
/// the peer)`. A signature change counts only with the new parameter count;
/// a rename and a removal always count. The session's own workspace first,
/// each workspace's newest notice first.
///
/// An uncommitted change counts within the briefing horizon while its
/// session is live. Another worktree's change that `standings` says is
/// committed and not merged into this session's checkout counts whatever
/// its age and whether or not its session has ended, and one it says is
/// merged never counts (issue #217). With `standings` `None` (asking which
/// notices Git should answer for), every other worktree's change counts
/// as if committed and not merged.
fn alertable<'a>(
    state: &'a State,
    session_id: &str,
    now_ms: u64,
    standings: Option<&Standings>,
) -> Result<Vec<(&'a InterfaceNotice, &'a InterfaceChange, &'a str)>, CoreError> {
    let me = current_session(state, session_id)?;
    let alerted = state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .map(|activity| activity.alerted.as_slice())
        .unwrap_or(&[]);
    let live = |peer: &str| {
        state
            .sessions
            .get(peer)
            .is_some_and(|session| session.generation == state.generation)
            && state
                .session_liveness
                .get(peer)
                .is_none_or(|liveness| liveness.status != LivenessStatus::Exited)
            && state
                .activity
                .get(peer)
                .is_none_or(|activity| !activity.ended)
    };
    let mut found = Vec::new();
    let own = state.interface_notices.get(&me.workspace_id);
    let others = state
        .interface_notices
        .iter()
        .filter(|(workspace_id, _)| {
            **workspace_id != me.workspace_id
                && related_workspace(state, &me.workspace_id, workspace_id)
        })
        .map(|(_, notices)| notices);
    for notice in own.into_iter().chain(others).flatten() {
        let Some(peer) = notice.by_session.as_deref() else {
            continue;
        };
        let standing = if notice.workspace_id == me.workspace_id {
            Some(Standing::Uncommitted)
        } else {
            match standings {
                None => Some(Standing::Unmerged),
                Some(standings) => standings
                    .get(&(notice.workspace_id.clone(), notice.notice_id.clone()))
                    .copied(),
            }
        };
        let committed = match standing {
            Some(Standing::Merged) => continue,
            Some(Standing::Unmerged) => true,
            Some(Standing::Uncommitted) | None => false,
        };
        if notice.generation != state.generation
            || peer == session_id
            || alerted.contains(&notice.notice_id)
            || (!committed
                && (now_ms.saturating_sub(notice.at_ms) > BRIEF_HORIZON_MS || !live(peer)))
        {
            continue;
        }
        for change in &notice.changes {
            let alerts = match change.change.as_str() {
                "signature_changed" => change.after_arity.is_some(),
                "renamed" => change.after_name.is_some(),
                "removed" => true,
                _ => false,
            };
            if alerts && matches!(change.kind.as_str(), "function" | "method") {
                found.push((notice, change, peer));
            }
        }
    }
    Ok(found)
}

/// The names among `calls` that a write check must look up in the index,
/// by the workspace whose index holds the change: those of a change it
/// could alert to, and a renamed function's new name. `standings` holds
/// Git's answers for `standing_questions`. Usually none, and then nothing
/// is looked up.
pub fn call_candidates(
    state: &State,
    session_id: &str,
    calls: &[CallSite],
    standings: &Standings,
    now_ms: u64,
) -> Result<BTreeMap<String, BTreeSet<String>>, CoreError> {
    validate_calls(calls)?;
    let mut candidates: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (notice, change, _) in alertable(state, session_id, now_ms, Some(standings))? {
        if calls.iter().any(|call| call.name == change.name) {
            candidates
                .entry(notice.workspace_id.clone())
                .or_default()
                .extend(
                    [&change.name]
                        .into_iter()
                        .chain(&change.after_name)
                        .cloned(),
                );
        }
    }
    Ok(candidates)
}

/// What one workspace's index holds of a write check's candidate names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexedNames {
    /// Every definition of a candidate name.
    pub definitions: Vec<SymbolRef>,
    /// The index reads every supported file of the workspace at its current
    /// content (`GraphCoverage::blind_in_scope` is zero).
    pub complete: bool,
}

/// The calls a peer's recent change makes wrong, at most `MAX_CALL_BREAKS`,
/// from notices this session was not alerted to. `current` holds, by
/// workspace, the index's definitions of every candidate name
/// (`call_candidates`), and a change counts only while it is still true in
/// the index of the workspace it was made in, so a notice the index has
/// moved past is never used, and a notice of a workspace missing from
/// `current` is not used at all:
///
/// - a signature change, while its file holds the new signature; a call
///   whose count does not fit it breaks;
/// - a rename, while its file holds the new name's signature and no
///   definition of the old name is left anywhere; every call of the old
///   name breaks;
/// - a removal, while no definition of the name is left anywhere; every
///   call of it breaks.
///
/// "No definition left anywhere" is read from that workspace's index, so a
/// rename or a removal counts only when it is `complete`. A name moved into
/// a file not indexed yet, partial or too large is otherwise missing from
/// `current` without being gone. A change made in another worktree is
/// judged in that worktree: the session's own checkout still defines the
/// old name until the branches meet, which is the break being told.
///
/// A same-count change is not alerted: calls are matched by name, and
/// without the argument types at a call site nothing tells a right call
/// from a wrong one (ADR 0029). `standings` is as for `call_candidates`.
/// Pure; the caller records the alert.
pub fn call_breaks(
    state: &State,
    session_id: &str,
    calls: &[CallSite],
    standings: &Standings,
    current: &BTreeMap<String, IndexedNames>,
    now_ms: u64,
) -> Result<Vec<CallBreak>, CoreError> {
    validate_calls(calls)?;
    let own_workspace = &current_session(state, session_id)?.workspace_id;
    let mut breaks = Vec::new();
    for (notice, change, peer) in alertable(state, session_id, now_ms, Some(standings))? {
        let Some(indexed) = current.get(&notice.workspace_id) else {
            continue;
        };
        let gone = |name: &str| {
            indexed.complete && !indexed.definitions.iter().any(|symbol| symbol.name == name)
        };
        let holds = |path: &str, name: &str, signature: &str| {
            indexed.definitions.iter().any(|symbol| {
                symbol.path == path && symbol.name == name && symbol.signature == signature
            })
        };
        let worktree = (notice.workspace_id != *own_workspace)
            .then(|| state.workspaces.get(&notice.workspace_id))
            .flatten()
            .map(|workspace| workspace.root.clone());
        let after = change.after.as_deref().unwrap_or("");
        // `Some`: a call breaks when its count does not fit; `None`: every
        // call of the name breaks.
        let fit: Option<Arity> = match change.change.as_str() {
            "signature_changed" => match change.after_arity {
                Some(arity) if holds(&notice.path, &change.name, after) => Some(arity),
                _ => continue,
            },
            "renamed" => match &change.after_name {
                Some(to) if holds(&notice.path, to, after) && gone(&change.name) => None,
                _ => continue,
            },
            "removed" if gone(&change.name) => None,
            _ => continue,
        };
        for call in calls.iter().filter(|call| {
            call.name == change.name && fit.is_none_or(|arity| !arity.fits(call.args, call.member))
        }) {
            if breaks.len() == MAX_CALL_BREAKS {
                return Ok(breaks);
            }
            breaks.push(CallBreak {
                notice_id: notice.notice_id.clone(),
                call: call.clone(),
                change: change.change.clone(),
                defined_in: notice.path.clone(),
                worktree: worktree.clone(),
                by_session: peer.to_owned(),
                before: change.before.clone().unwrap_or_default(),
                after: after.to_owned(),
                after_name: change.after_name.clone(),
                before_arity: change.before_arity,
                after_arity: change.after_arity,
                age_ms: now_ms.saturating_sub(notice.at_ms),
            });
        }
    }
    Ok(breaks)
}

pub fn validate_request_id(id: &str) -> Result<(), CoreError> {
    validate_id(id, "request ID").map_err(|mut error| {
        error.code = "invalid_request_id".into();
        error
    })
}

/// The most operations one tool-batch record names.
pub const MAX_BATCH_OPERATIONS: usize = 64;

/// A host's error type, such as Claude Code's `rate_limit`: 1 to 40
/// lower-case ASCII letters and underscores. Never free text.
fn validate_failure(failure: &str) -> Result<(), CoreError> {
    if failure.is_empty()
        || failure.len() > 40
        || !failure
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return Err(error(
            "invalid_failure",
            "A stop failure is the host's error type: 1 to 40 lower-case letters or underscores",
        ));
    }
    Ok(())
}

fn validate_batch(operation_ids: &[String], calls: u32) -> Result<(), CoreError> {
    let distinct: BTreeSet<&String> = operation_ids.iter().collect();
    if operation_ids.is_empty()
        || operation_ids.len() > MAX_BATCH_OPERATIONS
        || distinct.len() != operation_ids.len()
        || (calls as usize) < operation_ids.len()
        || operation_ids
            .iter()
            .any(|id| validate_id(id, "operation ID").is_err())
    {
        return Err(error(
            "invalid_batch",
            "A tool batch names 1 to 64 distinct operation IDs and at least as many calls",
        ));
    }
    Ok(())
}

fn validate_id(id: &str, label: &str) -> Result<(), CoreError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(error(
            "invalid_id",
            &format!(
                "{label} must contain 1 to 128 ASCII letters, digits, dots, underscores or hyphens"
            ),
        ));
    }
    Ok(())
}

fn validate_tool_identity(operation: &ToolIdentity) -> Result<(), CoreError> {
    validate_id(&operation.operation_id, "operation ID")?;
    validate_id(&operation.source, "observation source")?;
    validate_id(&operation.tool, "tool name")?;
    if !is_hex_digest(&operation.input_digest, 64) {
        return Err(error(
            "invalid_input_digest",
            "Tool input digest must contain exactly 64 hexadecimal characters",
        ));
    }
    Ok(())
}

fn is_hex_digest(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_relative_path(path: &str) -> Result<(), CoreError> {
    if path.is_empty()
        || path.len() > 512
        || path.contains(['\\', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(error(
            "invalid_path",
            "Each file key must be a relative path of at most 512 bytes without empty, dot or parent components, backslashes or NUL",
        ));
    }
    Ok(())
}

fn normalized_paths(paths: &[String]) -> Result<Vec<String>, CoreError> {
    if paths.is_empty() {
        return Err(error(
            "invalid_path",
            "Specify at least one normalized relative file path",
        ));
    }
    let mut unique = BTreeSet::new();
    for path in paths {
        validate_relative_path(path)?;
        unique.insert(path.clone());
        if unique.len() > 64 {
            return Err(error(
                "too_many_paths",
                "A command can name at most 64 unique file paths; split independent work into separate operations",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

/// Prompt target paths, sorted and without repeats: 1 to
/// `MAX_PROMPT_TARGETS` normalized relative paths.
fn prompt_target_paths(paths: &[String]) -> Result<Vec<String>, CoreError> {
    let paths = normalized_paths(paths)?;
    if paths.len() > MAX_PROMPT_TARGETS {
        return Err(error(
            "too_many_paths",
            &format!("A prompt names at most {MAX_PROMPT_TARGETS} target paths"),
        ));
    }
    Ok(paths)
}

/// Presence paths, sorted and without repeats: 1 to `PRESENCE_PATHS`
/// normalized relative paths, for a search or a shell command.
fn presence_paths(kind: PresenceKind, paths: &[String]) -> Result<Vec<String>, CoreError> {
    if kind == PresenceKind::Read {
        return Err(error(
            "invalid_presence",
            "A read is recorded as an observed source version, not as presence",
        ));
    }
    let paths = normalized_paths(paths)?;
    if paths.len() > PRESENCE_PATHS {
        return Err(error(
            "too_many_paths",
            &format!("A presence record names at most {PRESENCE_PATHS} paths"),
        ));
    }
    Ok(paths)
}

impl Default for PresenceSetting {
    fn default() -> Self {
        Self {
            shown: true,
            window_ms: PRESENCE_WINDOW_MS,
        }
    }
}

/// A session's presence setting: its own, or the default.
fn presence_setting(state: &State, session_id: &str) -> PresenceSetting {
    state.presence.get(session_id).copied().unwrap_or_default()
}

fn validate_presence_setting(setting: &PresenceSetting) -> Result<(), CoreError> {
    if !(MIN_PRESENCE_WINDOW_MS..=BRIEF_HORIZON_MS).contains(&setting.window_ms) {
        return Err(error(
            "invalid_limit",
            "A presence window is between 1 and 30 minutes",
        ));
    }
    Ok(())
}

/// Record that a session looked at `paths`: newest first, one entry per path.
fn look(activity: &mut SessionActivity, kind: PresenceKind, paths: &[String], at_ms: u64) {
    for path in paths.iter().rev() {
        activity.looking.retain(|looked| looked.path != *path);
        activity.looking.insert(
            0,
            LookedPath {
                path: path.clone(),
                kind,
                at_ms,
            },
        );
    }
    activity.looking.truncate(MAX_LOOKING);
}

fn normalized_related_ids(ids: &[String]) -> Result<Vec<String>, CoreError> {
    let mut unique = BTreeSet::new();
    for id in ids {
        validate_id(id, "related operation ID")?;
        unique.insert(id.clone());
        if unique.len() > 64 {
            return Err(error(
                "too_many_related_operations",
                "A disk observation may correlate at most 64 distinct operations",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

fn validate_fact_header(
    state: &State,
    observation_id: &str,
    workspace_id: &str,
    generation: u64,
) -> Result<(), CoreError> {
    validate_request_id(observation_id)?;
    validate_id(workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Observation workspace is not registered; register it before recording facts",
        ));
    }
    if generation != state.generation {
        return Err(error(
            "invalid_event",
            "Observation must belong to the current daemon generation",
        ));
    }
    Ok(())
}

fn validate_file_state(state: &FileState) -> Result<(), CoreError> {
    if let FileState::Present { digest, .. } = state
        && !is_hex_digest(digest, 64)
    {
        return Err(error(
            "invalid_content_digest",
            "Observed file content digest must contain exactly 64 hexadecimal characters",
        ));
    }
    Ok(())
}

/// Validate a prepared workspace-relative file key and its observed version.
/// Full snapshot projections use the same rules even for clean or unchanged
/// files, so an accepted baseline cannot become impossible to update later.
pub fn validate_observed_file(path: &str, version: &FileState) -> Result<(), CoreError> {
    validate_relative_path(path)?;
    validate_file_state(version)
}

fn validate_disk_observation(
    state: &State,
    observation: &DiskObservation,
) -> Result<(), CoreError> {
    validate_fact_header(
        state,
        &observation.observation_id,
        &observation.workspace_id,
        observation.generation,
    )?;
    validate_observed_file(&observation.path, &observation.after)?;
    if let Some(before) = &observation.before {
        validate_file_state(before)?;
    }
    if normalized_related_ids(&observation.related_operation_ids)?
        != observation.related_operation_ids
    {
        return Err(error(
            "invalid_event",
            "Related operation IDs in an event must already be sorted and unique",
        ));
    }
    Ok(())
}

fn validate_branch_observation(
    state: &State,
    observation: &BranchObservation,
) -> Result<(), CoreError> {
    validate_fact_header(
        state,
        &observation.observation_id,
        &observation.workspace_id,
        observation.generation,
    )?;
    let reference = &observation.branch_ref;
    if reference.len() > 512
        || reference.chars().any(char::is_control)
        || (reference != "HEAD"
            && !reference
                .strip_prefix("refs/heads/")
                .is_some_and(|name| !name.is_empty()))
    {
        return Err(error(
            "invalid_branch_ref",
            "Branch reference must be HEAD or a nonempty refs/heads/ name, at most 512 bytes without control characters",
        ));
    }
    if observation.head_oid.is_some() != observation.tree_oid.is_some() {
        return Err(error(
            "invalid_branch_oids",
            "Supply head and tree together; omit both only for an absent or unborn reference",
        ));
    }
    for oid in [&observation.head_oid, &observation.tree_oid]
        .into_iter()
        .flatten()
    {
        if !is_hex_digest(oid, 40) && !is_hex_digest(oid, 64) {
            return Err(error(
                "invalid_git_oid",
                "Observed Git object IDs must contain 40 or 64 hexadecimal characters",
            ));
        }
    }
    Ok(())
}

/// A host's command-line name: lower-case letters and hyphens, as the
/// adapters name them (`claude-code`, `antigravity`).
fn validate_host_name(host: &str) -> Result<(), CoreError> {
    if host.is_empty()
        || host.len() > 32
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
    {
        return Err(error(
            "invalid_hosts",
            "a host name is 1 to 32 lower-case letters and hyphens",
        ));
    }
    Ok(())
}

fn validate_coverage(coverage: &ScanCoverage) -> Result<(), CoreError> {
    for (paths, count) in [
        (&coverage.too_large_paths, coverage.too_large),
        (&coverage.submodule_paths, coverage.submodules),
    ] {
        if paths.len() > COVERAGE_PATHS || paths.len() as u64 > count {
            return Err(error(
                "invalid_coverage",
                "A coverage record names at most 8 paths per reason, and no more than it counts",
            ));
        }
        for path in paths {
            validate_relative_path(path)?;
        }
    }
    Ok(())
}

fn validate_checkout(state: &State, change: &CheckoutChange) -> Result<(), CoreError> {
    validate_fact_header(
        state,
        &change.change_id,
        &change.workspace_id,
        change.generation,
    )?;
    validate_checkout_state(
        [&change.from_branch, &change.to_branch],
        [&change.from_head, &change.to_head],
    )?;
    if change.from_branch == change.to_branch
        && change.from_head == change.to_head
        && change.files == 0
    {
        return Err(error(
            "invalid_checkout",
            "A checkout change needs a different branch or HEAD, or changed files",
        ));
    }
    Ok(())
}

fn validate_checkout_run(state: &State, run: &CheckoutRun) -> Result<(), CoreError> {
    validate_fact_header(state, &run.run_id, &run.workspace_id, run.generation)?;
    if state
        .sessions
        .get(&run.session_id)
        .is_none_or(|session| session.workspace_id != run.workspace_id)
    {
        return Err(error(
            "unknown_session",
            "A checkout run names a session registered in its workspace",
        ));
    }
    validate_checkout_state([&run.to_branch], [&run.to_head])?;
    if run.command.as_ref().is_some_and(|command| {
        command.len() > MAX_CHECKOUT_COMMAND_BYTES || command.chars().any(char::is_control)
    }) {
        return Err(error(
            "invalid_command",
            "A checkout run's command is at most 256 bytes without control characters",
        ));
    }
    if run.started_ms > run.finished_ms {
        return Err(error(
            "invalid_event",
            "A checkout run cannot start after it finished",
        ));
    }
    Ok(())
}

/// Branches are nonempty `refs/heads/` names; heads are Git object IDs.
fn validate_checkout_state<const B: usize, const H: usize>(
    branches: [&Option<String>; B],
    heads: [&Option<String>; H],
) -> Result<(), CoreError> {
    for reference in branches.into_iter().flatten() {
        if reference.len() > 512
            || reference.chars().any(char::is_control)
            || !reference
                .strip_prefix("refs/heads/")
                .is_some_and(|name| !name.is_empty())
        {
            return Err(error(
                "invalid_branch_ref",
                "A checkout branch is a nonempty refs/heads/ name, at most 512 bytes without control characters",
            ));
        }
    }
    for oid in heads.into_iter().flatten() {
        validate_oid(oid)?;
    }
    Ok(())
}

fn validate_canonical_paths(paths: &[String]) -> Result<(), CoreError> {
    if normalized_paths(paths)? != paths {
        return Err(error(
            "invalid_event",
            "Event file keys must already be sorted and unique",
        ));
    }
    Ok(())
}

fn validate_summary(summary: &str) -> Result<(), CoreError> {
    if summary.len() > MAX_INTENT_SUMMARY_BYTES {
        return Err(error(
            "invalid_summary",
            &format!(
                "Intent summary exceeds {MAX_INTENT_SUMMARY_BYTES} UTF-8 bytes; shorten the summary"
            ),
        ));
    }
    Ok(())
}

fn validate_workspace(state: &State, workspace: &Workspace) -> Result<(), CoreError> {
    validate_id(&workspace.workspace_id, "workspace ID")?;
    if workspace.root.is_empty() || workspace.root.contains('\0') {
        return Err(error(
            "invalid_root",
            "Workspace root must be a nonempty canonical path without NUL",
        ));
    }
    if normalized_workspace_links(&workspace.verified_overlaps)? != workspace.verified_overlaps {
        return Err(error(
            "invalid_event",
            "Verified workspace overlaps must already be sorted and unique",
        ));
    }
    validate_workspace_link_targets(
        state,
        &workspace.workspace_id,
        &workspace.root,
        &workspace.verified_overlaps,
    )?;
    if let Some(existing) = state.workspaces.get(&workspace.workspace_id)
        && (existing.root != workspace.root
            || existing.verified_overlaps != workspace.verified_overlaps)
    {
        return Err(error(
            "workspace_conflict",
            "Workspace ID is already bound to a different canonical root or verification record; use its existing registration",
        ));
    }
    for existing in state.workspaces.values() {
        if existing.workspace_id != workspace.workspace_id
            && roots_overlap(&existing.root, &workspace.root)
        {
            let verified = workspace
                .verified_overlaps
                .binary_search(&existing.workspace_id)
                .is_ok()
                || existing
                    .verified_overlaps
                    .binary_search(&workspace.workspace_id)
                    .is_ok();
            if existing.root == workspace.root || !verified {
                return Err(error(
                    "workspace_conflict",
                    "This root overlaps another workspace without a verified distinct worktree boundary; reuse its registered ID",
                ));
            }
        }
    }
    if !state.workspaces.contains_key(&workspace.workspace_id) {
        validate_no_parent_claims(state, workspace)?;
    }
    Ok(())
}

fn validate_no_parent_claims(state: &State, workspace: &Workspace) -> Result<(), CoreError> {
    for lease in state.leases.values() {
        if lease.status != LeaseStatus::Active || lease.generation != state.generation {
            continue;
        }
        let Some(parent) = state.workspaces.get(&lease.workspace_id) else {
            continue;
        };
        // A verified overlap whose roots do not overlap as text, such as a
        // registered root that is now a link: the core cannot tell which of
        // the lease's paths lie under this root, so any current admission
        // there makes the registration wait.
        if !roots_overlap(&parent.root, &workspace.root)
            && workspace
                .verified_overlaps
                .binary_search(&parent.workspace_id)
                .is_ok()
        {
            return Err(error(
                "workspace_in_use",
                "A current admission in a workspace this root overlaps through a link may cover it; complete it or wait for expiry, then start a new registration attempt with a new request ID",
            ));
        }
        let prefix = if parent.root.ends_with('/') {
            parent.root.clone()
        } else {
            format!("{}/", parent.root)
        };
        let Some(child_key) = workspace.root.strip_prefix(&prefix) else {
            continue;
        };
        let child_key = child_key.trim_end_matches('/');
        if !child_key.is_empty()
            && lease.paths.iter().any(|path| {
                path == child_key
                    || path
                        .strip_prefix(child_key)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
        {
            return Err(error(
                "workspace_in_use",
                "A current parent-workspace admission covers this subtree; complete it or wait for expiry, then start a new registration attempt with a new request ID",
            ));
        }
    }
    Ok(())
}

fn normalized_workspace_links(ids: &[String]) -> Result<Vec<String>, CoreError> {
    let mut unique = BTreeSet::new();
    for id in ids {
        validate_id(id, "verified workspace ID")?;
        unique.insert(id.clone());
        if unique.len() > 64 {
            return Err(error(
                "too_many_verified_overlaps",
                "A workspace registration can verify at most 64 distinct overlapping workspaces",
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

fn validate_workspace_link_targets(
    state: &State,
    workspace_id: &str,
    root: &str,
    ids: &[String],
) -> Result<(), CoreError> {
    for id in ids {
        let other = state.workspaces.get(id).ok_or_else(|| {
            error(
                "invalid_workspace_verification",
                "A verified overlap must name an already registered workspace",
            )
        })?;
        // The roots need not overlap as text: a registered root kept as
        // given can be a link to a directory around or inside `root`, and
        // only the adapter can resolve it.
        if id == workspace_id || other.root == root {
            return Err(error(
                "invalid_workspace_verification",
                "Verified overlaps must name distinct physical roots; identical roots must share one workspace ID",
            ));
        }
    }
    Ok(())
}

// The adapter supplies physical canonical roots. This lexical slash-component
// check is an additional guard; platform-specific alias checks belong there.
fn roots_overlap(a: &str, b: &str) -> bool {
    fn contains(parent: &str, child: &str) -> bool {
        parent == child
            || child
                .strip_prefix(parent)
                .is_some_and(|suffix| parent.ends_with('/') || suffix.starts_with('/'))
    }
    contains(a, b) || contains(b, a)
}

fn validate_session_registration(state: &State, session: &Session) -> Result<(), CoreError> {
    validate_id(&session.session_id, "session ID")?;
    validate_id(&session.workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(&session.workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Workspace is not registered; register it before attaching a session",
        ));
    }
    if session.generation != state.generation {
        return Err(error(
            "invalid_event",
            "Session registration must use the current daemon generation",
        ));
    }
    if let Some(parent) = &session.parent {
        validate_id(parent, "parent session ID")?;
        if *parent == session.session_id {
            return Err(error(
                "invalid_session",
                "A session cannot be its own parent; leave the parent out for a top-level session",
            ));
        }
    }
    if let Some(existing) = state.sessions.get(&session.session_id)
        && existing.workspace_id != session.workspace_id
    {
        return Err(error(
            "session_conflict",
            "Session ID is already bound to a different workspace; use a new session ID",
        ));
    }
    Ok(())
}

fn validate_session_liveness(state: &State, liveness: &SessionLiveness) -> Result<(), CoreError> {
    known_session(state, &liveness.session_id)?;
    if liveness.process.pid == 0 {
        return Err(error(
            "invalid_process_identity",
            "Process identity requires a positive PID and an OS boot/start token",
        ));
    }
    validate_id(&liveness.process.start_token, "process start token")?;
    if liveness.generation != state.generation {
        return Err(error(
            "invalid_event",
            "Liveness observations must use the current daemon generation",
        ));
    }
    if let Some(current) = state.session_liveness.get(&liveness.session_id)
        && current.generation == state.generation
    {
        if current.process != liveness.process {
            return Err(error(
                "process_identity_mismatch",
                "This session already names a different process incarnation in this generation; do not reuse its process identity",
            ));
        }
        if liveness.observed_at_ms < current.observed_at_ms {
            return Err(error(
                "stale_liveness_observation",
                "A liveness observation cannot replace newer evidence in the same generation",
            ));
        }
    }
    Ok(())
}

fn lease_relation(state: &State, lease: &Lease, now_ms: u64) -> LeaseRelation {
    match lease.status {
        LeaseStatus::Completed => LeaseRelation::Completed,
        LeaseStatus::Invalidated => LeaseRelation::Invalidated,
        _ if lease.generation != state.generation => LeaseRelation::Invalidated,
        LeaseStatus::Expired => LeaseRelation::Expired,
        LeaseStatus::Active if lease.expires_at_ms <= now_ms => LeaseRelation::Expired,
        LeaseStatus::Active => LeaseRelation::Current,
    }
}

fn known_session<'a>(state: &'a State, session_id: &str) -> Result<&'a Session, CoreError> {
    validate_id(session_id, "session ID")?;
    state.sessions.get(session_id).ok_or_else(|| {
        error(
            "session_not_found",
            "Session is not registered; attach it to a workspace first",
        )
    })
}

fn current_session<'a>(state: &'a State, session_id: &str) -> Result<&'a Session, CoreError> {
    validate_id(session_id, "session ID")?;
    let session = state.sessions.get(session_id).ok_or_else(|| {
        error(
            "session_not_found",
            "Session is not registered; attach it to a workspace first",
        )
    })?;
    if session.generation != state.generation {
        return Err(error(
            "stale_session",
            "Session belongs to a previous daemon generation; register it again to reattach",
        ));
    }
    Ok(session)
}

fn error(code: &str, message: &str) -> CoreError {
    CoreError {
        code: code.into(),
        message: message.into(),
    }
}

impl Recap {
    /// Drop items, newest-last lists first, until `fits` says the recap is
    /// small enough. Each drop is counted in `omitted_items`.
    pub fn shrink_until(&mut self, mut fits: impl FnMut(&Recap) -> bool) {
        while !fits(self) {
            let dropped = self.notes.pop().is_some()
                || self.trails.pop().is_some()
                || self.teammates.pop().is_some()
                || self.checks.pop().is_some()
                || self.landings.pop().is_some()
                || self.interface_changes.pop().is_some()
                || self.sessions.pop().is_some();
            if !dropped {
                return;
            }
            self.omitted_items = self.omitted_items.saturating_add(1);
        }
    }
}
