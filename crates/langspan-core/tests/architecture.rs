use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

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
    // and its own crate. Every file under src/ is a submodule, found when the
    // test runs, so a new module is checked without an edit here.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src);
    let submodules: Vec<String> = files
        .iter()
        .filter(|path| **path != src.join("lib.rs"))
        .map(|path| fs::read_to_string(path).expect("read a core source file"))
        .collect();
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
    // Each module the crate declares is one of the scanned files, so none can
    // come from outside src/ (`#[path]`) and escape the import check.
    assert!(
        !source.contains("#[path"),
        "a core module must live under src/"
    );
    for line in source.lines().map(str::trim) {
        let Some(name) = line
            .strip_prefix("pub ")
            .unwrap_or(line)
            .strip_prefix("mod ")
            .and_then(|rest| rest.strip_suffix(';'))
        else {
            continue;
        };
        assert!(
            files.contains(&src.join(format!("{name}.rs")))
                || files.contains(&src.join(name).join("mod.rs")),
            "core module {name} is not a file under src/"
        );
    }
}

/// Every `.rs` file under `dir`, sorted.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).expect("read a core source directory") {
        let path = entry.expect("read a core source directory entry").path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}
