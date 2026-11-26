//! The coordination report (roadmap H8) from decided events, with every
//! time injected on the daemon's clock and on the wall clock.
use std::collections::BTreeMap;

use langspan_core::{
    ClockAnchor, Command, Event, FileKind, FileState, ObservedFile, Outcome, REPORT_SESSIONS,
    RecordedEvent, RefusalKind, Report, ReportCounts, ReportKnown, ReportQuery, SourceOrigin,
    State, TodayWindow, ToolIdentity, apply, decide, known_sessions, open_intents, report,
    session_workspaces,
};

/// 2026-11-25T00:00:00Z, the wall time the daemon's clock reads `START` at.
const WALL: u64 = 1_795_564_800_000;
const START: u64 = 5_000;
const MINUTE: u64 = 60_000;

struct Log {
    state: State,
    events: Vec<RecordedEvent>,
}

fn digest(byte: char) -> String {
    byte.to_string().repeat(64)
}

fn present(byte: char) -> FileState {
    FileState::Present {
        digest: digest(byte),
        kind: FileKind::Regular,
    }
}

impl Log {
    fn record(&mut self, event: Event) {
        apply(&mut self.state, &event).unwrap();
        let seq = self.events.len() as u64 + 1;
        self.events.push(RecordedEvent { seq, event });
    }

    fn run(&mut self, id: &str, command: Command, now: u64) -> Outcome {
        let decision = decide(&self.state, &command, id, now);
        for event in decision.events {
            self.record(event);
        }
        decision.outcome
    }

    fn ready(sessions: &[&str]) -> Self {
        let mut log = Self {
            state: State::default(),
            events: Vec::new(),
        };
        log.record(Event::Booted { generation: 1 });
        log.record(Event::ClockAnchored {
            anchor: ClockAnchor {
                generation: 1,
                clock_ms: START,
                wall_ms: WALL,
            },
        });
        for workspace in ["work", "elsewhere"] {
            log.run(
                &format!("w-{workspace}"),
                Command::WorkspaceRegister {
                    workspace_id: workspace.into(),
                    root: format!("/repos/{workspace}"),
                },
                START,
            );
        }
        for session in sessions {
            log.register(session, "work");
        }
        log
    }

    fn register(&mut self, session: &str, workspace: &str) {
        self.run(
            &format!("s-{session}"),
            Command::SessionRegister {
                session_id: session.into(),
                workspace_id: workspace.into(),
                parent: None,
                headless: false,
            },
            START,
        );
    }

    /// A hook's admission of one edit of `path`, which is `on_disk` now.
    fn edit(
        &mut self,
        session: &str,
        call: &str,
        path: &str,
        on_disk: FileState,
        input: char,
        now: u64,
    ) -> Outcome {
        self.run(
            &format!("admit-{session}-{call}"),
            Command::AdmitTool {
                session_id: session.into(),
                operation: ToolIdentity {
                    operation_id: format!("op-{call}"),
                    source: "test".into(),
                    tool: "Edit".into(),
                    input_digest: digest(input),
                },
                paths: vec![path.into()],
                base: vec![ObservedFile {
                    path: path.into(),
                    state: on_disk,
                }],
                regions: Vec::new(),
                overwrites: Vec::new(),
                ttl_ms: 30_000,
            },
            now,
        )
    }

    fn complete(&mut self, session: &str, call: &str, now: u64) {
        let outcome = self.run(
            &format!("complete-{session}-{call}"),
            Command::Complete {
                session_id: session.into(),
                lease_id: format!("admit-{session}-{call}"),
            },
            now,
        );
        assert!(matches!(outcome, Outcome::Completed { .. }), "{outcome:?}");
    }

    /// A prompt: an event with a time of its own, which the events after it
    /// without one are placed at.
    fn prompt(&mut self, session: &str, now: u64) {
        self.run(
            &format!("prompt-{}", self.events.len()),
            Command::SessionPrompt {
                session_id: session.into(),
                summary: "carry on".into(),
                line_digests: Vec::new(),
            },
            now,
        );
    }

    fn source(&mut self, session: &str, path: &str, state: FileState, origin: SourceOrigin) {
        let outcome = self.run(
            &format!("source-{}", self.events.len()),
            Command::ObserveSource {
                session_id: session.into(),
                files: vec![ObservedFile {
                    path: path.into(),
                    state,
                }],
                origin,
            },
            START,
        );
        assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    }

    fn report_from(&self, from_ms: u64, earlier: bool) -> Report {
        report(&ReportQuery {
            workspace_id: "work",
            window: TodayWindow {
                from_ms,
                to_ms: WALL + 600 * MINUTE,
            },
            events: &self.events,
            anchor: None,
            earlier,
            retained_from_ms: None,
            open_intents: &BTreeMap::new(),
            session_workspaces: &BTreeMap::new(),
            known: None,
        })
    }
}

/// The sessions' workspaces just before the events read, as the store folds
/// them from the older events.
fn members_before(older: &[RecordedEvent]) -> BTreeMap<String, String> {
    let mut members = BTreeMap::new();
    for recorded in older {
        session_workspaces(&mut members, &recorded.event);
    }
    members
}

fn row<'a>(report: &'a Report, session: &str) -> &'a ReportCounts {
    &report
        .sessions
        .iter()
        .find(|row| row.session_id == session)
        .unwrap_or_else(|| panic!("no row for {session}: {report:#?}"))
        .counts
}

fn host<'a>(report: &'a Report, host: &str) -> &'a langspan_core::ReportHost {
    report
        .hosts
        .iter()
        .find(|row| row.host == host)
        .unwrap_or_else(|| panic!("no total for {host}: {report:#?}"))
}

#[test]
fn a_codex_sessions_first_edit_of_an_existing_file_is_base_unknown() {
    let codex = "codex.bob";
    let mut log = Log::ready(&[codex]);
    // Codex reports no read: its first edit has nothing to compare with.
    let first = log.edit(codex, "1", "src/lib.rs", present('a'), '1', START + MINUTE);
    let Outcome::Admitted { lease } = first else {
        panic!("{first:?}");
    };
    assert_eq!(lease.base_unknown, ["src/lib.rs"]);
    // The hook records the file as it was then; the next edit is compared.
    log.source(codex, "src/lib.rs", present('a'), SourceOrigin::Snapshot);
    log.complete(codex, "1", START + MINUTE + 1);
    log.source(codex, "src/lib.rs", present('b'), SourceOrigin::Write);
    log.edit(
        codex,
        "2",
        "src/lib.rs",
        present('b'),
        '2',
        START + 2 * MINUTE,
    );
    log.complete(codex, "2", START + 2 * MINUTE + 1);
    // A new file has no earlier version to miss.
    log.edit(
        codex,
        "3",
        "src/new.rs",
        FileState::Missing,
        '3',
        START + 3 * MINUTE,
    );

    let report = log.report_from(0, false);
    let bob = row(&report, codex);
    assert_eq!(
        (bob.edits_base_unknown, bob.edits_admitted),
        (1, 3),
        "{report:#?}"
    );
    assert_eq!(bob.reads_reported, 0);
    let total = host(&report, "codex");
    assert_eq!(total.not_reported, ["reads_reported"]);
    assert_eq!(report.sessions[0].host, "codex");
}

#[test]
fn a_session_that_ends_with_its_intent_set_leaves_one_open() {
    let (alice, bob) = ("claude-code.alice", "claude-code.bob");
    let mut log = Log::ready(&[alice, bob]);
    let intent = |session: &str, summary: &str| Command::IntentSet {
        session_id: session.into(),
        paths: vec!["src/lib.rs".into()],
        summary: summary.into(),
    };
    let end = |session: &str| Command::SessionEnd {
        session_id: session.into(),
    };
    // Alice sets an intent twice (an update) and ends the session with it.
    log.run("i-a1", intent(alice, "rework the parser"), START + MINUTE);
    log.run(
        "i-a2",
        intent(alice, "rework the lexer too"),
        START + 2 * MINUTE,
    );
    log.run("e-a", end(alice), START + 3 * MINUTE);
    // Bob ends his intent first.
    log.run("i-b", intent(bob, "rework the parser"), START + MINUTE);
    log.run(
        "ie-b",
        Command::IntentEnd {
            session_id: bob.into(),
        },
        START + 2 * MINUTE,
    );
    log.run("e-b", end(bob), START + 3 * MINUTE);

    let report = log.report_from(0, false);
    let a = row(&report, alice);
    assert_eq!(
        (a.intents_set, a.intents_ended, a.intents_left_open),
        (2, 0, 1),
        "{report:#?}"
    );
    let b = row(&report, bob);
    assert_eq!(
        (b.intents_set, b.intents_ended, b.intents_left_open),
        (1, 1, 0)
    );

    // An intent set before the window, left open by an end inside it.
    let mut log = Log::ready(&[alice]);
    log.run("i-a", intent(alice, "rework the parser"), START + MINUTE);
    log.prompt(alice, START + 30 * MINUTE);
    log.run("e-a", end(alice), START + 30 * MINUTE);
    let report = log.report_from(WALL + 10 * MINUTE, false);
    let a = row(&report, alice);
    assert_eq!((a.intents_set, a.intents_left_open), (0, 1), "{report:#?}");
}

/// Review 4703461855: an intent set before the events read, whose session
/// ends inside the window, is still left open. The window starts after the
/// first event read, so the report is not truncated and the older events
/// are only needed for the intent.
#[test]
fn an_intent_set_before_the_events_read_is_left_open_when_its_session_ends() {
    let alice = "claude-code.alice";
    let mut log = Log::ready(&[alice]);
    log.run(
        "i-a",
        Command::IntentSet {
            session_id: alice.into(),
            paths: vec!["src/lib.rs".into()],
            summary: "rework the parser".into(),
        },
        START + MINUTE,
    );
    log.prompt(alice, START + 5 * MINUTE);
    log.prompt(alice, START + 30 * MINUTE);
    log.run(
        "e-a",
        Command::SessionEnd {
            session_id: alice.into(),
        },
        START + 30 * MINUTE,
    );
    // Read from the first prompt on, as if the log held only that many.
    let start = log
        .events
        .iter()
        .position(|recorded| matches!(recorded.event, Event::SessionPrompted { .. }))
        .unwrap();
    let mut before = BTreeMap::new();
    for recorded in &log.events[..start] {
        open_intents(&mut before, &recorded.event);
    }
    assert!(before.contains_key(alice), "{before:?}");
    let cut = report(&ReportQuery {
        workspace_id: "work",
        window: TodayWindow {
            from_ms: WALL + 10 * MINUTE,
            to_ms: WALL + 600 * MINUTE,
        },
        events: &log.events[start..],
        anchor: Some(ClockAnchor {
            generation: 1,
            clock_ms: START,
            wall_ms: WALL,
        }),
        earlier: true,
        retained_from_ms: None,
        open_intents: &before,
        session_workspaces: &members_before(&log.events[..start]),
        known: None,
    });
    assert!(!cut.truncated, "{cut:#?}");
    let a = row(&cut, alice);
    assert_eq!((a.intents_set, a.intents_left_open), (0, 1), "{cut:#?}");
}

/// Review 4703929181: Alice sets an intent, the workspace is removed, which
/// ends it, and is registered again with her session; she ends the session
/// without a new intent. None is left open, whether the removal is among the
/// events read or only in the older events `open_intents` folds.
#[test]
fn a_removed_workspace_ends_its_intents_for_the_report() {
    let alice = "claude-code.alice";
    let mut log = Log::ready(&[alice]);
    log.run(
        "i-a",
        Command::IntentSet {
            session_id: alice.into(),
            paths: vec!["src/lib.rs".into()],
            summary: "rework the parser".into(),
        },
        START + MINUTE,
    );
    let outcome = log.run(
        "r-work",
        Command::WorkspaceRemove {
            workspace_id: "work".into(),
            apply: true,
        },
        START + 2 * MINUTE,
    );
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    log.run(
        "w-work-again",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
        START + 3 * MINUTE,
    );
    log.register(alice, "work");
    log.prompt(alice, START + 5 * MINUTE);
    log.prompt(alice, START + 30 * MINUTE);
    log.run(
        "e-a",
        Command::SessionEnd {
            session_id: alice.into(),
        },
        START + 30 * MINUTE,
    );
    let anchor = Some(ClockAnchor {
        generation: 1,
        clock_ms: START,
        wall_ms: WALL,
    });
    let window = TodayWindow {
        from_ms: WALL + 10 * MINUTE,
        to_ms: WALL + 600 * MINUTE,
    };
    let whole = report(&ReportQuery {
        workspace_id: "work",
        window,
        events: &log.events,
        anchor: None,
        earlier: false,
        retained_from_ms: None,
        open_intents: &BTreeMap::new(),
        session_workspaces: &BTreeMap::new(),
        known: None,
    });
    assert_eq!(row(&whole, alice).intents_left_open, 0, "{whole:#?}");
    // Read from the first prompt on: the removal is in the older events.
    let start = log
        .events
        .iter()
        .position(|recorded| matches!(recorded.event, Event::SessionPrompted { .. }))
        .unwrap();
    let mut before = BTreeMap::new();
    for recorded in &log.events[..start] {
        open_intents(&mut before, &recorded.event);
    }
    assert!(before.is_empty(), "{before:?}");
    let cut = report(&ReportQuery {
        workspace_id: "work",
        window,
        events: &log.events[start..],
        anchor,
        earlier: true,
        retained_from_ms: None,
        open_intents: &before,
        session_workspaces: &members_before(&log.events[..start]),
        known: None,
    });
    assert_eq!(row(&cut, alice).intents_left_open, 0, "{cut:#?}");
}

#[test]
fn a_refusal_followed_by_an_identical_retry_that_is_admitted_counts_once() {
    let (alice, bob) = ("claude-code.alice", "codex.bob");
    let mut log = Log::ready(&[alice, bob]);
    log.source(alice, "src/lib.rs", present('a'), SourceOrigin::Read);
    // Bob holds the file; alice's edit is refused. (A second delivery of the
    // same call is answered from the store's receipt: `tests/report.rs`.)
    log.edit(bob, "hold", "src/lib.rs", present('a'), 'b', START + MINUTE);
    let refused = log.edit(
        alice,
        "try",
        "src/lib.rs",
        present('a'),
        'c',
        START + MINUTE + 1,
    );
    assert!(matches!(refused, Outcome::Conflict { .. }), "{refused:?}");
    // A different call is refused too, and the session never sends it again.
    log.edit(
        alice,
        "other",
        "src/lib.rs",
        present('a'),
        'd',
        START + MINUTE + 3,
    );
    log.complete(bob, "hold", START + MINUTE + 10_000);
    // The identical call under a new tool-call ID, admitted now.
    let retry = log.edit(
        alice,
        "again",
        "src/lib.rs",
        present('a'),
        'c',
        START + MINUTE + 20_000,
    );
    assert!(matches!(retry, Outcome::Admitted { .. }), "{retry:?}");
    log.complete(alice, "again", START + MINUTE + 20_001);
    // Admitted again later: the refusal was already answered.
    log.edit(
        alice,
        "third",
        "src/lib.rs",
        present('a'),
        'c',
        START + MINUTE + 25_000,
    );

    let refusals: Vec<_> = log
        .events
        .iter()
        .filter_map(|recorded| match &recorded.event {
            Event::WriteRefused { input_digest, .. } => Some(input_digest.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(refusals, [Some(digest('c')), Some(digest('d'))]);

    let report = log.report_from(0, false);
    let a = row(&report, alice);
    assert_eq!(a.refusals_total(), 2, "{report:#?}");
    assert_eq!(a.refusals.get(RefusalKind::Conflict.name()), Some(&2));
    assert_eq!(a.retried_unchanged, 1, "{report:#?}");
    assert_eq!(a.edits_admitted, 2);
    assert_eq!(a.reads_reported, 1);
    assert_eq!(row(&report, bob).retried_unchanged, 0);
}

#[test]
fn a_refusal_recorded_before_input_digests_were_kept_is_no_retry() {
    let alice = "claude-code.alice";
    let mut log = Log::ready(&[alice]);
    // An older build's record: no input digest.
    log.record(Event::WriteRefused {
        session_id: alice.into(),
        workspace_id: "work".into(),
        kind: RefusalKind::StaleBase,
        paths: vec!["src/lib.rs".into()],
        peer_session_id: None,
        input_digest: None,
        at_ms: START + MINUTE,
        generation: 1,
    });
    log.edit(
        alice,
        "1",
        "src/lib.rs",
        present('a'),
        'c',
        START + 2 * MINUTE,
    );
    let report = log.report_from(0, false);
    let a = row(&report, alice);
    assert_eq!(a.refusals.get("stale_base"), Some(&1));
    assert_eq!(a.retried_unchanged, 0);
}

#[test]
fn host_totals_are_the_sum_of_every_session_listed_or_not() {
    let sessions: Vec<String> = (0..40)
        .map(|index| {
            // `gemini.`: a session recorded before Gemini CLI support was
            // removed, which counts as `other`.
            let host = ["claude-code", "codex", "cursor", "antigravity", "gemini"][index % 5];
            format!("{host}.s{index:02}")
        })
        .collect();
    let names: Vec<&str> = sessions.iter().map(String::as_str).collect();
    let mut log = Log::ready(&names);
    for (index, session) in names.iter().enumerate() {
        let now = START + index as u64 * 1000;
        log.edit(
            session,
            "e",
            &format!("src/f{index}.rs"),
            present('a'),
            'e',
            now,
        );
        log.complete(session, "e", now + 1);
        if index % 3 == 0 {
            log.source(session, "src/lib.rs", present('a'), SourceOrigin::Read);
        }
    }
    // A session of another workspace is in no row.
    log.register("claude-code.far", "elsewhere");
    log.edit(
        "claude-code.far",
        "e",
        "src/x.rs",
        present('a'),
        'e',
        START + 50_000,
    );

    let report = log.report_from(0, false);
    assert_eq!(report.sessions.len(), REPORT_SESSIONS);
    assert_eq!(report.omitted_sessions as usize, 40 - REPORT_SESSIONS);
    let hosts: Vec<&str> = report.hosts.iter().map(|row| row.host.as_str()).collect();
    assert_eq!(
        hosts,
        ["claude-code", "codex", "cursor", "antigravity", "other"]
    );
    let all = log.report_from(0, false);
    assert_eq!(
        all.hosts.iter().map(|row| row.sessions).sum::<u32>(),
        40,
        "{all:#?}"
    );
    for total in &report.hosts {
        assert_eq!(total.sessions, 8, "{total:?}");
        assert_eq!(total.counts.edits_admitted, 8, "{total:?}");
        assert_eq!(total.counts.edits_base_unknown, 8, "{total:?}");
    }
    assert_eq!(
        report
            .hosts
            .iter()
            .map(|row| row.counts.reads_reported)
            .sum::<u32>(),
        14
    );
    // With every row kept, each total is exactly the sum of its rows.
    let mut few = Log::ready(&["claude-code.a", "claude-code.b", "codex.c"]);
    for (index, session) in ["claude-code.a", "claude-code.b", "codex.c"]
        .iter()
        .enumerate()
    {
        for call in 0..=index {
            let id = format!("{call}");
            let now = START + (index * 10 + call) as u64 * 1000;
            few.edit(
                session,
                &id,
                &format!("src/{session}{call}.rs"),
                present('a'),
                'e',
                now,
            );
            few.complete(session, &id, now + 1);
        }
    }
    let report = few.report_from(0, false);
    assert_eq!(report.omitted_sessions, 0);
    for total in &report.hosts {
        let mut sum = 0;
        let mut sessions = 0;
        for session in report.sessions.iter().filter(|row| row.host == total.host) {
            sum += session.counts.edits_admitted;
            sessions += 1;
        }
        assert_eq!(
            (total.counts.edits_admitted, total.sessions),
            (sum, sessions)
        );
    }
    assert_eq!(host(&report, "claude-code").counts.edits_admitted, 3);
}

#[test]
fn the_window_leaves_out_older_events_and_says_what_it_covered() {
    let alice = "claude-code.alice";
    let mut log = Log::ready(&[alice]);
    log.source(alice, "src/a.rs", present('a'), SourceOrigin::Read);
    log.edit(alice, "1", "src/a.rs", present('a'), '1', START + MINUTE);
    log.complete(alice, "1", START + MINUTE + 1);
    log.prompt(alice, START + 20 * MINUTE);
    log.edit(
        alice,
        "2",
        "src/a.rs",
        present('a'),
        '2',
        START + 20 * MINUTE,
    );
    let windowed = log.report_from(WALL + 10 * MINUTE, false);
    assert_eq!(row(&windowed, alice).edits_admitted, 1, "{windowed:#?}");
    assert!(!windowed.truncated);
    assert_eq!(windowed.covered_from_ms, None);

    // Only the newest events were read, and older ones exist.
    let start = log
        .events
        .iter()
        .position(|recorded| matches!(recorded.event, Event::SourceObserved { .. }))
        .unwrap();
    let newest = Log {
        state: log.state.clone(),
        events: log.events[start..].to_vec(),
    };
    let cut = report(&ReportQuery {
        workspace_id: "work",
        window: TodayWindow {
            from_ms: 0,
            to_ms: WALL + 600 * MINUTE,
        },
        events: &newest.events,
        anchor: Some(ClockAnchor {
            generation: 1,
            clock_ms: START,
            wall_ms: WALL,
        }),
        earlier: true,
        retained_from_ms: None,
        open_intents: &BTreeMap::new(),
        session_workspaces: &members_before(&log.events[..start]),
        known: None,
    });
    assert!(cut.truncated, "{cut:#?}");
    assert_eq!(cut.covered_from_ms, Some(WALL));
    assert_eq!(row(&cut, alice).edits_admitted, 2, "{cut:#?}");
    // An empty log: no rows, nothing truncated.
    let empty = Log::ready(&[]).report_from(0, false);
    assert!(empty.sessions.is_empty() && empty.hosts.is_empty());
}

/// Review 4704154106: Alice works in `work`, the workspace is removed and
/// she registers the same session in `elsewhere`. Her earlier admission,
/// intent and read stay in `work`'s report and are not counted in
/// `elsewhere`'s, whether her first registration is among the events read
/// or only in the older events `session_workspaces` folds.
#[test]
fn a_session_registered_again_elsewhere_keeps_its_earlier_events_in_its_old_workspace() {
    let alice = "claude-code.alice";
    let mut log = Log::ready(&[alice]);
    log.prompt(alice, START + MINUTE);
    log.source(alice, "src/lib.rs", present('a'), SourceOrigin::Read);
    log.edit(alice, "1", "src/lib.rs", present('a'), '1', START + MINUTE);
    log.complete(alice, "1", START + MINUTE + 1);
    log.run(
        "i-a",
        Command::IntentSet {
            session_id: alice.into(),
            paths: vec!["src/lib.rs".into()],
            summary: "rework the parser".into(),
        },
        START + MINUTE,
    );
    let outcome = log.run(
        "r-work",
        Command::WorkspaceRemove {
            workspace_id: "work".into(),
            apply: true,
        },
        START + 2 * MINUTE,
    );
    assert!(!matches!(outcome, Outcome::Error { .. }), "{outcome:?}");
    log.run(
        "s-again",
        Command::SessionRegister {
            session_id: alice.into(),
            workspace_id: "elsewhere".into(),
            parent: None,
            headless: false,
        },
        START + 3 * MINUTE,
    );
    assert_eq!(log.state.sessions[alice].workspace_id, "elsewhere");
    log.prompt(alice, START + 4 * MINUTE);
    log.source(alice, "src/main.rs", present('b'), SourceOrigin::Read);

    let window = TodayWindow {
        from_ms: 0,
        to_ms: WALL + 600 * MINUTE,
    };
    // Read from the first prompt on: the first registration is older.
    let start = log
        .events
        .iter()
        .position(|recorded| matches!(recorded.event, Event::SessionPrompted { .. }))
        .unwrap();
    let mut before = BTreeMap::new();
    for recorded in &log.events[..start] {
        session_workspaces(&mut before, &recorded.event);
    }
    assert_eq!(before.get(alice).map(String::as_str), Some("work"));
    let anchor = Some(ClockAnchor {
        generation: 1,
        clock_ms: START,
        wall_ms: WALL,
    });
    for (events, anchor, members) in [
        (&log.events[..], None, BTreeMap::new()),
        (&log.events[start..], anchor, before),
    ] {
        let none = BTreeMap::new();
        let query = |workspace_id| ReportQuery {
            workspace_id,
            window,
            events,
            anchor,
            earlier: false,
            retained_from_ms: None,
            open_intents: &none,
            session_workspaces: &members,
            known: None,
        };
        let work = report(&query("work"));
        let old = row(&work, alice);
        assert_eq!(
            (old.edits_admitted, old.intents_set, old.reads_reported),
            (1, 1, 1),
            "{work:#?}"
        );
        let elsewhere = report(&query("elsewhere"));
        let new = row(&elsewhere, alice);
        assert_eq!(
            (new.edits_admitted, new.intents_set, new.reads_reported),
            (0, 0, 1),
            "{elsewhere:#?}"
        );
    }
}

/// Review 4704557874, in the core: every event left was read (`earlier`
/// false), and the first is the generation's anchor, older than the window.
/// Retention removed the events before its cutoff all the same, so a window
/// starting before the cutoff is truncated and covers from the cutoff; one
/// starting at the cutoff is complete.
#[test]
fn a_window_before_the_retention_cutoff_is_truncated_though_every_event_left_was_read() {
    let alice = "claude-code.alice";
    let mut log = Log::ready(&[alice]);
    log.prompt(alice, START + 40 * MINUTE);
    log.edit(
        alice,
        "1",
        "src/a.rs",
        present('a'),
        '1',
        START + 40 * MINUTE,
    );
    let cutoff = WALL + 30 * MINUTE;
    let none = BTreeMap::new();
    let query = |from_ms| ReportQuery {
        workspace_id: "work",
        window: TodayWindow {
            from_ms,
            to_ms: WALL + 600 * MINUTE,
        },
        events: &log.events,
        anchor: None,
        earlier: false,
        retained_from_ms: Some(cutoff),
        open_intents: &none,
        session_workspaces: &none,
        known: None,
    };
    let before = report(&query(WALL + 10 * MINUTE));
    assert!(before.truncated, "{before:#?}");
    assert_eq!(before.covered_from_ms, Some(cutoff));
    assert_eq!(row(&before, alice).edits_admitted, 1, "{before:#?}");
    let at = report(&query(cutoff));
    assert!(!at.truncated, "{at:#?}");
    assert_eq!(at.covered_from_ms, None);

    // Truncated by the limit too: the later of the two bounds is given.
    let start = log
        .events
        .iter()
        .position(|recorded| matches!(recorded.event, Event::SessionPrompted { .. }))
        .unwrap();
    let newest = &log.events[start..];
    let both = report(&ReportQuery {
        events: newest,
        anchor: Some(ClockAnchor {
            generation: 1,
            clock_ms: START,
            wall_ms: WALL,
        }),
        earlier: true,
        retained_from_ms: Some(WALL + 5 * MINUTE),
        ..query(WALL)
    });
    assert!(both.truncated, "{both:#?}");
    assert_eq!(both.covered_from_ms, Some(WALL + 40 * MINUTE));
    assert_eq!(row(&both, alice).edits_admitted, 1, "{both:#?}");
}

/// The report and the monthly count (H5) share one retention rule: a window
/// starting before the cutoff may miss removed events, and one starting at
/// or after it is complete. Each says so in its own field.
#[test]
fn the_report_and_the_refusal_count_read_the_retention_cutoff_alike() {
    let cutoff = WALL + 30 * MINUTE;
    let none = BTreeMap::new();
    for from_ms in [cutoff - 1, cutoff, cutoff + 1] {
        let report = report(&ReportQuery {
            workspace_id: "work",
            window: TodayWindow {
                from_ms,
                to_ms: WALL + 600 * MINUTE,
            },
            events: &[],
            anchor: None,
            earlier: false,
            retained_from_ms: Some(cutoff),
            open_intents: &none,
            session_workspaces: &none,
            known: None,
        });
        let count = langspan_core::count_refusals(
            &langspan_core::CountQuery {
                days: &[from_ms, WALL + 600 * MINUTE],
                refusals: &[],
                anchors: &[],
                earlier: false,
                retained_since_ms: Some(cutoff),
            },
            |_, _, _, _| Ok::<_, ()>(false),
        )
        .unwrap();
        assert_eq!(
            report.truncated,
            count.retained_since_ms.is_some(),
            "from {from_ms}: {report:#?} {count:#?}"
        );
        assert_eq!(report.truncated, from_ms < cutoff, "from {from_ms}");
        assert_eq!(
            count.retained_since_ms,
            (from_ms < cutoff).then_some(cutoff),
            "from {from_ms}"
        );
    }
}

/// Review 4705125393: when the store folded only part of the older history,
/// an event whose session's workspace or intent was not among it is in no
/// count and the report says `partial`; a session registered among the
/// events read is counted as usual. Folding the same older events with
/// what they make known gives the full answer and is not partial.
#[test]
fn a_session_unknown_to_a_partial_history_is_not_counted_and_the_report_is_partial() {
    let (alice, bob) = ("claude-code.alice", "codex.bob");
    let mut log = Log::ready(&[alice]);
    log.run(
        "i-a",
        Command::IntentSet {
            session_id: alice.into(),
            paths: vec!["src/lib.rs".into()],
            summary: "rework the parser".into(),
        },
        START + MINUTE,
    );
    log.prompt(alice, START + 5 * MINUTE);
    log.register(bob, "work");
    log.prompt(bob, START + 20 * MINUTE);
    log.prompt(alice, START + 30 * MINUTE);
    log.run(
        "e-a",
        Command::SessionEnd {
            session_id: alice.into(),
        },
        START + 30 * MINUTE,
    );
    let start = log
        .events
        .iter()
        .position(|recorded| matches!(recorded.event, Event::SessionPrompted { .. }))
        .unwrap();
    let cut = |open: &BTreeMap<String, String>,
               members: &BTreeMap<String, String>,
               known: Option<&ReportKnown>| {
        report(&ReportQuery {
            workspace_id: "work",
            window: TodayWindow {
                from_ms: WALL + 10 * MINUTE,
                to_ms: WALL + 600 * MINUTE,
            },
            events: &log.events[start..],
            anchor: Some(ClockAnchor {
                generation: 1,
                clock_ms: START,
                wall_ms: WALL,
            }),
            earlier: true,
            retained_from_ms: None,
            open_intents: open,
            session_workspaces: members,
            known,
        })
    };
    // Nothing of the older history folded.
    let none = BTreeMap::new();
    let partial = cut(&none, &none, Some(&ReportKnown::default()));
    assert!(partial.partial, "{partial:#?}");
    assert!(
        partial.sessions.iter().all(|row| row.session_id != alice),
        "{partial:#?}"
    );
    assert_eq!(row(&partial, bob), &ReportCounts::default(), "{partial:#?}");
    // The same older events folded, with what they make known.
    let (mut open, mut known) = (BTreeMap::new(), ReportKnown::default());
    for recorded in &log.events[..start] {
        open_intents(&mut open, &recorded.event);
        known_sessions(&mut known, &recorded.event);
    }
    let members = members_before(&log.events[..start]);
    let full = cut(&open, &members, Some(&known));
    assert!(!full.partial, "{full:#?}");
    assert_eq!(row(&full, alice).intents_left_open, 1, "{full:#?}");
    assert_eq!(full, cut(&open, &members, None));
}
