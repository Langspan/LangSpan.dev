//! A rename in one worktree of a repository, seen from another (issue
//! #194): a write check judges the change in the worktree that made it, and
//! the notice keeps the other worktrees' files that mention the old name.
use langspan_core::{
    CallSite, Command, Event, IndexedNames, InterfaceChange, InterfaceNotice, NOTICE_DEPENDENTS,
    Outcome, State, SymbolRef, WorktreeFile, apply, call_breaks, call_candidates, decide,
};
use std::collections::BTreeMap;

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let decision = decide(state, &command, id, now);
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

fn ok(outcome: Outcome) -> Outcome {
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    outcome
}

fn code(outcome: Outcome) -> String {
    match outcome {
        Outcome::Error { code, .. } => code,
        other => panic!("not refused: {other:?}"),
    }
}

/// Two worktrees of one repository, `main` and `tree`, and `other` in
/// another repository. Alice works in `tree`, Bob in `main`, Dave in
/// `other`. Alice renamed `parse_config` to `load_config` in `tree`.
fn renamed() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (id, root, repository) in [
        ("main", "/repos/app", "repo-app"),
        ("tree", "/repos/app-tree", "repo-app"),
        ("other", "/repos/other", "repo-other"),
    ] {
        let register = Command::WorkspaceRegister {
            workspace_id: id.into(),
            root: root.into(),
        };
        ok(run(&mut state, &format!("w-{id}"), register, 0));
        let git = Command::ObserveWorkspaceGit {
            workspace_id: id.into(),
            repository_id: repository.into(),
            branch_ref: Some("refs/heads/main".into()),
        };
        ok(run(&mut state, &format!("g-{id}"), git, 0));
    }
    for (session, workspace) in [("alice", "tree"), ("bob", "main"), ("dave", "other")] {
        let register = Command::SessionRegister {
            session_id: session.into(),
            workspace_id: workspace.into(),
            parent: None,
            headless: false,
        };
        ok(run(&mut state, &format!("s-{session}"), register, 0));
    }
    let edit = Command::Observe {
        session_id: "alice".into(),
        operation_id: "op-a1".into(),
        source: "test".into(),
        tool: "Write".into(),
        paths: vec!["src/config.rs".into()],
        result: langspan_core::ToolResult::Succeeded,
        lease_id: None,
    };
    ok(run(&mut state, "o-a1", edit, 10));
    let notice = Command::ObserveInterface {
        workspace_id: "tree".into(),
        path: "src/config.rs".into(),
        changes: vec![InterfaceChange {
            name: "parse_config".into(),
            kind: "function".into(),
            change: "renamed".into(),
            before: Some("pub fn parse_config(text: &str) -> usize".into()),
            after: Some("pub fn load_config(text: &str) -> usize".into()),
            after_name: Some("load_config".into()),
            before_arity: None,
            after_arity: None,
            before_count: None,
            after_count: None,
        }],
        dependents: vec!["src/main.rs".into()],
    };
    ok(run(&mut state, "notice-1", notice, 20));
    state
}

fn notice(state: &State) -> &InterfaceNotice {
    &state.interface_notices["tree"][0]
}

fn call() -> CallSite {
    CallSite {
        path: "src/report.rs".into(),
        line: 2,
        name: "parse_config".into(),
        args: 1,
        member: true,
    }
}

/// `tree`'s index: `load_config` defined where the notice says, and no
/// `parse_config` left.
fn tree_index(complete: bool) -> BTreeMap<String, IndexedNames> {
    let definition = SymbolRef {
        path: "src/config.rs".into(),
        name: "load_config".into(),
        kind: "function".into(),
        signature: "pub fn load_config(text: &str) -> usize".into(),
        exported: true,
        ..SymbolRef::default()
    };
    BTreeMap::from([(
        "tree".to_owned(),
        IndexedNames {
            definitions: vec![definition],
            complete,
        },
    )])
}

#[test]
fn a_rename_in_another_worktree_is_judged_in_that_worktrees_index() {
    let state = renamed();
    let candidates = call_candidates(&state, "bob", &[call()], 30).unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|(workspace, names)| (workspace.as_str(), names.iter().cloned().collect()))
            .collect::<Vec<(&str, Vec<String>)>>(),
        [("tree", vec!["load_config".into(), "parse_config".into()])]
    );
    let breaks = call_breaks(&state, "bob", &[call()], &tree_index(true), 30).unwrap();
    assert_eq!(breaks.len(), 1, "{breaks:?}");
    assert_eq!(breaks[0].change, "renamed");
    assert_eq!(breaks[0].after_name.as_deref(), Some("load_config"));
    assert_eq!(breaks[0].worktree.as_deref(), Some("/repos/app-tree"));
    assert_eq!(breaks[0].by_session, "alice");

    // Without that worktree's index, or with a partial one, nothing is said:
    // the old name may only have moved.
    assert!(
        call_breaks(&state, "bob", &[call()], &BTreeMap::new(), 30)
            .unwrap()
            .is_empty()
    );
    assert!(
        call_breaks(&state, "bob", &[call()], &tree_index(false), 30)
            .unwrap()
            .is_empty()
    );
    // Another repository's session is never told.
    assert!(
        call_candidates(&state, "dave", &[call()], 30)
            .unwrap()
            .is_empty()
    );
    assert!(
        call_breaks(&state, "dave", &[call()], &tree_index(true), 30)
            .unwrap()
            .is_empty()
    );
    // The session in the worktree that made it sees it as its own
    // workspace's, with no worktree named.
    let carol = Command::SessionRegister {
        session_id: "carol".into(),
        workspace_id: "tree".into(),
        parent: None,
        headless: false,
    };
    let mut state = state;
    ok(run(&mut state, "s-carol", carol, 30));
    let breaks = call_breaks(&state, "carol", &[call()], &tree_index(true), 30).unwrap();
    assert_eq!(breaks[0].worktree, None);
}

fn found(paths: &[&str], from: &str, notice_id: &str) -> Command {
    Command::ObserveInterfaceDependents {
        notice_workspace_id: "tree".into(),
        notice_id: notice_id.into(),
        workspace_id: from.into(),
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
    }
}

fn file(path: &str) -> WorktreeFile {
    WorktreeFile {
        workspace_id: "main".into(),
        path: path.into(),
    }
}

#[test]
fn another_worktrees_files_are_added_to_a_held_notice_once_each_up_to_the_cap() {
    let mut state = renamed();
    let id = notice(&state).notice_id.clone();
    assert_eq!(
        ok(run(
            &mut state,
            "d1",
            found(&["src/b.rs", "src/a.rs", "src/a.rs"], "main", &id),
            30
        )),
        Outcome::InterfaceDependentsObserved {
            notice_id: id.clone(),
            added: 2
        }
    );
    assert_eq!(
        notice(&state).elsewhere,
        [file("src/a.rs"), file("src/b.rs")]
    );
    // The notice's own worktree's dependents are unchanged.
    assert_eq!(notice(&state).dependents, ["src/main.rs"]);
    ok(run(
        &mut state,
        "d2",
        found(&["src/a.rs", "src/c.rs"], "main", &id),
        30,
    ));
    assert_eq!(notice(&state).elsewhere.len(), 3);
    // A file already listed is no new fact.
    let again = decide(&state, &found(&["src/a.rs"], "main", &id), "d3", 30);
    assert!(again.events.is_empty());

    // Full at `NOTICE_DEPENDENTS`.
    let many: Vec<String> = (0..NOTICE_DEPENDENTS)
        .map(|n| format!("m/{n}.rs"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    ok(run(&mut state, "d4", found(&many, "main", &id), 30));
    assert_eq!(notice(&state).elsewhere.len(), NOTICE_DEPENDENTS);
    let full = decide(&state, &found(&["src/z.rs"], "main", &id), "d5", 30);
    assert!(full.events.is_empty());

    // Its own worktree, another repository, an unknown notice, too many or
    // unsafe paths are refused.
    let mut fresh = renamed();
    for (command, expected) in [
        (found(&["src/x.rs"], "tree", &id), "invalid_notice"),
        (found(&["src/x.rs"], "other", &id), "invalid_notice"),
        (
            found(&["src/x.rs"], "main", "lsh1-unknown"),
            "unknown_notice",
        ),
        (found(&["../x.rs"], "main", &id), "invalid_path"),
    ] {
        assert_eq!(code(run(&mut fresh, "r", command, 30)), expected);
    }
    let too_many: Vec<String> = (0..=NOTICE_DEPENDENTS)
        .map(|n| format!("m/{n}.rs"))
        .collect();
    let too_many: Vec<&str> = too_many.iter().map(String::as_str).collect();
    assert_eq!(
        code(run(&mut fresh, "r2", found(&too_many, "main", &id), 30)),
        "invalid_notice"
    );
    assert!(notice(&fresh).elsewhere.is_empty());
}

#[test]
fn files_found_for_a_replaced_notice_change_nothing_when_replayed() {
    let mut state = renamed();
    let id = notice(&state).notice_id.clone();
    let event = Event::InterfaceDependentsFound {
        notice_workspace_id: "tree".into(),
        notice_id: id,
        files: vec![file("src/a.rs")],
    };
    // A newer notice for the file replaces the one the files were found for.
    let newer = Command::ObserveInterface {
        workspace_id: "tree".into(),
        path: "src/config.rs".into(),
        changes: notice(&state).changes.clone(),
        dependents: Vec::new(),
    };
    ok(run(&mut state, "notice-2", newer, 40));
    let before = state.clone();
    apply(&mut state, &event).unwrap();
    assert_eq!(state, before);
}
