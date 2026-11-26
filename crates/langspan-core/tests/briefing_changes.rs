//! Briefings of changes only (issue #222): what a session was told is left
//! out, what repeats for safety, the bound on what is remembered, records
//! stored before it, and the context each turn added.
use langspan_core::{
    Briefing, Command, ContextUse, Event, InterfaceChange, MAX_TOLD, Outcome, PresenceKind, State,
    ToolResult, apply, briefing_keys, decide,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let decision = decide(state, &command, id, now);
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

fn ok(outcome: Outcome) -> Outcome {
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    outcome
}

/// One checkout with three sessions in it.
fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    ok(run(
        &mut state,
        "w",
        Command::WorkspaceRegister {
            workspace_id: "main".into(),
            root: "/repos/app".into(),
        },
        0,
    ));
    for session in ["alice", "bob", "carol"] {
        ok(run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "main".into(),
                parent: None,
                headless: false,
            },
            0,
        ));
    }
    state
}

fn edited(state: &mut State, session: &str, path: &str, now: u64) {
    let id = path.replace('/', "-");
    ok(run(
        state,
        &format!("o-{session}-{now}-{id}"),
        Command::Observe {
            session_id: session.into(),
            operation_id: format!("op-{session}-{now}-{id}"),
            source: "test".into(),
            tool: "Edit".into(),
            paths: vec![path.into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        now,
    ));
}

fn brief_as(
    state: &mut State,
    session: &str,
    now: u64,
    changes_only: bool,
    mid_turn: bool,
) -> Briefing {
    match ok(run(
        state,
        &format!("b-{session}-{now}-{changes_only}-{mid_turn}"),
        Command::Brief {
            session_id: session.into(),
            max_peers: 8,
            deliver: true,
            mid_turn,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
            changes_only,
        },
        now,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

/// A prompt's briefing: changes only.
fn prompt(state: &mut State, session: &str, now: u64) -> Briefing {
    brief_as(state, session, now, true, false)
}

fn names(briefing: &Briefing) -> Vec<&str> {
    briefing
        .peers
        .iter()
        .map(|peer| peer.session_id.as_str())
        .collect()
}

fn signature_change(name: &str) -> InterfaceChange {
    InterfaceChange {
        name: name.into(),
        kind: "function".into(),
        change: "signature_changed".into(),
        before: Some(format!("fn {name}(x: u8)")),
        after: Some(format!("fn {name}(x: u8, y: u8)")),
        after_name: None,
        before_name: None,
        evidence: None,
        before_path: None,
        private: false,
        before_arity: None,
        after_arity: None,
        before_count: None,
        after_count: None,
    }
}

#[test]
fn a_full_view_then_only_the_peer_that_changed_and_only_its_new_file() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 1_000);
    edited(&mut state, "carol", "src/b.rs", 1_000);
    // The first briefing of a session is the full view, changes only or not.
    let first = prompt(&mut state, "alice", 2_000);
    assert_eq!(names(&first), ["bob", "carol"]);
    assert_eq!(first.unchanged, 0);
    assert!(first.changed);
    // Nothing new: nothing to say.
    let quiet = prompt(&mut state, "alice", 3_000);
    assert!(!quiet.changed, "{quiet:?}");

    edited(&mut state, "carol", "src/c.rs", 4_000);
    let next = prompt(&mut state, "alice", 5_000);
    assert!(next.changed);
    assert_eq!(names(&next), ["carol"]);
    let paths: Vec<&str> = next.peers[0]
        .recent
        .iter()
        .map(|recent| recent.path.as_str())
        .collect();
    assert_eq!(paths, ["src/c.rs"], "only the new file");
    // Told before: Bob's line and his file, Carol's line and her first file.
    assert_eq!(next.unchanged, 4, "{next:?}");
}

#[test]
fn an_item_that_goes_away_and_comes_back_is_told_again() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 1_000);
    assert_eq!(names(&prompt(&mut state, "alice", 2_000)), ["bob"]);
    // Bob ends: his line changes, and is told.
    ok(run(
        &mut state,
        "end-bob",
        Command::SessionEnd {
            session_id: "bob".into(),
            trail: None,
        },
        3_000,
    ));
    let ended = prompt(&mut state, "alice", 4_000);
    assert!(ended.peers[0].ended, "{ended:?}");
    // He comes back and edits the same file again: his running line was
    // forgotten when it changed, so it is news again.
    edited(&mut state, "bob", "src/a.rs", 5_000);
    let back = prompt(&mut state, "alice", 6_000);
    assert_eq!(names(&back), ["bob"], "{back:?}");
    assert!(!back.peers[0].ended);
}

#[test]
fn an_interface_change_repeats_only_after_the_session_edits_an_affected_file_again() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    edited(&mut state, "bob", "src/store.rs", 1_000);
    ok(run(
        &mut state,
        "interface-1",
        Command::ObserveInterface {
            workspace_id: "main".into(),
            path: "src/store.rs".into(),
            changes: vec![signature_change("save")],
            dependents: vec!["src/api.rs".into()],
        },
        1_001,
    ));
    let first = prompt(&mut state, "alice", 2_000);
    assert_eq!(first.interface_changes.len(), 1);
    assert!(!prompt(&mut state, "alice", 3_000).changed);
    // Alice edits a file that uses the changed name: the alert repeats.
    edited(&mut state, "alice", "src/api.rs", 4_000);
    let again = prompt(&mut state, "alice", 5_000);
    assert_eq!(again.interface_changes.len(), 1, "{again:?}");
    assert_eq!(again.interface_changes[0].affected, ["src/api.rs"]);
    // Once per act.
    assert!(!prompt(&mut state, "alice", 6_000).changed);
}

#[test]
fn where_a_peer_looks_waits_untold_mid_turn_and_is_told_at_the_next_prompt() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 1_000);
    assert_eq!(names(&prompt(&mut state, "alice", 2_000)), ["bob"]);
    ok(run(
        &mut state,
        "look-bob",
        Command::ObservePresence {
            session_id: "bob".into(),
            kind: PresenceKind::Search,
            paths: vec!["src/z.rs".into()],
        },
        3_000,
    ));
    // Presence alone never interrupts a turn, and is not used up by it.
    let mid = brief_as(&mut state, "alice", 4_000, true, true);
    assert!(!mid.changed, "{mid:?}");
    let next = prompt(&mut state, "alice", 5_000);
    assert!(next.changed);
    let looking: Vec<&str> = next.peers[0]
        .looking
        .iter()
        .map(|looked| looked.path.as_str())
        .collect();
    assert_eq!(looking, ["src/z.rs"]);
}

#[test]
fn a_full_view_on_request_tells_everything_and_a_look_from_outside_tells_nothing() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 1_000);
    // A look from outside (the CLI's `brief`) is not a delivery.
    let look = match ok(run(
        &mut state,
        "look",
        Command::Brief {
            session_id: "alice".into(),
            max_peers: 8,
            deliver: false,
            mid_turn: false,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
            changes_only: false,
        },
        2_000,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    };
    assert_eq!(names(&look), ["bob"]);
    assert_eq!(names(&prompt(&mut state, "alice", 3_000)), ["bob"]);
    // The full view, delivered, names everything again and leaves out none.
    let full = brief_as(&mut state, "alice", 4_000, false, false);
    assert_eq!(names(&full), ["bob"]);
    assert_eq!(full.unchanged, 0);
    assert!(!prompt(&mut state, "alice", 5_000).changed);
}

#[test]
fn a_delivery_stored_before_told_keys_reads_and_the_next_briefing_is_the_full_view() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 1_000);
    let older: Event = serde_json::from_value(serde_json::json!({
        "type": "briefing_delivered",
        "session_id": "alice",
        "signature": "0123456789abcdef",
    }))
    .unwrap();
    apply(&mut state, &older).unwrap();
    assert!(state.activity["alice"].told.is_empty());
    let next = prompt(&mut state, "alice", 2_000);
    assert_eq!(names(&next), ["bob"]);
    assert_eq!(next.unchanged, 0);
}

#[test]
fn a_delivery_remembers_at_most_max_told_keys() {
    let mut state = ready();
    let delivered = |told: usize| Event::BriefingDelivered {
        session_id: "alice".into(),
        signature: "0123456789abcdef".into(),
        told: (0..told as u64).collect(),
        told_at_ms: Some(1),
    };
    apply(&mut state, &delivered(MAX_TOLD)).unwrap();
    assert_eq!(state.activity["alice"].told.len(), MAX_TOLD);
    let refused = apply(&mut state, &delivered(MAX_TOLD + 1)).unwrap_err();
    assert_eq!(refused.code, "invalid_event");
    // A briefing never asks to remember more: its keys are cut to the bound.
    for index in 0..40 {
        edited(
            &mut state,
            "bob",
            &format!("src/f{index}.rs"),
            1_000 + index,
        );
        edited(
            &mut state,
            "carol",
            &format!("src/g{index}.rs"),
            1_000 + index,
        );
    }
    prompt(&mut state, "alice", 2_000);
    assert!(state.activity["alice"].told.len() <= MAX_TOLD);
}

#[test]
fn context_records_count_turns_bytes_and_the_largest_turn() {
    let mut state = ready();
    let record = |state: &mut State, id: &str, bytes: u64, turn: bool| {
        run(
            state,
            id,
            Command::RecordContext {
                session_id: "alice".into(),
                bytes,
                turn,
            },
            1_000,
        )
    };
    ok(record(&mut state, "c1", 700, true));
    ok(record(&mut state, "c2", 0, true));
    ok(record(&mut state, "c3", 50, false));
    let Outcome::ContextRecorded { context, .. } = ok(record(&mut state, "c4", 30, false)) else {
        panic!("not a context record");
    };
    assert_eq!(
        context,
        ContextUse {
            turns: 2,
            bytes: 780,
            largest_turn_bytes: 700,
            turn_bytes: 80,
        }
    );
    assert_eq!(state.activity["alice"].context, context);
    assert_eq!(context.bytes_per_turn(), 390);
    assert_eq!(ContextUse::estimated_tokens(390), 98);
    // Nothing added outside a turn's start is not a record; nor is more
    // than any hook could add.
    for (id, bytes, turn) in [("c5", 0, false), ("c6", 1024 * 1024 + 1, true)] {
        assert!(matches!(
            record(&mut state, id, bytes, turn),
            Outcome::Error { .. }
        ));
    }
}

/// Forget, for `session`, what `shown` holds of `delivered` and the hook
/// did not place: everything in `delivered` but not in `placed`.
fn forget_unplaced(
    state: &mut State,
    session: &str,
    delivered: &Briefing,
    placed: &Briefing,
    now: u64,
) -> Outcome {
    let placed = briefing_keys(placed);
    let keys: Vec<u64> = briefing_keys(delivered)
        .into_iter()
        .filter(|key| !placed.contains(key))
        .collect();
    run(
        state,
        &format!("forget-{session}-{now}"),
        Command::ForgetTold {
            session_id: session.into(),
            keys,
        },
        now,
    )
}

/// Review comment 4711375989: a briefing's hook may leave out a line that
/// does not fit its budget. What it did not place is forgotten, so the next
/// briefing of changes only tells it, though nothing about it changed; what
/// it placed stays told.
#[test]
fn an_item_the_hook_could_not_place_is_news_again_at_the_next_prompt() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 1_000);
    edited(&mut state, "carol", "src/b.rs", 1_000);
    // The full view at a start: the hook placed Bob's line, not Carol's.
    let first = brief_as(&mut state, "alice", 2_000, false, false);
    assert_eq!(names(&first), ["bob", "carol"]);
    let mut placed = first.clone();
    placed.peers.retain(|peer| peer.session_id == "bob");
    let Outcome::ToldForgotten { forgotten, .. } =
        ok(forget_unplaced(&mut state, "alice", &first, &placed, 2_000))
    else {
        panic!("not a forget");
    };
    assert_eq!(forgotten, 2, "Carol's line and her file");
    let next = prompt(&mut state, "alice", 3_000);
    assert!(next.changed, "{next:?}");
    assert_eq!(names(&next), ["carol"]);
    assert_eq!(next.unchanged, 2, "Bob's line and his file stay told");
    assert!(!prompt(&mut state, "alice", 4_000).changed);

    // Within a briefing of changes only: Carol's new file did not fit.
    edited(&mut state, "carol", "src/c.rs", 5_000);
    let delta = prompt(&mut state, "alice", 6_000);
    assert_eq!(names(&delta), ["carol"]);
    let mut placed = delta.clone();
    placed.peers[0].recent.clear();
    ok(forget_unplaced(&mut state, "alice", &delta, &placed, 6_000));
    let again = prompt(&mut state, "alice", 7_000);
    let paths: Vec<&str> = again.peers[0]
        .recent
        .iter()
        .map(|recent| recent.path.as_str())
        .collect();
    assert_eq!(paths, ["src/c.rs"], "{again:?}");
}

/// The same for every kind of keyed item: a notice, an interface change, a
/// remembered note and where a peer looks are each told again when their
/// line was not placed.
#[test]
fn every_kind_of_item_the_hook_could_not_place_is_told_again() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    edited(&mut state, "bob", "src/store.rs", 1_000);
    ok(run(
        &mut state,
        "interface-1",
        Command::ObserveInterface {
            workspace_id: "main".into(),
            path: "src/store.rs".into(),
            changes: vec![signature_change("save")],
            dependents: vec!["src/api.rs".into()],
        },
        1_001,
    ));
    ok(run(
        &mut state,
        "note-1",
        Command::Remember {
            workspace_id: "main".into(),
            author: Some("bob".into()),
            text: "run the store tests with --features sqlite".into(),
            paths: vec!["src/api.rs".into()],
        },
        1_002,
    ));
    ok(run(
        &mut state,
        "look-bob",
        Command::ObservePresence {
            session_id: "bob".into(),
            kind: PresenceKind::Search,
            paths: vec!["src/z.rs".into()],
        },
        1_003,
    ));
    let first = brief_as(&mut state, "alice", 2_000, false, false);
    assert_eq!(first.interface_changes.len(), 1, "{first:?}");
    assert_eq!(first.notes.len(), 1, "{first:?}");
    assert_eq!(first.peers[0].looking.len(), 1, "{first:?}");
    // Only the peer's own line was placed.
    let mut placed = first.clone();
    placed.interface_changes.clear();
    placed.notes.clear();
    placed.peers[0].looking.clear();
    ok(forget_unplaced(&mut state, "alice", &first, &placed, 2_000));
    let next = prompt(&mut state, "alice", 3_000);
    assert_eq!(next.interface_changes.len(), 1, "{next:?}");
    assert_eq!(next.notes.len(), 1, "{next:?}");
    assert_eq!(next.peers.len(), 1, "{next:?}");
    assert_eq!(next.peers[0].looking.len(), 1, "{next:?}");
    assert!(next.peers[0].recent.is_empty(), "Bob's file stays told");
}

/// Forgetting is bounded, for a current session only, and a retry, or a
/// key the session was never told, forgets nothing and records nothing.
#[test]
fn forgetting_is_bounded_idempotent_and_for_a_current_session() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 1_000);
    let first = prompt(&mut state, "alice", 2_000);
    let keys = briefing_keys(&first);
    let forget = |keys: Vec<u64>, session: &str| Command::ForgetTold {
        session_id: session.into(),
        keys,
    };
    let decision = decide(&state, &forget(vec![u64::MAX], "alice"), "f0", 2_000);
    assert!(decision.events.is_empty(), "never told: nothing recorded");
    assert!(matches!(
        decision.outcome,
        Outcome::ToldForgotten { forgotten: 0, .. }
    ));
    ok(run(&mut state, "f1", forget(keys.clone(), "alice"), 2_000));
    assert!(state.activity["alice"].told.is_empty());
    let retry = decide(&state, &forget(keys, "alice"), "f2", 2_000);
    assert!(retry.events.is_empty(), "a retry forgets nothing");
    let too_many = forget((0..=MAX_TOLD as u64).collect(), "alice");
    assert!(matches!(
        run(&mut state, "f3", too_many, 2_000),
        Outcome::Error { .. }
    ));
    assert!(matches!(
        run(&mut state, "f4", forget(vec![1], "nobody"), 2_000),
        Outcome::Error { .. }
    ));
    // The event itself is bounded as the delivery is.
    let refused = apply(
        &mut state,
        &Event::ToldForgotten {
            session_id: "alice".into(),
            keys: (0..=MAX_TOLD as u64).collect(),
        },
    )
    .unwrap_err();
    assert_eq!(refused.code, "invalid_event");
}

/// Review follow-up #270: a newer notice for the same file is news when it
/// differs only in a removed signature or in how many definitions are left.
#[test]
fn a_change_differing_only_in_a_removed_signature_or_its_counts_is_told() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    edited(&mut state, "bob", "src/store.rs", 1_000);
    let change = |change: &str, before: &str, counts: Option<(u32, u32)>| InterfaceChange {
        change: change.into(),
        before: Some(before.into()),
        after: None,
        before_count: counts.map(|(before, _)| before),
        after_count: counts.map(|(_, after)| after),
        ..signature_change("new")
    };
    for (n, next) in [
        change("definition_removed", "fn new() -> Self", Some((3, 2))),
        change("definition_removed", "fn new() -> Self", Some((2, 1))),
        change("removed", "fn new() -> Self", None),
        change("removed", "fn new(x: u8) -> Self", None),
    ]
    .into_iter()
    .enumerate()
    {
        let now = 10_000 * (n as u64 + 1);
        ok(run(
            &mut state,
            &format!("n{n}"),
            Command::ObserveInterface {
                workspace_id: "main".into(),
                path: "src/store.rs".into(),
                changes: vec![next],
                dependents: vec!["src/api.rs".into()],
            },
            now,
        ));
        let briefing = prompt(&mut state, "alice", now + 1);
        assert_eq!(
            briefing.interface_changes.len(),
            1,
            "notice {n}: {briefing:?}"
        );
    }
}

/// Review follow-up #269: a briefing of changes only names the IDs of the
/// interface notices it keeps, in their order, and no others.
#[test]
fn a_briefing_of_changes_only_names_only_the_notices_it_keeps() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    edited(&mut state, "bob", "src/store.rs", 1_000);
    edited(&mut state, "bob", "src/queue.rs", 1_000);
    let observe = |state: &mut State, id: &str, path: &str, dependent: &str, now: u64| {
        ok(run(
            state,
            id,
            Command::ObserveInterface {
                workspace_id: "main".into(),
                path: path.into(),
                changes: vec![signature_change("save")],
                dependents: vec![dependent.into()],
            },
            now,
        ));
    };
    observe(&mut state, "n1", "src/store.rs", "src/cli.rs", 1_001);
    observe(&mut state, "n2", "src/queue.rs", "src/api.rs", 1_002);
    assert_eq!(
        prompt(&mut state, "alice", 2_000).interface_notice_ids,
        ["n2"]
    );
    // Alice starts on a file the older change touches: only it is news.
    edited(&mut state, "alice", "src/cli.rs", 3_000);
    let next = prompt(&mut state, "alice", 4_000);
    assert_eq!(next.interface_changes.len(), 1, "{next:?}");
    assert_eq!(next.interface_changes[0].path, "src/store.rs");
    assert_eq!(next.interface_notice_ids, ["n1"]);
}
