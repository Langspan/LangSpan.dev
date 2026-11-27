//! State, the event log and Git state.

use super::workspaces::context_line;
use langspan_core::{LeaseStatus, State, StatusSummary};

pub(super) fn print_status(
    state: &State,
    summary: &Option<StatusSummary>,
    state_dir: &std::path::Path,
) -> Result<(), crate::Error> {
    println!("LangSpan generation {}", state.generation);
    for workspace in state.workspaces.values() {
        println!("Workspace {}: {:?}", workspace.workspace_id, workspace.root);
    }
    for session in state.sessions.values() {
        println!(
            "Session {} in {}: {}",
            session.session_id,
            session.workspace_id,
            if session.generation == state.generation {
                "registered in this generation"
            } else {
                "stale; reattach with a new request ID"
            }
        );
        match state.session_liveness.get(&session.session_id) {
            Some(liveness) => println!(
                "  Process {}: {:?} (observation generation {}, {} ms).",
                liveness.process.pid, liveness.status, liveness.generation, liveness.observed_at_ms
            ),
            None => {
                println!("  Process liveness: unknown; registration is not a heartbeat.")
            }
        }
        match summary
            .as_ref()
            .and_then(|summary| summary.last_hook_age_ms.get(&session.session_id))
        {
            Some(age) => println!("  Last hook run: {} s ago.", age / 1000),
            None => println!("  Last hook run: none recorded in this generation."),
        }
        if let Some(activity) = state
            .activity
            .get(&session.session_id)
            .filter(|activity| activity.generation == state.generation)
            .filter(|activity| !activity.context.is_empty())
        {
            println!("  {}", context_line(&activity.context));
        }
    }
    for adoption in state.adoptions.values() {
        println!(
            "Adoption: session {} took over the uncommitted work of ended session {}.",
            adoption.session_id, adoption.from
        );
    }
    for intent in state.intents.values() {
        println!(
            "Intent {} [{}]: {} {:?}",
            intent.session_id,
            if intent.generation == state.generation {
                "current"
            } else {
                "stale"
            },
            serde_json::to_string(&intent.summary)?,
            intent.paths
        );
    }
    for lease in state.leases.values() {
        println!(
            "Admission {}: {} {:?} {:?}",
            lease.lease_id, lease.session_id, lease.status, lease.paths
        );
    }
    let active = state
        .leases
        .values()
        .filter(|l| l.status == LeaseStatus::Active)
        .count();
    println!(
        "{active} active admission(s). Session registration is not a process-liveness signal."
    );
    for host in state.uninstalled_hosts.keys() {
        println!("{}", crate::install::detect::status_line(host, state_dir));
    }
    if let Some(summary) = summary {
        let closed = summary.closed_admissions.values().sum::<u64>() + summary.other_closed;
        if closed > 0 {
            let mut counts: Vec<String> = summary
                .closed_admissions
                .iter()
                .map(|(session, count)| format!("{session} {count}"))
                .collect();
            if summary.other_closed > 0 {
                counts.push(format!("other sessions {}", summary.other_closed));
            }
            println!(
                "{closed} closed admission(s), not listed: {}.",
                counts.join(", ")
            );
        }
        if summary.omitted_sessions > 0 {
            println!(
                "{} session(s) not shown: nothing open and idle for over {} minutes, or left out to fit.",
                summary.omitted_sessions,
                langspan_core::STATUS_RECENT_MS / 60_000
            );
        }
        if summary.omitted_open > 0 {
            println!(
                "{} open admission(s) left out to fit one answer.",
                summary.omitted_open
            );
        }
        if !summary.omitted.is_empty() {
            println!("Left out to fit: {}.", summary.omitted.join(", "));
        }
        if closed > 0
            || summary.omitted_sessions > 0
            || summary.omitted_open > 0
            || !summary.omitted.is_empty()
        {
            println!("Every record, history included: langspan status --all");
        }
    }
    for link in state.remote_links.values() {
        println!(
            "{} {} to {:?} at {} as {:?}; last publication {}.",
            match (link.enabled, link.withdrawn) {
                (true, _) => "Publishing",
                (false, true) => "No longer publishing",
                (false, false) =>
                    "Stopped publishing; its ref is still to be removed from the remote:",
            },
            link.workspace_id,
            link.remote,
            crate::remote::peer_ref(link),
            link.name,
            link.sequence
        );
    }
    for peer in state.remote_peers.values() {
        println!(
            "Teammate {:?} ({}): publication {}, {} session(s), received in generation {} at {} ms.",
            peer.publication.name,
            peer.publication.participant,
            peer.publication.sequence,
            peer.publication.work.len(),
            peer.generation,
            peer.received_ms
        );
    }
    Ok(())
}
