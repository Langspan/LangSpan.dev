//! The dashboard view: every session on every machine heard from, what is
//! held and what overlaps, bounded to one transport frame, and read-only.
use langspan_core::{
    AgentState, Applicability, CheckResult, Command, DASHBOARD_AGENTS, DASHBOARD_ITEMS, Dashboard,
    Event, Message, Note, Outcome, Publication, REMOTE_HORIZON_MS, Recipient, RemotePeer,
    RemoteWork, SessionActivity, State, ToolResult, TouchedPath, apply, decide,
};

const THEM: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let decision = decide(state, &command, id, now);
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    assert!(
        !matches!(decision.outcome, Outcome::Error { .. }),
        "{:?}",
        decision.outcome
    );
    decision.outcome
}

fn view(state: &State, now: u64) -> Dashboard {
    let before = state.clone();
    let decision = decide(state, &Command::Dashboard, "read", now);
    assert!(
        decision.events.is_empty(),
        "a dashboard read records nothing"
    );
    assert_eq!(*state, before, "a dashboard read changes nothing");
    match decision.outcome {
        Outcome::Dashboard { dashboard } => dashboard,
        other => panic!("{other:?}"),
    }
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
    for session in ["claude-code.alice-1234567890", "codex.bob-0987654321"] {
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
    state
}

fn edited(state: &mut State, session: &str, path: &str, now: u64) {
    run(
        state,
        &format!("o-{session}-{}-{now}", path.replace('/', "_")),
        Command::Observe {
            session_id: session.into(),
            operation_id: format!("op-{}-{now}", path.replace('/', "_")),
            source: "test".into(),
            tool: "Edit".into(),
            paths: vec![path.into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        now,
    );
}

#[test]
fn every_local_and_remote_session_is_listed_with_current_work_first() {
    let mut state = ready();
    let alice = "claude-code.alice-1234567890";
    let bob = "codex.bob-0987654321";
    run(
        &mut state,
        "task",
        Command::SessionPrompt {
            session_id: alice.into(),
            summary: "Add retry to the upload client".into(),
        },
        1_000,
    );
    edited(&mut state, alice, "src/upload.rs", 2_000);
    run(
        &mut state,
        "hold",
        Command::Admit {
            session_id: bob.into(),
            paths: vec!["src/gate.rs".into()],
            ttl_ms: 30_000,
        },
        5_000,
    );
    run(
        &mut state,
        "remote",
        Command::RemoteEnable {
            workspace_id: "work".into(),
            remote: "origin".into(),
            participant: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "Alice".into(),
            share_task: false,
            publish_as_branch: false,
        },
        0,
    );
    run(
        &mut state,
        "heard",
        Command::ObserveRemote {
            workspace_id: "work".into(),
            publication: Publication {
                participant: THEM.into(),
                name: "Carol".into(),
                sequence: 1,
                work: vec![RemoteWork {
                    host: "codex".into(),
                    session: "c0ffee00".into(),
                    branch_ref: Some("refs/heads/retry".into()),
                    summary: Some("Rework uploads".into()),
                    editing: vec![],
                    recent: vec!["src/upload.rs".into()],
                }],
                withdrawn: false,
            },
        },
        4_000,
    );

    let now = 6_000;
    let d = view(&state, now);
    assert_eq!(d.generation, 1);
    assert_eq!(d.uptime_ms, None, "the daemon fills uptime");
    assert_eq!(d.denials, None, "the store fills denials");
    let order: Vec<_> = d
        .agents
        .iter()
        .map(|row| {
            (
                row.state,
                row.host.as_str(),
                row.session.as_str(),
                row.local,
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            (AgentState::Editing, "codex", "bob-0987", true),
            (AgentState::Active, "claude-code", "alice-12", true),
            (AgentState::Published, "codex", "c0ffee00", false),
        ]
    );
    assert_eq!(d.active_agents, 3);
    assert_eq!((d.teammates_heard, d.teammates_known), (1, 1));
    let alice_row = &d.agents[1];
    assert_eq!(
        alice_row.task.as_deref(),
        Some("Add retry to the upload client")
    );
    assert_eq!(alice_row.recent[0].path, "src/upload.rs");
    assert_eq!(alice_row.recent[0].age_ms, Some(4_000));
    assert_eq!(alice_row.session_id.as_deref(), Some(alice));
    let carol = &d.agents[2];
    assert_eq!(carol.machine.as_deref(), Some("Carol"));
    assert_eq!(carol.heard_age_ms, Some(2_000));
    assert_eq!(carol.recent[0].age_ms, None, "a teammate's file has no age");

    assert_eq!(d.editing.len(), 1);
    assert_eq!(d.editing[0].path, "src/gate.rs");
    assert_eq!(d.editing[0].expires_in_ms, 29_000);

    // Alice here and Carol on her machine both touched src/upload.rs.
    assert_eq!(d.overlaps.len(), 1);
    assert_eq!(d.overlaps[0].path, "src/upload.rs");
    assert_eq!(
        d.overlaps[0].agents,
        ["Carol: codex c0ffee00", "claude-code alice-12"]
    );

    let workspace = &d.workspaces[0];
    assert_eq!((workspace.sessions, workspace.teammates), (2, 1));
    assert!(workspace.publishing.as_ref().unwrap().enabled);
    assert_eq!(workspace.coverage, None, "the daemon fills coverage");

    // Past the horizon, the teammate is no longer heard and no longer counts.
    let later = view(&state, 4_000 + REMOTE_HORIZON_MS + 1);
    let carol = later.agents.iter().find(|row| !row.local).unwrap();
    assert_eq!(carol.state, AgentState::Unheard);
    assert_eq!(later.teammates_heard, 0);
    assert!(later.overlaps.is_empty());

    // After a restart, sessions belong to an earlier run and times are not
    // compared across runs.
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    let restarted = view(&state, 10);
    assert!(
        restarted
            .agents
            .iter()
            .all(|row| matches!(row.state, AgentState::EarlierRun | AgentState::Unheard))
    );
    assert!(restarted.agents.iter().all(|row| row.heard_age_ms.is_none()
        && row.active_age_ms.is_none()
        && row.recent.iter().all(|file| file.age_ms.is_none())));
    assert_eq!(restarted.active_agents, 0);
    assert!(restarted.editing.is_empty());
}

#[test]
fn evidence_and_risks_are_carried_with_their_applicability() {
    let mut state = ready();
    let alice = "claude-code.alice-1234567890";
    run(
        &mut state,
        "check",
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "test".into(),
            command: "cargo test".into(),
            tree_oid: "a".repeat(40),
            head_oid: None,
            inputs: vec![],
            result: CheckResult::Failed { exit_code: 101 },
            duration_ms: 50,
            report: None,
            tests: None,
            rerun: None,
            writes_before: None,
            command_id: None,
        },
        100,
    );
    run(
        &mut state,
        "note",
        Command::Remember {
            workspace_id: "work".into(),
            author: None,
            text: "Release freeze until Friday".into(),
            paths: vec![],
        },
        100,
    );
    run(
        &mut state,
        "handoff",
        Command::Handoff {
            session_id: alice.into(),
            text: "Finish the retry tests".into(),
            paths: vec!["src/upload.rs".into()],
        },
        100,
    );
    run(
        &mut state,
        "notice",
        Command::ObserveInterface {
            workspace_id: "work".into(),
            path: "src/api.rs".into(),
            changes: vec![langspan_core::InterfaceChange {
                name: "upload".into(),
                kind: "function".into(),
                change: "signature_changed".into(),
                before: Some("pub fn upload()".into()),
                after: Some("pub fn upload(retries: u8)".into()),
                before_arity: None,
                after_arity: None,
                before_count: None,
                after_count: None,
            }],
            dependents: vec!["src/main.rs".into()],
        },
        200,
    );
    run(
        &mut state,
        "scan",
        Command::ObserveScan {
            workspace_id: "work".into(),
            failure: Some("git status timed out".into()),
            coverage: None,
        },
        300,
    );
    let d = view(&state, 1_000);
    assert_eq!(d.checks.len(), 1);
    assert_eq!(d.checks[0].result, CheckResult::Failed { exit_code: 101 });
    assert_eq!(d.checks[0].applies, Applicability::Unknown);
    assert_eq!(d.checks[0].age_ms, Some(900));
    assert_eq!(d.notes[0].text, "Release freeze until Friday");
    assert_eq!(d.messages[0].to, Recipient::NextSession {});
    assert_eq!(d.messages[0].from.as_deref(), Some(alice));
    assert_eq!(d.interface_changes[0].path, "src/api.rs");
    assert_eq!(d.interface_changes[0].dependents, ["src/main.rs"]);
    assert_eq!(
        d.workspaces[0].scan_failure.as_ref().unwrap().detail,
        "git status timed out"
    );
}

/// The largest state the core accepts in every listed respect still fits
/// one 64 KiB frame, with room for the daemon's additions.
#[test]
fn a_crowded_state_is_bounded_with_counts_of_what_was_left_out() {
    let mut state = ready();
    let quote_path = |n: usize| format!("dir\"{n}/{}", "\"\\x".repeat(160));
    for n in 0..120 {
        let session_id = format!("claude-code.{n:04}-{}", "s".repeat(100));
        state.sessions.insert(
            session_id.clone(),
            langspan_core::Session {
                session_id: session_id.clone(),
                workspace_id: "work".into(),
                generation: 1,
                parent: None,
                headless: false,
            },
        );
        state.activity.insert(
            session_id.clone(),
            SessionActivity {
                session_id: session_id.clone(),
                generation: 1,
                last_ms: 10,
                summary: Some("\"<task>\" ".repeat(50)),
                touched: (0..32)
                    .map(|i| TouchedPath {
                        path: quote_path(i),
                        at_ms: 5,
                    })
                    .collect(),
                ..SessionActivity::default()
            },
        );
    }
    for n in 0..30 {
        let participant = format!("{n:032x}");
        state.remote_peers.insert(
            format!("work:{participant}"),
            RemotePeer {
                workspace_id: "work".into(),
                publication: Publication {
                    participant,
                    name: "N".repeat(64),
                    sequence: 1,
                    work: (0..8)
                        .map(|i| RemoteWork {
                            host: "codex".into(),
                            session: format!("{i:08}"),
                            branch_ref: Some(format!("refs/heads/{}", "b".repeat(400))),
                            summary: Some("t".repeat(512)),
                            editing: (0..8).map(quote_path).collect(),
                            recent: (8..16).map(quote_path).collect(),
                        })
                        .collect(),
                    withdrawn: false,
                },
                received_ms: 5,
                generation: 1,
            },
        );
    }
    state.notes.insert(
        "work".into(),
        (0..64)
            .map(|n| Note {
                note_id: format!("note-{n}"),
                workspace_id: "work".into(),
                author: None,
                text: "\"".repeat(400),
                paths: (0..8).map(quote_path).collect(),
            })
            .collect(),
    );
    state.messages.insert(
        "work".into(),
        (0..64)
            .map(|n| Message {
                message_id: format!("message-{n}"),
                workspace_id: "work".into(),
                from: None,
                to: Recipient::Everyone {},
                text: "\\".repeat(400),
                paths: (0..8).map(quote_path).collect(),
                acknowledged: vec![],
            })
            .collect(),
    );
    let d = view(&state, 20);
    assert!(d.agents.len() <= DASHBOARD_AGENTS);
    assert_eq!(
        d.agents.len() as u32 + d.more_agents,
        2 + 120 + 30 * 8,
        "every agent is either shown or counted"
    );
    assert!(d.more_agents > 0);
    assert!(d.notes.len() <= DASHBOARD_ITEMS && d.more_notes > 0);
    assert!(d.messages.len() <= DASHBOARD_ITEMS && d.more_messages > 0);
    assert!(d.overlaps.len() <= DASHBOARD_ITEMS && d.more_overlaps > 0);
    let bytes = serde_json::to_vec(&Outcome::Dashboard { dashboard: d }).unwrap();
    assert!(bytes.len() < 48 * 1024, "{} bytes", bytes.len());
}

#[test]
fn a_parent_and_its_subagents_on_one_file_are_an_overlap_with_distinct_labels() {
    let mut state = ready();
    let parent = "claude-code.alice-1234567890";
    let first = "claude-code.alice-1234567890.agent-one";
    let second = "claude-code.alice-1234567890.agent-two";
    for child in [first, second] {
        run(
            &mut state,
            &format!("s-{child}"),
            Command::SessionRegister {
                session_id: child.into(),
                workspace_id: "work".into(),
                parent: Some(parent.into()),
                headless: false,
            },
            0,
        );
    }
    // The parent and one subagent share a file; two sibling subagents another.
    edited(&mut state, parent, "src/upload.rs", 1_000);
    edited(&mut state, first, "src/upload.rs", 1_000);
    edited(&mut state, first, "src/gate.rs", 1_000);
    edited(&mut state, second, "src/gate.rs", 1_000);
    let d = view(&state, 2_000);
    let mut overlaps: Vec<_> = d
        .overlaps
        .iter()
        .map(|row| (row.path.as_str(), row.agents.clone()))
        .collect();
    overlaps.sort();
    assert_eq!(
        overlaps,
        [
            (
                "src/gate.rs",
                vec![
                    "claude-code alice-12 subagent agent-on".to_owned(),
                    "claude-code alice-12 subagent agent-tw".to_owned(),
                ]
            ),
            (
                "src/upload.rs",
                vec![
                    "claude-code alice-12".to_owned(),
                    "claude-code alice-12 subagent agent-on".to_owned(),
                ]
            ),
        ]
    );
}
