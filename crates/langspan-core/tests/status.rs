//! The bounded status view and the paged record list, on states built in
//! memory: no I/O is needed to decide what a status answer holds.

use langspan_core::{
    FileState, KnownVersion, Lease, LeaseStatus, Note, STATUS_RECENT_MS, Session, SessionActivity,
    State, StatusRecord, TouchedPath, Workspace, shrink_status, status_records, status_view,
};

const NOW: u64 = 10 * STATUS_RECENT_MS;

fn lease(id: &str, session: &str, status: LeaseStatus) -> Lease {
    Lease {
        lease_id: id.into(),
        session_id: session.into(),
        workspace_id: "w".into(),
        paths: vec![format!("src/{id}.rs")],
        generation: 2,
        expires_at_ms: NOW + 60_000,
        status,
        operation: None,
        base_unknown: Vec::new(),
        regions: Vec::new(),
    }
}

fn session(state: &mut State, id: &str, last_ms: Option<u64>) {
    state.sessions.insert(
        id.into(),
        Session {
            session_id: id.into(),
            workspace_id: "w".into(),
            generation: 2,
            parent: None,
            headless: false,
        },
    );
    if let Some(last_ms) = last_ms {
        state.activity.insert(
            id.into(),
            SessionActivity {
                session_id: id.into(),
                generation: 2,
                last_ms,
                ..SessionActivity::default()
            },
        );
    }
}

fn base() -> State {
    let mut state = State {
        generation: 2,
        ..State::default()
    };
    state.workspaces.insert(
        "w".into(),
        Workspace {
            workspace_id: "w".into(),
            root: "/w".into(),
            verified_overlaps: Vec::new(),
        },
    );
    state
}

fn size(state: &State, summary: &langspan_core::StatusSummary) -> usize {
    serde_json::to_vec(&(state, summary)).unwrap().len()
}

#[test]
fn the_view_lists_open_admissions_counts_closed_ones_and_hides_idle_sessions() {
    let mut state = base();
    session(&mut state, "recent", Some(NOW - 1000));
    session(&mut state, "idle", Some(NOW - STATUS_RECENT_MS - 1));
    session(&mut state, "idle-holder", Some(NOW - STATUS_RECENT_MS - 1));
    session(&mut state, "just-registered", None);
    state.leases.insert(
        "open".into(),
        lease("open", "idle-holder", LeaseStatus::Active),
    );
    for (index, status) in [
        LeaseStatus::Completed,
        LeaseStatus::Expired,
        LeaseStatus::Invalidated,
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("closed-{index}");
        state.leases.insert(id.clone(), lease(&id, "idle", status));
    }
    state.leases.insert(
        "done".into(),
        lease("done", "recent", LeaseStatus::Completed),
    );

    let (view, summary) = status_view(&state, NOW);
    assert_eq!(view.leases.keys().collect::<Vec<_>>(), ["open"]);
    assert_eq!(summary.closed_admissions["idle"], 3);
    assert_eq!(summary.closed_admissions["recent"], 1);
    assert_eq!(
        view.sessions.keys().collect::<Vec<_>>(),
        ["idle-holder", "just-registered", "recent"]
    );
    assert!(!view.activity.contains_key("idle"));
    assert_eq!(summary.omitted_sessions, 1);
    assert!(summary.omitted.is_empty());
    assert!(view.workspaces.contains_key("w"));
}

#[test]
fn an_oversized_view_sheds_history_then_idle_sessions_but_keeps_open_admissions() {
    let mut state = base();
    for index in 0..400 {
        let id = format!("session-{index:03}");
        session(&mut state, &id, Some(NOW - index));
        let activity = state.activity.get_mut(&id).unwrap();
        activity.known = (0..20)
            .map(|file| KnownVersion {
                path: format!("src/{id}/file-{file}.rs"),
                state: FileState::Missing,
                at_ms: NOW,
            })
            .collect();
        activity.touched = vec![TouchedPath {
            path: format!("src/{id}/main.rs"),
            at_ms: NOW,
        }];
    }
    for index in 0..40 {
        let id = format!("open-{index:02}");
        state.leases.insert(
            id.clone(),
            lease(
                &id,
                &format!("session-{:03}", 399 - index),
                LeaseStatus::Active,
            ),
        );
    }
    state.notes.insert(
        "w".into(),
        (0..64)
            .map(|index| Note {
                note_id: format!("note-{index}"),
                workspace_id: "w".into(),
                author: None,
                text: "x".repeat(400),
                paths: Vec::new(),
            })
            .collect(),
    );
    let (mut view, mut summary) = status_view(&state, NOW);
    assert!(
        size(&view, &summary) > 48 * 1024,
        "the fixture must start too large"
    );

    shrink_status(&mut view, &mut summary, |state, summary| {
        size(state, summary) <= 48 * 1024
    });
    assert!(size(&view, &summary) <= 48 * 1024);
    assert_eq!(
        view.leases.len(),
        40,
        "open admissions are never shed first"
    );
    assert_eq!(summary.omitted_open, 0);
    assert!(summary.omitted.iter().any(|part| part == "activity.known"));
    assert!(summary.omitted.iter().any(|part| part == "notes"));
    assert!(summary.omitted_sessions > 0);
    for lease in view.leases.values() {
        assert!(
            view.sessions.contains_key(&lease.session_id),
            "a session holding an open admission stays"
        );
    }
    // The least recently active go first.
    assert!(!view.sessions.contains_key("session-359"));
    assert!(view.workspaces.contains_key("w"));
}

#[test]
fn a_page_resumes_after_its_cursor_at_every_split_without_loss_or_repeat() {
    let mut state = base();
    for index in 0..5 {
        let id = format!("s{index}");
        session(&mut state, &id, Some(NOW));
        state.leases.insert(
            format!("l{index}"),
            lease(&format!("l{index}"), &id, LeaseStatus::Completed),
        );
    }
    for workspace in ["a", "b:c", "w"] {
        state.notes.insert(
            workspace.into(),
            (0..3)
                .map(|index| Note {
                    note_id: format!("{workspace}-{index}"),
                    workspace_id: workspace.into(),
                    author: None,
                    text: format!("note {index}"),
                    paths: Vec::new(),
                })
                .collect(),
        );
    }
    let all: Vec<(String, StatusRecord)> = status_records(&state, None).unwrap().collect();
    assert_eq!(all.len(), 1 + 5 + 5 + 5 + 9);
    for split in 0..all.len() {
        let rest: Vec<StatusRecord> = status_records(&state, Some(&all[split].0))
            .unwrap()
            .map(|(_, record)| record)
            .collect();
        let expected: Vec<StatusRecord> = all[split + 1..]
            .iter()
            .map(|(_, record)| record.clone())
            .collect();
        assert_eq!(rest, expected, "resuming after record {split}");
    }
    let mut rebuilt = State {
        generation: state.generation,
        ..State::default()
    };
    for (_, record) in all {
        record.apply(&mut rebuilt);
    }
    assert_eq!(rebuilt, state);
}

#[test]
fn a_cursor_this_coordinator_did_not_give_is_refused() {
    let state = base();
    for cursor in ["", "lease", "lease:x:key", "unknown:0:key"] {
        let Err(error) = status_records(&state, Some(cursor)) else {
            panic!("{cursor:?} was accepted");
        };
        assert_eq!(error.code, "invalid_cursor");
    }
}
