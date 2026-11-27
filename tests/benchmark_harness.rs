//! The coordination benchmark harness in `benchmarks/coordination/`, run in
//! its dry-run mode through the `langspan` binary this checkout builds: real
//! Git clones and worktrees, real SQLite, a real coordinator on a Unix
//! socket, and real child processes. No model runs; the agent is the
//! harness's stand-in.
//!
//! The harness is Python (standard library only), so these tests need
//! `python3` 3.9 or later on `PATH`. Each Rust test runs one part of the
//! Python suite, so `cargo test` runs the parts in parallel.
//!
//! The dry runs are the slow part: about 11 minutes of an 18-minute macOS
//! job (issue #286). CI sets `LANGSPAN_BENCH_DRY_RUNS=skip` when a change
//! touches no benchmark, briefing, hook or daemon code, and runs them every
//! night on `main`; `.github/workflows/check.yml` owns the list of paths.
//! Any other value, or none, runs them, so a local check always does.

use std::ffi::OsStr;
use std::io::Write;
use std::process::Command;

const DRY_RUNS: &str = "LANGSPAN_BENCH_DRY_RUNS";

/// Only the exact value `skip` skips, so a misspelt value runs the dry runs.
fn dry_runs_skipped(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("skip"))
}

/// One dry-run part of the Python suite, unless CI said to skip it.
fn dry_run(filter: &str) {
    if dry_runs_skipped(std::env::var_os(DRY_RUNS).as_deref()) {
        // Written to stderr directly: the test harness keeps `eprintln!`
        // output of a passing test, and this line has to reach the CI log.
        let _ = writeln!(
            std::io::stderr(),
            "{filter}: skipped: no benchmark or briefing change ({DRY_RUNS}=skip)"
        );
        return;
    }
    python_suite("test_driver.py", Some(filter));
}

fn python_suite(pattern: &str, filter: Option<&str>) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/benchmarks/coordination");
    let mut command = Command::new("python3");
    // -B: write no bytecode beside the harness.
    command.args(["-B", "-m", "unittest", "discover", "-s", dir, "-p", pattern]);
    if let Some(filter) = filter {
        command.args(["-k", filter]);
    }
    command.env("LANGSPAN_BENCH_LANGSPAN", env!("CARGO_BIN_EXE_langspan"));
    let output = command
        .output()
        .expect("the benchmark harness tests need python3 on PATH");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{report}");
    // A filter that matches nothing passes vacuously: insist something ran.
    assert!(!report.contains("Ran 0 tests"), "{report}");
}

#[test]
fn only_the_exact_value_skip_skips_the_dry_runs() {
    assert!(dry_runs_skipped(Some(OsStr::new("skip"))));
    for value in ["", "0", "1", "true", "SKIP", "skip ", "run"] {
        assert!(!dry_runs_skipped(Some(OsStr::new(value))), "{value:?}");
    }
    assert!(!dry_runs_skipped(None));
}

#[test]
fn runner_mechanics() {
    python_suite("test_runner.py", None);
}

#[test]
fn driver_planning_oracle_and_refusals() {
    python_suite("test_driver.py", Some("DriverUnitTests"));
}

#[test]
fn a_real_refusal_reaches_the_run_record() {
    python_suite("test_driver.py", Some("RefusalCollection"));
}

#[test]
fn standin_hooks() {
    python_suite("test_driver.py", Some("StandinHooks"));
}

#[test]
fn dry_run_worktrees_arm() {
    dry_run("dry_run_arm_worktrees");
}

#[test]
fn dry_run_awareness_only_arm() {
    dry_run("dry_run_arm_awareness_only");
}

#[test]
fn dry_run_langspan_arm_ignoring_briefings() {
    dry_run("dry_run_arm_langspan_ignore");
}

#[test]
fn dry_run_langspan_arm_following_briefings() {
    dry_run("dry_run_arm_langspan_follow");
}

#[test]
fn dry_run_hedging_fails_in_every_arm() {
    dry_run("dry_run_hedging_fails_in_every_arm");
}

#[test]
fn dry_run_a_changer_that_works_around_the_change_is_change_not_made() {
    dry_run("dry_run_a_changer_that_works_around_the_change_is_change_not_made_in_every_arm");
}

#[test]
fn dry_run_with_stagger_tells_dependents_before_their_first_write() {
    dry_run("dry_run_with_stagger_tells_every_dependent_before_its_first_write");
}

#[test]
fn dry_run_records_an_ignored_option() {
    dry_run("a_dry_run_records_an_option_its_arm_ignores");
}

#[test]
fn holder_wait_comparison() {
    python_suite("test_holder_wait.py", None);
}
