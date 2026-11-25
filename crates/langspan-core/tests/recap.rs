//! Remembered notes, scan outcomes and the bounded account of a workspace.
use langspan_core::{Command, Event, Outcome, Recap, State, ToolResult, apply, decide};

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

fn edited(state: &mut State, session: &str, path: &str, now: u64) {
    run(
        state,
        &format!("o-{session}-{now}"),
        Command::Observe {
            session_id: session.into(),
            operation_id: format!("op-{now}"),
            source: "test".into(),
            tool: "Edit".into(),
            paths: vec![path.into()],
            result: ToolResult::Succeeded,
            lease_id: None,
        },
        now,
    );
}

fn remember(
    state: &mut State,
    id: &str,
    author: Option<&str>,
    text: &str,
    paths: &[&str],
) -> Outcome {
    run(
        state,
        id,
        Command::Remember {
            workspace_id: "work".into(),
            author: author.map(str::to_owned),
            text: text.into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
        },
        5,
    )
}

fn notes_for(state: &mut State, session: &str, id: &str) -> Vec<String> {
    match run(
        state,
        id,
        Command::Brief {
            session_id: session.into(),
            max_peers: 4,
            deliver: true,
            mid_turn: false,
        },
        50,
    ) {
        Outcome::Briefing { briefing } => {
            briefing.notes.into_iter().map(|note| note.text).collect()
        }
        other => panic!("{other:?}"),
    }
}

fn recap(state: &mut State) -> Recap {
    match run(
        state,
        "read",
        Command::Recap {
            workspace_id: "work".into(),
        },
        100,
    ) {
        Outcome::Recap { recap } => recap,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_note_reaches_sessions_it_concerns_and_survives_a_restart_until_forgotten() {
    let mut state = ready();
    edited(&mut state, "alice", "src/api.rs", 10);
    edited(&mut state, "bob", "docs/guide.md", 10);
    remember(
        &mut state,
        "n-file",
        Some("bob"),
        "api.rs is generated; edit api.tmpl",
        &["src/api.rs"],
    );
    remember(
        &mut state,
        "n-all",
        None,
        "Release freeze until Friday",
        &[],
    );
    // The file note concerns alice; the workspace note concerns everyone;
    // an author is not told their own note.
    assert_eq!(
        notes_for(&mut state, "alice", "b1"),
        [
            "Release freeze until Friday",
            "api.rs is generated; edit api.tmpl"
        ]
    );
    assert_eq!(
        notes_for(&mut state, "bob", "b2"),
        ["Release freeze until Friday"]
    );

    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    assert_eq!(
        recap(&mut state).notes.len(),
        2,
        "remembered across a restart"
    );
    let forgotten = run(
        &mut state,
        "f1",
        Command::Forget {
            workspace_id: "work".into(),
            note_id: "n-all".into(),
        },
        1,
    );
    assert!(matches!(
        forgotten,
        Outcome::Forgotten { removed: true, .. }
    ));
    let again = decide(
        &state,
        &Command::Forget {
            workspace_id: "work".into(),
            note_id: "n-all".into(),
        },
        "f2",
        2,
    );
    assert!(again.events.is_empty());
    assert!(matches!(
        again.outcome,
        Outcome::Forgotten { removed: false, .. }
    ));
    assert_eq!(recap(&mut state).notes[0].note_id, "n-file");
}

#[test]
fn notes_are_bounded_single_line_and_validated() {
    let mut state = ready();
    for (text, paths) in [
        ("", vec![]),
        ("two\nlines", vec![]),
        (&"x".repeat(401)[..], vec![]),
        ("ok", vec!["../escape"]),
        ("ok", vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"]),
    ] {
        let outcome = remember(&mut state, "bad", None, text, &paths);
        assert!(
            matches!(outcome, Outcome::Error { .. }),
            "{text:?} {paths:?}"
        );
    }
    assert!(matches!(
        remember(&mut state, "bad", Some("nobody"), "ok", &[]),
        Outcome::Error { .. }
    ));
    for index in 0..70 {
        remember(
            &mut state,
            &format!("n{index}"),
            None,
            &format!("note {index}"),
            &[],
        );
    }
    assert_eq!(state.notes["work"].len(), 64);
    assert_eq!(state.notes["work"][0].text, "note 69", "newest kept");
}

#[test]
fn a_failed_scan_is_recorded_once_and_cleared_by_a_complete_one() {
    let mut state = ready();
    let scan = |failure: Option<&str>| Command::ObserveScan {
        workspace_id: "work".into(),
        failure: failure.map(str::to_owned),
        coverage: None,
    };
    assert!(
        decide(&state, &scan(None), "s0", 1).events.is_empty(),
        "nothing to clear"
    );
    assert_eq!(
        decide(&state, &scan(Some("file-count bound exceeded")), "s1", 2)
            .events
            .len(),
        1
    );
    run(&mut state, "s1", scan(Some("file-count bound exceeded")), 2);
    assert!(
        decide(&state, &scan(Some("file-count bound exceeded")), "s2", 3)
            .events
            .is_empty(),
        "the same failure again is the same fact"
    );
    assert_eq!(
        decide(&state, &scan(Some("a different reason")), "s3", 4)
            .events
            .len(),
        1
    );
    assert_eq!(
        recap(&mut state).scan_failure.unwrap().detail,
        "file-count bound exceeded"
    );
    run(&mut state, "s4", scan(None), 5);
    assert!(recap(&mut state).scan_failure.is_none());
    let long = "x\n".repeat(400);
    run(&mut state, "s5", scan(Some(&long)), 6);
    let detail = &state.scan_failures["work"].detail;
    assert!(detail.len() <= 200 && !detail.contains('\n'));
}

#[test]
fn a_recap_is_bounded_and_orders_current_sessions_first() {
    let mut state = ready();
    for index in 0..20 {
        let session = format!("peer{index:02}");
        run(
            &mut state,
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.clone(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            0,
        );
        for file in 0..12 {
            edited(
                &mut state,
                &session,
                &format!("src/f{file}.rs"),
                100 + index * 20 + file,
            );
        }
    }
    let recap = recap(&mut state);
    assert_eq!(recap.sessions.len(), 16);
    assert_eq!(recap.omitted_sessions, 6);
    assert_eq!(recap.sessions[0].session_id, "peer19", "most recent first");
    assert!(
        recap
            .sessions
            .iter()
            .all(|session| session.recent.len() <= 8)
    );
    assert!(
        serde_json::to_vec(&recap).unwrap().len() < 48 * 1024,
        "fits one frame"
    );
    assert!(matches!(
        decide(
            &state,
            &Command::Recap {
                workspace_id: "nowhere".into()
            },
            "r",
            1
        )
        .outcome,
        Outcome::Error { .. }
    ));
}

#[test]
fn a_partial_scan_shows_its_coverage_until_a_scan_versions_everything() {
    let mut state = ready();
    let partial = langspan_core::ScanCoverage {
        omitted: 900,
        too_large: 1,
        too_large_paths: vec!["assets/video.bin".into()],
        ..Default::default()
    };
    let scan = |failure: Option<&str>, coverage: Option<langspan_core::ScanCoverage>| {
        Command::ObserveScan {
            workspace_id: "work".into(),
            failure: failure.map(str::to_owned),
            coverage,
        }
    };
    run(&mut state, "c1", scan(None, Some(partial.clone())), 1);
    assert_eq!(recap(&mut state).scan_coverage, Some(partial.clone()));
    assert!(
        decide(&state, &scan(None, Some(partial.clone())), "c2", 2)
            .events
            .is_empty(),
        "the same coverage again is the same fact"
    );
    // A failed attempt says nothing new about what the last completed scan
    // covered.
    run(&mut state, "c3", scan(Some("deadline elapsed"), None), 3);
    assert_eq!(recap(&mut state).scan_coverage, Some(partial));
    run(&mut state, "c4", scan(None, Some(Default::default())), 4);
    assert_eq!(recap(&mut state).scan_coverage, None);
    let too_many = langspan_core::ScanCoverage {
        too_large: 20,
        too_large_paths: (0..9).map(|i| format!("f{i}")).collect(),
        ..Default::default()
    };
    assert!(matches!(
        decide(&state, &scan(None, Some(too_many)), "c5", 5).outcome,
        Outcome::Error { .. }
    ));
}

// A definition's history (roadmap D4), read from constructed logs.

use langspan_core::{
    ClockAnchor, History, HistoryQuery, InterfaceChange, InterfaceNotice, Message, Note, Recipient,
    RecordedEvent, Stamp, definition_history,
};

const MINUTE: u64 = 60_000;

fn change(change: &str, before: Option<&str>, after: Option<&str>) -> InterfaceChange {
    InterfaceChange {
        name: "save".into(),
        kind: "function".into(),
        change: change.into(),
        before: before.map(str::to_owned),
        after: after.map(str::to_owned),
        before_arity: None,
        after_arity: None,
        before_count: None,
        after_count: None,
    }
}

fn notice(
    id: &str,
    path: &str,
    by: Option<&str>,
    at_ms: u64,
    generation: u64,
    changes: Vec<InterfaceChange>,
) -> Event {
    Event::InterfaceChanged {
        notice: InterfaceNotice {
            notice_id: id.into(),
            workspace_id: "work".into(),
            path: path.into(),
            changes,
            dependents: Vec::new(),
            by_session: by.map(str::to_owned),
            at_ms,
            generation,
        },
    }
}

fn prompted(session: &str, summary: &str, at_ms: u64) -> Event {
    Event::SessionPrompted {
        session_id: session.into(),
        summary: summary.into(),
        line_digests: Vec::new(),
        at_ms,
        generation: 1,
    }
}

fn stamp(at_ms: Option<u64>) -> Option<Stamp> {
    at_ms.map(|at_ms| Stamp {
        at_ms,
        generation: 1,
    })
}

fn note(id: &str, text: &str, paths: &[&str], at_ms: Option<u64>) -> Event {
    Event::Remembered {
        note: Note {
            note_id: id.into(),
            workspace_id: "work".into(),
            author: Some("alice".into()),
            text: text.into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
        },
        at: stamp(at_ms),
    }
}

fn message(id: &str, text: &str, paths: &[&str], at_ms: Option<u64>) -> Event {
    Event::MessageSent {
        message: Message {
            message_id: id.into(),
            workspace_id: "work".into(),
            from: Some("bob".into()),
            to: Recipient::Everyone {},
            text: text.into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
            acknowledged: Vec::new(),
        },
        at: stamp(at_ms),
    }
}

/// Events numbered 10, 20, 30, … in the order given.
fn log(events: Vec<Event>) -> Vec<RecordedEvent> {
    events
        .into_iter()
        .zip(1u64..)
        .map(|(event, index)| RecordedEvent {
            seq: 10 * index,
            event,
        })
        .collect()
}

fn ask(
    state: &State,
    events: &[RecordedEvent],
    anchors: &[ClockAnchor],
    (limit, before_seq): (u32, Option<u64>),
    now_ms: u64,
) -> History {
    definition_history(
        state,
        &HistoryQuery {
            workspace_id: "work",
            name: "save",
            path: None,
            limit,
            before_seq,
            events,
            anchors,
            now_ms,
        },
    )
    .unwrap()
}

#[test]
fn two_signature_changes_by_two_sessions_are_shown_newest_first_with_their_tasks() {
    let state = ready();
    let one = "fn save(x: &Item)";
    let two = "fn save(x: &Item, sync: bool)";
    let three = "fn save(x: &Item, sync: bool, retries: u32)";
    let events = log(vec![
        prompted("alice", "add a sync option to save", MINUTE),
        notice(
            "n1",
            "src/store.rs",
            Some("alice"),
            2 * MINUTE,
            1,
            vec![change("signature_changed", Some(one), Some(two))],
        ),
        prompted("bob", "retry a failed save", 60 * MINUTE),
        // Another name in the same notice is not part of save's history.
        notice(
            "n2",
            "src/store.rs",
            Some("bob"),
            61 * MINUTE,
            1,
            vec![
                change("signature_changed", Some(two), Some(three)),
                InterfaceChange {
                    name: "load".into(),
                    ..change("removed", Some("fn load()"), None)
                },
            ],
        ),
    ]);
    let history = ask(&state, &events, &[], (10, None), 90 * MINUTE);
    assert_eq!(history.entries.len(), 2, "{history:#?}");
    let (newest, oldest) = (&history.entries[0], &history.entries[1]);
    assert_eq!((newest.seq, oldest.seq), (40, 20));
    assert_eq!(newest.before.as_deref(), Some(two));
    assert_eq!(newest.after.as_deref(), Some(three));
    assert_eq!(newest.by_session.as_deref(), Some("bob"));
    assert_eq!(newest.task.as_deref(), Some("retry a failed save"));
    assert_eq!(newest.age_ms, Some(29 * MINUTE));
    assert_eq!(oldest.before.as_deref(), Some(one));
    assert_eq!(oldest.after.as_deref(), Some(two));
    assert_eq!(oldest.by_session.as_deref(), Some("alice"));
    assert_eq!(oldest.task.as_deref(), Some("add a sync option to save"));
    assert!(!newest.reverted && !oldest.reverted);
    assert!(!history.truncated && history.next_seq.is_none());
    // The answer says what `by_session` is, and is not.
    assert!(
        history
            .attribution
            .contains("last session that reported editing")
            && history.attribution.contains("not proof of authorship"),
        "{}",
        history.attribution
    );
    // The same events in another order give the same answer.
    let mut shuffled = events.clone();
    shuffled.reverse();
    assert_eq!(
        ask(&state, &shuffled, &[], (10, None), 90 * MINUTE),
        history
    );
}

#[test]
fn a_change_back_to_an_earlier_signature_is_marked_reverted() {
    let state = ready();
    let a = "fn save(x: &Item)";
    let b = "fn save(x: &Item, sync: bool)";
    let events = log(vec![
        notice(
            "n1",
            "src/store.rs",
            Some("alice"),
            MINUTE,
            1,
            vec![change("signature_changed", Some(a), Some(b))],
        ),
        notice(
            "n2",
            "src/store.rs",
            Some("bob"),
            2 * MINUTE,
            1,
            vec![change("signature_changed", Some(b), Some(a))],
        ),
        // The same signatures in another file are another definition.
        notice(
            "n3",
            "src/other.rs",
            Some("bob"),
            3 * MINUTE,
            1,
            vec![change("signature_changed", Some(b), Some(a))],
        ),
        // Removed, then added again with the signature it had.
        notice(
            "n4",
            "src/store.rs",
            Some("alice"),
            4 * MINUTE,
            1,
            vec![change("removed", Some(a), None)],
        ),
        notice(
            "n5",
            "src/store.rs",
            Some("alice"),
            5 * MINUTE,
            1,
            vec![change("added", None, Some(a))],
        ),
    ]);
    let history = ask(&state, &events, &[], (10, None), 6 * MINUTE);
    let marks: Vec<(u64, &str, bool)> = history
        .entries
        .iter()
        .map(|entry| (entry.seq, entry.path.as_str(), entry.reverted))
        .collect();
    assert_eq!(
        marks,
        [
            (50, "src/store.rs", true),
            (40, "src/store.rs", false),
            (30, "src/other.rs", false),
            (20, "src/store.rs", true),
            (10, "src/store.rs", false),
        ]
    );
    assert_eq!(history.entries[0].change, "added");
}

#[test]
fn notes_and_messages_naming_the_file_near_a_change_are_shown_with_it() {
    let state = ready();
    let at = 100 * MINUTE;
    let store = &["src/store.rs"];
    let events = log(vec![
        note("early", "too early", store, Some(at - 40 * MINUTE)),
        notice(
            "n1",
            "src/store.rs",
            Some("bob"),
            at,
            1,
            vec![change(
                "signature_changed",
                Some("fn save()"),
                Some("fn save(sync: bool)"),
            )],
        ),
        note(
            "why",
            "sync deadlocks under load; keep it off",
            store,
            Some(at + 5 * MINUTE),
        ),
        note(
            "named",
            "see src/store.rs for the lock order",
            &[],
            Some(at + 6 * MINUTE),
        ),
        note(
            "elsewhere",
            "unrelated",
            &["src/api.rs"],
            Some(at + 5 * MINUTE),
        ),
        note("gone", "forgotten later", store, Some(at + 5 * MINUTE)),
        Event::Forgotten {
            workspace_id: "work".into(),
            note_id: "gone".into(),
        },
        // Recorded before notes kept their time: never placed near anything.
        note("untimed", "no time", store, None),
        note("late", "far too late", store, Some(at + 31 * MINUTE)),
        message("m1", "I changed save; re-read it", store, Some(at + MINUTE)),
        message("m2", "lunch?", &[], Some(at + MINUTE)),
    ]);
    let history = ask(&state, &events, &[], (10, None), at + 60 * MINUTE);
    let entry = &history.entries[0];
    let notes: Vec<&str> = entry
        .notes
        .iter()
        .map(|note| note.note_id.as_str())
        .collect();
    assert_eq!(notes, ["why", "named"]);
    assert_eq!(
        entry.notes[0].text,
        "sync deadlocks under load; keep it off"
    );
    assert_eq!(entry.notes[0].author.as_deref(), Some("alice"));
    let messages: Vec<&str> = entry
        .messages
        .iter()
        .map(|message| message.message_id.as_str())
        .collect();
    assert_eq!(messages, ["m1"]);
    assert_eq!(entry.messages[0].from.as_deref(), Some("bob"));
}

#[test]
fn without_a_prompt_summary_from_the_editing_session_no_task_line_is_shown() {
    let state = ready();
    // Alice's summaries are on; Bob's are off, so he recorded none before
    // the change. A prompt after the change is not the task at the time.
    let events = log(vec![
        prompted("alice", "alice's task", MINUTE),
        notice(
            "n1",
            "src/store.rs",
            Some("bob"),
            2 * MINUTE,
            1,
            vec![change(
                "signature_changed",
                Some("fn save()"),
                Some("fn save(a: u8)"),
            )],
        ),
        prompted("bob", "a later task", 3 * MINUTE),
        notice(
            "n2",
            "src/store.rs",
            None,
            4 * MINUTE,
            1,
            vec![change(
                "signature_changed",
                Some("fn save(a: u8)"),
                Some("fn save(b: u8)"),
            )],
        ),
    ]);
    let history = ask(&state, &events, &[], (10, None), 5 * MINUTE);
    assert_eq!(history.entries.len(), 2);
    assert!(
        history.entries.iter().all(|entry| entry.task.is_none()),
        "{history:#?}"
    );
    assert_eq!(history.entries[0].by_session, None);
    assert_eq!(history.entries[1].by_session.as_deref(), Some("bob"));
}

#[test]
fn a_long_history_is_paged_from_next_seq_and_ages_cross_daemon_restarts() {
    let mut state = ready();
    let signature = |n: u64| format!("fn save(x: [u8; {n}])");
    let events = log((0..5u64)
        .map(|n| {
            notice(
                &format!("n{n}"),
                "src/store.rs",
                Some("alice"),
                (n + 1) * MINUTE,
                1,
                vec![change(
                    "signature_changed",
                    Some(&signature(n)),
                    Some(&signature(n + 1)),
                )],
            )
        })
        .collect());
    // The daemon restarted: generation 2's clock began again at zero.
    apply(&mut state, &Event::Booted { generation: 2 }).unwrap();
    let anchors = [
        ClockAnchor {
            generation: 1,
            clock_ms: 0,
            wall_ms: 1_000_000_000,
        },
        ClockAnchor {
            generation: 2,
            clock_ms: 0,
            wall_ms: 1_000_000_000 + 60 * MINUTE,
        },
    ];
    let seqs =
        |history: &History| -> Vec<u64> { history.entries.iter().map(|entry| entry.seq).collect() };
    let page = ask(&state, &events, &anchors, (2, None), 10 * MINUTE);
    assert_eq!(seqs(&page), [50, 40]);
    assert!(page.truncated);
    assert_eq!(page.next_seq, Some(40));
    // Five minutes into generation 1, against ten minutes into generation
    // 2, which started an hour after generation 1.
    assert_eq!(page.entries[0].age_ms, Some(65 * MINUTE));
    let next = ask(&state, &events, &anchors, (2, page.next_seq), 10 * MINUTE);
    assert_eq!(seqs(&next), [30, 20]);
    let last = ask(&state, &events, &anchors, (2, next.next_seq), 10 * MINUTE);
    assert_eq!(seqs(&last), [10]);
    assert!(!last.truncated && last.next_seq.is_none());
    // Without the anchors, another generation's change has no known age.
    assert_eq!(
        ask(&state, &events, &[], (2, None), MINUTE).entries[0].age_ms,
        None
    );

    // An answer cut to fit keeps the newest entries and says where to go on.
    let mut whole = ask(&state, &events, &anchors, (50, None), 10 * MINUTE);
    whole.shrink_until(|history| history.entries.len() <= 3);
    assert_eq!(seqs(&whole), [50, 40, 30]);
    assert!(whole.truncated);
    assert_eq!(whole.next_seq, Some(30));
    whole.shrink_until(|_| false);
    assert_eq!(seqs(&whole), [50], "the newest entry always stays");
}

#[test]
fn one_events_entries_stay_together_and_are_not_called_truncated_when_nothing_follows() {
    let state = ready();
    let shape = InterfaceChange {
        kind: "struct".into(),
        ..change(
            "signature_changed",
            Some("struct save"),
            Some("struct save<T>"),
        )
    };
    let both = vec![
        change(
            "signature_changed",
            Some("fn save()"),
            Some("fn save(a: u8)"),
        ),
        shape,
    ];
    let one = log(vec![notice(
        "n1",
        "src/store.rs",
        None,
        MINUTE,
        1,
        both.clone(),
    )]);
    let history = ask(&state, &one, &[], (1, None), MINUTE);
    assert_eq!(history.entries.len(), 2, "{history:#?}");
    assert!(!history.truncated && history.next_seq.is_none());
    let two = log(vec![
        notice(
            "n0",
            "src/store.rs",
            None,
            MINUTE,
            1,
            vec![change("added", None, Some("fn save()"))],
        ),
        notice("n1", "src/store.rs", None, 2 * MINUTE, 1, both),
    ]);
    let history = ask(&state, &two, &[], (1, None), MINUTE);
    assert_eq!(history.entries.len(), 2);
    assert!(history.truncated);
    assert_eq!(history.next_seq, Some(20));
}

#[test]
fn a_history_query_is_validated() {
    let state = ready();
    let query = |workspace_id, name, path, limit| HistoryQuery {
        workspace_id,
        name,
        path,
        limit,
        before_seq: None,
        events: &[],
        anchors: &[],
        now_ms: 0,
    };
    for (bad, code) in [
        (query("work", "save", None, 0), "invalid_limit"),
        (query("work", "save", None, 51), "invalid_limit"),
        (query("work", "", None, 10), "invalid_name"),
        (query("work", "two words", None, 10), "invalid_name"),
        (query("work", "save", Some("../x.rs"), 10), "invalid_path"),
    ] {
        let failure = definition_history(&state, &bad).unwrap_err();
        assert_eq!(failure.code, code, "{failure:?}");
    }
    assert!(definition_history(&state, &query("nowhere", "save", None, 10)).is_err());
    let empty = definition_history(&state, &query("work", "save", None, 50)).unwrap();
    assert!(empty.entries.is_empty() && !empty.truncated);
}

#[test]
fn an_added_export_is_recorded_for_history_but_never_held_for_briefings() {
    let mut state = ready();
    let held = |state: &State| -> Vec<String> {
        state
            .interface_notices
            .get("work")
            .into_iter()
            .flatten()
            .map(|notice| notice.notice_id.clone())
            .collect()
    };
    let changed = notice(
        "n1",
        "src/store.rs",
        Some("alice"),
        MINUTE,
        1,
        vec![change(
            "signature_changed",
            Some("fn save()"),
            Some("fn save(a: u8)"),
        )],
    );
    apply(&mut state, &changed).unwrap();
    // An addition alone is valid, and leaves the held notice in place.
    let added = notice(
        "n2",
        "src/store.rs",
        Some("alice"),
        2 * MINUTE,
        1,
        vec![change("added", None, Some("fn save2()"))],
    );
    apply(&mut state, &added).unwrap();
    assert_eq!(held(&state), ["n1"]);
    // Mixed with a real change, only the real change is held.
    let mixed = notice(
        "n3",
        "src/store.rs",
        Some("alice"),
        3 * MINUTE,
        1,
        vec![
            change("removed", Some("fn save(a: u8)"), None),
            change("added", None, Some("fn store(a: u8)")),
        ],
    );
    apply(&mut state, &mixed).unwrap();
    assert_eq!(held(&state), ["n3"]);
    let kept = &state.interface_notices["work"][0].changes;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].change, "removed");
    // An addition has its signature and no before.
    for bad in [
        change("added", None, None),
        change("added", Some("fn a()"), Some("fn b()")),
    ] {
        let event = notice("n4", "src/store.rs", None, 4 * MINUTE, 1, vec![bad]);
        assert!(apply(&mut state.clone(), &event).is_err());
    }
}

#[test]
fn notes_and_messages_are_recorded_with_the_time_they_were_made() {
    let mut state = ready();
    let now = |at_ms| {
        Some(Stamp {
            at_ms,
            generation: 1,
        })
    };
    let decision = decide(
        &state,
        &Command::Remember {
            workspace_id: "work".into(),
            author: None,
            text: "keep save synchronous".into(),
            paths: vec!["src/store.rs".into()],
        },
        "r1",
        42,
    );
    match decision.events.as_slice() {
        [Event::Remembered { at, .. }] => assert_eq!(*at, now(42)),
        other => panic!("{other:?}"),
    }
    for event in &decision.events {
        apply(&mut state, event).unwrap();
    }
    let decision = decide(
        &state,
        &Command::SendMessage {
            workspace_id: "work".into(),
            from: Some("alice".into()),
            to: None,
            text: "save changed".into(),
            paths: vec!["src/store.rs".into()],
        },
        "m1",
        43,
    );
    match decision.events.as_slice() {
        [Event::MessageSent { at, .. }] => assert_eq!(*at, now(43)),
        other => panic!("{other:?}"),
    }
    // A note or message recorded before the time was kept still replays.
    apply(&mut state, &note("old", "no time", &[], None)).unwrap();
    apply(&mut state, &message("m0", "no time", &[], None)).unwrap();
}
