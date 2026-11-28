//! Git diffs of a shared checkout, parsed into hunks, and partial patches built from them.

use super::attribution::{Side, pieces};
use crate::journal;
use std::collections::BTreeSet;

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
    pub(super) body: Vec<u8>,
    /// For a piece of a hunk split between sessions: which hunk of the
    /// file's diff it came from, and whose it is.
    pub(super) piece: Option<Piece>,
}

/// Where a piece of a split hunk came from (issue #106).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Piece {
    /// The index, in the file's diff before the split, of its hunk.
    pub(super) of: usize,
    pub(super) side: Side,
}

/// One file's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The lines from `diff --git` to `+++`, as Git printed them.
    pub(super) header: Vec<String>,
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
pub(super) fn body_lines(body: &[u8]) -> Vec<&[u8]> {
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
pub(super) fn old_first(hunk: &Hunk) -> u32 {
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
pub(super) fn patch(file: &FileDiff, chosen: &[usize]) -> Vec<u8> {
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
pub(super) fn apply_exactly(
    base: &[u8],
    file: &FileDiff,
    chosen: &[usize],
) -> Result<Vec<u8>, String> {
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
pub(super) fn in_one_order(file: &FileDiff, chosen: &[usize]) -> Result<(), String> {
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
