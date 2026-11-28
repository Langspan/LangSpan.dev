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
    assert_eq!(dev_dependencies, BTreeSet::from(["proptest", "serde_json"]));

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

/// The names at the crate root are its public interface: `pub` items of lib.rs
/// and what its `pub use` lines re-export, through `*` from a module's own
/// top-level `pub` items. A change shows as a diff of the golden list, which
/// `LANGSPAN_UPDATE_GOLDEN=1` rewrites.
#[test]
fn the_root_names_of_the_core_match_their_golden_list() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let golden = crate_dir.join("../../scripts/ratchets/baseline/crates/langspan-core/api.txt");
    let lib = fs::read_to_string(crate_dir.join("src/lib.rs")).expect("read the core lib.rs");
    let mut names = BTreeSet::new();
    for item in root_items(&lib) {
        let Some(used) = item.strip_prefix("pub use ") else {
            names.insert(item_name(&item).expect("a named pub item").to_owned());
            continue;
        };
        if let Some(module) = used.strip_suffix("::*;") {
            let path = module_file(&crate_dir.join("src"), module);
            let text = fs::read_to_string(&path).expect("read a re-exported core module");
            names.extend(
                root_items(&text)
                    .iter()
                    .filter_map(|item| item_name(item))
                    .map(str::to_owned),
            );
            continue;
        }
        let list = used.split_once('{').map_or(used, |(_, list)| list);
        for part in list.trim_end_matches([';', '}']).split(',') {
            let part = part.trim();
            let name = part.rsplit(" as ").next().expect("a re-export");
            let name = name.rsplit("::").next().expect("a re-export");
            if !name.is_empty() {
                names.insert(name.to_owned());
            }
        }
    }
    let actual: String = names.iter().map(|name| format!("{name}\n")).collect();
    if std::env::var_os("LANGSPAN_UPDATE_GOLDEN").is_some() {
        fs::write(&golden, &actual).expect("write the core API golden");
    }
    let expected = fs::read_to_string(&golden).unwrap_or_default();
    assert!(
        expected == actual,
        "the core's root names changed; review the change to its public interface, then \
         rerun with LANGSPAN_UPDATE_GOLDEN=1 to rewrite {}.\nadded: {:?}\nremoved: {:?}",
        golden.display(),
        names
            .iter()
            .filter(|name| !expected.lines().any(|line| line == name.as_str()))
            .collect::<Vec<_>>(),
        expected
            .lines()
            .filter(|line| !names.contains(*line))
            .collect::<Vec<_>>(),
    );
}

/// The file of the root module `module` under `src`: `module.rs`, or
/// `module/mod.rs` for a directory module.
fn module_file(src: &Path, module: &str) -> PathBuf {
    let file = src.join(format!("{module}.rs"));
    if file.is_file() {
        file
    } else {
        src.join(module).join("mod.rs")
    }
}

/// Touches the file system because a directory module is a layout on disk;
/// the directory is this test's own and is removed after.
#[test]
fn a_glob_re_export_resolves_a_directory_module_too() {
    let src = std::env::temp_dir().join(format!("langspan-core-arch-{}", std::process::id()));
    fs::create_dir_all(src.join("nested")).expect("make a scratch source tree");
    fs::write(src.join("flat.rs"), "pub struct Flat;\n").expect("write flat.rs");
    fs::write(src.join("nested/mod.rs"), "pub struct Nested;\n").expect("write nested/mod.rs");
    let names: Vec<_> = ["flat", "nested"]
        .iter()
        .map(|module| fs::read_to_string(module_file(&src, module)).expect("read the module"))
        .collect();
    fs::remove_dir_all(&src).expect("remove the scratch source tree");
    assert_eq!(names, ["pub struct Flat;\n", "pub struct Nested;\n"]);
}

/// The top-level `pub` items of a rustfmt-formatted file, each joined onto one
/// line: they start in the first column, and a `pub use` ends at its `;`.
fn root_items(text: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        if open {
            let last = items.last_mut().expect("an open pub use");
            last.push_str(line.trim());
            open = !line.ends_with(';');
        } else if line.starts_with("pub ") {
            items.push(line.to_owned());
            open = line.starts_with("pub use ") && !line.ends_with(';');
        }
    }
    items
}

/// The name a `pub` item declares, such as `State` in `pub struct State {`.
fn item_name(item: &str) -> Option<&str> {
    let mut words = item.split_whitespace().skip(1);
    words.find(|word| {
        matches!(
            *word,
            "fn" | "struct" | "enum" | "trait" | "type" | "const" | "static" | "mod" | "union"
        )
    })?;
    let mut name = words.next()?;
    // `pub const fn name` names the function, not a constant called `fn`.
    if name == "fn" {
        name = words.next()?;
    }
    name.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
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
