//! The automatic check after a session's writes (ADR 0030): the owner's
//! configuration, the boundary that says a session wrote since it was last
//! checked, and the result delivered once to the sessions it ran after.
use langspan_core::{
    CheckReport, CheckResult, Command, Event, Outcome, State, ToolIdentity, ToolResult, apply,
    decide,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    decision.outcome
}

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (id, workspace) in [("w", "work"), ("w2", "other")] {
        run(
            &mut state,
            id,
            Command::WorkspaceRegister {
                workspace_id: workspace.into(),
                root: format!("/repos/{workspace}"),
            },
            0,
        );
    }
    for (session, workspace) in [("alice", "work"), ("bob", "work"), ("carol", "other")] {
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: workspace.into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    state
}

fn enable(state: &mut State, argv: &[&str], timeout_s: u32) -> Outcome {
    run(
        state,
        "auto",
        Command::AutoCheckEnable {
            workspace_id: "work".into(),
            name: "test".into(),
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            timeout_s,
            on_interface_change: None,
        },
        1,
    )
}

/// A tool call by `session` that changed `path` (or failed to).
fn write(state: &mut State, session: &str, id: &str, path: &str, result: ToolResult) {
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
            paths: vec![path.into()],
            result,
            lease_id: None,
        },
        5,
    );
    assert!(matches!(outcome, Outcome::Observed { .. }), "{outcome:?}");
}

fn batch(state: &mut State, session: &str, id: &str) -> bool {
    match run(
        state,
        id,
        Command::ObserveToolBatch {
            session_id: session.into(),
            operation_ids: vec![format!("op-{id}")],
            calls: 1,
        },
        6,
    ) {
        Outcome::ToolBatchObserved { wrote, .. } => wrote,
        other => panic!("{other:?}"),
    }
}

fn stop(state: &mut State, session: &str, id: &str) -> bool {
    match run(
        state,
        id,
        Command::SessionStop {
            session_id: session.into(),
            failure: None,
            prompt_id: None,
        },
        7,
    ) {
        Outcome::SessionStopped { wrote, .. } => wrote,
        other => panic!("{other:?}"),
    }
}

fn record(state: &mut State, id: &str, tree: char, sessions: &[&str], tail: &str) -> Outcome {
    run(
        state,
        id,
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "test".into(),
            command: "cargo test".into(),
            tree_oid: tree.to_string().repeat(40),
            head_oid: None,
            inputs: Vec::new(),
            result: CheckResult::Failed { exit_code: 101 },
            duration_ms: 900,
            symbol_findings: None,
            report: Some(Box::new(CheckReport {
                session_ids: sessions.iter().map(|s| (*s).to_owned()).collect(),
                tail: tail.into(),
                timed_out_s: None,
                interface: None,
                index_incomplete: false,
                dependent_paths: Vec::new(),
            })),
            tests: None,
            rerun: None,
            writes_before: None,
            command_id: None,
        },
        10,
    )
}

/// The automatic results a delivered briefing carried.
fn delivered(state: &mut State, session: &str, id: &str) -> Vec<String> {
    match run(
        state,
        id,
        Command::Brief {
            session_id: session.into(),
            max_peers: 6,
            deliver: true,
            mid_turn: true,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
            changes_only: false,
            confirm: false,
            remote_messages: false,
        },
        20,
    ) {
        Outcome::Briefing { briefing } => {
            if !briefing.checks.is_empty() {
                assert!(briefing.changed, "a result to deliver is news");
            }
            briefing
                .checks
                .into_iter()
                .map(|notice| notice.check_id)
                .collect()
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn automatic_checks_are_off_until_enabled_and_off_again_when_disabled() {
    let mut state = ready();
    assert!(state.auto_checks.is_empty());
    let outcome = enable(&mut state, &["cargo", "test"], 600);
    assert!(matches!(outcome, Outcome::AutoCheck { .. }), "{outcome:?}");
    let check = &state.auto_checks["work"];
    assert_eq!((check.name.as_str(), check.timeout_s), ("test", 600));
    assert_eq!(check.argv, ["cargo", "test"]);
    // The same configuration again records nothing new.
    let decision = decide(
        &state,
        &Command::AutoCheckEnable {
            workspace_id: "work".into(),
            name: "test".into(),
            argv: vec!["cargo".into(), "test".into()],
            timeout_s: 600,
            on_interface_change: None,
        },
        "again",
        2,
    );
    assert!(decision.events.is_empty());
    let off = run(
        &mut state,
        "off",
        Command::AutoCheckDisable {
            workspace_id: "work".into(),
        },
        3,
    );
    assert!(matches!(off, Outcome::AutoCheckDisabled { .. }), "{off:?}");
    assert!(state.auto_checks.is_empty());
}

#[test]
fn an_automatic_check_needs_a_command_a_time_limit_and_a_known_workspace() {
    let mut state = ready();
    for (argv, timeout, code) in [
        (vec![], 60, "invalid_check"),
        (vec![""], 60, "invalid_check"),
        (vec!["a\0b"], 60, "invalid_check"),
        (vec!["cargo"], 0, "invalid_limit"),
        (vec!["cargo"], 3601, "invalid_limit"),
    ] {
        match enable(&mut state, &argv, timeout) {
            Outcome::Error { code: actual, .. } => assert_eq!(actual, code, "{argv:?}"),
            other => panic!("{argv:?}: {other:?}"),
        }
    }
    let long = "x".repeat(1025);
    assert!(matches!(
        enable(&mut state, &[&long], 60),
        Outcome::Error { .. }
    ));
    let unknown = run(
        &mut state,
        "u",
        Command::AutoCheckEnable {
            workspace_id: "nowhere".into(),
            name: "test".into(),
            argv: vec!["true".into()],
            timeout_s: 60,
            on_interface_change: None,
        },
        1,
    );
    assert!(matches!(unknown, Outcome::Error { .. }), "{unknown:?}");
    assert!(state.auto_checks.is_empty());
}

#[test]
fn a_store_written_before_automatic_checks_still_opens() {
    let old = r#"{"generation":3,"workspaces":{},"sessions":{},"intents":{},"leases":{},
                  "activity":{"alice":{"session_id":"alice","generation":3,"last_ms":1}}}"#;
    let state: State = serde_json::from_str(old).unwrap();
    assert!(state.auto_checks.is_empty() && state.check_notices.is_empty());
    assert!(!state.activity["alice"].wrote_since_boundary);
}

#[test]
fn a_batch_or_stop_says_whether_the_session_wrote_since_the_last_one() {
    let mut state = ready();
    assert!(!batch(&mut state, "alice", "b0"), "nothing written yet");
    write(&mut state, "alice", "w1", "a.rs", ToolResult::Succeeded);
    // Another session's write is not Alice's.
    assert!(!batch(&mut state, "bob", "b-bob"));
    assert!(batch(&mut state, "alice", "b1"));
    assert!(!batch(&mut state, "alice", "b2"), "the batch consumed it");
    assert!(!stop(&mut state, "alice", "s1"));
    write(&mut state, "alice", "w2", "a.rs", ToolResult::Failed);
    assert!(
        !batch(&mut state, "alice", "b3"),
        "a failed write changed nothing"
    );
    write(&mut state, "alice", "w3", "a.rs", ToolResult::Succeeded);
    assert!(
        stop(&mut state, "alice", "s2"),
        "Codex has no batches: its Stop"
    );
    assert!(!stop(&mut state, "alice", "s3"));
}

#[test]
fn a_reported_result_is_a_check_record_and_is_delivered_once_to_each_session() {
    let mut state = ready();
    let outcome = record(
        &mut state,
        "auto-1",
        'a',
        &["alice", "bob"],
        "assertion failed",
    );
    assert!(
        matches!(outcome, Outcome::CheckRecorded { .. }),
        "{outcome:?}"
    );
    // Ordinary evidence about that exact tree.
    assert_eq!(state.checks["work"][0].check_id, "auto-1");
    assert_eq!(state.checks["work"][0].tree_oid, "a".repeat(40));
    // A look from outside delivers nothing.
    let look = run(
        &mut state,
        "look",
        Command::Brief {
            session_id: "alice".into(),
            max_peers: 6,
            deliver: false,
            mid_turn: false,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
            changes_only: false,
            confirm: false,
            remote_messages: false,
        },
        15,
    );
    let Outcome::Briefing { briefing } = look else {
        panic!("{look:?}")
    };
    assert_eq!(briefing.checks[0].tail, "assertion failed");
    assert_eq!(delivered(&mut state, "alice", "d1"), ["auto-1"]);
    assert!(delivered(&mut state, "alice", "d2").is_empty(), "once");
    assert_eq!(delivered(&mut state, "bob", "d3"), ["auto-1"]);
    assert!(state.check_notices.is_empty());
}

#[test]
fn only_the_newest_result_of_a_check_waits_for_a_session() {
    let mut state = ready();
    record(&mut state, "auto-1", 'a', &["alice"], "first");
    record(&mut state, "auto-2", 'b', &["alice"], "second");
    assert_eq!(delivered(&mut state, "alice", "d"), ["auto-2"]);
}

#[test]
fn a_report_names_only_sessions_of_the_checked_workspace_and_a_bounded_tail() {
    let mut state = ready();
    for (id, sessions, tail) in [
        ("x1", vec!["carol"], "t".to_owned()),
        ("x2", vec!["nobody"], "t".to_owned()),
        ("x3", vec![], "t".to_owned()),
        ("x4", vec!["alice"], "t".repeat(2049)),
    ] {
        let outcome = record(&mut state, id, 'a', &sessions, &tail);
        assert!(
            matches!(outcome, Outcome::Error { .. }),
            "{id}: {outcome:?}"
        );
    }
    assert!(
        state.checks.is_empty(),
        "the record and its report are one decision"
    );
}

#[test]
fn several_waiting_results_are_delivered_one_per_briefing_newest_first() {
    let mut state = ready();
    record(&mut state, "auto-1", 'a', &["alice"], "first");
    let lint = |state: &mut State| {
        run(
            state,
            "auto-2",
            Command::RecordCheck {
                workspace_id: "work".into(),
                name: "lint".into(),
                command: "cargo clippy".into(),
                tree_oid: "b".repeat(40),
                head_oid: None,
                inputs: Vec::new(),
                result: CheckResult::Passed {},
                duration_ms: 10,
                symbol_findings: None,
                report: Some(Box::new(CheckReport {
                    session_ids: vec!["alice".into()],
                    tail: String::new(),
                    timed_out_s: None,
                    interface: None,
                    index_incomplete: false,
                    dependent_paths: Vec::new(),
                })),
                tests: None,
                rerun: None,
                writes_before: None,
                command_id: None,
            },
            11,
        )
    };
    lint(&mut state);
    assert_eq!(delivered(&mut state, "alice", "d1"), ["auto-2"]);
    assert_eq!(delivered(&mut state, "alice", "d2"), ["auto-1"]);
    assert!(delivered(&mut state, "alice", "d3").is_empty());
}

#[test]
fn a_boundary_neither_revives_an_ended_session_nor_creates_its_activity() {
    let mut state = ready();
    assert!(!state.activity.contains_key("bob"));
    batch(&mut state, "bob", "b0");
    assert!(!state.activity.contains_key("bob"));
    write(&mut state, "alice", "w1", "a.rs", ToolResult::Succeeded);
    run(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "alice".into(),
            trail: None,
        },
        8,
    );
    stop(&mut state, "alice", "s1");
    assert!(state.activity["alice"].ended);
}

#[test]
fn a_report_that_disagrees_with_its_record_or_has_none_does_not_apply() {
    let mut state = ready();
    let notice = langspan_core::CheckNotice {
        check_id: "auto-9".into(),
        name: "test".into(),
        tree_oid: "a".repeat(40),
        result: CheckResult::Passed {},
        duration_ms: 900,
        timed_out_s: None,
        tail: String::new(),
        failures: Vec::new(),
        more_failures: false,
        baseline_check_id: None,
        fixed: Vec::new(),
        writers: Vec::new(),
        interface_grade: None,
        interface: None,
        index_incomplete: false,
    };
    let report = |notice: &langspan_core::CheckNotice| Event::CheckReported {
        session_ids: vec!["alice".into()],
        notice: notice.clone(),
    };
    assert!(
        apply(&mut state.clone(), &report(&notice)).is_err(),
        "no record"
    );
    record(&mut state, "auto-9", 'a', &["alice"], "x");
    // The record failed (exit 101); a notice saying it passed is refused.
    assert!(apply(&mut state, &report(&notice)).is_err());
}

#[test]
fn every_argument_of_an_automatic_check_is_one_to_1024_bytes_without_control_characters() {
    let mut state = ready();
    for argv in [vec!["make", ""], vec!["make", "a\nb"]] {
        assert!(
            matches!(enable(&mut state, &argv, 60), Outcome::Error { .. }),
            "{argv:?}"
        );
    }
}
