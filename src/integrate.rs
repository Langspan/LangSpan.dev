//! Checks bound to exact trees, combining branches without a working tree,
//! and landing a result that has current evidence. Every Git write here is
//! an object, a ref under `refs/langspan/`, or a temporary detached worktree
//! in the system temporary directory. The owner's working tree, index, HEAD
//! and branches are never changed. See ADR 0007.

use crate::git;
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn text(root: &Path, args: &[&str], deadline: Instant) -> Result<String, String> {
    let bytes = git::output(root, args, deadline, 64 * 1024)?;
    let text = String::from_utf8(bytes).map_err(|_| "Git output is not UTF-8")?;
    Ok(text.trim_end_matches('\n').to_owned())
}

fn oid(text: String) -> Result<String, String> {
    if matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(text)
    } else {
        Err("Git did not return an object ID".into())
    }
}

/// A branch or commit name supplied by the owner. It is passed to Git as a
/// revision, so it must not be readable as an option.
pub fn revision(name: &str) -> Result<&str, String> {
    if name.is_empty()
        || name.len() > 255
        || name.starts_with('-')
        || name.chars().any(|c| c.is_control() || c == ' ')
        || name.contains("..")
        || name.contains(['^', '~', ':', '?', '*', '[', '\\'])
    {
        return Err(format!(
            "{name:?} is not a plain branch or commit name; nothing was done"
        ));
    }
    Ok(name)
}

fn commit(root: &Path, name: &str, deadline: Instant) -> Result<String, String> {
    let revision = format!("{}^{{commit}}", revision(name)?);
    oid(text(
        root,
        &["rev-parse", "--verify", "--quiet", &revision],
        deadline,
    )
    .map_err(|_| format!("{name:?} does not name a commit in this repository"))?)
}

fn tree_of(root: &Path, commit: &str, deadline: Instant) -> Result<String, String> {
    oid(text(
        root,
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
        deadline,
    )?)
}

fn is_ancestor(root: &Path, ancestor: &str, of: &str, deadline: Instant) -> Result<bool, String> {
    let (status, _) = git::run_with_env(
        root,
        &["merge-base", "--is-ancestor", ancestor, of],
        &[],
        deadline,
        256,
    )?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("Git could not compare the two commits".into()),
    }
}

/// Distinguishes the private index of snapshots taken at once by one
/// process: the daemon's automatic checks run on several threads.
static SNAPSHOTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The exact tree of the working directory as it is now: every tracked and
/// every untracked, unignored file, as its raw bytes on disk. Built through
/// a private index, so the owner's index is not read into or written.
///
/// No clean or process filter runs: a repository program is not started
/// for a snapshot, and a required filter that is not installed (Git LFS
/// absent) does not stop it. A filtered file that changed locally is
/// therefore recorded as its raw bytes, not as what the filter would store.
/// In a sparse checkout `git add` leaves the paths outside it as they are in
/// HEAD (Git 2.56.0, the tested version), so they are not deletions.
pub fn working_tree(root: &Path, deadline: Instant) -> Result<(Option<String>, String), String> {
    let index = PathBuf::from(text(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            &format!(
                "langspan-index-{}-{}",
                std::process::id(),
                SNAPSHOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        ],
        deadline,
    )?);
    let _ = fs::remove_file(&index);
    let env: [(&str, &OsStr); 1] = [("GIT_INDEX_FILE", index.as_os_str())];
    let result = (|| {
        let head = text(root, &["rev-parse", "--revs-only", "HEAD"], deadline)?;
        let head = (!head.is_empty()).then_some(head);
        // Every command on the private index: any of them may rehash a
        // racily clean entry, and that would run a filter.
        let overrides = git::filter_overrides(root, deadline)?;
        let private = |command: &[&str]| {
            let mut arguments: Vec<&str> = overrides.iter().map(String::as_str).collect();
            arguments.extend(["-c", "core.fsmonitor=false"]);
            arguments.extend_from_slice(command);
            git::output_with_env(root, &arguments, &env, deadline, 256)
        };
        if let Some(head) = &head {
            private(&["read-tree", head])?;
        }
        private(&[
            "-c",
            "core.safecrlf=false",
            "-c",
            "advice.addEmbeddedRepo=false",
            "add",
            "-A",
        ])
        .map_err(|e| format!("could not snapshot the working tree: {e}"))?;
        let tree = oid(String::from_utf8_lossy(&private(&["write-tree"])?)
            .trim()
            .to_owned())?;
        Ok((head, tree))
    })();
    let _ = fs::remove_file(&index);
    result
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Merged {
    /// A commit holding the combined result. It may be one of the inputs
    /// when the others were already contained in it.
    pub commit: String,
    pub tree: String,
    /// The target commit first, then each combined branch's commit.
    pub inputs: Vec<String>,
    /// Files the target renamed after a branch started, which the branch
    /// edits under the old name: its edits are in the new file.
    pub renames: Vec<Rename>,
}

/// A rename that had already landed, followed while combining `branch`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Rename {
    pub branch: String,
    pub from: String,
    pub to: String,
}

/// Paths reported per conflict, at most.
pub const CONFLICT_PATHS: usize = 64;
/// Files whose conflicting hunks are shown, at most.
pub const CONFLICT_FILES: usize = 8;
/// Hunks shown per file, at most.
pub const CONFLICT_HUNKS: usize = 4;
/// Lines shown of each side of a hunk, at most.
pub const CONFLICT_LINES: usize = 6;
/// Characters shown of one line, at most.
pub const CONFLICT_LINE_CHARS: usize = 200;
/// A conflicted file larger than this is named, not read.
const CONFLICT_FILE_BYTES: usize = 1024 * 1024;

/// One side of a conflicting hunk, as repository text: data, not
/// instructions. `start` is the hunk's first line in that side's own
/// version of the file (for no lines, the line it would follow plus one),
/// or unset when that version could not be read (the file has another path
/// on that side). `lines` is how many lines it has, `text` the first
/// `CONFLICT_LINES` of them, each at most `CONFLICT_LINE_CHARS` characters,
/// control characters replaced.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Side {
    pub start: Option<u32>,
    pub lines: u32,
    pub text: Vec<String>,
}

/// Where both sides changed the same lines. `target` is the target with
/// the branches named before this one; `branch` is the branch that stopped
/// the combination.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Hunk {
    pub target: Side,
    pub branch: Side,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConflictFile {
    pub path: String,
    /// Git's conflict types for this path, such as `contents` or
    /// `modify/delete`.
    pub kinds: Vec<String>,
    pub hunks: Vec<Hunk>,
    /// Conflicting hunks in this file that are not shown.
    pub more_hunks: u32,
    /// Why no hunk could be shown, when none could: a binary file, a large
    /// one, or a conflict about the file rather than its lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unshown: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Combination {
    Clean(Merged),
    /// Combining stopped at `branch`: these paths conflict, and `files`
    /// shows the hunks of the first `CONFLICT_FILES` of them.
    Conflict {
        branch: String,
        paths: Vec<String>,
        files: Vec<ConflictFile>,
    },
}

/// Git settings every combination uses, whatever the owner's configuration:
/// the result, and so the evidence for it, does not depend on them. Renames
/// are followed, bounded: inexact renames are looked for among at most 1000
/// files on each side (exact renames are always found). Conflicts are
/// marked in the plain two-sided style that `hunks` reads.
const COMBINE_CONFIG: [&str; 8] = [
    "-c",
    "merge.renames=true",
    "-c",
    "merge.conflictStyle=merge",
    "-c",
    "merge.renameLimit=1000",
    "-c",
    "diff.renameLimit=1000",
];

/// Combine `branches` onto `onto` in order, entirely inside the object
/// database. A branch already contained is skipped, a branch that contains
/// the result so far fast-forwards, anything else is a true merge, with
/// renames followed (see `COMBINE_CONFIG`).
pub fn combine(
    root: &Path,
    onto: &str,
    branches: &[String],
    deadline: Instant,
) -> Result<Combination, String> {
    if branches.is_empty() || branches.len() > 15 {
        return Err("name between 1 and 15 branches to combine".into());
    }
    let mut current = onto.to_owned();
    let mut inputs = vec![current.clone()];
    let mut renames = Vec::new();
    for branch in branches {
        let theirs = commit(root, branch, deadline)?;
        inputs.push(theirs.clone());
        if is_ancestor(root, &theirs, &current, deadline)? {
            continue;
        }
        if is_ancestor(root, &current, &theirs, deadline)? {
            current = theirs;
            continue;
        }
        let mut arguments = COMBINE_CONFIG.to_vec();
        arguments.extend([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "-z",
            &current,
            &theirs,
        ]);
        let (status, output) = git::run_with_env(root, &arguments, &[], deadline, 1024 * 1024)?;
        let output = String::from_utf8(output).map_err(|_| "Git output is not UTF-8")?;
        let mut fields = output.split('\0');
        let tree = fields.next().unwrap_or_default().to_owned();
        match status.code() {
            Some(0) => {
                let tree = oid(tree)?;
                renames.extend(renames_followed(root, &current, &theirs, branch, deadline)?);
                let message = format!("Merge {branch} (langspan integrate)");
                current = oid(text(
                    root,
                    &[
                        "commit-tree",
                        &tree,
                        "-p",
                        &current,
                        "-p",
                        &theirs,
                        "-m",
                        &message,
                    ],
                    deadline,
                )
                .map_err(|e| {
                    format!("{e}; a merge commit needs your Git user.name and user.email")
                })?)?;
            }
            Some(1) => {
                let tree = oid(tree)?;
                // The conflicted paths, up to an empty field; then Git's
                // messages, each: a path count, the paths, a type, a text.
                let paths: Vec<String> = fields
                    .by_ref()
                    .take_while(|field| !field.is_empty())
                    .map(str::to_owned)
                    .collect();
                let kinds = conflict_kinds(fields);
                let files = paths
                    .iter()
                    .take(CONFLICT_FILES)
                    .map(|path| {
                        conflict_file(root, &tree, [&current, &theirs], path, &kinds, deadline)
                    })
                    .collect::<Result<_, _>>()?;
                return Ok(Combination::Conflict {
                    branch: branch.clone(),
                    paths: paths.into_iter().take(CONFLICT_PATHS).collect(),
                    files,
                });
            }
            _ => return Err(format!("Git could not combine {branch:?}")),
        }
    }
    Ok(Combination::Clean(Merged {
        tree: tree_of(root, &current, deadline)?,
        commit: current,
        inputs,
        renames,
    }))
}

/// The renames between the merge base and `ours` of files `theirs` changed.
fn renames_followed(
    root: &Path,
    ours: &str,
    theirs: &str,
    branch: &str,
    deadline: Instant,
) -> Result<Vec<Rename>, String> {
    let base = oid(text(root, &["merge-base", ours, theirs], deadline)?)?;
    let list = |arguments: &[&str]| -> Result<Vec<String>, String> {
        let mut all = COMBINE_CONFIG.to_vec();
        all.extend_from_slice(arguments);
        let bytes = git::output(root, &all, deadline, 1024 * 1024)?;
        let listing = String::from_utf8(bytes).map_err(|_| "Git output is not UTF-8")?;
        Ok(listing
            .split('\0')
            .filter(|field| !field.is_empty())
            .map(str::to_owned)
            .collect())
    };
    let renamed = list(&[
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--name-status",
        "-z",
        "--find-renames",
        "--diff-filter=R",
        &base,
        ours,
    ])?;
    let changed = list(&[
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--name-only",
        "-z",
        "--no-renames",
        &base,
        theirs,
    ])?;
    // Each rename is three fields: a status such as R087, the old path and
    // the new one.
    Ok(renamed
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|entry| entry[0].starts_with('R') && changed.contains(&entry[1]))
        .map(|entry| Rename {
            branch: branch.into(),
            from: entry[1].clone(),
            to: entry[2].clone(),
        })
        .collect())
}

/// Git's conflict type per path, from `merge-tree -z` messages, keeping only
/// conflicts (`CONFLICT (contents)` becomes `contents`).
fn conflict_kinds<'a>(
    mut fields: impl Iterator<Item = &'a str>,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut kinds = std::collections::BTreeMap::<String, Vec<String>>::new();
    while let Some(count) = fields.next().and_then(|count| count.parse::<usize>().ok()) {
        let paths: Vec<&str> = fields.by_ref().take(count).collect();
        let (Some(kind), Some(_message)) = (fields.next(), fields.next()) else {
            break;
        };
        let Some(kind) = kind
            .strip_prefix("CONFLICT (")
            .and_then(|kind| kind.strip_suffix(')'))
        else {
            continue;
        };
        for path in paths {
            let known = kinds.entry(path.to_owned()).or_default();
            if !known.iter().any(|known| known == kind) && known.len() < 4 {
                known.push(kind.chars().take(40).collect());
            }
        }
    }
    kinds
}

/// The conflicting hunks of `path` in the conflicted `tree`.
fn conflict_file(
    root: &Path,
    tree: &str,
    sides: [&str; 2],
    path: &str,
    kinds: &std::collections::BTreeMap<String, Vec<String>>,
    deadline: Instant,
) -> Result<ConflictFile, String> {
    let mut file = ConflictFile {
        path: path.to_owned(),
        kinds: kinds.get(path).cloned().unwrap_or_default(),
        hunks: Vec::new(),
        more_hunks: 0,
        unshown: None,
    };
    let object = format!("{tree}:{path}");
    let kind = text(root, &["cat-file", "-t", &object], deadline).unwrap_or_default();
    let size = text(root, &["cat-file", "-s", &object], deadline)
        .ok()
        .and_then(|size| size.parse::<usize>().ok());
    let unshown = match (kind.as_str(), size) {
        ("blob", Some(size)) if size <= CONFLICT_FILE_BYTES => None,
        ("blob", Some(_)) => Some("larger than 1 MiB"),
        ("blob", None) => Some("its size is not known"),
        _ => Some("not in the combined tree as a file"),
    };
    if let Some(why) = unshown {
        file.unshown = Some(why.into());
        return Ok(file);
    }
    let bytes = git::output(
        root,
        &["cat-file", "blob", &object],
        deadline,
        CONFLICT_FILE_BYTES,
    )?;
    if bytes.contains(&0) {
        file.unshown = Some("binary".into());
        return Ok(file);
    }
    let (mut hunks, total, resolved) = hunks(&String::from_utf8_lossy(&bytes));
    // Lines outside a conflict include each side's clean changes, so a line
    // number counted in the merged file is not one in either side's file.
    // Each side's starts are counted in the merged file with that side
    // chosen in every conflict, and mapped to the side's own file by Git's
    // diff of the two.
    for (index, (commit, text)) in sides.iter().zip(resolved).enumerate() {
        let map = side_lines(root, commit, path, &text, deadline)?;
        for hunk in &mut hunks {
            let side = if index == 0 {
                &mut hunk.target
            } else {
                &mut hunk.branch
            };
            side.start = match &map {
                Some(map) => side.start.map(|start| map.line(start)),
                None => None,
            };
        }
    }
    file.more_hunks = u32::try_from(total - hunks.len()).unwrap_or(u32::MAX);
    file.hunks = hunks;
    if total == 0 {
        file.unshown =
            Some("no conflicting lines: the conflict is about the file itself (see kinds)".into());
    }
    Ok(file)
}

/// One line of repository text as it is shown: no control characters, at
/// most `CONFLICT_LINE_CHARS` characters.
fn shown(line: &str) -> String {
    line.chars()
        .take(CONFLICT_LINE_CHARS)
        .map(|c| {
            if c.is_control() && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

/// Where lines of a resolved file are in one side's own file: the hunks of
/// `git diff -U0 SIDE RESOLVED`, as (side start, side count, resolved start,
/// resolved count).
struct LineMap(Vec<(u32, u32, u32, u32)>);

impl LineMap {
    /// The side's line for line `n` of the resolved file.
    fn line(&self, n: u32) -> u32 {
        let mut shift = 0_i64;
        for &(old, old_count, new, new_count) in &self.0 {
            let before = if new_count == 0 {
                new < n
            } else {
                new + new_count - 1 < n
            };
            if before {
                shift += i64::from(old_count) - i64::from(new_count);
            } else if new <= n && new_count > 0 {
                // Inside a changed range: the side's range starts here.
                return old.max(1);
            }
        }
        u32::try_from((i64::from(n) + shift).max(1)).unwrap_or(n)
    }
}

/// Map lines of `resolved` (the merged file with this side chosen in every
/// conflict) to `commit`'s own version of `path`. `None` when that version
/// cannot be read as a text file within the bound.
fn side_lines(
    root: &Path,
    commit: &str,
    path: &str,
    resolved: &str,
    deadline: Instant,
) -> Result<Option<LineMap>, String> {
    let object = format!("{commit}:{path}");
    let size = text(root, &["cat-file", "-s", &object], deadline)
        .ok()
        .and_then(|size| size.parse::<usize>().ok());
    if !size.is_some_and(|size| size <= CONFLICT_FILE_BYTES) {
        return Ok(None);
    }
    let own = git::output(
        root,
        &["cat-file", "blob", &object],
        deadline,
        CONFLICT_FILE_BYTES,
    )?;
    if String::from_utf8_lossy(&own).lines().eq(resolved.lines()) {
        return Ok(Some(LineMap(Vec::new())));
    }
    // Two private files, compared outside any repository.
    let dir = std::env::temp_dir().join(format!(
        "langspan-hunks-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let result = (|| {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("cannot create a temporary directory: {e}"))?;
        fs::write(dir.join("side"), &own).map_err(|e| e.to_string())?;
        fs::write(dir.join("resolved"), resolved).map_err(|e| e.to_string())?;
        let (status, output) = git::run_with_env(
            &dir,
            &[
                "-c",
                "diff.noprefix=false",
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "-U0",
                "side",
                "resolved",
            ],
            &[],
            deadline,
            4 * CONFLICT_FILE_BYTES,
        )?;
        if !matches!(status.code(), Some(0 | 1)) {
            return Err("Git could not compare a conflicted file with its side".to_owned());
        }
        let range = |text: &str| -> Option<(u32, u32)> {
            let (start, count) = text.split_once(',').unwrap_or((text, "1"));
            Some((start.parse().ok()?, count.parse().ok()?))
        };
        let mut hunks = Vec::new();
        for line in String::from_utf8_lossy(&output).lines() {
            let Some(header) = line.strip_prefix("@@ -") else {
                continue;
            };
            let mut parts = header.split(' ');
            let (Some(old), Some(new)) = (
                parts.next().and_then(range),
                parts.next().and_then(|new| range(new.strip_prefix('+')?)),
            ) else {
                return Err("Git's diff of a conflicted file was not readable".to_owned());
            };
            hunks.push((old.0, old.1, new.0, new.1));
        }
        Ok(Some(LineMap(hunks)))
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

/// The first `CONFLICT_HUNKS` conflict-marked hunks of a merged file, how
/// many there are in all, and the file resolved to each side (the target's
/// lines in every conflict, then the branch's). Each side's `start` is a
/// line of that resolved file.
fn hunks(merged: &str) -> (Vec<Hunk>, usize, [String; 2]) {
    let marker =
        |line: &str, c: char| line.len() >= 7 && line.starts_with(&c.to_string().repeat(7));
    let side = |start: u32| Side {
        start: Some(start),
        lines: 0,
        text: Vec::new(),
    };
    let add = |side: &mut Side, line: &str| {
        side.lines += 1;
        if side.text.len() < CONFLICT_LINES {
            side.text.push(shown(line));
        }
    };
    enum At {
        Outside,
        Target(Hunk),
        Base(Hunk),
        Branch(Hunk),
    }
    let (mut target_line, mut branch_line) = (0_u32, 0_u32);
    let (mut kept, mut total) = (Vec::new(), 0);
    let (mut target_text, mut branch_text) = (String::new(), String::new());
    let both = |text: &mut String, line: &str| {
        text.push_str(line);
        text.push('\n');
    };
    let mut at = At::Outside;
    for line in merged.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        at = match at {
            At::Outside if marker(line, '<') => At::Target(Hunk {
                target: side(target_line + 1),
                branch: side(branch_line + 1),
            }),
            At::Outside => {
                target_line += 1;
                branch_line += 1;
                both(&mut target_text, line);
                both(&mut branch_text, line);
                At::Outside
            }
            At::Target(hunk) | At::Base(hunk)
                if marker(line, '=') && line.chars().all(|c| c == '=') =>
            {
                At::Branch(hunk)
            }
            At::Target(hunk) if marker(line, '|') => At::Base(hunk),
            At::Base(hunk) => At::Base(hunk),
            At::Target(mut hunk) => {
                target_line += 1;
                both(&mut target_text, line);
                add(&mut hunk.target, line);
                At::Target(hunk)
            }
            At::Branch(hunk) if marker(line, '>') => {
                total += 1;
                if kept.len() < CONFLICT_HUNKS {
                    kept.push(hunk);
                }
                At::Outside
            }
            At::Branch(mut hunk) => {
                branch_line += 1;
                both(&mut branch_text, line);
                add(&mut hunk.branch, line);
                At::Branch(hunk)
            }
        };
    }
    (kept, total, [target_text, branch_text])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    Exited(i32),
    /// Killed by a signal or at the time limit: no result.
    Unfinished,
}

/// Run the owner's check command in `dir`, without a shell. Its output goes
/// to standard error, because standard output is reserved for results.
pub fn run_command(dir: &Path, argv: &[String], limit: Duration) -> Result<(Ran, u64), String> {
    let (program, args) = argv.split_first().ok_or("give a command after --")?;
    use std::os::unix::process::CommandExt;
    let started = Instant::now();
    // Its own process group, so everything it starts can be stopped with it.
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(std::io::stderr())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot start {program:?}: {e}"))?;
    let group = rustix::process::Pid::from_child(&child);
    let ran = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status.code().map(Ran::Exited).unwrap_or(Ran::Unfinished),
            None if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break Ran::Unfinished;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Whatever it left running, on time-out or after a normal exit, would
    // go on changing the tree after its result was taken. Stop all of it.
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    Ok((
        ran,
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    ))
}

/// Check out `commit` into a temporary detached worktree, run `body` there,
/// and remove it again whatever happened.
pub fn with_checkout<T>(
    root: &Path,
    commit: &str,
    deadline: Instant,
    body: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let dir = std::env::temp_dir().join(format!(
        "langspan-check-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let path = dir.to_str().ok_or("temporary directory is not UTF-8")?;
    let status = git::run_network(
        root,
        &["worktree", "add", "--detach", "--quiet", path, commit],
        deadline,
    )?;
    if !status.success() {
        return Err("Git could not create a temporary checkout of the combined result".into());
    }
    let result = body(&dir);
    let removal = Instant::now() + Duration::from_secs(30);
    let _ = git::run_network(root, &["worktree", "remove", "--force", path], removal);
    let _ = fs::remove_dir_all(&dir);
    let _ = git::run_network(root, &["worktree", "prune"], removal);
    result
}

/// The remote's current tip of `onto`, fetched into a LangSpan ref so the
/// owner's remote-tracking branches stay as they were.
pub fn remote_tip(
    root: &Path,
    remote: &str,
    onto: &str,
    deadline: Instant,
) -> Result<String, String> {
    let (remote, onto) = (revision(remote)?, revision(onto)?);
    // Combining needs the branches' shared history. A shallow clone cuts it
    // off, so a merge base can be missing or wrong. Refuse before fetching.
    if text(root, &["rev-parse", "--is-shallow-repository"], deadline)? != "false" {
        return Err(
            "this clone is shallow; combining branches needs their history: run `git fetch --unshallow`, then try again. Nothing was fetched, combined or pushed".into(),
        );
    }
    let local = format!("refs/langspan/land/{remote}/{onto}");
    let refspec = format!("+refs/heads/{onto}:{local}");
    let status = git::run_network(
        root,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            remote,
            &refspec,
        ],
        deadline,
    )?;
    if !status.success() {
        return Err(format!(
            "could not fetch {onto:?} from {remote:?}; nothing was done"
        ));
    }
    oid(text(root, &["rev-parse", "--verify", &local], deadline)?)
}

/// The remote's tip of `onto` as the remote reports it now (`git
/// ls-remote`), without fetching or changing any local ref. `None` when the
/// remote has no such branch.
pub fn remote_branch_tip(
    root: &Path,
    remote: &str,
    onto: &str,
    deadline: Instant,
) -> Result<Option<String>, String> {
    let branch = format!("refs/heads/{}", revision(onto)?);
    let (status, bytes) = git::run_network_output(
        root,
        &["ls-remote", revision(remote)?, &branch],
        deadline,
        64 * 1024,
    )?;
    if !status.success() {
        return Err(format!("could not ask {remote:?} for {branch}"));
    }
    let listing = String::from_utf8(bytes).map_err(|_| "Git output is not UTF-8")?;
    for line in listing.lines() {
        if let Some((oid_text, name)) = line.split_once('\t')
            && name == branch
        {
            return oid(oid_text.to_owned()).map(Some);
        }
    }
    Ok(None)
}

/// Push `commit` to the remote's `onto` branch. Never forced: if the branch
/// moved since `remote_tip`, the remote refuses and `Ok(false)` is returned.
/// The owner's pre-push hooks run, as for any push of their code.
pub fn push(
    root: &Path,
    remote: &str,
    onto: &str,
    commit: &str,
    deadline: Instant,
) -> Result<bool, String> {
    let target = format!("{commit}:refs/heads/{}", revision(onto)?);
    Ok(git::run_network(
        root,
        &["push", "--quiet", revision(remote)?, &target],
        deadline,
    )?
    .success())
}
