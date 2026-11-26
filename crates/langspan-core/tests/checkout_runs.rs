//! A checkout change a session's own shell command explains (issue #139):
//! not told to that session, and named to the others.
use langspan_core::{
    CHECKOUT_RUN_GRACE_MS, CheckoutNotice, Command, Event, Outcome, State, ToolResult, apply,
    decide,
};

const MAIN: &str = "refs/heads/main";
const OTHER: &str = "refs/heads/other";

fn oid(digit: char) -> String {
    digit.to_string().repeat(40)
}

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

/// Alice and Bob in one checkout, each with an edit at time 10.
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
        run(
            &mut state,
            &format!("o-{session}"),
            Command::Observe {
                session_id: session.into(),
                operation_id: format!("op-{session}"),
                source: "test".into(),
                tool: "Edit".into(),
                paths: vec![format!("{session}.txt")],
                result: ToolResult::Succeeded,
                lease_id: None,
            },
            10,
        );
    }
    state
}

/// The scan sees HEAD move on `branch` to `to`, at `now`.
fn checkout(state: &mut State, id: &str, branch: &str, to: char, now: u64) {
    let outcome = run(
        state,
        id,
        Command::ObserveCheckout {
            workspace_id: "work".into(),
            from_branch: Some(MAIN.into()),
            to_branch: Some(branch.into()),
            from_head: Some(oid('a')),
            to_head: Some(oid(to)),
            files: 2,
        },
        now,
    );
    assert!(
        matches!(outcome, Outcome::CheckoutObserved { .. }),
        "{outcome:?}"
    );
}

/// `session`'s `git merge` ran for `ran` ms and left `main` at `to`;
/// reported at `now`, as it finished.
fn merged(state: &mut State, session: &str, to: char, ran: u64, now: u64) -> Outcome {
    reported(state, session, to, ran, 0, now)
}

/// As `merged`, finished `ago` ms before it was reported at `now`.
fn reported(state: &mut State, session: &str, to: char, ran: u64, ago: u64, now: u64) -> Outcome {
    run(
        state,
        &format!("run-{session}-{now}"),
        Command::ObserveCheckoutRun {
            session_id: session.into(),
            command: Some("git merge origin/main".into()),
            to_branch: Some(MAIN.into()),
            to_head: Some(oid(to)),
            ran_ms: ran,
            finished_ago_ms: ago,
            files_unchanged: false,
        },
        now,
    )
}

/// `session`'s `git commit` ran for `ran` ms and moved `main` to `to`,
/// changing no file; reported at `now`, as it finished.
fn committed(state: &mut State, session: &str, to: char, ran: u64, now: u64) -> Outcome {
    run(
        state,
        &format!("commit-{session}-{now}"),
        Command::ObserveCheckoutRun {
            session_id: session.into(),
            command: Some("git commit -m work".into()),
            to_branch: Some(MAIN.into()),
            to_head: Some(oid(to)),
            ran_ms: ran,
            finished_ago_ms: 0,
            files_unchanged: true,
        },
        now,
    )
}

/// The scan sees `files` restored with branch `main` and HEAD `at` in place.
fn restored(state: &mut State, id: &str, at: char, now: u64) {
    let outcome = run(
        state,
        id,
        Command::ObserveCheckout {
            workspace_id: "work".into(),
            from_branch: Some(MAIN.into()),
            to_branch: Some(MAIN.into()),
            from_head: Some(oid(at)),
            to_head: Some(oid(at)),
            files: 2,
        },
        now,
    );
    assert!(
        matches!(outcome, Outcome::CheckoutObserved { .. }),
        "{outcome:?}"
    );
}

fn notices(state: &mut State, session: &str, now: u64) -> Vec<CheckoutNotice> {
    match run(
        state,
        &format!("b-{session}-{now}"),
        Command::Brief {
            session_id: session.into(),
            max_peers: 4,
            deliver: false,
            mid_turn: false,
            recent: false,
            recent_window_ms: None,
            mcp: false,
        },
        now,
    ) {
        Outcome::Briefing { briefing } => briefing.checkout_changes,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_change_observed_while_the_command_ran_is_its_sessions_own() {
    let mut state = ready();
    checkout(&mut state, "c1", MAIN, 'b', 1_000);
    // The scan saw it before the hook reported: the harder order.
    let outcome = merged(&mut state, "alice", 'b', 300, 1_100);
    assert!(
        matches!(&outcome, Outcome::CheckoutRunObserved { run } if run.started_ms == 800 && run.finished_ms == 1_100),
        "{outcome:?}"
    );
    assert_eq!(notices(&mut state, "alice", 1_200), []);
    let bob = notices(&mut state, "bob", 1_200);
    assert_eq!(bob.len(), 1);
    assert_eq!(bob[0].by_session.as_deref(), Some("alice"));
    assert_eq!(bob[0].command.as_deref(), Some("git merge origin/main"));
}

#[test]
fn the_first_change_after_the_command_at_the_checkout_it_left_is_its_own() {
    let mut state = ready();
    merged(&mut state, "alice", 'b', 300, 1_000);
    // The scan sees it only after the hook reported.
    checkout(&mut state, "c1", MAIN, 'b', 3_000);
    assert_eq!(notices(&mut state, "alice", 3_100), []);
    assert_eq!(
        notices(&mut state, "bob", 3_100)[0].by_session.as_deref(),
        Some("alice")
    );
    // A later change, even back to the same state, is nobody's.
    checkout(&mut state, "c2", OTHER, 'c', 4_000);
    checkout(&mut state, "c3", MAIN, 'b', 5_000);
    let alice = notices(&mut state, "alice", 5_100);
    assert_eq!(alice.len(), 2, "{alice:?}");
    assert!(alice.iter().all(|notice| notice.by_session.is_none()));
}

/// Once a scan while the command ran saw the checkout it left, a later
/// change at that same state within the grace period (the owner restoring
/// dirty files, with branch and HEAD unchanged) came after it: not its own.
#[test]
fn a_change_after_the_run_whose_result_was_already_seen_is_not_its_own() {
    let mut state = ready();
    checkout(&mut state, "c1", MAIN, 'b', 1_000);
    merged(&mut state, "alice", 'b', 300, 1_100);
    restored(&mut state, "c2", 'b', 5_000);
    let alice = notices(&mut state, "alice", 5_100);
    assert_eq!(alice.len(), 1, "{alice:?}");
    assert_eq!(alice[0].by_session, None);
    let bob = notices(&mut state, "bob", 5_100);
    assert_eq!(bob.len(), 2, "{bob:?}");
    assert_eq!(
        bob.iter()
            .filter(|notice| notice.by_session.as_deref() == Some("alice"))
            .count(),
        1,
        "{bob:?}"
    );

    // A scan while it ran that saw only a step on the way (a rebase's
    // detour) leaves the grace match for the state it left.
    let mut state = ready();
    checkout(&mut state, "c1", OTHER, 'c', 1_000);
    merged(&mut state, "alice", 'b', 300, 1_100);
    checkout(&mut state, "c2", MAIN, 'b', 3_000);
    assert_eq!(notices(&mut state, "alice", 3_100), []);
}

/// A commit moves HEAD and changes no file, so the scan records no checkout
/// change for it. A restore of dirty files the owner makes afterward, within
/// the grace period, at the branch and HEAD the commit left, is the first
/// change after the run, but not its own.
#[test]
fn a_restore_after_a_commit_that_changed_no_file_is_not_its_own() {
    let mut state = ready();
    committed(&mut state, "alice", 'b', 300, 1_000);
    restored(&mut state, "c1", 'b', 5_000);
    let alice = notices(&mut state, "alice", 5_100);
    assert_eq!(alice.len(), 1, "{alice:?}");
    assert_eq!(alice[0].by_session, None);
    let bob = notices(&mut state, "bob", 5_100);
    assert_eq!(bob.len(), 1, "{bob:?}");
    assert_eq!(bob[0].by_session, None);

    // Such a run still explains a change that moved HEAD: a `reset --soft`
    // whose staged files the scan sees after the hook reported.
    let mut state = ready();
    committed(&mut state, "alice", 'b', 300, 1_000);
    checkout(&mut state, "c1", MAIN, 'b', 3_000);
    assert_eq!(notices(&mut state, "alice", 3_100), []);

    // And a restore while it ran, as any run does.
    let mut state = ready();
    restored(&mut state, "c1", 'b', 900);
    committed(&mut state, "alice", 'b', 300, 1_000);
    assert_eq!(notices(&mut state, "alice", 1_100), []);

    // A run that changed files explains a later restore as before: a stash.
    let mut state = ready();
    merged(&mut state, "alice", 'b', 300, 1_000);
    restored(&mut state, "c1", 'b', 3_000);
    assert_eq!(notices(&mut state, "alice", 3_100), []);
}

/// Commits add checkout runs but no checkout change, so the two lists move
/// independently. Later runs do not push out the run that explains a change
/// still held.
#[test]
fn later_commits_keep_the_run_that_explains_a_held_change() {
    let mut state = ready();
    checkout(&mut state, "c1", MAIN, 'b', 1_000);
    merged(&mut state, "alice", 'b', 300, 1_100);
    for i in 0..langspan_core::MAX_CHECKOUT_RUNS as u64 + 2 {
        committed(&mut state, "bob", 'c', 100, 2_000 + i * 1_000);
    }
    assert_eq!(notices(&mut state, "alice", 20_000), []);
    let bob = notices(&mut state, "bob", 20_000);
    assert_eq!(bob.len(), 1, "{bob:?}");
    assert_eq!(bob[0].by_session.as_deref(), Some("alice"));
    let runs = &state.checkout_runs["work"];
    assert!(
        runs.len() <= langspan_core::MAX_CHECKOUT_RUNS + langspan_core::MAX_CHECKOUTS,
        "{runs:?}"
    );
}

#[test]
fn a_change_the_command_did_not_leave_or_that_came_too_late_or_too_early_is_told() {
    // At another HEAD than the command left.
    let mut state = ready();
    merged(&mut state, "alice", 'b', 300, 1_000);
    checkout(&mut state, "c1", MAIN, 'c', 2_000);
    let alice = notices(&mut state, "alice", 2_100);
    assert_eq!(alice.len(), 1);
    assert_eq!(alice[0].by_session, None);

    // At the state it left, but after the grace period.
    let mut state = ready();
    merged(&mut state, "alice", 'b', 300, 1_000);
    let late = 1_000 + CHECKOUT_RUN_GRACE_MS + 1;
    checkout(&mut state, "c1", MAIN, 'b', late);
    assert_eq!(notices(&mut state, "alice", late + 1).len(), 1);

    // Before the command started.
    let mut state = ready();
    checkout(&mut state, "c1", MAIN, 'b', 500);
    merged(&mut state, "alice", 'b', 300, 1_000);
    let alice = notices(&mut state, "alice", 1_100);
    assert_eq!(alice.len(), 1);
    assert_eq!(alice[0].by_session, None);
}

#[test]
fn a_redelivered_report_keeps_the_commands_own_window() {
    // Alice's one-second merge ran from 9_000 to 10_000; the scan saw it at
    // 10_500. Another checkout change, nobody's, came at 69_500. Alice's
    // post hook reached the daemon only at 70_000, on redelivery: 60 s after
    // the merge finished, as the hook measured.
    let mut state = ready();
    checkout(&mut state, "c1", MAIN, 'b', 10_500);
    checkout(&mut state, "c2", OTHER, 'c', 69_500);
    let outcome = reported(&mut state, "alice", 'b', 1_000, 60_000, 70_000);
    assert!(
        matches!(&outcome, Outcome::CheckoutRunObserved { run }
            if run.started_ms == 9_000 && run.finished_ms == 10_000),
        "{outcome:?}"
    );
    // Her own merge is not news to her; the other change still is.
    let alice = notices(&mut state, "alice", 70_100);
    assert_eq!(alice.len(), 1, "{alice:?}");
    assert_eq!(alice[0].to_branch.as_deref(), Some(OTHER));
    assert_eq!(alice[0].by_session, None);
    let bob = notices(&mut state, "bob", 70_100);
    assert_eq!(bob.len(), 2, "{bob:?}");
    assert_eq!(bob[0].by_session, None, "{bob:?}");
    assert_eq!(bob[1].by_session.as_deref(), Some("alice"), "{bob:?}");
}

#[test]
fn a_run_is_refused_for_an_unknown_session_or_an_invalid_state() {
    let mut state = ready();
    let outcome = merged(&mut state, "carol", 'b', 300, 1_000);
    assert!(
        matches!(&outcome, Outcome::Error { code, .. } if code == "unknown_session"),
        "{outcome:?}"
    );
    let outcome = run(
        &mut state,
        "bad-branch",
        Command::ObserveCheckoutRun {
            session_id: "alice".into(),
            command: None,
            to_branch: Some("main".into()),
            to_head: Some(oid('b')),
            ran_ms: 0,
            finished_ago_ms: 0,
            files_unchanged: false,
        },
        1_000,
    );
    assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    let outcome = run(
        &mut state,
        "long-command",
        Command::ObserveCheckoutRun {
            session_id: "alice".into(),
            command: Some("x".repeat(1_000)),
            to_branch: Some(MAIN.into()),
            to_head: Some(oid('b')),
            ran_ms: 0,
            finished_ago_ms: 0,
            files_unchanged: false,
        },
        1_000,
    );
    assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    assert!(state.checkout_runs.is_empty());
    // A duration past a shell call's lifetime is taken as that lifetime.
    let outcome = merged(&mut state, "alice", 'b', u64::MAX, 10_000_000_000);
    assert!(
        matches!(&outcome, Outcome::CheckoutRunObserved { run }
            if run.started_ms == 10_000_000_000 - langspan_core::MAX_CHECKOUT_RUN_MS),
        "{outcome:?}"
    );
    // A finish longer ago than the daemon's clock has run is taken as its
    // start: the run is placed back from the daemon's now, never past it.
    let outcome = reported(&mut state, "alice", 'b', 300, u64::MAX, 20_000);
    assert!(
        matches!(&outcome, Outcome::CheckoutRunObserved { run }
            if run.started_ms == 0 && run.finished_ms == 0),
        "{outcome:?}"
    );
}

/// The hook measures how long ago the command finished each time it sends
/// the report, so a retry of a request the daemon already recorded differs
/// only in that age: the same request, answered from its receipt rather
/// than refused as another command.
#[test]
fn a_retry_differing_only_in_the_finish_age_is_the_same_request() {
    let command = |to, ran, ago| Command::ObserveCheckoutRun {
        session_id: "alice".into(),
        command: Some("git merge origin/main".into()),
        to_branch: Some(MAIN.into()),
        to_head: Some(oid(to)),
        ran_ms: ran,
        finished_ago_ms: ago,
        files_unchanged: false,
    };
    let first = command('b', 300, 0);
    assert!(first.same_request(&command('b', 300, 5_000)));
    assert!(
        !first.same_request(&command('c', 300, 0)),
        "the head counts"
    );
    assert!(
        !first.same_request(&command('b', 301, 0)),
        "the duration counts"
    );
}
