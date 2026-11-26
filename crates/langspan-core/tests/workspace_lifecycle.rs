//! Moving, switching and removing a workspace (roadmap G6): what is kept,
//! what ends, and what refuses the change.
use langspan_core::{
    CheckResult, Command, Event, FileState, ObservedFile, Outcome, SourceOrigin, State,
    TestResults, ToolIdentity, apply, decide,
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

fn error_code(outcome: &Outcome) -> (&str, &str) {
    match outcome {
        Outcome::Error { code, message } => (code, message),
        other => panic!("expected an error, got {other:?}"),
    }
}

fn oid(c: char) -> String {
    std::iter::repeat_n(c, 40).collect()
}

fn present(digest: char) -> FileState {
    FileState::Present {
        digest: std::iter::repeat_n(digest, 64).collect(),
        kind: Default::default(),
    }
}

/// `work` at /repos/work with alice and bob, a note, a message to bob, a
/// check, a landing and an automatic check; `other` at /repos/other.
fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (id, root) in [("work", "/repos/work"), ("other", "/repos/other")] {
        run(
            &mut state,
            &format!("w-{id}"),
            Command::WorkspaceRegister {
                workspace_id: id.into(),
                root: root.into(),
            },
            0,
        );
    }
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
    }
    run(
        &mut state,
        "s-carol",
        Command::SessionRegister {
            session_id: "carol".into(),
            workspace_id: "other".into(),
            parent: None,
            headless: false,
        },
        0,
    );
    assert!(matches!(
        run(
            &mut state,
            "note",
            Command::Remember {
                workspace_id: "work".into(),
                author: None,
                text: "upload keeps its retry budget".into(),
                paths: vec!["src/upload.rs".into()],
            },
            1,
        ),
        Outcome::Remembered { .. }
    ));
    assert!(matches!(
        run(
            &mut state,
            "message",
            Command::SendMessage {
                workspace_id: "work".into(),
                from: Some("alice".into()),
                to: Some("bob".into()),
                text: "I am changing upload".into(),
                paths: Vec::new(),
            },
            2,
        ),
        Outcome::MessageSent { .. }
    ));
    let Outcome::CheckRecorded { check } = run(
        &mut state,
        "check",
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "test".into(),
            command: "cargo test".into(),
            tree_oid: oid('1'),
            head_oid: Some(oid('2')),
            inputs: Vec::new(),
            result: CheckResult::Passed {},
            duration_ms: 10,
            report: None,
            tests: None,
            rerun: None,
            writes_before: None,
            command_id: None,
        },
        3,
    ) else {
        panic!("check recorded")
    };
    assert!(matches!(
        run(
            &mut state,
            "landing",
            Command::RecordLanding {
                workspace_id: "work".into(),
                onto: "main".into(),
                commit_oid: oid('3'),
                tree_oid: oid('1'),
                inputs: Vec::new(),
                check_id: check.check_id,
            },
            4,
        ),
        Outcome::Landed { .. }
    ));
    assert!(matches!(
        run(
            &mut state,
            "auto",
            Command::AutoCheckEnable {
                workspace_id: "work".into(),
                name: "test".into(),
                argv: vec!["cargo".into(), "test".into()],
                timeout_s: 60,
            },
            5,
        ),
        Outcome::AutoCheck { .. }
    ));
    state
}

fn move_to(root: &str, switch: bool) -> Command {
    Command::WorkspaceMove {
        workspace_id: "work".into(),
        root: root.into(),
        switch,
    }
}

fn remove(apply: bool) -> Command {
    Command::WorkspaceRemove {
        workspace_id: "work".into(),
        apply,
    }
}

#[test]
fn list_shows_each_workspace_with_what_it_holds() {
    let mut state = ready();
    let Outcome::Workspaces { workspaces, next } = run(
        &mut state,
        "read",
        Command::WorkspaceList { after: None },
        10,
    ) else {
        panic!("a list")
    };
    assert_eq!(next, None, "one page holds both");
    let ids: Vec<&str> = workspaces.iter().map(|w| w.workspace_id.as_str()).collect();
    assert_eq!(ids, ["other", "work"]);
    let work = &workspaces[1];
    assert_eq!(work.root, "/repos/work");
    assert_eq!(work.sessions_current, 2);
    assert_eq!(work.notes, 1);
    assert_eq!(work.messages_pending, 1);
    assert_eq!(work.checks, 1);
    assert!(work.auto_check);
    assert!(!work.remote_sharing);
    // Only the daemon can look at the filesystem.
    assert!(!work.root_exists);
    let other = &workspaces[0];
    assert_eq!((other.sessions_current, other.notes), (1, 0));
}

#[test]
fn a_move_rebinds_the_root_and_keeps_notes_messages_checks_and_landings() {
    let mut state = ready();
    run(
        &mut state,
        "read-a",
        Command::ObserveSource {
            session_id: "alice".into(),
            files: vec![ObservedFile {
                path: "src/upload.rs".into(),
                state: present('a'),
            }],
            origin: SourceOrigin::Read,
        },
        6,
    );
    assert!(!state.activity["alice"].known.is_empty());
    let Outcome::WorkspaceMoved {
        workspace,
        from_root,
        kept,
    } = run(&mut state, "move-1", move_to("/work/app", false), 10)
    else {
        panic!("moved")
    };
    assert_eq!(workspace.root, "/work/app");
    assert_eq!(from_root, "/repos/work");
    assert_eq!(
        (
            kept.notes,
            kept.messages_pending,
            kept.checks,
            kept.landings
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(state.workspaces["work"].root, "/work/app");
    assert!(state.auto_checks.contains_key("work"));
    // Versions read under the old root say nothing about the new one.
    assert!(state.activity["alice"].known.is_empty());

    let Outcome::Recap { recap } = run(
        &mut state,
        "read",
        Command::Recap {
            workspace_id: "work".into(),
        },
        11,
    ) else {
        panic!("recap")
    };
    assert_eq!(recap.root, "/work/app");
    assert_eq!(recap.notes.len(), 1);
    assert_eq!(recap.checks.len(), 1);
    assert_eq!(recap.landings.len(), 1);

    // The ID now registers at the new root; the old root is someone else's.
    assert!(matches!(
        run(
            &mut state,
            "again",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/work/app".into(),
            },
            12,
        ),
        Outcome::Workspace { .. }
    ));
    let refused = run(
        &mut state,
        "old",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        12,
    );
    assert_eq!(error_code(&refused).0, "workspace_conflict");
}

#[test]
fn a_move_onto_another_workspace_or_its_own_root_is_refused_and_changes_nothing() {
    let mut state = ready();
    let before = state.clone();
    for (id, root) in [
        ("m1", "/repos/other"),
        ("m2", "/repos/other/sub"),
        ("m3", "/repos"),
    ] {
        let outcome = run(&mut state, id, move_to(root, false), 10);
        assert_eq!(error_code(&outcome).0, "workspace_conflict", "{root}");
    }
    let outcome = run(&mut state, "m4", move_to("/repos/work", true), 10);
    assert_eq!(error_code(&outcome).0, "invalid_move");
    let outcome = run(
        &mut state,
        "m5",
        Command::WorkspaceMove {
            workspace_id: "nobody".into(),
            root: "/x".into(),
            switch: false,
        },
        10,
    );
    assert_eq!(error_code(&outcome).0, "workspace_not_found");
    assert_eq!(state, before);
}

#[test]
fn a_dry_run_removal_changes_nothing_and_lists_what_would_end() {
    let mut state = ready();
    run(
        &mut state,
        "intent",
        Command::IntentSet {
            session_id: "alice".into(),
            paths: vec!["src/upload.rs".into()],
            summary: "retry budget".into(),
        },
        6,
    );
    let before = state.clone();
    let decision = decide(&state, &remove(false), "dry", 10);
    assert!(decision.events.is_empty());
    let Outcome::WorkspaceRemoval {
        workspace_id,
        applied,
        ending,
    } = decision.outcome
    else {
        panic!("a removal preview: {:?}", decision.outcome)
    };
    assert_eq!(workspace_id, "work");
    assert!(!applied);
    assert_eq!(ending.sessions, ["alice", "bob"]);
    assert_eq!(
        (ending.intents, ending.notes, ending.messages_pending),
        (1, 1, 1)
    );
    assert_eq!(ending.auto_check.as_deref(), Some("test"));
    assert_eq!(state, before);
}

fn admit(state: &mut State, now: u64) -> String {
    let Outcome::Admitted { lease } = run(
        state,
        "admit",
        Command::AdmitTool {
            session_id: "alice".into(),
            paths: vec!["src/upload.rs".into()],
            ttl_ms: 1_000,
            operation: ToolIdentity {
                operation_id: "op".into(),
                source: "test".into(),
                tool: "Edit".into(),
                input_digest: "a".repeat(64),
            },
            base: Vec::new(),
            regions: Vec::new(),
            overwrites: Vec::new(),
        },
        now,
    ) else {
        panic!("admitted")
    };
    lease.lease_id
}

#[test]
fn a_held_admission_refuses_removal_and_moves_as_workspace_busy() {
    let mut state = ready();
    let lease = admit(&mut state, 10);
    let before = state.clone();
    for (id, command) in [
        ("r", remove(true)),
        ("d", remove(false)),
        ("m", move_to("/work/app", false)),
        ("s", move_to("/work/app", true)),
    ] {
        let outcome = run(&mut state, id, command, 20);
        let (code, message) = error_code(&outcome);
        assert_eq!(code, "workspace_busy");
        assert!(message.contains("alice"), "{message}");
    }
    assert_eq!(state, before);
    run(
        &mut state,
        "done",
        Command::Complete {
            session_id: "alice".into(),
            lease_id: lease,
        },
        21,
    );
    assert!(matches!(
        run(&mut state, "r2", remove(true), 22),
        Outcome::WorkspaceRemoval { applied: true, .. }
    ));
}

#[test]
fn an_expired_admission_does_not_hold_the_workspace() {
    let mut state = ready();
    admit(&mut state, 10);
    assert!(matches!(
        run(&mut state, "r", remove(true), 10 + 1_000),
        Outcome::WorkspaceRemoval { applied: true, .. }
    ));
}

#[test]
fn a_landing_holding_its_push_turn_refuses_removal() {
    let mut state = ready();
    assert!(matches!(
        run(
            &mut state,
            "turn",
            Command::TakeTurn {
                workspace_id: "work".into(),
                onto: "main".into(),
                session_id: None,
                ttl_ms: 1000,
                turn_id: None,
                queue: None,
            },
            10,
        ),
        Outcome::Turn { .. }
    ));
    let outcome = run(&mut state, "r", remove(true), 11);
    let (code, message) = error_code(&outcome);
    assert_eq!(code, "workspace_busy");
    assert!(message.contains("main"), "{message}");
}

#[test]
fn remote_sharing_refuses_removal_until_disabled_and_withdrawn() {
    let mut state = ready();
    run(
        &mut state,
        "remote",
        Command::RemoteEnable {
            workspace_id: "work".into(),
            remote: "origin".into(),
            participant: "0123456789abcdef0123456789abcdef".into(),
            name: "Alice".into(),
            share_task: false,
            publish_as_branch: false,
        },
        10,
    );
    for (id, command) in [("r", remove(true)), ("m", move_to("/work/app", false))] {
        let outcome = run(&mut state, id, command, 11);
        let (code, message) = error_code(&outcome);
        assert_eq!(code, "remote_sharing_on");
        assert!(
            message.contains("langspan remote disable work"),
            "{message}"
        );
    }
    run(
        &mut state,
        "off",
        Command::RemoteDisable {
            workspace_id: "work".into(),
        },
        12,
    );
    // The published ref is not yet gone: removing now would leave it there,
    // and moving now would send the withdrawal to the new root's remote,
    // which may not be the one the ref was published to.
    for (id, command) in [("r2", remove(true)), ("m2", move_to("/work/app", false))] {
        let outcome = run(&mut state, id, command, 13);
        let (code, message) = error_code(&outcome);
        assert_eq!(code, "remote_sharing_on", "{id}");
        assert!(message.contains("not yet removed"), "{message}");
    }
    run(
        &mut state,
        "gone",
        Command::RemoteWithdrawn {
            workspace_id: "work".into(),
        },
        14,
    );
    assert!(matches!(
        run(&mut state, "r3", remove(true), 15),
        Outcome::WorkspaceRemoval { applied: true, .. }
    ));
}

#[test]
fn a_removed_workspace_forgets_what_its_tests_taught() {
    let mut state = ready();
    let writes = state.writes.get("work").copied().unwrap_or(0);
    let recorded = run(
        &mut state,
        "tests",
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "tests".into(),
            command: "cargo test".into(),
            tree_oid: oid('7'),
            head_oid: None,
            inputs: Vec::new(),
            result: CheckResult::Passed {},
            duration_ms: 10,
            report: None,
            tests: Some(TestResults {
                all_passed: true,
                passed: vec!["net::times_out".into()],
                ..TestResults::default()
            }),
            rerun: None,
            writes_before: Some(writes),
            command_id: Some("c".repeat(64)),
        },
        5,
    );
    assert!(
        matches!(recorded, Outcome::CheckRecorded { .. }),
        "{recorded:?}"
    );
    assert!(state.test_learning.contains_key("work"));
    run(&mut state, "rm", remove(true), 10);
    // An ID registered again is a new workspace: its tests start unlearned.
    assert!(!state.test_learning.contains_key("work"));
}

#[test]
fn a_removed_workspace_leaves_status_and_briefings_and_its_id_can_start_again() {
    let mut state = ready();
    let decision = decide(&state, &remove(true), "rm", 10);
    assert_eq!(
        decision.events,
        [Event::WorkspaceRemoved {
            workspace_id: "work".into()
        }]
    );
    run(&mut state, "rm", remove(true), 10);
    assert!(!state.workspaces.contains_key("work"));
    assert!(state.sessions.keys().all(|id| id == "carol"));
    for map_has_work in [
        state.notes.contains_key("work"),
        state.messages.contains_key("work"),
        state.checks.contains_key("work"),
        state.landings.contains_key("work"),
        state.auto_checks.contains_key("work"),
        state.activity.contains_key("alice"),
    ] {
        assert!(!map_has_work);
    }
    let Outcome::Status { state: view, .. } = run(&mut state, "read", Command::Status, 11) else {
        panic!("status")
    };
    assert!(!view.workspaces.contains_key("work"));
    assert!(view.sessions.values().all(|s| s.workspace_id != "work"));
    let briefed = run(
        &mut state,
        "brief",
        Command::Brief {
            session_id: "alice".into(),
            max_peers: 8,
            deliver: true,
            mid_turn: false,
        },
        11,
    );
    assert_eq!(error_code(&briefed).0, "session_not_found");
    let Outcome::Briefing { briefing } = run(
        &mut state,
        "brief-carol",
        Command::Brief {
            session_id: "carol".into(),
            max_peers: 8,
            deliver: false,
            mid_turn: false,
        },
        11,
    ) else {
        panic!("carol is briefed")
    };
    assert!(briefing.peers.is_empty());
    // A second removal has nothing to remove.
    assert_eq!(
        error_code(&run(&mut state, "rm-2", remove(true), 12)).0,
        "workspace_not_found"
    );
    // Registered again, the ID starts with nothing.
    run(
        &mut state,
        "w-again",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/elsewhere/work".into(),
        },
        13,
    );
    assert!(!state.notes.contains_key("work"));
}

/// A withdrawn message is in the workspace's message history and, until
/// delivered, in each reader's withdrawal notices. Removal clears both, so
/// the dashboard and recap stop showing it, and the ID or a session ID
/// registered again does not bring the old withdrawal back.
#[test]
fn a_removed_workspace_forgets_its_message_history_and_withdrawal_notices() {
    let mut state = ready();
    let brief = |session: &str| Command::Brief {
        session_id: session.into(),
        max_peers: 8,
        deliver: true,
        mid_turn: false,
    };
    run(&mut state, "brief-bob", brief("bob"), 3);
    let Outcome::MessageRetracted { .. } = run(
        &mut state,
        "retract",
        Command::RetractMessage {
            workspace_id: "work".into(),
            from: Some("alice".into()),
            message_id: Some("message".into()),
        },
        4,
    ) else {
        panic!("the message is withdrawn")
    };
    assert!(state.message_history.contains_key("work"));
    assert!(state.retracted_notices.contains_key("bob"));
    run(&mut state, "rm", remove(true), 10);
    assert!(!state.message_history.contains_key("work"));
    assert!(
        state
            .retracted_notices
            .values()
            .flatten()
            .all(|notice| notice.workspace_id != "work"),
        "{:?}",
        state.retracted_notices
    );
    // The same IDs registered again start with no withdrawal to deliver.
    run(
        &mut state,
        "w-again",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        11,
    );
    run(
        &mut state,
        "s-bob-again",
        Command::SessionRegister {
            session_id: "bob".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        11,
    );
    let Outcome::Briefing { briefing } = run(&mut state, "brief-bob-2", brief("bob"), 12) else {
        panic!("bob is briefed")
    };
    let text = serde_json::to_string(&briefing).unwrap();
    assert!(!text.contains("I am changing upload"), "{text}");
}

/// An uncoordinated change describes the files of one checkout on disk, as
/// a checkout change does: a move to another checkout and a removal each
/// drop it, so no session is told of the old checkout's files.
#[test]
fn a_move_or_removal_drops_the_old_checkouts_uncoordinated_changes() {
    for command in [move_to("/repos/moved", true), remove(true)] {
        let mut state = ready();
        apply(
            &mut state,
            &Event::UncoordinatedChanged {
                change: langspan_core::UncoordinatedChange {
                    change_id: "unreported".into(),
                    workspace_id: "work".into(),
                    sessions: vec!["alice".into()],
                    files: vec![ObservedFile {
                        path: "src/upload.rs".into(),
                        state: present('a'),
                    }],
                    at_ms: 3,
                    generation: 1,
                    delivered: Vec::new(),
                },
            },
        )
        .unwrap();
        assert!(state.uncoordinated.contains_key("work"));
        let outcome = run(&mut state, "change", command.clone(), 10);
        assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
        assert!(!state.uncoordinated.contains_key("work"), "{command:?}");
    }
}

/// A session's presence setting (roadmap A13) ends with its workspace, as
/// its activity does: a session registered again under the same ID shows
/// its reads by default, not as the removed session chose.
#[test]
fn a_removed_workspace_forgets_its_sessions_presence_settings() {
    let mut state = ready();
    let outcome = run(
        &mut state,
        "presence",
        Command::SetPresence {
            session_id: "alice".into(),
            shown: false,
            window_ms: langspan_core::PRESENCE_WINDOW_MS,
        },
        6,
    );
    assert!(matches!(outcome, Outcome::Presence { .. }), "{outcome:?}");
    assert!(state.presence.contains_key("alice"));
    run(&mut state, "rm", remove(true), 10);
    assert!(
        !state.presence.contains_key("alice"),
        "{:?}",
        state.presence
    );
}

#[test]
fn replaying_the_log_reproduces_moves_and_removals() {
    let mut state = State::default();
    let mut log = vec![Event::Booted { generation: 1 }];
    apply(&mut state, &log[0]).unwrap();
    let steps = [
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        Command::SessionRegister {
            session_id: "alice".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        move_to("/work/app", false),
        remove(true),
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
    ];
    for (index, command) in steps.iter().enumerate() {
        let decision = decide(&state, command, &format!("step-{index}"), index as u64);
        assert!(
            !matches!(decision.outcome, Outcome::Error { .. }),
            "{decision:?}"
        );
        for event in decision.events {
            apply(&mut state, &event).unwrap();
            log.push(event);
        }
    }
    let json: Vec<String> = log
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect();
    assert!(
        json.iter()
            .any(|j| j.contains(r#""type":"workspace_moved""#))
    );
    assert!(
        json.iter()
            .any(|j| j.contains(r#""type":"workspace_removed""#))
    );
    let mut replayed = State::default();
    for line in json {
        apply(&mut replayed, &serde_json::from_str(&line).unwrap()).unwrap();
    }
    assert_eq!(replayed, state);
}

/// Pages hold at most `MAX_WORKSPACE_PAGE` workspaces in ID order; the
/// cursor is the last one's ID, and a cursor whose workspace was removed
/// since still continues after it.
#[test]
fn the_list_pages_in_id_order_from_a_cursor() {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    let total = langspan_core::MAX_WORKSPACE_PAGE * 2 + 5;
    let ids: Vec<String> = (0..total).map(|n| format!("w{n:04}")).collect();
    for id in &ids {
        run(
            &mut state,
            &format!("add-{id}"),
            Command::WorkspaceRegister {
                workspace_id: id.clone(),
                root: format!("/repos/{id}"),
            },
            0,
        );
    }
    let page = |state: &mut State, after: Option<&str>| match run(
        state,
        "read",
        Command::WorkspaceList {
            after: after.map(str::to_owned),
        },
        10,
    ) {
        Outcome::Workspaces { workspaces, next } => (
            workspaces
                .into_iter()
                .map(|w| w.workspace_id)
                .collect::<Vec<_>>(),
            next,
        ),
        other => panic!("{other:?}"),
    };
    let (mut seen, mut after, mut pages) = (Vec::new(), None, 0);
    loop {
        let (listed, next) = page(&mut state, after.as_deref());
        assert!(listed.len() <= langspan_core::MAX_WORKSPACE_PAGE);
        assert_eq!(next.as_ref(), next.as_ref().and(listed.last()));
        seen.extend(listed);
        pages += 1;
        match next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, ids);

    // The cursor's own workspace is removed: the next page is unchanged.
    let (first, cursor) = page(&mut state, None);
    let cursor = cursor.unwrap();
    assert_eq!(first.last(), Some(&cursor));
    let (expected, _) = page(&mut state, Some(&cursor));
    run(
        &mut state,
        "rm",
        Command::WorkspaceRemove {
            workspace_id: cursor.clone(),
            apply: true,
        },
        20,
    );
    assert_eq!(page(&mut state, Some(&cursor)).0, expected);
    // Past the last: an empty page, no cursor.
    assert_eq!(page(&mut state, Some("z")), (Vec::new(), None));
    // Not a workspace ID: refused.
    let refused = run(
        &mut state,
        "read",
        Command::WorkspaceList {
            after: Some("../x".into()),
        },
        30,
    );
    assert_eq!(error_code(&refused).0, "invalid_id", "{refused:?}");
}

#[test]
fn the_wire_forms_are_strict() {
    for (json, ok) in [
        (r#"{"op":"workspace_list"}"#, true),
        (r#"{"op":"workspace_list","after":"w"}"#, true),
        (r#"{"op":"workspace_list","x":1}"#, false),
        (
            r#"{"op":"workspace_move","workspace_id":"w","root":"/r"}"#,
            true,
        ),
        (
            r#"{"op":"workspace_move","workspace_id":"w","root":"/r","switch":true}"#,
            true,
        ),
        (
            r#"{"op":"workspace_move","workspace_id":"w","root":"/r","verified_overlaps":[]}"#,
            false,
        ),
        (r#"{"op":"workspace_remove","workspace_id":"w"}"#, true),
        (
            r#"{"op":"workspace_remove","workspace_id":"w","apply":true}"#,
            true,
        ),
    ] {
        assert_eq!(serde_json::from_str::<Command>(json).is_ok(), ok, "{json}");
    }
}
