//! Messages to sessions on other machines (ADR 0041, issue #65): the
//! opt-in, the send checks, what the publication carries and for how long,
//! and delivery into the receiving sessions' briefings. In process, against
//! the pure core.
use langspan_core::{
    Briefing, Command, Event, Outcome, Publication, REMOTE_MESSAGE_MS, RemoteMessage, State, apply,
    decide, messages_bytes,
};

const ME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const THEM: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const ALICE: &str = "claude-code.alice-session";
const CAROL: &str = "codex.carol-session";

fn run(state: &mut State, log: &mut Vec<Event>, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
        log.push(event);
    }
    decision.outcome
}

fn ok(outcome: Outcome) -> Outcome {
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    outcome
}

fn code(outcome: Outcome) -> String {
    match outcome {
        Outcome::Error { code, .. } => code,
        other => panic!("expected an error: {other:?}"),
    }
}

fn enable(share_messages: bool) -> Command {
    Command::RemoteEnable {
        workspace_id: "work".into(),
        remote: "origin".into(),
        participant: ME.into(),
        name: "Alice".into(),
        share_task: false,
        publish_as_branch: false,
        share_messages,
    }
}

/// A workspace with two sessions; publication enabled when `link` is set.
fn ready(link: Option<bool>) -> (State, Vec<Event>) {
    let mut state = State::default();
    let mut log = vec![Event::Booted { generation: 1 }];
    apply(&mut state, &log[0]).unwrap();
    ok(run(
        &mut state,
        &mut log,
        "w",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        0,
    ));
    for session in [ALICE, CAROL] {
        register(&mut state, &mut log, session);
    }
    if let Some(share) = link {
        ok(run(&mut state, &mut log, "enable", enable(share), 0));
    }
    (state, log)
}

fn register(state: &mut State, log: &mut Vec<Event>, session: &str) {
    ok(run(
        state,
        log,
        &format!("s-{session}-{}", state.generation),
        Command::SessionRegister {
            session_id: session.into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        0,
    ));
}

fn send(text: &str, paths: &[&str], remote: bool) -> Command {
    send_as(ALICE, None, text, paths, remote)
}

fn send_as(from: &str, to: Option<&str>, text: &str, paths: &[&str], remote: bool) -> Command {
    Command::SendMessage {
        workspace_id: "work".into(),
        from: Some(from.into()),
        to: to.map(str::to_owned),
        text: text.into(),
        paths: paths.iter().map(|path| (*path).into()).collect(),
        remote,
    }
}

fn outbox(state: &mut State, log: &mut Vec<Event>, now: u64) -> Publication {
    match ok(run(
        state,
        log,
        "read",
        Command::RemoteOutbox {
            workspace_id: "work".into(),
        },
        now,
    )) {
        Outcome::RemoteOutbox { publication, .. } => publication,
        other => panic!("{other:?}"),
    }
}

fn brief(
    state: &mut State,
    log: &mut Vec<Event>,
    session: &str,
    id: &str,
    remote: bool,
) -> Briefing {
    match ok(run(
        state,
        log,
        id,
        Command::Brief {
            session_id: session.into(),
            max_peers: 8,
            deliver: true,
            mid_turn: false,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
            changes_only: false,
            confirm: false,
            remote_messages: remote,
        },
        1_000,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

fn message(id: &str, text: &str) -> RemoteMessage {
    RemoteMessage {
        message_id: id.into(),
        from: Some("codex.0b0b0b0b".into()),
        to: None,
        text: text.into(),
        paths: vec!["src/api/upload.rs".into()],
    }
}

fn theirs(sequence: u64, messages: Vec<RemoteMessage>) -> Publication {
    Publication {
        participant: THEM.into(),
        name: "Bob".into(),
        sequence,
        work: Vec::new(),
        withdrawn: false,
        messages,
    }
}

fn receive(state: &mut State, log: &mut Vec<Event>, id: &str, publication: Publication) -> Outcome {
    run(
        state,
        log,
        id,
        Command::ObserveRemote {
            workspace_id: "work".into(),
            publication,
        },
        500,
    )
}

fn texts(briefing: &Briefing) -> Vec<String> {
    briefing
        .remote_messages
        .iter()
        .map(|view| view.message.text.clone())
        .collect()
}

#[test]
fn a_remote_send_is_refused_until_the_owner_shares_messages_and_adds_nothing() {
    let (mut state, mut log) = ready(None);
    let before = log.len();
    let refused = run(&mut state, &mut log, "m1", send("Hold off", &[], true), 10);
    assert_eq!(code(refused), "remote_not_enabled");
    ok(run(&mut state, &mut log, "enable", enable(false), 10));
    let refused = run(&mut state, &mut log, "m1", send("Hold off", &[], true), 10);
    let Outcome::Error { code, message } = refused else {
        panic!()
    };
    assert_eq!(code, "remote_messages_off");
    assert!(message.contains("--share-messages on"), "{message}");
    assert_eq!(log.len(), before + 1, "only the enable was recorded");
    assert!(outbox(&mut state, &mut log, 10).messages.is_empty());
    // Without `remote`, the message stays local as before.
    ok(run(
        &mut state,
        &mut log,
        "m2",
        send("Local", &[], false),
        10,
    ));
    assert!(outbox(&mut state, &mut log, 10).messages.is_empty());
}

#[test]
fn each_enable_restates_the_choice_and_without_it_sharing_is_off() {
    let (mut state, mut log) = ready(Some(true));
    ok(run(
        &mut state,
        &mut log,
        "m1",
        send("Hold off", &[], true),
        10,
    ));
    assert_eq!(outbox(&mut state, &mut log, 20).messages.len(), 1);
    ok(run(&mut state, &mut log, "again", enable(false), 30));
    assert!(outbox(&mut state, &mut log, 40).messages.is_empty());
    let refused = run(&mut state, &mut log, "m2", send("Again", &[], true), 50);
    assert_eq!(code(refused), "remote_messages_off");
}

#[test]
fn a_shared_message_is_published_for_thirty_minutes_and_delivered_locally_too() {
    let (mut state, mut log) = ready(Some(true));
    let sent = ok(run(
        &mut state,
        &mut log,
        "m-20",
        send(
            "Changing the upload API in the next hour.",
            &["src/api/upload.rs"],
            true,
        ),
        1_000,
    ));
    assert!(matches!(sent, Outcome::MessageSent { message } if message.remote));
    let publication = outbox(&mut state, &mut log, 1_000);
    assert_eq!(
        publication.messages,
        vec![RemoteMessage {
            message_id: "m-20".into(),
            from: Some("claude-code.alice-se".into()),
            to: None,
            text: "Changing the upload API in the next hour.".into(),
            paths: vec!["src/api/upload.rs".into()],
        }]
    );
    // Still there just inside the window, gone just past it.
    let last = 1_000 + REMOTE_MESSAGE_MS - 1;
    assert_eq!(outbox(&mut state, &mut log, last).messages.len(), 1);
    assert!(outbox(&mut state, &mut log, last + 1).messages.is_empty());
    // The local session is told as for any message.
    let carol = brief(&mut state, &mut log, CAROL, "b", true);
    assert_eq!(carol.messages.len(), 1);
}

#[test]
fn a_redacted_secret_is_refused_and_a_message_to_one_session_cannot_go_remote() {
    let (mut state, mut log) = ready(Some(true));
    let refused = run(
        &mut state,
        &mut log,
        "m1",
        send("token [redacted] for staging", &[], true),
        10,
    );
    assert_eq!(code(refused), "message_secret_shape");
    let refused = run(
        &mut state,
        &mut log,
        "m2",
        send_as(ALICE, Some(CAROL), "Just you", &[], true),
        10,
    );
    assert_eq!(code(refused), "invalid_message");
    assert!(outbox(&mut state, &mut log, 10).messages.is_empty());
    // The same request ID works once the cause is fixed.
    ok(run(
        &mut state,
        &mut log,
        "m1",
        send("Clean now", &[], true),
        20,
    ));
}

#[test]
fn a_retraction_frees_no_slot_and_the_quota_survives_a_restart() {
    let (mut state, mut log) = ready(Some(true));
    for n in 0..8 {
        ok(run(
            &mut state,
            &mut log,
            &format!("m{n}"),
            send("Hold off", &[], true),
            100 + n,
        ));
    }
    ok(run(
        &mut state,
        &mut log,
        "r",
        Command::RetractMessage {
            workspace_id: "work".into(),
            from: Some(ALICE.into()),
            message_id: Some("m7".into()),
        },
        200,
    ));
    assert_eq!(
        outbox(&mut state, &mut log, 200).messages.len(),
        7,
        "it left"
    );
    let refused = run(&mut state, &mut log, "m8", send("Ninth", &[], true), 300);
    let Outcome::Error {
        code: refusal,
        message,
    } = refused
    else {
        panic!()
    };
    assert_eq!(refusal, "remote_messages_quota");
    assert!(message.contains("in 30 minutes"), "{message}");
    // A restart replays the log: the window still holds all eight.
    let mut replayed = State::default();
    for event in &log {
        apply(&mut replayed, event).unwrap();
    }
    apply(&mut replayed, &Event::Booted { generation: 2 }).unwrap();
    let refused = run(&mut replayed, &mut log, "m8", send("Ninth", &[], true), 400);
    assert_eq!(code(refused), "remote_messages_quota");
    // Once the first leaves the window, one more may go.
    ok(run(
        &mut replayed,
        &mut log,
        "m8",
        send("Ninth", &[], true),
        100 + REMOTE_MESSAGE_MS,
    ));
}

#[test]
fn messages_past_six_kibibytes_are_refused() {
    let (mut state, mut log) = ready(Some(true));
    let long: Vec<String> = (0..8)
        .map(|n| format!("src/{n}/{}.rs", "x".repeat(180)))
        .collect();
    let paths: Vec<&str> = long.iter().map(String::as_str).collect();
    let text = "t".repeat(400);
    let mut refused = None;
    for n in 0..8 {
        if let Outcome::Error { code, message } = run(
            &mut state,
            &mut log,
            &format!("m{n}"),
            send(&text, &paths, true),
            10,
        ) {
            refused = Some((n, code, message));
            break;
        }
    }
    let (n, code, message) = refused.expect("a send past 6 KiB is refused");
    assert_eq!(code, "remote_messages_full");
    assert!(message.contains("in 30 minutes"), "{message}");
    let published = outbox(&mut state, &mut log, 10).messages;
    assert_eq!(published.len(), n);
    assert!(messages_bytes(&published) <= 6 * 1024);
}

#[test]
fn the_size_of_messages_is_what_serde_json_writes() {
    let mut odd = message("m-1", "quote \" back \\ tab\u{7f} é");
    odd.to = Some(THEM.into());
    let mut owner = message("m-2", "plain");
    owner.from = None;
    owner.paths.clear();
    for messages in [vec![], vec![odd.clone()], vec![odd, owner]] {
        assert_eq!(
            messages_bytes(&messages),
            serde_json::to_vec(&messages).unwrap().len(),
            "{messages:?}"
        );
    }
}

#[test]
fn a_teammates_message_reaches_each_session_once_and_a_replay_delivers_nothing() {
    let (mut state, mut log) = ready(Some(true));
    ok(receive(
        &mut state,
        &mut log,
        "o1",
        theirs(1, vec![message("m-20", "Changing the upload API.")]),
    ));
    // A client that does not show them is never given them.
    assert!(
        brief(&mut state, &mut log, ALICE, "b0", false)
            .remote_messages
            .is_empty()
    );
    let alice = brief(&mut state, &mut log, ALICE, "b1", true);
    assert_eq!(texts(&alice), ["Changing the upload API."]);
    assert_eq!(alice.remote_messages[0].name, "Bob");
    assert_eq!(alice.remote_messages[0].participant, THEM);
    assert!(log.iter().any(|event| matches!(
        event,
        Event::RemoteMessageDelivered { session_id, message_id, text, .. }
            if session_id == ALICE && message_id == "m-20" && text == "Changing the upload API."
    )));
    assert!(
        brief(&mut state, &mut log, ALICE, "b2", true)
            .remote_messages
            .is_empty()
    );
    // Telling one session leaves it due for the other.
    assert_eq!(
        texts(&brief(&mut state, &mut log, CAROL, "b3", true)).len(),
        1
    );
    // The same or an older publication again delivers nothing twice.
    receive(
        &mut state,
        &mut log,
        "o2",
        theirs(1, vec![message("m-20", "Changing the upload API.")]),
    );
    receive(
        &mut state,
        &mut log,
        "o3",
        theirs(2, vec![message("m-20", "Changing the upload API.")]),
    );
    assert!(
        brief(&mut state, &mut log, ALICE, "b4", true)
            .remote_messages
            .is_empty()
    );
    // A restart rebuilds the delivered set from the log.
    let mut replayed = State::default();
    for event in &log {
        apply(&mut replayed, event).unwrap();
    }
    apply(&mut replayed, &Event::Booted { generation: 2 }).unwrap();
    register(&mut replayed, &mut log, ALICE);
    register(&mut replayed, &mut log, "claude-code.dave-session");
    ok(receive(
        &mut replayed,
        &mut log,
        "o4",
        theirs(3, vec![message("m-20", "Changing the upload API.")]),
    ));
    assert!(
        brief(&mut replayed, &mut log, ALICE, "b5", true)
            .remote_messages
            .is_empty()
    );
    // A session that registers while it is still held receives it.
    let dave = brief(
        &mut replayed,
        &mut log,
        "claude-code.dave-session",
        "b6",
        true,
    );
    assert_eq!(texts(&dave).len(), 1);
}

#[test]
fn at_most_two_remote_messages_per_briefing_and_local_ones_are_not_pushed_out() {
    let (mut state, mut log) = ready(Some(true));
    let mut elsewhere = message("m-x", "For another machine");
    elsewhere.to = Some("cccccccccccccccccccccccccccccccc".into());
    let mut mine = message("m-me", "For this machine");
    mine.to = Some(ME.into());
    ok(receive(
        &mut state,
        &mut log,
        "o1",
        theirs(
            1,
            vec![
                elsewhere,
                mine,
                message("m-1", "one"),
                message("m-2", "two"),
            ],
        ),
    ));
    for n in 0..3 {
        ok(run(
            &mut state,
            &mut log,
            &format!("local-{n}"),
            send_as(CAROL, None, &format!("local {n}"), &[], false),
            10,
        ));
    }
    let first = brief(&mut state, &mut log, ALICE, "b1", true);
    assert_eq!(first.messages.len(), 3, "every local message");
    assert_eq!(texts(&first).len(), 1, "four in all");
    let second = brief(&mut state, &mut log, ALICE, "b2", true);
    assert_eq!(texts(&second).len(), 2, "at most two remote");
    let third = brief(&mut state, &mut log, ALICE, "b3", true);
    let mut all = [texts(&first), texts(&second), texts(&third)].concat();
    all.sort();
    assert_eq!(
        all,
        ["For this machine", "one", "two"],
        "never the other machine's"
    );
}

#[test]
fn a_publication_with_an_invalid_message_is_rejected_whole() {
    let (mut state, mut log) = ready(Some(true));
    for bad in [
        message("m-1", "line\nbreak"),
        message("m-1", &"x".repeat(401)),
        RemoteMessage {
            to: Some("not-a-participant".into()),
            ..message("m-1", "ok")
        },
        RemoteMessage {
            paths: vec!["../outside".into()],
            ..message("m-1", "ok")
        },
    ] {
        let outcome = receive(&mut state, &mut log, "o", theirs(1, vec![bad.clone()]));
        assert!(matches!(outcome, Outcome::Error { .. }), "{bad:?}");
    }
    let nine = (0..9).map(|n| message(&format!("m-{n}"), "x")).collect();
    assert!(matches!(
        receive(&mut state, &mut log, "o", theirs(1, nine)),
        Outcome::Error { .. }
    ));
}
