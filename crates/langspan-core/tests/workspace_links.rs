use langspan_core::{Command, Event, Outcome, State, apply, decide, expiry_events};
use serde_json::json;

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    state
}

fn register(state: &mut State, id: &str, root: &str) -> Outcome {
    let decision = decide(
        state,
        &Command::WorkspaceRegister {
            workspace_id: id.into(),
            root: root.into(),
        },
        &format!("register-{id}"),
        0,
    );
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

fn verified_event(id: &str, root: &str, linked: &[&str]) -> Event {
    serde_json::from_value(json!({"type":"workspace_registered", "workspace":{
        "workspace_id":id, "root":root, "verified_overlaps":linked,
    }}))
    .expect("verified workspace event must be supported")
}

#[test]
fn verified_nested_workspaces_replay_and_plain_reregistration_preserves_the_proof() {
    let mut state = ready();
    assert!(matches!(
        register(&mut state, "parent", "/repo"),
        Outcome::Workspace { .. }
    ));
    apply(
        &mut state,
        &verified_event("child", "/repo/.worktrees/child", &["parent"]),
    )
    .unwrap();
    let before = state.clone();
    assert!(matches!(
        register(&mut state, "parent", "/repo"),
        Outcome::Workspace { .. }
    ));
    assert!(matches!(
        register(&mut state, "child", "/repo/.worktrees/child"),
        Outcome::Workspace { .. }
    ));
    assert_eq!(state, before);
    assert_eq!(
        serde_json::to_value(&state.workspaces["child"]).unwrap()["verified_overlaps"],
        json!(["parent"])
    );
}

#[test]
fn verification_works_when_the_child_is_registered_first() {
    let mut state = ready();
    register(&mut state, "child", "/repo/.worktrees/child");
    apply(&mut state, &verified_event("parent", "/repo", &["child"])).unwrap();
    assert!(matches!(
        register(&mut state, "child", "/repo/.worktrees/child"),
        Outcome::Workspace { .. }
    ));
    assert!(matches!(
        register(&mut state, "child", "/elsewhere"),
        Outcome::Error { .. }
    ));
}

#[test]
fn ordinary_overlaps_same_root_aliases_and_incomplete_proofs_are_rejected() {
    let mut state = ready();
    register(&mut state, "parent", "/repo");
    register(&mut state, "other", "/other");
    assert!(matches!(
        register(&mut state, "plain-child", "/repo/ordinary"),
        Outcome::Error { .. }
    ));
    for event in [
        verified_event("alias", "/repo", &["parent"]),
        verified_event("child", "/repo/.worktrees/child", &["missing"]),
        verified_event("child", "/repo/.worktrees/child", &["other", "parent"]),
    ] {
        let before = state.clone();
        assert!(apply(&mut state, &event).is_err());
        assert_eq!(state, before);
    }
    apply(
        &mut state,
        &verified_event("child", "/repo/.worktrees/child", &["parent"]),
    )
    .unwrap();
    let before = state.clone();
    assert!(
        apply(
            &mut state,
            &verified_event("grandchild", "/repo/.worktrees/child/nested", &["child"])
        )
        .is_err()
    );
    assert_eq!(
        state, before,
        "every overlapping workspace needs verification"
    );
    apply(
        &mut state,
        &verified_event(
            "grandchild",
            "/repo/.worktrees/child/nested",
            &["child", "parent"],
        ),
    )
    .unwrap();
}

#[test]
fn workspace_verification_cannot_be_asserted_through_the_wire_command() {
    assert!(serde_json::from_value::<Command>(json!({
        "op":"workspace_register_verified", "workspace_id":"child", "root":"/repo/child", "verified_overlaps":["parent"],
    })).is_err());
    assert!(serde_json::from_value::<Command>(json!({
        "op":"workspace_register", "workspace_id":"child", "root":"/repo/child", "verified_overlaps":["parent"],
    })).is_err());
}

#[test]
fn historical_workspace_events_round_trip_without_metadata() {
    let wire =
        r#"{"type":"workspace_registered","workspace":{"workspace_id":"parent","root":"/repo"}}"#;
    let event: Event = serde_json::from_str(wire).unwrap();
    let mut state = ready();
    apply(&mut state, &event).unwrap();
    assert_eq!(serde_json::to_string(&event).unwrap(), wire);
    assert!(matches!(
        register(&mut state, "child", "/repo/ordinary"),
        Outcome::Error { .. }
    ));
}

#[test]
fn prepared_registration_records_proof_but_cannot_be_persisted_as_the_original_command() {
    let mut state = ready();
    register(&mut state, "parent", "/repo");
    let prepared = Command::WorkspaceRegisterVerified {
        workspace_id: "child".into(),
        root: "/repo/.worktrees/child".into(),
        verified_overlaps: vec!["parent".into(), "parent".into()],
    };
    assert!(
        serde_json::to_string(&prepared).is_err(),
        "store the ordinary original request, never a prepared verification"
    );
    let decision = decide(&state, &prepared, "verified-child", 0);
    assert!(matches!(decision.outcome, Outcome::Workspace { .. }));
    for event in &decision.events {
        apply(&mut state, event).unwrap();
    }
    assert_eq!(state.workspaces["child"].verified_overlaps, ["parent"]);
    let reread = decide(
        &state,
        &Command::WorkspaceRegisterVerified {
            workspace_id: "parent".into(),
            root: "/repo".into(),
            verified_overlaps: vec!["child".into()],
        },
        "reregister-parent",
        0,
    );
    assert!(
        matches!(reread.outcome, Outcome::Workspace { ref workspace } if workspace.verified_overlaps.is_empty())
    );
    assert!(reread.events.is_empty());
}

fn lease_in(state: &mut State, workspace: &str, path: &str) {
    for (id, command) in [
        (
            "session",
            Command::SessionRegister {
                session_id: "alice".into(),
                workspace_id: workspace.into(),
                parent: None,
                headless: false,
            },
        ),
        (
            "lease",
            Command::Admit {
                session_id: "alice".into(),
                paths: vec![path.into()],
                ttl_ms: 1000,
            },
        ),
    ] {
        let decision = decide(state, &command, id, 10);
        assert!(!matches!(decision.outcome, Outcome::Error { .. }));
        for event in decision.events {
            apply(state, &event).unwrap();
        }
    }
}

fn nested_registration() -> Command {
    Command::WorkspaceRegisterVerified {
        workspace_id: "child".into(),
        root: "/repo/.worktrees/child".into(),
        verified_overlaps: vec!["parent".into()],
    }
}

#[test]
fn an_active_parent_claim_blocks_new_nested_registration_until_completion() {
    for path in [".worktrees/child", ".worktrees/child/src/api.rs"] {
        let mut state = ready();
        register(&mut state, "parent", "/repo");
        lease_in(&mut state, "parent", path);
        let before = state.clone();
        let blocked = decide(&state, &nested_registration(), "nested", 20);
        assert!(
            matches!(blocked.outcome, Outcome::Error { code, .. } if code == "workspace_in_use")
        );
        assert!(blocked.events.is_empty());
        assert_eq!(state, before);
        let complete = decide(
            &state,
            &Command::Complete {
                session_id: "alice".into(),
                lease_id: "lease".into(),
            },
            "complete",
            20,
        );
        for event in complete.events {
            apply(&mut state, &event).unwrap();
        }
        assert!(matches!(
            decide(&state, &nested_registration(), "nested", 21).outcome,
            Outcome::Workspace { .. }
        ));
    }
}

#[test]
fn expired_or_invalidated_parent_claims_do_not_block_nested_registration() {
    let mut state = ready();
    register(&mut state, "parent", "/repo");
    lease_in(&mut state, "parent", ".worktrees/child/src/api.rs");
    let mut expired = state.clone();
    for event in expiry_events(&expired, 1010) {
        apply(&mut expired, &event).unwrap();
    }
    assert!(matches!(
        decide(&expired, &nested_registration(), "nested", 1010).outcome,
        Outcome::Workspace { .. }
    ));
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert!(matches!(
        decide(&state, &nested_registration(), "nested", 20).outcome,
        Outcome::Workspace { .. }
    ));
}

#[test]
fn an_active_child_claim_does_not_block_registering_its_parent() {
    let mut state = ready();
    register(&mut state, "child", "/repo/.worktrees/child");
    lease_in(&mut state, "child", "src/api.rs");
    let decision = decide(
        &state,
        &Command::WorkspaceRegisterVerified {
            workspace_id: "parent".into(),
            root: "/repo".into(),
            verified_overlaps: vec!["child".into()],
        },
        "parent",
        20,
    );
    assert!(matches!(decision.outcome, Outcome::Workspace { .. }));
}

#[test]
fn only_component_aligned_parent_paths_block_nested_registration() {
    for path in ["src/api.rs", ".worktrees/childish/api.rs"] {
        let mut state = ready();
        register(&mut state, "parent", "/repo");
        lease_in(&mut state, "parent", path);
        assert!(matches!(
            decide(&state, &nested_registration(), "nested", 20).outcome,
            Outcome::Workspace { .. }
        ));
    }
}
