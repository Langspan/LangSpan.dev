//! What the `langspan` command prints.

mod graph;
mod landing;
mod messages;
mod recaps;
mod remote;
mod state;
mod workspaces;

use crate::protocol::Response;
use crate::today::utc;
use graph::{
    print_blind_spots, print_graph, print_graph_imported, print_graph_inflight, print_map,
    print_paths, print_write_check,
};
pub use landing::{
    print_combined, print_conflict, print_inconclusive, print_not_rewritten, print_notes,
    print_renames, print_replay_conflict,
};
use langspan_core::Outcome;
use messages::{print_message_sent, print_message_shown, print_message_status};
use recaps::{print_history, print_recall, print_recap};
use remote::{print_compaction, print_remote_sent};
use state::print_status;
pub use workspaces::print_project_hook_reruns;
use workspaces::{context_line, print_workspace_removal, print_workspaces};

/// What the last completed scan did not version, in one paragraph. A
/// partial scan is always called partial.
pub fn coverage_text(coverage: &langspan_core::ScanCoverage) -> String {
    let named = |paths: &[String], count: u64| {
        let mut text = paths
            .iter()
            .map(|path| format!("{path:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        if count > paths.len() as u64 {
            text.push_str(&format!(" and {} more", count - paths.len() as u64));
        }
        text
    };
    let mut parts = Vec::new();
    if coverage.omitted > 0 {
        parts.push(format!(
            "{} file(s) left out at the scan's file, byte or time bound (changed files are versioned first)",
            coverage.omitted
        ));
    }
    if coverage.too_large > 0 {
        parts.push(format!(
            "{} file(s) not versioned because they are over {} MiB: {}",
            coverage.too_large,
            crate::observer::Limits::default().max_file_bytes / (1024 * 1024),
            named(&coverage.too_large_paths, coverage.too_large)
        ));
    }
    if coverage.submodules > 0 {
        parts.push(format!(
            "{} submodule(s) present and not read into: {}",
            coverage.submodules,
            named(&coverage.submodule_paths, coverage.submodules)
        ));
    }
    if coverage.skip_worktree > 0 {
        parts.push(format!(
            "{} file(s) outside the sparse checkout, not observed",
            coverage.skip_worktree
        ));
    }
    let lead = if coverage.is_partial() {
        "The last scan was partial: file observations, check staleness marks and the index do not cover the files it did not version."
    } else {
        "The last scan versioned every file in the working tree."
    };
    format!("{lead} {}.", parts.join("; "))
}

/// How long ago, for a person: seconds, minutes, hours or days.
fn ago(ms: u64) -> String {
    match ms / 1000 {
        seconds @ 0..=89 => format!("{seconds}s ago"),
        seconds @ 90..=5399 => format!("{}m ago", seconds / 60),
        seconds @ 5400..=129_599 => format!("{}h ago", seconds / 3600),
        seconds => format!("{}d ago", seconds / 86_400),
    }
}

pub fn print_human(response: &Response, state_dir: &std::path::Path) -> Result<(), crate::Error> {
    if response.replayed {
        println!(
            "Original result replayed; current daemon generation {}.",
            response.generation
        );
    }
    match &response.outcome {
        // Workspaces and sessions
        Outcome::Workspace { workspace } => println!(
            "Workspace {} registered at {:?}.",
            workspace.workspace_id, workspace.root
        ),
        Outcome::Workspaces { workspaces, next } => print_workspaces(workspaces, next),
        Outcome::WorkspaceMoved {
            workspace, kept, ..
        } => println!(
            "Workspace {} now at {}. Kept: {} notes, {} pending messages, {} check records, {} landings.",
            workspace.workspace_id,
            workspace.root,
            kept.notes,
            kept.messages_pending,
            kept.checks,
            kept.landings
        ),
        Outcome::WorkspaceRemoval {
            workspace_id,
            applied,
            ending,
        } => print_workspace_removal(workspace_id, applied, ending),
        Outcome::Session { session } => println!(
            "Session {} attached to {} in generation {}.",
            session.session_id, session.workspace_id, session.generation
        ),
        Outcome::SessionLiveness { liveness } => println!(
            "Session {} process {}: {:?}, observed at {} ms in generation {}.",
            liveness.session_id,
            liveness.process.pid,
            liveness.status,
            liveness.observed_at_ms,
            liveness.generation
        ),
        Outcome::SessionSeen {
            session_id,
            last_hook_ms,
        } => println!("Session {session_id}: last hook run recorded at {last_hook_ms} ms."),
        Outcome::ContextRecorded {
            session_id,
            context,
        } => println!("Session {session_id}: {}", context_line(context)),
        Outcome::ToldForgotten {
            session_id,
            forgotten,
        } => println!("Session {session_id}: {forgotten} briefing item(s) will be told again."),
        // Intents and admissions
        Outcome::Intent { intent } => println!(
            "Intent for {}: {} on {:?}.",
            intent.session_id,
            serde_json::to_string(&intent.summary)?,
            intent.paths
        ),
        Outcome::IntentEnded { session_id } => println!("Intent ended for {session_id}."),
        Outcome::Reserved { reservation, held } => {
            println!(
                "Reserved {} {} for session {}. No other session of this repository is given it.",
                reservation.key, reservation.number, reservation.session_id
            );
            for claim in held {
                println!(
                    "Also held: {} {} by session {} in workspace {}.",
                    claim.key, claim.number, claim.session_id, claim.workspace_id
                );
            }
        }
        Outcome::Admitted { lease } => println!(
            "Admission {}: holder {}, files {:?}, generation {}, current validity {}. Expires at {} ms of that daemon generation.",
            lease.lease_id,
            lease.session_id,
            lease.paths,
            lease.generation,
            response.lease_valid == Some(true),
            lease.expires_at_ms
        ),
        Outcome::Conflict { holders } => {
            println!("Conflict; no files admitted.");
            for holder in holders {
                println!(
                    "  {} holds {:?} through admission {}.",
                    holder.session_id, holder.paths, holder.lease_id
                );
            }
        }
        Outcome::Completed { lease_id } => println!("Admission {lease_id} completed."),
        // Observed operations, files and branches
        Outcome::Observed { observation } => println!(
            "Observation {}: {} {:?} reported {:?} for {:?}; admission relation {:?}.",
            observation.observation_id,
            observation.source,
            observation.tool,
            observation.result,
            observation.paths,
            observation.lease_relation
        ),
        Outcome::DiskObserved { observation } => println!(
            "Unattributed disk observation {}: {:?}, {:?} -> {:?}.",
            observation.observation_id, observation.path, observation.before, observation.after
        ),
        Outcome::BranchObserved { observation } => println!(
            "Local branch observation {}: {:?} -> {:?}.",
            observation.observation_id, observation.branch_ref, observation.head_oid
        ),
        // State, the event log and Git state
        Outcome::Status { state, summary } => print_status(state, summary, state_dir)?,
        Outcome::StatusRecords { records, next } => {
            for record in records {
                println!("{}", serde_json::to_string(record)?);
            }
            if let Some(next) = next {
                println!(
                    "More: langspan status --all --after {}",
                    serde_json::to_string(next)?
                );
            }
        }
        Outcome::Events { events } => {
            for event in events {
                println!("{} {}", event.seq, serde_json::to_string(&event.event)?);
            }
        }
        Outcome::Changes { events, scan } => {
            match scan {
                Some(scan) => println!(
                    "Latest complete scan: {} files, {} local branches, generation {}, observed at {} ms. Disk changes are unattributed.",
                    scan.file_count, scan.branch_count, scan.generation, scan.observed_at_ms
                ),
                None => println!("No complete Git baseline yet; coverage is unknown."),
            }
            for event in events {
                println!("{} {}", event.seq, serde_json::to_string(&event.event)?);
            }
        }
        Outcome::WorkspaceGit { git } => println!(
            "Workspace {} is in repository {} on {:?}.",
            git.workspace_id, git.repository_id, git.branch_ref
        ),
        // Prompts, presence and a session's turns
        Outcome::SessionPrompted { session_id, .. } => {
            println!("Task summary recorded for {session_id}.")
        }
        Outcome::PromptTargets { session_id, paths } => println!(
            "Prompt targets recorded for {session_id}: {} file(s).",
            paths.len()
        ),
        Outcome::PresenceObserved { session_id, paths } => {
            println!("Recorded {paths} looked-at path(s) for {session_id}.")
        }
        Outcome::Presence {
            session_id,
            setting,
        } => println!(
            "Presence for {session_id}: {}; peers' presence shown for {} minute(s).",
            if setting.shown {
                "shown to peers"
            } else {
                "off, never shown to peers"
            },
            setting.window_ms / 60_000
        ),
        Outcome::SessionEnded { session_id } => {
            println!("Session {session_id} marked ended.")
        }
        Outcome::Adopted { adoption, .. } => println!(
            "Session {} adopted the uncommitted work of session {}.",
            adoption.session_id, adoption.from
        ),
        Outcome::SessionStopped {
            session_id,
            released,
            ..
        } => println!(
            "Session {session_id} finished its reply; {} open admission(s) released.",
            released.len()
        ),
        Outcome::ToolBatchObserved {
            session_id, calls, ..
        } => {
            println!("Batch of {calls} tool call(s) recorded for {session_id}.")
        }
        // Automatic checks
        Outcome::AutoCheck { check } => {
            println!(
                "Automatic check {} is on for workspace {}: after a session's writes, {:?} runs on the exact working tree (time limit {} s).",
                check.name,
                check.workspace_id,
                crate::landing::command_line(&check.argv),
                check.timeout_s
            );
            if let Some(wider) = &check.on_interface_change {
                println!(
                    "On an interface change: {} runs {:?} whole, after {} on the same tree, when the writes changed an indexed export that other files mention (time limit {} s).",
                    wider.name,
                    crate::landing::command_line(&wider.argv),
                    check.name,
                    wider.timeout_s
                );
            }
        }
        Outcome::AutoCheckDisabled { workspace_id } => {
            println!("Automatic check is off for workspace {workspace_id}.")
        }
        // Briefings, refused edits and source versions
        Outcome::Briefing { briefing } => {
            if !crate::briefing::render::has_content(briefing) {
                println!("No relevant peer work.");
            } else {
                println!("{}", crate::briefing::render::render_briefing(briefing));
            }
        }
        Outcome::StaleBase { stale } => {
            println!("Stale source; no files admitted. Read these again first:");
            for file in stale {
                println!(
                    "  {:?} changed since this session read it{}.",
                    file.path,
                    file.changed_by
                        .as_ref()
                        .map(|session| format!(" (last reported edit by {session})"))
                        .unwrap_or_default()
                );
            }
        }
        Outcome::UnreadLines { lines } => {
            println!("Unread lines; no files admitted. Read these first:");
            for entry in lines {
                println!(
                    "  {:?}: lines {} written by {} and not read since.",
                    entry.path,
                    if entry.lines.is_empty() {
                        "that could not be placed".to_owned()
                    } else {
                        entry
                            .lines
                            .iter()
                            .map(|range| format!("{}-{}", range.start_line, range.end_line))
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    entry.session_id
                );
            }
        }
        Outcome::SourceObserved { session_id, files } => {
            println!("Recorded {files} file version(s) for {session_id}.")
        }
        // Remote publication and retention
        Outcome::RemoteLink { link } => println!(
            "Workspace {} publishes to Git remote {:?} as {:?} (participant {}) at {}{}; task lines {}; last publication {}.",
            link.workspace_id,
            link.remote,
            link.name,
            link.participant,
            crate::remote::peer_ref(link),
            if link.publish_as_branch {
                ", a branch everyone in the repository can see"
            } else {
                ""
            },
            if link.share_task {
                "shared"
            } else {
                "not shared"
            },
            link.sequence
        ),
        Outcome::RemoteDisabled { workspace_id } => println!(
            "Workspace {workspace_id} no longer publishes. The daemon removes its ref from the remote on its next pass; `langspan doctor` shows whether the remote has let it."
        ),
        Outcome::RemoteOutbox {
            publication,
            changed,
            ..
        } => {
            println!(
                "{} This is everything that leaves the machine for this workspace:",
                if *changed {
                    "Not yet published."
                } else {
                    "Matches the last publication."
                }
            );
            println!("{}", serde_json::to_string_pretty(publication)?);
        }
        Outcome::RemoteSent {
            sent,
            more,
            since_ms,
        } => print_remote_sent(sent, more, since_ms),
        Outcome::Compaction { report } => print_compaction(report),
        Outcome::RemoteObserved {
            participant,
            accepted,
        } => println!("Publication from {participant}: accepted {accepted}."),
        // Checks and test results
        Outcome::CheckRecorded { check } => {
            println!("Check {} recorded for tree {}.", check.name, check.tree_oid)
        }
        Outcome::Checks { checks } => println!("{} check record(s).", checks.len()),
        Outcome::FlakyTests { tests, .. } => {
            println!("{} test(s) known to flip on unchanged code.", tests.len())
        }
        Outcome::FlakyTestForgotten {
            test, forgotten, ..
        } => println!("Forgot {test:?}: {forgotten}."),
        Outcome::Readiness { ready, reason, .. } => println!("Ready: {ready}. {reason}."),
        // Landings and landing turns
        Outcome::Landed { landing } => println!(
            "Landing {} on {} recorded.",
            landing.commit_oid, landing.onto
        ),
        Outcome::Turn { turn } => println!(
            "Push turn {} for {} held until {} ms.",
            turn.turn_id, turn.onto, turn.expires_at_ms
        ),
        Outcome::TurnBusy { holder, held_ms } => println!(
            "Another landing holds the push turn for {}: turn {}, for {held_ms} ms.",
            holder.onto, holder.turn_id
        ),
        Outcome::TurnWaiting {
            position, waiting, ..
        } => println!(
            "Waiting for the push turn: number {position} of {} in line.",
            waiting.len()
        ),
        Outcome::TurnUnavailable {
            waiting,
            queue_full,
            ..
        } => println!(
            "The push turn is not available: {} landing(s) in line{}.",
            waiting.len(),
            if *queue_full {
                ", and the line is full"
            } else {
                ""
            }
        ),
        Outcome::TurnQueue {
            holder, waiting, ..
        } => println!(
            "Push turn {}; {} landing(s) in line.",
            holder.as_ref().map_or("free".to_owned(), |turn| format!(
                "held by {}",
                turn.turn_id
            )),
            waiting.len()
        ),
        Outcome::TurnQueueLeft { ticket, left } => println!(
            "Landing {ticket} {}.",
            if *left {
                "stopped waiting for the push turn"
            } else {
                "was not waiting for the push turn"
            }
        ),
        Outcome::TurnReleased { turn_id, released } => println!(
            "Push turn {turn_id} {}.",
            if *released {
                "released"
            } else {
                "had already ended"
            }
        ),
        // Interface changes and notes
        Outcome::InterfaceObserved { notice } => println!(
            "Interface change in {:?}: {} symbol(s), {} file(s) mention them.",
            notice.path,
            notice.changes.len(),
            notice.dependents.len()
        ),
        Outcome::InterfaceDependentsObserved { notice_id, added } => {
            println!("Interface notice {notice_id}: {added} file(s) of another worktree added.")
        }
        Outcome::Remembered { note } => println!(
            "Remembered as {}. Forget it with `langspan forget {} {}`.",
            note.note_id, note.workspace_id, note.note_id
        ),
        Outcome::NoteAnchorsObserved { changed } => {
            println!("{changed} anchored note(s) changed state.");
        }
        Outcome::Forgotten { note_id, removed } => println!(
            "{}",
            if *removed {
                format!("Forgot {note_id}.")
            } else {
                format!("No note {note_id} to forget.")
            }
        ),
        // Messages
        Outcome::MessageSent { message } => print_message_sent(message),
        Outcome::MessageAcknowledged {
            message_id,
            acknowledged,
        } => println!(
            "{}",
            if *acknowledged {
                format!("Acknowledged {message_id}; it is not delivered to this session again.")
            } else {
                format!("Message {message_id} was not waiting for this session.")
            }
        ),
        Outcome::MessageShown { message } => print_message_shown(message)?,
        Outcome::MessageStatus { status } => print_message_status(status),
        Outcome::MessageRetracted {
            message_id,
            withdrawn_unread,
            told_to_disregard,
        } => {
            let sessions = |sessions: &[String]| {
                if sessions.is_empty() {
                    "none".to_owned()
                } else {
                    sessions.join(", ")
                }
            };
            println!(
                "Withdrew {message_id}. Not delivered to: {}. Told to disregard it at their next briefing: {}.",
                sessions(withdrawn_unread),
                sessions(told_to_disregard)
            );
        }
        // Scans, hosts, checkouts and uncoordinated changes
        Outcome::ScanObserved {
            workspace_id,
            failed,
        } => println!("Scan of {workspace_id} recorded; failed: {failed}."),
        Outcome::HostsObserved { not_installed } => {
            println!("Hosts not installed: {}.", not_installed.join(", "))
        }
        Outcome::CheckoutObserved { change } => println!(
            "Checkout change recorded for {}: {}.",
            change.workspace_id,
            crate::briefing::render::checkout_text(
                change.from_branch.as_deref(),
                change.to_branch.as_deref(),
                change.from_head.as_deref(),
                change.to_head.as_deref(),
                change.files
            )
        ),
        Outcome::CheckoutRunObserved { run } => println!(
            "Checkout change by session {} recorded for {}.",
            run.session_id, run.workspace_id
        ),
        Outcome::UncoordinatedObserved { sessions, paths } => println!(
            "Uncoordinated change recorded: sessions {}; files {}.",
            sessions.join(", "),
            paths.join(", ")
        ),
        // Recaps, trails and reports
        Outcome::Recap { recap } => print_recap(recap)?,
        Outcome::Trails {
            workspace_id,
            trails,
            omitted,
        } => {
            if trails.is_empty() {
                println!("No trails recorded in workspace {workspace_id}.");
            }
            if *omitted > 0 {
                println!("({omitted} older trail(s) left out: they did not fit in one reply.)");
            }
            for view in trails {
                println!("{}", crate::trail::text(view));
                match &view.trail.taken_by {
                    Some(session) => println!("  Told to session {session}."),
                    None => println!("  Not told to any session yet."),
                }
            }
        }
        Outcome::Recall { recall } => print_recall(recall)?,
        Outcome::History { history } => print_history(history)?,
        Outcome::Today { today } => {
            println!(
                "{}",
                crate::today::text(today, &crate::today::shown(&today.window), None)
            );
        }
        Outcome::Report { report } => println!("{}", crate::report::text(report)),
        Outcome::Count { count } => println!(
            "{} refusals from {} to {} (UTC).",
            count.refusals,
            utc(count.from_ms),
            utc(count.to_ms)
        ),
        Outcome::StatusLine { line } => println!("{}", line.text()),
        // Refusals, the conflict policy and waits
        Outcome::RefusalRecorded { session_id } => {
            println!("Refusal recorded for session {session_id}.")
        }
        Outcome::Policy { workspace_id, rows } => {
            println!("Coordination policies for workspace {workspace_id}:");
            for row in rows {
                let wait = if row.wait_ms > 0 {
                    format!(" up to {} ms", row.wait_ms)
                } else {
                    String::new()
                };
                let fixed = if row.configurable { "" } else { " (fixed)" };
                println!(
                    "  {:<18} {}{wait}{fixed}",
                    row.case.name(),
                    row.policy.name()
                );
            }
        }
        Outcome::Holders { holders } => {
            println!("{} current holder(s).", holders.len());
        }
        Outcome::WaitRecorded { session_id } => {
            println!("Wait recorded for session {session_id}.")
        }
        // The dashboard and the daemon's version
        // The view is for the dashboard page; a terminal gets it as data.
        Outcome::Dashboard { dashboard } => println!("{}", serde_json::to_string(dashboard)?),
        Outcome::Version { version, pid } => {
            println!("Coordinator version {version}, process {pid}.")
        }
        // The source graph and write checks
        Outcome::Graph {
            symbols,
            references,
            coverage,
            truncated,
            levels,
            renamed,
            ..
        } => print_graph(symbols, references, coverage, truncated, levels, renamed)?,
        Outcome::GraphContext { matches, coverage } => {
            for context in matches {
                for symbol in &context.definitions {
                    println!(
                        "{}:{} {} {} — {}",
                        symbol.path, symbol.line, symbol.kind, symbol.name, symbol.signature
                    );
                }
                println!(
                    "{} is mentioned in: {}{}",
                    context.name,
                    context.mentioned_in.join(", "),
                    if context.truncated { " (and more)" } else { "" }
                );
            }
            println!(
                "Matches are by name, not resolved. Index: {} of {} supported files at their current content.",
                coverage.current_files, coverage.indexable_files
            );
        }
        Outcome::GraphInflight {
            inflight,
            coverage,
            truncated,
        } => print_graph_inflight(inflight, coverage, truncated),
        Outcome::GraphPath {
            from,
            to,
            max_hops,
            paths,
            coverage,
            truncated,
            ..
        } => print_paths((from, to, *max_hops), paths, coverage, *truncated),
        Outcome::GraphMap {
            depth,
            areas,
            edges,
            coverage,
            truncated,
        } => print_map(*depth, areas, edges, coverage, *truncated),
        Outcome::GraphExported {
            workspace_id,
            files,
            partial_files,
            bytes,
        } => println!(
            "Exported {files} files ({partial_files} partial, {bytes} bytes) from workspace {workspace_id}."
        ),
        Outcome::GraphImported {
            workspace_id,
            applied,
            loaded,
            different,
            kept,
            over_limits,
            malformed,
        } => print_graph_imported(
            workspace_id,
            applied,
            loaded,
            different,
            kept,
            over_limits,
            malformed,
        ),
        Outcome::CallBreaks { ready: false, .. } => {
            println!("Not checked: the source index has no complete scan of this workspace yet.")
        }
        Outcome::CallBreaks { breaks, .. } => {
            for found in breaks {
                println!(
                    "{}:{} {} passes {} argument(s); {} in {} changed by {}: {}",
                    found.call.path,
                    found.call.line,
                    found.call.name,
                    found.call.args,
                    found.call.name,
                    found.defined_in,
                    found.by_session,
                    found.after
                );
            }
            println!(
                "{} call(s) that may not fit; matched by name, not resolved.",
                breaks.len()
            );
        }
        Outcome::WriteCheck { check } => print_write_check(check)?,
        Outcome::GraphBlindSpots {
            blind_spots,
            coverage,
            scan_failure,
            truncated,
        } => print_blind_spots(blind_spots, coverage, scan_failure.as_ref(), *truncated),
        // Interface notices, outlines and what is told between steps
        Outcome::InterfaceAcknowledged {
            session_id,
            notice_ids,
        } => println!(
            "Session {session_id} will not be alerted again to {} notice(s).",
            notice_ids.len()
        ),
        Outcome::ReadOutline {
            outline,
            definitions,
            skipped,
        } => match (outline, skipped) {
            (Some(outline), _) => println!(
                "Outlined: {} of {} definition(s).",
                outline.len(),
                definitions.max(&(outline.len() as u32))
            ),
            (None, why) => println!("Not outlined ({}).", why.as_deref().unwrap_or("unknown")),
        },
        Outcome::BetweenSteps {
            session_id,
            open,
            interface_changes,
            ..
        } => println!(
            "Session {session_id}: {} interface notice(s) told between steps; turn {}.",
            interface_changes.len(),
            if *open { "open" } else { "over" }
        ),
        // Errors
        Outcome::Error { code, message } => println!("{code}: {}", serde_json::to_string(message)?),
    }
    Ok(())
}
