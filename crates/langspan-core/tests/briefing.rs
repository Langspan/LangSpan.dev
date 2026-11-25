//! Which peers a briefing names, and when it is delivered again.
use langspan_core::{
    BRIEF_HORIZON_MS, Briefing, Command, Event, LivenessStatus, Outcome, ProcessIdentity, State,
    ToolResult, apply, decide,
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

fn ok(outcome: Outcome) -> Outcome {
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    outcome
}

/// Two worktrees of one repository, a third workspace in another repository.
fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (id, root, repository, branch) in [
        ("main", "/repos/app", "repo-app", "refs/heads/main"),
        ("tree", "/repos/app-tree", "repo-app", "refs/heads/feature"),
        ("other", "/repos/other", "repo-other", "refs/heads/main"),
    ] {
        ok(run(
            &mut state,
            &format!("w-{id}"),
            Command::WorkspaceRegister {
                workspace_id: id.into(),
                root: root.into(),
            },
            0,
        ));
        ok(run(
            &mut state,
            &format!("g-{id}"),
            Command::ObserveWorkspaceGit {
                workspace_id: id.into(),
                repository_id: repository.into(),
                branch_ref: Some(branch.into()),
            },
            0,
        ));
    }
    for (session, workspace) in [
        ("alice", "main"),
        ("bob", "main"),
        ("carol", "tree"),
        ("dave", "other"),
    ] {
        ok(run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: workspace.into(),
                parent: None,
                headless: false,
            },
            0,
        ));
    }
    state
}

fn edited(state: &mut State, session: &str, path: &str, now: u64) {
    ok(run(
        state,
        &format!("o-{session}-{now}-{}", path.len()),
        Command::Observe {
            session_id: session.into(),
            operation_id: format!("op-{session}-{now}"),
            source: "test".into(),
            tool: "Edit".into(),
            paths: vec![path.into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        now,
    ));
}

fn brief(state: &mut State, session: &str, id: &str, now: u64) -> Briefing {
    match ok(run(
        state,
        id,
        Command::Brief {
            session_id: session.into(),
            max_peers: 8,
            deliver: true,
            mid_turn: false,
        },
        now,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

fn names(briefing: &Briefing) -> Vec<&str> {
    briefing
        .peers
        .iter()
        .map(|peer| peer.session_id.as_str())
        .collect()
}

#[test]
fn a_briefing_names_only_related_sessions_with_current_work() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    edited(&mut state, "bob", "src/api.rs", 2_000);
    edited(&mut state, "carol", "src/api.rs", 3_000);
    edited(&mut state, "dave", "src/api.rs", 3_000);
    let briefing = brief(&mut state, "alice", "b1", 4_000);
    // dave works in another repository; alice never sees herself.
    assert_eq!(names(&briefing), ["carol", "bob"], "newest overlap first");
    let carol = &briefing.peers[0];
    assert!(!carol.same_workspace, "a worktree is a logical neighbour");
    assert_eq!(carol.branch_ref.as_deref(), Some("refs/heads/feature"));
    assert_eq!(carol.overlap, ["src/api.rs"]);
    assert_eq!(carol.recent[0].age_ms, 1_000);
    assert!(briefing.peers[1].same_workspace);
    assert!(briefing.changed);
}

#[test]
fn idle_registered_sessions_and_unrelated_repositories_are_left_out() {
    let mut state = ready();
    let briefing = brief(&mut state, "alice", "b1", 10);
    assert!(briefing.peers.is_empty(), "registration alone is not work");
    assert!(!briefing.changed, "nothing to deliver");
    edited(&mut state, "dave", "src/api.rs", 20);
    assert!(brief(&mut state, "alice", "b2", 30).peers.is_empty());
}

#[test]
fn an_unchanged_situation_is_not_delivered_twice_and_a_change_is() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 100);
    assert!(brief(&mut state, "alice", "b1", 200).changed);
    let again = brief(&mut state, "alice", "b2", 5_000);
    assert!(!again.changed, "only ages moved");
    assert_eq!(names(&again), ["bob"], "the content is still returned");
    edited(&mut state, "bob", "src/b.rs", 6_000);
    assert!(brief(&mut state, "alice", "b3", 7_000).changed);
    // Each session has its own delivery record.
    edited(&mut state, "alice", "src/a.rs", 7_500);
    assert!(brief(&mut state, "bob", "b4", 8_000).changed);
}

#[test]
fn stale_exited_and_ended_work_is_excluded() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 100);
    edited(&mut state, "carol", "src/a.rs", 100);
    // Beyond the horizon, bob's edit is history, not current work.
    let late = 100 + BRIEF_HORIZON_MS + 1;
    assert!(brief(&mut state, "alice", "b1", late).peers.is_empty());
    edited(&mut state, "bob", "src/a.rs", late);
    edited(&mut state, "carol", "src/a.rs", late);
    ok(run(
        &mut state,
        "live",
        Command::ObserveSessionLiveness {
            session_id: "bob".into(),
            process: ProcessIdentity {
                pid: 7,
                start_token: "boot-1".into(),
            },
            status: LivenessStatus::Exited,
        },
        late,
    ));
    assert_eq!(
        names(&brief(&mut state, "alice", "b2", late + 1)),
        ["carol"]
    );
    ok(run(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "carol".into(),
        },
        late + 2,
    ));
    // An ended session's recent edits are still what a peer must not overwrite.
    let after_end = brief(&mut state, "alice", "b3", late + 3);
    assert_eq!(names(&after_end), ["carol"]);
    assert!(after_end.peers[0].ended && after_end.changed);
    assert_eq!(after_end.peers[0].recent[0].path, "src/a.rs");
    let aged = brief(&mut state, "alice", "b3b", late + BRIEF_HORIZON_MS + 3);
    assert!(aged.peers.is_empty(), "and they age out like any other");
    // A restart makes every earlier session stale until it registers again.
    edited(&mut state, "alice", "src/a.rs", late + 4);
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    ok(run(
        &mut state,
        "s-carol-2",
        Command::SessionRegister {
            session_id: "carol".into(),
            workspace_id: "tree".into(),
            parent: None,
            headless: false,
        },
        0,
    ));
    assert!(brief(&mut state, "carol", "b4", 1).peers.is_empty());
}

#[test]
fn current_admissions_intents_and_prompt_summaries_are_reported() {
    let mut state = ready();
    ok(run(
        &mut state,
        "lease",
        Command::Admit {
            session_id: "bob".into(),
            paths: vec!["src/gate.rs".into()],
            ttl_ms: 1_000,
        },
        10,
    ));
    ok(run(
        &mut state,
        "prompt",
        Command::SessionPrompt {
            session_id: "bob".into(),
            summary: "rename the gate module".into(),
        },
        10,
    ));
    ok(run(
        &mut state,
        "intent",
        Command::IntentSet {
            session_id: "carol".into(),
            paths: vec!["docs/plan.md".into()],
            summary: "plan".into(),
        },
        10,
    ));
    let briefing = brief(&mut state, "alice", "b1", 20);
    assert_eq!(names(&briefing), ["bob", "carol"], "current edits first");
    assert_eq!(briefing.peers[0].editing, ["src/gate.rs"]);
    assert_eq!(
        briefing.peers[0].summary.as_deref(),
        Some("rename the gate module")
    );
    assert_eq!(briefing.peers[1].intent.as_deref(), Some("plan"));
    // The admission expires; the prompt keeps bob visible, without the path.
    let later = brief(&mut state, "alice", "b2", 2_000);
    assert!(later.peers[0].editing.is_empty() || later.peers[0].session_id != "bob");
    assert!(later.changed);
}

#[test]
fn a_briefing_is_bounded_and_counts_what_it_omits() {
    let mut state = ready();
    for index in 0..12 {
        let session = format!("peer{index:02}");
        ok(run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.clone(),
                workspace_id: "main".into(),
                parent: None,
                headless: false,
            },
            0,
        ));
        for file in 0..12 {
            edited(&mut state, &session, &format!("src/f{file}.rs"), 100 + file);
        }
    }
    let briefing = brief(&mut state, "alice", "b1", 500);
    assert_eq!(briefing.peers.len(), 8);
    assert_eq!(briefing.omitted_peers, 4);
    assert!(briefing.peers.iter().all(|peer| peer.recent.len() == 8));
    assert_eq!(
        briefing.peers[0].recent[0].path, "src/f11.rs",
        "newest first"
    );
}

#[test]
fn invalid_briefing_inputs_are_rejected_without_events() {
    let mut state = ready();
    for command in [
        Command::Brief {
            session_id: "alice".into(),
            max_peers: 0,
            deliver: true,
            mid_turn: false,
        },
        Command::Brief {
            session_id: "nobody".into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
        },
        Command::SessionPrompt {
            session_id: "alice".into(),
            summary: "two\nlines".into(),
        },
        Command::SessionPrompt {
            session_id: "alice".into(),
            summary: "x".repeat(513),
        },
        Command::ObserveWorkspaceGit {
            workspace_id: "main".into(),
            repository_id: "repo".into(),
            branch_ref: Some("HEAD".into()),
        },
    ] {
        let decision = decide(&state, &command, "bad", 1);
        assert!(
            matches!(decision.outcome, Outcome::Error { .. }),
            "{command:?}"
        );
        assert!(decision.events.is_empty());
    }
    assert!(matches!(
        run(
            &mut state,
            "g",
            Command::ObserveWorkspaceGit {
                workspace_id: "main".into(),
                repository_id: "repo-app".into(),
                branch_ref: Some("refs/heads/main".into()),
            },
            1
        ),
        Outcome::WorkspaceGit { .. }
    ));
}

#[test]
fn the_largest_briefing_is_still_an_event_the_core_accepts_and_fits_one_frame() {
    let mut state = ready();
    let long = |n: usize| format!("{}/{n:03}", "d".repeat(500));
    for index in 0..16 {
        let session = format!("big{index:02}");
        ok(run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.clone(),
                workspace_id: "main".into(),
                parent: None,
                headless: false,
            },
            0,
        ));
        for file in 0..16 {
            edited(&mut state, &session, &long(file), 100 + file as u64);
        }
        ok(run(
            &mut state,
            &format!("i-{session}"),
            Command::IntentSet {
                session_id: session.clone(),
                paths: vec![long(0)],
                summary: "s".repeat(2048),
            },
            200,
        ));
    }
    // `run` applies the decision's events: a rejected one would panic there.
    let outcome = ok(run(
        &mut state,
        "big",
        Command::Brief {
            session_id: "alice".into(),
            max_peers: 16,
            deliver: true,
            mid_turn: false,
        },
        300,
    ));
    let bytes = serde_json::to_vec(&outcome).unwrap().len();
    assert!(bytes < 60 * 1024, "{bytes}");
    let Outcome::Briefing { briefing } = outcome else {
        unreachable!()
    };
    assert!(!briefing.peers.is_empty());
    assert_eq!(briefing.peers.len() + briefing.omitted_peers as usize, 16);
    assert!(state.activity["alice"].briefed.as_ref().unwrap().len() <= 64);
}

#[test]
fn a_look_from_outside_is_not_a_delivery_and_not_activity() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 100);
    let look = Command::Brief {
        session_id: "alice".into(),
        max_peers: 8,
        deliver: false,
        mid_turn: false,
    };
    let decision = decide(&state, &look, "look", 200);
    assert!(decision.events.is_empty());
    assert!(matches!(decision.outcome, Outcome::Briefing { ref briefing } if briefing.changed));
    assert!(brief(&mut state, "alice", "b1", 300).changed, "still due");
}

#[test]
fn ending_a_session_always_labels_it_and_drops_its_task_line() {
    let mut state = ready();
    ok(run(
        &mut state,
        "p",
        Command::SessionPrompt {
            session_id: "bob".into(),
            summary: "private once ended".into(),
        },
        10,
    ));
    edited(&mut state, "bob", "src/a.rs", 20);
    ok(run(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "bob".into(),
        },
        30,
    ));
    let briefing = brief(&mut state, "alice", "b1", 40);
    assert!(briefing.peers[0].ended);
    assert_eq!(briefing.peers[0].summary, None);
    // A session with nothing recorded is labelled too, so later edits by a
    // process that outlived its host session are not shown as a live session's.
    ok(run(
        &mut state,
        "end2",
        Command::SessionEnd {
            session_id: "carol".into(),
        },
        50,
    ));
    assert!(state.activity["carol"].ended);
}

/// Eight hours: a night with the lid closed. The daemon's clock counts it
/// (see `daemon::clock`), so the core sees it as a gap in `now_ms`.
const NIGHT_MS: u64 = 8 * 60 * 60 * 1000;

#[test]
fn a_night_of_sleep_in_now_ms_ages_peers_out_and_expires_leases() {
    let mut state = ready();
    edited(&mut state, "bob", "src/a.rs", 10);
    ok(run(
        &mut state,
        "admit",
        Command::Admit {
            session_id: "bob".into(),
            paths: vec!["src/gate.rs".into()],
            ttl_ms: 60_000,
        },
        10,
    ));
    let before = brief(&mut state, "alice", "b1", 20);
    assert_eq!(names(&before), ["bob"]);
    assert_eq!(before.peers[0].editing, ["src/gate.rs"]);

    let after = brief(&mut state, "alice", "b2", 10 + NIGHT_MS);
    assert!(after.peers.is_empty(), "{after:?}");
    match ok(run(
        &mut state,
        "admit-after",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/gate.rs".into()],
            ttl_ms: 60_000,
        },
        10 + NIGHT_MS,
    )) {
        Outcome::Admitted { .. } => {}
        other => panic!("bob's admission expired overnight: {other:?}"),
    }
}

fn register(state: &mut State, session: &str, parent: Option<&str>, headless: bool) {
    ok(run(
        state,
        &format!("s-{session}"),
        Command::SessionRegister {
            session_id: session.into(),
            workspace_id: "main".into(),
            parent: parent.map(str::to_owned),
            headless,
        },
        0,
    ));
}

fn prompted(state: &mut State, session: &str, summary: &str, now: u64) {
    ok(run(
        state,
        &format!("p-{session}-{now}"),
        Command::SessionPrompt {
            session_id: session.into(),
            summary: summary.into(),
        },
        now,
    ));
}

#[test]
fn a_subagent_is_not_briefed_about_its_parent_but_still_about_siblings_and_others() {
    use langspan_core::PeerRelation;
    let mut state = ready();
    register(&mut state, "alice.a1", Some("alice"), false);
    register(&mut state, "alice.b2", Some("alice"), false);
    prompted(&mut state, "alice", "Step 1: run this command", 5);
    for session in ["alice", "alice.a1", "alice.b2", "bob"] {
        edited(&mut state, session, &format!("src/{session}.rs"), 10);
    }
    // The parent edited and holds files of the subagent's own work: still
    // not news to the subagent.
    edited(&mut state, "alice", "src/alice.a1.rs", 11);
    ok(run(
        &mut state,
        "parent-holds",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/alice.a1.rs".into()],
            ttl_ms: 60_000,
        },
        12,
    ));
    let child = brief(&mut state, "alice.a1", "b1", 20);
    // The parent started this subagent and knows its own task and files.
    assert_eq!(names(&child), ["alice.b2", "bob"], "{child:?}");
    let sibling = &child.peers[0];
    assert_eq!(sibling.relation, Some(PeerRelation::Sibling));
    assert_eq!(sibling.recent[0].path, "src/alice.b2.rs");
    assert_eq!(child.peers[1].relation, None);
    // Admission still applies between them: the parent's held file is
    // refused to the subagent as to anyone.
    match run(
        &mut state,
        "child-admit",
        Command::Admit {
            session_id: "alice.a1".into(),
            paths: vec!["src/alice.a1.rs".into()],
            ttl_ms: 60_000,
        },
        21,
    ) {
        Outcome::Conflict { .. } => {}
        other => panic!("the parent's held file was admitted: {other:?}"),
    }
}

#[test]
fn a_parent_sees_its_subagents_files_but_not_their_task_lines() {
    use langspan_core::PeerRelation;
    let mut state = ready();
    register(&mut state, "alice.a1", Some("alice"), false);
    register(&mut state, "alice.b2", Some("alice"), false);
    prompted(&mut state, "alice.a1", "the parent's request, relayed", 5);
    // A subagent with a task and no edits has nothing to tell its parent.
    prompted(&mut state, "alice.b2", "another relayed request", 5);
    ok(run(
        &mut state,
        "child-intent",
        Command::IntentSet {
            session_id: "alice.a1".into(),
            paths: vec!["src/plan.rs".into()],
            summary: "what the parent asked for".into(),
        },
        6,
    ));
    edited(&mut state, "alice.a1", "src/child.rs", 10);
    let parent = brief(&mut state, "alice", "b1", 20);
    assert_eq!(names(&parent), ["alice.a1"]);
    let child = &parent.peers[0];
    assert_eq!(child.relation, Some(PeerRelation::Subagent));
    assert_eq!(child.recent[0].path, "src/child.rs");
    assert_eq!(child.summary, None, "its task is the parent's own request");
    assert_eq!(child.intent, None, "its task is the parent's own request");
}

#[test]
fn a_subagents_edit_of_a_file_its_parent_read_is_still_a_stale_read() {
    use langspan_core::{FileState, ObservedFile, SourceOrigin};
    let mut state = ready();
    register(&mut state, "alice.a1", Some("alice"), false);
    ok(run(
        &mut state,
        "parent-read",
        Command::ObserveSource {
            session_id: "alice".into(),
            files: vec![ObservedFile {
                path: "src/api.rs".into(),
                state: FileState::Present {
                    digest: "1".repeat(64),
                    kind: Default::default(),
                },
            }],
            origin: SourceOrigin::Read,
        },
        5,
    ));
    edited(&mut state, "alice.a1", "src/api.rs", 10);
    let parent = brief(&mut state, "alice", "b1", 20);
    assert_eq!(parent.stale_reads.len(), 1, "{parent:?}");
    assert_eq!(parent.stale_reads[0].path, "src/api.rs");
    assert_eq!(parent.stale_reads[0].session_id, "alice.a1");
}

/// The coordinator log of a real Claude Code 2.1.330 run (recorded for
/// PR #37, `docs/evidence/turn-events/langspan-events.json`), replayed up to
/// the subagent's first completed tool call, where the recording delivered
/// it a briefing (seq 23) naming its parent and the parent's task line.
/// Event kinds this build does not know are skipped; none of them changes
/// what a briefing contains.
#[test]
fn a_recorded_subagents_first_briefing_names_no_parent() {
    let log: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/evidence/turn-events/langspan-events.json"
    ))
    .unwrap();
    let mut state = State::default();
    let mut subagent = None;
    for entry in log.as_array().unwrap() {
        if entry["seq"].as_u64().unwrap() > 22 {
            break;
        }
        let kind = entry["event"]["type"].as_str().unwrap();
        if matches!(kind, "tool_batch_observed" | "session_stopped") {
            continue;
        }
        let event: Event = serde_json::from_value(entry["event"].clone()).unwrap();
        if let Event::SessionRegistered { session } = &event
            && session.parent.is_some()
        {
            subagent = Some(session.session_id.clone());
        }
        apply(&mut state, &event).unwrap();
    }
    let subagent = subagent.expect("the recording registers a subagent");
    // The time of the subagent's first completed Read in the recording.
    let briefing = brief(&mut state, &subagent, "recorded", 1_934_203_657);
    assert!(briefing.peers.is_empty(), "{briefing:?}");
    assert!(!briefing.changed, "nothing to deliver to the subagent");
}

#[test]
fn a_session_cannot_be_its_own_parent() {
    let state = ready();
    let decision = decide(
        &state,
        &Command::SessionRegister {
            session_id: "alice".into(),
            workspace_id: "main".into(),
            parent: Some("alice".into()),
            headless: false,
        },
        "self",
        0,
    );
    assert!(
        matches!(&decision.outcome, Outcome::Error { code, .. } if code == "invalid_session"),
        "{:?}",
        decision.outcome
    );
}

#[test]
fn a_parent_relation_in_a_briefing_stored_by_an_older_version_still_reads() {
    let relation: langspan_core::PeerRelation =
        serde_json::from_str("\"parent\"").expect("an older stored relation reads");
    assert_eq!(relation, langspan_core::PeerRelation::Parent);
}
