//! Which peers a briefing names, and when it is delivered again.
use langspan_core::{
    BRIEF_HORIZON_MS, Briefing, Command, Event, InterfaceChange, LivenessStatus, Outcome,
    PresenceKind, ProcessIdentity, RECENT_WINDOW_MS, RecentChanges, SessionActivity, State,
    ToolIdentity, ToolResult, apply, decide,
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
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
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

/// The Git observer scans a workspace after its first session registers, on
/// its own thread. A peer in this session's own checkout shares its branch,
/// so learning that branch after a briefing is not news; a peer in another
/// worktree switching branch still is.
#[test]
fn a_branch_learned_after_delivery_is_news_only_from_another_worktree() {
    let mut state = ready();
    ok(run(
        &mut state,
        "w-late",
        Command::WorkspaceRegister {
            workspace_id: "late".into(),
            root: "/repos/late".into(),
        },
        0,
    ));
    for session in ["erin", "frank"] {
        ok(run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "late".into(),
                parent: None,
                headless: false,
            },
            0,
        ));
    }
    edited(&mut state, "frank", "src/a.rs", 100);
    let first = brief(&mut state, "erin", "b1", 200);
    assert!(first.changed);
    assert_eq!(first.peers[0].branch_ref, None, "not scanned yet");
    ok(run(
        &mut state,
        "g-late",
        Command::ObserveWorkspaceGit {
            workspace_id: "late".into(),
            repository_id: "repo-late".into(),
            branch_ref: Some("refs/heads/main".into()),
        },
        300,
    ));
    let again = brief(&mut state, "erin", "b2", 400);
    assert!(!again.changed, "the checkout's own branch is not news");
    assert_eq!(
        again.peers[0].branch_ref.as_deref(),
        Some("refs/heads/main"),
        "it is still shown"
    );

    edited(&mut state, "carol", "src/api.rs", 500);
    assert!(brief(&mut state, "alice", "b3", 600).changed);
    ok(run(
        &mut state,
        "g-tree-2",
        Command::ObserveWorkspaceGit {
            workspace_id: "tree".into(),
            repository_id: "repo-app".into(),
            branch_ref: Some("refs/heads/other".into()),
        },
        700,
    ));
    assert!(
        brief(&mut state, "alice", "b4", 800).changed,
        "another worktree's branch switch is news"
    );
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
            line_digests: Vec::new(),
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
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
        },
        Command::Brief {
            session_id: "nobody".into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
        },
        Command::SessionPrompt {
            session_id: "alice".into(),
            summary: "two\nlines".into(),
            line_digests: Vec::new(),
        },
        Command::SessionPrompt {
            session_id: "alice".into(),
            summary: "x".repeat(513),
            line_digests: Vec::new(),
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
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
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
        left: Vec::new(),
        recent: false,
        recent_window_ms: None,
        mcp: false,
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
            line_digests: Vec::new(),
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
            line_digests: Vec::new(),
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

// Prompt targets (roadmap C8, ADR 0023): the definition paths a prompt's
// code briefing found, compared with other sessions' targets and work.

fn targets(state: &mut State, session: &str, paths: &[&str], now: u64) {
    match ok(run(
        state,
        &format!("t-{session}-{now}"),
        Command::SetPromptTargets {
            session_id: session.into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
        },
        now,
    )) {
        Outcome::PromptTargets {
            session_id,
            paths: recorded,
        } => {
            assert_eq!(session_id, session);
            assert_eq!(recorded, paths);
        }
        other => panic!("{other:?}"),
    }
}

fn brief_mid_turn(state: &mut State, session: &str, id: &str, now: u64) -> Briefing {
    match ok(run(
        state,
        id,
        Command::Brief {
            session_id: session.into(),
            max_peers: 8,
            deliver: true,
            mid_turn: true,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
        },
        now,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

fn peer<'a>(briefing: &'a Briefing, session: &str) -> &'a langspan_core::PeerWork {
    briefing
        .peers
        .iter()
        .find(|peer| peer.session_id == session)
        .unwrap_or_else(|| panic!("{session} not in {briefing:?}"))
}

#[test]
fn two_sessions_whose_tasks_point_at_one_file_are_each_told_before_either_writes() {
    let mut state = ready();
    prompted(
        &mut state,
        "alice",
        "make parse_header accept folded lines",
        1_000,
    );
    targets(&mut state, "alice", &["src/http.rs"], 1_000);
    // Alice is first: nobody else's task points anywhere yet.
    assert!(brief(&mut state, "alice", "a1", 1_000).peers.is_empty());

    prompted(
        &mut state,
        "bob",
        "fix the 400 from parse_header on empty values",
        61_000,
    );
    targets(&mut state, "bob", &["src/http.rs"], 61_000);
    // Bob is told in his prompt's briefing.
    let bobs = brief(&mut state, "bob", "b1", 61_000);
    assert!(bobs.changed);
    let alice = peer(&bobs, "alice");
    assert_eq!(alice.target_overlap, ["src/http.rs"]);
    assert!(alice.overlap.is_empty(), "nobody has edited anything");
    assert!(alice.editing.is_empty() && alice.recent.is_empty());

    // Alice is told at her next delivery point, once.
    let alices = brief_mid_turn(&mut state, "alice", "a2", 62_000);
    assert!(alices.changed);
    assert_eq!(peer(&alices, "bob").target_overlap, ["src/http.rs"]);
    assert!(!brief_mid_turn(&mut state, "alice", "a3", 63_000).changed);
    // Naming the same function again renews the targets without a repeat.
    targets(&mut state, "bob", &["src/http.rs"], 64_000);
    assert!(!brief_mid_turn(&mut state, "alice", "a4", 65_000).changed);
    // No write is held because of it: targets are not admissions.
    assert!(matches!(
        ok(run(
            &mut state,
            "admit",
            Command::Admit {
                session_id: "alice".into(),
                paths: vec!["src/http.rs".into()],
                ttl_ms: 1_000,
            },
            66_000,
        )),
        Outcome::Admitted { .. }
    ));
}

#[test]
fn a_task_pointing_at_a_peers_edits_admissions_or_intent_is_told_to_both() {
    let mut state = ready();
    edited(&mut state, "alice", "src/edited.rs", 100);
    ok(run(
        &mut state,
        "lease",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/held.rs".into()],
            ttl_ms: 60_000,
        },
        100,
    ));
    ok(run(
        &mut state,
        "intent",
        Command::IntentSet {
            session_id: "alice".into(),
            paths: vec!["src/declared.rs".into()],
            summary: "plan".into(),
        },
        100,
    ));
    // Carol is in a linked worktree of the same repository.
    prompted(&mut state, "carol", "rework three things", 200);
    targets(
        &mut state,
        "carol",
        &[
            "src/declared.rs",
            "src/edited.rs",
            "src/elsewhere.rs",
            "src/held.rs",
        ],
        200,
    );
    // Carol's task points at files alice works on: for carol that is the
    // ordinary overlap of alice's work with her own.
    let carols = brief(&mut state, "carol", "c1", 300);
    let alice = peer(&carols, "alice");
    assert_eq!(
        alice.overlap,
        ["src/declared.rs", "src/edited.rs", "src/held.rs"]
    );
    assert!(alice.target_overlap.is_empty());
    // Alice is told carol's task points at her files.
    let alices = brief(&mut state, "alice", "a1", 300);
    let carol = peer(&alices, "carol");
    assert_eq!(
        carol.target_overlap,
        ["src/declared.rs", "src/edited.rs", "src/held.rs"]
    );
    assert!(carol.overlap.is_empty(), "carol has not edited anything");
    // Dave works in another repository: never compared.
    prompted(&mut state, "dave", "same names elsewhere", 400);
    targets(&mut state, "dave", &["src/edited.rs"], 400);
    assert!(
        !brief(&mut state, "alice", "a2", 500)
            .peers
            .iter()
            .any(|peer| peer.session_id == "dave")
    );
}

#[test]
fn old_targets_and_an_ended_sessions_targets_cause_no_notice() {
    let mut state = ready();
    prompted(&mut state, "alice", "parse_header", 1_000);
    targets(&mut state, "alice", &["src/http.rs"], 1_000);
    let late = 1_000 + BRIEF_HORIZON_MS + 1;
    prompted(&mut state, "bob", "parse_header", late);
    targets(&mut state, "bob", &["src/http.rs"], late);
    let bobs = brief(&mut state, "bob", "b1", late);
    assert!(
        bobs.peers.iter().all(|peer| peer.target_overlap.is_empty()),
        "alice's targets are past the horizon: {bobs:?}"
    );
    // Alice is not told either: her own targets are past it.
    let alices = brief(&mut state, "alice", "a1", late);
    assert!(
        alices
            .peers
            .iter()
            .all(|peer| peer.target_overlap.is_empty()),
        "{alices:?}"
    );
    // Carol's are fresh, but her session ended.
    prompted(&mut state, "carol", "parse_header", late);
    targets(&mut state, "carol", &["src/http.rs"], late);
    ok(run(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "carol".into(),
        },
        late + 1,
    ));
    let bobs = brief(&mut state, "bob", "b2", late + 2);
    assert!(
        bobs.peers.iter().all(|peer| peer.session_id != "carol"),
        "an ended session with no edits is not news: {bobs:?}"
    );
}

#[test]
fn the_next_prompts_targets_supersede_the_last() {
    let mut state = ready();
    prompted(&mut state, "alice", "parse_header", 100);
    targets(&mut state, "alice", &["src/http.rs"], 100);
    prompted(&mut state, "bob", "parse_header", 200);
    targets(&mut state, "bob", &["src/http.rs"], 200);
    assert_eq!(
        peer(&brief(&mut state, "bob", "b1", 300), "alice").target_overlap,
        ["src/http.rs"]
    );
    targets(&mut state, "alice", &["src/other.rs"], 400);
    let after = brief(&mut state, "bob", "b2", 500);
    assert!(after.changed, "the overlap ended");
    assert!(peer(&after, "alice").target_overlap.is_empty());
    let recorded = state.activity["alice"].prompt_targets.as_ref().unwrap();
    assert_eq!(recorded.paths, ["src/other.rs"]);
    assert_eq!(recorded.at_ms, 400);
    // The whole state, as `langspan status --json` shows it, carries them.
    let json = serde_json::to_value(&state).unwrap();
    assert_eq!(
        json["activity"]["alice"]["prompt_targets"]["paths"][0],
        "src/other.rs"
    );
}

#[test]
fn invalid_prompt_targets_are_rejected_without_events() {
    let state = ready();
    let many: Vec<String> = (0..=langspan_core::MAX_PROMPT_TARGETS)
        .map(|n| format!("src/f{n}.rs"))
        .collect();
    for (session, paths) in [
        ("alice", Vec::new()),
        ("alice", many),
        ("alice", vec!["../outside.rs".to_owned()]),
        ("alice", vec!["/abs/path.rs".to_owned()]),
        ("nobody", vec!["src/http.rs".to_owned()]),
    ] {
        let command = Command::SetPromptTargets {
            session_id: session.into(),
            paths,
        };
        let decision = decide(&state, &command, "bad", 1);
        assert!(
            matches!(decision.outcome, Outcome::Error { .. }),
            "{command:?}"
        );
        assert!(decision.events.is_empty());
    }
    // At the limit is accepted, and a repeated path counts once.
    let full: Vec<String> = (0..langspan_core::MAX_PROMPT_TARGETS)
        .flat_map(|n| [format!("src/f{n}.rs"), format!("src/f{n}.rs")])
        .collect();
    let decision = decide(
        &state,
        &Command::SetPromptTargets {
            session_id: "alice".into(),
            paths: full,
        },
        "full",
        1,
    );
    assert!(
        matches!(
            decision.outcome,
            Outcome::PromptTargets { ref paths, .. }
                if paths.len() == langspan_core::MAX_PROMPT_TARGETS
        ),
        "{:?}",
        decision.outcome
    );
}

#[test]
fn a_targets_event_holds_paths_only_and_from_an_older_generation_is_refused() {
    let mut state = ready();
    let decision = decide(
        &state,
        &Command::SetPromptTargets {
            session_id: "alice".into(),
            paths: vec!["src/http.rs".into()],
        },
        "t",
        5,
    );
    let [event] = decision.events.as_slice() else {
        panic!("{decision:?}")
    };
    assert_eq!(
        serde_json::to_value(event).unwrap(),
        serde_json::json!({
            "type": "prompt_targets_set",
            "session_id": "alice",
            "paths": ["src/http.rs"],
            "at_ms": 5,
            "generation": 1,
        })
    );
    apply(&mut state, event).unwrap();
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert!(apply(&mut state, event).is_err(), "a stale generation");
}

// Presence (roadmap A13): where peers are looking.

fn read(state: &mut State, session: &str, path: &str, now: u64) {
    use langspan_core::{FileState, ObservedFile, SourceOrigin};
    ok(run(
        state,
        &format!("r-{session}-{}-{now}", path.replace('/', "_")),
        Command::ObserveSource {
            session_id: session.into(),
            files: vec![ObservedFile {
                path: path.into(),
                state: FileState::Present {
                    digest: "1".repeat(64),
                    kind: Default::default(),
                },
            }],
            origin: SourceOrigin::Read,
        },
        now,
    ));
}

fn looked(state: &mut State, session: &str, kind: PresenceKind, paths: &[&str], now: u64) {
    ok(run(
        state,
        &format!("p-{session}-{now}"),
        Command::ObservePresence {
            session_id: session.into(),
            kind,
            paths: paths.iter().map(|path| path.to_string()).collect(),
        },
        now,
    ));
}

fn looking<'a>(briefing: &'a Briefing, peer: &str) -> Vec<(&'a str, PresenceKind)> {
    briefing
        .peers
        .iter()
        .find(|work| work.session_id == peer)
        .map(|work| {
            work.looking
                .iter()
                .map(|looked| (looked.path.as_str(), looked.kind))
                .collect()
        })
        .unwrap_or_default()
}

const MINUTE: u64 = 60_000;

#[test]
fn a_peers_reads_and_searches_are_shown_as_presence_until_the_window_passes() {
    let mut state = ready();
    read(&mut state, "alice", "src/auth.rs", 1_000);
    looked(
        &mut state,
        "alice",
        PresenceKind::Search,
        &["src/session"],
        2_000,
    );
    // A peer known only by where it looked is still a peer worth naming.
    let briefing = brief(&mut state, "bob", "b1", 3_000);
    assert_eq!(
        looking(&briefing, "alice"),
        [
            ("src/session", PresenceKind::Search),
            ("src/auth.rs", PresenceKind::Read)
        ]
    );
    let presence = &briefing.peers[0].looking[1];
    assert_eq!(presence.age_s, 2, "seconds since the read");
    // Not to a peer in another repository, and never to oneself.
    assert!(brief(&mut state, "dave", "d1", 3_000).peers.is_empty());
    assert!(looking(&brief(&mut state, "alice", "a1", 3_000), "alice").is_empty());
    // Ten minutes after the last read and search, the clause and the peer
    // are gone; at exactly ten minutes the search is still shown.
    let edge = brief(&mut state, "bob", "b2", 2_000 + 10 * MINUTE);
    assert_eq!(
        looking(&edge, "alice"),
        [("src/session", PresenceKind::Search)]
    );
    let late = brief(&mut state, "bob", "b2b", 2_000 + 10 * MINUTE + 1);
    assert!(late.peers.is_empty(), "{late:?}");
    // Each newer read comes first; one path is listed once; at most four.
    for (i, path) in ["a", "b", "c", "d", "e", "a"].iter().enumerate() {
        read(&mut state, "alice", path, 20 * MINUTE + i as u64);
    }
    let briefing = brief(&mut state, "bob", "b3", 21 * MINUTE);
    let paths: Vec<&str> = looking(&briefing, "alice").iter().map(|l| l.0).collect();
    assert_eq!(paths, ["a", "e", "d", "c"]);
}

#[test]
fn a_file_a_peer_edited_or_holds_is_shown_as_edited_not_as_looked_at() {
    let mut state = ready();
    read(&mut state, "alice", "src/auth.rs", 1_000);
    read(&mut state, "alice", "src/session.rs", 1_100);
    edited(&mut state, "alice", "src/auth.rs", 2_000);
    ok(run(
        &mut state,
        "hold",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/session.rs".into()],
            ttl_ms: 60_000,
        },
        2_100,
    ));
    read(&mut state, "alice", "src/other.rs", 2_200);
    let briefing = brief(&mut state, "bob", "b1", 3_000);
    let alice = &briefing.peers[0];
    assert_eq!(alice.recent[0].path, "src/auth.rs");
    assert_eq!(alice.editing, ["src/session.rs"]);
    assert_eq!(
        looking(&briefing, "alice"),
        [("src/other.rs", PresenceKind::Read)]
    );
}

#[test]
fn presence_never_takes_the_place_of_a_peer_with_work() {
    let mut state = ready();
    read(&mut state, "alice", "src/auth.rs", 5_000);
    edited(&mut state, "carol", "src/old.rs", 1_000);
    let Outcome::Briefing { briefing } = decide(
        &state,
        &Command::Brief {
            session_id: "bob".into(),
            max_peers: 1,
            deliver: false,
            mid_turn: false,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
        },
        "b",
        6_000,
    )
    .outcome
    else {
        panic!()
    };
    // Carol's edit is older than Alice's read, but it is work.
    assert_eq!(names(&briefing), ["carol"]);
    assert_eq!(briefing.omitted_peers, 1);
}

#[test]
fn presence_off_hides_a_sessions_reads_and_searches_from_every_peer() {
    let mut state = ready();
    read(&mut state, "alice", "src/before.rs", 500);
    ok(run(
        &mut state,
        "off",
        Command::SetPresence {
            session_id: "alice".into(),
            shown: false,
            window_ms: langspan_core::PRESENCE_WINDOW_MS,
        },
        600,
    ));
    read(&mut state, "alice", "src/auth.rs", 1_000);
    let refused = run(
        &mut state,
        "search",
        Command::ObservePresence {
            session_id: "alice".into(),
            kind: PresenceKind::Search,
            paths: vec!["src".into()],
        },
        1_100,
    );
    assert!(
        matches!(&refused, Outcome::Error { code, .. } if code == "presence_off"),
        "{refused:?}"
    );
    assert!(brief(&mut state, "bob", "b1", 2_000).peers.is_empty());
    // Turned on again, nothing looked at before or while it was off appears.
    ok(run(
        &mut state,
        "on",
        Command::SetPresence {
            session_id: "alice".into(),
            shown: true,
            window_ms: langspan_core::PRESENCE_WINDOW_MS,
        },
        2_100,
    ));
    assert!(state.presence.is_empty(), "the default is not stored");
    assert!(brief(&mut state, "bob", "b2", 2_200).peers.is_empty());
    // The reads are still versions for the session's own stale checks.
    assert!(
        state.activity["alice"]
            .known
            .iter()
            .any(|known| known.path == "src/auth.rs")
    );
}

#[test]
fn a_session_chooses_how_old_a_peers_presence_it_is_shown() {
    let mut state = ready();
    read(&mut state, "alice", "src/auth.rs", 0);
    let set = |window_ms| Command::SetPresence {
        session_id: "bob".into(),
        shown: true,
        window_ms,
    };
    for wrong in [0, MINUTE - 1, BRIEF_HORIZON_MS + 1] {
        assert!(matches!(
            run(&mut state, "w", set(wrong), 1),
            Outcome::Error { .. }
        ));
    }
    ok(run(&mut state, "w", set(2 * MINUTE), 1));
    assert_eq!(
        looking(&brief(&mut state, "bob", "b1", MINUTE), "alice").len(),
        1
    );
    assert!(
        brief(&mut state, "bob", "b2", 2 * MINUTE + 1)
            .peers
            .is_empty()
    );
    // Bob's window is his own: Carol, in a linked worktree, has the default.
    assert_eq!(
        looking(&brief(&mut state, "carol", "c1", 9 * MINUTE), "alice").len(),
        1
    );
}

#[test]
fn presence_records_name_paths_only_within_bounds() {
    let mut state = ready();
    let observe = |kind, paths: &[&str]| Command::ObservePresence {
        session_id: "alice".into(),
        kind,
        paths: paths.iter().map(|path| path.to_string()).collect(),
    };
    for (kind, paths) in [
        (PresenceKind::Read, &["a"][..]),
        (PresenceKind::Search, &[][..]),
        (PresenceKind::Search, &["a", "b", "c", "d", "e"][..]),
        (PresenceKind::Shell, &["/etc/passwd"][..]),
        (PresenceKind::Shell, &["../outside"][..]),
    ] {
        let outcome = run(&mut state, "bad", observe(kind, paths), 1);
        assert!(matches!(outcome, Outcome::Error { .. }), "{paths:?}");
    }
    assert!(
        state
            .activity
            .get("alice")
            .is_none_or(|a| a.looking.is_empty())
    );
    ok(run(
        &mut state,
        "good",
        observe(PresenceKind::Shell, &["src/b.rs", "src/a.rs", "src/a.rs"]),
        1,
    ));
    // The wire form is the issue's: path, kind and age in seconds.
    let briefing = brief(&mut state, "bob", "b", 5_000);
    let wire = serde_json::to_value(&briefing.peers[0].looking).unwrap();
    assert_eq!(
        wire,
        serde_json::json!([
            {"path": "src/a.rs", "kind": "shell", "age_s": 4},
            {"path": "src/b.rs", "kind": "shell", "age_s": 4},
        ])
    );
}

#[test]
fn a_change_of_presence_alone_waits_for_the_next_prompt_not_the_next_tool_call() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 1_000);
    assert!(brief(&mut state, "bob", "b1", 2_000).changed);
    let briefed = state.activity["bob"].briefed.clone().unwrap();
    assert!(
        !briefed.contains('+'),
        "no presence, the signature as before"
    );
    read(&mut state, "alice", "src/auth.rs", 3_000);
    let mid_turn = |state: &mut State, id: &str, now| match ok(run(
        state,
        id,
        Command::Brief {
            session_id: "bob".into(),
            max_peers: 8,
            deliver: true,
            mid_turn: true,
            left: Vec::new(),
            recent: false,
            recent_window_ms: None,
            mcp: false,
        },
        now,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    };
    assert!(!mid_turn(&mut state, "m1", 4_000).changed);
    assert_eq!(state.activity["bob"].briefed.as_ref(), Some(&briefed));
    // At the prompt it is news, once.
    let prompt = brief(&mut state, "bob", "b2", 5_000);
    assert!(prompt.changed);
    assert_eq!(
        looking(&prompt, "alice"),
        [("src/auth.rs", PresenceKind::Read)]
    );
    assert!(!brief(&mut state, "bob", "b3", 6_000).changed);
    // When it ages out, nothing new is said.
    let quiet = brief(&mut state, "bob", "b4", 3_000 + 10 * MINUTE + 1);
    assert!(!quiet.changed, "{quiet:?}");
    // A peer known only by where it looked is not news mid-turn either.
    read(&mut state, "carol", "src/tree.rs", 11 * MINUTE);
    let later = mid_turn(&mut state, "m3", 11 * MINUTE + 1);
    assert_eq!(looking(&later, "carol").len(), 1);
    assert!(!later.changed, "{later:?}");
    assert!(brief(&mut state, "bob", "b5", 11 * MINUTE + 2).changed);
    // Real work still arrives mid-turn, with the presence beside it.
    edited(&mut state, "alice", "src/b.rs", 11 * MINUTE + 3);
    let news = mid_turn(&mut state, "m2", 11 * MINUTE + 4);
    assert!(news.changed);
    assert_eq!(looking(&news, "carol").len(), 1);
}

#[test]
fn presence_belongs_to_one_generation_and_an_ended_session_has_none() {
    let mut state = ready();
    read(&mut state, "alice", "src/auth.rs", 1_000);
    ok(run(
        &mut state,
        "end",
        Command::SessionEnd {
            session_id: "alice".into(),
        },
        1_500,
    ));
    assert!(brief(&mut state, "bob", "b1", 2_000).peers.is_empty());
    // A record stored before presence existed reads as before.
    let old: SessionActivity = serde_json::from_value(serde_json::json!({
        "session_id": "x", "generation": 1, "last_ms": 0
    }))
    .unwrap();
    assert!(old.looking.is_empty());
    let event: Event = serde_json::from_value(serde_json::json!({
        "type": "source_observed", "session_id": "alice", "files": [],
        "origin": "read", "at_ms": 0, "generation": 1
    }))
    .unwrap();
    assert!(matches!(event, Event::SourceObserved { .. }));
}

/// A prompt's line digests, for prompts that differ only after `shared`.
fn template(shared: usize, own: &str) -> Vec<String> {
    let mut lines: Vec<String> = (0..shared).map(|line| format!("{line:032x}")).collect();
    lines.push(format!("{:0>32}", own));
    lines
}

fn prompted_with(
    state: &mut State,
    session: &str,
    summary: &str,
    lines: Vec<String>,
    now: u64,
) -> u32 {
    match ok(run(
        state,
        &format!("p-{session}-{now}"),
        Command::SessionPrompt {
            session_id: session.into(),
            summary: summary.into(),
            line_digests: lines,
        },
        now,
    )) {
        Outcome::SessionPrompted { shared_lines, .. } => shared_lines,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_prompt_is_told_how_many_leading_lines_a_live_peer_in_its_repository_shares() {
    let mut state = ready();
    let first = prompted_with(&mut state, "alice", "Shared brief", template(3, "a"), 10);
    assert_eq!(first, 0, "nobody to share with yet");
    assert_eq!(state.activity["alice"].line_digests, template(3, "a"));
    // Bob in the same checkout and Carol in a linked worktree share the
    // template's three lines; Dave's repository is someone else's.
    assert_eq!(
        prompted_with(&mut state, "bob", "Shared brief", template(3, "b"), 20),
        3
    );
    assert_eq!(
        prompted_with(&mut state, "carol", "Shared brief", template(3, "c"), 30),
        3
    );
    assert_eq!(
        prompted_with(&mut state, "dave", "Shared brief", template(5, "d"), 40),
        0
    );
    // The longest shared run counts; a session's own earlier prompt does not.
    assert_eq!(
        prompted_with(&mut state, "alice", "Shared brief", template(3, "a"), 50),
        3
    );
    // An ended session is not live: what it shared no longer counts.
    for session in ["alice", "bob", "carol"] {
        if session != "bob" {
            ok(run(
                &mut state,
                &format!("end-{session}"),
                Command::SessionEnd {
                    session_id: session.into(),
                },
                60,
            ));
        }
    }
    assert_eq!(
        prompted_with(&mut state, "bob", "Shared brief", template(3, "b"), 70),
        0
    );
    // Digests are bounded in number and form.
    for lines in [
        vec!["not hex".to_string()],
        vec!["0".repeat(65)],
        (0..33).map(|line| format!("{line:032x}")).collect(),
    ] {
        let decision = decide(
            &state,
            &Command::SessionPrompt {
                session_id: "bob".into(),
                summary: "x".into(),
                line_digests: lines.clone(),
            },
            "bad",
            80,
        );
        assert!(
            matches!(decision.outcome, Outcome::Error { .. }),
            "{lines:?}"
        );
    }
}

#[test]
fn a_declared_intent_replaces_the_prompt_summary_in_a_briefing() {
    let mut state = ready();
    prompted_with(&mut state, "bob", "Shared brief", template(3, "b"), 10);
    let before = brief(&mut state, "alice", "b1", 20);
    assert_eq!(before.peers[0].summary.as_deref(), Some("Shared brief"));
    ok(run(
        &mut state,
        "intent",
        Command::IntentSet {
            session_id: "bob".into(),
            paths: vec!["src/session.rs".into()],
            summary: "refresh the session handler".into(),
        },
        30,
    ));
    let after = brief(&mut state, "alice", "b2", 40);
    assert_eq!(names(&after), ["bob"]);
    assert_eq!(after.peers[0].summary, None);
    assert_eq!(
        after.peers[0].intent.as_deref(),
        Some("refresh the session handler")
    );
}

fn quota(workspace: &str, signature: &str, mentioned_in: &[&str]) -> langspan_core::InflightSymbol {
    langspan_core::InflightSymbol {
        workspace_id: workspace.into(),
        path: "src/quota.rs".into(),
        name: "quota".into(),
        kind: "function".into(),
        signature: signature.into(),
        mentioned_in: mentioned_in.iter().map(|path| (*path).into()).collect(),
        renamed_from: None,
    }
}

fn brief_with(
    state: &mut State,
    session: &str,
    id: &str,
    now: u64,
    inflight: Vec<langspan_core::InflightSymbol>,
) -> Briefing {
    match ok(run(
        state,
        id,
        Command::BriefPrepared {
            session_id: session.into(),
            max_peers: 8,
            deliver: true,
            mid_turn: false,
            recent: false,
            recent_window_ms: None,
            mcp: false,
            left: Vec::new(),
            inflight,
        },
        now,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

/// Roadmap C7, as the core decides it: what the daemon found in flight in
/// another worktree is told to a session whose own files mention it, once
/// per signature, and waits while a message takes the briefing.
#[test]
fn an_in_flight_signature_is_told_once_per_signature_and_only_from_another_worktree() {
    let mut state = ready();
    edited(&mut state, "carol", "src/quota.rs", 1_000);
    edited(&mut state, "alice", "src/routes.rs", 1_000);
    let v1 = "pub fn quota(user: &User) -> u64";
    let found = |signature: &str| {
        vec![
            quota("tree", signature, &["src/routes.rs", "src/other.rs"]),
            // The asking session's own checkout, and another repository:
            // never told through this channel.
            quota("main", signature, &["src/routes.rs"]),
            quota("other", signature, &["src/routes.rs"]),
        ]
    };
    let told = brief_with(&mut state, "alice", "b1", 2_000, found(v1));
    assert_eq!(told.inflight.len(), 1, "{told:?}");
    let notice = &told.inflight[0];
    assert_eq!(notice.worktree, "/repos/app-tree");
    assert_eq!(notice.session.as_deref(), Some("carol"));
    assert_eq!(notice.signature, v1);
    // Only alice's own files are named.
    assert_eq!(notice.mentioned_in, ["src/routes.rs"]);
    assert!(told.changed);
    // Bob's files mention nothing of it.
    assert!(
        brief_with(&mut state, "bob", "b2", 2_000, found(v1))
            .inflight
            .is_empty()
    );
    // Told once.
    let again = brief_with(&mut state, "alice", "b3", 3_000, found(v1));
    assert!(again.inflight.is_empty(), "{again:?}");
    // A message waits for no one: the new signature waits for the next
    // briefing, counted.
    ok(run(
        &mut state,
        "m1",
        Command::SendMessage {
            workspace_id: "main".into(),
            from: Some("bob".into()),
            to: Some("alice".into()),
            text: "hello".into(),
            paths: Vec::new(),
        },
        3_500,
    ));
    let v2 = "pub fn quota(user: &User, limit: u64) -> u64";
    let busy = brief_with(&mut state, "alice", "b4", 4_000, found(v2));
    assert_eq!(busy.messages.len(), 1);
    assert!(busy.inflight.is_empty());
    assert_eq!(busy.omitted_inflight, 1);
    let next = brief_with(&mut state, "alice", "b5", 5_000, found(v2));
    assert_eq!(next.inflight.len(), 1);
    assert_eq!(next.inflight[0].signature, v2);
    assert!(
        brief_with(&mut state, "alice", "b6", 6_000, found(v2))
            .inflight
            .is_empty()
    );
    // A look from outside shows it and records nothing.
    let v3 = "pub fn quota() -> u64";
    let look = match ok(run(
        &mut state,
        "b7",
        Command::BriefPrepared {
            session_id: "alice".into(),
            max_peers: 8,
            deliver: false,
            mid_turn: false,
            recent: false,
            recent_window_ms: None,
            mcp: false,
            left: Vec::new(),
            inflight: found(v3),
        },
        7_000,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    };
    assert_eq!(look.inflight.len(), 1);
    assert_eq!(
        brief_with(&mut state, "alice", "b8", 8_000, found(v3))
            .inflight
            .len(),
        1
    );
}

#[test]
fn in_flight_lines_are_at_most_three_within_their_share_and_the_rest_counted() {
    let mut state = ready();
    edited(&mut state, "alice", "src/routes.rs", 1_000);
    let many: Vec<_> = (0..10)
        .map(|n| langspan_core::InflightSymbol {
            name: format!("quota_{n}"),
            ..quota("tree", "pub fn q() -> u64", &["src/routes.rs"])
        })
        .collect();
    let told = brief_with(&mut state, "alice", "b1", 2_000, many.clone());
    assert!((1..=3).contains(&told.inflight.len()), "{told:?}");
    assert!(
        told.inflight
            .iter()
            .map(langspan_core::inflight_cost)
            .sum::<usize>()
            <= langspan_core::INFLIGHT_BYTES
    );
    assert_eq!(told.omitted_inflight as usize, 10 - told.inflight.len());
    // More than the daemon may offer is refused.
    let too_many: Vec<_> = (0..=langspan_core::INFLIGHT_CANDIDATES)
        .map(|n| langspan_core::InflightSymbol {
            name: format!("q{n}"),
            ..quota("tree", "pub fn q() -> u64", &["src/routes.rs"])
        })
        .collect();
    let refused = run(
        &mut state,
        "b2",
        Command::BriefPrepared {
            session_id: "alice".into(),
            max_peers: 8,
            deliver: true,
            mid_turn: false,
            recent: false,
            recent_window_ms: None,
            mcp: false,
            left: Vec::new(),
            inflight: too_many,
        },
        3_000,
    );
    assert!(matches!(refused, Outcome::Error { .. }), "{refused:?}");
}

/// `count` in-flight signatures of one worktree, each a name of its own.
fn many_inflight(count: usize) -> Vec<langspan_core::InflightSymbol> {
    (0..count)
        .map(|n| langspan_core::InflightSymbol {
            name: format!("quota_{n:04}"),
            ..quota("tree", "pub fn q() -> u64", &["src/routes.rs"])
        })
        .collect()
}

/// Brief `session` as the daemon does, offering at most
/// `INFLIGHT_CANDIDATES` of `all` it was not told, until it was told every
/// one. Returns the names told, in order, and the clock after.
fn tell_every(
    state: &mut State,
    session: &str,
    all: &[langspan_core::InflightSymbol],
    mut now: u64,
) -> (Vec<String>, u64) {
    let mut told = Vec::new();
    while told.len() < all.len() {
        let offered: Vec<_> = all
            .iter()
            .filter(|item| !langspan_core::inflight_told(state, session, item))
            .take(langspan_core::INFLIGHT_CANDIDATES)
            .cloned()
            .collect();
        if offered.is_empty() {
            break;
        }
        now += 1;
        let briefing = brief_with(state, session, &format!("t{now}"), now, offered);
        assert!(!briefing.inflight.is_empty(), "nothing told at {now}");
        told.extend(briefing.inflight.into_iter().map(|notice| notice.name));
    }
    (told, now)
}

/// Offer every one of `all` again, unfiltered, as a briefing window that
/// came round to them would, and return the names told again.
fn told_again(
    state: &mut State,
    session: &str,
    all: &[langspan_core::InflightSymbol],
    mut now: u64,
) -> Vec<String> {
    let mut again = Vec::new();
    for chunk in all.chunks(langspan_core::INFLIGHT_CANDIDATES) {
        now += 1;
        let briefing = brief_with(state, session, &format!("r{now}"), now, chunk.to_vec());
        again.extend(briefing.inflight.into_iter().map(|notice| notice.name));
    }
    again
}

/// What a session was told is remembered for as many signatures as it can
/// be offered in turn (review comment 4704678374): the daemon's briefing
/// window now rotates over every in-flight row, so a memory smaller than
/// the rows would tell the oldest again. At the limit nothing is told
/// twice; one past it, every signature is still told once, and only one of
/// the oldest is forgotten.
#[test]
fn a_session_is_told_each_in_flight_signature_once_up_to_its_memory() {
    let limit = langspan_core::MAX_INFLIGHT_TOLD;
    assert!(limit >= 1024, "a session remembers only {limit}");
    let mut state = ready();
    edited(&mut state, "alice", "src/routes.rs", 1_000);
    let all = many_inflight(limit);
    let (told, now) = tell_every(&mut state, "alice", &all, 2_000);
    let names: Vec<String> = all.iter().map(|item| item.name.clone()).collect();
    assert_eq!(told, names, "each told once, in order");
    assert_eq!(
        told_again(&mut state, "alice", &all, now),
        Vec::<String>::new()
    );

    let mut state = ready();
    edited(&mut state, "alice", "src/routes.rs", 1_000);
    let all = many_inflight(limit + 1);
    let (told, now) = tell_every(&mut state, "alice", &all, 2_000);
    let names: Vec<String> = all.iter().map(|item| item.name.clone()).collect();
    assert_eq!(told, names, "each told once, none starved");
    // One of the first briefing's lines, which were told together.
    let again = told_again(&mut state, "alice", &all, now);
    assert_eq!(again.len(), 1, "only the oldest is forgotten: {again:?}");
    assert!(
        names[..langspan_core::INFLIGHT_LINES].contains(&again[0]),
        "{again:?}"
    );
}

/// Presence (roadmap A13) and in-flight signatures (roadmap C7) share one
/// briefing signature: each is news on its own, and neither repeats the
/// other once told.
#[test]
fn presence_and_an_in_flight_signature_are_each_news_once() {
    let mut state = ready();
    edited(&mut state, "carol", "src/quota.rs", 1_000);
    edited(&mut state, "alice", "src/routes.rs", 1_000);
    read(&mut state, "bob", "src/auth.rs", 1_500);
    let found = |signature: &str| vec![quota("tree", signature, &["src/routes.rs"])];
    let v1 = "pub fn quota(user: &User) -> u64";
    let both = brief_with(&mut state, "alice", "b1", 2_000, found(v1));
    assert!(both.changed);
    assert_eq!(both.inflight.len(), 1, "{both:?}");
    assert_eq!(looking(&both, "bob"), [("src/auth.rs", PresenceKind::Read)]);
    // Told once: the next briefings carry presence but not the signature,
    // and once settled nothing new is news.
    let next = brief_with(&mut state, "alice", "b2", 2_500, found(v1));
    assert!(next.inflight.is_empty(), "{next:?}");
    assert_eq!(looking(&next, "bob"), [("src/auth.rs", PresenceKind::Read)]);
    let same = brief_with(&mut state, "alice", "b2b", 3_000, found(v1));
    assert!(!same.changed, "{same:?}");
    assert!(same.inflight.is_empty());
    // Mid-turn, a new presence path is not news, with a signature carried.
    read(&mut state, "bob", "src/session.rs", 3_200);
    let mid = match ok(run(
        &mut state,
        "b2c",
        Command::BriefPrepared {
            session_id: "alice".into(),
            max_peers: 8,
            deliver: true,
            mid_turn: true,
            recent: false,
            recent_window_ms: None,
            mcp: false,
            left: Vec::new(),
            inflight: found(v1),
        },
        3_500,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    };
    assert!(!mid.changed, "{mid:?}");
    assert!(mid.inflight.is_empty());
    // At the next prompt it is news; the told signature stays told.
    let looked = brief_with(&mut state, "alice", "b3", 4_000, found(v1));
    assert!(looked.changed, "{looked:?}");
    assert!(looked.inflight.is_empty());
    assert_eq!(looking(&looked, "bob").len(), 2);
    // A changed signature is news while presence is unchanged.
    let v2 = "pub fn quota(user: &User, limit: u64) -> u64";
    let changed = brief_with(&mut state, "alice", "b4", 5_000, found(v2));
    assert!(changed.changed, "{changed:?}");
    assert_eq!(changed.inflight.len(), 1);
    assert_eq!(changed.inflight[0].signature, v2);
    assert!(
        brief_with(&mut state, "alice", "b5", 6_000, found(v2))
            .inflight
            .is_empty()
    );
}

// Recent change at session start (roadmap C3, ADR 0023).

const HOUR: u64 = 60 * MINUTE;

fn register_at(state: &mut State, session: &str, workspace: &str, now: u64) {
    ok(run(
        state,
        &format!("s-{session}-{now}"),
        Command::SessionRegister {
            session_id: session.into(),
            workspace_id: workspace.into(),
            parent: None,
            headless: false,
        },
        now,
    ));
}

fn end(state: &mut State, session: &str, now: u64) {
    ok(run(
        state,
        &format!("end-{session}-{now}"),
        Command::SessionEnd {
            session_id: session.into(),
        },
        now,
    ));
}

fn start_brief(state: &mut State, session: &str, window: Option<u64>, now: u64) -> Briefing {
    match ok(run(
        state,
        &format!("start-{session}-{now}-{}", window.unwrap_or(0)),
        Command::Brief {
            session_id: session.into(),
            max_peers: 8,
            deliver: true,
            mid_turn: false,
            left: Vec::new(),
            recent: true,
            recent_window_ms: window,
            mcp: false,
        },
        now,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

fn recent(state: &mut State, session: &str, now: u64) -> RecentChanges {
    start_brief(state, session, None, now)
        .recent
        .expect("a starting briefing carries the recent section")
}

fn admit(state: &mut State, id: &str, session: &str, path: &str, now: u64) -> Outcome {
    run(
        state,
        id,
        Command::AdmitTool {
            session_id: session.into(),
            operation: ToolIdentity {
                operation_id: format!("op-{id}"),
                source: "test".into(),
                tool: "Edit".into(),
                input_digest: "f".repeat(64),
            },
            paths: vec![path.into()],
            base: Vec::new(),
            regions: Vec::new(),
            overwrites: Vec::new(),
            ttl_ms: 30_000,
        },
        now,
    )
}

fn signature_change(name: &str, before: &str, after: &str) -> InterfaceChange {
    InterfaceChange {
        name: name.into(),
        kind: "function".into(),
        change: "signature_changed".into(),
        before: Some(before.into()),
        after: Some(after.into()),
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

fn changed_paths(recent: &RecentChanges) -> Vec<String> {
    recent
        .changed_paths
        .iter()
        .map(|path| path.path.clone())
        .collect()
}

#[test]
fn a_starting_session_hears_which_files_changed_recently_and_by_how_many_sessions() {
    let mut state = ready();
    let start = 10 * HOUR;
    edited(&mut state, "alice", "src/api.rs", start - 2 * HOUR);
    edited(&mut state, "bob", "src/store.rs", start - HOUR);
    edited(&mut state, "bob", "src/api.rs", start - 40 * MINUTE);
    // Another repository is not this one's news.
    edited(&mut state, "dave", "src/api.rs", start - MINUTE);
    end(&mut state, "alice", start - 90 * MINUTE);
    end(&mut state, "bob", start - 30 * MINUTE);
    register_at(&mut state, "erin", "main", start);
    let recent = recent(&mut state, "erin", start);
    assert_eq!(recent.window_ms, RECENT_WINDOW_MS);
    let changed: Vec<(&str, u32, u64)> = recent
        .changed_paths
        .iter()
        .map(|path| (path.path.as_str(), path.sessions, path.age_ms))
        .collect();
    assert_eq!(
        changed,
        [("src/api.rs", 2, 40 * MINUTE), ("src/store.rs", 1, HOUR)],
        "most sessions first, each with the newest age"
    );
    assert!(recent.changed_paths.iter().all(|path| !path.elsewhere));
    // Two sessions edited one file: it was worked on from two sides.
    assert_eq!(recent.contested_paths.len(), 1);
    assert_eq!(recent.contested_paths[0].path, "src/api.rs");
    assert_eq!(recent.contested_paths[0].sessions, 2);
    assert_eq!(recent.contested_paths[0].refusals, 0);
}

#[test]
fn edits_in_a_linked_worktree_are_counted_and_marked_as_elsewhere() {
    let mut state = ready();
    edited(&mut state, "carol", "src/tree.rs", HOUR);
    end(&mut state, "carol", HOUR + 1);
    register_at(&mut state, "erin", "main", 2 * HOUR);
    let recent = recent(&mut state, "erin", 2 * HOUR);
    assert_eq!(changed_paths(&recent), ["src/tree.rs"]);
    assert!(recent.changed_paths[0].elsewhere, "{recent:?}");
    // The line journal is per checkout: nothing of carol's is left in this one.
    assert!(recent.left_uncommitted.is_empty(), "{recent:?}");
}

#[test]
fn an_interface_change_appears_even_when_the_new_sessions_files_do_not_mention_it() {
    let mut state = ready();
    edited(&mut state, "bob", "src/store.rs", HOUR);
    ok(run(
        &mut state,
        "interface-1",
        Command::ObserveInterface {
            workspace_id: "main".into(),
            path: "src/store.rs".into(),
            changes: vec![signature_change(
                "save",
                "fn save(x: &Item)",
                "fn save(x: &Item, sync: bool)",
            )],
            dependents: vec!["src/api.rs".into()],
        },
        HOUR + 1,
    ));
    register_at(&mut state, "erin", "main", 2 * HOUR);
    let briefing = start_brief(&mut state, "erin", None, 2 * HOUR);
    // The peer section names only changes its own files mention.
    assert!(briefing.interface_changes.is_empty());
    let recent = briefing.recent.unwrap();
    assert_eq!(recent.interface_changes.len(), 1);
    let change = &recent.interface_changes[0];
    assert_eq!(change.path, "src/store.rs");
    assert_eq!(change.by_session.as_deref(), Some("bob"));
    assert_eq!(
        change.changes[0].before.as_deref(),
        Some("fn save(x: &Item)")
    );
    assert_eq!(
        change.changes[0].after.as_deref(),
        Some("fn save(x: &Item, sync: bool)")
    );
    assert_eq!(change.dependents, ["src/api.rs"]);
    assert_eq!(change.age_ms, HOUR - 1);
}

#[test]
fn a_path_refused_to_one_session_and_edited_by_another_is_contested() {
    let mut state = ready();
    assert!(matches!(
        ok(admit(&mut state, "bob-1", "bob", "src/api.rs", HOUR)),
        Outcome::Admitted { .. }
    ));
    let refused = admit(&mut state, "alice-1", "alice", "src/api.rs", HOUR + 10);
    assert!(
        !matches!(refused, Outcome::Admitted { .. }),
        "the held file is refused: {refused:?}"
    );
    edited(&mut state, "bob", "src/api.rs", HOUR + 20);
    register_at(&mut state, "erin", "main", 2 * HOUR);
    let recent = recent(&mut state, "erin", 2 * HOUR);
    assert_eq!(recent.contested_paths.len(), 1, "{recent:?}");
    let contested = &recent.contested_paths[0];
    assert_eq!(contested.path, "src/api.rs");
    assert_eq!(contested.refusals, 1);
    assert_eq!(contested.sessions, 1, "only bob's edit landed");
}

#[test]
fn an_ended_session_in_this_checkout_is_a_candidate_for_work_left_uncommitted() {
    let mut state = ready();
    edited(&mut state, "alice", "src/lib.rs", HOUR);
    edited(&mut state, "alice", "src/main.rs", HOUR + 5);
    end(&mut state, "alice", HOUR + 10);
    // A live session's edits are its own, still in progress.
    edited(&mut state, "bob", "src/bob.rs", HOUR + 20);
    register_at(&mut state, "erin", "main", 2 * HOUR);
    let recent = recent(&mut state, "erin", 2 * HOUR);
    assert_eq!(recent.left_uncommitted.len(), 1, "{recent:?}");
    let left = &recent.left_uncommitted[0];
    assert_eq!(left.session_id, "alice");
    assert_eq!(left.paths, ["src/main.rs", "src/lib.rs"], "newest first");
    assert_eq!(left.age_ms, HOUR - 5);
    assert_eq!(recent.root, "/repos/app");
}

#[test]
fn nothing_older_than_the_window_or_from_before_the_current_boot_appears() {
    let mut state = ready();
    let now = 10 * HOUR;
    edited(
        &mut state,
        "alice",
        "src/old.rs",
        now - RECENT_WINDOW_MS - 1,
    );
    edited(&mut state, "alice", "src/edge.rs", now - RECENT_WINDOW_MS);
    edited(&mut state, "bob", "src/new.rs", now - 2 * MINUTE);
    register_at(&mut state, "erin", "main", now);
    assert_eq!(
        changed_paths(&recent(&mut state, "erin", now)),
        ["src/new.rs", "src/edge.rs"]
    );
    // A shorter configured window.
    let short = start_brief(&mut state, "erin", Some(5 * MINUTE), now)
        .recent
        .unwrap();
    assert_eq!(short.window_ms, 5 * MINUTE);
    assert_eq!(changed_paths(&short), ["src/new.rs"]);
    // A window outside the accepted range is refused, not stretched.
    for window in [MINUTE - 1, 24 * HOUR + 1] {
        let outcome = run(
            &mut state,
            &format!("bad-{window}"),
            Command::Brief {
                session_id: "erin".into(),
                max_peers: 8,
                deliver: false,
                mid_turn: false,
                left: Vec::new(),
                recent: true,
                recent_window_ms: Some(window),
                mcp: false,
            },
            now,
        );
        assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    }
    // After a restart, the earlier run's edits and refusals are gone, even
    // within the window: only this boot's events count.
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    register_at(&mut state, "erin", "main", now + MINUTE);
    register_at(&mut state, "bob", "main", now + MINUTE);
    let after = recent(&mut state, "erin", now + MINUTE);
    assert!(after.is_empty(), "{after:?}");
    edited(&mut state, "bob", "src/later.rs", now + 2 * MINUTE);
    assert_eq!(
        changed_paths(&recent(&mut state, "erin", now + 3 * MINUTE)),
        ["src/later.rs"]
    );
}

#[test]
fn the_section_is_bounded_and_counts_what_it_leaves_out() {
    let mut state = ready();
    for n in 0..12 {
        edited(&mut state, "alice", &format!("src/a{n:02}.rs"), HOUR + n);
        edited(
            &mut state,
            "bob",
            &format!("src/a{n:02}.rs"),
            HOUR + 100 + n,
        );
    }
    register_at(&mut state, "erin", "main", 2 * HOUR);
    let recent = recent(&mut state, "erin", 2 * HOUR);
    assert_eq!(recent.changed_paths.len(), 8);
    assert_eq!(recent.contested_paths.len(), 4);
    // 4 changed and 8 contested paths left out.
    assert_eq!(recent.omitted, 12, "{recent:?}");
    // Newest first among equals.
    assert_eq!(recent.changed_paths[0].path, "src/a11.rs");
}

#[test]
fn only_a_starting_briefing_carries_the_section_and_it_is_never_news_by_itself() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", HOUR);
    end(&mut state, "alice", HOUR + 1);
    register_at(&mut state, "erin", "main", 2 * HOUR);
    let start = start_brief(&mut state, "erin", None, 2 * HOUR);
    assert!(!start.recent.unwrap().is_empty());
    // A prompt briefing does not carry it, and what it said is not
    // remembered as delivered peer work.
    let prompt = brief(&mut state, "erin", "prompt-1", 2 * HOUR + 10);
    assert_eq!(prompt.recent, None);
    // A mid-turn briefing cannot ask for it.
    let outcome = run(
        &mut state,
        "mid",
        Command::Brief {
            session_id: "erin".into(),
            max_peers: 8,
            deliver: true,
            mid_turn: true,
            left: Vec::new(),
            recent: true,
            recent_window_ms: None,
            mcp: false,
        },
        2 * HOUR + 20,
    );
    assert!(matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    // The asking session's own edits are not news to it.
    edited(&mut state, "erin", "src/mine.rs", 2 * HOUR + 30);
    let again = recent(&mut state, "erin", 2 * HOUR + 40);
    assert!(
        again
            .changed_paths
            .iter()
            .all(|path| path.path != "src/mine.rs")
    );
}

#[test]
fn presence_that_only_partly_ages_out_is_not_news() {
    let mut state = ready();
    read(&mut state, "alice", "src/auth.rs", 1_000);
    read(&mut state, "alice", "src/token.rs", 5 * MINUTE);
    assert!(brief(&mut state, "bob", "b1", 5 * MINUTE + 1).changed);
    // The older path ages out, the newer one remains: nothing happened.
    let partly = brief(&mut state, "bob", "b2", 10 * MINUTE + 2_000);
    assert_eq!(
        looking(&partly, "alice"),
        [("src/token.rs", PresenceKind::Read)]
    );
    assert!(!partly.changed, "{partly:?}");
    // A path not shown before is news again.
    read(&mut state, "alice", "src/session.rs", 11 * MINUTE);
    assert!(brief(&mut state, "bob", "b3", 11 * MINUTE + 1).changed);
}

#[test]
fn presence_never_takes_a_work_bearing_peers_place_in_the_briefing() {
    let mut state = ready();
    let long =
        |peer: usize, n: usize, kind: &str| format!("src/{kind}{peer}-{n}-{}", "x".repeat(450));
    for peer in 0..8 {
        let id = format!("p{peer}");
        ok(run(
            &mut state,
            &format!("s-{id}"),
            Command::SessionRegister {
                session_id: id.clone(),
                workspace_id: "main".into(),
                parent: None,
                headless: false,
            },
            0,
        ));
        for n in 0..8 {
            edited(&mut state, &id, &long(peer, n, "w"), 1_000 + n as u64);
        }
        let looked_at: Vec<String> = (0..4).map(|n| long(peer, n, "l")).collect();
        let looked_at: Vec<&str> = looked_at.iter().map(String::as_str).collect();
        looked(&mut state, &id, PresenceKind::Search, &looked_at, 2_000);
    }
    let briefing = brief(&mut state, "bob", "b1", 3_000);
    for peer in 0..8 {
        assert!(
            briefing
                .peers
                .iter()
                .any(|work| work.session_id == format!("p{peer}")),
            "p{peer} dropped: {} peers shown",
            briefing.peers.len()
        );
    }
    assert_eq!(briefing.omitted_peers, 0);
}
