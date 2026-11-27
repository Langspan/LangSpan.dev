//! Properties of `decide` and `apply` over generated command sequences
//! (issue #336). Each case registers two workspaces and four sessions, then
//! runs a generated sequence of admissions (manual and by a tool), completions,
//! intents, re-registrations, daemon restarts, clock advances and retries,
//! deciding each as the store does: the expiry events due at a command's time
//! first, then the command's own events. Four invariants are checked:
//!
//! 1. `apply` accepts every event `decide` and `expiry_events` return.
//! 2. Folding the event log, read back from JSON as the store reads it, gives
//!    the same `State` as applying each event as it was decided.
//! 3. A request whose first answer granted or completed a lease, decided
//!    again under its ID, is not a second admission and does not change the
//!    leases. Replaying the recorded answer is the store's job (its request
//!    receipts); this is the core's half: a retry that reaches `decide`
//!    cannot grant twice. Deciding is also a function of its inputs: the
//!    same state, command, ID and time give the same decision.
//! 4. A file in a workspace has live leases of at most one session, and a
//!    manual admission is the only live lease on its files. (A host's tool
//!    call is not held back by an earlier tool call of its own session, so
//!    one session can hold two tool leases on a file; ADR 0001.)
//!
//! A failing case is saved under `proptest-regressions/properties.txt` and
//! replayed first on every later run; commit that file with the fix.

use langspan_core::{
    Command, Decision, Event, LeaseStatus, Outcome, State, ToolIdentity, apply, decide,
    expiry_events,
};
use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, TestCaseError};
use std::collections::BTreeMap;

const WORKSPACES: [(&str, &str); 2] = [("w1", "/repos/w1"), ("w2", "/repos/w2")];
/// Each session and its workspace: three share `w1`, so they contend.
const SESSIONS: [(&str, &str); 4] = [("s0", "w1"), ("s1", "w1"), ("s2", "w1"), ("s3", "w2")];
const PATHS: [&str; 3] = ["src/a.rs", "src/b.rs", "src/c.rs"];

#[derive(Debug, Clone)]
enum Step {
    /// A session registers again, as a host does after a restart.
    Register {
        session: usize,
    },
    Admit {
        session: usize,
        /// A nonempty subset of `PATHS`, as a bit set.
        paths: u8,
        ttl_ms: u64,
        tool: bool,
    },
    /// Complete the lease granted `lease`-th (modulo how many were granted),
    /// as `session`, which may not hold it.
    Complete {
        session: usize,
        lease: usize,
    },
    Intent {
        session: usize,
        paths: u8,
    },
    Advance {
        ms: u64,
    },
    /// The daemon restarts: the store records the next generation.
    Boot,
    /// The previous command again, under its request ID, at the same time.
    Retry,
}

fn step() -> impl Strategy<Value = Step> {
    let session = 0..SESSIONS.len();
    let paths = 1_u8..(1 << PATHS.len());
    prop_oneof![
        2 => session.clone().prop_map(|session| Step::Register { session }),
        6 => (session.clone(), paths.clone(), 1_u64..=400, any::<bool>()).prop_map(
            |(session, paths, ttl_ms, tool)| Step::Admit { session, paths, ttl_ms, tool }
        ),
        3 => (session.clone(), 0_usize..8)
            .prop_map(|(session, lease)| Step::Complete { session, lease }),
        1 => (session, paths).prop_map(|(session, paths)| Step::Intent { session, paths }),
        3 => (1_u64..=300).prop_map(|ms| Step::Advance { ms }),
        1 => Just(Step::Boot),
        2 => Just(Step::Retry),
    ]
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(step(), 1..40)
}

fn chosen(paths: u8) -> Vec<String> {
    PATHS
        .iter()
        .enumerate()
        .filter(|(index, _)| paths & (1 << index) != 0)
        .map(|(_, path)| (*path).to_owned())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    ApplyAccepts,
    ReplaySame,
    RetryKeepsFirst,
    OneHolder,
}

/// The store's loop: decide, then apply what was decided, keeping the log.
struct Run {
    check: Check,
    state: State,
    log: Vec<Event>,
    now_ms: u64,
    next_id: u64,
    granted: Vec<String>,
    /// The last command decided, its ID, the state and time it was decided
    /// against and its decision, for `Step::Retry`.
    last: Option<(Command, String, State, u64, Decision)>,
}

impl Run {
    fn new(check: Check) -> Result<Self, TestCaseError> {
        let mut run = Run {
            check,
            state: State::default(),
            log: Vec::new(),
            now_ms: 0,
            next_id: 0,
            granted: Vec::new(),
            last: None,
        };
        run.record(Event::Booted { generation: 1 })?;
        for (workspace_id, root) in WORKSPACES {
            run.decide(Command::WorkspaceRegister {
                workspace_id: workspace_id.into(),
                root: root.into(),
            })?;
        }
        for session in 0..SESSIONS.len() {
            run.decide(register(session))?;
        }
        Ok(run)
    }

    fn record(&mut self, event: Event) -> Result<(), TestCaseError> {
        if let Err(error) = apply(&mut self.state, &event) {
            // Property 1. Every other check needs the events applied too.
            return Err(TestCaseError::fail(format!(
                "apply refused a decided event: {}: {}: {event:?}",
                error.code, error.message
            )));
        }
        self.log.push(event);
        Ok(())
    }

    fn decide(&mut self, command: Command) -> Result<(), TestCaseError> {
        self.next_id += 1;
        let id = format!("r{}", self.next_id);
        for event in expiry_events(&self.state, self.now_ms) {
            self.record(event)?;
        }
        let before = self.state.clone();
        let decision = decide(&self.state, &command, &id, self.now_ms);
        if let Outcome::Admitted { lease } = &decision.outcome {
            self.granted.push(lease.lease_id.clone());
        }
        for event in decision.events.clone() {
            self.record(event)?;
        }
        self.last = Some((command, id, before, self.now_ms, decision));
        Ok(())
    }

    fn retry(&mut self) -> Result<(), TestCaseError> {
        let Some((command, id, before, at_ms, first)) = self.last.clone() else {
            return Ok(());
        };
        if self.check == Check::RetryKeepsFirst {
            let again = decide(&before, &command, &id, at_ms);
            prop_assert_eq!(&again, &first, "the same inputs decided differently");
        }
        for event in expiry_events(&self.state, self.now_ms) {
            self.record(event)?;
        }
        let leases = self.state.leases.clone();
        let retry = decide(&self.state, &command, &id, self.now_ms);
        for event in retry.events.clone() {
            self.record(event)?;
        }
        // A refusal leaves nothing to keep: decided again once the holder is
        // gone, the request is admitted. The store answers that retry from
        // its receipt and never decides it again.
        let kept = matches!(
            first.outcome,
            Outcome::Admitted { .. } | Outcome::Completed { .. }
        );
        if self.check == Check::RetryKeepsFirst && kept {
            prop_assert!(
                !matches!(retry.outcome, Outcome::Admitted { .. }),
                "request {id} was admitted twice: first {:?}, retry {:?}",
                first.outcome,
                retry.outcome
            );
            prop_assert_eq!(
                &self.state.leases,
                &leases,
                "a retry of {} changed the leases",
                id
            );
        }
        Ok(())
    }

    fn step(&mut self, step: &Step) -> Result<(), TestCaseError> {
        match *step {
            Step::Register { session } => self.decide(register(session)),
            Step::Admit {
                session,
                paths,
                ttl_ms,
                tool,
            } => {
                let session_id = SESSIONS[session].0.to_owned();
                let paths = chosen(paths);
                self.decide(if tool {
                    Command::AdmitTool {
                        session_id,
                        paths,
                        ttl_ms,
                        operation: ToolIdentity {
                            operation_id: format!("op{}", self.next_id + 1),
                            source: "test".into(),
                            tool: "Edit".into(),
                            input_digest: "a".repeat(64),
                        },
                        base: Vec::new(),
                        regions: Vec::new(),
                        overwrites: Vec::new(),
                    }
                } else {
                    Command::Admit {
                        session_id,
                        paths,
                        ttl_ms,
                    }
                })
            }
            Step::Complete { session, lease } => {
                let lease_id = if self.granted.is_empty() {
                    "none".to_owned()
                } else {
                    self.granted[lease % self.granted.len()].clone()
                };
                self.decide(Command::Complete {
                    session_id: SESSIONS[session].0.into(),
                    lease_id,
                })
            }
            Step::Intent { session, paths } => self.decide(Command::IntentSet {
                session_id: SESSIONS[session].0.into(),
                paths: chosen(paths),
                summary: "change the parser".into(),
            }),
            Step::Advance { ms } => {
                self.now_ms += ms;
                Ok(())
            }
            Step::Boot => {
                let generation = self.state.generation + 1;
                self.last = None;
                self.record(Event::Booted { generation })
            }
            Step::Retry => self.retry(),
        }
    }

    /// Property 4, at the current time.
    fn one_holder(&self) -> Result<(), TestCaseError> {
        let mut holders: BTreeMap<(&str, &str), Vec<(&str, bool)>> = BTreeMap::new();
        for lease in self.state.leases.values().filter(|lease| {
            lease.status == LeaseStatus::Active
                && lease.generation == self.state.generation
                && lease.expires_at_ms > self.now_ms
        }) {
            for path in &lease.paths {
                holders
                    .entry((&lease.workspace_id, path))
                    .or_default()
                    .push((&lease.session_id, lease.operation.is_some()));
            }
        }
        for ((workspace, path), leases) in holders {
            let (first, _) = leases[0];
            prop_assert!(
                leases.iter().all(|(session, _)| *session == first),
                "{workspace}:{path} has live leases of more than one session: {leases:?}"
            );
            prop_assert!(
                leases.len() == 1 || leases.iter().all(|(_, tool)| *tool),
                "{workspace}:{path} has a manual lease beside another live lease: {leases:?}"
            );
        }
        Ok(())
    }

    /// Property 2: the log, through JSON, folds to the state.
    fn replay_same(&self) -> Result<(), TestCaseError> {
        let mut replayed = State::default();
        for (index, event) in self.log.iter().enumerate() {
            let text = serde_json::to_string(event).expect("an event serializes");
            let read: Event = serde_json::from_str(&text)
                .map_err(|e| TestCaseError::fail(format!("event {index} reads back: {e}")))?;
            apply(&mut replayed, &read).map_err(|e| {
                TestCaseError::fail(format!("event {index} replays: {}: {read:?}", e.message))
            })?;
        }
        prop_assert_eq!(&replayed, &self.state);
        Ok(())
    }
}

fn register(session: usize) -> Command {
    let (session_id, workspace_id) = SESSIONS[session];
    Command::SessionRegister {
        session_id: session_id.into(),
        workspace_id: workspace_id.into(),
        parent: None,
        headless: false,
    }
}

fn run(check: Check, steps: &[Step]) -> Result<(), TestCaseError> {
    let mut run = Run::new(check)?;
    for step in steps {
        run.step(step)?;
        if check == Check::OneHolder {
            run.one_holder()?;
        }
    }
    if check == Check::ReplaySame {
        run.replay_same()?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/properties.txt"
        )))),
        ..ProptestConfig::default()
    })]

    #[test]
    fn apply_accepts_every_decided_event(steps in steps()) {
        run(Check::ApplyAccepts, &steps)?;
    }

    #[test]
    fn replaying_the_log_gives_the_same_state(steps in steps()) {
        run(Check::ReplaySame, &steps)?;
    }

    #[test]
    fn a_repeated_request_id_keeps_the_first_answer(steps in steps()) {
        run(Check::RetryKeepsFirst, &steps)?;
    }

    #[test]
    fn a_file_has_at_most_one_live_holder(steps in steps()) {
        run(Check::OneHolder, &steps)?;
    }
}
