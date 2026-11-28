//! `status --mine`: this session's uncommitted changes, attributed as `commit --mine` would, read-only.

use super::attribution::{FileOwners, Owner, Reading, file_owners, span};
use super::commit::{diff, untracked};
use super::commit_diff::FileDiff;
use super::commit_mine::{ONLY_WHOLE, checkout_paths, git_in, splits_only_whole};
use crate::journal;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    path::Path,
    time::{Duration, Instant},
};

/// The most files `status --mine` lists.
pub const MINE_LISTED: usize = 200;

/// One hunk of a file `status --mine` lists.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MineHunk {
    /// Its first line in the working file; where it was in `HEAD`'s when it
    /// only removes lines.
    pub start_line: u32,
    pub line_count: u32,
    /// `mine`, `others`, `none` (no session wrote it) or `unknown`.
    pub owner: &'static str,
    /// For `others`: the sessions that wrote it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
    /// For `unknown`: why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A file that holds this session's uncommitted changes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MineFile {
    pub path: String,
    /// `mine`: `commit --mine` would commit all of it; `mixed`: only its
    /// hunks owned `mine`; `unknown`: `commit --mine` would refuse.
    pub owner: &'static str,
    /// Some of its changes are staged in the shared index.
    pub staged: bool,
    /// Attributed as one whole file (binary, too large, a mode change, or
    /// changed by this session in lines that are not known): no hunks.
    pub whole: bool,
    pub hunks: Vec<MineHunk>,
    /// For a whole file `unknown`: why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// This session's uncommitted changes in a shared checkout: what
/// `commit --mine` would commit and refuse, without committing.
#[derive(Debug, Clone, Serialize)]
pub struct MineStatus {
    pub session_id: String,
    /// By path, at most `MINE_LISTED`.
    pub files: Vec<MineFile>,
    /// Changed files that hold other sessions' lines, listed or not.
    pub others: usize,
    /// More files hold this session's changes than are listed.
    pub truncated: bool,
}

impl MineStatus {
    /// For a person: one line per file.
    pub fn text(&self) -> String {
        let mut out = Vec::new();
        if self.files.is_empty() {
            out.push(format!(
                "Nothing of this session's is uncommitted ({}).",
                self.session_id
            ));
        }
        let width = self.files.iter().map(|f| f.path.len()).max().unwrap_or(0);
        for file in &self.files {
            let parts: Vec<String> = if file.whole {
                vec![format!(
                    "the whole file{}",
                    file.reason
                        .as_ref()
                        .map(|why| format!(": {why}"))
                        .unwrap_or_default()
                )]
            } else {
                file.hunks
                    .iter()
                    .map(|hunk| {
                        let lines = if hunk.line_count <= 1 {
                            format!("line {}", hunk.start_line)
                        } else {
                            format!(
                                "lines {}-{}",
                                hunk.start_line,
                                hunk.start_line + hunk.line_count - 1
                            )
                        };
                        let who = match hunk.owner {
                            "others" => hunk.sessions.join(" and "),
                            "none" => "no session's".into(),
                            "unknown" => format!(
                                "not known ({})",
                                hunk.reason.as_deref().unwrap_or_default()
                            ),
                            mine => mine.into(),
                        };
                        format!("{lines} {who}")
                    })
                    .collect()
            };
            let owner = if file.staged {
                format!("{} staged", file.owner)
            } else {
                file.owner.into()
            };
            out.push(format!(
                "{:width$}  {owner:14}  {}",
                file.path,
                parts.join(", ")
            ));
        }
        if self.truncated {
            out.push(format!("Only the first {MINE_LISTED} files are listed."));
        }
        if self.others > 0 {
            out.push(format!(
                "{} changed file(s) hold other sessions' lines.",
                self.others
            ));
        }
        out.join("\n")
    }
}

/// How `status --mine` lists one file, and whether it holds other
/// sessions' lines. `None` when nothing of it is this session's.
pub(super) fn mine_file(
    file: &FileDiff,
    owners: &FileOwners,
    staged: bool,
) -> (Option<MineFile>, bool) {
    // Other sessions' lines also sit in a hunk or file whose owner is not
    // known: count the foreign contributions recorded for every owner.
    let theirs = owners
        .owners
        .iter()
        .any(|owner| matches!(owner, Owner::Others(_)))
        || owners.foreign.iter().any(|sessions| !sessions.is_empty());
    let own = owners.owners.iter().filter(|o| **o == Owner::Own).count();
    let unknown = owners.owners.iter().find_map(|owner| match owner {
        Owner::Unknown(why) => Some(*why),
        _ => None,
    });
    if own == 0 && unknown.is_none() {
        return (None, theirs);
    }
    let all_own = own == owners.owners.len();
    let only_whole = !all_own && unknown.is_none() && splits_only_whole(file);
    let owner = if unknown.is_some() || only_whole {
        "unknown"
    } else if all_own {
        "mine"
    } else {
        "mixed"
    };
    let hunks = if owners.whole {
        Vec::new()
    } else {
        file.hunks
            .iter()
            .zip(&owners.owners)
            .map(|(hunk, owner)| {
                let (start_line, line_count) = span(hunk);
                let (owner, sessions, reason) = match owner {
                    Owner::Own => ("mine", Vec::new(), None),
                    Owner::Others(sessions) => ("others", sessions.iter().cloned().collect(), None),
                    Owner::Nobody => ("none", Vec::new(), None),
                    Owner::Unknown(why) => ("unknown", Vec::new(), Some((*why).to_owned())),
                };
                MineHunk {
                    start_line,
                    line_count,
                    owner,
                    sessions,
                    reason,
                }
            })
            .collect()
    };
    let reason = if owners.whole {
        unknown.map(str::to_owned)
    } else {
        only_whole.then(|| ONLY_WHOLE.to_owned())
    };
    let listed = MineFile {
        path: file.path.clone(),
        owner,
        staged,
        whole: owners.whole,
        hunks,
        reason,
    };
    (Some(listed), theirs)
}

/// The paths staged in the shared index, against `HEAD`.
fn staged_paths(root: &Path, deadline: Instant) -> Result<BTreeSet<String>, String> {
    let listed = git_in(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-renames",
            "--ignore-submodules=all",
            "--no-relative",
        ],
        None,
        deadline,
    )?;
    listed
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| {
            String::from_utf8(p.to_vec()).map_err(|_| "a staged path that is not UTF-8".into())
        })
        .collect()
}

/// `session_id`'s uncommitted changes in the checkout at `root`, attributed
/// as `commit --mine` attributes them. Read-only: it builds no index,
/// commits nothing and leaves the shared index and working tree as they are
/// (Git runs with `--no-optional-locks`).
pub fn status_mine(
    state_dir: &Path,
    root: &Path,
    session_id: &str,
) -> Result<MineStatus, crate::Error> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let (root, _) = checkout_paths(root, deadline)?;
    let head = current_head(&root, deadline)
        .map_err(|_| "this repository has no commit yet; nothing is uncommitted against HEAD")?;
    let entries = journal::entries(state_dir, &root);
    let mut files = diff(&root, &[&head], &[], deadline)?;
    files.extend(untracked(&root, &root, &[], deadline)?);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let staged = staged_paths(&root, deadline)?;
    let claims = journal::by_file(&entries);
    let reading = Reading {
        deadline,
        made: None,
    };
    let mut status = MineStatus {
        session_id: session_id.into(),
        files: Vec::new(),
        others: 0,
        truncated: false,
    };
    for file in &mut files {
        // A change of mode alone is nobody's (as in `plan`).
        if file.mode_change && file.hunks.is_empty() {
            continue;
        }
        let owners = file_owners(
            session_id,
            &root,
            file,
            &claims,
            &entries,
            &crate::file_state::file_state,
            reading,
        );
        let (listed, theirs) = mine_file(file, &owners, staged.contains(&file.path));
        status.add(listed, theirs);
    }
    Ok(status)
}

impl MineStatus {
    /// One file attributed: listed while fewer than `MINE_LISTED` are.
    pub(super) fn add(&mut self, listed: Option<MineFile>, theirs: bool) {
        self.others += usize::from(theirs);
        if let Some(listed) = listed {
            if self.files.len() < MINE_LISTED {
                self.files.push(listed);
            } else {
                self.truncated = true;
            }
        }
    }
}

pub(super) fn current_head(root: &Path, deadline: Instant) -> Result<String, String> {
    git_in(
        root,
        &["rev-parse", "-q", "--verify", "HEAD^{commit}"],
        None,
        deadline,
    )
    .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
}

/// A name for this run's private index and patch, unique within the
/// machine's processes and within this one.
pub(super) fn private_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "langspan-mine-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        crate::shell::now_ms()
    )
}
