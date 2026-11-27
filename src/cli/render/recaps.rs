//! Recaps, trails and reports.

use super::{ago, coverage_text};
use langspan_core::{History, Recall, Recap};

pub(super) fn print_recap(recap: &Recap) -> Result<(), crate::Error> {
    println!(
        "Workspace {} at {:?}{}.",
        recap.workspace_id,
        recap.root,
        recap
            .branch_ref
            .as_ref()
            .map(|branch| format!(" on {branch}"))
            .unwrap_or_default()
    );
    if let Some(failure) = &recap.scan_failure {
        println!(
            "The last scan failed ({}); file facts and the index below are as of the last complete scan.",
            failure.detail
        );
    }
    if let Some(coverage) = &recap.scan_coverage {
        println!("{}", coverage_text(coverage));
    }
    for session in &recap.sessions {
        println!(
            "Session {}{}{}:{}{}{}",
            session.session_id,
            if session.current {
                ""
            } else {
                " (from before the coordinator restarted)"
            },
            if session.ended { " (ended)" } else { "" },
            if session.editing.is_empty() {
                String::new()
            } else {
                format!(" editing {:?};", session.editing)
            },
            if session.recent.is_empty() {
                String::new()
            } else {
                format!(
                    " edited {:?};",
                    session
                        .recent
                        .iter()
                        .map(|recent| &recent.path)
                        .collect::<Vec<_>>()
                )
            },
            session
                .summary
                .as_ref()
                .map(|summary| format!(
                    " task {}",
                    serde_json::to_string(summary).unwrap_or_default()
                ))
                .unwrap_or_default()
        );
        if let Some(adopter) = &session.adopted_by {
            println!("  Its uncommitted work was adopted by session {adopter}.");
        }
        if !session.adopted.is_empty() {
            println!(
                "  Adopted the uncommitted work of ended session(s) {}.",
                session.adopted.join(", ")
            );
        }
    }
    if recap.omitted_sessions > 0 {
        println!("({} more session(s) not shown.)", recap.omitted_sessions);
    }
    if recap.omitted_items > 0 {
        println!(
            "({} more item(s) left out to fit one response; see `langspan events` for all.)",
            recap.omitted_items
        );
    }
    for notice in &recap.interface_changes {
        println!(
            "Interface change in {:?}: {} symbol(s); {} file(s) mention them.",
            notice.path,
            notice.changes.len(),
            notice.dependents.len()
        );
    }
    for check in &recap.checks {
        println!(
            "Check {} on tree {}: {:?}{}.",
            check.name,
            check.tree_oid,
            check.result,
            if check.changed_since {
                "; the working tree has changed since"
            } else {
                ""
            }
        );
    }
    for landing in &recap.landings {
        println!("Landed {} on {}.", landing.commit_oid, landing.onto);
    }
    for peer in &recap.teammates {
        println!(
            "Teammate {:?}: {} session(s) published.",
            peer.publication.name,
            peer.publication.work.len()
        );
    }
    for message in &recap.messages {
        println!(
            "{} {} from {} to {}: {}{}",
            if message.retracted {
                "Withdrawn message"
            } else {
                "Message"
            },
            message.message_id,
            message
                .from
                .as_ref()
                .map(|session| format!("session {session}"))
                .unwrap_or_else(|| "the owner".into()),
            match &message.to {
                langspan_core::Recipient::Session { session_id } => format!("session {session_id}"),
                langspan_core::Recipient::Everyone {} if message.retracted =>
                    "every session".into(),
                langspan_core::Recipient::Everyone {} => format!(
                    "every session ({} acknowledged so far)",
                    message.acknowledged.len()
                ),
                langspan_core::Recipient::NextSession {} => "the next session (handoff)".into(),
            },
            serde_json::to_string(&message.text)?,
            if message.paths.is_empty() {
                String::new()
            } else {
                format!(" about {:?}", message.paths)
            }
        );
    }
    for note in &recap.notes {
        // An anchored or tagged note says what it is about as a
        // briefing does, with the state of its definition.
        let about = if note.anchor.is_some() || !note.tags.is_empty() {
            crate::briefing::render::note_about(note)
        } else if note.paths.is_empty() {
            String::new()
        } else {
            format!(" about {:?}", note.paths)
        };
        println!(
            "Remembered ({}){about}: {}",
            note.note_id,
            serde_json::to_string(&note.text)?,
        );
    }
    for line in recap.since.iter().flat_map(crate::since::lines) {
        println!("{line}");
    }
    Ok(())
}

pub(super) fn print_recall(recall: &Recall) -> Result<(), crate::Error> {
    if recall.notes.is_empty() {
        println!(
            "No held note in {} holds every word of {}.",
            recall.workspace_id,
            serde_json::to_string(&recall.query)?
        );
    }
    // Note text is remembered repository text: quoted, so it reads
    // as data.
    for note in &recall.notes {
        println!(
            "Remembered ({}{}): {}{}",
            note.note_id,
            note.author
                .as_ref()
                .map(|author| format!(", by {author}"))
                .unwrap_or_default(),
            serde_json::to_string(&note.text)?,
            if note.anchor.is_some() || !note.tags.is_empty() {
                crate::briefing::render::about_note(
                    &note.paths,
                    note.anchor.as_ref(),
                    note.anchor_state,
                    &note.tags,
                )
            } else if note.paths.is_empty() {
                String::new()
            } else {
                format!(" about {:?}", note.paths)
            }
        );
    }
    if recall.truncated {
        println!("(More notes match; raise --limit, up to 50, or narrow the query.)");
    }
    Ok(())
}

pub(super) fn print_history(history: &History) -> Result<(), crate::Error> {
    if history.entries.is_empty() {
        println!(
            "No interface change to a definition named {} is recorded in {}.",
            serde_json::to_string(&history.name)?,
            history.workspace_id
        );
    }
    // Signatures, task lines, notes and messages are repository and
    // session text: quoted, so they read as data.
    let quoted = |text: &Option<String>| serde_json::to_string(text.as_deref().unwrap_or_default());
    for entry in &history.entries {
        let what = match entry.change.as_str() {
            "added" => format!("{} {} added", entry.kind, quoted(&entry.after)?),
            "removed" if entry.before.is_some() => {
                format!(
                    "{} {} removed or renamed",
                    entry.kind,
                    quoted(&entry.before)?
                )
            }
            "removed" => format!("{} removed or renamed", entry.kind),
            // Roadmap D7: a definition moved with its whole file.
            "renamed" if entry.before_path.is_some() => format!(
                "{} {} moved with its file from {}",
                entry.kind,
                quoted(&entry.after)?,
                quoted(&entry.before_path)?
            ),
            "renamed" => format!(
                "{} renamed from {} to {}{}",
                entry.kind,
                quoted(&entry.before)?,
                quoted(&entry.after)?,
                langspan_core::rename_evidence_words(entry.evidence.as_deref())
            ),
            "definition_added" => {
                format!("{} definition {} added", entry.kind, quoted(&entry.after)?)
            }
            "definition_removed" => {
                format!(
                    "{} definition {} removed",
                    entry.kind,
                    quoted(&entry.before)?
                )
            }
            _ => format!(
                "{} signature changed from {} to {}",
                entry.kind,
                quoted(&entry.before)?,
                quoted(&entry.after)?
            ),
        };
        println!(
            "seq {}, {}: {} {what}{}{}{}",
            entry.seq,
            entry.age_ms.map_or_else(|| "age unknown".into(), ago),
            serde_json::to_string(&entry.path)?,
            entry
                .by_session
                .as_ref()
                .map(|session| format!(" by session {session}"))
                .unwrap_or_default(),
            if entry.reverted {
                " (reverted: back to an earlier signature)"
            } else {
                ""
            },
            match &entry.task {
                Some(task) => format!("; task: {}", serde_json::to_string(task)?),
                None => String::new(),
            }
        );
        for note in &entry.notes {
            println!(
                "  note ({}{}): {}",
                note.note_id,
                note.author
                    .as_ref()
                    .map(|author| format!(", by {author}"))
                    .unwrap_or_default(),
                serde_json::to_string(&note.text)?
            );
        }
        for message in &entry.messages {
            println!(
                "  message ({}, from {}): {}",
                message.message_id,
                message.from.as_deref().unwrap_or("the owner"),
                serde_json::to_string(&message.text)?
            );
        }
    }
    if let Some(next) = history.next_seq {
        println!("(More is recorded; ask again with --before {next}.)");
    }
    println!("Sessions are the last reported editors of the file, not proof of authorship.");
    Ok(())
}
