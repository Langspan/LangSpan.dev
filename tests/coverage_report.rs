//! The coverage baseline names only files that exist, so a pull request
//! that moves or splits a file carries its rows with it instead of leaving
//! the nightly coverage job to fail on main
//! (docs/development-process.md#coverage).

use std::path::Path;

/// Each `path<TAB>percent` row of scripts/coverage-baseline.tsv whose path
/// is not a file under `root`.
fn rows_without_a_file(root: &Path, baseline: &str) -> Vec<String> {
    baseline
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| line.split('\t').next())
        .filter(|path| !root.join(path).is_file())
        .map(str::to_owned)
        .collect()
}

/// The coverage job fails for each measured file of 100 production lines
/// or more that has no row, and only a coverage run counts production
/// lines. The baseline holds the rows of such a run, so the job fails on a
/// drop, not on a missing row; these files had no row before it.
#[test]
fn the_coverage_baseline_has_rows_for_large_files_outside_the_first_ten() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let baseline = std::fs::read_to_string(root.join("scripts/coverage-baseline.tsv"))
        .expect("scripts/coverage-baseline.tsv is readable");
    let rows: Vec<&str> = baseline
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split('\t').next())
        .collect();
    for file in ["src/daemon/prepare.rs", "src/graph/query.rs"] {
        assert!(
            rows.contains(&file),
            "scripts/coverage-baseline.tsv has no row for {file}; run \
             scripts/coverage.sh and add one (docs/development-process.md#coverage)"
        );
    }
}

#[test]
fn every_coverage_row_names_a_file_that_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let baseline = std::fs::read_to_string(root.join("scripts/coverage-baseline.tsv"))
        .expect("scripts/coverage-baseline.tsv is readable");
    let missing = rows_without_a_file(root, &baseline);
    assert!(
        missing.is_empty(),
        "scripts/coverage-baseline.tsv has rows for files that no longer exist: \
         {missing:?}; move each row to the files its lines went to, in the same \
         pull request (docs/development-process.md#coverage)"
    );
}
