//! Publishing a workspace's work and receiving teammates' publications:
//! what leaves, what is accepted, how it is ordered and when it goes stale.
use langspan_core::{
    Command, Event, Outcome, Publication, REMOTE_HORIZON_MS, RemoteWork, State, ToolResult, apply,
    decide,
};

const ME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const THEM: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

fn enable(state: &mut State, id: &str, share_task: bool) -> Outcome {
    run(
        state,
        id,
        Command::RemoteEnable {
            workspace_id: "work".into(),
            remote: "origin".into(),
            participant: ME.into(),
            name: "Alice".into(),
            share_task,
            publish_as_branch: false,
        },
        0,
    )
}

fn ready(share_task: bool) -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (id, root) in [("work", "/repos/work"), ("other", "/repos/other")] {
        ok(run(
            &mut state,
            &format!("w-{id}"),
            Command::WorkspaceRegister {
                workspace_id: id.into(),
                root: root.into(),
            },
            0,
        ));
    }
    for (session, workspace) in [
        ("claude-code.alice-session", "work"),
        ("codex.elsewhere", "other"),
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
    ok(enable(&mut state, "enable", share_task));
    state
}

fn works(state: &mut State, session: &str, path: &str, task: &str, now: u64) {
    ok(run(
        state,
        &format!("p-{session}-{now}"),
        Command::SessionPrompt {
            session_id: session.into(),
            summary: task.into(),
            line_digests: Vec::new(),
        },
        now,
    ));
    ok(run(
        state,
        &format!("o-{session}-{now}"),
        Command::Observe {
            session_id: session.into(),
            operation_id: format!("op-{now}"),
            source: "test".into(),
            tool: "Edit".into(),
            paths: vec![path.into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        now,
    ));
}

fn outbox(state: &mut State, now: u64) -> (Publication, String, bool) {
    match ok(run(
        state,
        "read",
        Command::RemoteOutbox {
            workspace_id: "work".into(),
        },
        now,
    )) {
        Outcome::RemoteOutbox {
            publication,
            signature,
            changed,
        } => (publication, signature, changed),
        other => panic!("{other:?}"),
    }
}

fn published(state: &mut State, id: &str, sequence: u64, signature: &str) -> Outcome {
    run(
        state,
        id,
        Command::RemotePublished {
            workspace_id: "work".into(),
            sequence,
            signature: signature.into(),
            payload: "{}".into(),
            published_at_ms: 0,
            as_branch: None,
        },
        0,
    )
}

fn theirs(sequence: u64, path: &str) -> Publication {
    Publication {
        participant: THEM.into(),
        name: "Bob".into(),
        sequence,
        work: vec![RemoteWork {
            host: "codex".into(),
            session: "0b0b0b0b".into(),
            branch_ref: Some("refs/heads/bob/feature".into()),
            summary: Some("Rename the gate module".into()),
            editing: vec![],
            recent: vec![path.into()],
        }],
        withdrawn: false,
    }
}

fn receive(state: &mut State, id: &str, publication: Publication, now: u64) -> Outcome {
    run(
        state,
        id,
        Command::ObserveRemote {
            workspace_id: "work".into(),
            publication,
        },
        now,
    )
}

fn briefed(state: &mut State, id: &str, now: u64) -> langspan_core::Briefing {
    match ok(run(
        state,
        id,
        Command::Brief {
            session_id: "claude-code.alice-session".into(),
            max_peers: 8,
            deliver: true,
            mid_turn: false,
            recent: false,
            recent_window_ms: None,
        },
        now,
    )) {
        Outcome::Briefing { briefing } => briefing,
        other => panic!("{other:?}"),
    }
}

#[test]
fn prompt_targets_are_never_published() {
    let mut state = ready(true);
    works(
        &mut state,
        "claude-code.alice-session",
        "src/api.rs",
        "Add retry",
        10,
    );
    let (before, signature, _) = outbox(&mut state, 20);
    ok(run(
        &mut state,
        "targets",
        Command::SetPromptTargets {
            session_id: "claude-code.alice-session".into(),
            paths: vec!["src/targeted-only.rs".into()],
        },
        30,
    ));
    let (after, unchanged, _) = outbox(&mut state, 40);
    assert_eq!(after, before);
    assert_eq!(unchanged, signature, "targets are not part of what leaves");
    assert!(
        !serde_json::to_string(&after)
            .unwrap()
            .contains("targeted-only"),
        "{after:?}"
    );
}

#[test]
fn a_publication_carries_this_repositorys_work_and_no_task_line_unless_shared() {
    let mut state = ready(false);
    works(
        &mut state,
        "claude-code.alice-session",
        "src/api.rs",
        "Add retry",
        10,
    );
    works(
        &mut state,
        "codex.elsewhere",
        "secret/other-repo.rs",
        "Unrelated",
        10,
    );
    let (publication, _, changed) = outbox(&mut state, 20);
    assert!(changed);
    assert_eq!(publication.participant, ME);
    assert_eq!(publication.sequence, 1);
    assert_eq!(
        publication.work.len(),
        1,
        "another repository's work never leaves"
    );
    let work = &publication.work[0];
    assert_eq!(
        (work.host.as_str(), work.session.as_str()),
        ("claude-code", "alice-se")
    );
    assert_eq!(work.recent, ["src/api.rs"]);
    assert_eq!(work.summary, None, "task lines stay local by default");

    let mut sharing = ready(true);
    works(
        &mut sharing,
        "claude-code.alice-session",
        "src/api.rs",
        "Add retry",
        10,
    );
    assert_eq!(
        outbox(&mut sharing, 20).0.work[0].summary.as_deref(),
        Some("Add retry")
    );
}

#[test]
fn sequences_only_rise_and_unchanged_content_is_not_a_change() {
    let mut state = ready(false);
    works(
        &mut state,
        "claude-code.alice-session",
        "src/api.rs",
        "t",
        10,
    );
    let (first, signature, _) = outbox(&mut state, 20);
    assert!(matches!(
        published(&mut state, "skip", 2, &signature),
        Outcome::Error { .. }
    ));
    ok(published(&mut state, "pub-1", first.sequence, &signature));
    let (second, same, changed) = outbox(&mut state, 30);
    assert_eq!((second.sequence, changed), (2, false));
    assert_eq!(same, signature);
    // A heartbeat republishes unchanged content under the next sequence.
    ok(published(&mut state, "pub-2", 2, &same));
    // Re-enabling as the same participant continues the sequence.
    ok(enable(&mut state, "enable-again", true));
    assert_eq!(outbox(&mut state, 40).0.sequence, 3);
}

#[test]
fn a_received_publication_is_ordered_by_its_senders_sequence_alone() {
    let mut state = ready(false);
    let accepted = |outcome| matches!(outcome, Outcome::RemoteObserved { accepted: true, .. });
    assert!(accepted(receive(
        &mut state,
        "r1",
        theirs(5, "src/a.rs"),
        100
    )));
    // A replay and an older publication arriving late change nothing.
    for (id, sequence) in [("r2", 5), ("r3", 4)] {
        let outcome = receive(&mut state, id, theirs(sequence, "src/old.rs"), 200);
        assert!(matches!(
            outcome,
            Outcome::RemoteObserved {
                accepted: false,
                ..
            }
        ));
    }
    let held = &state.remote_peers[&format!("work:{THEM}")];
    assert_eq!(held.publication.work[0].recent, ["src/a.rs"]);
    assert_eq!(held.received_ms, 100, "a replay does not refresh its age");
    assert!(accepted(receive(
        &mut state,
        "r4",
        theirs(6, "src/b.rs"),
        300
    )));
    // After a restart nothing held is current; the same sequence is accepted
    // again because it is this generation's first sight of it.
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert!(accepted(receive(
        &mut state,
        "r5",
        theirs(6, "src/b.rs"),
        5
    )));
}

#[test]
fn hostile_or_malformed_publications_are_rejected_whole() {
    let state = ready(false);
    let mut cases = Vec::new();
    let mut edit = |change: &dyn Fn(&mut Publication)| {
        let mut publication = theirs(1, "src/a.rs");
        change(&mut publication);
        cases.push(publication);
    };
    edit(&|p| p.participant = "not-hex".into());
    edit(&|p| p.participant = ME.into());
    edit(&|p| p.name = "Bob\n- injected line".into());
    edit(&|p| p.name = "x".repeat(65));
    edit(&|p| p.work[0].recent = vec!["../../etc/passwd".into()]);
    edit(&|p| p.work[0].recent = vec!["a".into(); 9]);
    edit(&|p| p.work[0].summary = Some("two\nlines".into()));
    edit(&|p| p.work[0].branch_ref = Some("HEAD".into()));
    edit(&|p| p.work[0].host = "co dex".into());
    edit(&|p| p.work[0].session = "x".repeat(17));
    edit(&|p| p.work = vec![p.work[0].clone(); 9]);
    for publication in cases {
        let command = Command::ObserveRemote {
            workspace_id: "work".into(),
            publication,
        };
        let decision = decide(&state, &command, "bad", 1);
        assert!(
            matches!(decision.outcome, Outcome::Error { .. }),
            "{command:?}"
        );
        assert!(decision.events.is_empty());
    }
    // A workspace that does not publish does not receive either.
    let decision = decide(
        &state,
        &Command::ObserveRemote {
            workspace_id: "other".into(),
            publication: theirs(1, "src/a.rs"),
        },
        "bad",
        1,
    );
    assert!(
        matches!(decision.outcome, Outcome::Error { ref code, .. } if code == "remote_not_enabled")
    );
}

#[test]
fn a_briefing_names_a_teammates_work_until_they_go_quiet_or_publication_is_disabled() {
    let mut state = ready(false);
    works(
        &mut state,
        "claude-code.alice-session",
        "src/api.rs",
        "t",
        10,
    );
    ok(receive(&mut state, "r1", theirs(1, "src/api.rs"), 1_000));
    let briefing = briefed(&mut state, "b1", 2_000);
    assert!(briefing.changed && briefing.peers.is_empty());
    assert_eq!(briefing.remote.len(), 1);
    let bob = &briefing.remote[0];
    assert_eq!(bob.name, "Bob");
    assert_eq!(bob.overlap, ["src/api.rs"]);
    assert_eq!(bob.received_age_ms, 1_000);
    assert!(
        !briefed(&mut state, "b2", 3_000).changed,
        "only its age moved"
    );

    // Not heard from within the horizon: treated as disconnected.
    let quiet = briefed(&mut state, "b3", 1_000 + REMOTE_HORIZON_MS + 1);
    assert!(quiet.remote.is_empty() && quiet.changed);
    // A new publication brings them back; an empty one withdraws their work.
    ok(receive(
        &mut state,
        "r2",
        theirs(2, "src/api.rs"),
        2_000_000,
    ));
    assert_eq!(briefed(&mut state, "b4", 2_000_001).remote.len(), 1);
    let mut withdrawn = theirs(3, "src/api.rs");
    withdrawn.work.clear();
    ok(receive(&mut state, "r3", withdrawn, 2_000_002));
    assert!(briefed(&mut state, "b5", 2_000_003).remote.is_empty());

    ok(receive(
        &mut state,
        "r4",
        theirs(4, "src/api.rs"),
        2_000_004,
    ));
    ok(run(
        &mut state,
        "off",
        Command::RemoteDisable {
            workspace_id: "work".into(),
        },
        2_000_005,
    ));
    assert!(state.remote_peers.is_empty());
    let link = &state.remote_links["work"];
    assert!(
        !link.enabled && !link.withdrawn,
        "the ref is still to be removed"
    );
    assert!(briefed(&mut state, "b6", 2_000_006).remote.is_empty());
    // Authority is untouched throughout: no lease was ever created.
    assert!(state.leases.is_empty());
}

#[test]
fn enabling_validates_its_inputs() {
    let state = ready(false);
    for (workspace, remote, participant, name) in [
        ("missing", "origin", ME, "Alice"),
        ("work", "ori gin", ME, "Alice"),
        (
            "work",
            "origin",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "Alice",
        ),
        ("work", "origin", ME, ""),
    ] {
        let command = Command::RemoteEnable {
            workspace_id: workspace.into(),
            remote: remote.into(),
            participant: participant.into(),
            name: name.into(),
            share_task: false,
            publish_as_branch: false,
        };
        let decision = decide(&state, &command, "bad", 1);
        assert!(
            matches!(decision.outcome, Outcome::Error { .. }),
            "{command:?}"
        );
    }
}

#[test]
fn disabling_keeps_the_sequence_so_enabling_again_never_reuses_one() {
    let mut state = ready(true);
    works(
        &mut state,
        "claude-code.alice-session",
        "src/api.rs",
        "t",
        10,
    );
    let (first, signature, _) = outbox(&mut state, 20);
    ok(published(&mut state, "pub-1", first.sequence, &signature));
    ok(run(
        &mut state,
        "off",
        Command::RemoteDisable {
            workspace_id: "work".into(),
        },
        30,
    ));
    // While disabled nothing is offered for publication or accepted.
    for command in [
        Command::RemoteOutbox {
            workspace_id: "work".into(),
        },
        Command::ObserveRemote {
            workspace_id: "work".into(),
            publication: theirs(1, "src/a.rs"),
        },
    ] {
        let decision = decide(&state, &command, "x", 31);
        assert!(
            matches!(decision.outcome, Outcome::Error { ref code, .. } if code == "remote_not_enabled")
        );
    }
    // The daemon confirms the ref is gone, once.
    let withdrawn = Command::RemoteWithdrawn {
        workspace_id: "work".into(),
    };
    assert_eq!(decide(&state, &withdrawn, "wd", 32).events.len(), 1);
    ok(run(&mut state, "wd", withdrawn.clone(), 32));
    assert!(state.remote_links["work"].withdrawn);
    assert!(decide(&state, &withdrawn, "wd2", 33).events.is_empty());

    ok(enable(&mut state, "on-again", false));
    let (next, _, changed) = outbox(&mut state, 40);
    assert_eq!(next.sequence, 2, "teammates already hold sequence 1");
    assert!(changed, "the first pass after enabling always publishes");
    assert_eq!(next.work[0].summary, None, "and without the task line now");
}

#[test]
fn one_repository_publishes_through_one_workspace() {
    let mut state = ready(false);
    ok(run(
        &mut state,
        "w-tree",
        Command::WorkspaceRegister {
            workspace_id: "tree".into(),
            root: "/repos/work-tree".into(),
        },
        0,
    ));
    for workspace in ["work", "tree"] {
        ok(run(
            &mut state,
            &format!("g-{workspace}"),
            Command::ObserveWorkspaceGit {
                workspace_id: workspace.into(),
                repository_id: "repo".into(),
                branch_ref: None,
            },
            0,
        ));
    }
    let second = Command::RemoteEnable {
        workspace_id: "tree".into(),
        remote: "origin".into(),
        participant: ME.into(),
        name: "Alice".into(),
        share_task: false,
        publish_as_branch: false,
    };
    let decision = decide(&state, &second, "en2", 1);
    assert!(
        matches!(decision.outcome, Outcome::Error { ref code, .. } if code == "already_published")
    );
    for remote in ["-all", "--upload-pack=x", ".", ".."] {
        let command = Command::RemoteEnable {
            workspace_id: "work".into(),
            remote: remote.into(),
            participant: ME.into(),
            name: "Alice".into(),
            share_task: false,
            publish_as_branch: false,
        };
        assert!(
            matches!(
                decide(&state, &command, "bad", 1).outcome,
                Outcome::Error { .. }
            ),
            "{remote}"
        );
    }
}

#[test]
fn moving_to_the_branch_namespace_is_recorded_and_publishes_at_once() {
    let mut state = ready(false);
    works(
        &mut state,
        "claude-code.alice-session",
        "src/api.rs",
        "t",
        10,
    );
    let (first, signature, _) = outbox(&mut state, 20);
    ok(published(&mut state, "pub-1", first.sequence, &signature));
    assert!(!state.remote_links["work"].publish_as_branch);
    let (_, _, changed) = outbox(&mut state, 21);
    assert!(!changed, "nothing new to publish");

    let as_branch = Command::RemoteEnable {
        workspace_id: "work".into(),
        remote: "origin".into(),
        participant: ME.into(),
        name: "Alice".into(),
        share_task: false,
        publish_as_branch: true,
    };
    let Outcome::RemoteLink { link } = ok(run(&mut state, "as-branch", as_branch.clone(), 30))
    else {
        panic!("enabling returns the link")
    };
    assert!(link.publish_as_branch);
    assert!(state.remote_links["work"].publish_as_branch);
    let (next, _, changed) = outbox(&mut state, 31);
    assert!(
        changed,
        "the ref in the new namespace is written on the next pass"
    );
    assert_eq!(next.sequence, 2, "and the sequence continues");
    // Restating the same choice changes nothing.
    assert!(decide(&state, &as_branch, "again", 32).events.is_empty());
}

#[test]
fn a_withdrawn_publication_removes_the_teammate_and_its_briefing() {
    let mut state = ready(false);
    ok(run(
        &mut state,
        "obs-1",
        Command::ObserveRemote {
            workspace_id: "work".into(),
            publication: theirs(3, "src/a.rs"),
        },
        10,
    ));
    assert_eq!(state.remote_peers.len(), 1);
    let gone = Publication {
        work: Vec::new(),
        withdrawn: true,
        ..theirs(4, "src/a.rs")
    };
    // A withdrawal that still lists work is not believed.
    let lying = Command::ObserveRemote {
        workspace_id: "work".into(),
        publication: Publication {
            withdrawn: true,
            ..theirs(5, "src/a.rs")
        },
    };
    assert!(matches!(
        decide(&state, &lying, "lie", 11).outcome,
        Outcome::Error { .. }
    ));
    // An older withdrawal, a replay, changes nothing.
    let stale = Command::ObserveRemote {
        workspace_id: "work".into(),
        publication: Publication {
            sequence: 2,
            ..gone.clone()
        },
    };
    assert!(decide(&state, &stale, "stale", 11).events.is_empty());
    ok(run(
        &mut state,
        "obs-2",
        Command::ObserveRemote {
            workspace_id: "work".into(),
            publication: gone.clone(),
        },
        12,
    ));
    assert!(
        state.remote_peers.is_empty(),
        "the teammate is shown as gone"
    );
    // Seen again on every later pass: nothing more happens.
    let again = Command::ObserveRemote {
        workspace_id: "work".into(),
        publication: gone,
    };
    assert!(decide(&state, &again, "again", 13).events.is_empty());
}

#[test]
fn a_disabled_workspace_records_its_final_withdrawn_publication() {
    let mut state = ready(false);
    ok(run(
        &mut state,
        "off",
        Command::RemoteDisable {
            workspace_id: "work".into(),
        },
        5,
    ));
    ok(published(&mut state, "final", 1, "withdrawn"));
    assert_eq!(state.remote_links["work"].sequence, 1);
    assert!(
        !state.remote_links["work"].withdrawn,
        "the ref is still there"
    );
    ok(enable(&mut state, "on", false));
    let (next, _, changed) = outbox(&mut state, 6);
    assert_eq!(next.sequence, 2);
    assert!(changed);
}

#[test]
fn a_push_to_the_old_namespace_recorded_after_a_move_does_not_cancel_the_move() {
    // Found on CI: a pass read the link, the owner moved to the branch
    // namespace, and the pass then pushed to the old ref and recorded it.
    // The record restored the signature, so the new ref was never written.
    let mut state = ready(false);
    works(
        &mut state,
        "claude-code.alice-session",
        "src/api.rs",
        "t",
        10,
    );
    let (first, signature, _) = outbox(&mut state, 20);
    ok(published(&mut state, "pub-1", first.sequence, &signature));
    // The pass reads the outbox, still for the default namespace...
    let (stale, stale_signature, _) = outbox(&mut state, 21);
    // ...the owner moves...
    ok(run(
        &mut state,
        "as-branch",
        Command::RemoteEnable {
            workspace_id: "work".into(),
            remote: "origin".into(),
            participant: ME.into(),
            name: "Alice".into(),
            share_task: false,
            publish_as_branch: true,
        },
        30,
    ));
    // ...and the pass records what it pushed to the default namespace.
    ok(run(
        &mut state,
        "pub-2",
        Command::RemotePublished {
            workspace_id: "work".into(),
            sequence: stale.sequence,
            signature: stale_signature,
            payload: "{}".into(),
            published_at_ms: 0,
            as_branch: Some(false),
        },
        31,
    ));
    let (next, _, changed) = outbox(&mut state, 32);
    assert!(changed, "the branch ref is still owed a publication");
    assert_eq!(next.sequence, stale.sequence + 1);
}
