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
            rank: false,
            session_id: None,
            qualifiers: Vec::new(),
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

// Roadmap C2: the fixed ranking rules.

use langspan_core::{
    BRIEF_HORIZON_MS, Dependent, FileState, KnownVersion, Lease, LeaseStatus, LivenessStatus,
    ProcessIdentity, RankReason, Session, SessionActivity, SessionLiveness, SymbolRef, TouchedPath,
    rank_definitions, rank_dependents, rank_inputs, validate_rank,
};

const NOW: u64 = 10 * 60 * 60 * 1000;

fn definition(path: &str, exported: bool) -> SymbolRef {
    SymbolRef {
        path: path.into(),
        name: "upload".into(),
        kind: "function".into(),
        line: 1,
        end_line: 3,
        signature: "fn upload()".into(),
        exported,
        ..SymbolRef::default()
    }
}

fn session(state: &mut State, id: &str) {
    state.sessions.insert(
        id.into(),
        Session {
            session_id: id.into(),
            workspace_id: "work".into(),
            generation: state.generation,
            parent: None,
            headless: false,
        },
    );
    state.activity.insert(
        id.into(),
        SessionActivity {
            session_id: id.into(),
            generation: state.generation,
            last_ms: NOW,
            ..SessionActivity::default()
        },
    );
}

fn touched(state: &mut State, id: &str, path: &str, at_ms: u64) {
    let activity = state.activity.get_mut(id).unwrap();
    activity.touched.push(TouchedPath {
        path: path.into(),
        at_ms,
    });
}

fn read(state: &mut State, id: &str, path: &str) {
    let activity = state.activity.get_mut(id).unwrap();
    activity.known.push(KnownVersion {
        path: path.into(),
        state: FileState::Missing,
        at_ms: NOW,
    });
}

fn world() -> State {
    let mut state = State {
        generation: 1,
        ..State::default()
    };
    session(&mut state, "claude-code.alice");
    session(&mut state, "codex.bob");
    state
}

fn order(definitions: &[SymbolRef]) -> Vec<&str> {
    definitions.iter().map(|d| d.path.as_str()).collect()
}

fn reasons(symbol: &SymbolRef) -> &[RankReason] {
    symbol.rank_reasons.as_deref().unwrap()
}

#[test]
fn a_definition_in_a_file_this_session_edited_ranks_first() {
    let mut state = world();
    touched(&mut state, "claude-code.alice", "c/upload.rs", NOW - 1000);
    let inputs = rank_inputs(&state, "work", Some("claude-code.alice"), &[], NOW);
    let mut found = vec![
        definition("a/upload.rs", false),
        definition("b/upload.rs", false),
        definition("c/upload.rs", false),
    ];
    rank_definitions(&inputs, &mut found);
    assert_eq!(order(&found), ["c/upload.rs", "a/upload.rs", "b/upload.rs"]);
    assert_eq!(reasons(&found[0]), [RankReason::OwnFile { edited: true }]);
    assert!(reasons(&found[1]).is_empty(), "an unranked one says so");

    // A file the session only read ranks the same way, and says it was read.
    let mut state = world();
    read(&mut state, "claude-code.alice", "b/upload.rs");
    let inputs = rank_inputs(&state, "work", Some("claude-code.alice"), &[], NOW);
    rank_definitions(&inputs, &mut found);
    assert_eq!(order(&found), ["b/upload.rs", "a/upload.rs", "c/upload.rs"]);
    assert_eq!(reasons(&found[0]), [RankReason::OwnFile { edited: false }]);
}

#[test]
fn a_live_peers_file_ranks_above_an_unrelated_exported_one_and_names_the_peer() {
    let mut state = world();
    touched(&mut state, "codex.bob", "b/upload.rs", NOW - 1000);
    let inputs = rank_inputs(&state, "work", Some("claude-code.alice"), &[], NOW);
    let mut found = vec![
        definition("a/upload.rs", true),
        definition("b/upload.rs", false),
    ];
    rank_definitions(&inputs, &mut found);
    assert_eq!(order(&found), ["b/upload.rs", "a/upload.rs"]);
    assert_eq!(
        reasons(&found[0]),
        [RankReason::PeerEditing {
            session_id: "codex.bob".into()
        }]
    );
    assert_eq!(reasons(&found[1]), [RankReason::Exported]);

    // A file the peer holds under a live lease counts too.
    let mut held = world();
    held.leases.insert(
        "lease-1".into(),
        Lease {
            lease_id: "lease-1".into(),
            session_id: "codex.bob".into(),
            workspace_id: "work".into(),
            paths: vec!["b/upload.rs".into()],
            generation: 1,
            expires_at_ms: NOW + 1000,
            status: LeaseStatus::Active,
            operation: None,
            base_unknown: Vec::new(),
            regions: Vec::new(),
        },
    );
    let inputs = rank_inputs(&held, "work", Some("claude-code.alice"), &[], NOW);
    let mut found = vec![
        definition("a/upload.rs", true),
        definition("b/upload.rs", false),
    ];
    rank_definitions(&inputs, &mut found);
    assert_eq!(order(&found), ["b/upload.rs", "a/upload.rs"]);
}

#[test]
fn an_ended_exited_stale_or_unrelated_peer_does_not_rank_a_file() {
    let peer_cases: [fn(&mut State); 4] = [
        |state| state.activity.get_mut("codex.bob").unwrap().ended = true,
        |state| {
            state.session_liveness.insert(
                "codex.bob".into(),
                SessionLiveness {
                    session_id: "codex.bob".into(),
                    process: ProcessIdentity {
                        pid: 1,
                        start_token: "t".into(),
                    },
                    status: LivenessStatus::Exited,
                    observed_at_ms: NOW,
                    generation: 1,
                },
            );
        },
        |state| {
            let activity = state.activity.get_mut("codex.bob").unwrap();
            activity.touched[0].at_ms = NOW - BRIEF_HORIZON_MS - 1;
        },
        |state| state.sessions.get_mut("codex.bob").unwrap().workspace_id = "elsewhere".into(),
    ];
    for (case, change) in peer_cases.iter().enumerate() {
        let mut state = world();
        touched(&mut state, "codex.bob", "b/upload.rs", NOW - 1000);
        change(&mut state);
        let inputs = rank_inputs(&state, "work", Some("claude-code.alice"), &[], NOW);
        assert!(inputs.peers.is_empty(), "case {case}: {inputs:?}");
    }
    // The asking session is never its own peer.
    let mut state = world();
    touched(&mut state, "claude-code.alice", "b/upload.rs", NOW - 1000);
    let inputs = rank_inputs(&state, "work", Some("claude-code.alice"), &[], NOW);
    assert!(inputs.peers.is_empty(), "{inputs:?}");
    // Without a session, every live session's work is a peer's.
    let inputs = rank_inputs(&state, "work", None, &[], NOW);
    assert!(inputs.own.is_empty());
    assert_eq!(inputs.peers["b/upload.rs"], "claude-code.alice");
    // The session's own files count only in its own workspace.
    let inputs = rank_inputs(&state, "elsewhere", Some("claude-code.alice"), &[], NOW);
    assert!(inputs.own.is_empty(), "{inputs:?}");
}

#[test]
fn exported_ranks_above_qualified_which_ranks_above_path_order() {
    let state = world();
    let qualifiers = names(&["store"]);
    let inputs = rank_inputs(&state, "work", Some("claude-code.alice"), &qualifiers, NOW);
    let mut found = vec![
        definition("src/api/upload.rs", false),
        definition("src/store/upload.rs", false),
        definition("tools/upload.rs", true),
        definition("src/net/upload.rs", false),
    ];
    rank_definitions(&inputs, &mut found);
    assert_eq!(
        order(&found),
        [
            "tools/upload.rs",
            "src/store/upload.rs",
            "src/api/upload.rs",
            "src/net/upload.rs"
        ]
    );
    assert_eq!(reasons(&found[1]), [RankReason::QualifiedInPrompt]);

    // A qualifier names a directory, a file stem or name, or a path suffix.
    for (qualifier, path) in [
        ("store", "src/store/upload.rs"),
        ("store", "src/store.rs"),
        ("store.rs", "src/store.rs"),
        ("src/store", "src/store/upload.rs"),
        ("store/upload.rs", "src/store/upload.rs"),
    ] {
        let inputs = rank_inputs(&state, "work", None, &names(&[qualifier]), NOW);
        let mut one = vec![definition(path, false)];
        rank_definitions(&inputs, &mut one);
        assert_eq!(
            reasons(&one[0]),
            [RankReason::QualifiedInPrompt],
            "{qualifier} {path}"
        );
    }
    for (qualifier, path) in [
        ("tore", "src/store/upload.rs"),
        ("re/upload.rs", "src/store/upload.rs"),
        ("upload", "src/upload_tests/x.rs"),
    ] {
        let inputs = rank_inputs(&state, "work", None, &names(&[qualifier]), NOW);
        let mut one = vec![definition(path, false)];
        rank_definitions(&inputs, &mut one);
        assert!(reasons(&one[0]).is_empty(), "{qualifier} {path}");
    }
}

#[test]
fn every_reason_is_listed_in_rank_order() {
    let mut state = world();
    touched(&mut state, "claude-code.alice", "src/store/upload.rs", NOW);
    touched(&mut state, "codex.bob", "src/store/upload.rs", NOW);
    let qualifiers = names(&["store"]);
    let inputs = rank_inputs(&state, "work", Some("claude-code.alice"), &qualifiers, NOW);
    let mut found = vec![definition("src/store/upload.rs", true)];
    rank_definitions(&inputs, &mut found);
    assert_eq!(
        reasons(&found[0]),
        [
            RankReason::OwnFile { edited: true },
            RankReason::PeerEditing {
                session_id: "codex.bob".into()
            },
            RankReason::Exported,
            RankReason::QualifiedInPrompt,
        ]
    );
}

#[test]
fn dependents_rank_by_the_same_rules_then_by_mention_count_then_path() {
    let mut state = world();
    read(&mut state, "claude-code.alice", "src/cli.rs");
    touched(&mut state, "codex.bob", "src/api/routes.rs", NOW);
    let qualifiers = names(&["store"]);
    let inputs = rank_inputs(&state, "work", Some("claude-code.alice"), &qualifiers, NOW);
    let files: Vec<(String, u32)> = [
        ("a.rs", 2),
        ("b.rs", 9),
        ("c.rs", 9),
        ("src/api/routes.rs", 1),
        ("src/cli.rs", 1),
        ("src/store/mod.rs", 1),
    ]
    .iter()
    .map(|(path, count)| ((*path).to_owned(), *count))
    .collect();
    let ranked: Vec<Dependent> = rank_dependents(&inputs, &files);
    let paths: Vec<&str> = ranked.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "src/cli.rs",
            "src/api/routes.rs",
            "src/store/mod.rs",
            "b.rs",
            "c.rs",
            "a.rs"
        ]
    );
    assert_eq!(
        ranked[0].rank_reasons,
        [RankReason::OwnFile { edited: false }]
    );
    assert_eq!(ranked[3].mentions, 9);
    assert!(ranked[3].rank_reasons.is_empty());
}

#[test]
fn ranking_inputs_are_bounded() {
    let good = names(&["store", "src/a.rs"]);
    assert!(validate_rank(Some("claude-code.alice"), &good).is_ok());
    assert!(validate_rank(None, &[]).is_ok());
    let too_many: Vec<String> = (0..=CONTEXT_NAMES).map(|n| format!("q{n}")).collect();
    for (session, qualifiers) in [
        (Some("has space"), Vec::new()),
        (None, too_many),
        (None, names(&[""])),
        (None, names(&["/etc/passwd"])),
        (None, names(&["a/../b"])),
        (None, names(&["a b"])),
        (None, names(&["dup", "dup"])),
        (None, vec!["x".repeat(129)]),
    ] {
        let error = validate_rank(session, &qualifiers).unwrap_err();
        assert_eq!(
            error.code, "invalid_graph_context",
            "{session:?} {qualifiers:?}"
        );
    }
}

#[test]
fn the_core_never_records_a_ranked_code_context_query() {
    let decision = decide(
        &world(),
        &Command::GraphContext {
            workspace_id: "work".into(),
            names: names(&["upload"]),
            limit: 3,
            rank: true,
            session_id: Some("claude-code.alice".into()),
            qualifiers: names(&["store"]),
        },
        "read",
        NOW,
    );
    assert!(decision.events.is_empty());
}

#[test]
fn the_ranked_wire_form_is_strict_and_the_plain_one_unchanged() {
    let wire = r#"{"op":"graph_context","workspace_id":"work","names":["upload"],"limit":3,"rank":true,"session_id":"codex.bob","qualifiers":["store"]}"#;
    let command: Command = serde_json::from_str(wire).unwrap();
    assert!(matches!(&command, Command::GraphContext { rank: true, .. }));
    assert_eq!(serde_json::to_string(&command).unwrap(), wire);
    let plain = r#"{"op":"graph_context","workspace_id":"work","names":["upload"],"limit":3}"#;
    let command: Command = serde_json::from_str(plain).unwrap();
    assert!(matches!(
        &command,
        Command::GraphContext { rank: false, session_id: None, qualifiers, .. } if qualifiers.is_empty()
    ));
    assert_eq!(serde_json::to_string(&command).unwrap(), plain);

    let symbol = r#"{"op":"graph_symbol","workspace_id":"work","name":"upload","limit":5,"rank":true,"session_id":"codex.bob"}"#;
    let command: Command = serde_json::from_str(symbol).unwrap();
    assert_eq!(serde_json::to_string(&command).unwrap(), symbol);

    let reason = serde_json::to_string(&RankReason::PeerEditing {
        session_id: "codex.bob".into(),
    })
    .unwrap();
    assert_eq!(
        reason,
        r#"{"reason":"peer_editing","session_id":"codex.bob"}"#
    );
    let own = serde_json::to_string(&RankReason::OwnFile { edited: true }).unwrap();
    assert_eq!(own, r#"{"reason":"own_file","edited":true}"#);
    let unknown = r#"{"reason":"own_file","edited":true,"x":1}"#;
    assert!(serde_json::from_str::<RankReason>(unknown).is_err());
}
