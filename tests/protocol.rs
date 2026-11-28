use langspan::protocol::{CAPABILITIES, Capabilities, encode_with, fits_with, for_capabilities};
use langspan::protocol::{
    OLDEST_PROTOCOL_VERSION, PROTOCOL_VERSION, PROTOCOL_VERSION_4, PROTOCOL_VERSION_5,
    PROTOCOL_VERSION_6, PROTOCOL_VERSION_7, PROTOCOL_VERSION_8, PROTOCOL_VERSION_9,
    PROTOCOL_VERSION_10, Request, Response, encode, encode_for, encode_record_for, read_frame,
};
use langspan_core::Command;

// V1 fixtures are archived parsing/replay vectors. Their continued readability
// does not imply that the live V2 daemon accepts V1 IPC requests.
#[test]
fn v1_request_matches_the_committed_golden_vector() {
    let request: Request =
        serde_json::from_str(include_str!("../fixtures/request-v1.json")).unwrap();
    assert_eq!(request.version, 1);
    assert_eq!(
        request.command,
        Command::Admit {
            session_id: "alice".into(),
            paths: vec!["src/api.rs".into()],
            ttl_ms: 5000
        }
    );
    assert_eq!(
        encode(&request).unwrap(),
        include_bytes!("../fixtures/request-v1.json")
    );
}

#[test]
fn frames_are_bounded_and_require_newline() {
    assert!(read_frame(&b"{}"[..]).is_err());
    assert!(read_frame(vec![b'a'; 65_537].as_slice()).is_err());
    assert_eq!(read_frame(&b"{}\nignored"[..]).unwrap(), b"{}\n");
}

#[test]
fn typed_contract_rejects_unknown_fields() {
    assert!(
        serde_json::from_str::<Request>(
            r#"{"version":1,"request_id":"r","extra":true,"command":{"op":"status"}}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<Request>(
            r#"{"version":1,"request_id":"r","command":{"op":"status","allow":true}}"#
        )
        .is_err()
    );
}

#[test]
fn v1_response_distinguishes_an_original_grant_from_current_validity() {
    let response: langspan::protocol::Response =
        serde_json::from_str(include_str!("../fixtures/response-v1.json")).unwrap();
    assert!(response.replayed);
    assert_eq!(response.lease_valid, Some(false));
    assert!(matches!(
        response.outcome,
        langspan_core::Outcome::Admitted { .. }
    ));
    assert_eq!(
        encode(&response).unwrap(),
        include_bytes!("../fixtures/response-v1.json")
    );
}

#[test]
fn v1_observe_request_matches_the_committed_golden_vector() {
    let request: Request =
        serde_json::from_str(include_str!("../fixtures/observe-request-v1.json")).unwrap();
    assert!(matches!(
        &request.command,
        Command::Observe {
            result: langspan_core::ToolResult::Succeeded,
            lease_id: Some(_),
            ..
        }
    ));
    assert_eq!(
        encode(&request).unwrap(),
        include_bytes!("../fixtures/observe-request-v1.json")
    );
}

#[test]
fn v2_requests_match_the_committed_golden_vectors() {
    let vectors: &[&[u8]] = &[
        include_bytes!("../fixtures/request-v2.json"),
        include_bytes!("../fixtures/changes-request-v2.json"),
        include_bytes!("../fixtures/disk-request-v2.json"),
    ];
    for wire in vectors {
        let request: Request = serde_json::from_slice(wire).unwrap();
        assert_eq!(request.version, 2, "version 2 vectors stay decodable");
        assert!(matches!(
            &request.command,
            Command::AdmitTool { .. } | Command::Changes { .. } | Command::ObserveDisk { .. }
        ));
        assert_eq!(encode(&request).unwrap(), *wire);
    }
}

#[test]
fn v2_changes_response_matches_the_committed_golden_vector() {
    let wire = include_bytes!("../fixtures/response-v2.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(response.version, 2);
    assert!(!response.replayed);
    assert!(
        matches!(&response.outcome, langspan_core::Outcome::Changes { events, scan: Some(scan) }
        if events.is_empty() && scan.workspace_id == "work" && scan.generation == 2)
    );
    assert_eq!(encode(&response).unwrap(), wire);
}

#[test]
fn v3_adoption_request_outcome_and_left_work_briefing_request_match_the_golden_vectors() {
    use langspan_core::{AdoptReason, Adoption, Outcome};
    let wire = include_bytes!("../fixtures/adopt-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::Adopt {
            session_id: "claude-code.bob".into(),
            from: "claude-code.alice".into(),
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);
    let unknown = String::from_utf8_lossy(wire).replace(r#""from""#, r#""paths":["a.txt"],"from""#);
    assert!(serde_json::from_str::<Request>(&unknown).is_err());

    let wire = include_bytes!("../fixtures/adopted-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        response.outcome,
        Outcome::Adopted {
            adoption: Adoption {
                from: "claude-code.alice".into(),
                session_id: "claude-code.bob".into(),
                workspace_id: "work".into(),
                reason: Some(AdoptReason::SameTask {}),
                at_ms: 5000,
                generation: 2,
            },
            root: "/repos/work".into(),
            recorded: true,
            live: Vec::new(),
            earlier: Vec::new(),
            kept: Vec::new(),
        }
    );
    assert_eq!(encode(&response).unwrap(), wire);

    // A briefing request names the sessions that left work; one without
    // the field (every earlier request) names none.
    let wire = include_bytes!("../fixtures/left-work-brief-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &request.command,
        Command::Brief { left, .. } if left == &["claude-code.alice".to_owned()]
    ));
    assert_eq!(encode(&request).unwrap(), wire);
    let older: Request =
        serde_json::from_slice(include_bytes!("../fixtures/brief-request-v3.json")).unwrap();
    assert!(matches!(&older.command, Command::Brief { left, .. } if left.is_empty()));
}

#[test]
fn v3_briefing_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/brief-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, OLDEST_PROTOCOL_VERSION);
    assert!(matches!(
        &request.command,
        Command::Brief { max_peers: 6, .. }
    ));
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_bytes!("../fixtures/briefing-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(response.version, OLDEST_PROTOCOL_VERSION);
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    assert!(briefing.changed);
    assert_eq!(briefing.peers[0].overlap, ["src/api.rs"]);
    assert!(!briefing.peers[0].same_workspace);
    assert_eq!(encode(&response).unwrap(), wire);
    let unknown = String::from_utf8_lossy(wire)
        .replace(r#""changed":true"#, r#""changed":true,"allow":true"#);
    // Ignored, not obeyed (issue #332).
    assert_eq!(
        serde_json::from_str::<Response>(&unknown).unwrap(),
        response
    );
}

/// What changed recently (roadmap C3): optional fields, so the earlier
/// briefing vectors above still decode and encode unchanged.
#[test]
fn v3_recent_briefing_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/brief-recent-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &request.command,
        Command::Brief {
            recent: true,
            recent_window_ms: Some(14_400_000),
            mid_turn: false,
            ..
        }
    ));
    assert_eq!(encode(&request).unwrap(), wire);
    let old: Request =
        serde_json::from_slice(include_bytes!("../fixtures/brief-request-v3.json")).unwrap();
    assert!(matches!(
        &old.command,
        Command::Brief {
            recent: false,
            recent_window_ms: None,
            ..
        }
    ));

    let wire = include_bytes!("../fixtures/recent-briefing-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    let recent = briefing.recent.as_ref().unwrap();
    assert_eq!(recent.changed_paths[0].sessions, 2);
    assert!(recent.changed_paths[1].elsewhere);
    assert_eq!(recent.contested_paths[0].refusals, 1);
    assert_eq!(recent.left_uncommitted[0].paths, ["src/store.rs"]);
    assert_eq!(recent.omitted, 3);
    assert_eq!(encode(&response).unwrap(), wire);
    let unknown = String::from_utf8_lossy(wire).replace(
        r#""window_ms":14400000"#,
        r#""window_ms":14400000,"summary":"x""#,
    );
    assert!(serde_json::from_str::<Response>(&unknown).is_err());
}

#[test]
fn v3_base_admission_and_stale_result_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/admit-base-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(&request.command, Command::AdmitTool { base, .. } if base.len() == 1));
    assert_eq!(encode(&request).unwrap(), wire);
    // A version 2 admission, which has no base, still decodes and re-encodes.
    let old = include_bytes!("../fixtures/request-v2.json");
    let request: Request = serde_json::from_slice(old).unwrap();
    assert!(matches!(&request.command, Command::AdmitTool { base, .. } if base.is_empty()));
    assert_eq!(encode(&request).unwrap(), old);

    let wire = include_bytes!("../fixtures/stale-base-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::StaleBase { stale } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(stale[0].changed_by.as_deref(), Some("codex.bob"));
    assert_eq!(response.lease_valid, None, "a stale result grants nothing");
    assert_eq!(encode(&response).unwrap(), wire);
}

#[test]
fn v3_overwritten_lines_and_unread_lines_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/admit-overwrites-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    let Command::AdmitTool { overwrites, .. } = &request.command else {
        panic!("{request:?}");
    };
    assert_eq!(overwrites[0].session_id, "claude-code.alice");
    assert_eq!(encode(&request).unwrap(), wire);
    // An admission without them encodes as before.
    let old = include_bytes!("../fixtures/admit-base-request-v3.json");
    let request: Request = serde_json::from_slice(old).unwrap();
    assert!(
        matches!(&request.command, Command::AdmitTool { overwrites, .. } if overwrites.is_empty())
    );
    assert_eq!(encode(&request).unwrap(), old);

    let wire = include_bytes!("../fixtures/unread-lines-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::UnreadLines { lines } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(lines[0].lines[0].start_line, 3);
    assert_eq!(response.lease_valid, None, "a refusal grants nothing");
    assert_eq!(encode(&response).unwrap(), wire);
    let unknown =
        String::from_utf8_lossy(wire).replace(r#""age_ms":2000"#, r#""age_ms":2000,"allow":true"#);
    assert!(serde_json::from_str::<Response>(&unknown).is_err());
}

#[test]
fn v3_code_context_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/graph-context-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, OLDEST_PROTOCOL_VERSION);
    assert!(matches!(
        &request.command,
        Command::GraphContext { names, limit: 3, .. } if names == &["upload", "gate"]
    ));
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_bytes!("../fixtures/graph-context-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::GraphContext { matches, coverage } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(matches[0].definitions[0].path, "src/upload.rs");
    assert_eq!(matches[0].mentioned_in, ["src/main.rs"]);
    assert!(coverage.scanned);
    assert_eq!(response.lease_valid, None, "a read grants nothing");
    assert_eq!(encode(&response).unwrap(), wire);
    let unknown = String::from_utf8_lossy(wire)
        .replace(r#""truncated":false"#, r#""truncated":false,"prompt":"x""#);
    // Ignored, not obeyed (issue #332).
    assert_eq!(
        serde_json::from_str::<Response>(&unknown).unwrap(),
        response
    );
}

#[test]
fn v4_version_request_and_answer_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/version-request-v4.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.command, Command::Version);
    assert_eq!(encode(&request).unwrap(), wire);
    // A unit request, strict as `status` is: no field is accepted.
    assert!(
        serde_json::from_str::<Request>(
            r#"{"version":4,"request_id":"read","command":{"op":"version","check":true}}"#
        )
        .is_err()
    );

    let wire = include_bytes!("../fixtures/version-response-v4.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        response.outcome,
        langspan_core::Outcome::Version {
            version: "0.4.0".into(),
            pid: 4242
        }
    );
    assert_eq!(response.lease_valid, None, "a read grants nothing");
    assert_eq!(encode(&response).unwrap(), wire);
}

#[test]
fn v3_compaction_request_and_reports_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/compact-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::Compact {
            keep_days: 90,
            apply: true
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);
    // `apply` may be left out: a dry run.
    let dry: Request = serde_json::from_str(
        r#"{"version":3,"request_id":"read","command":{"op":"compact","keep_days":30}}"#,
    )
    .unwrap();
    assert_eq!(
        dry.command,
        Command::Compact {
            keep_days: 30,
            apply: false
        }
    );

    let wire = include_bytes!("../fixtures/compaction-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::Compaction { report } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(report.snapshot_seq, Some(812_440));
    assert!(report.held_by.as_deref().unwrap().contains("lease-7"));
    assert_eq!(encode(&response).unwrap(), wire);
    let unknown =
        String::from_utf8_lossy(wire).replace(r#""receipts":61204"#, r#""receipts":61204,"x":1"#);
    // Ignored (issue #332).
    assert_eq!(
        serde_json::from_str::<Response>(&unknown).unwrap(),
        response
    );

    let wire = include_bytes!("../fixtures/remote-sent-since-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        response.outcome,
        langspan_core::Outcome::RemoteSent {
            since_ms: Some(1_788_134_400_000),
            ..
        }
    ));
    assert_eq!(encode(&response).unwrap(), wire);
    // A store never compacted leaves the field out, as earlier builds did.
    let earlier = String::from_utf8_lossy(wire).replace(r#","since_ms":1788134400000"#, "");
    assert!(matches!(
        serde_json::from_str::<Response>(&earlier).unwrap().outcome,
        langspan_core::Outcome::RemoteSent { since_ms: None, .. }
    ));
}

#[test]
fn v3_blind_spots_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/graph-blind-spots-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &request.command,
        Command::GraphBlindSpots { workspace_id, limit: 50 } if workspace_id == "app"
    ));
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_bytes!("../fixtures/graph-blind-spots-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::GraphBlindSpots {
        blind_spots,
        coverage,
        scan_failure,
        truncated,
    } = &response.outcome
    else {
        panic!("{response:?}");
    };
    assert_eq!(blind_spots.partial[0].since_ms, 1_700_000_000_000);
    assert_eq!(blind_spots.unsupported_languages[0].files, 10);
    assert_eq!((coverage.blind_in_scope, *truncated), (3, false));
    assert!(scan_failure.is_none(), "absent when no scan failed");
    assert_eq!(encode(&response).unwrap(), wire);
    // A field of a later build is ignored (issue #332).
    let unknown = String::from_utf8_lossy(wire).replace(r#""files":10"#, r#""files":10,"x":1"#);
    assert_eq!(
        serde_json::from_str::<Response>(&unknown).unwrap(),
        response
    );
    // Coverage without `blind_in_scope` is what earlier builds send: zero.
    let earlier =
        String::from_utf8_lossy(include_bytes!("../fixtures/graph-context-response-v3.json"))
            .into_owned();
    let response: Response = serde_json::from_str(&earlier).unwrap();
    let langspan_core::Outcome::GraphContext { coverage, .. } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(coverage.blind_in_scope, 0);
    assert_eq!(encode(&response).unwrap(), earlier.as_bytes());

    // A lookup asks for the count; without the flag it is the request
    // earlier clients send.
    let asked = r#"{"version":3,"request_id":"read","command":{"op":"graph_impact","workspace_id":"app","path":"src/a.rs","limit":10,"blind_in_scope":true}}"#;
    let request: Request = serde_json::from_str(asked).unwrap();
    assert!(matches!(
        request.command,
        Command::GraphImpact {
            blind_in_scope: true,
            ..
        }
    ));
    assert_eq!(encode(&request).unwrap(), format!("{asked}\n").as_bytes());
    let plain = asked.replace(r#","blind_in_scope":true"#, "");
    let request: Request = serde_json::from_str(&plain).unwrap();
    assert!(matches!(
        request.command,
        Command::GraphImpact {
            blind_in_scope: false,
            ..
        }
    ));
    assert_eq!(encode(&request).unwrap(), format!("{plain}\n").as_bytes());

    // Roadmap D2: a depth above 1 is on the wire; depth 1 never is, so a
    // depth-1 request is byte-identical to the one earlier builds send.
    let deep = r#"{"version":3,"request_id":"read","command":{"op":"graph_impact","workspace_id":"app","path":"src/a.rs","limit":10,"blind_in_scope":true,"depth":2}}"#;
    let request: Request = serde_json::from_str(deep).unwrap();
    assert!(matches!(
        request.command,
        Command::GraphImpact { depth: 2, .. }
    ));
    assert_eq!(encode(&request).unwrap(), format!("{deep}\n").as_bytes());
    let request: Request = serde_json::from_str(asked).unwrap();
    assert!(matches!(
        request.command,
        Command::GraphImpact { depth: 1, .. }
    ));
}

/// Roadmap D3: `graph_path` and `graph_map` requests and answers.
#[test]
fn v4_path_and_map_requests_and_responses_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/graph-path-request-v4.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::GraphPath {
            workspace_id: "app".into(),
            from: "handler".into(),
            to: "save".into(),
            max_hops: 4,
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);
    // Without `max_hops`, the default of 4.
    let plain = String::from_utf8_lossy(wire).replace(r#","max_hops":4"#, "");
    let request: Request = serde_json::from_str(&plain).unwrap();
    assert!(matches!(
        request.command,
        Command::GraphPath { max_hops: 4, .. }
    ));

    let wire = include_bytes!("../fixtures/graph-path-response-v4.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::GraphPath {
        paths,
        edges_visited,
        truncated,
        ..
    } = &response.outcome
    else {
        panic!("{response:?}");
    };
    assert_eq!((paths.len(), *edges_visited, *truncated), (1, 2, false));
    assert_eq!(paths[0].steps[2].edge, "defines");
    assert_eq!(paths[0].steps[2].callee, None, "absent on the last step");
    assert_eq!(paths[0].steps[1].resolution, "path");
    assert_eq!(encode(&response).unwrap(), wire);
    let unknown =
        String::from_utf8_lossy(wire).replace(r#""edge":"defines""#, r#""edge":"defines","x":1"#);
    // Ignored (issue #332).
    assert_eq!(
        serde_json::from_str::<Response>(&unknown).unwrap(),
        response
    );

    let wire = include_bytes!("../fixtures/graph-map-request-v4.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::GraphMap {
            workspace_id: "app".into(),
            depth: 1,
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);
    let plain = String::from_utf8_lossy(wire).replace(r#","depth":1"#, "");
    let request: Request = serde_json::from_str(&plain).unwrap();
    assert!(matches!(
        request.command,
        Command::GraphMap { depth: 1, .. }
    ));

    let wire = include_bytes!("../fixtures/graph-map-response-v4.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::GraphMap { areas, edges, .. } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!((areas.len(), edges.len()), (2, 2));
    assert_eq!((edges[1].kind.as_str(), edges[1].count), ("imports", 1));
    assert_eq!(encode(&response).unwrap(), wire);
}

#[test]
fn v3_prompt_targets_request_and_target_briefing_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/prompt-targets-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::SetPromptTargets {
            session_id: "codex.bob".into(),
            paths: vec!["src/http.rs".into()],
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);
    // Paths only: a prompt's words have no field to travel in.
    let worded = String::from_utf8_lossy(wire).replace(
        r#""paths":["src/http.rs"]"#,
        r#""paths":["src/http.rs"],"prompt":"x""#,
    );
    assert!(serde_json::from_str::<Request>(&worded).is_err());

    let wire = include_bytes!("../fixtures/target-briefing-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(briefing.peers[0].target_overlap, ["src/http.rs"]);
    assert_eq!(encode(&response).unwrap(), wire);
    // A briefing from before prompt targets has no such field and still reads.
    let older = String::from_utf8_lossy(wire).replace(r#","target_overlap":["src/http.rs"]"#, "");
    let response: Response = serde_json::from_str(&older).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    assert!(briefing.peers[0].target_overlap.is_empty());
    assert_eq!(encode(&response).unwrap(), older.as_bytes());
}

/// Issue #537: a peer on the same task. Additive and defaulted, as
/// `target_overlap` was: a daemon or stored receipt without the field reads
/// it as false, and a briefing without it encodes as it did before.
#[test]
fn v3_same_task_briefing_matches_the_committed_golden_vector() {
    let wire = include_bytes!("../fixtures/same-task-briefing-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    assert!(briefing.peers[0].same_task);
    assert_eq!(encode(&response).unwrap(), wire);
    let older = String::from_utf8_lossy(wire).replace(r#","same_task":true"#, "");
    let response: Response = serde_json::from_str(&older).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    assert!(!briefing.peers[0].same_task);
    assert_eq!(encode(&response).unwrap(), older.as_bytes());
}

#[test]
fn v3_presence_requests_and_briefing_match_the_committed_golden_vectors() {
    use langspan_core::PresenceKind;
    let wire = include_bytes!("../fixtures/presence-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::ObservePresence {
            session_id: "claude-code.alice".into(),
            kind: PresenceKind::Search,
            paths: vec!["src/auth".into()],
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);
    // Paths only: a search pattern or a command has no field to travel in.
    for field in ["pattern", "command", "glob"] {
        let worded = String::from_utf8_lossy(wire).replace(
            r#""paths":["src/auth"]"#,
            &format!(r#""paths":["src/auth"],"{field}":"x""#),
        );
        assert!(serde_json::from_str::<Request>(&worded).is_err(), "{field}");
    }

    let wire = include_bytes!("../fixtures/set-presence-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::SetPresence {
            session_id: "claude-code.alice".into(),
            shown: false,
            window_ms: 600_000,
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_bytes!("../fixtures/presence-briefing-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    let looking = &briefing.peers[0].looking;
    assert_eq!(looking[0].path, "src/auth.rs");
    assert_eq!(looking[0].kind, PresenceKind::Read);
    assert_eq!(looking[1].age_s, 200);
    assert_eq!(encode(&response).unwrap(), wire);
    // A briefing from before presence has no such field and still reads.
    let older = String::from_utf8_lossy(wire).replace(
        r#","looking":[{"path":"src/auth.rs","kind":"read","age_s":180},{"path":"src/session","kind":"search","age_s":200}]"#,
        "",
    );
    let response: Response = serde_json::from_str(&older).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    assert!(briefing.peers[0].looking.is_empty());
    assert_eq!(encode(&response).unwrap(), older.as_bytes());
}

#[test]
fn v3_symbol_detail_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/graph-symbol-detail-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &request.command,
        Command::GraphSymbol { name, limit: 10, detail: true, .. } if name == "upload"
    ));
    assert_eq!(encode(&request).unwrap(), wire);
    // Without detail the request is the one earlier clients send.
    let plain = String::from_utf8_lossy(wire).replace(r#","detail":true"#, "");
    let request: Request = serde_json::from_str(&plain).unwrap();
    assert!(matches!(
        request.command,
        Command::GraphSymbol { detail: false, .. }
    ));
    assert_eq!(encode(&request).unwrap(), plain.as_bytes());

    let wire = include_bytes!("../fixtures/graph-symbol-detail-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::Graph {
        symbols,
        resolution,
        ..
    } = &response.outcome
    else {
        panic!("{response:?}");
    };
    assert_eq!(resolution.as_deref(), Some("name"));
    let symbol = &symbols[0];
    assert_eq!(symbol.calls.as_ref().unwrap()[0].count, 2);
    assert_eq!(symbol.callers.as_ref().unwrap().sites.len(), 2);
    assert_eq!(symbol.notes.as_ref().unwrap()[0].id, "note-1");
    assert_eq!(symbol.last_check.as_ref().unwrap().result, "passed");
    assert_eq!(encode(&response).unwrap(), wire);
    let unknown = String::from_utf8_lossy(wire).replace(r#""count":1,"#, r#""count":1,"x":1,"#);
    // Ignored (issue #332).
    assert_eq!(
        serde_json::from_str::<Response>(&unknown).unwrap(),
        response
    );
}

#[test]
fn v3_message_handoff_and_ack_requests_match_the_committed_golden_vectors() {
    let vectors: &[&[u8]] = &[
        include_bytes!("../fixtures/send-message-request-v3.json"),
        include_bytes!("../fixtures/broadcast-request-v3.json"),
        include_bytes!("../fixtures/handoff-request-v3.json"),
        include_bytes!("../fixtures/ack-message-request-v3.json"),
        include_bytes!("../fixtures/brief-mid-turn-request-v3.json"),
    ];
    for wire in vectors {
        let request: Request = serde_json::from_slice(wire).unwrap();
        assert_eq!(request.version, OLDEST_PROTOCOL_VERSION);
        assert_eq!(encode(&request).unwrap(), *wire);
    }
    let request: Request = serde_json::from_slice(vectors[0]).unwrap();
    assert!(matches!(
        &request.command,
        Command::SendMessage { to: Some(to), from: Some(_), .. } if to == "claude-code.alice"
    ));
    let request: Request = serde_json::from_slice(vectors[1]).unwrap();
    assert!(matches!(
        &request.command,
        Command::SendMessage {
            to: None,
            from: None,
            ..
        }
    ));
    let request: Request = serde_json::from_slice(vectors[2]).unwrap();
    assert!(matches!(&request.command, Command::Handoff { paths, .. } if paths.len() == 1));
    let request: Request = serde_json::from_slice(vectors[3]).unwrap();
    assert!(
        matches!(&request.command, Command::AckMessage { message_id, .. } if message_id == "msg-1")
    );
    let request: Request = serde_json::from_slice(vectors[4]).unwrap();
    assert!(matches!(
        &request.command,
        Command::Brief { mid_turn: true, .. }
    ));
    // The earlier briefing request, without the new field, is not mid-turn.
    let old: Request =
        serde_json::from_slice(include_bytes!("../fixtures/brief-request-v3.json")).unwrap();
    assert!(matches!(
        &old.command,
        Command::Brief {
            recent_window_ms: None,
            ..
        }
    ));
    let unknown = String::from_utf8_lossy(vectors[3])
        .replace(r#""message_id""#, r#""all":true,"message_id""#);
    assert!(serde_json::from_str::<Request>(&unknown).is_err());
}

#[test]
fn v3_briefing_with_messages_matches_the_committed_golden_vector() {
    let wire = include_bytes!("../fixtures/message-briefing-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(briefing.messages.len(), 2);
    assert_eq!(
        briefing.messages[1].to,
        langspan_core::Recipient::NextSession {}
    );
    assert_eq!(encode(&response).unwrap(), wire);
    let unknown = String::from_utf8_lossy(wire).replace(
        r#""kind":"next_session""#,
        r#""kind":"next_session","all":true"#,
    );
    assert!(serde_json::from_str::<Response>(&unknown).is_err());
}

#[test]
fn v3_show_message_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/show-message-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &request.command,
        Command::ShowMessage { workspace_id, message_id }
            if workspace_id == "app" && message_id == "msg-1"
    ));
    assert_eq!(encode(&request).unwrap(), wire);
    let unknown = String::from_utf8_lossy(wire).replace(
        r#""message_id":"msg-1""#,
        r#""message_id":"msg-1","session_id":"x""#,
    );
    assert!(serde_json::from_str::<Request>(&unknown).is_err());

    let wire = include_bytes!("../fixtures/show-message-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::MessageShown { message } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(message.paths, ["src/api.rs"]);
    assert!(!message.retracted);
    assert_eq!(encode(&response).unwrap(), wire);
}

#[test]
fn v3_message_status_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/message-status-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &request.command,
        Command::MessageStatus { workspace_id, from: Some(from), message_id: Some(id) }
            if workspace_id == "demo" && from == "codex.bob" && id == "m-9"
    ));
    assert_eq!(encode(&request).unwrap(), wire);
    // `last` leaves the ID out, and the owner leaves out the sender.
    let last: Request = serde_json::from_str(
        r#"{"version":3,"request_id":"read","command":{"op":"message_status","workspace_id":"demo"}}"#,
    )
    .unwrap();
    assert!(matches!(
        last.command,
        Command::MessageStatus {
            from: None,
            message_id: None,
            ..
        }
    ));
    let unknown = String::from_utf8_lossy(wire).replace(
        r#""message_id":"m-9""#,
        r#""message_id":"m-9","session_id":"x""#,
    );
    assert!(serde_json::from_str::<Request>(&unknown).is_err());

    let wire = include_bytes!("../fixtures/message-status-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::MessageStatus { status } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(status.state, langspan_core::MessageState::Pending);
    assert_eq!(
        status.recipients[0].via,
        Some(langspan_core::Delivery::Briefing)
    );
    assert_eq!(
        status.recipients[2].status,
        langspan_core::RecipientState::Ended
    );
    assert_eq!(encode(&response).unwrap(), wire);
}

#[test]
fn message_events_recorded_before_delivery_receipts_still_read() {
    use langspan_core::Event;
    let sent: Event = serde_json::from_str(
        r#"{"type":"message_sent","message":{"message_id":"m1","workspace_id":"app","from":null,"to":{"kind":"everyone"},"text":"hi","paths":[]}}"#,
    )
    .unwrap();
    assert!(matches!(
        sent,
        Event::MessageSent {
            at_ms: None,
            generation: None,
            ..
        }
    ));
    let acknowledged: Event = serde_json::from_str(
        r#"{"type":"message_acknowledged","workspace_id":"app","message_id":"m1","session_id":"codex.bob"}"#,
    )
    .unwrap();
    assert!(matches!(
        acknowledged,
        Event::MessageAcknowledged {
            via: None,
            at_ms: None,
            generation: None,
            ..
        }
    ));
    // A new record carries how and when, and is written the same way back.
    let json = r#"{"type":"message_acknowledged","workspace_id":"app","message_id":"m1","session_id":"codex.bob","via":"mcp_brief","at_ms":5,"generation":2}"#;
    let event: Event = serde_json::from_str(json).unwrap();
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    // The brief command says when the MCP tool asked, and only then.
    let brief = r#"{"version":3,"request_id":"b","command":{"op":"brief","session_id":"s","max_peers":4,"deliver":true,"mcp":true}}"#;
    let request: Request = serde_json::from_str(brief).unwrap();
    assert!(matches!(request.command, Command::Brief { mcp: true, .. }));
    assert_eq!(encode(&request).unwrap(), format!("{brief}\n").into_bytes());
}

#[test]
fn v3_message_retraction_matches_the_committed_golden_vectors() {
    let last = include_bytes!("../fixtures/retract-message-request-v3.json");
    let by_id = include_bytes!("../fixtures/retract-message-by-id-request-v3.json");
    for wire in [&last[..], &by_id[..]] {
        let request: Request = serde_json::from_slice(wire).unwrap();
        assert_eq!(request.version, OLDEST_PROTOCOL_VERSION);
        assert_eq!(encode(&request).unwrap(), wire);
    }
    let request: Request = serde_json::from_slice(last).unwrap();
    assert!(matches!(
        &request.command,
        Command::RetractMessage { from: Some(from), message_id: None, .. } if from == "codex.bob"
    ));
    let request: Request = serde_json::from_slice(by_id).unwrap();
    assert!(matches!(
        &request.command,
        Command::RetractMessage { from: None, message_id: Some(id), .. } if id == "m-9"
    ));
    let unknown = String::from_utf8_lossy(last)
        .replace(r#""message_id":null"#, r#""message_id":null,"all":true"#);
    assert!(serde_json::from_str::<Request>(&unknown).is_err());

    let wire = include_bytes!("../fixtures/retract-message-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &response.outcome,
        langspan_core::Outcome::MessageRetracted { withdrawn_unread, told_to_disregard, .. }
            if withdrawn_unread == &["codex.carol"] && told_to_disregard == &["claude-code.alice"]
    ));
    assert_eq!(encode(&response).unwrap(), wire);

    let wire = include_bytes!("../fixtures/retracted-briefing-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::Briefing { briefing } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(briefing.retracted.len(), 1);
    assert!(briefing.retracted[0].cut);
    assert_eq!(encode(&response).unwrap(), wire);
    // The briefing before withdrawals existed still reads the same.
    let old = include_bytes!("../fixtures/message-briefing-response-v3.json");
    let response: Response = serde_json::from_slice(old).unwrap();
    assert_eq!(encode(&response).unwrap(), old);
    let unknown = String::from_utf8_lossy(wire).replace(r#""cut":true"#, r#""cut":true,"all":1"#);
    assert!(serde_json::from_str::<Response>(&unknown).is_err());
}

#[test]
fn v3_region_admission_matches_the_committed_golden_vector() {
    let wire = include_bytes!("../fixtures/admit-regions-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    let Command::AdmitTool { regions, .. } = &request.command else {
        panic!("{request:?}");
    };
    assert_eq!(regions.len(), 2);
    assert!(matches!(
        &regions[1].regions[0],
        langspan_core::Region::Symbol { key, start_line: 12, .. } if key == &["Store", "open"]
    ));
    assert_eq!(encode(&request).unwrap(), wire);
    // An unknown field inside a region is refused, not ignored.
    let unknown =
        String::from_utf8_lossy(wire).replace(r#""end_line":13"#, r#""end_line":13,"allow":true"#);
    assert!(serde_json::from_str::<Request>(&unknown).is_err());
}

#[test]
fn v3_status_line_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/status-line-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::StatusLine {
            root: "/repos/app".into(),
            from_ms: 1_795_564_800_000,
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_bytes!("../fixtures/status-line-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let langspan_core::Outcome::StatusLine { line } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(line.text(), "langspan · 3 sessions here · 2 refusals today");
    assert_eq!(response.lease_valid, None, "a read grants nothing");
    assert_eq!(encode(&response).unwrap(), wire);
    // Counts only: a path has no field to travel in.
    let pathed = String::from_utf8_lossy(wire).replace(
        r#""refusals_today":2"#,
        r#""refusals_today":2,"paths":["src/api.rs"]"#,
    );
    assert!(serde_json::from_str::<Response>(&pathed).is_err());
}

#[test]
fn v3_held_commit_report_and_refusal_event_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/record-refusal-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::RecordRefusal {
            session_id: "claude-code.alice".into(),
            kind: langspan_core::RefusalKind::CommitHeld,
            paths: vec!["src/api.rs".into()],
            peer_session_id: Some("codex.bob".into()),
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_str!("../fixtures/write-refused-event-v3.json");
    let event: langspan_core::Event = serde_json::from_str(wire).unwrap();
    assert!(matches!(
        &event,
        langspan_core::Event::WriteRefused {
            kind: langspan_core::RefusalKind::Conflict,
            at_ms: 5000,
            ..
        }
    ));
    assert_eq!(
        format!("{}\n", serde_json::to_string(&event).unwrap()),
        wire
    );
    // An event without a peer omits it, and still reads.
    let alone = wire.replace(r#""peer_session_id":"codex.bob","#, "");
    assert!(serde_json::from_str::<langspan_core::Event>(&alone).is_ok());
    // A refused tool call keeps its input digest, for the report (H8); the
    // record above, written before it was kept, reads without one.
    let wire = include_str!("../fixtures/write-refused-digest-event-v3.json");
    let event: langspan_core::Event = serde_json::from_str(wire).unwrap();
    assert!(matches!(
        &event,
        langspan_core::Event::WriteRefused {
            kind: langspan_core::RefusalKind::StaleBase,
            input_digest: Some(digest),
            ..
        } if digest.len() == 64
    ));
    assert_eq!(
        format!("{}\n", serde_json::to_string(&event).unwrap()),
        wire
    );
}

#[test]
fn v3_index_export_and_import_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/graph-export-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &request.command,
        Command::GraphExport { workspace_id, path } if workspace_id == "app" && path == "/srv/index.jsonl"
    ));
    assert_eq!(encode(&request).unwrap(), wire);
    let wire = include_bytes!("../fixtures/graph-export-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        response.outcome,
        langspan_core::Outcome::GraphExported {
            files: 3912,
            partial_files: 2,
            ..
        }
    ));
    assert_eq!(encode(&response).unwrap(), wire);

    let wire = include_bytes!("../fixtures/graph-import-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        &request.command,
        Command::GraphImport { apply: true, .. }
    ));
    assert_eq!(encode(&request).unwrap(), wire);
    // Without `apply` it is a dry run, and the field is left out.
    let dry = String::from_utf8_lossy(wire).replace(r#","apply":true"#, "");
    let request: Request = serde_json::from_str(&dry).unwrap();
    assert!(matches!(
        &request.command,
        Command::GraphImport { apply: false, .. }
    ));
    assert_eq!(encode(&request).unwrap(), dry.as_bytes());
    let unknown = String::from_utf8_lossy(wire).replace(r#""apply":true"#, r#""apply":true,"x":1"#);
    assert!(serde_json::from_str::<Request>(&unknown).is_err());

    let wire = include_bytes!("../fixtures/graph-import-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert!(matches!(
        response.outcome,
        langspan_core::Outcome::GraphImported {
            applied: true,
            loaded: 3870,
            different: 42,
            ..
        }
    ));
    assert_eq!(encode(&response).unwrap(), wire);
}

/// Version 4 (roadmap D7): a history answer with a rename and a file move
/// carries `after_name`, `evidence` and `before_path`. To a version 3
/// client the same answer is encoded without them, and the earlier build
/// reads that form.
#[test]
fn v4_rename_history_response_and_its_version_3_form_match_the_golden_vectors() {
    let wire = include_bytes!("../fixtures/rename-history-response-v4.json");
    let mut response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(response.version, PROTOCOL_VERSION_4);
    let langspan_core::Outcome::History { history } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(history.entries[0].evidence.as_deref(), Some("same_body"));
    assert_eq!(
        history.entries[1].before_path.as_deref(),
        Some("app/old_users.py")
    );
    // Versions 5 to 8 added nothing to a history answer: 4 to 8 agree.
    assert_eq!(encode_for(PROTOCOL_VERSION_4, &response).unwrap(), wire);
    assert_eq!(encode_for(PROTOCOL_VERSION_5, &response).unwrap(), wire);
    assert_eq!(encode_for(PROTOCOL_VERSION_6, &response).unwrap(), wire);
    assert_eq!(encode_for(PROTOCOL_VERSION_7, &response).unwrap(), wire);
    assert_eq!(encode_for(PROTOCOL_VERSION, &response).unwrap(), wire);

    response.version = OLDEST_PROTOCOL_VERSION;
    let earlier = include_bytes!("../fixtures/rename-history-response-v3.json");
    assert_eq!(
        encode_for(OLDEST_PROTOCOL_VERSION, &response).unwrap(),
        earlier
    );
    // The earlier form is only for that reply: the response is unchanged.
    assert!(
        String::from_utf8(encode(&response).unwrap())
            .unwrap()
            .contains("before_path")
    );
}

/// A report's counts as a build before protocol version 5 reads them.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct EarlierCounts {
    edits_admitted: u32,
    edits_base_unknown: u32,
    shell_writes_after: u32,
    reads_reported: u32,
    intents_set: u32,
    intents_ended: u32,
    intents_left_open: u32,
    refusals: std::collections::BTreeMap<String, u32>,
    retried_unchanged: u32,
    briefings_delivered: u32,
}

/// Every `counts` object of a report answer, as an earlier build reads it.
fn earlier_counts(wire: &[u8]) -> Result<usize, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_slice(wire)?;
    let report = &value["outcome"]["report"];
    let mut read = 0;
    for list in ["hosts", "sessions"] {
        for row in report[list].as_array().unwrap() {
            serde_json::from_value::<EarlierCounts>(row["counts"].clone())?;
            read += 1;
        }
    }
    Ok(read)
}

/// Version 5 (roadmap H9, review comment 4711096877): a report answer's
/// counts carry `refusal_outcomes`. To a version 4 or version 3 client the
/// same answer is encoded without it, so an earlier build, which reads the
/// counts with `deny_unknown_fields`, still reads the whole answer.
#[test]
fn v5_report_response_and_its_version_4_and_3_forms_match_the_golden_vectors() {
    let wire = include_bytes!("../fixtures/report-response-v5.json");
    let mut response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(response.version, PROTOCOL_VERSION_5);
    let langspan_core::Outcome::Report { report } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(report.sessions[0].counts.refusal_outcomes["adapted"], 1);
    assert_eq!(report.hosts[0].counts.refusal_outcomes["adapted"], 1);
    // A version 5 client still reads `refusal_outcomes`.
    assert_eq!(encode_for(PROTOCOL_VERSION_5, &response).unwrap(), wire);
    // Version 6 changed landings, version 7 (issue #222) activity,
    // briefings and events, version 8 (issue #262) briefings, version 9
    // (roadmap E1) notes and version 10 (roadmap A11) activity and events:
    // their report is the version 5 form.
    for later in [
        PROTOCOL_VERSION_6,
        PROTOCOL_VERSION_7,
        PROTOCOL_VERSION_8,
        PROTOCOL_VERSION,
    ] {
        let mut current = response.clone();
        current.version = later;
        assert_eq!(
            String::from_utf8(encode_for(later, &current).unwrap()).unwrap(),
            String::from_utf8_lossy(wire).replacen(
                r#""version":5"#,
                &format!(r#""version":{later}"#),
                1
            ),
            "version {later}"
        );
    }
    assert!(
        earlier_counts(wire).is_err(),
        "an earlier build cannot read version 5 counts"
    );

    let earlier = include_bytes!("../fixtures/report-response-v4.json");
    response.version = 4;
    assert_eq!(encode_for(4, &response).unwrap(), earlier);
    assert_eq!(earlier_counts(earlier).unwrap(), 2);
    // Version 3 has the version 4 form of a report.
    response.version = OLDEST_PROTOCOL_VERSION;
    let oldest = encode_for(OLDEST_PROTOCOL_VERSION, &response).unwrap();
    assert_eq!(
        String::from_utf8(oldest).unwrap(),
        String::from_utf8_lossy(earlier).replacen(r#""version":4"#, r#""version":3"#, 1)
    );
    // The earlier form is only for that reply: the response is unchanged.
    assert!(
        String::from_utf8(encode(&response).unwrap())
            .unwrap()
            .contains("refusal_outcomes")
    );
}

/// Roadmap C9: a session end carrying the journal's facts for its trail, a
/// trail listing request, and its answer. An end without `trail`, as an
/// earlier client sends it, still reads.
#[test]
fn v4_trail_requests_and_listing_match_the_committed_golden_vectors() {
    use langspan_core::{CheckResult, Outcome};
    let wire = include_bytes!("../fixtures/session-end-trail-request-v4.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    let Command::SessionEnd {
        session_id,
        trail: Some(trail),
    } = &request.command
    else {
        panic!("{request:?}");
    };
    assert_eq!(session_id, "claude-code.alice");
    assert_eq!(trail.edited[0].lines[0].end_line, 72);
    assert_eq!(trail.uncommitted, ["src/upload.rs", "tests/upload.rs"]);
    assert_eq!(encode(&request).unwrap(), wire);
    let earlier: Request = serde_json::from_str(
        r#"{"version":3,"request_id":"e","command":{"op":"session_end","session_id":"claude-code.alice"}}"#,
    )
    .unwrap();
    assert!(matches!(
        earlier.command,
        Command::SessionEnd { trail: None, .. }
    ));

    let wire = include_bytes!("../fixtures/trail-request-v4.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::Trails {
            workspace_id: "demo".into(),
            limit: Some(4),
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_bytes!("../fixtures/trails-response-v4.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let Outcome::Trails { trails, .. } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(trails[0].age_ms, Some(1_200_000));
    assert_eq!(trails[0].trail.checks[0].result, CheckResult::Passed {});
    assert_eq!(trails[0].trail.taken_by.as_deref(), Some("codex.bob"));
    assert_eq!(encode(&response).unwrap(), wire);
}

/// Issue #214: a between-step watcher's ask and the answer that tells it a
/// notice. `stops` is absent from a first ask.
#[test]
fn v4_between_steps_request_and_outcome_match_the_committed_golden_vectors() {
    use langspan_core::Outcome;
    let wire = include_bytes!("../fixtures/between-steps-request-v4.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(
        request.command,
        Command::BetweenSteps {
            session_id: "claude-code.alice".into(),
            stops: Some(3),
            prompt_id: Some("38ca4096-2385-4d27-97e1-59b74594c136".into()),
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);
    let first: Request = serde_json::from_str(
        r#"{"version":4,"request_id":"w","command":{"op":"between_steps","session_id":"claude-code.alice"}}"#,
    )
    .unwrap();
    assert!(matches!(
        first.command,
        Command::BetweenSteps {
            stops: None,
            prompt_id: None,
            ..
        }
    ));
    // The hook's report of the notices its briefing showed.
    let shown = r#"{"version":4,"request_id":"s","command":{"op":"interface_shown","session_id":"claude-code.alice","notice_ids":["n1","n2"]}}"#;
    let request: Request = serde_json::from_str(shown).unwrap();
    assert_eq!(
        request.command,
        Command::InterfaceShown {
            session_id: "claude-code.alice".into(),
            notice_ids: vec!["n1".into(), "n2".into()],
        }
    );
    assert_eq!(
        String::from_utf8(encode(&request).unwrap())
            .unwrap()
            .trim_end(),
        shown
    );
    // A stop names its prompt; one from an older hook names none.
    let stop: Request = serde_json::from_str(
        r#"{"version":4,"request_id":"t","command":{"op":"session_stop","session_id":"claude-code.alice","prompt_id":"p1"}}"#,
    )
    .unwrap();
    assert!(matches!(
        stop.command,
        Command::SessionStop { prompt_id: Some(ref id), .. } if id == "p1"
    ));
    let older: Request = serde_json::from_str(
        r#"{"version":4,"request_id":"t","command":{"op":"session_stop","session_id":"claude-code.alice"}}"#,
    )
    .unwrap();
    assert!(matches!(
        older.command,
        Command::SessionStop {
            prompt_id: None,
            ..
        }
    ));

    let wire = include_bytes!("../fixtures/between-steps-response-v4.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let Outcome::BetweenSteps {
        open,
        stops,
        interface_changes,
        ..
    } = &response.outcome
    else {
        panic!("{response:?}");
    };
    assert!(*open);
    assert_eq!(*stops, 3);
    assert_eq!(interface_changes[0].affected, ["src/main.rs"]);
    assert_eq!(encode(&response).unwrap(), wire);
}

/// Roadmap C9: a version 3 client reads briefings and recaps with
/// `deny_unknown_fields`, so its answer carries no trail.
#[test]
fn a_version_3_reply_leaves_out_trails() {
    use langspan_core::{Briefing, Outcome, TrailView};
    let listing: Response =
        serde_json::from_slice(include_bytes!("../fixtures/trails-response-v4.json")).unwrap();
    let Outcome::Trails { trails, .. } = listing.outcome else {
        unreachable!()
    };
    let trail: TrailView = trails[0].clone();
    let mut briefing: Briefing = serde_json::from_value(serde_json::json!({
        "session_id": "codex.bob", "changed": true, "peers": [], "omitted_peers": 0,
    }))
    .unwrap();
    briefing.trail = Some(trail.clone());
    // Issue #214: notice IDs are version 5.
    briefing.interface_notice_ids = vec!["n1".into()];
    let response = Response {
        version: OLDEST_PROTOCOL_VERSION,
        request_id: "r".into(),
        replayed: false,
        generation: 1,
        lease_valid: None,
        outcome: Outcome::Briefing {
            briefing: briefing.clone(),
        },
    };
    let earlier: serde_json::Value =
        serde_json::from_slice(&encode_for(OLDEST_PROTOCOL_VERSION, &response).unwrap()).unwrap();
    assert!(
        earlier["outcome"]["briefing"].get("trail").is_none(),
        "{earlier:#}"
    );
    assert!(
        earlier["outcome"]["briefing"]
            .get("interface_notice_ids")
            .is_none(),
        "{earlier:#}"
    );
    assert_eq!(earlier["outcome"]["briefing"]["session_id"], "codex.bob");
    // A version 4 client, which also reads a briefing with
    // `deny_unknown_fields`, keeps the trail and gets no notice IDs.
    let four: serde_json::Value =
        serde_json::from_slice(&encode_for(4, &response).unwrap()).unwrap();
    assert!(
        four["outcome"]["briefing"]
            .get("interface_notice_ids")
            .is_none(),
        "{four:#}"
    );
    assert!(
        four["outcome"]["briefing"].get("trail").is_some(),
        "{four:#}"
    );
    let current: Response =
        serde_json::from_slice(&encode_for(PROTOCOL_VERSION, &response).unwrap()).unwrap();
    assert!(
        matches!(&current.outcome, Outcome::Briefing { briefing: read } if read.trail == Some(trail) && read.interface_notice_ids == ["n1"])
    );
}

/// Issue #262: a client of version 7 or earlier reads briefings with
/// `deny_unknown_fields`, so its answer does not say the index was partial;
/// a client of version 8 or later is told.
#[test]
fn a_version_7_reply_leaves_out_a_partial_index() {
    use langspan_core::{Briefing, Outcome};
    let mut briefing: Briefing = serde_json::from_value(serde_json::json!({
        "session_id": "codex.bob", "changed": true, "peers": [], "omitted_peers": 0,
    }))
    .unwrap();
    briefing.inflight_partial = true;
    let response = Response {
        version: PROTOCOL_VERSION,
        request_id: "r".into(),
        replayed: false,
        generation: 1,
        lease_valid: None,
        outcome: Outcome::Briefing { briefing },
    };
    for version in std::iter::once(PROTOCOL_VERSION_7).chain(EARLIER_VERSIONS) {
        let earlier: serde_json::Value =
            serde_json::from_slice(&encode_for(version, &response).unwrap()).unwrap();
        assert!(
            earlier["outcome"]["briefing"]
                .get("inflight_partial")
                .is_none(),
            "{version}: {earlier:#}"
        );
    }
    for version in [PROTOCOL_VERSION_8, PROTOCOL_VERSION_9, PROTOCOL_VERSION] {
        let current: Response =
            serde_json::from_slice(&encode_for(version, &response).unwrap()).unwrap();
        assert!(
            matches!(&current.outcome, Outcome::Briefing { briefing: read } if read.inflight_partial),
            "version {version}"
        );
    }
}

/// Roadmap D7 (review comment 4707749784): to a version 3 client an
/// interface notice has no rename fields and no change of a definition that
/// is not exported, while a version 4 client gets both.
#[test]
fn a_version_3_reply_leaves_out_rename_fields_and_private_changes() {
    use langspan_core::{InterfaceChange, InterfaceNotice, Outcome};
    let renamed = InterfaceChange {
        name: "fetch_user".into(),
        kind: "function".into(),
        change: "renamed".into(),
        before: Some("def fetch_user(id)".into()),
        after: Some("def load_user(id)".into()),
        after_name: Some("load_user".into()),
        before_name: Some("fetch_user".into()),
        evidence: Some("same_body".into()),
        before_path: None,
        private: false,
        before_arity: None,
        after_arity: None,
        before_count: None,
        after_count: None,
    };
    let private = InterfaceChange {
        name: "helper".into(),
        before: Some("def helper(x)".into()),
        after: Some("def helper(x)".into()),
        after_name: Some("helper".into()),
        before_name: Some("helper".into()),
        evidence: Some("file_moved".into()),
        before_path: Some("app/old.py".into()),
        private: true,
        ..renamed.clone()
    };
    let notice = InterfaceNotice {
        notice_id: "n1".into(),
        workspace_id: "work".into(),
        path: "app/users.py".into(),
        changes: vec![renamed, private],
        dependents: Vec::new(),
        elsewhere: Vec::new(),
        by_session: None,
        at_ms: 1,
        generation: 1,
    };
    let response = Response {
        version: OLDEST_PROTOCOL_VERSION,
        request_id: "r".into(),
        replayed: false,
        generation: 1,
        lease_valid: None,
        outcome: Outcome::InterfaceObserved {
            notice: notice.clone(),
        },
    };
    let earlier: serde_json::Value =
        serde_json::from_slice(&encode_for(OLDEST_PROTOCOL_VERSION, &response).unwrap()).unwrap();
    assert_eq!(
        earlier["outcome"]["notice"]["changes"],
        serde_json::json!([{"name": "fetch_user", "kind": "function", "change": "renamed",
            "before": "def fetch_user(id)", "after": "def load_user(id)", "after_name": "load_user"}]),
        "{earlier:#}"
    );
    let current: Response =
        serde_json::from_slice(&encode_for(PROTOCOL_VERSION, &response).unwrap()).unwrap();
    assert!(
        matches!(&current.outcome, Outcome::InterfaceObserved { notice: read } if read == &notice)
    );
}

/// Roadmap D7 with C7: an in-flight rename's old name goes to a version 4
/// client only; a version 3 client reads the in-flight symbol as before.
#[test]
fn a_version_3_reply_leaves_out_an_inflight_renames_old_name() {
    use langspan_core::{InflightNotice, Outcome};
    let inflight = InflightNotice {
        path: "src/config.rs".into(),
        worktree: "/work/tree".into(),
        session: Some("claude-code.alice".into()),
        name: "load_config".into(),
        kind: "function".into(),
        signature: "pub fn load_config(text: &str) -> usize".into(),
        mentioned_in: vec!["src/main.rs".into()],
        renamed_from: Some("parse_config".into()),
    };
    let response = Response {
        version: OLDEST_PROTOCOL_VERSION,
        request_id: "r".into(),
        replayed: false,
        generation: 1,
        lease_valid: None,
        outcome: Outcome::GraphInflight {
            inflight: vec![inflight.clone()],
            coverage: serde_json::from_value(serde_json::json!({"indexable_files": 3,
                "current_files": 3, "partial_files": 0, "scanned": true}))
            .unwrap(),
            truncated: false,
        },
    };
    let earlier: serde_json::Value =
        serde_json::from_slice(&encode_for(OLDEST_PROTOCOL_VERSION, &response).unwrap()).unwrap();
    let shown = &earlier["outcome"]["inflight"][0];
    assert!(shown.get("renamed_from").is_none(), "{earlier:#}");
    assert_eq!(shown["name"], "load_config", "{earlier:#}");
    let current: Response =
        serde_json::from_slice(&encode_for(PROTOCOL_VERSION, &response).unwrap()).unwrap();
    assert!(
        matches!(&current.outcome, Outcome::GraphInflight { inflight: read, .. } if read == &[inflight])
    );
}

/// What issue #222 added in version 7, as field and event names.
const ADDED_IN_7: [&str; 6] = [
    "told",
    "told_at_ms",
    "context",
    "unchanged",
    "context_added",
    "told_forgotten",
];
/// Every version the daemon answers before version 7. Version 8 (issue
/// #262) and version 10 (roadmap A11) keep what version 7 added.
const EARLIER_VERSIONS: [u16; 4] = [
    PROTOCOL_VERSION_6,
    PROTOCOL_VERSION_5,
    PROTOCOL_VERSION_4,
    OLDEST_PROTOCOL_VERSION,
];

/// Review comment 4712860167: a state record too large for a page is given
/// as parts of its JSON, which `encode_for` cannot reshape. The store pages
/// the text `encode_record_for` gives, which for an earlier version is
/// already in that version's shape: activity without what version 7 added,
/// and an interface notice without version 4's `renamed_from` for version
/// 3. A version 7 record is its own JSON.
#[test]
fn a_state_record_is_encoded_in_each_versions_shape_for_its_parts() {
    use langspan_core::{ContextUse, SessionActivity, StatusRecord};
    let activity = StatusRecord::Activity {
        key: "codex.bob".into(),
        activity: SessionActivity {
            told: vec![7],
            told_at_ms: 1_000,
            context: ContextUse {
                turns: 1,
                bytes: 10,
                largest_turn_bytes: 10,
                turn_bytes: 10,
            },
            ..SessionActivity::default()
        },
    };
    assert_eq!(
        encode_record_for(PROTOCOL_VERSION_7, &activity).unwrap(),
        serde_json::to_string(&activity).unwrap()
    );
    assert_eq!(
        encode_record_for(PROTOCOL_VERSION, &activity).unwrap(),
        serde_json::to_string(&activity).unwrap()
    );
    for version in EARLIER_VERSIONS {
        let text = encode_record_for(version, &activity).unwrap();
        for field in ADDED_IN_7 {
            assert!(!text.contains(&format!("\"{field}\"")), "{version}: {text}");
        }
        // Still the same record otherwise.
        let read: StatusRecord = serde_json::from_str(&text).unwrap();
        assert!(matches!(read, StatusRecord::Activity { key, .. } if key == "codex.bob"));
    }
    // A version 3 reader also loses version 4's fields in a record's parts.
    let notice: StatusRecord = serde_json::from_value(serde_json::json!({
        "kind": "interface_notice", "key": "app",
        "notice": {
            "notice_id": "n1", "workspace_id": "app", "path": "a.py",
            "changes": [{
                "name": "renamed", "kind": "function", "change": "renamed",
                "before": "def original()", "after": "def renamed()",
                "before_name": "original", "evidence": "same_body",
            }],
            "dependents": [], "by_session": null, "at_ms": 1, "generation": 1,
        },
    }))
    .unwrap_or_else(|error| panic!("an interface notice: {error}"));
    let current = encode_record_for(PROTOCOL_VERSION, &notice).unwrap();
    assert!(current.contains("before_name"), "{current}");
    let oldest = encode_record_for(OLDEST_PROTOCOL_VERSION, &notice).unwrap();
    assert!(!oldest.contains("before_name"), "{oldest}");
    assert!(!oldest.contains("evidence"), "{oldest}");
}

/// Issue #222 (review comments 4711375982 and 4712860167): version 7 added
/// what a session was told (`told`, `told_at_ms`), the context its hooks
/// added (`context`), a briefing's `unchanged` count, the told keys of a
/// briefing delivery and the `context_added` and `told_forgotten` events.
/// A client of version 6 (roadmap F5's build), 5 (roadmap H9's), 4 or 3
/// reads activity, briefings and events with `deny_unknown_fields`: its
/// reply leaves every one of them out, in every outcome that carries them,
/// while a version 7 client reads them all.
#[test]
fn an_earlier_reply_leaves_out_what_version_7_added_to_activity_briefings_and_events() {
    use langspan_core::{
        ContextUse, Event, Outcome, RecordedEvent, SessionActivity, State, StatusRecord,
    };
    let activity = SessionActivity {
        told: vec![7, 9],
        told_at_ms: 1_000,
        context: ContextUse {
            turns: 2,
            bytes: 300,
            largest_turn_bytes: 200,
            turn_bytes: 100,
        },
        ..SessionActivity::default()
    };
    let mut state = State::default();
    state.activity.insert("codex.bob".into(), activity.clone());
    let wire = include_bytes!("../fixtures/briefing-response-v3.json");
    let Outcome::Briefing { mut briefing } =
        serde_json::from_slice::<Response>(wire).unwrap().outcome
    else {
        panic!("a briefing fixture");
    };
    briefing.unchanged = 3;
    let events = vec![
        RecordedEvent {
            seq: 1,
            event: Event::BriefingDelivered {
                session_id: "codex.bob".into(),
                signature: "sig".into(),
                told: vec![7, 9],
                told_at_ms: Some(1_000),
            },
        },
        RecordedEvent {
            seq: 2,
            event: Event::ContextAdded {
                session_id: "codex.bob".into(),
                bytes: 120,
                turn: true,
                generation: 1,
            },
        },
        RecordedEvent {
            seq: 3,
            event: Event::ToldForgotten {
                session_id: "codex.bob".into(),
                keys: vec![9],
            },
        },
    ];
    let outcomes = [
        Outcome::Status {
            state,
            summary: None,
        },
        Outcome::StatusRecords {
            records: vec![StatusRecord::Activity {
                key: "codex.bob".into(),
                activity,
            }],
            next: None,
        },
        Outcome::Briefing { briefing },
        Outcome::Events {
            events: events.clone(),
        },
        Outcome::Changes { events, scan: None },
    ];
    for outcome in outcomes {
        let response = Response {
            version: PROTOCOL_VERSION,
            request_id: "r".into(),
            replayed: false,
            generation: 1,
            lease_valid: None,
            outcome,
        };
        let current = String::from_utf8(encode_for(PROTOCOL_VERSION, &response).unwrap()).unwrap();
        assert!(
            ADDED_IN_7
                .iter()
                .any(|field| current.contains(&format!("\"{field}\""))),
            "a version 7 client reads them: {current}"
        );
        assert_eq!(
            serde_json::from_str::<Response>(&current).unwrap(),
            response
        );
        // Version 8 (issue #262) changed only a partial index, version 9
        // (roadmap E1) only notes and version 10 (roadmap A11) only outline
        // reads: version 7, 8 and 9 clients read the same answer.
        for version in [PROTOCOL_VERSION_7, PROTOCOL_VERSION_8, PROTOCOL_VERSION_9] {
            assert_eq!(
                String::from_utf8(encode_for(version, &response).unwrap()).unwrap(),
                current,
                "version {version}"
            );
        }
        for version in EARLIER_VERSIONS {
            let earlier = String::from_utf8(encode_for(version, &response).unwrap()).unwrap();
            for field in ADDED_IN_7 {
                assert!(
                    !earlier.contains(&format!("\"{field}\"")),
                    "version {version} has {field}: {earlier}"
                );
            }
            // What is left still reads as a response.
            serde_json::from_str::<Response>(&earlier).unwrap();
        }
    }
}

/// Review comment 4714078108: version 10 (roadmap A11) added the file
/// versions a session was given an outline of (`outlined`) and the
/// `read_outlined` event. A client of version 8 or earlier reads activity
/// and events with `deny_unknown_fields`: in every outcome and state record
/// that carries them, its reply leaves both out, and what is left reads in
/// that version's own shape. A version 10 client reads them.
#[test]
fn an_earlier_reply_leaves_out_what_version_10_added_to_activity_and_events() {
    use langspan_core::{
        Event, Outcome, OutlinedRead, RecordedEvent, SessionActivity, State, StatusRecord,
    };
    let activity = SessionActivity {
        outlined: vec![OutlinedRead {
            path: "big.rs".into(),
            digest: "a".repeat(64),
        }],
        ..SessionActivity::default()
    };
    let mut state = State::default();
    state.activity.insert("codex.bob".into(), activity.clone());
    let outlined = RecordedEvent {
        seq: 1,
        event: Event::ReadOutlined {
            session_id: "codex.bob".into(),
            path: "big.rs".into(),
            digest: "a".repeat(64),
        },
    };
    let kept = RecordedEvent {
        seq: 2,
        event: Event::ToldForgotten {
            session_id: "codex.bob".into(),
            keys: vec![9],
        },
    };
    let events = vec![outlined, kept.clone()];
    let outcomes = [
        Outcome::Status {
            state,
            summary: None,
        },
        Outcome::StatusRecords {
            records: vec![StatusRecord::Activity {
                key: "codex.bob".into(),
                activity: activity.clone(),
            }],
            next: None,
        },
        Outcome::Events {
            events: events.clone(),
        },
        Outcome::Changes { events, scan: None },
    ];
    for outcome in outcomes {
        let response = Response {
            version: PROTOCOL_VERSION,
            request_id: "r".into(),
            replayed: false,
            generation: 1,
            lease_valid: None,
            outcome,
        };
        let current = String::from_utf8(encode_for(PROTOCOL_VERSION, &response).unwrap()).unwrap();
        assert!(
            current.contains("\"outlined\"") || current.contains("\"read_outlined\""),
            "a version 10 client reads them: {current}"
        );
        assert_eq!(
            serde_json::from_str::<Response>(&current).unwrap(),
            response
        );
        for version in [PROTOCOL_VERSION_9, PROTOCOL_VERSION_8, PROTOCOL_VERSION_7]
            .into_iter()
            .chain(EARLIER_VERSIONS)
        {
            let earlier = String::from_utf8(encode_for(version, &response).unwrap()).unwrap();
            assert!(!earlier.contains("\"outlined\""), "{version}: {earlier}");
            assert!(!earlier.contains("read_outlined"), "{version}: {earlier}");
            let read = serde_json::from_str::<Response>(&earlier).unwrap();
            // Versions 9, 8 and 7 keep what version 7 added; earlier
            // versions lose it too.
            if let Outcome::Events { events } | Outcome::Changes { events, .. } = read.outcome {
                let expected = if version >= PROTOCOL_VERSION_7 {
                    vec![kept.clone()]
                } else {
                    Vec::new()
                };
                assert_eq!(events, expected, "{version}");
            }
        }
    }
    // A state record given in parts, in each earlier version's shape.
    let record = StatusRecord::Activity {
        key: "codex.bob".into(),
        activity,
    };
    assert!(
        encode_record_for(PROTOCOL_VERSION, &record)
            .unwrap()
            .contains("\"outlined\"")
    );
    for version in [PROTOCOL_VERSION_9, PROTOCOL_VERSION_8, PROTOCOL_VERSION_7]
        .into_iter()
        .chain(EARLIER_VERSIONS)
    {
        let text = encode_record_for(version, &record).unwrap();
        assert!(!text.contains("\"outlined\""), "{version}: {text}");
    }
}

/// Roadmap A11's golden vectors: the hook's `outline_read` request, the
/// `read_outline` answer, and an event page with `read_outlined`, which a
/// version 9 or 8 client receives without that event.
#[test]
fn v10_outline_request_answer_and_events_match_the_committed_golden_vectors() {
    use langspan_core::{Event, Outcome};
    let wire = include_bytes!("../fixtures/outline-read-request-v10.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, PROTOCOL_VERSION);
    assert_eq!(
        request.command,
        Command::OutlineRead {
            session_id: "claude-code.alice".into(),
            path: "src/big.rs".into(),
            digest: "a".repeat(64),
            lines: 2400,
            bytes: 96000,
        }
    );
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_bytes!("../fixtures/read-outline-response-v10.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    let Outcome::ReadOutline {
        outline: Some(outline),
        definitions: 2,
        skipped: None,
    } = &response.outcome
    else {
        panic!("{response:?}");
    };
    assert_eq!((outline[0].line, outline[0].end_line), (12, 80));
    assert!(outline[0].exported && !outline[1].exported);
    assert_eq!(encode(&response).unwrap(), wire);

    let wire = include_bytes!("../fixtures/events-response-v10.json");
    let mut response: Response = serde_json::from_slice(wire).unwrap();
    let Outcome::Events { events } = &response.outcome else {
        panic!("{response:?}");
    };
    assert!(matches!(events[0].event, Event::ReadOutlined { .. }));
    assert_eq!(encode(&response).unwrap(), wire);
    // Version 9 (roadmap E1) and version 8 have no `read_outlined`: theirs
    // is the page without it.
    for (version, earlier) in [
        (
            PROTOCOL_VERSION_9,
            &include_bytes!("../fixtures/events-response-v9.json")[..],
        ),
        (
            PROTOCOL_VERSION_8,
            &include_bytes!("../fixtures/events-response-v8.json")[..],
        ),
    ] {
        response.version = version;
        assert_eq!(
            encode_for(version, &response).unwrap(),
            earlier,
            "version {version}"
        );
    }
}

/// Version 9 is roadmap E1's, before roadmap A11's outline reads: the
/// daemon answers every version from the oldest to its own, 9 included.
#[test]
fn the_daemon_answers_every_version_from_the_oldest_to_its_own() {
    use langspan::protocol::answers;
    for version in OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION {
        assert!(answers(version), "version {version}");
    }
    assert!(!answers(OLDEST_PROTOCOL_VERSION - 1));
    assert!(!answers(PROTOCOL_VERSION + 1));
}

/// A landing as a build before roadmap F5 reads it: no `rebased` and no
/// `adapted`, and every other field.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct EarlierLanding {
    landing_id: String,
    workspace_id: String,
    onto: String,
    commit_oid: String,
    tree_oid: String,
    inputs: Vec<String>,
    check_id: String,
}

fn rebased_landing() -> langspan_core::Landing {
    langspan_core::Landing {
        landing_id: "land-1".into(),
        workspace_id: "app".into(),
        onto: "main".into(),
        commit_oid: "c".repeat(40),
        tree_oid: "d".repeat(40),
        inputs: vec!["e".repeat(40)],
        check_id: "check-1".into(),
        rebased: vec![langspan_core::Rewritten {
            from: "e".repeat(40),
            to: "c".repeat(40),
        }],
        adapted: vec!["f".repeat(40)],
    }
}

/// Version 6 (roadmap F5, review comment 4712442596): a landing carries
/// `rebased` and `adapted`. To a client of version 5, 4 or 3 the same
/// answer is encoded without them, so an earlier build, which reads a
/// landing with `deny_unknown_fields`, still reads the whole answer.
/// Version 7 (issue #222) added nothing to a landing: 6 and 7 agree.
#[test]
fn v6_landed_response_and_its_earlier_forms_match_the_golden_vectors() {
    use langspan_core::Outcome;
    let wire = include_bytes!("../fixtures/landed-response-v6.json");
    let mut response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(response.version, PROTOCOL_VERSION_6);
    assert_eq!(
        response.outcome,
        Outcome::Landed {
            landing: rebased_landing()
        }
    );
    assert_eq!(encode_for(PROTOCOL_VERSION_6, &response).unwrap(), wire);
    let mut current = response.clone();
    current.version = PROTOCOL_VERSION;
    assert_eq!(
        String::from_utf8(encode_for(PROTOCOL_VERSION, &current).unwrap()).unwrap(),
        String::from_utf8_lossy(wire).replacen(
            r#""version":6"#,
            &format!(r#""version":{PROTOCOL_VERSION}"#),
            1
        )
    );
    let earlier_reads = |wire: &[u8]| {
        let value: serde_json::Value = serde_json::from_slice(wire).unwrap();
        serde_json::from_value::<EarlierLanding>(value["outcome"]["landing"].clone()).is_ok()
    };
    assert!(!earlier_reads(wire), "an earlier build cannot read it");

    let earlier = include_bytes!("../fixtures/landed-response-v5.json");
    for version in [
        PROTOCOL_VERSION_5,
        PROTOCOL_VERSION_4,
        OLDEST_PROTOCOL_VERSION,
    ] {
        response.version = version;
        let reply = String::from_utf8(encode_for(version, &response).unwrap()).unwrap();
        let expected = String::from_utf8_lossy(earlier).replacen(
            "\"version\":5",
            &format!("\"version\":{version}"),
            1,
        );
        assert_eq!(reply, expected, "version {version}");
        assert!(earlier_reads(reply.as_bytes()), "version {version}");
    }
    // The earlier form is only for that reply: the response is unchanged.
    assert!(
        String::from_utf8(encode(&response).unwrap())
            .unwrap()
            .contains("rebased")
    );
}

/// A request to record a rebased or adapted landing needs version 6 or
/// later: an earlier daemon refuses its fields as unreadable, and that is
/// too late once the push is done. A landing that rewrote nothing fits
/// every version.
#[test]
fn recording_a_rebased_or_adapted_landing_fits_only_version_6_and_later() {
    use langspan::protocol::fits;
    let wire = include_bytes!("../fixtures/record-landing-request-v6.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, PROTOCOL_VERSION_6);
    assert_eq!(encode(&request).unwrap(), wire);
    let with = |rebased: bool, adapted: bool| {
        let mut command = request.command.clone();
        let Command::RecordLanding {
            rebased: r,
            adapted: a,
            ..
        } = &mut command
        else {
            panic!("{request:?}");
        };
        assert_eq!((r.len(), a.len()), (1, 1));
        if !rebased {
            r.clear();
        }
        if !adapted {
            a.clear();
        }
        command
    };
    for (rebased, adapted) in [(true, true), (true, false), (false, true)] {
        let command = with(rebased, adapted);
        assert!(fits(PROTOCOL_VERSION, &command));
        assert!(fits(PROTOCOL_VERSION_6, &command));
        for version in [
            PROTOCOL_VERSION_5,
            PROTOCOL_VERSION_4,
            OLDEST_PROTOCOL_VERSION,
        ] {
            assert!(
                !fits(version, &command),
                "version {version}, rebased {rebased}, adapted {adapted}"
            );
        }
    }
    for version in std::iter::once(PROTOCOL_VERSION).chain(EARLIER_VERSIONS) {
        assert!(fits(version, &with(false, false)), "version {version}");
    }
}

/// Review follow-up #259: a briefing of changes only (issue #222) is new
/// in version 7. A request to a daemon of version 7 or later keeps asking for
/// changes only; one to a daemon of version 6 or earlier, which refuses
/// the field, asks for the full view and is otherwise the same. Other
/// requests are left as they are.
#[test]
fn a_briefing_of_changes_only_is_asked_of_version_7_and_later() {
    let for_version = |version: u16, command: &mut Command| {
        for_capabilities(Capabilities::negotiate(version, None), command);
    };
    let wire = include_bytes!("../fixtures/brief-changes-only-request-v7.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, PROTOCOL_VERSION_7);
    assert_eq!(encode(&request).unwrap(), wire);
    let with_changes_only = |value: bool| {
        let mut command = request.command.clone();
        let Command::Brief { changes_only, .. } = &mut command else {
            panic!("{request:?}");
        };
        assert!(*changes_only);
        *changes_only = value;
        command
    };
    let changes_only_versions = [
        PROTOCOL_VERSION_10,
        PROTOCOL_VERSION_9,
        PROTOCOL_VERSION_8,
        PROTOCOL_VERSION_7,
    ];
    assert_eq!(PROTOCOL_VERSION, PROTOCOL_VERSION_10, "add the new version");
    for version in changes_only_versions {
        let mut command = request.command.clone();
        for_version(version, &mut command);
        assert_eq!(command, with_changes_only(true), "version {version}");
    }
    for version in EARLIER_VERSIONS {
        let mut command = request.command.clone();
        for_version(version, &mut command);
        assert_eq!(command, with_changes_only(false), "version {version}");
    }
    for version in changes_only_versions.into_iter().chain(EARLIER_VERSIONS) {
        let mut command = Command::Dashboard;
        for_version(version, &mut command);
        assert_eq!(command, Command::Dashboard, "version {version}");
    }
}

/// Version 9 (roadmap E1): a note carries `anchor`, `anchor_state` and
/// `tags`. A client of version 8 or earlier reads a note with
/// `deny_unknown_fields` and would show an anchored note without its state,
/// so it is told an anchored note only while its definition is current, as
/// a plain note; a changed, gone or unknown one is left out.
#[test]
fn anchored_notes_reach_an_earlier_client_only_while_current_and_without_the_new_fields() {
    use langspan_core::{AnchorState, Note, NoteAnchor, Outcome, Recap};
    let note = |id: &str, state: Option<AnchorState>| Note {
        note_id: id.into(),
        workspace_id: "app".into(),
        author: None,
        text: "keep it pure".into(),
        paths: vec!["src/api.rs".into()],
        anchor: state.map(|_| NoteAnchor {
            name: "parse_upload".into(),
            path: Some("src/api.rs".into()),
            kind: Some("function".into()),
            line: Some(1),
            body_digest: Some("a".repeat(64)),
        }),
        anchor_state: state,
        tags: if state.is_some() {
            vec!["api".into()]
        } else {
            Vec::new()
        },
    };
    let recap = Recap {
        workspace_id: "app".into(),
        root: "/repo".into(),
        branch_ref: None,
        sessions: Vec::new(),
        omitted_sessions: 0,
        omitted_items: 0,
        interface_changes: Vec::new(),
        checks: Vec::new(),
        landings: Vec::new(),
        notes: vec![
            note("plain", None),
            note("current", Some(AnchorState::Current)),
            note("changed", Some(AnchorState::Changed)),
            note("gone", Some(AnchorState::Gone)),
            note("unknown", Some(AnchorState::Unknown)),
        ],
        messages: Vec::new(),
        trails: Vec::new(),
        teammates: Vec::new(),
        publishing: None,
        scan_failure: None,
        scan_coverage: None,
        since: None,
    };
    let response = Response {
        version: PROTOCOL_VERSION,
        request_id: "r".into(),
        replayed: false,
        generation: 1,
        lease_valid: None,
        outcome: Outcome::Recap { recap },
    };
    // Version 9 introduced them and version 10 (roadmap A11) keeps them.
    for version in [PROTOCOL_VERSION_9, PROTOCOL_VERSION] {
        let now: serde_json::Value =
            serde_json::from_slice(&encode_for(version, &response).unwrap()).unwrap();
        assert_eq!(
            now["outcome"]["recap"]["notes"].as_array().unwrap().len(),
            5,
            "version {version}"
        );
        assert_eq!(
            now["outcome"]["recap"]["notes"][2]["anchor_state"], "changed",
            "version {version}"
        );
    }
    for version in [PROTOCOL_VERSION_8, PROTOCOL_VERSION_7]
        .into_iter()
        .chain(EARLIER_VERSIONS)
    {
        let earlier: serde_json::Value =
            serde_json::from_slice(&encode_for(version, &response).unwrap()).unwrap();
        let notes = earlier["outcome"]["recap"]["notes"].as_array().unwrap();
        let ids: Vec<&str> = notes
            .iter()
            .map(|held| held["note_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["plain", "current"], "version {version}");
        for held in notes {
            let fields: Vec<&String> = held.as_object().unwrap().keys().collect();
            assert_eq!(
                fields,
                ["note_id", "workspace_id", "author", "text", "paths"],
                "version {version}"
            );
        }
    }
}

/// A note with a symbol or tags, and a recap by tag, need version 9 or
/// later; the same requests without them fit every version.
#[test]
fn a_note_with_a_symbol_or_tags_fits_only_version_9_and_later() {
    use langspan::protocol::fits;
    let remember = |symbol: Option<&str>, tags: &[&str]| Command::Remember {
        workspace_id: "app".into(),
        author: None,
        text: "keep it pure".into(),
        paths: Vec::new(),
        symbol: symbol.map(str::to_owned),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
    };
    let recap = |tag: Option<&str>| Command::Recap {
        workspace_id: "app".into(),
        tag: tag.map(str::to_owned),
        since: None,
    };
    for command in [
        remember(Some("parse_upload"), &[]),
        remember(None, &["api"]),
        recap(Some("api")),
    ] {
        assert!(fits(PROTOCOL_VERSION, &command));
        assert!(fits(PROTOCOL_VERSION_9, &command));
        for version in [PROTOCOL_VERSION_8, PROTOCOL_VERSION_7]
            .into_iter()
            .chain(EARLIER_VERSIONS)
        {
            assert!(!fits(version, &command), "version {version}: {command:?}");
        }
    }
    for version in [
        PROTOCOL_VERSION,
        PROTOCOL_VERSION_9,
        PROTOCOL_VERSION_8,
        PROTOCOL_VERSION_7,
    ]
    .into_iter()
    .chain(EARLIER_VERSIONS)
    {
        assert!(fits(version, &remember(None, &[])));
        assert!(fits(version, &recap(None)));
    }
}

/// Golden vectors for version 9 (roadmap E1): a note with a symbol and
/// tags, and a recap by tag, read and encode as the fixtures, and only a
/// version 9 or later daemon is sent them.
#[test]
fn v9_note_requests_match_the_golden_vectors() {
    use langspan::protocol::fits;
    for wire in [
        &include_bytes!("../fixtures/remember-request-v9.json")[..],
        &include_bytes!("../fixtures/recap-tag-request-v9.json")[..],
    ] {
        let request: Request = serde_json::from_slice(wire).unwrap();
        assert_eq!(request.version, PROTOCOL_VERSION_9);
        assert_eq!(encode(&request).unwrap(), wire);
        assert!(fits(PROTOCOL_VERSION_9, &request.command));
        assert!(fits(PROTOCOL_VERSION, &request.command));
        assert!(!fits(PROTOCOL_VERSION_8, &request.command));
    }
    let remember: Request =
        serde_json::from_slice(include_bytes!("../fixtures/remember-request-v9.json")).unwrap();
    assert!(matches!(
        &remember.command,
        Command::Remember { symbol: Some(name), tags, .. } if name == "parse_upload" && tags == &["api"]
    ));
}

/// A version 9 recall answer and its version 8 form: an earlier client is
/// told an anchored note only while its definition is current, as a plain
/// note, and a version 8 client keeps everything else.
#[test]
fn v9_recall_response_and_its_version_8_form_match_the_golden_vectors() {
    use langspan_core::{AnchorState, Outcome};
    let wire = include_bytes!("../fixtures/recall-response-v9.json");
    let mut response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(response.version, PROTOCOL_VERSION_9);
    let Outcome::Recall { recall } = &response.outcome else {
        panic!("{response:?}");
    };
    let states: Vec<Option<AnchorState>> =
        recall.notes.iter().map(|note| note.anchor_state).collect();
    assert_eq!(
        states,
        [None, Some(AnchorState::Current), Some(AnchorState::Changed)]
    );
    assert_eq!(encode_for(PROTOCOL_VERSION_9, &response).unwrap(), wire);

    let earlier = include_bytes!("../fixtures/recall-response-v8.json");
    response.version = PROTOCOL_VERSION_8;
    assert_eq!(
        String::from_utf8(encode_for(PROTOCOL_VERSION_8, &response).unwrap()).unwrap(),
        String::from_utf8_lossy(earlier)
    );
    let read: Response = serde_json::from_slice(earlier).unwrap();
    assert_eq!(read.version, PROTOCOL_VERSION_8);
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_string()).collect()
}

/// ADR 0037: a request of version 3 to 10 that names no capabilities is
/// granted its version's, so each of those clients keeps its exact shape.
#[test]
fn versions_3_to_10_map_to_their_capability_sets() {
    let four: &[&str] = &["renames", "trails"];
    let five = [four, &["interface_notices", "refusal_outcomes"]].concat();
    let six = [&five[..], &["landing_rewrites", "symbol_findings"]].concat();
    let seven = [&six[..], &["briefing_memory"]].concat();
    let eight = [&seven[..], &["inflight_partial"]].concat();
    let nine = [&eight[..], &["anchored_notes"]].concat();
    let ten = [&nine[..], &["outline_reads"]].concat();
    for (version, set) in [
        (OLDEST_PROTOCOL_VERSION, &[][..]),
        (PROTOCOL_VERSION_4, four),
        (PROTOCOL_VERSION_5, &five),
        (PROTOCOL_VERSION_6, &six),
        (PROTOCOL_VERSION_7, &seven),
        (PROTOCOL_VERSION_8, &eight),
        (PROTOCOL_VERSION_9, &nine),
        (PROTOCOL_VERSION_10, &ten),
    ] {
        let mut expected = names(set);
        expected.sort();
        assert_eq!(
            Capabilities::negotiate(version, None).names(),
            expected,
            "{version}"
        );
    }
    // A capability added after version 10 (`None` in the table) reaches
    // only a client that names it.
    let all = Capabilities::ALL.names();
    assert_eq!(
        Capabilities::negotiate(PROTOCOL_VERSION, Some(&all)),
        Capabilities::ALL
    );
    assert!(!Capabilities::negotiate(PROTOCOL_VERSION, None).has("recap_since"));
}

/// Roadmap C10: a recap's `since` reaches only a client with the
/// `recap_since` capability, which no frozen version has, and a recap
/// asked for `since` is sent only to a daemon that grants it.
#[test]
fn a_recap_since_reaches_only_a_client_with_the_recap_since_capability() {
    let with = include_bytes!("../fixtures/capabilities/recap_since/with.json");
    let without = include_bytes!("../fixtures/capabilities/recap_since/without.json");
    let response: Response = serde_json::from_slice(with).unwrap();
    let all = Capabilities::ALL.names();
    assert_eq!(
        encode_with(Capabilities::ALL, Some(&all), &response).unwrap(),
        with
    );
    let others: Vec<String> = all
        .iter()
        .filter(|name| *name != "recap_since")
        .cloned()
        .collect();
    let granted = Capabilities::negotiate(PROTOCOL_VERSION, Some(&others));
    assert_eq!(
        String::from_utf8(encode_with(granted, Some(&others), &response).unwrap()).unwrap(),
        String::from_utf8_lossy(without)
    );
    for version in OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION {
        let text = String::from_utf8(encode_for(version, &response).unwrap()).unwrap();
        assert!(!text.contains("\"since\""), "{version}: {text}");
    }

    let wire = include_bytes!("../fixtures/capabilities/recap_since/request.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(encode(&request).unwrap(), wire);
    assert!(matches!(
        request.command,
        Command::Recap {
            since: Some(langspan_core::SinceFrom::Away {
                after_ms: 7_200_000
            }),
            ..
        }
    ));
    assert!(fits_with(Capabilities::ALL, &request.command));
    assert!(!fits_with(granted, &request.command));
    for version in OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION {
        let granted = Capabilities::negotiate(version, None);
        assert!(!fits_with(granted, &request.command), "{version}");
    }
    let mut plain = request.command.clone();
    let Command::Recap { since, .. } = &mut plain else {
        unreachable!()
    };
    *since = None;
    assert!(fits_with(granted, &plain));
}

/// Issue #213: the peers' uncommitted callers of a renamed name reach only
/// a client with `rename_holds`, which no frozen version has. A daemon
/// without it is asked no `renamed`, so it never holds a write, and a
/// check of renamed names alone is not sent to it.
#[test]
fn peer_callers_reach_only_a_client_with_the_rename_holds_capability() {
    let with = include_bytes!("../fixtures/capabilities/rename_holds/with.json");
    let without = include_bytes!("../fixtures/capabilities/rename_holds/without.json");
    let response: Response = serde_json::from_slice(with).unwrap();
    let all = Capabilities::ALL.names();
    assert_eq!(
        encode_with(Capabilities::ALL, Some(&all), &response).unwrap(),
        with
    );
    let others: Vec<String> = all
        .iter()
        .filter(|name| *name != "rename_holds")
        .cloned()
        .collect();
    let granted = Capabilities::negotiate(PROTOCOL_VERSION, Some(&others));
    assert_eq!(
        String::from_utf8(encode_with(granted, Some(&others), &response).unwrap()).unwrap(),
        String::from_utf8_lossy(without)
    );
    for version in OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION {
        let text = String::from_utf8(encode_for(version, &response).unwrap()).unwrap();
        assert!(!text.contains("\"callers\""), "{version}: {text}");
    }

    let wire = include_bytes!("../fixtures/capabilities/rename_holds/request.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(encode(&request).unwrap(), wire);
    let Command::CheckCalls { renamed, .. } = &request.command else {
        panic!("{request:?}");
    };
    assert_eq!(renamed, &["parse_config".to_owned()]);
    assert!(fits_with(Capabilities::ALL, &request.command));
    assert!(!fits_with(granted, &request.command));
    // With calls to check, the request goes without its names.
    let mut with_calls = request.command.clone();
    let Command::CheckCalls { calls, .. } = &mut with_calls else {
        unreachable!()
    };
    calls.push(langspan_core::CallSite {
        path: "src/api.rs".into(),
        line: 1,
        name: "parse".into(),
        args: 1,
        member: true,
    });
    assert!(fits_with(granted, &with_calls));
    let mut sent = with_calls.clone();
    for_capabilities(Capabilities::ALL, &mut sent);
    assert_eq!(sent, with_calls);
    for_capabilities(granted, &mut sent);
    let Command::CheckCalls { renamed, .. } = &sent else {
        unreachable!()
    };
    assert!(renamed.is_empty());
    // Earlier check_calls requests keep their bytes.
    let earlier = include_bytes!("../fixtures/check-calls-command-v10.json");
    let request: Request = serde_json::from_slice(earlier).unwrap();
    assert_eq!(encode(&request).unwrap(), earlier);
}

/// The handshake: the daemon grants the names it knows and, when that is
/// fewer than the request named, says which. A client and daemon of the
/// same build exchange the bytes they did before capabilities.
#[test]
fn the_daemon_grants_the_capabilities_it_knows_and_names_them_when_fewer() {
    let wire = include_bytes!("../fixtures/capabilities/request.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(encode(&request).unwrap(), wire);
    let named = request.capabilities.as_deref();
    let granted = Capabilities::negotiate(request.version, named);
    assert_eq!(granted, Capabilities::ALL);

    let answer = include_bytes!("../fixtures/capabilities/answer.json");
    let mut value: serde_json::Value = serde_json::from_slice(answer).unwrap();
    value.as_object_mut().unwrap().shift_remove("capabilities");
    let response: Response = serde_json::from_value(value).unwrap();
    assert_eq!(encode_with(granted, named, &response).unwrap(), answer);

    let all = Capabilities::ALL.names();
    assert_eq!(
        encode_with(granted, Some(&all), &response).unwrap(),
        encode(&response).unwrap()
    );
    let current = Request::new("r-1", Command::Status);
    assert_eq!(current.capabilities, Some(all));
    assert_eq!(current.with_version(PROTOCOL_VERSION_9).capabilities, None);
}

/// Issue #265: a check record's findings reach only a client with the
/// `symbol_findings` capability, which versions 6 to 10 have.
#[test]
fn check_findings_reach_only_a_client_with_the_symbol_findings_capability() {
    let with = include_bytes!("../fixtures/capabilities/symbol_findings/with.json");
    let without = include_bytes!("../fixtures/capabilities/symbol_findings/without.json");
    let mut response: Response = serde_json::from_slice(with).unwrap();
    let all = Capabilities::ALL.names();
    assert_eq!(
        encode_with(Capabilities::ALL, Some(&all), &response).unwrap(),
        with
    );
    let others: Vec<String> = all
        .into_iter()
        .filter(|name| name != "symbol_findings")
        .collect();
    let granted = Capabilities::negotiate(PROTOCOL_VERSION, Some(&others));
    assert_eq!(
        String::from_utf8(encode_with(granted, Some(&others), &response).unwrap()).unwrap(),
        String::from_utf8_lossy(without)
    );
    let langspan_core::Outcome::CheckRecorded { check } = response.outcome.clone() else {
        panic!("{response:?}");
    };
    let record = langspan_core::StatusRecord::Check {
        key: "check-1".into(),
        check,
    };
    for version in OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION {
        response.version = version;
        let text = String::from_utf8(encode_for(version, &response).unwrap()).unwrap();
        let has = version >= PROTOCOL_VERSION_6;
        assert_eq!(text.contains("symbol_findings"), has, "{version}");
        let text = encode_record_for(version, &record).unwrap();
        assert_eq!(text.contains("symbol_findings"), has, "record, {version}");
    }
}

/// Issue #265: a daemon without `symbol_findings` is asked to record the
/// check without its findings, which are advisory, instead of refusing the
/// request after the check has run.
#[test]
fn a_check_is_recorded_without_findings_by_a_daemon_without_them() {
    let base: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/record-check-command-v10.json"))
            .unwrap();
    let with: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../fixtures/capabilities/symbol_findings/with.json"
    ))
    .unwrap();
    let mut value = base.clone();
    value["command"]["symbol_findings"] = with["outcome"]["check"]["symbol_findings"].clone();
    let found: Request = serde_json::from_value(value).unwrap();
    for version in OLDEST_PROTOCOL_VERSION..=PROTOCOL_VERSION {
        let granted = Capabilities::negotiate(version, None);
        let mut request = found.clone();
        assert!(fits_with(granted, &request.command), "{version}");
        for_capabilities(granted, &mut request.command);
        let expected = if version >= PROTOCOL_VERSION_6 {
            found.clone()
        } else {
            serde_json::from_value(base.clone()).unwrap()
        };
        assert_eq!(request, expected, "{version}");
    }
}

/// Issue #283: two features each add a capability on a line of its own, in
/// name order, and Git merges the two additions without a conflict.
#[test]
fn two_new_capabilities_are_added_on_lines_of_their_own() {
    let source = include_str!("../src/protocol.rs");
    let start = source.find("pub const CAPABILITIES").expect("the table");
    let body: Vec<String> = source[start..]
        .lines()
        .skip(1)
        .take_while(|line| line.trim() != "];")
        .map(str::to_string)
        .collect();
    assert_eq!(body.len(), CAPABILITIES.len(), "one line per capability");
    for (line, (name, _)) in body.iter().zip(CAPABILITIES) {
        let entry = format!("(\"{name}\", ");
        assert!(line.trim_start().starts_with(&entry), "{line}");
        // rustfmt aligns trailing comments, so one would move its neighbours.
        assert!(line.ends_with("),"), "no trailing comment: {line}");
    }
    let listed: Vec<&str> = CAPABILITIES.iter().map(|(name, _)| *name).collect();
    let mut sorted = listed.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(listed, sorted, "in name order, each once");

    let add = |name: &str| {
        let line = format!("    (\"{name}\", None),");
        let mut lines = body.clone();
        let at = lines
            .iter()
            .position(|held| held.trim_start() > line.trim_start())
            .unwrap_or(lines.len());
        lines.insert(at, line);
        lines.join("\n") + "\n"
    };
    let dir = std::env::temp_dir().join(format!("ls-capabilities-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("base"), body.join("\n") + "\n").unwrap();
    std::fs::write(dir.join("ours"), add("batched_reads")).unwrap();
    std::fs::write(dir.join("theirs"), add("session_tags")).unwrap();
    let merged = std::process::Command::new("git")
        .args(["merge-file", "-p", "ours", "base", "theirs"])
        .current_dir(&dir)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let text = String::from_utf8(merged.stdout).unwrap();
    assert!(merged.status.success(), "a conflict:\n{text}");
    assert!(
        text.contains("\"batched_reads\"") && text.contains("\"session_tags\""),
        "{text}"
    );
}

/// The capabilities 0.5.0 and 0.5.1 name in a request: each one that has a
/// version.
const NAMED_0_5: &[&str] = &[
    "anchored_notes",
    "briefing_memory",
    "inflight_partial",
    "interface_notices",
    "landing_rewrites",
    "outline_reads",
    "refusal_outcomes",
    "renames",
    "symbol_findings",
    "trails",
];

/// The capabilities 0.6.0 and 0.7.0 name: 0.5.1's and three without one.
const NAMED_0_6: &[&str] = &[
    "anchored_notes",
    "briefing_memory",
    "inflight_partial",
    "interface_notices",
    "landing_rewrites",
    "outline_reads",
    "recap_since",
    "refusal_outcomes",
    "renames",
    "symbol_findings",
    "tolerant_responses",
    "trails",
    "withheld_events",
];

/// The capabilities 0.8.0 names: 0.7.0's and `rename_holds`.
const NAMED_0_8: &[&str] = &[
    "anchored_notes",
    "briefing_memory",
    "inflight_partial",
    "interface_notices",
    "landing_rewrites",
    "outline_reads",
    "recap_since",
    "refusal_outcomes",
    "rename_holds",
    "renames",
    "symbol_findings",
    "tolerant_responses",
    "trails",
    "withheld_events",
];

/// Each release, the protocol version it speaks, the capabilities it names
/// (none before 0.5.0), and the event types it reads, frozen from that
/// release's tag (issue #331).
const RELEASES: [(&str, u16, Option<&[&str]>, &str); 9] = [
    (
        "0.1.0",
        3,
        None,
        include_str!("../fixtures/event-types/0.1.0.txt"),
    ),
    (
        "0.2.0",
        3,
        None,
        include_str!("../fixtures/event-types/0.2.0.txt"),
    ),
    (
        "0.3.0",
        8,
        None,
        include_str!("../fixtures/event-types/0.3.0.txt"),
    ),
    (
        "0.4.0",
        9,
        None,
        include_str!("../fixtures/event-types/0.4.0.txt"),
    ),
    (
        "0.5.0",
        10,
        Some(NAMED_0_5),
        include_str!("../fixtures/event-types/0.5.0.txt"),
    ),
    (
        "0.5.1",
        10,
        Some(NAMED_0_5),
        include_str!("../fixtures/event-types/0.5.1.txt"),
    ),
    (
        "0.6.0",
        10,
        Some(NAMED_0_6),
        include_str!("../fixtures/event-types/0.6.0.txt"),
    ),
    (
        "0.7.0",
        10,
        Some(NAMED_0_6),
        include_str!("../fixtures/event-types/0.7.0.txt"),
    ),
    (
        "0.8.0",
        10,
        Some(NAMED_0_8),
        include_str!("../fixtures/event-types/0.8.0.txt"),
    ),
];

/// What a release's requests are granted: the capabilities it names, or
/// its version's set when it names none.
fn release_grant(version: u16, named: Option<&[&str]>) -> (Capabilities, Option<Vec<String>>) {
    let named: Option<Vec<String>> =
        named.map(|names| names.iter().map(|name| (*name).to_string()).collect());
    (Capabilities::negotiate(version, named.as_deref()), named)
}

/// The releases `changelog` names that are published. A release PR adds the
/// heading of the version it creates and bumps `current` to it, so that
/// heading is published only once `tagged` finds its tag; every other
/// heading is a release that has been tagged (issue #518).
fn published_releases<'a>(
    changelog: &'a str,
    current: &str,
    tagged: impl Fn(&str) -> bool,
) -> Vec<&'a str> {
    changelog
        .lines()
        .filter_map(|line| line.strip_prefix("## ["))
        .filter_map(|line| line.split(']').next())
        .filter(|release| *release != current || tagged(release))
        .collect()
}

/// The published releases of `changelog`, after checking that the parse
/// found the eight bracketed headings the repository already has, so a
/// change of heading format cannot leave the test checking nothing. The
/// check counts every heading, published or not: a run without tags drops
/// the version this build carries from the published list (issue #518).
fn published_releases_checked<'a>(
    changelog: &'a str,
    current: &str,
    tagged: impl Fn(&str) -> bool,
) -> Vec<&'a str> {
    let headings = published_releases(changelog, current, |_| true);
    assert!(headings.len() >= 8, "{headings:?}");
    published_releases(changelog, current, tagged)
}

/// Whether the repository has the tag `v<release>`. A failing `git` fails
/// the test: it never counts as untagged.
fn has_release_tag(root: &std::path::Path, release: &str) -> bool {
    let output = std::process::Command::new("git")
        .args([
            "--no-optional-locks",
            "tag",
            "--list",
            &format!("v{release}"),
        ])
        .current_dir(root)
        .output()
        .unwrap_or_else(|error| panic!("cannot run git to list the tags: {error}; install git"));
    assert!(
        output.status.success(),
        "git tag --list v{release} failed in {}: {}; run the test in a Git checkout",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    !output.stdout.trim_ascii().is_empty()
}

const RELEASE_PR_CHANGELOG: &str = "# Changelog\n\n## [0.9.0](link) (2026-11-27)\n\n### Features\n\n## [0.8.0](link) (2026-11-01)\n";

/// Issue #518: on a release PR the version is bumped and the changelog
/// names it, but there is no tag yet, so it is not published; older
/// headings are published without looking for their tags.
#[test]
fn a_release_pr_version_without_its_tag_is_not_published() {
    let published = published_releases(RELEASE_PR_CHANGELOG, "0.9.0", |_| false);
    assert_eq!(published, ["0.8.0"]);
}

/// Issue #518: once the version being released is tagged it is published,
/// so a tagged release without a frozen list still fails.
#[test]
fn a_tagged_release_is_published() {
    let published = published_releases(RELEASE_PR_CHANGELOG, "0.9.0", |release| release == "0.9.0");
    assert_eq!(published, ["0.9.0", "0.8.0"]);
}

/// A pull request run has no tags, so the version this build carries is
/// not published. The repository's changelog still passes the parse check,
/// and every other heading is published.
#[test]
fn a_tagless_run_of_the_repository_changelog_passes_the_parse_check() {
    let changelog = include_str!("../CHANGELOG.md");
    let current = env!("CARGO_PKG_VERSION");
    let published = published_releases_checked(changelog, current, |_| false);
    let others: Vec<&str> = published_releases(changelog, current, |_| true)
        .into_iter()
        .filter(|release| *release != current)
        .collect();
    assert_eq!(published, others);
}

/// With every tag present, the repository's changelog passes the parse
/// check and every heading is published, the current version's too.
#[test]
fn a_tagged_run_of_the_repository_changelog_publishes_every_heading() {
    let changelog = include_str!("../CHANGELOG.md");
    let current = env!("CARGO_PKG_VERSION");
    let published = published_releases_checked(changelog, current, |_| true);
    assert!(published.contains(&current), "{published:?}");
}

/// The parse check counts the headings the changelog names, not the ones
/// published, so a release PR's untagged version does not count against it.
#[test]
fn the_parse_check_does_not_count_the_untagged_version_against_it() {
    let changelog: String = (1..=8)
        .rev()
        .map(|minor| format!("## [0.{minor}.0](link) (2026-11-01)\n\n"))
        .collect();
    let published = published_releases_checked(&changelog, "0.8.0", |_| false);
    assert_eq!(published.len(), 7, "{published:?}");
}

/// Issue #331: every published release `CHANGELOG.md` names has a frozen
/// list in `fixtures/event-types/` and a row in `RELEASES`, and every list
/// has a row, so a release cannot ship without the test of its pages. The
/// version this build carries counts once its `v` tag exists (issue #518):
/// locally and in the merge queue, which fetches the tags; a pull request
/// run has no tags and checks it from the next release on. This test runs
/// `git` because what it checks is the repository's tags.
#[test]
fn every_published_release_has_a_frozen_list() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rows: std::collections::BTreeSet<&str> = RELEASES.iter().map(|row| row.0).collect();
    let changelog = std::fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
    let published = published_releases_checked(&changelog, env!("CARGO_PKG_VERSION"), |release| {
        has_release_tag(root, release)
    });
    for release in published {
        assert!(
            root.join(format!("fixtures/event-types/{release}.txt"))
                .is_file(),
            "{release} is published and has no fixtures/event-types/{release}.txt: \
             write the event types its tag reads there"
        );
        assert!(rows.contains(release), "{release}: add it to RELEASES");
    }
    for entry in std::fs::read_dir(root.join("fixtures/event-types")).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().to_string();
        let release = name.strip_suffix(".txt").unwrap_or(&name);
        assert!(rows.contains(release), "{name} has no row in RELEASES");
    }
}

/// One event of each type this build has: the event vectors, one per type
/// (`tests/event_vectors.rs`).
fn every_event_type() -> Vec<langspan_core::RecordedEvent> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with("-event-v10.json"))
        .collect();
    paths.sort();
    (1..)
        .zip(paths)
        .map(|(seq, path)| langspan_core::RecordedEvent {
            seq,
            event: serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
        })
        .collect()
}

fn event_types(page: &serde_json::Value) -> std::collections::BTreeSet<String> {
    page["outcome"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|recorded| recorded["event"]["type"].as_str().unwrap().to_string())
        .collect()
}

fn answer(outcome: langspan_core::Outcome) -> Response {
    Response {
        version: PROTOCOL_VERSION,
        request_id: "r".into(),
        replayed: false,
        generation: 1,
        lease_valid: None,
        outcome,
    }
}

/// Issue #331: a page holding every current event type, encoded for what a
/// release's requests are granted, holds only types of that release's
/// frozen list, and every current type all releases granted the same read.
/// 0.1.0 and 0.2.0 both speak version 3, which gets what 0.1.0 reads. A
/// release that reads `withheld` is told how many were left out.
#[test]
fn each_release_is_sent_only_the_event_types_it_reads() {
    use langspan_core::Outcome;
    let events = every_event_type();
    let current = event_types(
        &serde_json::to_value(answer(Outcome::Events {
            events: events.clone(),
        }))
        .unwrap(),
    );
    let reads = |granted: Capabilities| {
        RELEASES
            .iter()
            .filter(|(_, version, named, _)| release_grant(*version, *named).0 == granted)
            .map(|(_, _, _, list)| list.lines().map(str::to_string).collect())
            .reduce(|both: std::collections::BTreeSet<String>, next| &both & &next)
            .unwrap()
    };
    for (release, version, named, _) in RELEASES {
        let (granted, named) = release_grant(version, named);
        let expected = &reads(granted) & &current;
        for outcome in [
            Outcome::Events {
                events: events.clone(),
            },
            Outcome::Changes {
                events: events.clone(),
                scan: None,
            },
        ] {
            let wire = encode_with(granted, named.as_deref(), &answer(outcome)).unwrap();
            let page: serde_json::Value = serde_json::from_slice(&wire).unwrap();
            assert_eq!(event_types(&page), expected, "{release}");
            let left_out = events.len() - expected.len();
            if granted.has("withheld_events") && left_out > 0 {
                assert_eq!(page["withheld"], left_out, "{release}");
            } else {
                assert!(page.get("withheld").is_none(), "{release}: {page}");
            }
        }
    }
}

/// Issue #259: a `changes` page holds only `disk_observed` and
/// `branch_observed`, so it pages by its limit for every client: a client
/// granted nothing reads both. A type the store's `changes` returns that
/// some client cannot read needs the store to page by the client's reads,
/// as `events` does.
#[test]
fn every_client_reads_the_event_types_of_a_changes_page() {
    let events: Vec<_> = every_event_type()
        .into_iter()
        .filter(|recorded| {
            matches!(
                recorded.event,
                langspan_core::Event::DiskObserved { .. }
                    | langspan_core::Event::BranchObserved { .. }
            )
        })
        .collect();
    assert_eq!(events.len(), 2);
    let response = answer(langspan_core::Outcome::Changes {
        events: events.clone(),
        scan: None,
    });
    let nothing = Capabilities::negotiate(OLDEST_PROTOCOL_VERSION, Some(&[]));
    let page: serde_json::Value =
        serde_json::from_slice(&encode_with(nothing, Some(&[]), &response).unwrap()).unwrap();
    assert_eq!(event_types(&page).len(), 2, "{page}");
}

/// Issue #331: the reply says how many events it left out, to a client
/// that reads that count (`withheld_events`). A frozen version cannot read
/// it and is not told.
#[test]
fn the_reply_says_how_many_events_it_left_out() {
    let events = every_event_type();
    let response = answer(langspan_core::Outcome::Events {
        events: events.clone(),
    });
    let named: Vec<String> = Capabilities::ALL
        .names()
        .into_iter()
        .filter(|name| name != "outline_reads" && name != "anchored_notes")
        .collect();
    let granted = Capabilities::negotiate(PROTOCOL_VERSION, Some(&named));
    let page: serde_json::Value =
        serde_json::from_slice(&encode_with(granted, Some(&named), &response).unwrap()).unwrap();
    assert_eq!(page["withheld"], 2, "read_outlined and note_anchor_changed");
    assert_eq!(event_types(&page).len(), events.len() - 2);
    let all = Capabilities::ALL.names();
    let page: serde_json::Value =
        serde_json::from_slice(&encode_with(Capabilities::ALL, Some(&all), &response).unwrap())
            .unwrap();
    assert!(page.get("withheld").is_none(), "nothing left out");
}

/// Issue #332: the current client reads a response tolerantly. Every
/// response vector, with a field this build does not know added to the
/// frame and to its outcome, reads as it did without it; so does one
/// added to an object only responses carry. Requests and events stay
/// strict.
#[test]
fn the_client_ignores_response_fields_it_does_not_know() {
    use serde_json::{Value, json};
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut read = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !(name.contains("-outcome-v") || name.contains("-response-v")) {
            continue;
        }
        let wire: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let known: Response = serde_json::from_value(wire.clone()).unwrap();
        let mut later = wire;
        later["from_a_later_build"] = json!({"nested": [1]});
        later["outcome"]["from_a_later_build"] = json!(true);
        let response: Response = serde_json::from_value(later).expect(&name);
        assert_eq!(response, known, "{name}");
        read += 1;
    }
    assert!(read > 100, "{read} response vectors");

    // Objects only responses carry: a briefing, a peer's work, a recent
    // path, a scan summary.
    let mut briefing: Value =
        serde_json::from_slice(include_bytes!("../fixtures/briefing-outcome-v10.json")).unwrap();
    briefing["outcome"]["briefing"]["later"] = json!(1);
    briefing["outcome"]["briefing"]["peers"][0]["later"] = json!(1);
    briefing["outcome"]["briefing"]["peers"][0]["recent"][0]["later"] = json!(1);
    serde_json::from_value::<Response>(briefing).unwrap();
    let mut changes: Value =
        serde_json::from_slice(include_bytes!("../fixtures/changes-outcome-v10.json")).unwrap();
    changes["outcome"]["scan"]["later"] = json!(1);
    serde_json::from_value::<Response>(changes).unwrap();

    // Requests and events, which the daemon and its store read, do not.
    let mut request: Value =
        serde_json::from_slice(include_bytes!("../fixtures/changes-command-v10.json")).unwrap();
    request["command"]["later"] = json!(1);
    assert!(serde_json::from_value::<Request>(request).is_err());
    let event: Value = serde_json::from_slice(include_bytes!(
        "../fixtures/workspace-registered-event-v10.json"
    ))
    .unwrap();
    for at in ["/later", "/workspace/later"] {
        let mut extra = event.clone();
        let (parent, key) = at.rsplit_once('/').unwrap();
        extra.pointer_mut(parent).unwrap()[key] = json!(1);
        assert!(
            serde_json::from_value::<langspan_core::Event>(extra).is_err(),
            "{at}"
        );
    }
}

/// Issue #332: versions 3 to 10 are frozen. Their shapes are kept by the
/// capability table, never by a function per version, and no such
/// function is added again.
#[test]
fn no_version_gets_a_shape_function_of_its_own() {
    let source = include_str!("../src/protocol.rs");
    let shaped = source.lines().filter(|line| {
        line.split("fn protocol_").nth(1).is_some_and(|rest| {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            digits > 0 && rest[digits..].starts_with("_shape")
        })
    });
    assert_eq!(shaped.count(), 0);
}
