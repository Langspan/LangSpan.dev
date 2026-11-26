//! Liveness from recent hook runs (roadmap J12): a session that only reads or
//! runs commands stays current while its hooks keep reaching the daemon, and
//! leaves peers' briefings once they stop. Times are injected.
use langspan_core::{
    BRIEF_HORIZON_MS, Briefing, Command, Event, Outcome, PRESENCE_WINDOW_MS, PresenceKind,
    SESSION_SEEN_INTERVAL_MS, State, acting_session, apply, decide, status_view,
};

const MINUTE: u64 = 60 * 1000;

fn run(state: &mut State, id: &str, command: Command, now: u64) -> (Outcome, Vec<Event>) {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    assert!(
        !matches!(decision.outcome, Outcome::Error { .. }),
        "{:?}",
        decision.outcome
    );
    (decision.outcome, decision.events)
}

fn seen(state: &mut State, session: &str, now: u64) -> Vec<Event> {
    run(
        state,
        &format!("seen-{session}-{now}"),
        Command::SessionSeen {
            session_id: session.into(),
        },
        now,
    )
    .1
}

/// Alice and Bob in one checkout. Alice states her task at time zero.
fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    run(
        &mut state,
        "workspace",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        0,
    );
    for session in ["alice", "bob"] {
        run(
            &mut state,
            &format!("register-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    run(
        &mut state,
        "prompt-alice",
        Command::SessionPrompt {
            session_id: "alice".into(),
            summary: "Trace the upload failure".into(),
            line_digests: Vec::new(),
        },
        0,
    );
    state
}

fn brief(state: &mut State, now: u64) -> Briefing {
    match run(
        state,
        &format!("brief-bob-{now}"),
        Command::Brief {
            session_id: "bob".into(),
            max_peers: 8,
            deliver: false,
            mid_turn: false,
            left: Vec::new(),
        },
        now,
    )
    .0
    {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

fn lists_alice(briefing: &Briefing) -> bool {
    briefing
        .peers
        .iter()
        .any(|peer| peer.session_id == "alice" && peer.summary.is_some())
}

#[test]
fn a_session_that_only_reads_for_forty_minutes_stays_in_a_peers_briefing() {
    let mut state = ready();
    let mut quiet = state.clone();
    // Reads and test runs: a hook run every five minutes, no edit, no prompt.
    for minute in (5..=40).step_by(5) {
        seen(&mut state, "alice", minute * MINUTE);
    }
    let briefing = brief(&mut state, 40 * MINUTE);
    assert!(lists_alice(&briefing), "{briefing:?}");
    // Without those hook runs her task line is older than the horizon.
    assert!(!lists_alice(&brief(&mut quiet, 40 * MINUTE)));
}

#[test]
fn a_session_with_no_hook_run_for_thirty_one_minutes_leaves_peers_briefings() {
    let mut state = ready();
    seen(&mut state, "alice", 5 * MINUTE);
    assert!(lists_alice(&brief(
        &mut state,
        5 * MINUTE + BRIEF_HORIZON_MS
    )));
    assert!(!lists_alice(&brief(&mut state, 36 * MINUTE)));
}

#[test]
fn a_burst_of_hook_runs_records_at_most_one_sighting_per_interval() {
    let mut state = ready();
    let mut recorded = 0;
    // One hundred hook runs within one interval, then one at its end.
    for run in 0..100 {
        recorded += seen(&mut state, "alice", MINUTE + run * 100).len();
    }
    assert_eq!(recorded, 1);
    assert_eq!(state.activity["alice"].last_hook_ms, MINUTE);
    assert!(seen(&mut state, "alice", MINUTE + SESSION_SEEN_INTERVAL_MS - 1).is_empty());
    let later = seen(&mut state, "alice", MINUTE + SESSION_SEEN_INTERVAL_MS);
    assert_eq!(
        later,
        vec![Event::SessionSeen {
            session_id: "alice".into(),
            at_ms: MINUTE + SESSION_SEEN_INTERVAL_MS,
            generation: 1,
        }]
    );
    // Bob's first hook run is recorded however recently Alice's was.
    assert_eq!(seen(&mut state, "bob", MINUTE + 1).len(), 1);
}

#[test]
fn a_sighting_never_extends_an_admission() {
    let mut state = ready();
    let (outcome, _) = run(
        &mut state,
        "admit",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/api.rs".into()],
            ttl_ms: 1000,
        },
        MINUTE,
    );
    let Outcome::Admitted { lease } = outcome else {
        panic!("{outcome:?}")
    };
    let leases = state.leases.clone();
    assert_eq!(seen(&mut state, "alice", MINUTE + 900).len(), 1);
    assert_eq!(state.leases, leases, "a sighting touches no admission");
    let expired = langspan_core::expiry_events(&state, MINUTE + 1000);
    assert!(
        expired.iter().any(|event| matches!(
            event,
            Event::LeaseExpired { lease_id } if *lease_id == lease.lease_id
        )),
        "{expired:?}"
    );
}

#[test]
fn a_sighting_starts_over_in_a_new_generation_and_needs_a_current_session() {
    let mut state = ready();
    seen(&mut state, "alice", 10 * MINUTE);
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    // Not registered in this generation: refused, nothing recorded.
    let refused = decide(
        &state,
        &Command::SessionSeen {
            session_id: "alice".into(),
        },
        "seen-old",
        1,
    );
    assert!(refused.events.is_empty());
    assert!(matches!(refused.outcome, Outcome::Error { .. }));
    run(
        &mut state,
        "register-again",
        Command::SessionRegister {
            session_id: "alice".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        1,
    );
    // The earlier generation's time is not compared with this one's.
    assert_eq!(seen(&mut state, "alice", 2).len(), 1);
    assert_eq!(state.activity["alice"].last_hook_ms, 2);
    assert_eq!(state.activity["alice"].generation, 2);
}

#[test]
fn status_shows_each_sessions_last_hook_age() {
    let mut state = ready();
    seen(&mut state, "alice", 10 * MINUTE);
    let (_, summary) = status_view(&state, 12 * MINUTE);
    assert_eq!(summary.last_hook_age_ms.get("alice"), Some(&(2 * MINUTE)));
    assert_eq!(summary.last_hook_age_ms.get("bob"), None);
}

#[test]
fn a_sighting_is_a_versioned_wire_record() {
    let command: Command =
        serde_json::from_str(r#"{"op":"session_seen","session_id":"alice"}"#).unwrap();
    assert_eq!(
        command,
        Command::SessionSeen {
            session_id: "alice".into()
        }
    );
    let event = Event::SessionSeen {
        session_id: "alice".into(),
        at_ms: 5,
        generation: 1,
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"session_seen","session_id":"alice","at_ms":5,"generation":1}"#
    );
    // Activity recorded before this field existed is still read.
    let old: langspan_core::SessionActivity =
        serde_json::from_str(r#"{"session_id":"alice","generation":1,"last_ms":3}"#).unwrap();
    assert_eq!(old.last_hook_ms, 0);
}

/// A hook run that only searches, runs a shell command or sends its presence
/// setting is the session at work, as a read is (roadmap A13 with J12).
#[test]
fn a_presence_request_is_the_session_at_work() {
    for command in [
        Command::ObservePresence {
            session_id: "alice".into(),
            kind: PresenceKind::Search,
            paths: vec!["src".into()],
        },
        Command::SetPresence {
            session_id: "alice".into(),
            shown: false,
            window_ms: PRESENCE_WINDOW_MS,
        },
    ] {
        assert_eq!(acting_session(&command), Some("alice"), "{command:?}");
    }
}
