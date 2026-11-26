use langspan_core::{
    Command, Event, Lease, LeaseRelation, Outcome, State, ToolResult, apply, decide,
};
use serde_json::{Value, json};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before);
    for event in decision.events {
        apply(state, &event).expect("a valid decision must replay");
    }
    decision.outcome
}

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    run(
        &mut state,
        "workspace",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        0,
    );
    for session in ["alice", "bob"] {
        run(
            &mut state,
            &format!("register-{session}"),
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

fn identity() -> Value {
    json!({
        "operation_id": "op-1",
        "source": "claude-code",
        "tool": "Write",
        "input_digest": "a".repeat(64),
    })
}

fn command(value: Value) -> Command {
    serde_json::from_value(value).expect("versioned tool commands must be accepted")
}

fn admit(operation: Value) -> Command {
    command(json!({
        "op": "admit_tool", "session_id": "alice", "paths": ["src/api.rs"],
        "ttl_ms": 1000, "operation": operation,
    }))
}

fn observe(operation: Value) -> Command {
    command(json!({
        "op": "observe_tool", "session_id": "alice", "paths": ["src/api.rs"],
        "operation": operation, "result": "succeeded", "lease_id": "lease-a",
    }))
}

fn relation(outcome: &Outcome) -> String {
    let Outcome::Observed { observation } = outcome else {
        panic!("expected observation, got {outcome:?}")
    };
    serde_json::to_string(&observation.lease_relation).unwrap()
}

#[test]
fn admission_retains_the_complete_tool_identity() {
    let mut state = ready();
    let outcome = run(&mut state, "lease-a", admit(identity()), 10);
    let Outcome::Admitted { lease } = outcome else {
        panic!("expected admission")
    };
    let encoded = serde_json::to_value(&lease).unwrap();
    assert_eq!(encoded["operation"], identity());
    let replayed: Lease = serde_json::from_value(encoded).unwrap();
    assert_eq!(replayed, lease);
}

#[test]
fn tool_admission_uses_the_same_all_or_none_conflict_rules() {
    let mut state = ready();
    run(
        &mut state,
        "lease-b",
        Command::Admit {
            session_id: "bob".into(),
            paths: vec!["src/api.rs".into()],
            ttl_ms: 1000,
        },
        10,
    );
    let before = state.clone();
    let attempt = command(json!({
        "op": "admit_tool", "session_id": "alice", "paths": ["src/other.rs", "src/api.rs"],
        "ttl_ms": 1000, "operation": identity(),
    }));
    assert!(matches!(
        run(&mut state, "lease-a", attempt, 20),
        Outcome::Conflict { .. }
    ));
    // Nothing is reserved; only the refusal is remembered, for a starting
    // session's recent section (roadmap C3).
    assert_eq!(state.path_refusals.len(), 1);
    assert_eq!(state.path_refusals[0].paths, ["src/api.rs", "src/other.rs"]);
    state.path_refusals.clear();
    assert_eq!(state, before);
}

#[test]
fn every_changed_identity_field_is_an_input_mismatch_without_changing_authority() {
    let mut state = ready();
    run(&mut state, "lease-a", admit(identity()), 10);
    let before = state.clone();
    for (field, changed) in [
        ("operation_id", json!("op-2")),
        ("source", json!("codex")),
        ("tool", json!("Edit")),
        ("input_digest", json!("b".repeat(64))),
    ] {
        let mut different = identity();
        different[field] = changed;
        let outcome = run(
            &mut state,
            &format!("observe-{field}"),
            observe(different),
            20,
        );
        assert_eq!(relation(&outcome), "\"input_mismatch\"", "{field}");
        // The advisory record of touched paths may move; authority may not.
        let mut authority = state.clone();
        authority.activity = before.activity.clone();
        // Counted writes mark automatic-check reruns, and who wrote when
        // marks a check's comparison with its baseline; they grant nothing.
        authority.writes = before.writes.clone();
        authority.first_writes = before.first_writes.clone();
        authority.write_runs = before.write_runs.clone();
        assert_eq!(authority, before, "a mismatch is evidence, never authority");
    }
}

#[test]
fn matching_tool_identity_keeps_current_expired_and_invalidated_relations() {
    let mut state = ready();
    run(&mut state, "lease-a", admit(identity()), 10);
    assert_eq!(
        relation(&run(&mut state, "obs-current", observe(identity()), 20)),
        "\"current\""
    );
    assert_eq!(
        relation(&run(&mut state, "obs-expired", observe(identity()), 1010)),
        "\"expired\""
    );
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert_eq!(
        relation(&run(&mut state, "obs-restart", observe(identity()), 20)),
        "\"invalidated\""
    );
}

#[test]
fn a_legacy_observation_cannot_downgrade_a_typed_admission() {
    let mut state = ready();
    run(&mut state, "lease-a", admit(identity()), 10);
    let legacy = Command::Observe {
        session_id: "alice".into(),
        operation_id: "op-1".into(),
        source: "claude-code".into(),
        tool: "Write".into(),
        paths: vec!["src/api.rs".into()],
        result: ToolResult::Succeeded,
        lease_id: Some("lease-a".into()),
    };
    assert_eq!(
        relation(&run(&mut state, "legacy-observation", legacy, 20)),
        "\"input_mismatch\""
    );
}

#[test]
fn a_typed_observation_cannot_inherit_an_untyped_admission() {
    let mut state = ready();
    run(
        &mut state,
        "lease-a",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/api.rs".into()],
            ttl_ms: 1000,
        },
        10,
    );
    assert_eq!(
        relation(&run(
            &mut state,
            "typed-observation",
            observe(identity()),
            20
        )),
        "\"input_mismatch\""
    );
}

#[test]
fn invalid_tool_identity_is_rejected_before_authority_changes() {
    let state = ready();
    for (field, invalid) in [
        ("operation_id", json!("bad/id")),
        ("source", json!("")),
        ("tool", json!("x".repeat(129))),
        ("input_digest", json!("a".repeat(63))),
        ("input_digest", json!("a".repeat(65))),
        ("input_digest", json!("g".repeat(64))),
    ] {
        let mut invalid_identity = identity();
        invalid_identity[field] = invalid;
        for input in [admit(invalid_identity.clone()), observe(invalid_identity)] {
            let decision = decide(&state, &input, "invalid", 10);
            assert!(matches!(decision.outcome, Outcome::Error { .. }), "{field}");
            assert!(decision.events.is_empty());
        }
    }
    let mut extra = identity();
    extra["extra"] = json!(true);
    assert!(
        serde_json::from_value::<Command>(json!({
            "op": "admit_tool", "session_id": "alice", "paths": ["src/api.rs"],
            "ttl_ms": 1000, "operation": extra,
        }))
        .is_err()
    );
}

#[test]
fn legacy_lease_json_remains_unchanged_and_replayable() {
    let old = json!({
        "lease_id": "old", "session_id": "alice", "workspace_id": "work",
        "paths": ["src/api.rs"], "generation": 1, "expires_at_ms": 1000, "status": "active",
    });
    let lease: Lease = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(&lease).unwrap(), old);
    let mut state = ready();
    apply(&mut state, &Event::LeaseGranted { lease }).unwrap();
    let outcome = run(
        &mut state,
        "legacy-observation",
        Command::Observe {
            session_id: "alice".into(),
            operation_id: "old-op".into(),
            source: "claude-code".into(),
            tool: "Edit".into(),
            paths: vec!["src/api.rs".into()],
            result: ToolResult::Succeeded,
            lease_id: Some("old".into()),
        },
        20,
    );
    let Outcome::Observed { observation } = outcome else {
        panic!("expected observation")
    };
    assert_eq!(observation.lease_relation, LeaseRelation::Current);
}

#[test]
fn tool_command_wire_vectors_are_stable() {
    let vectors = [
        r#"{"op":"admit_tool","session_id":"alice","paths":["src/api.rs"],"ttl_ms":1000,"operation":{"operation_id":"op-1","source":"claude-code","tool":"Write","input_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#,
        r#"{"op":"observe_tool","session_id":"alice","operation":{"operation_id":"op-1","source":"claude-code","tool":"Write","input_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"paths":["src/api.rs"],"result":"succeeded","lease_id":"lease-a"}"#,
    ];
    for wire in vectors {
        let command: Command = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&command).unwrap(), wire);
    }
}
