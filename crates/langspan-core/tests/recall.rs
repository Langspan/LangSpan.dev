//! Searching remembered notes (roadmap E2): a read over the held notes of one
//! workspace, matched word by word without case, never recorded.
use langspan_core::{Command, Event, Outcome, Recall, State, apply, decide};

fn run(state: &mut State, id: &str, command: Command) -> Outcome {
    let before = state.clone();
    let decision = decide(state, &command, id, 5);
    assert_eq!(*state, before, "decisions stay pure");
    for event in decision.events {
        apply(state, &event).unwrap();
    }
    decision.outcome
}

fn ready() -> State {
    let mut state = State::default();
    apply(&mut state, &Event::Booted { generation: 1 }).unwrap();
    run(
        &mut state,
        "w",
        Command::WorkspaceRegister {
            workspace_id: "work".into(),
            root: "/repos/work".into(),
        },
    );
    run(
        &mut state,
        "s-alice",
        Command::SessionRegister {
            session_id: "alice".into(),
            workspace_id: "work".into(),
            parent: None,
            headless: false,
        },
    );
    state
}

fn remember(state: &mut State, id: &str, text: &str, paths: &[&str]) {
    let outcome = run(
        state,
        id,
        Command::Remember {
            workspace_id: "work".into(),
            author: Some("alice".into()),
            text: text.into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
        },
    );
    assert!(matches!(outcome, Outcome::Remembered { .. }), "{outcome:?}");
}

fn recall_command(query: &str, path: Option<&str>, limit: u32) -> Command {
    Command::Recall {
        workspace_id: "work".into(),
        query: query.into(),
        path: path.map(str::to_owned),
        limit,
    }
}

fn recall(state: &State, query: &str, path: Option<&str>, limit: u32) -> Recall {
    let decision = decide(state, &recall_command(query, path, limit), "read", 5);
    assert!(decision.events.is_empty(), "a recall records nothing");
    match decision.outcome {
        Outcome::Recall { recall } => recall,
        other => panic!("{other:?}"),
    }
}

fn ids(recall: &Recall) -> Vec<&str> {
    recall
        .notes
        .iter()
        .map(|note| note.note_id.as_str())
        .collect()
}

fn refusal(state: &State, command: Command) -> (String, String) {
    match decide(state, &command, "read", 5).outcome {
        Outcome::Error { code, message } => (code, message),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn every_word_must_appear_without_case_in_text_or_paths() {
    let mut state = ready();
    remember(
        &mut state,
        "n-both",
        "Upload_ID is minted by the upload handler.",
        &["src/upload.rs"],
    );
    remember(&mut state, "n-one", "Uploads are retried twice.", &[]);
    remember(&mut state, "n-path", "Named here.", &["src/upload/id.rs"]);
    remember(&mut state, "n-none", "Nothing relevant.", &[]);

    let found = recall(&state, "upload id", None, 10);
    assert_eq!(found.workspace_id, "work");
    assert_eq!(found.query, "upload id");
    assert_eq!(ids(&found), ["n-both", "n-path"], "{found:?}");
    assert!(!found.truncated);
    let note = &found.notes[0];
    assert_eq!(note.author.as_deref(), Some("alice"));
    assert_eq!(note.text, "Upload_ID is minted by the upload handler.");
    assert_eq!(note.paths, ["src/upload.rs"]);

    // Case of the query does not matter either, and extra spaces split
    // nothing.
    assert_eq!(
        ids(&recall(&state, "  UPLOAD\tId ", None, 10)),
        ["n-both", "n-path"]
    );
    // A note with only one of two words is not a hit.
    assert_eq!(ids(&recall(&state, "uploads twice", None, 10)), ["n-one"]);
    assert!(recall(&state, "retried handler", None, 10).notes.is_empty());
}

#[test]
fn notes_hit_in_more_fields_come_first_then_newest() {
    let mut state = ready();
    remember(&mut state, "old-text", "The parser owns tokens.", &[]);
    remember(
        &mut state,
        "both",
        "The parser is strict.",
        &["src/parser.rs"],
    );
    remember(&mut state, "new-text", "A parser rewrite is planned.", &[]);
    remember(
        &mut state,
        "path-only",
        "Owned by alice.",
        &["src/parser.rs"],
    );
    assert_eq!(
        ids(&recall(&state, "parser", None, 10)),
        ["both", "path-only", "new-text", "old-text"]
    );
}

#[test]
fn a_path_keeps_notes_about_it_or_about_the_whole_workspace() {
    let mut state = ready();
    remember(&mut state, "api", "Errors are typed.", &["src/api.rs"]);
    remember(&mut state, "whole", "Errors never panic.", &[]);
    remember(&mut state, "other", "Errors are strings.", &["src/cli.rs"]);
    remember(
        &mut state,
        "both",
        "Errors cross this boundary.",
        &["src/cli.rs", "src/api.rs"],
    );
    let found = recall(&state, "errors", Some("src/api.rs"), 10);
    assert_eq!(ids(&found), ["both", "whole", "api"], "{found:?}");
    assert_eq!(
        refusal(&state, recall_command("errors", Some("../api.rs"), 10)).0,
        "invalid_path"
    );
}

#[test]
fn a_limit_below_the_hits_is_truncated_and_said() {
    let mut state = ready();
    for index in 0..3 {
        remember(&mut state, &format!("n{index}"), "Retry with backoff.", &[]);
    }
    let found = recall(&state, "retry", None, 2);
    assert_eq!(ids(&found), ["n2", "n1"]);
    assert!(found.truncated);
    let all = recall(&state, "retry", None, 3);
    assert_eq!(all.notes.len(), 3);
    assert!(!all.truncated);
}

#[test]
fn bounds_are_refused_with_the_bound_named() {
    let state = ready();
    let (code, message) = refusal(&state, recall_command(&"a".repeat(201), None, 10));
    assert_eq!(code, "invalid_query");
    assert!(message.contains("200 bytes"), "{message}");
    assert_eq!(recall(&state, &"a".repeat(200), None, 10).notes.len(), 0);
    let (code, message) = refusal(&state, recall_command("a b c d e f g h i", None, 10));
    assert_eq!(code, "invalid_query");
    assert!(message.contains("8 words"), "{message}");
    assert!(recall(&state, "a b c d e f g h", None, 10).notes.is_empty());
    for blank in ["", "   ", "\t"] {
        assert_eq!(
            refusal(&state, recall_command(blank, None, 10)).0,
            "invalid_query"
        );
    }
    for limit in [0, 51] {
        let (code, message) = refusal(&state, recall_command("a", None, limit));
        assert_eq!(code, "invalid_limit");
        assert!(message.contains("1 to 50"), "{message}");
    }
    assert!(recall(&state, "a", None, 50).notes.is_empty());
    let (code, _) = refusal(
        &state,
        Command::Recall {
            workspace_id: "nowhere".into(),
            query: "a".into(),
            path: None,
            limit: 10,
        },
    );
    assert_eq!(code, "workspace_not_found");
}

#[test]
fn a_forgotten_note_is_never_returned() {
    let mut state = ready();
    remember(&mut state, "keep", "Cache keys are hashed.", &[]);
    remember(&mut state, "drop", "Cache keys are raw.", &[]);
    assert_eq!(ids(&recall(&state, "cache", None, 10)), ["drop", "keep"]);
    let outcome = run(
        &mut state,
        "f",
        Command::Forget {
            workspace_id: "work".into(),
            note_id: "drop".into(),
        },
    );
    assert!(
        matches!(outcome, Outcome::Forgotten { removed: true, .. }),
        "{outcome:?}"
    );
    assert_eq!(ids(&recall(&state, "cache", None, 10)), ["keep"]);
}

#[test]
fn shrinking_to_fit_drops_the_last_hits_and_says_so() {
    let mut state = ready();
    for index in 0..4 {
        remember(&mut state, &format!("n{index}"), "Retry with backoff.", &[]);
    }
    let mut found = recall(&state, "retry", None, 10);
    assert!(!found.truncated);
    found.shrink_until(|recall| recall.notes.len() <= 2);
    assert_eq!(ids(&found), ["n3", "n2"]);
    assert!(found.truncated);
}
