//! Test names read from a check command's output, for learning which tests
//! flip on unchanged code (issue #70, ADR 0030).
//!
//! The runner is recognised by the shape of its lines: `cargo test`
//! (`test NAME ... ok|FAILED|ignored`), `go test -v` (`--- PASS: NAME (…)`,
//! `--- FAIL: NAME (…)`, `--- SKIP: NAME (…)`) and `pytest` (`FAILED
//! path::name`, and with `-v` or `-rA` its passing and skipped tests too).
//! A skipped test (ignored, skipped, or pytest's expected failure) is kept
//! as skipped: it did not pass, even in a run that passed as a whole. An
//! output with none of these lines names no test, and nothing is learned
//! from it. The output is the command's own text: names are kept only when
//! they are one short line without anything that looks like a secret.

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
    pub skipped: Vec<String>,
}

/// What one test line says about its test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Passed,
    Failed,
    Skipped,
}

/// The tests `output` names, or `None` when no line of it is a known
/// runner's test line.
pub fn parse(output: &str) -> Option<Named> {
    let mut named = Named::default();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if let Some((name, seen)) = cargo(line).or_else(|| go(line)).or_else(|| pytest(line))
            && let Some(name) = usable(name)
        {
            match seen {
                Seen::Passed => named.passed.push(name),
                Seen::Failed => named.failed.push(name),
                Seen::Skipped => named.skipped.push(name),
            }
        }
    }
    if named.failed.is_empty() && named.passed.is_empty() && named.skipped.is_empty() {
        return None;
    }
    // A test named as failing anywhere failed, and one named as skipped
    // anywhere else is not known to have passed; each name once.
    let mut seen = BTreeSet::new();
    named.failed.retain(|name| seen.insert(name.clone()));
    named.skipped.retain(|name| seen.insert(name.clone()));
    named.passed.retain(|name| seen.insert(name.clone()));
    Some(named)
}

/// The results to record: every failure that fits the bound, then skipped
/// tests, then passing tests, those in `prefer` first. `None` when the
/// output contradicts the exit status (a run that passed and names a
/// failure), or when a run that passed skipped more tests than fit: those
/// left out would read as passed.
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
    let skipped: Vec<String> = named
        .skipped
        .iter()
        .take_while(|name| budget.take(name))
        .cloned()
        .collect();
    if all_passed && skipped.len() < named.skipped.len() {
        return None;
    }
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
        more_failed: failed.len() < named.failed.len(),
        failed,
        passed,
        skipped,
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

/// `test NAME ... ok`, `test NAME ... FAILED` or `test NAME ... ignored`,
/// with `, reason` when the attribute gives one (libtest's pretty format).
fn cargo(line: &str) -> Option<(&str, Seen)> {
    let rest = line.strip_prefix("test ")?;
    let (name, status) = rest.split_once(" ... ")?;
    let name = name.strip_suffix(" - should panic").unwrap_or(name);
    match status.split_whitespace().next()?.trim_end_matches(',') {
        "ok" => Some((name, Seen::Passed)),
        "FAILED" => Some((name, Seen::Failed)),
        "ignored" => Some((name, Seen::Skipped)),
        _ => None,
    }
}

/// `--- PASS: NAME (0.00s)`, `--- FAIL: NAME (0.00s)` or `--- SKIP: NAME
/// (0.00s)`, indented for a subtest.
fn go(line: &str) -> Option<(&str, Seen)> {
    let line = line.trim_start().strip_prefix("--- ")?;
    let (word, rest) = line.split_once(": ")?;
    let seen = match word {
        "PASS" => Seen::Passed,
        "FAIL" => Seen::Failed,
        "SKIP" => Seen::Skipped,
        _ => return None,
    };
    let (name, _) = rest.split_once(" (")?;
    Some((name, seen))
}

/// `FAILED path::name - message`, `ERROR path::name`, `PASSED path::name`,
/// `XFAIL path::name - reason` (the summary), or `path::name PASSED [ 50%]`,
/// `path::name SKIPPED (reason) [ 50%]` (verbose). An expected failure, or
/// an unexpected pass of one, is no plain pass or failure: it is skipped.
/// The summary's `SKIPPED [1] path:line: reason` names no test.
fn pytest(line: &str) -> Option<(&str, Seen)> {
    let word = |word: &str| match word {
        "PASSED" => Some(Seen::Passed),
        "FAILED" | "ERROR" => Some(Seen::Failed),
        "SKIPPED" | "XFAIL" | "XPASS" => Some(Seen::Skipped),
        _ => None,
    };
    let (first, rest) = line.split_once(' ')?;
    if let Some(seen) = word(first) {
        let name = rest.split_once(" - ").map_or(rest, |(name, _)| name);
        return name.contains("::").then_some((name, seen));
    }
    if !first.contains("::") {
        return None;
    }
    Some((first, word(rest.split_whitespace().next()?)?))
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

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn named(failed: &[&str], passed: &[&str]) -> Option<Named> {
        named_skipping(failed, passed, &[])
    }

    fn named_skipping(failed: &[&str], passed: &[&str], skipped: &[&str]) -> Option<Named> {
        Some(Named {
            failed: list(failed),
            passed: list(passed),
            skipped: list(skipped),
        })
    }

    #[test]
    fn a_skipped_test_is_kept_and_a_passing_run_never_says_it_passed() {
        // `cargo test` 1.x on a crate with one failing and two ignored
        // tests, then the same crate with the failing test ignored: output
        // captured from real runs.
        let failing = "\
running 4 tests
test needs_net ... ignored, needs network
test slow_one ... ignored
test passes ... ok
test fails ... FAILED

failures:

---- fails stdout ----

thread 'fails' (161318422) panicked at src/lib.rs:14:5:
boom
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    fails

test result: FAILED. 1 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s
";
        let passing = "\
running 4 tests
test fails ... ignored
test needs_net ... ignored, needs network
test slow_one ... ignored
test passes ... ok

test result: ok. 1 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 0.00s
";
        assert_eq!(
            parse(failing),
            named_skipping(&["fails"], &["passes"], &["needs_net", "slow_one"])
        );
        let rerun = read(passing.as_bytes(), false, true, &list(&["fails"])).unwrap();
        assert!(rerun.all_passed);
        assert_eq!(rerun.skipped, ["fails", "needs_net", "slow_one"]);
        // `pytest -v`, and its `-rA` summary, whose skip line names a
        // location rather than a test.
        let pytest = "\
tests/test_a.py::test_ok PASSED                                          [ 25%]
tests/test_a.py::test_net SKIPPED (needs network)                        [ 50%]
tests/test_a.py::test_known XFAIL (issue 12)                             [ 75%]
tests/test_a.py::test_lucky XPASS                                        [100%]
=========================== short test summary info ============================
PASSED tests/test_a.py::test_ok
SKIPPED [1] tests/test_a.py:7: needs network
XFAIL tests/test_a.py::test_known - issue 12
XPASS tests/test_a.py::test_lucky
=================== 1 passed, 1 skipped, 1 xfailed, 1 xpassed in 0.02s ===================
";
        assert_eq!(
            parse(pytest),
            named_skipping(
                &[],
                &["tests/test_a.py::test_ok"],
                &[
                    "tests/test_a.py::test_net",
                    "tests/test_a.py::test_known",
                    "tests/test_a.py::test_lucky"
                ]
            )
        );
        // `go test -v`, a skipped test and a skipped subtest.
        let go = "\
=== RUN   TestOne
--- PASS: TestOne (0.00s)
=== RUN   TestNet
    net_test.go:9: needs network
--- SKIP: TestNet (0.00s)
=== RUN   TestTable
=== RUN   TestTable/slow
    table_test.go:14: slow
=== RUN   TestTable/fast
--- PASS: TestTable (0.00s)
    --- SKIP: TestTable/slow (0.00s)
    --- PASS: TestTable/fast (0.00s)
PASS
ok  \texample.com/pkg\t0.004s
";
        assert_eq!(
            parse(go),
            named_skipping(
                &[],
                &["TestOne", "TestTable", "TestTable/fast"],
                &["TestNet", "TestTable/slow"]
            )
        );
        // A test skipped and also named as passing is not known to have
        // passed; one that failed anywhere failed.
        assert_eq!(
            parse("test a ... ok\ntest a ... ignored\ntest b ... ignored\ntest b ... FAILED\n"),
            named_skipping(&["b"], &[], &["a"])
        );
        // An output naming only skipped tests still names them.
        assert_eq!(
            parse("test a ... ignored\n"),
            named_skipping(&[], &[], &["a"])
        );
    }

    #[test]
    fn a_passing_run_that_skipped_more_tests_than_fit_is_not_used() {
        let mut output = String::new();
        for n in 0..=MAX_TEST_NAMES {
            output.push_str(&format!("test skip_{n:03} ... ignored\n"));
        }
        let named = parse(&output).unwrap();
        assert_eq!(
            results(&named, true, &[]),
            None,
            "unnamed skips read as passed"
        );
        // A failing run says nothing of the tests it left out.
        let failing = results(&named, false, &[]).unwrap();
        assert_eq!(failing.skipped.len(), MAX_TEST_NAMES);
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
            named_skipping(
                &["net::times_out"],
                &["parser::keeps_order", "panics", "src/lib.rs - doc (line 3)"],
                &["slow::one"]
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
