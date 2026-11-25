//! The admission rule for lines other sessions wrote recently (ADR 0028):
//! the hook supplies them from the line journal; the core refuses once, and
//! only a session with no recorded version of the file, and only for a live
//! writer within the horizon.
use langspan_core::{
    Command, Event, FileState, LINE_HORIZON_MS, LineRange, LivenessStatus, MAX_WRITTEN,
    ObservedFile, Outcome, ProcessIdentity, SourceOrigin, State, ToolIdentity, WrittenLines, apply,
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

fn version(n: u32) -> FileState {
    FileState::Present {
        digest: format!("{n:064x}"),
        kind: Default::default(),
    }
}

fn ready(sessions: &[&str]) -> State {
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
    for session in sessions {
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: (*session).into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    state
}

fn written(writer: &str, start_line: u32, age_ms: u64) -> WrittenLines {
    WrittenLines {
        path: "a.txt".into(),
        session_id: writer.into(),
        lines: vec![LineRange {
            start_line,
            end_line: start_line,
        }],
        age_ms,
    }
}

fn admit(state: &mut State, session: &str, id: &str, overwrites: Vec<WrittenLines>) -> Outcome {
    run(
        state,
        id,
        Command::AdmitTool {
            session_id: session.into(),
            paths: vec!["a.txt".into()],
            ttl_ms: 1_000,
            operation: ToolIdentity {
                operation_id: format!("op-{id}"),
                source: "test".into(),
                tool: "apply_patch".into(),
                input_digest: "a".repeat(64),
            },
            base: vec![ObservedFile {
                path: "a.txt".into(),
                state: version(2),
            }],
            regions: Vec::new(),
            overwrites,
        },
        10,
    )
}

fn admitted(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Admitted { .. })
}

#[test]
fn an_unread_edit_over_another_sessions_lines_is_refused_once() {
    let mut state = ready(&["alice", "bob"]);
    let first = admit(&mut state, "bob", "b1", vec![written("alice", 3, 2_000)]);
    let Outcome::UnreadLines { lines } = &first else {
        panic!("{first:?}");
    };
    assert_eq!(lines, &[written("alice", 3, 2_000)]);
    // The retry against the same version is admitted.
    let retry = admit(&mut state, "bob", "b2", vec![written("alice", 3, 2_000)]);
    assert!(admitted(&retry), "{retry:?}");
}

#[test]
fn a_session_with_a_recorded_version_is_left_to_the_stale_source_rule() {
    let mut state = ready(&["alice", "bob"]);
    run(
        &mut state,
        "read",
        Command::ObserveSource {
            session_id: "bob".into(),
            files: vec![ObservedFile {
                path: "a.txt".into(),
                state: version(2),
            }],
            origin: SourceOrigin::Read,
        },
        5,
    );
    let outcome = admit(&mut state, "bob", "b1", vec![written("alice", 3, 2_000)]);
    assert!(admitted(&outcome), "{outcome:?}");
}

#[test]
fn lines_older_than_the_horizon_no_longer_refuse() {
    let mut state = ready(&["alice", "bob"]);
    let at_edge = admit(
        &mut state.clone(),
        "bob",
        "b1",
        vec![written("alice", 3, LINE_HORIZON_MS)],
    );
    assert!(
        matches!(at_edge, Outcome::UnreadLines { .. }),
        "{at_edge:?}"
    );
    let past = admit(
        &mut state,
        "bob",
        "b1",
        vec![written("alice", 3, LINE_HORIZON_MS + 1)],
    );
    assert!(admitted(&past), "{past:?}");
}

#[test]
fn an_ended_exited_unknown_or_own_writer_does_not_refuse() {
    let mut state = ready(&["alice", "erin", "bob"]);
    run(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "alice".into(),
        },
        5,
    );
    let exited = run(
        &mut state,
        "exit",
        Command::ObserveSessionLiveness {
            session_id: "erin".into(),
            process: ProcessIdentity {
                pid: 4,
                start_token: "t".into(),
            },
            status: LivenessStatus::Exited,
        },
        5,
    );
    assert!(
        matches!(exited, Outcome::SessionLiveness { .. }),
        "{exited:?}"
    );
    let outcome = admit(
        &mut state,
        "bob",
        "b1",
        vec![
            written("alice", 1, 100),
            written("erin", 2, 100),
            written("nobody", 3, 100),
            written("bob", 4, 100),
        ],
    );
    assert!(admitted(&outcome), "{outcome:?}");
}

#[test]
fn malformed_or_too_many_written_lines_are_refused_with_their_reason() {
    let mut state = ready(&["alice", "bob"]);
    let mut reversed = written("alice", 5, 0);
    reversed.lines[0].end_line = 4;
    let too_many = vec![written("alice", 1, 0); MAX_WRITTEN + 1];
    let mut elsewhere = written("alice", 1, 0);
    elsewhere.path = "b.txt".into();
    for overwrites in [vec![reversed], too_many, vec![elsewhere]] {
        let outcome = admit(&mut state, "bob", "b1", overwrites);
        assert!(
            matches!(&outcome, Outcome::Error { code, .. } if code == "invalid_overwrites"),
            "{outcome:?}"
        );
    }
}
