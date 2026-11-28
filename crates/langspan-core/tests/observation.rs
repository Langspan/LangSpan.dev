//! Observations record what a host reported after a tool ran. They never grant,
//! renew, complete, expire or reactivate admission.

use langspan_core::{
    Command, Conflict, Decision, Event, LeaseRelation, LeaseStatus, Outcome, SnapshotRecord, State,
    ToolResult, apply, decide, expiry_events,
};

/// Everything that decides admission, as the stored snapshot holds it. An
/// observation may update the advisory record of what a session last
/// touched; it may change nothing else.
fn authority(state: &State) -> serde_json::Value {
    let mut state = state.clone();
    state.activity.clear();
    // Counted writes mark automatic-check reruns, and who wrote when marks
    // a check's comparison with its baseline; they grant nothing.
    state.writes.clear();
    let mut record = serde_json::to_value(SnapshotRecord::of(&state)).unwrap();
    let fields = record.as_object_mut().unwrap();
    fields.remove("first_writes");
    fields.remove("write_runs");
    record
}

fn paths(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}

fn run(state: &mut State, command: Command, id: &str, now: u64) -> Decision {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decide is pure");
    for event in &decision.events {
        apply(state, event).expect("core accepts its own event");
    }
    decision
}

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (command, id) in [
        (
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/repos/work".into(),
            },
            "w",
        ),
        (
            Command::SessionRegister {
                session_id: "alice".into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            "sa",
        ),
        (
            Command::SessionRegister {
                session_id: "bob".into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            "sb",
        ),
    ] {
        run(&mut state, command, id, 0);
    }
    state
}

fn admit(state: &mut State, session: &str, id: &str, file: &str, now: u64, ttl_ms: u64) {
    let decision = run(
        state,
        Command::Admit {
            session_id: session.into(),
            paths: paths(&[file]),
            ttl_ms,
        },
        id,
        now,
    );
    assert!(
        matches!(decision.outcome, Outcome::Admitted { .. }),
        "{decision:?}"
    );
}

fn observe(session: &str, lease_id: Option<&str>, file: &str, result: ToolResult) -> Command {
    Command::Observe {
        session_id: session.into(),
        operation_id: "op-1".into(),
        source: "claude-code".into(),
        tool: "Edit".into(),
        paths: paths(&[file]),
        result,
        lease_id: lease_id.map(Into::into),
    }
}

fn observed(decision: &Decision) -> &langspan_core::Observation {
    match &decision.outcome {
        Outcome::Observed { observation } => observation,
        other => panic!("expected an observation, got {other:?}"),
    }
}

#[test]
fn observation_of_a_current_lease_records_without_changing_authority() {
    let mut state = ready();
    admit(&mut state, "alice", "lease-a", "src/api.rs", 10, 1000);
    let before = state.clone();
    let decision = run(
        &mut state,
        observe(
            "alice",
            Some("lease-a"),
            "src/api.rs",
            ToolResult::Succeeded,
        ),
        "obs-1",
        20,
    );
    let observation = observed(&decision);
    assert_eq!(observation.lease_relation, LeaseRelation::Current);
    assert_eq!(observation.observed_at_ms, 20, "time comes from the caller");
    assert_eq!(observation.observation_id, "obs-1");
    assert_eq!(observation.workspace_id, "work");
    assert_eq!(observation.generation, 1);
    assert_eq!(
        authority(&state),
        authority(&before),
        "an observation is history, not state"
    );
    assert_eq!(
        decision.events,
        vec![Event::OperationObserved {
            observation: observation.clone()
        }]
    );
}

#[test]
fn late_observation_after_expiry_is_flagged_and_never_revives_the_lease() {
    let mut state = ready();
    admit(&mut state, "alice", "lease-a", "src/api.rs", 10, 100);
    for event in expiry_events(&state, 500) {
        apply(&mut state, &event).unwrap();
    }
    admit(&mut state, "bob", "lease-b", "src/api.rs", 600, 1000);
    let before = state.clone();
    let decision = run(
        &mut state,
        observe(
            "alice",
            Some("lease-a"),
            "src/api.rs",
            ToolResult::Succeeded,
        ),
        "obs-late",
        700,
    );
    let observation = observed(&decision);
    assert_eq!(observation.lease_relation, LeaseRelation::Expired);
    assert_eq!(
        observation.concurrent_holders,
        vec![Conflict {
            lease_id: "lease-b".into(),
            session_id: "bob".into(),
            paths: paths(&["src/api.rs"]),
            regions: Vec::new(),
        }],
        "the holder admitted in the meantime is named"
    );
    assert_eq!(authority(&state), authority(&before));
    assert_eq!(state.leases["lease-a"].status, LeaseStatus::Expired);
}

#[test]
fn an_unexpired_active_lease_past_its_deadline_counts_as_expired() {
    let mut state = ready();
    admit(&mut state, "alice", "lease-a", "src/api.rs", 10, 100);
    let decision = decide(
        &state,
        &observe("alice", Some("lease-a"), "src/api.rs", ToolResult::Failed),
        "obs",
        110,
    );
    assert_eq!(observed(&decision).lease_relation, LeaseRelation::Expired);
}

#[test]
fn observation_after_restart_is_accepted_from_a_stale_session_as_invalidated() {
    let mut state = ready();
    admit(&mut state, "alice", "lease-a", "src/api.rs", 10, 1000);
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    let before = state.clone();
    let decision = run(
        &mut state,
        observe(
            "alice",
            Some("lease-a"),
            "src/api.rs",
            ToolResult::Succeeded,
        ),
        "obs-restart",
        20,
    );
    let observation = observed(&decision);
    assert_eq!(observation.lease_relation, LeaseRelation::Invalidated);
    assert_eq!(observation.generation, 2);
    assert_eq!(authority(&state), authority(&before));
    assert_eq!(state.leases["lease-a"].status, LeaseStatus::Invalidated);
}

#[test]
fn observation_without_a_grant_is_not_admitted_and_names_current_holders() {
    let mut state = ready();
    admit(&mut state, "bob", "lease-b", "src/api.rs", 10, 1000);
    let decision = run(
        &mut state,
        observe(
            "alice",
            Some("never-granted"),
            "src/api.rs",
            ToolResult::Succeeded,
        ),
        "obs-none",
        20,
    );
    let observation = observed(&decision);
    assert_eq!(observation.lease_relation, LeaseRelation::NotAdmitted);
    assert_eq!(observation.concurrent_holders.len(), 1);
    let decision = run(
        &mut state,
        observe("alice", None, "src/other.rs", ToolResult::Interrupted),
        "obs-no-lease",
        20,
    );
    assert_eq!(
        observed(&decision).lease_relation,
        LeaseRelation::NotAdmitted
    );
    assert!(observed(&decision).concurrent_holders.is_empty());
}

#[test]
fn completed_leases_report_another_call_in_the_same_session() {
    let mut state = ready();
    admit(&mut state, "alice", "lease-a", "src/api.rs", 10, 1000);
    run(
        &mut state,
        Command::Complete {
            session_id: "alice".into(),
            lease_id: "lease-a".into(),
        },
        "complete-a",
        20,
    );
    admit(&mut state, "alice", "lease-a2", "src/api.rs", 30, 1000);
    let decision = decide(
        &state,
        &observe(
            "alice",
            Some("lease-a"),
            "src/api.rs",
            ToolResult::Succeeded,
        ),
        "obs",
        40,
    );
    let observation = observed(&decision);
    assert_eq!(observation.lease_relation, LeaseRelation::Completed);
    assert_eq!(
        observation.concurrent_holders,
        vec![Conflict {
            lease_id: "lease-a2".into(),
            session_id: "alice".into(),
            paths: paths(&["src/api.rs"]),
            regions: Vec::new(),
        }],
        "a different tool call is a concurrent holder even in the same session"
    );
}

#[test]
fn an_unadmitted_path_is_a_scope_mismatch_without_changing_authority() {
    let mut state = ready();
    admit(&mut state, "alice", "lease-a", "src/api.rs", 10, 1000);
    admit(&mut state, "bob", "lease-b", "src/other.rs", 10, 1000);
    let before = state.clone();
    let decision = run(
        &mut state,
        observe(
            "alice",
            Some("lease-a"),
            "src/other.rs",
            ToolResult::Succeeded,
        ),
        "obs-mismatch",
        20,
    );
    let observation = observed(&decision);
    assert_eq!(
        serde_json::to_string(&observation.lease_relation).unwrap(),
        "\"scope_mismatch\"",
        "an admission for api.rs must not cover a result for other.rs"
    );
    assert_eq!(observation.paths, paths(&["src/other.rs"]));
    assert_eq!(observation.lease_id.as_deref(), Some("lease-a"));
    assert_eq!(observation.concurrent_holders[0].lease_id, "lease-b");
    assert_eq!(
        authority(&state),
        authority(&before),
        "mismatch evidence does not change authority"
    );
}

#[test]
fn an_observed_subset_of_the_admitted_paths_is_covered() {
    let mut state = ready();
    let decision = run(
        &mut state,
        Command::Admit {
            session_id: "alice".into(),
            paths: paths(&["src/api.rs", "src/lib.rs"]),
            ttl_ms: 1000,
        },
        "lease-a",
        10,
    );
    assert!(matches!(decision.outcome, Outcome::Admitted { .. }));
    let decision = decide(
        &state,
        &observe("alice", Some("lease-a"), "src/api.rs", ToolResult::Failed),
        "obs-subset",
        20,
    );
    let observation = observed(&decision);
    assert_eq!(observation.lease_relation, LeaseRelation::Current);
    assert!(observation.concurrent_holders.is_empty());
}

#[test]
fn late_observation_reports_a_new_lease_in_the_same_session() {
    let mut state = ready();
    admit(&mut state, "alice", "old-call", "src/api.rs", 10, 10);
    for event in expiry_events(&state, 21) {
        apply(&mut state, &event).unwrap();
    }
    admit(&mut state, "alice", "new-call", "src/api.rs", 21, 1000);
    let before = state.clone();
    let decision = run(
        &mut state,
        observe(
            "alice",
            Some("old-call"),
            "src/api.rs",
            ToolResult::Succeeded,
        ),
        "obs-old-call",
        22,
    );
    let observation = observed(&decision);
    assert_eq!(observation.lease_relation, LeaseRelation::Expired);
    assert_eq!(
        observation.concurrent_holders,
        vec![Conflict {
            lease_id: "new-call".into(),
            session_id: "alice".into(),
            paths: paths(&["src/api.rs"]),
            regions: Vec::new(),
        }]
    );
    assert_eq!(authority(&state), authority(&before));
}

#[test]
fn historical_observation_attribution_remains_replayable() {
    let mut state = ready();
    admit(&mut state, "alice", "lease-a", "src/api.rs", 10, 1000);
    let decision = decide(
        &state,
        &observe(
            "alice",
            Some("lease-a"),
            "src/other.rs",
            ToolResult::Succeeded,
        ),
        "obs-historical",
        20,
    );
    let Event::OperationObserved { mut observation } = decision.events[0].clone() else {
        unreachable!()
    };
    // M2 wrote Current for this mismatch and omitted same-session holders.
    // New classification must not make those already committed events unreadable.
    observation.lease_relation = LeaseRelation::Current;
    observation.concurrent_holders.clear();
    let before = state.clone();
    apply(&mut state, &Event::OperationObserved { observation }).unwrap();
    assert_eq!(authority(&state), authority(&before));
}

#[test]
fn another_sessions_lease_cannot_be_claimed_and_unknown_sessions_are_rejected() {
    let mut state = ready();
    admit(&mut state, "bob", "lease-b", "src/api.rs", 10, 1000);
    let decision = decide(
        &state,
        &observe(
            "alice",
            Some("lease-b"),
            "src/api.rs",
            ToolResult::Succeeded,
        ),
        "obs",
        20,
    );
    assert!(
        matches!(&decision.outcome, Outcome::Error { code, .. } if code == "not_holder"),
        "{decision:?}"
    );
    assert!(decision.events.is_empty());
    let decision = decide(
        &state,
        &observe("mallory", None, "src/api.rs", ToolResult::Succeeded),
        "obs",
        20,
    );
    assert!(
        matches!(&decision.outcome, Outcome::Error { code, .. } if code == "session_not_found")
    );
}

#[test]
fn observation_fields_are_bounded_and_validated() {
    let state = ready();
    let mut bad = observe("alice", None, "../escape", ToolResult::Succeeded);
    assert!(matches!(
        decide(&state, &bad, "obs", 0).outcome,
        Outcome::Error { .. }
    ));
    for field in ["operation_id", "source", "tool", "lease_id"] {
        bad = observe("alice", Some("lease"), "src/a.rs", ToolResult::Succeeded);
        if let Command::Observe {
            operation_id,
            source,
            tool,
            lease_id,
            ..
        } = &mut bad
        {
            let target = match field {
                "operation_id" => operation_id,
                "source" => source,
                "tool" => tool,
                _ => lease_id.as_mut().unwrap(),
            };
            *target = "has/slash".into();
        }
        let decision = decide(&state, &bad, "obs", 0);
        assert!(
            matches!(decision.outcome, Outcome::Error { .. }),
            "{field}: {decision:?}"
        );
        assert!(decision.events.is_empty());
    }
}

#[test]
fn persisted_observations_replay_and_reject_invalid_generation_or_session() {
    let mut state = ready();
    admit(&mut state, "alice", "lease-a", "src/api.rs", 10, 1000);
    let decision = decide(
        &state,
        &observe(
            "alice",
            Some("lease-a"),
            "src/api.rs",
            ToolResult::Succeeded,
        ),
        "obs",
        20,
    );
    let event = decision.events[0].clone();
    let json = serde_json::to_string(&event).unwrap();
    let parsed: Event = serde_json::from_str(&json).unwrap();
    let mut replayed = state.clone();
    apply(&mut replayed, &parsed).unwrap();
    assert_eq!(authority(&replayed), authority(&state));

    let Event::OperationObserved { mut observation } = event else {
        unreachable!()
    };
    observation.generation = 7;
    assert!(
        apply(
            &mut state.clone(),
            &Event::OperationObserved {
                observation: observation.clone()
            }
        )
        .is_err(),
        "an observation must belong to the current generation"
    );
    observation.generation = 1;
    observation.session_id = "mallory".into();
    assert!(
        apply(
            &mut state.clone(),
            &Event::OperationObserved { observation }
        )
        .is_err(),
        "an observation must name a known session"
    );
}

#[test]
fn observe_wire_command_is_tagged_and_rejects_unknown_fields() {
    let json = r#"{"op":"observe","session_id":"alice","operation_id":"op-1","source":"claude-code","tool":"Edit","paths":["src/api.rs"],"result":"succeeded","lease_id":"lease-a"}"#;
    let command: Command = serde_json::from_str(json).unwrap();
    assert_eq!(serde_json::to_string(&command).unwrap(), json);
    let extra = json.replace(r#""op":"observe""#, r#""op":"observe","allow":true"#);
    assert!(serde_json::from_str::<Command>(&extra).is_err());
}
