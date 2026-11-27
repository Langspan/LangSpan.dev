//! One golden vector per `Event` variant, at the current protocol version
//! (issue #320). Events are the persisted format: each fixture holds one
//! event as the store writes it, and must read back and encode to the same
//! bytes.
use std::collections::BTreeSet;
use std::path::Path;

use langspan_core::Event;

macro_rules! vectors {
    ($($variant:ident => $file:literal,)*) => {
        /// Each variant's fixture. The match is exhaustive, so a new variant
        /// fails to compile here until it has a vector.
        fn fixture(event: &Event) -> &'static str {
            match event {
                $(Event::$variant { .. } => $file,)*
            }
        }

        const FIXTURES: &[&str] = &[$($file),*];
    };
}

vectors! {
    Booted => "booted-event-v10.json",
    ClockAnchored => "clock-anchored-event-v10.json",
    WriteRefused => "write-refused-event-v10.json",
    WriteWaited => "write-waited-event-v10.json",
    RefusalSettled => "refusal-settled-event-v10.json",
    PolicySet => "policy-set-event-v10.json",
    WorkspaceRegistered => "workspace-registered-event-v10.json",
    WorkspaceMoved => "workspace-moved-event-v10.json",
    WorkspaceRemoved => "workspace-removed-event-v10.json",
    SessionRegistered => "session-registered-event-v10.json",
    SessionLivenessObserved => "session-liveness-observed-event-v10.json",
    SessionSeen => "session-seen-event-v10.json",
    IntentSet => "intent-set-event-v10.json",
    IntentEnded => "intent-ended-event-v10.json",
    Reserved => "reserved-event-v10.json",
    LeaseGranted => "lease-granted-event-v10.json",
    LeaseCompleted => "lease-completed-event-v10.json",
    LeaseExpired => "lease-expired-event-v10.json",
    OperationObserved => "operation-observed-event-v10.json",
    DiskObserved => "disk-observed-event-v10.json",
    BranchObserved => "branch-observed-event-v10.json",
    WorkspaceGitObserved => "workspace-git-observed-event-v10.json",
    SessionPrompted => "session-prompted-event-v10.json",
    PromptTargetsSet => "prompt-targets-set-event-v10.json",
    PresenceObserved => "presence-observed-event-v10.json",
    PresenceSet => "presence-set-event-v10.json",
    SessionEnded => "session-ended-event-v10.json",
    SessionAdopted => "session-adopted-event-v10.json",
    TrailRecorded => "trail-recorded-event-v10.json",
    TrailTaken => "trail-taken-event-v10.json",
    SessionStopped => "session-stopped-event-v10.json",
    ToolBatchObserved => "tool-batch-observed-event-v10.json",
    BriefingDelivered => "briefing-delivered-event-v10.json",
    ContextAdded => "context-added-event-v10.json",
    ToldForgotten => "told-forgotten-event-v10.json",
    InflightDelivered => "inflight-delivered-event-v10.json",
    InterfaceAlerted => "interface-alerted-event-v10.json",
    InterfaceNoticesTold => "interface-notices-told-event-v10.json",
    ReadOutlined => "read-outlined-event-v10.json",
    SourceObserved => "source-observed-event-v10.json",
    RemoteEnabled => "remote-enabled-event-v10.json",
    RemoteDisabled => "remote-disabled-event-v10.json",
    RemotePublished => "remote-published-event-v10.json",
    RemoteObserved => "remote-observed-event-v10.json",
    RemoteForgotten => "remote-forgotten-event-v10.json",
    RemoteWithdrawn => "remote-withdrawn-event-v10.json",
    CheckRecorded => "check-recorded-event-v10.json",
    AutoCheckEnabled => "auto-check-enabled-event-v10.json",
    AutoCheckDisabled => "auto-check-disabled-event-v10.json",
    CheckReported => "check-reported-event-v10.json",
    CheckDelivered => "check-delivered-event-v10.json",
    TestsRecorded => "tests-recorded-event-v10.json",
    FlakyTestForgotten => "flaky-test-forgotten-event-v10.json",
    Landed => "landed-event-v10.json",
    TurnTaken => "turn-taken-event-v10.json",
    TurnReleased => "turn-released-event-v10.json",
    TurnRenewed => "turn-renewed-event-v10.json",
    TurnExpired => "turn-expired-event-v10.json",
    TurnQueueJoined => "turn-queue-joined-event-v10.json",
    TurnQueueSeen => "turn-queue-seen-event-v10.json",
    TurnQueueLeft => "turn-queue-left-event-v10.json",
    InterfaceChanged => "interface-changed-event-v10.json",
    InterfaceDependentsFound => "interface-dependents-found-event-v10.json",
    Remembered => "remembered-event-v10.json",
    Forgotten => "forgotten-event-v10.json",
    NoteAnchorChanged => "note-anchor-changed-event-v10.json",
    MessageSent => "message-sent-event-v10.json",
    MessageAcknowledged => "message-acknowledged-event-v10.json",
    MessageRetracted => "message-retracted-event-v10.json",
    RetractionDelivered => "retraction-delivered-event-v10.json",
    ScanFailed => "scan-failed-event-v10.json",
    ScanRecovered => "scan-recovered-event-v10.json",
    ScanCoverageObserved => "scan-coverage-observed-event-v10.json",
    CheckoutChanged => "checkout-changed-event-v10.json",
    CheckoutRunObserved => "checkout-run-observed-event-v10.json",
    UncoordinatedChanged => "uncoordinated-changed-event-v10.json",
    UncoordinatedDelivered => "uncoordinated-delivered-event-v10.json",
    HostNotInstalled => "host-not-installed-event-v10.json",
    HostCleared => "host-cleared-event-v10.json",
}

#[test]
fn every_event_variant_has_a_golden_vector_that_round_trips_byte_for_byte() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut named = BTreeSet::new();
    for &file in FIXTURES {
        assert!(named.insert(file.to_string()), "{file} is named twice");
        let wire = std::fs::read_to_string(dir.join(file))
            .unwrap_or_else(|error| panic!("{file}: {error}; add the vector"));
        let event: Event = serde_json::from_str(&wire)
            .unwrap_or_else(|error| panic!("{file} does not read as an event: {error}"));
        assert_eq!(fixture(&event), file, "{file} holds another variant");
        assert_eq!(
            format!("{}\n", serde_json::to_string(&event).unwrap()),
            wire,
            "{file} does not encode to its own bytes"
        );
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with("-event-v10.json"))
        .collect();
    assert_eq!(on_disk, named, "a vector on disk that no variant names");
}
