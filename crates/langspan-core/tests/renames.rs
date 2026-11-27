//! A renamed function in an interface notice (roadmap B10, D7): the old name
//! in `name` and `before_name`, the new one in `after_name`, both
//! signatures, what links them in `evidence`, and no counts, checked at the
//! core's edge. A definition that moved with its whole file keeps its name,
//! and the file's notes and history follow it to the new path.
use langspan_core::{
    Command, Event, HistoryQuery, InterfaceChange, InterfaceNotice, Note, Outcome, RecordedEvent,
    State, apply, decide, definition_history, renamed_to,
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

fn renamed() -> InterfaceChange {
    InterfaceChange {
        name: "fetch_user".into(),
        kind: "function".into(),
        change: "renamed".into(),
        before: Some("def fetch_user(id)".into()),
        after: Some("def load_user(id)".into()),
        after_name: Some("load_user".into()),
        before_name: Some("fetch_user".into()),
        evidence: Some("same_body".into()),
        before_path: None,
        private: false,
        before_arity: None,
        after_arity: None,
        before_count: None,
        after_count: None,
    }
}

/// `save` moved with its file from `old` to the notice's path.
fn moved(name: &str, old: &str) -> InterfaceChange {
    let signature = format!("def {name}(x)");
    InterfaceChange {
        name: name.into(),
        before: Some(signature.clone()),
        after: Some(signature),
        after_name: Some(name.into()),
        before_name: Some(name.into()),
        evidence: Some("file_moved".into()),
        before_path: Some(old.into()),
        ..renamed()
    }
}

fn observed(state: &State, path: &str, change: InterfaceChange, id: &str) -> Decision {
    let decision = decide(
        state,
        &Command::ObserveInterface {
            workspace_id: "work".into(),
            path: path.into(),
            changes: vec![change],
            dependents: vec![],
        },
        id,
        10,
    );
    Decision(decision.events, decision.outcome)
}

struct Decision(Vec<Event>, Outcome);

fn refused_at(path: &str, change: InterfaceChange) -> Option<String> {
    match observed(&ready(), path, change, "notice").1 {
        Outcome::Error { code, .. } => Some(code),
        _ => None,
    }
}

fn refused(change: InterfaceChange) -> Option<String> {
    refused_at("app/users.py", change)
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
            before_name: None,
            evidence: None,
            ..renamed()
        },
        InterfaceChange {
            change: "signature_changed".into(),
            before_name: None,
            evidence: None,
            ..renamed()
        },
    ] {
        assert_eq!(refused(bad.clone()), invalid, "{bad:?}");
    }
}

#[test]
fn a_renames_evidence_old_name_and_old_path_are_checked() {
    assert_eq!(
        refused(InterfaceChange {
            evidence: Some("same_signature".into()),
            ..renamed()
        }),
        None
    );
    assert_eq!(refused(moved("save", "app/old.py")), None);
    let invalid = Some("invalid_notice".to_owned());
    for bad in [
        // Unknown evidence.
        InterfaceChange {
            evidence: Some("similar".into()),
            ..renamed()
        },
        // The old name is the name.
        InterfaceChange {
            before_name: Some("load_user".into()),
            ..renamed()
        },
        // Only a rename has evidence or an old name.
        InterfaceChange {
            change: "signature_changed".into(),
            after_name: None,
            before_name: None,
            ..renamed()
        },
        InterfaceChange {
            change: "signature_changed".into(),
            after_name: None,
            evidence: None,
            ..renamed()
        },
        // An old path only for a moved file, and a moved file always has
        // one, keeps its name and was not at the notice's path.
        InterfaceChange {
            before_path: Some("app/old.py".into()),
            ..renamed()
        },
        InterfaceChange {
            before_path: None,
            ..moved("save", "app/old.py")
        },
        InterfaceChange {
            after_name: Some("store".into()),
            ..moved("save", "app/old.py")
        },
        moved("save", "app/users.py"),
    ] {
        assert_eq!(refused(bad.clone()), invalid, "{bad:?}");
    }
    for outside in ["../outside.py", "/abs/old.py"] {
        assert_eq!(
            refused(moved("save", outside)),
            Some("invalid_path".to_owned()),
            "{outside}"
        );
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
    assert!(text.contains(r#""before_name":"fetch_user""#), "{text}");
    assert!(text.contains(r#""evidence":"same_body""#), "{text}");
    assert!(!text.contains("before_path"), "{text}");
    assert_eq!(
        serde_json::from_str::<InterfaceChange>(&text).unwrap(),
        renamed()
    );
}

/// Roadmap D7: a rename recorded before `before_name` and `evidence` were
/// kept still reads, is accepted, and keeps its place in history.
#[test]
fn an_older_rename_without_its_old_name_or_evidence_still_reads() {
    let old: InterfaceChange = serde_json::from_str(
        r#"{"name":"fetch_user","kind":"function","change":"renamed","before":"def fetch_user(id)","after":"def load_user(id)","after_name":"load_user"}"#,
    )
    .unwrap();
    assert_eq!(
        (old.before_name.as_deref(), old.evidence.as_deref()),
        (None, None)
    );
    assert_eq!(refused(old.clone()), None);
    let mut state = ready();
    for event in observed(&state, "app/users.py", old, "n1").0 {
        apply(&mut state, &event).unwrap();
    }
    let found = renamed_to(&state, "work", "fetch_user").unwrap();
    assert_eq!(
        found.last().map(|hop| hop.after_name.as_str()),
        Some("load_user")
    );
    assert_eq!(found[0].evidence, None);
}

fn remember(state: &mut State, id: &str, paths: &[&str]) {
    let decision = decide(
        state,
        &Command::Remember {
            workspace_id: "work".into(),
            author: None,
            text: "keep the retry off".into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
            symbol: None,
            tags: Vec::new(),
        },
        id,
        5,
    );
    for event in decision.events {
        apply(state, &event).unwrap();
    }
}

fn notes(state: &State) -> Vec<Note> {
    state.notes.get("work").cloned().unwrap_or_default()
}

#[test]
fn a_moved_files_notes_follow_it_to_the_new_path() {
    let mut state = ready();
    remember(&mut state, "note-a", &["src/a.rs", "README.md"]);
    remember(&mut state, "note-other", &["src/other.rs"]);
    for event in observed(&state, "src/b.rs", moved("save", "src/a.rs"), "move").0 {
        apply(&mut state, &event).unwrap();
    }
    let paths: Vec<(String, Vec<String>)> = notes(&state)
        .into_iter()
        .map(|note| (note.note_id, note.paths))
        .collect();
    assert!(
        paths.contains(&("note-a".into(), vec!["README.md".into(), "src/b.rs".into()])),
        "{paths:?}"
    );
    assert!(
        paths.contains(&("note-other".into(), vec!["src/other.rs".into()])),
        "{paths:?}"
    );
}

/// A file with no exports, moved whole, is told by its private definitions
/// (`private`): its notes follow it, and the notice is history only, never
/// held for briefings. `private` comes only with `file_moved`.
#[test]
fn a_moved_file_without_exports_takes_its_notes_and_is_not_held() {
    let mut state = ready();
    remember(&mut state, "note-a", &["src/a.rs"]);
    let private = InterfaceChange {
        private: true,
        ..moved("helper", "src/a.rs")
    };
    for event in observed(&state, "src/b.rs", private.clone(), "move").0 {
        apply(&mut state, &event).unwrap();
    }
    assert_eq!(notes(&state)[0].paths, ["src/b.rs"]);
    assert!(state.interface_notices.is_empty(), "{state:?}");
    let invalid = Some("invalid_notice".to_owned());
    assert_eq!(refused_at("src/b.rs", private), None);
    assert_eq!(
        refused(InterfaceChange {
            private: true,
            ..renamed()
        }),
        invalid
    );
}

#[test]
fn a_rename_is_followed_to_its_newest_name_and_forgotten_when_the_name_returns() {
    let mut state = ready();
    let rename = |from: &str, to: &str| InterfaceChange {
        name: from.into(),
        after_name: Some(to.into()),
        before_name: Some(from.into()),
        ..renamed()
    };
    assert_eq!(renamed_to(&state, "work", "fetch_user"), None);
    for (id, change) in [
        ("n1", rename("fetch_user", "load_user")),
        ("n2", rename("load_user", "read_user")),
    ] {
        for event in observed(&state, "app/users.py", change, id).0 {
            apply(&mut state, &event).unwrap();
        }
    }
    let hops = renamed_to(&state, "work", "fetch_user").unwrap();
    assert_eq!(
        hops.iter()
            .map(|hop| (hop.name.as_str(), hop.after_name.as_str()))
            .collect::<Vec<_>>(),
        [("fetch_user", "load_user"), ("load_user", "read_user")]
    );
    assert_eq!(hops[0].path, "app/users.py");
    assert_eq!(hops[0].evidence.as_deref(), Some("same_body"));
    // A move keeps the name: not a rename to follow.
    for event in observed(
        &state,
        "app/people.py",
        moved("read_user", "app/users.py"),
        "n3",
    )
    .0
    {
        apply(&mut state, &event).unwrap();
    }
    assert_eq!(renamed_to(&state, "work", "read_user"), None);
    // `fetch_user` defined again: it is no longer a name that went.
    let added = InterfaceChange {
        change: "added".into(),
        before: None,
        after_name: None,
        before_name: None,
        evidence: None,
        ..renamed()
    };
    for event in observed(&state, "app/users.py", added, "n4").0 {
        apply(&mut state, &event).unwrap();
    }
    assert_eq!(renamed_to(&state, "work", "fetch_user"), None);
    assert!(renamed_to(&state, "work", "load_user").is_some());
    // A removed workspace forgets its renames with its notes.
    apply(
        &mut state,
        &Event::WorkspaceRemoved {
            workspace_id: "work".into(),
        },
    )
    .unwrap();
    assert!(state.renames().is_empty());
}

/// Review finding on #203: function `fetch_user` became `load_user`, then
/// an unrelated method `load_user` became `read_user`. The function's chain
/// ends at `load_user`; the method's rename is never a hop of it, and
/// neither rename replaces the other.
#[test]
fn a_rename_chain_keeps_to_the_first_renames_kind() {
    let mut state = ready();
    let method = InterfaceChange {
        name: "load_user".into(),
        kind: "method".into(),
        before: Some("def load_user(self, id)".into()),
        after: Some("def read_user(self, id)".into()),
        after_name: Some("read_user".into()),
        before_name: Some("load_user".into()),
        ..renamed()
    };
    for (id, change) in [("n1", renamed()), ("n2", method)] {
        for event in observed(&state, "app/users.py", change, id).0 {
            apply(&mut state, &event).unwrap();
        }
    }
    let hops = |name| -> Vec<(String, String, String)> {
        renamed_to(&state, "work", name)
            .unwrap_or_default()
            .into_iter()
            .map(|hop| (hop.kind, hop.name, hop.after_name))
            .collect()
    };
    assert_eq!(
        hops("fetch_user"),
        [("function".into(), "fetch_user".into(), "load_user".into())]
    );
    assert_eq!(
        hops("load_user"),
        [("method".into(), "load_user".into(), "read_user".into())]
    );
}

/// A method's rename of a name does not replace a function's rename of the
/// same name: function `fetch_user` became `load_user`, then `get_user`,
/// and a method `load_user` became `read_user` after both.
#[test]
fn a_rename_of_another_kind_keeps_a_chains_hop() {
    let mut state = ready();
    let rename = |kind: &str, from: &str, to: &str| InterfaceChange {
        name: from.into(),
        kind: kind.into(),
        after_name: Some(to.into()),
        before_name: Some(from.into()),
        ..renamed()
    };
    for (id, change) in [
        ("n1", rename("function", "fetch_user", "load_user")),
        ("n2", rename("function", "load_user", "get_user")),
        ("n3", rename("method", "load_user", "read_user")),
    ] {
        for event in observed(&state, "app/users.py", change, id).0 {
            apply(&mut state, &event).unwrap();
        }
    }
    let hops = renamed_to(&state, "work", "fetch_user").unwrap();
    assert_eq!(
        hops.iter()
            .map(|hop| (hop.kind.as_str(), hop.after_name.as_str()))
            .collect::<Vec<_>>(),
        [("function", "load_user"), ("function", "get_user")]
    );
}

fn notice(id: &str, path: &str, change: InterfaceChange) -> Event {
    Event::InterfaceChanged {
        notice: InterfaceNotice {
            notice_id: id.into(),
            workspace_id: "work".into(),
            path: path.into(),
            changes: vec![change],
            dependents: Vec::new(),
            elsewhere: Vec::new(),
            by_session: None,
            at_ms: 10,
            generation: 1,
        },
    }
}

#[test]
fn the_history_of_a_moved_file_includes_what_was_recorded_under_its_old_path() {
    let state = ready();
    let changed = InterfaceChange {
        name: "save".into(),
        change: "signature_changed".into(),
        before: Some("def save(x)".into()),
        after: Some("def save(x, y)".into()),
        after_name: None,
        before_name: None,
        evidence: None,
        ..renamed()
    };
    let events: Vec<RecordedEvent> = [
        notice("n1", "src/a.py", changed.clone()),
        notice("n2", "src/other.py", changed),
        notice("n3", "src/b.py", moved("save", "src/a.py")),
        Event::Remembered {
            note: Note {
                note_id: "note-a".into(),
                workspace_id: "work".into(),
                author: None,
                text: "save must stay idempotent".into(),
                paths: vec!["src/a.py".into()],
                anchor: None,
                anchor_state: None,
                tags: Vec::new(),
            },
            at: Some(langspan_core::Stamp {
                at_ms: 9,
                generation: 1,
            }),
        },
    ]
    .into_iter()
    .zip(1u64..)
    .map(|(event, seq)| RecordedEvent { seq, event })
    .collect();
    let history = definition_history(
        &state,
        &HistoryQuery {
            workspace_id: "work",
            name: "save",
            path: Some("src/b.py"),
            limit: 10,
            before_seq: None,
            events: &events,
            moves: &[],
            anchors: &[],
            now_ms: 20,
        },
    )
    .unwrap();
    let entries: Vec<(&str, &str)> = history
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.change.as_str()))
        .collect();
    assert_eq!(
        entries,
        [("src/b.py", "renamed"), ("src/a.py", "signature_changed")],
        "{history:#?}"
    );
    assert_eq!(history.entries[0].evidence.as_deref(), Some("file_moved"));
    assert_eq!(history.entries[0].before_path.as_deref(), Some("src/a.py"));
    // The note on the old path is shown with the change at the new one.
    assert_eq!(
        history.entries[0]
            .notes
            .iter()
            .map(|note| note.note_id.as_str())
            .collect::<Vec<_>>(),
        ["note-a"]
    );
}

/// A path another file takes after a move belongs to that file from then
/// on: `a.py` moves to `b.py`, a new `a.py` is recorded and moves to
/// `c.py`. Each of `b.py` and `c.py` has its own file's history and notes,
/// never the other's.
#[test]
fn a_reused_path_keeps_each_files_history_to_its_own_lifetime() {
    let state = ready();
    let changed = |after: &str| InterfaceChange {
        name: "save".into(),
        change: "signature_changed".into(),
        before: Some("def save(x)".into()),
        after: Some(after.into()),
        after_name: None,
        before_name: None,
        evidence: None,
        ..renamed()
    };
    let events: Vec<RecordedEvent> = [
        Event::Remembered {
            note: Note {
                note_id: "note-first".into(),
                workspace_id: "work".into(),
                author: None,
                text: "the first save must stay idempotent".into(),
                paths: vec!["src/a.py".into()],
                anchor: None,
                anchor_state: None,
                tags: Vec::new(),
            },
            at: Some(langspan_core::Stamp {
                at_ms: 9,
                generation: 1,
            }),
        },
        notice("n1", "src/a.py", changed("def save(x, first)")),
        notice("n2", "src/b.py", moved("save", "src/a.py")),
        notice("n3", "src/a.py", changed("def save(x, second)")),
        notice("n4", "src/c.py", moved("save", "src/a.py")),
    ]
    .into_iter()
    .zip(1u64..)
    .map(|(event, seq)| RecordedEvent { seq, event })
    .collect();
    let history_of = |path| {
        definition_history(
            &state,
            &HistoryQuery {
                workspace_id: "work",
                name: "save",
                path: Some(path),
                limit: 10,
                before_seq: None,
                events: &events,
                moves: &[],
                anchors: &[],
                now_ms: 20,
            },
        )
        .unwrap()
    };
    let notices = |history: &langspan_core::History| -> Vec<(String, Vec<String>)> {
        history
            .entries
            .iter()
            .map(|entry| {
                let notes = entry.notes.iter().map(|note| note.note_id.clone());
                (entry.notice_id.clone(), notes.collect())
            })
            .collect()
    };
    let first = history_of("src/b.py");
    assert_eq!(
        notices(&first),
        [
            ("n2".into(), vec!["note-first".into()]),
            ("n1".into(), vec!["note-first".into()])
        ],
        "{first:#?}"
    );
    let second = history_of("src/c.py");
    assert_eq!(
        notices(&second),
        [("n4".into(), vec![]), ("n3".into(), vec![])],
        "{second:#?}"
    );
    // Asked by the path the second file left, it is that file's history.
    let left = history_of("src/a.py");
    assert_eq!(
        notices(&left),
        [("n4".into(), vec![]), ("n3".into(), vec![])],
        "{left:#?}"
    );
}
