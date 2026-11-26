//! Interface notices told to a running session between its steps (issue
//! #214): once, only while its turn is open, never again in a briefing.
use langspan_core::{
    Briefing, Command, Event, InterfaceChange, NoticeVia, Outcome, State, ToolResult,
    acting_session, apply, decide,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> (Outcome, Vec<Event>) {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    assert!(
        !matches!(decision.outcome, Outcome::Error { .. }),
        "{:?}",
        decision.outcome
    );
    (decision.outcome, decision.events)
}

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    run(
        &mut state,
        "w",
        Command::WorkspaceRegister {
            workspace_id: "main".into(),
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
                workspace_id: "main".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    state
}

fn edited(state: &mut State, session: &str, path: &str, now: u64) {
    run(
        state,
        &format!("o-{session}-{now}-{}", path.replace('/', "_")),
        Command::Observe {
            session_id: session.into(),
            operation_id: format!("op-{session}-{now}"),
            source: "test".into(),
            tool: "Edit".into(),
            paths: vec![path.into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        now,
    );
}

/// Bob changes `save` in `src/store.rs`, which `dependents` mention.
fn notice(state: &mut State, id: &str, dependents: &[&str], now: u64) {
    edited(state, "bob", "src/store.rs", now - 1);
    run(
        state,
        id,
        Command::ObserveInterface {
            workspace_id: "main".into(),
            path: "src/store.rs".into(),
            changes: vec![InterfaceChange {
                name: "save".into(),
                kind: "function".into(),
                change: "signature_changed".into(),
                before: Some("fn save(x: &Item)".into()),
                after: Some("fn save(x: &Item, sync: bool)".into()),
                after_name: None,
                before_name: None,
                evidence: None,
                before_path: None,
                private: false,
                before_arity: None,
                after_arity: None,
                before_count: None,
                after_count: None,
            }],
            dependents: dependents.iter().map(|path| (*path).into()).collect(),
        },
        now,
    );
}

fn between(
    state: &mut State,
    id: &str,
    stops: Option<u64>,
    now: u64,
) -> (bool, u64, usize, Vec<Event>) {
    let command = Command::BetweenSteps {
        session_id: "alice".into(),
        stops,
    };
    assert_eq!(
        acting_session(&command),
        None,
        "a waiting watcher is not the session at work"
    );
    match run(state, id, command, now) {
        (
            Outcome::BetweenSteps {
                open,
                stops,
                interface_changes,
                ..
            },
            events,
        ) => (open, stops, interface_changes.len(), events),
        other => panic!("{other:?}"),
    }
}

fn brief(state: &mut State, id: &str, now: u64) -> Briefing {
    match run(
        state,
        id,
        Command::Brief {
            session_id: "alice".into(),
            max_peers: 8,
            deliver: true,
            mid_turn: true,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
        },
        now,
    )
    .0
    {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

fn told(events: &[Event]) -> Vec<(Vec<String>, NoticeVia)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::InterfaceNoticesTold {
                notice_ids, via, ..
            } => Some((notice_ids.clone(), *via)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_notice_on_a_written_file_is_told_once_between_steps_and_recorded() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    let (open, stops, count, events) = between(&mut state, "p1", None, 2_000);
    assert!(open);
    assert_eq!((stops, count), (0, 0), "nothing to tell yet");
    assert!(
        events.is_empty(),
        "a poll with nothing to tell writes nothing"
    );
    notice(&mut state, "n1", &["src/api.rs"], 3_000);
    let (open, _, count, events) = between(&mut state, "p2", Some(stops), 4_000);
    assert!(open);
    assert_eq!(count, 1);
    let told = told(&events);
    assert_eq!(told.len(), 1, "{events:?}");
    assert_eq!(told[0].1, NoticeVia::BetweenSteps);
    // A second watcher, or a retry under a new request ID, tells nothing.
    let (_, _, count, events) = between(&mut state, "p3", Some(stops), 5_000);
    assert_eq!(count, 0);
    assert!(events.is_empty());
}

#[test]
fn a_notice_told_between_steps_is_left_out_of_the_next_briefing() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    notice(&mut state, "n1", &["src/api.rs"], 2_000);
    let (_, _, count, _) = between(&mut state, "p1", None, 3_000);
    assert_eq!(count, 1);
    let briefing = brief(&mut state, "b1", 4_000);
    assert!(briefing.interface_changes.is_empty(), "{briefing:?}");
}

#[test]
fn a_notice_told_in_a_briefing_is_not_told_between_steps() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    notice(&mut state, "n1", &["src/api.rs"], 2_000);
    let briefing = brief(&mut state, "b1", 3_000);
    assert_eq!(briefing.interface_changes.len(), 1);
    let (_, _, count, events) = between(&mut state, "p1", None, 4_000);
    assert_eq!(count, 0);
    assert!(events.is_empty());
}

#[test]
fn a_briefing_records_the_notices_it_told() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    notice(&mut state, "n1", &["src/api.rs"], 2_000);
    let decision = decide(
        &state,
        &Command::Brief {
            session_id: "alice".into(),
            max_peers: 8,
            deliver: true,
            mid_turn: false,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
        },
        "b1",
        3_000,
    );
    let told = told(&decision.events);
    assert_eq!(told.len(), 1, "{:?}", decision.events);
    assert_eq!(told[0].1, NoticeVia::Briefing);
}

#[test]
fn a_stop_closes_the_turn_and_nothing_is_told_after_it() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    let (_, stops, _, _) = between(&mut state, "p1", None, 2_000);
    run(
        &mut state,
        "stop",
        Command::SessionStop {
            session_id: "alice".into(),
            failure: None,
        },
        3_000,
    );
    notice(&mut state, "n1", &["src/api.rs"], 4_000);
    let (open, _, count, events) = between(&mut state, "p2", Some(stops), 5_000);
    assert!(!open, "the turn the watcher started in is over");
    assert_eq!(count, 0, "an idle session is not woken");
    assert!(events.is_empty());
    // The next prompt's briefing still carries it.
    assert_eq!(brief(&mut state, "b1", 6_000).interface_changes.len(), 1);
}

#[test]
fn an_unrelated_notice_or_an_ended_session_is_not_told() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    notice(&mut state, "n1", &["src/other.rs"], 2_000);
    let (open, _, count, _) = between(&mut state, "p1", None, 3_000);
    assert!(open);
    assert_eq!(count, 0, "alice's files do not mention the changed name");
    notice(&mut state, "n2", &["src/api.rs"], 4_000);
    run(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "alice".into(),
            trail: None,
        },
        5_000,
    );
    let decision = decide(
        &state,
        &Command::BetweenSteps {
            session_id: "alice".into(),
            stops: None,
        },
        "p2",
        6_000,
    );
    assert!(decision.events.is_empty());
    assert!(
        matches!(
            decision.outcome,
            Outcome::BetweenSteps { open: false, ref interface_changes, .. } if interface_changes.is_empty()
        ),
        "{:?}",
        decision.outcome
    );
}

#[test]
fn the_notice_told_event_survives_a_round_trip() {
    let event = Event::InterfaceNoticesTold {
        session_id: "alice".into(),
        notice_ids: vec!["n1".into()],
        via: NoticeVia::BetweenSteps,
    };
    let wire = serde_json::to_string(&event).unwrap();
    assert_eq!(
        wire,
        r#"{"type":"interface_notices_told","session_id":"alice","notice_ids":["n1"],"via":"between_steps"}"#
    );
    assert_eq!(serde_json::from_str::<Event>(&wire).unwrap(), event);
}
