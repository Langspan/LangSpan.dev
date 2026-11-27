//! Pure coordination decisions and event replay. Time and identifiers are supplied
//! by the caller; this crate performs no I/O and cannot fence filesystem writers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

mod admission;
mod anchors;
mod authority;
mod briefing;
mod checks;
mod classes;
mod command;
mod count;
mod dashboard;
mod event;
mod flaky;
mod history;
mod lines;
mod messages;
mod notices;
mod observation;
mod outcome;
mod policy;
mod rank;
mod recall;
mod recent;
mod refusal_outcome;
mod remote;
mod report;
mod sessions;
mod since;
mod status;
mod status_line;
mod today;
mod trail;
mod validate;
mod workspaces;
mod write_check;
// The eight domain modules are empty until each domain moves in from its
// banners below; the allow does nothing once one holds an item.
#[allow(unused_imports)]
pub use admission::*;
pub use anchors::{
    ANCHOR_CANDIDATES, AnchorIndex, AnchorLookup, AnchorState, MAX_TAGS, NoteAnchor, SYMBOL_BYTES,
    TAG_BYTES, anchored, remember_lookups, validate_tag, validate_tags,
};
pub use authority::{Authority, authority};
#[allow(unused_imports)]
pub use briefing::*;
#[allow(unused_imports)]
pub use checks::*;
pub use classes::{
    FailureClass, KeptTests, MAX_FIXED_NAMED, MAX_WRITE_RUNS, MAX_WRITERS_NAMED, WriteRun,
};
pub use command::Command;
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
pub use event::Event;
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
#[allow(unused_imports)]
pub use messages::*;
#[allow(unused_imports)]
pub use notices::*;
#[allow(unused_imports)]
pub use observation::*;
pub use outcome::Outcome;
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
#[allow(unused_imports)]
pub use remote::*;
pub use report::{
    OTHER_HOST, REPORT_HISTORY_EVENTS, REPORT_SESSIONS, Report, ReportCounts, ReportHost,
    ReportKnown, ReportQuery, ReportSession, host_kind, known_sessions, open_intents, report,
    session_workspaces,
};
#[allow(unused_imports)]
pub use sessions::*;
pub use since::{
    AWAY_AFTER_MS, MAX_AWAY_AFTER_MS, SINCE_EVENTS, SINCE_ITEMS, Since, SinceChecks, SinceFailure,
    SinceFrom, SinceInterface, SinceLanding, SinceMessage, SinceQuery, since,
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
pub use validate::{MAX_BATCH_OPERATIONS, validate_observed_file, validate_request_id};
pub(crate) use validate::{
    current_session, error, is_hex_digest, known_session, normalized_paths, normalized_related_ids,
    normalized_workspace_links, presence_paths, prompt_target_paths, validate_batch,
    validate_branch_observation, validate_canonical_paths, validate_checkout,
    validate_checkout_run, validate_coverage, validate_disk_observation, validate_fact_header,
    validate_failure, validate_host_name, validate_id, validate_presence_setting,
    validate_relative_path, validate_session_liveness, validate_session_registration,
    validate_summary, validate_tool_identity, validate_workspace, validate_workspace_link_targets,
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

// ---- domain: messages ----
// ---- end domain: messages ----

// ---- domain: remote ----
// ---- end domain: remote ----

// ---- domain: checks ----

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

// ---- end domain: checks ----

// ---- domain: observation ----

// ---- end domain: observation ----

// ---- domain: notices ----

// ---- end domain: notices ----

// ---- domain: sessions ----

// ---- end domain: sessions ----

// ---- domain: admission ----

// ---- end domain: admission ----

// ---- domain: briefing ----

/// The signature of a briefing with nothing in it.
const EMPTY_SIGNATURE: &str = "cbf29ce484222325";
/// A briefing outcome stays well inside one 64 KiB transport frame.
const BRIEF_BYTES: usize = 40 * 1024;

pub const MAX_TOUCHED: usize = 32;
pub const MAX_KNOWN: usize = 128;
pub const MAX_BRIEF_PEERS: u32 = 16;
pub const BRIEF_PATHS: usize = 8;
/// Activity older than this, within one generation, is left out of briefings.
pub const BRIEF_HORIZON_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentPath {
    pub path: String,
    pub age_ms: u64,
}

/// One other session's relevant work, as shown to the asking session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// The source index has not finished a first scan of this session's
    /// workspace or of another worktree of its repository, so `inflight`
    /// may leave out work in flight there (issue #262). Not part of the
    /// signature: it is said beside news, and is not news by itself.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inflight_partial: bool,
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

// Ties go to the lexically smallest session, so the result is deterministic.
fn smallest_id_first(id: &str) -> core::cmp::Reverse<&str> {
    core::cmp::Reverse(id)
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
            // Set by the request that carries the index's answer.
            inflight_partial: false,
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
            // Everything that says what changed: a removal's signature and
            // a count change's counts too (issue #270).
            format!(
                "{:?}",
                (
                    &change.name,
                    &change.kind,
                    &change.change,
                    &change.before,
                    &change.after,
                    &change.after_name,
                    change.before_count,
                    change.after_count,
                )
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
    stale_again: &dyn Fn(&StaleRead) -> bool,
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
        .retain(|stale| keep(stale_key(stale), acted(&stale.path) || stale_again(stale)));
    briefing
        .checkout_changes
        .retain(|change| keep(checkout_key(change), false));
    briefing.notes.retain(|note| keep(note_key(note), false));
    // Each notice's ID goes with its change, so the hook's report of what it
    // showed names the right one (issue #269).
    let mut ids = briefing.interface_notice_ids.clone().into_iter();
    let mut kept_ids = Vec::new();
    briefing.interface_changes.retain(|impact| {
        let id = ids.next();
        let fresh = keep(
            interface_key(impact),
            impact.affected.iter().any(|path| acted(path)),
        );
        if fresh {
            kept_ids.extend(id);
        }
        fresh
    });
    briefing.interface_notice_ids = kept_ids;
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

// ---- end domain: briefing ----

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordedEvent {
    pub seq: u64,
    pub event: Event,
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
        // ---- domain: messages ----
        Command::SendMessage {
            workspace_id,
            from,
            to,
            text,
            paths,
        } => messages::decide_send_message(
            state,
            workspace_id,
            from,
            to,
            text,
            paths,
            request_id,
            now_ms,
        ),
        Command::Handoff {
            session_id,
            text,
            paths,
        } => messages::decide_handoff(state, session_id, text, paths, request_id, now_ms),
        Command::AckMessage {
            session_id,
            message_id,
        } => messages::decide_ack_message(state, session_id, message_id, now_ms),
        Command::RetractMessage {
            workspace_id,
            from,
            message_id,
        } => retract(state, workspace_id, from.as_deref(), message_id.as_deref()),
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
            messages::decide_observe_note_anchors(state, workspace_id)
        }
        Command::NoteAnchorsPrepared {
            workspace_id,
            index,
        } => messages::decide_note_anchors_prepared(state, workspace_id, index),
        Command::RememberPrepared {
            workspace_id,
            author,
            text,
            paths,
            symbol,
            tags,
            index,
        } => messages::decide_remember_prepared(
            state,
            workspace_id,
            author,
            text,
            paths,
            symbol,
            tags,
            index,
            request_id,
            now_ms,
        ),
        Command::Forget {
            workspace_id,
            note_id,
        } => messages::decide_forget(state, workspace_id, note_id),
        Command::Recap {
            workspace_id,
            tag,
            since,
        } => messages::decide_recap(state, workspace_id, tag, since, now_ms),
        Command::Recall {
            workspace_id,
            query,
            path,
            limit,
        } => messages::decide_recall(state, workspace_id, query, path, limit),
        // ---- end domain: messages ----
        // ---- domain: remote ----
        Command::RemoteEnable {
            workspace_id,
            remote,
            participant,
            name,
            share_task,
            publish_as_branch,
        } => remote::decide_remote_enable(
            state,
            workspace_id,
            remote,
            participant,
            name,
            share_task,
            publish_as_branch,
        ),
        Command::RemoteDisable { workspace_id } => {
            remote::decide_remote_disable(state, workspace_id)
        }
        Command::RemoteOutbox { workspace_id } => {
            remote::decide_remote_outbox(state, workspace_id, now_ms)
        }
        Command::RemotePublished {
            workspace_id,
            sequence,
            signature,
            payload,
            published_at_ms,
            as_branch,
        } => remote::decide_remote_published(
            state,
            workspace_id,
            sequence,
            signature,
            payload,
            published_at_ms,
            as_branch,
        ),
        Command::ObserveRemote {
            workspace_id,
            publication,
        } => remote::decide_observe_remote(state, workspace_id, publication, now_ms),
        Command::RemoteWithdrawn { workspace_id } => {
            remote::decide_remote_withdrawn(state, workspace_id)
        }
        Command::ForgetRemote {
            workspace_id,
            participant,
        } => remote::decide_forget_remote(state, workspace_id, participant),
        // ---- end domain: remote ----
        // ---- domain: checks ----
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
        // ---- end domain: checks ----
        // ---- domain: observation ----
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
        } => observation::decide_observe_disk(
            state,
            workspace_id,
            path,
            before,
            after,
            related_operation_ids,
            request_id,
            now_ms,
        ),
        Command::ObserveBranch {
            workspace_id,
            branch_ref,
            head_oid,
            tree_oid,
        } => observation::decide_observe_branch(
            state,
            workspace_id,
            branch_ref,
            head_oid,
            tree_oid,
            request_id,
            now_ms,
        ),
        Command::ObserveWorkspaceGit {
            workspace_id,
            repository_id,
            branch_ref,
        } => observation::decide_observe_workspace_git(
            state,
            workspace_id,
            repository_id,
            branch_ref,
        ),
        Command::ObserveScan {
            workspace_id,
            failure,
            coverage,
        } => observation::decide_observe_scan(state, workspace_id, failure, coverage, now_ms),
        Command::ObserveHosts { not_installed } => {
            observation::decide_observe_hosts(state, not_installed, now_ms)
        }
        Command::ObserveCheckout {
            workspace_id,
            from_branch,
            to_branch,
            from_head,
            to_head,
            files,
        } => observation::decide_observe_checkout(
            state,
            workspace_id,
            from_branch,
            to_branch,
            from_head,
            to_head,
            files,
            request_id,
            now_ms,
        ),
        Command::ObserveCheckoutRun {
            session_id,
            command,
            to_branch,
            to_head,
            ran_ms,
            finished_ago_ms,
            files_unchanged,
        } => observation::decide_observe_checkout_run(
            state,
            session_id,
            command,
            to_branch,
            to_head,
            ran_ms,
            finished_ago_ms,
            files_unchanged,
            request_id,
            now_ms,
        ),
        Command::ObserveUncoordinated {
            workspace_id,
            files,
            scanned_from_ms,
        } => observation::decide_observe_uncoordinated(
            state,
            workspace_id,
            files,
            scanned_from_ms,
            request_id,
            now_ms,
        ),
        // ---- end domain: observation ----
        // ---- domain: notices ----
        Command::AcknowledgeInterface {
            session_id,
            notice_ids,
        } => notices::decide_acknowledge_interface(state, session_id, notice_ids),
        Command::InterfaceShown {
            session_id,
            notice_ids,
        } => notices::decide_interface_shown(state, session_id, notice_ids),
        Command::OutlineRead {
            session_id,
            path,
            digest,
            ..
        } => notices::decide_outline_read(state, session_id, path, digest),
        Command::BetweenSteps {
            session_id,
            stops,
            prompt_id,
        } => notices::decide_between_steps(state, session_id, stops, prompt_id, now_ms),
        Command::ObserveInterface {
            workspace_id,
            path,
            changes,
            dependents,
        } => notices::decide_observe_interface(
            state,
            workspace_id,
            path,
            changes,
            dependents,
            request_id,
            now_ms,
        ),
        Command::ObserveInterfaceDependents {
            notice_workspace_id,
            notice_id,
            workspace_id,
            paths,
        } => notices::decide_observe_interface_dependents(
            state,
            notice_workspace_id,
            notice_id,
            workspace_id,
            paths,
        ),
        // ---- end domain: notices ----
        // ---- domain: sessions ----
        Command::WorkspaceRegister { workspace_id, root }
        | Command::WorkspaceRegisterVerified {
            workspace_id, root, ..
        } => sessions::decide_workspace_register(state, command, workspace_id, root),
        Command::WorkspaceList { after } => sessions::decide_workspace_list(state, after),
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
        } => sessions::decide_session_register(state, session_id, workspace_id, parent, headless),
        Command::ObserveSessionLiveness {
            session_id,
            process,
            status,
        } => sessions::decide_observe_session_liveness(state, session_id, process, status, now_ms),
        Command::SessionSeen { session_id } => {
            sessions::decide_session_seen(state, session_id, now_ms)
        }
        Command::RecordContext {
            session_id,
            bytes,
            turn,
        } => sessions::decide_record_context(state, session_id, bytes, turn),
        Command::ForgetTold { session_id, keys } => {
            sessions::decide_forget_told(state, session_id, keys)
        }
        Command::IntentSet {
            session_id,
            paths,
            summary,
        } => sessions::decide_intent_set(state, session_id, paths, summary),
        Command::IntentEnd { session_id } => sessions::decide_intent_end(state, session_id),
        // The summary is the adapter's one line; the digests let it learn,
        // without any peer's words, whether a live peer's prompt began the
        // same way, so that it can name this prompt by where it differs.
        Command::SessionPrompt {
            session_id,
            summary,
            line_digests,
        } => sessions::decide_session_prompt(state, session_id, summary, line_digests, now_ms),
        Command::SetPromptTargets { session_id, paths } => {
            sessions::decide_set_prompt_targets(state, session_id, paths, now_ms)
        }
        Command::ObservePresence {
            session_id,
            kind,
            paths,
        } => sessions::decide_observe_presence(state, session_id, kind, paths, now_ms),
        Command::SetPresence {
            session_id,
            shown,
            window_ms,
        } => sessions::decide_set_presence(state, session_id, shown, window_ms),
        Command::SessionEnd { session_id, trail } => {
            sessions::decide_session_end(state, session_id, trail, now_ms)
        }
        Command::SessionStop {
            session_id,
            failure,
            prompt_id,
        } => sessions::decide_session_stop(state, session_id, failure, prompt_id, now_ms),
        Command::Adopt { session_id, from } => adopt(state, session_id, from, now_ms),
        // ---- end domain: sessions ----
        // ---- domain: admission ----
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
        } => admission::decide_admit(
            state, command, session_id, paths, ttl_ms, request_id, now_ms,
        ),
        Command::Complete {
            session_id,
            lease_id,
        } => admission::decide_complete(state, session_id, lease_id, now_ms),
        Command::ObserveToolBatch {
            session_id,
            operation_ids,
            calls,
        } => admission::decide_observe_tool_batch(state, session_id, operation_ids, calls, now_ms),
        Command::ObserveSource {
            session_id,
            files,
            origin,
        } => admission::decide_observe_source(state, session_id, files, origin, now_ms),
        Command::Policy { workspace_id } => admission::decide_policy(state, workspace_id),
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
        } => admission::decide_holders(state, session_id, paths, base, regions, now_ms),
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
        } => admission::decide_record_refusal(
            state,
            session_id,
            kind,
            paths,
            peer_session_id,
            now_ms,
        ),
        // ---- end domain: admission ----
        // ---- domain: briefing ----
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
            let (inflight, inflight_partial, mentioned) = match command {
                Command::BriefPrepared {
                    inflight,
                    inflight_partial,
                    mentioned,
                    ..
                } => {
                    if inflight.len() > INFLIGHT_CANDIDATES {
                        return Err(error(
                            "invalid_limit",
                            "A briefing is offered at most 32 in-flight symbols",
                        ));
                    }
                    (inflight.as_slice(), *inflight_partial, mentioned)
                }
                _ => (&[][..], false, &none),
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
            // Not part of the signature (see `Briefing::inflight_partial`).
            briefing.inflight_partial = inflight_partial;
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
                // That read may still have come after the briefing, from a
                // concurrent hook: a peer's edit after that millisecond makes
                // the file stale again (issue #259).
                let stale_again = |stale: &StaleRead| {
                    now_ms.saturating_sub(stale.age_ms) > since
                        && activity.is_some_and(|activity| {
                            activity
                                .known
                                .iter()
                                .any(|known| known.path == stale.path && known.at_ms >= since)
                        })
                };
                let shown_keys =
                    keep_changes(&mut briefing, &told, &acted, &stale_again, *mid_turn, once);
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
        // ---- end domain: briefing ----
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

/// Replay an event, rejecting invalid references or transitions before mutation.
/// Events carry decisions rather than a clock: expiry ordering is established by
/// the writer calling `expiry_events` before command admission and persistence.
pub fn apply(state: &mut State, event: &Event) -> Result<(), CoreError> {
    match event {
        // ---- domain: messages ----
        Event::Remembered { note, .. } => messages::apply_remembered(state, note)?,
        Event::Forgotten {
            workspace_id,
            note_id,
        } => messages::apply_forgotten(state, workspace_id, note_id)?,
        Event::NoteAnchorChanged {
            workspace_id,
            note_id,
            anchor,
            state: anchor_state,
        } => {
            messages::apply_note_anchor_changed(state, workspace_id, note_id, anchor, anchor_state)?
        }
        Event::MessageSent { message, .. } => messages::apply_message_sent(state, message)?,
        Event::MessageAcknowledged {
            workspace_id,
            message_id,
            session_id,
            ..
        } => messages::apply_message_acknowledged(state, workspace_id, message_id, session_id)?,
        Event::MessageRetracted {
            workspace_id,
            message_id,
            by,
        } => messages::apply_message_retracted(state, workspace_id, message_id, by)?,
        Event::RetractionDelivered {
            session_id,
            message_id,
        } => messages::apply_retraction_delivered(state, session_id, message_id)?,
        // ---- end domain: messages ----
        // ---- domain: remote ----
        Event::RemoteEnabled { link } => remote::apply_remote_enabled(state, link)?,
        Event::RemoteDisabled { workspace_id } => {
            remote::apply_remote_disabled(state, workspace_id)?
        }
        Event::RemotePublished {
            workspace_id,
            sequence,
            signature,
            ..
        } => remote::apply_remote_published(state, workspace_id, sequence, signature)?,
        Event::RemoteObserved { peer } => remote::apply_remote_observed(state, peer)?,
        Event::RemoteWithdrawn { workspace_id } => {
            remote::apply_remote_withdrawn(state, workspace_id)?
        }
        Event::RemoteForgotten {
            workspace_id,
            participant,
        } => remote::apply_remote_forgotten(state, workspace_id, participant)?,
        // ---- end domain: remote ----
        // ---- domain: checks ----
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
        // ---- end domain: checks ----
        // ---- domain: observation ----
        Event::OperationObserved { observation } => {
            observation::apply_operation_observed(state, observation)?
        }
        Event::WorkspaceGitObserved { git } => {
            observation::apply_workspace_git_observed(state, git)?
        }
        Event::ScanFailed { failure } => observation::apply_scan_failed(state, failure)?,
        Event::ScanRecovered { workspace_id } => {
            observation::apply_scan_recovered(state, workspace_id)?
        }
        Event::ScanCoverageObserved {
            workspace_id,
            coverage,
        } => observation::apply_scan_coverage_observed(state, workspace_id, coverage)?,
        Event::HostNotInstalled { host } => observation::apply_host_not_installed(state, host)?,
        Event::HostCleared { host } => observation::apply_host_cleared(state, host)?,
        Event::CheckoutChanged { change } => observation::apply_checkout_changed(state, change)?,
        Event::CheckoutRunObserved { run } => observation::apply_checkout_run_observed(state, run)?,
        Event::UncoordinatedChanged { change } => {
            observation::apply_uncoordinated_changed(state, change)?
        }
        Event::UncoordinatedDelivered {
            workspace_id,
            change_id,
            session_id,
        } => {
            observation::apply_uncoordinated_delivered(state, workspace_id, change_id, session_id)?
        }
        Event::DiskObserved { observation } => {
            observation::apply_disk_observed(state, observation)?
        }
        Event::BranchObserved { observation } => {
            observation::apply_branch_observed(state, observation)?
        }
        // ---- end domain: observation ----
        // ---- domain: notices ----
        Event::InterfaceChanged { notice } => notices::apply_interface_changed(state, notice)?,
        Event::InterfaceDependentsFound {
            notice_workspace_id,
            notice_id,
            files,
        } => {
            notices::apply_interface_dependents_found(state, notice_workspace_id, notice_id, files)?
        }
        Event::InterfaceAlerted {
            session_id,
            notice_ids,
        } => notices::apply_interface_alerted(state, session_id, notice_ids)?,
        Event::InterfaceNoticesTold {
            session_id,
            notice_ids,
            via,
        } => notices::apply_interface_notices_told(state, session_id, notice_ids, via)?,
        Event::ReadOutlined {
            session_id,
            path,
            digest,
        } => notices::apply_read_outlined(state, session_id, path, digest)?,
        Event::InflightDelivered { session_id, keys } => {
            notices::apply_inflight_delivered(state, session_id, keys)?
        }
        // ---- end domain: notices ----
        // ---- domain: sessions ----
        Event::WorkspaceRegistered { workspace } => {
            sessions::apply_workspace_registered(state, workspace)?
        }
        Event::WorkspaceMoved {
            workspace,
            from_root,
        } => workspaces::apply_moved(state, workspace, from_root)?,
        Event::WorkspaceRemoved { workspace_id } => {
            workspaces::apply_removed(state, workspace_id)?;
        }
        Event::SessionRegistered { session } => sessions::apply_session_registered(state, session)?,
        Event::SessionLivenessObserved { liveness } => {
            sessions::apply_session_liveness_observed(state, liveness)?
        }
        Event::SessionSeen {
            session_id,
            at_ms,
            generation,
        } => sessions::apply_session_seen(state, session_id, at_ms, generation)?,
        Event::IntentSet { intent } => sessions::apply_intent_set(state, intent)?,
        Event::IntentEnded { session_id } => sessions::apply_intent_ended(state, session_id)?,
        Event::SessionPrompted {
            session_id,
            summary,
            line_digests,
            at_ms,
            generation,
        } => sessions::apply_session_prompted(
            state,
            session_id,
            summary,
            line_digests,
            at_ms,
            generation,
        )?,
        Event::PromptTargetsSet {
            session_id,
            paths,
            at_ms,
            generation,
        } => sessions::apply_prompt_targets_set(state, session_id, paths, at_ms, generation)?,
        Event::PresenceObserved {
            session_id,
            kind,
            paths,
            at_ms,
            generation,
        } => sessions::apply_presence_observed(state, session_id, kind, paths, at_ms, generation)?,
        Event::PresenceSet {
            session_id,
            setting,
        } => sessions::apply_presence_set(state, session_id, setting)?,
        Event::SessionEnded { session_id } => sessions::apply_session_ended(state, session_id)?,
        Event::SessionAdopted { adoption } => sessions::apply_session_adopted(state, adoption)?,
        // Boundaries in the log. The admissions a stop released ended in
        // their own `LeaseCompleted` events.
        Event::SessionStopped {
            session_id,
            failure,
            generation,
            prompt_id,
            ..
        } => sessions::apply_session_stopped(state, session_id, failure, generation, prompt_id)?,
        Event::ContextAdded {
            session_id,
            bytes,
            turn,
            generation,
        } => sessions::apply_context_added(state, session_id, bytes, turn, generation)?,
        Event::ToldForgotten { session_id, keys } => {
            sessions::apply_told_forgotten(state, session_id, keys)?
        }
        // ---- end domain: sessions ----
        // ---- domain: admission ----
        Event::WriteRefused {
            session_id,
            workspace_id,
            kind,
            paths,
            peer_session_id,
            input_digest,
            at_ms,
            generation,
        } => admission::apply_write_refused(
            state,
            session_id,
            workspace_id,
            kind,
            paths,
            peer_session_id,
            input_digest,
            at_ms,
            generation,
        )?,
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
        Event::LeaseGranted { lease } => admission::apply_lease_granted(state, lease)?,
        Event::ToolBatchObserved {
            session_id,
            operation_ids,
            calls,
            generation,
            ..
        } => admission::apply_tool_batch_observed(
            state,
            session_id,
            operation_ids,
            calls,
            generation,
        )?,
        Event::SourceObserved {
            session_id,
            files,
            origin,
            at_ms,
            generation,
        } => admission::apply_source_observed(state, session_id, files, origin, at_ms, generation)?,
        Event::LeaseCompleted { lease_id } | Event::LeaseExpired { lease_id } => {
            admission::apply_lease_ended(state, event, lease_id)?
        }
        // ---- end domain: admission ----
        // ---- domain: briefing ----
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
        // ---- end domain: briefing ----
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

// ---- shared helpers ----

pub(crate) fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(crate) fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn one() -> u32 {
    1
}

pub(crate) fn default_path_hops() -> u32 {
    DEFAULT_PATH_HOPS
}

pub(crate) fn is_one(value: &u32) -> bool {
    *value == 1
}

/// FNV-1a, 64 bits. It only tells one briefing from the next for the same
/// session; a collision would withhold one repeat briefing and nothing else.
pub(crate) fn fingerprint(text: &str) -> String {
    format!("{:016x}", fingerprint_u64(text))
}

pub(crate) fn fingerprint_u64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One in-flight signature, as remembered once told to a session: a change
/// of signature is a new key, so it is told again.
pub(crate) fn inflight_key(item: &InflightSymbol) -> String {
    fingerprint(&format!(
        "{}\n{}\n{}\n{}\n{}",
        item.workspace_id, item.path, item.kind, item.name, item.signature
    ))
}

/// Whether `session_id` is still owed `message`. A handoff is owed to any
/// other session until one takes it; `handoffs` is false mid-turn.
pub(crate) fn owed_to(message: &Message, session_id: &str, handoffs: bool) -> bool {
    message.from.as_deref() != Some(session_id)
        && !message.acknowledged.iter().any(|held| held == session_id)
        && match &message.to {
            Recipient::Session { session_id: to } => to == session_id,
            Recipient::Everyone {} => true,
            Recipient::NextSession {} => handoffs,
        }
}

pub(crate) fn validate_notice(notice: &InterfaceNotice) -> Result<(), CoreError> {
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

pub(crate) fn validate_known_workspace(state: &State, workspace_id: &str) -> Result<(), CoreError> {
    validate_id(workspace_id, "workspace ID")?;
    if !state.workspaces.contains_key(workspace_id) {
        return Err(error(
            "workspace_not_found",
            "Workspace is not registered; register it first",
        ));
    }
    Ok(())
}

/// Workspaces whose sessions belong to the same repository as `workspace_id`:
/// itself, and any the observer placed in the same repository.
pub(crate) fn related_workspace(state: &State, workspace_id: &str, other: &str) -> bool {
    if workspace_id == other {
        return true;
    }
    let repository = |id: &str| state.workspace_git.get(id).map(|git| &git.repository_id);
    repository(workspace_id).is_some() && repository(workspace_id) == repository(other)
}

pub(crate) fn known_versions<'a>(state: &'a State, session_id: &str) -> &'a [KnownVersion] {
    state
        .activity
        .get(session_id)
        .map(|activity| activity.known.as_slice())
        .unwrap_or(&[])
}

/// The other session in `workspace_id` that most recently reported a
/// successful edit of `path` after `after_ms`, with the time it did.
pub(crate) fn last_peer_edit<'a>(
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

pub(crate) fn normalized_files(files: &[ObservedFile]) -> Result<Vec<ObservedFile>, CoreError> {
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

pub(crate) fn live_lease(state: &State, lease: &Lease, now_ms: u64) -> bool {
    lease.status == LeaseStatus::Active
        && lease.generation == state.generation
        && lease.expires_at_ms > now_ms
}

/// Whether the session wrote a file since its last tool batch or stop.
pub(crate) fn wrote_since_boundary(state: &State, session_id: &str) -> bool {
    state
        .activity
        .get(session_id)
        .is_some_and(|activity| activity.wrote_since_boundary)
}

/// What session `id` works on: the paths its live leases hold, the files it
/// touched within the briefing horizon (with their ages), and those with the
/// paths of its declared intent.
pub(crate) fn work_paths(
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
pub(crate) fn concerned_paths(state: &State, id: &str, now_ms: u64) -> BTreeSet<String> {
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
pub(crate) fn interface_impacts<'a>(
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
pub(crate) fn told_notices<'a>(
    state: &'a State,
    session_id: &str,
) -> impl Iterator<Item = &'a ToldNotice> {
    state
        .activity
        .get(session_id)
        .filter(|activity| activity.generation == state.generation)
        .into_iter()
        .flat_map(|activity| &activity.notices_told)
}

pub(crate) fn validate_prompt_summary(summary: &str) -> Result<(), CoreError> {
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

pub(crate) fn activity_mut<'a>(state: &'a mut State, session_id: &str) -> &'a mut SessionActivity {
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

pub(crate) fn take_all<T: Default>(value: &mut T) -> T {
    core::mem::take(value)
}

/// A session's presence setting: its own, or the default.
pub(crate) fn presence_setting(state: &State, session_id: &str) -> PresenceSetting {
    state.presence.get(session_id).copied().unwrap_or_default()
}

/// Record that a session looked at `paths`: newest first, one entry per path.
pub(crate) fn look(
    activity: &mut SessionActivity,
    kind: PresenceKind,
    paths: &[String],
    at_ms: u64,
) {
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

// ---- end shared helpers ----
