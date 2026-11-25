use langspan_core::{Command, Event, Outcome, State, apply, decide};
use serde_json::{Value, json};

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (id, command) in [
        (
            "work",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/repos/work".into(),
            },
        ),
        (
            "session",
            Command::SessionRegister {
                session_id: "alice".into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
        ),
        (
            "lease",
            Command::Admit {
                session_id: "alice".into(),
                paths: vec!["src/api.rs".into()],
                ttl_ms: 1000,
            },
        ),
    ] {
        for event in decide(&state, &command, id, 10).events {
            apply(&mut state, &event).unwrap();
        }
    }
    state
}

/// The state without its counted writes, which mark automatic-check reruns
/// and grant nothing.
fn authority(state: &State) -> State {
    let mut state = state.clone();
    state.writes.clear();
    state
}

fn present(digit: &str) -> Value {
    json!({"type": "present", "digest": digit.repeat(64)})
}

fn disk() -> Value {
    json!({
        "op": "observe_disk", "workspace_id": "work", "path": "src/api.rs",
        "before": null, "after": present("a"), "related_operation_ids": [],
    })
}

fn branch() -> Value {
    json!({
        "op": "observe_branch", "workspace_id": "work", "branch_ref": "refs/heads/main",
        "head_oid": "a".repeat(40), "tree_oid": "b".repeat(40),
    })
}

fn command(value: Value) -> Command {
    serde_json::from_value(value).expect("bounded disk and branch fact commands must be supported")
}

#[test]
fn disk_facts_keep_baseline_distinct_from_creation_and_never_change_authority() {
    let mut state = ready();
    let before = state.clone();
    for prior in [Value::Null, json!({"type": "missing"}), present("b")] {
        let mut input = disk();
        input["before"] = prior.clone();
        let decision = decide(&state, &command(input), "scan-1", 20);
        assert_eq!(authority(&state), authority(&before));
        let outcome = serde_json::to_value(&decision.outcome).unwrap();
        assert_eq!(outcome["type"], "disk_observed");
        assert_eq!(outcome["observation"]["before"], prior);
        assert_eq!(outcome["observation"]["after"], present("a"));
        assert_eq!(outcome["observation"]["observation_id"], "scan-1");
        assert_eq!(outcome["observation"]["observed_at_ms"], 20);
        assert_eq!(outcome["observation"]["generation"], 1);
        assert_eq!(decision.events.len(), 1);
        apply(&mut state, &decision.events[0]).unwrap();
        assert_eq!(authority(&state), authority(&before));
    }
}

#[test]
fn deletion_is_a_missing_file_and_correlations_are_sorted_unverified_ids() {
    let state = ready();
    let mut input = disk();
    input["before"] = present("a");
    input["after"] = json!({"type": "missing"});
    input["related_operation_ids"] = json!(["unverified-b", "unverified-a", "unverified-b"]);
    let decision = decide(&state, &command(input), "scan-deletion", 20);
    let outcome = serde_json::to_value(decision.outcome).unwrap();
    assert_eq!(outcome["observation"]["after"], json!({"type": "missing"}));
    assert_eq!(
        outcome["observation"]["related_operation_ids"],
        json!(["unverified-a", "unverified-b"])
    );
    let mut replay = state.clone();
    apply(&mut replay, &decision.events[0]).unwrap();
    assert_eq!(
        authority(&replay),
        authority(&state),
        "correlation never grants or completes a lease"
    );
}

#[test]
fn a_kind_change_is_visible_when_content_digest_is_the_same() {
    let state = ready();
    let mut input = disk();
    input["before"] = present("a");
    input["after"]["kind"] = json!("symlink");
    let decision = decide(&state, &command(input), "kind-change", 20);
    let outcome = serde_json::to_value(&decision.outcome).unwrap();
    assert_eq!(outcome["observation"]["after"]["kind"], "symlink");
    assert_ne!(
        outcome["observation"]["before"],
        outcome["observation"]["after"]
    );
    let mut replay = state.clone();
    apply(&mut replay, &decision.events[0]).unwrap();
    assert_eq!(authority(&replay), authority(&state));
}

#[test]
fn file_kind_is_bounded_and_legacy_regular_states_stay_unchanged() {
    let old = present("a");
    let parsed: langspan_core::FileState = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), old);
    let mut unknown = old;
    unknown["kind"] = json!("socket");
    assert!(serde_json::from_value::<langspan_core::FileState>(unknown).is_err());
}

#[test]
fn branch_facts_include_detached_unborn_and_absent_local_refs() {
    let state = ready();
    for (reference, head, tree) in [
        ("HEAD", Value::Null, Value::Null),
        ("HEAD", json!("a".repeat(40)), json!("b".repeat(40))),
        ("refs/heads/deleted", Value::Null, Value::Null),
        (
            "refs/heads/fix-été",
            json!("a".repeat(64)),
            json!("b".repeat(64)),
        ),
    ] {
        let mut input = branch();
        input["branch_ref"] = json!(reference);
        input["head_oid"] = head.clone();
        input["tree_oid"] = tree.clone();
        let decision = decide(&state, &command(input), "branch-scan", 20);
        let outcome = serde_json::to_value(decision.outcome).unwrap();
        assert_eq!(outcome["type"], "branch_observed");
        assert_eq!(outcome["observation"]["branch_ref"], reference);
        assert_eq!(outcome["observation"]["head_oid"], head);
        assert_eq!(outcome["observation"]["tree_oid"], tree);
        let mut replay = state.clone();
        apply(&mut replay, &decision.events[0]).unwrap();
        assert_eq!(replay, state);
    }
}

#[test]
fn invalid_disk_facts_are_bounded_before_emitting_events() {
    let state = ready();
    for (field, invalid) in [
        ("workspace_id", json!("missing")),
        ("path", json!("../escape")),
        ("path", json!("x".repeat(513))),
        ("after", present("g")),
        (
            "before",
            json!({"type": "present", "digest": "a".repeat(63)}),
        ),
        ("related_operation_ids", json!(["has/slash"])),
        (
            "related_operation_ids",
            json!((0..65).map(|n| format!("op-{n}")).collect::<Vec<_>>()),
        ),
    ] {
        let mut input = disk();
        input[field] = invalid;
        let decision = decide(&state, &command(input), "invalid", 20);
        assert!(matches!(decision.outcome, Outcome::Error { .. }), "{field}");
        assert!(decision.events.is_empty());
    }
}

#[test]
fn invalid_branch_facts_are_bounded_before_emitting_events() {
    let state = ready();
    for (field, invalid) in [
        ("workspace_id", json!("missing")),
        ("branch_ref", json!("main")),
        ("branch_ref", json!("refs/heads/")),
        ("branch_ref", json!("refs/heads/line\nbreak")),
        (
            "branch_ref",
            json!(format!("refs/heads/{}", "x".repeat(502))),
        ),
        ("head_oid", json!("a".repeat(41))),
        ("tree_oid", json!("z".repeat(40))),
        ("tree_oid", Value::Null),
    ] {
        let mut input = branch();
        input[field] = invalid;
        let decision = decide(&state, &command(input), "invalid", 20);
        assert!(matches!(decision.outcome, Outcome::Error { .. }), "{field}");
        assert!(decision.events.is_empty());
    }
}

#[test]
fn facts_replay_without_projections_and_reject_invalid_event_generations() {
    let state = ready();
    for input in [disk(), branch()] {
        let decision = decide(&state, &command(input), "scan", 20);
        let event = &decision.events[0];
        let wire = serde_json::to_string(event).unwrap();
        let mut replay = state.clone();
        apply(&mut replay, &serde_json::from_str(&wire).unwrap()).unwrap();
        assert_eq!(replay, state);
        let mut invalid = serde_json::to_value(event).unwrap();
        invalid["observation"]["generation"] = json!(2);
        assert!(apply(&mut replay, &serde_json::from_value(invalid).unwrap()).is_err());
        assert_eq!(replay, state);
    }
}

#[test]
fn fact_commands_have_stable_strict_wire_vectors() {
    let vectors = [
        r#"{"op":"observe_disk","workspace_id":"work","path":"src/api.rs","before":null,"after":{"type":"present","digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"related_operation_ids":[]}"#,
        r#"{"op":"observe_branch","workspace_id":"work","branch_ref":"HEAD","head_oid":null,"tree_oid":null}"#,
    ];
    for wire in vectors {
        let parsed: Command = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), wire);
    }
    let extra = r#"{"op":"observe_disk","workspace_id":"work","path":"src/api.rs","before":null,"after":{"type":"missing","allow":true},"related_operation_ids":[]}"#;
    assert!(serde_json::from_str::<Command>(extra).is_err());
}
