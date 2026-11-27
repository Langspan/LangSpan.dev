use super::*;
use std::fs;

/// Index one worktree's files as a complete scan would.
fn scan(index: &mut crate::graph::Index, workspace_id: &str, root: &std::path::Path) {
    let text = "pub fn load_config() -> u8 { 0 }\n";
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), text).unwrap();
    let scan = crate::graph::ScanView {
        files: [(
            "src/a.rs".to_owned(),
            crate::sha256::hex(&crate::sha256::digest(text.as_bytes())),
        )]
        .into(),
        ..Default::default()
    };
    index
        .update(
            workspace_id,
            root,
            0,
            &scan,
            1_000,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
}

/// Issue #262: while the index has no first scan of the session's
/// worktree or of another worktree of its repository, the briefing
/// is told the comparison was partial instead of finding nothing.
/// The first scan is held back by not running it, so no load or
/// timing decides the result.
#[test]
fn a_worktree_without_a_first_scan_makes_the_briefing_partial() {
    let directory = std::env::temp_dir().join(format!("langspan-inflight-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    let (main, tree) = (directory.join("main"), directory.join("tree"));
    let mut index = crate::graph::Index::open(&directory.join("graph.sqlite3")).unwrap();
    let mut state = State::default();
    for (id, root) in [("main", &main), ("tree", &tree)] {
        state.workspaces.insert(
            id.into(),
            langspan_core::Workspace {
                workspace_id: id.into(),
                root: root.to_str().unwrap().into(),
                verified_overlaps: Vec::new(),
            },
        );
        state.workspace_git.insert(
            id.into(),
            langspan_core::WorkspaceGit {
                workspace_id: id.into(),
                repository_id: "repository".into(),
                branch_ref: None,
            },
        );
    }
    state.sessions.insert(
        "alice".into(),
        langspan_core::Session {
            session_id: "alice".into(),
            workspace_id: "main".into(),
            generation: 0,
            parent: None,
            headless: false,
        },
    );
    let concerned = ["src/a.rs".to_owned()].into();
    let partial = |index: &crate::graph::Index, state: &State| {
        let (found, partial) = brief_inflight(index, state, "alice", "main", &concerned).unwrap();
        assert!(found.is_empty(), "{found:?}");
        partial
    };
    assert!(partial(&index, &state), "neither worktree scanned");
    scan(&mut index, "main", &main);
    assert!(partial(&index, &state), "the other worktree not scanned");
    scan(&mut index, "tree", &tree);
    assert!(!partial(&index, &state), "both scanned");
    // A checkout with no other worktree misses nothing in flight,
    // scanned or not.
    state.workspace_git.remove("tree");
    let solo = crate::graph::Index::open(&directory.join("solo.sqlite3")).unwrap();
    assert!(!partial(&solo, &state));
    drop((index, solo));
    fs::remove_dir_all(directory).unwrap();
}
