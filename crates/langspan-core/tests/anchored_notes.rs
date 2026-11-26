//! Notes anchored to a definition, their staleness and their tags (roadmap
//! E1). The index is the daemon's; here its answers are given directly, as
//! the daemon prepares them.
use langspan_core::{
    AnchorIndex, AnchorState, Command, Event, Note, NoteAnchor, Outcome, State, SymbolRef, apply,
    decide,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> (Outcome, Vec<Event>) {
    let before = state.clone();
    let decision = decide(state, &command, id, now);
    assert_eq!(*state, before, "decisions stay pure");
    for event in &decision.events {
        apply(state, event).unwrap();
    }
    (decision.outcome, decision.events)
}

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    run(
        &mut state,
        "w",
        Command::WorkspaceRegister {
            workspace_id: "demo".into(),
            root: "/repos/demo".into(),
        },
        0,
    );
    for session in ["alice", "carol"] {
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: "demo".into(),
                parent: None,
                headless: false,
            },
            0,
        );
    }
    state
}

fn def(path: &str, name: &str, line: u32, body: &str) -> SymbolRef {
    SymbolRef {
        path: path.into(),
        name: name.into(),
        kind: "function".into(),
        line,
        end_line: line + 3,
        signature: format!("pub fn {name}()"),
        exported: true,
        digest: "f".repeat(64),
        body_digest: Some(body.repeat(64)),
        ..SymbolRef::default()
    }
}

fn index(definitions: Vec<SymbolRef>) -> AnchorIndex {
    let fresh = definitions
        .iter()
        .map(|symbol| symbol.path.clone())
        .chain(["src/api.rs".to_owned(), "app/upload.py".to_owned()])
        .collect();
    AnchorIndex {
        definitions,
        fresh,
        complete: true,
        ..AnchorIndex::default()
    }
}

fn remember(
    state: &mut State,
    id: &str,
    paths: &[&str],
    symbol: &str,
    tags: &[&str],
    found: Option<AnchorIndex>,
) -> (Outcome, Vec<Event>) {
    run(
        state,
        id,
        Command::RememberPrepared {
            workspace_id: "demo".into(),
            author: Some("alice".into()),
            text: "parse_upload must not touch the network.".into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
            symbol: Some(symbol.into()),
            tags: tags.iter().map(|tag| (*tag).into()).collect(),
            index: found,
        },
        5,
    )
}

fn observe(state: &mut State, id: &str, found: AnchorIndex) -> Vec<Event> {
    run(
        state,
        id,
        Command::NoteAnchorsPrepared {
            workspace_id: "demo".into(),
            index: found,
        },
        10,
    )
    .1
}

fn note(state: &State, id: &str) -> Note {
    state.notes["demo"]
        .iter()
        .find(|note| note.note_id == id)
        .cloned()
        .unwrap()
}

fn api_index(body: &str, other: &str) -> AnchorIndex {
    index(vec![
        def("src/api.rs", "parse_upload", 10, body),
        def("src/api.rs", "send_upload", 20, other),
    ])
}

#[test]
fn a_symbol_note_stores_the_definitions_line_and_body_digest() {
    let mut state = ready();
    let (outcome, events) = remember(
        &mut state,
        "note-7",
        &["src/api.rs"],
        "parse_upload",
        &["api"],
        Some(api_index("a", "b")),
    );
    assert!(matches!(outcome, Outcome::Remembered { .. }), "{outcome:?}");
    assert_eq!(events.len(), 1);
    let held = note(&state, "note-7");
    assert_eq!(
        held.anchor,
        Some(NoteAnchor {
            name: "parse_upload".into(),
            path: Some("src/api.rs".into()),
            kind: Some("function".into()),
            line: Some(10),
            body_digest: Some("a".repeat(64)),
        })
    );
    assert_eq!(held.anchor_state, Some(AnchorState::Current));
    assert_eq!(held.tags, vec!["api".to_owned()]);
}

#[test]
fn a_name_defined_twice_without_a_path_is_ambiguous_and_lists_both() {
    let mut state = ready();
    let (outcome, events) = remember(
        &mut state,
        "note-1",
        &[],
        "parse_upload",
        &[],
        Some(index(vec![
            def("src/api.rs", "parse_upload", 10, "a"),
            def("app/upload.py", "parse_upload", 3, "c"),
        ])),
    );
    match outcome {
        Outcome::Error { code, message } => {
            assert_eq!(code, "symbol_ambiguous");
            assert!(
                message.contains("\"src/api.rs\" line 10")
                    && message.contains("\"app/upload.py\" line 3"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(events.is_empty());
    assert!(!state.notes.contains_key("demo"));
}

#[test]
fn a_name_not_defined_in_the_named_path_is_not_found_and_lists_where_it_is() {
    let mut state = ready();
    let (outcome, events) = remember(
        &mut state,
        "note-1",
        &["src/other.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "b")),
    );
    match outcome {
        Outcome::Error { code, message } => {
            assert_eq!(code, "symbol_not_found");
            assert!(message.contains("\"src/api.rs\" line 10"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert!(events.is_empty());
}

#[test]
fn changing_the_body_marks_the_note_changed_and_another_function_leaves_it_current() {
    let mut state = ready();
    remember(
        &mut state,
        "note-7",
        &["src/api.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "b")),
    );
    // Another function of the same file changes: still current, nothing
    // recorded.
    assert!(observe(&mut state, "scan-1", api_index("a", "d")).is_empty());
    assert_eq!(
        note(&state, "note-7").anchor_state,
        Some(AnchorState::Current)
    );
    let events = observe(&mut state, "scan-2", api_index("e", "d"));
    assert!(
        matches!(
            events.as_slice(),
            [Event::NoteAnchorChanged {
                state: AnchorState::Changed,
                ..
            }]
        ),
        "{events:?}"
    );
    let held = note(&state, "note-7");
    assert_eq!(held.anchor_state, Some(AnchorState::Changed));
    // The anchor keeps the digest it was written about.
    assert_eq!(
        held.anchor.unwrap().body_digest,
        Some("a".repeat(64)),
        "the recorded digest stays"
    );
    // Put back as it was: current again.
    observe(&mut state, "scan-3", api_index("a", "d"));
    assert_eq!(
        note(&state, "note-7").anchor_state,
        Some(AnchorState::Current)
    );
}

#[test]
fn a_deleted_definition_reads_gone_and_the_note_is_still_listed() {
    let mut state = ready();
    remember(
        &mut state,
        "note-7",
        &["src/api.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "b")),
    );
    let events = observe(
        &mut state,
        "scan-1",
        index(vec![def("src/api.rs", "send_upload", 20, "b")]),
    );
    assert_eq!(events.len(), 1);
    assert_eq!(note(&state, "note-7").anchor_state, Some(AnchorState::Gone));
    let (outcome, _) = run(
        &mut state,
        "recap",
        Command::Recap {
            workspace_id: "demo".into(),
            tag: None,
        },
        20,
    );
    match outcome {
        Outcome::Recap { recap } => {
            assert_eq!(recap.notes.len(), 1);
            assert_eq!(recap.notes[0].anchor_state, Some(AnchorState::Gone));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_definition_whose_file_the_index_cannot_read_now_is_never_current() {
    let mut state = ready();
    remember(
        &mut state,
        "note-7",
        &["src/api.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "b")),
    );
    // The file is waiting or has a syntax error: its rows may be stale.
    let mut stale = api_index("a", "b");
    stale.fresh.remove("src/api.rs");
    stale.complete = false;
    observe(&mut state, "scan-1", stale);
    assert_eq!(
        note(&state, "note-7").anchor_state,
        Some(AnchorState::Unknown)
    );
    observe(&mut state, "scan-2", api_index("a", "b"));
    assert_eq!(
        note(&state, "note-7").anchor_state,
        Some(AnchorState::Current)
    );
}

#[test]
fn without_a_complete_scan_the_anchor_is_unknown_until_the_next_one() {
    let mut state = ready();
    let (outcome, _) = remember(
        &mut state,
        "note-7",
        &["src/api.rs"],
        "parse_upload",
        &[],
        None,
    );
    assert!(matches!(outcome, Outcome::Remembered { .. }), "{outcome:?}");
    let held = note(&state, "note-7");
    assert_eq!(held.anchor_state, Some(AnchorState::Unknown));
    assert_eq!(held.anchor.as_ref().unwrap().body_digest, None);
    observe(&mut state, "scan-1", api_index("a", "b"));
    let held = note(&state, "note-7");
    assert_eq!(held.anchor_state, Some(AnchorState::Current));
    assert_eq!(held.anchor.unwrap().line, Some(10));
    // A plain `Remember` with a symbol, decided without the daemon's
    // index, is recorded the same way.
    let (outcome, _) = run(
        &mut state,
        "note-8",
        Command::Remember {
            workspace_id: "demo".into(),
            author: None,
            text: "keep it small".into(),
            paths: Vec::new(),
            symbol: Some("parse_upload".into()),
            tags: Vec::new(),
        },
        11,
    );
    assert!(matches!(outcome, Outcome::Remembered { .. }), "{outcome:?}");
    assert_eq!(
        note(&state, "note-8").anchor_state,
        Some(AnchorState::Unknown)
    );
}

fn briefed(state: &mut State, id: &str, mentioned: &[&str], now: u64) -> Vec<Note> {
    let (outcome, _) = run(
        state,
        id,
        Command::BriefPrepared {
            session_id: "carol".into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
            recent: false,
            recent_window_ms: None,
            mcp: false,
            left: Vec::new(),
            inflight: Vec::new(),
            mentioned: mentioned.iter().map(|name| (*name).into()).collect(),
        },
        now,
    );
    match outcome {
        Outcome::Briefing { briefing } => briefing.notes,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_session_whose_files_mention_the_name_is_told_the_note_with_its_state() {
    let mut state = ready();
    remember(
        &mut state,
        "note-7",
        &["src/api.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "b")),
    );
    // Carol's files neither are the anchor's file nor mention the name.
    assert!(briefed(&mut state, "b-1", &[], 20).is_empty());
    let told = briefed(&mut state, "b-2", &["parse_upload"], 21);
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].anchor_state, Some(AnchorState::Current));
    // The state changes: the same note is news again, with its new state.
    observe(&mut state, "scan-1", api_index("e", "b"));
    let told = briefed(&mut state, "b-3", &["parse_upload"], 22);
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].anchor_state, Some(AnchorState::Changed));
}

#[test]
fn current_notes_are_chosen_before_changed_and_gone_ones() {
    let mut state = ready();
    for n in 0..4 {
        remember(
            &mut state,
            &format!("old-{n}"),
            &["src/api.rs"],
            "send_upload",
            &[],
            Some(api_index("a", "b")),
        );
    }
    observe(&mut state, "scan-1", api_index("a", "z"));
    remember(
        &mut state,
        "fresh",
        &["src/api.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "z")),
    );
    // Five notes reach Carol; the briefing holds four, the current first.
    let told = briefed(&mut state, "b-1", &["parse_upload", "send_upload"], 20);
    assert_eq!(told.len(), 4);
    assert_eq!(told[0].note_id, "fresh");
    assert!(
        told[1..]
            .iter()
            .all(|note| note.anchor_state == Some(AnchorState::Changed))
    );
}

#[test]
fn recap_with_a_tag_lists_only_notes_tagged_so() {
    let mut state = ready();
    remember(
        &mut state,
        "tagged",
        &["src/api.rs"],
        "parse_upload",
        &["api", "net"],
        Some(api_index("a", "b")),
    );
    remember(
        &mut state,
        "other",
        &["src/api.rs"],
        "send_upload",
        &["net"],
        Some(api_index("a", "b")),
    );
    let (outcome, _) = run(
        &mut state,
        "recap",
        Command::Recap {
            workspace_id: "demo".into(),
            tag: Some("api".into()),
        },
        20,
    );
    match outcome {
        Outcome::Recap { recap } => {
            let ids: Vec<&str> = recap.notes.iter().map(|n| n.note_id.as_str()).collect();
            assert_eq!(ids, ["tagged"]);
        }
        other => panic!("{other:?}"),
    }
    let (outcome, _) = run(
        &mut state,
        "recap-2",
        Command::Recap {
            workspace_id: "demo".into(),
            tag: Some("API".into()),
        },
        20,
    );
    assert!(
        matches!(&outcome, Outcome::Error { code, .. } if code == "invalid_tag"),
        "{outcome:?}"
    );
}

#[test]
fn a_bad_tag_is_refused_before_anything_is_recorded() {
    let long = "a".repeat(33);
    let exactly = "a".repeat(32);
    for (tags, refused) in [
        (vec!["Api"], true),
        (vec![long.as_str()], true),
        (vec![""], true),
        (vec!["api_v2"], true),
        (vec!["a", "b", "c", "d", "e"], true),
        (vec![exactly.as_str(), "v2-api"], false),
    ] {
        let mut state = ready();
        let before = state.clone();
        let (outcome, events) = remember(
            &mut state,
            "note-1",
            &["src/api.rs"],
            "parse_upload",
            &tags,
            Some(api_index("a", "b")),
        );
        if refused {
            assert!(
                matches!(&outcome, Outcome::Error { code, .. } if code == "invalid_tag"),
                "{tags:?}: {outcome:?}"
            );
            assert!(events.is_empty());
            assert_eq!(state, before);
        } else {
            assert!(matches!(outcome, Outcome::Remembered { .. }), "{outcome:?}");
        }
    }
    // A plain note's tags are checked the same way.
    let mut state = ready();
    let (outcome, events) = run(
        &mut state,
        "note-2",
        Command::Remember {
            workspace_id: "demo".into(),
            author: None,
            text: "release freeze".into(),
            paths: Vec::new(),
            symbol: None,
            tags: vec!["Release".into()],
        },
        5,
    );
    assert!(
        matches!(&outcome, Outcome::Error { code, .. } if code == "invalid_tag"),
        "{outcome:?}"
    );
    assert!(events.is_empty());
}

#[test]
fn a_note_recorded_before_anchors_reads_as_unanchored() {
    // A `remembered` event as earlier builds wrote it.
    let event: Event = serde_json::from_str(
        r#"{"type":"remembered","note":{"note_id":"n1","workspace_id":"demo","author":null,"text":"old note","paths":["src/api.rs"]}}"#,
    )
    .unwrap();
    let mut state = ready();
    apply(&mut state, &event).unwrap();
    let held = note(&state, "n1");
    assert_eq!((held.anchor, held.anchor_state), (None, None));
    assert!(held.tags.is_empty());
    // An index pass leaves it alone.
    assert!(observe(&mut state, "scan-1", api_index("a", "b")).is_empty());
    assert_eq!(note(&state, "n1").anchor_state, None);
}

#[test]
fn a_definition_found_only_in_another_file_reads_gone() {
    let mut state = ready();
    remember(
        &mut state,
        "note-7",
        &["src/api.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "b")),
    );
    // Anchors use the index's name lookup in their own file: a definition
    // of the name elsewhere is not the one the note was written about.
    let moved = index(vec![def("src/upload/api.rs", "parse_upload", 10, "a")]);
    observe(&mut state, "scan-1", moved);
    assert_eq!(note(&state, "note-7").anchor_state, Some(AnchorState::Gone));
}

/// Review comment 4713922957: a deleted or moved anchor file has no row in
/// the index, so it is never fresh; a complete scan that holds no row for
/// it reads the note gone, not unknown.
#[test]
fn a_deleted_or_moved_anchor_file_reads_gone_after_a_complete_scan() {
    for (case, definitions) in [
        ("deleted", vec![]),
        (
            "moved",
            vec![def("src/upload/api.rs", "parse_upload", 10, "a")],
        ),
    ] {
        let mut state = ready();
        remember(
            &mut state,
            "note-7",
            &["src/api.rs"],
            "parse_upload",
            &[],
            Some(api_index("a", "b")),
        );
        let mut without = index(definitions);
        without.fresh.remove("src/api.rs");
        without.absent.insert("src/api.rs".into());
        // Before a complete scan, a missing row may be a file not yet
        // read: unknown.
        let mut partial = without.clone();
        partial.complete = false;
        observe(&mut state, "scan-1", partial);
        assert_eq!(
            note(&state, "note-7").anchor_state,
            Some(AnchorState::Unknown),
            "{case}"
        );
        observe(&mut state, "scan-2", without);
        assert_eq!(
            note(&state, "note-7").anchor_state,
            Some(AnchorState::Gone),
            "{case}"
        );
    }
}

/// Only a confirmed absence reads gone: a file the index holds a row for
/// but cannot read now (waiting, a syntax error, a bound reached) stays
/// unknown after a complete scan.
#[test]
fn a_present_but_unreadable_anchor_file_stays_unknown_after_a_complete_scan() {
    let mut state = ready();
    remember(
        &mut state,
        "note-7",
        &["src/api.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "b")),
    );
    let mut stale = index(vec![]);
    stale.fresh.remove("src/api.rs");
    observe(&mut state, "scan-1", stale);
    assert_eq!(
        note(&state, "note-7").anchor_state,
        Some(AnchorState::Unknown)
    );
}

/// Review comment 4713922965: the daemon looks a name up in the files each
/// note names, and workspace-wide only for a note that names none.
#[test]
fn anchored_lists_each_notes_own_files_and_a_workspace_lookup_only_without_them() {
    let mut state = ready();
    remember(
        &mut state,
        "resolved",
        &["src/api.rs"],
        "parse_upload",
        &[],
        Some(api_index("a", "b")),
    );
    remember(
        &mut state,
        "scoped",
        &["app/upload.py", "lib/x.py"],
        "check_size",
        &[],
        None,
    );
    remember(&mut state, "wide", &[], "send_upload", &[], None);
    let lookups = langspan_core::anchored(&state, "demo");
    let paths = |name: &str| lookups[name].paths.iter().cloned().collect::<Vec<_>>();
    assert_eq!(paths("parse_upload"), ["src/api.rs"]);
    assert!(!lookups["parse_upload"].workspace);
    assert_eq!(paths("check_size"), ["app/upload.py", "lib/x.py"]);
    assert!(!lookups["check_size"].workspace);
    assert!(paths("send_upload").is_empty());
    assert!(lookups["send_upload"].workspace);
    // `remember` looks in each named file in full and, for the hint of
    // where else a name is defined, across the workspace. A command names
    // at most 64 paths, so one more is enough to refuse it.
    let many: Vec<String> = (0..100).map(|n| format!("f/{n}.rs")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let lookup = langspan_core::remember_lookups("parse_upload", &refs);
    assert_eq!(lookup["parse_upload"].paths.len(), 65);
    assert!(lookup["parse_upload"].workspace);
}

/// A workspace-wide lookup that reached its cap counts what it found as a
/// lower bound.
#[test]
fn a_capped_workspace_lookup_counts_its_definitions_as_a_lower_bound() {
    let mut state = ready();
    let mut found = index(
        (0..5)
            .map(|n| def(&format!("a/{n}.rs"), "parse_upload", 1, "a"))
            .collect(),
    );
    found.truncated.insert("parse_upload".into());
    let (outcome, events) = remember(&mut state, "n1", &[], "parse_upload", &[], Some(found));
    match outcome {
        Outcome::Error { code, message } => {
            assert_eq!(code, "symbol_ambiguous");
            assert!(message.contains("defined at least 5 times"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert!(events.is_empty());
}
