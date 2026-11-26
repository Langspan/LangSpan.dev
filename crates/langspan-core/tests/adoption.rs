//! Taking over an ended session's uncommitted work (issue #161): who may
//! adopt whom, the recorded transfer, its reason, the recap, replay, and the
//! briefing that names an ended session's work left in the checkout.
use langspan_core::{
    AdoptReason, Command, Event, LeftWork, LivenessStatus, Outcome, ProcessIdentity, State, apply,
    decide,
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

fn events(state: &State, command: Command) -> Vec<Event> {
    decide(state, &command, "probe", 50).events
}

fn register(state: &mut State, session: &str, workspace: &str) {
    run(
        state,
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

fn prompt(state: &mut State, session: &str, summary: &str) {
    run(
        state,
        &format!("p-{session}"),
        Command::SessionPrompt {
            session_id: session.into(),
            summary: summary.into(),
            line_digests: Vec::new(),
        },
        1,
    );
}

fn end(state: &mut State, session: &str) {
    run(
        state,
        &format!("e-{session}"),
        Command::SessionEnd {
            session_id: session.into(),
        },
        2,
    );
}

/// Alice (ended), Bob and Carol in `work`; Dave (ended) in `other`.
fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (workspace, root) in [("work", "/repos/work"), ("other", "/repos/other")] {
        run(
            &mut state,
            &format!("w-{workspace}"),
            Command::WorkspaceRegister {
                workspace_id: workspace.into(),
                root: root.into(),
            },
            0,
        );
    }
    for session in ["alice", "bob", "carol"] {
        register(&mut state, session, "work");
    }
    register(&mut state, "dave", "other");
    prompt(&mut state, "alice", "Fix the upload handler");
    end(&mut state, "alice");
    end(&mut state, "dave");
    state
}

fn adopt(state: &mut State, session: &str, from: &str) -> Outcome {
    run(
        state,
        &format!("a-{session}-{from}"),
        Command::Adopt {
            session_id: session.into(),
            from: from.into(),
        },
        10,
    )
}

fn refused(outcome: Outcome) -> String {
    match outcome {
        Outcome::Error { code, .. } => code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_live_session_another_workspaces_and_ones_own_work_are_refused_and_nothing_is_recorded() {
    let mut state = ready();
    let before = state.clone();
    assert_eq!(refused(adopt(&mut state, "bob", "carol")), "session_live");
    assert_eq!(refused(adopt(&mut state, "bob", "dave")), "other_workspace");
    assert_eq!(refused(adopt(&mut state, "bob", "bob")), "invalid_adoption");
    assert_eq!(refused(adopt(&mut state, "alice", "bob")), "session_ended");
    assert_eq!(
        refused(adopt(&mut state, "bob", "nobody")),
        "session_not_found"
    );
    assert_eq!(state, before);
}

#[test]
fn a_session_whose_process_exited_can_be_adopted_without_an_end_report() {
    let mut state = ready();
    let liveness = |status| Command::ObserveSessionLiveness {
        session_id: "carol".into(),
        process: ProcessIdentity {
            pid: 4242,
            start_token: "boot-1".into(),
        },
        status,
    };
    run(&mut state, "l1", liveness(LivenessStatus::Alive), 3);
    assert_eq!(refused(adopt(&mut state, "bob", "carol")), "session_live");
    run(&mut state, "l2", liveness(LivenessStatus::Exited), 4);
    match adopt(&mut state, "bob", "carol") {
        Outcome::Adopted { recorded, .. } => assert!(recorded),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_adoption_is_one_event_naming_both_sessions_and_a_retry_records_nothing_more() {
    let mut state = ready();
    let command = Command::Adopt {
        session_id: "bob".into(),
        from: "alice".into(),
    };
    let recorded = events(&state, command.clone());
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    let Event::SessionAdopted { adoption } = &recorded[0] else {
        panic!("{recorded:?}");
    };
    assert_eq!(
        (adoption.from.as_str(), adoption.session_id.as_str()),
        ("alice", "bob")
    );
    assert_eq!(adoption.workspace_id, "work");
    match adopt(&mut state, "bob", "alice") {
        Outcome::Adopted { root, recorded, .. } => {
            assert!(recorded);
            assert_eq!(root, "/repos/work");
        }
        other => panic!("{other:?}"),
    }
    assert!(events(&state, command).is_empty());
    match adopt(&mut state, "bob", "alice") {
        Outcome::Adopted { recorded, .. } => assert!(!recorded),
        other => panic!("{other:?}"),
    }
    // Carol cannot take what Bob holds now; she asks Bob, or adopts Bob
    // once he has ended.
    assert_eq!(
        refused(adopt(&mut state, "carol", "alice")),
        "already_adopted"
    );
}

#[test]
fn the_reason_is_a_handoff_to_the_adopter_or_a_shared_task_line_and_neither_is_required() {
    // No link: adopted, without a reason.
    let mut state = ready();
    match adopt(&mut state, "bob", "alice") {
        Outcome::Adopted { adoption, .. } => assert_eq!(adoption.reason, None),
        other => panic!("{other:?}"),
    }

    // The same task line.
    let mut state = ready();
    prompt(&mut state, "bob", "Fix the upload handler");
    match adopt(&mut state, "bob", "alice") {
        Outcome::Adopted { adoption, .. } => {
            assert_eq!(adoption.reason, Some(AdoptReason::SameTask {}));
        }
        other => panic!("{other:?}"),
    }

    // A message from the ended session to the adopter, before it ended.
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
    register(&mut state, "alice", "work");
    register(&mut state, "bob", "work");
    run(
        &mut state,
        "m-1",
        Command::SendMessage {
            workspace_id: "work".into(),
            from: Some("alice".into()),
            to: Some("bob".into()),
            text: "Disk is nearly full; carry on with the upload handler".into(),
            paths: Vec::new(),
        },
        1,
    );
    end(&mut state, "alice");
    match adopt(&mut state, "bob", "alice") {
        Outcome::Adopted { adoption, .. } => assert_eq!(
            adoption.reason,
            Some(AdoptReason::Handoff {
                message_id: "m-1".into()
            })
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_recap_names_the_transfer_and_replay_gives_the_same_state() {
    let mut state = ready();
    let mut log = Vec::new();
    for (id, command) in [(
        "a-1",
        Command::Adopt {
            session_id: "bob".into(),
            from: "alice".into(),
        },
    )] {
        let decision = decide(&state, &command, id, 10);
        for event in decision.events {
            apply(&mut state, &event).unwrap();
            log.push(event);
        }
    }
    let recap = match run(
        &mut state,
        "read",
        Command::Recap {
            workspace_id: "work".into(),
        },
        20,
    ) {
        Outcome::Recap { recap } => recap,
        other => panic!("{other:?}"),
    };
    let session = |id: &str| {
        recap
            .sessions
            .iter()
            .find(|session| session.session_id == id)
            .unwrap()
            .clone()
    };
    assert_eq!(session("alice").adopted_by.as_deref(), Some("bob"));
    assert_eq!(session("bob").adopted, vec!["alice".to_owned()]);
    assert!(session("carol").adopted_by.is_none() && session("carol").adopted.is_empty());

    // The event survives as JSON and replays to the same adoption.
    let mut replayed = ready();
    for event in &log {
        let json = serde_json::to_string(event).unwrap();
        assert!(json.contains("\"type\":\"session_adopted\""), "{json}");
        apply(&mut replayed, &serde_json::from_str(&json).unwrap()).unwrap();
    }
    assert_eq!(replayed.adoptions, state.adoptions);
}

fn left_work(state: &mut State, session: &str, left: &[&str]) -> Vec<LeftWork> {
    match run(
        state,
        &format!("b-{session}-{}", left.join("-")),
        Command::Brief {
            session_id: session.into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
            mcp: false,
            left: left.iter().map(|id| (*id).to_owned()).collect(),
        },
        30,
    ) {
        Outcome::Briefing { briefing } => {
            if !briefing.left_work.is_empty() {
                assert!(briefing.changed, "left work is news");
            }
            briefing.left_work
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_briefing_names_only_ended_unadopted_sessions_of_its_workspace_with_their_task_line() {
    let mut state = ready();
    // Carol is live, Dave is of another workspace, Bob is the session
    // itself and "ghost" is not registered.
    let named = left_work(
        &mut state,
        "bob",
        &["alice", "carol", "dave", "bob", "ghost"],
    );
    assert_eq!(
        named,
        vec![LeftWork {
            session_id: "alice".into(),
            summary: Some("Fix the upload handler".into()),
        }]
    );
    // Without sessions named (a prompt, or nothing journaled), none.
    assert!(left_work(&mut state, "bob", &[]).is_empty());
    // Once adopted, Alice's work is nobody's to suggest.
    adopt(&mut state, "bob", "alice");
    assert!(left_work(&mut state, "carol", &["alice"]).is_empty());
}

#[test]
fn a_briefing_request_naming_too_many_sessions_is_refused() {
    let state = ready();
    let left: Vec<String> = (0..=langspan_core::MAX_LEFT)
        .map(|n| format!("s{n}"))
        .collect();
    let decision = decide(
        &state,
        &Command::Brief {
            session_id: "bob".into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
            mcp: false,
            left,
        },
        "b",
        30,
    );
    assert!(
        matches!(&decision.outcome, Outcome::Error { code, .. } if code == "invalid_left"),
        "{:?}",
        decision.outcome
    );
}

#[test]
fn removing_the_workspace_drops_its_adoptions() {
    let mut state = ready();
    adopt(&mut state, "bob", "alice");
    assert!(state.adoptions.contains_key("alice"));
    apply(
        &mut state,
        &Event::WorkspaceRemoved {
            workspace_id: "work".into(),
        },
    )
    .unwrap();
    assert!(state.adoptions.is_empty());
}
