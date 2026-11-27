//! Reserving the next number of a named counter (issue #299, ADR 0040): two
//! sessions in separate worktrees of one repository get different numbers,
//! and each one's briefing names the other's claim. Pure decisions and
//! replay, in process.
use langspan_core::{Briefing, Command, Event, Outcome, Reservation, State, apply, decide};

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
        let outcome = reserve(&mut state, &format!("k{index}"), "alice", &format!("k{index}"), 0);
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
