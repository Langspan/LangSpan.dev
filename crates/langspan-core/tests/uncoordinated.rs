//! A change on disk no session reported, in a checkout with a registered
//! Codex session: a Codex session resumed under another path for the same
//! directory runs none of LangSpan's hooks (issue #129). The scan reports the
//! changed files; the core keeps those no session explains and tells each
//! session of that checkout once, naming the Codex session.
use langspan_core::{
    BRIEF_HORIZON_MS, Briefing, Command, Event, FileState, ObservedFile, Outcome, SourceOrigin,
    State, apply, decide,
};
use serde_json::json;

const CODEX: &str = "codex.0198c0de-0000-7000-8000-000000000001";
const CLAUDE: &str = "claude-code.00000000-0000-4000-8000-00000000000a";
const ELSEWHERE: &str = "claude-code.00000000-0000-4000-8000-00000000000b";

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

fn ok(outcome: Outcome) -> Outcome {
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    outcome
}

fn register(state: &mut State, session: &str, workspace: &str) {
    ok(run(
        state,
        &format!("s-{}", session.len() + workspace.len()),
        Command::SessionRegister {
            session_id: session.into(),
            workspace_id: workspace.into(),
            parent: None,
            headless: false,
        },
        0,
    ));
}

/// One checkout with a Codex and a Claude session, another with a Claude one.
fn ready(codex: bool) -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (id, root) in [("work", "/repos/work"), ("other", "/repos/other")] {
        ok(run(
            &mut state,
            &format!("w-{id}"),
            Command::WorkspaceRegister {
                workspace_id: id.into(),
                root: root.into(),
            },
            0,
        ));
    }
    if codex {
        register(&mut state, CODEX, "work");
    }
    register(&mut state, CLAUDE, "work");
    register(&mut state, ELSEWHERE, "other");
    state
}

fn version(digit: &str) -> FileState {
    serde_json::from_value(json!({"type": "present", "digest": digit.repeat(64)})).unwrap()
}

fn file(path: &str, digit: &str) -> ObservedFile {
    ObservedFile {
        path: path.into(),
        state: version(digit),
    }
}

fn observe(state: &mut State, id: &str, files: Vec<ObservedFile>, now: u64) -> Outcome {
    run(
        state,
        id,
        Command::ObserveUncoordinated {
            workspace_id: "work".into(),
            files,
            scanned_from_ms: None,
        },
        now,
    )
}

/// A scan reads the checkout off the state owner's thread, so a session can
/// report a newer version of a file between the scan's read and its result.
/// The version the scan saw is then the one before the session's own edit:
/// not a change no session reported (the macOS failure on pull request #242).
#[test]
fn a_file_a_session_reported_after_the_scan_read_it_is_not_recorded() {
    let mut state = ready(true);
    let write = |state: &mut State, id: &str, digit: &str, at: u64| {
        ok(run(
            state,
            id,
            Command::ObserveSource {
                session_id: CLAUDE.into(),
                files: vec![file("src/a.rs", digit)],
                origin: SourceOrigin::Write,
            },
            at,
        ));
    };
    write(&mut state, "w1", "1", 100);
    // The scan starts at 500 and reads version 1; Claude writes version 2 at
    // 600; the scan's result reaches the core at 1_000.
    write(&mut state, "w2", "2", 600);
    let stale = |from: Option<u64>| Command::ObserveUncoordinated {
        workspace_id: "work".into(),
        files: vec![file("src/a.rs", "1")],
        scanned_from_ms: from,
    };
    let outcome = run(&mut state, "scan-1", stale(Some(500)), 1_000);
    assert_eq!(recorded(outcome), (vec![], vec![]));
    assert!(
        brief(&mut state, CLAUDE, "b1", 2_000)
            .uncoordinated
            .is_empty()
    );

    // A report from before the scan started explains only its own version:
    // a different one on disk is still a change no session reported.
    let outcome = run(&mut state, "scan-2", stale(Some(700)), 1_000);
    assert_eq!(
        recorded(outcome),
        (vec![CODEX.into()], vec!["src/a.rs".into()])
    );
}

fn recorded(outcome: Outcome) -> (Vec<String>, Vec<String>) {
    match ok(outcome) {
        Outcome::UncoordinatedObserved { sessions, paths } => (sessions, paths),
        other => panic!("{other:?}"),
    }
}

fn brief(state: &mut State, session: &str, id: &str, now: u64) -> Briefing {
    match ok(run(
        state,
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
            remote_messages: false,
        },
        now,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unreported_change_is_told_once_to_each_session_of_the_checkout_naming_the_codex_session() {
    let mut state = ready(true);
    let (sessions, paths) = recorded(observe(
        &mut state,
        "scan-1",
        vec![file("src/b.rs", "b"), file("src/a.rs", "a")],
        1_000,
    ));
    assert_eq!(sessions, [CODEX]);
    assert_eq!(paths, ["src/a.rs", "src/b.rs"]);

    let first = brief(&mut state, CLAUDE, "b1", 2_000);
    assert!(first.changed, "a notice is news");
    assert_eq!(first.uncoordinated.len(), 1, "{first:?}");
    let notice = &first.uncoordinated[0];
    assert_eq!(notice.sessions, [CODEX]);
    assert_eq!(notice.paths, ["src/a.rs", "src/b.rs"]);
    assert_eq!(notice.age_ms, 1_000);

    let again = brief(&mut state, CLAUDE, "b2", 3_000);
    assert!(again.uncoordinated.is_empty(), "told once");
    assert!(!again.changed);

    // Each session of the checkout is told once, the Codex session too: its
    // own next briefing comes only once its hooks run again.
    assert_eq!(brief(&mut state, CODEX, "b3", 4_000).uncoordinated.len(), 1);
    assert!(
        brief(&mut state, CODEX, "b4", 5_000)
            .uncoordinated
            .is_empty()
    );
    assert!(
        brief(&mut state, ELSEWHERE, "b5", 5_000)
            .uncoordinated
            .is_empty(),
        "another checkout's sessions are not told"
    );
}

#[test]
fn a_change_a_session_reported_or_is_editing_or_with_no_codex_session_is_not_recorded() {
    let mut state = ready(true);
    // Claude read a.rs at this version; Codex holds b.rs for an admitted edit
    // whose report has not arrived yet.
    ok(run(
        &mut state,
        "read",
        Command::ObserveSource {
            session_id: CLAUDE.into(),
            files: vec![file("src/a.rs", "a")],
            origin: SourceOrigin::Read,
        },
        100,
    ));
    ok(run(
        &mut state,
        "admit",
        Command::Admit {
            session_id: CODEX.into(),
            paths: vec!["src/b.rs".into()],
            ttl_ms: 60_000,
        },
        100,
    ));
    let outcome = observe(
        &mut state,
        "scan-1",
        vec![file("src/a.rs", "a"), file("src/b.rs", "b")],
        1_000,
    );
    assert_eq!(recorded(outcome), (vec![], vec![]));
    assert!(
        brief(&mut state, CLAUDE, "b1", 2_000)
            .uncoordinated
            .is_empty()
    );

    // Without a registered Codex session nothing names a likely cause.
    let mut state = ready(false);
    let outcome = observe(&mut state, "scan-2", vec![file("src/c.rs", "c")], 1_000);
    assert_eq!(recorded(outcome), (vec![], vec![]));
    assert!(
        brief(&mut state, CLAUDE, "b2", 2_000)
            .uncoordinated
            .is_empty()
    );
}

#[test]
fn a_report_arriving_after_the_scan_explains_the_change() {
    let mut state = ready(true);
    recorded(observe(
        &mut state,
        "scan-1",
        vec![file("src/a.rs", "a"), file("src/b.rs", "b")],
        1_000,
    ));
    ok(run(
        &mut state,
        "late",
        Command::ObserveSource {
            session_id: CODEX.into(),
            files: vec![file("src/a.rs", "a")],
            origin: SourceOrigin::Write,
        },
        1_500,
    ));
    let briefing = brief(&mut state, CLAUDE, "b1", 2_000);
    assert_eq!(briefing.uncoordinated[0].paths, ["src/b.rs"]);
    ok(run(
        &mut state,
        "late-b",
        Command::ObserveSource {
            session_id: CODEX.into(),
            files: vec![file("src/b.rs", "b")],
            origin: SourceOrigin::Write,
        },
        2_500,
    ));
    assert!(
        brief(&mut state, CODEX, "b2", 3_000)
            .uncoordinated
            .is_empty()
    );
}

#[test]
fn a_codex_session_is_named_once_while_its_notice_is_held_and_again_after_the_horizon() {
    let mut state = ready(true);
    recorded(observe(
        &mut state,
        "scan-1",
        vec![file("src/a.rs", "a")],
        1_000,
    ));
    let outcome = observe(&mut state, "scan-2", vec![file("src/b.rs", "b")], 2_000);
    assert_eq!(recorded(outcome), (vec![], vec![]), "already named");
    let briefing = brief(&mut state, CLAUDE, "b1", 3_000);
    assert_eq!(briefing.uncoordinated.len(), 1);
    assert_eq!(briefing.uncoordinated[0].paths, ["src/a.rs"]);

    let later = 1_000 + BRIEF_HORIZON_MS + 1;
    let (sessions, _) = recorded(observe(
        &mut state,
        "scan-3",
        vec![file("src/c.rs", "c")],
        later,
    ));
    assert_eq!(sessions, [CODEX]);
    assert_eq!(
        brief(&mut state, CLAUDE, "b2", later + 1).uncoordinated[0].paths,
        ["src/c.rs"]
    );
}

#[test]
fn invalid_observations_are_rejected_without_events_and_events_replay() {
    let state = ready(true);
    for files in [
        vec![],
        vec![file("../escape", "a")],
        vec![file("src/a.rs", "a"), file("src/a.rs", "b")],
        (0..=langspan_core::MAX_UNCOORDINATED_OBSERVED)
            .map(|n| file(&format!("src/{n}.rs"), "a"))
            .collect(),
    ] {
        let command = Command::ObserveUncoordinated {
            workspace_id: "work".into(),
            files,
            scanned_from_ms: None,
        };
        let decision = decide(&state, &command, "bad", 10);
        assert!(matches!(decision.outcome, Outcome::Error { .. }));
        assert!(decision.events.is_empty());
    }
    let missing = Command::ObserveUncoordinated {
        workspace_id: "missing".into(),
        files: vec![file("src/a.rs", "a")],
        scanned_from_ms: None,
    };
    assert!(matches!(
        decide(&state, &missing, "bad", 10).outcome,
        Outcome::Error { .. }
    ));

    let command = Command::ObserveUncoordinated {
        workspace_id: "work".into(),
        files: vec![file("src/a.rs", "a")],
        scanned_from_ms: None,
    };
    let decision = decide(&state, &command, "scan-1", 10);
    let wire = serde_json::to_string(&decision.events[0]).unwrap();
    assert!(wire.contains(r#""uncoordinated_changed""#), "{wire}");
    let mut replay = state.clone();
    apply(&mut replay, &serde_json::from_str(&wire).unwrap()).unwrap();
    assert_eq!(replay.uncoordinated["work"].len(), 1);
    let mut invalid = serde_json::to_value(&decision.events[0]).unwrap();
    invalid["change"]["generation"] = json!(2);
    let mut other = state.clone();
    assert!(apply(&mut other, &serde_json::from_value(invalid).unwrap()).is_err());
    assert_eq!(other, state);
}

/// The texts of the messages a briefing carried.
fn texts(briefing: &Briefing) -> Vec<String> {
    briefing
        .messages
        .iter()
        .map(|message| message.text.clone())
        .collect()
}

#[test]
fn a_briefing_with_an_uncoordinated_change_reserves_its_line_before_choosing_messages() {
    let mut state = ready(true);
    // Two messages at the longest rendered line: both fit a briefing alone,
    // but not beside an uncoordinated change's line, so the second waits.
    for (id, letter) in [("m1", "x"), ("m2", "y")] {
        ok(run(
            &mut state,
            id,
            Command::SendMessage {
                workspace_id: "work".into(),
                from: None,
                to: None,
                text: letter.repeat(langspan_core::MESSAGE_BYTES),
                paths: Vec::new(),
                remote: false,
            },
            500,
        ));
    }
    recorded(observe(
        &mut state,
        "scan-1",
        vec![file("src/a.rs", "a")],
        1_000,
    ));
    let first = brief(&mut state, CLAUDE, "b1", 2_000);
    assert_eq!(first.uncoordinated.len(), 1);
    assert_eq!(first.messages.len(), 1, "{:?}", texts(&first));
    let second = brief(&mut state, CLAUDE, "b2", 3_000);
    assert!(second.uncoordinated.is_empty(), "told once");
    assert_eq!(second.messages.len(), 1, "the waiting message comes next");
    assert_ne!(texts(&first), texts(&second));
}

#[test]
fn the_command_has_a_stable_strict_wire_vector() {
    let wire = r#"{"op":"observe_uncoordinated","workspace_id":"work","files":[{"path":"src/a.rs","state":{"type":"present","digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]}"#;
    let parsed: Command = serde_json::from_str(wire).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), wire);
    let extra = r#"{"op":"observe_uncoordinated","workspace_id":"work","files":[],"sessions":[]}"#;
    assert!(serde_json::from_str::<Command>(extra).is_err());
}
