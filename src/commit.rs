//! Commits in a checkout shared by several sessions
//! ([ADR 0026](../docs/adr/0026-commit-guard-and-commit-mine.md)).
//!
//! The guard: before a shell command that stages or commits broadly runs,
//! the lines it would take are matched against the line journal; when some
//! are another live session's, the command is held once, naming that
//! session and its files. `commit_mine`: stage only this session's lines in
//! a private index and commit them with the owner's identity and hooks.
//!
//! A line belongs to the sessions whose journaled writes added it (or, for
//! a removed line, removed it). A hunk is this session's when each of its
//! lines is, and not every line is also another's; another session's when
//! none of its lines is this session's and some are that session's; nobody's
//! when no session wrote any of it. Anything else is not known, and
//! `commit_mine` refuses it rather than guess. Files Git cannot diff by line
//! (binary, over 512 KiB, a mode change) belong to the session whose
//! journaled write left them as they are now.

use crate::{
    hook::{Config, Reply, ShellCall},
    journal::{self, Entry},
    shell::GitStep,
};
use langspan_core::{Command, FileState, Outcome};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Diff output read for one decision, at most.
const MAX_DIFF_BYTES: usize = 16 * 1024 * 1024;
/// Untracked files read for one decision, at most.
const MAX_UNTRACKED: usize = 1024;
/// Files and sessions named in one held command's reason, at most.
const NAMED: usize = 8;
/// The longest `git commit` this waits for (its hooks run inside it).
const COMMIT_TIMEOUT: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------------------
// Diffs
// ---------------------------------------------------------------------------

/// One hunk of a zero-context diff: the lines it removes and adds, by key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub removed: Vec<String>,
    pub added: Vec<String>,
    /// The hunk's lines as Git printed them, after its `@@` line.
    body: Vec<u8>,
}

/// One file's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The lines from `diff --git` to `+++`, as Git printed them.
    header: Vec<String>,
    pub hunks: Vec<Hunk>,
    /// Not comparable line by line: binary, over the bound, or a change of
    /// mode only. Attributed as a whole file.
    pub whole: bool,
    pub created: bool,
    pub deleted: bool,
    /// Not known to Git: staged by adding the file.
    pub untracked: bool,
    /// The file's mode changed. No write records who changed a mode, so it
    /// is never committed with a session's lines.
    pub mode_change: bool,
}

fn parse_range(text: &str) -> Option<(u32, u32)> {
    let (start, len) = match text.split_once(',') {
        Some((start, len)) => (start, len),
        None => (text, "1"),
    };
    Some((start.parse().ok()?, len.parse().ok()?))
}

/// The path of a `diff --git a/P b/P` line. Without renames both halves are
/// the same path, so the line splits exactly in two whatever P holds.
fn diff_path(rest: &str) -> Option<String> {
    let rest = rest.strip_prefix("a/")?;
    if rest.len() < 3 || (rest.len() - 3) % 2 != 0 {
        return None;
    }
    let half = (rest.len() - 3) / 2;
    let (path, other) = (rest.get(..half)?, rest.get(half..)?);
    (other.strip_prefix(" b/")? == path && !path.starts_with('"')).then(|| path.to_owned())
}

/// Parse `git diff -U0 --no-renames` output.
pub fn parse_diff(bytes: &[u8]) -> Result<Vec<FileDiff>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "a diff that is not UTF-8")?;
    let mut files: Vec<FileDiff> = Vec::new();
    // The sign and content of the last body line, for "\ No newline".
    let mut last: Option<(char, String)> = None;
    for line in text.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = diff_path(rest.trim_end_matches('\n'))
                .ok_or("a changed path Git quotes or that cannot be read")?;
            files.push(FileDiff {
                path,
                header: vec![line.trim_end_matches('\n').to_owned()],
                hunks: Vec::new(),
                whole: false,
                created: false,
                deleted: false,
                untracked: false,
                mode_change: false,
            });
            last = None;
            continue;
        }
        let file = files
            .last_mut()
            .ok_or("a diff that does not start with a file")?;
        if let Some(header) = line.strip_prefix("@@ -") {
            let (ranges, _) = header.split_once(" @@").ok_or("a malformed hunk header")?;
            let (old, new) = ranges.split_once(" +").ok_or("a malformed hunk header")?;
            let ((old_start, old_len), (new_start, new_len)) = (
                parse_range(old).ok_or("a malformed hunk range")?,
                parse_range(new).ok_or("a malformed hunk range")?,
            );
            file.hunks.push(Hunk {
                old_start,
                old_len,
                new_start,
                new_len,
                removed: Vec::new(),
                added: Vec::new(),
                body: Vec::new(),
            });
            last = None;
            continue;
        }
        let Some(hunk) = file.hunks.last_mut() else {
            let line = line.trim_end_matches('\n');
            if line.starts_with("new file mode") {
                file.created = true;
            } else if line.starts_with("deleted file mode") {
                file.deleted = true;
            } else if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
                file.whole = true;
            }
            if line.starts_with("old mode ") {
                file.mode_change = true;
            }
            file.header.push(line.to_owned());
            continue;
        };
        hunk.body.extend_from_slice(line.as_bytes());
        match line.chars().next() {
            Some(sign @ ('+' | '-')) => {
                let content = &line[1..];
                let key = journal::line_key(content.as_bytes());
                if sign == '+' {
                    hunk.added.push(key);
                } else {
                    hunk.removed.push(key);
                }
                last = Some((sign, content.to_owned()));
            }
            Some('\\') => {
                // The line before has no terminator: its key is without it.
                let (sign, content) = last.take().ok_or("a misplaced no-newline marker")?;
                let key = journal::line_key(content.trim_end_matches('\n').as_bytes());
                let side = if sign == '+' {
                    &mut hunk.added
                } else {
                    &mut hunk.removed
                };
                if let Some(previous) = side.last_mut() {
                    *previous = key;
                }
            }
            _ => return Err("an unexpected line in a hunk".into()),
        }
    }
    for file in &mut files {
        if file.hunks.is_empty() {
            file.whole = true;
        }
    }
    Ok(files)
}

/// The patch that applies `chosen` hunks of `file` to the version its diff
/// started from. Later hunks' new positions account for the hunks left out.
fn patch(file: &FileDiff, chosen: &[usize]) -> Vec<u8> {
    let partial = chosen.len() != file.hunks.len();
    let mut out = Vec::new();
    for line in &file.header {
        // The postimage's blob ID is not the result of a partial patch.
        // Nor is a change of mode, which belongs to the whole file.
        // A change of mode is nobody's: it is left out of every patch.
        if line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || (partial || file.mode_change) && line.starts_with("index ")
        {
            continue;
        }
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    let mut delta: i64 = 0;
    for &index in chosen {
        let hunk = &file.hunks[index];
        let new_start = i64::from(hunk.old_start) + delta + i64::from(hunk.old_len == 0)
            - i64::from(hunk.new_len == 0);
        out.extend_from_slice(
            format!(
                "@@ -{},{} +{},{} @@\n",
                hunk.old_start,
                hunk.old_len,
                new_start.max(0),
                hunk.new_len
            )
            .as_bytes(),
        );
        out.extend_from_slice(&hunk.body);
        delta += i64::from(hunk.new_len) - i64::from(hunk.old_len);
    }
    out
}

// ---------------------------------------------------------------------------
// Attribution
// ---------------------------------------------------------------------------

/// Whose a hunk, or a whole file, is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    Own,
    /// No line of it is this session's; these sessions wrote some of it.
    Others(BTreeSet<String>),
    /// No session wrote any of it.
    Nobody,
    /// Not known: why.
    Unknown(&'static str),
}

/// This session or one of its subagents (a subagent's ID is its parent's
/// followed by `.` and the agent's own).
pub(crate) fn is_own(me: &str, session: &str) -> bool {
    session == me
        || session
            .strip_prefix(me)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn hunk_owner(me: &str, hunk: &Hunk, path: Option<&journal::FileLines<'_>>) -> Owner {
    let empty = BTreeSet::new();
    let lines = hunk
        .removed
        .iter()
        .map(|key| {
            path.and_then(|p| p.removed.get(key.as_str()))
                .unwrap_or(&empty)
        })
        .chain(hunk.added.iter().map(|key| {
            path.and_then(|p| p.added.get(key.as_str()))
                .unwrap_or(&empty)
        }));
    let (mut total, mut own, mut shared) = (0, 0, 0);
    let mut others = BTreeSet::new();
    for claimants in lines {
        total += 1;
        let mine = claimants.iter().any(|s| is_own(me, s));
        let theirs: Vec<&&str> = claimants.iter().filter(|s| !is_own(me, s)).collect();
        if mine {
            own += 1;
            if !theirs.is_empty() {
                shared += 1;
            }
        } else {
            others.extend(theirs.into_iter().map(|s| (*s).to_owned()));
        }
    }
    let unknown_own = path.is_some_and(|p| p.unknown.iter().any(|s| is_own(me, s)));
    if own == 0 {
        return match (others.is_empty(), unknown_own) {
            (false, _) => Owner::Others(others),
            (true, true) => {
                Owner::Unknown("this session changed this file in lines that are not known")
            }
            (true, false) => Owner::Nobody,
        };
    }
    if own == total {
        return if shared == total {
            Owner::Unknown("each of its lines was written by this session and by another")
        } else {
            Owner::Own
        };
    }
    Owner::Unknown("part of it was written by this session and part by another session or by none")
}

/// The owner of a file attributed whole: the session whose journaled write
/// left it as it is now.
fn whole_owner(
    me: &str,
    current: Option<&FileState>,
    path: Option<&journal::FileLines<'_>>,
) -> Owner {
    let Some(path) = path else {
        return Owner::Nobody;
    };
    let latest = current.and_then(|current| {
        path.versions
            .iter()
            .filter(|(_, _, after)| *after == current)
            .max_by_key(|(at, _, _)| *at)
    });
    match latest {
        Some((_, session, _)) if is_own(me, session) => Owner::Own,
        Some((_, session, _)) => Owner::Others(BTreeSet::from([(*session).to_owned()])),
        None if path.versions.iter().any(|(_, s, _)| is_own(me, s)) => {
            Owner::Unknown("this session changed this file, and it has changed again since")
        }
        None => Owner::Nobody,
    }
}

/// How one file splits between this session and the rest.
struct FileOwners {
    /// Per hunk, or one entry for a file attributed whole.
    owners: Vec<Owner>,
    /// For each owner: other sessions that wrote some of it where this
    /// session did not, whoever else did. What the guard holds a command for.
    foreign: Vec<BTreeSet<String>>,
    /// Attributed as one whole file.
    whole: bool,
}

/// Of a hunk's lines, those no own session wrote: the sessions that did.
fn foreign_sessions(
    me: &str,
    hunk: &Hunk,
    path: Option<&journal::FileLines<'_>>,
) -> BTreeSet<String> {
    let Some(path) = path else {
        return BTreeSet::new();
    };
    let removed = hunk
        .removed
        .iter()
        .filter_map(|key| path.removed.get(key.as_str()));
    let added = hunk
        .added
        .iter()
        .filter_map(|key| path.added.get(key.as_str()));
    removed
        .chain(added)
        .filter(|claimants| !claimants.iter().any(|s| is_own(me, s)))
        .flatten()
        .map(|s| (*s).to_owned())
        .collect()
}

fn file_owners(
    me: &str,
    root: &Path,
    file: &FileDiff,
    claims: &BTreeMap<&str, journal::FileLines<'_>>,
    current: &impl Fn(&Path, &str) -> Option<FileState>,
) -> FileOwners {
    let path = claims.get(file.path.as_str());
    // A write whose lines are not known, or a file too large to journal by
    // line, makes the file attributable only whole.
    let oversize = fs::symlink_metadata(root.join(&file.path))
        .is_ok_and(|m| m.len() > journal::MAX_FILE_BYTES);
    let whole = file.whole || oversize || path.is_some_and(|p| !p.unknown.is_empty());
    if whole {
        let others: BTreeSet<String> = path
            .map(|p| {
                p.versions
                    .iter()
                    .filter(|(_, s, _)| !is_own(me, s))
                    .map(|(_, s, _)| (*s).to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut owner = whole_owner(me, current(root, &file.path).as_ref(), path);
        if owner == Owner::Own && !others.is_empty() {
            owner = Owner::Unknown(
                "another session also changed this file, and its lines cannot be told apart",
            );
        }
        let foreign = if owner == Owner::Nobody {
            BTreeSet::new()
        } else {
            others
        };
        return FileOwners {
            owners: vec![owner],
            foreign: vec![foreign],
            whole: true,
        };
    }
    FileOwners {
        owners: file
            .hunks
            .iter()
            .map(|hunk| hunk_owner(me, hunk, path))
            .collect(),
        foreign: file
            .hunks
            .iter()
            .map(|hunk| foreign_sessions(me, hunk, path))
            .collect(),
        whole: false,
    }
}

/// Whether `session`, or the session it is a subagent of, is live.
fn is_live(live: &BTreeSet<String>, session: &str) -> bool {
    let mut name = session;
    loop {
        if live.contains(name) {
            return true;
        }
        match name.rsplit_once('.') {
            // `source.host` is the shortest session ID.
            Some((parent, _)) if parent.contains('.') => name = parent,
            _ => return false,
        }
    }
}

/// Lines `start`-`end` of a hunk, for people.
fn lines_of(hunk: &Hunk) -> String {
    let (start, len) = if hunk.new_len == 0 {
        (hunk.old_start, hunk.old_len)
    } else {
        (hunk.new_start, hunk.new_len)
    };
    if len <= 1 {
        format!("line {start}")
    } else {
        format!("lines {start}-{}", start + len - 1)
    }
}

// ---------------------------------------------------------------------------
// Reading what a command would take
// ---------------------------------------------------------------------------

const DIFF: [&str; 12] = [
    "-c",
    "core.quotePath=false",
    "diff",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--no-renames",
    "--ignore-submodules=all",
    "--no-relative",
    "-U0",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

fn diff(
    dir: &Path,
    extra: &[&str],
    pathspecs: &[String],
    deadline: Instant,
) -> Result<Vec<FileDiff>, String> {
    let mut args: Vec<&str> = DIFF.to_vec();
    args.extend_from_slice(extra);
    args.push("--");
    args.extend(pathspecs.iter().map(String::as_str));
    let bytes = git_in(dir, &args, None, deadline)?;
    parse_diff(&bytes)
}

/// Untracked, unignored files under `pathspecs`, each as a new file whose
/// every line is added.
fn untracked(
    root: &Path,
    dir: &Path,
    pathspecs: &[String],
    deadline: Instant,
) -> Result<Vec<FileDiff>, String> {
    let mut args = vec![
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
        "--full-name",
        "--",
    ];
    // Without a pathspec, `git add -A` takes the whole tree, wherever it runs.
    if pathspecs.is_empty() {
        args.push(":/");
    }
    args.extend(pathspecs.iter().map(String::as_str));
    let listed = git_bounded(dir, &args, None, deadline, 1024 * 1024)?;
    let mut files = Vec::new();
    for path in listed.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        if files.len() >= MAX_UNTRACKED {
            return Err(format!("more than {MAX_UNTRACKED} untracked files"));
        }
        let path = std::str::from_utf8(path).map_err(|_| "an untracked path that is not UTF-8")?;
        let full = root.join(path);
        let metadata = fs::symlink_metadata(&full).map_err(|e| format!("{path}: {e}"))?;
        let content = (metadata.is_file() && metadata.len() <= journal::MAX_FILE_BYTES)
            .then(|| fs::read(&full).ok())
            .flatten()
            .filter(|bytes| !bytes.is_empty() && !bytes.contains(&0));
        let mut file = FileDiff {
            path: path.to_owned(),
            header: Vec::new(),
            hunks: Vec::new(),
            whole: content.is_none(),
            created: true,
            deleted: false,
            untracked: true,
            mode_change: false,
        };
        if let Some(content) = content {
            let added = journal::line_keys(&content);
            file.hunks.push(Hunk {
                old_start: 0,
                old_len: 0,
                new_start: 1,
                new_len: added.len() as u32,
                removed: Vec::new(),
                added,
                body: Vec::new(),
            });
        }
        files.push(file);
    }
    Ok(files)
}

/// What the Git steps of one command would put into the index or a commit.
fn taken(
    root: &Path,
    start: &Path,
    steps: &[GitStep],
    deadline: Instant,
) -> Result<Vec<FileDiff>, String> {
    let mut files = Vec::new();
    for step in steps {
        let cwd = match step {
            GitStep::Add { cwd, .. } | GitStep::Commit { cwd, .. } => cwd,
        };
        let dir = match cwd {
            Some(cwd) => start.join(cwd),
            None => start.to_path_buf(),
        };
        // A command for another repository is not this checkout's concern.
        let Ok(dir) = fs::canonicalize(&dir) else {
            return Err("a Git command in a directory that does not exist".into());
        };
        if !dir.starts_with(root) {
            continue;
        }
        match step {
            GitStep::Add {
                pathspecs, update, ..
            } => {
                files.extend(diff(&dir, &[], pathspecs, deadline)?);
                if !update {
                    files.extend(untracked(root, &dir, pathspecs, deadline)?);
                }
            }
            GitStep::Commit {
                all,
                include,
                pathspecs,
                ..
            } => {
                if pathspecs.is_empty() || *include {
                    files.extend(diff(&dir, &["--cached"], &[], deadline)?);
                }
                if !pathspecs.is_empty() {
                    files.extend(diff(&dir, &["--cached"], pathspecs, deadline)?);
                    files.extend(diff(&dir, &[], pathspecs, deadline)?);
                } else if *all {
                    files.extend(diff(&dir, &[], &[], deadline)?);
                }
            }
        }
    }
    Ok(files)
}

// ---------------------------------------------------------------------------
// The guard
// ---------------------------------------------------------------------------

/// The guard's answer for one shell command.
pub enum Guarded {
    /// The command stages and commits nothing.
    NotACommit,
    /// Held: the hook's deny reply.
    Held(Reply),
    /// Not held, and why, for the hook's detail.
    Passed(String),
}

/// The other sessions of the workspace that are live: registered, not
/// ended and not known to have exited.
fn live_sessions(config: &Config, deadline: Instant) -> Result<BTreeSet<String>, String> {
    let request = crate::protocol::Request {
        version: crate::protocol::PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Recap {
            workspace_id: config.workspace_id.clone(),
        },
    };
    let response = crate::client::call_until(&config.state_dir, &request, deadline)
        .map_err(|e| format!("coordinator: {e}"))?;
    match response.outcome {
        Outcome::Recap { recap } => Ok(recap
            .sessions
            .into_iter()
            .filter(|session| {
                !session.ended && session.liveness != Some(langspan_core::LivenessStatus::Exited)
            })
            .map(|session| session.session_id)
            .collect()),
        other => Err(format!("coordinator answered {other:?}")),
    }
}

/// The commit guard, from the Bash pre-tool hook.
pub fn guard_hook(config: &Config, me: &str, call: &ShellCall, deadline: Instant) -> Guarded {
    let steps = match crate::shell::git_steps(&call.command) {
        Ok(steps) if steps.is_empty() => return Guarded::NotACommit,
        Ok(steps) => steps,
        Err(why) => return Guarded::Passed(format!("commit guard: no decision ({why})")),
    };
    let passed = |why: String| Guarded::Passed(format!("commit guard: {why}"));
    let entries = journal::entries(&config.state_dir, &config.root);
    if !entries.iter().any(|entry| !is_own(me, &entry.session_id)) {
        return passed("no other session's uncommitted lines are journaled".into());
    }
    // Decisions get two thirds of the time left; the shell snapshot that
    // follows needs the rest.
    let now = Instant::now();
    let until = now + deadline.saturating_duration_since(now) * 2 / 3;
    let live = match live_sessions(config, until) {
        Ok(live) => live,
        Err(error) => return passed(format!("no decision ({error})")),
    };
    let live_others: Vec<&Entry> = entries
        .iter()
        .filter(|entry| !is_own(me, &entry.session_id) && is_live(&live, &entry.session_id))
        .collect();
    if live_others.is_empty() {
        return passed("no live session's uncommitted lines are journaled".into());
    }
    let start = match &call.cwd {
        Some(cwd) => PathBuf::from(cwd),
        None => config.root.clone(),
    };
    let files = match taken(&config.root, &start, &steps, until) {
        Ok(files) => files,
        Err(error) => return passed(format!("no decision ({error})")),
    };
    let claims = journal::by_file(&entries);
    // Session -> path -> where.
    let mut found: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    let mut evidence = Vec::new();
    for file in &files {
        let owners = file_owners(me, &config.root, file, &claims, &crate::hook::file_state);
        for (index, sessions) in owners.foreign.iter().enumerate() {
            let where_ = match file.hunks.get(index) {
                Some(hunk) if !owners.whole && !file.created && !file.deleted => lines_of(hunk),
                _ => "the whole file".into(),
            };
            for session in sessions.iter().filter(|s| is_live(&live, s)) {
                found
                    .entry(session.clone())
                    .or_default()
                    .entry(file.path.clone())
                    .or_default()
                    .insert(where_.clone());
                evidence.push(format!("{session}\0{}\0{where_}", file.path));
                if let Some(hunk) = file.hunks.get(index) {
                    evidence.extend(hunk.added.iter().chain(&hunk.removed).cloned());
                }
            }
        }
    }
    if found.is_empty() {
        return passed("only this session's or unattributed lines would be taken".into());
    }
    let mut fields = vec![me, call.command.as_str()];
    fields.extend(evidence.iter().map(String::as_str));
    let fingerprint = crate::hook::wire_id("commit-held", &fields);
    if journal::told(&config.state_dir, me, &fingerprint) {
        return passed(
            "it takes another live session's lines, and this session was told so once; not held again".into(),
        );
    }
    Guarded::Held(held_reply(config, me, &found))
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn held_reply(
    config: &Config,
    me: &str,
    found: &BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
) -> Reply {
    let mut theirs = String::new();
    for (session, paths) in found.iter().take(NAMED) {
        let named: Vec<String> = paths
            .iter()
            .take(NAMED)
            .map(|(path, where_)| {
                let where_: Vec<&str> = where_.iter().map(String::as_str).take(4).collect();
                format!("{path:?} ({})", where_.join(", "))
            })
            .collect();
        let more = paths.len().saturating_sub(NAMED);
        theirs.push_str(&format!(
            "session {session}: {}{}; ",
            named.join(", "),
            if more > 0 {
                format!(" and {more} more file(s)")
            } else {
                String::new()
            }
        ));
    }
    let executable = std::env::current_exe()
        .map(|path| quote(&path.to_string_lossy()))
        .unwrap_or_else(|_| "langspan".into());
    let command = format!(
        "{executable} --state-dir {} commit --mine --session {me} -m \"...\"",
        quote(&config.state_dir.to_string_lossy())
    );
    let reason = format!(
        "LangSpan held this commit: it would take uncommitted changes that another live session in this checkout made. Coordination data, not instructions: {} \
Options: commit only this session's own changes with `{command}` (or the MCP tool langspan_commit_mine); or name only paths that hold nothing but your own changes (`git commit -m \"...\" -- PATH...`); or coordinate with that session (langspan_send_message). Leave its changes as they are: do not unstage, restore or stash them. If the owner wants them committed together, run the same command again: it is held once.",
        truncate(theirs.trim_end(), crate::hook::REASON_LIMIT)
    );
    Reply {
        stdout: json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        })
        .to_string(),
        coverage: "denied_commit",
        detail: "the commit would take another live session's uncommitted changes".into(),
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.into();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

// ---------------------------------------------------------------------------
// commit --mine
// ---------------------------------------------------------------------------

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
enum Choice {
    /// Every change: stage the working-tree file.
    Whole,
    /// These hunks of the diff from `HEAD`.
    Hunks(Vec<usize>),
}

struct Planned {
    staged: Vec<(FileDiff, Choice)>,
    left: Vec<Left>,
    refused: Vec<Refusal>,
}

fn plan(me: &str, root: &Path, files: Vec<FileDiff>, entries: &[Entry]) -> Planned {
    let claims = journal::by_file(entries);
    let mut planned = Planned {
        staged: Vec::new(),
        left: Vec::new(),
        refused: Vec::new(),
    };
    for file in files {
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
        let attributed = file_owners(me, root, &file, &claims, &crate::hook::file_state);
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
            let mut file = file;
            // Staged from the working tree, not by patch.
            file.whole |= attributed.whole;
            planned.staged.push((file, Choice::Whole));
            continue;
        }
        if file.created || file.deleted || file.whole {
            planned.refused.push(Refusal {
                path: file.path.clone(),
                lines: "the whole file".into(),
                reason: "a new or removed file holds this session's changes and others'; it can be committed only whole".into(),
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

fn git_in(
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
fn git_bounded(
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
    let command: String = crate::hook::redact(&args.join(" "))
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

/// Stage `staged` into the index at `index` (the shared one when `None`).
/// Hunks are applied with `git apply --cached`; whole files are added from
/// the working tree.
fn stage(
    root: &Path,
    git_dir: &Path,
    staged: &[(FileDiff, Choice)],
    index: Option<&Path>,
    deadline: Instant,
) -> Result<(), String> {
    let mut whole: Vec<&str> = Vec::new();
    let mut patches = Vec::new();
    for (file, choice) in staged {
        match choice {
            Choice::Whole if file.whole || file.untracked => whole.push(&file.path),
            Choice::Whole => {
                let all: Vec<usize> = (0..file.hunks.len()).collect();
                patches.extend(patch(file, &all));
            }
            Choice::Hunks(hunks) => patches.extend(patch(file, hunks)),
        }
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

/// The session `langspan commit --mine` was run by, when not given: the one
/// whose shell call running it now was recorded by the pre-tool hook.
pub fn running_session(state_dir: &Path, root: &Path) -> Result<String, crate::Error> {
    let root = fs::canonicalize(root)?;
    let sessions = crate::shell::committing_sessions(state_dir, &root);
    match sessions.len() {
        1 => Ok(sessions.into_iter().next().unwrap_or_default()),
        0 => Err(no_running_session(state_dir, &root).into()),
        _ => Err(format!(
            "several sessions are running `commit --mine` here now ({}); pass --session ID",
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
    let looked = crate::install::quote(&state_dir.to_string_lossy());
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
            crate::install::Vendor::ALL
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
            crate::install::quote(&other.to_string_lossy()),
            crate::install::quote(&other.to_string_lossy()),
        ),
        None => format!(
            "no session is running this command through its shell tool in the state directory {looked}; if LangSpan's hooks were installed with --state-dir, pass the same --state-dir, {session}"
        ),
    }
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
    let root = fs::canonicalize(top)?;
    let git_dir = PathBuf::from(git_dir);
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
        let planned = plan(session_id, &root, files, &entries);
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
            .unwrap_or(commit),
    );
    report.result = "committed";
    // The shared index learns the commit: this session's changes, on top of
    // whatever else is staged there, so they do not show as staged undone.
    let committed: BTreeSet<String> = report.paths.iter().cloned().collect();
    if let Err(error) =
        update_shared_index(&root, &git_dir, session_id, &committed, &entries, deadline)
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
    deadline: Instant,
) -> Result<(), String> {
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
    let planned = plan(me, root, files, entries);
    if let Some(refusal) = planned.refused.first() {
        return Err(format!(
            "{} ({}) does not split the same way against it: {}",
            refusal.path, refusal.lines, refusal.reason
        ));
    }
    stage(root, git_dir, &planned.staged, None, deadline)
}

fn current_head(root: &Path, deadline: Instant) -> Result<String, String> {
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
fn private_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "langspan-mine-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        crate::shell::now_ms()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF_TEXT: &str = "diff --git a/f.txt b/f.txt\nindex 1111111..2222222 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -2 +2 @@ x\n-line 2\n+line 2, mine\n@@ -18,0 +19,2 @@\n+theirs\n+more\n@@ -20 +21 @@\n-end\n+end\n\\ No newline at end of file\ndiff --git a/a b c b/a b c\nnew file mode 100644\nindex 0000000..3333333\nBinary files /dev/null and b/a b c differ\n";

    #[test]
    fn a_zero_context_diff_is_read_into_files_hunks_and_line_keys() {
        let files = parse_diff(DIFF_TEXT.as_bytes()).unwrap();
        assert_eq!(files.len(), 2);
        let f = &files[0];
        assert_eq!(f.path, "f.txt");
        assert_eq!(f.hunks.len(), 3);
        assert_eq!((f.hunks[1].old_start, f.hunks[1].old_len), (18, 0));
        assert_eq!(f.hunks[0].removed, vec![journal::line_key(b"line 2\n")]);
        assert_eq!(f.hunks[0].added, vec![journal::line_key(b"line 2, mine\n")]);
        // The last line lost its newline: a changed line, keyed without it.
        assert_eq!(f.hunks[2].added, vec![journal::line_key(b"end")]);
        assert_eq!(f.hunks[2].removed, vec![journal::line_key(b"end\n")]);
        assert_eq!(files[1].path, "a b c");
        assert!(files[1].whole && files[1].created);
        assert!(parse_diff(b"diff --git \"a/q\\tx\" \"b/q\\tx\"\n").is_err());
    }

    #[test]
    fn a_partial_patch_renumbers_the_hunks_it_keeps() {
        let files = parse_diff(DIFF_TEXT.as_bytes()).unwrap();
        let text = String::from_utf8(patch(&files[0], &[1, 2])).unwrap();
        assert!(!text.contains("index "), "{text}");
        assert!(
            text.contains("@@ -18,0 +19,2 @@\n+theirs\n+more\n"),
            "{text}"
        );
        assert!(text.contains("@@ -20,1 +22,1 @@\n"), "{text}");
        let skipped = String::from_utf8(patch(&files[0], &[2])).unwrap();
        assert!(skipped.contains("@@ -20,1 +20,1 @@\n"), "{skipped}");
    }

    fn entry(session: &str, added: &[&[u8]], removed: &[&[u8]]) -> Entry {
        Entry {
            session_id: session.into(),
            path: "f.txt".into(),
            at_ms: 1,
            after: FileState::Missing,
            lines: Some(journal::Lines {
                added: added.iter().map(|l| journal::line_key(l)).collect(),
                removed: removed.iter().map(|l| journal::line_key(l)).collect(),
            }),
        }
    }

    #[test]
    fn a_hunk_is_its_writers_and_anything_mixed_is_not_known() {
        let files = parse_diff(DIFF_TEXT.as_bytes()).unwrap();
        let entries = vec![
            entry("me", &[b"line 2, mine\n"], &[b"line 2\n"]),
            entry("me.sub", &[b"end"], &[b"end\n"]),
            entry("them", &[b"theirs\n", b"more\n"], &[]),
        ];
        let claims = journal::by_file(&entries);
        let owner = |index: usize| hunk_owner("me", &files[0].hunks[index], claims.get("f.txt"));
        assert_eq!(owner(0), Owner::Own);
        assert_eq!(owner(1), Owner::Others(BTreeSet::from(["them".to_owned()])));
        // A subagent's lines are its parent's own.
        assert_eq!(owner(2), Owner::Own);
        let none: BTreeMap<&str, journal::FileLines<'_>> = BTreeMap::new();
        assert_eq!(
            hunk_owner("me", &files[0].hunks[0], none.get("f.txt")),
            Owner::Nobody
        );

        let mixed = vec![
            entry("me", &[b"theirs\n"], &[]),
            entry("them", &[b"more\n"], &[]),
        ];
        let claims = journal::by_file(&mixed);
        assert!(matches!(
            hunk_owner("me", &files[0].hunks[1], claims.get("f.txt")),
            Owner::Unknown(_)
        ));
        let both = vec![
            entry("me", &[b"theirs\n", b"more\n"], &[]),
            entry("them", &[b"theirs\n", b"more\n"], &[]),
        ];
        let claims = journal::by_file(&both);
        assert!(matches!(
            hunk_owner("me", &files[0].hunks[1], claims.get("f.txt")),
            Owner::Unknown(_)
        ));
        assert!(!is_own("me", "meow") && is_own("me", "me.agent"));
    }
}
