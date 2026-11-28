//! One golden vector per `Command` variant, at the current protocol version
//! (issue #321). Each fixture holds one request frame and must read back and
//! encode to the same bytes. Prepared adapter inputs never cross the wire:
//! they map to no fixture, and the wire refuses their names.
use std::collections::BTreeSet;
use std::path::Path;

use langspan::protocol::{PROTOCOL_VERSION, Request, encode};
use langspan_core::{Command, Outcome, Route, State, decide};

macro_rules! vectors {
    (
        on_wire { $($variant:ident => $file:literal,)* }
        not_on_wire { $($internal:ident => $op:literal,)* }
    ) => {
        /// Each variant's fixture, or `None` for one that never crosses the
        /// wire. The match is exhaustive, so a new variant fails to compile
        /// here until it has a vector or is listed as not on the wire.
        fn fixture(command: &Command) -> Option<&'static str> {
            match command {
                $(Command::$variant { .. } => Some($file),)*
                $(Command::$internal { .. } => None,)*
            }
        }

        const FIXTURES: &[&str] = &[$($file),*];
        const NOT_ON_WIRE: &[&str] = &[$($op),*];
    };
}

vectors! {
    on_wire {
        WorkspaceRegister => "workspace-register-command-v10.json",
        WorkspaceList => "workspace-list-command-v10.json",
        WorkspaceMove => "workspace-move-command-v10.json",
        WorkspaceRemove => "workspace-remove-command-v10.json",
        SessionRegister => "session-register-command-v10.json",
        ObserveSessionLiveness => "observe-session-liveness-command-v10.json",
        SessionSeen => "session-seen-command-v10.json",
        RecordContext => "record-context-command-v10.json",
        ForgetTold => "forget-told-command-v10.json",
        ConfirmTold => "confirm-told-command-v10.json",
        IntentSet => "intent-set-command-v10.json",
        IntentEnd => "intent-end-command-v10.json",
        Admit => "admit-command-v10.json",
        AdmitTool => "admit-tool-command-v10.json",
        ObserveSource => "observe-source-command-v10.json",
        RemoteEnable => "remote-enable-command-v10.json",
        RemoteDisable => "remote-disable-command-v10.json",
        RemoteOutbox => "remote-outbox-command-v10.json",
        RemotePublished => "remote-published-command-v10.json",
        RemoteSent => "remote-sent-command-v10.json",
        ObserveRemote => "observe-remote-command-v10.json",
        ForgetRemote => "forget-remote-command-v10.json",
        RemoteWithdrawn => "remote-withdrawn-command-v10.json",
        RecordCheck => "record-check-command-v10.json",
        FlakyTests => "flaky-tests-command-v10.json",
        ForgetFlakyTest => "forget-flaky-test-command-v10.json",
        AutoCheckEnable => "auto-check-enable-command-v10.json",
        AutoCheckDisable => "auto-check-disable-command-v10.json",
        CheckStatus => "check-status-command-v10.json",
        LandingReadiness => "landing-readiness-command-v10.json",
        RecordLanding => "record-landing-command-v10.json",
        TakeTurn => "take-turn-command-v10.json",
        ReleaseTurn => "release-turn-command-v10.json",
        LeaveTurnQueue => "leave-turn-queue-command-v10.json",
        TurnQueue => "turn-queue-command-v10.json",
        ObserveInterface => "observe-interface-command-v10.json",
        ObserveInterfaceDependents => "observe-interface-dependents-command-v10.json",
        Remember => "remember-command-v10.json",
        ObserveNoteAnchors => "observe-note-anchors-command-v10.json",
        Forget => "forget-command-v10.json",
        SendMessage => "send-message-command-v10.json",
        AckMessage => "ack-message-command-v10.json",
        ShowMessage => "show-message-command-v10.json",
        MessageStatus => "message-status-command-v10.json",
        Handoff => "handoff-command-v10.json",
        RetractMessage => "retract-message-command-v10.json",
        ObserveScan => "observe-scan-command-v10.json",
        ObserveCheckout => "observe-checkout-command-v10.json",
        ObserveCheckoutRun => "observe-checkout-run-command-v10.json",
        ObserveUncoordinated => "observe-uncoordinated-command-v10.json",
        ObserveHosts => "observe-hosts-command-v10.json",
        Recap => "recap-command-v10.json",
        Trails => "trails-command-v10.json",
        Recall => "recall-command-v10.json",
        History => "history-command-v10.json",
        Today => "today-command-v10.json",
        Report => "report-command-v10.json",
        Count => "count-command-v10.json",
        StatusLine => "status-line-command-v10.json",
        RecordRefusal => "record-refusal-command-v10.json",
        Policy => "policy-command-v10.json",
        SetPolicy => "set-policy-command-v10.json",
        Holders => "holders-command-v10.json",
        RecordWait => "record-wait-command-v10.json",
        GraphSymbol => "graph-symbol-command-v10.json",
        GraphReferences => "graph-references-command-v10.json",
        GraphImpact => "graph-impact-command-v10.json",
        GraphBlindSpots => "graph-blind-spots-command-v10.json",
        GraphPath => "graph-path-command-v10.json",
        GraphMap => "graph-map-command-v10.json",
        GraphContext => "graph-context-command-v10.json",
        GraphInflight => "graph-inflight-command-v10.json",
        GraphExport => "graph-export-command-v10.json",
        GraphImport => "graph-import-command-v10.json",
        CheckWrite => "check-write-command-v10.json",
        CheckCalls => "check-calls-command-v10.json",
        AcknowledgeInterface => "acknowledge-interface-command-v10.json",
        BetweenSteps => "between-steps-command-v10.json",
        InterfaceShown => "interface-shown-command-v10.json",
        OutlineRead => "outline-read-command-v10.json",
        Complete => "complete-command-v10.json",
        Observe => "observe-command-v10.json",
        ObserveTool => "observe-tool-command-v10.json",
        ObserveDisk => "observe-disk-command-v10.json",
        ObserveBranch => "observe-branch-command-v10.json",
        ObserveWorkspaceGit => "observe-workspace-git-command-v10.json",
        SessionPrompt => "session-prompt-command-v10.json",
        SetPromptTargets => "set-prompt-targets-command-v10.json",
        ObservePresence => "observe-presence-command-v10.json",
        SetPresence => "set-presence-command-v10.json",
        SessionEnd => "session-end-command-v10.json",
        SessionStop => "session-stop-command-v10.json",
        ObserveToolBatch => "observe-tool-batch-command-v10.json",
        Brief => "brief-command-v10.json",
        Adopt => "adopt-command-v10.json",
        Status => "status-command-v10.json",
        StatusRecords => "status-records-command-v10.json",
        Dashboard => "dashboard-command-v10.json",
        Version => "version-command-v10.json",
        Events => "events-command-v10.json",
        Changes => "changes-command-v10.json",
        Compact => "compact-command-v10.json",
    }
    not_on_wire {
        WorkspaceRegisterVerified => "workspace_register_verified",
        WorkspaceMoveVerified => "workspace_move_verified",
        BriefPrepared => "brief_prepared",
        RememberPrepared => "remember_prepared",
        NoteAnchorsPrepared => "note_anchors_prepared",
    }
}

#[test]
fn every_command_variant_has_a_golden_vector_that_round_trips_byte_for_byte() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut named = BTreeSet::new();
    for &file in FIXTURES {
        assert!(named.insert(file.to_string()), "{file} is named twice");
        let wire = std::fs::read(dir.join(file))
            .unwrap_or_else(|error| panic!("{file}: {error}; add the vector"));
        let request: Request = serde_json::from_slice(&wire)
            .unwrap_or_else(|error| panic!("{file} does not read as a request: {error}"));
        assert_eq!(request.version, PROTOCOL_VERSION, "{file}");
        assert_eq!(
            fixture(&request.command),
            Some(file),
            "{file} holds another variant"
        );
        assert_eq!(
            encode(&request).unwrap(),
            wire,
            "{file} does not encode to its own bytes"
        );
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with("-command-v10.json"))
        .collect();
    assert_eq!(on_disk, named, "a vector on disk that no variant names");
}

/// The fields messages to other machines add (ADR 0041), each beside the
/// per-variant vector without them, which stays the shape older clients
/// send: a `--remote` send, an enable that shares messages, a briefing
/// that shows them, and a publication that carries them.
#[test]
fn the_fields_of_messages_to_other_machines_round_trip_byte_for_byte() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    for (file, holds) in [
        (
            "send-message-remote-request-v10.json",
            (|c: &Command| matches!(c, Command::SendMessage { remote: true, .. }))
                as fn(&Command) -> bool,
        ),
        ("remote-enable-share-messages-request-v10.json", |c| {
            matches!(
                c,
                Command::RemoteEnable {
                    share_messages: true,
                    ..
                }
            )
        }),
        ("brief-remote-messages-request-v10.json", |c| {
            matches!(
                c,
                Command::Brief {
                    remote_messages: true,
                    ..
                }
            )
        }),
        ("observe-remote-messages-request-v10.json", |c| {
            matches!(c, Command::ObserveRemote { publication, .. }
                if publication.messages.len() == 2)
        }),
    ] {
        let wire = std::fs::read(dir.join(file)).unwrap();
        let request: Request = serde_json::from_slice(&wire)
            .unwrap_or_else(|error| panic!("{file} does not read as a request: {error}"));
        assert!(holds(&request.command), "{file}: {:?}", request.command);
        assert_eq!(encode(&request).unwrap(), wire, "{file}");
    }
}

/// Today's route of each command, read from the three routing sites before
/// `Command::route` existed (issue #371): the core's refusal of commands
/// answered by the daemon's stores, the store's reads that it never records,
/// and the daemon's answers from itself and its source index.
fn recorded_route(op: &str) -> Route {
    match op {
        "version" => Route::Internal,
        "check_write" | "check_calls" => Route::Index,
        op if op.starts_with("graph_") => Route::Index,
        "events" | "today" | "report" | "count" | "history" | "status_records" | "changes"
        | "remote_sent" | "compact" | "show_message" | "message_status" => Route::Store,
        "status" | "dashboard" | "status_line" | "remote_outbox" | "check_status"
        | "flaky_tests" | "landing_readiness" | "turn_queue" | "workspace_list" | "recap"
        | "trails" | "policy" | "holders" | "recall" => Route::Read,
        // A dry run without `apply`, as its vector sends it.
        "workspace_remove" => Route::Read,
        _ => Route::Decide,
    }
}

#[test]
fn every_command_routes_as_recorded() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut cases: Vec<(String, Command, Route)> = FIXTURES
        .iter()
        .map(|file| {
            let wire = std::fs::read(dir.join(file)).unwrap();
            let request: Request = serde_json::from_slice(&wire).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&wire).unwrap();
            let op = value["command"]["op"].as_str().unwrap().to_string();
            let route = recorded_route(&op);
            (op, request.command, route)
        })
        .collect();
    for (json, route) in [
        (
            r#"{"op":"workspace_remove","workspace_id":"app","apply":true}"#,
            Route::Decide,
        ),
        (
            r#"{"op":"compact","keep_days":1,"apply":true}"#,
            Route::Store,
        ),
        (
            r#"{"op":"graph_import","workspace_id":"app","path":"/x","apply":true}"#,
            Route::Index,
        ),
    ] {
        cases.push((json.into(), serde_json::from_str(json).unwrap(), route));
    }
    for (name, command) in [
        (
            "workspace_register_verified",
            Command::WorkspaceRegisterVerified {
                workspace_id: "app".into(),
                root: "/app".into(),
                verified_overlaps: Vec::new(),
            },
        ),
        (
            "workspace_move_verified",
            Command::WorkspaceMoveVerified {
                workspace_id: "app".into(),
                root: "/app".into(),
                verified_overlaps: Vec::new(),
            },
        ),
        (
            "remember_prepared",
            Command::RememberPrepared {
                workspace_id: "app".into(),
                author: None,
                text: "t".into(),
                paths: Vec::new(),
                symbol: None,
                tags: Vec::new(),
                index: None,
            },
        ),
    ] {
        cases.push((name.into(), command, Route::Decide));
    }
    let state = State::default();
    for (name, command, route) in &cases {
        assert_eq!(command.route(), *route, "{name}");
        // The core refuses exactly what the daemon answers elsewhere.
        let refused = matches!(
            decide(&state, command, "r-1", 0).outcome,
            Outcome::Error { ref code, .. } if code == "storage_operation"
        );
        assert_eq!(
            refused,
            matches!(route, Route::Store | Route::Index | Route::Internal),
            "{name}"
        );
    }
}

#[test]
fn prepared_adapter_inputs_are_refused_on_the_wire() {
    for op in NOT_ON_WIRE {
        let wire = format!(
            r#"{{"version":{PROTOCOL_VERSION},"request_id":"r-1","command":{{"op":"{op}"}}}}"#
        );
        let error = serde_json::from_str::<Request>(&wire).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("unknown variant `{op}`")),
            "{op}: {error}"
        );
    }
}
