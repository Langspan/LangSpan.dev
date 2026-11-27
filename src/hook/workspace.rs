use super::*;

/// A linked worktree has a distinct physical identity, even when it inherits hooks.
/// A user-level hook takes its workspace from the event's working directory.
pub fn resolve_workspace(
    config: &Config,
    cwd: Option<&str>,
    deadline: Instant,
) -> Result<Config, Reply> {
    let unsupported = |detail: String| Reply::none("unsupported", detail);
    if config.user_level {
        return resolve_user_workspace(config.vendor, config, cwd, deadline);
    }
    let anchor = fs::canonicalize(&config.root)
        .map_err(|e| unsupported(format!("cannot resolve installed root: {e}")))?;
    let mut resolved = config.clone();
    resolved.root = anchor.clone();
    let Some(cwd) = cwd else { return Ok(resolved) };
    if cwd.len() > 4096 {
        return Err(unsupported("cwd exceeds 4096 bytes".into()));
    }
    let cwd =
        fs::canonicalize(cwd).map_err(|e| unsupported(format!("cannot resolve host cwd: {e}")))?;
    // Linked worktrees can live under the checkout (for example .worktrees/x).
    // A nested Git boundary wins over lexical containment.
    let nested_boundary = cwd
        .ancestors()
        .take_while(|p| *p != anchor)
        .any(|p| p.join(".git").try_exists().unwrap_or(true));
    if cwd.starts_with(&anchor) && !nested_boundary {
        return Ok(resolved);
    }
    let own = worktree_install(config, &anchor, &cwd);
    // Step aside only where the host is known to run the worktree's own
    // install for this same event. Otherwise this hook may be the only one
    // the host runs there (Codex), or that is not known: it decides.
    if let Some((file, _)) = &own
        && config.vendor.worktree_hooks() == crate::install::WorktreeHooks::Both
    {
        return Err(Reply::none(
            "deferred_to_worktree",
            format!(
                "the LangSpan hook installed in {} covers this worktree",
                file.display()
            ),
        ));
    }
    let anchor_git = crate::git::identity(&anchor, deadline).map_err(unsupported)?;
    let current_git = crate::git::identity(&cwd, deadline).map_err(unsupported)?;
    if anchor_git.common_dir != current_git.common_dir {
        return Err(unsupported(
            "host cwd is outside the installed repository and its linked worktrees".into(),
        ));
    }
    match own {
        // Decide as the worktree's own install would, so this session is
        // coordinated in the same workspace as sessions that install covers,
        // and a host that runs both reaches one decision. An opt-out in
        // either install holds.
        Some((_, registration)) if registration.root == current_git.root => {
            resolved.workspace_id = registration.workspace_id;
            resolved.share_prompt &= registration.share_prompt;
            resolved.code_briefing &= registration.code_briefing;
            resolved.presence &= registration.presence.shown;
            resolved.outline_reads &= registration.outline_reads;
            resolved.presence_window_ms = registration.presence.window_ms();
        }
        _ => {
            resolved.workspace_id = wire_id(
                "worktree",
                &[&config.workspace_id, &current_git.root.to_string_lossy()],
            );
        }
    }
    resolved.root = current_git.root;
    Ok(resolved)
}

/// The project file of the worktree holding `cwd` that has a LangSpan hook of
/// its own for this host and state directory, when that worktree is not the
/// installed root, with that hook's registration (its root canonical). A
/// host started in a linked worktree may run the main checkout's project
/// hooks as well as, or instead of, the worktree's own
/// (`Vendor::worktree_hooks`). Only files are read: no Git process and no
/// coordinator call. The worktree is the nearest directory holding `.git`.
/// Anything that cannot be read is not a match, and the event is handled as
/// before.
fn worktree_install(
    config: &Config,
    anchor: &Path,
    cwd: &Path,
) -> Option<(PathBuf, crate::install::Registration)> {
    let worktree = cwd
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())?;
    if worktree == anchor {
        return None;
    }
    install_at(config.vendor, &config.state_dir, worktree)
}

/// The project file in `dir` (canonical) holding a LangSpan hook for
/// `vendor` that names `dir` as its root and `state_dir`, with that hook's
/// registration, its root canonical. Also used by `doctor`.
pub fn install_at(
    vendor: crate::install::Vendor,
    state_dir: &Path,
    dir: &Path,
) -> Option<(PathBuf, crate::install::Registration)> {
    let canonical = |dir: &Path| fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    vendor.project_hook_files().iter().find_map(|file| {
        let path = dir.join(file);
        let Ok(Some(settings)) = crate::install::read(&path) else {
            return None;
        };
        crate::install::managed_hooks(&settings, |_| true)
            .iter()
            .filter_map(|hook| crate::install::parse_project_hook(vendor, hook))
            .map(|mut registration| {
                registration.root = canonical(&registration.root);
                registration
            })
            .find(|registration| {
                registration.root == dir
                    && crate::install::same_directory(&registration.state_dir, state_dir)
            })
            .map(|registration| (path, registration))
    })
}

fn resolve_user_workspace(
    vendor: crate::install::Vendor,
    config: &Config,
    cwd: Option<&str>,
    deadline: Instant,
) -> Result<Config, Reply> {
    let unsupported = |detail: String| Reply::none("unsupported", detail);
    let cwd = cwd
        .filter(|cwd| !cwd.is_empty())
        .ok_or_else(|| unsupported("a user-level hook needs the event's cwd".into()))?;
    if cwd.len() > 4096 {
        return Err(unsupported("cwd exceeds 4096 bytes".into()));
    }
    let cwd =
        fs::canonicalize(cwd).map_err(|e| unsupported(format!("cannot resolve host cwd: {e}")))?;
    let git = crate::git::identity(&cwd, deadline)
        .map_err(|_| unsupported("host cwd is not inside a Git repository".into()))?;
    if let Some(file) = project_install(vendor, &cwd, &git)? {
        return Err(Reply::none(
            "deferred_to_project",
            format!(
                "a project-level LangSpan hook in {} covers this repository",
                file.display()
            ),
        ));
    }
    let mut resolved = config.clone();
    resolved.workspace_id = adopted_workspace_id(config, &git.root, deadline)
        .unwrap_or_else(|| user_workspace_id(&git.root));
    resolved.root = git.root;
    Ok(resolved)
}

/// The ID this root is already registered under, if the coordinator has it
/// under another one. Any failure keeps the derived ID; the later
/// registration then reports the problem in the usual way.
fn adopted_workspace_id(config: &Config, root: &Path, deadline: Instant) -> Option<String> {
    let derived = user_workspace_id(root);
    let root_text = root.to_string_lossy().into_owned();
    let response = client::call_until(
        &config.state_dir,
        &Request::new(
            wire_id("workspace", &[&derived, &root_text]),
            Command::WorkspaceRegister {
                workspace_id: derived,
                root: root_text,
            },
        ),
        deadline,
    )
    .ok()?;
    match response.outcome {
        Outcome::Workspace { workspace } => Some(workspace.workspace_id),
        _ => None,
    }
}

/// The project file holding a LangSpan project-level hook for this vendor
/// that the host would also run here, if there is one. Looked for at every
/// directory from `cwd` up to the repository root, at the main checkout (where
/// Claude Code keeps a worktree's local settings) and, for Claude Code, at the
/// directory the session started in; for Antigravity, only at `cwd`. A file
/// that cannot be read safely means no decision, not a guess.
fn project_install(
    vendor: crate::install::Vendor,
    cwd: &Path,
    git: &crate::git::Identity,
) -> Result<Option<PathBuf>, Reply> {
    // Antigravity reads `.agents/hooks.json` at the workspace root, which
    // its adapter passes as the cwd.
    if vendor == crate::install::Vendor::Antigravity {
        let path = cwd.join(vendor.project_hook_files()[0]);
        return covered(vendor, path);
    }
    let mut roots: Vec<PathBuf> = cwd
        .ancestors()
        .take_while(|dir| dir.starts_with(&git.root))
        .map(Path::to_path_buf)
        .collect();
    if git.common_dir.file_name().and_then(|n| n.to_str()) == Some(".git")
        && let Some(main) = git.common_dir.parent()
    {
        roots.push(main.to_path_buf());
    }
    if vendor == crate::install::Vendor::ClaudeCode
        && let Some(dir) = std::env::var_os("CLAUDE_PROJECT_DIR")
    {
        roots.push(dir.into());
    }
    for root in roots {
        for file in vendor.project_hook_files() {
            if let Some(path) = covered(vendor, root.join(file))? {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

/// `path`, if it holds a LangSpan project hook for `vendor` that the host
/// runs. A definition switched off as a whole runs none of its hooks, so it
/// covers nothing and the user-level hook decides.
fn covered(vendor: crate::install::Vendor, path: PathBuf) -> Result<Option<PathBuf>, Reply> {
    let settings = crate::install::read(&path).map_err(|error| {
        Reply::none(
            "unsupported",
            format!("cannot tell whether a project install covers this repository: {error}"),
        )
    })?;
    Ok(settings
        .filter(|settings| !crate::install::switched_off(vendor, settings))
        .filter(|settings| {
            !crate::install::managed_hooks(settings, |hook| {
                crate::install::is_managed_for(vendor, hook)
            })
            .is_empty()
        })
        .map(|_| path))
}
