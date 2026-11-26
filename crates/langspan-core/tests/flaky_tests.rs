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
    LOG.with(|log| log.borrow_mut().extend(decision.events.iter().cloned()));
    decision.outcome
}

thread_local! {
    /// Every event this test's state took, in order.
    static LOG: std::cell::RefCell<Vec<Event>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The state rebuilt from this test's events alone, as on a restart.
fn replayed() -> State {
    let mut state = State::default();
    LOG.with(|log| {
        for event in log.borrow().iter() {
            apply(&mut state, event).unwrap();
        }
    });
    state
}

fn ready() -> State {
    let mut state = State::default();
    let booted = Event::Booted { generation: 1 };
    apply(&mut state, &booted).unwrap();
    LOG.with(|log| log.borrow_mut().push(booted));
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
        ..TestResults::default()
    }
}

fn passing() -> TestResults {
    TestResults {
        all_passed: true,
        ..TestResults::default()
    }
}

/// A run that passed as a whole and skipped `skipped`.
fn passing_skipping(skipped: &[&str]) -> TestResults {
    TestResults {
        skipped: names(skipped),
        ..passing()
    }
}

/// One record of check `tests` on `tree`, with its per-test results and, for
/// an automatic check, a rerun's, made with no write recorded since the
/// first run, and a notice for alice.
fn record(
    state: &mut State,
    id: &str,
    tree: &str,
    tests: TestResults,
    rerun: Option<TestResults>,
    now: u64,
) -> Outcome {
    let writes = state.writes.get("work").copied().unwrap_or(0);
    record_marked(state, id, tree, tests, rerun, Some(writes), now)
}

/// The identity of the command `record` runs, `cargo test`.
fn cargo_test() -> Option<String> {
    Some("c".repeat(64))
}

/// As `record`, with the write mark the first run was taken at.
fn record_marked(
    state: &mut State,
    id: &str,
    tree: &str,
    tests: TestResults,
    rerun: Option<TestResults>,
    writes_before: Option<u64>,
    now: u64,
) -> Outcome {
    let writes = writes_before;
    record_as(state, id, tree, cargo_test(), tests, rerun, writes, now)
}

/// As `record_marked`, of the command whose identity is `command_id`; its
/// summary is `cargo test` whatever the identity.
#[allow(clippy::too_many_arguments)]
fn record_as(
    state: &mut State,
    id: &str,
    tree: &str,
    command_id: Option<String>,
    tests: TestResults,
    rerun: Option<TestResults>,
    writes_before: Option<u64>,
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
            writes_before,
            command_id,
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
            class: None,
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
fn a_rerun_after_a_write_recorded_since_its_first_run_is_not_compared() {
    let mut state = ready();
    let mark = state.writes.get("work").copied().unwrap_or(0);
    // Written and put back while the rerun ran: the tree is the same, the
    // writes were recorded, and both results arrive together afterwards.
    write(&mut state, "w1");
    write(&mut state, "w2");
    let outcome = record_marked(
        &mut state,
        "c1",
        &tree('a'),
        failing(&["net::times_out"], &[]),
        Some(passing()),
        Some(mark),
        100,
    );
    assert!(
        matches!(outcome, Outcome::CheckRecorded { .. }),
        "{outcome:?}"
    );
    assert_eq!(history(&state, "net::times_out"), None);
    // Nor is the first run's result learned from: the writes may have been
    // recorded while it ran, so the next run on this tree, with no write
    // since, is not compared with it either.
    record(&mut state, "c2", &tree('a'), passing(), None, 200);
    assert_eq!(history(&state, "net::times_out"), None);
    // That next run was of unchanged code, and the one after it is
    // compared with it.
    record(
        &mut state,
        "c2b",
        &tree('a'),
        failing(&["net::times_out"], &[]),
        None,
        250,
    );
    assert_eq!(history(&state, "net::times_out"), Some((1, 1)));
    // A rerun without its mark is refused.
    let outcome = record_marked(
        &mut state,
        "c3",
        &tree('a'),
        failing(&["net::times_out"], &[]),
        Some(passing()),
        None,
        300,
    );
    assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
}

#[test]
fn results_of_two_commands_that_share_a_summary_are_not_compared() {
    let mut state = ready();
    let first = || Some("1".repeat(64));
    let second = || Some("2".repeat(64));
    let failed = || failing(&["net::times_out"], &[]);
    let writes = Some(0);
    // Both are summarised `cargo test`: their identities differ.
    record_as(
        &mut state,
        "c1",
        &tree('a'),
        first(),
        failed(),
        None,
        writes,
        100,
    );
    record_as(
        &mut state,
        "c2",
        &tree('a'),
        second(),
        passing(),
        None,
        writes,
        200,
    );
    assert_eq!(history(&state, "net::times_out"), None);
    // A rerun counts as the same command as its own record.
    record_as(
        &mut state,
        "c3",
        &tree('a'),
        first(),
        failed(),
        Some(passing()),
        writes,
        300,
    );
    assert_eq!(history(&state, "net::times_out"), Some((1, 1)));
    // Compared with the last result of the same identity only.
    record_as(
        &mut state,
        "c4",
        &tree('a'),
        first(),
        failed(),
        None,
        writes,
        400,
    );
    assert_eq!(history(&state, "net::times_out"), Some((2, 2)));
    // Test results without an identity, or with a malformed one, are
    // refused; so is an identity without test results.
    for command_id in [None, Some("C".repeat(64)), Some("1".repeat(63))] {
        let outcome = record_as(
            &mut state,
            "bad",
            &tree('a'),
            command_id,
            failed(),
            None,
            writes,
            500,
        );
        assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    }
    let outcome = run(
        &mut state,
        "bad",
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "tests".into(),
            command: "cargo test".into(),
            tree_oid: tree('a'),
            head_oid: None,
            inputs: Vec::new(),
            result: CheckResult::Passed {},
            duration_ms: 1,
            report: None,
            tests: None,
            rerun: None,
            writes_before: None,
            command_id: first(),
        },
        600,
    );
    assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
}

#[test]
fn a_test_skipped_by_a_passing_run_is_neither_a_pass_nor_a_flip() {
    let mut state = ready();
    let failed = || failing(&["net::times_out"], &["parser::keeps_order"]);
    record(&mut state, "c1", &tree('a'), failed(), None, 100);
    // Fails, is skipped by a passing rerun, fails again: no flip.
    record(
        &mut state,
        "c2",
        &tree('a'),
        failed(),
        Some(passing_skipping(&["net::times_out"])),
        200,
    );
    record(&mut state, "c3", &tree('a'), failed(), None, 300);
    assert_eq!(history(&state, "net::times_out"), None);
    // Skipped in a failing run, too.
    record(
        &mut state,
        "c4",
        &tree('a'),
        TestResults {
            skipped: names(&["net::times_out"]),
            ..failing(&["parser::keeps_order"], &[])
        },
        None,
        400,
    );
    assert_eq!(history(&state, "net::times_out"), None);
    // A test the passing run did not name as skipped still passed in it.
    record(
        &mut state,
        "c5",
        &tree('a'),
        failed(),
        Some(passing_skipping(&["other::test"])),
        500,
    );
    assert_eq!(history(&state, "net::times_out"), Some((1, 1)));
    // One name in two lists is refused.
    let outcome = record(
        &mut state,
        "c6",
        &tree('a'),
        TestResults {
            skipped: names(&["net::times_out"]),
            ..failed()
        },
        None,
        600,
    );
    assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
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
fn forgetting_a_test_drops_its_last_result_so_no_flip_is_counted_against_it() {
    let mut state = ready();
    let failed = || failing(&["net::times_out"], &["parser::keeps_order"]);
    for (n, c) in ['a', 'b'].into_iter().enumerate() {
        write(&mut state, &format!("w{n}"));
        record(
            &mut state,
            &format!("c{n}"),
            &tree(c),
            failed(),
            Some(passing()),
            100,
        );
    }
    // The last result on tree b, before forgetting, is a failure.
    record(&mut state, "c2", &tree('b'), failed(), None, 200);
    assert!(notice_failures(&state)[0].flaky.is_some());
    let outcome = run(
        &mut state,
        "f1",
        Command::ForgetFlakyTest {
            workspace_id: "work".into(),
            name: Some("tests".into()),
            test: "net::times_out".into(),
        },
        300,
    );
    assert!(
        matches!(
            outcome,
            Outcome::FlakyTestForgotten {
                forgotten: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    // Fail, forget, pass, fail on one unchanged tree: only the pass and the
    // failure after the forget are compared, one flip, so it is unlabelled.
    record(&mut state, "c3", &tree('b'), passing(), None, 400);
    assert_eq!(history(&state, "net::times_out"), None);
    record(&mut state, "c4", &tree('b'), failed(), None, 500);
    assert_eq!(history(&state, "net::times_out"), Some((1, 1)));
    assert!(notice_failures(&state)[0].flaky.is_none());
    // The state after a forget is the one its events rebuild on a restart.
    assert_eq!(replayed(), state);
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
    let refused_marked =
        |state: &mut State, tests: TestResults, result: CheckResult, writes_before: Option<u64>| {
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
                    writes_before,
                    command_id: cargo_test(),
                },
                1,
            );
            matches!(outcome, Outcome::Error { .. })
        };
    let refused = |state: &mut State, tests: TestResults, result: CheckResult| {
        refused_marked(state, tests, result, Some(0))
    };
    let failed = CheckResult::Failed { exit_code: 1 };
    // Test results without the write generation the run took are refused.
    assert!(refused_marked(
        &mut state,
        failing(&["a"], &["b"]),
        failed.clone(),
        None
    ));
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
        "rerun": {"all_passed": true}, "writes_before": 0, "command_id": "c".repeat(64),
    }))
    .unwrap();
    let Command::RecordCheck {
        tests,
        rerun,
        writes_before,
        command_id,
        ..
    } = &command
    else {
        panic!("{command:?}")
    };
    assert_eq!(*writes_before, Some(0));
    assert_eq!(*command_id, cargo_test());
    assert_eq!(
        tests.as_ref(),
        Some(&failing(&["net::times_out"], &["parser::keeps_order"]))
    );
    assert_eq!(rerun.as_ref(), Some(&passing()));
    // Skipped tests have a list of their own.
    assert_eq!(
        serde_json::to_value(passing_skipping(&["net::times_out"])).unwrap(),
        json!({"skipped": ["net::times_out"], "all_passed": true})
    );
    // The write generation check runs take is in the status reply.
    let mut state = ready();
    write(&mut state, "w1");
    let (view, _) = langspan_core::status_view(&state, 10);
    assert_eq!(
        serde_json::to_value(&view).unwrap()["writes"],
        json!({"work": 1})
    );
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
            "tree_oid": tree('a'), "command": "cargo test", "command_id": "c".repeat(64),
            "results":{"failed": ["net::times_out"], "passed": ["parser::keeps_order"]},
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
        json!([{"test": "net::times_out", "flaky": {"flips": 2, "runs": 2}, "class": "already_failing"}])
    );
    // It failed in c1, from before alice's first write (issue #69).
    assert_eq!(
        state.check_notices["alice"][0].baseline_check_id.as_deref(),
        Some("c1")
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
