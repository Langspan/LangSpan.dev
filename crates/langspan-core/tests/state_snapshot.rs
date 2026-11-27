//! `SnapshotRecord`, the stored form of a `State` (ADR 0025): the state and
//! every field it leaves out of its replies. In-process, against the replay
//! fixture; the store's own round trip is in the root crate's
//! `tests/retention.rs`.

use langspan_core::{Event, SnapshotRecord, State, apply};
use serde_json::{Value, json};
use std::path::Path;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(name);
    std::fs::read_to_string(path).expect("read the replay fixture")
}

fn replayed() -> State {
    let mut state = State::default();
    for line in fixture("events.jsonl").lines() {
        let event: Event = serde_json::from_str(line).expect("an event parses");
        apply(&mut state, &event).expect("the fixture applies");
    }
    state
}

#[test]
fn a_stored_snapshot_restores_every_derived_field_of_the_replay() {
    let record: SnapshotRecord =
        serde_json::from_str(&fixture("state.json")).expect("the fixture is a snapshot record");
    assert_eq!(record.into_state(), replayed());
}

#[test]
fn a_snapshot_of_a_state_writes_back_what_it_read() {
    let mut stored: Value = serde_json::from_str(&fixture("state.json")).expect("JSON");
    // The replay settles every refusal; one left open is carried too.
    stored["unsettled"] =
        json!({"alice": [{"kind": "conflict", "paths": ["src/a.rs"], "at_ms": 5}]});
    let record: SnapshotRecord = serde_json::from_value(stored.clone()).expect("a record");
    let state = record.into_state();
    assert_eq!(state.unsettled()["alice"].len(), 1);
    let written = serde_json::to_value(SnapshotRecord::of(&state)).expect("a record serializes");
    assert_eq!(written, stored);
}

#[test]
fn a_snapshot_without_derived_fields_reads_them_as_empty() {
    let state = replayed();
    let bare = json!({ "state": state });
    let record: SnapshotRecord = serde_json::from_value(bare).expect("a record");
    let restored = record.into_state();
    assert!(restored.clock_anchor().is_none() && restored.trails().is_empty());
    assert_eq!(SnapshotRecord::of(&restored).into_state(), restored);
}
