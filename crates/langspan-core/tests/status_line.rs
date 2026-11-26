//! The person's status line (roadmap H2): sessions here, refusals today.
use langspan_core::{
    BRIEF_HORIZON_MS, ClockAnchor, Command, Event, FileState, ObservedFile, Outcome, RefusalKind,
    SourceOrigin, State, StatusLine, ToolIdentity, ToolResult, apply, decide,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> (Outcome, Vec<Event>) {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    (decision.outcome, decision.events)
}

fn ok(state: &mut State, id: &str, command: Command, now: u64) -> Vec<Event> {
    let (outcome, events) = run(state, id, command, now);
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    events
}

/// Wall time at the daemon clock's zero, for this generation.
const WALL_AT_ZERO: u64 = 1_800_000_000_000;

fn boot(state: &mut State, generation: u64) {
    apply(state, &Event::Booted { generation }).unwrap();
    apply(
        state,
        &Event::ClockAnchored {
            anchor: ClockAnchor {
                generation,
                clock_ms: 0,
                wall_ms: WALL_AT_ZERO,
            },
        },
    )
    .unwrap();
}

/// Two worktrees of one repository, and a workspace in another one.
fn ready() -> State {
    let mut state = State::default();
    boot(&mut state, 1);
    for (id, root, repository) in [
        ("main", "/repos/app", "repo-app"),
        ("tree", "/repos/app-tree", "repo-app"),
        ("other", "/repos/other", "repo-other"),
    ] {
        ok(
            &mut state,
            &format!("w-{id}"),
            Command::WorkspaceRegister {
                workspace_id: id.into(),
                root: root.into(),
            },
            0,
        );
        ok(
            &mut state,
            &format!("g-{id}"),
            Command::ObserveWorkspaceGit {
                workspace_id: id.into(),
                repository_id: repository.into(),
                branch_ref: Some("refs/heads/main".into()),
            },
            0,
        );
    }
    state
}

fn register(state: &mut State, session: &str, workspace: &str, parent: Option<&str>) {
    ok(
        state,
        &format!("s-{session}"),
        Command::SessionRegister {
            session_id: session.into(),
            workspace_id: workspace.into(),
            parent: parent.map(str::to_owned),
            headless: false,
        },
        0,
    );
}

fn prompt(state: &mut State, session: &str, now: u64) {
    ok(
        state,
        &format!("p-{session}-{now}"),
        Command::SessionPrompt {
            session_id: session.into(),
            summary: "work".into(),
            line_digests: Vec::new(),
        },
        now,
    );
}

fn line(state: &State, root: &str, from_ms: u64, now: u64) -> StatusLine {
    let decision = decide(
        state,
        &Command::StatusLine {
            root: root.into(),
            from_ms,
        },
        "read",
        now,
    );
    assert!(decision.events.is_empty(), "the status line is a read");
    match decision.outcome {
        Outcome::StatusLine { line } => line,
        other => panic!("{other:?}"),
    }
}

fn admit(state: &mut State, id: &str, session: &str, path: &str, now: u64) -> Outcome {
    admit_with(state, id, session, path, Vec::new(), now)
}

fn admit_with(
    state: &mut State,
    id: &str,
    session: &str,
    path: &str,
    base: Vec<ObservedFile>,
    now: u64,
) -> Outcome {
    run(
        state,
        id,
        Command::AdmitTool {
            session_id: session.into(),
            operation: tool(format!("op-{id}")),
            paths: vec![path.into()],
            base,
            regions: Vec::new(),
            overwrites: Vec::new(),
            ttl_ms: 30_000,
        },
        now,
    )
    .0
}

fn tool(operation_id: impl Into<String>) -> ToolIdentity {
    ToolIdentity {
        operation_id: operation_id.into(),
        source: "test".into(),
        tool: "Edit".into(),
        input_digest: "f".repeat(64),
    }
}

fn refusals(state: &State) -> usize {
    state.refusals.len()
}

#[test]
fn counts_current_sessions_of_the_repository_including_its_worktrees() {
    let mut state = ready();
    for (session, workspace) in [
        ("alice", "main"),
        ("bob", "main"),
        ("carol", "tree"),
        ("dave", "other"),
        ("ended", "main"),
        ("idle", "main"),
        ("never", "main"),
    ] {
        register(&mut state, session, workspace, None);
    }
    register(&mut state, "alice.helper", "main", Some("alice"));
    let now = BRIEF_HORIZON_MS + 10_000;
    for session in ["alice", "bob", "carol", "dave", "ended", "alice.helper"] {
        prompt(&mut state, session, now - 1_000);
    }
    // Active more than 30 minutes ago.
    prompt(&mut state, "idle", now - BRIEF_HORIZON_MS - 1);
    ok(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "ended".into(),
            trail: None,
        },
        now - 500,
    );
    let here = line(&state, "/repos/app", 0, now);
    assert_eq!(here.workspace_id.as_deref(), Some("main"));
    // alice, bob and carol (a linked worktree of the same repository); not
    // dave (another repository), the ended or idle session, one never
    // active, or alice's subagent.
    assert_eq!(here.sessions_here, 3, "{here:?}");
    assert_eq!(here.text(), "langspan · 3 sessions here · 0 refusals today");
    // A subdirectory of the checkout is the same workspace; a sibling whose
    // name only starts the same is the other worktree.
    assert_eq!(
        line(&state, "/repos/app/src/deep", 0, now).workspace_id,
        Some("main".into())
    );
    assert_eq!(
        line(&state, "/repos/app-tree", 0, now).workspace_id,
        Some("tree".into())
    );
    let outside = line(&state, "/elsewhere", 0, now);
    assert_eq!(outside.workspace_id, None);
    assert_eq!(outside.sessions_here, 0);
    // A session of an earlier daemon run is not current.
    boot(&mut state, 2);
    assert_eq!(line(&state, "/repos/app", 0, now).sessions_here, 0);
}

#[test]
fn a_refused_admission_appends_one_write_refused_event_and_the_retry_none() {
    let mut state = ready();
    register(&mut state, "alice", "main", None);
    register(&mut state, "bob", "main", None);
    let now = 1_000;
    assert!(matches!(
        admit(&mut state, "a-bob", "bob", "src/api.rs", now),
        Outcome::Admitted { .. }
    ));
    let decision = decide(
        &state,
        &Command::AdmitTool {
            session_id: "alice".into(),
            operation: tool("op-a-alice"),
            paths: vec!["src/api.rs".into()],
            base: Vec::new(),
            regions: Vec::new(),
            overwrites: Vec::new(),
            ttl_ms: 30_000,
        },
        "a-alice",
        now + 1,
    );
    assert!(matches!(decision.outcome, Outcome::Conflict { .. }));
    let refused: Vec<&Event> = decision
        .events
        .iter()
        .filter(|event| matches!(event, Event::WriteRefused { .. }))
        .collect();
    assert_eq!(refused.len(), 1, "{:?}", decision.events);
    assert_eq!(
        refused[0],
        &Event::WriteRefused {
            session_id: "alice".into(),
            workspace_id: "main".into(),
            kind: RefusalKind::Conflict,
            paths: vec!["src/api.rs".into()],
            peer_session_id: Some("bob".into()),
            input_digest: Some("f".repeat(64)),
            at_ms: now + 1,
            generation: 1,
        }
    );
    for event in &decision.events {
        apply(&mut state, event).unwrap();
    }
    assert_eq!(refusals(&state), 1);
    let today = line(&state, "/repos/app", WALL_AT_ZERO, now + 2);
    assert_eq!(today.refusals_today, 1);
    // Bob finishes; Alice's next call is admitted and records nothing more.
    ok(
        &mut state,
        "c-bob",
        Command::Complete {
            session_id: "bob".into(),
            lease_id: "a-bob".into(),
        },
        now + 3,
    );
    let events = match run(
        &mut state,
        "a-alice-2",
        Command::AdmitTool {
            session_id: "alice".into(),
            operation: tool("op-a-alice-2"),
            paths: vec!["src/api.rs".into()],
            base: Vec::new(),
            regions: Vec::new(),
            overwrites: Vec::new(),
            ttl_ms: 30_000,
        },
        now + 4,
    ) {
        (Outcome::Admitted { .. }, events) => events,
        other => panic!("{other:?}"),
    };
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::WriteRefused { .. }))
    );
    assert_eq!(refusals(&state), 1);
}

#[test]
fn a_stale_refusal_is_recorded_once_and_its_retry_is_admitted_without_one() {
    let mut state = ready();
    register(&mut state, "alice", "main", None);
    register(&mut state, "bob", "main", None);
    let version = |digest: &str| FileState::Present {
        digest: digest.into(),
        kind: Default::default(),
    };
    let file = |digest: &str| ObservedFile {
        path: "src/api.rs".into(),
        state: version(digest),
    };
    ok(
        &mut state,
        "read",
        Command::ObserveSource {
            session_id: "alice".into(),
            files: vec![file(&"a".repeat(64))],
            origin: SourceOrigin::Read,
        },
        10,
    );
    // Bob's edit, then Alice edits from her older read.
    ok(
        &mut state,
        "bob-edit",
        Command::Observe {
            session_id: "bob".into(),
            operation_id: "op-bob".into(),
            source: "test".into(),
            tool: "Edit".into(),
            paths: vec!["src/api.rs".into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        20,
    );
    let newer = vec![file(&"b".repeat(64))];
    let (outcome, events) = {
        let decision = decide(
            &state,
            &Command::AdmitTool {
                session_id: "alice".into(),
                operation: tool("op-stale"),
                paths: vec!["src/api.rs".into()],
                base: newer.clone(),
                regions: Vec::new(),
                overwrites: Vec::new(),
                ttl_ms: 30_000,
            },
            "stale",
            30,
        );
        for event in &decision.events {
            apply(&mut state, event).unwrap();
        }
        (decision.outcome, decision.events)
    };
    assert!(matches!(outcome, Outcome::StaleBase { .. }), "{outcome:?}");
    let kinds: Vec<(RefusalKind, Option<String>)> = events
        .iter()
        .filter_map(|event| match event {
            Event::WriteRefused {
                kind,
                peer_session_id,
                ..
            } => Some((*kind, peer_session_id.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![(RefusalKind::StaleBase, Some("bob".into()))]);
    // The identical retry (a new call with the same base) is let through.
    assert!(matches!(
        admit_with(&mut state, "retry", "alice", "src/api.rs", newer, 40),
        Outcome::Admitted { .. }
    ));
    assert_eq!(refusals(&state), 1);
}

#[test]
fn a_held_commit_is_recorded_from_the_hook_once_per_request() {
    let mut state = ready();
    register(&mut state, "alice", "main", None);
    register(&mut state, "bob", "main", None);
    let command = Command::RecordRefusal {
        session_id: "alice".into(),
        kind: RefusalKind::CommitHeld,
        paths: (0..12).map(|n| format!("src/f{n:02}.rs")).collect(),
        peer_session_id: Some("bob".into()),
    };
    let events = ok(&mut state, "held-1", command.clone(), 50);
    match events.as_slice() {
        [
            Event::WriteRefused {
                kind: RefusalKind::CommitHeld,
                paths,
                workspace_id,
                ..
            },
        ] => {
            assert_eq!(paths.len(), 8, "at most 8 paths are kept");
            assert_eq!(workspace_id, "main");
        }
        other => panic!("{other:?}"),
    }
    // The other kinds are decided by the coordinator itself, not reported.
    let (outcome, events) = run(
        &mut state,
        "held-2",
        Command::RecordRefusal {
            session_id: "alice".into(),
            kind: RefusalKind::Conflict,
            paths: vec!["src/a.rs".into()],
            peer_session_id: None,
        },
        51,
    );
    assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    assert!(events.is_empty());
    let (outcome, _) = run(
        &mut state,
        "held-3",
        Command::RecordRefusal {
            session_id: "nobody".into(),
            kind: RefusalKind::CommitHeld,
            paths: vec!["src/a.rs".into()],
            peer_session_id: None,
        },
        52,
    );
    assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    assert_eq!(refusals(&state), 1);
}

#[test]
fn refusals_today_start_at_the_given_midnight_and_reset_after_it() {
    let mut state = ready();
    register(&mut state, "alice", "main", None);
    register(&mut state, "dave", "other", None);
    let held = |state: &mut State, id: &str, session: &str, at: u64| {
        ok(
            state,
            id,
            Command::RecordRefusal {
                session_id: session.into(),
                kind: RefusalKind::CommitHeld,
                paths: vec!["src/a.rs".into()],
                peer_session_id: None,
            },
            at,
        );
    };
    let hour = 3_600_000;
    let midnight = WALL_AT_ZERO + 10 * hour;
    held(&mut state, "before", "alice", 10 * hour - 1);
    held(&mut state, "after-1", "alice", 10 * hour);
    held(&mut state, "after-2", "alice", 12 * hour);
    held(&mut state, "elsewhere", "dave", 12 * hour);
    let today = line(&state, "/repos/app", midnight, 13 * hour);
    assert_eq!(today.refusals_today, 2, "{today:?}");
    assert_eq!(
        today.text(),
        "langspan · 0 sessions here · 2 refusals today"
    );
    // The next day starts at the next midnight: nothing yet.
    let tomorrow = line(&state, "/repos/app", midnight + 24 * hour, 35 * hour);
    assert_eq!(tomorrow.refusals_today, 0);
    // One refusal reads in the singular; a daemon restart keeps the count,
    // placed by the new generation's anchor.
    boot(&mut state, 2);
    let one = line(&state, "/repos/other", midnight, 13 * hour);
    assert_eq!(one.refusals_today, 1);
    assert!(one.text().ends_with("· 1 refusal today"), "{}", one.text());
}

#[test]
fn refusals_without_a_clock_anchor_are_not_placed_on_a_day() {
    // History from a build before the anchor: the event is valid, uncounted.
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    ok(
        &mut state,
        "w",
        Command::WorkspaceRegister {
            workspace_id: "main".into(),
            root: "/repos/app".into(),
        },
        0,
    );
    register(&mut state, "alice", "main", None);
    apply(
        &mut state,
        &Event::WriteRefused {
            session_id: "alice".into(),
            workspace_id: "main".into(),
            kind: RefusalKind::UnreadLines,
            paths: vec!["src/a.rs".into()],
            peer_session_id: None,
            input_digest: None,
            at_ms: 5,
            generation: 1,
        },
    )
    .unwrap();
    assert_eq!(line(&state, "/repos/app", 0, 10).refusals_today, 0);
    // A clock anchor names the generation that is starting.
    let wrong = apply(
        &mut state,
        &Event::ClockAnchored {
            anchor: ClockAnchor {
                generation: 7,
                clock_ms: 0,
                wall_ms: 0,
            },
        },
    );
    assert!(wrong.is_err());
}

#[test]
fn the_kept_refusals_are_bounded() {
    let mut state = ready();
    register(&mut state, "alice", "main", None);
    for n in 0..(langspan_core::MAX_REFUSALS as u64 + 10) {
        ok(
            &mut state,
            &format!("held-{n}"),
            Command::RecordRefusal {
                session_id: "alice".into(),
                kind: RefusalKind::CommitHeld,
                paths: vec!["src/a.rs".into()],
                peer_session_id: None,
            },
            100 + n,
        );
    }
    assert_eq!(refusals(&state), langspan_core::MAX_REFUSALS);
    // Two days later, older ones are dropped as a new one arrives.
    ok(
        &mut state,
        "late",
        Command::RecordRefusal {
            session_id: "alice".into(),
            kind: RefusalKind::CommitHeld,
            paths: vec!["src/a.rs".into()],
            peer_session_id: None,
        },
        49 * 3_600_000,
    );
    assert_eq!(refusals(&state), 1);
}
