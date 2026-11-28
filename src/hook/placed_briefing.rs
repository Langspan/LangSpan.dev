use super::*;
use langspan_core::{Briefing, Note, PeerWork, PresencePath, RecentPath, StaleRead};

fn peer(id: &str, paths: &[String]) -> PeerWork {
    PeerWork {
        session_id: format!("codex.{id}"),
        workspace_id: "w".into(),
        workspace_root: "/repos/app".into(),
        same_workspace: true,
        branch_ref: Some("refs/heads/main".into()),
        liveness: None,
        ended: false,
        summary: Some("fix the upload retry".into()),
        intent: None,
        editing: Vec::new(),
        recent: paths
            .iter()
            .map(|path| RecentPath {
                path: path.clone(),
                age_ms: 1_000,
            })
            .collect(),
        overlap: Vec::new(),
        target_overlap: Vec::new(),
        same_task: false,
        relation: None,
        looking: Vec::new(),
        place_told: false,
    }
}

fn briefing(peers: Vec<PeerWork>) -> Briefing {
    Briefing {
        session_id: "claude-code.me".into(),
        changed: true,
        peers,
        omitted_peers: 0,
        unchanged: 0,
        stale_reads: Vec::new(),
        remote: Vec::new(),
        interface_changes: Vec::new(),
        interface_notice_ids: Vec::new(),
        inflight: Vec::new(),
        omitted_inflight: 0,
        inflight_partial: false,
        notes: Vec::new(),
        messages: Vec::new(),
        retracted: Vec::new(),
        checkout_changes: Vec::new(),
        uncoordinated: Vec::new(),
        checks: Vec::new(),
        left_work: Vec::new(),
        recent: None,
        trail: None,
    }
}

fn paths(prefix: &str, count: usize, width: usize) -> Vec<String> {
    (0..count)
        .map(|n| format!("src/{prefix}{n}_{}.rs", "x".repeat(width)))
        .collect()
}

/// Every item `placed` holds is in `text`, and every key of `delivered`
/// that `placed` lacks is one the hook forgets.
fn assert_placed_items_are_shown(text: &str, placed: &Briefing) {
    for peer in &placed.peers {
        let id = peer.session_id.trim_start_matches("codex.");
        assert!(text.contains(&format!("codex session {id}")), "{text}");
        for path in peer
            .overlap
            .iter()
            .chain(&peer.target_overlap)
            .chain(&peer.editing)
            .chain(peer.recent.iter().map(|recent| &recent.path))
            .chain(peer.looking.iter().map(|looked| &looked.path))
        {
            assert!(text.contains(&quoted(path)), "{path} not in {text}");
        }
        if let Some(summary) = &peer.summary {
            assert!(text.contains(&quoted(summary)), "{text}");
        }
    }
    for note in &placed.notes {
        assert!(text.contains(&format!("{:?}", note.text)), "{text}");
    }
    for stale in &placed.stale_reads {
        assert!(text.contains(&quoted(&stale.path)), "{text}");
    }
}

#[test]
fn a_briefing_that_fits_places_everything_and_forgets_nothing() {
    let delivered = briefing(vec![
        peer("bob", &paths("a", 2, 4)),
        peer("carol", &paths("b", 1, 4)),
    ]);
    let (text, placed) = render_briefing_placed(&delivered, BRIEFING_LIMIT);
    assert_placed_items_are_shown(&text, &placed);
    assert_eq!(placed, delivered);
    assert!(unplaced_keys(&delivered, Some(&placed)).is_empty());
    // Nothing placed at all: every key is forgotten.
    assert_eq!(
        unplaced_keys(&delivered, None).len(),
        langspan_core::briefing_keys(&delivered).len()
    );
}

#[test]
fn a_peer_line_that_does_not_fit_is_not_placed() {
    let delivered = briefing(
        (0..16)
            .map(|n| peer(&format!("peer{n}"), &paths(&format!("p{n}_"), 3, 150)))
            .collect(),
    );
    let (text, placed) = render_briefing_placed(&delivered, BRIEFING_LIMIT);
    assert!(text.contains("more session(s) not shown"), "{text}");
    assert!(placed.peers.len() < delivered.peers.len(), "{text}");
    assert_placed_items_are_shown(&text, &placed);
    let unplaced = unplaced_keys(&delivered, Some(&placed));
    let left_out = briefing(
        delivered
            .peers
            .iter()
            .filter(|peer| !placed.peers.iter().any(|p| p.session_id == peer.session_id))
            .cloned()
            .collect(),
    );
    assert!(!left_out.peers.is_empty());
    for key in langspan_core::briefing_keys(&left_out) {
        assert!(unplaced.contains(&key), "a left-out peer stays told");
    }
}

#[test]
fn paths_past_a_cut_and_a_task_cut_short_are_not_placed() {
    let mut long = peer("bob", &paths("r", 30, 30));
    long.summary = Some("t".repeat(300));
    let delivered = briefing(vec![long]);
    let (text, placed) = render_briefing_placed(&delivered, BRIEFING_LIMIT);
    let shown = &placed.peers[0];
    assert!(!shown.recent.is_empty(), "{text}");
    assert!(shown.recent.len() < 30, "{text}");
    assert_eq!(shown.summary, None, "the task was cut: {text}");
    assert_placed_items_are_shown(&text, &placed);
    // The first path left out is not whole in the text, with its age.
    let next = &delivered.peers[0].recent[shown.recent.len()].path;
    assert!(
        !text.contains(&format!("{} (1s ago)", quoted(next))),
        "{text}"
    );
    assert!(!unplaced_keys(&delivered, Some(&placed)).is_empty());
}

#[test]
fn presence_without_room_is_not_placed() {
    let looking = |id: &str, width: usize| PeerWork {
        looking: vec![PresencePath {
            path: format!("src/{id}/{}.rs", "z".repeat(40)),
            kind: langspan_core::PresenceKind::Read,
            age_s: 60,
        }],
        ..peer(id, &paths(id, 2, width))
    };
    // Roomy: presence is shown and placed.
    let roomy = briefing(vec![looking("bob", 20)]);
    let (text, placed) = render_briefing_placed(&roomy, BRIEFING_LIMIT);
    assert!(text.contains("looking at"), "{text}");
    assert_eq!(placed.peers[0].looking.len(), 1);
    assert!(unplaced_keys(&roomy, Some(&placed)).is_empty());
    // Crowded: the work lines take the room, presence is left out.
    let crowded = briefing((0..8).map(|n| looking(&format!("p{n}"), 100)).collect());
    let (text, placed) = render_briefing_placed(&crowded, BRIEFING_LIMIT);
    assert_placed_items_are_shown(&text, &placed);
    let unshown = placed
        .peers
        .iter()
        .filter(|peer| peer.looking.is_empty())
        .count();
    assert!(unshown > 0, "{text}");
    assert!(!unplaced_keys(&crowded, Some(&placed)).is_empty());
}

#[test]
fn notices_without_room_and_a_stale_list_cut_short_are_not_placed() {
    let mut delivered = briefing(Vec::new());
    delivered.notes = (0..10)
        .map(|n| Note {
            note_id: format!("note-{n}"),
            workspace_id: "w".into(),
            author: None,
            text: format!("{n} {}", "n".repeat(300)),
            paths: Vec::new(),
            anchor: None,
            anchor_state: None,
            tags: Vec::new(),
        })
        .collect();
    delivered.stale_reads = paths("s", 30, 30)
        .into_iter()
        .map(|path| StaleRead {
            path,
            session_id: "codex.bob".into(),
            age_ms: 1_000,
        })
        .collect();
    let (text, placed) = render_briefing_placed(&delivered, BRIEFING_LIMIT);
    assert!(text.contains("more notice(s) not shown"), "{text}");
    assert!(placed.notes.len() < delivered.notes.len(), "{text}");
    assert!(!placed.stale_reads.is_empty(), "{text}");
    assert!(
        placed.stale_reads.len() < delivered.stale_reads.len(),
        "{text}"
    );
    assert_placed_items_are_shown(&text, &placed);
    let next = &delivered.stale_reads[placed.stale_reads.len()].path;
    assert!(!text.contains(&quoted(next)), "{text}");
}

#[test]
fn peer_lines_left_out_for_a_check_result_are_not_placed() {
    let mut delivered = briefing(vec![peer("bob", &paths("a", 2, 4))]);
    delivered.checks = vec![langspan_core::CheckNotice {
        check_id: "lsh1-auto-check-1".into(),
        name: "tests".into(),
        tree_oid: "a".repeat(40),
        result: langspan_core::CheckResult::Failed { exit_code: 101 },
        duration_ms: 2300,
        timed_out_s: None,
        tail: "failed".into(),
        failures: Vec::new(),
        more_failures: false,
        baseline_check_id: None,
        fixed: Vec::new(),
        writers: Vec::new(),
        interface_grade: None,
        interface: None,
        index_incomplete: false,
    }];
    let (text, placed) = render_briefing_placed(&delivered, PEERS_MINIMUM);
    assert!(text.contains(PEERS_NOT_SHOWN), "{text}");
    assert!(placed.peers.is_empty());
    assert_eq!(
        unplaced_keys(&delivered, Some(&placed)),
        unplaced_keys(&delivered, None)
    );
}
