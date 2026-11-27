//! Golden files: output a test pins byte for byte, kept under
//! `tests/golden/`. To change one on purpose, run the test with
//! `LANGSPAN_UPDATE_GOLDEN=1`, which rewrites the file from what the code
//! prints now, then read `git diff tests/golden` before committing it: the
//! diff is the change users will see. A mismatch also writes the whole
//! output to `target/golden-actual/`, to diff or copy over the golden file.

use std::path::{Path, PathBuf};

/// The environment variable that rewrites golden files instead of
/// comparing them.
pub const UPDATE: &str = "LANGSPAN_UPDATE_GOLDEN";

fn path_of(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Compares `actual` with the golden file `name`, byte for byte. A
/// mismatch names the first line that differs and how to refresh the file.
pub fn check(name: &str, actual: &str) {
    let path = path_of(name);
    if std::env::var_os(UPDATE).is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).ok();
    if expected.as_deref() == Some(actual) {
        return;
    }
    let expected = expected.unwrap_or_default();
    // The whole output, to diff against the golden file.
    let written = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/golden-actual")
        .join(name);
    std::fs::create_dir_all(written.parent().unwrap()).unwrap();
    std::fs::write(&written, actual).unwrap();
    let mut expected_lines = expected.split_inclusive('\n');
    let mut actual_lines = actual.split_inclusive('\n');
    let mut line = 1;
    loop {
        match (expected_lines.next(), actual_lines.next()) {
            (Some(left), Some(right)) if left == right => line += 1,
            (left, right) => panic!(
                "output differs from tests/golden/{name} (missing or empty if it is new) at line {line}:\n  golden: {left:?}\n  now:    {right:?}\nThe whole output is in target/golden-actual/{name}. If the change is intended, run this test with {UPDATE}=1 and review `git diff tests/golden`."
            ),
        }
    }
}
