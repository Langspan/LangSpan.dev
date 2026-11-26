//! A session's edit is refused when the file is no longer the version that
//! session last read or wrote. A file it has no recorded version of is
//! admitted and reported as an unknown base, never as verified.
use langspan_core::{
    Command, Event, FileState, LeaseStatus, ObservedFile, Outcome, SourceOrigin, State,
    ToolIdentity, ToolResult, apply, decide,
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

fn version(text: &str) -> FileState {
    FileState::Present {
        digest: format!(
            "{:0>64}",
            text.bytes().map(|b| format!("{b:02x}")).collect::<String>()
        ),
        kind: Default::default(),
    }
}

fn file(path: &str, state: FileState) -> ObservedFile {
    ObservedFile {
        path: path.into(),
        state,
    }
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

fn saw(
    state: &mut State,
    session: &str,
    path: &str,
    state_now: FileState,
    origin: SourceOrigin,
    now: u64,
) {
    let outcome = run(
        state,
        &format!("saw-{session}-{now}"),
        Command::ObserveSource {
            session_id: session.into(),
            files: vec![file(path, state_now)],
            origin,
        },
        now,
    );
    assert!(
        matches!(outcome, Outcome::SourceObserved { files: 1, .. }),
        "{outcome:?}"
    );
}

fn admit(state: &mut State, session: &str, id: &str, base: Vec<ObservedFile>, now: u64) -> Outcome {
    let paths = if base.is_empty() {
        vec!["src/api.rs".into()]
    } else {
        base.iter().map(|file| file.path.clone()).collect()
    };
    run(
        state,
        id,
        Command::AdmitTool {
            session_id: session.into(),
            paths,
            ttl_ms: 1_000,
            operation: ToolIdentity {
                operation_id: format!("op-{id}"),
                source: "test".into(),
                tool: "Write".into(),
                input_digest: "a".repeat(64),
            },
            base,
            regions: Vec::new(),
            overwrites: Vec::new(),
        },
        now,
    )
}

/// Bob's reported, successful edit: releases his admission and records it.
fn bob_edits(state: &mut State, id: &str, now: u64) {
    let outcome = admit(state, "bob", id, vec![], now);
    assert!(matches!(outcome, Outcome::Admitted { .. }), "{outcome:?}");
    run(
        state,
        &format!("obs-{id}"),
        Command::Observe {
            session_id: "bob".into(),
            operation_id: format!("op-{id}"),
            source: "test".into(),
            tool: "Write".into(),
            paths: vec!["src/api.rs".into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        now,
    );
    run(
        state,
        &format!("done-{id}"),
        Command::Complete {
            session_id: "bob".into(),
            lease_id: id.into(),
        },
        now,
    );
}

#[test]
fn a_whole_file_replacement_based_on_an_old_read_is_refused_after_the_holder_finished() {
    // The case the M3 review found admitted: A completes, then B replaces the
    // whole file from a read taken before A's change.
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        10,
    );
    bob_edits(&mut state, "bob-1", 20);
    let outcome = admit(
        &mut state,
        "alice",
        "alice-1",
        vec![file("src/api.rs", version("v2"))],
        30,
    );
    let Outcome::StaleBase { stale } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].read, version("v1"));
    assert_eq!(stale[0].current, version("v2"));
    assert_eq!(stale[0].changed_by.as_deref(), Some("bob"));
    assert!(
        !state.leases.contains_key("alice-1"),
        "nothing was admitted"
    );

    // Re-reading makes the session's view current; the next attempt is admitted.
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v2"),
        SourceOrigin::Read,
        40,
    );
    let outcome = admit(
        &mut state,
        "alice",
        "alice-2",
        vec![file("src/api.rs", version("v2"))],
        50,
    );
    let Outcome::Admitted { lease } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        lease.base_unknown.is_empty(),
        "the base was compared and matched"
    );
}

#[test]
fn a_change_made_outside_any_hook_is_still_caught_by_the_version_comparison() {
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        10,
    );
    let outcome = admit(
        &mut state,
        "alice",
        "a1",
        vec![file("src/api.rs", version("vX"))],
        20,
    );
    let Outcome::StaleBase { stale } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(stale[0].changed_by, None, "no reported edit explains it");
    // A deletion is a version change too.
    let outcome = admit(
        &mut state,
        "alice",
        "a2",
        vec![file("src/api.rs", FileState::Missing)],
        30,
    );
    assert!(matches!(outcome, Outcome::StaleBase { .. }), "{outcome:?}");
}

#[test]
fn an_unread_file_is_admitted_as_an_unknown_base_and_a_new_file_as_verified() {
    let mut state = ready();
    let outcome = admit(
        &mut state,
        "alice",
        "a1",
        vec![file("src/api.rs", version("v1"))],
        10,
    );
    let Outcome::Admitted { lease } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(lease.base_unknown, ["src/api.rs"], "no read was recorded");
    let outcome = admit(
        &mut state,
        "alice",
        "a2",
        vec![file("src/new.rs", FileState::Missing)],
        20,
    );
    let Outcome::Admitted { lease } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        lease.base_unknown.is_empty(),
        "creating a file has no earlier version"
    );
    // An adapter that supplies no version at all gets no verification either.
    let mut other = ready();
    saw(
        &mut other,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        5,
    );
    let Outcome::Admitted { lease } = admit(&mut other, "alice", "a3", vec![], 10) else {
        panic!();
    };
    assert_eq!(lease.base_unknown, ["src/api.rs"]);
}

#[test]
fn a_sessions_own_write_becomes_its_known_version() {
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        10,
    );
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v2"),
        SourceOrigin::Write,
        20,
    );
    assert!(matches!(
        admit(
            &mut state,
            "alice",
            "a1",
            vec![file("src/api.rs", version("v2"))],
            30
        ),
        Outcome::Admitted { .. }
    ));
    assert!(
        matches!(
            admit(
                &mut state,
                "bob",
                "b1",
                vec![file("src/api.rs", version("v2"))],
                30
            ),
            Outcome::Conflict { .. }
        ),
        "a current holder is reported before any version comparison"
    );
}

#[test]
fn stale_is_all_or_none_and_a_conflict_is_reported_first() {
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/a.rs",
        version("a1"),
        SourceOrigin::Read,
        10,
    );
    saw(
        &mut state,
        "alice",
        "src/b.rs",
        version("b1"),
        SourceOrigin::Read,
        11,
    );
    let outcome = admit(
        &mut state,
        "alice",
        "a1",
        vec![
            file("src/a.rs", version("a1")),
            file("src/b.rs", version("b2")),
        ],
        20,
    );
    let Outcome::StaleBase { stale } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].path, "src/b.rs");
    assert!(
        state
            .leases
            .values()
            .all(|lease| lease.status != LeaseStatus::Active)
    );
}

#[test]
fn a_read_version_survives_a_daemon_restart() {
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        10,
    );
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    run(
        &mut state,
        "s-alice-2",
        Command::SessionRegister {
            session_id: "alice".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        0,
    );
    assert!(matches!(
        admit(
            &mut state,
            "alice",
            "a1",
            vec![file("src/api.rs", version("v2"))],
            5
        ),
        Outcome::StaleBase { .. }
    ));
    let Outcome::Admitted { lease } = admit(
        &mut state,
        "alice",
        "a2",
        vec![file("src/api.rs", version("v2"))],
        6,
    ) else {
        panic!();
    };
    assert!(lease.base_unknown.is_empty());
}

#[test]
fn a_change_is_refused_once_so_no_session_is_locked_out_of_a_file() {
    // A host with no reported read (Codex), or a file that was deleted and
    // cannot be read, has no way to refresh its record except this.
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Write,
        10,
    );
    for (id, current) in [("a1", version("v2")), ("a3", FileState::Missing)] {
        let first = admit(
            &mut state,
            "alice",
            id,
            vec![file("src/api.rs", current.clone())],
            20,
        );
        assert!(matches!(first, Outcome::StaleBase { .. }), "{first:?}");
        assert!(
            state
                .leases
                .values()
                .all(|lease| lease.status != LeaseStatus::Active)
        );
        let again = admit(
            &mut state,
            "alice",
            &format!("{id}-retry"),
            vec![file("src/api.rs", current)],
            30,
        );
        let Outcome::Admitted { lease } = again else {
            panic!("{again:?}");
        };
        run(
            &mut state,
            &format!("{id}-done"),
            Command::Complete {
                session_id: "alice".into(),
                lease_id: lease.lease_id,
            },
            31,
        );
    }
    // A further change after the refusal is a new change, refused in its turn.
    let outcome = admit(
        &mut state,
        "alice",
        "a5",
        vec![file("src/api.rs", version("v9"))],
        40,
    );
    assert!(matches!(outcome, Outcome::StaleBase { .. }), "{outcome:?}");
}

#[test]
fn a_reported_edit_without_its_new_version_leaves_the_base_unknown_not_stale() {
    // The write succeeded but recording what it produced did not happen.
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        10,
    );
    run(
        &mut state,
        "obs",
        Command::Observe {
            session_id: "alice".into(),
            operation_id: "op".into(),
            source: "test".into(),
            tool: "Write".into(),
            paths: vec!["src/api.rs".into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        20,
    );
    let Outcome::Admitted { lease } = admit(
        &mut state,
        "alice",
        "a1",
        vec![file("src/api.rs", version("v2"))],
        30,
    ) else {
        panic!("the session's own unrecorded write must not refuse its next edit");
    };
    assert_eq!(lease.base_unknown, ["src/api.rs"]);
}

#[test]
fn after_a_restart_a_peer_edit_is_still_after_an_earlier_read() {
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        3_600_000,
    );
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    for session in ["alice", "bob"] {
        run(
            &mut state,
            &format!("s2-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    bob_edits(&mut state, "bob-1", 5_000);
    let outcome = run(
        &mut state,
        "b",
        Command::Brief {
            session_id: "alice".into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
            left: Vec::new(),
        },
        6_000,
    );
    let Outcome::Briefing { briefing } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        briefing.stale_reads.len(),
        1,
        "the restart must not hide it"
    );
}

#[test]
fn a_briefing_reports_files_read_before_a_peer_edited_them() {
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        10,
    );
    bob_edits(&mut state, "bob-1", 20);
    let brief = |state: &mut State, id: &str, now| match run(
        state,
        id,
        Command::Brief {
            session_id: "alice".into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
            left: Vec::new(),
        },
        now,
    ) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    };
    let briefing = brief(&mut state, "b1", 1_020);
    assert_eq!(briefing.stale_reads.len(), 1);
    assert_eq!(briefing.stale_reads[0].path, "src/api.rs");
    assert_eq!(briefing.stale_reads[0].session_id, "bob");
    assert_eq!(briefing.stale_reads[0].age_ms, 1_000);
    assert!(briefing.changed);
    // Once alice re-reads, the notice is gone, and that is a change too.
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v2"),
        SourceOrigin::Read,
        2_000,
    );
    let briefing = brief(&mut state, "b2", 2_100);
    assert!(briefing.stale_reads.is_empty());
    assert!(briefing.changed);
}

#[test]
fn invalid_source_observations_and_bases_are_rejected_without_events() {
    let state = ready();
    let bad = [
        Command::ObserveSource {
            session_id: "alice".into(),
            files: vec![],
            origin: SourceOrigin::Read,
        },
        Command::ObserveSource {
            session_id: "alice".into(),
            files: vec![file("../escape", version("v"))],
            origin: SourceOrigin::Read,
        },
        Command::ObserveSource {
            session_id: "alice".into(),
            files: vec![file("a.rs", version("v")), file("a.rs", version("w"))],
            origin: SourceOrigin::Read,
        },
        Command::ObserveSource {
            session_id: "nobody".into(),
            files: vec![file("a.rs", version("v"))],
            origin: SourceOrigin::Write,
        },
        Command::AdmitTool {
            session_id: "alice".into(),
            paths: vec!["a.rs".into()],
            ttl_ms: 1_000,
            operation: ToolIdentity {
                operation_id: "op".into(),
                source: "test".into(),
                tool: "Write".into(),
                input_digest: "a".repeat(64),
            },
            base: vec![file("b.rs", version("v"))],
            regions: Vec::new(),
            overwrites: Vec::new(),
        },
    ];
    for command in bad {
        let decision = decide(&state, &command, "bad", 1);
        assert!(
            matches!(decision.outcome, Outcome::Error { .. }),
            "{command:?}"
        );
        assert!(decision.events.is_empty());
    }
}

/// A peer edited the file, then the
/// checkout changed it again (`git checkout`, `reset --hard`, `stash`). The
/// refusal names nobody, and comes once.
#[test]
fn a_file_changed_after_a_peers_edit_is_refused_once_without_naming_the_peer() {
    let mut state = ready();
    saw(
        &mut state,
        "alice",
        "src/api.rs",
        version("v1"),
        SourceOrigin::Read,
        10,
    );
    bob_edits(&mut state, "bob-1", 20);
    saw(
        &mut state,
        "bob",
        "src/api.rs",
        version("v2"),
        SourceOrigin::Write,
        21,
    );
    // The checkout now holds v3, which nobody reported writing.
    let outcome = admit(
        &mut state,
        "alice",
        "alice-1",
        vec![file("src/api.rs", version("v3"))],
        30,
    );
    let Outcome::StaleBase { stale } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(stale[0].current, version("v3"));
    assert_eq!(stale[0].changed_by, None, "Bob did not write v3");
    let again = admit(
        &mut state,
        "alice",
        "alice-2",
        vec![file("src/api.rs", version("v3"))],
        31,
    );
    assert!(matches!(again, Outcome::Admitted { .. }), "{again:?}");
}
