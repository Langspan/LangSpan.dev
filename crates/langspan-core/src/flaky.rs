//! Tests that flip on unchanged code (issue #70,
//! [ADR 0030](../../../docs/adr/0030-automatic-checks-after-write-batches.md)).
//!
//! Two results of one check (same name and command identity) on one tree,
//! with no
//! recorded write in the workspace between them, that disagree about a test
//! are one flip of that test. A test that flipped at least
//! [`FLAKY_MIN_FLIPS`] times in its last [`FLAKY_WINDOW_RUNS`] such runs is
//! known to flip; one with no flip in its last [`FLAKY_DECAY_RUNS`] is
//! forgotten. Only test names are kept, never output, and nothing here
//! changes a check's result or what a landing accepts.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{CoreError, error, is_false};

/// Test names in one result, and bytes in one name.
pub const MAX_TEST_NAMES: usize = 100;
pub const MAX_TEST_NAME_BYTES: usize = 200;
/// Bytes of all the names in one result, so that a record with a rerun's
/// results fits one request frame however its names are escaped.
pub const MAX_TEST_RESULT_BYTES: usize = 10 * 1024;
/// A test is known to flip after this many flips in its last
/// `FLAKY_WINDOW_RUNS` runs on unchanged trees.
pub const FLAKY_MIN_FLIPS: usize = 2;
pub const FLAKY_WINDOW_RUNS: usize = 20;
/// A test with no flip in this many runs is forgotten.
pub const FLAKY_DECAY_RUNS: usize = 50;
/// Tests followed per workspace, and check names whose last result is kept.
pub const MAX_LEARNED_TESTS: usize = 500;
pub const MAX_LEARNED_CHECKS: usize = 16;
/// Known-flaky tests one `check flaky` answer lists.
pub const MAX_FLAKY_LISTED: usize = 100;

/// Which tests one run of a check failed and passed, read from its output.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestResults {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
    /// Passing tests the output named, as many as fit the bound.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passed: Vec<String>,
    /// Tests the output named as skipped (ignored, or not run): neither a
    /// pass nor a failure, so never compared. A run that passed as a whole
    /// names every one it skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// The run passed as a whole (exit status 0): every test it ran passed,
    /// named here or not. A skipped test did not run.
    #[serde(default, skip_serializing_if = "is_false")]
    pub all_passed: bool,
    /// The output named more failing tests than fit the bound: `failed`
    /// lists only the first of them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub more_failed: bool,
}

impl TestResults {
    /// Say nothing about `test` from now on, as if the run had skipped it.
    /// Only a forgotten test is added, and each record replaces the result,
    /// so this grows it by at most the tests followed.
    fn unknown(&mut self, test: &str) {
        self.failed.retain(|name| name != test);
        self.passed.retain(|name| name != test);
        if !self.skipped.iter().any(|name| name == test) {
            self.skipped.push(test.to_owned());
        }
    }

    /// Whether the run passed `test`: `None` when it says nothing about it,
    /// or skipped it.
    pub(crate) fn passed(&self, test: &str) -> Option<bool> {
        if self.failed.iter().any(|name| name == test) {
            Some(false)
        } else if self.skipped.iter().any(|name| name == test) {
            None
        } else if self.all_passed || self.passed.iter().any(|name| name == test) {
            Some(true)
        } else {
            None
        }
    }
}

/// The last per-test result of one check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LastTests {
    pub tree_oid: String,
    /// The identity of its command (`record_check.command_id`).
    pub command_id: String,
    /// The workspace's write generation during its run: a later result is
    /// compared with it only at the same generation, with no write between.
    #[serde(default)]
    pub writes: u64,
    pub results: TestResults,
}

/// One test's runs on unchanged trees since its first flip.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestHistory {
    /// Oldest first, at most `FLAKY_DECAY_RUNS`: `true` where it flipped.
    pub runs: Vec<bool>,
    /// Known to flip: set by the flip threshold, kept until it decays.
    #[serde(default, skip_serializing_if = "is_false")]
    pub flaky: bool,
    /// Wall-clock time of its newest flip, for an age shown to people.
    pub last_flip_ms: u64,
}

impl TestHistory {
    fn flips(&self) -> usize {
        self.runs.iter().filter(|flipped| **flipped).count()
    }
}

/// What one workspace has learned, local to this machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestLearning {
    /// Per check name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub last: BTreeMap<String, LastTests>,
    /// Per check name, then test name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tests: BTreeMap<String, BTreeMap<String, TestHistory>>,
}

/// A known-flaky test's counts, as a failure of it is labelled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FlakyCount {
    pub flips: u32,
    pub runs: u32,
}

/// One failing test of a reported check result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestFailure {
    pub test: String,
    /// Set when the test is known to flip on unchanged code. The failure is
    /// still a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaky: Option<FlakyCount>,
    /// How it compares with the session's baseline (issue #69); `None` when
    /// no baseline was compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<crate::FailureClass>,
}

/// One line of `check flaky`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FlakyTest {
    pub test: String,
    pub check_name: String,
    pub runs: u32,
    pub flips: u32,
    pub last_flip_age_ms: u64,
}

/// A command identity: 64 lowercase hex digits, as a SHA-256 of the
/// argument vector is written.
pub(crate) fn is_command_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn validate_test_results(results: &TestResults, passed: bool) -> Result<(), CoreError> {
    let names = || {
        results
            .failed
            .iter()
            .chain(&results.passed)
            .chain(&results.skipped)
    };
    let count = names().count();
    let bytes: usize = names().map(String::len).sum();
    let mut seen = BTreeSet::new();
    let names_ok = names().all(|name| {
        !name.is_empty()
            && name.len() <= MAX_TEST_NAME_BYTES
            && name.trim() == name
            && !name.chars().any(char::is_control)
            && seen.insert(name.as_str())
    });
    if count > MAX_TEST_NAMES || bytes > MAX_TEST_RESULT_BYTES || !names_ok {
        return Err(error(
            "invalid_test_results",
            &format!(
                "Test results name at most {MAX_TEST_NAMES} distinct tests in {MAX_TEST_RESULT_BYTES} bytes, each 1 to {MAX_TEST_NAME_BYTES} bytes on one line without surrounding spaces"
            ),
        ));
    }
    if results.all_passed != passed
        || (results.all_passed && (!results.failed.is_empty() || results.more_failed))
    {
        return Err(error(
            "invalid_test_results",
            "Test results pass as a whole exactly when their run passed, and then name no failure",
        ));
    }
    Ok(())
}

impl TestLearning {
    /// Learn from one result of check `name`, taken with no write recorded
    /// during its run, at write generation `writes`; compared with the last
    /// one when that was on the same tree, of the command with the same
    /// identity, at the same generation.
    pub(crate) fn learn(
        &mut self,
        name: &str,
        tree_oid: &str,
        command_id: &str,
        writes: u64,
        results: &TestResults,
        now_ms: u64,
    ) {
        if let Some(last) = self.last.get(name)
            && last.tree_oid == tree_oid
            && last.command_id == command_id
            && last.writes == writes
        {
            let tracked = self.tests.entry(name.to_owned()).or_default();
            let candidates: BTreeSet<String> = last
                .results
                .failed
                .iter()
                .chain(&results.failed)
                .chain(tracked.keys())
                .cloned()
                .collect();
            for test in candidates {
                let (Some(before), Some(now)) = (last.results.passed(&test), results.passed(&test))
                else {
                    continue;
                };
                let flipped = before != now;
                if !flipped && !tracked.contains_key(&test) {
                    continue;
                }
                let history = tracked.entry(test.clone()).or_default();
                history.runs.push(flipped);
                if history.runs.len() > FLAKY_DECAY_RUNS {
                    history.runs.remove(0);
                }
                if flipped {
                    history.last_flip_ms = now_ms;
                }
                let recent = history
                    .runs
                    .iter()
                    .rev()
                    .take(FLAKY_WINDOW_RUNS)
                    .filter(|flipped| **flipped)
                    .count();
                if recent >= FLAKY_MIN_FLIPS {
                    history.flaky = true;
                }
                if history.runs.len() >= FLAKY_DECAY_RUNS && history.flips() == 0 {
                    tracked.remove(&test);
                }
            }
            if tracked.is_empty() {
                self.tests.remove(name);
            }
            self.bound();
        }
        if !self.last.contains_key(name) && self.last.len() >= MAX_LEARNED_CHECKS {
            let other = self.last.keys().next().cloned();
            if let Some(other) = other {
                self.last.remove(&other);
            }
        }
        self.last.insert(
            name.to_owned(),
            LastTests {
                tree_oid: tree_oid.to_owned(),
                command_id: command_id.to_owned(),
                writes,
                results: results.clone(),
            },
        );
    }

    /// At most `MAX_LEARNED_TESTS` followed: the ones whose newest flip is
    /// oldest go first.
    fn bound(&mut self) {
        loop {
            let count: usize = self.tests.values().map(BTreeMap::len).sum();
            if count <= MAX_LEARNED_TESTS {
                return;
            }
            let oldest = self
                .tests
                .iter()
                .flat_map(|(check, tests)| {
                    tests
                        .iter()
                        .map(move |(test, history)| (history.last_flip_ms, check, test))
                })
                .min()
                .map(|(_, check, test)| (check.clone(), test.clone()));
            let Some((check, test)) = oldest else { return };
            if let Some(tests) = self.tests.get_mut(&check) {
                tests.remove(&test);
                if tests.is_empty() {
                    self.tests.remove(&check);
                }
            }
        }
    }

    /// The label for a failure of `test` in check `name`.
    pub(crate) fn label(&self, name: &str, test: &str) -> Option<FlakyCount> {
        let history = self.tests.get(name)?.get(test)?;
        history.flaky.then(|| FlakyCount {
            flips: history.flips() as u32,
            runs: history.runs.len() as u32,
        })
    }

    /// Known-flaky tests, of check `name` or of every check, newest flip
    /// first, at most `MAX_FLAKY_LISTED`; and whether more were left out.
    pub(crate) fn flaky(&self, name: Option<&str>, now_ms: u64) -> (Vec<FlakyTest>, bool) {
        let mut listed: Vec<FlakyTest> = self
            .tests
            .iter()
            .filter(|(check, _)| name.is_none_or(|name| name == check.as_str()))
            .flat_map(|(check, tests)| {
                tests
                    .iter()
                    .filter(|(_, history)| history.flaky)
                    .map(move |(test, history)| FlakyTest {
                        test: test.clone(),
                        check_name: check.clone(),
                        runs: history.runs.len() as u32,
                        flips: history.flips() as u32,
                        last_flip_age_ms: now_ms.saturating_sub(history.last_flip_ms),
                    })
            })
            .collect();
        listed.sort_by(|a, b| {
            (a.last_flip_age_ms, &a.check_name, &a.test).cmp(&(
                b.last_flip_age_ms,
                &b.check_name,
                &b.test,
            ))
        });
        let more = listed.len() > MAX_FLAKY_LISTED;
        listed.truncate(MAX_FLAKY_LISTED);
        (listed, more)
    }

    /// Forget `test`, in check `name` or in every check: whether it was
    /// followed. Its result in each such check's last result is forgotten
    /// too, so its next result is compared with nothing from before.
    pub(crate) fn forget(&mut self, name: Option<&str>, test: &str) -> bool {
        let matches = |check: &str| name.is_none_or(|name| name == check);
        let mut forgotten = false;
        self.tests.retain(|check, tests| {
            if matches(check) {
                forgotten |= tests.remove(test).is_some();
            }
            !tests.is_empty()
        });
        for (_, last) in self.last.iter_mut().filter(|(check, _)| matches(check)) {
            last.results.unknown(test);
        }
        forgotten
    }

    pub(crate) fn follows(&self, name: Option<&str>, test: &str) -> bool {
        self.tests
            .iter()
            .any(|(check, tests)| name.is_none_or(|name| name == check) && tests.contains_key(test))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.last.is_empty() && self.tests.is_empty()
    }
}
