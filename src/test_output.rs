//! Test names read from a check command's output, for learning which tests
//! flip on unchanged code (issue #70, ADR 0030).
//!
//! The runner is recognised by the shape of its lines: `cargo test`
//! (`test NAME ... ok|FAILED`), `go test -v` (`--- PASS: NAME (…)`,
//! `--- FAIL: NAME (…)`) and `pytest` (`FAILED path::name`, and with `-v`
//! or `-rA` its passing tests too). An output with none of these lines names
//! no test, and nothing is learned from it. The output is the command's own
//! text: names are kept only when they are one short line without anything
//! that looks like a secret.

use langspan_core::{MAX_TEST_NAME_BYTES, MAX_TEST_NAMES, MAX_TEST_RESULT_BYTES, TestResults};
use std::collections::BTreeSet;

/// How much output is read for test names. A longer output is not parsed:
/// its start, with tests it ran, is gone.
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// The tests an output named, in the order it named them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Named {
    pub failed: Vec<String>,
    pub passed: Vec<String>,
}

/// The tests `output` names, or `None` when no line of it is a known
/// runner's test line.
pub fn parse(output: &str) -> Option<Named> {
    let mut failed = Vec::new();
    let mut passed = Vec::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if let Some((name, ok)) = cargo(line).or_else(|| go(line)).or_else(|| pytest(line))
            && let Some(name) = usable(name)
        {
            if ok {
                passed.push(name);
            } else {
                failed.push(name);
            }
        }
    }
    if failed.is_empty() && passed.is_empty() {
        return None;
    }
    // A test named as failing anywhere failed; each name once.
    let failing: BTreeSet<String> = failed.iter().cloned().collect();
    let mut seen = BTreeSet::new();
    failed.retain(|name| seen.insert(name.clone()));
    passed.retain(|name| !failing.contains(name) && seen.insert(name.clone()));
    Some(Named { failed, passed })
}

/// The results to record: every failure that fits the bound, then passing
/// tests, those in `prefer` first. `None` when the output contradicts the
/// exit status (a run that passed and names a failure).
pub fn results(named: &Named, all_passed: bool, prefer: &[String]) -> Option<TestResults> {
    if all_passed && !named.failed.is_empty() {
        return None;
    }
    let mut budget = Budget {
        names: MAX_TEST_NAMES,
        bytes: MAX_TEST_RESULT_BYTES,
    };
    let failed: Vec<String> = named
        .failed
        .iter()
        .take_while(|name| budget.take(name))
        .cloned()
        .collect();
    // A run that passed says every test it ran passed: names add nothing.
    let passed = if all_passed {
        Vec::new()
    } else {
        let preferred: BTreeSet<&String> = prefer.iter().collect();
        let (first, rest): (Vec<&String>, Vec<&String>) = named
            .passed
            .iter()
            .partition(|name| preferred.contains(name));
        first
            .into_iter()
            .chain(rest)
            .take_while(|name| budget.take(name))
            .cloned()
            .collect()
    };
    Some(TestResults {
        failed,
        passed,
        all_passed,
    })
}

/// Names and bytes left in one result.
struct Budget {
    names: usize,
    bytes: usize,
}

impl Budget {
    fn take(&mut self, name: &str) -> bool {
        if self.names == 0 || name.len() > self.bytes {
            return false;
        }
        self.names -= 1;
        self.bytes -= name.len();
        true
    }
}

/// The output of a finished run, as far as it can be read: `None` when it
/// was longer than `MAX_OUTPUT_BYTES` and its start was not kept.
pub fn read(bytes: &[u8], cut: bool, all_passed: bool, prefer: &[String]) -> Option<TestResults> {
    if cut {
        return None;
    }
    results(&parse(&String::from_utf8_lossy(bytes))?, all_passed, prefer)
}

/// `test NAME ... ok` or `test NAME ... FAILED` (libtest's pretty format).
fn cargo(line: &str) -> Option<(&str, bool)> {
    let rest = line.strip_prefix("test ")?;
    let (name, status) = rest.split_once(" ... ")?;
    let name = name.strip_suffix(" - should panic").unwrap_or(name);
    match status.split_whitespace().next()? {
        "ok" => Some((name, true)),
        "FAILED" => Some((name, false)),
        _ => None,
    }
}

/// `--- PASS: NAME (0.00s)` or `--- FAIL: NAME (0.00s)`, indented for a
/// subtest.
fn go(line: &str) -> Option<(&str, bool)> {
    let line = line.trim_start();
    let (rest, ok) = if let Some(rest) = line.strip_prefix("--- PASS: ") {
        (rest, true)
    } else {
        (line.strip_prefix("--- FAIL: ")?, false)
    };
    let (name, _) = rest.split_once(" (")?;
    Some((name, ok))
}

/// `FAILED path::name - message`, `ERROR path::name`, `PASSED path::name`
/// (the summary), or `path::name PASSED [ 50%]` (verbose).
fn pytest(line: &str) -> Option<(&str, bool)> {
    for (word, ok) in [("FAILED ", false), ("ERROR ", false), ("PASSED ", true)] {
        if let Some(rest) = line.strip_prefix(word) {
            let name = rest.split_once(" - ").map_or(rest, |(name, _)| name);
            return name.contains("::").then_some((name, ok));
        }
    }
    let (name, rest) = line.split_once(' ')?;
    if !name.contains("::") {
        return None;
    }
    match rest.split_whitespace().next()? {
        "PASSED" => Some((name, true)),
        "FAILED" | "ERROR" => Some((name, false)),
        _ => None,
    }
}

/// A name as it is recorded: trimmed, one line within the bound, and
/// nothing redaction would change.
fn usable(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()
        && name.len() <= MAX_TEST_NAME_BYTES
        && !name.chars().any(char::is_control)
        && !crate::hook::contains_secret(name))
    .then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(failed: &[&str], passed: &[&str]) -> Option<Named> {
        Some(Named {
            failed: failed.iter().map(|name| (*name).to_owned()).collect(),
            passed: passed.iter().map(|name| (*name).to_owned()).collect(),
        })
    }

    #[test]
    fn cargo_test_output_names_its_failing_and_passing_tests() {
        let output = "\
running 4 tests
test parser::keeps_order ... ok
test net::times_out ... FAILED
test slow::one ... ignored
test panics - should panic ... ok
test src/lib.rs - doc (line 3) ... ok

failures:

---- net::times_out stdout ----
thread 'net::times_out' panicked at src/net.rs:9:5:
failures:
    net::times_out

test result: FAILED. 3 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out
";
        assert_eq!(
            parse(output),
            named(
                &["net::times_out"],
                &["parser::keeps_order", "panics", "src/lib.rs - doc (line 3)"]
            )
        );
    }

    #[test]
    fn go_test_and_pytest_output_name_their_tests() {
        let go = "=== RUN   TestOne\n--- PASS: TestOne (0.00s)\n=== RUN   TestTwo\n    --- FAIL: TestTwo/sub_case (0.01s)\n--- FAIL: TestTwo (0.01s)\nFAIL\texample.com/pkg\t0.012s\n";
        assert_eq!(
            parse(go),
            named(&["TestTwo/sub_case", "TestTwo"], &["TestOne"])
        );
        let pytest = "\
tests/test_a.py::test_ok PASSED                                [ 50%]
tests/test_a.py::test_bad FAILED                               [100%]
=========================== short test summary info ============================
FAILED tests/test_a.py::test_bad - assert 1 == 2
ERROR tests/test_b.py::test_setup
FAILED (failures=1)
";
        assert_eq!(
            parse(pytest),
            named(
                &["tests/test_a.py::test_bad", "tests/test_b.py::test_setup"],
                &["tests/test_a.py::test_ok"]
            )
        );
    }

    #[test]
    fn an_unknown_runner_names_nothing_and_a_contradiction_is_not_used() {
        assert_eq!(parse("Ran 3 tests in 0.1s\nFAILED (failures=1)\n"), None);
        assert_eq!(parse(""), None);
        let failing = parse("test a ... FAILED\n").unwrap();
        assert_eq!(results(&failing, true, &[]), None);
        assert_eq!(read(b"test a ... ok\n", true, true, &[]), None, "cut");
    }

    #[test]
    fn results_keep_failures_first_then_preferred_passes_within_the_bound() {
        let mut output = String::new();
        for n in 0..150 {
            output.push_str(&format!("test pass_{n:03} ... ok\n"));
        }
        output.push_str("test broken ... FAILED\n");
        let named = parse(&output).unwrap();
        let prefer = vec!["pass_149".to_owned()];
        let results = results(&named, false, &prefer).unwrap();
        assert_eq!(results.failed, ["broken"]);
        assert_eq!(results.passed.len(), MAX_TEST_NAMES - 1);
        assert_eq!(results.passed[0], "pass_149");
        assert_eq!(results.passed[1], "pass_000");
        // A run that passed names no test: all of them passed.
        let passing = super::results(&parse("test a ... ok\n").unwrap(), true, &[]).unwrap();
        assert!(passing.all_passed && passing.passed.is_empty());
    }

    #[test]
    fn a_name_that_looks_like_a_secret_or_is_too_long_is_left_out() {
        let long = format!("test {} ... FAILED\n", "x".repeat(MAX_TEST_NAME_BYTES + 1));
        assert_eq!(parse(&long), None);
        let secret = "test password=hunter2hunter2 ... FAILED\ntest fine ... FAILED\n";
        assert_eq!(parse(secret), named(&["fine"], &[]));
        // A test that failed and also passed (retried by its runner) failed.
        assert_eq!(
            parse("test a ... ok\ntest a ... FAILED\n"),
            named(&["a"], &[])
        );
    }
}
