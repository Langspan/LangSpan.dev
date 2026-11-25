//! Which part of a shell command's change to a file only reformats lines
//! its own session wrote (issue #124, ADR 0011).
//!
//! While another session's shell command runs, a command's writes cannot be
//! told from that command's by comparing the workspace before and after: the
//! other command could have written anything. A formatter's rewrite is the
//! exception this module finds. It keeps every token of the lines it
//! rewrites, in order, and changes only whitespace and line breaks; when the
//! lines it rewrote are lines the formatting session itself wrote, the
//! result is that session's whoever else was running.
//!
//! Pure: the caller passes both versions and the lines it may credit.
//!
//! Tokens are runs of word bytes (ASCII letters and digits, `_`, any byte of
//! a non-ASCII character) and each other byte that is not ASCII whitespace.
//! A change of whitespace inside a string literal therefore counts as a
//! reformat; nothing else that changes a token does.

use crate::journal::{Lines, MAX_KEYS, line_key};
use std::collections::BTreeMap;

/// What of a change may be credited to the session whose lines it reformats.
#[derive(Debug, PartialEq, Eq)]
pub struct Credited {
    /// The credited part, as a journal entry records a write's lines.
    pub lines: Lines,
    /// Every changed line is in it: the whole change only reformats this
    /// session's lines.
    pub whole: bool,
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// A line's tokens, in order.
fn tokens(line: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < line.len() {
        let byte = line[at];
        if byte.is_ascii_whitespace() {
            at += 1;
        } else if word(byte) {
            let start = at;
            while at < line.len() && word(line[at]) {
                at += 1;
            }
            out.push(&line[start..at]);
        } else {
            out.push(&line[at..=at]);
            at += 1;
        }
    }
    out
}

/// The lines of one change, split into those credited and the rest.
#[derive(Default)]
struct Split<'a> {
    credited_removed: Vec<&'a [u8]>,
    credited_added: Vec<&'a [u8]>,
    other_removed: Vec<&'a [u8]>,
    other_added: Vec<&'a [u8]>,
}

/// From `removed[i]` and `added[j]` on, the smallest run of removed lines
/// (at least one) and added lines with the same tokens: the ends of both
/// runs, or `None` when the tokens part first.
fn group(removed: &[&[u8]], added: &[&[u8]], i: usize, j: usize) -> Option<(usize, usize)> {
    let (mut i, mut j) = (i, j);
    let mut ours = tokens(removed.get(i)?);
    i += 1;
    let mut theirs: Vec<&[u8]> = Vec::new();
    let mut matched = 0;
    loop {
        let common = ours.len().min(theirs.len());
        if ours[matched..common] != theirs[matched..common] {
            return None;
        }
        matched = common;
        if ours.len() == theirs.len() {
            return Some((i, j));
        }
        if ours.len() < theirs.len() {
            ours.extend(tokens(removed.get(i)?));
            i += 1;
        } else {
            theirs.extend(tokens(added.get(j)?));
            j += 1;
        }
    }
}

/// Split one change (`removed` lines became `added` lines) into groups of
/// lines with the same tokens. A group is credited when each line it removes
/// has a copy left in `budget`, which it takes. Where the tokens part, one
/// removed and one added line are left uncredited and grouping goes on
/// after them.
fn split<'a>(
    removed: &[&'a [u8]],
    added: &[&'a [u8]],
    budget: &mut BTreeMap<String, usize>,
    out: &mut Split<'a>,
) {
    let (mut i, mut j) = (0, 0);
    while i < removed.len() {
        let credited = group(removed, added, i, j).filter(|(end, _)| {
            let mut wanted: BTreeMap<String, usize> = BTreeMap::new();
            for line in &removed[i..*end] {
                *wanted.entry(line_key(line)).or_default() += 1;
            }
            wanted
                .iter()
                .all(|(key, count)| budget.get(key).is_some_and(|left| left >= count))
        });
        match credited {
            Some((end_removed, end_added)) => {
                for line in &removed[i..end_removed] {
                    if let Some(left) = budget.get_mut(&line_key(line)) {
                        *left -= 1;
                    }
                }
                out.credited_removed.extend(&removed[i..end_removed]);
                out.credited_added.extend(&added[j..end_added]);
                (i, j) = (end_removed, end_added);
            }
            None => {
                out.other_removed.push(removed[i]);
                out.other_added.extend(added.get(j));
                i += 1;
                j = (j + 1).min(added.len());
            }
        }
    }
    out.other_added.extend(&added[j..]);
}

/// The part of the change from `before` to `after` that only reformats
/// lines of `own`: line key -> copies of that line in `before` this
/// session may be credited with rewriting. `None` when the versions cannot
/// be compared (not UTF-8, or more changed lines than the journal keeps).
///
/// A credited line whose key the uncredited part also adds or removes is
/// not credited: the journal matches lines by content, so that copy could
/// be taken for the other.
pub fn own_reformat(
    before: &[u8],
    after: &[u8],
    own: &BTreeMap<String, usize>,
) -> Option<Credited> {
    let changes = crate::lines::compared(
        std::str::from_utf8(before).ok()?,
        std::str::from_utf8(after).ok()?,
    )?;
    let old: Vec<&[u8]> = before.split_inclusive(|byte| *byte == b'\n').collect();
    let new: Vec<&[u8]> = after.split_inclusive(|byte| *byte == b'\n').collect();
    let mut budget = own.clone();
    let mut out = Split::default();
    // Added lines start this far after removed ones in `after`.
    let mut shift: isize = 0;
    for change in changes {
        let start = (change.start as usize).checked_sub(1)?;
        let (removed, added) = (change.removed as usize, change.added as usize);
        let from = start.checked_add_signed(shift)?;
        split(
            old.get(start..start + removed)?,
            new.get(from..from + added)?,
            &mut budget,
            &mut out,
        );
        shift += added as isize - removed as isize;
    }
    let keys = |lines: &[&[u8]]| -> Vec<String> { lines.iter().map(|l| line_key(l)).collect() };
    let (other_removed, other_added) = (keys(&out.other_removed), keys(&out.other_added));
    let mut net: BTreeMap<String, i64> = BTreeMap::new();
    let mut dropped = false;
    for key in keys(&out.credited_removed) {
        if other_removed.contains(&key) {
            dropped = true;
        } else {
            *net.entry(key).or_default() -= 1;
        }
    }
    for key in keys(&out.credited_added) {
        if other_added.contains(&key) {
            dropped = true;
        } else {
            *net.entry(key).or_default() += 1;
        }
    }
    let mut lines = Lines {
        added: Vec::new(),
        removed: Vec::new(),
    };
    for (key, count) in net {
        let side = if count > 0 {
            &mut lines.added
        } else {
            &mut lines.removed
        };
        side.extend(std::iter::repeat_n(key, count.unsigned_abs() as usize));
    }
    if lines.added.len() > MAX_KEYS || lines.removed.len() > MAX_KEYS {
        return None;
    }
    Some(Credited {
        lines,
        whole: !dropped && other_removed.is_empty() && other_added.is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of `text` may be credited once per copy.
    fn own(text: &str) -> BTreeMap<String, usize> {
        let mut budget = BTreeMap::new();
        for line in text.split_inclusive('\n') {
            *budget.entry(line_key(line.as_bytes())).or_default() += 1;
        }
        budget
    }

    fn keys(lines: &[&str]) -> Vec<String> {
        let mut keys: Vec<String> = lines.iter().map(|l| line_key(l.as_bytes())).collect();
        keys.sort();
        keys
    }

    fn credit(before: &str, after: &str, own: &BTreeMap<String, usize>) -> Credited {
        own_reformat(before.as_bytes(), after.as_bytes(), own).unwrap()
    }

    #[test]
    fn tokens_ignore_whitespace_but_not_where_words_meet() {
        assert_eq!(tokens(b"fn a(x,y) {}\n"), tokens(b"  fn a( x, y ) { }"));
        assert_ne!(tokens(b"let x"), tokens(b"letx"));
        assert_ne!(tokens(b"a(x,y)"), tokens(b"a(x,z)"));
        assert_eq!(tokens(b"\t\n"), Vec::<&[u8]>::new());
        // A non-ASCII character is part of its word.
        assert_eq!(tokens("é1 x".as_bytes()), vec!["é1".as_bytes(), b"x"]);
    }

    #[test]
    fn a_reformat_of_own_lines_is_credited_whole() {
        let before = "keep\nfn a(x,y) {}\nfn b(p,q) {}\n";
        let after = "keep\nfn a(x, y) {}\nfn b(p, q) {}\n";
        let credited = credit(before, after, &own("fn a(x,y) {}\nfn b(p,q) {}\n"));
        assert!(credited.whole);
        assert_eq!(
            credited.lines.added,
            keys(&["fn a(x, y) {}\n", "fn b(p, q) {}\n"])
        );
        assert_eq!(
            credited.lines.removed,
            keys(&["fn a(x,y) {}\n", "fn b(p,q) {}\n"])
        );
    }

    #[test]
    fn joined_and_split_lines_are_a_reformat() {
        let mine = own("call(a,\nb);\nlong(x, y, z);\n\n");
        let joined = credit(
            "call(a,\nb);\nlong(x, y, z);\n\n",
            "call(a, b);\nlong(\n    x,\n    y,\n    z,\n);\n",
            &mine,
        );
        // The trailing comma `z,` is a new token: that group is not credited.
        assert!(!joined.whole);
        let joined = credit(
            "call(a,\nb);\nlong(x, y, z);\n\n",
            "call(a, b);\nlong(\n    x,\n    y,\n    z\n);\n",
            &mine,
        );
        assert!(joined.whole, "{joined:?}");
    }

    #[test]
    fn a_line_not_its_own_or_with_changed_tokens_is_not_credited() {
        let before = "fn a(x,y) {}\nkeep\nfn b(x,y) {}\nfn c(x,y) {}\n";
        // b's tokens changed; c is not this session's line.
        let after = "fn a(x, y) {}\nkeep\nfn b(x, z) {}\nfn c(x, y) {}\n";
        let credited = credit(before, after, &own("fn a(x,y) {}\nfn b(x,y) {}\n"));
        assert!(!credited.whole);
        assert_eq!(credited.lines.added, keys(&["fn a(x, y) {}\n"]));
        assert_eq!(credited.lines.removed, keys(&["fn a(x,y) {}\n"]));
        // Grouping goes on after a line whose tokens changed.
        let credited = credit(
            "fn b(x,y) {}\nfn a(x,y) {}\n",
            "fn b(x, z) {}\nfn a(x, y) {}\n",
            &own("fn a(x,y) {}\nfn b(x,y) {}\n"),
        );
        assert_eq!(credited.lines.added, keys(&["fn a(x, y) {}\n"]));
    }

    #[test]
    fn only_as_many_copies_as_are_its_own_are_credited() {
        let before = "f(a,b);\nf(a,b);\n";
        let after = "f(a, b);\nf(a, b);\n";
        let credited = credit(before, after, &own("f(a,b);\n"));
        assert!(!credited.whole);
        // The second copy is not credited, so neither is the first: the
        // journal could not tell one from the other.
        assert!(credited.lines.added.is_empty() && credited.lines.removed.is_empty());
        assert!(credit(before, after, &own(before)).whole);
    }

    #[test]
    fn added_lines_with_no_removed_line_are_not_credited() {
        let credited = credit("f(a,b);\n", "f(a, b);\nnew();\n", &own("f(a,b);\n"));
        assert!(!credited.whole);
        assert_eq!(credited.lines.added, keys(&["f(a, b);\n"]));
        assert!(own_reformat(b"\xff\n", b"x\n", &BTreeMap::new()).is_none());
    }
}
