//! The read-only code-context query: its bounds and its wire form.
use langspan_core::{
    CONTEXT_DEFINITIONS, CONTEXT_NAMES, Command, Outcome, State, decide, is_graph_name,
    validate_graph_context,
};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn a_name_is_an_ascii_identifier_of_3_to_64_bytes() {
    for good in ["upload", "_private", "HTTPClient", "a_b", &"x".repeat(64)] {
        assert!(is_graph_name(good), "{good}");
    }
    for bad in [
        "",
        "ab",
        "9lives",
        "has space",
        "a::b",
        "a.b",
        "café",
        "up-load",
        &"x".repeat(65),
    ] {
        assert!(!is_graph_name(bad), "{bad:?}");
    }
}

#[test]
fn names_and_limit_are_bounded() {
    assert!(validate_graph_context(&names(&["upload", "gate"]), 3).is_ok());
    let full: Vec<String> = (0..CONTEXT_NAMES).map(|n| format!("name{n}")).collect();
    assert!(validate_graph_context(&full, CONTEXT_DEFINITIONS).is_ok());

    let too_many: Vec<String> = (0..=CONTEXT_NAMES).map(|n| format!("name{n}")).collect();
    for (list, limit) in [
        (Vec::new(), 3),
        (too_many, 3),
        (names(&["upload", "upload"]), 3),
        (names(&["upload", "no good"]), 3),
        (names(&["upload"]), 0),
        (names(&["upload"]), CONTEXT_DEFINITIONS + 1),
    ] {
        let error = validate_graph_context(&list, limit).unwrap_err();
        assert_eq!(error.code, "invalid_graph_context", "{list:?} {limit}");
        assert!(error.message.contains("12"), "{}", error.message);
    }
}

#[test]
fn the_core_never_records_a_code_context_query() {
    let decision = decide(
        &State::default(),
        &Command::GraphContext {
            workspace_id: "work".into(),
            names: names(&["upload"]),
            limit: 3,
        },
        "read",
        0,
    );
    assert!(decision.events.is_empty());
    assert!(
        matches!(&decision.outcome, Outcome::Error { code, .. } if code == "storage_operation"),
        "{:?}",
        decision.outcome
    );
}

#[test]
fn the_wire_form_is_strict() {
    let wire = r#"{"op":"graph_context","workspace_id":"work","names":["upload"],"limit":3}"#;
    let command: Command = serde_json::from_str(wire).unwrap();
    assert_eq!(serde_json::to_string(&command).unwrap(), wire);
    let extra = wire.replace(r#""limit":3"#, r#""limit":3,"prompt":"x""#);
    assert!(serde_json::from_str::<Command>(&extra).is_err());
}
