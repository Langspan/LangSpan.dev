//! Who wrote the lines a shell command's change only reformats (issues #124
//! and #141, ADR 0011).
//!
//! A formatter keeps every token of the lines it rewrites, in order, and
//! changes only whitespace and line breaks. Such a rewrite does not change
//! who wrote the code: each line it leaves keeps the author of the lines it
//! came from, whoever ran the formatter and whatever ran alongside it. Only
//! a change that alters tokens is credited as any other shell change is: to
//! the command's session, or to no session when another session's command
//! could have made it (or it rewrote another session's uncommitted lines).
//!
//! Pure: the caller passes both versions and who wrote each line before.
//!
//! What a token is depends on the file's language (`language`):
//!
//! - Code (Rust, TypeScript, JavaScript): runs of word bytes (ASCII letters
//!   and digits, `_`, any byte of a non-ASCII character), and each other
//!   byte that is not ASCII whitespace. A comma just before a closing `)`,
//!   `]` or `}` is not a token, as formatters add and remove such trailing
//!   commas when they wrap a list.
//! - Prose (Markdown): the same tokens, every comma kept.
//! - Any other file: every byte that is not ASCII whitespace, so the text
//!   is compared with all whitespace removed.
//!
//! A change of whitespace inside a string literal therefore counts as a
//! reformat, in every language.

use crate::journal::{Lines, MAX_KEYS, line_key};
use std::collections::BTreeMap;

/// Tokens one file's comparison may examine. Past it the change is not
/// split, and nothing in it is credited by this module: the caller's rules
/// for a change that alters tokens apply to all of it.
pub const MAX_WORK: usize = 4_000_000;

/// How a file's text is split into tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// Rust, TypeScript and JavaScript.
    Code,
    /// Markdown.
    Prose,
    /// Anything else: compared with all whitespace removed.
    Other,
}

/// The language of a workspace path, by its file name's extension.
pub fn language(path: &str) -> Language {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_ascii_lowercase(),
        _ => return Language::Other,
    };
    match extension.as_str() {
        "rs" | "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Language::Code,
        "md" | "markdown" | "mdx" => Language::Prose,
        _ => Language::Other,
    }
}

/// Who wrote one line of the file before the command, by the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Author {
    /// Every copy of the line is in `HEAD`: no session's uncommitted line.
    Committed,
    /// Every copy is uncommitted, and journaled as this one session's.
    Session(String),
    /// Not known to be one session's or committed.
    Unknown,
}

/// What of a change is credited, and to whom.
#[derive(Debug, PartialEq, Eq)]
pub struct Credited {
    /// Per session, the lines credited to it, as a journal entry records a
    /// write's lines. Sessions credited with nothing are absent.
    pub sessions: BTreeMap<String, Lines>,
    /// The whole change is credited to the command's session alone.
    pub whole: bool,
}

/// What a change is compared as.
pub struct Change<'a> {
    pub before: &'a [u8],
    pub after: &'a [u8],
    pub language: Language,
    /// The session whose command made the change.
    pub command: &'a str,
    /// Another session's command could also have made it.
    pub overlapped: bool,
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// A line's tokens, in order, before any comma is dropped.
fn tokens(line: &[u8], language: Language) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < line.len() {
        let byte = line[at];
        if byte.is_ascii_whitespace() {
            at += 1;
        } else if word(byte) && language != Language::Other {
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

fn closing(token: &[u8]) -> bool {
    matches!(token, b")" | b"]" | b"}")
}

/// The tokens of `lines[index]` in a version, with a code file's trailing
/// commas dropped: `next` is the first token of the following line that
/// has one.
fn line_tokens<'a>(
    lines: &[&'a [u8]],
    index: usize,
    language: Language,
    next: &[Option<&'a [u8]>],
) -> Vec<&'a [u8]> {
    let mut found = tokens(lines[index], language);
    if language == Language::Code {
        let following = next.get(index).copied().flatten();
        let mut kept = Vec::with_capacity(found.len());
        for (at, token) in found.iter().enumerate() {
            let after = found.get(at + 1).copied().or(following);
            if *token == b"," && after.is_some_and(closing) {
                continue;
            }
            kept.push(*token);
        }
        found = kept;
    }
    found
}

/// For each line, the first token of the next line that has one: what a
/// trailing comma at its end is followed by. Only code files need it.
fn next_tokens<'a>(lines: &[&'a [u8]], language: Language) -> Vec<Option<&'a [u8]>> {
    if language != Language::Code {
        return Vec::new();
    }
    let mut next = vec![None; lines.len()];
    let mut following = None;
    for index in (0..lines.len()).rev() {
        next[index] = following;
        if let Some(first) = tokens(lines[index], language).first() {
            following = Some(*first);
        }
    }
    next
}

/// One change's lines, by index into the change's removed and added lines:
/// the groups with the same tokens, and the lines in none.
#[derive(Debug, Default)]
struct Split {
    groups: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
    other_removed: Vec<usize>,
    other_added: Vec<usize>,
}

/// The work bound was reached.
struct Exhausted;

/// Take `tokens` from the tokens a comparison may still examine.
fn spend(left: &mut usize, tokens: usize) -> Result<(), Exhausted> {
    *left = left.checked_sub(tokens).ok_or(Exhausted)?;
    Ok(())
}

/// From `removed[i]` and `added[j]` on, the smallest run of removed lines
/// (at least one) and added lines with the same tokens, at least one token
/// unless no line left on either side has one, followed by the lines
/// without tokens after it: the ends of both runs, or `None` when the
/// tokens part first. `left` is the tokens it may still examine.
fn group(
    removed: &[Vec<&[u8]>],
    added: &[Vec<&[u8]>],
    i: usize,
    j: usize,
    left: &mut usize,
) -> Result<Option<(usize, usize)>, Exhausted> {
    let (mut i, mut j) = (i, j);
    let mut ours: Vec<&[u8]> = Vec::new();
    let mut theirs: Vec<&[u8]> = Vec::new();
    let mut matched = 0;
    loop {
        let common = ours.len().min(theirs.len());
        spend(left, common - matched)?;
        if ours[matched..common] != theirs[matched..common] {
            return Ok(None);
        }
        matched = common;
        if ours.len() == theirs.len() && !ours.is_empty() {
            while removed.get(i).is_some_and(Vec::is_empty) {
                i += 1;
            }
            while added.get(j).is_some_and(Vec::is_empty) {
                j += 1;
            }
            return Ok(Some((i, j)));
        }
        if ours.len() <= theirs.len() {
            match removed.get(i) {
                Some(line) => {
                    spend(left, line.len())?;
                    ours.extend(line);
                    i += 1;
                }
                // Only lines without tokens are left on both sides.
                None if theirs.is_empty() && added[j..].iter().all(Vec::is_empty) => {
                    return Ok(Some((i, added.len())));
                }
                None => return Ok(None),
            }
        } else {
            let Some(line) = added.get(j) else {
                return Ok(None);
            };
            spend(left, line.len())?;
            theirs.extend(line);
            j += 1;
        }
    }
}

/// Split one change (`removed` lines became `added` lines) into groups of
/// lines with the same tokens. Where the tokens part, one removed and one
/// added line are left out and grouping goes on after them; added lines
/// left at the end are in no group, unless none of them has a token and a
/// group comes before them.
fn split(
    removed: &[Vec<&[u8]>],
    added: &[Vec<&[u8]>],
    left: &mut usize,
) -> Result<Split, Exhausted> {
    let mut out = Split::default();
    let (mut i, mut j) = (0, 0);
    while i < removed.len() {
        match group(removed, added, i, j, left)? {
            Some((end_removed, end_added)) => {
                out.groups.push((i..end_removed, j..end_added));
                (i, j) = (end_removed, end_added);
            }
            None => {
                out.other_removed.push(i);
                if j < added.len() {
                    out.other_added.push(j);
                }
                i += 1;
                j = (j + 1).min(added.len());
            }
        }
    }
    let rest = j..added.len();
    match out.groups.last_mut() {
        Some((_, last)) if last.end == j && added[rest.clone()].iter().all(Vec::is_empty) => {
            last.end = added.len();
        }
        _ => out.other_added.extend(rest),
    }
    Ok(out)
}

/// The session a line is credited to, `None` for no session.
type Owner = Option<String>;

/// Who is credited with the lines a change of `change.before` into
/// `change.after` gives, where `author` says who wrote each line of
/// `before`, by its line key.
///
/// A group of lines that keeps its tokens is credited to the one session
/// that wrote the lines it removes (those with a token; a group of lines
/// without any goes by all its removed lines). A committed line counts as
/// the command's session's unless the change `overlapped` another session's
/// command; an unknown line, or lines of several sessions, credit the group
/// to no session. The other lines of a change are the command's session's
/// when nothing overlapped and every line they remove is committed or that
/// session's own; otherwise no session's.
///
/// A credited line whose key the uncredited part also adds or removes is
/// not credited: the journal matches lines by content, so that copy could
/// be taken for the other. `None` when the versions cannot be compared (not
/// UTF-8, the work bound, or more credited lines than the journal keeps).
pub fn authors(change: &Change<'_>, author: impl Fn(&str) -> Author) -> Option<Credited> {
    authors_within(change, author, MAX_WORK)
}

/// `authors`, examining at most `limit` tokens.
fn authors_within(
    change: &Change<'_>,
    author: impl Fn(&str) -> Author,
    limit: usize,
) -> Option<Credited> {
    let changes = crate::lines::compared(
        std::str::from_utf8(change.before).ok()?,
        std::str::from_utf8(change.after).ok()?,
    )?;
    let old: Vec<&[u8]> = change
        .before
        .split_inclusive(|byte| *byte == b'\n')
        .collect();
    let new: Vec<&[u8]> = change
        .after
        .split_inclusive(|byte| *byte == b'\n')
        .collect();
    let (old_next, new_next) = (
        next_tokens(&old, change.language),
        next_tokens(&new, change.language),
    );
    let command = || Some(change.command.to_owned());
    let resolve = |line: &[u8]| -> Owner {
        match author(&line_key(line)) {
            Author::Committed if change.overlapped => None,
            Author::Committed => command(),
            Author::Session(session) => Some(session),
            Author::Unknown => None,
        }
    };
    let mut nets: BTreeMap<Owner, BTreeMap<String, i64>> = BTreeMap::new();
    let mut credit = |owner: &Owner, lines: &mut dyn Iterator<Item = (&[u8], i64)>| {
        let net = nets.entry(owner.clone()).or_default();
        for (line, sign) in lines {
            *net.entry(line_key(line)).or_default() += sign;
        }
    };
    let mut left = limit;
    // Each change as the lines it replaces in `before` and those it gives in
    // `after`. Changes with only blank lines between them are one: the
    // comparison may have matched a blank line inside one reformat.
    let mut blocks: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    // Added lines start this far after removed ones in `after`.
    let mut shift: isize = 0;
    for run in changes {
        let start = (run.start as usize).checked_sub(1)?;
        let (count_removed, count_added) = (run.removed as usize, run.added as usize);
        let from = start.checked_add_signed(shift)?;
        shift += count_added as isize - count_removed as isize;
        let (rows, cols) = (start..start + count_removed, from..from + count_added);
        match blocks.last_mut() {
            Some((last_rows, last_cols))
                if old
                    .get(last_rows.end..rows.start)?
                    .iter()
                    .all(|line| line.iter().all(u8::is_ascii_whitespace)) =>
            {
                last_rows.end = rows.end;
                last_cols.end = cols.end;
            }
            _ => blocks.push((rows, cols)),
        }
    }
    for (rows, cols) in blocks {
        let removed_lines = old.get(rows.clone())?;
        let added_lines = new.get(cols.clone())?;
        let removed: Vec<Vec<&[u8]>> = rows
            .map(|index| line_tokens(&old, index, change.language, &old_next))
            .collect();
        let added: Vec<Vec<&[u8]>> = cols
            .map(|index| line_tokens(&new, index, change.language, &new_next))
            .collect();
        spend(
            &mut left,
            removed.iter().chain(&added).map(Vec::len).sum::<usize>(),
        )
        .ok()?;
        let parts = split(&removed, &added, &mut left).ok()?;
        for (rows, cols) in &parts.groups {
            let with_tokens: Vec<usize> =
                rows.clone().filter(|r| !removed[*r].is_empty()).collect();
            let deciding = if with_tokens.is_empty() {
                rows.clone().collect()
            } else {
                with_tokens
            };
            let owners: Vec<Owner> = deciding
                .iter()
                .map(|r| resolve(removed_lines[*r]))
                .collect();
            let owner = match owners.split_first() {
                Some((first, rest)) if rest.iter().all(|o| o == first) => first.clone(),
                _ => None,
            };
            credit(
                &owner,
                &mut rows
                    .clone()
                    .map(|r| (removed_lines[r], -1))
                    .chain(cols.clone().map(|c| (added_lines[c], 1))),
            );
        }
        let others_own = !change.overlapped
            && parts
                .other_removed
                .iter()
                .all(|r| resolve(removed_lines[*r]).as_deref() == Some(change.command));
        let owner = if others_own { command() } else { None };
        credit(
            &owner,
            &mut parts
                .other_removed
                .iter()
                .map(|r| (removed_lines[*r], -1))
                .chain(parts.other_added.iter().map(|c| (added_lines[*c], 1))),
        );
    }
    let uncredited = nets.remove(&None).unwrap_or_default();
    let unmatched = |key: &str| uncredited.get(key).is_some_and(|count| *count != 0);
    let mut dropped = uncredited.values().any(|count| *count != 0);
    let mut sessions = BTreeMap::new();
    for (owner, net) in nets {
        let Some(session) = owner else { continue };
        let mut lines = Lines {
            added: Vec::new(),
            removed: Vec::new(),
        };
        for (key, count) in net {
            if count == 0 {
                continue;
            }
            if unmatched(&key) {
                dropped = true;
                continue;
            }
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
        if !lines.added.is_empty() || !lines.removed.is_empty() {
            sessions.insert(session, lines);
        }
    }
    let whole = !dropped && sessions.keys().all(|session| session == change.command);
    Some(Credited { sessions, whole })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "me";

    fn keys(lines: &[&str]) -> Vec<String> {
        let mut keys: Vec<String> = lines.iter().map(|l| line_key(l.as_bytes())).collect();
        keys.sort();
        keys
    }

    /// Who wrote each line of `before`: `who` maps a line's text to its
    /// author; any other line is committed.
    fn credit(
        before: &str,
        after: &str,
        language: Language,
        overlapped: bool,
        who: &[(&str, Author)],
    ) -> Credited {
        let who: BTreeMap<String, Author> = who
            .iter()
            .map(|(line, author)| (line_key(line.as_bytes()), author.clone()))
            .collect();
        let change = Change {
            before: before.as_bytes(),
            after: after.as_bytes(),
            language,
            command: ME,
            overlapped,
        };
        authors(&change, |key| {
            who.get(key).cloned().unwrap_or(Author::Committed)
        })
        .unwrap()
    }

    fn session(name: &str) -> Author {
        Author::Session(name.into())
    }

    #[test]
    fn tokens_ignore_whitespace_but_not_where_words_meet() {
        let code = Language::Code;
        assert_eq!(
            tokens(b"fn a(x,y) {}\n", code),
            tokens(b"  fn a( x, y ) { }", code)
        );
        assert_ne!(tokens(b"let x", code), tokens(b"letx", code));
        assert_ne!(tokens(b"a(x,y)", code), tokens(b"a(x,z)", code));
        assert_eq!(tokens(b"\t\n", code), Vec::<&[u8]>::new());
        // A non-ASCII character is part of its word.
        assert_eq!(tokens("é1 x".as_bytes(), code), vec!["é1".as_bytes(), b"x"]);
        // Other files: all whitespace removed, so words may meet.
        let other = Language::Other;
        assert_eq!(tokens(b"let x", other), tokens(b"letx", other));
        assert_ne!(tokens(b"key: 1", other), tokens(b"key: 2", other));
    }

    #[test]
    fn a_files_language_is_its_extension() {
        for path in ["src/a.rs", "web/App.TSX", "x/y.mjs", "lib.cts", "a.js"] {
            assert_eq!(language(path), Language::Code, "{path}");
        }
        for path in ["README.md", "docs/a.markdown", "page.mdx"] {
            assert_eq!(language(path), Language::Prose, "{path}");
        }
        for path in [
            "Cargo.toml",
            "a.py",
            "Makefile",
            ".rs",
            "dir.rs/notes",
            "a.txt",
        ] {
            assert_eq!(language(path), Language::Other, "{path}");
        }
    }

    #[test]
    fn a_reformat_keeps_each_lines_author_whoever_ran_it() {
        let before = "keep\nfn a(x,y) {}\nfn b(p,q) {}\n";
        let after = "keep\nfn a(x, y) {}\nfn b(p, q) {}\n";
        for overlapped in [false, true] {
            let credited = credit(
                before,
                after,
                Language::Code,
                overlapped,
                &[
                    ("fn a(x,y) {}\n", session("alice")),
                    ("fn b(p,q) {}\n", session("bob")),
                ],
            );
            assert!(!credited.whole);
            assert_eq!(credited.sessions["alice"].added, keys(&["fn a(x, y) {}\n"]));
            assert_eq!(
                credited.sessions["alice"].removed,
                keys(&["fn a(x,y) {}\n"])
            );
            assert_eq!(credited.sessions["bob"].added, keys(&["fn b(p, q) {}\n"]));
            assert!(!credited.sessions.contains_key(ME));
        }
        // All the command's session's own: the whole change is its own.
        let mine = credit(
            before,
            after,
            Language::Code,
            true,
            &[
                ("fn a(x,y) {}\n", session(ME)),
                ("fn b(p,q) {}\n", session(ME)),
            ],
        );
        assert!(mine.whole, "{mine:?}");
    }

    #[test]
    fn joined_and_split_lines_are_a_reformat_and_a_wrapped_lists_trailing_comma_is_not_a_token() {
        let alice = |text: &'static str| -> Vec<(&'static str, Author)> {
            text.split_inclusive('\n')
                .map(|line| (line, session("alice")))
                .collect()
        };
        let before = "call(a,\nb);\nlong(x, y, z);\n";
        let after = "call(a, b);\nlong(\n    x,\n    y,\n    z,\n);\n";
        let code = credit(before, after, Language::Code, true, &alice(before));
        assert!(
            code.sessions["alice"].added.len() == 6 && code.sessions.len() == 1,
            "{code:?}"
        );
        // Wrapped with a trailing comma whose closing bracket did not change.
        let wrapped = credit(
            "f(\n    a,\n    b\n)\n",
            "f(\n    a,\n    b,\n)\n",
            Language::Code,
            true,
            &alice("f(\n    a,\n    b\n)\n"),
        );
        assert_eq!(wrapped.sessions["alice"].added, keys(&["    b,\n"]));
        // In Markdown, or any other file, a comma is a comma: only the
        // joined call keeps its author.
        for language in [Language::Prose, Language::Other] {
            let other = credit(before, after, language, true, &alice(before));
            assert_eq!(
                other.sessions["alice"].added,
                keys(&["call(a, b);\n"]),
                "{language:?}"
            );
        }
        // A comma before anything else is still a token.
        let changed = credit(
            "f(a, b)\n",
            "f(a b)\n",
            Language::Code,
            true,
            &alice("f(a, b)\n"),
        );
        assert!(changed.sessions.is_empty(), "{changed:?}");
    }

    #[test]
    fn a_rewrapped_markdown_paragraph_and_reindented_other_text_keep_their_author() {
        let before = "# Title\n\nA paragraph that\nwraps   here and\nends.\n";
        let after = "# Title\n\nA paragraph that wraps here\nand ends.\n";
        let who: Vec<(&str, Author)> = before
            .split_inclusive('\n')
            .skip(2)
            .map(|line| (line, session("bob")))
            .collect();
        let prose = credit(before, after, Language::Prose, true, &who);
        assert_eq!(
            prose.sessions["bob"].added,
            keys(&["A paragraph that wraps here\n", "and ends.\n"])
        );
        let toml = credit(
            "[a]\nkey=1\n",
            "[a]\n  key = 1\n",
            Language::Other,
            true,
            &[("key=1\n", session("bob"))],
        );
        assert_eq!(toml.sessions["bob"].added, keys(&["  key = 1\n"]));
    }

    #[test]
    fn a_token_change_is_the_commands_unless_it_overlapped_or_rewrote_anothers_line() {
        let before = "keep\nlet total = 1;\n";
        let after = "keep\nlet sum = 1;\nsum;\n";
        let own = [("let total = 1;\n", session(ME))];
        let alone = credit(before, after, Language::Code, false, &own);
        assert!(alone.whole, "{alone:?}");
        // While another session's command ran: no session's.
        let overlapped = credit(before, after, Language::Code, true, &own);
        assert!(
            overlapped.sessions.is_empty() && !overlapped.whole,
            "{overlapped:?}"
        );
        // Rewriting another session's uncommitted line: no session's.
        let theirs = credit(
            before,
            after,
            Language::Code,
            false,
            &[("let total = 1;\n", session("bob"))],
        );
        assert!(theirs.sessions.is_empty() && !theirs.whole, "{theirs:?}");
        // A pure addition while nothing ran is the command's.
        let added = credit("a\n", "a\nb\n", Language::Code, false, &[]);
        assert!(added.whole && added.sessions[ME].added == keys(&["b\n"]));
    }

    #[test]
    fn committed_lines_count_as_the_commands_only_when_nothing_overlapped() {
        let before = "fn a(x,y) {}\n";
        let after = "fn a(x, y) {}\n";
        assert!(credit(before, after, Language::Code, false, &[]).whole);
        let overlapped = credit(before, after, Language::Code, true, &[]);
        assert!(overlapped.sessions.is_empty() && !overlapped.whole);
    }

    #[test]
    fn a_group_of_several_sessions_or_unknown_lines_is_no_ones() {
        // Alice's line and Bob's joined into one.
        let joined = credit(
            "call(a,\nb);\n",
            "call(a, b);\n",
            Language::Code,
            false,
            &[("call(a,\n", session("alice")), ("b);\n", session("bob"))],
        );
        assert!(joined.sessions.is_empty() && !joined.whole, "{joined:?}");
        let unknown = credit(
            "f(a,b);\n",
            "f(a, b);\n",
            Language::Code,
            false,
            &[("f(a,b);\n", Author::Unknown)],
        );
        assert!(unknown.sessions.is_empty() && !unknown.whole);
    }

    #[test]
    fn a_line_also_in_the_uncredited_part_is_not_credited() {
        // Bob's `x();` is reformatted; another `x();` is added by a change
        // nobody is credited with, so neither copy is credited.
        let credited = credit(
            "x( );\ny\n",
            "x();\nz\nx();\n",
            Language::Code,
            true,
            &[("x( );\n", session("bob")), ("y\n", session("bob"))],
        );
        assert!(!credited.whole);
        assert!(
            credited
                .sessions
                .get("bob")
                .is_none_or(|lines| !lines.added.contains(&line_key(b"x();\n"))),
            "{credited:?}"
        );
    }

    #[test]
    fn lines_without_tokens_join_a_neighbouring_group() {
        let who: Vec<(&str, Author)> =
            vec![("fn a() {\n", session("alice")), ("x\n", session("alice"))];
        // A blank line removed between Alice's lines, and one added after.
        let credited = credit(
            "fn a() {\n\nx\n}\n",
            "fn a() {\n    x\n\n}\n",
            Language::Code,
            true,
            &who,
        );
        assert_eq!(credited.sessions.len(), 1, "{credited:?}");
        assert!(
            credited.sessions["alice"]
                .added
                .contains(&line_key(b"    x\n"))
        );
    }

    #[test]
    fn not_utf8_or_past_the_work_bound_is_not_compared() {
        let change = Change {
            before: b"\xff\n",
            after: b"x\n",
            language: Language::Code,
            command: ME,
            overlapped: true,
        };
        assert!(authors(&change, |_| Author::Committed).is_none());
        // A long line split in two: 4,000 tokens to read, 4,000 to gather
        // and 2,000 to compare. Within a bound of that, it is compared;
        // below, nothing.
        let long: String = (0..1000).map(|n| format!("a{n} ")).collect();
        let before = format!("{long}{long}\n");
        let after = format!("{long}\n{long}\n");
        let change = Change {
            before: before.as_bytes(),
            after: after.as_bytes(),
            language: Language::Code,
            command: ME,
            overlapped: true,
        };
        let bob = |_: &str| Author::Session("bob".into());
        assert_eq!(
            authors_within(&change, bob, 10_000).unwrap().sessions["bob"]
                .added
                .len(),
            2
        );
        assert!(authors_within(&change, bob, 9_999).is_none());
    }

    /// A large file every line of which is re-indented: what one file's
    /// comparison costs at the journal's limit of 8,192 credited lines.
    #[test]
    fn a_large_reformat_is_compared_within_the_bound() {
        let before: String = (0..8000)
            .map(|n| format!("fn f{n}(a,b) {{ a+b }}\n"))
            .collect();
        let after: String = (0..8000)
            .map(|n| format!("    fn f{n}(a, b) {{ a + b }}\n"))
            .collect();
        let change = Change {
            before: before.as_bytes(),
            after: after.as_bytes(),
            language: Language::Code,
            command: ME,
            overlapped: true,
        };
        let started = std::time::Instant::now();
        let credited = authors(&change, |_| Author::Session("bob".into())).unwrap();
        eprintln!(
            "{} KiB, 8,000 lines re-indented: {:?}",
            before.len() / 1024,
            started.elapsed()
        );
        assert_eq!(credited.sessions["bob"].added.len(), 8000);
        assert!(!credited.whole);
    }
}
