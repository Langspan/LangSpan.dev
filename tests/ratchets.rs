//! The architecture ratchets (docs/development-process.md#ratchets): file
//! size, function length, module dependencies and public surface may not
//! grow past `scripts/ratchets/baseline/`. Needs `python3` 3.9 or later on
//! `PATH`, like the benchmark harness tests.

use std::process::Command;

fn python(args: &[&str]) -> (bool, String) {
    let output = Command::new("python3")
        // -I: ignore the environment and the user's site packages; -B: write
        // no bytecode beside the scripts.
        .args(["-I", "-B"])
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("the ratchet check needs python3 on PATH");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), report)
}

#[test]
fn no_ratchet_grows_past_its_baseline() {
    let (passed, report) = python(&["scripts/ratchets.py", "--check", "."]);
    // The report names what can be tightened even when the check passes.
    eprintln!("{report}");
    assert!(passed, "{report}");
}

#[test]
fn the_ratchet_checker_passes_its_own_tests() {
    let (passed, report) = python(&[
        "-m",
        "unittest",
        "discover",
        "-s",
        "scripts/ratchets",
        "-p",
        "test_*.py",
    ]);
    eprintln!("{report}");
    assert!(passed, "{report}");
    // Python before 3.12 passes a run that found no tests.
    assert!(!report.contains("Ran 0 tests"), "{report}");
}
