//! Commits in a checkout shared by several sessions
//! ([ADR 0026](../../docs/adr/0026-commit-guard-and-commit-mine.md)).
//!
//! The guard: before a shell command that stages or commits broadly runs,
//! the lines it would take are matched against the line journal; when some
//! are another live session's, the command is held once, naming that
//! session and its files. `commit_mine`: stage only this session's lines in
//! a private index and commit them with the owner's identity and hooks.
//!
//! A line belongs to the sessions whose journaled writes added it (or, for
//! a removed line, removed it). A hunk Git joined from this session's lines
//! and other sessions' lines is first split where they meet, unless one of
//! its lines is no session's, one session's line replaced another's, or the
//! sessions' lines could be in more than one order (issue #106); a partial
//! patch must keep this session's lines in their one place among the other
//! sessions' and apply exactly to the index it is staged in, or nothing is
//! committed. A line both this session and another wrote (a blank line, a
//! closing brace) goes with the lines on either side of it in the hunk when
//! they are one side's, or with the only side next to it at the hunk's edge;
//! it goes with another session's lines only while this session's writes
//! still account for each copy of it elsewhere in the file's diff (issue
//! #141). A hunk is this session's when each of its
//! lines is, and not every line is also another's; another session's when
//! none of its lines is this session's and some are that session's; nobody's
//! when no session wrote any of it. Anything else is not known, and
//! `commit_mine` refuses it rather than guess. Files Git cannot diff by line
//! (binary, over 512 KiB, a mode change), or that this session changed in
//! lines that are not known, belong to the session whose journaled write
//! left them as they are now. Another session's write of unknown lines does
//! not make a file whole: a hunk not wholly this session's may be that
//! session's, and one that is wholly this session's stays so only while its
//! writes not yet committed account for every copy of its lines in the
//! file's whole diff from `HEAD`.

pub use super::attribution::Owner;
use super::attribution::{Reading, file_owners, lines_of};
pub(crate) use super::attribution::{is_live, is_own};
pub use super::commit_diff::{FileDiff, Hunk, parse_diff};
pub use super::commit_mine::{Left, MineReport, Refusal, commit_mine, running_session};
use super::commit_mine::{git_bounded, git_in};
pub use super::status_mine::{MINE_LISTED, MineFile, MineHunk, MineStatus, status_mine};
#[cfg(test)]
use super::{attribution::*, commit_diff::*, commit_mine::*, status_mine::*};
use crate::{
    hosts::port::{Config, Reply, ShellCall},
    journal::{self, Entry},
    shell::GitStep,
};
#[cfg(test)]
use langspan_core::FileState;
use langspan_core::{Command, Outcome};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Diff output read for one decision, at most.
pub(super) const MAX_DIFF_BYTES: usize = 16 * 1024 * 1024;
/// Untracked files read for one decision, at most.
const MAX_UNTRACKED: usize = 1024;
/// Files and sessions named in one held command's reason, at most.
const NAMED: usize = 8;
/// The held commit's opening words, which the LangSpan skill quotes.
pub const HELD: &str = "LangSpan held this commit:";
/// The longest `git commit` this waits for (its hooks run inside it).
pub(super) const COMMIT_TIMEOUT: Duration = Duration::from_secs(600);

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

pub(super) fn diff(
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
pub(super) fn untracked(
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
                piece: None,
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
pub(crate) fn live_sessions(
    config: &Config,
    deadline: Instant,
) -> Result<BTreeSet<String>, String> {
    let request = crate::protocol::Request::new(
        "read",
        Command::Recap {
            workspace_id: config.workspace_id.clone(),
            tag: None,
            since: None,
        },
    );
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
    // Decisions get two thirds of the time left; the shell snapshot that
    // follows needs the rest.
    let now = Instant::now();
    let until = now + deadline.saturating_duration_since(now) * 2 / 3;
    let start = match &call.cwd {
        Some(cwd) => PathBuf::from(cwd),
        None => config.root.clone(),
    };
    let mut files = match taken(&config.root, &start, &steps, until) {
        Ok(files) => files,
        Err(error) => return passed(format!("no decision ({error})")),
    };
    // Only the journal entries of the paths the command would take: the
    // guard's cost grows with the changed files, not with the journal.
    let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
    let entries = journal::entries_for(&config.state_dir, &config.root, &paths);
    if !entries.iter().any(|entry| !is_own(me, &entry.session_id)) {
        return passed(
            "no other session's uncommitted lines are journaled in what it would take".into(),
        );
    }
    let live = match live_sessions(config, until) {
        Ok(live) => live,
        Err(error) => return passed(format!("no decision ({error})")),
    };
    let live_others: Vec<&Entry> = entries
        .iter()
        .filter(|entry| !is_own(me, &entry.session_id) && is_live(&live, &entry.session_id))
        .collect();
    if live_others.is_empty() {
        return passed(
            "no live session's uncommitted lines are journaled in what it would take".into(),
        );
    }
    let claims = journal::by_file(&entries);
    // Session -> path -> where.
    let mut found: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    let mut evidence = Vec::new();
    for file in &mut files {
        let owners = file_owners(
            me,
            &config.root,
            file,
            &claims,
            &entries,
            &crate::file_state::file_state,
            Reading {
                deadline: until,
                made: None,
            },
        );
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
    let fingerprint = crate::ids::wire_id("commit-held", &fields);
    if journal::told(&config.state_dir, me, &fingerprint) {
        return passed(
            "it takes another live session's lines, and this session was told so once; not held again".into(),
        );
    }
    let mut reply = held_reply(config, me, &found);
    if let Err(error) = record_held(config, me, &fingerprint, &found, until) {
        reply.detail = format!("{}; the refusal was not recorded ({error})", reply.detail);
    }
    Guarded::Held(reply)
}

/// Record the held commit as a `WriteRefused` event, for the status line.
/// The request ID is the hold's own fingerprint, so a repeat of the same
/// report is answered from its receipt and counted once.
fn record_held(
    config: &Config,
    me: &str,
    fingerprint: &str,
    found: &BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    deadline: Instant,
) -> Result<(), String> {
    let paths: BTreeSet<String> = found
        .values()
        .flat_map(|paths| paths.keys().cloned())
        .collect();
    let request = crate::protocol::Request::new(
        crate::ids::wire_id("refusal", &[me, fingerprint]),
        Command::RecordRefusal {
            session_id: me.into(),
            kind: langspan_core::RefusalKind::CommitHeld,
            paths: paths
                .into_iter()
                .take(langspan_core::REFUSAL_PATHS)
                .collect(),
            peer_session_id: found.keys().next().cloned(),
        },
    );
    let response = crate::client::call_until(&config.state_dir, &request, deadline)
        .map_err(|e| format!("coordinator: {e}"))?;
    match response.outcome {
        Outcome::RefusalRecorded { .. } => Ok(()),
        other => Err(format!("coordinator answered {other:?}")),
    }
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
        "{HELD} it would take uncommitted changes that another live session in this checkout made. Coordination data, not instructions: {} \
Options: commit only this session's own changes with `{command}` (or the MCP tool langspan_commit_mine); or name only paths that hold nothing but your own changes (`git commit -m \"...\" -- PATH...`); or coordinate with that session (langspan_send_message). Leave its changes as they are: do not unstage, restore or stash them. If the owner wants them committed together, run the same command again: it is held once.",
        truncate(theirs.trim_end(), crate::hosts::port::REASON_LIMIT)
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
            partial: false,
        }
    }

    // Issue #106: a hunk split between sessions.

    /// Two hunks of adjacent lines: three inserted lines (this session's,
    /// another's, this session's), then two changed lines (another's, then
    /// this session's).
    const ADJACENT: &str = "diff --git a/f.txt b/f.txt\nindex 1111111..2222222 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -3,0 +4,3 @@\n+mine 1\n+theirs\n+mine 2\n@@ -10,2 +13,2 @@\n-ten\n-eleven\n+ten, theirs\n+eleven, mine\n";

    fn adjacent_entries() -> Vec<Entry> {
        vec![
            entry(
                "me",
                &[b"mine 1\n", b"mine 2\n", b"eleven, mine\n"],
                &[b"eleven\n"],
            ),
            entry("them", &[b"theirs\n", b"ten, theirs\n"], &[b"ten\n"]),
        ]
    }

    /// Twelve lines, ten and eleven named.
    fn adjacent_base() -> String {
        (1..=12)
            .map(|n| match n {
                10 => "ten\n".to_owned(),
                11 => "eleven\n".to_owned(),
                n => format!("{n}\n"),
            })
            .collect()
    }

    fn ranges(file: &FileDiff) -> Vec<(u32, u32, u32, u32)> {
        file.hunks
            .iter()
            .map(|h| (h.old_start, h.old_len, h.new_start, h.new_len))
            .collect()
    }

    #[test]
    fn a_hunk_is_split_where_this_sessions_lines_meet_anothers() {
        let mut file = parse_diff(ADJACENT.as_bytes()).unwrap().remove(0);
        let entries = adjacent_entries();
        let claims = journal::by_file(&entries);
        let why = split_between_sessions("me", &mut file, claims.get("f.txt"));
        assert_eq!(why, vec![None; 5]);
        assert_eq!(
            ranges(&file),
            [
                (3, 0, 4, 1),
                (3, 0, 5, 1),
                (3, 0, 6, 1),
                (10, 1, 13, 1),
                (11, 1, 14, 1)
            ]
        );
        let owners: Vec<Owner> = file
            .hunks
            .iter()
            .map(|h| hunk_owner("me", h, claims.get("f.txt")))
            .collect();
        let them = Owner::Others(BTreeSet::from(["them".to_owned()]));
        assert_eq!(
            owners,
            [Owner::Own, them.clone(), Owner::Own, them, Owner::Own]
        );

        // This session's three pieces: the two insertions on either side of
        // another's line are one hunk in the patch.
        let text = String::from_utf8(patch(&file, &[0, 2, 4])).unwrap();
        assert!(
            text.ends_with(
                "@@ -3,0 +4,2 @@\n+mine 1\n+mine 2\n@@ -11,1 +13,1 @@\n-eleven\n+eleven, mine\n"
            ),
            "{text}"
        );
        let base = adjacent_base();
        let mine = apply_exactly(base.as_bytes(), &file, &[0, 2, 4]).unwrap();
        let want = base
            .replace("3\n", "3\nmine 1\nmine 2\n")
            .replace("eleven\n", "eleven, mine\n");
        assert_eq!(String::from_utf8(mine).unwrap(), want);
        // Every piece, applied, is the whole diff.
        let all = apply_exactly(base.as_bytes(), &file, &[0, 1, 2, 3, 4]).unwrap();
        let want = base
            .replace("3\n", "3\nmine 1\ntheirs\nmine 2\n")
            .replace("ten\neleven\n", "ten, theirs\neleven, mine\n");
        assert_eq!(String::from_utf8(all).unwrap(), want);
    }

    #[test]
    fn a_patch_that_does_not_apply_exactly_where_it_says_is_refused() {
        let file = parse_diff(ADJACENT.as_bytes()).unwrap().remove(0);
        let moved = format!("0\n{}", adjacent_base());
        let why = apply_exactly(moved.as_bytes(), &file, &[1]).unwrap_err();
        assert!(why.contains("line 10"), "{why}");
        let short = apply_exactly(b"1\n2\n", &file, &[1]).unwrap_err();
        assert!(short.contains("past the end"), "{short}");
        // A line without its newline is compared without it.
        let files = parse_diff(DIFF_TEXT.as_bytes()).unwrap();
        let base: String = (1..=19).map(|n| format!("{n}\n")).collect::<String>() + "end\n";
        let ended = apply_exactly(base.as_bytes(), &files[0], &[2]).unwrap();
        assert!(ended.ends_with(b"19\nend"), "{ended:?}");
    }

    #[test]
    fn a_hunk_is_not_split_where_one_sessions_line_replaced_anothers_or_no_session_wrote_one() {
        use Side::{Mine as M, Theirs as T};
        // Another session's removed line and this session's added one: the
        // added line could go before or after it.
        assert_eq!(pieces(&[T], &[M]), Err(ORDER));
        assert_eq!(pieces(&[M, T], &[T, M]), Err(ORDER));
        assert_eq!(pieces(&[M, T], &[M, T]), Ok(vec![(M, 1, 1), (T, 1, 1)]));
        assert_eq!(pieces(&[T, M], &[M]), Ok(vec![(T, 1, 0), (M, 1, 1)]));
        assert_eq!(pieces(&[M, T], &[M]), Ok(vec![(M, 1, 1), (T, 1, 0)]));
        assert_eq!(
            pieces(&[], &[T, M, T]),
            Ok(vec![(T, 0, 1), (M, 0, 1), (T, 0, 1)])
        );
        assert_eq!(
            pieces(&[M, T, M], &[]),
            Ok(vec![(M, 1, 0), (T, 1, 0), (M, 1, 0)])
        );
        // Each session's changed line next to the other's, three in a row.
        assert_eq!(
            pieces(&[M, T, M], &[M, T, M]),
            Ok(vec![(M, 1, 1), (T, 1, 1), (M, 1, 1)])
        );
        assert_eq!(
            pieces(&[T, M, T], &[M]),
            Ok(vec![(T, 1, 0), (M, 1, 1), (T, 1, 0)])
        );

        // The review's case: "A B C" became "B X" by this session, then
        // another removed B. X could have taken A's place or C's, so it
        // could go before B or after it.
        assert_eq!(pieces(&[M, T, M], &[M]), Err(ORDER));
        // The same with the sessions the other way round.
        assert_eq!(pieces(&[T, M, T], &[T]), Err(ORDER));
        // One removed run and two added runs of a side, with the other's
        // between: which added run took the removed run's place is not known.
        assert_eq!(pieces(&[M], &[M, T, M]), Err(ORDER));
        assert_eq!(pieces(&[T], &[T, M, T]), Err(ORDER));
        // Where the other session's added run fixes the order, it is split.
        assert_eq!(
            pieces(&[M, T, M], &[M, T]),
            Ok(vec![(M, 1, 1), (T, 1, 1), (M, 1, 0)])
        );

        // Another session removed a line this session added: the journal
        // says a line of one replaced the other's, wherever it was.
        let mut entries = adjacent_entries();
        entries.push(entry("them", &[], &[b"gone, mine\n"]));
        entries.push(entry("me", &[b"gone, mine\n"], &[]));
        let claims = journal::by_file(&entries);
        let mut file = parse_diff(ADJACENT.as_bytes()).unwrap().remove(0);
        let why = split_between_sessions("me", &mut file, claims.get("f.txt"));
        assert_eq!(why, vec![Some(REPLACED); 2]);
        assert_eq!(file.hunks.len(), 2);

        // A line no session wrote: the hunk stays whole, and not known.
        let entries = vec![entry("me", &[b"mine 1\n", b"mine 2\n"], &[])];
        let claims = journal::by_file(&entries);
        let mut file = parse_diff(ADJACENT.as_bytes()).unwrap().remove(0);
        let why = split_between_sessions("me", &mut file, claims.get("f.txt"));
        assert_eq!(why, vec![None; 2]);
        assert!(matches!(
            hunk_owner("me", &file.hunks[0], claims.get("f.txt")),
            Owner::Unknown(_)
        ));
    }

    /// A Git repository of its own in a temporary directory, removed on drop.
    struct Repo(PathBuf);

    impl Repo {
        fn new(content: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ls-commit-unit-{}", private_name()));
            fs::create_dir(&dir).unwrap();
            let repo = Self(fs::canonicalize(&dir).unwrap());
            fs::write(repo.0.join("f.txt"), content).unwrap();
            repo.git(&["init", "-q"]);
            repo.git(&["add", "f.txt"]);
            repo.git(&[
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@example.invalid",
                "commit",
                "-q",
                "-m",
                "start",
            ]);
            repo
        }

        fn git(&self, args: &[&str]) -> Vec<u8> {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
            output.stdout
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_partial_patch_is_checked_against_the_index_and_nothing_is_staged_when_it_does_not_apply() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let base = adjacent_base();
        let repo = Repo::new(&base);
        let git_dir = repo.0.join(".git");
        let mut file = parse_diff(ADJACENT.as_bytes()).unwrap().remove(0);
        let entries = adjacent_entries();
        let claims = journal::by_file(&entries);
        split_between_sessions("me", &mut file, claims.get("f.txt"));
        let staged = vec![(file, Choice::Hunks(vec![0, 2, 4]))];

        let index = git_dir.join("private.index");
        git_in(&repo.0, &["read-tree", "HEAD"], Some(&index), deadline).unwrap();
        stage(&repo.0, &git_dir, &staged, Some(&index), deadline).unwrap();
        let want = base
            .replace("3\n", "3\nmine 1\nmine 2\n")
            .replace("eleven\n", "eleven, mine\n");
        assert_eq!(
            index_blob(&repo.0, "f.txt", Some(&index), deadline).unwrap(),
            want.as_bytes()
        );

        // The index holds another version than the diff was made from.
        let index = git_dir.join("moved.index");
        git_in(&repo.0, &["read-tree", "HEAD"], Some(&index), deadline).unwrap();
        stage(&repo.0, &git_dir, &staged, Some(&index), deadline).unwrap();
        let before = fs::read(&index).unwrap();
        let error = stage(&repo.0, &git_dir, &staged, Some(&index), deadline).unwrap_err();
        assert!(error.contains("does not apply exactly"), "{error}");
        assert!(error.contains("f.txt"), "{error}");
        assert_eq!(fs::read(&index).unwrap(), before);
        assert_eq!(repo.git(&["diff", "--cached", "--stat"]), b"");
    }

    /// The review's case: "A B C" in `HEAD`, "X" now. This session removed
    /// A and C and added X; another session removed B.
    const ABC: &str = "diff --git a/f.txt b/f.txt\nindex 1111111..2222222 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,1 @@\n-A\n-B\n-C\n+X\n";

    #[test]
    fn a_patch_that_moves_this_sessions_line_across_anothers_is_never_staged() {
        use Side::{Mine as M, Theirs as T};
        let deadline = Instant::now() + Duration::from_secs(30);
        let repo = Repo::new("A\nB\nC\n");
        let git_dir = repo.0.join(".git");
        let index = git_dir.join("private.index");
        git_in(&repo.0, &["read-tree", "HEAD"], Some(&index), deadline).unwrap();
        let before = fs::read(&index).unwrap();
        let whole = parse_diff(ABC.as_bytes()).unwrap().remove(0);

        // Split as if X took A's place: this session's pieces would stage
        // "X B", X moved above the other session's uncommitted B. And as if
        // it took C's place: "B X". Neither is the one order, so neither is
        // staged, and the split itself refuses it.
        let entries = vec![
            entry("me", &[b"X\n"], &[b"A\n", b"C\n"]),
            entry("them", &[], &[b"B\n"]),
        ];
        let claims = journal::by_file(&entries);
        let mut split = whole.clone();
        let why = split_between_sessions("me", &mut split, claims.get("f.txt"));
        assert_eq!(why, vec![Some(ORDER)]);
        for layout in [
            [(M, 1, 1), (T, 1, 0), (M, 1, 0)],
            [(M, 1, 0), (T, 1, 0), (M, 1, 1)],
        ] {
            let mut file = whole.clone();
            file.hunks = cut(&whole.hunks[0], 0, &layout);
            // The patch would apply exactly where it says: only the order
            // check stops it.
            assert!(apply_exactly(b"A\nB\nC\n", &file, &[0, 2]).is_ok());
            let staged = vec![(file, Choice::Hunks(vec![0, 2]))];
            let error = stage(&repo.0, &git_dir, &staged, Some(&index), deadline).unwrap_err();
            assert!(
                error.contains("f.txt does not keep this session's lines in place"),
                "{error}"
            );
            assert!(error.contains("more than one order"), "{error}");
            assert_eq!(fs::read(&index).unwrap(), before);
        }

        // One piece of this session's two, or a piece of each session's,
        // is not this session's change in place either.
        let mut file = parse_diff(ADJACENT.as_bytes()).unwrap().remove(0);
        let entries = adjacent_entries();
        let claims = journal::by_file(&entries);
        split_between_sessions("me", &mut file, claims.get("f.txt"));
        for chosen in [vec![0], vec![0, 1, 2, 4], vec![2, 4]] {
            let error = in_one_order(&file, &chosen).unwrap_err();
            assert!(error.contains("some of one session's pieces"), "{error}");
        }
        assert_eq!(in_one_order(&file, &[0, 2, 4]), Ok(()));
        assert_eq!(in_one_order(&file, &[1, 3]), Ok(()));
        // A split not in the one order is refused, though it is
        // otherwise this session's whole side.
        let mut moved = file.clone();
        moved.hunks.truncate(3);
        let wrong = cut(
            &parse_diff(ADJACENT.as_bytes()).unwrap().remove(0).hunks[1],
            1,
            &[(T, 1, 0), (M, 1, 0), (T, 0, 1), (M, 0, 1)],
        );
        moved.hunks.extend(wrong);
        let error = in_one_order(&moved, &[0, 2, 4, 6]).unwrap_err();
        assert!(error.contains("not split in the one order"), "{error}");
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

    #[test]
    fn status_mine_lists_a_file_by_what_commit_mine_would_do_with_it() {
        let files = parse_diff(DIFF_TEXT.as_bytes()).unwrap();
        let them = || Owner::Others(BTreeSet::from(["them".to_owned()]));
        let owners = |owners: Vec<Owner>, whole: bool| FileOwners {
            foreign: vec![BTreeSet::new(); owners.len()],
            owners,
            whole,
        };
        let (listed, theirs) = mine_file(
            &files[0],
            &owners(vec![Owner::Own, them(), Owner::Nobody], false),
            false,
        );
        let listed = listed.unwrap();
        assert!(theirs);
        assert_eq!(listed.owner, "mixed");
        let owned: Vec<_> = listed
            .hunks
            .iter()
            .map(|h| (h.start_line, h.line_count, h.owner))
            .collect();
        assert_eq!(owned, [(2, 1, "mine"), (19, 2, "others"), (21, 1, "none")]);
        assert_eq!(listed.hunks[1].sessions, ["them"]);

        // Nothing of it this session's: not listed, but counted as others'.
        let (listed, theirs) = mine_file(
            &files[0],
            &owners(vec![them(), Owner::Nobody, Owner::Nobody], false),
            false,
        );
        assert!(listed.is_none() && theirs);
        let (listed, theirs) = mine_file(&files[0], &owners(vec![Owner::Nobody; 3], false), false);
        assert!(listed.is_none() && !theirs);

        // A new binary file attributed whole, and one partly its own that
        // can be committed only whole: commit --mine would refuse it.
        let (listed, _) = mine_file(&files[1], &owners(vec![Owner::Own], true), true);
        let listed = listed.unwrap();
        assert_eq!(
            (listed.owner, listed.whole, listed.staged),
            ("mine", true, true)
        );
        assert!(listed.hunks.is_empty());
        let mut created = files[0].clone();
        created.created = true;
        let (listed, _) = mine_file(
            &created,
            &owners(vec![Owner::Own, them(), Owner::Own], false),
            false,
        );
        let listed = listed.unwrap();
        assert_eq!(listed.owner, "unknown");
        assert_eq!(listed.reason.as_deref(), Some(ONLY_WHOLE));
    }

    #[test]
    fn status_mine_text_says_whose_each_hunk_is_and_when_the_list_is_cut() {
        let hunk = |start_line, line_count, owner: &'static str, sessions: &[&str]| MineHunk {
            start_line,
            line_count,
            owner,
            sessions: sessions.iter().map(|s| (*s).to_owned()).collect(),
            reason: None,
        };
        let status = MineStatus {
            session_id: "me".into(),
            files: vec![MineFile {
                path: "src/api.rs".into(),
                owner: "mixed",
                staged: false,
                whole: false,
                hunks: vec![hunk(10, 5, "mine", &[]), hunk(40, 2, "others", &["them"])],
                reason: None,
            }],
            others: 3,
            truncated: true,
        };
        let mut cut = MineStatus {
            files: Vec::new(),
            others: 0,
            truncated: false,
            ..status.clone()
        };
        for _ in 0..=MINE_LISTED {
            cut.add(Some(status.files[0].clone()), true);
        }
        cut.add(None, false);
        assert_eq!(
            (cut.files.len(), cut.truncated, cut.others),
            (MINE_LISTED, true, MINE_LISTED + 1)
        );

        let text = status.text();
        assert!(text.contains("src/api.rs  mixed"), "{text}");
        assert!(
            text.contains("lines 10-14 mine, lines 40-41 them"),
            "{text}"
        );
        assert!(
            text.contains(&format!("first {MINE_LISTED} files")),
            "{text}"
        );
        assert!(text.contains("3 changed file(s)"), "{text}");
    }

    /// The guard reads the journal entries of the paths the command would
    /// take, not the whole workspace's (issue #25): its cost grows with the
    /// changed files, not with the journal.
    #[test]
    fn the_guard_reads_only_the_journal_entries_of_the_paths_it_would_take() {
        let base = std::env::temp_dir().join(format!("ls-guard-reads-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("work")).unwrap();
        let root = fs::canonicalize(base.join("work")).unwrap();
        let state = base.join("state");
        let git = |arguments: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(arguments)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(output.status.success(), "git {arguments:?}: {output:?}");
        };
        let files: Vec<String> = (0..42).map(|n| format!("f{n:02}.txt")).collect();
        for file in &files {
            fs::write(root.join(file), "one\ntwo\n").unwrap();
        }
        git(&["init", "-q"]);
        git(&["config", "user.name", "Owner"]);
        git(&["config", "user.email", "owner@example.invalid"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "start"]);
        // Another session's writes of 40 files, ten each, and this
        // session's one write each of the first two.
        let write = |session: &str, operation: &str, path: &str, content: &str| {
            let paths = vec![path.to_owned()];
            journal::before_edit(&state, operation, &root, &paths);
            fs::write(root.join(path), content).unwrap();
            journal::record(
                &state,
                &root,
                session,
                &paths,
                journal::Origin::Edit {
                    operation_id: operation,
                },
            )
            .unwrap();
        };
        for (n, file) in files.iter().enumerate().skip(2) {
            for round in 0..10 {
                let content = format!("one\ntwo, round {round}\n");
                write(
                    "claude-code.other",
                    &format!("o-{n}-{round}"),
                    file,
                    &content,
                );
            }
        }
        write("claude-code.me", "m-0", &files[0], "one, mine\ntwo\n");
        write("claude-code.me", "m-1", &files[1], "one, mine\ntwo\n");
        assert_eq!(journal::entries(&state, &root).len(), 402);

        let config = Config {
            state_dir: state.clone(),
            workspace_id: "work".into(),
            root: root.clone(),
            ttl_ms: 30_000,
            budget: Duration::from_millis(1500),
            share_prompt: false,
            code_briefing: false,
            presence: false,
            presence_window_ms: langspan_core::PRESENCE_WINDOW_MS,
            outline_reads: false,
            vendor: crate::vendor::Vendor::ClaudeCode,
            user_level: false,
        };
        let call = |command: &str| ShellCall {
            command: command.into(),
            cwd: Some(root.to_string_lossy().into_owned()),
        };
        let deadline = Instant::now() + Duration::from_secs(4);
        journal::entry_reads();
        let detail = match guard_hook(
            &config,
            "claude-code.me",
            &call("git commit -m x -- f00.txt f01.txt"),
            deadline,
        ) {
            Guarded::Passed(detail) => detail,
            _ => panic!("not passed"),
        };
        // Only this session's entries are on the two paths: no coordinator
        // call (none runs here), and only those two entries are read.
        assert!(
            detail.contains("no other session's uncommitted lines are journaled"),
            "{detail}"
        );
        assert_eq!(journal::entry_reads(), 2);

        // Another session's entries on a path it would take are read, and
        // only that path's: here the coordinator is down, so no decision.
        let detail = match guard_hook(
            &config,
            "claude-code.me",
            &call("git commit -m x -- f00.txt f02.txt"),
            deadline,
        ) {
            Guarded::Passed(detail) => detail,
            _ => panic!("not passed"),
        };
        assert!(detail.contains("no decision (coordinator"), "{detail}");
        assert_eq!(journal::entry_reads(), 11);
        let _ = fs::remove_dir_all(&base);
    }
}
