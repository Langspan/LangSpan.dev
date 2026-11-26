//! Coordination policies (roadmap B28, ADR 0036): the table, the one
//! setting, the refusal a wait defers, and how a refusal is settled.
use langspan_core::{
    Command, Event, FileState, ObservedFile, Outcome, Policy, PolicyCase, RefusalKind,
    SettledOutcome, State, ToolIdentity, WaitResult, apply, decide,
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
            workspace_id: "app".into(),
            root: "/repos/app".into(),
        },
        0,
    );
    for session in ["alice", "bob"] {
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "app".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    state
}

fn admit(state: &mut State, id: &str, session: &str, path: &str, now: u64) -> Vec<Event> {
    let (_, events) = run(
        state,
        id,
        Command::AdmitTool {
            session_id: session.into(),
            operation: ToolIdentity {
                operation_id: format!("op-{id}"),
                source: "test".into(),
                tool: "Edit".into(),
                input_digest: "f".repeat(64),
            },
            paths: vec![path.into()],
            base: Vec::new(),
            regions: Vec::new(),
            overwrites: Vec::new(),
            ttl_ms: 30_000,
        },
        now,
    );
    events
}

fn set(
    state: &mut State,
    id: &str,
    case: PolicyCase,
    policy: Policy,
    wait_ms: Option<u32>,
) -> Outcome {
    run(
        state,
        id,
        Command::SetPolicy {
            workspace_id: "app".into(),
            case,
            policy,
            wait_ms,
        },
        0,
    )
    .0
}

fn table(state: &State) -> Vec<(PolicyCase, Policy, bool, u32)> {
    match decide(
        state,
        &Command::Policy {
            workspace_id: "app".into(),
        },
        "read",
        0,
    )
    .outcome
    {
        Outcome::Policy { rows, .. } => rows
            .into_iter()
            .map(|row| (row.case, row.policy, row.configurable, row.wait_ms))
            .collect(),
        other => panic!("{other:?}"),
    }
}

fn error_code(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Error { code, .. } => code,
        other => panic!("{other:?}"),
    }
}

fn refused(events: &[Event]) -> Vec<RefusalKind> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::WriteRefused { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect()
}

fn settled(events: &[Event]) -> Vec<(SettledOutcome, Option<u64>)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::RefusalSettled {
                outcome,
                ms_to_next_admitted,
                ..
            } => Some((*outcome, *ms_to_next_admitted)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_default_table_refuses_a_holder_and_fixes_every_other_case() {
    let state = ready();
    assert_eq!(
        table(&state),
        vec![
            (PolicyCase::Advisory, Policy::Advisory, false, 0),
            (PolicyCase::Conflict, Policy::Refuse, true, 0),
            (PolicyCase::StaleBase, Policy::Refuse, false, 0),
            (PolicyCase::UnreadLines, Policy::Refuse, false, 0),
            (PolicyCase::CommitHeld, Policy::Refuse, false, 0),
            (PolicyCase::LandingNotReady, Policy::Refuse, false, 0),
        ]
    );
}

#[test]
fn only_the_conflict_policy_can_be_set_and_within_its_bounds() {
    let mut state = ready();
    for case in [
        PolicyCase::Advisory,
        PolicyCase::StaleBase,
        PolicyCase::UnreadLines,
        PolicyCase::CommitHeld,
        PolicyCase::LandingNotReady,
    ] {
        for policy in [Policy::Advisory, Policy::Wait, Policy::Refuse] {
            let outcome = set(&mut state, "x", case, policy, None);
            let Outcome::Error { code, message } = &outcome else {
                panic!("{outcome:?}");
            };
            assert_eq!(code, "policy_fixed");
            assert!(message.contains(case.name()), "{message}");
            assert!(message.contains("fixed"), "{message}");
        }
    }
    let advisory = set(
        &mut state,
        "a",
        PolicyCase::Conflict,
        Policy::Advisory,
        None,
    );
    assert_eq!(error_code(&advisory), "invalid_policy");
    let long = set(
        &mut state,
        "l",
        PolicyCase::Conflict,
        Policy::Wait,
        Some(2_001),
    );
    assert_eq!(error_code(&long), "invalid_wait");
    let refuse = set(
        &mut state,
        "r",
        PolicyCase::Conflict,
        Policy::Refuse,
        Some(5),
    );
    assert_eq!(error_code(&refuse), "invalid_wait");
    assert_eq!(
        table(&state)[1],
        (PolicyCase::Conflict, Policy::Refuse, true, 0)
    );

    set(&mut state, "w1", PolicyCase::Conflict, Policy::Wait, None);
    assert_eq!(
        table(&state)[1],
        (PolicyCase::Conflict, Policy::Wait, true, 2_000)
    );
    set(
        &mut state,
        "w2",
        PolicyCase::Conflict,
        Policy::Wait,
        Some(400),
    );
    assert_eq!(
        table(&state)[1],
        (PolicyCase::Conflict, Policy::Wait, true, 400)
    );
    // A wait of zero is a refusal.
    set(
        &mut state,
        "w3",
        PolicyCase::Conflict,
        Policy::Wait,
        Some(0),
    );
    assert_eq!(
        table(&state)[1],
        (PolicyCase::Conflict, Policy::Refuse, true, 0)
    );
    let unknown = decide(
        &state,
        &Command::Policy {
            workspace_id: "nowhere".into(),
        },
        "read",
        0,
    );
    assert!(matches!(unknown.outcome, Outcome::Error { .. }));
}

#[test]
fn with_a_wait_the_conflict_refusal_is_recorded_by_the_wait_not_by_the_admission() {
    let mut state = ready();
    admit(&mut state, "a1", "alice", "src/api.rs", 10);
    assert_eq!(
        refused(&admit(&mut state, "b1", "bob", "src/api.rs", 20)),
        vec![RefusalKind::Conflict],
        "today's refusal, by default"
    );
    set(
        &mut state,
        "w",
        PolicyCase::Conflict,
        Policy::Wait,
        Some(1_000),
    );
    let events = admit(&mut state, "b2", "bob", "src/api.rs", 30);
    assert!(refused(&events).is_empty(), "{events:?}");

    // The hook polls while it waits: a read, recording nothing.
    let polled = decide(
        &state,
        &Command::Holders {
            session_id: "bob".into(),
            paths: vec!["src/api.rs".into()],
            base: Vec::new(),
            regions: Vec::new(),
        },
        "read",
        40,
    );
    assert!(polled.events.is_empty());
    let Outcome::Holders { holders } = polled.outcome else {
        panic!("{:?}", polled.outcome);
    };
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0].session_id, "alice");

    let (_, events) = run(
        &mut state,
        "waited-1",
        Command::RecordWait {
            session_id: "bob".into(),
            paths: vec!["src/api.rs".into()],
            holder_session_id: Some("alice".into()),
            waited_ms: 700,
            result: WaitResult::RefusedConflict,
            changed: Vec::new(),
            input_digest: Some("f".repeat(64)),
        },
        800,
    );
    assert!(matches!(
        &events[0],
        Event::WriteWaited {
            waited_ms: 700,
            result: WaitResult::RefusedConflict,
            ..
        }
    ));
    assert_eq!(refused(&events), vec![RefusalKind::Conflict]);
}

#[test]
fn a_file_changed_during_a_wait_is_refused_once_and_its_retry_is_admitted() {
    let mut state = ready();
    let now = ObservedFile {
        path: "src/api.rs".into(),
        state: FileState::Present {
            digest: "b".repeat(64),
            kind: Default::default(),
        },
    };
    let (_, events) = run(
        &mut state,
        "waited-1",
        Command::RecordWait {
            session_id: "bob".into(),
            paths: vec!["src/api.rs".into()],
            holder_session_id: Some("alice".into()),
            waited_ms: 300,
            result: WaitResult::RefusedStale,
            changed: vec![now.clone()],
            input_digest: None,
        },
        400,
    );
    assert_eq!(refused(&events), vec![RefusalKind::StaleBase]);
    assert!(events.iter().any(
        |event| matches!(event, Event::SourceObserved { files, .. } if files == std::slice::from_ref(&now))
    ));
    // The version Bob was told about is his known version: the same edit
    // again is compared with it and admitted.
    let (outcome, events) = run(
        &mut state,
        "b2",
        Command::AdmitTool {
            session_id: "bob".into(),
            operation: ToolIdentity {
                operation_id: "op-b2".into(),
                source: "test".into(),
                tool: "Edit".into(),
                input_digest: "f".repeat(64),
            },
            paths: vec!["src/api.rs".into()],
            base: vec![now],
            regions: Vec::new(),
            overwrites: Vec::new(),
            ttl_ms: 30_000,
        },
        1_400,
    );
    assert!(matches!(outcome, Outcome::Admitted { .. }), "{outcome:?}");
    assert_eq!(
        settled(&events),
        vec![(SettledOutcome::RetriedAdmitted, Some(1_000))]
    );

    // Changed files come only with a stale refusal, and only of the paths
    // waited for.
    for (result, path) in [
        (WaitResult::Admitted, "src/api.rs"),
        (WaitResult::RefusedStale, "src/other.rs"),
    ] {
        let outcome = decide(
            &state,
            &Command::RecordWait {
                session_id: "bob".into(),
                paths: vec!["src/api.rs".into()],
                holder_session_id: None,
                waited_ms: 1,
                result,
                changed: vec![ObservedFile {
                    path: path.into(),
                    state: FileState::Missing,
                }],
                input_digest: None,
            },
            "bad",
            1_500,
        )
        .outcome;
        assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    }
}

#[test]
fn a_refusal_is_settled_by_the_next_admitted_write_or_the_stop() {
    let mut state = ready();
    admit(&mut state, "a1", "alice", "src/api.rs", 10);
    admit(&mut state, "b1", "bob", "src/api.rs", 20);
    // Another file next: moved on, with no time to an admitted retry.
    let events = admit(&mut state, "b2", "bob", "src/other.rs", 50);
    assert_eq!(settled(&events), vec![(SettledOutcome::MovedOn, None)]);
    // Settled once: the next write settles nothing more.
    assert!(settled(&admit(&mut state, "b3", "bob", "src/third.rs", 60)).is_empty());

    admit(&mut state, "b4", "bob", "src/api.rs", 70);
    let (_, events) = run(
        &mut state,
        "stop",
        Command::SessionStop {
            session_id: "bob".into(),
            failure: None,
        },
        90,
    );
    assert_eq!(settled(&events), vec![(SettledOutcome::Stopped, None)]);
    let recorded = events
        .iter()
        .find(|event| matches!(event, Event::RefusalSettled { .. }))
        .unwrap();
    assert!(matches!(
        recorded,
        Event::RefusalSettled {
            kind: RefusalKind::Conflict,
            refused_at_ms: 70,
            ..
        }
    ));
    assert!(state.unsettled.is_empty());

    // A daemon restart leaves earlier refusals unsettled for good.
    admit(&mut state, "a2", "alice", "src/x.rs", 100);
    admit(&mut state, "b5", "bob", "src/x.rs", 110);
    assert!(!state.unsettled.is_empty());
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert!(state.unsettled.is_empty());
}
