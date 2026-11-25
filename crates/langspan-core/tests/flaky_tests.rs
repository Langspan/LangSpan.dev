//! Tests that flip on unchanged code (issue #70, ADR 0030): two results of
//! one check on one tree, with no recorded write between, that disagree
//! about a test are one flip; enough flips make the test known to flip, and
//! a failure of it is labelled so, without ever counting as a pass.
use langspan_core::{
    CheckReport, CheckResult, Command, Event, FLAKY_DECAY_RUNS, Outcome, State, TestFailure,
    TestResults, ToolIdentity, ToolResult, apply, decide,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    // The state is derived from the events alone: replaying them gives it.
    decision.outcome
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
    run(
        &mut state,
        "s-alice",
        Command::SessionRegister {
            session_id: "alice".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        0,
    );
    state
}

fn tree(c: char) -> String {
    c.to_string().repeat(40)
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

fn failing(failed: &[&str], passed: &[&str]) -> TestResults {
    TestResults {
        failed: names(failed),
        passed: names(passed),
        all_passed: false,
    }
}

fn passing() -> TestResults {
    TestResults {
        failed: Vec::new(),
        passed: Vec::new(),
        all_passed: true,
    }
}

/// One record of check `tests` on `tree`, with its per-test results and, for
/// an automatic check, a rerun's and a notice for alice.
fn record(
    state: &mut State,
    id: &str,
    tree: &str,
    tests: TestResults,
    rerun: Option<TestResults>,
    now: u64,
) -> Outcome {
    let result = if tests.all_passed {
        CheckResult::Passed {}
    } else {
        CheckResult::Failed { exit_code: 101 }
    };
    run(
        state,
        id,
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "tests".into(),
            command: "cargo test".into(),
            tree_oid: tree.into(),
            head_oid: None,
            inputs: Vec::new(),
            result,
            duration_ms: 900,
            report: Some(CheckReport {
                session_ids: vec!["alice".into()],
                tail: "test net::times_out ... FAILED".into(),
                timed_out_s: None,
            }),
            tests: Some(tests),
            rerun,
        },
        now,
    )
}

fn write(state: &mut State, id: &str) {
    let outcome = run(
        state,
        id,
        Command::ObserveTool {
            session_id: "alice".into(),
            operation: ToolIdentity {
                operation_id: format!("op-{id}"),
                source: "test".into(),
                tool: "Write".into(),
                input_digest: "a".repeat(64),
            },
            paths: vec!["src/lib.rs".into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        5,
    );
    assert!(matches!(outcome, Outcome::Observed { .. }), "{outcome:?}");
}

/// Flips and runs the state holds for one test of check `tests`.
fn history(state: &State, test: &str) -> Option<(usize, usize)> {
    let history = state
        .test_learning
        .get("work")?
        .tests
        .get("tests")?
        .get(test)?;
    Some((
        history.runs.iter().filter(|flipped| **flipped).count(),
        history.runs.len(),
    ))
}

/// The failures the newest notice for alice carries.
fn notice_failures(state: &State) -> Vec<TestFailure> {
    state.check_notices["alice"][0].failures.clone()
}

fn flaky(state: &mut State, name: Option<&str>) -> Vec<langspan_core::FlakyTest> {
    match run(
        state,
        "read",
        Command::FlakyTests {
            workspace_id: "work".into(),
            name: name.map(str::to_owned),
        },
        1_000_000,
    ) {
        Outcome::FlakyTests { tests, .. } => tests,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failure_then_a_pass_on_a_rerun_of_the_identical_tree_is_one_flip() {
    let mut state = ready();
    record(
        &mut state,
        "c1",
        &tree('a'),
        failing(&["net::times_out"], &["parser::keeps_order"]),
        Some(passing()),
        100,
    );
    assert_eq!(history(&state, "net::times_out"), Some((1, 1)));
    // A test that passed both times flipped nowhere and is not tracked.
    assert_eq!(history(&state, "parser::keeps_order"), None);
    // One flip is not yet "known to flip".
    assert_eq!(
        notice_failures(&state),
        [TestFailure {
            test: "net::times_out".into(),
            flaky: None,
        }]
    );
    assert!(flaky(&mut state, None).is_empty());
    // The record is the failure; the rerun's pass is no record.
    let checks = &state.checks["work"];
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].result, CheckResult::Failed { exit_code: 101 });
}

#[test]
fn a_failure_then_a_pass_after_a_write_is_no_flip() {
    let mut state = ready();
    record(
        &mut state,
        "c1",
        &tree('a'),
        failing(&["net::times_out"], &[]),
        None,
        100,
    );
    write(&mut state, "w1");
    // Even on the same tree (the write was undone), a write between is a
    // change: nothing is compared.
    record(&mut state, "c2", &tree('a'), passing(), None, 200);
    assert_eq!(history(&state, "net::times_out"), None);
    // A different tree without a write between is not compared either.
    record(
        &mut state,
        "c3",
        &tree('b'),
        failing(&["net::times_out"], &[]),
        None,
        300,
    );
    assert_eq!(history(&state, "net::times_out"), None);
}

#[test]
fn two_records_on_one_tree_with_no_write_between_are_compared() {
    let mut state = ready();
    record(
        &mut state,
        "c1",
        &tree('a'),
        failing(&["net::times_out"], &[]),
        None,
        100,
    );
    // A pass named in a failing run counts: the second run failed on
    // another test, and named this one as passing.
    record(
        &mut state,
        "c2",
        &tree('a'),
        failing(&["parser::keeps_order"], &["net::times_out"]),
        None,
        200,
    );
    assert_eq!(history(&state, "net::times_out"), Some((1, 1)));
    assert_eq!(
        history(&state, "parser::keeps_order"),
        None,
        "never seen passing"
    );
}

#[test]
fn after_two_flips_a_failure_is_labelled_with_its_counts_and_is_still_a_failure() {
    let mut state = ready();
    record(
        &mut state,
        "c1",
        &tree('a'),
        failing(&["net::times_out"], &[]),
        Some(passing()),
        100,
    );
    write(&mut state, "w1");
    record(
        &mut state,
        "c2",
        &tree('b'),
        failing(&["net::times_out", "parser::rejects_empty"], &[]),
        Some(failing(&["parser::rejects_empty"], &["net::times_out"])),
        2_000,
    );
    assert_eq!(history(&state, "net::times_out"), Some((2, 2)));
    let failures = notice_failures(&state);
    assert_eq!(failures.len(), 2, "{failures:?}");
    let label = failures[0].flaky.as_ref().expect("labelled");
    assert_eq!(
        (failures[0].test.as_str(), label.flips, label.runs),
        ("net::times_out", 2, 2)
    );
    assert_eq!(failures[1].test, "parser::rejects_empty");
    assert!(failures[1].flaky.is_none());
    let listed = flaky(&mut state, None);
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (
            listed[0].test.as_str(),
            listed[0].check_name.as_str(),
            listed[0].runs,
            listed[0].flips,
            listed[0].last_flip_age_ms
        ),
        ("net::times_out", "tests", 2, 2, 1_000_000 - 2_000)
    );
    assert!(flaky(&mut state, Some("other")).is_empty());
    // Every failure of a flaky test: the record is still failed, and a
    // landing does not accept it.
    record(
        &mut state,
        "c3",
        &tree('b'),
        failing(&["net::times_out"], &[]),
        None,
        3_000,
    );
    assert_eq!(
        state.checks["work"][0].result,
        CheckResult::Failed { exit_code: 101 }
    );
    match run(
        &mut state,
        "read",
        Command::LandingReadiness {
            workspace_id: "work".into(),
            name: "tests".into(),
            tree_oid: tree('b'),
        },
        3_001,
    ) {
        Outcome::Readiness { ready, .. } => assert!(!ready),
        other => panic!("{other:?}"),
    }
}

#[test]
fn forgetting_a_test_clears_it_and_its_next_failure_is_unlabelled() {
    let mut state = ready();
    for (n, c) in ['a', 'b'].into_iter().enumerate() {
        write(&mut state, &format!("w{n}"));
        record(
            &mut state,
            &format!("c{n}"),
            &tree(c),
            failing(&["net::times_out"], &[]),
            Some(passing()),
            100,
        );
    }
    assert!(notice_failures(&state)[0].flaky.is_some());
    let forget = |state: &mut State, id: &str| match run(
        state,
        id,
        Command::ForgetFlakyTest {
            workspace_id: "work".into(),
            name: None,
            test: "net::times_out".into(),
        },
        500,
    ) {
        Outcome::FlakyTestForgotten { forgotten, .. } => forgotten,
        other => panic!("{other:?}"),
    };
    assert!(forget(&mut state, "f1"));
    assert_eq!(history(&state, "net::times_out"), None);
    assert!(!forget(&mut state, "f2"), "nothing left to forget");
    write(&mut state, "w9");
    record(
        &mut state,
        "c9",
        &tree('c'),
        failing(&["net::times_out"], &[]),
        None,
        600,
    );
    assert!(notice_failures(&state)[0].flaky.is_none());
}

#[test]
fn a_test_with_no_flip_in_its_last_fifty_runs_is_no_longer_flaky() {
    let mut state = ready();
    for (n, c) in ['a', 'b'].into_iter().enumerate() {
        write(&mut state, &format!("w{n}"));
        record(
            &mut state,
            &format!("c{n}"),
            &tree(c),
            failing(&["net::times_out"], &[]),
            Some(passing()),
            100,
        );
    }
    assert_eq!(flaky(&mut state, None).len(), 1);
    // Passing runs of the same tree: each one is a run without a flip.
    for n in 0..FLAKY_DECAY_RUNS - 1 {
        record(
            &mut state,
            &format!("p{n}"),
            &tree('b'),
            passing(),
            None,
            200,
        );
        assert_eq!(flaky(&mut state, None).len(), 1, "after {n} clean runs");
    }
    record(&mut state, "last", &tree('b'), passing(), None, 300);
    assert!(flaky(&mut state, None).is_empty());
    assert_eq!(history(&state, "net::times_out"), None);
}

#[test]
fn test_results_are_bounded_and_belong_to_a_conclusive_working_tree_check() {
    let mut state = ready();
    let refused = |state: &mut State, tests: TestResults, result: CheckResult| {
        let outcome = run(
            state,
            "bad",
            Command::RecordCheck {
                workspace_id: "work".into(),
                name: "tests".into(),
                command: "cargo test".into(),
                tree_oid: tree('a'),
                head_oid: None,
                inputs: Vec::new(),
                result,
                duration_ms: 1,
                report: None,
                tests: Some(tests),
                rerun: None,
            },
            1,
        );
        matches!(outcome, Outcome::Error { .. })
    };
    let failed = CheckResult::Failed { exit_code: 1 };
    let many: Vec<String> = (0..=langspan_core::MAX_TEST_NAMES)
        .map(|n| format!("t{n}"))
        .collect();
    assert!(refused(
        &mut state,
        TestResults {
            failed: many,
            ..failing(&[], &[])
        },
        failed.clone()
    ));
    assert!(refused(
        &mut state,
        failing(&[&"x".repeat(201)], &[]),
        failed.clone()
    ));
    assert!(refused(&mut state, failing(&["a\nb"], &[]), failed.clone()));
    assert!(refused(&mut state, failing(&["a"], &["a"]), failed.clone()));
    // Passed as a whole with a failing test, or the reverse.
    assert!(refused(&mut state, passing(), failed.clone()));
    assert!(refused(
        &mut state,
        failing(&["a"], &[]),
        CheckResult::Passed {}
    ));
    assert!(refused(
        &mut state,
        failing(&["a"], &[]),
        CheckResult::Inconclusive {}
    ));
    assert!(state.checks.is_empty());
    assert!(!refused(&mut state, failing(&["a"], &["b"]), failed));
}

#[test]
fn the_wire_forms_of_flaky_test_commands_outcomes_and_events_are_fixed() {
    use serde_json::json;
    let command: Command = serde_json::from_value(json!({
        "op": "record_check", "workspace_id": "work", "name": "tests", "command": "cargo test",
        "tree_oid": tree('a'), "head_oid": null, "inputs": [], "result": {"type": "failed", "exit_code": 101},
        "duration_ms": 9,
        "tests": {"failed": ["net::times_out"], "passed": ["parser::keeps_order"]},
        "rerun": {"all_passed": true},
    }))
    .unwrap();
    let Command::RecordCheck { tests, rerun, .. } = &command else {
        panic!("{command:?}")
    };
    assert_eq!(
        tests.as_ref(),
        Some(&failing(&["net::times_out"], &["parser::keeps_order"]))
    );
    assert_eq!(rerun.as_ref(), Some(&passing()));
    assert_eq!(
        serde_json::to_value(Command::FlakyTests {
            workspace_id: "work".into(),
            name: None,
        })
        .unwrap(),
        json!({"op": "flaky_tests", "workspace_id": "work"})
    );
    assert_eq!(
        serde_json::to_value(Command::ForgetFlakyTest {
            workspace_id: "work".into(),
            name: Some("tests".into()),
            test: "net::times_out".into(),
        })
        .unwrap(),
        json!({"op": "forget_flaky_test", "workspace_id": "work", "name": "tests", "test": "net::times_out"})
    );
    assert!(
        serde_json::from_value::<Command>(json!({
            "op": "flaky_tests", "workspace_id": "work", "limit": 3
        }))
        .is_err(),
        "unknown fields are refused at the wire"
    );
    let mut state = ready();
    let decision = decide(&state, &command, "c1", 77);
    assert_eq!(
        serde_json::to_value(&decision.events[1]).unwrap(),
        json!({
            "type": "tests_recorded", "workspace_id": "work", "check_id": "c1", "name": "tests",
            "tree_oid": tree('a'), "command": "cargo test",
            "results": {"failed": ["net::times_out"], "passed": ["parser::keeps_order"]},
            "at_ms": 77,
        })
    );
    assert_eq!(
        serde_json::to_value(&decision.events[2]).unwrap()["rerun"],
        json!(true)
    );
    for event in &decision.events {
        apply(&mut state, event).unwrap();
    }
    // Two flips, so it is listed.
    write(&mut state, "w1");
    record(
        &mut state,
        "c2",
        &tree('b'),
        failing(&["net::times_out"], &[]),
        Some(passing()),
        1_000,
    );
    let outcome = run(
        &mut state,
        "read",
        Command::FlakyTests {
            workspace_id: "work".into(),
            name: None,
        },
        4_000,
    );
    assert_eq!(
        serde_json::to_value(&outcome).unwrap(),
        json!({"type": "flaky_tests", "workspace_id": "work", "tests": [
            {"test": "net::times_out", "check_name": "tests", "runs": 2, "flips": 2, "last_flip_age_ms": 3_000}
        ]})
    );
    assert_eq!(
        serde_json::to_value(&state.check_notices["alice"][0].failures).unwrap(),
        json!([{"test": "net::times_out", "flaky": {"flips": 2, "runs": 2}}])
    );
}

#[test]
fn an_earlier_store_without_test_learning_opens_and_learns() {
    let mut state = ready();
    let json = serde_json::to_value(&state).unwrap();
    assert!(json.get("test_learning").is_none(), "{json}");
    let mut reopened: State = serde_json::from_value(json).unwrap();
    record(
        &mut reopened,
        "c1",
        &tree('a'),
        failing(&["x"], &[]),
        Some(passing()),
        1,
    );
    state = reopened;
    assert_eq!(history(&state, "x"), Some((1, 1)));
}
