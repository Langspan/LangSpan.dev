//! The replay guard for `apply` (issue #317): a synthetic event log that
//! holds every `Event` variant, the `State` it folds to, and a match that
//! names the log entry covering each variant.
//!
//! `fixtures/replay/commands.jsonl` is a script of commands, decided one at
//! a time as the daemon's store decides them (the expiry events due at a
//! command's time first). The events they write are the log,
//! `fixtures/replay/events.jsonl`; `fixtures/replay/state.json` is the state
//! the log folds to. The script's `{"event": ...}` lines are events no
//! command writes: the store records `booted` and `clock_anchored` as it
//! opens, and `inflight_delivered` and `note_anchor_changed` need the source
//! index, which only the daemon prepares (`BriefPrepared`,
//! `NoteAnchorsPrepared`). Every other event comes from `decide`.
//!
//! `covering_line` matches every `Event` variant with no wildcard, so a new
//! variant does not compile until it names the log line that applies it.
//! Every type in a release's frozen list (`fixtures/event-types/` at the
//! workspace root) must be in the log too, so a type a released store
//! holds keeps applying.
//!
//! `fixtures/replay/refused.jsonl` changes one field of a log line so that
//! `apply` must refuse it, which covers the refusals the log never takes;
//! `fixtures/replay/refused.txt` is the error each one gets.
//!
//! A change to what a command writes or to what an event does to the state
//! fails here with the lines that differ, as does a changed refusal. To
//! accept one, run
//! `LANGSPAN_UPDATE_GOLDEN=1 cargo nextest run -p langspan-core --test replay`,
//! review `git diff`, and explain in the pull request why the log or the
//! state changed.

#[path = "support/snapshot.rs"]
mod snapshot;

use langspan_core::{Command, Event, Outcome, State, apply, decide, expiry_events};
use serde::Deserialize;
use serde_json::Value;
use std::mem::discriminant;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(name)
}

/// One line of the command script.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    /// The request ID, which names what the command creates.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    at_ms: u64,
    #[serde(default)]
    command: Option<Command>,
    /// An event recorded without a command.
    #[serde(default)]
    event: Option<Event>,
}

/// The events the command script writes, in order.
fn run_script() -> Vec<Event> {
    let script = std::fs::read_to_string(fixture("commands.jsonl")).expect("read the script");
    let mut state = State::default();
    let mut log = Vec::new();
    let mut record = |state: &mut State, event: Event, line: usize| {
        apply(state, &event)
            .unwrap_or_else(|e| panic!("commands.jsonl line {line}: {e}: {event:?}"));
        log.push(event);
    };
    for (index, text) in script.lines().enumerate() {
        let line = index + 1;
        if text.trim().is_empty() || text.starts_with('#') {
            continue;
        }
        let step: Step = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("commands.jsonl line {line} does not parse: {e}"));
        match (step.command, step.event) {
            (None, Some(event)) => record(&mut state, event, line),
            (Some(command), None) => {
                for event in expiry_events(&state, step.at_ms) {
                    record(&mut state, event, line);
                }
                let id = step.id.unwrap_or_else(|| format!("line-{line}"));
                let decision = decide(&state, &command, &id, step.at_ms);
                if let Outcome::Error { code, message } = &decision.outcome {
                    panic!("commands.jsonl line {line} was refused: {code}: {message}");
                }
                assert!(
                    !decision.events.is_empty(),
                    "commands.jsonl line {line} wrote no event: {:?}",
                    decision.outcome
                );
                for event in decision.events {
                    record(&mut state, event, line);
                }
            }
            _ => panic!("commands.jsonl line {line} needs one of `command` or `event`"),
        }
    }
    log
}

fn read_log() -> Vec<Event> {
    let text = std::fs::read_to_string(fixture("events.jsonl")).expect("read the event log");
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("events.jsonl line {} does not parse: {e}", index + 1))
        })
        .collect()
}

fn log_text(log: &[Event]) -> String {
    log.iter()
        .map(|event| serde_json::to_string(event).expect("an event serializes") + "\n")
        .collect()
}

#[test]
fn the_command_script_writes_the_committed_event_log() {
    snapshot::check(&fixture("events.jsonl"), &log_text(&run_script()));
}

#[test]
fn the_event_log_folds_to_the_committed_state() {
    let mut state = State::default();
    for (index, event) in read_log().iter().enumerate() {
        apply(&mut state, event)
            .unwrap_or_else(|e| panic!("events.jsonl line {} does not apply: {e}", index + 1));
    }
    snapshot::check(&fixture("state.json"), &snapshot::state_json(&state));
}

/// Every event type's name, as the store writes it: serde lists them all
/// when it is given one it does not know.
fn every_type() -> Vec<String> {
    let error = serde_json::from_str::<Event>(r#"{"type":"-"}"#)
        .expect_err("`-` names no event type")
        .to_string();
    let (_, names) = error
        .split_once("expected one of ")
        .unwrap_or_else(|| panic!("serde's message changed: {error}"));
    // `booted`, `clock_anchored`, ... `host_cleared` at line 1 column 11
    let names: Vec<String> = names
        .split(", ")
        .filter_map(|name| name.split('`').nth(1))
        .map(str::to_string)
        .collect();
    assert!(names.len() > 1, "serde's message changed: {error}");
    names
}

fn type_of(event: &Event) -> String {
    serde_json::to_value(event).expect("an event serializes")["type"]
        .as_str()
        .expect("an event has a type")
        .to_string()
}

#[test]
fn every_event_type_is_in_the_log_at_the_line_the_match_names() {
    let log = read_log();
    for event in &log {
        let line = covering_line(event);
        let covering = log
            .get(line - 1)
            .unwrap_or_else(|| panic!("covering_line names line {line}, past the log's end"));
        assert_eq!(
            discriminant(covering),
            discriminant(event),
            "covering_line says line {line} holds a `{}`; it holds a `{}`",
            type_of(event),
            type_of(covering)
        );
    }
    let missing: Vec<String> = every_type()
        .into_iter()
        .chain(frozen_types())
        .filter(|name| !log.iter().any(|event| type_of(event) == *name))
        .collect();
    assert!(
        missing.is_empty(),
        "the log applies no {missing:?}: add a command to commands.jsonl that writes it"
    );
}

/// Every event type a release reads, from each frozen list.
fn frozen_types() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/event-types");
    let lists = std::fs::read_dir(&dir).expect("read fixtures/event-types");
    let mut names = Vec::new();
    for list in lists {
        let path = list.expect("list fixtures/event-types").path();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        names.extend(text.lines().map(str::to_string));
    }
    assert!(names.len() > 1, "fixtures/event-types holds no list");
    names
}

/// One line of `refused.jsonl`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    /// The line of `events.jsonl` to change.
    line: usize,
    /// The fields to change, by JSON pointer, and their new values.
    set: serde_json::Map<String, Value>,
}

/// `event` with the field at `pointer` set to `value`, added if absent.
fn set_field(event: &mut Value, pointer: &str, value: Value) {
    let (parent, key) = pointer
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("`{pointer}` is not a JSON pointer"));
    event
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("`{parent}` names no object"))
        .insert(key.to_string(), value);
}

#[test]
fn each_changed_event_is_refused_with_the_committed_error() {
    let log = read_log();
    let script = std::fs::read_to_string(fixture("refused.jsonl")).expect("read the refusals");
    let mut errors = String::new();
    for (index, text) in script.lines().enumerate() {
        let n = index + 1;
        if text.trim().is_empty() || text.starts_with('#') {
            continue;
        }
        let case: Refusal = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("refused.jsonl line {n} does not parse: {e}"));
        let original = case
            .line
            .checked_sub(1)
            .and_then(|at| log.get(at))
            .unwrap_or_else(|| panic!("refused.jsonl line {n} names no line of events.jsonl"));
        let mut changed = serde_json::to_value(original).expect("an event serializes");
        for (pointer, value) in case.set {
            set_field(&mut changed, &pointer, value);
        }
        let changed: Event = serde_json::from_value(changed)
            .unwrap_or_else(|e| panic!("refused.jsonl line {n} is no event: {e}"));
        let mut state = State::default();
        for event in &log[..case.line - 1] {
            apply(&mut state, event).expect("the log applies");
        }
        match apply(&mut state, &changed) {
            Ok(()) => panic!("refused.jsonl line {n} was applied: {changed:?}"),
            Err(e) => errors += &format!("{n}: events.jsonl line {}: {e}\n", case.line),
        }
    }
    snapshot::check(&fixture("refused.txt"), &errors);
}

/// The line of `events.jsonl` that applies an event of this type. A new
/// variant does not compile until it has an arm, and so a line in the log.
fn covering_line(event: &Event) -> usize {
    match event {
        Event::Booted { .. } => 1,
        Event::ClockAnchored { .. } => 2,
        Event::WriteRefused { .. } => 27,
        Event::WriteWaited { .. } => 26,
        Event::RefusalSettled { .. } => 35,
        Event::PolicySet { .. } => 14,
        Event::WorkspaceRegistered { .. } => 3,
        Event::WorkspaceMoved { .. } => 102,
        Event::WorkspaceRemoved { .. } => 103,
        Event::SessionRegistered { .. } => 8,
        Event::SessionLivenessObserved { .. } => 12,
        Event::SessionSeen { .. } => 13,
        Event::IntentSet { .. } => 17,
        Event::IntentEnded { .. } => 94,
        Event::LeaseGranted { .. } => 25,
        Event::LeaseCompleted { .. } => 30,
        Event::LeaseExpired { .. } => 37,
        Event::OperationObserved { .. } => 28,
        Event::DiskObserved { .. } => 32,
        Event::BranchObserved { .. } => 33,
        Event::WorkspaceGitObserved { .. } => 6,
        Event::SessionPrompted { .. } => 18,
        Event::PromptTargetsSet { .. } => 19,
        Event::PresenceObserved { .. } => 23,
        Event::PresenceSet { .. } => 24,
        Event::SessionEnded { .. } => 95,
        Event::SessionAdopted { .. } => 97,
        Event::TrailRecorded { .. } => 96,
        Event::TrailTaken { .. } => 100,
        Event::SessionStopped { .. } => 34,
        Event::ToolBatchObserved { .. } => 31,
        Event::BriefingDelivered { .. } => 46,
        Event::ContextAdded { .. } => 48,
        Event::ToldForgotten { .. } => 93,
        Event::InflightDelivered { .. } => 47,
        Event::InterfaceAlerted { .. } => 43,
        Event::InterfaceNoticesTold { .. } => 44,
        Event::ReadOutlined { .. } => 21,
        Event::SourceObserved { .. } => 20,
        Event::RemoteEnabled { .. } => 59,
        Event::RemoteDisabled { .. } => 63,
        Event::RemotePublished { .. } => 60,
        Event::RemoteObserved { .. } => 61,
        Event::RemoteForgotten { .. } => 62,
        Event::RemoteWithdrawn { .. } => 64,
        Event::CheckRecorded { .. } => 65,
        Event::AutoCheckEnabled { .. } => 67,
        Event::AutoCheckDisabled { .. } => 75,
        Event::CheckReported { .. } => 71,
        Event::CheckDelivered { .. } => 74,
        Event::TestsRecorded { .. } => 66,
        Event::FlakyTestForgotten { .. } => 72,
        Event::Landed { .. } => 80,
        Event::TurnTaken { .. } => 76,
        Event::TurnReleased { .. } => 81,
        Event::TurnRenewed { .. } => 79,
        Event::TurnExpired { .. } => 84,
        Event::TurnQueueJoined { .. } => 77,
        Event::TurnQueueSeen { .. } => 78,
        Event::TurnQueueLeft { .. } => 82,
        Event::InterfaceChanged { .. } => 40,
        Event::InterfaceDependentsFound { .. } => 42,
        Event::Remembered { .. } => 55,
        Event::Forgotten { .. } => 58,
        Event::NoteAnchorChanged { .. } => 56,
        Event::MessageSent { .. } => 49,
        Event::MessageAcknowledged { .. } => 50,
        Event::MessageRetracted { .. } => 53,
        Event::RetractionDelivered { .. } => 54,
        Event::ScanFailed { .. } => 85,
        Event::ScanRecovered { .. } => 86,
        Event::ScanCoverageObserved { .. } => 87,
        Event::CheckoutChanged { .. } => 88,
        Event::CheckoutRunObserved { .. } => 89,
        Event::UncoordinatedChanged { .. } => 90,
        Event::UncoordinatedDelivered { .. } => 92,
        Event::HostNotInstalled { .. } => 15,
        Event::HostCleared { .. } => 16,
    }
}
