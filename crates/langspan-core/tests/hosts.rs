//! Hosts present with no LangSpan install (roadmap A7): recorded once while
//! nothing changes, cleared when covered, and kept across a restart.
use langspan_core::{Command, Event, MAX_HOSTS, Outcome, State, apply, decide, status_records};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> (Outcome, Vec<Event>) {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    (decision.outcome, decision.events)
}

fn observe(names: &[&str]) -> Command {
    Command::ObserveHosts {
        not_installed: names.iter().map(|name| (*name).into()).collect(),
    }
}

#[test]
fn a_host_is_recorded_once_and_cleared_when_covered() {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();

    let (outcome, events) = run(&mut state, "h1", observe(&["cursor"]), 10);
    assert_eq!(
        outcome,
        Outcome::HostsObserved {
            not_installed: vec!["cursor".into()]
        }
    );
    assert!(
        matches!(&events[..], [Event::HostNotInstalled { host }] if host.host == "cursor" && host.at_ms == 10),
        "{events:?}"
    );
    // The same answer again is not a new fact.
    for now in [20, 30] {
        let (_, events) = run(&mut state, &format!("h{now}"), observe(&["cursor"]), now);
        assert!(events.is_empty(), "{events:?}");
    }
    assert_eq!(state.uninstalled_hosts["cursor"].at_ms, 10);

    // A daemon restart replays the record: still once.
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    let (_, events) = run(&mut state, "h40", observe(&["cursor"]), 40);
    assert!(events.is_empty(), "{events:?}");

    // Covered now: cleared, and a later loss of coverage is recorded anew.
    let (_, events) = run(&mut state, "h50", observe(&[]), 50);
    assert_eq!(
        events,
        [Event::HostCleared {
            host: "cursor".into()
        }]
    );
    assert!(state.uninstalled_hosts.is_empty());
    let (_, events) = run(&mut state, "h60", observe(&[]), 60);
    assert!(events.is_empty());
    let (_, events) = run(&mut state, "h70", observe(&["codex", "cursor"]), 70);
    assert_eq!(events.len(), 2, "{events:?}");

    // The bounded status view and the dashboard both carry them.
    let (Outcome::Status { state: view, .. }, _) = run(&mut state, "read", Command::Status, 71)
    else {
        panic!("status expected");
    };
    assert_eq!(view.uninstalled_hosts, state.uninstalled_hosts);
    let (Outcome::Dashboard { dashboard }, _) = run(&mut state, "read", Command::Dashboard, 72)
    else {
        panic!("dashboard expected");
    };
    assert_eq!(dashboard.uninstalled_hosts, ["codex", "cursor"]);

    // `status --all` carries the record and rebuilds it.
    let (_, records) = status_records(&state, None).unwrap();
    let mut rebuilt = State::default();
    for (_, record) in records {
        record.apply(&mut rebuilt).unwrap();
    }
    assert_eq!(rebuilt.uninstalled_hosts, state.uninstalled_hosts);
}

#[test]
fn a_host_check_with_bad_names_records_nothing() {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    let too_many: Vec<String> = (0..=MAX_HOSTS)
        .map(|n| format!("host-{}", "x".repeat(n)))
        .collect();
    for command in [
        observe(&["Cursor"]),
        observe(&[""]),
        observe(&["cursor", "cursor"]),
        observe(&["../etc"]),
        observe(&[&"a".repeat(33)]),
        Command::ObserveHosts {
            not_installed: too_many,
        },
    ] {
        let decision = decide(&state, &command, "bad", 1);
        assert!(
            matches!(&decision.outcome, Outcome::Error { code, .. } if code == "invalid_hosts"),
            "{command:?}: {:?}",
            decision.outcome
        );
        assert!(decision.events.is_empty());
    }
}
