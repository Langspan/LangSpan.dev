//! Pure coordination decisions and event replay. Time and identifiers are supplied
//! by the caller; this crate performs no I/O and cannot fence filesystem writers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

mod dashboard;
mod lines;
mod recall;
mod status;
mod status_line;
mod today;
pub use dashboard::{
    AgentRow, AgentState, ChangeRow, CheckRow, DASHBOARD_AGENTS, DASHBOARD_ITEMS, DASHBOARD_PATHS,
    DASHBOARD_TEXT, DENIAL_WINDOW, Dashboard, DenialRecord, DenialRow, Denials, EditRow,
    LandingRow, MessageRow, NoteRow, NoticeRow, OverlapRow, PublishRow, RecentFile, ScanRow,
    WorkspaceRow, dashboard_denials,
};
pub use recall::{
    RECALL_LIMIT, RECALL_QUERY_BYTES, RECALL_WORDS, Recall, RecalledNote, recall as recall_notes,
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
    /// Interface notices this session was alerted to at a write, newest
    /// first, at most `MAX_ALERTED`. Each is alerted once (ADR 0029).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alerted: Vec<String>,
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

fn is_false(value: &bool) -> bool {
    !*value
}

fn one() -> u32 {
    1
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
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
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
}

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
}

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
    /// `removed`, `signature_changed`, `definition_added` or
    /// `definition_removed`. The last two mean only the number of
    /// definitions sharing the name and kind changed: `after` (added) or
    /// `before` (removed) holds the signatures that came or went, and the
    /// counts say how many there were and are.
    pub change: String,
    pub before: Option<String>,
    pub after: Option<String>,
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

/// A written call whose argument count does not fit a signature another
/// live session just changed. A name match, not a resolved call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallBreak {
    pub notice_id: String,
    pub call: CallSite,
    /// The file whose exported signature changed.
    pub defined_in: String,
    pub by_session: String,
    pub before: String,
    pub after: String,
    pub before_arity: Option<Arity>,
    pub after_arity: Arity,
    pub age_ms: u64,
}

/// Calls one write check names at most.
pub const MAX_CALL_SITES: usize = 64;
/// Breaks one answer carries at most.
pub const MAX_CALL_BREAKS: usize = 8;
/// Notices remembered as alerted per session.
pub const MAX_ALERTED: usize = 64;

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
    /// The session that most recently reported editing `path`. A
    /// correlation, not proof of who made the change.
    pub by_session: Option<String>,
    pub at_ms: u64,
    pub generation: u64,
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
    /// Sessions that have acknowledged a message for everyone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acknowledged: Vec<String>,
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
    /// Messages still owed to at least one session, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
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
}

pub const MAX_CHECKS: usize = 64;
pub const MAX_LANDINGS: usize = 32;
/// The longest a push turn may be held without being released.
pub const MAX_TURN_TTL_MS: u64 = 4 * 60 * 60 * 1000;
/// Tries at landing within one turn, when the target keeps moving.
pub const MAX_LANDING_ATTEMPTS: usize = 3;
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
    /// Remembered notes about the workspace or about files this session
    /// works on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    /// Messages addressed to this session or to everyone, and a pending
    /// handoff, that it has not yet acknowledged. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// The checkout this session works in changed under it since it last
    /// read or edited a file there. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkout_changes: Vec<CheckoutNotice>,
    /// Results of the automatic check run after this session's writes, not
    /// yet delivered. Each is delivered once. Newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckNotice>,
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
    /// Workspaces whose last scan attempt failed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scan_failures: BTreeMap<String, ScanFailure>,
    /// Workspaces whose last completed scan left something unversioned.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scan_coverage: BTreeMap<String, ScanCoverage>,
    /// Checkout changes per workspace, newest first, at most `MAX_CHECKOUTS`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checkouts: BTreeMap<String, Vec<CheckoutChange>>,
    /// The automatic check per workspace, for the workspaces where the owner
    /// turned one on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub auto_checks: BTreeMap<String, AutoCheck>,
    /// Automatic check results not yet delivered, per session, newest first.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub check_notices: BTreeMap<String, Vec<CheckNotice>>,
    /// Agent hosts present on this machine with no LangSpan install that
    /// covers them, by host name (roadmap A7). For the owner; agents are
    /// not told.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub uninstalled_hosts: BTreeMap<String, UninstalledHost>,
    /// Sessions whose presence setting is not the default, by session ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub presence: BTreeMap<String, PresenceSetting>,
    /// The current generation's clock anchor, once recorded. Derived from
    /// the events on replay; not part of the status reply.
    #[serde(skip)]
    pub clock_anchor: Option<ClockAnchor>,
    /// Recent refusals placed in wall time, newest first, for the status
    /// line (roadmap H2). Derived on replay; not part of the status reply.
    #[serde(skip)]
    pub refusals: Vec<RecentRefusal>,
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
/// scan's branch observation. Who did it is not known.
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
        at_ms: u64,
        generation: u64,
    },
    WorkspaceRegistered {
        workspace: Workspace,
    },
    SessionRegistered {
        session: Session,
    },
    SessionLivenessObserved {
        liveness: SessionLiveness,
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
    },
    /// A write was told about these interface notices (ADR 0029).
    InterfaceAlerted {
        session_id: String,
        notice_ids: Vec<String>,
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
    Remembered {
        note: Note,
    },
    Forgotten {
        workspace_id: String,
        note_id: String,
    },
    MessageSent {
        message: Message,
    },
    MessageAcknowledged {
        workspace_id: String,
        message_id: String,
        session_id: String,
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
    /// of each written-lines entry of an admission (`WrittenLines::age_ms`),
    /// which changes between two deliveries of one tool call.
    pub fn same_request(&self, other: &Command) -> bool {
        fn ageless(command: &Command) -> Option<Command> {
            let mut command = command.clone();
            let Command::AdmitTool { overwrites, .. } = &mut command else {
                return None;
            };
            for entry in overwrites.iter_mut() {
                entry.age_ms = 0;
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
        report: Option<CheckReport>,
    },
    /// Run `argv` automatically after a session's writes in the workspace,
    /// recorded as check `name`. Replaces any earlier automatic check.
    AutoCheckEnable {
        workspace_id: String,
        name: String,
        argv: Vec<String>,
        timeout_s: u32,
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
    Remember {
        workspace_id: String,
        author: Option<String>,
        text: String,
        paths: Vec<String>,
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
    /// Leave one line for the next other session that starts or is
    /// prompted in this session's workspace. Leases, intents and activity
    /// are unchanged.
    Handoff {
        session_id: String,
        text: String,
        paths: Vec<String>,
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
    /// The daemon's host check: every host present now with no LangSpan
    /// install covering it, by name (roadmap A7). Hosts held from an
    /// earlier check and absent here are cleared.
    ObserveHosts {
        not_installed: Vec<String>,
    },
    /// Read-only: a bounded account of one workspace.
    Recap {
        workspace_id: String,
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
    /// Read-only, answered from the event log: what happened since
    /// `from_ms` (wall clock, Unix milliseconds) in one workspace, or in
    /// every workspace when none is named, and what is still open.
    Today {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        from_ms: u64,
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
    /// Read-only: for each name that is defined in the workspace, up to
    /// `limit` definitions and the files that mention it. Never recorded.
    GraphContext {
        workspace_id: String,
        names: Vec<String>,
        limit: u32,
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
    },
    /// The session finished a reply: complete what its tool calls still
    /// hold. `failure` is the host's error type (lower-case letters and
    /// underscores) when the reply ended on an error.
    SessionStop {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<String>,
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
    Events {
        after: u64,
        limit: u32,
    },
    Changes {
        workspace_id: String,
        after: u64,
        limit: u32,
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
        report: Option<CheckReport>,
    },
    /// Run `argv` automatically after a session's writes in the workspace,
    /// recorded as check `name`. Replaces any earlier automatic check.
    AutoCheckEnable {
        workspace_id: String,
        name: String,
        argv: Vec<String>,
        timeout_s: u32,
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
    Remember {
        workspace_id: String,
        author: Option<String>,
        text: String,
        paths: Vec<String>,
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
    /// Leave one line for the next other session that starts or is
    /// prompted in this session's workspace. Leases, intents and activity
    /// are unchanged.
    Handoff {
        session_id: String,
        text: String,
        paths: Vec<String>,
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
    /// The daemon's host check: every host present now with no LangSpan
    /// install covering it, by name (roadmap A7). Hosts held from an
    /// earlier check and absent here are cleared.
    ObserveHosts {
        not_installed: Vec<String>,
    },
    /// Read-only: a bounded account of one workspace.
    Recap {
        workspace_id: String,
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
    /// Read-only, answered from the event log: what happened since
    /// `from_ms` (wall clock, Unix milliseconds) in one workspace, or in
    /// every workspace when none is named, and what is still open.
    Today {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        from_ms: u64,
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
    /// Read-only: for each name that is defined in the workspace, up to
    /// `limit` definitions and the files that mention it. Never recorded.
    GraphContext {
        workspace_id: String,
        names: Vec<String>,
        limit: u32,
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
    },
    /// The session finished a reply: complete what its tool calls still
    /// hold. `failure` is the host's error type (lower-case letters and
    /// underscores) when the reply ended on an error.
    SessionStop {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<String>,
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
    },
    Status {},
    StatusRecords {
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        page_bytes: Option<u32>,
    },
    Dashboard {},
    Events {
        after: u64,
        limit: u32,
    },
    Changes {
        workspace_id: String,
        after: u64,
        limit: u32,
    },
}

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        CommandWire::deserialize(deserializer)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Workspace {
        workspace: Workspace,
    },
    Session {
        session: Session,
    },
    SessionLiveness {
        liveness: SessionLiveness,
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
    Remembered {
        note: Note,
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
    Recap {
        recap: Recap,
    },
    Recall {
        recall: Recall,
    },
    Today {
        today: Today,
    },
    StatusLine {
        line: StatusLine,
    },
    RefusalRecorded {
        session_id: String,
    },
    Dashboard {
        dashboard: Dashboard,
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
    },
    /// Only names with at least one definition, in the order asked.
    GraphContext {
        matches: Vec<NameContext>,
        coverage: GraphCoverage,
    },
    /// `ready` is false when the index has no complete scan of the
    /// workspace: nothing was checked.
    CallBreaks {
        breaks: Vec<CallBreak>,
        coverage: GraphCoverage,
        ready: bool,
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
        Ok(decision) => decision,
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
            let regions = normalized_regions(regions, &paths)?
                .into_iter()
                // Regions are trusted only for the version the hook saw on
                // disk when it asked; otherwise the path is held whole.
                .filter(|located| {
                    base.iter().flatten().any(|file| {
                        file.path == located.path
                            && matches!(&file.state, FileState::Present { digest, .. } if *digest == located.digest)
                    })
                })
                .collect::<Vec<_>>();
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
            let holders: Vec<Conflict> =
                conflicts(state, &session.workspace_id, &paths, &regions, now_ms)
                    .into_iter()
                    .filter(|holder| {
                        !(tool_call
                            && holder.session_id == *session_id
                            && state
                                .leases
                                .get(&holder.lease_id)
                                .is_some_and(|lease| lease.operation.is_some()))
                    })
                    .collect();
            // Each refusal is one event, stored with the refusal's receipt:
            // the same request again is answered from it and adds none.
            let refused = |kind, peer: Option<&String>| Event::WriteRefused {
                session_id: session_id.clone(),
                workspace_id: session.workspace_id.clone(),
                kind,
                paths: paths.iter().take(REFUSAL_PATHS).cloned().collect(),
                peer_session_id: peer.cloned(),
                at_ms: now_ms,
                generation: state.generation,
            };
            if !holders.is_empty() {
                // A hook's tool call; a manual `admit` refused is not a write.
                return Ok(Decision {
                    events: tool_call
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
        Command::SessionEnd { session_id } => {
            known_session(state, session_id)?;
            let events = if state
                .activity
                .get(session_id)
                .is_none_or(|activity| !activity.ended)
                || state.intents.contains_key(session_id)
            {
                vec![Event::SessionEnded {
                    session_id: session_id.clone(),
                }]
            } else {
                Vec::new()
            };
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
        } => {
            known_session(state, session_id)?;
            if let Some(failure) = failure {
                validate_failure(failure)?;
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
            };
            validate_check(state, &check)?;
            let mut events = vec![Event::CheckRecorded {
                check: check.clone(),
            }];
            if let Some(report) = report {
                let reported = Event::CheckReported {
                    session_ids: report.session_ids.clone(),
                    notice: CheckNotice {
                        check_id: check.check_id.clone(),
                        name: check.name.clone(),
                        tree_oid: check.tree_oid.clone(),
                        result: check.result.clone(),
                        duration_ms: check.duration_ms,
                        timed_out_s: report.timed_out_s,
                        tail: report.tail.clone(),
                    },
                };
                validate_check_report(state, &check.workspace_id, &reported)?;
                events.push(reported);
            }
            Ok(Decision {
                events,
                outcome: Outcome::CheckRecorded { check },
            })
        }
        Command::AutoCheckEnable {
            workspace_id,
            name,
            argv,
            timeout_s,
        } => {
            let check = AutoCheck {
                workspace_id: workspace_id.clone(),
                name: name.clone(),
                argv: argv.clone(),
                timeout_s: *timeout_s,
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
        } => {
            let landing = Landing {
                landing_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                onto: onto.clone(),
                commit_oid: commit_oid.clone(),
                tree_oid: tree_oid.clone(),
                inputs: inputs.clone(),
                check_id: check_id.clone(),
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
        Command::Brief {
            session_id,
            max_peers,
            deliver,
            mid_turn,
        } => {
            if !(1..=MAX_BRIEF_PEERS).contains(max_peers) {
                return Err(error(
                    "invalid_limit",
                    "A briefing names between 1 and 16 peers",
                ));
            }
            let (mut briefing, signature) =
                brief(state, session_id, *max_peers, !*mid_turn, now_ms)?;
            let previous = state
                .activity
                .get(session_id)
                .and_then(|activity| activity.briefed.as_deref())
                .unwrap_or(EMPTY_SIGNATURE);
            // Delivered messages are acknowledged below and so are absent
            // from the next briefing: the remembered signature leaves them
            // out, and any message at all makes this briefing new.
            // Presence is news only at a session start or prompt, and only
            // while there is some: where peers look, newly or differently.
            // Mid-turn, or when it merely aged out, it is not (see `brief`).
            let differs = without_presence(&signature) != without_presence(previous)
                || (!*mid_turn && signature.contains('+') && signature != previous);
            briefing.changed =
                differs || !briefing.messages.is_empty() || !briefing.checks.is_empty();
            let mut events = Vec::new();
            if *deliver {
                if signature != previous && briefing.changed {
                    events.push(Event::BriefingDelivered {
                        session_id: session_id.clone(),
                        signature,
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
                        }),
                );
                events.extend(briefing.checks.iter().map(|notice| Event::CheckDelivered {
                    session_id: session_id.clone(),
                    check_id: notice.check_id.clone(),
                }));
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
            sent(request_id, workspace_id, from.clone(), to, text, paths)
        }
        Command::Handoff {
            session_id,
            text,
            paths,
        } => {
            let session = known_session(state, session_id)?;
            sent(
                request_id,
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
        } => {
            validate_known_workspace(state, workspace_id)?;
            if let Some(author) = author {
                known_session(state, author)?;
            }
            let note = Note {
                note_id: request_id.into(),
                workspace_id: workspace_id.clone(),
                author: author.clone(),
                text: text.clone(),
                paths: if paths.is_empty() {
                    Vec::new()
                } else {
                    normalized_paths(paths)?
                },
            };
            validate_note(&note)?;
            Ok(Decision {
                events: vec![Event::Remembered { note: note.clone() }],
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
        Command::Recap { workspace_id } => {
            validate_known_workspace(state, workspace_id)?;
            Ok(Decision {
                events: Vec::new(),
                outcome: Outcome::Recap {
                    recap: recap(state, workspace_id, now_ms),
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
        Command::Events { .. }
        | Command::Today { .. }
        | Command::StatusRecords { .. }
        | Command::Changes { .. }
        | Command::RemoteSent { .. }
        | Command::GraphSymbol { .. }
        | Command::GraphReferences { .. }
        | Command::GraphImpact { .. }
        | Command::GraphBlindSpots { .. }
        | Command::GraphContext { .. }
        | Command::CheckCalls { .. } => Err(error(
            "storage_operation",
            "Event history, status pages, change projections and index queries are answered by the daemon's stores",
        )),
    }
}

fn validate_note(note: &Note) -> Result<(), CoreError> {
    validate_request_id(&note.note_id)?;
    if note.text.trim().is_empty()
        || note.text.len() > NOTE_BYTES
        || note.text.chars().any(char::is_control)
        || note.paths.len() > BRIEF_PATHS
    {
        return Err(error(
            "invalid_note",
            "A note is one line of 1 to 400 bytes about at most 8 files",
        ));
    }
    Ok(())
}

fn sent(
    request_id: &str,
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
    };
    validate_message(&message)?;
    Ok(Decision {
        events: vec![Event::MessageSent {
            message: message.clone(),
        }],
        outcome: Outcome::MessageSent { message },
    })
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
/// that quoting can grow at most fourfold, cut at `MESSAGE_LINE_LIMIT` plus
/// its ellipsis and newline.
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
fn recap(state: &State, workspace_id: &str, now_ms: u64) -> Recap {
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
                in_generation.map(|activity| activity.last_ms).unwrap_or(0),
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
            .map(|notes| notes.iter().take(RECAP_SESSIONS).cloned().collect())
            .unwrap_or_default(),
        messages: state
            .messages
            .get(workspace_id)
            .map(|messages| messages.iter().take(RECAP_SESSIONS).cloned().collect())
            .unwrap_or_default(),
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
    {
        return Err(error(
            "invalid_notice",
            "An interface notice names 1 to 16 changes and at most 32 dependent files",
        ));
    }
    for path in &notice.dependents {
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
        ];
        let counts = match (change.before_count, change.after_count) {
            (Some(before), Some(after)) => Some((before, after)),
            (None, None) => None,
            _ => Some((0, 0)),
        };
        // A count change names its counts, in the direction it says, and
        // carries the signatures that came or went; nothing else has
        // counts.
        let shaped = match (change.change.as_str(), counts) {
            ("removed" | "signature_changed", None) => true,
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
                "An interface change has a name, a kind, `removed`, `signature_changed`, or `definition_added` or `definition_removed` with both counts, and single-line signatures of at most 256 bytes",
            ));
        }
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
                    && now_ms.saturating_sub(activity.last_ms) <= BRIEF_HORIZON_MS
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
/// so its absence afterwards is not a change.
fn brief(
    state: &State,
    session_id: &str,
    max_peers: u32,
    handoffs: bool,
    now_ms: u64,
) -> Result<(Briefing, String), CoreError> {
    let me = current_session(state, session_id)?;
    let repository = state
        .workspace_git
        .get(&me.workspace_id)
        .map(|git| git.repository_id.as_str());
    let work_paths = |id: &str| -> (Vec<String>, Vec<RecentPath>, BTreeSet<String>) {
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
    };
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
                && now_ms.saturating_sub(activity.last_ms) <= BRIEF_HORIZON_MS
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
        let last_ms = activity.map(|activity| activity.last_ms).unwrap_or(0);
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
    peers.sort_by(|a, b| (&a.0, &a.1.session_id).cmp(&(&b.0, &b.1.session_id)));
    let total = peers.len();
    let mut bytes = 0;
    let peers: Vec<PeerWork> = peers
        .into_iter()
        .map(|(_, peer)| peer)
        .take(max_peers as usize)
        .take_while(|peer| {
            bytes += 256
                + peer.workspace_root.len()
                + [&peer.summary, &peer.intent, &peer.branch_ref]
                    .into_iter()
                    .flatten()
                    .map(String::len)
                    .sum::<usize>()
                + peer
                    .editing
                    .iter()
                    .chain(&peer.overlap)
                    .chain(&peer.target_overlap)
                    .chain(peer.recent.iter().map(|recent| &recent.path))
                    .chain(peer.looking.iter().map(|looked| &looked.path))
                    .map(|path| path.len() + 32)
                    .sum::<usize>();
            bytes <= BRIEF_BYTES
        })
        .collect();
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
    // Files this session works on or has read: what a changed interface
    // could break for it.
    let mut concerned = mine.clone();
    concerned.extend(
        known_versions(state, session_id)
            .iter()
            .map(|version| version.path.clone()),
    );
    let interface_changes: Vec<InterfaceImpact> = state
        .interface_notices
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|notice| {
            notice.generation == state.generation
                && now_ms.saturating_sub(notice.at_ms) <= BRIEF_HORIZON_MS
                && notice.by_session.as_deref() != Some(session_id)
        })
        .filter_map(|notice| {
            let affected: Vec<String> = notice
                .dependents
                .iter()
                .filter(|path| concerned.contains(*path))
                .take(BRIEF_PATHS)
                .cloned()
                .collect();
            (!affected.is_empty()).then(|| InterfaceImpact {
                path: notice.path.clone(),
                by_session: notice.by_session.clone(),
                changes: notice.changes.clone(),
                affected,
                age_ms: now_ms.saturating_sub(notice.at_ms),
            })
        })
        .take(4)
        .collect();
    // Notes about the whole workspace, or about a file this session works on.
    let notes: Vec<Note> = state
        .notes
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|note| {
            note.author.as_deref() != Some(session_id)
                && (note.paths.is_empty() || note.paths.iter().any(|path| concerned.contains(path)))
        })
        .take(4)
        .cloned()
        .collect();
    // Messages owed to this session, newest first, as many as the text of
    // one briefing is sure to hold; the rest wait for the next one.
    let mut bytes = 0;
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
            bytes <= MESSAGE_BRIEF_BYTES
        })
        .map(|message| Message {
            acknowledged: Vec::new(),
            ..message.clone()
        })
        .collect();
    // A checkout change matters to a session that read or edited files
    // here before it: what it knows may no longer be on disk.
    let mut checkout_ids = Vec::new();
    let checkout_changes: Vec<CheckoutNotice> = state
        .checkouts
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
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
            CheckoutNotice {
                from_branch: change.from_branch.clone(),
                to_branch: change.to_branch.clone(),
                from_head: change.from_head.clone(),
                to_head: change.to_head.clone(),
                files: change.files,
                age_ms: now_ms.saturating_sub(change.at_ms),
            }
        })
        .collect();
    let mut signature = String::new();
    for id in &checkout_ids {
        signature.push_str(&format!("checkout|{id}\n"));
    }
    for note in &notes {
        signature.push_str(&format!("note|{}\n", note.note_id));
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
    // reads do not interrupt a turn. The paths are sorted: reading the same
    // files in another order is not news.
    let mut presence = String::new();
    for peer in &peers {
        if peer.looking.is_empty() {
            continue;
        }
        let paths: BTreeSet<&str> = peer
            .looking
            .iter()
            .map(|looked| looked.path.as_str())
            .collect();
        presence.push_str(&format!(
            "{}|{}\n",
            peer.session_id,
            paths.into_iter().collect::<Vec<_>>().join(",")
        ));
    }
    // Without presence the signature is as it was before presence existed,
    // so a briefing stored then is not delivered again.
    let mut signature = fingerprint(&signature);
    if !presence.is_empty() {
        signature.push('+');
        signature.push_str(&fingerprint(&presence));
    }
    Ok((
        Briefing {
            session_id: session_id.into(),
            changed: false,
            omitted_peers: u32::try_from(total - peers.len()).unwrap_or(u32::MAX),
            peers,
            stale_reads,
            remote,
            interface_changes,
            notes,
            messages,
            checkout_changes,
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
        },
        signature,
    ))
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

/// Whether the session wrote a file since its last tool batch or stop.
fn wrote_since_boundary(state: &State, session_id: &str) -> bool {
    state
        .activity
        .get(session_id)
        .is_some_and(|activity| activity.wrote_since_boundary)
}

fn validate_auto_check(state: &State, check: &AutoCheck) -> Result<(), CoreError> {
    validate_known_workspace(state, &check.workspace_id)?;
    validate_id(&check.name, "check name")?;
    if check.name.len() > 64
        || check.argv.is_empty()
        || check.argv.len() > MAX_AUTO_CHECK_ARGS
        || check.argv[0].is_empty()
        || check.argv.iter().any(|arg| {
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
    if !(1..=MAX_AUTO_CHECK_TIMEOUT_S).contains(&check.timeout_s) {
        return Err(error(
            "invalid_limit",
            &format!("An automatic check's time limit is 1 to {MAX_AUTO_CHECK_TIMEOUT_S} seconds"),
        ));
    }
    Ok(())
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
            paths,
            peer_session_id,
            at_ms,
            generation,
            ..
        } => {
            known_session(state, session_id)?;
            validate_known_workspace(state, workspace_id)?;
            normalized_paths(paths)?;
            if paths.len() > REFUSAL_PATHS
                || peer_session_id
                    .as_ref()
                    .is_some_and(|peer| peer.len() > 200)
            {
                return Err(error(
                    "invalid_event",
                    "A refusal keeps at most 8 paths and one peer session ID",
                ));
            }
            status_line::remember(state, workspace_id, *at_ms, *generation);
        }
        Event::WorkspaceRegistered { workspace } => {
            validate_workspace(state, workspace)?;
            state
                .workspaces
                .insert(workspace.workspace_id.clone(), workspace.clone());
        }
        Event::SessionRegistered { session } => {
            validate_session_registration(state, session)?;
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
        // Boundaries in the log. The admissions a stop released ended in
        // their own `LeaseCompleted` events.
        Event::SessionStopped {
            session_id,
            failure,
            generation,
            ..
        } => {
            known_session(state, session_id)?;
            if let Some(failure) = failure {
                validate_failure(failure)?;
            }
            if *generation != state.generation {
                return Err(error(
                    "invalid_event",
                    "A stop record must belong to the current daemon generation",
                ));
            }
            // Only the mark: a boundary does not bring an ended session back
            // or start an activity record.
            if let Some(activity) = state.activity.get_mut(session_id) {
                activity.wrote_since_boundary = false;
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
        Event::Remembered { note } => {
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
        Event::MessageSent { message } => {
            validate_message(message)?;
            validate_known_workspace(state, &message.workspace_id)?;
            if !message.acknowledged.is_empty() {
                return Err(error("invalid_event", "A message is sent unacknowledged"));
            }
            let messages = state
                .messages
                .entry(message.workspace_id.clone())
                .or_default();
            messages.retain(|held| held.message_id != message.message_id);
            messages.insert(0, message.clone());
            messages.truncate(MAX_MESSAGES);
        }
        Event::MessageAcknowledged {
            workspace_id,
            message_id,
            session_id,
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
        Event::InterfaceChanged { notice } => {
            validate_notice(notice)?;
            validate_known_workspace(state, &notice.workspace_id)?;
            let notices = state
                .interface_notices
                .entry(notice.workspace_id.clone())
                .or_default();
            // A newer notice for the same file replaces the older one.
            notices.retain(|held| held.path != notice.path);
            notices.insert(0, notice.clone());
            notices.truncate(MAX_NOTICES);
        }
        Event::CheckRecorded { check } => {
            validate_check(state, check)?;
            let records = state.checks.entry(check.workspace_id.clone()).or_default();
            // One record per name and tree: a rerun replaces the earlier result.
            records
                .retain(|record| !(record.name == check.name && record.tree_oid == check.tree_oid));
            records.insert(0, check.clone());
            records.truncate(MAX_CHECKS);
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
        Event::BriefingDelivered {
            session_id,
            signature,
        } => {
            current_session(state, session_id)?;
            if signature.len() > 64 {
                return Err(error(
                    "invalid_event",
                    "A briefing signature is at most 64 bytes",
                ));
            }
            activity_mut(state, session_id).briefed = Some(signature.clone());
        }
        Event::DiskObserved { observation } => {
            validate_disk_observation(state, observation)?;
            // A first baseline is not a change. Anything else means the
            // working tree a check ran on is no longer the working tree.
            if observation
                .before
                .as_ref()
                .is_some_and(|before| before != &observation.after)
                && let Some(records) = state.checks.get_mut(&observation.workspace_id)
            {
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

/// Signature changes to functions that another live session of this
/// session's workspace made, in this generation and within the briefing
/// horizon, that this session has not been alerted to: `(notice, change,
/// after's arity, the peer)`. Newest notice first.
fn alertable<'a>(
    state: &'a State,
    session_id: &str,
    now_ms: u64,
) -> Result<Vec<(&'a InterfaceNotice, &'a InterfaceChange, Arity, &'a str)>, CoreError> {
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
    for notice in state
        .interface_notices
        .get(&me.workspace_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(peer) = notice.by_session.as_deref() else {
            continue;
        };
        if notice.generation != state.generation
            || now_ms.saturating_sub(notice.at_ms) > BRIEF_HORIZON_MS
            || peer == session_id
            || alerted.contains(&notice.notice_id)
            || !live(peer)
        {
            continue;
        }
        for change in &notice.changes {
            if let (true, Some(arity)) = (
                change.change == "signature_changed"
                    && matches!(change.kind.as_str(), "function" | "method"),
                change.after_arity,
            ) {
                found.push((notice, change, arity, peer));
            }
        }
    }
    Ok(found)
}

/// The names among `calls` that a write check must look up in the index:
/// those of a change it could alert to. Usually none, and then nothing is
/// looked up.
pub fn call_candidates(
    state: &State,
    session_id: &str,
    calls: &[CallSite],
    now_ms: u64,
) -> Result<BTreeSet<String>, CoreError> {
    validate_calls(calls)?;
    Ok(alertable(state, session_id, now_ms)?
        .into_iter()
        .filter(|(_, change, _, _)| calls.iter().any(|call| call.name == change.name))
        .map(|(_, change, _, _)| change.name.clone())
        .collect())
}

/// The calls that do not fit a changed signature, at most
/// `MAX_CALL_BREAKS`, from notices this session was not alerted to. A
/// change counts only while `current` (the index's definitions of the
/// called names) still holds its new signature in its file, so a notice
/// the index has moved past is never used. Pure; the caller records the
/// alert.
pub fn call_breaks(
    state: &State,
    session_id: &str,
    calls: &[CallSite],
    current: &[SymbolRef],
    now_ms: u64,
) -> Result<Vec<CallBreak>, CoreError> {
    validate_calls(calls)?;
    let mut breaks = Vec::new();
    for (notice, change, arity, peer) in alertable(state, session_id, now_ms)? {
        let after = change.after.as_deref().unwrap_or("");
        let still = current.iter().any(|symbol| {
            symbol.path == notice.path && symbol.name == change.name && symbol.signature == after
        });
        if !still {
            continue;
        }
        for call in calls
            .iter()
            .filter(|call| call.name == change.name && !arity.fits(call.args, call.member))
        {
            if breaks.len() == MAX_CALL_BREAKS {
                return Ok(breaks);
            }
            breaks.push(CallBreak {
                notice_id: notice.notice_id.clone(),
                call: call.clone(),
                defined_in: notice.path.clone(),
                by_session: peer.to_owned(),
                before: change.before.clone().unwrap_or_default(),
                after: after.to_owned(),
                before_arity: change.before_arity,
                after_arity: arity,
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
/// adapters name them (`claude-code`, `gemini-cli`).
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
    for reference in [&change.from_branch, &change.to_branch]
        .into_iter()
        .flatten()
    {
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
    for oid in [&change.from_head, &change.to_head].into_iter().flatten() {
        validate_oid(oid)?;
    }
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
        if id == workspace_id || other.root == root || !roots_overlap(&other.root, root) {
            return Err(error(
                "invalid_workspace_verification",
                "Verified overlaps must name distinct, overlapping physical roots; identical roots must share one workspace ID",
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
