//! The Live view's model, `src/dashboard-live.js` (issue #491), is tested
//! by `tests/live-view-model.test.mjs` under `node --test`. This spawns a
//! process only because node is the one runner for the page's script; the
//! model itself is pure, with no DOM, clock or network.

use std::path::Path;
use std::process::Command;

#[test]
fn the_live_view_model_passes_its_node_tests() {
    let required = std::env::var_os("LANGSPAN_REQUIRE_TOOLS").is_some_and(|v| v == "1");
    let found = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    if !found {
        assert!(
            !required,
            "LANGSPAN_REQUIRE_TOOLS=1 needs node to run this test; install it, or unset the variable to skip"
        );
        eprintln!("skipped: node is not available on this machine");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("node")
        .arg("--test")
        .arg(root.join("tests/live-view-model.test.mjs"))
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "node --test failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
