//! Reserving the next number of a named counter (issue #299, ADR 0040): two
//! sessions in separate worktrees of one repository get different numbers,
//! and each one's briefing names the other's claim. Pure decisions and
//! replay, in process.
use langspan_core::{
    Briefing, Command, Event, LivenessStatus, Outcome, ProcessIdentity, Reservation, State, apply,
    decide,
};

fn run(state: &mut State, id: &str, command: Command, at: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, at);
    assert_eq!(*state, before, "decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

/// Two worktrees of one repository and a third checkout of another, one
/// session in each.
fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (workspace, repository, session) in [
        ("main", "repo-app", "alice"),
        ("tree", "repo-app", "bob"),
        ("other", "repo-other", "carol"),
    ] {
        run(
            &mut state,
            &format!("w-{workspace}"),
            Command::WorkspaceRegister {
                workspace_id: workspace.into(),
                root: format!("/repos/{workspace}"),
            },
            0,
        );
        run(
            &mut state,
            &format!("g-{workspace}"),
            Command::ObserveWorkspaceGit {
                workspace_id: workspace.into(),
                repository_id: repository.into(),
                branch_ref: Some("refs/heads/main".into()),
            },
            0,
        );
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: workspace.into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    state
}

fn reserve(state: &mut State, id: &str, session: &str, key: &str, after: u64) -> Outcome {
    run(
        state,
        id,
        Command::Reserve {
            session_id: session.into(),
            key: key.into(),
            after,
        },
        10,
    )
}

fn reserved(outcome: &Outcome) -> (&Reservation, &[Reservation]) {
    match outcome {
        Outcome::Reserved { reservation, held } => (reservation, held),
        other => panic!("{other:?}"),
    }
}

fn brief(state: &State, session: &str) -> Briefing {
    let command = Command::Brief {
        session_id: session.into(),
        max_peers: 8,
        deliver: false,
        mid_turn: false,
        left: Vec::new(),
        recent: false,
        recent_window_ms: None,
        mcp: false,
        changes_only: false,
        confirm: false,
    };
    match decide(state, &command, "brief", 20).outcome {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

fn claims(briefing: &Briefing) -> Vec<(&str, u64, &str)> {
    briefing
        .reserved
        .iter()
        .map(|claim| (claim.key.as_str(), claim.number, claim.session_id.as_str()))
        .collect()
}

#[test]
fn two_worktrees_get_different_numbers_and_see_each_others_claim() {
    let mut state = ready();
    let first = reserve(&mut state, "r1", "alice", "adr", 39);
    let (alice, held) = reserved(&first);
    assert_eq!((alice.number, alice.session_id.as_str()), (40, "alice"));
    assert!(held.is_empty(), "{held:?}");
    let second = reserve(&mut state, "r2", "bob", "adr", 39);
    let (bob, held) = reserved(&second);
    assert_eq!((bob.number, bob.session_id.as_str()), (41, "bob"));
    // The second is told who holds the number before its own.
    assert_eq!(held, std::slice::from_ref(alice));
    assert_eq!(claims(&brief(&state, "alice")), [("adr", 41, "bob")]);
    assert_eq!(claims(&brief(&state, "bob")), [("adr", 40, "alice")]);
    // Another repository counts on its own and is told of neither.
    let third = reserve(&mut state, "r3", "carol", "adr", 5);
    assert_eq!(reserved(&third).0.number, 6);
    assert!(brief(&state, "carol").reserved.is_empty());
}

#[test]
fn a_number_above_every_claim_wins_and_none_is_handed_out_twice() {
    let mut state = ready();
    reserve(&mut state, "r1", "alice", "migration", 0);
    let outcome = reserve(&mut state, "r2", "bob", "migration", 7);
    assert_eq!(reserved(&outcome).0.number, 8);
    // An ended session's claim leaves briefings, but not the counter.
    run(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "bob".into(),
            trail: None,
        },
        11,
    );
    assert!(brief(&state, "alice").reserved.is_empty());
    let outcome = reserve(&mut state, "r3", "alice", "migration", 0);
    let (claim, held) = reserved(&outcome);
    assert_eq!(claim.number, 9);
    assert!(held.is_empty(), "an ended holder is not named: {held:?}");
}

#[test]
fn a_retry_replays_and_the_log_rebuilds_the_counters() {
    let mut state = ready();
    let mut log = Vec::new();
    for (id, session) in [("r1", "alice"), ("r2", "bob")] {
        let command = Command::Reserve {
            session_id: session.into(),
            key: "protocol-version".into(),
            after: 10,
        };
        let decision = decide(&state, &command, id, 10);
        for event in &decision.events {
            apply(&mut state, event).unwrap();
            log.push(event.clone());
        }
    }
    let mut replayed = ready();
    for event in &log {
        apply(&mut replayed, event).unwrap();
    }
    assert_eq!(replayed, state);
    let outcome = reserve(&mut replayed, "r3", "alice", "protocol-version", 0);
    assert_eq!(reserved(&outcome).0.number, 13);
}

#[test]
fn a_key_is_short_and_plain_and_the_keys_are_bounded() {
    let mut state = ready();
    for key in ["", "ADR", "adr number", "a\"b", &"a".repeat(65)] {
        match reserve(&mut state, "bad", "alice", key, 0) {
            Outcome::Error { code, message } => {
                assert_eq!(code, "invalid_key", "{key:?}");
                assert!(message.contains("lowercase"), "{message}");
            }
            other => panic!("{key:?}: {other:?}"),
        }
    }
    match reserve(&mut state, "big", "alice", "adr", u64::MAX) {
        Outcome::Error { code, .. } => assert_eq!(code, "invalid_number"),
        other => panic!("{other:?}"),
    }
    for index in 0..langspan_core::MAX_RESERVATION_KEYS {
        let outcome = reserve(
            &mut state,
            &format!("k{index}"),
            "alice",
            &format!("k{index}"),
            0,
        );
        reserved(&outcome);
    }
    match reserve(&mut state, "over", "bob", "one-more", 0) {
        Outcome::Error { code, message } => {
            assert_eq!(code, "too_many_keys");
            assert!(message.contains("existing key"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    // A key already counted still takes its next number.
    let outcome = reserve(&mut state, "again", "bob", "k0", 0);
    assert_eq!(reserved(&outcome).0.number, 2);
}

fn register(state: &mut State, workspace: &str, session: &str) {
    run(
        state,
        &format!("w-{workspace}"),
        Command::WorkspaceRegister {
            workspace_id: workspace.into(),
            root: format!("/repos/{workspace}"),
        },
        0,
    );
    run(
        state,
        &format!("s-{session}"),
        Command::SessionRegister {
            session_id: session.into(),
            workspace_id: workspace.into(),
            parent: None,
            headless: false,
        },
        0,
    );
}

fn scanned(state: &mut State, id: &str, workspace: &str, failure: Option<&str>) {
    run(
        state,
        id,
        Command::ObserveScan {
            workspace_id: workspace.into(),
            failure: failure.map(Into::into),
            coverage: None,
        },
        1,
    );
}

fn git(state: &mut State, id: &str, workspace: &str, repository: &str) {
    run(
        state,
        id,
        Command::ObserveWorkspaceGit {
            workspace_id: workspace.into(),
            repository_id: repository.into(),
            branch_ref: Some("refs/heads/main".into()),
        },
        1,
    );
}

#[test]
fn a_workspace_not_yet_scanned_is_refused_until_its_repository_is_known() {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    register(&mut state, "one", "alice");
    register(&mut state, "two", "bob");
    // Two new worktrees of one repository, neither scanned yet: counting each
    // alone would give both 1.
    for session in ["alice", "bob"] {
        let before = state.clone();
        match reserve(&mut state, &format!("early-{session}"), session, "adr", 0) {
            Outcome::Error { code, message } => {
                assert_eq!(code, "scope_unknown");
                assert!(message.contains("reserve again"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(state, before, "a refusal records nothing");
    }
    git(&mut state, "g1", "one", "repo-app");
    git(&mut state, "g2", "two", "repo-app");
    let first = reserve(&mut state, "r1", "alice", "adr", 0);
    let second = reserve(&mut state, "r2", "bob", "adr", 0);
    assert_eq!(
        (reserved(&first).0.number, reserved(&second).0.number),
        (1, 2)
    );
}

#[test]
fn a_workspace_outside_git_counts_alone_once_its_scan_failed() {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    register(&mut state, "notes", "alice");
    scanned(
        &mut state,
        "f1",
        "notes",
        Some("the workspace is not a Git working tree"),
    );
    let outcome = reserve(&mut state, "r1", "alice", "adr", 0);
    let (claim, _) = reserved(&outcome);
    assert_eq!((claim.scope.as_str(), claim.number), ("notes", 1));
}

#[test]
fn a_checkout_that_counted_alone_joins_its_repository_above_its_own_numbers() {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    register(&mut state, "one", "alice");
    scanned(&mut state, "f1", "one", Some("git timed out"));
    for id in ["a1", "a2", "a3"] {
        reserve(&mut state, id, "alice", "adr", 0);
    }
    // The scan succeeds later: the repository's counter has not handed out
    // 1 to 3, but this workspace has.
    git(&mut state, "g1", "one", "repo-app");
    let outcome = reserve(&mut state, "r1", "alice", "adr", 0);
    let (claim, _) = reserved(&outcome);
    assert_eq!((claim.scope.as_str(), claim.number), ("repo-app", 4));
}

#[test]
fn a_session_whose_process_exited_is_neither_held_nor_briefed() {
    let mut state = ready();
    reserve(&mut state, "r1", "bob", "adr", 0);
    assert_eq!(claims(&brief(&state, "alice")), [("adr", 1, "bob")]);
    // No end report, but its process is known to have exited.
    run(
        &mut state,
        "l1",
        Command::ObserveSessionLiveness {
            session_id: "bob".into(),
            process: ProcessIdentity {
                pid: 4242,
                start_token: "boot-1".into(),
            },
            status: LivenessStatus::Exited,
        },
        11,
    );
    assert!(brief(&state, "alice").reserved.is_empty());
    let outcome = reserve(&mut state, "r2", "alice", "adr", 0);
    let (claim, held) = reserved(&outcome);
    assert_eq!(claim.number, 2, "the counter keeps its highest number");
    assert!(held.is_empty(), "an exited holder is not named: {held:?}");
}

#[test]
fn a_briefing_names_at_most_eight_claims_newest_first() {
    let mut state = ready();
    for index in 0..12 {
        run(
            &mut state,
            &format!("r{index}"),
            Command::Reserve {
                session_id: "bob".into(),
                key: "adr".into(),
                after: 0,
            },
            100 + index,
        );
    }
    let numbers: Vec<u64> = brief(&state, "alice")
        .reserved
        .iter()
        .map(|claim| claim.number)
        .collect();
    assert_eq!(numbers, [12, 11, 10, 9, 8, 7, 6, 5]);
}
