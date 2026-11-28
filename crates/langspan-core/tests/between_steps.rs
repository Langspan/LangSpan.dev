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
    notice_on(state, id, "src/store.rs", dependents, now);
}

/// Bob changes `save` in `path`.
fn notice_on(state: &mut State, id: &str, path: &str, dependents: &[&str], now: u64) {
    edited(state, "bob", path, now - 1);
    run(
        state,
        id,
        Command::ObserveInterface {
            workspace_id: "main".into(),
            path: path.into(),
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
    between_for(state, id, stops, None, now)
}

/// A watcher's ask, started for the prompt `prompt`.
fn between_for(
    state: &mut State,
    id: &str,
    stops: Option<u64>,
    prompt: Option<&str>,
    now: u64,
) -> (bool, u64, usize, Vec<Event>) {
    let command = Command::BetweenSteps {
        session_id: "alice".into(),
        stops,
        prompt_id: prompt.map(str::to_owned),
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

/// A hook's briefing, delivered: it records no notice itself.
fn brief(state: &mut State, id: &str, now: u64) -> Briefing {
    let (outcome, events) = run(
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
            changes_only: false,
            confirm: false,
        },
        now,
    );
    assert!(
        told(&events).is_empty(),
        "a hook reports what it showed: {events:?}"
    );
    match outcome {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

/// The hook reports the notices its rendered briefing showed.
fn shown(state: &mut State, id: &str, notice_ids: &[&str], now: u64) -> Vec<Event> {
    run(
        state,
        id,
        Command::InterfaceShown {
            session_id: "alice".into(),
            notice_ids: notice_ids.iter().map(|id| (*id).into()).collect(),
        },
        now,
    )
    .1
}

/// The stop of alice's reply to `prompt`.
fn stop(state: &mut State, id: &str, prompt: Option<&str>, now: u64) {
    run(
        state,
        id,
        Command::SessionStop {
            session_id: "alice".into(),
            failure: None,
            prompt_id: prompt.map(str::to_owned),
        },
        now,
    );
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
    assert_eq!(briefing.interface_notice_ids, ["n1"]);
    let told_events = shown(&mut state, "s1", &["n1"], 3_500);
    assert_eq!(
        told(&told_events),
        [(vec!["n1".into()], NoticeVia::Briefing)]
    );
    let (_, _, count, events) = between(&mut state, "p1", None, 4_000);
    assert_eq!(count, 0);
    assert!(events.is_empty());
    // Reported again, by a retry or a second hook, it is not recorded twice.
    assert!(shown(&mut state, "s2", &["n1"], 4_500).is_empty());
}

/// Review comment 4712432083: a hook's briefing can leave a notice out of
/// its capped text. One its hook did not report as shown is still owed.
#[test]
fn a_notice_a_hook_briefing_carried_but_did_not_show_is_told_between_steps() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    edited(&mut state, "alice", "src/cli.rs", 1_001);
    notice(&mut state, "n1", &["src/api.rs"], 2_000);
    notice_on(&mut state, "n2", "src/queue.rs", &["src/cli.rs"], 2_100);
    let briefing = brief(&mut state, "b1", 3_000);
    assert_eq!(briefing.interface_notice_ids.len(), 2, "{briefing:?}");
    assert_eq!(
        briefing.interface_notice_ids.len(),
        briefing.interface_changes.len()
    );
    // The hook's text showed only the first.
    let first = briefing.interface_notice_ids[0].clone();
    shown(&mut state, "s1", &[&first], 3_500);
    let (open, _, count, events) = between(&mut state, "p1", None, 4_000);
    assert!(open);
    assert_eq!(count, 1, "the notice left out is told between steps");
    let told = told(&events);
    assert_eq!(told.len(), 1);
    assert!(!told[0].0.contains(&first), "{told:?}");
    assert_eq!(told[0].1, NoticeVia::BetweenSteps);
}

/// A briefing the hook showed none of records nothing: the next watcher
/// tells every notice, and the next briefing still carries them.
#[test]
fn a_hook_briefing_alone_records_no_notice_as_told() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    notice(&mut state, "n1", &["src/api.rs"], 2_000);
    assert_eq!(brief(&mut state, "b1", 3_000).interface_changes.len(), 1);
    let (_, _, count, _) = between(&mut state, "p1", None, 4_000);
    assert_eq!(count, 1);
}

#[test]
fn a_shown_report_names_one_to_four_valid_notices_of_a_current_session() {
    let mut state = ready();
    let ask = |session: &str, ids: Vec<String>| Command::InterfaceShown {
        session_id: session.into(),
        notice_ids: ids,
    };
    for (command, code) in [
        (ask("alice", Vec::new()), "invalid_told"),
        (
            ask("alice", (0..5).map(|n| format!("n{n}")).collect()),
            "invalid_told",
        ),
        (ask("alice", vec!["bad id".into()]), "invalid_request_id"),
        (ask("nobody", vec!["n1".into()]), "session_not_found"),
    ] {
        let decision = decide(&state, &command, "x", 1_000);
        assert!(decision.events.is_empty());
        assert!(
            matches!(&decision.outcome, Outcome::Error { code: got, .. } if got == code),
            "{command:?}: {:?}",
            decision.outcome
        );
    }
    // Four is the most one briefing tells, and is accepted.
    let four: Vec<String> = (0..4).map(|n| format!("n{n}")).collect();
    let (_, events) = run(&mut state, "ok", ask("alice", four), 1_000);
    assert_eq!(told(&events).len(), 1);
}

#[test]
fn a_briefing_asked_for_through_mcp_records_the_notices_it_told() {
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
            mcp: true,
            changes_only: false,
            confirm: false,
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
    stop(&mut state, "stop", None, 3_000);
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
            prompt_id: None,
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

/// Review comment 4712602568: the reply to the watcher's own prompt
/// finished before the watcher's first answer. Its first answer must not
/// take the stop count after that reply as the start of an open turn.
#[test]
fn a_stop_of_the_watchers_own_prompt_before_its_first_answer_closes_its_turn() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    stop(&mut state, "stop", Some("prompt-1"), 2_000);
    notice(&mut state, "n1", &["src/api.rs"], 3_000);
    let (open, _, count, events) = between_for(&mut state, "p1", None, Some("prompt-1"), 4_000);
    assert!(!open, "its own prompt's reply finished");
    assert_eq!(count, 0, "an idle session is not woken");
    assert!(events.is_empty());
    // A later ask, with the count of that first answer, stays closed.
    let (open, _, _, _) = between_for(&mut state, "p2", Some(1), Some("prompt-1"), 5_000);
    assert!(!open);
    // The next prompt's briefing still carries the notice.
    assert_eq!(brief(&mut state, "b1", 6_000).interface_changes.len(), 1);
}

/// The stop of the reply before the watcher's prompt leaves its turn open,
/// and the stop of its own prompt closes it, whatever count it holds.
#[test]
fn an_earlier_prompts_stop_leaves_a_watcher_open_and_its_own_closes_it() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    stop(&mut state, "stop-0", Some("prompt-0"), 2_000);
    let (open, stops, _, _) = between_for(&mut state, "p1", None, Some("prompt-1"), 3_000);
    assert!(open, "the stop before this prompt is not its turn's end");
    assert_eq!(stops, 1);
    stop(&mut state, "stop-1", Some("prompt-1"), 4_000);
    notice(&mut state, "n1", &["src/api.rs"], 5_000);
    let (open, _, count, _) = between_for(&mut state, "p2", Some(stops), Some("prompt-1"), 6_000);
    assert!(!open);
    assert_eq!(count, 0);
    // A watcher with no prompt ID still ends on the count alone.
    let (open, _, _, _) = between_for(&mut state, "p3", Some(stops), None, 6_100);
    assert!(!open);
}

/// Only the newest `MAX_STOPPED_PROMPTS` prompts are remembered, a
/// repeated stop is held once, and an invalid prompt ID is refused.
#[test]
fn finished_prompts_are_bounded_held_once_and_validated() {
    use langspan_core::MAX_STOPPED_PROMPTS;
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    for n in 0..=MAX_STOPPED_PROMPTS {
        stop(
            &mut state,
            &format!("stop-{n}"),
            Some(&format!("prompt-{n}")),
            2_000 + n as u64,
        );
    }
    stop(
        &mut state,
        "stop-again",
        Some(&format!("prompt-{MAX_STOPPED_PROMPTS}")),
        3_000,
    );
    let held = &state.activity["alice"].stopped_prompts;
    assert_eq!(held.len(), MAX_STOPPED_PROMPTS, "{held:?}");
    assert_eq!(held[0], format!("prompt-{MAX_STOPPED_PROMPTS}"));
    assert!(!held.contains(&"prompt-0".to_owned()));
    for command in [
        Command::SessionStop {
            session_id: "alice".into(),
            failure: None,
            prompt_id: Some("bad id".into()),
        },
        Command::BetweenSteps {
            session_id: "alice".into(),
            stops: None,
            prompt_id: Some("bad id".into()),
        },
    ] {
        let decision = decide(&state, &command, "x", 4_000);
        assert!(decision.events.is_empty());
        assert!(
            matches!(&decision.outcome, Outcome::Error { code, .. } if code == "invalid_request_id"),
            "{:?}",
            decision.outcome
        );
    }
}

/// A stop recorded before prompt IDs were kept still reads, as a stop of
/// no particular prompt.
#[test]
fn an_older_stop_record_without_a_prompt_id_still_reads() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    let older: Event = serde_json::from_str(
        r#"{"type":"session_stopped","session_id":"alice","released":[],"failure":null,"at_ms":2000,"generation":1}"#,
    )
    .unwrap();
    assert!(matches!(
        older,
        Event::SessionStopped {
            prompt_id: None,
            ..
        }
    ));
    apply(&mut state, &older).unwrap();
    assert_eq!(state.activity["alice"].stops, 1);
    assert!(state.activity["alice"].stopped_prompts.is_empty());
    // And a new one round-trips with its prompt.
    let event = Event::SessionStopped {
        session_id: "alice".into(),
        released: Vec::new(),
        failure: None,
        at_ms: 3_000,
        generation: 1,
        prompt_id: Some("prompt-1".into()),
    };
    let wire = serde_json::to_string(&event).unwrap();
    assert!(wire.ends_with(r#""prompt_id":"prompt-1"}"#), "{wire}");
    assert_eq!(serde_json::from_str::<Event>(&wire).unwrap(), event);
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
