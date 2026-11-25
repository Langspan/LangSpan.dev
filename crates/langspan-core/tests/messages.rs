//! Messages between sessions and handoffs: who receives what, exactly once,
//! within the bounds, and the same after replay.
use langspan_core::{
    Command, Event, MAX_MESSAGES, Message, Outcome, Recipient, State, apply, decide,
};

/// Decide and apply, keeping every persisted event for the replay check.
fn run(state: &mut State, log: &mut Vec<Event>, id: &str, command: Command) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, 10);
    assert_eq!(*state, before, "decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
        log.push(event);
    }
    decision.outcome
}

fn ready() -> (State, Vec<Event>) {
    let mut state = State::default();
    let mut log = vec![Event::Booted { generation: 1 }];
    apply(&mut state, &log[0]).unwrap();
    for (workspace, root) in [("work", "/repos/work"), ("other", "/repos/other")] {
        run(
            &mut state,
            &mut log,
            &format!("w-{workspace}"),
            Command::WorkspaceRegister {
                workspace_id: workspace.into(),
                root: root.into(),
            },
        );
    }
    for (session, workspace) in [
        ("alice", "work"),
        ("bob", "work"),
        ("carol", "work"),
        ("dave", "other"),
    ] {
        run(
            &mut state,
            &mut log,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: workspace.into(),
                parent: None,
                headless: false,
            },
        );
    }
    (state, log)
}

fn send(from: Option<&str>, to: Option<&str>, text: &str, paths: &[&str]) -> Command {
    Command::SendMessage {
        workspace_id: "work".into(),
        from: from.map(str::to_owned),
        to: to.map(str::to_owned),
        text: text.into(),
        paths: paths.iter().map(|path| (*path).into()).collect(),
    }
}

fn brief(session: &str, deliver: bool, mid_turn: bool) -> Command {
    Command::Brief {
        session_id: session.into(),
        max_peers: 4,
        deliver,
        mid_turn,
        recent: false,
        recent_window_ms: None,
    }
}

/// The messages a delivered briefing carried, by text.
fn delivered(state: &mut State, log: &mut Vec<Event>, session: &str, id: &str) -> Vec<String> {
    received(state, log, id, brief(session, true, false))
}

fn received(state: &mut State, log: &mut Vec<Event>, id: &str, command: Command) -> Vec<String> {
    match run(state, log, id, command) {
        Outcome::Briefing { briefing } => briefing
            .messages
            .into_iter()
            .map(|message| message.text)
            .collect(),
        other => panic!("{other:?}"),
    }
}

fn error_code(outcome: Outcome) -> String {
    match outcome {
        Outcome::Error { code, .. } => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn a_direct_message_reaches_its_recipient_once_and_no_one_else() {
    let (mut state, mut log) = ready();
    let sent = run(
        &mut state,
        &mut log,
        "m1",
        send(
            None,
            Some("bob"),
            "Please leave src/api.rs to me",
            &["src/api.rs"],
        ),
    );
    let Outcome::MessageSent { message } = sent else {
        panic!("{sent:?}");
    };
    assert_eq!(message.message_id, "m1", "the request ID names the message");
    assert_eq!(message.from, None, "from the owner");
    assert_eq!(
        message.to,
        Recipient::Session {
            session_id: "bob".into()
        }
    );
    assert!(delivered(&mut state, &mut log, "alice", "b-a").is_empty());
    assert!(delivered(&mut state, &mut log, "carol", "b-c").is_empty());
    assert_eq!(
        delivered(&mut state, &mut log, "bob", "b-b1"),
        ["Please leave src/api.rs to me"]
    );
    assert!(
        delivered(&mut state, &mut log, "bob", "b-b2").is_empty(),
        "delivered once"
    );
    assert!(
        state.messages.get("work").is_none_or(Vec::is_empty),
        "an acknowledged message leaves state"
    );
}

#[test]
fn a_message_for_everyone_reaches_each_other_session_once_and_never_its_sender() {
    let (mut state, mut log) = ready();
    run(
        &mut state,
        &mut log,
        "m-all",
        send(Some("alice"), None, "Rebasing main at noon", &[]),
    );
    assert!(
        delivered(&mut state, &mut log, "alice", "b-a").is_empty(),
        "a sender is never delivered its own message"
    );
    assert!(
        delivered(&mut state, &mut log, "dave", "b-d").is_empty(),
        "another workspace hears nothing"
    );
    assert_eq!(
        delivered(&mut state, &mut log, "bob", "b-b1"),
        ["Rebasing main at noon"]
    );
    assert!(delivered(&mut state, &mut log, "bob", "b-b2").is_empty());
    assert_eq!(
        state.messages["work"][0].acknowledged,
        ["bob"],
        "held until every other session has it"
    );
    assert_eq!(
        delivered(&mut state, &mut log, "carol", "b-c1"),
        ["Rebasing main at noon"]
    );
    assert!(delivered(&mut state, &mut log, "carol", "b-c2").is_empty());
    assert!(
        state.messages.get("work").is_none_or(Vec::is_empty),
        "removed once every other session acknowledged it"
    );
}

#[test]
fn recipients_and_senders_must_be_sessions_of_that_workspace() {
    let (state, _) = ready();
    let refuse = |command: Command| error_code(decide(&state, &command, "m", 1).outcome);
    assert_eq!(
        refuse(send(None, Some("nobody"), "hi", &[])),
        "recipient_not_found"
    );
    assert_eq!(
        refuse(send(None, Some("dave"), "hi", &[])),
        "recipient_not_found",
        "a session of another workspace is not a recipient here"
    );
    assert_eq!(
        refuse(send(Some("nobody"), None, "hi", &[])),
        "session_not_found"
    );
    assert_eq!(
        refuse(send(Some("dave"), Some("bob"), "hi", &[])),
        "sender_not_in_workspace"
    );
    assert_eq!(
        refuse(send(Some("bob"), Some("bob"), "hi", &[])),
        "invalid_message"
    );
    assert_eq!(
        refuse(Command::SendMessage {
            workspace_id: "nowhere".into(),
            from: None,
            to: None,
            text: "hi".into(),
            paths: Vec::new(),
        }),
        "workspace_not_found"
    );
    assert_eq!(
        refuse(Command::Handoff {
            session_id: "nobody".into(),
            text: "hi".into(),
            paths: Vec::new(),
        }),
        "session_not_found"
    );
}

#[test]
fn messages_are_one_bounded_line_about_at_most_eight_normalized_files() {
    let (mut state, mut log) = ready();
    let nine = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for (text, paths) in [
        ("", &[][..]),
        ("   ", &[][..]),
        ("two\nlines", &[][..]),
        ("tab\there", &[][..]),
        (&"x".repeat(401)[..], &[][..]),
        ("ok", &["../escape"][..]),
        ("ok", &["/abs/path"][..]),
        ("ok", &nine[..]),
    ] {
        let outcome = decide(&state, &send(None, Some("bob"), text, paths), "bad", 1).outcome;
        assert!(
            matches!(outcome, Outcome::Error { .. }),
            "{text:?} {paths:?}"
        );
    }
    let handoff = Command::Handoff {
        session_id: "alice".into(),
        text: "two\nlines".into(),
        paths: Vec::new(),
    };
    assert_eq!(
        error_code(decide(&state, &handoff, "bad", 1).outcome),
        "invalid_message"
    );
    let longest = "é".repeat(200);
    assert_eq!(longest.len(), 400);
    let sent = run(
        &mut state,
        &mut log,
        "m-ok",
        send(
            None,
            Some("bob"),
            &longest,
            &["src/b.rs", "src/a.rs", "src/a.rs"],
        ),
    );
    let Outcome::MessageSent { message } = sent else {
        panic!("{sent:?}");
    };
    assert_eq!(message.paths, ["src/a.rs", "src/b.rs"], "sorted, once each");
}

#[test]
fn at_most_sixty_four_unacknowledged_messages_are_held_and_the_oldest_go_first() {
    let (mut state, mut log) = ready();
    for index in 0..70 {
        run(
            &mut state,
            &mut log,
            &format!("m{index}"),
            send(None, Some("bob"), &format!("message {index}"), &[]),
        );
    }
    let held = &state.messages["work"];
    assert_eq!(held.len(), MAX_MESSAGES);
    assert_eq!(held[0].message_id, "m69", "newest first");
    assert_eq!(held[63].message_id, "m6", "m0 to m5 were dropped");
    // A briefing carries at most four, newest first; the rest wait.
    assert_eq!(
        delivered(&mut state, &mut log, "bob", "b1"),
        ["message 69", "message 68", "message 67", "message 66"]
    );
    assert_eq!(state.messages["work"].len(), 60);
    assert_eq!(
        delivered(&mut state, &mut log, "bob", "b2")[0],
        "message 65"
    );
}

#[test]
fn a_briefing_carries_only_as_many_messages_as_its_text_can_hold() {
    let (mut state, mut log) = ready();
    // Quotes double when quoted: four of these cannot all fit the text.
    for index in 0..4 {
        run(
            &mut state,
            &mut log,
            &format!("q{index}"),
            send(None, Some("bob"), &"\"".repeat(400), &[]),
        );
    }
    let first = delivered(&mut state, &mut log, "bob", "b1").len();
    assert!((2..4).contains(&first), "{first}");
    let second = delivered(&mut state, &mut log, "bob", "b2").len();
    assert_eq!(first + second, 4, "what did not fit comes next time");
    assert!(state.messages.get("work").is_none_or(Vec::is_empty));
}

#[test]
fn acknowledging_removes_a_message_once_and_only_for_its_recipient() {
    let (mut state, mut log) = ready();
    run(
        &mut state,
        &mut log,
        "m1",
        send(Some("alice"), Some("bob"), "hi bob", &[]),
    );
    let ack = |session: &str| Command::AckMessage {
        session_id: session.into(),
        message_id: "m1".into(),
    };
    let decision = decide(&state, &ack("carol"), "a0", 1);
    assert!(decision.events.is_empty(), "not addressed to carol");
    assert!(matches!(
        decision.outcome,
        Outcome::MessageAcknowledged {
            acknowledged: false,
            ..
        }
    ));
    assert!(decide(&state, &ack("alice"), "a0", 1).events.is_empty());
    assert!(matches!(
        run(&mut state, &mut log, "a1", ack("bob")),
        Outcome::MessageAcknowledged {
            acknowledged: true,
            ..
        }
    ));
    assert!(state.messages.get("work").is_none_or(Vec::is_empty));
    let again = decide(&state, &ack("bob"), "a2", 1);
    assert!(again.events.is_empty(), "idempotent");
    assert!(delivered(&mut state, &mut log, "bob", "b").is_empty());
    assert_eq!(
        error_code(decide(&state, &ack("nobody"), "a3", 1).outcome),
        "session_not_found"
    );
}

#[test]
fn a_look_from_outside_shows_messages_without_acknowledging_them() {
    let (mut state, mut log) = ready();
    run(
        &mut state,
        &mut log,
        "m1",
        send(None, Some("bob"), "hi", &[]),
    );
    for id in ["look-1", "look-2"] {
        let decision = decide(&state, &brief("bob", false, false), id, 1);
        assert!(decision.events.is_empty(), "a look records nothing");
        let Outcome::Briefing { briefing } = decision.outcome else {
            panic!();
        };
        assert_eq!(briefing.messages.len(), 1);
    }
    assert_eq!(delivered(&mut state, &mut log, "bob", "b1"), ["hi"]);
    assert!(delivered(&mut state, &mut log, "bob", "b2").is_empty());
}

#[test]
fn a_new_message_is_a_change_and_a_delivered_one_does_not_make_the_next_briefing_new() {
    let (mut state, mut log) = ready();
    let changed = |state: &mut State, log: &mut Vec<Event>, id: &str| match run(
        state,
        log,
        id,
        brief("bob", true, false),
    ) {
        Outcome::Briefing { briefing } => briefing.changed,
        other => panic!("{other:?}"),
    };
    assert!(!changed(&mut state, &mut log, "b0"), "nothing to say");
    run(
        &mut state,
        &mut log,
        "m1",
        send(None, Some("bob"), "hi", &[]),
    );
    assert!(changed(&mut state, &mut log, "b1"), "a message is news");
    assert!(
        !changed(&mut state, &mut log, "b2"),
        "once delivered it is not repeated, and nothing else is either"
    );
}

#[test]
fn a_handoff_goes_to_exactly_the_first_other_session_that_starts_or_prompts() {
    let (mut state, mut log) = ready();
    let handed = run(
        &mut state,
        &mut log,
        "h1",
        Command::Handoff {
            session_id: "alice".into(),
            text: "Login form half done; tests in tests/login.rs fail".into(),
            paths: vec!["tests/login.rs".into()],
        },
    );
    let Outcome::MessageSent { message } = handed else {
        panic!("{handed:?}");
    };
    assert_eq!(message.to, Recipient::NextSession {});
    assert_eq!(message.from.as_deref(), Some("alice"));
    assert!(
        delivered(&mut state, &mut log, "alice", "b-a").is_empty(),
        "not to its sender"
    );
    assert!(
        received(&mut state, &mut log, "b-mid", brief("bob", true, true)).is_empty(),
        "a session in the middle of a turn does not pick it up"
    );
    assert!(
        delivered(&mut state, &mut log, "dave", "b-d").is_empty(),
        "nor a session of another workspace"
    );
    assert_eq!(
        delivered(&mut state, &mut log, "carol", "b-c"),
        ["Login form half done; tests in tests/login.rs fail"]
    );
    assert!(delivered(&mut state, &mut log, "bob", "b-b").is_empty());
    assert!(delivered(&mut state, &mut log, "carol", "b-c2").is_empty());
    assert!(state.messages.get("work").is_none_or(Vec::is_empty));
    // Leases, intents and activity are untouched by a handoff.
    assert!(state.intents.is_empty() && state.leases.is_empty());
}

#[test]
fn replaying_the_persisted_events_reproduces_the_same_state() {
    let (mut state, mut log) = ready();
    run(
        &mut state,
        &mut log,
        "m1",
        send(None, Some("bob"), "one", &[]),
    );
    run(
        &mut state,
        &mut log,
        "m2",
        send(Some("alice"), None, "two", &["src/a.rs"]),
    );
    run(
        &mut state,
        &mut log,
        "h1",
        Command::Handoff {
            session_id: "carol".into(),
            text: "three".into(),
            paths: Vec::new(),
        },
    );
    delivered(&mut state, &mut log, "bob", "b1");
    let mut replayed = State::default();
    for event in &log {
        let wire = serde_json::to_string(event).unwrap();
        let event: Event = serde_json::from_str(&wire).unwrap();
        apply(&mut replayed, &event).unwrap();
    }
    assert_eq!(replayed, state);
    let held: Vec<&Message> = state.messages["work"].iter().collect();
    assert_eq!(held.len(), 1, "only the broadcast is still owed to carol");
    assert_eq!(held[0].message_id, "m2");
    // A sent event cannot arrive already acknowledged or malformed.
    let mut forged = held[0].clone();
    forged.message_id = "forged".into();
    forged.text = "two\nlines".into();
    assert!(apply(&mut replayed, &Event::MessageSent { message: forged }).is_err());
}

#[test]
fn a_recap_lists_the_messages_still_owed_newest_first_and_bounded() {
    let (mut state, mut log) = ready();
    for index in 0..20 {
        run(
            &mut state,
            &mut log,
            &format!("m{index}"),
            send(None, Some("bob"), &format!("message {index}"), &[]),
        );
    }
    let recap = match run(
        &mut state,
        &mut log,
        "read",
        Command::Recap {
            workspace_id: "work".into(),
        },
    ) {
        Outcome::Recap { recap } => recap,
        other => panic!("{other:?}"),
    };
    assert_eq!(recap.messages.len(), 16);
    assert_eq!(recap.messages[0].message_id, "m19");
}

mod headless_handoffs {
    use super::*;

    fn register(state: &mut State, session: &str, parent: Option<&str>, headless: bool) {
        let decision = decide(
            state,
            &Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "w".into(),
                parent: parent.map(str::to_owned),
                headless,
            },
            &format!("reg-{session}"),
            0,
        );
        for event in decision.events {
            apply(state, &event).unwrap();
        }
    }

    fn handoff_from(state: &mut State, from: &str) {
        let decision = decide(
            state,
            &Command::Handoff {
                session_id: from.into(),
                text: "carry on with the parser".into(),
                paths: Vec::new(),
            },
            &format!("handoff-{from}"),
            0,
        );
        for event in decision.events {
            apply(state, &event).unwrap();
        }
    }

    fn takes(state: &State, session: &str) -> bool {
        match decide(
            state,
            &Command::Brief {
                session_id: session.into(),
                max_peers: 4,
                deliver: false,
                mid_turn: false,
                recent: false,
                recent_window_ms: None,
            },
            "look",
            0,
        )
        .outcome
        {
            Outcome::Briefing { briefing } => !briefing.messages.is_empty(),
            other => panic!("{other:?}"),
        }
    }

    fn workspace() -> State {
        let mut state = State::default();
        apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
        let decision = decide(
            &state,
            &Command::WorkspaceRegister {
                workspace_id: "w".into(),
                root: "/repos/app".into(),
            },
            "ws",
            0,
        );
        for event in decision.events {
            apply(&mut state, &event).unwrap();
        }
        state
    }

    #[test]
    fn a_handoff_from_a_person_waits_for_an_interactive_session() {
        let mut state = workspace();
        register(&mut state, "alice", None, false);
        register(&mut state, "script", None, true);
        register(&mut state, "carol", None, false);
        register(&mut state, "carol.agent", Some("carol"), false);
        handoff_from(&mut state, "alice");
        assert!(!takes(&state, "script"), "headless");
        assert!(
            !takes(&state, "carol.agent"),
            "a subagent works for its parent"
        );
        assert!(takes(&state, "carol"));
    }

    #[test]
    fn a_headless_session_takes_a_handoff_written_by_a_headless_session() {
        // The next step of the same script, with a person's session open too.
        let mut state = workspace();
        register(&mut state, "carol", None, false);
        register(&mut state, "step-1", None, true);
        register(&mut state, "step-2", None, true);
        handoff_from(&mut state, "step-1");
        assert!(takes(&state, "step-2"));
        assert!(takes(&state, "carol"), "whichever comes first takes it");
    }
}
