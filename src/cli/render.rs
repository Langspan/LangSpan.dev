//! What the `langspan` command prints.

/// What the last completed scan did not version, in one paragraph. A
/// partial scan is always called partial.
pub fn coverage_text(coverage: &langspan_core::ScanCoverage) -> String {
    let named = |paths: &[String], count: u64| {
        let mut text = paths
            .iter()
            .map(|path| format!("{path:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        if count > paths.len() as u64 {
            text.push_str(&format!(" and {} more", count - paths.len() as u64));
        }
        text
    };
    let mut parts = Vec::new();
    if coverage.omitted > 0 {
        parts.push(format!(
            "{} file(s) left out at the scan's file, byte or time bound (changed files are versioned first)",
            coverage.omitted
        ));
    }
    if coverage.too_large > 0 {
        parts.push(format!(
            "{} file(s) not versioned because they are over {} MiB: {}",
            coverage.too_large,
            crate::observer::Limits::default().max_file_bytes / (1024 * 1024),
            named(&coverage.too_large_paths, coverage.too_large)
        ));
    }
    if coverage.submodules > 0 {
        parts.push(format!(
            "{} submodule(s) present and not read into: {}",
            coverage.submodules,
            named(&coverage.submodule_paths, coverage.submodules)
        ));
    }
    if coverage.skip_worktree > 0 {
        parts.push(format!(
            "{} file(s) outside the sparse checkout, not observed",
            coverage.skip_worktree
        ));
    }
    let lead = if coverage.is_partial() {
        "The last scan was partial: file observations, check staleness marks and the index do not cover the files it did not version."
    } else {
        "The last scan versioned every file in the working tree."
    };
    format!("{lead} {}.", parts.join("; "))
}
