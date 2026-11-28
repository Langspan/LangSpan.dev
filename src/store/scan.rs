//! Persisting a complete Git scan and the facts derived from it.

use super::{IDENTITY_CHANGED, Store, StoreResult};
use langspan_core::{Command, Outcome, decide};
use rusqlite::OptionalExtension;

impl Store {
    /// Persist a complete read-only Git scan and its derived facts atomically.
    pub fn record_scan(
        &mut self,
        workspace_id: &str,
        snapshot: &crate::observer::Snapshot,
        now_ms: u64,
    ) -> StoreResult<usize> {
        let workspace = self
            .state
            .workspaces
            .get(workspace_id)
            .ok_or("cannot store a scan for an unregistered workspace")?;
        if workspace.root != snapshot.root
            && !crate::observer::same_root(
                std::path::Path::new(&workspace.root),
                std::path::Path::new(&snapshot.root),
            )
        {
            return Err("scan root differs from the registered physical workspace".into());
        }
        let limits = crate::observer::Limits::default();
        if snapshot.files.len() > limits.max_files || snapshot.branches.len() > limits.max_branches
        {
            return Err("complete scan exceeds the persistence bounds".into());
        }
        for (path, version) in &snapshot.files {
            let state = observed_file_state(Some(version))?;
            langspan_core::validate_observed_file(path, &state)?;
        }
        let branches = observed_branches(snapshot);
        if branches.len() != snapshot.branches.len() + 1 {
            return Err("Git snapshot contains duplicate branch identities".into());
        }
        for (name, (head_oid, tree_oid)) in &branches {
            let command = Command::ObserveBranch {
                workspace_id: workspace_id.into(),
                branch_ref: name.clone(),
                head_oid: head_oid.clone(),
                tree_oid: tree_oid.clone(),
            };
            if let Outcome::Error { code, message } =
                decide(&self.state, &command, "scan-validation", now_ms).outcome
            {
                return Err(format!("invalid complete Git snapshot: {code}: {message}").into());
            }
        }
        let snapshot_json = serde_json::to_string(snapshot)?;
        if snapshot_json.len() > 4 * 1024 * 1024 {
            return Err("serialized Git snapshot exceeds 4 MiB".into());
        }
        let previous_json: Option<String> = self
            .connection
            .query_row(
                "SELECT snapshot_json FROM git_snapshots WHERE workspace_id=?1",
                [workspace_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut previous: Option<crate::observer::Snapshot> = previous_json
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        if previous.as_ref().is_some_and(|old| {
            if old.root == snapshot.root && old.common_dir == snapshot.common_dir {
                return false;
            }
            // Reached through a link left where the checkout used to be,
            // both paths now name the same directories. A link to any other
            // checkout would too, so the checkout reached must also hold the
            // commit the previous scan saw. One bounded Git call, once per
            // change of path: the next scan compares equal paths.
            let same = |before: &str, now: &str| {
                crate::observer::same_root(std::path::Path::new(before), std::path::Path::new(now))
            };
            !(same(&old.root, &snapshot.root)
                && same(&old.common_dir, &snapshot.common_dir)
                && old.head.as_deref().is_some_and(|head| {
                    crate::observer::holds_commit(
                        std::path::Path::new(&snapshot.root),
                        head,
                        std::time::Instant::now() + std::time::Duration::from_millis(500),
                    )
                    .unwrap_or(false)
                }))
        }) {
            return Err(IDENTITY_CHANGED.into());
        }
        // Accepted as the same checkout: the previous scan stays the
        // baseline under the paths it is reached by now, so a change made
        // while it moved is still observed with its earlier version.
        if let Some(old) = previous.as_mut() {
            old.root.clone_from(&snapshot.root);
            old.common_dir.clone_from(&snapshot.common_dir);
        }
        let changes = crate::observer::diff(&previous, snapshot);
        // The sequence counter, not the newest row: retention can remove
        // every row, and a scan ID must not repeat.
        let sequence: i64 = self.connection.query_row(
            "SELECT coalesce((SELECT seq FROM sqlite_sequence WHERE name = 'events'), 0)",
            [],
            |row| row.get(0),
        )?;
        let scan_id = format!("scan-{}-{sequence}", self.state.generation);
        let mut commands = Vec::new();
        // Files whose content changed since a complete earlier scan, at their
        // new versions: the core keeps those no session reported (issue #129).
        let mut content_changes = Vec::new();
        // Several changed files back at their committed version at once,
        // with HEAD where it was: a reset, stash or checkout of files.
        let restored = changes
            .iter()
            .filter(|change| {
                change.before.as_ref().is_some_and(|before| before.dirty)
                    && change.after.as_ref().is_some_and(|after| !after.dirty)
            })
            .count();
        let file_changes = u32::try_from(changes.len()).unwrap_or(u32::MAX);
        // A changed file now at a version a session of this workspace wrote
        // or read is explained by that session, not by the checkout: an edit
        // no scan saw before it was committed moves HEAD "with the files".
        let known = |path: &str, after: &langspan_core::FileState| {
            self.state.activity.iter().any(|(session, activity)| {
                self.state
                    .sessions
                    .get(session)
                    .is_some_and(|s| s.workspace_id == workspace_id)
                    && activity
                        .known
                        .iter()
                        .any(|version| version.path == path && version.state == *after)
            })
        };
        let unexplained = changes
            .iter()
            .filter(|change| {
                observed_file_state(change.after.as_ref())
                    .map_or(true, |after| !known(&change.path, &after))
            })
            .count();
        for change in changes {
            // A new path absent from an existing complete baseline was missing.
            // No previous baseline, or a previous scan that did not version
            // the path, is deliberately recorded as unknown instead.
            let before = if previous.is_some() && !change.before_unknown {
                Some(observed_file_state(change.before.as_ref())?)
            } else {
                None
            };
            let after = observed_file_state(change.after.as_ref())?;
            if before.as_ref().is_some_and(|before| *before != after)
                && content_changes.len() < langspan_core::MAX_UNCOORDINATED_OBSERVED
            {
                content_changes.push(langspan_core::ObservedFile {
                    path: change.path.clone(),
                    state: after.clone(),
                });
            }
            commands.push(Command::ObserveDisk {
                workspace_id: workspace_id.into(),
                path: change.path,
                before,
                after,
                related_operation_ids: Vec::new(),
            });
        }
        let previous_branches = previous.as_ref().map(observed_branches).unwrap_or_default();
        let names: std::collections::BTreeSet<_> =
            branches.keys().chain(previous_branches.keys()).collect();
        for name in names {
            let current = branches.get(name);
            let changed = current != previous_branches.get(name)
                || name == "HEAD"
                    && previous
                        .as_ref()
                        .is_some_and(|old| old.branch != snapshot.branch);
            if changed {
                let (head_oid, tree_oid) = current.cloned().unwrap_or_default();
                commands.push(Command::ObserveBranch {
                    workspace_id: workspace_id.into(),
                    branch_ref: name.clone(),
                    head_oid,
                    tree_oid,
                });
            }
        }
        let facts = commands.len();
        let mut checkout_changed = false;
        // The checkout changed under the sessions in it: another branch, a
        // HEAD that moved with files no session put there, or a bulk
        // restore. A commit moves HEAD without changing any file, or with
        // only files at versions their sessions wrote, and is not one.
        if let Some(old) = &previous {
            let head_moved = old.head != snapshot.head;
            if old.branch != snapshot.branch
                || head_moved && unexplained > 0
                || !head_moved && restored >= CHECKOUT_RESTORED_FILES
            {
                let branch =
                    |name: &Option<String>| name.as_ref().map(|name| format!("refs/heads/{name}"));
                commands.push(Command::ObserveCheckout {
                    workspace_id: workspace_id.into(),
                    from_branch: branch(&old.branch),
                    to_branch: branch(&snapshot.branch),
                    from_head: old.head.clone(),
                    to_head: snapshot.head.clone(),
                    files: file_changes,
                });
                checkout_changed = true;
            }
        }
        // Changed files no checkout change accounts for. Which of them no
        // session reported, and whom to name, is the core's decision.
        if !checkout_changed && !content_changes.is_empty() {
            commands.push(Command::ObserveUncoordinated {
                workspace_id: workspace_id.into(),
                files: content_changes,
                scanned_from_ms: snapshot.scanned_from_ms,
            });
        }
        // Worktrees of one repository share a Git common directory. Only its
        // digest is recorded; it groups workspaces without merging them.
        commands.push(Command::ObserveWorkspaceGit {
            workspace_id: workspace_id.into(),
            repository_id: format!(
                "local-{}",
                &crate::sha256::hex(&crate::sha256::digest(snapshot.common_dir.as_bytes()))[..32]
            ),
            branch_ref: snapshot
                .branch
                .as_ref()
                .map(|name| format!("refs/heads/{name}")),
        });
        let mut events = Vec::new();
        // The returned count is of disk and branch facts, not the grouping record.
        let mut observed = 0;
        for (index, command) in commands.iter().enumerate() {
            let id = format!("{scan_id}-{index}");
            let decision = decide(&self.state, command, &id, now_ms);
            if let Outcome::Error { code, message } = decision.outcome {
                return Err(format!("invalid Git observation: {code}: {message}").into());
            }
            events.extend(decision.events);
            if index + 1 == facts {
                observed = events.len();
            }
        }
        let summary = langspan_core::ScanSummary {
            workspace_id: workspace_id.into(),
            observed_at_ms: now_ms,
            generation: self.state.generation,
            file_count: snapshot.files.len() as u64,
            branch_count: snapshot.branches.len() as u64,
            head_oid: snapshot.head.clone(),
            branch_ref: snapshot
                .branch
                .as_ref()
                .map(|name| format!("refs/heads/{name}")),
            coverage: (snapshot.coverage != langspan_core::ScanCoverage::default())
                .then(|| snapshot.coverage.clone()),
        };
        self.commit_with_snapshot(
            &events,
            None,
            Some((
                workspace_id,
                &snapshot_json,
                &serde_json::to_string(&summary)?,
            )),
        )?;
        Ok(observed)
    }
}

/// Changed files that went back to their committed version in one scan,
/// with HEAD unmoved, before it counts as the checkout changing. One file is
/// as likely an edit undone by hand.
const CHECKOUT_RESTORED_FILES: usize = 2;

fn observed_file_state(
    version: Option<&crate::observer::FileVersion>,
) -> StoreResult<langspan_core::FileState> {
    use crate::observer::FileKind;
    match version {
        None
        | Some(crate::observer::FileVersion {
            kind: FileKind::Missing,
            digest: None,
            ..
        }) => Ok(langspan_core::FileState::Missing),
        Some(crate::observer::FileVersion {
            kind,
            digest: Some(digest),
            ..
        }) if *kind != FileKind::Missing => Ok(langspan_core::FileState::Present {
            digest: digest.clone(),
            kind: if *kind == FileKind::Symlink {
                langspan_core::FileKind::Symlink
            } else {
                langspan_core::FileKind::Regular
            },
        }),
        _ => Err("inconsistent file kind and digest in complete Git snapshot".into()),
    }
}

fn observed_branches(
    snapshot: &crate::observer::Snapshot,
) -> std::collections::BTreeMap<String, (Option<String>, Option<String>)> {
    let mut branches = std::collections::BTreeMap::from([(
        "HEAD".into(),
        (snapshot.head.clone(), snapshot.head_tree.clone()),
    )]);
    for branch in &snapshot.branches {
        branches.insert(
            format!("refs/heads/{}", branch.name),
            (Some(branch.commit.clone()), Some(branch.tree.clone())),
        );
    }
    branches
}
