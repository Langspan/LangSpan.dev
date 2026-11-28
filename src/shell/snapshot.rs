//! What Git reports as differing from `HEAD`, read within bounds and a
//! deadline, and the files whose content differs between two snapshots.

use langspan_core::FileState;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

// ---------------------------------------------------------------------------
// Workspace snapshot
// ---------------------------------------------------------------------------

/// At most this many changed paths in one snapshot.
pub const MAX_SNAPSHOT_FILES: usize = 1024;
/// At most this many bytes of changed files are read for one snapshot.
pub const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STATUS_BYTES: usize = 1024 * 1024;

/// One path Git reports as differing from `HEAD` or the index: its status
/// codes (two with `--no-renames` when, say, it is both deleted from the
/// index and untracked) and its content version on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub codes: Vec<String>,
    pub state: FileState,
}

/// What Git reports as differing, with each such file's content version. A
/// file not listed is, on disk, as it is in `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub head: Option<String>,
    /// The branch checked out, as `refs/heads/NAME`; `None` when `HEAD` is
    /// detached, or in a record written before this was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub files: BTreeMap<String, Entry>,
    /// A merge, rebase, cherry-pick or revert is under way, or a file is
    /// conflicted. Nothing is denied then.
    pub in_progress: bool,
}

fn git_line(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec())
        .map(|text| text.trim_end_matches('\n').to_owned())
        .map_err(|_| "Git printed a path that is not UTF-8".into())
}

/// Snapshot the workspace at `root`, which must be the top level of a Git
/// working tree. Only files Git lists are read, so the cost follows the
/// number of changed files, not the size of the repository. Git's own
/// racy-clean check compares the content of a file modified too recently to
/// trust its timestamp. Every failure and every bound is an error: an
/// incomplete snapshot is never used.
pub fn snapshot(root: &Path, deadline: Instant) -> Result<Snapshot, String> {
    let paths = crate::git::output(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--absolute-git-dir",
        ],
        deadline,
        16 * 1024,
    )
    .map_err(|_| "the workspace is not a Git working tree".to_owned())?;
    let text = git_line(&paths)?;
    let lines: Vec<&str> = text.lines().collect();
    let [top, git_dir] = lines.as_slice() else {
        return Err("Git did not report the working tree's paths".into());
    };
    let top = fs::canonicalize(top).map_err(|e| format!("cannot resolve the working tree: {e}"))?;
    if top != root {
        return Err("the workspace root is not the top level of its Git working tree".into());
    }
    let git_dir = PathBuf::from(git_dir);
    let in_progress = [
        "MERGE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ]
    .iter()
    .any(|marker| git_dir.join(marker).symlink_metadata().is_ok());
    let (status, oid) = crate::git::run_with_env(
        root,
        &["rev-parse", "-q", "--verify", "HEAD^{commit}"],
        &[],
        deadline,
        1024,
    )?;
    let head = status.success().then(|| git_line(&oid)).transpose()?;
    let listing = crate::git::output(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=all",
            "--no-renames",
        ],
        deadline,
        MAX_STATUS_BYTES,
    )
    .map_err(|e| format!("git status: {e}"))?;
    let mut snapshot = Snapshot {
        head,
        branch: checked_out_branch(&git_dir),
        files: BTreeMap::new(),
        in_progress,
    };
    let mut bytes = 0_u64;
    for entry in listing.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|_| "a changed path is not UTF-8")?;
        let (code, path) = entry
            .split_at_checked(3)
            .filter(|(code, _)| code.ends_with(' '))
            .ok_or("an unexpected git status entry")?;
        let code = &code[..2];
        if matches!(code, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
            snapshot.in_progress = true;
        }
        // A nested repository is listed as a directory; it is not covered.
        if path.ends_with('/') {
            continue;
        }
        if path.len() > 512 || path.contains('\\') || path.chars().any(char::is_control) {
            return Err("a changed path cannot be named as a workspace file".into());
        }
        if let Some(seen) = snapshot.files.get_mut(path) {
            seen.codes.push(code.into());
            continue;
        }
        if snapshot.files.len() >= MAX_SNAPSHOT_FILES {
            return Err(format!(
                "more than {MAX_SNAPSHOT_FILES} files differ from HEAD"
            ));
        }
        if Instant::now() >= deadline {
            return Err("the hook deadline elapsed during the snapshot".into());
        }
        bytes += fs::symlink_metadata(root.join(path)).map_or(0, |m| m.len());
        if bytes > MAX_SNAPSHOT_BYTES {
            return Err("the changed files are larger than the snapshot bound".into());
        }
        let state = crate::file_state::file_state(root, path).ok_or_else(unversioned)?;
        snapshot.files.insert(
            path.to_owned(),
            Entry {
                codes: vec![code.into()],
                state,
            },
        );
    }
    Ok(snapshot)
}

/// The branch `HEAD` names in the Git directory `git_dir`, as
/// `refs/heads/NAME`, read from its `HEAD` file (at most 1 KiB). `None` when
/// it is detached or cannot be read.
fn checked_out_branch(git_dir: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    fs::File::open(git_dir.join("HEAD"))
        .ok()?
        .take(1024)
        .read_to_string(&mut text)
        .ok()?;
    let reference = text.strip_prefix("ref: ")?.trim_end();
    (reference.len() > "refs/heads/".len()
        && reference.starts_with("refs/heads/")
        && !reference.chars().any(char::is_control))
    .then(|| reference.to_owned())
}

/// The checkout a run's Git command left (`Record::checkout`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutLeft {
    pub branch: Option<String>,
    pub head: Option<String>,
    /// It changed no file's content, as a commit does. Absent from a record
    /// written before it was kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub files_unchanged: bool,
}

/// What a run whose command runs a Git operation that changes the checkout
/// (`moves_checkout`) left, when the checkout changed while it ran: another
/// branch or HEAD, or files changed with them in place (a restore or stash).
pub fn checkout_left(
    before: &Snapshot,
    after: &Snapshot,
    changed_files: bool,
) -> Option<CheckoutLeft> {
    (before.head != after.head || before.branch != after.branch || changed_files).then(|| {
        CheckoutLeft {
            branch: after.branch.clone(),
            head: after.head.clone(),
            files_unchanged: !changed_files,
        }
    })
}

fn unversioned() -> String {
    "a changed file cannot be versioned (over 4 MiB, a symlink or not a regular file)".into()
}

/// The files whose content differs between two snapshots of one workspace.
///
/// - Listed in both: their content versions are compared.
/// - Listed only before: the file is now as in `HEAD`, and `current` (the
///   file on disk now) is compared with the version before.
/// - Listed only after: it was as in `HEAD` before. It changed when Git says
///   the file on disk now differs from `HEAD`: untracked, or one status
///   column clean. When both columns changed the index moved too, and
///   whether the file differs from `HEAD` is not known.
///
/// A moved `HEAD` changes what "as in `HEAD`" means, so the set is unknown.
pub fn changed(
    before: &Snapshot,
    after: &Snapshot,
    current: impl Fn(&str) -> Option<FileState>,
) -> Result<Vec<String>, String> {
    if before.head != after.head {
        return Err("HEAD moved during the command".into());
    }
    let paths: BTreeSet<&String> = before.files.keys().chain(after.files.keys()).collect();
    let mut changed = Vec::new();
    for path in paths {
        let differs = match (before.files.get(path), after.files.get(path)) {
            (Some(old), Some(new)) => old.state != new.state,
            (Some(old), None) => current(path).ok_or_else(unversioned)? != old.state,
            (None, Some(new)) => match new.codes.as_slice() {
                [code] if code == "??" || code.starts_with(' ') || code.ends_with(' ') => true,
                _ => {
                    return Err(
                        "Git's index changed for a file during the command, so whether its content changed is not known"
                            .into(),
                    );
                }
            },
            (None, None) => false,
        };
        if differs {
            changed.push(path.clone());
        }
    }
    Ok(changed)
}
