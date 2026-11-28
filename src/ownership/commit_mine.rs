//! `commit --mine`: this session's lines staged in a private index and committed with the owner's hooks.

use super::attribution::{Owner, Reading, file_owners, is_own, lines_of};
use super::commit::{COMMIT_TIMEOUT, MAX_DIFF_BYTES, diff, untracked};
use super::commit_diff::{FileDiff, apply_exactly, in_one_order, patch};
use super::status_mine::{current_head, private_name};
use crate::journal::{self, Entry};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// A file left out of the commit, and whose its changes are.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Left {
    pub path: String,
    /// Empty when no session wrote them.
    pub sessions: Vec<String>,
}

/// A change `commit --mine` cannot attribute.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Refusal {
    pub path: String,
    pub lines: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MineReport {
    /// `committed`, `nothing_to_commit` or `refused`.
    pub result: &'static str,
    pub session_id: String,
    pub commit: Option<String>,
    /// The files the commit took this session's changes of.
    pub paths: Vec<String>,
    pub left: Vec<Left>,
    pub refused: Vec<Refusal>,
    /// When the commit was made but the shared index could not be brought
    /// up to date with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// Another commit landed on the branch while this one was being made:
    /// this session's changes were planned again on top of it.
    pub moved: bool,
}

impl MineReport {
    pub fn exit_code(&self) -> u8 {
        match self.result {
            "refused" => 1,
            _ => 0,
        }
    }

    /// What happened, for a person.
    pub fn text(&self) -> String {
        let mut out = Vec::new();
        match self.result {
            "committed" => out.push(format!(
                "Committed this session's changes of {} file(s) as {}: {}.",
                self.paths.len(),
                self.commit.as_deref().unwrap_or("?"),
                self.paths.join(", ")
            )),
            "nothing_to_commit" => out.push(format!(
                "Nothing of this session's is uncommitted ({}); nothing was committed.",
                self.session_id
            )),
            _ => {
                out.push(
                    "LangSpan could not tell whose these changes are, so nothing was committed and nothing was staged:".into(),
                );
                for refusal in &self.refused {
                    out.push(format!(
                        "  {} ({}): {}",
                        refusal.path, refusal.lines, refusal.reason
                    ));
                }
                out.push("Commit them yourself with paths you name (git commit -m \"...\" -- PATH...) once you know they are yours, or ask the owner.".into());
            }
        }
        if !self.left.is_empty() {
            let left: Vec<String> = self
                .left
                .iter()
                .map(|left| {
                    if left.sessions.is_empty() {
                        format!("{} (no session's)", left.path)
                    } else {
                        format!("{} ({})", left.path, left.sessions.join(", "))
                    }
                })
                .collect();
            out.push(format!(
                "Left as they are, uncommitted: {}.",
                left.join(", ")
            ));
        }
        if !self.left.is_empty() || !self.refused.is_empty() {
            out.push(
                "`langspan status --mine` lists which uncommitted lines are still this session's."
                    .into(),
            );
        }
        if self.moved && self.result == "refused" {
            out.push("HEAD moved: another commit landed on the branch while this one was being made, and this session's changes no longer apply on top of it.".into());
        } else if self.moved {
            out.push(
                "Another commit landed on the branch meanwhile; this one was made on top of it."
                    .into(),
            );
        }
        out.extend(self.index.iter().cloned());
        out.join("\n")
    }
}

/// What to stage of one file.
pub(super) enum Choice {
    /// Every change: stage the working-tree file.
    Whole,
    /// These hunks of the diff from `HEAD`.
    Hunks(Vec<usize>),
}

/// Why a file that holds this session's changes and others' is refused when
/// it can be committed only whole.
pub(super) const ONLY_WHOLE: &str = "a new or removed file holds this session's changes and others'; it can be committed only whole";

/// Whether a file can be committed only whole, not hunk by hunk.
pub(super) fn splits_only_whole(file: &FileDiff) -> bool {
    file.created || file.deleted || file.whole
}

struct Planned {
    staged: Vec<(FileDiff, Choice)>,
    left: Vec<Left>,
    refused: Vec<Refusal>,
}

fn plan(
    me: &str,
    root: &Path,
    files: Vec<FileDiff>,
    entries: &[Entry],
    reading: Reading,
) -> Planned {
    let claims = journal::by_file(entries);
    let mut planned = Planned {
        staged: Vec::new(),
        left: Vec::new(),
        refused: Vec::new(),
    };
    for mut file in files {
        if file.mode_change {
            planned.left.push(Left {
                path: file.path.clone(),
                sessions: Vec::new(),
            });
            // A change of mode alone: nothing of it is this session's.
            if file.hunks.is_empty() {
                continue;
            }
        }
        let attributed = file_owners(
            me,
            root,
            &mut file,
            &claims,
            entries,
            &crate::file_state::file_state,
            reading,
        );
        let owners = attributed.owners;
        let mut own = Vec::new();
        let mut others = BTreeSet::new();
        for (index, owner) in owners.iter().enumerate() {
            match owner {
                Owner::Own => own.push(index),
                Owner::Others(sessions) => others.extend(sessions.iter().cloned()),
                Owner::Nobody => {}
                Owner::Unknown(reason) => planned.refused.push(Refusal {
                    path: file.path.clone(),
                    lines: match file.hunks.get(index) {
                        Some(hunk) if !attributed.whole => lines_of(hunk),
                        _ => "the whole file".into(),
                    },
                    reason: (*reason).into(),
                }),
            }
        }
        if own.is_empty() {
            if owners.iter().all(|o| !matches!(o, Owner::Unknown(_))) {
                planned.left.push(Left {
                    path: file.path.clone(),
                    sessions: others.into_iter().collect(),
                });
            }
            continue;
        }
        if own.len() == owners.len() {
            // Staged from the working tree, not by patch.
            file.whole |= attributed.whole;
            planned.staged.push((file, Choice::Whole));
            continue;
        }
        if splits_only_whole(&file) {
            planned.refused.push(Refusal {
                path: file.path.clone(),
                lines: "the whole file".into(),
                reason: ONLY_WHOLE.into(),
            });
            continue;
        }
        planned.left.push(Left {
            path: file.path.clone(),
            sessions: others.into_iter().collect(),
        });
        planned.staged.push((file, Choice::Hunks(own)));
    }
    planned
}

pub(super) fn git_in(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    git_bounded(root, args, index, deadline, MAX_DIFF_BYTES)
}

/// The most characters of a Git command line named in an error.
const SHOWN_COMMAND: usize = 160;

/// One Git step of the commit or of the guard. A failure names the step,
/// its exit status and Git's own words (redacted and capped by
/// `git::said`), with the usual fix when Git could not write.
pub(super) fn git_bounded(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    deadline: Instant,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let env: Vec<(&str, &std::ffi::OsStr)> = index
        .map(|index| vec![("GIT_INDEX_FILE", index.as_os_str())])
        .unwrap_or_default();
    let (status, bytes, errors) = crate::git::run_with_errors(root, args, &env, deadline, limit)
        .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
    if status.success() && errors.is_empty() {
        return Ok(bytes);
    }
    let command: String = crate::text::redact::redact(&args.join(" "))
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(SHOWN_COMMAND)
        .collect();
    let what = if status.success() {
        "printed a warning".to_owned()
    } else {
        format!("failed ({status})")
    };
    let said = crate::git::said(&errors);
    let mut error = format!("git {command} {what}");
    if !said.is_empty() {
        error.push_str(&format!(". Git said: {said}"));
    }
    let text = String::from_utf8_lossy(&errors);
    if !status.success()
        && [
            "Read-only file system",
            "Permission denied",
            "Operation not permitted",
        ]
        .iter()
        .any(|cause| text.contains(cause))
    {
        error.push_str(&format!(". {READ_ONLY}"));
    }
    Err(error)
}

/// The usual cause and fix when Git, or LangSpan in the Git directory,
/// cannot write.
const READ_ONLY: &str = "The Git directory is read-only for this process, as it is inside some agents' workspace sandboxes: run `langspan commit --mine` outside the agent's sandbox (for example from a terminal, or with the host's permission to run it unsandboxed), or make the Git directory writable, then run it again";

/// A file LangSpan writes in the Git directory could not be written: what,
/// why, and the usual fix when the cause is a read-only directory.
fn write_failed(what: &str, error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::ReadOnlyFilesystem | std::io::ErrorKind::PermissionDenied => {
            format!("cannot write {what}: {error}. {READ_ONLY}")
        }
        _ => format!("cannot write {what}: {error}"),
    }
}

/// The version of `path` in the index at `index` (the shared one when
/// `None`).
pub(super) fn index_blob(
    root: &Path,
    path: &str,
    index: Option<&Path>,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    git_in(
        root,
        &["cat-file", "blob", &format!(":{path}")],
        index,
        deadline,
    )
}

/// Stage `staged` into the index at `index` (the shared one when `None`).
/// Hunks are applied with `git apply --cached`; whole files are added from
/// the working tree. Before the patch is applied, each file's hunks are
/// checked to apply exactly to the version in that index; after, the index
/// must hold exactly what they make of it (issue #106). If not, an error.
pub(super) fn stage(
    root: &Path,
    git_dir: &Path,
    staged: &[(FileDiff, Choice)],
    index: Option<&Path>,
    deadline: Instant,
) -> Result<(), String> {
    let mut whole: Vec<&str> = Vec::new();
    let mut patches = Vec::new();
    // Each patched file that remains, and what the index must then hold.
    let mut expected: Vec<(&str, Vec<u8>)> = Vec::new();
    for (file, choice) in staged {
        let chosen: Vec<usize> = match choice {
            Choice::Whole if file.whole || file.untracked => {
                whole.push(&file.path);
                continue;
            }
            Choice::Whole => (0..file.hunks.len()).collect(),
            Choice::Hunks(hunks) => hunks.clone(),
        };
        in_one_order(file, &chosen).map_err(|why| {
            format!(
                "the patch of {} does not keep this session's lines in place among another session's: {why}",
                file.path
            )
        })?;
        let base = if file.created {
            Vec::new()
        } else {
            index_blob(root, &file.path, index, deadline)?
        };
        let after = apply_exactly(&base, file, &chosen).map_err(|why| {
            format!(
                "the patch of {} does not apply exactly to the version in the index: {why}",
                file.path
            )
        })?;
        if !file.deleted {
            expected.push((&file.path, after));
        }
        patches.extend(patch(file, &chosen));
    }
    if !patches.is_empty() {
        let patch_file = git_dir.join(format!("{}.patch", private_name()));
        fs::write(&patch_file, &patches).map_err(|e| write_failed("the patch", &e))?;
        let applied = git_in(
            root,
            &[
                "apply",
                "--cached",
                "--unidiff-zero",
                "--whitespace=nowarn",
                &patch_file.to_string_lossy(),
            ],
            index,
            deadline,
        );
        let _ = fs::remove_file(&patch_file);
        applied?;
        for (path, after) in expected {
            if index_blob(root, path, index, deadline)? != after {
                return Err(format!(
                    "the patch of {path} was applied, but the index does not hold the version it should"
                ));
            }
        }
    }
    if !whole.is_empty() {
        let mut args = vec!["update-index", "--add", "--remove", "--"];
        args.extend(whole);
        git_in(root, &args, index, deadline)?;
    }
    Ok(())
}

/// Run a child to completion within `COMMIT_TIMEOUT`, its output on stderr
/// (stdout is kept for this command's own output).
#[cfg(unix)]
fn run_child(
    mut command: std::process::Command,
    what: &str,
) -> Result<std::process::ExitStatus, String> {
    use std::{os::fd::AsFd, process::Stdio};
    let stdout = std::io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| format!("cannot pass output on: {e}"))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot start {what}: {e}"))?;
    let deadline = Instant::now() + COMMIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{what} did not finish within 10 minutes and was stopped"
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(format!("{what}: {error}")),
        }
    }
}

#[cfg(not(unix))]
fn run_child(_: std::process::Command, _: &str) -> Result<std::process::ExitStatus, String> {
    Err("commit --mine requires Unix".into())
}

/// How the owner's Git hooks are run: by `git hook run` (Git 2.36 and
/// later), or from the hooks directory, `core.hooksPath` respected.
enum Hooks {
    GitHookRun,
    Directory(PathBuf),
}

impl Hooks {
    fn find(root: &Path, deadline: Instant) -> Result<Self, String> {
        let probe = crate::git::run_with_env(
            root,
            &["hook", "run", "--ignore-missing", "langspan-no-such-hook"],
            &[],
            deadline,
            4096,
        );
        if probe.is_ok_and(|(status, _)| status.success()) {
            return Ok(Self::GitHookRun);
        }
        let configured = crate::git::run_with_env(
            root,
            &["config", "--path", "--get", "core.hooksPath"],
            &[],
            deadline,
            8192,
        )
        .ok()
        .filter(|(status, _)| status.success())
        .map(|(_, bytes)| String::from_utf8_lossy(&bytes).trim().to_owned())
        .filter(|path| !path.is_empty());
        let dir = match configured {
            Some(path) => root.join(path),
            None => PathBuf::from(
                String::from_utf8_lossy(&git_in(
                    root,
                    &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
                    None,
                    deadline,
                )?)
                .trim(),
            ),
        };
        Ok(Self::Directory(dir))
    }

    /// Run hook `name` as `git commit` would, with the index being
    /// committed. `Ok(true)` when it is absent or succeeded.
    fn run(&self, root: &Path, name: &str, args: &[&str], index: &Path) -> Result<bool, String> {
        let mut command = match self {
            Self::GitHookRun => {
                let mut command = std::process::Command::new("git");
                command
                    .arg("-C")
                    .arg(root)
                    .args(["hook", "run", "--ignore-missing", name, "--"])
                    .args(args);
                command
            }
            Self::Directory(dir) => {
                let path = dir.join(name);
                let executable = fs::metadata(&path).is_ok_and(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.is_file() && m.permissions().mode() & 0o111 != 0
                });
                if !executable {
                    return Ok(true);
                }
                let mut command = std::process::Command::new(path);
                command.args(args).current_dir(root);
                command
            }
        };
        command.env("GIT_INDEX_FILE", index).env("GIT_EDITOR", ":");
        Ok(run_child(command, &format!("the {name} hook"))?.success())
    }
}

/// `git commit -m`'s default clean-up of a message given on the command
/// line: trailing whitespace off each line, runs of blank lines made one,
/// no blank lines at either end, one final newline.
fn clean_message(message: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in message.lines().map(str::trim_end) {
        if line.is_empty() && out.last().is_none_or(|last| last.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|last| last.is_empty()) {
        out.pop();
    }
    if out.is_empty() {
        String::new()
    } else {
        format!("{}\n", out.join("\n"))
    }
}

/// Why one attempt made no commit.
enum Attempt {
    /// HEAD moved after it was read: nothing was changed.
    Moved,
    Failed(String),
}

/// Make the commit of `staged` on `head`, atomically: a private index built
/// from `head`, the owner's commit hooks, `write-tree` and `commit-tree`, and
/// the branch moved only if it still points at `head`.
fn commit_on(
    root: &Path,
    git_dir: &Path,
    head: &str,
    staged: &[(FileDiff, Choice)],
    message: &str,
    deadline: Instant,
) -> Result<String, Attempt> {
    let failed = Attempt::Failed;
    let name = private_name();
    let index = git_dir.join(format!("{name}.index"));
    let message_file = git_dir.join(format!("{name}.msg"));
    let result = (|| {
        git_in(root, &["read-tree", head], Some(&index), deadline).map_err(failed)?;
        stage(root, git_dir, staged, Some(&index), deadline).map_err(failed)?;
        // As `git commit -m` hands it to the hooks: cleaned, newline-ended.
        fs::write(&message_file, clean_message(message))
            .map_err(|e| failed(write_failed("the commit message", &e)))?;
        let message_path = message_file.to_string_lossy().into_owned();
        let hooks = Hooks::find(root, deadline).map_err(failed)?;
        for (hook, args) in [
            ("pre-commit", vec![]),
            ("prepare-commit-msg", vec![message_path.as_str(), "message"]),
            ("commit-msg", vec![message_path.as_str()]),
        ] {
            if !hooks.run(root, hook, &args, &index).map_err(failed)? {
                return Err(failed(format!("the {hook} hook refused the commit")));
            }
        }
        // Hooks may take minutes: what follows has its own time.
        let deadline = Instant::now() + Duration::from_secs(60);
        let cleaned = clean_message(
            &fs::read_to_string(&message_file)
                .map_err(|e| failed(format!("cannot read the commit message: {e}")))?,
        );
        if cleaned.is_empty() {
            return Err(failed("the commit message is empty".into()));
        }
        fs::write(&message_file, &cleaned)
            .map_err(|e| failed(write_failed("the commit message", &e)))?;
        let tree = git_in(root, &["write-tree"], Some(&index), deadline).map_err(failed)?;
        let tree = String::from_utf8_lossy(&tree).trim().to_owned();
        let sign = crate::git::run_with_env(
            root,
            &["config", "--bool", "--get", "commit.gpgSign"],
            &[],
            deadline,
            64,
        )
        .is_ok_and(|(status, value)| status.success() && value.trim_ascii() == b"true");
        let mut args = vec![
            "commit-tree",
            tree.as_str(),
            "-p",
            head,
            "-F",
            message_path.as_str(),
        ];
        if sign {
            args.push("-S");
        }
        let commit = git_in(root, &args, None, deadline).map_err(failed)?;
        let commit = String::from_utf8_lossy(&commit).trim().to_owned();
        // The branch HEAD names, so the branch moves; HEAD itself when
        // detached. Compare-and-swap: refused if HEAD moved.
        let target =
            crate::git::run_with_env(root, &["symbolic-ref", "-q", "HEAD"], &[], deadline, 4096)
                .ok()
                .filter(|(status, _)| status.success())
                .map(|(_, bytes)| String::from_utf8_lossy(&bytes).trim().to_owned())
                .unwrap_or_else(|| "HEAD".into());
        let moved = git_in(
            root,
            &[
                "update-ref",
                "-m",
                "langspan commit --mine",
                &target,
                &commit,
                head,
            ],
            None,
            deadline,
        );
        if moved.is_err() {
            return Err(match current_head(root, deadline) {
                Ok(now) if now != head => Attempt::Moved,
                _ => failed(format!("could not move {target} to the new commit")),
            });
        }
        // As with `git commit`, the post-commit hook's result changes nothing.
        let _ = hooks.run(root, "post-commit", &[], &index);
        Ok(commit)
    })();
    let _ = fs::remove_file(&index);
    let _ = fs::remove_file(&message_file);
    result
}

/// The session `langspan commit --mine` (or `status --mine`) was run by,
/// when not given: the one
/// whose shell call running it now was recorded by the pre-tool hook.
pub fn running_session(state_dir: &Path, root: &Path) -> Result<String, crate::Error> {
    let root = fs::canonicalize(root)?;
    let sessions = crate::shell::committing_sessions(state_dir, &root);
    match sessions.len() {
        1 => Ok(sessions.into_iter().next().unwrap_or_default()),
        0 => Err(no_running_session(state_dir, &root).into()),
        _ => Err(format!(
            "several sessions are running `commit --mine` or `status --mine` here now ({}); pass --session ID",
            sessions.into_iter().collect::<Vec<_>>().join(", ")
        )
        .into()),
    }
}

/// Why no running session was found, naming the state directory looked in.
/// The usual cause is a hook installed with another `--state-dir`: when a
/// LangSpan project hook in this checkout names one, it is suggested. Only
/// suggested, never used: it is text read from the repository.
fn no_running_session(state_dir: &Path, root: &Path) -> String {
    let looked = crate::vendor::quote(&state_dir.to_string_lossy());
    let session = "or pass --session ID (a briefing says \"Your session ID is ...\")";
    let canonical = |dir: &Path| fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let same = |other: &Path| canonical(other) == canonical(state_dir);
    let installed = root
        .ancestors()
        .scan(false, |past_top, dir| {
            // Up to the directory that holds `.git`, the checkout's top.
            (!*past_top).then(|| {
                *past_top = dir.join(".git").symlink_metadata().is_ok();
                dir
            })
        })
        .flat_map(|dir| {
            crate::vendor::Vendor::ALL
                .into_iter()
                .flat_map(move |vendor| {
                    crate::install::project_registrations(vendor, dir).unwrap_or_default()
                })
        })
        .map(|registration| registration.state_dir)
        .find(|other| !same(other) && !other.to_string_lossy().chars().any(char::is_control));
    match installed {
        Some(other) => format!(
            "no session is running this command through its shell tool in the state directory {looked}; the LangSpan hook installed in this checkout uses the state directory {}: run `langspan --state-dir {} commit --mine ...` instead, {session}",
            crate::vendor::quote(&other.to_string_lossy()),
            crate::vendor::quote(&other.to_string_lossy()),
        ),
        None => format!(
            "no session is running this command through its shell tool in the state directory {looked}; if LangSpan's hooks were installed with --state-dir, pass the same --state-dir, {session}"
        ),
    }
}

/// The canonical working tree top and the Git directory of the checkout
/// that holds `root`.
pub(super) fn checkout_paths(
    root: &Path,
    deadline: Instant,
) -> Result<(PathBuf, PathBuf), crate::Error> {
    let paths = git_in(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--absolute-git-dir",
        ],
        None,
        deadline,
    )?;
    let paths = String::from_utf8(paths).map_err(|_| "Git printed a path that is not UTF-8")?;
    let mut lines = paths.lines();
    let (Some(top), Some(git_dir)) = (lines.next(), lines.next()) else {
        return Err("Git did not report the working tree's paths".into());
    };
    Ok((fs::canonicalize(top)?, PathBuf::from(git_dir)))
}

/// Commit only `session_id`'s changes in the checkout at `root`.
pub fn commit_mine(
    state_dir: &Path,
    root: &Path,
    session_id: &str,
    message: &str,
) -> Result<MineReport, crate::Error> {
    if message.trim().is_empty() {
        return Err("a commit message is required (-m MESSAGE)".into());
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let (root, git_dir) = checkout_paths(root, deadline)?;
    for marker in [
        "MERGE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        if git_dir.join(marker).symlink_metadata().is_ok() {
            return Err(
                "a merge, rebase, cherry-pick or revert is under way; finish it first".into(),
            );
        }
    }
    let mut head = current_head(&root, deadline)
        .map_err(|_| "this repository has no commit yet; make the first commit directly")?;
    let entries = journal::entries(state_dir, &root);
    // At most two attempts: if HEAD moves under the first, this session's
    // changes are planned again on top of the commit that moved it.
    let mut moved = false;
    let (report, commit) = loop {
        let mut files = diff(&root, &[&head], &[], deadline)?;
        files.extend(untracked(&root, &root, &[], deadline)?);
        let reading = Reading {
            deadline,
            made: None,
        };
        let planned = plan(session_id, &root, files, &entries, reading);
        let mut report = MineReport {
            result: "refused",
            session_id: session_id.into(),
            commit: None,
            paths: planned.staged.iter().map(|(f, _)| f.path.clone()).collect(),
            left: planned.left,
            refused: planned.refused,
            index: None,
            moved,
        };
        if !report.refused.is_empty() {
            report.paths.clear();
            return Ok(report);
        }
        if planned.staged.is_empty() {
            report.result = "nothing_to_commit";
            return Ok(report);
        }
        match commit_on(&root, &git_dir, &head, &planned.staged, message, deadline) {
            Ok(commit) => break (report, commit),
            Err(Attempt::Moved) if !moved => {
                moved = true;
                head = current_head(&root, deadline)?;
            }
            Err(Attempt::Moved) => {
                return Err("HEAD moved twice while the commit was being made; nothing was committed. Run the command again".into());
            }
            Err(Attempt::Failed(error)) => {
                return Err(
                    format!("{error}; nothing was committed, and the index is as it was").into(),
                );
            }
        }
    };
    let mut report = report;
    // The commit is made: from here on, nothing may turn it into an error,
    // and the hooks' time does not count against what follows.
    let deadline = Instant::now() + Duration::from_secs(60);
    report.commit = Some(
        git_in(&root, &["rev-parse", "--short", &commit], None, deadline)
            .map(|short| String::from_utf8_lossy(&short).trim().to_owned())
            .unwrap_or_else(|_| commit.clone()),
    );
    report.result = "committed";
    // The shared index learns the commit: this session's changes, on top of
    // whatever else is staged there, so they do not show as staged undone.
    let committed: BTreeSet<String> = report.paths.iter().cloned().collect();
    let reading = Reading {
        deadline,
        made: Some(&commit),
    };
    if let Err(error) =
        update_shared_index(&root, &git_dir, session_id, &committed, &entries, reading)
    {
        report.index = Some(format!(
            "The commit was made, but the shared index could not be updated for it ({error}): `git status` may show this session's committed changes as staged reversals. `git diff --cached` shows them."
        ));
    }
    let sessions: BTreeSet<String> = entries
        .iter()
        .map(|entry| entry.session_id.clone())
        .filter(|session| is_own(session_id, session))
        .collect();
    journal::forget(state_dir, &root, &sessions, &committed);
    Ok(report)
}

/// After the commit: stage the committed changes in the shared index too,
/// leaving everything else in it as it was.
fn update_shared_index(
    root: &Path,
    git_dir: &Path,
    me: &str,
    committed: &BTreeSet<String>,
    entries: &[Entry],
    reading: Reading,
) -> Result<(), String> {
    let deadline = reading.deadline;
    let pathspecs: Vec<String> = committed
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect();
    let mut files = diff(root, &[], &pathspecs, deadline)?;
    files.extend(
        untracked(root, root, &pathspecs, deadline)?
            .into_iter()
            .filter(|file| committed.contains(&file.path)),
    );
    let planned = plan(me, root, files, entries, reading);
    if let Some(refusal) = planned.refused.first() {
        return Err(format!(
            "{} ({}) does not split the same way against it: {}",
            refusal.path, refusal.lines, refusal.reason
        ));
    }
    stage(root, git_dir, &planned.staged, None, deadline)
}
