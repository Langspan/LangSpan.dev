use langspan_core::{
    Command, Conflict, Decision, Event, Intent, Lease, LeaseStatus, Outcome, Session, State,
    ToolIdentity, Workspace, apply, decide, expiry_events, validate_request_id,
};

fn paths(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}

fn booted() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).expect("first boot");
    state
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

fn workspace(state: &mut State, id: &str, root: &str) {
    let decision = run(
        state,
        Command::WorkspaceRegister {
            workspace_id: id.into(),
            root: root.into(),
        },
        &format!("workspace-{id}"),
        0,
    );
    assert!(
        matches!(decision.outcome, Outcome::Workspace { .. }),
        "{decision:?}"
    );
}

fn session(state: &mut State, id: &str, workspace_id: &str) {
    let decision = run(
        state,
        Command::SessionRegister {
            session_id: id.into(),
            workspace_id: workspace_id.into(),
            parent: None,
            headless: false,
        },
        &format!("session-{id}"),
        0,
    );
    assert!(
        matches!(decision.outcome, Outcome::Session { .. }),
        "{decision:?}"
    );
}

fn ready() -> State {
    let mut state = booted();
    workspace(&mut state, "work", "/repos/work");
    session(&mut state, "alice", "work");
    session(&mut state, "bob", "work");
    state
}

fn admit(session_id: &str, values: &[&str], ttl_ms: u64) -> Command {
    Command::Admit {
        session_id: session_id.into(),
        paths: paths(values),
        ttl_ms,
    }
}

fn complete(session_id: &str, lease_id: &str) -> Command {
    Command::Complete {
        session_id: session_id.into(),
        lease_id: lease_id.into(),
    }
}

fn error_code(decision: &Decision) -> &str {
    assert!(decision.events.is_empty(), "errors cannot mutate state");
    match &decision.outcome {
        Outcome::Error { code, .. } => code,
        outcome => panic!("expected error, got {outcome:?}"),
    }
}

#[test]
fn registration_emits_replayable_event_without_mutating_input() {
    let state = booted();
    let decision = decide(
        &state,
        &Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        "register-work",
        0,
    );
    let expected = Workspace {
        workspace_id: "work".into(),
        root: "/repos/work".into(),
        verified_overlaps: Vec::new(),
    };
    assert_eq!(
        decision.events,
        vec![Event::WorkspaceRegistered {
            workspace: expected.clone()
        }]
    );
    assert_eq!(
        decision.outcome,
        Outcome::Workspace {
            workspace: expected.clone()
        }
    );
    assert!(state.workspaces.is_empty());
    let mut replay = state;
    apply(&mut replay, &decision.events[0]).expect("valid registration");
    assert_eq!(replay.workspaces.get("work"), Some(&expected));
}

#[test]
fn all_or_none_admission_does_not_reserve_nonconflicting_subset() {
    let mut state = ready();
    assert!(matches!(
        run(&mut state, admit("alice", &["a.rs"], 500), "lease-a", 100).outcome,
        Outcome::Admitted { .. }
    ));
    let before = state.clone();
    let blocked = run(
        &mut state,
        admit("bob", &["b.rs", "a.rs"], 500),
        "lease-b",
        100,
    );
    assert_eq!(
        blocked.outcome,
        Outcome::Conflict {
            holders: vec![Conflict {
                lease_id: "lease-a".into(),
                session_id: "alice".into(),
                paths: paths(&["a.rs"]),
                regions: Vec::new(),
            }]
        }
    );
    // Only the refusal is recorded (for `langspan today`); nothing is held.
    assert!(only_refused(&blocked.events), "{:?}", blocked.events);
    assert_eq!(state, before);
    assert!(matches!(
        run(&mut state, admit("alice", &["b.rs"], 500), "lease-c", 100).outcome,
        Outcome::Admitted { .. }
    ));
}

#[test]
fn intentions_are_visible_but_do_not_reserve_files() {
    let mut state = ready();
    run(
        &mut state,
        Command::IntentSet {
            session_id: "alice".into(),
            paths: paths(&["src/été.rs"]),
            summary: "Update parsing".into(),
        },
        "intent-a",
        0,
    );
    assert!(state.leases.is_empty());
    assert!(matches!(
        run(&mut state, admit("bob", &["src/été.rs"], 50), "lease-b", 10).outcome,
        Outcome::Admitted { .. }
    ));
    assert_eq!(state.intents["alice"].summary, "Update parsing");
}

#[test]
fn independent_workspaces_do_not_share_file_ownership() {
    let mut state = ready();
    workspace(&mut state, "other", "/repos/other");
    session(&mut state, "carol", "other");
    run(&mut state, admit("alice", &["same.rs"], 50), "lease-a", 0);
    assert!(matches!(
        run(&mut state, admit("carol", &["same.rs"], 50), "lease-c", 0).outcome,
        Outcome::Admitted { .. }
    ));
}

#[test]
fn a_sessions_own_active_lease_conflicts_with_a_new_acquisition() {
    let mut state = ready();
    run(&mut state, admit("alice", &["a.rs"], 10), "lease-a", 0);
    let decision = run(&mut state, admit("alice", &["a.rs"], 10), "lease-a2", 1);
    assert!(matches!(decision.outcome, Outcome::Conflict { .. }));
    assert!(only_refused(&decision.events), "{:?}", decision.events);
}

/// A refused admission records that it was refused and nothing else.
fn only_refused(events: &[Event]) -> bool {
    matches!(
        events,
        [Event::WriteRefused {
            kind: langspan_core::RefusalKind::Conflict,
            ..
        }]
    )
}

#[test]
fn expiration_is_explicit_and_deadline_is_exclusive() {
    let mut state = ready();
    run(&mut state, admit("alice", &["a.rs"], 10), "lease-a", 100);
    assert!(expiry_events(&state, 109).is_empty());
    let events = expiry_events(&state, 110);
    assert_eq!(
        events,
        vec![Event::LeaseExpired {
            lease_id: "lease-a".into()
        }]
    );
    assert_eq!(state.leases["lease-a"].status, LeaseStatus::Active);
    assert_eq!(
        error_code(&decide(
            &state,
            &complete("alice", "lease-a"),
            "finish-a",
            110
        )),
        "lease_expired"
    );
    // Defensive time check permits the new grant even before the expiry event is applied.
    assert!(matches!(
        decide(&state, &admit("bob", &["a.rs"], 10), "lease-b", 110).outcome,
        Outcome::Admitted { .. }
    ));
    apply(&mut state, &events[0]).expect("expiration event");
    assert_eq!(state.leases["lease-a"].status, LeaseStatus::Expired);
    assert!(expiry_events(&state, 110).is_empty());
}

#[test]
fn complete_requires_holder_and_active_unexpired_generation() {
    let mut state = ready();
    run(&mut state, admit("alice", &["a.rs"], 10), "lease-a", 0);
    assert_eq!(
        error_code(&decide(
            &state,
            &complete("bob", "lease-a"),
            "bad-finish",
            1
        )),
        "not_holder"
    );
    assert_eq!(
        run(&mut state, complete("alice", "lease-a"), "finish-a", 9).outcome,
        Outcome::Completed {
            lease_id: "lease-a".into()
        }
    );
    assert_eq!(state.leases["lease-a"].status, LeaseStatus::Completed);
    assert_eq!(
        error_code(&decide(
            &state,
            &complete("alice", "lease-a"),
            "finish-again",
            9
        )),
        "lease_not_active"
    );
    assert!(matches!(
        run(&mut state, admit("bob", &["a.rs"], 10), "lease-b", 9).outcome,
        Outcome::Admitted { .. }
    ));
}

#[test]
fn restart_invalidates_authority_and_retains_stale_intentions() {
    let mut state = ready();
    run(
        &mut state,
        Command::IntentSet {
            session_id: "alice".into(),
            paths: paths(&["a.rs"]),
            summary: "Keep context".into(),
        },
        "intent-a",
        0,
    );
    run(&mut state, admit("alice", &["a.rs"], 100), "lease-a", 0);
    apply(&mut state, &Event::Booted { generation: 2 }).expect("restart");
    assert_eq!(state.leases["lease-a"].status, LeaseStatus::Invalidated);
    assert_eq!(state.sessions["alice"].generation, 1);
    assert_eq!(state.intents["alice"].generation, 1);
    assert_eq!(
        error_code(&decide(
            &state,
            &admit("alice", &["a.rs"], 10),
            "lease-b",
            0
        )),
        "stale_session"
    );
    session(&mut state, "alice", "work");
    assert_eq!(state.sessions["alice"].generation, 2);
    assert_eq!(state.intents["alice"].generation, 1);
    assert_eq!(
        error_code(&decide(
            &state,
            &complete("alice", "lease-a"),
            "finish-a",
            0
        )),
        "stale_lease"
    );
    run(
        &mut state,
        Command::IntentSet {
            session_id: "alice".into(),
            paths: paths(&["a.rs"]),
            summary: "Keep context".into(),
        },
        "intent-new",
        0,
    );
    assert_eq!(state.intents["alice"].generation, 2);
    assert!(matches!(
        run(&mut state, admit("alice", &["a.rs"], 10), "lease-new", 0).outcome,
        Outcome::Admitted { .. }
    ));
}

#[test]
fn completed_and_expired_history_survives_restart() {
    let mut state = ready();
    run(&mut state, admit("alice", &["a.rs"], 10), "done", 0);
    run(&mut state, complete("alice", "done"), "complete-done", 1);
    run(&mut state, admit("alice", &["a.rs"], 1), "old", 1);
    for event in expiry_events(&state, 2) {
        apply(&mut state, &event).expect("expire");
    }
    apply(&mut state, &Event::Booted { generation: 2 }).expect("boot");
    assert_eq!(state.leases["done"].status, LeaseStatus::Completed);
    assert_eq!(state.leases["old"].status, LeaseStatus::Expired);
}

#[test]
fn duplicate_paths_are_sorted_without_losing_unicode() {
    let mut state = ready();
    let decision = run(
        &mut state,
        admit("alice", &["z.rs", "été/文件.rs", "a.rs", "z.rs"], 1),
        "lease-a",
        0,
    );
    let Outcome::Admitted { lease } = decision.outcome else {
        panic!("expected grant")
    };
    assert_eq!(lease.paths, paths(&["a.rs", "z.rs", "été/文件.rs"]));
    assert_eq!(lease.lease_id, "lease-a");
}

#[test]
fn malformed_paths_and_excessive_sizes_are_rejected_without_events() {
    let state = ready();
    for path in [
        "", "/a", "a/", "a//b", ".", "..", "a/./b", "a/../b", "a\\b", "a\0b",
    ] {
        assert_eq!(
            error_code(&decide(&state, &admit("alice", &[path], 1), "req", 0)),
            "invalid_path",
            "{path:?}"
        );
    }
    assert_eq!(
        error_code(&decide(&state, &admit("alice", &[], 1), "req", 0)),
        "invalid_path"
    );
    let large = "x".repeat(513);
    assert_eq!(
        error_code(&decide(&state, &admit("alice", &[&large], 1), "req", 0)),
        "invalid_path"
    );
    let too_many = Command::Admit {
        session_id: "alice".into(),
        paths: (0..65).map(|n| format!("f{n}")).collect(),
        ttl_ms: 1,
    };
    assert_eq!(
        error_code(&decide(&state, &too_many, "req", 0)),
        "too_many_paths"
    );
    let duplicates = Command::Admit {
        session_id: "alice".into(),
        paths: vec!["a.rs".into(); 65],
        ttl_ms: 1,
    };
    assert!(matches!(
        decide(&state, &duplicates, "req", 0).outcome,
        Outcome::Admitted { .. }
    ));
    let maximum = "x".repeat(512);
    assert!(matches!(
        decide(&state, &admit("alice", &[&maximum], 1), "req", 0).outcome,
        Outcome::Admitted { .. }
    ));
}

#[test]
fn identifiers_have_exact_ascii_grammar_and_length_bounds() {
    assert!(validate_request_id("").is_err());
    assert!(validate_request_id(&"x".repeat(129)).is_err());
    assert!(validate_request_id(&"x".repeat(128)).is_ok());
    assert!(validate_request_id("été").is_err());
    for byte in 0_u8..=127 {
        let id = char::from(byte).to_string();
        assert_eq!(
            validate_request_id(&id).is_ok(),
            byte.is_ascii_alphanumeric() || b"._-".contains(&byte),
            "byte {byte}"
        );
    }
    let state = ready();
    assert_eq!(
        error_code(&decide(&state, &Command::Status, "bad request", 0)),
        "invalid_request_id"
    );
    assert_eq!(
        error_code(&decide(
            &state,
            &admit("bad/session", &["a.rs"], 1),
            "req",
            0
        )),
        "invalid_id"
    );
}

#[test]
fn ttl_and_summary_bounds_include_overflow() {
    let state = ready();
    for ttl in [0, 60_001, u64::MAX] {
        assert_eq!(
            error_code(&decide(&state, &admit("alice", &["a.rs"], ttl), "req", 0)),
            "invalid_ttl"
        );
    }
    assert!(matches!(
        decide(&state, &admit("alice", &["a.rs"], 60_000), "req", 0).outcome,
        Outcome::Admitted { .. }
    ));
    assert_eq!(
        error_code(&decide(
            &state,
            &admit("alice", &["a.rs"], 1),
            "req",
            u64::MAX
        )),
        "invalid_deadline"
    );
    for (length, expected_ok) in [(2048, true), (2049, false)] {
        let decision = decide(
            &state,
            &Command::IntentSet {
                session_id: "alice".into(),
                paths: paths(&["a.rs"]),
                summary: "x".repeat(length),
            },
            "req",
            0,
        );
        assert_eq!(
            matches!(decision.outcome, Outcome::Intent { .. }),
            expected_ok
        );
    }
}

#[test]
fn workspace_and_session_bindings_cannot_change_or_alias() {
    let mut state = ready();
    for (id, root) in [
        ("work", "/repos/different"),
        ("alias", "/repos/work"),
        ("nested", "/repos/work/nested"),
        ("parent", "/repos"),
    ] {
        let decision = decide(
            &state,
            &Command::WorkspaceRegister {
                workspace_id: id.into(),
                root: root.into(),
            },
            "req",
            0,
        );
        assert_eq!(error_code(&decision), "workspace_conflict");
    }
    workspace(&mut state, "sibling", "/repos/work-copy");
    let same = run(
        &mut state,
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        "again",
        0,
    );
    assert!(same.events.is_empty());
    assert_eq!(
        error_code(&decide(
            &state,
            &Command::SessionRegister {
                session_id: "alice".into(),
                workspace_id: "sibling".into(),
                parent: None,
                headless: false,
            },
            "rebind",
            0
        )),
        "session_conflict"
    );
}

#[test]
fn ended_intent_does_not_complete_lease_and_lease_ids_cannot_be_reused() {
    let mut state = ready();
    run(
        &mut state,
        Command::IntentSet {
            session_id: "alice".into(),
            paths: paths(&["a.rs"]),
            summary: String::new(),
        },
        "intent",
        0,
    );
    run(&mut state, admit("alice", &["a.rs"], 10), "lease", 0);
    run(
        &mut state,
        Command::IntentEnd {
            session_id: "alice".into(),
        },
        "end",
        0,
    );
    assert!(state.intents.is_empty());
    assert_eq!(state.leases["lease"].status, LeaseStatus::Active);
    run(&mut state, complete("alice", "lease"), "finish", 1);
    assert_eq!(
        error_code(&decide(&state, &admit("alice", &["b.rs"], 10), "lease", 1)),
        "lease_id_reused"
    );
}

#[test]
fn conflict_details_and_expiration_order_are_deterministic() {
    let mut state = ready();
    run(
        &mut state,
        admit("alice", &["b.rs", "a.rs"], 10),
        "z-lease",
        0,
    );
    run(&mut state, admit("alice", &["c.rs"], 10), "a-lease", 0);
    let decision = decide(
        &state,
        &admit("bob", &["c.rs", "b.rs", "a.rs"], 10),
        "new",
        0,
    );
    assert_eq!(
        decision.outcome,
        Outcome::Conflict {
            holders: vec![
                Conflict {
                    lease_id: "a-lease".into(),
                    session_id: "alice".into(),
                    paths: paths(&["c.rs"]),
                    regions: Vec::new(),
                },
                Conflict {
                    lease_id: "z-lease".into(),
                    session_id: "alice".into(),
                    paths: paths(&["a.rs", "b.rs"]),
                    regions: Vec::new(),
                },
            ]
        }
    );
    assert_eq!(
        expiry_events(&state, 10),
        vec![
            Event::LeaseExpired {
                lease_id: "a-lease".into()
            },
            Event::LeaseExpired {
                lease_id: "z-lease".into()
            }
        ]
    );
}

#[test]
fn exact_event_stream_replays_deterministically_through_json() {
    let mut state = State::default();
    let mut log = vec![Event::Booted { generation: 1 }];
    apply(&mut state, &log[0]).expect("boot");
    let commands = [
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        Command::SessionRegister {
            session_id: "alice".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        Command::IntentSet {
            session_id: "alice".into(),
            paths: paths(&["a.rs"]),
            summary: "change".into(),
        },
        admit("alice", &["a.rs"], 10),
        complete("alice", "op-3"),
        Command::IntentEnd {
            session_id: "alice".into(),
        },
    ];
    for (n, command) in commands.into_iter().enumerate() {
        let decision = run(&mut state, command, &format!("op-{n}"), n as u64);
        assert!(
            !matches!(decision.outcome, Outcome::Error { .. }),
            "{decision:?}"
        );
        log.extend(decision.events);
    }
    assert_eq!(log.len(), 7);
    let bytes = serde_json::to_vec(&log).expect("serialize log");
    let decoded: Vec<Event> = serde_json::from_slice(&bytes).expect("decode log");
    let mut replay = State::default();
    for event in &decoded {
        apply(&mut replay, event).expect("replay");
    }
    assert_eq!(replay, state);
    assert_eq!(
        serde_json::to_vec(&replay).expect("serialize state"),
        serde_json::to_vec(&state).expect("serialize state")
    );
}

#[test]
fn corrupt_events_are_rejected_without_partially_mutating_state() {
    let mut state = ready();
    let invalid = [
        Event::Booted { generation: 1 },
        Event::SessionRegistered {
            session: Session {
                session_id: "unknown".into(),
                workspace_id: "missing".into(),
                generation: 1,
                parent: None,
                headless: false,
            },
        },
        Event::IntentSet {
            intent: Intent {
                session_id: "alice".into(),
                workspace_id: "work".into(),
                paths: paths(&["b.rs", "a.rs"]),
                summary: String::new(),
                generation: 1,
            },
        },
        Event::LeaseGranted {
            lease: Lease {
                lease_id: "bad".into(),
                session_id: "alice".into(),
                workspace_id: "work".into(),
                paths: paths(&["a.rs"]),
                generation: 0,
                expires_at_ms: 10,
                status: LeaseStatus::Active,
                operation: None,
                base_unknown: Vec::new(),
                regions: Vec::new(),
            },
        },
        Event::LeaseCompleted {
            lease_id: "missing".into(),
        },
        Event::LeaseExpired {
            lease_id: "missing".into(),
        },
    ];
    for event in invalid {
        let before = state.clone();
        assert!(apply(&mut state, &event).is_err(), "{event:?}");
        assert_eq!(state, before);
    }
}

#[test]
fn wire_contract_is_tagged_and_rejects_unknown_fields() {
    assert_eq!(
        serde_json::to_value(admit("alice", &["a.rs"], 10)).expect("serialize"),
        serde_json::json!({"op":"admit","session_id":"alice","paths":["a.rs"],"ttl_ms":10})
    );
    assert_eq!(
        serde_json::to_value(Event::Booted { generation: 1 }).expect("serialize"),
        serde_json::json!({"type":"booted","generation":1})
    );
    assert_eq!(
        serde_json::to_value(LeaseStatus::Active).expect("serialize"),
        "active"
    );
    assert!(serde_json::from_str::<Command>(r#"{"op":"status","unexpected":true}"#).is_err());
    assert!(
        serde_json::from_str::<Event>(r#"{"type":"booted","generation":1,"unexpected":true}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<Workspace>(
            r#"{"workspace_id":"work","root":"/work","unexpected":true}"#
        )
        .is_err()
    );
}

#[test]
fn bounded_action_sequences_preserve_exclusive_current_authority() {
    fn explore(state: &State, depth: u32, path: &str) {
        if depth == 4 {
            return;
        }
        let now = u64::from(depth);
        for action in 0..9 {
            let mut next = state.clone();
            for event in expiry_events(&next, now) {
                apply(&mut next, &event).expect("expire before next action");
            }
            let id = format!("{path}-{action}");
            if action == 5 {
                let generation = next.generation + 1;
                apply(&mut next, &Event::Booted { generation }).expect("advance generation");
            } else {
                let command = match action {
                    0 => admit("alice", &["a.rs"], 2),
                    1 => admit("alice", &["a.rs", "b.rs"], 3),
                    2 => admit("bob", &["a.rs"], 3),
                    3 => admit("bob", &["b.rs"], 2),
                    4 => complete(
                        "alice",
                        next.leases
                            .keys()
                            .next()
                            .map(String::as_str)
                            .unwrap_or("missing"),
                    ),
                    6 => Command::SessionRegister {
                        session_id: "alice".into(),
                        workspace_id: "work".into(),
                        parent: None,
                        headless: false,
                    },
                    7 => Command::SessionRegister {
                        session_id: "bob".into(),
                        workspace_id: "work".into(),
                        parent: None,
                        headless: false,
                    },
                    _ => Command::IntentSet {
                        session_id: "bob".into(),
                        paths: paths(&["a.rs"]),
                        summary: "context".into(),
                    },
                };
                run(&mut next, command, &id, now);
            }
            let valid: Vec<_> = next
                .leases
                .values()
                .filter(|lease| {
                    lease.status == LeaseStatus::Active
                        && lease.generation == next.generation
                        && lease.expires_at_ms > now
                })
                .collect();
            for (i, first) in valid.iter().enumerate() {
                for second in valid.iter().skip(i + 1) {
                    assert!(
                        first.workspace_id != second.workspace_id
                            || !first.paths.iter().any(|path| second.paths.contains(path)),
                        "two live grants overlap after {id}: {first:?} and {second:?}"
                    );
                }
            }
            explore(&next, depth + 1, &id);
        }
    }
    explore(&ready(), 0, "sequence");
}

#[test]
fn a_sessions_tool_call_is_never_denied_by_its_own_earlier_tool_call() {
    // Found by the scenario benchmark: a refused permission prompt leaves
    // a pre-tool admission without a post event, and the session's next tool
    // call on the same file was denied as if another session held it.
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    for (id, command) in [
        (
            "w",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/repos/work".into(),
            },
        ),
        (
            "s1",
            Command::SessionRegister {
                session_id: "alice".into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
        ),
        (
            "s2",
            Command::SessionRegister {
                session_id: "bob".into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
        ),
    ] {
        for event in decide(&state, &command, id, 0).events {
            apply(&mut state, &event).unwrap();
        }
    }
    let tool = |state: &mut State, session: &str, id: &str| {
        let decision = decide(
            state,
            &Command::AdmitTool {
                session_id: session.into(),
                paths: vec!["a.rs".into()],
                ttl_ms: 30_000,
                operation: ToolIdentity {
                    operation_id: format!("op-{id}"),
                    source: "test".into(),
                    tool: "Edit".into(),
                    input_digest: "a".repeat(64),
                },
                base: Vec::new(),
                regions: Vec::new(),
                overwrites: Vec::new(),
            },
            id,
            10,
        );
        for event in &decision.events {
            apply(state, event).unwrap();
        }
        decision.outcome
    };
    assert!(matches!(
        tool(&mut state, "alice", "a1"),
        Outcome::Admitted { .. }
    ));
    assert!(
        matches!(tool(&mut state, "alice", "a2"), Outcome::Admitted { .. }),
        "the same session's next tool call is admitted"
    );
    let Outcome::Conflict { holders } = tool(&mut state, "bob", "b1") else {
        panic!("another session is still held back");
    };
    assert_eq!(holders.len(), 2, "both of alice's admissions are reported");
}
