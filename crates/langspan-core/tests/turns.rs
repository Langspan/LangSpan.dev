//! A push turn: one landing at a time per workspace and target branch.
use langspan_core::{
    AttemptResult, Command, Event, LandingAttempt, LeftQueue, MAX_TURN_QUEUE, MAX_TURN_TTL_MS,
    MAX_TURN_WAIT_MS, Outcome, State, TURN_WAITER_SILENCE_MS, TurnQueueing, apply, decide,
    expiry_events,
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
    state
}

fn take(state: &mut State, id: &str, onto: &str, session: Option<&str>, now: u64) -> Outcome {
    run(
        state,
        id,
        Command::TakeTurn {
            workspace_id: "work".into(),
            onto: onto.into(),
            session_id: session.map(Into::into),
            ttl_ms: 1000,
            turn_id: None,
            queue: None,
        },
        now,
    )
}

fn release(state: &mut State, id: &str, turn: &str, attempts: Vec<LandingAttempt>) -> Outcome {
    run(
        state,
        id,
        Command::ReleaseTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            turn_id: turn.into(),
            attempts,
        },
        50,
    )
}

fn holder(outcome: &Outcome) -> String {
    match outcome {
        Outcome::TurnBusy { holder, held_ms } => {
            assert_eq!(
                *held_ms,
                20 - holder.taken_ms,
                "how long it has held the turn"
            );
            holder.turn_id.clone()
        }
        other => panic!("expected the turn to be busy: {other:?}"),
    }
}

#[test]
fn one_landing_holds_the_turn_until_it_releases() {
    let mut state = ready();
    let Outcome::Turn { turn } = take(&mut state, "t1", "main", Some("claude-1"), 10) else {
        panic!("the first landing takes the turn")
    };
    assert_eq!(
        (turn.turn_id.as_str(), turn.expires_at_ms),
        ("t1", 1010),
        "the turn is named by its request and ends at a deadline"
    );
    assert_eq!(turn.session_id.as_deref(), Some("claude-1"));

    // A second landing onto the same target is told who has it.
    let busy = take(&mut state, "t2", "main", None, 20);
    assert_eq!(holder(&busy), "t1");
    // Another target of the same workspace is independent.
    assert!(matches!(
        take(&mut state, "t3", "release", None, 20),
        Outcome::Turn { .. }
    ));

    let attempts = vec![LandingAttempt {
        target_oid: Some("a".repeat(40)),
        commit_oid: Some("b".repeat(40)),
        result: AttemptResult::Landed {},
    }];
    let decision = decide(
        &state,
        &Command::ReleaseTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            turn_id: "t1".into(),
            attempts: attempts.clone(),
        },
        "r1",
        50,
    );
    assert!(
        matches!(&decision.events[..], [Event::TurnReleased { attempts: recorded, .. }] if *recorded == attempts),
        "every attempt is recorded with the release: {:?}",
        decision.events
    );
    assert!(matches!(
        release(&mut state, "r1", "t1", attempts),
        Outcome::TurnReleased { released: true, .. }
    ));
    assert!(matches!(
        take(&mut state, "t4", "main", None, 60),
        Outcome::Turn { .. }
    ));
}

#[test]
fn releasing_a_turn_not_held_changes_nothing_and_says_so() {
    let mut state = ready();
    take(&mut state, "t1", "main", None, 10);
    // Somebody else's turn is not released by naming the wrong one.
    assert!(matches!(
        release(&mut state, "r1", "t-other", vec![]),
        Outcome::TurnReleased {
            released: false,
            ..
        }
    ));
    assert_eq!(holder(&take(&mut state, "t2", "main", None, 20)), "t1");
}

#[test]
fn a_turn_whose_holder_vanished_expires_at_its_deadline() {
    let mut state = ready();
    take(&mut state, "t1", "main", None, 10);
    assert!(expiry_events(&state, 1009).is_empty(), "nothing ends early");
    let events = expiry_events(&state, 1010);
    assert_eq!(
        events,
        [Event::TurnExpired {
            workspace_id: "work".into(),
            onto: "main".into(),
            turn_id: "t1".into(),
        }]
    );
    // Even before the writer has applied the expiry, a turn past its
    // deadline does not block: it is expired first, then taken.
    let decision = decide(
        &state,
        &Command::TakeTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            session_id: None,
            ttl_ms: 1000,
            turn_id: None,
            queue: None,
        },
        "t2",
        1010,
    );
    assert!(
        matches!(
            &decision.events[..],
            [Event::TurnExpired { .. }, Event::TurnTaken { .. }]
        ),
        "{:?}",
        decision.events
    );
    for event in events {
        apply(&mut state, &event).unwrap();
    }
    assert!(state.turns.is_empty());
    assert!(matches!(
        take(&mut state, "t2", "main", None, 1010),
        Outcome::Turn { .. }
    ));
}

#[test]
fn the_holder_extends_its_own_turn_and_a_lost_turn_is_refused() {
    let mut state = ready();
    take(&mut state, "t1", "main", None, 10);
    let renew = |state: &mut State, id: &str, turn: &str, now: u64| {
        run(
            state,
            id,
            Command::TakeTurn {
                workspace_id: "work".into(),
                onto: "main".into(),
                session_id: None,
                ttl_ms: 5000,
                turn_id: Some(turn.into()),
                queue: None,
            },
            now,
        )
    };
    let Outcome::Turn { turn } = renew(&mut state, "n1", "t1", 900) else {
        panic!("the holder renews")
    };
    assert_eq!((turn.turn_id.as_str(), turn.expires_at_ms), ("t1", 5900));
    assert!(
        expiry_events(&state, 1010).is_empty(),
        "the old deadline no longer ends it"
    );
    // Somebody else cannot extend a turn they do not hold.
    assert!(matches!(
        renew(&mut state, "n2", "t-other", 1000),
        Outcome::Error { ref code, .. } if code == "turn_lost"
    ));
    // Once it has ended, the holder is told it lost the turn.
    for event in expiry_events(&state, 5900) {
        apply(&mut state, &event).unwrap();
    }
    assert!(matches!(
        renew(&mut state, "n3", "t1", 5901),
        Outcome::Error { ref code, .. } if code == "turn_lost"
    ));
}

#[test]
fn a_restart_ends_every_turn() {
    let mut state = ready();
    take(&mut state, "t1", "main", None, 10);
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert!(state.turns.is_empty(), "no turn survives the daemon");
    assert!(matches!(
        take(&mut state, "t2", "main", None, 20),
        Outcome::Turn { .. }
    ));
}

#[test]
fn retrying_the_same_request_is_the_same_turn() {
    let mut state = ready();
    let Outcome::Turn { turn: first } = take(&mut state, "t1", "main", None, 10) else {
        panic!()
    };
    let decision = decide(
        &state,
        &Command::TakeTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            session_id: None,
            ttl_ms: 1000,
            turn_id: None,
            queue: None,
        },
        "t1",
        30,
    );
    assert!(decision.events.is_empty(), "no second grant");
    assert_eq!(decision.outcome, Outcome::Turn { turn: first });
}

#[test]
fn invalid_turn_requests_are_rejected_without_events() {
    let state = ready();
    let base = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut value = serde_json::json!({
            "op": "take_turn", "workspace_id": "work", "onto": "main",
            "session_id": null, "ttl_ms": 1000,
        });
        edit(&mut value);
        value
    };
    for value in [
        base(&|v| v["workspace_id"] = "nowhere".into()),
        base(&|v| v["onto"] = "".into()),
        base(&|v| v["onto"] = "has space".into()),
        base(&|v| v["session_id"] = "bad id".into()),
        base(&|v| v["ttl_ms"] = 0.into()),
        base(&|v| v["ttl_ms"] = (MAX_TURN_TTL_MS + 1).into()),
    ] {
        let command: Command = serde_json::from_value(value.clone()).unwrap();
        let decision = decide(&state, &command, "bad", 1);
        assert!(matches!(decision.outcome, Outcome::Error { .. }), "{value}");
        assert!(decision.events.is_empty());
    }
    let attempt = LandingAttempt {
        target_oid: Some("not-an-oid".into()),
        commit_oid: None,
        result: AttemptResult::Moved {},
    };
    for attempts in [
        vec![attempt],
        vec![
            LandingAttempt {
                target_oid: None,
                commit_oid: None,
                result: AttemptResult::Error {},
            };
            4
        ],
    ] {
        let decision = decide(
            &state,
            &Command::ReleaseTurn {
                workspace_id: "work".into(),
                onto: "main".into(),
                turn_id: "t1".into(),
                attempts,
            },
            "r",
            1,
        );
        assert!(
            matches!(decision.outcome, Outcome::Error { .. }),
            "{decision:?}"
        );
    }
    // An event that would give one target two holders is refused on replay.
    let mut state = ready();
    take(&mut state, "t1", "main", None, 10);
    let mut turn = state.turns.values().next().unwrap().clone();
    turn.turn_id = "t2".into();
    assert!(apply(&mut state, &Event::TurnTaken { turn }).is_err());
}

/// Ask for the turn while willing to wait for `wait` ms, as landing
/// `ticket`; `joined` once it has been told it is waiting.
fn wait(state: &mut State, ticket: &str, joined: bool, wait: u64, now: u64) -> Outcome {
    run(
        state,
        &format!("{ticket}-ask-{now}"),
        Command::TakeTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            session_id: Some(format!("s-{ticket}")),
            ttl_ms: 100_000,
            turn_id: None,
            queue: Some(TurnQueueing {
                ticket: ticket.into(),
                wait_ms: wait,
                joined,
            }),
        },
        now,
    )
}

fn position(outcome: &Outcome) -> u32 {
    match outcome {
        Outcome::TurnWaiting { position, .. } => *position,
        other => panic!("expected to be waiting: {other:?}"),
    }
}

fn line(state: &State, now: u64) -> Vec<String> {
    match decide(
        state,
        &Command::TurnQueue {
            workspace_id: "work".into(),
            onto: "main".into(),
        },
        "read",
        now,
    )
    .outcome
    {
        Outcome::TurnQueue { waiting, .. } => waiting.into_iter().map(|w| w.ticket).collect(),
        other => panic!("{other:?}"),
    }
}

fn expire(state: &mut State, now: u64) -> Vec<Event> {
    let events = expiry_events(state, now);
    for event in &events {
        apply(state, event).unwrap();
    }
    events
}

#[test]
fn landings_that_find_the_turn_held_wait_in_line_and_take_it_first_come_first_served() {
    let mut state = ready();
    assert!(matches!(
        wait(&mut state, "a", false, 60_000, 10),
        Outcome::Turn { ref turn } if turn.turn_id == "a"
    ));
    assert_eq!(position(&wait(&mut state, "b", false, 60_000, 20)), 1);
    assert_eq!(position(&wait(&mut state, "c", false, 60_000, 30)), 2);
    // Asking again keeps the place; it does not join twice.
    assert_eq!(position(&wait(&mut state, "c", true, 60_000, 40)), 2);
    assert_eq!(
        position(&wait(&mut state, "b", false, 60_000, 40)),
        1,
        "a retry keeps its place"
    );
    let Outcome::TurnQueue {
        holder,
        held_ms,
        waiting,
    } = decide(
        &state,
        &Command::TurnQueue {
            workspace_id: "work".into(),
            onto: "main".into(),
        },
        "read",
        50,
    )
    .outcome
    else {
        panic!()
    };
    assert_eq!(holder.unwrap().turn_id, "a");
    assert_eq!(held_ms, Some(40));
    assert_eq!(
        waiting
            .iter()
            .map(|w| (w.ticket.as_str(), w.position, w.waited_ms, w.wait_left_ms))
            .collect::<Vec<_>>(),
        [("b", 1, 30, 59_970), ("c", 2, 20, 59_980)]
    );
    assert_eq!(waiting[0].session_id.as_deref(), Some("s-b"));

    release(&mut state, "r-a", "a", vec![]);
    // The turn is free, but only the first in line may take it: not the
    // second, and not a landing that did not wait at all.
    assert_eq!(position(&wait(&mut state, "c", true, 60_000, 60)), 2);
    assert!(matches!(
        take(&mut state, "d", "main", None, 60),
        Outcome::TurnUnavailable { holder: None, queue_full: false, ref waiting, .. } if waiting.len() == 2
    ));
    assert!(matches!(
        wait(&mut state, "b", true, 60_000, 70),
        Outcome::Turn { ref turn } if turn.turn_id == "b"
    ));
    assert_eq!(line(&state, 70), ["c"], "taking the turn leaves the line");
    assert_eq!(position(&wait(&mut state, "c", true, 60_000, 80)), 1);
}

#[test]
fn the_line_is_bounded() {
    let mut state = ready();
    wait(&mut state, "holder", false, 60_000, 0);
    for n in 0..MAX_TURN_QUEUE {
        assert_eq!(
            position(&wait(&mut state, &format!("w{n}"), false, 60_000, 1)),
            n as u32 + 1
        );
    }
    assert!(matches!(
        wait(&mut state, "one-too-many", false, 60_000, 2),
        Outcome::TurnUnavailable { queue_full: true, ref waiting, .. } if waiting.len() == MAX_TURN_QUEUE
    ));
    assert_eq!(line(&state, 2).len(), MAX_TURN_QUEUE);
    for wait_ms in [0, MAX_TURN_WAIT_MS + 1] {
        assert!(matches!(
            wait(&mut state, "bad-wait", false, wait_ms, 3),
            Outcome::Error { ref code, .. } if code == "invalid_wait"
        ));
    }
}

#[test]
fn a_waiting_landing_leaves_when_its_wait_runs_out_or_it_stops_asking() {
    let mut state = ready();
    wait(&mut state, "holder", false, 60_000, 0);
    wait(&mut state, "short", false, 10_000, 10);
    wait(&mut state, "silent", false, 60_000, 10);
    // Asked again before half its silence allowance is used: no event.
    let decision = decide(
        &state,
        &Command::TakeTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            session_id: None,
            ttl_ms: 1000,
            turn_id: None,
            queue: Some(TurnQueueing {
                ticket: "short".into(),
                wait_ms: 10_000,
                joined: true,
            }),
        },
        "short-again",
        1_000,
    );
    assert!(decision.events.is_empty(), "{:?}", decision.events);
    // Later, it is: the landing stays in line while it keeps asking.
    let decision = decide(
        &state,
        &Command::TakeTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            session_id: None,
            ttl_ms: 1000,
            turn_id: None,
            queue: Some(TurnQueueing {
                ticket: "short".into(),
                wait_ms: 10_000,
                joined: true,
            }),
        },
        "short-later",
        TURN_WAITER_SILENCE_MS / 2 + 100,
    );
    assert!(matches!(
        &decision.events[..],
        [Event::TurnQueueSeen { .. }]
    ));
    assert!(expire(&mut state, 10_009).is_empty(), "nothing ends early");
    assert_eq!(
        expire(&mut state, 10_010),
        [Event::TurnQueueLeft {
            workspace_id: "work".into(),
            onto: "main".into(),
            ticket: "short".into(),
            reason: LeftQueue::TimedOut,
        }]
    );
    assert_eq!(
        expire(&mut state, 10 + TURN_WAITER_SILENCE_MS),
        [Event::TurnQueueLeft {
            workspace_id: "work".into(),
            onto: "main".into(),
            ticket: "silent".into(),
            reason: LeftQueue::Stopped,
        }],
        "a landing that stopped asking does not hold up the line"
    );
    // A landing that is no longer in line is told so; it does not rejoin.
    assert!(matches!(
        wait(&mut state, "silent", true, 60_000, 20_000),
        Outcome::Error { ref code, .. } if code == "not_queued"
    ));
    assert!(line(&state, 20_000).is_empty());
}

#[test]
fn a_waiting_landing_can_be_cancelled_and_a_restart_empties_the_line() {
    let mut state = ready();
    wait(&mut state, "holder", false, 60_000, 0);
    wait(&mut state, "w1", false, 60_000, 1);
    wait(&mut state, "w2", false, 60_000, 1);
    let leave = |state: &mut State, ticket: &str, reason| {
        run(
            state,
            &format!("leave-{ticket}"),
            Command::LeaveTurnQueue {
                workspace_id: "work".into(),
                onto: "main".into(),
                ticket: ticket.into(),
                reason,
            },
            2,
        )
    };
    assert_eq!(
        leave(&mut state, "w1", LeftQueue::Cancelled),
        Outcome::TurnQueueLeft {
            ticket: "w1".into(),
            left: true
        }
    );
    assert_eq!(line(&state, 2), ["w2"]);
    assert!(matches!(
        wait(&mut state, "w1", true, 60_000, 3),
        Outcome::Error { ref code, .. } if code == "not_queued"
    ));
    assert_eq!(
        leave(&mut state, "w1", LeftQueue::Cancelled),
        Outcome::TurnQueueLeft {
            ticket: "w1".into(),
            left: false
        },
        "cancelling again changes nothing"
    );
    // The holder is not stopped by cancelling it.
    assert!(matches!(
        leave(&mut state, "holder", LeftQueue::Cancelled),
        Outcome::TurnQueueLeft { left: false, .. }
    ));
    assert!(matches!(
        leave(&mut state, "w2", LeftQueue::Stopped),
        Outcome::Error { ref code, .. } if code == "invalid_reason"
    ));
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert!(
        state.turn_queues.is_empty(),
        "nobody waits across a restart"
    );
    assert!(matches!(
        wait(&mut state, "w2", true, 60_000, 4),
        Outcome::Error { ref code, .. } if code == "not_queued"
    ));
}

#[test]
fn the_dashboard_shows_each_push_turn_and_its_line() {
    let mut state = ready();
    wait(&mut state, "a", false, 60_000, 10);
    wait(&mut state, "b", false, 60_000, 20);
    let Outcome::Dashboard { dashboard } = decide(&state, &Command::Dashboard, "read", 30).outcome
    else {
        panic!()
    };
    assert_eq!(dashboard.push_turns.len(), 1);
    let row = &dashboard.push_turns[0];
    assert_eq!(
        (row.onto.as_str(), row.holder.as_deref(), row.held_ms),
        ("main", Some("a"), Some(20))
    );
    assert_eq!(row.holder_session.as_deref(), Some("s-a"));
    assert_eq!(row.waiting.len(), 1);
    assert_eq!(
        (row.waiting[0].ticket.as_str(), row.waiting[0].waited_ms),
        ("b", 10)
    );
}
