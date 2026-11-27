//! One golden vector per `Outcome` variant, at the current protocol version
//! (issue #322). Each fixture holds one response frame and must read back and
//! encode to the same bytes.
use std::collections::BTreeSet;
use std::path::Path;

use langspan::protocol::{PROTOCOL_VERSION, Response, encode};
use langspan_core::Outcome;

macro_rules! vectors {
    ($($variant:ident => $file:literal,)*) => {
        /// Each variant's fixture. The match is exhaustive, so a new variant
        /// fails to compile here until it has a vector.
        fn fixture(outcome: &Outcome) -> &'static str {
            match outcome {
                $(Outcome::$variant { .. } => $file,)*
            }
        }

        const FIXTURES: &[&str] = &[$($file),*];
    };
}

vectors! {
    Workspace => "workspace-outcome-v10.json",
    Workspaces => "workspaces-outcome-v10.json",
    WorkspaceMoved => "workspace-moved-outcome-v10.json",
    WorkspaceRemoval => "workspace-removal-outcome-v10.json",
    Session => "session-outcome-v10.json",
    SessionLiveness => "session-liveness-outcome-v10.json",
    SessionSeen => "session-seen-outcome-v10.json",
    ContextRecorded => "context-recorded-outcome-v10.json",
    ToldForgotten => "told-forgotten-outcome-v10.json",
    Intent => "intent-outcome-v10.json",
    IntentEnded => "intent-ended-outcome-v10.json",
    Admitted => "admitted-outcome-v10.json",
    Conflict => "conflict-outcome-v10.json",
    Completed => "completed-outcome-v10.json",
    Observed => "observed-outcome-v10.json",
    DiskObserved => "disk-observed-outcome-v10.json",
    BranchObserved => "branch-observed-outcome-v10.json",
    Status => "status-outcome-v10.json",
    StatusRecords => "status-records-outcome-v10.json",
    Events => "events-outcome-v10.json",
    Changes => "changes-outcome-v10.json",
    WorkspaceGit => "workspace-git-outcome-v10.json",
    SessionPrompted => "session-prompted-outcome-v10.json",
    PromptTargets => "prompt-targets-outcome-v10.json",
    PresenceObserved => "presence-observed-outcome-v10.json",
    Presence => "presence-outcome-v10.json",
    SessionEnded => "session-ended-outcome-v10.json",
    Adopted => "adopted-outcome-v10.json",
    SessionStopped => "session-stopped-outcome-v10.json",
    ToolBatchObserved => "tool-batch-observed-outcome-v10.json",
    AutoCheck => "auto-check-outcome-v10.json",
    AutoCheckDisabled => "auto-check-disabled-outcome-v10.json",
    Briefing => "briefing-outcome-v10.json",
    StaleBase => "stale-base-outcome-v10.json",
    UnreadLines => "unread-lines-outcome-v10.json",
    SourceObserved => "source-observed-outcome-v10.json",
    RemoteLink => "remote-link-outcome-v10.json",
    RemoteDisabled => "remote-disabled-outcome-v10.json",
    RemoteOutbox => "remote-outbox-outcome-v10.json",
    RemoteSent => "remote-sent-outcome-v10.json",
    Compaction => "compaction-outcome-v10.json",
    RemoteObserved => "remote-observed-outcome-v10.json",
    CheckRecorded => "check-recorded-outcome-v10.json",
    Checks => "checks-outcome-v10.json",
    FlakyTests => "flaky-tests-outcome-v10.json",
    FlakyTestForgotten => "flaky-test-forgotten-outcome-v10.json",
    Readiness => "readiness-outcome-v10.json",
    Landed => "landed-outcome-v10.json",
    Turn => "turn-outcome-v10.json",
    TurnBusy => "turn-busy-outcome-v10.json",
    TurnWaiting => "turn-waiting-outcome-v10.json",
    TurnUnavailable => "turn-unavailable-outcome-v10.json",
    TurnQueue => "turn-queue-outcome-v10.json",
    TurnQueueLeft => "turn-queue-left-outcome-v10.json",
    TurnReleased => "turn-released-outcome-v10.json",
    InterfaceObserved => "interface-observed-outcome-v10.json",
    InterfaceDependentsObserved => "interface-dependents-observed-outcome-v10.json",
    Remembered => "remembered-outcome-v10.json",
    NoteAnchorsObserved => "note-anchors-observed-outcome-v10.json",
    Forgotten => "forgotten-outcome-v10.json",
    MessageSent => "message-sent-outcome-v10.json",
    MessageAcknowledged => "message-acknowledged-outcome-v10.json",
    MessageShown => "message-shown-outcome-v10.json",
    MessageStatus => "message-status-outcome-v10.json",
    MessageRetracted => "message-retracted-outcome-v10.json",
    ScanObserved => "scan-observed-outcome-v10.json",
    HostsObserved => "hosts-observed-outcome-v10.json",
    CheckoutObserved => "checkout-observed-outcome-v10.json",
    CheckoutRunObserved => "checkout-run-observed-outcome-v10.json",
    UncoordinatedObserved => "uncoordinated-observed-outcome-v10.json",
    Recap => "recap-outcome-v10.json",
    Trails => "trails-outcome-v10.json",
    Recall => "recall-outcome-v10.json",
    History => "history-outcome-v10.json",
    Today => "today-outcome-v10.json",
    Report => "report-outcome-v10.json",
    Count => "count-outcome-v10.json",
    StatusLine => "status-line-outcome-v10.json",
    RefusalRecorded => "refusal-recorded-outcome-v10.json",
    Policy => "policy-outcome-v10.json",
    Holders => "holders-outcome-v10.json",
    WaitRecorded => "wait-recorded-outcome-v10.json",
    Dashboard => "dashboard-outcome-v10.json",
    Version => "version-outcome-v10.json",
    Graph => "graph-outcome-v10.json",
    GraphContext => "graph-context-outcome-v10.json",
    GraphInflight => "graph-inflight-outcome-v10.json",
    GraphPath => "graph-path-outcome-v10.json",
    GraphMap => "graph-map-outcome-v10.json",
    GraphExported => "graph-exported-outcome-v10.json",
    GraphImported => "graph-imported-outcome-v10.json",
    CallBreaks => "call-breaks-outcome-v10.json",
    WriteCheck => "write-check-outcome-v10.json",
    GraphBlindSpots => "graph-blind-spots-outcome-v10.json",
    InterfaceAcknowledged => "interface-acknowledged-outcome-v10.json",
    ReadOutline => "read-outline-outcome-v10.json",
    BetweenSteps => "between-steps-outcome-v10.json",
    Error => "error-outcome-v10.json",
}

#[test]
fn every_outcome_variant_has_a_golden_vector_that_round_trips_byte_for_byte() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut named = BTreeSet::new();
    for &file in FIXTURES {
        assert!(named.insert(file.to_string()), "{file} is named twice");
        let wire = std::fs::read(dir.join(file))
            .unwrap_or_else(|error| panic!("{file}: {error}; add the vector"));
        let response: Response = serde_json::from_slice(&wire)
            .unwrap_or_else(|error| panic!("{file} does not read as a response: {error}"));
        assert_eq!(response.version, PROTOCOL_VERSION, "{file}");
        assert_eq!(
            fixture(&response.outcome),
            file,
            "{file} holds another variant"
        );
        assert_eq!(
            encode(&response).unwrap(),
            wire,
            "{file} does not encode to its own bytes"
        );
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with("-outcome-v10.json"))
        .collect();
    assert_eq!(on_disk, named, "a vector on disk that no variant names");
}
