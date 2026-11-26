use langspan::protocol::{
    OLDEST_PROTOCOL_VERSION, PROTOCOL_VERSION, Request, Response, encode, encode_for, read_frame,
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
    assert!(serde_json::from_str::<Response>(&unknown).is_err());
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
    assert!(serde_json::from_str::<Response>(&unknown).is_err());
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
    assert!(serde_json::from_str::<Response>(&unknown).is_err());

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
    let unknown = String::from_utf8_lossy(wire).replace(r#""files":10"#, r#""files":10,"x":1"#);
    assert!(serde_json::from_str::<Response>(&unknown).is_err());
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
    assert!(serde_json::from_str::<Response>(&unknown).is_err());
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
    assert_eq!(response.version, PROTOCOL_VERSION);
    let langspan_core::Outcome::History { history } = &response.outcome else {
        panic!("{response:?}");
    };
    assert_eq!(history.entries[0].evidence.as_deref(), Some("same_body"));
    assert_eq!(
        history.entries[1].before_path.as_deref(),
        Some("app/old_users.py")
    );
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
