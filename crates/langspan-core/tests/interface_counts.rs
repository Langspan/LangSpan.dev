//! Interface changes that only alter how many definitions share a name
//! (issue #118): an addition or a removal with both counts, checked at the
//! core's edge, while older records without counts stay readable.
use langspan_core::{
    Command, Event, InterfaceChange, MAX_DEFINITION_COUNT, Outcome, State, apply, decide,
};

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

fn change(label: &str, before: Option<&str>, after: Option<&str>) -> InterfaceChange {
    InterfaceChange {
        name: "text".into(),
        kind: "method".into(),
        change: label.into(),
        before: before.map(Into::into),
        after: after.map(Into::into),
        after_name: None,
        before_name: None,
        evidence: None,
        before_path: None,
        before_arity: None,
        after_arity: None,
        before_count: None,
        after_count: None,
    }
}

fn counted(mut change: InterfaceChange, before: u32, after: u32) -> InterfaceChange {
    change.before_count = Some(before);
    change.after_count = Some(after);
    change
}

/// The error code of observing `change`, or `None` when it is recorded.
fn refused(change: InterfaceChange) -> Option<String> {
    let state = ready();
    let decision = decide(
        &state,
        &Command::ObserveInterface {
            workspace_id: "work".into(),
            path: "src/commit.rs".into(),
            changes: vec![change],
            dependents: vec!["src/main.rs".into()],
        },
        "notice",
        10,
    );
    match decision.outcome {
        Outcome::Error { code, .. } => Some(code),
        _ => {
            assert!(
                decision
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::InterfaceChanged { .. })),
                "{:?}",
                decision.events
            );
            None
        }
    }
}

const SIGNATURE: &str = "pub fn text(&self) -> String";

#[test]
fn an_added_or_removed_same_named_definition_is_recorded_with_both_counts() {
    let added = counted(change("definition_added", None, Some(SIGNATURE)), 1, 2);
    assert_eq!(refused(added), None);
    let removed = counted(change("definition_removed", Some(SIGNATURE), None), 2, 1);
    assert_eq!(refused(removed), None);
    let many = counted(
        change("definition_added", None, Some(SIGNATURE)),
        1,
        MAX_DEFINITION_COUNT,
    );
    assert_eq!(refused(many), None);
    // The kinds recorded before counts existed are still accepted as they were.
    assert_eq!(refused(change("removed", None, None)), None);
    assert_eq!(
        refused(change(
            "signature_changed",
            Some(SIGNATURE),
            Some(SIGNATURE)
        )),
        None
    );
}

#[test]
fn a_count_change_without_its_counts_or_in_the_wrong_direction_is_refused() {
    let invalid = Some("invalid_notice".to_owned());
    let added = || change("definition_added", None, Some(SIGNATURE));
    let removed = || change("definition_removed", Some(SIGNATURE), None);
    for bad in [
        added(),
        removed(),
        counted(added(), 2, 2),
        counted(added(), 2, 1),
        counted(added(), 0, 1),
        counted(added(), 1, MAX_DEFINITION_COUNT + 1),
        counted(removed(), 1, 2),
        counted(removed(), 1, 0),
        counted(removed(), MAX_DEFINITION_COUNT + 1, 1),
        // The signatures that came go in `after`, those that went in `before`.
        counted(change("definition_added", Some(SIGNATURE), None), 1, 2),
        counted(change("definition_removed", None, Some(SIGNATURE)), 2, 1),
        // Only a count change has counts, and it has both.
        counted(
            change("signature_changed", Some(SIGNATURE), Some(SIGNATURE)),
            1,
            2,
        ),
        counted(change("removed", None, None), 2, 1),
        InterfaceChange {
            before_count: Some(1),
            ..added()
        },
        InterfaceChange {
            after_count: Some(1),
            ..removed()
        },
    ] {
        assert_eq!(refused(bad.clone()), invalid, "{bad:?}");
    }
}

#[test]
fn a_stored_change_from_before_counts_reads_without_them_and_new_ones_round_trip() {
    let old: InterfaceChange = serde_json::from_str(
        r#"{"name":"text","kind":"method","change":"signature_changed","before":"a","after":"b"}"#,
    )
    .unwrap();
    assert_eq!(old.before_count, None);
    assert_eq!(old.after_count, None);
    let text = serde_json::to_string(&old).unwrap();
    assert!(!text.contains("count"), "{text}");

    let added = counted(change("definition_added", None, Some(SIGNATURE)), 1, 2);
    let text = serde_json::to_string(&added).unwrap();
    assert!(
        text.contains(r#""before_count":1"#) && text.contains(r#""after_count":2"#),
        "{text}"
    );
    assert_eq!(
        serde_json::from_str::<InterfaceChange>(&text).unwrap(),
        added
    );
}
