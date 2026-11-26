//! A renamed function in an interface notice (roadmap B10): the old name in
//! `name`, the new one in `after_name`, both signatures, and no counts,
//! checked at the core's edge.
use langspan_core::{Command, Event, InterfaceChange, Outcome, State, apply, decide};

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    let decision = decide(
        &state,
        &Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        "w",
        0,
    );
    for event in decision.events {
        apply(&mut state, &event).unwrap();
    }
    state
}

fn renamed() -> InterfaceChange {
    InterfaceChange {
        name: "fetch_user".into(),
        kind: "function".into(),
        change: "renamed".into(),
        before: Some("def fetch_user(id)".into()),
        after: Some("def load_user(id)".into()),
        after_name: Some("load_user".into()),
        before_arity: None,
        after_arity: None,
        before_count: None,
        after_count: None,
    }
}

fn refused(change: InterfaceChange) -> Option<String> {
    let decision = decide(
        &ready(),
        &Command::ObserveInterface {
            workspace_id: "work".into(),
            path: "app/users.py".into(),
            changes: vec![change],
            dependents: vec![],
        },
        "notice",
        10,
    );
    match decision.outcome {
        Outcome::Error { code, .. } => Some(code),
        _ => None,
    }
}

#[test]
fn a_rename_names_the_new_name_and_both_signatures() {
    assert_eq!(refused(renamed()), None);
    let invalid = Some("invalid_notice".to_owned());
    for bad in [
        InterfaceChange {
            after_name: None,
            ..renamed()
        },
        InterfaceChange {
            after_name: Some(String::new()),
            ..renamed()
        },
        InterfaceChange {
            after_name: Some("load\nuser".into()),
            ..renamed()
        },
        InterfaceChange {
            after_name: Some("x".repeat(129)),
            ..renamed()
        },
        InterfaceChange {
            after_name: Some("fetch_user".into()),
            ..renamed()
        },
        InterfaceChange {
            after: None,
            ..renamed()
        },
        InterfaceChange {
            before: None,
            ..renamed()
        },
        InterfaceChange {
            before_count: Some(1),
            after_count: Some(2),
            ..renamed()
        },
        // Only a rename has a new name.
        InterfaceChange {
            change: "removed".into(),
            before: None,
            after: None,
            ..renamed()
        },
        InterfaceChange {
            change: "signature_changed".into(),
            ..renamed()
        },
    ] {
        assert_eq!(refused(bad.clone()), invalid, "{bad:?}");
    }
}

#[test]
fn a_stored_change_without_a_new_name_reads_and_a_rename_round_trips() {
    let old: InterfaceChange = serde_json::from_str(
        r#"{"name":"text","kind":"method","change":"removed","before":null,"after":null}"#,
    )
    .unwrap();
    assert_eq!(old.after_name, None);
    assert!(!serde_json::to_string(&old).unwrap().contains("after_name"));
    let text = serde_json::to_string(&renamed()).unwrap();
    assert!(text.contains(r#""after_name":"load_user""#), "{text}");
    assert_eq!(
        serde_json::from_str::<InterfaceChange>(&text).unwrap(),
        renamed()
    );
}
