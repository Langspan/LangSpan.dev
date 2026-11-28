//! The architecture ratchets (docs/development-process.md#ratchets): file
//! size, function length, module dependencies and public surface may not
//! grow past `scripts/ratchets/baseline/`. Needs `python3` 3.9 or later on
//! `PATH`, like the benchmark harness tests.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
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

/// The most test files outside `tests/support/` that may define their own
/// `fn git(` or `fn hook(` (issue #374). Lower it when the count falls.
const OWN_GIT_OR_HOOK_FILES: usize = 39;

/// The `.rs` files under `dir`, recursively, except `tests/support/`.
fn test_sources(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("read a directory under tests/") {
        let path = entry.expect("read an entry under tests/").path();
        if path.is_dir() {
            if !path.ends_with("tests/support") {
                test_sources(&path, found);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn no_ratchet_grows_past_its_baseline() {
    let (passed, report) = python(&["scripts/ratchets.py", "--check", "."]);
    // The report names what can be tightened even when the check passes.
    eprintln!("{report}");
    assert!(passed, "{report}");

    // Test files with a Git or hook helper of their own, instead of
    // tests/support's `git` and `hook_call`.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    test_sources(&root.join("tests"), &mut sources);
    let mut own: Vec<String> = sources
        .iter()
        // This file names the two in its messages.
        .filter(|path| !path.ends_with("tests/ratchets.rs"))
        .filter(|path| {
            let text = fs::read_to_string(path).expect("read a test file");
            text.contains("fn git(") || text.contains("fn hook(")
        })
        .map(|path| path.strip_prefix(root).unwrap().display().to_string())
        .collect();
    own.sort();
    assert!(
        own.len() <= OWN_GIT_OR_HOOK_FILES,
        "{} test files define their own `fn git(` or `fn hook(`, more than the {OWN_GIT_OR_HOOK_FILES} \
         allowed; use tests/support's `git` or `hook_call` instead: {own:#?}",
        own.len()
    );
    if own.len() < OWN_GIT_OR_HOOK_FILES {
        eprintln!(
            "only {} test files define their own `fn git(` or `fn hook(`: lower \
             OWN_GIT_OR_HOOK_FILES in tests/ratchets.rs to that",
            own.len()
        );
    }
}

/// A second reading of the namespaces, independent of the Python lexer: the
/// files and directories of src/ and the `mod` lines of lib.rs are exactly the
/// `namespace` rows of scripts/ratchets/namespaces.tsv.
#[test]
fn the_top_level_modules_are_the_listed_namespaces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rows = fs::read_to_string(root.join("scripts/ratchets/namespaces.tsv"))
        .expect("read scripts/ratchets/namespaces.tsv");
    let listed: BTreeSet<String> = rows
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some("namespace")).then(|| words.next())?
        })
        .map(str::to_owned)
        .collect();
    let entries: BTreeSet<String> = fs::read_dir(root.join("src"))
        .expect("read src/")
        .map(|entry| entry.expect("read an entry of src/").path())
        .filter(|path| path.is_dir() || path.extension().is_some_and(|ext| ext == "rs"))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
        .filter(|name| name != "lib" && name != "main")
        .collect();
    let lib = fs::read_to_string(root.join("src/lib.rs")).expect("read src/lib.rs");
    let declared: BTreeSet<String> = lib
        .lines()
        .filter_map(|line| {
            let line = line.strip_prefix("pub ").unwrap_or(line);
            line.strip_prefix("mod ")?.strip_suffix(';')
        })
        .map(str::to_owned)
        .collect();
    assert_eq!(
        entries, listed,
        "files and directories of src/ against namespaces.tsv"
    );
    assert_eq!(
        declared, listed,
        "mod lines of src/lib.rs against namespaces.tsv"
    );
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
