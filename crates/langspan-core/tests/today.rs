//! The daily account (roadmap H4), across a simulated midnight: every time
//! is injected, on the daemon's clock and on the wall clock.
use langspan_core::{
    AttemptResult, CheckResult, ClockAnchor, Command, Event, LandingAttempt, Outcome,
    RecordedEvent, RefusalKind, State, TODAY_SESSIONS, Today, TodayQuery, TodayWindow,
    ToolIdentity, ToolResult, TurnQueueing, apply, decide, today,
};

/// Midnight, wall clock, in Unix milliseconds (2026-11-25T00:00:00Z).
const MIDNIGHT: u64 = 1_795_564_800_000;
const HOUR: u64 = 3_600_000;
/// The daemon started an hour before midnight, at 5 s on its own clock.
const START: u64 = 5_000;

struct Log {
    state: State,
    events: Vec<RecordedEvent>,
}

impl Log {
    fn record(&mut self, event: Event) {
        apply(&mut self.state, &event).unwrap();
        let seq = self.events.len() as u64 + 1;
        self.events.push(RecordedEvent { seq, event });
    }

    fn boot(&mut self, generation: u64, clock_ms: u64, wall_ms: Option<u64>) {
        self.record(Event::Booted { generation });
        if let Some(wall_ms) = wall_ms {
            self.record(Event::ClockAnchored {
                anchor: ClockAnchor {
                    generation,
                    clock_ms,
                    wall_ms,
                },
            });
        }
    }

    fn run(&mut self, id: &str, command: Command, now: u64) -> Outcome {
        let decision = decide(&self.state, &command, id, now);
        for event in decision.events {
            self.record(event);
        }
        decision.outcome
    }

    fn ready() -> Self {
        let mut log = Self {
            state: State::default(),
            events: Vec::new(),
        };
        log.boot(1, START, Some(MIDNIGHT - HOUR));
        log.run(
            "w",
            Command::WorkspaceRegister {
                workspace_id: "work".into(),
                root: "/repos/work".into(),
            },
            START,
        );
        for session in ["claude-code.alice", "codex.bob"] {
            log.run(
                &format!("s-{session}"),
                Command::SessionRegister {
                    session_id: session.into(),
                    workspace_id: "work".into(),
                    parent: None,
                    headless: false,
                },
                START,
            );
        }
        log
    }

    fn edited(&mut self, session: &str, path: &str, now: u64) {
        let id = format!("{session}-{path}-{now}").replace('/', "-");
        let outcome = self.run(
            &format!("o-{id}"),
            Command::Observe {
                session_id: session.into(),
                operation_id: format!("op-{id}"),
                source: "test".into(),
                tool: "Edit".into(),
                paths: vec![path.into()],
                result: ToolResult::Succeeded,
                lease_id: None,
            },
            now,
        );
        assert!(matches!(outcome, Outcome::Observed { .. }), "{outcome:?}");
    }

    fn prompted(&mut self, session: &str, now: u64) {
        self.run(
            &format!("p-{session}-{now}"),
            Command::SessionPrompt {
                session_id: session.into(),
                summary: "work on the parser".into(),
                line_digests: Vec::new(),
            },
            now,
        );
    }

    fn today_from(&self, from_ms: u64, earlier: bool, now: u64) -> Today {
        today(
            &self.state,
            &TodayQuery {
                workspace_id: Some("work"),
                window: TodayWindow {
                    from_ms,
                    to_ms: MIDNIGHT + 12 * HOUR,
                },
                events: &self.events,
                anchor: None,
                earlier,
                retained_from_ms: None,
                now_ms: now,
            },
        )
    }
}

/// The daemon's clock at a wall time after the start.
fn at(wall_ms: u64) -> u64 {
    START + (wall_ms - (MIDNIGHT - HOUR))
}

#[test]
fn a_session_that_edited_three_files_today_and_one_yesterday_shows_three() {
    let mut log = Log::ready();
    log.prompted("claude-code.alice", at(MIDNIGHT - 30 * 60_000));
    log.edited(
        "claude-code.alice",
        "src/old.rs",
        at(MIDNIGHT - 20 * 60_000),
    );
    for (minute, path) in [
        (1, "src/a.rs"),
        (2, "src/b.rs"),
        (3, "src/c.rs"),
        (4, "src/a.rs"),
    ] {
        log.prompted("claude-code.alice", at(MIDNIGHT + minute * 60_000));
        log.edited(
            "claude-code.alice",
            path,
            at(MIDNIGHT + minute * 60_000 + 1),
        );
    }
    let summary = log.today_from(MIDNIGHT, false, at(MIDNIGHT + HOUR));
    assert_eq!(summary.sessions.len(), 1, "{summary:?}");
    let alice = &summary.sessions[0];
    assert_eq!(alice.session_id, "claude-code.alice");
    assert_eq!(alice.host, "claude-code");
    assert_eq!(alice.files_edited, 3);
    assert_eq!(alice.files, ["src/a.rs", "src/b.rs", "src/c.rs"]);
    assert_eq!(alice.prompts, 4);
    assert_eq!(alice.first_ms, MIDNIGHT + 60_000);
    assert_eq!(alice.last_ms, MIDNIGHT + 4 * 60_000 + 1);
    assert!(!summary.truncated);
    assert_eq!(summary.undated, 0);

    // A window that starts before the oldest event holds everything.
    let everything = log.today_from(0, false, at(MIDNIGHT + HOUR));
    let alice = &everything.sessions[0];
    assert_eq!((alice.files_edited, alice.prompts), (4, 5));
    // The registration, an hour before midnight, is its first event.
    assert_eq!(alice.first_ms, MIDNIGHT - HOUR);
    assert_eq!(everything.sessions.len(), 2, "bob registered too");
}

#[test]
fn each_refusal_in_the_window_is_counted_once_under_its_kind() {
    let mut log = Log::ready();
    let hold = Command::Admit {
        session_id: "codex.bob".into(),
        paths: vec!["src/lib.rs".into()],
        ttl_ms: 60_000,
    };
    // A hook's tool call: a refused one is a refused write.
    let edit = |id: &str| Command::AdmitTool {
        session_id: "claude-code.alice".into(),
        operation: ToolIdentity {
            operation_id: format!("op-{id}"),
            source: "test".into(),
            tool: "Edit".into(),
            input_digest: "f".repeat(64),
        },
        paths: vec!["src/lib.rs".into()],
        base: Vec::new(),
        regions: Vec::new(),
        overwrites: Vec::new(),
        ttl_ms: 30_000,
    };
    // Yesterday: bob holds the file and alice is refused.
    log.run("hold-1", hold.clone(), at(MIDNIGHT - 2 * 60_000));
    let refused = log.run("try-1", edit("try-1"), at(MIDNIGHT - 2 * 60_000 + 1_000));
    assert!(matches!(refused, Outcome::Conflict { .. }), "{refused:?}");
    // Today: twice more, and a commit the hook held.
    log.run("hold-2", hold, at(MIDNIGHT + 60_000));
    for id in ["try-2", "try-3"] {
        let refused = log.run(id, edit(id), at(MIDNIGHT + 61_000));
        assert!(matches!(refused, Outcome::Conflict { .. }), "{refused:?}");
    }
    let held = log.run(
        "held-commit",
        Command::RecordRefusal {
            session_id: "claude-code.alice".into(),
            kind: RefusalKind::CommitHeld,
            paths: vec!["src/lib.rs".into()],
            peer_session_id: Some("codex.bob".into()),
        },
        at(MIDNIGHT + 62_000),
    );
    assert!(matches!(held, Outcome::RefusalRecorded { .. }), "{held:?}");
    let written = log
        .events
        .iter()
        .filter(|recorded| matches!(recorded.event, Event::WriteRefused { .. }))
        .count();
    assert_eq!(written, 4, "one write_refused event per refusal");
    let summary = log.today_from(MIDNIGHT, false, at(MIDNIGHT + 3 * 60_000));
    assert_eq!(summary.refusals.get("conflict"), Some(&2), "{summary:?}");
    assert_eq!(summary.refusals.get("commit_held"), Some(&1), "{summary:?}");
    assert_eq!(summary.refusals.len(), 2);
}

#[test]
fn a_held_admission_an_intent_and_an_unacknowledged_message_are_open() {
    let mut log = Log::ready();
    let now = at(MIDNIGHT + 60_000);
    log.run(
        "hold",
        Command::Admit {
            session_id: "codex.bob".into(),
            paths: vec!["src/lib.rs".into()],
            ttl_ms: 60_000,
        },
        now,
    );
    log.run(
        "intent",
        Command::IntentSet {
            session_id: "claude-code.alice".into(),
            paths: vec!["src/api.rs".into()],
            summary: "rework the API".into(),
        },
        now,
    );
    let sent = log.run(
        "message",
        Command::SendMessage {
            workspace_id: "work".into(),
            from: Some("claude-code.alice".into()),
            to: Some("codex.bob".into()),
            text: "I changed parse()".into(),
            paths: vec![],
            remote: false,
        },
        now,
    );
    assert!(matches!(sent, Outcome::MessageSent { .. }), "{sent:?}");
    let summary = log.today_from(MIDNIGHT, false, now + 1);
    let open = &summary.open;
    assert_eq!(open.admissions.len(), 1, "{open:?}");
    assert_eq!(open.admissions[0].session_id, "codex.bob");
    assert_eq!(open.admissions[0].paths, ["src/lib.rs"]);
    assert_eq!(open.intents.len(), 1);
    assert_eq!(open.intents[0].summary, "rework the API");
    assert_eq!(open.messages.len(), 1);
    assert_eq!(open.messages[0].waiting_for, ["codex.bob"]);
    assert!(open.handoffs.is_empty());
    // With no room at all, every list is emptied and what went is counted.
    let mut squeezed = summary.clone();
    squeezed.shrink_until(|_| false);
    assert!(squeezed.sessions.is_empty());
    assert_eq!(squeezed.omitted_sessions as usize, summary.sessions.len());
    assert!(squeezed.open.admissions.is_empty() && squeezed.open.messages.is_empty());
    assert_eq!(
        squeezed.open.omitted, 3,
        "admission, intent, message: {squeezed:?}"
    );
    // The admission expires; it is no longer open.
    let later = log.today_from(MIDNIGHT, false, now + 60_001);
    assert!(later.open.admissions.is_empty());
}

#[test]
fn a_restart_without_an_anchor_leaves_its_events_undated_and_a_reboot_is_placed() {
    let mut log = Log::ready();
    log.edited("claude-code.alice", "src/a.rs", at(MIDNIGHT + 60_000));
    // An older build restarted the daemon: no anchor for generation 2.
    log.boot(2, 0, None);
    log.run(
        "s2",
        Command::SessionRegister {
            session_id: "codex.carol".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        10,
    );
    log.edited("codex.carol", "src/b.rs", 20);
    // Then the machine rebooted: the daemon's clock starts again near zero.
    log.boot(3, 100, Some(MIDNIGHT + 2 * HOUR));
    log.run(
        "s3",
        Command::SessionRegister {
            session_id: "codex.dave".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
        150,
    );
    log.edited("codex.dave", "src/c.rs", 200);
    let summary = log.today_from(MIDNIGHT, false, 300);
    let ids: Vec<&str> = summary
        .sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();
    assert_eq!(ids, ["claude-code.alice", "codex.dave"], "{summary:?}");
    // A registration carries no time: it is placed at the generation's start.
    assert_eq!(summary.sessions[1].first_ms, MIDNIGHT + 2 * HOUR);
    assert_eq!(summary.sessions[1].last_ms, MIDNIGHT + 2 * HOUR + 100);
    assert_eq!(summary.undated, 2, "carol's registration and edit");
}

#[test]
fn a_window_that_reaches_before_the_events_read_says_truncated() {
    let mut log = Log::ready();
    log.edited("claude-code.alice", "src/a.rs", at(MIDNIGHT + 60_000));
    // Only the events from the edit on were read, with the generation's anchor.
    let first_today = log
        .events
        .iter()
        .position(|recorded| matches!(recorded.event, Event::OperationObserved { .. }))
        .unwrap();
    let anchor = ClockAnchor {
        generation: 1,
        clock_ms: START,
        wall_ms: MIDNIGHT - HOUR,
    };
    let read = |from_ms| {
        today(
            &log.state,
            &TodayQuery {
                workspace_id: Some("work"),
                window: TodayWindow {
                    from_ms,
                    to_ms: MIDNIGHT + HOUR,
                },
                events: &log.events[first_today..],
                anchor: Some(anchor),
                earlier: true,
                retained_from_ms: None,
                now_ms: at(MIDNIGHT + HOUR),
            },
        )
    };
    let summary = read(MIDNIGHT);
    assert!(summary.truncated, "older events may be in the window");
    assert_eq!(summary.sessions[0].files_edited, 1);
    assert!(!read(MIDNIGHT + 30 * 60_000).truncated);
}

#[test]
fn another_workspaces_work_is_left_out_unless_every_workspace_is_asked_for() {
    let mut log = Log::ready();
    log.run(
        "w2",
        Command::WorkspaceRegister {
            workspace_id: "other".into(),
            root: "/repos/other".into(),
        },
        at(MIDNIGHT),
    );
    log.run(
        "s-erin",
        Command::SessionRegister {
            session_id: "codex.erin".into(),
            workspace_id: "other".into(),
            parent: None,
            headless: false,
        },
        at(MIDNIGHT),
    );
    log.edited("codex.erin", "README.md", at(MIDNIGHT + 1));
    log.edited("claude-code.alice", "src/a.rs", at(MIDNIGHT + 2));
    let ours = log.today_from(MIDNIGHT, false, at(MIDNIGHT + 3));
    assert_eq!(ours.sessions.len(), 1);
    let all = today(
        &log.state,
        &TodayQuery {
            workspace_id: None,
            window: TodayWindow {
                from_ms: MIDNIGHT,
                to_ms: MIDNIGHT + HOUR,
            },
            events: &log.events,
            anchor: None,
            earlier: false,
            retained_from_ms: None,
            now_ms: at(MIDNIGHT + 3),
        },
    );
    assert_eq!(all.sessions.len(), 2);
}

#[test]
fn a_landing_today_on_yesterdays_check_evidence_is_counted_today() {
    let mut log = Log::ready();
    // Yesterday: the last edit; the passing check it lands on is reused.
    log.edited("claude-code.alice", "src/a.rs", at(MIDNIGHT - 20 * 60_000));
    // Today: a landing with nothing recorded before it but its own turn.
    let now = at(MIDNIGHT + 60_000);
    let taken = log.run(
        "land-1",
        Command::TakeTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            session_id: Some("claude-code.alice".into()),
            ttl_ms: 60_000,
            turn_id: None,
            queue: None,
        },
        now,
    );
    assert!(matches!(taken, Outcome::Turn { .. }), "{taken:?}");
    let released = log.run(
        "land-1-release",
        Command::ReleaseTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            turn_id: "land-1".into(),
            attempts: vec![LandingAttempt {
                target_oid: Some("a".repeat(40)),
                commit_oid: Some("b".repeat(40)),
                result: AttemptResult::Landed {},
            }],
        },
        now + 1_000,
    );
    assert!(
        matches!(released, Outcome::TurnReleased { released: true, .. }),
        "{released:?}"
    );
    let summary = log.today_from(MIDNIGHT, false, now + 2_000);
    assert_eq!(summary.landings.landed, 1, "{summary:?}");
    let yesterday = today(
        &log.state,
        &TodayQuery {
            workspace_id: Some("work"),
            window: TodayWindow {
                from_ms: MIDNIGHT - HOUR,
                to_ms: MIDNIGHT - 1,
            },
            events: &log.events,
            anchor: None,
            earlier: false,
            retained_from_ms: None,
            now_ms: now + 2_000,
        },
    );
    assert_eq!(yesterday.landings.landed, 0, "{yesterday:?}");
}

#[test]
fn a_queued_landing_today_after_yesterdays_prompt_edit_and_check_is_dated_today() {
    let mut log = Log::ready();
    // Yesterday: the last prompt, edit and passing check.
    log.prompted("claude-code.alice", at(MIDNIGHT - 30 * 60_000));
    log.edited("claude-code.alice", "src/a.rs", at(MIDNIGHT - 20 * 60_000));
    let recorded = log.run(
        "check-1",
        Command::RecordCheck {
            workspace_id: "work".into(),
            name: "test".into(),
            command: "cargo test".into(),
            tree_oid: "c".repeat(40),
            head_oid: None,
            inputs: Vec::new(),
            result: CheckResult::Passed {},
            duration_ms: 900,
            symbol_findings: None,
            report: None,
            tests: None,
            rerun: None,
            writes_before: None,
            command_id: None,
        },
        at(MIDNIGHT - 10 * 60_000),
    );
    assert!(
        matches!(recorded, Outcome::CheckRecorded { .. }),
        "{recorded:?}"
    );
    // Today: `langspan land` waits in line, holds the turn, renews it, finds
    // the passing check for the combined tree and pushes. Nothing else is
    // recorded: the landing reuses yesterday's evidence.
    let now = at(MIDNIGHT + 60_000);
    let take = |queue, turn_id| Command::TakeTurn {
        workspace_id: "work".into(),
        onto: "main".into(),
        session_id: Some("claude-code.alice".into()),
        ttl_ms: 60_000,
        turn_id,
        queue,
    };
    let taken = log.run(
        "land-1-wait",
        take(
            Some(TurnQueueing {
                ticket: "land-1".into(),
                wait_ms: 60_000,
                joined: false,
            }),
            None,
        ),
        now,
    );
    assert!(matches!(taken, Outcome::Turn { .. }), "{taken:?}");
    log.run("land-1-renew", take(None, Some("land-1".into())), now + 100);
    let released = log.run(
        "land-1-release",
        Command::ReleaseTurn {
            workspace_id: "work".into(),
            onto: "main".into(),
            turn_id: "land-1".into(),
            attempts: vec![LandingAttempt {
                target_oid: Some("a".repeat(40)),
                commit_oid: Some("b".repeat(40)),
                result: AttemptResult::Landed {},
            }],
        },
        now + 1_000,
    );
    assert!(
        matches!(released, Outcome::TurnReleased { released: true, .. }),
        "{released:?}"
    );
    let kinds: Vec<&str> = log.events[log.events.len() - 3..]
        .iter()
        .map(|recorded| match recorded.event {
            Event::TurnTaken { .. } => "taken",
            Event::TurnRenewed { .. } => "renewed",
            Event::TurnReleased { .. } => "released",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["taken", "renewed", "released"]);
    let summary = log.today_from(MIDNIGHT, false, now + 2_000);
    assert_eq!(summary.landings.landed, 1, "{summary:?}");
    assert_eq!(summary.checks.passed, 0, "the check was yesterday's");
    assert!(summary.sessions.is_empty(), "{summary:?}");
}

#[test]
fn a_read_that_starts_on_an_event_without_a_time_says_truncated() {
    let mut log = Log::ready();
    log.edited("claude-code.alice", "src/old.rs", at(MIDNIGHT + 60_000));
    // Today: an admission (lease_granted carries no time), then an edit.
    log.run(
        "hold",
        Command::Admit {
            session_id: "codex.bob".into(),
            paths: vec!["src/lib.rs".into()],
            ttl_ms: 60_000,
        },
        at(MIDNIGHT + 2 * 60_000),
    );
    log.edited("codex.bob", "src/lib.rs", at(MIDNIGHT + 3 * 60_000));
    // The cut fell on the grant: today's first edit was left out.
    let first = log
        .events
        .iter()
        .position(|recorded| matches!(recorded.event, Event::LeaseGranted { .. }))
        .unwrap();
    // The generation started yesterday, an hour before midnight.
    let anchor = ClockAnchor {
        generation: 1,
        clock_ms: START,
        wall_ms: MIDNIGHT - HOUR,
    };
    let read = |from_ms| {
        today(
            &log.state,
            &TodayQuery {
                workspace_id: Some("work"),
                window: TodayWindow {
                    from_ms,
                    to_ms: MIDNIGHT + HOUR,
                },
                events: &log.events[first..],
                anchor: Some(anchor),
                earlier: true,
                retained_from_ms: None,
                now_ms: at(MIDNIGHT + HOUR),
            },
        )
    };
    assert!(read(MIDNIGHT).truncated, "{:?}", read(MIDNIGHT));
    // The first event with a time of its own bounds what was left out: a
    // window that starts after it misses nothing.
    assert!(!read(MIDNIGHT + 30 * 60_000).truncated);
}

#[test]
fn sessions_beyond_the_cap_are_counted_and_shrinking_keeps_the_counts() {
    let mut log = Log::ready();
    for index in 0..(TODAY_SESSIONS + 3) {
        let session = format!("codex.s{index:02}");
        log.run(
            &format!("r{index}"),
            Command::SessionRegister {
                session_id: session.clone(),
                workspace_id: "work".into(),
                parent: None,
                headless: false,
            },
            at(MIDNIGHT + index as u64),
        );
        for file in 0..10 {
            log.edited(&session, &format!("src/f{file}.rs"), at(MIDNIGHT + 100));
        }
    }
    let mut summary = log.today_from(MIDNIGHT, false, at(MIDNIGHT + 200));
    assert_eq!(summary.sessions.len(), TODAY_SESSIONS);
    assert_eq!(summary.omitted_sessions, 3);
    assert_eq!(summary.sessions[0].files.len(), 8);
    assert_eq!(summary.sessions[0].files_edited, 10);
    summary.shrink_until(|today| serde_json::to_vec(today).unwrap().len() <= 2_000);
    assert!(serde_json::to_vec(&summary).unwrap().len() <= 2_000);
    assert_eq!(
        summary.sessions.len() as u32 + summary.omitted_sessions,
        TODAY_SESSIONS as u32 + 3
    );
    assert!(
        summary
            .sessions
            .iter()
            .all(|session| session.files_edited == 10)
    );
}
