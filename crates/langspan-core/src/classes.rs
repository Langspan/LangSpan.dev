//! Which tests a write broke (issue #69,
//! [ADR 0030](../../../docs/adr/0030-automatic-checks-after-write-batches.md)).
//!
//! An automatic check's failing tests are compared with its session's
//! baseline: the newest result of the same check (name and command identity),
//! with per-test results, whose tree was taken before the session's first
//! write since it registered. A failing test that passed there is `new`, one
//! that failed there is `already_failing`, and one it did not name is
//! `unknown_before`; a test that failed there and passes now is `fixed`. The
//! sessions other than the receiving one that wrote between the two trees are
//! named. Every input is a recorded event, so the answer is the same on
//! replay. Nothing here changes a record, its result or what a landing
//! accepts.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{State, TestResults};

/// Runs of consecutive writes kept per workspace, for naming who wrote
/// between two trees. A writer whose writes all fell out of this window is
/// not named.
pub const MAX_WRITE_RUNS: usize = 256;
/// Fixed tests one notice names.
pub const MAX_FIXED_NAMED: usize = 20;
/// Other sessions one notice names as writers.
pub const MAX_WRITERS_NAMED: usize = 8;

/// How one failing test compares with the session's baseline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Passed in the baseline, fails now.
    New,
    /// Failed in the baseline too.
    AlreadyFailing,
    /// The baseline did not name it, or skipped it.
    UnknownBefore,
}

/// A record's per-test results, kept so that a later run can be compared
/// with them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeptTests {
    pub results: TestResults,
    /// The workspace's write count taken before the run's tree.
    pub writes_before: u64,
    /// The identity of its command (`record_check.command_id`): a record's
    /// `command` is only a shortened summary two commands can share.
    pub command_id: String,
}

/// Writes `first..=last` of a workspace's write count, all by one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRun {
    pub session_id: String,
    pub first: u64,
    pub last: u64,
}

/// What one session is told about a run beyond its result.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Comparison {
    pub baseline_check_id: String,
    /// One per failing test of the run, in its order.
    pub classes: Vec<FailureClass>,
    pub fixed: Vec<String>,
    pub writers: Vec<String>,
}

/// A write was recorded in `workspace_id`, by `session_id` when a session's
/// tool made it: the count moves on, the session's first write is noted, and
/// the write joins its run.
pub(crate) fn wrote(state: &mut State, workspace_id: &str, session_id: Option<&str>) {
    let writes = state.writes.entry(workspace_id.to_owned()).or_default();
    let before = *writes;
    *writes = writes.saturating_add(1);
    let count = *writes;
    let Some(session_id) = session_id else {
        return;
    };
    state
        .first_writes
        .entry(session_id.to_owned())
        .or_insert(before);
    let runs = state.write_runs.entry(workspace_id.to_owned()).or_default();
    match runs.last_mut() {
        Some(run) if run.session_id == session_id && run.last.saturating_add(1) == count => {
            run.last = count;
        }
        _ => {
            runs.push(WriteRun {
                session_id: session_id.to_owned(),
                first: count,
                last: count,
            });
            if runs.len() > MAX_WRITE_RUNS {
                runs.remove(0);
            }
        }
    }
}

/// Keep `kept` for record `check_id`, and forget results of records the
/// workspace no longer holds.
pub(crate) fn keep(
    state: &mut State,
    workspace_id: &str,
    check_id: &str,
    kept: Option<&KeptTests>,
) {
    let held: BTreeSet<&str> = state
        .checks
        .get(workspace_id)
        .into_iter()
        .flatten()
        .map(|record| record.check_id.as_str())
        .collect();
    let tests = state.kept_tests.entry(workspace_id.to_owned()).or_default();
    if let Some(kept) = kept {
        tests.insert(check_id.to_owned(), kept.clone());
    }
    tests.retain(|id, _| held.contains(id.as_str()));
    if tests.is_empty() {
        state.kept_tests.remove(workspace_id);
    }
}

/// Compare a run of check `name`, whose results and tree's write count are
/// `now`, with `session_id`'s baseline. `None` when the session has no
/// recorded write or no baseline exists.
pub(crate) fn compare(
    state: &State,
    workspace_id: &str,
    name: &str,
    now: &KeptTests,
    session_id: &str,
) -> Option<Comparison> {
    let (writes_before, command_id) = (now.writes_before, &now.command_id);
    let now = &now.results;
    let first = *state.first_writes.get(session_id)?;
    let kept = state.kept_tests.get(workspace_id)?;
    let (baseline_check_id, baseline) = state
        .checks
        .get(workspace_id)?
        .iter()
        .filter(|record| record.name == name && record.inputs.is_empty())
        .find_map(|record| {
            kept.get(&record.check_id)
                .filter(|tests| tests.writes_before <= first && tests.command_id == *command_id)
                .map(|tests| (record.check_id.clone(), tests))
        })?;
    let classes = now
        .failed
        .iter()
        .map(|test| match baseline.results.passed(test) {
            Some(true) => FailureClass::New,
            Some(false) => FailureClass::AlreadyFailing,
            None => FailureClass::UnknownBefore,
        })
        .collect();
    let fixed = baseline
        .results
        .failed
        .iter()
        .filter(|test| now.passed(test) == Some(true))
        .take(MAX_FIXED_NAMED)
        .cloned()
        .collect();
    // Writes counted after the baseline took its tree and no later than
    // this run took its own.
    let since = baseline.writes_before;
    let writers: BTreeSet<&String> = state
        .write_runs
        .get(workspace_id)
        .into_iter()
        .flatten()
        .filter(|run| {
            run.session_id != session_id && run.last > since && run.first <= writes_before
        })
        .map(|run| &run.session_id)
        .collect();
    Some(Comparison {
        baseline_check_id,
        classes,
        fixed,
        writers: writers
            .into_iter()
            .take(MAX_WRITERS_NAMED)
            .cloned()
            .collect(),
    })
}

/// Group `sessions` by what each is told, in the order each group's first
/// session appears.
pub(crate) fn group(
    sessions: &[String],
    mut told: impl FnMut(&str) -> Option<Comparison>,
) -> Vec<(Option<Comparison>, Vec<String>)> {
    let mut groups: Vec<(Option<Comparison>, Vec<String>)> = Vec::new();
    let mut index = BTreeMap::<Option<Comparison>, usize>::new();
    for session in sessions {
        let comparison = told(session);
        match index.get(&comparison) {
            Some(&at) => groups[at].1.push(session.clone()),
            None => {
                index.insert(comparison.clone(), groups.len());
                groups.push((comparison, vec![session.clone()]));
            }
        }
    }
    groups
}
