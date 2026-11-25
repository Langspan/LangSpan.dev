use langspan_core::{Command, Event, LeaseStatus, Outcome, State, apply, decide};
use serde_json::{Value, json};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "observation decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    run(
        &mut state,
        "work",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        0,
    );
    run(
        &mut state,
        "session",
        Command::SessionRegister {
            session_id: "alice".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        0,
    );
    run(
        &mut state,
        "lease",
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/api.rs".into()],
            ttl_ms: 1000,
        },
        10,
    );
    state
}

fn probe(status: &str, pid: u32, start: &str) -> Command {
    serde_json::from_value(json!({
        "op": "observe_session_liveness", "session_id": "alice",
        "process": {"pid": pid, "start_token": start}, "status": status,
    }))
    .expect("the liveness observation command must be supported")
}

fn record(state: &State) -> Value {
    serde_json::to_value(state).unwrap()["session_liveness"]["alice"].clone()
}

#[test]
fn process_observations_do_not_grant_renew_release_or_reattach() {
    let mut state = ready();
    let before = state.clone();
    for (status, at) in [("alive", 20), ("unknown", 30), ("exited", 40)] {
        let outcome = run(&mut state, status, probe(status, 42, "boot-start-1"), at);
        assert_eq!(
            serde_json::to_value(outcome).unwrap()["type"],
            "session_liveness"
        );
        assert_eq!(record(&state)["status"], status);
        assert_eq!(record(&state)["observed_at_ms"], at);
        assert_eq!(record(&state)["generation"], 1);
        assert_eq!(state.leases, before.leases);
        assert_eq!(state.sessions, before.sessions);
        assert_eq!(state.intents, before.intents);
        assert_eq!(state.workspaces, before.workspaces);
    }
    assert_eq!(state.leases["lease"].status, LeaseStatus::Active);
    assert_eq!(state.leases["lease"].expires_at_ms, 1010);
}

#[test]
fn restart_marks_liveness_unknown_and_old_times_remain_generation_scoped() {
    let mut state = ready();
    run(&mut state, "alive", probe("alive", 42, "boot-start-1"), 100);
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert_eq!(record(&state)["status"], "unknown");
    assert_eq!(record(&state)["observed_at_ms"], 100);
    assert_eq!(record(&state)["generation"], 1);
    assert_eq!(record(&state)["process"]["start_token"], "boot-start-1");
    run(
        &mut state,
        "new-probe",
        probe("alive", 42, "boot-start-1"),
        5,
    );
    assert_eq!(record(&state)["status"], "alive");
    assert_eq!(record(&state)["generation"], 2);
    assert_eq!(record(&state)["observed_at_ms"], 5);
    assert_eq!(
        state.sessions["alice"].generation, 1,
        "a probe is not re-registration"
    );
    assert_eq!(state.leases["lease"].status, LeaseStatus::Invalidated);
    let admission = decide(
        &state,
        &Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/other.rs".into()],
            ttl_ms: 1000,
        },
        "after-probe",
        5,
    );
    assert!(matches!(admission.outcome, Outcome::Error { code, .. } if code == "stale_session"));
}

#[test]
fn same_generation_process_replacement_is_an_explicit_mismatch() {
    let mut state = ready();
    run(&mut state, "alive", probe("alive", 42, "boot-start-1"), 20);
    let before = state.clone();
    for (pid, start) in [(42, "boot-start-2"), (43, "boot-start-1")] {
        let decision = decide(&state, &probe("alive", pid, start), "replacement", 30);
        assert!(
            matches!(decision.outcome, Outcome::Error { code, .. } if code == "process_identity_mismatch")
        );
        assert!(decision.events.is_empty());
        assert_eq!(state, before);
    }
}

#[test]
fn out_of_order_same_generation_probe_cannot_replace_newer_evidence() {
    let mut state = ready();
    run(&mut state, "alive", probe("alive", 42, "boot-start-1"), 20);
    let decision = decide(
        &state,
        &probe("exited", 42, "boot-start-1"),
        "old-probe",
        19,
    );
    assert!(
        matches!(decision.outcome, Outcome::Error { code, .. } if code == "stale_liveness_observation")
    );
    assert!(decision.events.is_empty());
    assert_eq!(record(&state)["status"], "alive");
}

#[test]
fn malformed_process_identity_is_rejected_without_events() {
    let state = ready();
    for (pid, start) in [
        (0, "boot-start".into()),
        (42, String::new()),
        (42, "bad/start".into()),
        (42, "x".repeat(129)),
    ] {
        let decision = decide(&state, &probe("alive", pid, &start), "invalid", 20);
        assert!(matches!(decision.outcome, Outcome::Error { .. }));
        assert!(decision.events.is_empty());
    }
    let mut unknown = serde_json::to_value(probe("alive", 42, "boot-start")).unwrap();
    unknown["session_id"] = json!("missing");
    let decision = decide(
        &state,
        &serde_json::from_value(unknown).unwrap(),
        "unknown-session",
        20,
    );
    assert!(matches!(decision.outcome, Outcome::Error { code, .. } if code == "session_not_found"));
}

#[test]
fn liveness_events_replay_and_reject_wrong_generations() {
    let state = ready();
    let decision = decide(&state, &probe("alive", 42, "boot-start-1"), "alive", 20);
    let event = &decision.events[0];
    let mut replayed = state.clone();
    let encoded = serde_json::to_string(event).unwrap();
    apply(&mut replayed, &serde_json::from_str(&encoded).unwrap()).unwrap();
    assert_eq!(record(&replayed)["status"], "alive");
    let mut invalid = serde_json::to_value(event).unwrap();
    invalid["liveness"]["generation"] = json!(2);
    let mut unchanged = state.clone();
    assert!(apply(&mut unchanged, &serde_json::from_value(invalid).unwrap()).is_err());
    assert_eq!(unchanged, state);
}

#[test]
fn registration_and_legacy_state_json_make_no_liveness_claim() {
    let state = ready();
    let encoded = serde_json::to_string(&state).unwrap();
    assert!(!encoded.contains("session_liveness"));
    let decoded: State = serde_json::from_str(&encoded).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
}

#[test]
fn liveness_command_has_a_stable_strict_wire_shape() {
    let wire = r#"{"op":"observe_session_liveness","session_id":"alice","process":{"pid":42,"start_token":"boot-start-1"},"status":"alive"}"#;
    let parsed: Command = serde_json::from_str(wire).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), wire);
    let extra = wire.replace("\"pid\":42", "\"pid\":42,\"allow\":true");
    assert!(serde_json::from_str::<Command>(&extra).is_err());
}
