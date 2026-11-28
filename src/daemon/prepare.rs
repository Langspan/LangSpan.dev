//! Command preparation: resolving roots, file keys, regions and overlaps,
//! then rewriting a command into its verified or prepared form before the
//! core decides.

use super::unix::{LIVENESS_COALESCE_MS, now_ms};
use langspan_core::{Command, ErrorCode, State};
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

/// The other workspaces whose roots overlap `canonical` as distinct
/// linked worktrees of one repository. Any other overlap is refused.
/// A registered root is kept as given, so it is compared both as given
/// and resolved: a root that is now a link to `canonical` overlaps it.
fn verified_overlaps(
    state: &State,
    workspace_id: &str,
    canonical: &Path,
    deadline: Instant,
) -> Result<Vec<String>, String> {
    let mut verified = Vec::new();
    for old in state.workspaces.values() {
        let given = Path::new(&old.root);
        let resolved = fs::canonicalize(given).ok();
        let overlaps = |other: &Path| canonical.starts_with(other) || other.starts_with(canonical);
        if old.workspace_id != workspace_id
            && (overlaps(given) || resolved.as_deref().is_some_and(overlaps))
        {
            let other = resolved.as_deref().unwrap_or(given);
            let linked = crate::git::identity(canonical, deadline)
                .and_then(|a| crate::git::identity(other, deadline).map(|b| (a, b)))
                .is_ok_and(|(a, b)| {
                    a.root == canonical
                        && b.root == other
                        && a.root != b.root
                        && a.common_dir == b.common_dir
                });
            if !linked {
                return Err(
                    "workspace root overlaps an existing physical workspace; reuse its ID".into(),
                );
            }
            verified.push(old.workspace_id.clone());
        }
    }
    Ok(verified)
}

/// The checks `workspace move` makes before the core decides (roadmap
/// G6), each refusing with what failed and changing nothing: NEW_ROOT is
/// the top of a Git checkout; without `switch` the old root is gone;
/// with it the old root is still there and its `HEAD` commit is in
/// NEW_ROOT's object database, so both are checkouts of one repository.
/// Read-only Git, bounded by one deadline.
pub(super) fn prepare_move(
    state: &State,
    workspace_id: &str,
    root: &str,
    switch: bool,
) -> Result<Command, (ErrorCode, String)> {
    let Some(registered) = state.workspaces.get(workspace_id) else {
        // The core refuses an unknown ID in its own words.
        return Ok(Command::WorkspaceMoveVerified {
            workspace_id: workspace_id.into(),
            root: root.into(),
            verified_overlaps: Vec::new(),
        });
    };
    let invalid = |message: String| (ErrorCode::InvalidRoot, message);
    if root.len() > 4096 {
        return Err(invalid("NEW_ROOT exceeds 4096 bytes".into()));
    }
    let canonical = fs::canonicalize(root)
        .map_err(|e| invalid(format!("cannot resolve NEW_ROOT {root}: {e}")))?;
    if !canonical.is_dir() {
        return Err(invalid(format!(
            "NEW_ROOT {} is not a directory",
            canonical.display()
        )));
    }
    let text = canonical
        .to_str()
        .ok_or_else(|| invalid("NEW_ROOT must be UTF-8".into()))?
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(1);
    let not_top = |detail: String| {
        (
            ErrorCode::NotCheckoutTop,
            format!("{detail}; give the top directory of the checkout. Nothing changed"),
        )
    };
    let identity = crate::git::identity(&canonical, deadline).map_err(|_| {
        not_top(format!(
            "NEW_ROOT {} is not inside a Git checkout",
            canonical.display()
        ))
    })?;
    if identity.root != canonical {
        return Err(not_top(format!(
            "NEW_ROOT {} is inside the Git checkout at {}, not its top",
            canonical.display(),
            identity.root.display()
        )));
    }
    let old = Path::new(&registered.root);
    let old_exists = match fs::symlink_metadata(old) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(invalid(format!(
                "cannot tell whether the old root {} still exists: {error}",
                old.display()
            )));
        }
    };
    // The old root still resolves to NEW_ROOT, as when a link was left
    // where the checkout used to be: the same checkout, moved.
    // With or without --switch, there is nothing else to tell apart.
    let same = old_exists && crate::observer::same_root(old, &canonical);
    match (switch, old_exists) {
        _ if same => {}
        (false, true) => {
            // A link at the old root, or on the way to it, names the
            // directory it now leads to.
            let link = match fs::canonicalize(old) {
                Ok(target) if target != old => format!(
                    ": it is reached through a symbolic link and resolves to {}, not to NEW_ROOT",
                    target.display()
                ),
                Err(_) if old.is_symlink() => {
                    ": it is a symbolic link whose target cannot be resolved".into()
                }
                _ => String::new(),
            };
            return Err((
                ErrorCode::OldRootExists,
                format!(
                    "The old root {} still exists{link}. To coordinate the checkout at NEW_ROOT under {workspace_id} instead, add --switch; to coordinate both, register NEW_ROOT with `langspan workspace add`. Nothing changed",
                    old.display()
                ),
            ));
        }
        (true, false) => {
            return Err((
                ErrorCode::OldRootMissing,
                format!(
                    "The old root {} no longer exists; for a checkout that was moved, run without --switch. Nothing changed",
                    old.display()
                ),
            ));
        }
        // User-level hooks derive this ID from the old root, so every
        // new session there would claim it again and be refused.
        (true, true) if workspace_id == crate::ids::user_workspace_id(old) => {
            return Err((
                ErrorCode::RootDerivedId,
                format!(
                    "Workspace {workspace_id} has the ID user-level hooks derive from {}, so new sessions there would still claim it. To coordinate NEW_ROOT under this ID, move or delete that checkout first, then run this again without --switch; to coordinate both, register NEW_ROOT with `langspan workspace add`. Nothing changed",
                    old.display()
                ),
            ));
        }
        (true, true) => same_repository(old, &canonical, deadline)?,
        (false, false) => {}
    }
    let verified_overlaps = verified_overlaps(state, workspace_id, &canonical, deadline)
        .map_err(|message| (ErrorCode::WorkspaceConflict, message))?;
    Ok(Command::WorkspaceMoveVerified {
        workspace_id: workspace_id.into(),
        root: text,
        verified_overlaps,
    })
}

/// `old`'s `HEAD` commit is in `new`'s object database.
fn same_repository(old: &Path, new: &Path, deadline: Instant) -> Result<(), (ErrorCode, String)> {
    let refused = |detail: String| {
        (
            ErrorCode::DifferentRepository,
            format!("{detail}. Nothing changed"),
        )
    };
    // As the observer runs Git: no fsmonitor program, no replace refs.
    const SAFE: [&str; 3] = ["--no-replace-objects", "-c", "core.fsmonitor=false"];
    let head = crate::git::output(
        old,
        &[
            &SAFE[..],
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        ]
        .concat(),
        deadline,
        256,
    )
    .ok()
    .and_then(|bytes| String::from_utf8(bytes).ok())
    .map(|text| text.trim().to_owned())
    .filter(|oid| matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit()))
    .ok_or_else(|| {
        refused(format!(
            "The old root {} has no commit to compare with NEW_ROOT; --switch needs one",
            old.display()
        ))
    })?;
    let found = crate::observer::holds_commit(new, &head, deadline)
        .map_err(|error| refused(format!("cannot read NEW_ROOT's objects: {error}")))?;
    if found {
        Ok(())
    } else {
        Err(refused(format!(
            "NEW_ROOT {} does not hold the old root's HEAD commit {head}, so it is not a checkout of the same repository",
            new.display()
        )))
    }
}

pub(super) fn prepare(state: &State, command: &Command) -> Result<Command, crate::Error> {
    match command {
        Command::WorkspaceRegister { workspace_id, root } => {
            if root.len() > 4096 {
                return Err("workspace root exceeds 4096 bytes".into());
            }
            let canonical = fs::canonicalize(root)
                .map_err(|e| format!("cannot resolve workspace root: {e}"))?;
            if !canonical.is_dir() {
                return Err("workspace root must be a directory".into());
            }
            // The registered root reached another way, such as through a
            // link left where the checkout used to be: the same
            // registration, kept as given.
            if let Some(existing) = state.workspaces.get(workspace_id)
                && crate::observer::same_root(Path::new(&existing.root), &canonical)
            {
                return Ok(Command::WorkspaceRegisterVerified {
                    workspace_id: workspace_id.clone(),
                    root: existing.root.clone(),
                    verified_overlaps: existing.verified_overlaps.clone(),
                });
            }
            let deadline = Instant::now() + Duration::from_millis(250);
            let verified_overlaps = verified_overlaps(state, workspace_id, &canonical, deadline)?;
            Ok(Command::WorkspaceRegisterVerified {
                workspace_id: workspace_id.clone(),
                root: canonical
                    .to_str()
                    .ok_or("workspace root must be UTF-8")?
                    .into(),
                verified_overlaps,
            })
        }
        Command::IntentSet {
            session_id,
            paths,
            summary,
        } => Ok(Command::IntentSet {
            session_id: session_id.clone(),
            paths: prepare_paths(state, session_id, paths)?,
            summary: summary.clone(),
        }),
        Command::Admit {
            session_id,
            paths,
            ttl_ms,
        } => Ok(Command::Admit {
            session_id: session_id.clone(),
            paths: prepare_paths(state, session_id, paths)?,
            ttl_ms: *ttl_ms,
        }),
        Command::AdmitTool {
            session_id,
            paths,
            ttl_ms,
            operation,
            base,
            regions,
            overwrites,
        } => Ok(Command::AdmitTool {
            session_id: session_id.clone(),
            paths: prepare_paths(state, session_id, paths)?,
            ttl_ms: *ttl_ms,
            operation: operation.clone(),
            base: prepare_files(state, session_id, base)?,
            regions: prepare_regions(state, session_id, regions)?,
            overwrites: prepare_overwrites(state, session_id, overwrites)?,
        }),
        Command::Holders {
            session_id,
            paths,
            base,
            regions,
        } => Ok(Command::Holders {
            session_id: session_id.clone(),
            paths: prepare_paths(state, session_id, paths)?,
            base: prepare_files(state, session_id, base)?,
            regions: prepare_regions(state, session_id, regions)?,
        }),
        Command::RecordWait {
            session_id,
            paths,
            holder_session_id,
            waited_ms,
            result,
            changed,
            input_digest,
        } => Ok(Command::RecordWait {
            session_id: session_id.clone(),
            paths: prepare_paths(state, session_id, paths)?,
            holder_session_id: holder_session_id.clone(),
            waited_ms: *waited_ms,
            result: *result,
            changed: prepare_files(state, session_id, changed)?,
            input_digest: input_digest.clone(),
        }),
        Command::ObserveSource {
            session_id,
            files,
            origin,
        } => Ok(Command::ObserveSource {
            session_id: session_id.clone(),
            files: prepare_files(state, session_id, files)?,
            origin: *origin,
        }),
        Command::Observe {
            session_id,
            operation_id,
            source,
            tool,
            paths,
            result,
            lease_id,
        } => Ok(Command::Observe {
            session_id: session_id.clone(),
            operation_id: operation_id.clone(),
            source: source.clone(),
            tool: tool.clone(),
            paths: prepare_paths(state, session_id, paths)?,
            result: *result,
            lease_id: lease_id.clone(),
        }),
        Command::ObserveTool {
            session_id,
            operation,
            paths,
            result,
            lease_id,
        } => Ok(Command::ObserveTool {
            session_id: session_id.clone(),
            operation: operation.clone(),
            paths: prepare_paths(state, session_id, paths)?,
            result: *result,
            lease_id: lease_id.clone(),
        }),
        Command::ObserveSessionLiveness {
            session_id,
            process,
            ..
        } => Ok(Command::ObserveSessionLiveness {
            session_id: session_id.clone(),
            process: process.clone(),
            // The same incarnation seen alive within the poll's
            // coalescing window is not probed again on the writer: on
            // macOS each probe starts a process listing.
            status: match state.session_liveness.get(session_id) {
                Some(prior)
                    if prior.process == *process
                        && prior.generation == state.generation
                        && prior.status == langspan_core::LivenessStatus::Alive
                        && now_ms().saturating_sub(prior.observed_at_ms) < LIVENESS_COALESCE_MS =>
                {
                    langspan_core::LivenessStatus::Alive
                }
                _ => crate::process::probe(process),
            },
        }),
        Command::StatusLine { root, from_ms: ms } => Ok(crate::status_line::resolved(root, *ms)),
        _ => Ok(command.clone()),
    }
}
/// Each observed version is keyed by the same canonical file key an
/// admission of that path would use.
fn prepare_files(
    state: &State,
    session_id: &str,
    files: &[langspan_core::ObservedFile],
) -> Result<Vec<langspan_core::ObservedFile>, crate::Error> {
    if files.len() > 64 {
        return Err("supply at most 64 observed file versions".into());
    }
    files
        .iter()
        .map(|file| {
            let key = prepare_paths(state, session_id, std::slice::from_ref(&file.path))?
                .pop()
                .ok_or("observed file has no key")?;
            Ok(langspan_core::ObservedFile {
                path: key,
                state: file.state.clone(),
            })
        })
        .collect()
}

/// Regions name their path as the hook did; map it through the same
/// canonical file-key checks, so it matches the admitted key.
fn prepare_regions(
    state: &State,
    session_id: &str,
    regions: &[langspan_core::PathRegions],
) -> Result<Vec<langspan_core::PathRegions>, crate::Error> {
    if regions.len() > 64 {
        return Err("supply regions for at most 64 files".into());
    }
    regions
        .iter()
        .map(|located| {
            let key = prepare_paths(state, session_id, std::slice::from_ref(&located.path))?
                .pop()
                .ok_or("located file has no key")?;
            Ok(langspan_core::PathRegions {
                path: key,
                ..located.clone()
            })
        })
        .collect()
}

/// Written lines name their path as the hook did, like regions.
fn prepare_overwrites(
    state: &State,
    session_id: &str,
    overwrites: &[langspan_core::WrittenLines],
) -> Result<Vec<langspan_core::WrittenLines>, crate::Error> {
    if overwrites.len() > langspan_core::MAX_WRITTEN {
        return Err("supply at most 64 written-lines entries".into());
    }
    overwrites
        .iter()
        .map(|entry| {
            let key = prepare_paths(state, session_id, std::slice::from_ref(&entry.path))?
                .pop()
                .ok_or("written file has no key")?;
            Ok(langspan_core::WrittenLines {
                path: key,
                ..entry.clone()
            })
        })
        .collect()
}

fn prepare_paths(
    state: &State,
    session_id: &str,
    paths: &[String],
) -> Result<Vec<String>, crate::Error> {
    let paths: std::collections::BTreeSet<_> = paths.iter().collect();
    if paths.is_empty() || paths.len() > 64 {
        return Err("supply 1..64 relative file paths".into());
    }
    let Some(session) = state.sessions.get(session_id) else {
        return Ok(paths.into_iter().cloned().collect());
    };
    let Some(workspace) = state.workspaces.get(&session.workspace_id) else {
        return Err("session workspace is missing".into());
    };
    let root = Path::new(&workspace.root);
    // A root reached through a link is the checkout the link leads to,
    // unless the scan found that to be another repository.
    let current_root = fs::canonicalize(root)?;
    if current_root != root
        && state
            .scan_failures
            .get(&session.workspace_id)
            .is_some_and(|failure| failure.detail == crate::store::IDENTITY_CHANGED)
    {
        return Err("workspace root identity changed; use a new workspace registration".into());
    }
    // Other registered roots, resolved like this one, so a root behind
    // a link still bounds its own files. A root that no longer resolves
    // keeps its stored form.
    let others: Vec<std::path::PathBuf> = state
        .workspaces
        .iter()
        .filter(|(id, _)| **id != session.workspace_id)
        .map(|(_, workspace)| {
            let stored = Path::new(&workspace.root);
            fs::canonicalize(stored).unwrap_or_else(|_| stored.to_path_buf())
        })
        .collect();
    // Another workspace ID that now resolves to this very root, as when
    // a registered clone's root was replaced by a link to another
    // registered clone: both would admit the same physical files while
    // the core keeps their leases apart, so neither admits any.
    if others.contains(&current_root) {
        return Err("workspace root now resolves to another registered workspace's root; move or remove one of them".into());
    }
    let mut keys = paths
            .iter()
            .map(|path| -> Result<String, crate::Error> {
                // Resolved paths are compared with the resolved root.
                let key = file_key(&current_root, path)?;
                let full = current_root.join(&key);
                if others.iter().any(|descendant| {
                    *descendant != current_root
                        && descendant.starts_with(&current_root)
                        && full.starts_with(descendant)
                }) {
                    return Err("file belongs to a registered descendant workspace; attach that physical root separately".into());
                }
                Ok(key)
            })
            .collect::<Result<Vec<_>, _>>()?;
    keys.sort();
    keys.dedup();
    Ok(keys)
}
fn file_key(root: &Path, key: &str) -> Result<String, crate::Error> {
    if key.is_empty()
        || key.len() > 512
        || key.contains(['\\', '\0'])
        || key
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(
            "file keys must be normalized relative paths of 1..512 bytes without traversal".into(),
        );
    }
    let full = root.join(key);
    let mut ancestor = full.clone();
    let mut suffix = Vec::new();
    let metadata = loop {
        match fs::symlink_metadata(&ancestor) {
            Ok(metadata) => break metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or("cannot resolve file parent")?
                        .to_owned(),
                );
                if !ancestor.pop() {
                    return Err("cannot resolve file parent".into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    };
    let mut resolved = fs::canonicalize(&ancestor)?;
    if !resolved.starts_with(root) {
        return Err("resolved file path leaves the registered workspace".into());
    }
    let target = fs::metadata(&resolved)?;
    if suffix.is_empty() {
        if !target.is_file() || target.nlink() > 1 {
            return Err("admission supports regular files with a single hard link".into());
        }
    } else if !target.is_dir() || (!metadata.is_dir() && !metadata.file_type().is_symlink()) {
        return Err("new file parent must be a directory".into());
    }
    for part in suffix.iter().rev() {
        resolved.push(part);
    }
    if resolved.parent().is_some_and(|parent| {
        parent
            .ancestors()
            .take_while(|directory| *directory != root)
            .any(|directory| directory.join(".git").try_exists().unwrap_or(true))
    }) {
        return Err(
            "file belongs to a nested Git workspace; attach that physical root separately".into(),
        );
    }
    let relative = resolved
        .strip_prefix(root)?
        .to_str()
        .ok_or("file path must be UTF-8")?
        .to_string();
    if relative.len() > 512 {
        return Err("resolved file key exceeds 512 bytes".into());
    }
    Ok(relative)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn a_registered_child_boundary_survives_removal_of_its_git_marker() {
        let directory =
            std::env::temp_dir().join(format!("langspan-worktree-boundary-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let root = fs::canonicalize(&directory).unwrap();
        let child = root.join("nested");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("a.rs"), "fixture").unwrap();
        let mut state = State::default();
        for (id, path, links) in [
            ("parent", &root, vec![]),
            ("child", &child, vec!["parent".into()]),
        ] {
            state.workspaces.insert(
                id.into(),
                langspan_core::Workspace {
                    workspace_id: id.into(),
                    root: path.to_str().unwrap().into(),
                    verified_overlaps: links,
                },
            );
            state.sessions.insert(
                id.into(),
                langspan_core::Session {
                    session_id: id.into(),
                    workspace_id: id.into(),
                    generation: 0,
                    parent: None,
                    headless: false,
                },
            );
        }
        let parent = prepare_paths(&state, "parent", &["nested/a.rs".into()]);
        let own_child = prepare_paths(&state, "child", &["a.rs".into()]);
        fs::remove_dir_all(directory).unwrap();
        assert!(
            parent
                .unwrap_err()
                .to_string()
                .contains("registered descendant")
        );
        assert_eq!(own_child.unwrap(), ["a.rs"]);
    }
}
