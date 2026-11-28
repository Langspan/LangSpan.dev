//! The Live view's model, `src/dashboard-live.js` (issue #491), is tested
//! by `tests/live-view-model.test.mjs` under `node --test`. This spawns a
//! process only because node is the one runner for the page's script; the
//! model itself is pure, with no DOM, clock or network.

use std::path::Path;
use std::process::Command;

/// The model draws a step as an edit only if its tool is in `EDIT_TOOLS`;
/// a host tool missing there draws as a command and never saves a hold.
#[test]
fn the_live_view_names_every_adapters_edit_tools() {
    let source = include_str!("../src/dashboard-live.js");
    let start = source.find("const EDIT_TOOLS = new Set([").unwrap();
    let list = &source[start..start + source[start..].find("]);").unwrap()];
    let cursor = langspan::cursor::ADMITTED_TOOLS
        .into_iter()
        .filter(|t| *t != langspan::cursor::SHELL_TOOL);
    let tools = langspan::claude_code::SUPPORTED_TOOLS
        .into_iter()
        .chain(langspan::antigravity::EDIT_TOOLS)
        .chain(cursor)
        .chain(["apply_patch"]);
    for tool in tools {
        assert!(
            list.contains(&format!("\"{tool}\"")),
            "src/dashboard-live.js EDIT_TOOLS lacks {tool}; add it so its edits draw as edits"
        );
    }
}

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
