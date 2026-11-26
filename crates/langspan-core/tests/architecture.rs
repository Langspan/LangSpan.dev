use std::collections::BTreeSet;

#[test]
fn pure_core_dependencies_point_inward_and_import_no_effect_adapters() {
    let manifest = include_str!("../Cargo.toml");
    let dependencies: BTreeSet<_> = manifest
        .split("[dependencies]")
        .nth(1)
        .expect("dependency section")
        .split('\n')
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .collect();
    assert_eq!(dependencies, BTreeSet::from(["serde"]));
    let dev_dependencies: BTreeSet<_> = manifest
        .split("[dev-dependencies]")
        .nth(1)
        .expect("test dependency section")
        .split('\n')
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .collect();
    assert_eq!(dev_dependencies, BTreeSet::from(["serde_json"]));

    // Keep the production std boundary explicit. An intentional new pure import
    // can extend this list; clocks, files, sockets and root adapters cannot slip
    // into the current boundary with an otherwise routine dependency addition.
    let source = include_str!("../src/lib.rs");
    let allowed_imports = [
        "use std::collections::{BTreeMap, BTreeSet};",
        "use std::fmt;",
        "use serde::{Deserialize, Serialize};",
    ];
    for line in source.lines().map(str::trim) {
        if line.starts_with("use ") {
            assert!(
                allowed_imports.contains(&line),
                "unreviewed core import: {line}"
            );
        }
        if line.contains("std::") && !line.starts_with("use ") {
            assert_eq!(
                line, "impl std::error::Error for CoreError {}",
                "unreviewed fully qualified standard-library access"
            );
        }
        assert!(
            !line.contains("extern crate"),
            "an extern alias bypasses the explicit import boundary"
        );
    }
    // A submodule may import only the same pure standard-library items, serde
    // and its own crate.
    let submodules = [
        include_str!("../src/classes.rs"),
        include_str!("../src/count.rs"),
        include_str!("../src/dashboard.rs"),
        include_str!("../src/flaky.rs"),
        include_str!("../src/history.rs"),
        include_str!("../src/lines.rs"),
        include_str!("../src/policy.rs"),
        include_str!("../src/rank.rs"),
        include_str!("../src/recall.rs"),
        include_str!("../src/recent.rs"),
        include_str!("../src/refusal_outcome.rs"),
        include_str!("../src/report.rs"),
        include_str!("../src/status.rs"),
        include_str!("../src/status_line.rs"),
        include_str!("../src/today.rs"),
        include_str!("../src/workspaces.rs"),
        include_str!("../src/write_check.rs"),
    ];
    // Range bounds, `mem::take` and `iter::empty` for the status pages: pure.
    let allowed_imports = [
        &allowed_imports[..],
        &[
            "use std::collections::BTreeSet;",
            "use std::ops::Bound;",
            "use std::{iter, mem};",
        ],
    ]
    .concat();
    for line in submodules
        .iter()
        .flat_map(|module| module.lines())
        .map(str::trim)
    {
        if line.starts_with("use ") {
            assert!(
                allowed_imports.contains(&line) || line.starts_with("use crate::"),
                "unreviewed core import: {line}"
            );
        }
        assert!(
            !line.contains("std::") || allowed_imports.contains(&line),
            "unreviewed standard-library access in a core submodule: {line}"
        );
        assert!(!line.contains("extern crate"));
    }
    let modules: Vec<_> = source
        .lines()
        .filter(|line| line.trim_start().starts_with("mod "))
        .collect();
    assert_eq!(
        modules,
        [
            "mod classes;",
            "mod count;",
            "mod dashboard;",
            "mod flaky;",
            "mod history;",
            "mod lines;",
            "mod policy;",
            "mod rank;",
            "mod recall;",
            "mod recent;",
            "mod refusal_outcome;",
            "mod report;",
            "mod status;",
            "mod status_line;",
            "mod today;",
            "mod workspaces;",
            "mod write_check;"
        ],
        "a new core module needs this check"
    );
}
