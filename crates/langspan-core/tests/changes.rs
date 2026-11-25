use langspan_core::{Command, Outcome, State, decide};

#[test]
fn changes_read_contract_has_a_stable_strict_wire_shape() {
    let wire = r#"{"op":"changes","workspace_id":"work","after":17,"limit":25}"#;
    let command: Command = serde_json::from_str(wire).unwrap();
    assert_eq!(serde_json::to_string(&command).unwrap(), wire);
    let extra = wire.replace("\"limit\":25", "\"limit\":25,\"allow\":true");
    assert!(serde_json::from_str::<Command>(&extra).is_err());
}

#[test]
fn changes_reads_are_owned_by_storage_and_emit_no_authority_events() {
    let state = State::default();
    let before = state.clone();
    let command: Command =
        serde_json::from_str(r#"{"op":"changes","workspace_id":"work","after":0,"limit":25}"#)
            .unwrap();
    let decision = decide(&state, &command, "read-changes", 10);
    assert!(matches!(decision.outcome, Outcome::Error { code, .. } if code == "storage_operation"));
    assert!(decision.events.is_empty());
    assert_eq!(state, before);
}

#[test]
fn changes_outcome_distinguishes_no_scan_from_an_empty_completed_scan() {
    let wires = [
        r#"{"type":"changes","events":[],"scan":null}"#,
        r#"{"type":"changes","events":[],"scan":{"workspace_id":"work","observed_at_ms":15,"generation":2,"file_count":0,"branch_count":0,"head_oid":null,"branch_ref":null}}"#,
    ];
    for wire in wires {
        let outcome: Outcome = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&outcome).unwrap(), wire);
    }
}
