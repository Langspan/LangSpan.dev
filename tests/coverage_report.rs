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
