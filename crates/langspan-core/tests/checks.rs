//! A check result is evidence about one exact tree and nothing else.
use langspan_core::{
    Applicability, CheckResult, Command, Event, FileState, Outcome, State, apply, decide,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

fn oid(c: char) -> String {
    c.to_string().repeat(40)
}

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    run(
        &mut state,
        "w",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        0,
    );
    state
}

fn record(
    state: &mut State,
    id: &str,
    tree: char,
    result: CheckResult,
    inputs: Vec<String>,
) -> Outcome {
    run(
        state,
        id,
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "test".into(),
            command: "cargo test".into(),
            tree_oid: oid(tree),
            head_oid: Some(oid('9')),
            inputs,
            result,
            duration_ms: 1200,
            report: None,
            tests: None,
            rerun: None,
        },
        10,
    )
}

fn views(state: &mut State, tree: Option<char>) -> Vec<(String, Applicability)> {
    match run(
        state,
        "read",
        Command::CheckStatus {
            workspace_id: "work".into(),
            tree_oid: tree.map(oid),
        },
        20,
    ) {
        Outcome::Checks { checks } => checks
            .into_iter()
            .map(|view| (view.record.check_id, view.applicability))
            .collect(),
        other => panic!("{other:?}"),
    }
}

fn ready_for(state: &mut State, tree: char) -> (bool, String) {
    match run(
        state,
        "read",
        Command::LandingReadiness {
            workspace_id: "work".into(),
            name: "test".into(),
            tree_oid: oid(tree),
        },
        30,
    ) {
        Outcome::Readiness { ready, reason, .. } => (ready, reason),
        other => panic!("{other:?}"),
    }
}

fn disk_change(state: &mut State, id: &str, before: Option<char>) {
    let version = |c: char| FileState::Present {
        digest: c.to_string().repeat(64),
        kind: Default::default(),
    };
    let outcome = run(
        state,
        id,
        Command::ObserveDisk {
            workspace_id: "work".into(),
            path: "src/a.rs".into(),
            before: before.map(version),
            after: version('2'),
            related_operation_ids: vec![],
        },
        15,
    );
    assert!(
        matches!(outcome, Outcome::DiskObserved { .. }),
        "{outcome:?}"
    );
}

#[test]
fn a_pass_applies_only_to_the_tree_it_ran_on() {
    let mut state = ready();
    record(&mut state, "c1", 'a', CheckResult::Passed {}, vec![]);
    assert_eq!(
        views(&mut state, Some('a')),
        [("c1".into(), Applicability::Current)]
    );
    assert_eq!(
        views(&mut state, Some('b')),
        [("c1".into(), Applicability::Stale)]
    );
    assert_eq!(
        views(&mut state, None),
        [("c1".into(), Applicability::Unknown)],
        "with nothing to compare, it is not called current"
    );
    assert!(ready_for(&mut state, 'a').0);
    let (ready, reason) = ready_for(&mut state, 'b');
    assert!(!ready && reason.contains("other trees"), "{reason}");
}

#[test]
fn an_observed_file_change_makes_a_working_tree_check_stale_and_keeps_its_history() {
    let mut state = ready();
    record(&mut state, "c1", 'a', CheckResult::Passed {}, vec![]);
    record(
        &mut state,
        "merged",
        'e',
        CheckResult::Passed {},
        vec![oid('1'), oid('2')],
    );
    // The observer's first baseline of a file is not a change.
    disk_change(&mut state, "d0", None);
    assert_eq!(
        views(&mut state, None)[1],
        ("c1".into(), Applicability::Unknown)
    );
    disk_change(&mut state, "d1", Some('1'));
    let all = views(&mut state, None);
    assert_eq!(all[1], ("c1".into(), Applicability::Stale));
    // A check of a merged tree does not depend on any working tree.
    assert_eq!(all[0], ("merged".into(), Applicability::Unknown));
    // The record is still there, and still exactly right about its own tree.
    assert_eq!(
        views(&mut state, Some('a'))[1],
        ("c1".into(), Applicability::Current)
    );
    assert_eq!(state.checks["work"].len(), 2);
}

#[test]
fn failed_and_inconclusive_results_never_make_a_tree_ready_and_a_rerun_replaces_them() {
    let mut state = ready();
    record(
        &mut state,
        "c1",
        'a',
        CheckResult::Failed { exit_code: 101 },
        vec![],
    );
    let (ready, reason) = ready_for(&mut state, 'a');
    assert!(!ready && reason.contains("exit 101"), "{reason}");
    record(&mut state, "c2", 'a', CheckResult::Inconclusive {}, vec![]);
    let (ready, reason) = ready_for(&mut state, 'a');
    assert!(!ready && reason.contains("inconclusive"), "{reason}");
    record(&mut state, "c3", 'a', CheckResult::Passed {}, vec![]);
    assert!(ready_for(&mut state, 'a').0);
    assert_eq!(
        state.checks["work"].len(),
        1,
        "one record per name and tree"
    );
    // A later failure on the same tree withdraws readiness again.
    record(
        &mut state,
        "c4",
        'a',
        CheckResult::Failed { exit_code: 1 },
        vec![],
    );
    assert!(!ready_for(&mut state, 'a').0);
    let (ready, reason) = ready_for(&mut state, 'f');
    assert!(!ready && reason.contains("other trees"), "{reason}");
}

#[test]
fn a_landing_is_recorded_only_against_a_passing_check_of_exactly_that_tree() {
    let mut state = ready();
    record(
        &mut state,
        "pass-a",
        'a',
        CheckResult::Passed {},
        vec![oid('1')],
    );
    record(
        &mut state,
        "fail-b",
        'b',
        CheckResult::Failed { exit_code: 1 },
        vec![oid('1')],
    );
    let land = |state: &mut State, id: &str, tree: char, check: &str| {
        run(
            state,
            id,
            Command::RecordLanding {
                workspace_id: "work".into(),
                onto: "main".into(),
                commit_oid: oid('c'),
                tree_oid: oid(tree),
                inputs: vec![oid('1')],
                check_id: check.into(),
            },
            40,
        )
    };
    for (id, tree, check) in [
        ("l1", 'b', "pass-a"),
        ("l2", 'b', "fail-b"),
        ("l3", 'a', "missing"),
    ] {
        let outcome = land(&mut state, id, tree, check);
        assert!(
            matches!(outcome, Outcome::Error { ref code, .. } if code == "no_current_evidence"),
            "{outcome:?}"
        );
    }
    assert!(state.landings.is_empty());
    assert!(matches!(
        land(&mut state, "l4", 'a', "pass-a"),
        Outcome::Landed { .. }
    ));
    assert_eq!(state.landings["work"][0].check_id, "pass-a");
}

#[test]
fn invalid_check_records_are_rejected_without_events() {
    let state = ready();
    let base = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut value = serde_json::json!({
            "op": "record_check", "workspace_id": "work", "name": "test", "command": "cargo test",
            "tree_oid": oid('a'), "head_oid": null, "inputs": [], "result": {"type": "passed"},
            "duration_ms": 1,
        });
        edit(&mut value);
        value
    };
    for value in [
        base(&|v| v["workspace_id"] = "nowhere".into()),
        base(&|v| v["tree_oid"] = "short".into()),
        base(&|v| v["command"] = "two\nlines".into()),
        base(&|v| v["command"] = "".into()),
        base(&|v| v["name"] = "has space".into()),
        base(&|v| v["inputs"] = serde_json::json!(["nope"])),
    ] {
        let command: Command = serde_json::from_value(value.clone()).unwrap();
        let decision = decide(&state, &command, "bad", 1);
        assert!(matches!(decision.outcome, Outcome::Error { .. }), "{value}");
        assert!(decision.events.is_empty());
    }
    assert!(
        serde_json::from_value::<Command>(base(
            &|v| v["result"] = serde_json::json!({"type": "passed", "note": 1})
        ))
        .is_err(),
        "unknown fields in a result are rejected at the wire"
    );
}
