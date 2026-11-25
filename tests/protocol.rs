use langspan::protocol::{PROTOCOL_VERSION, Request, Response, encode, read_frame};
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
fn v3_briefing_request_and_response_match_the_committed_golden_vectors() {
    let wire = include_bytes!("../fixtures/brief-request-v3.json");
    let request: Request = serde_json::from_slice(wire).unwrap();
    assert_eq!(request.version, PROTOCOL_VERSION);
    assert!(matches!(
        &request.command,
        Command::Brief { max_peers: 6, .. }
    ));
    assert_eq!(encode(&request).unwrap(), wire);

    let wire = include_bytes!("../fixtures/briefing-response-v3.json");
    let response: Response = serde_json::from_slice(wire).unwrap();
    assert_eq!(response.version, PROTOCOL_VERSION);
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
    assert_eq!(request.version, PROTOCOL_VERSION);
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
        assert_eq!(request.version, PROTOCOL_VERSION);
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
            mid_turn: false,
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
