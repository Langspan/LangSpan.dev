//! The coordination benchmark harness in `benchmarks/coordination/`, run in
//! its dry-run mode through the `langspan` binary this checkout builds: real
//! Git worktrees, real SQLite, a real coordinator on a Unix socket and real
//! child processes. No model runs; the agent is the harness's stand-in.
//!
//! The harness is Python (standard library only), so these tests need
//! `python3` 3.9 or later on `PATH`. Each Rust test runs one part of the
//! Python suite, so `cargo test` runs the parts in parallel.

use std::process::Command;

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
fn dry_run_worktrees_arm() {
    python_suite("test_driver.py", Some("dry_run_arm_worktrees"));
}

#[test]
fn dry_run_awareness_only_arm() {
    python_suite("test_driver.py", Some("dry_run_arm_awareness_only"));
}

#[test]
fn dry_run_langspan_arm() {
    python_suite("test_driver.py", Some("dry_run_arm_langspan"));
}
