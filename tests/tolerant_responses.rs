//! Issue #332: the current client ignores a field it does not know at every
//! level of a response, except in the objects the store or a request also
//! carries, which stay strict (ADR 0015). Pure parsing of the committed
//! vectors: no daemon, socket or repository.

use langspan::protocol::{Request, Response};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Places in the response vectors that still reject an unknown field, as
/// `fixture pointer` with each array index written `N`. Each is an object an
/// event or the derived state also carries (a lease, a session, a check
/// record, an interface change, a message's recipient, a recorded event and
/// the like), which the store replays; it stays strict until those readers
/// are tolerant.
const STRICT: &[&str] = &[
    "admitted-outcome-v10.json /outcome/lease",
    "adopted-outcome-v10.json /outcome/adoption",
    "adopted-response-v3.json /outcome/adoption",
    "adopted-response-v3.json /outcome/adoption/reason",
    "auto-check-outcome-v10.json /outcome/check",
    "between-steps-response-v4.json /outcome/interface_changes/N/changes/N",
    "branch-observed-outcome-v10.json /outcome/observation",
    "call-breaks-outcome-v10.json /outcome/breaks/N/after_arity",
    "call-breaks-outcome-v10.json /outcome/breaks/N/before_arity",
    "call-breaks-outcome-v10.json /outcome/breaks/N/call",
    "changes-outcome-v10.json /outcome/events/N",
    "changes-outcome-v10.json /outcome/events/N/event",
    "check-recorded-outcome-v10.json /outcome/check",
    "check-recorded-outcome-v10.json /outcome/check/result",
    "checkout-observed-outcome-v10.json /outcome/change",
    "checkout-run-observed-outcome-v10.json /outcome/run",
    "checks-outcome-v10.json /outcome/checks/N/record",
    "checks-outcome-v10.json /outcome/checks/N/record/result",
    "conflict-outcome-v10.json /outcome/holders/N",
    "context-recorded-outcome-v10.json /outcome/context",
    "dashboard-outcome-v10.json /outcome/dashboard/checks/N/result",
    "dashboard-outcome-v10.json /outcome/dashboard/messages/N/to",
    "disk-observed-outcome-v10.json /outcome/observation",
    "disk-observed-outcome-v10.json /outcome/observation/after",
    "disk-observed-outcome-v10.json /outcome/observation/before",
    "events-outcome-v10.json /outcome/events/N",
    "events-outcome-v10.json /outcome/events/N/event",
    "events-response-v10.json /outcome/events/N",
    "events-response-v10.json /outcome/events/N/event",
    "events-response-v8.json /outcome/events/N",
    "events-response-v8.json /outcome/events/N/event",
    "events-response-v9.json /outcome/events/N",
    "events-response-v9.json /outcome/events/N/event",
    "holders-outcome-v10.json /outcome/holders/N",
    "intent-outcome-v10.json /outcome/intent",
    "interface-observed-outcome-v10.json /outcome/notice",
    "interface-observed-outcome-v10.json /outcome/notice/changes/N",
    "landed-outcome-v10.json /outcome/landing",
    "landed-response-v5.json /outcome/landing",
    "landed-response-v6.json /outcome/landing",
    "landed-response-v6.json /outcome/landing/rebased/N",
    "message-briefing-response-v3.json /outcome/briefing/messages/N",
    "message-briefing-response-v3.json /outcome/briefing/messages/N/to",
    "message-sent-outcome-v10.json /outcome/message",
    "message-sent-outcome-v10.json /outcome/message/to",
    "message-shown-outcome-v10.json /outcome/message/to",
    "message-status-outcome-v10.json /outcome/status/to",
    "message-status-response-v3.json /outcome/status/to",
    "observed-outcome-v10.json /outcome/observation",
    "observed-outcome-v10.json /outcome/observation/concurrent_holders/N",
    "presence-outcome-v10.json /outcome/setting",
    "readiness-outcome-v10.json /outcome/check",
    "readiness-outcome-v10.json /outcome/check/result",
    "recall-response-v9.json /outcome/recall/notes/N/anchor",
    "recap-outcome-v10.json /outcome/recap/checks/N",
    "recap-outcome-v10.json /outcome/recap/checks/N/result",
    "recap-outcome-v10.json /outcome/recap/interface_changes/N",
    "recap-outcome-v10.json /outcome/recap/interface_changes/N/changes/N",
    "recap-outcome-v10.json /outcome/recap/landings/N",
    "recap-outcome-v10.json /outcome/recap/notes/N",
    "recap-outcome-v10.json /outcome/recap/publishing",
    "recap-outcome-v10.json /outcome/recap/scan_failure",
    "recap-outcome-v10.json /outcome/recap/teammates/N",
    "recap-outcome-v10.json /outcome/recap/teammates/N/publication",
    "recap-outcome-v10.json /outcome/recap/teammates/N/publication/work/N",
    "recent-briefing-response-v3.json /outcome/briefing/recent/interface_changes/N/changes/N",
    "remembered-outcome-v10.json /outcome/note",
    "remote-link-outcome-v10.json /outcome/link",
    "remote-outbox-outcome-v10.json /outcome/publication",
    "remote-outbox-outcome-v10.json /outcome/publication/work/N",
    "retracted-briefing-response-v3.json /outcome/briefing/retracted/N",
    "session-liveness-outcome-v10.json /outcome/liveness",
    "session-liveness-outcome-v10.json /outcome/liveness/process",
    "session-outcome-v10.json /outcome/session",
    "show-message-response-v3.json /outcome/message/to",
    "stale-base-outcome-v10.json /outcome/stale/N/current",
    "stale-base-outcome-v10.json /outcome/stale/N/read",
    "stale-base-response-v3.json /outcome/stale/N/current",
    "stale-base-response-v3.json /outcome/stale/N/read",
    "status-outcome-v10.json /outcome/state",
    "status-outcome-v10.json /outcome/state/intents/intent-1",
    "status-outcome-v10.json /outcome/state/leases/lease-1",
    "status-outcome-v10.json /outcome/state/sessions/claude-code.alice",
    "status-outcome-v10.json /outcome/state/workspaces/app",
    "status-records-outcome-v10.json /outcome/records/N/workspace",
    "trails-outcome-v10.json /outcome/trails/N/trail",
    "trails-response-v4.json /outcome/trails/N/trail",
    "trails-response-v4.json /outcome/trails/N/trail/checks/N",
    "trails-response-v4.json /outcome/trails/N/trail/checks/N/result",
    "trails-response-v4.json /outcome/trails/N/trail/edited/N",
    "trails-response-v4.json /outcome/trails/N/trail/edited/N/lines/N",
    "turn-busy-outcome-v10.json /outcome/holder",
    "turn-outcome-v10.json /outcome/turn",
    "turn-queue-outcome-v10.json /outcome/holder",
    "turn-unavailable-outcome-v10.json /outcome/holder",
    "turn-waiting-outcome-v10.json /outcome/holder",
    "unread-lines-outcome-v10.json /outcome/lines/N",
    "unread-lines-outcome-v10.json /outcome/lines/N/lines/N",
    "unread-lines-response-v3.json /outcome/lines/N",
    "unread-lines-response-v3.json /outcome/lines/N/lines/N",
    "workspace-git-outcome-v10.json /outcome/git",
    "workspace-moved-outcome-v10.json /outcome/workspace",
    "workspace-outcome-v10.json /outcome/workspace",
    "write-check-outcome-v10.json /outcome/check/paths/N/unread_lines/N/lines/N",
];

/// Every object in `value`, as JSON pointers.
fn objects(value: &Value, at: String, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            out.push(at.clone());
            for (key, child) in map {
                let key = key.replace('~', "~0").replace('/', "~1");
                objects(child, format!("{at}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                objects(child, format!("{at}/{index}"), out);
            }
        }
        _ => {}
    }
}

/// For each vector whose name contains one of `kinds`: checks that `reads`
/// accepts it as committed, then adds a field to each of its objects in turn.
/// Returns the places where `reads` rejected that and where it accepted it.
fn with_an_unknown_field(
    kinds: &[&str],
    reads: impl Fn(Value) -> Result<(), String>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let (mut rejected, mut accepted) = (BTreeSet::new(), BTreeSet::new());
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !kinds.iter().any(|kind| name.contains(kind)) {
            continue;
        }
        let wire: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        reads(wire.clone()).unwrap_or_else(|error| panic!("{name} as committed: {error}"));
        let mut at = Vec::new();
        objects(&wire, String::new(), &mut at);
        for pointer in at {
            let mut extra = wire.clone();
            let object = extra
                .pointer_mut(&pointer)
                .unwrap()
                .as_object_mut()
                .unwrap();
            // A copy of a sibling: a map takes it as one more entry, and a
            // struct sees a field it does not know.
            let sibling = object.values().next().cloned().unwrap_or(json!(1));
            object.insert("from_a_later_build".into(), sibling);
            let generic: Vec<&str> = pointer
                .split('/')
                .map(|step| match step.parse::<usize>() {
                    Ok(_) => "N",
                    Err(_) => step,
                })
                .collect();
            let place = format!("{name} {}", generic.join("/"));
            match reads(extra) {
                Ok(()) => accepted.insert(place),
                Err(_) => rejected.insert(place),
            };
        }
    }
    (rejected, accepted)
}

#[test]
fn every_response_object_ignores_a_field_it_does_not_know_unless_stored() {
    let (rejected, accepted) = with_an_unknown_field(&["-outcome-v", "-response-v"], |wire| {
        serde_json::from_value::<Response>(wire)
            .map(drop)
            .map_err(|error| error.to_string())
    });
    assert!(accepted.len() > 300, "{} places accepted", accepted.len());
    let strict: BTreeSet<String> = STRICT.iter().map(|place| place.to_string()).collect();
    let newly: Vec<_> = rejected.difference(&strict).collect();
    assert!(
        newly.is_empty(),
        "a response object rejects a field it does not know; drop deny_unknown_fields from \
         its type if only responses carry it, or list it in STRICT if it is stored: {newly:#?}"
    );
    let tolerant: Vec<_> = strict.difference(&rejected).collect();
    assert!(
        tolerant.is_empty(),
        "now tolerant or gone from the vectors; remove from STRICT: {tolerant:#?}"
    );
}

/// Requests, which the daemon reads, and events, which the store replays,
/// reject an unknown field at every level.
#[test]
fn requests_and_events_reject_a_field_they_do_not_know_at_every_level() {
    let (requests, accepted) = with_an_unknown_field(&["-command-v", "-request-v"], |wire| {
        serde_json::from_value::<Request>(wire)
            .map(drop)
            .map_err(|error| error.to_string())
    });
    assert!(requests.len() > 150, "{} request places", requests.len());
    assert!(accepted.is_empty(), "requests accepted: {accepted:#?}");
    let (events, accepted) = with_an_unknown_field(&["-event-v"], |wire| {
        serde_json::from_value::<langspan_core::Event>(wire)
            .map(drop)
            .map_err(|error| error.to_string())
    });
    assert!(events.len() > 100, "{} event places", events.len());
    assert!(accepted.is_empty(), "events accepted: {accepted:#?}");
}
