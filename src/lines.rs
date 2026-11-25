//! The lines an edit changes, and which of them other sessions wrote
//! recently according to the line journal, for line ownership ([ADR 0028]).
//!
//! Pure: the caller reads the file once and passes its bytes, the same bytes
//! its admission's base version is taken from. The edit is applied in memory
//! as the host states it (Claude Code's string replacements and `Write`
//! content) and the two versions are compared line by line; a Codex hunk is
//! placed as region admission places it ([ADR 0012]) and read off directly.
//! Every answer is `None`, meaning "may change any line", unless the edit
//! applies exactly as stated.
//!
//! Lines are matched with the journal's by content (ADR 0026): a line is
//! another session's when that session's journaled write added a line with
//! the same bytes.
//!
//! [ADR 0012]: ../docs/adr/0012-region-admission.md
//! [ADR 0028]: ../docs/adr/0028-line-ownership.md

use crate::journal::{Entry, written_lines};
use crate::regions::{Edit, HunkLine, MAX_LOCATED_BYTES, Replacement, file_lines, hunk_places};
use langspan_core::{
    LINE_HORIZON_MS, LineRange, MAX_REGIONS, MAX_WRITTEN, MAX_WRITTEN_RANGES, WrittenLines,
};
use std::collections::BTreeMap;

/// Edit distance traced before a comparison calls the rest one change.
const MAX_DISTANCE: usize = 256;
/// Lines compared times distance traced, at most.
const MAX_WORK: usize = 4_000_000;
/// Bytes of the compared lines times distance traced, at most: a line
/// comparison costs its length.
const MAX_BYTE_WORK: usize = 64 * 1024 * 1024;

/// One run of changed lines in an edit: `removed` lines from line `start`
/// (one-based) of the version before it became `added` lines. With
/// `removed` zero, the lines are inserted before line `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineChange {
    pub start: u32,
    pub removed: u32,
    pub added: u32,
}

/// The lines a change set removes or replaces, in the version before it,
/// joined. An insertion changes no existing line.
pub fn changed_ranges(changes: &[LineChange]) -> Vec<LineRange> {
    joined(
        changes
            .iter()
            .filter(|change| change.removed > 0)
            .map(|change| LineRange {
                start_line: change.start,
                end_line: change.start + change.removed - 1,
            })
            .collect(),
    )
}

/// Sorted, touching ranges joined, and at most `MAX_WRITTEN_RANGES`: past
/// that they become one span, which can only cover more.
fn joined(mut ranges: Vec<LineRange>) -> Vec<LineRange> {
    ranges.sort();
    let mut out: Vec<LineRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start_line <= last.end_line.saturating_add(1) => {
                last.end_line = last.end_line.max(range.end_line);
            }
            _ => out.push(range),
        }
    }
    if out.len() > MAX_WRITTEN_RANGES {
        out = vec![LineRange {
            start_line: out[0].start_line,
            end_line: out[out.len() - 1].end_line,
        }];
    }
    out
}

/// A line too common to name a writer by: fewer than three letters or
/// digits (blank lines, lone braces, `);`).
fn trivial(line: &[u8]) -> bool {
    line.iter()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .count()
        < 3
}

/// The lines of `base` (the file `path` as it is now) that `changed` would
/// remove or replace (`None`: any line) and that another session's journaled
/// write added within `LINE_HORIZON_MS` of `now_ms` (wall clock), newest
/// writer first, read through `journal::written_lines`. A line several
/// sessions added names each of them. A writer whose journaled write of the
/// file has unknown lines is named with no lines when the edit changes any.
pub fn overwritten(
    entries: &[Entry],
    session_id: &str,
    path: &str,
    base: &[u8],
    changed: Option<&[LineRange]>,
    now_ms: u64,
) -> Vec<WrittenLines> {
    let recent: Vec<Entry> = entries
        .iter()
        .filter(|entry| {
            entry.path == path
                && entry.session_id != session_id
                && now_ms.saturating_sub(entry.at_ms) <= LINE_HORIZON_MS
        })
        .cloned()
        .collect();
    let lines: Vec<&[u8]> = base.split_inclusive(|byte| *byte == b'\n').collect();
    let total = lines.len();
    let changes = |line: usize| {
        changed.is_none_or(|ranges| {
            ranges
                .iter()
                .any(|range| range.start_line as usize <= line && line <= range.end_line as usize)
        })
    };
    let mut found: BTreeMap<String, (Vec<LineRange>, u64)> = BTreeMap::new();
    for written in written_lines(&recent, path, base) {
        if !changes(written.line) || trivial(lines[written.line - 1]) {
            continue;
        }
        let number = u32::try_from(written.line).unwrap_or(u32::MAX);
        let slot = found.entry(written.session_id).or_default();
        slot.0.push(LineRange {
            start_line: number,
            end_line: number,
        });
        slot.1 = slot.1.max(written.at_ms);
    }
    let changes_any = (1..=total).any(changes);
    for entry in recent.iter().filter(|entry| entry.lines.is_none()) {
        if changes_any {
            let slot = found.entry(entry.session_id.clone()).or_default();
            slot.1 = slot.1.max(entry.at_ms);
        }
    }
    let mut out: Vec<WrittenLines> = found
        .into_iter()
        .map(|(writer, (ranges, at))| WrittenLines {
            path: path.to_owned(),
            session_id: writer,
            lines: joined(ranges),
            age_ms: now_ms.saturating_sub(at),
        })
        .collect();
    out.sort_by_key(|entry| entry.age_ms);
    out.truncate(MAX_WRITTEN);
    out
}

/// The line changes `edit` makes to `before` (`None`: the file does not
/// exist), in file order, or `None` when they cannot be stated exactly.
pub fn changes(before: Option<&[u8]>, edit: &Edit) -> Option<Vec<LineChange>> {
    let content = match before {
        Some(bytes) if bytes.len() <= MAX_LOCATED_BYTES => std::str::from_utf8(bytes).ok()?,
        Some(_) => return None,
        None if matches!(edit, Edit::Write(_)) => "",
        None => return None,
    };
    let changes = match edit {
        Edit::Replace(replacements) => {
            let after = replaced(content, replacements)?;
            compared(content, &after)?
        }
        Edit::Write(after) if after.len() <= MAX_LOCATED_BYTES => compared(content, after)?,
        Edit::Write(_) => return None,
        Edit::Hunks(hunks) => {
            let mut changes = Vec::new();
            for (hunk, (at, placed)) in hunks.iter().zip(hunk_places(content, hunks)?) {
                // Placed only without trailing empty lines: the host then
                // applies a shorter hunk than the one stated.
                let stated = hunk
                    .lines
                    .iter()
                    .filter(|(kind, _)| *kind != HunkLine::Added)
                    .count();
                if stated != placed {
                    return None;
                }
                // `position` counts the hunk's context and removed lines.
                let mut position = 0usize;
                let mut open: Option<(usize, u32, u32)> = None;
                for (kind, _) in &hunk.lines {
                    match kind {
                        HunkLine::Context => {
                            changes.extend(open.take().map(|run| change(at, run)));
                            position += 1;
                        }
                        HunkLine::Removed => {
                            open.get_or_insert((position, 0, 0)).1 += 1;
                            position += 1;
                        }
                        HunkLine::Added => open.get_or_insert((position, 0, 0)).2 += 1,
                    }
                }
                changes.extend(open.map(|run| change(at, run)));
            }
            let total = u32::try_from(file_lines(content).len()).ok()?;
            // Hunks the host applies in order, inside the file.
            if changes.windows(2).any(|pair| {
                pair[1].start <= pair[0].start || pair[1].start < pair[0].start + pair[0].removed
            }) || changes
                .iter()
                .any(|c| c.start + c.removed > total + 1 || c.start == 0)
            {
                return None;
            }
            changes
        }
    };
    (changes.len() <= MAX_REGIONS).then_some(changes)
}

fn change(at: usize, (position, removed, added): (usize, u32, u32)) -> LineChange {
    LineChange {
        start: u32::try_from(at + position + 1).unwrap_or(u32::MAX),
        removed,
        added,
    }
}

/// The content after the replacements, applied in order as the host applies
/// them: each `old` exactly once, or every occurrence with `replace_all`.
fn replaced(content: &str, replacements: &[Replacement]) -> Option<String> {
    if replacements.is_empty() || replacements.len() > MAX_REGIONS {
        return None;
    }
    let mut current = content.to_owned();
    for replacement in replacements {
        let old = replacement.old.as_str();
        if old.is_empty() {
            return None;
        }
        let count = current.matches(old).count();
        if count == 0 || (count > 1 && !replacement.replace_all) {
            return None;
        }
        // Bounded before anything is allocated: `replace_all` of a short
        // string could otherwise grow a 1 MiB file a thousandfold.
        replaced_size(current.len(), count, old.len(), replacement.new.len())?;
        current = current.replace(old, &replacement.new);
    }
    Some(current)
}

/// The length of a text of `len` bytes after `count` occurrences of `old`
/// bytes are replaced by `new` bytes, if it is within `MAX_LOCATED_BYTES`.
fn replaced_size(len: usize, count: usize, old: usize, new: usize) -> Option<usize> {
    len.checked_sub(count.checked_mul(old)?)?
        .checked_add(count.checked_mul(new)?)
        .filter(|size| *size <= MAX_LOCATED_BYTES)
}

/// The runs of lines that differ between two versions, each line compared
/// with its terminator. The common head and tail are cut first; past
/// `MAX_DISTANCE` edits, or the work bound, the middle is one change.
pub(crate) fn compared(before: &str, after: &str) -> Option<Vec<LineChange>> {
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let whole = |start: usize| -> Option<Vec<LineChange>> {
        Some(vec![LineChange {
            start: u32::try_from(start + 1).ok()?,
            removed: u32::try_from(a.len()).ok()?,
            added: u32::try_from(b.len()).ok()?,
        }])
    };
    if a.is_empty() && b.is_empty() {
        return Some(Vec::new());
    }
    if a.is_empty() || b.is_empty() {
        return whole(head);
    }
    let bytes: usize = a.iter().chain(b).map(|line| line.len()).sum();
    let distance = MAX_DISTANCE
        .min(MAX_WORK / (a.len() + b.len()))
        .min(MAX_BYTE_WORK / bytes.max(1));
    let Some(script) = shortest_edit(a, b, distance) else {
        return whole(head);
    };
    // Group the script's deletions and insertions between equal lines.
    let mut changes = Vec::new();
    let mut line = head;
    let mut open: Option<(usize, u32, u32)> = None;
    for step in script {
        match step {
            Step::Equal => {
                changes.extend(open.take().map(|run| change(0, run)));
                line += 1;
            }
            Step::Delete => {
                open.get_or_insert((line, 0, 0)).1 += 1;
                line += 1;
            }
            Step::Insert => open.get_or_insert((line, 0, 0)).2 += 1,
        }
    }
    changes.extend(open.map(|run| change(0, run)));
    Some(changes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Equal,
    Delete,
    Insert,
}

/// Myers' greedy shortest edit script from `a` to `b`, or `None` past
/// `limit` edits. Keeps each round's furthest points to trace the path back.
fn shortest_edit(a: &[&str], b: &[&str], limit: usize) -> Option<Vec<Step>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let offset = limit as isize + 1;
    let at = |k: isize| (k + offset) as usize;
    let mut furthest = vec![0isize; 2 * limit + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=limit as isize {
        trace.push(furthest.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && furthest[at(k - 1)] < furthest[at(k + 1)]) {
                furthest[at(k + 1)]
            } else {
                furthest[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            furthest[at(k)] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m, at));
            }
            k += 2;
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize, at: impl Fn(isize) -> usize) -> Vec<Step> {
    let (mut x, mut y) = (n, m);
    let mut steps = Vec::new();
    for (d, furthest) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let k = x - y;
        let previous = if k == -d || (k != d && furthest[at(k - 1)] < furthest[at(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = furthest[at(previous)];
        let previous_y = previous_x - previous;
        while x > previous_x && y > previous_y {
            steps.push(Step::Equal);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            steps.push(if x == previous_x {
                Step::Insert
            } else {
                Step::Delete
            });
        }
        x = previous_x;
        y = previous_y;
    }
    steps.reverse();
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::Hunk;

    fn c(start: u32, removed: u32, added: u32) -> LineChange {
        LineChange {
            start,
            removed,
            added,
        }
    }
    fn replace(old: &str, new: &str) -> Edit {
        Edit::Replace(vec![Replacement {
            old: old.into(),
            new: new.into(),
            replace_all: false,
        }])
    }
    const FILE: &str = "one\ntwo\nthree\nfour\nfive\n";
    const FILE_THREE: &str = "line 1\nline 2\nline 3\n";

    #[test]
    fn an_edit_inside_one_line_changes_that_line() {
        assert_eq!(
            changes(Some(FILE.as_bytes()), &replace("hre", "HRE")),
            Some(vec![c(3, 1, 1)])
        );
    }

    #[test]
    fn an_insertion_changes_no_existing_line() {
        assert_eq!(
            changes(Some(FILE.as_bytes()), &replace("two\n", "two\nnew\n")),
            Some(vec![c(3, 0, 1)])
        );
    }

    #[test]
    fn a_multi_edit_far_apart_is_two_changes_not_one_span() {
        let edit = Edit::Replace(vec![
            Replacement {
                old: "one".into(),
                new: "1".into(),
                replace_all: false,
            },
            Replacement {
                old: "five".into(),
                new: "5\n6".into(),
                replace_all: false,
            },
        ]);
        assert_eq!(
            changes(Some(FILE.as_bytes()), &edit),
            Some(vec![c(1, 1, 1), c(5, 1, 2)])
        );
    }

    #[test]
    fn an_ambiguous_or_missing_old_string_is_not_located() {
        assert_eq!(changes(Some(b"a\na\n"), &replace("a", "b")), None);
        assert_eq!(changes(Some(FILE.as_bytes()), &replace("six", "6")), None);
    }

    #[test]
    fn a_whole_file_write_changes_only_the_lines_that_differ() {
        let after = "one\nTWO\nthree\nfour\nfive\nsix\n";
        assert_eq!(
            changes(Some(FILE.as_bytes()), &Edit::Write(after.into())),
            Some(vec![c(2, 1, 1), c(6, 0, 1)])
        );
        assert_eq!(
            changes(None, &Edit::Write("a\nb\n".into())),
            Some(vec![c(1, 0, 2)])
        );
    }

    #[test]
    fn a_codex_hunk_is_read_off_where_it_matches() {
        let hunk = Hunk {
            lines: vec![
                (HunkLine::Context, "two".into()),
                (HunkLine::Removed, "three".into()),
                (HunkLine::Added, "3".into()),
                (HunkLine::Context, "four".into()),
                (HunkLine::Added, "4.5".into()),
            ],
        };
        assert_eq!(
            changes(Some(FILE.as_bytes()), &Edit::Hunks(vec![hunk])),
            Some(vec![c(3, 1, 1), c(5, 0, 1)])
        );
    }

    #[test]
    fn a_hunk_placed_only_without_its_trailing_empty_lines_is_not_located() {
        // The host retries without the empty removed line, which makes the
        // hunk an insertion; what it changes is then not stated exactly.
        let hunk = Hunk {
            lines: vec![
                (HunkLine::Context, "x".into()),
                (HunkLine::Removed, "".into()),
                (HunkLine::Added, "q".into()),
            ],
        };
        assert_eq!(changes(Some(b"x\nz\n"), &Edit::Hunks(vec![hunk])), None);
    }

    #[test]
    fn a_replacement_that_would_grow_past_the_bound_is_refused_before_it_is_made() {
        // 1 MiB of `a`, each replaced by 1 KiB: about 1 GiB if made.
        assert_eq!(
            replaced_size(MAX_LOCATED_BYTES, MAX_LOCATED_BYTES, 1, 1024),
            None
        );
        assert_eq!(replaced_size(10, usize::MAX, 1, 2), None);
        assert_eq!(replaced_size(10, 2, 1, 3), Some(14));
        let edit = Edit::Replace(vec![Replacement {
            old: "a".into(),
            new: "b".repeat(1024),
            replace_all: true,
        }]);
        assert_eq!(changes(Some("a".repeat(4096).as_bytes()), &edit), None);
    }

    fn entry(session: &str, at_ms: u64, added: Option<&[&str]>) -> Entry {
        Entry {
            session_id: session.into(),
            path: "a.txt".into(),
            at_ms,
            after: langspan_core::FileState::Missing,
            lines: added.map(|added| crate::journal::Lines {
                added: added
                    .iter()
                    .map(|line| crate::journal::line_key(line.as_bytes()))
                    .collect(),
                removed: Vec::new(),
            }),
            partial: false,
        }
    }
    fn range(start_line: u32, end_line: u32) -> LineRange {
        LineRange {
            start_line,
            end_line,
        }
    }
    const NOW: u64 = 10 * LINE_HORIZON_MS;

    #[test]
    fn a_changed_line_another_session_added_names_it_and_its_age() {
        let entries = [entry("alice", NOW - 2_000, Some(&["line 3\n"]))];
        let found = overwritten(
            &entries,
            "bob",
            "a.txt",
            FILE_THREE.as_bytes(),
            Some(&[range(2, 3)]),
            NOW,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "alice");
        assert_eq!(found[0].lines, [range(3, 3)]);
        assert_eq!(found[0].age_ms, 2_000);
        // Its own lines, other lines, and lines past the horizon do not count.
        assert!(
            overwritten(&entries, "alice", "a.txt", FILE_THREE.as_bytes(), None, NOW).is_empty()
        );
        assert!(
            overwritten(
                &entries,
                "bob",
                "a.txt",
                FILE_THREE.as_bytes(),
                Some(&[range(1, 2)]),
                NOW
            )
            .is_empty()
        );
        let old = [entry(
            "alice",
            NOW - LINE_HORIZON_MS - 1,
            Some(&["line 3\n"]),
        )];
        assert!(overwritten(&old, "bob", "a.txt", FILE_THREE.as_bytes(), None, NOW).is_empty());
    }

    #[test]
    fn every_writer_of_a_line_is_named_and_trivial_lines_name_nobody() {
        // Lines are known by content: both sessions added this text.
        let file = "}\nshared line\n";
        let entries = [
            entry("alice", NOW - 5_000, Some(&["shared line\n", "}\n"])),
            entry("carol", NOW - 1_000, Some(&["shared line\n"])),
        ];
        let found = overwritten(&entries, "bob", "a.txt", file.as_bytes(), None, NOW);
        let named: Vec<(&str, &[LineRange])> = found
            .iter()
            .map(|entry| (entry.session_id.as_str(), entry.lines.as_slice()))
            .collect();
        assert_eq!(
            named,
            [("carol", &[range(2, 2)][..]), ("alice", &[range(2, 2)][..])],
            "newest first; line 1 is a lone brace and names nobody"
        );
    }

    #[test]
    fn a_write_with_unknown_lines_names_its_writer_for_any_changed_line() {
        let entries = [entry("alice", NOW - 1_000, None)];
        let found = overwritten(
            &entries,
            "bob",
            "a.txt",
            FILE_THREE.as_bytes(),
            Some(&[range(1, 1)]),
            NOW,
        );
        assert_eq!(found.len(), 1);
        assert!(found[0].lines.is_empty());
        // An edit that only adds lines changes none.
        assert!(
            overwritten(
                &entries,
                "bob",
                "a.txt",
                FILE_THREE.as_bytes(),
                Some(&[]),
                NOW
            )
            .is_empty()
        );
    }

    #[test]
    fn writers_are_bounded_newest_first() {
        let names: Vec<String> = (0..=MAX_WRITTEN).map(|i| format!("w{i:03}")).collect();
        let entries: Vec<Entry> = names
            .iter()
            .enumerate()
            .map(|(i, name)| entry(name, NOW - 100_000 + i as u64, None))
            .collect();
        let found = overwritten(&entries, "bob", "a.txt", FILE_THREE.as_bytes(), None, NOW);
        assert_eq!(found.len(), MAX_WRITTEN);
        assert_eq!(found[0].session_id, names[MAX_WRITTEN], "newest first");
        assert!(
            !found.iter().any(|entry| entry.session_id == names[0]),
            "oldest dropped"
        );
    }

    #[test]
    fn the_comparison_matches_a_brute_force_line_count_on_many_pairs() {
        // Applying the changes to `before` must give `after`, and the number
        // of changed lines must be the minimum a line edit can reach.
        let words = ["a\n", "b\n", "c\n", "d"];
        let mut seed = 7u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 33) as usize
        };
        for _ in 0..500 {
            let mut make = || {
                let len = next() % 7;
                let mut text: String = (0..len).map(|_| words[next() % 3]).collect();
                if next() % 4 == 0 {
                    text.push_str(words[3]);
                }
                text
            };
            let before = make();
            let after = make();
            let found = compared(&before, &after).unwrap();
            let old: Vec<&str> = before.split_inclusive('\n').collect();
            let new: Vec<&str> = after.split_inclusive('\n').collect();
            let mut rebuilt: Vec<&str> = Vec::new();
            let mut taken = 0usize;
            for change in &found {
                let start = change.start as usize - 1;
                rebuilt.extend(&old[taken..start]);
                taken = start + change.removed as usize;
                // Unchanged lines and earlier additions are already in place.
                let at = rebuilt.len();
                rebuilt.extend(&new[at..at + change.added as usize]);
            }
            rebuilt.extend(&old[taken..]);
            assert_eq!(rebuilt, new, "{before:?} -> {after:?}: {found:?}");
            // Edit distance by dynamic programming.
            let mut table = vec![vec![0usize; new.len() + 1]; old.len() + 1];
            for i in 0..=old.len() {
                for j in 0..=new.len() {
                    table[i][j] = if i == 0 || j == 0 {
                        i + j
                    } else if old[i - 1] == new[j - 1] {
                        table[i - 1][j - 1]
                    } else {
                        1 + table[i - 1][j].min(table[i][j - 1])
                    };
                }
            }
            let distance: usize = found.iter().map(|c| (c.removed + c.added) as usize).sum();
            assert_eq!(
                distance,
                table[old.len()][new.len()],
                "{before:?} -> {after:?}"
            );
        }
    }
}
