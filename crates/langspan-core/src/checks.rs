//! Checks, landing and push turns. Their record types and rules, and the
//! `decide_checked` and `apply` arms of their commands and events, which
//! call these functions. Learning which tests flip is in `flaky.rs`, and
//! comparing failures with a baseline in `classes.rs`.

// The arm functions take what the arm's pattern bound, as bound (`&String`,
// `&Vec<_>`), so that each body moved out of the match unchanged.
#![allow(clippy::ptr_arg)]

use serde::{Deserialize, Serialize};

use crate::{
    Arity, CoreError, Decision, Event, InterfaceChange, InterfaceNotice, KeptTests, MAX_ARITY,
    MAX_FIXED_NAMED, MAX_TEST_NAME_BYTES, MAX_WRITERS_NAMED, NOTICE_DEPENDENTS, Outcome, State,
    TestFailure, TestResults, classes, concerned_paths, error, flaky, is_false, is_hex_digest,
    is_zero, known_session, validate_id, validate_known_workspace, validate_notice,
    validate_relative_path, validate_request_id,
};

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

pub(crate) fn validate_oid(oid: &str) -> Result<(), CoreError> {
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
pub(crate) fn still_waiting(waiter: &TurnWaiter, generation: u64, now_ms: u64) -> bool {
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_record_check(
    state: &State,
    request_id: &str,
    workspace_id: &String,
    name: &String,
    command: &String,
    tree_oid: &String,
    head_oid: &Option<String>,
    inputs: &Vec<String>,
    result: &CheckResult,
    duration_ms: &u64,
    report: &Option<Box<CheckReport>>,
    tests: &Option<TestResults>,
    rerun: &Option<TestResults>,
    writes_before: &Option<u64>,
    command_id: &Option<String>,
    symbol_findings: &Option<Box<SymbolFindings>>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
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
                class: comparison.and_then(|comparison| comparison.classes.get(at).copied()),
            })
            .collect(),
        more_failures: tests.as_ref().is_some_and(|results| results.more_failed),
        baseline_check_id: comparison.map(|comparison| comparison.baseline_check_id.clone()),
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

pub(crate) fn decide_flaky_tests(
    state: &State,
    workspace_id: &String,
    name: &Option<String>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
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

pub(crate) fn decide_forget_flaky_test(
    state: &State,
    workspace_id: &String,
    name: &Option<String>,
    test: &String,
) -> Result<Decision, CoreError> {
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

pub(crate) fn decide_auto_check_enable(
    state: &State,
    workspace_id: &String,
    name: &String,
    argv: &Vec<String>,
    timeout_s: &u32,
    on_interface_change: &Option<Box<WiderCheck>>,
) -> Result<Decision, CoreError> {
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

pub(crate) fn decide_auto_check_disable(
    state: &State,
    workspace_id: &String,
) -> Result<Decision, CoreError> {
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

pub(crate) fn decide_check_status(
    state: &State,
    workspace_id: &String,
    tree_oid: &Option<String>,
) -> Result<Decision, CoreError> {
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

pub(crate) fn decide_landing_readiness(
    state: &State,
    workspace_id: &String,
    name: &String,
    tree_oid: &String,
) -> Result<Decision, CoreError> {
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_record_landing(
    state: &State,
    request_id: &str,
    workspace_id: &String,
    onto: &String,
    commit_oid: &String,
    tree_oid: &String,
    inputs: &Vec<String>,
    check_id: &String,
    rebased: &Vec<Rewritten>,
    adapted: &Vec<String>,
) -> Result<Decision, CoreError> {
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_take_turn(
    state: &State,
    request_id: &str,
    workspace_id: &String,
    onto: &String,
    session_id: &Option<String>,
    ttl_ms: &u64,
    turn_id: &Option<String>,
    queue: &Option<TurnQueueing>,
    now_ms: u64,
) -> Result<Decision, CoreError> {
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

pub(crate) fn decide_leave_turn_queue(
    state: &State,
    workspace_id: &String,
    onto: &String,
    ticket: &String,
    reason: &LeftQueue,
) -> Result<Decision, CoreError> {
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

pub(crate) fn decide_turn_queue(
    state: &State,
    workspace_id: &String,
    onto: &String,
    now_ms: u64,
) -> Result<Decision, CoreError> {
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

pub(crate) fn decide_release_turn(
    state: &State,
    workspace_id: &String,
    onto: &String,
    turn_id: &String,
    attempts: &Vec<LandingAttempt>,
) -> Result<Decision, CoreError> {
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

pub(crate) fn apply_check_recorded(
    state: &mut State,
    check: &CheckRecord,
    tests: &Option<KeptTests>,
) -> Result<(), CoreError> {
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
    records.retain(|record| !(record.name == check.name && record.tree_oid == check.tree_oid));
    records.insert(0, check.clone());
    records.truncate(MAX_CHECKS);
    // Kept only when no write was counted after the run took its
    // count; an earlier version also stored results whose count had
    // moved, and on replay they are no baseline either.
    let writes = state.writes.get(&check.workspace_id).copied().unwrap_or(0);
    let kept = tests.as_ref().filter(|tests| tests.writes_before == writes);
    classes::keep(state, &check.workspace_id, &check.check_id, kept);
    Ok(())
}

pub(crate) fn apply_auto_check_enabled(
    state: &mut State,
    check: &AutoCheck,
) -> Result<(), CoreError> {
    validate_auto_check(state, check)?;
    state
        .auto_checks
        .insert(check.workspace_id.clone(), check.clone());
    Ok(())
}

pub(crate) fn apply_auto_check_disabled(
    state: &mut State,
    workspace_id: &String,
) -> Result<(), CoreError> {
    if state.auto_checks.remove(workspace_id).is_none() {
        return Err(error(
            "invalid_event",
            "No automatic check was enabled for this workspace",
        ));
    }
    Ok(())
}

pub(crate) fn apply_check_reported(
    state: &mut State,
    event: &Event,
    session_ids: &Vec<String>,
    notice: &CheckNotice,
) -> Result<(), CoreError> {
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
    Ok(())
}

pub(crate) fn apply_check_delivered(
    state: &mut State,
    session_id: &String,
    check_id: &String,
) -> Result<(), CoreError> {
    validate_id(session_id, "session ID")?;
    validate_request_id(check_id)?;
    if let Some(notices) = state.check_notices.get_mut(session_id) {
        notices.retain(|notice| notice.check_id != *check_id);
        if notices.is_empty() {
            state.check_notices.remove(session_id);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_tests_recorded(
    state: &mut State,
    workspace_id: &String,
    check_id: &String,
    name: &String,
    tree_oid: &String,
    command: &String,
    command_id: &String,
    results: &TestResults,
    rerun: &bool,
    at_ms: &u64,
) -> Result<(), CoreError> {
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
    Ok(())
}

pub(crate) fn apply_flaky_test_forgotten(
    state: &mut State,
    workspace_id: &String,
    name: &Option<String>,
    test: &String,
) -> Result<(), CoreError> {
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
    Ok(())
}

pub(crate) fn apply_landed(state: &mut State, landing: &Landing) -> Result<(), CoreError> {
    validate_request_id(&landing.landing_id)?;
    validate_known_workspace(state, &landing.workspace_id)?;
    let landings = state
        .landings
        .entry(landing.workspace_id.clone())
        .or_default();
    landings.insert(0, landing.clone());
    landings.truncate(MAX_LANDINGS);
    Ok(())
}

pub(crate) fn apply_turn_taken(state: &mut State, turn: &Turn) -> Result<(), CoreError> {
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
    Ok(())
}

pub(crate) fn apply_turn_queue_joined(
    state: &mut State,
    waiter: &TurnWaiter,
) -> Result<(), CoreError> {
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
    Ok(())
}

pub(crate) fn apply_turn_queue_seen(
    state: &mut State,
    workspace_id: &String,
    onto: &String,
    ticket: &String,
    seen_until_ms: &u64,
) -> Result<(), CoreError> {
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
    Ok(())
}

pub(crate) fn apply_turn_queue_left(
    state: &mut State,
    workspace_id: &String,
    onto: &String,
    ticket: &String,
) -> Result<(), CoreError> {
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
    Ok(())
}

pub(crate) fn apply_turn_released(
    state: &mut State,
    workspace_id: &String,
    onto: &String,
    turn_id: &String,
    attempts: &Vec<LandingAttempt>,
) -> Result<(), CoreError> {
    validate_attempts(attempts)?;
    let key = turn_key(workspace_id, onto);
    if state
        .turns
        .get(&key)
        .is_some_and(|held| held.turn_id == *turn_id)
    {
        state.turns.remove(&key);
    }
    Ok(())
}

pub(crate) fn apply_turn_renewed(
    state: &mut State,
    workspace_id: &String,
    onto: &String,
    turn_id: &String,
    expires_at_ms: &u64,
) -> Result<(), CoreError> {
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
    Ok(())
}

pub(crate) fn apply_turn_expired(
    state: &mut State,
    workspace_id: &String,
    onto: &String,
    turn_id: &String,
) -> Result<(), CoreError> {
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
    Ok(())
}
