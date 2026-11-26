//! The wider check after an interface change (issue #71): the owner's
//! second command, its grade from the result, and who is told.
use langspan_core::{
    Arity, CheckNotice, CheckReport, CheckResult, Command, Event, InterfaceCause, InterfaceChange,
    InterfaceGrade, Outcome, State, ToolIdentity, ToolResult, WiderCheck, apply, decide,
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
    for (session, workspace) in [
        ("alice", "work"),
        ("bob", "work"),
        ("dave", "work"),
        ("carol", "other"),
    ] {
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

fn wider(name: &str) -> Option<WiderCheck> {
    Some(WiderCheck {
        name: name.into(),
        argv: vec!["cargo".into(), "test".into(), "--workspace".into()],
        timeout_s: 600,
    })
}

fn enable(state: &mut State, on_interface_change: Option<WiderCheck>) -> Outcome {
    run(
        state,
        "auto",
        Command::AutoCheckEnable {
            workspace_id: "work".into(),
            name: "fast".into(),
            argv: vec!["cargo".into(), "test".into(), "-p".into(), "core".into()],
            timeout_s: 60,
            on_interface_change: on_interface_change.map(Box::new),
        },
        1,
    )
}

/// A successful write of `path` by `session`, in its own workspace.
fn write(state: &mut State, session: &str, id: &str, path: &str) {
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
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        5,
    );
    assert!(matches!(outcome, Outcome::Observed { .. }), "{outcome:?}");
}

fn cause(dependents: &[&str]) -> InterfaceCause {
    InterfaceCause {
        path: "core/src/lib.rs".into(),
        change: InterfaceChange {
            name: "parse_upload".into(),
            kind: "function".into(),
            change: "signature_changed".into(),
            before: Some("pub fn parse_upload(a: u32, b: u32) -> u32".into()),
            after: Some("pub fn parse_upload(a: u32) -> u32".into()),
            before_arity: Some(Arity {
                min: 2,
                max: Some(2),
                receiver: false,
            }),
            after_arity: Some(Arity {
                min: 1,
                max: Some(1),
                receiver: false,
            }),
            before_count: None,
            after_count: None,
            after_name: None,
            before_name: None,
            evidence: None,
            before_path: None,
            private: false,
        },
        more_changes: 0,
        dependents: dependents.iter().map(|path| (*path).to_owned()).collect(),
        more_dependents: 0,
        blind_files: 0,
    }
}

fn record(
    state: &mut State,
    id: &str,
    result: CheckResult,
    interface: Option<InterfaceCause>,
) -> Result<Outcome, String> {
    record_with(state, id, result, interface, Vec::new())
}

/// As [`record`], with every mentioning file the run was for in
/// `dependent_paths`.
fn record_with(
    state: &mut State,
    id: &str,
    result: CheckResult,
    interface: Option<InterfaceCause>,
    dependent_paths: Vec<String>,
) -> Result<Outcome, String> {
    let command = Command::RecordCheck {
        workspace_id: "work".into(),
        name: "all".into(),
        command: "cargo test --workspace".into(),
        tree_oid: "a".repeat(40),
        head_oid: None,
        inputs: Vec::new(),
        result,
        duration_ms: 4000,
        report: Some(Box::new(CheckReport {
            session_ids: vec!["alice".into()],
            tail: "error[E0061]".into(),
            timed_out_s: None,
            interface: interface.map(Box::new),
            index_incomplete: false,
            dependent_paths,
        })),
        tests: None,
        rerun: None,
        writes_before: None,
        command_id: None,
    };
    let decision = decide(state, &command, id, 10);
    if let Outcome::Error { code, message } = &decision.outcome {
        return Err(format!("{code}: {message}"));
    }
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    Ok(decision.outcome)
}

fn waiting(state: &State, session: &str) -> Vec<CheckNotice> {
    state
        .check_notices
        .get(session)
        .cloned()
        .unwrap_or_default()
}

#[test]
fn the_wider_command_is_stored_with_the_check_and_needs_a_name_of_its_own() {
    let mut state = ready();
    let outcome = enable(&mut state, wider("all"));
    assert!(matches!(outcome, Outcome::AutoCheck { .. }), "{outcome:?}");
    let check = &state.auto_checks["work"];
    assert_eq!(check.on_interface_change, wider("all"));
    // Replacing it with none removes it.
    enable(&mut state, None);
    assert_eq!(state.auto_checks["work"].on_interface_change, None);
    // The same name as the regular check, an empty command or a time limit
    // out of range is refused, and nothing changes.
    for bad in [
        wider("fast"),
        Some(WiderCheck {
            argv: Vec::new(),
            ..wider("all").unwrap()
        }),
        Some(WiderCheck {
            timeout_s: 0,
            ..wider("all").unwrap()
        }),
        Some(WiderCheck {
            argv: vec!["bad\narg".into()],
            ..wider("all").unwrap()
        }),
    ] {
        let outcome = enable(&mut state, bad.clone());
        assert!(
            matches!(outcome, Outcome::Error { .. }),
            "{bad:?}: {outcome:?}"
        );
    }
    assert_eq!(state.auto_checks["work"].on_interface_change, None);
}

#[test]
fn a_check_enabled_before_wider_checks_existed_still_replays() {
    let mut state = ready();
    let event: Event = serde_json::from_value(serde_json::json!({
        "type": "auto_check_enabled",
        "check": {"workspace_id": "work", "name": "fast", "argv": ["true"], "timeout_s": 60}
    }))
    .unwrap();
    apply(&mut state, &event).unwrap();
    assert_eq!(state.auto_checks["work"].on_interface_change, None);
    // And a check without one is written as it was before.
    let written = serde_json::to_value(&state.auto_checks["work"]).unwrap();
    assert!(written.get("on_interface_change").is_none(), "{written}");
}

#[test]
fn a_failing_wider_check_grades_the_change_broke_and_tells_the_sessions_on_mentioning_files() {
    let mut state = ready();
    enable(&mut state, wider("all"));
    // Bob works on a file that mentions the changed export; dave on another
    // file; carol in another workspace on a file of the same name.
    write(&mut state, "bob", "b1", "cli/src/main.rs");
    write(&mut state, "dave", "d1", "docs/notes.md");
    write(&mut state, "carol", "c1", "cli/src/main.rs");
    record(
        &mut state,
        "all-1",
        CheckResult::Failed { exit_code: 101 },
        Some(cause(&["cli/src/main.rs", "web/client.ts"])),
    )
    .unwrap();
    for session in ["alice", "bob"] {
        let notices = waiting(&state, session);
        assert_eq!(notices.len(), 1, "{session}");
        assert_eq!(notices[0].interface_grade, Some(InterfaceGrade::Broke));
        assert_eq!(
            notices[0].interface.as_ref().unwrap().dependents,
            ["cli/src/main.rs", "web/client.ts"]
        );
    }
    assert!(waiting(&state, "dave").is_empty());
    assert!(waiting(&state, "carol").is_empty());
}

#[test]
fn a_session_on_a_mentioning_file_past_the_shown_eight_is_still_told() {
    let mut state = ready();
    enable(&mut state, wider("all"));
    // Nine files mention the change; the cause shows the first eight, and
    // bob works only on the ninth.
    let all: Vec<String> = (0..9).map(|n| format!("src/f{n}.rs")).collect();
    write(&mut state, "bob", "b1", &all[8]);
    let shown: Vec<&str> = all[..8].iter().map(String::as_str).collect();
    let mut cause = cause(&shown);
    cause.more_dependents = 1;
    record_with(
        &mut state,
        "all-1",
        CheckResult::Failed { exit_code: 101 },
        Some(cause),
        all.clone(),
    )
    .unwrap();
    let notices = waiting(&state, "bob");
    assert_eq!(notices.len(), 1, "bob works on the ninth mentioning file");
    assert_eq!(notices[0].interface_grade, Some(InterfaceGrade::Broke));
    // The notice still shows eight files and counts the rest.
    let shown = notices[0].interface.as_ref().unwrap();
    assert_eq!(shown.dependents.len(), 8);
    assert_eq!(shown.more_dependents, 1);
}

#[test]
fn dependent_paths_are_bounded_valid_and_only_with_an_interface_change() {
    let mut state = ready();
    enable(&mut state, wider("all"));
    let too_many: Vec<String> = (0..=langspan_core::MAX_REPORT_DEPENDENT_PATHS)
        .map(|n| format!("src/f{n}.rs"))
        .collect();
    for (interface, paths) in [
        (Some(cause(&["cli/src/main.rs"])), too_many),
        (
            Some(cause(&["cli/src/main.rs"])),
            vec!["../outside.rs".into()],
        ),
        (None, vec!["cli/src/main.rs".into()]),
    ] {
        let refused = record_with(
            &mut state,
            "bad",
            CheckResult::Failed { exit_code: 1 },
            interface,
            paths.clone(),
        );
        assert!(refused.is_err(), "{paths:?}");
    }
    assert!(state.checks.get("work").is_none_or(Vec::is_empty));
}

#[test]
fn a_passing_wider_check_grades_the_change_kept_and_an_inconclusive_one_is_not_graded() {
    let mut state = ready();
    enable(&mut state, wider("all"));
    record(
        &mut state,
        "all-1",
        CheckResult::Passed {},
        Some(cause(&["cli/src/main.rs"])),
    )
    .unwrap();
    assert_eq!(
        waiting(&state, "alice")[0].interface_grade,
        Some(InterfaceGrade::Kept)
    );
    record(
        &mut state,
        "all-2",
        CheckResult::Inconclusive {},
        Some(cause(&["cli/src/main.rs"])),
    )
    .unwrap();
    let notice = &waiting(&state, "alice")[0];
    assert_eq!(notice.interface_grade, None);
    assert!(notice.interface.is_some());
    // A regular check is never graded.
    record(
        &mut state,
        "fast-1",
        CheckResult::Failed { exit_code: 1 },
        None,
    )
    .unwrap();
    assert_eq!(waiting(&state, "alice")[0].interface_grade, None);
}

#[test]
fn a_cause_names_one_to_eight_valid_mentioning_files_and_a_valid_change() {
    let mut state = ready();
    let nine: Vec<String> = (0..9).map(|n| format!("src/f{n}.rs")).collect();
    let nine: Vec<&str> = nine.iter().map(String::as_str).collect();
    let mut bad_change = cause(&["cli/src/main.rs"]);
    bad_change.change.change = "renamed".into();
    let mut bad_path = cause(&["cli/src/main.rs"]);
    bad_path.path = "/etc/passwd".into();
    for bad in [
        cause(&[]),
        cause(&nine),
        cause(&["../outside.rs"]),
        bad_change,
        bad_path,
    ] {
        let refused = record(
            &mut state,
            "bad",
            CheckResult::Failed { exit_code: 1 },
            Some(bad.clone()),
        );
        assert!(refused.is_err(), "{bad:?}");
    }
    assert!(state.checks.get("work").is_none_or(Vec::is_empty));
}

#[test]
fn a_reported_grade_that_disagrees_with_the_result_does_not_apply() {
    let mut state = ready();
    record(
        &mut state,
        "all-1",
        CheckResult::Failed { exit_code: 101 },
        Some(cause(&["cli/src/main.rs"])),
    )
    .unwrap();
    let record = state.checks["work"][0].clone();
    let notice = |grade| CheckNotice {
        check_id: record.check_id.clone(),
        name: record.name.clone(),
        tree_oid: record.tree_oid.clone(),
        result: record.result.clone(),
        duration_ms: record.duration_ms,
        timed_out_s: None,
        tail: String::new(),
        failures: Vec::new(),
        more_failures: false,
        baseline_check_id: None,
        fixed: Vec::new(),
        writers: Vec::new(),
        interface_grade: grade,
        interface: Some(Box::new(cause(&["cli/src/main.rs"]))),
        index_incomplete: false,
    };
    for grade in [Some(InterfaceGrade::Kept), None] {
        let event = Event::CheckReported {
            session_ids: vec!["alice".into()],
            notice: notice(grade),
        };
        assert!(apply(&mut state.clone(), &event).is_err(), "{grade:?}");
    }
    let event = Event::CheckReported {
        session_ids: vec!["alice".into()],
        notice: notice(Some(InterfaceGrade::Broke)),
    };
    apply(&mut state, &event).unwrap();
}
