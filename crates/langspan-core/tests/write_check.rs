//! The check an agent asks for before a write (roadmap B26): what may be
//! claimed per path and per symbol from the coordinator's records, the
//! index and the line journal, how one check is bounded, and that the core
//! never records it.
use langspan_core::{
    Command, DefinitionFacts, Event, FileState, IndexFacts, InterfaceChange, InterfaceNotice,
    LineRange, ObservedFile, Outcome, PathFacts, SourceOrigin, State, SymbolRef,
    WRITE_CHECK_SYMBOLS, WRITE_CHECK_WRITERS, WriteCheckRequest, WrittenLines, apply, decide,
    write_check, write_check_request,
};

fn run(state: &mut State, id: &str, command: Command, now: u64) -> Outcome {
    let decision = decide(state, &command, id, now);
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
    for session in ["alice", "bob", "carol"] {
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

fn version(text: &str) -> FileState {
    FileState::Present {
        digest: format!(
            "{:0>64}",
            text.bytes().map(|b| format!("{b:02x}")).collect::<String>()
        ),
        kind: Default::default(),
    }
}

fn changed(name: &str, change: &str, before: &str, after: Option<&str>) -> InterfaceChange {
    InterfaceChange {
        name: name.into(),
        kind: "function".into(),
        change: change.into(),
        before: Some(before.into()),
        after: after.map(Into::into),
        after_name: None,
        before_arity: None,
        after_arity: None,
        before_count: None,
        after_count: None,
    }
}

fn notice(
    state: &mut State,
    id: &str,
    path: &str,
    by: &str,
    at_ms: u64,
    changes: Vec<InterfaceChange>,
) {
    apply(
        state,
        &Event::InterfaceChanged {
            notice: InterfaceNotice {
                notice_id: id.into(),
                workspace_id: "work".into(),
                path: path.into(),
                changes,
                dependents: vec![],
                elsewhere: vec![],
                by_session: Some(by.into()),
                at_ms,
                generation: 1,
            },
        },
    )
    .unwrap();
}

fn definition(
    path: &str,
    name: &str,
    signature: &str,
    writers: Option<Vec<WrittenLines>>,
) -> DefinitionFacts {
    DefinitionFacts {
        symbol: SymbolRef {
            path: path.into(),
            name: name.into(),
            kind: "function".into(),
            line: 5,
            end_line: 7,
            signature: signature.into(),
            exported: true,
            digest: "d".repeat(64),
            ..SymbolRef::default()
        },
        body_writers: writers,
    }
}

/// An index read that held every matching definition.
fn read_all(definitions: &[DefinitionFacts]) -> IndexFacts {
    IndexFacts {
        definitions: definitions.to_vec(),
        ..IndexFacts::default()
    }
}

fn request(state: &State, session: &str, paths: &[&str], names: &[&str]) -> WriteCheckRequest {
    write_check_request(
        state,
        session,
        &paths.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>(),
        &names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>(),
    )
    .unwrap()
}

fn no_paths(request: &WriteCheckRequest) -> Vec<PathFacts> {
    request
        .paths
        .iter()
        .map(|path| PathFacts {
            path: path.clone(),
            current: None,
            written: vec![],
        })
        .collect()
}

const NEW: &str = "pub fn upload(path: &str, timeout_ms: u64) -> bool";
const OLD: &str = "pub fn upload(path: &str) -> bool";

#[test]
fn a_signature_a_peer_changed_two_minutes_ago_is_reported_with_its_session_and_age() {
    let mut state = ready();
    notice(
        &mut state,
        "n1",
        "src/net.rs",
        "alice",
        1_000,
        vec![changed("upload", "signature_changed", OLD, Some(NEW))],
    );
    run(
        &mut state,
        "i",
        Command::IntentSet {
            session_id: "alice".into(),
            paths: vec!["src/net.rs".into()],
            summary: "make upload take a timeout; not uploads".into(),
        },
        1_000,
    );
    let now = 1_000 + 120_000;
    let asked = request(&state, "bob", &["src/main.rs"], &["upload"]);
    let index = [definition("src/net.rs", "upload", NEW, Some(vec![]))];
    let check = write_check(
        &state,
        "bob",
        &asked,
        &no_paths(&asked),
        Some(&read_all(&index)),
        now,
    );
    assert!(check.index_ready);
    assert_eq!(check.symbols.len(), 1, "{check:?}");
    let upload = &check.symbols[0];
    assert_eq!(
        (
            upload.change.as_str(),
            upload.path.as_str(),
            upload.by_session.as_deref(),
            upload.age_ms
        ),
        ("signature", "src/net.rs", Some("alice"), Some(120_000))
    );
    assert_eq!(
        (upload.before.as_deref(), upload.after.as_deref()),
        (Some(OLD), Some(NEW))
    );
    assert_eq!(upload.intents, ["alice"], "a whole word, not `uploads`");
    assert_eq!(upload.confidence, "name_match");

    // Not news to the session that made it.
    let own = request(&state, "alice", &["src/main.rs"], &["upload"]);
    assert!(
        write_check(
            &state,
            "alice",
            &own,
            &no_paths(&own),
            Some(&read_all(&index)),
            now
        )
        .symbols
        .is_empty()
    );
    // Not claimed once the index no longer holds the new signature.
    let reverted = [definition("src/net.rs", "upload", OLD, Some(vec![]))];
    assert!(
        write_check(
            &state,
            "bob",
            &asked,
            &no_paths(&asked),
            Some(&read_all(&reverted)),
            now
        )
        .symbols
        .is_empty()
    );
    // Past the briefing horizon it is no longer just changed.
    assert!(
        write_check(
            &state,
            "bob",
            &asked,
            &no_paths(&asked),
            Some(&read_all(&index)),
            1_000 + 31 * 60_000
        )
        .symbols
        .is_empty()
    );
    // Without a complete scan, path results only.
    let blind = write_check(&state, "bob", &asked, &no_paths(&asked), None, now);
    assert!(!blind.index_ready && blind.symbols.is_empty());
    assert_eq!(blind.paths.len(), 1);
}

#[test]
fn a_removal_or_rename_counts_only_while_no_definition_is_left_in_its_file() {
    let mut state = ready();
    let mut renamed = changed("fetch", "renamed", "pub fn fetch()", Some("pub fn load()"));
    renamed.after_name = Some("load".into());
    notice(
        &mut state,
        "n1",
        "src/a.rs",
        "alice",
        10,
        vec![
            renamed,
            changed("drop_all", "removed", "pub fn drop_all()", None),
        ],
    );
    let asked = request(&state, "bob", &["src/a.rs"], &[]);
    let check = write_check(
        &state,
        "bob",
        &asked,
        &no_paths(&asked),
        Some(&read_all(&[])),
        20,
    );
    let found: Vec<(&str, &str, Option<&str>)> = check
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.change.as_str(), s.renamed_to.as_deref()))
        .collect();
    assert!(
        found.contains(&("fetch", "removed", Some("load"))),
        "{found:?}"
    );
    assert!(found.contains(&("drop_all", "removed", None)), "{found:?}");
    let back = [definition(
        "src/a.rs",
        "fetch",
        "pub fn fetch()",
        Some(vec![]),
    )];
    let check = write_check(
        &state,
        "bob",
        &asked,
        &no_paths(&asked),
        Some(&read_all(&back)),
        20,
    );
    assert_eq!(
        check
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["drop_all"]
    );
}

#[test]
fn a_body_change_names_the_newest_other_writer_and_an_unreadable_body_is_counted() {
    let state = ready();
    let lines = |start, end| {
        vec![LineRange {
            start_line: start,
            end_line: end,
        }]
    };
    let writers = vec![
        WrittenLines {
            path: "src/net.rs".into(),
            session_id: "carol".into(),
            lines: lines(5, 5),
            age_ms: 9_000,
        },
        WrittenLines {
            path: "src/net.rs".into(),
            session_id: "alice".into(),
            lines: lines(6, 6),
            age_ms: 2_000,
        },
        WrittenLines {
            path: "src/net.rs".into(),
            session_id: "bob".into(),
            lines: lines(7, 7),
            age_ms: 1,
        },
    ];
    let asked = request(&state, "bob", &["src/net.rs"], &["download", "other"]);
    let index = [
        definition("src/net.rs", "download", "pub fn download()", Some(writers)),
        definition("src/x.rs", "other", "pub fn other()", None),
        definition("src/y.rs", "other", "pub fn other()", Some(vec![])),
    ];
    let check = write_check(
        &state,
        "bob",
        &asked,
        &no_paths(&asked),
        Some(&read_all(&index)),
        10,
    );
    assert_eq!(check.symbols.len(), 1, "{check:?}");
    let body = &check.symbols[0];
    assert_eq!(
        (
            body.change.as_str(),
            body.by_session.as_deref(),
            body.age_ms,
            body.lines.clone()
        ),
        ("body", Some("alice"), Some(2_000), lines(6, 6)),
        "its own lines are not news to the asking session"
    );
    assert_eq!(check.unchecked_bodies, 1);
}

/// Two methods of one name in one file (`A::text` and `B::text`) are one
/// name-matched symbol: the newest body change among them is reported,
/// whatever their order in the index.
#[test]
fn same_named_definitions_in_one_file_report_their_newest_body_change() {
    let state = ready();
    let writer = |session: &str, line, age_ms| WrittenLines {
        path: "src/doc.rs".into(),
        session_id: session.into(),
        lines: vec![LineRange {
            start_line: line,
            end_line: line,
        }],
        age_ms,
    };
    let at = |line, writers| {
        let mut found = definition("src/doc.rs", "text", "fn text(&self)", Some(writers));
        found.symbol.line = line;
        found.symbol.end_line = line + 2;
        found
    };
    let older = at(5, vec![writer("carol", 6, 90_000)]);
    let newer = at(20, vec![writer("alice", 21, 2_000)]);
    let asked = request(&state, "bob", &["src/doc.rs"], &["text"]);
    for index in [
        [older.clone(), newer.clone()],
        [newer.clone(), older.clone()],
    ] {
        let check = write_check(
            &state,
            "bob",
            &asked,
            &no_paths(&asked),
            Some(&read_all(&index)),
            10,
        );
        assert_eq!(check.symbols.len(), 1, "{check:?}");
        let body = &check.symbols[0];
        assert_eq!(
            (
                body.change.as_str(),
                body.by_session.as_deref(),
                body.age_ms,
                body.lines.clone()
            ),
            (
                "body",
                Some("alice"),
                Some(2_000),
                newer.body_writers.clone().unwrap()[0].lines.clone()
            ),
            "{check:?}"
        );
        assert_eq!(check.unchecked_bodies, 0);
    }
}

/// A removal or signature change is checked against the index's definitions
/// at its own place, not against the bounded list of definitions compared
/// for bodies, and a truncated list is said.
#[test]
fn a_change_past_the_definition_bound_is_checked_at_its_place() {
    let mut state = ready();
    notice(
        &mut state,
        "n1",
        "src/many.rs",
        "alice",
        10,
        vec![
            changed("f69", "removed", "pub fn f69()", None),
            changed(
                "f70",
                "signature_changed",
                "pub fn f70()",
                Some("pub fn f70(a: u8)"),
            ),
            changed("f71", "removed", "pub fn f71()", None),
        ],
    );
    let asked = request(&state, "bob", &["src/many.rs"], &[]);
    assert_eq!(
        langspan_core::write_check_noticed(&state, "bob", &asked, 20),
        [
            ("src/many.rs", "f69"),
            ("src/many.rs", "f70"),
            ("src/many.rs", "f71")
        ]
        .map(|(path, name)| (path.to_owned(), name.to_owned()))
    );
    assert!(
        langspan_core::write_check_noticed(&state, "alice", &asked, 20).is_empty(),
        "her own changes"
    );
    // The bounded list holds f00 only; the index still defines f69 and the
    // new f70, and no longer f71.
    let index = IndexFacts {
        definitions: vec![definition(
            "src/many.rs",
            "f00",
            "pub fn f00()",
            Some(vec![]),
        )],
        truncated: true,
        noticed: vec![
            definition("src/many.rs", "f69", "pub fn f69()", None).symbol,
            definition("src/many.rs", "f70", "pub fn f70(a: u8)", None).symbol,
        ],
    };
    let check = write_check(&state, "bob", &asked, &no_paths(&asked), Some(&index), 20);
    let changes: Vec<(&str, &str)> = check
        .symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.change.as_str()))
        .collect();
    assert_eq!(
        changes,
        [("f70", "signature"), ("f71", "removed")],
        "{check:?}"
    );
    assert!(check.definitions_truncated);
}

/// A journal entry whose lines are unknown (its pre-write content could not
/// be read) is no evidence that any one body changed: it is not reported as
/// a body change, does not displace an older writer whose lines are in the
/// definition, and leaves the definition counted as unchecked when it is
/// the only other writer.
#[test]
fn a_writer_whose_lines_are_unknown_is_not_a_body_change() {
    let state = ready();
    let unknown = WrittenLines {
        path: "src/net.rs".into(),
        session_id: "alice".into(),
        lines: vec![],
        age_ms: 1_000,
    };
    let known = WrittenLines {
        path: "src/net.rs".into(),
        session_id: "carol".into(),
        lines: vec![LineRange {
            start_line: 6,
            end_line: 6,
        }],
        age_ms: 9_000,
    };
    let asked = request(&state, "bob", &["src/net.rs"], &["download", "upload"]);
    let index = [
        definition(
            "src/net.rs",
            "download",
            "pub fn download()",
            Some(vec![unknown.clone(), known.clone()]),
        ),
        definition(
            "src/net.rs",
            "upload",
            "pub fn upload()",
            Some(vec![unknown]),
        ),
    ];
    let check = write_check(
        &state,
        "bob",
        &asked,
        &no_paths(&asked),
        Some(&read_all(&index)),
        10,
    );
    assert_eq!(check.symbols.len(), 1, "{check:?}");
    let body = &check.symbols[0];
    assert_eq!(
        (
            body.name.as_str(),
            body.by_session.as_deref(),
            body.age_ms,
            body.lines.clone()
        ),
        ("download", Some("carol"), Some(9_000), known.lines),
        "{check:?}"
    );
    assert_eq!(check.unchecked_bodies, 1, "upload was not compared");
}

#[test]
fn a_path_reports_its_holder_its_peers_lines_and_whether_it_is_stale() {
    let mut state = ready();
    let outcome = run(
        &mut state,
        "admit",
        Command::Admit {
            session_id: "carol".into(),
            paths: vec!["src/held.rs".into()],
            ttl_ms: 60_000,
        },
        10,
    );
    assert!(matches!(outcome, Outcome::Admitted { .. }), "{outcome:?}");
    run(
        &mut state,
        "read",
        Command::ObserveSource {
            session_id: "bob".into(),
            files: vec![
                ObservedFile {
                    path: "src/old.rs".into(),
                    state: version("v1"),
                },
                ObservedFile {
                    path: "src/same.rs".into(),
                    state: version("s1"),
                },
            ],
            origin: SourceOrigin::Read,
        },
        10,
    );
    let asked = request(
        &state,
        "bob",
        &["src/held.rs", "src/old.rs", "src/same.rs", "src/held.rs"],
        &[],
    );
    assert_eq!(
        asked.paths,
        ["src/held.rs", "src/old.rs", "src/same.rs"],
        "repeats collapse"
    );
    let written = |path: &str| WrittenLines {
        path: path.into(),
        session_id: "alice".into(),
        lines: vec![LineRange {
            start_line: 2,
            end_line: 3,
        }],
        age_ms: 500,
    };
    let facts = vec![
        PathFacts {
            path: "src/held.rs".into(),
            current: Some(version("h")),
            written: vec![],
        },
        PathFacts {
            path: "src/old.rs".into(),
            current: Some(version("v2")),
            written: vec![written("src/old.rs")],
        },
        PathFacts {
            path: "src/same.rs".into(),
            current: Some(version("s1")),
            written: vec![written("src/same.rs")],
        },
    ];
    let check = write_check(&state, "bob", &asked, &facts, None, 20);
    assert_eq!(check.paths[0].held_by.as_deref(), Some("carol"));
    assert_eq!(check.paths[0].stale, None, "never read");
    assert_eq!(check.paths[1].stale, Some(true));
    assert_eq!(check.paths[1].unread_lines[0].session_id, "alice");
    assert_eq!(
        check.paths[1].unread_lines[0].lines,
        [LineRange {
            start_line: 2,
            end_line: 3
        }]
    );
    assert_eq!(check.paths[2].stale, Some(false));
    assert!(
        check.paths[2].unread_lines.is_empty(),
        "it read exactly this content"
    );
    // The holder itself is not told it holds the file; an expired hold is no hold.
    let own = request(&state, "carol", &["src/held.rs"], &[]);
    assert_eq!(
        write_check(&state, "carol", &own, &no_paths(&own), None, 20).paths[0].held_by,
        None
    );
    assert_eq!(
        write_check(&state, "bob", &asked, &facts, None, 60_011).paths[0].held_by,
        None
    );
}

#[test]
fn one_check_is_bounded_and_says_what_it_left_out() {
    let mut state = ready();
    // A notice holds at most 16 changes; two files hold 19.
    for (id, path, range) in [("n1", "src/a.rs", 0..16), ("n2", "src/b.rs", 16..19)] {
        let changes: Vec<InterfaceChange> = range
            .map(|i| changed(&format!("f{i:02}"), "removed", "pub fn f()", None))
            .collect();
        notice(&mut state, id, path, "alice", 10, changes);
    }
    assert_eq!(WRITE_CHECK_SYMBOLS, 16);
    let asked = request(&state, "bob", &["src/a.rs", "src/b.rs"], &[]);
    let check = write_check(
        &state,
        "bob",
        &asked,
        &no_paths(&asked),
        Some(&read_all(&[])),
        20,
    );
    assert_eq!(check.symbols.len(), WRITE_CHECK_SYMBOLS);
    assert_eq!(check.omitted_symbols, 3);

    // Writers per path are capped newest first, and an answer too large for
    // its frame drops symbols, then writers, and counts each.
    let writers: Vec<WrittenLines> = (0..WRITE_CHECK_WRITERS + 2)
        .map(|i| WrittenLines {
            path: "src/a.rs".into(),
            session_id: format!("w{i}"),
            lines: vec![LineRange {
                start_line: 1,
                end_line: 1,
            }],
            age_ms: 100 - i as u64,
        })
        .collect();
    let facts = [PathFacts {
        path: "src/a.rs".into(),
        current: None,
        written: writers,
    }];
    let mut check = write_check(&state, "bob", &asked, &facts, Some(&read_all(&[])), 20);
    let unread = &check.paths[0].unread_lines;
    assert_eq!(unread.len(), WRITE_CHECK_WRITERS);
    assert_eq!(
        unread[0].session_id,
        format!("w{}", WRITE_CHECK_WRITERS + 1)
    );
    assert_eq!(check.paths[0].omitted_writers, 2);
    check.shrink_until(|check| check.symbols.len() + check.paths[0].unread_lines.len() <= 1);
    assert!(check.symbols.is_empty());
    assert_eq!(check.omitted_symbols, 3 + WRITE_CHECK_SYMBOLS);
    assert_eq!(check.paths[0].unread_lines.len(), 1);
    assert_eq!(check.paths[0].omitted_writers, 2 + WRITE_CHECK_WRITERS - 1);
    check.shrink_until(|_| false);
    assert!(check.paths[0].unread_lines.is_empty(), "stops when empty");

    let strings = |n: usize, f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<Vec<_>>();
    for (paths, names, code) in [
        (vec![], vec![], "invalid_paths"),
        (strings(9, &|i| format!("f{i}.rs")), vec![], "invalid_paths"),
        (
            vec!["a.rs".to_owned()],
            strings(17, &|i| format!("n{i}")),
            "invalid_symbols",
        ),
        (vec!["../a.rs".to_owned()], vec![], "invalid_path"),
        (vec!["/etc/passwd".to_owned()], vec![], "invalid_path"),
        (
            vec!["a.rs".to_owned()],
            vec!["up load".to_owned()],
            "invalid_name",
        ),
        (vec!["a.rs".to_owned()], vec![String::new()], "invalid_name"),
        (
            vec!["a.rs".to_owned()],
            vec!["x".repeat(129)],
            "invalid_name",
        ),
    ] {
        let refused = write_check_request(&state, "bob", &paths, &names).unwrap_err();
        assert_eq!(refused.code, code, "{paths:?} {names:?}");
    }
    assert_eq!(
        write_check_request(&state, "nobody", &["a.rs".into()], &[])
            .unwrap_err()
            .code,
        "session_not_found"
    );
}

#[test]
fn the_core_never_records_a_check() {
    let command: Command = serde_json::from_str(
        r#"{"op":"check_write","session_id":"bob","paths":["src/a.rs"],"symbols":["upload"]}"#,
    )
    .unwrap();
    assert!(matches!(&command, Command::CheckWrite { symbols, .. } if symbols == &["upload"]));
    let without: Command =
        serde_json::from_str(r#"{"op":"check_write","session_id":"bob","paths":["src/a.rs"]}"#)
            .unwrap();
    assert!(matches!(&without, Command::CheckWrite { symbols, .. } if symbols.is_empty()));
    let decision = decide(&ready(), &command, "read", 0);
    assert!(decision.events.is_empty());
    assert!(
        matches!(decision.outcome, Outcome::Error { ref code, .. } if code == "storage_operation")
    );
    assert_eq!(
        langspan_core::acting_session(&command),
        None,
        "not even a sighting"
    );
}
