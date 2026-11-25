//! Two edits of one file are admitted together only when both name regions
//! and the regions cannot touch the same code: different symbols (neither
//! inside the other), or disjoint line ranges located in the same version of
//! the file. Anything else conflicts exactly as a whole-file admission.
use langspan_core::{
    Command, Event, FileState, Lease, ObservedFile, Outcome, PathRegions, Region, State,
    ToolIdentity, apply, decide,
};

const V1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const V2: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const H: &str = "3333333333333333333333333333333333333333333333333333333333333333";

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
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
        "w",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        0,
    );
    for session in ["alice", "bob"] {
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    state
}

fn lines(start_line: u32, end_line: u32) -> Region {
    Region::Lines {
        start_line,
        end_line,
    }
}

fn symbol(key: &[&str], start_line: u32, end_line: u32) -> Region {
    Region::Symbol {
        key: key.iter().map(|name| (*name).into()).collect(),
        digest: H.into(),
        start_line,
        end_line,
    }
}

fn located(digest: &str, regions: Vec<Region>) -> PathRegions {
    PathRegions {
        path: "src/lib.rs".into(),
        digest: digest.into(),
        regions,
    }
}

fn operation(id: &str) -> ToolIdentity {
    ToolIdentity {
        operation_id: format!("op-{id}"),
        source: "test".into(),
        tool: "Edit".into(),
        input_digest: "a".repeat(64),
    }
}

fn base(digest: &str) -> Vec<ObservedFile> {
    vec![ObservedFile {
        path: "src/lib.rs".into(),
        state: FileState::Present {
            digest: digest.into(),
            kind: Default::default(),
        },
    }]
}

fn admit(session: &str, id: &str, base: Vec<ObservedFile>, regions: Vec<PathRegions>) -> Command {
    Command::AdmitTool {
        session_id: session.into(),
        paths: vec!["src/lib.rs".into()],
        ttl_ms: 1_000,
        operation: operation(id),
        base,
        regions,
        overwrites: Vec::new(),
    }
}

/// An edit of `src/lib.rs` whose base on disk is `digest`, with `regions`
/// located in that version, or the whole file when `regions` is `None`.
fn edit(session: &str, id: &str, digest: &str, regions: Option<Vec<Region>>) -> Command {
    admit(
        session,
        id,
        base(digest),
        regions
            .map(|regions| vec![located(digest, regions)])
            .unwrap_or_default(),
    )
}

fn admitted(outcome: &Outcome) -> &Lease {
    match outcome {
        Outcome::Admitted { lease } => lease,
        other => panic!("expected admission, got {other:?}"),
    }
}

fn holders(outcome: &Outcome) -> &[langspan_core::Conflict] {
    match outcome {
        Outcome::Conflict { holders } => holders,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

/// Alice holds `first`; Bob asks for `second`. Returns Bob's outcome.
fn pair(first: (&str, Option<Vec<Region>>), second: (&str, Option<Vec<Region>>)) -> Outcome {
    let mut state = ready();
    admitted(&run(
        &mut state,
        "a1",
        edit("alice", "a1", first.0, first.1),
        10,
    ));
    run(&mut state, "b1", edit("bob", "b1", second.0, second.1), 10)
}

#[test]
fn different_symbols_are_admitted_together_whatever_the_file_version() {
    let outcome = pair(
        (V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        (V1, Some(vec![symbol(&["beta"], 7, 9)])),
    );
    assert_eq!(
        admitted(&outcome).regions,
        vec![located(V1, vec![symbol(&["beta"], 7, 9)])]
    );
    // Alice's edit of alpha changed the file; beta is still another symbol.
    admitted(&pair(
        (V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        (V2, Some(vec![symbol(&["beta"], 30, 40)])),
    ));
    // Two methods of one type are siblings, not one symbol.
    admitted(&pair(
        (V1, Some(vec![symbol(&["Store", "open"], 2, 5)])),
        (V1, Some(vec![symbol(&["Store", "close"], 6, 9)])),
    ));
}

#[test]
fn the_same_symbol_or_one_inside_the_other_conflicts_and_names_the_holders_region() {
    let outcome = pair(
        (V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        (V1, Some(vec![symbol(&["alpha"], 1, 5)])),
    );
    let held = holders(&outcome);
    assert_eq!(held[0].session_id, "alice");
    assert_eq!(held[0].paths, ["src/lib.rs"]);
    assert_eq!(
        held[0].regions,
        vec![located(V1, vec![symbol(&["alpha"], 1, 5)])],
        "the deny reason can name what the holder is editing"
    );
    // Same key located in another version is still the same symbol.
    holders(&pair(
        (V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        (V2, Some(vec![symbol(&["alpha"], 9, 14)])),
    ));
    // A nested function and its enclosing function, either way round.
    holders(&pair(
        (V1, Some(vec![symbol(&["outer", "inner"], 3, 4)])),
        (V1, Some(vec![symbol(&["outer"], 1, 9)])),
    ));
    holders(&pair(
        (V1, Some(vec![symbol(&["Store"], 1, 20)])),
        (V1, Some(vec![symbol(&["Store", "open"], 2, 5)])),
    ));
    // One shared symbol among several is enough.
    holders(&pair(
        (
            V1,
            Some(vec![symbol(&["alpha"], 1, 5), symbol(&["gamma"], 20, 25)]),
        ),
        (
            V1,
            Some(vec![symbol(&["beta"], 7, 9), symbol(&["gamma"], 20, 25)]),
        ),
    ));
}

#[test]
fn line_ranges_are_admitted_together_only_when_disjoint_in_the_same_version() {
    admitted(&pair(
        (V1, Some(vec![lines(1, 5)])),
        (V1, Some(vec![lines(6, 9)])),
    ));
    // One shared line is an overlap.
    holders(&pair(
        (V1, Some(vec![lines(1, 5)])),
        (V1, Some(vec![lines(5, 9)])),
    ));
    // Line numbers from different versions cannot be compared.
    holders(&pair(
        (V1, Some(vec![lines(1, 5)])),
        (V2, Some(vec![lines(40, 50)])),
    ));
}

#[test]
fn a_whole_file_edit_or_a_mix_of_symbols_and_lines_conflicts() {
    let outcome = pair((V1, None), (V1, Some(vec![symbol(&["beta"], 7, 9)])));
    assert!(holders(&outcome)[0].regions.is_empty(), "held whole");
    holders(&pair(
        (V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        (V1, None),
    ));
    holders(&pair((V1, Some(vec![lines(1, 5)])), (V1, None)));
    holders(&pair(
        (V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        (V1, Some(vec![lines(40, 50)])),
    ));
}

#[test]
fn regions_count_only_when_the_requests_own_base_is_their_version() {
    let mut state = ready();
    admitted(&run(
        &mut state,
        "a1",
        edit("alice", "a1", V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        10,
    ));
    // Regions located in V1, but the file on disk was V2 when the hook asked.
    let mismatched = admit(
        "bob",
        "b1",
        base(V2),
        vec![located(V1, vec![symbol(&["beta"], 7, 9)])],
    );
    holders(&run(&mut state, "b1", mismatched, 10));
    // With no base at all the regions are not trusted either.
    let no_base = admit(
        "bob",
        "b2",
        Vec::new(),
        vec![located(V1, vec![symbol(&["beta"], 7, 9)])],
    );
    holders(&run(&mut state, "b2", no_base, 10));
}

#[test]
fn a_completed_edit_releases_its_region() {
    let mut state = ready();
    admitted(&run(
        &mut state,
        "a1",
        edit("alice", "a1", V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        10,
    ));
    run(
        &mut state,
        "c1",
        Command::Complete {
            session_id: "alice".into(),
            lease_id: "a1".into(),
        },
        11,
    );
    admitted(&run(
        &mut state,
        "b1",
        edit("bob", "b1", V2, Some(vec![symbol(&["alpha"], 1, 5)])),
        12,
    ));
}

#[test]
fn an_earlier_lease_without_regions_replays_and_conflicts_as_the_whole_file() {
    let mut state = ready();
    // A lease recorded before regions existed: the field is absent.
    let old = r#"{"type":"lease_granted","lease":{"lease_id":"old","session_id":"alice","workspace_id":"work","paths":["src/lib.rs"],"generation":1,"expires_at_ms":1000,"status":"active"}}"#;
    let event: Event = serde_json::from_str(old).unwrap();
    apply(&mut state, &event).unwrap();
    assert!(state.leases["old"].regions.is_empty());
    let outcome = run(
        &mut state,
        "b1",
        edit("bob", "b1", V1, Some(vec![symbol(&["beta"], 7, 9)])),
        10,
    );
    assert_eq!(holders(&outcome)[0].lease_id, "old");
    // A lease without regions still serializes without the field.
    assert!(
        !serde_json::to_string(&state.leases["old"])
            .unwrap()
            .contains("regions")
    );
}

#[test]
fn regions_are_validated_and_normalized() {
    let state = ready();
    let rejected = |regions: Vec<PathRegions>| {
        let decision = decide(&state, &admit("alice", "a1", base(V1), regions), "a1", 10);
        assert!(decision.events.is_empty());
        match decision.outcome {
            Outcome::Error { code, .. } => code,
            other => panic!("expected an error, got {other:?}"),
        }
    };
    let many: Vec<Region> = (0..65).map(|i| lines(i * 2 + 1, i * 2 + 1)).collect();
    let long_name = "n".repeat(129);
    let deep: Vec<&str> = vec!["n"; 9];
    let elsewhere = PathRegions {
        path: "src/other.rs".into(),
        ..located(V1, vec![lines(1, 1)])
    };
    let bad_symbol_digest = Region::Symbol {
        key: vec!["alpha".into()],
        digest: "short".into(),
        start_line: 1,
        end_line: 2,
    };
    for (case, regions) in [
        ("too many", vec![located(V1, many)]),
        ("line zero", vec![located(V1, vec![lines(0, 3)])]),
        ("reversed", vec![located(V1, vec![lines(9, 3)])]),
        (
            "reversed symbol",
            vec![located(V1, vec![symbol(&["a"], 9, 3)])],
        ),
        (
            "past the line bound",
            vec![located(V1, vec![lines(1, 3_000_000)])],
        ),
        ("empty", vec![located(V1, Vec::new())]),
        ("bad file digest", vec![located("xyz", vec![lines(1, 2)])]),
        (
            "bad symbol digest",
            vec![located(V1, vec![bad_symbol_digest])],
        ),
        ("unrequested path", vec![elsewhere]),
        (
            "named twice",
            vec![
                located(V1, vec![lines(1, 2)]),
                located(V1, vec![lines(5, 6)]),
            ],
        ),
        ("empty key", vec![located(V1, vec![symbol(&[], 1, 2)])]),
        ("empty name", vec![located(V1, vec![symbol(&[""], 1, 2)])]),
        (
            "name too long",
            vec![located(V1, vec![symbol(&[&long_name], 1, 2)])],
        ),
        ("key too deep", vec![located(V1, vec![symbol(&deep, 1, 2)])]),
        (
            "control character in a name",
            vec![located(V1, vec![symbol(&["a\nb"], 1, 2)])],
        ),
        (
            "symbols and lines mixed",
            vec![located(V1, vec![symbol(&["a"], 1, 2), lines(5, 6)])],
        ),
    ] {
        assert_eq!(rejected(regions), "invalid_regions", "{case}");
    }

    // Exactly 64 is accepted; the stored regions are sorted and deduplicated.
    let mut state = ready();
    let mut sixty_four: Vec<Region> = (0..64).rev().map(|i| lines(i * 2 + 1, i * 2 + 1)).collect();
    sixty_four.push(lines(1, 1));
    let lease = run(
        &mut state,
        "a1",
        edit("alice", "a1", V1, Some(sixty_four)),
        10,
    );
    let stored = &admitted(&lease).regions[0].regions;
    assert_eq!(stored.len(), 64);
    assert!(stored.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn a_replayed_lease_with_invalid_regions_is_rejected() {
    let mut state = ready();
    let event = Event::LeaseGranted {
        lease: Lease {
            lease_id: "bad".into(),
            session_id: "alice".into(),
            workspace_id: "work".into(),
            paths: vec!["src/lib.rs".into()],
            generation: 1,
            expires_at_ms: 1_000,
            status: langspan_core::LeaseStatus::Active,
            operation: None,
            base_unknown: Vec::new(),
            regions: vec![PathRegions {
                path: "src/other.rs".into(),
                ..located(V1, vec![lines(1, 2)])
            }],
        },
    };
    let before = state.clone();
    assert!(apply(&mut state, &event).is_err());
    assert_eq!(state, before);
}

#[test]
fn an_observation_does_not_report_a_disjoint_region_holder_as_concurrent() {
    let mut state = ready();
    admitted(&run(
        &mut state,
        "a1",
        edit("alice", "a1", V1, Some(vec![symbol(&["alpha"], 1, 5)])),
        10,
    ));
    admitted(&run(
        &mut state,
        "b1",
        edit("bob", "b1", V1, Some(vec![symbol(&["beta"], 7, 9)])),
        10,
    ));
    let outcome = run(
        &mut state,
        "obs-b1",
        Command::ObserveTool {
            session_id: "bob".into(),
            operation: operation("b1"),
            paths: vec!["src/lib.rs".into()],
            result: langspan_core::ToolResult::Succeeded,
            lease_id: Some("b1".into()),
        },
        11,
    );
    match outcome {
        Outcome::Observed { observation } => {
            assert!(observation.concurrent_holders.is_empty(), "{observation:?}")
        }
        other => panic!("{other:?}"),
    }
}
