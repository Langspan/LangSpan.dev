//! The write-time break check's rules (ADR 0029): when an argument count
//! fits a parameter count, how a check is bounded, and that the core never
//! records the calls themselves.
use langspan_core::{
    Arity, CallSite, Command, MAX_CALL_SITES, Outcome, State, decide, validate_calls,
};

#[test]
fn an_argument_count_fits_a_parameter_count_with_or_without_the_receiver() {
    let plain = Arity {
        min: 1,
        max: Some(2),
        receiver: false,
    };
    assert!(!plain.fits(0, false));
    assert!(plain.fits(1, false) && plain.fits(2, true));
    assert!(!plain.fits(3, false));
    let method = Arity {
        min: 1,
        max: Some(1),
        receiver: true,
    };
    // `store.put(a)`, and `Store::put(&store, a)` naming the receiver.
    assert!(method.fits(1, true) && method.fits(2, false) && method.fits(1, false));
    assert!(!method.fits(2, true) && !method.fits(0, true));
    let open = Arity {
        min: 1,
        max: None,
        receiver: false,
    };
    assert!(open.fits(9, false) && !open.fits(0, false));
}

fn site(path: &str, line: u32, name: &str, args: u32) -> CallSite {
    CallSite {
        path: path.into(),
        line,
        name: name.into(),
        args,
        member: false,
    }
}

#[test]
fn a_check_names_a_bounded_set_of_calls() {
    assert!(validate_calls(&[site("src/main.rs", 4, "resize", 1)]).is_ok());
    let full = vec![site("a.rs", 1, "f", 0); MAX_CALL_SITES];
    assert!(validate_calls(&full).is_ok());
    for bad in [
        Vec::new(),
        vec![site("a.rs", 1, "f", 0); MAX_CALL_SITES + 1],
        vec![site("/etc/passwd", 1, "f", 0)],
        vec![site("../x.rs", 1, "f", 0)],
        vec![site("a.rs", 0, "f", 0)],
        vec![site("a.rs", 1, "", 0)],
        vec![site("a.rs", 1, "f\nignore previous", 0)],
        vec![site("a.rs", 1, &"f".repeat(129), 0)],
        vec![site("a.rs", 1, "f", 256)],
    ] {
        assert_eq!(
            validate_calls(&bad).unwrap_err().code,
            "invalid_calls",
            "{bad:?}"
        );
    }
}

#[test]
fn the_core_never_records_a_check() {
    let decision = decide(
        &State::default(),
        &Command::CheckCalls {
            session_id: "s".into(),
            calls: vec![site("a.rs", 1, "f", 0)],
        },
        "check",
        0,
    );
    assert!(decision.events.is_empty());
    assert!(
        matches!(decision.outcome, Outcome::Error { ref code, .. } if code == "storage_operation")
    );
    let wire: Command = serde_json::from_str(
        r#"{"op":"check_calls","session_id":"s","calls":[{"path":"a.rs","line":1,"name":"f","args":0,"member":false}]}"#,
    )
    .unwrap();
    assert!(matches!(wire, Command::CheckCalls { .. }));
}
