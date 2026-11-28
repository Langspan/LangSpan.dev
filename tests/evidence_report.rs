//! Runs the unit tests of `scripts/evidence-report.py`, the person-touches
//! report (#529), on its recorded `gh` output. A child process, since the
//! Python script is what they verify; they make no network call.

use std::process::Command;

#[test]
fn the_evidence_report_passes_its_own_tests() {
    let output = Command::new("python3")
        // -I: ignore the environment and the user's site packages; -B: write
        // no bytecode beside the scripts.
        .args(["-I", "-B", "-m", "unittest", "discover", "-v"])
        .args(["-s", "scripts/evidence-report", "-p", "test_*.py"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("the evidence report's tests need python3 on PATH");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{report}");
    assert!(output.status.success(), "{report}");
    // Python before 3.12 passes a run that found no tests.
    assert!(!report.contains("Ran 0 tests"), "{report}");
}
