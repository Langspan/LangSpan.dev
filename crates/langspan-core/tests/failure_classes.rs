//! Which tests a write broke (issue #69, ADR 0030): an automatic check's
//! failing tests are compared with the newest result of the same check from
//! before the session's first write, its baseline. Each failure is `new`,
//! `already_failing` or `unknown_before`; tests that failed then and pass now
//! are `fixed`; and the sessions that wrote between the two trees are named.
//! Classification never changes a record, its result or a landing.
use langspan_core::{
    CheckNotice, CheckReport, CheckResult, Command, Event, FailureClass, Outcome, State,
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
    static LOG: std::cell::RefCell<Vec<Event>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn replayed() -> State {
    let mut state = State::default();
    LOG.with(|log| {
        for event in log.borrow().iter() {
            apply(&mut state, event).unwrap();
        }
    });
    state
}

fn ready(sessions: &[&str]) -> State {
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
    for session in sessions {
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: (*session).into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
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

/// A record of check `tests` (`cargo test`) on `tree`, reported to
/// `sessions`, its tree taken at the workspace's current write count.
fn record(
    state: &mut State,
    id: &str,
    tree: &str,
    tests: Option<TestResults>,
    sessions: &[&str],
) -> Outcome {
    let writes = state.writes.get("work").copied().unwrap_or(0);
    let result = match &tests {
        Some(tests) if tests.all_passed => CheckResult::Passed {},
        _ => CheckResult::Failed { exit_code: 101 },
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
            report: (!sessions.is_empty()).then(|| CheckReport {
                session_ids: names(sessions),
                tail: "test result: FAILED".into(),
                timed_out_s: None,
            }),
            writes_before: tests.as_ref().map(|_| writes),
            command_id: tests.as_ref().map(|_| "c".repeat(64)),
            tests,
            rerun: None,
        },
        100,
    )
}

fn write(state: &mut State, session: &str, id: &str) {
    let outcome = run(
        state,
        id,
        Command::ObserveTool {
            session_id: session.into(),
            operation: ToolIdentity {
                operation_id: format!("op-{id}"),
                source: "test".into(),
                tool: "Write".into(),
                input_digest: "a".repeat(64),
            },
            paths: vec![format!("src/{session}.rs")],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        5,
    );
    assert!(matches!(outcome, Outcome::Observed { .. }), "{outcome:?}");
}

fn notice<'a>(state: &'a State, session: &str) -> &'a CheckNotice {
    &state.check_notices[session][0]
}

fn classes(notice: &CheckNotice) -> Vec<(String, Option<FailureClass>)> {
    notice
        .failures
        .iter()
        .map(|failure| (failure.test.clone(), failure.class))
        .collect()
}

#[test]
fn failures_are_compared_with_the_result_from_before_the_sessions_first_write() {
    let mut state = ready(&["alice"]);
    // The baseline: before alice wrote anything.
    record(
        &mut state,
        "c0",
        &tree('a'),
        Some(failing(
            &["net::times_out", "old::broken"],
            &["parser::keeps_order", "parser::rejects_empty"],
        )),
        &[],
    );
    write(&mut state, "alice", "w1");
    // A record after her first write is never her baseline.
    record(
        &mut state,
        "c1",
        &tree('b'),
        Some(failing(&["parser::keeps_order"], &[])),
        &["alice"],
    );
    write(&mut state, "alice", "w2");
    record(
        &mut state,
        "c2",
        &tree('c'),
        Some(failing(
            &[
                "parser::rejects_empty",
                "parser::keeps_order",
                "net::times_out",
                "brand::new_test",
            ],
            &["old::broken"],
        )),
        &["alice"],
    );
    let notice = notice(&state, "alice");
    assert_eq!(notice.check_id, "c2");
    assert_eq!(notice.baseline_check_id.as_deref(), Some("c0"));
    assert_eq!(
        classes(notice),
        [
            ("parser::rejects_empty".into(), Some(FailureClass::New)),
            ("parser::keeps_order".into(), Some(FailureClass::New)),
            ("net::times_out".into(), Some(FailureClass::AlreadyFailing)),
            ("brand::new_test".into(), Some(FailureClass::UnknownBefore)),
        ]
    );
    assert_eq!(notice.fixed, ["old::broken"]);
    assert!(notice.writers.is_empty(), "{:?}", notice.writers);
    assert_eq!(replayed(), state, "derived from the events alone");
}

#[test]
fn a_baseline_that_passed_as_a_whole_makes_every_failure_new() {
    let mut state = ready(&["alice"]);
    record(&mut state, "c0", &tree('a'), Some(passing()), &[]);
    write(&mut state, "alice", "w1");
    record(
        &mut state,
        "c1",
        &tree('b'),
        Some(failing(&["parser::keeps_order"], &[])),
        &["alice"],
    );
    assert_eq!(
        classes(notice(&state, "alice")),
        [("parser::keeps_order".into(), Some(FailureClass::New))]
    );
    // And the run that fixes it says so.
    write(&mut state, "alice", "w2");
    record(&mut state, "c2", &tree('c'), Some(passing()), &["alice"]);
    let fixed = notice(&state, "alice");
    assert_eq!(fixed.baseline_check_id.as_deref(), Some("c0"));
    assert!(fixed.fixed.is_empty(), "nothing failed in the baseline");
}

#[test]
fn without_a_baseline_or_without_named_tests_nothing_is_classified() {
    let mut state = ready(&["alice"]);
    write(&mut state, "alice", "w1");
    record(
        &mut state,
        "c1",
        &tree('b'),
        Some(failing(&["parser::keeps_order"], &[])),
        &["alice"],
    );
    let first = notice(&state, "alice");
    assert_eq!(first.baseline_check_id, None);
    assert_eq!(classes(first), [("parser::keeps_order".into(), None)]);
    assert!(first.fixed.is_empty() && first.writers.is_empty());

    // An output no parser recognised: no classes, whatever the baseline.
    let mut state = ready(&["alice"]);
    record(&mut state, "c0", &tree('a'), Some(passing()), &[]);
    write(&mut state, "alice", "w1");
    record(&mut state, "c1", &tree('b'), None, &["alice"]);
    let unclassified = notice(&state, "alice");
    assert_eq!(unclassified.baseline_check_id, None);
    assert!(unclassified.failures.is_empty());
}

#[test]
fn the_sessions_that_wrote_between_the_two_trees_are_named() {
    let mut state = ready(&["alice", "bob", "carol"]);
    // Carol wrote before the baseline: her write is in both trees.
    write(&mut state, "carol", "w0");
    record(&mut state, "c0", &tree('a'), Some(passing()), &[]);
    write(&mut state, "bob", "w1");
    write(&mut state, "bob", "w2");
    write(&mut state, "alice", "w3");
    record(
        &mut state,
        "c1",
        &tree('b'),
        Some(failing(&["parser::keeps_order"], &[])),
        &["alice", "bob"],
    );
    // Each session is told of the others' writes, never of its own.
    assert_eq!(notice(&state, "alice").writers, ["bob"]);
    assert_eq!(notice(&state, "bob").writers, ["alice"]);
    for session in ["alice", "bob"] {
        assert_eq!(
            classes(notice(&state, session)),
            [("parser::keeps_order".into(), Some(FailureClass::New))]
        );
    }
    // A write after the tree was taken is not in it.
    let writes = state.writes["work"];
    write(&mut state, "carol", "w4");
    run(
        &mut state,
        "c2",
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "tests".into(),
            command: "cargo test".into(),
            tree_oid: tree('c'),
            head_oid: None,
            inputs: Vec::new(),
            result: CheckResult::Failed { exit_code: 101 },
            duration_ms: 900,
            report: Some(CheckReport {
                session_ids: names(&["alice"]),
                tail: String::new(),
                timed_out_s: None,
            }),
            writes_before: Some(writes),
            command_id: Some("c".repeat(64)),
            tests: Some(failing(&["parser::keeps_order"], &[])),
            rerun: None,
        },
        100,
    );
    assert_eq!(notice(&state, "alice").writers, ["bob"]);
    assert_eq!(replayed(), state);
}

#[test]
fn a_record_and_its_landing_answer_are_the_same_with_and_without_classification() {
    let landing = |state: &mut State, tree: &str| {
        run(
            state,
            "read",
            Command::LandingReadiness {
                workspace_id: "work".into(),
                name: "tests".into(),
                tree_oid: tree.into(),
            },
            200,
        )
    };
    let finish = |baseline: bool| {
        let mut state = ready(&["alice"]);
        if baseline {
            record(&mut state, "c0", &tree('a'), Some(passing()), &[]);
        }
        write(&mut state, "alice", "w1");
        record(
            &mut state,
            "c1",
            &tree('b'),
            Some(failing(&["parser::keeps_order"], &[])),
            &["alice"],
        );
        let failed = state.checks["work"][0].clone();
        let failed_landing = landing(&mut state, &tree('b'));
        write(&mut state, "alice", "w2");
        record(&mut state, "c2", &tree('c'), Some(passing()), &["alice"]);
        let passed = state.checks["work"][0].clone();
        let passed_landing = landing(&mut state, &tree('c'));
        (failed, failed_landing, passed, passed_landing)
    };
    let classified = finish(true);
    let plain = finish(false);
    assert_eq!(classified, plain);
    assert!(matches!(
        classified.1,
        Outcome::Readiness { ready: false, .. }
    ));
    assert!(matches!(
        classified.3,
        Outcome::Readiness { ready: true, .. }
    ));
}

#[test]
fn a_result_of_another_command_with_the_same_summary_is_no_baseline() {
    let mut state = ready(&["alice"]);
    // `cargo test` too, by its summary, but another argument list.
    run(
        &mut state,
        "c0",
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "tests".into(),
            command: "cargo test".into(),
            tree_oid: tree('a'),
            head_oid: None,
            inputs: Vec::new(),
            result: CheckResult::Passed {},
            duration_ms: 900,
            report: None,
            writes_before: Some(0),
            command_id: Some("d".repeat(64)),
            tests: Some(passing()),
            rerun: None,
        },
        100,
    );
    write(&mut state, "alice", "w1");
    record(
        &mut state,
        "c1",
        &tree('b'),
        Some(failing(&["parser::keeps_order"], &[])),
        &["alice"],
    );
    let notice = notice(&state, "alice");
    assert_eq!(notice.baseline_check_id, None);
    assert_eq!(classes(notice), [("parser::keeps_order".into(), None)]);
}

#[test]
fn an_older_check_recorded_event_without_test_results_still_applies() {
    let mut state = ready(&["alice"]);
    record(&mut state, "c0", &tree('a'), None, &[]);
    let event = LOG.with(|log| {
        log.borrow()
            .iter()
            .rev()
            .find(|event| matches!(event, Event::CheckRecorded { .. }))
            .cloned()
            .unwrap()
    });
    let json = serde_json::to_value(&event).unwrap();
    assert!(json.get("tests").is_none(), "{json}");
    let parsed: Event = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, event);
}

/// A record of check `tests` (`cargo test`) on `tree` that says its tree was
/// taken at write count `writes_before`.
fn record_taken_at(
    state: &mut State,
    id: &str,
    tree: &str,
    writes_before: u64,
    tests: TestResults,
) {
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
            result: CheckResult::Failed { exit_code: 101 },
            duration_ms: 900,
            report: None,
            writes_before: Some(writes_before),
            command_id: Some("c".repeat(64)),
            tests: Some(tests),
            rerun: None,
        },
        100,
    );
}

#[test]
fn a_run_whose_write_count_moved_before_it_was_recorded_is_no_baseline() {
    let mut state = ready(&["alice"]);
    // The run takes count 0, then alice's first write breaks a test before
    // the run takes its tree: the tree it tests already holds her write.
    write(&mut state, "alice", "w1");
    record_taken_at(
        &mut state,
        "c0",
        &tree('a'),
        0,
        failing(&["parser::keeps_order"], &["parser::rejects_empty"]),
    );
    assert!(
        state.kept_tests.is_empty(),
        "kept as a baseline: {:?}",
        state.kept_tests
    );
    write(&mut state, "alice", "w2");
    record(
        &mut state,
        "c1",
        &tree('b'),
        Some(failing(
            &["parser::keeps_order"],
            &["parser::rejects_empty"],
        )),
        &["alice"],
    );
    let notice = notice(&state, "alice");
    assert_eq!(notice.baseline_check_id, None);
    assert_eq!(classes(notice), [("parser::keeps_order".into(), None)]);
    assert_eq!(replayed().kept_tests, state.kept_tests);
}

#[test]
fn an_older_record_whose_write_count_had_moved_is_no_baseline_on_replay() {
    let mut state = ready(&["alice"]);
    write(&mut state, "alice", "w1");
    // An event an earlier version stored: results kept although a write was
    // counted after the run took its count.
    let event = Event::CheckRecorded {
        check: langspan_core::CheckRecord {
            check_id: "c0".into(),
            workspace_id: "work".into(),
            name: "tests".into(),
            command: "cargo test".into(),
            tree_oid: tree('a'),
            head_oid: None,
            inputs: Vec::new(),
            result: CheckResult::Failed { exit_code: 101 },
            duration_ms: 900,
            recorded_ms: 100,
            generation: 1,
            changed_since: false,
        },
        tests: Some(langspan_core::KeptTests {
            results: failing(&["parser::keeps_order"], &[]),
            writes_before: 0,
            command_id: "c".repeat(64),
        }),
    };
    apply(&mut state, &event).unwrap();
    assert!(
        state.checks["work"]
            .iter()
            .any(|record| record.check_id == "c0")
    );
    assert!(state.kept_tests.is_empty(), "{:?}", state.kept_tests);
}
