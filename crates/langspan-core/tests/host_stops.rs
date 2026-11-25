//! The end of a host's reply (Stop, StopFailure, SubagentStop) and the end
//! of a batch of tool calls (PostToolBatch). A stop releases what the
//! session's own tool calls still hold; a batch is recorded as a boundary.
use langspan_core::{
    Command, Event, LeaseStatus, Outcome, State, ToolIdentity, apply, decide, expiry_events,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> (Outcome, Vec<Event>) {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    (decision.outcome, decision.events)
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
    for session in ["alice", "bob"] {
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    state
}

fn tool_admission(state: &mut State, session: &str, id: &str, path: &str, now: u64) {
    let (outcome, _) = run(
        state,
        id,
        Command::AdmitTool {
            session_id: session.into(),
            paths: vec![path.into()],
            ttl_ms: 30_000,
            operation: ToolIdentity {
                operation_id: format!("op-{id}"),
                source: "test".into(),
                tool: "Write".into(),
                input_digest: "a".repeat(64),
            },
            base: Vec::new(),
            regions: Vec::new(),
            overwrites: Vec::new(),
        },
        now,
    );
    assert!(matches!(outcome, Outcome::Admitted { .. }), "{outcome:?}");
}

fn stop(state: &mut State, id: &str, session: &str, failure: Option<&str>, now: u64) -> Outcome {
    run(
        state,
        id,
        Command::SessionStop {
            session_id: session.into(),
            failure: failure.map(str::to_owned),
        },
        now,
    )
    .0
}

#[test]
fn a_stop_releases_only_the_sessions_own_open_tool_admissions() {
    let mut state = ready();
    // Open: its post-tool event never came (for example, the owner refused
    // the call at a permission prompt).
    tool_admission(&mut state, "alice", "alice-open", "a.txt", 10);
    // Finished normally before the stop.
    tool_admission(&mut state, "alice", "alice-done", "b.txt", 11);
    run(
        &mut state,
        "c",
        Command::Complete {
            session_id: "alice".into(),
            lease_id: "alice-done".into(),
        },
        12,
    );
    // Asked for by hand (CLI or MCP): not a tool call, so not this turn's.
    let (manual, _) = run(
        &mut state,
        "alice-manual",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["c.txt".into()],
            ttl_ms: 30_000,
        },
        13,
    );
    assert!(matches!(manual, Outcome::Admitted { .. }), "{manual:?}");
    tool_admission(&mut state, "bob", "bob-open", "d.txt", 14);

    let (outcome, events) = run(
        &mut state,
        "stop-1",
        Command::SessionStop {
            session_id: "alice".into(),
            failure: None,
        },
        20,
    );
    let Outcome::SessionStopped {
        session_id,
        released,
        wrote: false,
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(session_id, "alice");
    assert_eq!(released, ["alice-open"]);
    assert_eq!(
        events,
        [
            Event::LeaseCompleted {
                lease_id: "alice-open".into()
            },
            Event::SessionStopped {
                session_id: "alice".into(),
                released: vec!["alice-open".into()],
                failure: None,
                at_ms: 20,
                generation: 1,
            },
        ]
    );
    let status = |id: &str| state.leases[id].status.clone();
    assert_eq!(status("alice-open"), LeaseStatus::Completed);
    assert_eq!(status("alice-manual"), LeaseStatus::Active);
    assert_eq!(status("bob-open"), LeaseStatus::Active);

    // A second stop finds nothing open, and still marks the boundary.
    let Outcome::SessionStopped { released, .. } = stop(&mut state, "stop-2", "alice", None, 30)
    else {
        panic!();
    };
    assert!(released.is_empty());
}

#[test]
fn an_admission_already_past_its_deadline_is_left_to_expire() {
    let mut state = ready();
    tool_admission(&mut state, "alice", "late", "a.txt", 0);
    let Outcome::SessionStopped { released, .. } = stop(&mut state, "stop", "alice", None, 40_000)
    else {
        panic!();
    };
    assert!(released.is_empty(), "an expired lease is not completed");
    assert_eq!(
        expiry_events(&state, 40_000),
        [Event::LeaseExpired {
            lease_id: "late".into()
        }]
    );
}

#[test]
fn a_failed_stop_records_its_error_type_and_refuses_free_text() {
    let mut state = ready();
    tool_admission(&mut state, "alice", "open", "a.txt", 1);
    let (_, events) = run(
        &mut state,
        "stop",
        Command::SessionStop {
            session_id: "alice".into(),
            failure: Some("rate_limit".into()),
        },
        2,
    );
    assert!(
        matches!(events.last(), Some(Event::SessionStopped { failure: Some(f), released, .. })
            if f == "rate_limit" && released == &["open".to_owned()]),
        "{events:?}"
    );
    for bad in [
        "",
        "API Error: Rate limit reached",
        "Rate_Limit",
        &"a".repeat(41),
    ] {
        let outcome = stop(
            &mut state,
            &format!("bad-{}", bad.len()),
            "alice",
            Some(bad),
            3,
        );
        assert!(
            matches!(&outcome, Outcome::Error { code, .. } if code == "invalid_failure"),
            "{bad:?}: {outcome:?}"
        );
    }
}

#[test]
fn a_stop_from_a_session_never_registered_changes_nothing() {
    let mut state = ready();
    let before = state.clone();
    let outcome = stop(&mut state, "stop", "nobody", None, 1);
    assert!(
        matches!(&outcome, Outcome::Error { code, .. } if code == "session_not_found"),
        "{outcome:?}"
    );
    assert_eq!(state, before);
}

#[test]
fn a_tool_batch_is_recorded_with_the_operations_it_contained() {
    let mut state = ready();
    let batch = |ops: Vec<String>, calls: u32| Command::ObserveToolBatch {
        session_id: "alice".into(),
        operation_ids: ops,
        calls,
    };
    let (outcome, events) = run(
        &mut state,
        "batch-1",
        batch(vec!["op-a".into(), "op-b".into()], 2),
        5,
    );
    assert_eq!(
        outcome,
        Outcome::ToolBatchObserved {
            session_id: "alice".into(),
            calls: 2,
            wrote: false,
        }
    );
    assert_eq!(
        events,
        [Event::ToolBatchObserved {
            session_id: "alice".into(),
            operation_ids: vec!["op-a".into(), "op-b".into()],
            calls: 2,
            at_ms: 5,
            generation: 1,
        }]
    );
    // More calls than the operations named is allowed: only the first 64
    // are named. The opposite, duplicates, too many or a bad ID are refused.
    let (outcome, _) = run(&mut state, "batch-2", batch(vec!["op-c".into()], 200), 6);
    assert!(matches!(
        outcome,
        Outcome::ToolBatchObserved { calls: 200, .. }
    ));
    let many: Vec<String> = (0..65).map(|n| format!("op-{n}")).collect();
    for (id, command) in [
        ("bad-1", batch(vec!["op-a".into(), "op-b".into()], 1)),
        ("bad-2", batch(vec!["op-a".into(), "op-a".into()], 2)),
        ("bad-3", batch(many, 65)),
        ("bad-4", batch(vec!["bad id".into()], 1)),
        ("bad-5", batch(Vec::new(), 0)),
    ] {
        let (outcome, events) = run(&mut state, id, command, 7);
        assert!(
            matches!(&outcome, Outcome::Error { code, .. } if code == "invalid_batch"),
            "{id}: {outcome:?}"
        );
        assert!(events.is_empty());
    }
}
