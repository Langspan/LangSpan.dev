//! The dashboard view: every session on every machine heard from, what is
//! held and what overlaps, bounded to one transport frame, and read-only.
use langspan_core::{
    AgentState, Applicability, CheckResult, Command, DASHBOARD_AGENTS, DASHBOARD_ITEMS, Dashboard,
    Event, Message, Note, Outcome, Publication, REMOTE_HORIZON_MS, Recipient, RemotePeer,
    RemoteWork, SessionActivity, State, ToolResult, TouchedPath, apply, decide,
};

use std::collections::{BTreeMap, BTreeSet};

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
            line_digests: Vec::new(),
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
fn a_withdrawn_message_is_shown_as_withdrawn() {
    let mut state = ready();
    let alice = "claude-code.alice-1234567890";
    for id in ["h1", "h2"] {
        run(
            &mut state,
            id,
            Command::Handoff {
                session_id: alice.into(),
                text: id.into(),
                paths: vec![],
            },
            100,
        );
    }
    run(
        &mut state,
        "r1",
        Command::RetractMessage {
            workspace_id: "work".into(),
            from: Some(alice.into()),
            message_id: Some("h1".into()),
        },
        100,
    );
    let d = view(&state, 200);
    let shown: Vec<(&str, bool)> = d
        .messages
        .iter()
        .map(|row| (row.text.as_str(), row.retracted))
        .collect();
    assert_eq!(shown, [("h2", false), ("h1", true)]);
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
            symbol_findings: None,
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
            symbol: None,
            tags: Vec::new(),
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
                after_name: None,
                before_name: None,
                evidence: None,
                before_path: None,
                private: false,
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
                anchor: None,
                anchor_state: None,
                tags: Vec::new(),
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
                retracted: false,
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

fn register(state: &mut State, session: &str, workspace: &str) {
    run(
        state,
        &format!("s-{session}"),
        Command::SessionRegister {
            session_id: session.into(),
            workspace_id: workspace.into(),
            parent: None,
            headless: false,
        },
        0,
    );
}

fn workspace(state: &mut State, workspace: &str, repository: &str) {
    run(
        state,
        &format!("w-{workspace}"),
        Command::WorkspaceRegister {
            workspace_id: workspace.into(),
            root: format!("/repos/{workspace}"),
        },
        0,
    );
    run(
        state,
        &format!("g-{workspace}"),
        Command::ObserveWorkspaceGit {
            workspace_id: workspace.into(),
            repository_id: repository.into(),
            branch_ref: Some(format!("refs/heads/{workspace}")),
        },
        0,
    );
}

fn heard(state: &mut State, participant: &str, name: &str, session: &str, recent: &str, now: u64) {
    if !state.remote_links.contains_key("work") {
        run(
            state,
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
    }
    run(
        state,
        &format!("heard-{participant}-{now}"),
        Command::ObserveRemote {
            workspace_id: "work".into(),
            publication: Publication {
                participant: participant.into(),
                name: name.into(),
                sequence: now,
                work: vec![RemoteWork {
                    host: "codex".into(),
                    session: session.into(),
                    branch_ref: None,
                    summary: None,
                    editing: vec![],
                    recent: vec![recent.into()],
                }],
                withdrawn: false,
            },
        },
        now,
    );
}

/// The agent row an overlap member names, by its stable ID.
fn row_of<'a>(d: &'a Dashboard, id: &str) -> Vec<&'a langspan_core::AgentRow> {
    d.agents
        .iter()
        .filter(|row| row.id.as_deref() == Some(id))
        .collect()
}

/// Two local sessions share a label (the same host and first eight
/// characters), and two teammates share a published name and session: each
/// overlap member still names exactly one row, by an ID a label cannot fake.
#[test]
fn overlap_members_name_their_sessions_by_stable_id_when_labels_collide() {
    let mut state = ready();
    let first = "claude-code.alice-12-first";
    let second = "claude-code.alice-12-second";
    register(&mut state, first, "work");
    register(&mut state, second, "work");
    edited(&mut state, first, "src/upload.rs", 1_000);
    edited(&mut state, second, "src/upload.rs", 1_000);
    heard(
        &mut state,
        THEM,
        "Carol",
        "c0ffee00",
        "src/upload.rs",
        1_000,
    );
    heard(
        &mut state,
        "cccccccccccccccccccccccccccccccc",
        "Carol",
        "c0ffee00",
        "src/upload.rs",
        1_000,
    );
    let d = view(&state, 2_000);
    let overlap = &d.overlaps[0];
    assert_eq!(overlap.path, "src/upload.rs");
    assert_eq!(
        overlap.agents,
        [
            "Carol: codex c0ffee00",
            "Carol: codex c0ffee00",
            "claude-code alice-12",
            "claude-code alice-12",
        ],
        "the labels collide"
    );
    assert_eq!(overlap.members.len(), overlap.agents.len());
    assert_eq!(overlap.more_agents, Some(0));
    let distinct: BTreeSet<&String> = overlap.members.iter().collect();
    assert_eq!(distinct.len(), 4, "{:?}", overlap.members);
    for (member, label) in overlap.members.iter().zip(&overlap.agents) {
        let rows = row_of(&d, member);
        assert_eq!(rows.len(), 1, "{member} names one row");
        let row = rows[0];
        let shown = match &row.machine {
            Some(machine) => format!("{machine}: {} {}", row.host, row.session),
            None => format!("{} {}", row.host, row.session),
        };
        assert_eq!(&shown, label, "member {member} is aligned with its label");
        assert_eq!(row.group, overlap.group, "{member}");
    }
    let locals: Vec<_> = d
        .agents
        .iter()
        .filter(|row| row.session_id.as_deref() == Some(first))
        .collect();
    assert_eq!(
        locals[0].id.as_deref(),
        Some("local:claude-code.alice-12-first")
    );
    assert!(
        d.agents
            .iter()
            .all(|row| row.id.is_some() && row.group.is_some()),
        "every row has an identity and a group"
    );
}

/// The page scopes a path's messages and notes to the path's overlap group
/// through each record's workspace. Notes on one relative path in a linked
/// worktree and in an unrelated repository resolve to the right groups.
#[test]
fn notes_on_one_path_in_unrelated_repositories_resolve_to_different_groups() {
    let mut state = ready();
    workspace(&mut state, "work", "repo-one");
    workspace(&mut state, "work-linked", "repo-one");
    workspace(&mut state, "other", "repo-two");
    for workspace in ["work", "work-linked", "other"] {
        run(
            &mut state,
            &format!("note-{workspace}"),
            Command::Remember {
                workspace_id: workspace.into(),
                author: None,
                text: format!("about src/api.rs in {workspace}"),
                paths: vec!["src/api.rs".into()],
                symbol: None,
                tags: Vec::new(),
            },
            100,
        );
    }
    let d = view(&state, 2_000);
    let group_of_note = |workspace: &str| {
        let note = d
            .notes
            .iter()
            .find(|n| n.workspace_id == workspace)
            .unwrap_or_else(|| panic!("a note in {workspace}: {:?}", d.notes));
        d.workspaces
            .iter()
            .find(|w| w.workspace_id == note.workspace_id)
            .and_then(|w| w.group.clone())
            .unwrap_or_else(|| panic!("a group for {workspace}"))
    };
    assert_eq!(group_of_note("work"), group_of_note("work-linked"));
    assert_ne!(group_of_note("work"), group_of_note("other"));
}

/// Linked worktrees of one repository share relative paths, so their work
/// on one file is one overlap; an unrelated repository's file with the same
/// name is not. Holds stay in the physical checkout that admitted them.
#[test]
fn linked_worktrees_share_an_overlap_group_and_other_repositories_do_not() {
    let mut state = ready();
    workspace(&mut state, "work", "repo-one");
    workspace(&mut state, "work-linked", "repo-one");
    workspace(&mut state, "other", "repo-two");
    let here = "claude-code.alice-1234567890";
    let linked = "codex.dana-1234567890";
    let elsewhere = "codex.erin-1234567890";
    register(&mut state, linked, "work-linked");
    register(&mut state, elsewhere, "other");
    edited(&mut state, here, "src/lib.rs", 1_000);
    run(
        &mut state,
        "hold-linked",
        Command::Admit {
            session_id: linked.into(),
            paths: vec!["src/lib.rs".into()],
            ttl_ms: 30_000,
        },
        1_000,
    );
    edited(&mut state, elsewhere, "src/lib.rs", 1_000);
    let d = view(&state, 2_000);
    let group = |session: &str| {
        d.agents
            .iter()
            .find(|row| row.session_id.as_deref() == Some(session))
            .unwrap()
            .group
            .clone()
            .unwrap()
    };
    assert_eq!(group(here), group(linked));
    assert_ne!(group(here), group(elsewhere));
    assert_eq!(d.overlaps.len(), 1, "{:?}", d.overlaps);
    let overlap = &d.overlaps[0];
    assert_eq!(overlap.group.as_deref(), Some(group(here).as_str()));
    let mut members = overlap.members.clone();
    members.sort();
    assert_eq!(
        members,
        [format!("local:{here}"), format!("local:{linked}")],
        "the unrelated repository's session is not a member"
    );
    assert!(overlap.editing);
    assert_eq!(d.editing.len(), 1);
    assert_eq!(
        d.editing[0].workspace_id, "work-linked",
        "the hold stays in its checkout"
    );
    let groups: BTreeMap<&str, Option<&str>> = d
        .workspaces
        .iter()
        .map(|row| (row.workspace_id.as_str(), row.group.as_deref()))
        .collect();
    assert_eq!(groups["work"], groups["work-linked"]);
    assert_ne!(groups["work"], groups["other"]);
    assert_eq!(groups["work"], Some(group(here).as_str()));
}

/// An overlap lists at most eight labels; the rest are counted, so a long
/// list is never read as a complete one.
#[test]
fn overlap_participants_beyond_the_label_limit_are_counted() {
    let mut state = ready();
    for n in 0..11 {
        let session = format!("codex.many-{n:02}-session");
        register(&mut state, &session, "work");
        edited(&mut state, &session, "src/crowded.rs", 1_000);
    }
    let d = view(&state, 2_000);
    let overlap = &d.overlaps[0];
    assert_eq!(overlap.agents.len(), 8);
    assert_eq!(overlap.members.len(), 8);
    assert_eq!(overlap.more_agents, Some(3));
}

/// A teammate past the horizon keeps its identity and group but leaves the
/// overlaps; the same teammate session heard again is the same row ID.
#[test]
fn a_stale_publication_keeps_its_identity_and_leaves_the_overlaps() {
    let mut state = ready();
    let alice = "claude-code.alice-1234567890";
    edited(&mut state, alice, "src/upload.rs", 1_000);
    heard(
        &mut state,
        THEM,
        "Carol",
        "c0ffee00",
        "src/upload.rs",
        1_000,
    );
    let fresh = view(&state, 2_000);
    let carol = fresh.agents.iter().find(|row| !row.local).unwrap();
    let id = carol.id.clone().unwrap();
    assert_eq!(id, format!("peer:{THEM}:codex:c0ffee00"));
    assert!(fresh.overlaps[0].members.contains(&id));
    let stale = view(&state, 1_000 + REMOTE_HORIZON_MS + 1);
    let carol = stale.agents.iter().find(|row| !row.local).unwrap();
    assert_eq!(carol.state, AgentState::Unheard);
    assert_eq!(carol.id.as_deref(), Some(id.as_str()));
    assert!(carol.group.is_some());
    assert!(stale.overlaps.is_empty());
}

/// A view from a coordinator that predates the identity fields still
/// decodes, with each field absent rather than invented.
#[test]
fn a_view_without_identity_fields_from_an_older_coordinator_still_decodes() {
    let mut state = ready();
    let alice = "claude-code.alice-1234567890";
    let bob = "codex.bob-0987654321";
    edited(&mut state, alice, "src/upload.rs", 1_000);
    edited(&mut state, bob, "src/upload.rs", 1_000);
    let mut value = serde_json::to_value(view(&state, 2_000)).unwrap();
    for row in value["agents"].as_array_mut().unwrap() {
        let row = row.as_object_mut().unwrap();
        assert!(row.remove("id").is_some() && row.remove("group").is_some());
    }
    for row in value["overlaps"].as_array_mut().unwrap() {
        let row = row.as_object_mut().unwrap();
        for field in ["group", "members", "more_agents"] {
            assert!(row.remove(field).is_some(), "{field}");
        }
    }
    for row in value["workspaces"].as_array_mut().unwrap() {
        assert!(row.as_object_mut().unwrap().remove("group").is_some());
    }
    let old: Dashboard = serde_json::from_value(value).unwrap();
    assert!(
        old.agents
            .iter()
            .all(|row| row.id.is_none() && row.group.is_none())
    );
    assert!(old.overlaps[0].members.is_empty());
    assert_eq!(old.overlaps[0].more_agents, None, "unknown, not zero");
    assert_eq!(old.overlaps[0].agents.len(), 2);
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
    // Each subagent is its own row and its own member, by its full ID.
    let gate = d
        .overlaps
        .iter()
        .find(|row| row.path == "src/gate.rs")
        .unwrap();
    assert_eq!(
        gate.members,
        [format!("local:{first}"), format!("local:{second}")]
    );
    for id in [parent, first, second] {
        assert_eq!(row_of(&d, &format!("local:{id}")).len(), 1, "{id}");
    }
}

/// The `/api/events` page, version 2 (issue #491): each row also carries the
/// typed facts the Live view's cards and wires read, and nothing free-form.
/// The golden vector pins the wire; a version 1 page still decodes.
#[test]
fn an_event_page_carries_the_live_view_facts_as_ids_enums_and_numbers() {
    use langspan_core::{DashboardEvent, DashboardEvents};
    let events = [
        r#"{"type":"clock_anchored","anchor":{"generation":2,"clock_ms":5000,"wall_ms":1800000000000}}"#,
        r#"{"type":"session_registered","session":{"session_id":"codex.bob.sub","workspace_id":"app","generation":2,"parent":"codex.bob"}}"#,
        r#"{"type":"write_refused","session_id":"codex.bob.sub","workspace_id":"app","kind":"stale_base","paths":["src/api.rs"],"peer_session_id":"claude-code.alice","input_digest":"SECRET-DIGEST","at_ms":6000,"generation":2}"#,
        r#"{"type":"source_observed","session_id":"codex.bob.sub","files":[{"path":"src/api.rs","state":{"type":"missing"}}],"origin":"read","at_ms":6100,"generation":2}"#,
        r#"{"type":"operation_observed","observation":{"observation_id":"o1","session_id":"codex.bob.sub","workspace_id":"app","operation_id":"op1","source":"codex","tool":"Edit","paths":["src/api.rs"],"result":"succeeded","lease_id":"l1","lease_relation":"current","concurrent_holders":[],"observed_at_ms":6200,"generation":2}}"#,
    ];
    let bob = Some("codex.bob.sub");
    let api = || vec!["src/api.rs".to_owned()];
    let rows = [
        (None, None, None, vec![]),
        (bob, Some("app"), None, vec![]),
        (bob, Some("app"), Some(6000), api()),
        (bob, None, Some(6100), api()),
        (bob, Some("app"), Some(6200), api()),
    ];
    let page = DashboardEvents::new(
        0,
        events
            .iter()
            .zip(rows)
            .zip(1..)
            .map(|((json, (session, workspace, at, paths)), seq)| {
                let event: Event = serde_json::from_str(json).unwrap();
                let kind = serde_json::to_value(&event).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                DashboardEvent::new(seq, &kind, session, workspace, at, paths).detail(&event)
            })
            .collect(),
    );
    let wire = include_str!("../../../fixtures/dashboard-events-v2.json");
    let golden: serde_json::Value = serde_json::from_str(wire).unwrap();
    assert_eq!(serde_json::to_value(&page).unwrap(), golden);
    assert_eq!(serde_json::from_str::<DashboardEvents>(wire).unwrap(), page);
    assert!(
        !wire.contains("SECRET"),
        "the digest is not a Live view fact"
    );
    // A version 1 page, without the new fields, still decodes.
    let old: DashboardEvents = serde_json::from_str(
        r#"{"version":1,"next":3,"events":[{"seq":3,"type":"write_refused","session_id":"s1","at_ms":5,"paths":["a.rs"],"more_paths":0}]}"#,
    )
    .unwrap();
    assert_eq!(old.events[0].peer_session_id, None);
    assert_eq!(old.events[0].refusal, None);
}
