//! Remembered notes, scan outcomes and the bounded account of a workspace.
use langspan_core::{Command, Event, Outcome, Recap, State, ToolResult, apply, decide};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
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

fn edited(state: &mut State, session: &str, path: &str, now: u64) {
    run(
        state,
        &format!("o-{session}-{now}"),
        Command::Observe {
            session_id: session.into(),
            operation_id: format!("op-{now}"),
            source: "test".into(),
            tool: "Edit".into(),
            paths: vec![path.into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        now,
    );
}

fn remember(
    state: &mut State,
    id: &str,
    author: Option<&str>,
    text: &str,
    paths: &[&str],
) -> Outcome {
    run(
        state,
        id,
        Command::Remember {
            workspace_id: "work".into(),
            author: author.map(str::to_owned),
            text: text.into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
        },
        5,
    )
}

fn notes_for(state: &mut State, session: &str, id: &str) -> Vec<String> {
    match run(
        state,
        id,
        Command::Brief {
            session_id: session.into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
            recent: false,
            recent_window_ms: None,
        },
        50,
    ) {
        Outcome::Briefing { briefing } => {
            briefing.notes.into_iter().map(|note| note.text).collect()
        }
        other => panic!("{other:?}"),
    }
}

fn recap(state: &mut State) -> Recap {
    match run(
        state,
        "read",
        Command::Recap {
            workspace_id: "work".into(),
        },
        100,
    ) {
        Outcome::Recap { recap } => recap,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_note_reaches_sessions_it_concerns_and_survives_a_restart_until_forgotten() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 10);
    edited(&mut state, "bob", "docs/guide.md", 10);
    remember(
        &mut state,
        "n-file",
        Some("bob"),
        "api.rs is generated; edit api.tmpl",
        &["src/api.rs"],
    );
    remember(
        &mut state,
        "n-all",
        None,
        "Release freeze until Friday",
        &[],
    );
    // The file note concerns alice; the workspace note concerns everyone;
    // an author is not told their own note.
    assert_eq!(
        notes_for(&mut state, "alice", "b1"),
        [
            "Release freeze until Friday",
            "api.rs is generated; edit api.tmpl"
        ]
    );
    assert_eq!(
        notes_for(&mut state, "bob", "b2"),
        ["Release freeze until Friday"]
    );

    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert_eq!(
        recap(&mut state).notes.len(),
        2,
        "remembered across a restart"
    );
    let forgotten = run(
        &mut state,
        "f1",
        Command::Forget {
            workspace_id: "work".into(),
            note_id: "n-all".into(),
        },
        1,
    );
    assert!(matches!(
        forgotten,
        Outcome::Forgotten { removed: true, .. }
    ));
    let again = decide(
        &state,
        &Command::Forget {
            workspace_id: "work".into(),
            note_id: "n-all".into(),
        },
        "f2",
        2,
    );
    assert!(again.events.is_empty());
    assert!(matches!(
        again.outcome,
        Outcome::Forgotten { removed: false, .. }
    ));
    assert_eq!(recap(&mut state).notes[0].note_id, "n-file");
}

#[test]
fn notes_are_bounded_single_line_and_validated() {
    let mut state = ready();
    for (text, paths) in [
        ("", vec![]),
        ("two\nlines", vec![]),
        (&"x".repeat(401)[..], vec![]),
        ("ok", vec!["../escape"]),
        ("ok", vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"]),
    ] {
        let outcome = remember(&mut state, "bad", None, text, &paths);
        assert!(
            matches!(outcome, Outcome::Error { .. }),
            "{text:?} {paths:?}"
        );
    }
    assert!(matches!(
        remember(&mut state, "bad", Some("nobody"), "ok", &[]),
        Outcome::Error { .. }
    ));
    for index in 0..70 {
        remember(
            &mut state,
            &format!("n{index}"),
            None,
            &format!("note {index}"),
            &[],
        );
    }
    assert_eq!(state.notes["work"].len(), 64);
    assert_eq!(state.notes["work"][0].text, "note 69", "newest kept");
}

#[test]
fn a_failed_scan_is_recorded_once_and_cleared_by_a_complete_one() {
    let mut state = ready();
    let scan = |failure: Option<&str>| Command::ObserveScan {
        workspace_id: "work".into(),
        failure: failure.map(str::to_owned),
        coverage: None,
    };
    assert!(
        decide(&state, &scan(None), "s0", 1).events.is_empty(),
        "nothing to clear"
    );
    assert_eq!(
        decide(&state, &scan(Some("file-count bound exceeded")), "s1", 2)
            .events
            .len(),
        1
    );
    run(&mut state, "s1", scan(Some("file-count bound exceeded")), 2);
    assert!(
        decide(&state, &scan(Some("file-count bound exceeded")), "s2", 3)
            .events
            .is_empty(),
        "the same failure again is the same fact"
    );
    assert_eq!(
        decide(&state, &scan(Some("a different reason")), "s3", 4)
            .events
            .len(),
        1
    );
    assert_eq!(
        recap(&mut state).scan_failure.unwrap().detail,
        "file-count bound exceeded"
    );
    run(&mut state, "s4", scan(None), 5);
    assert!(recap(&mut state).scan_failure.is_none());
    let long = "x\n".repeat(400);
    run(&mut state, "s5", scan(Some(&long)), 6);
    let detail = &state.scan_failures["work"].detail;
    assert!(detail.len() <= 200 && !detail.contains('\n'));
}

#[test]
fn a_recap_is_bounded_and_orders_current_sessions_first() {
    let mut state = ready();
    for index in 0..20 {
        let session = format!("peer{index:02}");
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.clone(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            0,
        );
        for file in 0..12 {
            edited(
                &mut state,
                &session,
                &format!("src/f{file}.rs"),
                100 + index * 20 + file,
            );
        }
    }
    let recap = recap(&mut state);
    assert_eq!(recap.sessions.len(), 16);
    assert_eq!(recap.omitted_sessions, 6);
    assert_eq!(recap.sessions[0].session_id, "peer19", "most recent first");
    assert!(
        recap
            .sessions
            .iter()
            .all(|session| session.recent.len() <= 8)
    );
    assert!(
        serde_json::to_vec(&recap).unwrap().len() < 48 * 1024,
        "fits one frame"
    );
    assert!(matches!(
        decide(
            &state,
            &Command::Recap {
                workspace_id: "nowhere".into()
            },
            "r",
            1
        )
        .outcome,
        Outcome::Error { .. }
    ));
}

#[test]
fn a_partial_scan_shows_its_coverage_until_a_scan_versions_everything() {
    let mut state = ready();
    let partial = langspan_core::ScanCoverage {
        omitted: 900,
        too_large: 1,
        too_large_paths: vec!["assets/video.bin".into()],
        ..Default::default()
    };
    let scan = |failure: Option<&str>, coverage: Option<langspan_core::ScanCoverage>| {
        Command::ObserveScan {
            workspace_id: "work".into(),
            failure: failure.map(str::to_owned),
            coverage,
        }
    };
    run(&mut state, "c1", scan(None, Some(partial.clone())), 1);
    assert_eq!(recap(&mut state).scan_coverage, Some(partial.clone()));
    assert!(
        decide(&state, &scan(None, Some(partial.clone())), "c2", 2)
            .events
            .is_empty(),
        "the same coverage again is the same fact"
    );
    // A failed attempt says nothing new about what the last completed scan
    // covered.
    run(&mut state, "c3", scan(Some("deadline elapsed"), None), 3);
    assert_eq!(recap(&mut state).scan_coverage, Some(partial));
    run(&mut state, "c4", scan(None, Some(Default::default())), 4);
    assert_eq!(recap(&mut state).scan_coverage, None);
    let too_many = langspan_core::ScanCoverage {
        too_large: 20,
        too_large_paths: (0..9).map(|i| format!("f{i}")).collect(),
        ..Default::default()
    };
    assert!(matches!(
        decide(&state, &scan(None, Some(too_many)), "c5", 5).outcome,
        Outcome::Error { .. }
    ));
}
