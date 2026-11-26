//! A whole read outlined once per session and file version (roadmap A11).
use langspan_core::{Command, Event, MAX_OUTLINED, Outcome, State, apply, decide, was_outlined};

fn run(state: &mut State, id: &str, command: Command) -> (Outcome, Vec<Event>) {
    let before = state.clone();
    let decision = decide(state, &command, id, 0);
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
            workspace_id: "main".into(),
            root: "/repos/app".into(),
        },
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
        );
    }
    state
}

fn digest(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn read(state: &mut State, id: &str, session: &str, path: &str, version: u8) -> bool {
    let (outcome, _) = run(
        state,
        id,
        Command::OutlineRead {
            session_id: session.into(),
            path: path.into(),
            digest: digest(version),
            lines: 2000,
            bytes: 40_000,
        },
    );
    match outcome {
        Outcome::ReadOutline {
            outline: Some(_),
            skipped: None,
            ..
        } => true,
        Outcome::ReadOutline {
            outline: None,
            skipped: Some(why),
            ..
        } => {
            assert_eq!(why, "shown");
            false
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn each_session_is_outlined_once_per_file_version() {
    let mut state = ready();
    assert!(read(&mut state, "r1", "alice", "src/store.rs", 1));
    assert!(was_outlined(&state, "alice", "src/store.rs", &digest(1)));
    assert!(!read(&mut state, "r2", "alice", "src/store.rs", 1));
    // Another session, another file: each outlined once.
    assert!(read(&mut state, "r3", "bob", "src/store.rs", 1));
    assert!(read(&mut state, "r4", "alice", "src/other.rs", 1));
    // A changed file starts afresh, and replaces the old version.
    assert!(read(&mut state, "r5", "alice", "src/store.rs", 2));
    assert!(!was_outlined(&state, "alice", "src/store.rs", &digest(1)));
    assert!(!read(&mut state, "r6", "alice", "src/store.rs", 2));
}

#[test]
fn a_bad_path_or_digest_and_an_unknown_session_are_refused() {
    let mut state = ready();
    for (session, path, digest) in [
        ("alice", "../outside.rs", "00".repeat(32)),
        ("alice", "src/a.rs", "xyz".into()),
        ("nobody", "src/a.rs", "00".repeat(32)),
    ] {
        let (outcome, events) = run(
            &mut state,
            "bad",
            Command::OutlineRead {
                session_id: session.into(),
                path: path.into(),
                digest,
                lines: 1,
                bytes: 1,
            },
        );
        assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
        assert!(events.is_empty());
    }
}

#[test]
fn the_record_is_bounded() {
    let mut state = ready();
    for n in 0..=MAX_OUTLINED {
        assert!(read(
            &mut state,
            &format!("r{n}"),
            "alice",
            &format!("f{n}.rs"),
            1
        ));
    }
    assert!(!was_outlined(&state, "alice", "f0.rs", &digest(1)));
    assert!(was_outlined(
        &state,
        "alice",
        &format!("f{MAX_OUTLINED}.rs"),
        &digest(1)
    ));
}
