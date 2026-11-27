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
/// The held commit's opening words, which the LangSpan skill quotes.
pub const HELD: &str = "LangSpan held this commit:";
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
    /// For a piece of a hunk split between sessions: which hunk of the
    /// file's diff it came from, and whose it is.
    piece: Option<Piece>,
}

/// Where a piece of a split hunk came from (issue #106).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    /// The index, in the file's diff before the split, of its hunk.
    of: usize,
    side: Side,
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
                piece: None,
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

/// A hunk's body, one entry per line it removes or adds, each as Git printed
/// it: its sign, its text, and its "\ No newline" marker if it has one.
fn body_lines(body: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    for piece in body.split_inclusive(|byte| *byte == b'\n') {
        let end = at + piece.len();
        match lines.last_mut() {
            Some(last) if piece.first() == Some(&b'\\') => last.1 = end,
            _ => lines.push((at, end)),
        }
        at = end;
    }
    lines
        .into_iter()
        .map(|(start, end)| &body[start..end])
        .collect()
}

/// The text of one body line, as it is in the file: without its sign, and
/// without its line end when a "\ No newline" marker follows.
fn line_text(line: &[u8]) -> &[u8] {
    let text = line.get(1..).unwrap_or_default();
    match text.iter().position(|byte| *byte == b'\n') {
        Some(end) if end + 1 < text.len() => &text[..end],
        _ => text,
    }
}

/// The first line a hunk removes, or the line its insertion goes before.
fn old_first(hunk: &Hunk) -> u32 {
    hunk.old_start + u32::from(hunk.old_len == 0)
}

/// `chosen` hunks of `file`, with each run that touches in the old file
/// (as a hunk split between sessions does, issue #106) made one hunk, so
/// that no two hunks of a patch start at the same place.
fn joined(file: &FileDiff, chosen: &[usize]) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    for &index in chosen {
        let hunk = &file.hunks[index];
        match out.last_mut() {
            Some(last) if old_first(last) + last.old_len == old_first(hunk) => {
                let mut lines = body_lines(&last.body);
                let added = lines.split_off(last.removed.len());
                let mut more = body_lines(&hunk.body);
                let more_added = more.split_off(hunk.removed.len());
                let body: Vec<u8> = lines
                    .into_iter()
                    .chain(more)
                    .chain(added)
                    .chain(more_added)
                    .flatten()
                    .copied()
                    .collect();
                let first = old_first(last);
                last.removed.extend(hunk.removed.iter().cloned());
                last.added.extend(hunk.added.iter().cloned());
                last.old_len += hunk.old_len;
                last.new_len += hunk.new_len;
                last.old_start = first - u32::from(last.old_len == 0);
                last.body = body;
            }
            _ => out.push(hunk.clone()),
        }
    }
    out
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
    for hunk in &joined(file, chosen) {
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

/// What `base` becomes with `chosen` hunks of `file` applied exactly where
/// they say: each hunk's removed lines must be `base`'s lines at its stated
/// place, byte for byte, with no search for them elsewhere. If not, why.
fn apply_exactly(base: &[u8], file: &FileDiff, chosen: &[usize]) -> Result<Vec<u8>, String> {
    let lines: Vec<&[u8]> = base.split_inclusive(|byte| *byte == b'\n').collect();
    let mut out = Vec::with_capacity(base.len());
    let mut next = 0;
    for hunk in joined(file, chosen) {
        let first = old_first(&hunk) as usize - 1;
        let end = first + hunk.old_len as usize;
        if first < next || end > lines.len() {
            return Err(format!(
                "its hunk at line {} is past the end of the version there",
                first + 1
            ));
        }
        out.extend(lines[next..first].iter().copied().flatten());
        let body = body_lines(&hunk.body);
        let (removed, added) = body.split_at(hunk.removed.len().min(body.len()));
        for (n, (line, want)) in lines[first..end].iter().zip(removed).enumerate() {
            if *line != line_text(want) {
                return Err(format!(
                    "line {} there is not the line the patch removes",
                    first + n + 1
                ));
            }
        }
        out.extend(added.iter().flat_map(|line| line_text(line)));
        next = end;
    }
    out.extend(lines[next..].iter().copied().flatten());
    Ok(out)
}

/// For each hunk split between sessions that `chosen` takes pieces of: the
/// pieces it takes must be every piece of one side and none of the other's,
/// and the hunk's pieces must be the one likeliest order of its sides'
/// lines (`pieces`), worked out again from the pieces themselves. With
/// `apply_exactly`, the version staged is then the index's with exactly
/// this session's changes in their one place: never one where its lines
/// were moved across another session's uncommitted lines.
fn in_one_order(file: &FileDiff, chosen: &[usize]) -> Result<(), String> {
    let split: BTreeSet<usize> = chosen
        .iter()
        .filter_map(|&index| Some(file.hunks.get(index)?.piece?.of))
        .collect();
    for of in split {
        let members: Vec<(usize, &Hunk, Side)> = file
            .hunks
            .iter()
            .enumerate()
            .filter_map(|(index, hunk)| match hunk.piece {
                Some(piece) if piece.of == of => Some((index, hunk, piece.side)),
                _ => None,
            })
            .collect();
        let line = members.first().map_or(0, |(_, hunk, _)| old_first(hunk));
        let taken: BTreeSet<Side> = members
            .iter()
            .filter(|(index, _, _)| chosen.contains(index))
            .map(|(_, _, side)| *side)
            .collect();
        let whole_side = taken.len() == 1
            && members
                .iter()
                .all(|(index, _, side)| chosen.contains(index) == taken.contains(side));
        if !whole_side {
            return Err(format!(
                "of the hunk at line {line}, it takes some of one session's pieces or some of each session's"
            ));
        }
        let lines = |count: fn(&Hunk) -> usize| -> Vec<Side> {
            members
                .iter()
                .flat_map(|(_, hunk, side)| std::iter::repeat_n(*side, count(hunk)))
                .collect()
        };
        let removed = lines(|hunk| hunk.removed.len());
        let added = lines(|hunk| hunk.added.len());
        let layout: Vec<(Side, usize, usize)> = members
            .iter()
            .map(|(_, hunk, side)| (*side, hunk.removed.len(), hunk.added.len()))
            .collect();
        match pieces(&removed, &added) {
            Ok(order) if order == layout => {}
            Ok(_) => {
                return Err(format!(
                    "the hunk at line {line} is not split in the one order its sessions' lines can be in"
                ));
            }
            Err(why) => return Err(format!("at line {line}, {why}")),
        }
    }
    Ok(())
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
    // A piece split off as other sessions' takes the lines this session
    // wrote too with it (`split_hunk`).
    let theirs_piece = hunk.piece.is_some_and(|piece| piece.side == Side::Theirs);
    let (mut total, mut own, mut shared) = (0, 0, 0);
    let mut others = BTreeSet::new();
    for claimants in lines {
        total += 1;
        let theirs: Vec<&&str> = claimants.iter().filter(|s| !is_own(me, s)).collect();
        let mine = claimants.iter().any(|s| is_own(me, s)) && !(theirs_piece && !theirs.is_empty());
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

/// Why a hunk that holds this session's lines and another session's is not
/// split between them.
const REPLACED: &str = "here a line of one session's replaced another session's, so this hunk cannot be split between them";

/// Whose one line of a hunk is, for splitting it between sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    /// This session's (another's too, perhaps).
    Mine,
    /// Only other sessions'.
    Theirs,
}

/// `None` when no session wrote the line.
fn side(me: &str, claimants: Option<&BTreeSet<&str>>) -> Option<Side> {
    let claimants = claimants.filter(|c| !c.is_empty())?;
    Some(if claimants.iter().any(|s| is_own(me, s)) {
        Side::Mine
    } else {
        Side::Theirs
    })
}

/// Runs of one side, by length.
fn runs(sides: &[Side]) -> Vec<(Side, usize)> {
    let mut runs: Vec<(Side, usize)> = Vec::new();
    for side in sides {
        match runs.last_mut() {
            Some((last, count)) if last == side => *count += 1,
            _ => runs.push((*side, 1)),
        }
    }
    runs
}

/// Whether, by the journal, a line one of this session's writes added was
/// removed by another session's, or the reverse. Lines are known by text,
/// not place, so this holds for the whole file.
fn replaced_across(me: &str, path: &journal::FileLines<'_>) -> bool {
    let mine = |set: &BTreeSet<&str>| set.iter().any(|s| is_own(me, s));
    let theirs = |set: &BTreeSet<&str>| set.iter().any(|s| !is_own(me, s));
    path.added.iter().any(|(key, adders)| {
        path.removed.get(key).is_some_and(|removers| {
            (mine(adders) && theirs(removers)) || (theirs(adders) && mine(removers))
        })
    })
}

/// Why a hunk whose sessions' lines can be ordered more than one way is not
/// split between them.
const ORDER: &str = "here this session's lines and another session's could be in more than one order, so this hunk cannot be split between them";

/// The most runs of removed lines times runs of added lines in one hunk
/// that `pieces` orders; past it, the hunk is not split. It bounds the
/// search's table (three entries per pair) to a few megabytes.
const MAX_RUN_PAIRS: usize = 1 << 16;

/// A hunk's pieces, in order: each piece's side and how many lines it
/// removes and adds. A zero-context hunk lists every removed line before
/// every added one, so where an added run goes among the other session's
/// removed lines is not in the diff. Each piece takes one whole run of
/// removed lines, one whole run of added lines of the same side, or both;
/// pieces alternate between the sides. Of every such order, those where the
/// most pieces both remove and add (an added run takes the place of its
/// side's removed run) are the likely ones. Only when exactly one order is
/// likeliest is the hunk split; otherwise `ORDER`. For example, when this
/// session's added run could take the place of either of two of its removed
/// runs, one before another session's removed line and one after it, the
/// pairing has two answers, and which is right decides on which side of the
/// other session's line this session's line goes.
fn pieces(removed: &[Side], added: &[Side]) -> Result<Vec<(Side, usize, usize)>, &'static str> {
    let (r, a) = (runs(removed), runs(added));
    let (rn, an) = (r.len(), a.len());
    if (rn + 1).saturating_mul(an + 1) > MAX_RUN_PAIRS {
        return Err(ORDER);
    }
    // `best[(i, j, last)]`: from runs `r[i..]` and `a[j..]`, after a piece
    // of side `last` (0: none yet, 1: Mine, 2: Theirs), the most pieces
    // that both remove and add, how many orders reach it (2 meaning two or
    // more), and the first piece of one of them. Filled from the end.
    #[derive(Clone, Copy)]
    struct Best {
        pairs: usize,
        orders: u8,
        next: Option<(Side, bool, bool)>,
    }
    let none = Best {
        pairs: 0,
        orders: 0,
        next: None,
    };
    let at = |i: usize, j: usize, last: usize| (i * (an + 1) + j) * 3 + last;
    let code = |side: Side| match side {
        Side::Mine => 1,
        Side::Theirs => 2,
    };
    let mut best = vec![none; (rn + 1) * (an + 1) * 3];
    for i in (0..=rn).rev() {
        for j in (0..=an).rev() {
            for last in 0..3 {
                if i == rn && j == an {
                    best[at(i, j, last)] = Best {
                        pairs: 0,
                        orders: 1,
                        next: None,
                    };
                    continue;
                }
                let mut here = none;
                for side in [Side::Mine, Side::Theirs] {
                    if code(side) == last {
                        continue;
                    }
                    let can_remove = r.get(i).is_some_and(|run| run.0 == side);
                    let can_add = a.get(j).is_some_and(|run| run.0 == side);
                    for (take_r, take_a) in [(true, true), (true, false), (false, true)] {
                        if (take_r && !can_remove) || (take_a && !can_add) {
                            continue;
                        }
                        let then =
                            best[at(i + usize::from(take_r), j + usize::from(take_a), code(side))];
                        if then.orders == 0 {
                            continue;
                        }
                        let pairs = then.pairs + usize::from(take_r && take_a);
                        if here.orders == 0 || pairs > here.pairs {
                            here = Best {
                                pairs,
                                orders: then.orders,
                                next: Some((side, take_r, take_a)),
                            };
                        } else if pairs == here.pairs {
                            here.orders = (here.orders + then.orders).min(2);
                        }
                    }
                }
                best[at(i, j, last)] = here;
            }
        }
    }
    match best[at(0, 0, 0)].orders {
        0 => return Err(REPLACED),
        1 => {}
        _ => return Err(ORDER),
    }
    let (mut i, mut j, mut last) = (0, 0, 0);
    let mut out = Vec::new();
    while let Some((side, take_r, take_a)) = best[at(i, j, last)].next {
        out.push((
            side,
            if take_r { r[i].1 } else { 0 },
            if take_a { a[j].1 } else { 0 },
        ));
        (i, j, last) = (i + usize::from(take_r), j + usize::from(take_a), code(side));
    }
    Ok(out)
}

/// Split each of `file`'s hunks that holds this session's lines and other
/// sessions' lines, and no line no session wrote, at the boundaries between
/// them (issue #106), as `git add -p` lets a person split a hunk. Per hunk
/// after the split: why it was left whole although it holds both, if so.
#[cfg(test)]
fn split_between_sessions(
    me: &str,
    file: &mut FileDiff,
    path: Option<&journal::FileLines<'_>>,
) -> Vec<Option<&'static str>> {
    split_counted(me, file, path, None)
}

/// Per line key, how many more copies this session's journaled writes of
/// `path` added than removed.
fn own_copies(me: &str, path: &str, entries: &[Entry]) -> BTreeMap<String, i64> {
    let mut net: BTreeMap<String, i64> = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|e| e.path == path && is_own(me, &e.session_id))
    {
        for (keys, sign) in entry
            .lines
            .iter()
            .flat_map(|lines| [(&lines.added, 1), (&lines.removed, -1)])
        {
            for key in keys {
                *net.entry(key.clone()).or_default() += sign;
            }
        }
    }
    net
}

/// The copies of each line key a file's diff adds and removes, and how many
/// of them have gone with other sessions' pieces: what tells whether a line
/// both this session and another wrote may go with the other's lines.
struct Shared<'a> {
    own: &'a BTreeMap<String, i64>,
    added: BTreeMap<String, i64>,
    removed: BTreeMap<String, i64>,
}

impl<'a> Shared<'a> {
    fn new(own: &'a BTreeMap<String, i64>, hunks: &[Hunk]) -> Self {
        let mut shared = Self {
            own,
            added: BTreeMap::new(),
            removed: BTreeMap::new(),
        };
        for hunk in hunks {
            for key in &hunk.added {
                *shared.added.entry(key.clone()).or_default() += 1;
            }
            for key in &hunk.removed {
                *shared.removed.entry(key.clone()).or_default() += 1;
            }
        }
        shared
    }

    /// Give `added` and `removed` (keys of lines both wrote) to other
    /// sessions' pieces, if every copy this session's writes account for is
    /// still left for its own: `false`, and nothing given, if not.
    fn give(&mut self, added: &[&String], removed: &[&String]) -> bool {
        let own = |key: &str| self.own.get(key).copied().unwrap_or_default();
        let mut added_left = self.added.clone();
        let mut removed_left = self.removed.clone();
        for key in added {
            let left = added_left.entry((*key).clone()).or_default();
            *left -= 1;
            if *left < own(key) {
                return false;
            }
        }
        for key in removed {
            let left = removed_left.entry((*key).clone()).or_default();
            *left -= 1;
            if *left < -own(key) {
                return false;
            }
        }
        (self.added, self.removed) = (added_left, removed_left);
        true
    }
}

/// Each line's side where its claimants decide it: `None` for a line this
/// session and another both wrote. Such a line takes the side of the lines
/// next to it in its list (another line both wrote is skipped): the one
/// side on both ends, or the only end there is. Lines between the two
/// sides, or with none, stay this session's.
fn sides_around(claimed: &[Option<Side>]) -> Vec<Side> {
    let mut before = Vec::with_capacity(claimed.len());
    let mut last = None;
    for side in claimed {
        before.push(last);
        last = side.or(last);
    }
    let mut out = vec![Side::Mine; claimed.len()];
    let mut next = None;
    for index in (0..claimed.len()).rev() {
        out[index] = match (claimed[index], before[index], next) {
            (Some(side), _, _) => side,
            (None, Some(a), Some(b)) if a == b => a,
            (None, Some(a), None) | (None, None, Some(a)) => a,
            (None, _, _) => Side::Mine,
        };
        next = claimed[index].or(next);
    }
    out
}

/// `split_between_sessions`; with `own`, the copies of each line this
/// session's writes account for, a line both it and another session wrote
/// may go with the other session's lines around it (issue #141).
fn split_counted(
    me: &str,
    file: &mut FileDiff,
    path: Option<&journal::FileLines<'_>>,
    own: Option<&BTreeMap<String, i64>>,
) -> Vec<Option<&'static str>> {
    let (Some(path), false) = (path, splits_only_whole(file)) else {
        return vec![None; file.hunks.len()];
    };
    let mut shared = own.map(|own| Shared::new(own, &file.hunks));
    let mut hunks = Vec::with_capacity(file.hunks.len());
    let mut why = Vec::with_capacity(file.hunks.len());
    for (of, hunk) in std::mem::take(&mut file.hunks).into_iter().enumerate() {
        match split_hunk(me, of, &hunk, path, shared.as_mut()) {
            Ok(Some(split)) => {
                why.extend(split.iter().map(|_| None));
                hunks.extend(split);
            }
            Ok(None) => {
                hunks.push(hunk);
                why.push(None);
            }
            Err(reason) => {
                hunks.push(hunk);
                why.push(Some(reason));
            }
        }
    }
    file.hunks = hunks;
    why
}

/// One hunk's pieces, or `None` when it is not split. `of` is its index in
/// its file's diff.
fn split_hunk<'a>(
    me: &str,
    of: usize,
    hunk: &'a Hunk,
    path: &journal::FileLines<'_>,
    shared: Option<&mut Shared<'_>>,
) -> Result<Option<Vec<Hunk>>, &'static str> {
    let sides = |keys: &[String], claims: &BTreeMap<&str, BTreeSet<&str>>| {
        keys.iter()
            .map(|key| side(me, claims.get(key.as_str())))
            .collect::<Option<Vec<Side>>>()
    };
    // A line no session wrote keeps the hunk whole: it is not known.
    let (Some(mut removed), Some(mut added)) = (
        sides(&hunk.removed, &path.removed),
        sides(&hunk.added, &path.added),
    ) else {
        return Ok(None);
    };
    // Lines both this session and another wrote go with the lines around
    // them, while this session's own copies of them are left elsewhere.
    let mut given = false;
    if let Some(shared) = shared {
        let both = |keys: &[String], claims: &BTreeMap<&str, BTreeSet<&str>>| -> Vec<bool> {
            keys.iter()
                .map(|key| {
                    claims.get(key.as_str()).is_some_and(|c| {
                        c.iter().any(|s| is_own(me, s)) && c.iter().any(|s| !is_own(me, s))
                    })
                })
                .collect()
        };
        let resolve = |sides: &[Side], both: &[bool]| -> Vec<Side> {
            let claimed: Vec<Option<Side>> = sides
                .iter()
                .zip(both)
                .map(|(side, both)| (!both).then_some(*side))
                .collect();
            sides_around(&claimed)
        };
        let (both_removed, both_added) = (
            both(&hunk.removed, &path.removed),
            both(&hunk.added, &path.added),
        );
        let (to_removed, to_added) = (
            resolve(&removed, &both_removed),
            resolve(&added, &both_added),
        );
        // The lines both wrote that go with other sessions' lines.
        let given_of = |keys: &'a [String], both: &[bool], to: &[Side]| -> Vec<&'a String> {
            keys.iter()
                .zip(both.iter().zip(to))
                .filter(|(_, (both, side))| **both && **side == Side::Theirs)
                .map(|(key, _)| key)
                .collect()
        };
        let given_removed = given_of(&hunk.removed, &both_removed, &to_removed);
        let given_added = given_of(&hunk.added, &both_added, &to_added);
        if shared.give(&given_added, &given_removed) {
            given = !given_added.is_empty() || !given_removed.is_empty();
            (removed, added) = (to_removed, to_added);
        }
    }
    let all: Vec<Side> = removed.iter().chain(&added).copied().collect();
    let lines = body_lines(&hunk.body);
    let (minus, plus) = lines.split_at(hunk.removed.len().min(lines.len()));
    let as_listed = lines.len() == all.len()
        && minus.iter().all(|l| l.first() == Some(&b'-'))
        && plus.iter().all(|l| l.first() == Some(&b'+'));
    if !as_listed {
        return Ok(None);
    }
    if !all.contains(&Side::Mine) {
        // Wholly other sessions' once the lines both wrote go with them:
        // one piece, so that those lines are not counted as this session's.
        return Ok(given.then(|| {
            cut(
                hunk,
                of,
                &[(Side::Theirs, hunk.removed.len(), hunk.added.len())],
            )
        }));
    }
    if !all.contains(&Side::Theirs) {
        return Ok(None);
    }
    if replaced_across(me, path) {
        return Err(REPLACED);
    }
    Ok(Some(cut(hunk, of, &pieces(&removed, &added)?)))
}

/// `hunk`, the `of`th of its file's diff, cut into `layout`'s pieces: each
/// a side and how many of the hunk's removed and added lines it takes, in
/// order. The hunk lists its removed lines, then its added ones.
fn cut(hunk: &Hunk, of: usize, layout: &[(Side, usize, usize)]) -> Vec<Hunk> {
    let lines = body_lines(&hunk.body);
    let (minus, plus) = lines.split_at(hunk.removed.len().min(lines.len()));
    let new_first = hunk.new_start + u32::from(hunk.new_len == 0);
    let (mut rs, mut as_) = (0, 0);
    let mut out = Vec::new();
    for &(side, rc, ac) in layout {
        let (old_at, new_at) = (old_first(hunk) + rs as u32, new_first + as_ as u32);
        out.push(Hunk {
            piece: Some(Piece { of, side }),
            old_start: old_at - u32::from(rc == 0),
            old_len: rc as u32,
            new_start: new_at - u32::from(ac == 0),
            new_len: ac as u32,
            removed: hunk.removed[rs..rs + rc].to_vec(),
            added: hunk.added[as_..as_ + ac].to_vec(),
            body: minus[rs..rs + rc]
                .iter()
                .chain(&plus[as_..as_ + ac])
                .flat_map(|line| line.iter().copied())
                .collect(),
        });
        (rs, as_) = (rs + rc, as_ + ac);
    }
    out
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

/// Why a hunk of this session's lines is not committed when another session
/// also changed the file in lines that are not known.
const UNSEEN_COPY: &str = "another session changed this file in lines that are not known, and some of this hunk's lines appear more often than this session wrote them";
const COMMITTED_COPY: &str = "another session changed this file in lines that are not known, and this session's writes of some of this hunk's lines may already be committed";
const UNCOUNTED_COPY: &str = "another session changed this file in lines that are not known, and the file's changes since HEAD, or the commits made since this session wrote it, could not be read";

/// The most moves of `HEAD` read back to find what was committed since this
/// session's writes of a file.
const MAX_MOVES: usize = 256;

/// How one attribution reads Git: by when, and the commit `commit --mine`
/// just made, if any, whose changes are this session's own.
#[derive(Clone, Copy)]
struct Reading<'a> {
    deadline: Instant,
    made: Option<&'a str>,
}

/// What decides whether this session's writes account for every copy of a
/// hunk's lines, when another session changed the file in lines that are not
/// known (issue #99 and its review).
struct Copies {
    /// Per line key: how many more copies this session's journaled writes
    /// added than removed (negative when it removed more).
    net: BTreeMap<String, i64>,
    /// The file's whole diff from `HEAD` to the working tree, whatever part
    /// of it a command would take. `None` when it could not be read.
    full: Option<FileDiff>,
    /// Line keys this session's writes added (or removed) that a move of
    /// `HEAD` since the first of those writes also added (or removed): a
    /// commit through Git may have taken that copy, and the journal still
    /// counts it. `None` when the moves could not be read.
    committed: Option<BTreeSet<String>>,
}

impl Copies {
    fn read(me: &str, root: &Path, file: &FileDiff, entries: &[Entry], reading: Reading) -> Self {
        let mut net: BTreeMap<String, i64> = BTreeMap::new();
        let (mut added, mut removed) = (BTreeSet::new(), BTreeSet::new());
        let mut since: Option<u64> = None;
        for entry in entries
            .iter()
            .filter(|e| e.path == file.path && is_own(me, &e.session_id))
        {
            since = Some(since.map_or(entry.at_ms, |at| at.min(entry.at_ms)));
            for key in entry.lines.iter().flat_map(|lines| &lines.added) {
                *net.entry(key.clone()).or_default() += 1;
                added.insert(key.clone());
            }
            for key in entry.lines.iter().flat_map(|lines| &lines.removed) {
                *net.entry(key.clone()).or_default() -= 1;
                removed.insert(key.clone());
            }
        }
        let full = if file.untracked {
            Some(file.clone())
        } else {
            let literal = format!(":(literal){}", file.path);
            diff(root, &["HEAD"], &[literal], reading.deadline)
                .ok()
                .map(|files| {
                    files
                        .into_iter()
                        .find(|f| f.path == file.path)
                        .unwrap_or_else(|| FileDiff {
                            hunks: Vec::new(),
                            ..file.clone()
                        })
                })
        };
        let committed = match since {
            Some(since) => committed_since(root, &file.path, since, (&added, &removed), reading),
            None => Some(BTreeSet::new()),
        };
        Self {
            net,
            full,
            committed,
        }
    }

    /// Whether this session's uncommitted writes account for every copy, in
    /// the file's whole diff from `HEAD`, of each line `hunk` adds and
    /// removes; if not, why. With another session's write of unknown lines
    /// in the file, a line it may also have written is told apart only by
    /// count.
    fn accounted(&self, file: &FileDiff, hunk: &Hunk) -> Result<(), &'static str> {
        let (Some(full), Some(committed)) = (&self.full, &self.committed) else {
            return Err(UNCOUNTED_COPY);
        };
        if hunk
            .added
            .iter()
            .chain(&hunk.removed)
            .any(|key| committed.contains(key))
        {
            return Err(COMMITTED_COPY);
        }
        // The diff a command takes may be part of the whole (only the staged
        // changes), or, rarely, hold a staged line the working tree has
        // since lost: the larger count of the two.
        let count = |key: &str, side: fn(&Hunk) -> &Vec<String>| {
            [file, full]
                .iter()
                .map(|f| {
                    f.hunks
                        .iter()
                        .flat_map(side)
                        .filter(|k| k.as_str() == key)
                        .count() as i64
                })
                .max()
                .unwrap_or_default()
        };
        let net = |key: &str| self.net.get(key).copied().unwrap_or_default();
        let all = hunk
            .added
            .iter()
            .all(|key| count(key, |h| &h.added) <= net(key))
            && hunk
                .removed
                .iter()
                .all(|key| count(key, |h| &h.removed) <= -net(key));
        if all { Ok(()) } else { Err(UNSEEN_COPY) }
    }
}

/// Of `added` and `removed` (line keys of `path` this session's writes
/// added and removed, the first at `since_ms`), those that a move of `HEAD`
/// at or after that write also added or removed. `HEAD`'s reflog gives the
/// moves; its times are this machine's clock, as the journal's are, and in
/// whole seconds, so a move in the same second as the write counts as after
/// it. `None` when that cannot be told: no reflog, more moves than are read,
/// or a version Git cannot give in time. The move to `reading.made`, the
/// commit `commit --mine` just made of this session's own lines, is not one.
fn committed_since(
    root: &Path,
    path: &str,
    since_ms: u64,
    (added, removed): (&BTreeSet<String>, &BTreeSet<String>),
    reading: Reading,
) -> Option<BTreeSet<String>> {
    let deadline = reading.deadline;
    let limit = format!("-n{}", MAX_MOVES + 1);
    let listed = git_bounded(
        root,
        &[
            "reflog",
            "show",
            "--format=%H %gd",
            "--date=unix",
            &limit,
            "HEAD",
            "--",
        ],
        None,
        deadline,
        1024 * 1024,
    )
    .ok()?;
    let text = String::from_utf8(listed).ok()?;
    // Newest first: each move's commit, and when it was made.
    let mut moves: Vec<(&str, u64)> = Vec::new();
    for line in text.lines() {
        let (commit, selector) = line.split_once(' ')?;
        let seconds = selector
            .strip_prefix("HEAD@{")?
            .strip_suffix('}')?
            .parse::<u64>()
            .ok()?;
        moves.push((commit, seconds));
    }
    // With no reflog, no move since the write can be ruled out.
    if moves.is_empty() {
        return None;
    }
    let mut found = BTreeSet::new();
    for (index, (commit, seconds)) in moves.iter().enumerate() {
        if seconds.saturating_mul(1000).saturating_add(999) < since_ms {
            return Some(found);
        }
        // The move before this one is where it came from; the oldest move
        // read came from nothing only if the whole reflog was read.
        let from = match moves.get(index + 1) {
            Some((from, _)) => Some(*from),
            None if moves.len() > MAX_MOVES => return None,
            None => None,
        };
        if from == Some(*commit) || reading.made == Some(*commit) {
            continue;
        }
        let before = match from {
            Some(from) => journal::head_content(root, path, Some(from), deadline)?,
            None => Vec::new(),
        };
        let after = journal::head_content(root, path, Some(commit), deadline)?;
        let lines = journal::diff_lines(&before, &after)?;
        found.extend(lines.added.into_iter().filter(|key| added.contains(key)));
        found.extend(
            lines
                .removed
                .into_iter()
                .filter(|key| removed.contains(key)),
        );
    }
    Some(found)
}

/// Whose each of `file`'s hunks is, after splitting those that hold this
/// session's lines and another's (so `file.hunks` may change).
fn file_owners(
    me: &str,
    root: &Path,
    file: &mut FileDiff,
    claims: &BTreeMap<&str, journal::FileLines<'_>>,
    entries: &[Entry],
    current: &impl Fn(&Path, &str) -> Option<FileState>,
    reading: Reading,
) -> FileOwners {
    let path = claims.get(file.path.as_str());
    // A file too large to journal by line, or one this session changed in
    // lines that are not known, is attributable only whole. Another
    // session's write of unknown lines is not a reason to: this session's
    // hunks are still told apart by their lines (issue #99).
    let oversize = fs::symlink_metadata(root.join(&file.path))
        .is_ok_and(|m| m.len() > journal::MAX_FILE_BYTES);
    let unknown_own = path.is_some_and(|p| p.unknown.iter().any(|s| is_own(me, s)));
    let whole = file.whole || oversize || unknown_own;
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
    // Other sessions that changed the file in lines that are not known: any
    // hunk not wholly this session's may be theirs.
    let unseen: BTreeSet<String> = path
        .map(|p| p.unknown.iter().map(|s| (*s).to_owned()).collect())
        .unwrap_or_default();
    let own = own_copies(me, &file.path, entries);
    let unsplit = split_counted(me, file, path, Some(&own));
    let file = &*file;
    let mut copies: Option<Copies> = None;
    let mut owners = Vec::new();
    let mut foreign = Vec::new();
    for (hunk, unsplit) in file.hunks.iter().zip(unsplit) {
        let mut owner = match (hunk_owner(me, hunk, path), unsplit) {
            (Owner::Unknown(_), Some(why)) => Owner::Unknown(why),
            (owner, _) => owner,
        };
        let mut theirs = foreign_sessions(me, hunk, path);
        if !unseen.is_empty() {
            owner = match owner {
                // Read once per file, and only for a hunk that needs it.
                Owner::Own => match copies
                    .get_or_insert_with(|| Copies::read(me, root, file, entries, reading))
                    .accounted(file, hunk)
                {
                    Ok(()) => Owner::Own,
                    Err(why) => Owner::Unknown(why),
                },
                Owner::Nobody => Owner::Others(unseen.clone()),
                Owner::Others(mut sessions) => {
                    sessions.extend(unseen.iter().cloned());
                    Owner::Others(sessions)
                }
                unknown => unknown,
            };
            if owner != Owner::Own {
                theirs.extend(unseen.iter().cloned());
            }
        }
        owners.push(owner);
        foreign.push(theirs);
    }
    FileOwners {
        owners,
        foreign,
        whole: false,
    }
}

/// Whether `session`, or the session it is a subagent of, is live.
pub(crate) fn is_live(live: &BTreeSet<String>, session: &str) -> bool {
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

/// Where a hunk is: its first line and how many lines, in the working file,
/// or where it was in `HEAD`'s when it only removes lines.
fn span(hunk: &Hunk) -> (u32, u32) {
    if hunk.new_len == 0 {
        (hunk.old_start, hunk.old_len)
    } else {
        (hunk.new_start, hunk.new_len)
    }
}

/// Lines `start`-`end` of a hunk, for people.
fn lines_of(hunk: &Hunk) -> String {
    let (start, len) = span(hunk);
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
            &crate::hook::file_state,
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
    let fingerprint = crate::hook::wire_id("commit-held", &fields);
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
        crate::hook::wire_id("refusal", &[me, fingerprint]),
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
enum Choice {
    /// Every change: stage the working-tree file.
    Whole,
    /// These hunks of the diff from `HEAD`.
    Hunks(Vec<usize>),
}

/// Why a file that holds this session's changes and others' is refused when
/// it can be committed only whole.
const ONLY_WHOLE: &str = "a new or removed file holds this session's changes and others'; it can be committed only whole";

/// Whether a file can be committed only whole, not hunk by hunk.
fn splits_only_whole(file: &FileDiff) -> bool {
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
            &crate::hook::file_state,
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

/// The version of `path` in the index at `index` (the shared one when
/// `None`).
fn index_blob(
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
fn stage(
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

/// The canonical working tree top and the Git directory of the checkout
/// that holds `root`.
fn checkout_paths(root: &Path, deadline: Instant) -> Result<(PathBuf, PathBuf), crate::Error> {
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

// ---------------------------------------------------------------------------
// status --mine
// ---------------------------------------------------------------------------

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
fn mine_file(file: &FileDiff, owners: &FileOwners, staged: bool) -> (Option<MineFile>, bool) {
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
            &crate::hook::file_state,
            reading,
        );
        let (listed, theirs) = mine_file(file, &owners, staged.contains(&file.path));
        status.add(listed, theirs);
    }
    Ok(status)
}

impl MineStatus {
    /// One file attributed: listed while fewer than `MINE_LISTED` are.
    fn add(&mut self, listed: Option<MineFile>, theirs: bool) {
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
            vendor: crate::install::Vendor::ClaudeCode,
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
