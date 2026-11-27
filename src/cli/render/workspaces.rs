//! Workspaces and sessions.

use langspan_core::{WorkspaceEnding, WorkspaceSummary};

/// After `workspace move`: the hosts whose project-level hooks, in the new
/// checkout or the old one, still name the old root for this workspace and
/// state directory, with the command that installs them for the new root.
/// Host configuration is never edited here. User-level hooks need nothing:
/// they adopt the moved ID from the new root on their next event.
pub fn print_project_hook_reruns(
    state_dir: &std::path::Path,
    workspace: &langspan_core::Workspace,
    from_root: &str,
) {
    use crate::install::{self, Vendor};
    let canonical =
        |path: &std::path::Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let state = canonical(state_dir);
    let dirs = [
        std::path::Path::new(&workspace.root),
        std::path::Path::new(from_root),
    ];
    let hosts: Vec<Vendor> = Vendor::ALL
        .into_iter()
        .filter(|vendor| {
            dirs.iter()
                .flat_map(|dir| {
                    vendor
                        .project_hook_files()
                        .iter()
                        .map(|file| dir.join(file))
                })
                .filter_map(|path| install::read(&path).ok().flatten())
                .any(|settings| {
                    install::managed_hooks(&settings, |_| true)
                        .iter()
                        .filter_map(|hook| install::parse_project_hook(*vendor, hook))
                        .any(|registration| {
                            registration.workspace_id == workspace.workspace_id
                                && canonical(&registration.state_dir) == state
                                && registration.root != std::path::Path::new(&workspace.root)
                        })
                })
        })
        .collect();
    if hosts.is_empty() {
        return;
    }
    println!("Project hooks name the old root; rerun:");
    for vendor in hosts {
        println!(
            "  langspan hooks install {} --workspace {} --root {} --state-dir {} --apply",
            vendor.name(),
            install::quote(&workspace.workspace_id),
            install::quote(&workspace.root),
            install::quote(&state.to_string_lossy()),
        );
    }
}

/// The context a session's hooks added, in bytes and estimated tokens
/// (issue #222).
pub(super) fn context_line(context: &langspan_core::ContextUse) -> String {
    let per_turn = context.bytes_per_turn();
    let tokens = langspan_core::ContextUse::estimated_tokens;
    format!(
        "Added context: {} bytes over {} turn(s), {per_turn} bytes (about {} tokens) per turn; the largest turn {} bytes (about {} tokens). Tokens are estimated at {} bytes each.",
        context.bytes,
        context.turns,
        tokens(per_turn),
        context.largest_turn_bytes,
        tokens(context.largest_turn_bytes),
        langspan_core::BYTES_PER_TOKEN
    )
}

pub(super) fn print_workspaces(workspaces: &[WorkspaceSummary], next: &Option<String>) {
    if workspaces.is_empty() {
        println!("No workspaces are registered.");
    }
    for workspace in workspaces {
        let mut extras = Vec::new();
        if workspace.auto_check {
            extras.push(", automatic check on");
        }
        if workspace.remote_sharing {
            extras.push(", remote sharing on");
        }
        println!(
            "{} {:?}{}: {} current sessions, {} notes, {} pending messages, {} check records{}",
            workspace.workspace_id,
            workspace.root,
            if workspace.root_exists {
                ""
            } else {
                " (root missing)"
            },
            workspace.sessions_current,
            workspace.notes,
            workspace.messages_pending,
            workspace.checks,
            extras.concat()
        );
    }
    if let Some(next) = next {
        println!("More: langspan workspace list --after {next}");
    }
}

pub(super) fn print_workspace_removal(
    workspace_id: &str,
    applied: &bool,
    ending: &WorkspaceEnding,
) {
    let mut sessions = ending.sessions.join(", ");
    if ending.more_sessions > 0 {
        sessions.push_str(&format!(" and {} more", ending.more_sessions));
    }
    let check = ending
        .auto_check
        .as_ref()
        .map(|name| format!(", the automatic check {name:?}"))
        .unwrap_or_default();
    let ends = format!(
        "{} sessions{}, {} intents, {} notes, {} pending messages{check}",
        ending.sessions.len() + ending.more_sessions as usize,
        if sessions.is_empty() {
            String::new()
        } else {
            format!(" ({sessions})")
        },
        ending.intents,
        ending.notes,
        ending.messages_pending,
    );
    if *applied {
        println!(
            "Workspace {workspace_id} removed. Ended: {ends}. Its history stays in `langspan events`."
        );
    } else {
        println!(
            "Dry run, nothing changed. Removing workspace {workspace_id} would end: {ends}. Its history would stay in `langspan events`. Add --apply to remove it."
        );
    }
}
