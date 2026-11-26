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
//! What a token is depends on the file's language (`language`). A word is
//! a run of word bytes (ASCII letters and digits, `_`, any byte of a
//! non-ASCII character); any other byte that is not ASCII whitespace is a
//! token of its own.
//!
//! - Rust, TypeScript and JavaScript: words and other bytes, and each
//!   string or character literal is one token, whitespace and all (#158).
//!   Comments are words and other bytes. A comma just before a closing
//!   `)`, `]` or `}` is not a token, as formatters add and remove such
//!   trailing commas when they wrap a list; a Rust one-element tuple's
//!   comma, `(u32,)`, is.
//! - Prose (Markdown): words and other bytes, every comma kept.
//! - Python: words, other bytes and string literals, as in Rust, and each
//!   line that starts a logical line (outside brackets and strings) begins
//!   with its indentation as a token (#158). A call re-wrapped inside its
//!   parentheses is a reformat; a re-indented block is not.
//! - Other indentation-sensitive files (YAML, Makefiles and the others in
//!   `language`): words and other bytes, and every line that has a token
//!   begins with its indentation as one.
//! - Any other file: every byte that is not ASCII whitespace, so the text
//!   is compared with all whitespace removed.
//!
//! An indentation token is the line's leading spaces, tabs and form feeds,
//! empty for none, so two lines joined into one also differ. Trailing
//! whitespace and blank lines never count.

use crate::journal::{Lines, MAX_KEYS, line_key};
use std::collections::BTreeMap;

/// Tokens one file's comparison may examine. Past it the change is not
/// split, and nothing in it is credited by this module: the caller's rules
/// for a change that alters tokens apply to all of it.
pub const MAX_WORK: usize = 4_000_000;

/// How a file's text is split into tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// Rust.
    Rust,
    /// TypeScript and JavaScript.
    Script,
    /// Markdown.
    Prose,
    /// Python: indentation of a logical line's first line is a token.
    Python,
    /// Another indentation-sensitive format: every line's indentation is a
    /// token.
    Indented,
    /// Anything else: compared with all whitespace removed.
    Other,
}

/// The language of a workspace path, by its file name or extension.
pub fn language(path: &str) -> Language {
    let name = path.rsplit('/').next().unwrap_or(path);
    if matches!(name, "Makefile" | "makefile" | "GNUmakefile") {
        return Language::Indented;
    }
    let extension = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_ascii_lowercase(),
        _ => return Language::Other,
    };
    match extension.as_str() {
        "rs" => Language::Rust,
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Language::Script,
        "md" | "markdown" | "mdx" => Language::Prose,
        "py" | "pyi" | "pyw" => Language::Python,
        "yaml" | "yml" | "mk" | "mak" | "haml" | "pug" | "jade" | "slim" | "sass" | "styl"
        | "coffee" | "nim" | "nims" | "nimble" | "fs" | "fsi" | "fsx" => Language::Indented,
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

fn indentation(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\x0c')
}

/// Where a quoted literal opened at `at` ends: after its closing quote, or
/// unterminated at the end of the line (`multiline` false) or the text. A
/// backslash escapes the byte after it, a line break included.
fn quoted_end(text: &[u8], at: usize, multiline: bool) -> usize {
    let quote = text[at];
    let mut i = at + 1;
    while i < text.len() {
        match text[i] {
            b'\\' => i += 2,
            b'\n' if !multiline => return i,
            byte if byte == quote => return i + 1,
            _ => i += 1,
        }
    }
    text.len()
}

/// Where a Python string opened at `at` ends; a triple quote closes only at
/// the same triple quote, and may span lines.
fn python_string_end(text: &[u8], at: usize) -> usize {
    let quote = text[at];
    let triple = [quote; 3];
    if !text[at..].starts_with(&triple) {
        return quoted_end(text, at, false);
    }
    let mut i = at + 3;
    while i < text.len() {
        if text[i] == b'\\' {
            i += 2;
        } else if text[i..].starts_with(&triple) {
            return i + 3;
        } else {
            i += 1;
        }
    }
    text.len()
}

/// Where a JavaScript template literal opened at `at` ends, `${...}`
/// substitutions (and the literals inside them) included. Iterative, so
/// nesting is bounded by the text, not the stack.
fn template_end(text: &[u8], at: usize) -> usize {
    enum Frame {
        Template,
        /// Inside `${`, with this many braces open.
        Code(usize),
    }
    let mut stack = vec![Frame::Template];
    let mut i = at + 1;
    while i < text.len() {
        let Some(frame) = stack.last_mut() else {
            return i;
        };
        match (frame, text[i]) {
            (Frame::Template, b'\\') => i += 2,
            (Frame::Template, b'`') => {
                stack.pop();
                i += 1;
            }
            (Frame::Template, b'$') if text.get(i + 1) == Some(&b'{') => {
                stack.push(Frame::Code(1));
                i += 2;
            }
            (Frame::Code(open), b'{') => {
                *open += 1;
                i += 1;
            }
            (Frame::Code(open), b'}') => {
                *open -= 1;
                if *open == 0 {
                    stack.pop();
                }
                i += 1;
            }
            (Frame::Code(_), b'`') => {
                stack.push(Frame::Template);
                i += 1;
            }
            (Frame::Code(_), b'\'' | b'"') => i = quoted_end(text, i, false),
            _ => i += 1,
        }
        if stack.is_empty() {
            return i;
        }
    }
    text.len()
}

/// A Rust literal starting at `at`, which is not inside a word: a string,
/// byte or C string, raw or not, or a character or byte literal. `None`
/// for anything else, a lifetime's `'` included.
fn rust_literal_end(text: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    if matches!(text[i], b'b' | b'c') {
        i += 1;
    }
    let raw = text.get(i) == Some(&b'r');
    if raw {
        i += 1;
        let hashes = text[i..].iter().take_while(|byte| **byte == b'#').count();
        i += hashes;
        if text.get(i) != Some(&b'"') {
            return None;
        }
        let mut close = vec![b'"'];
        close.extend(std::iter::repeat_n(b'#', hashes));
        let end = text[i + 1..]
            .windows(close.len())
            .position(|window| window == close.as_slice())
            .map_or(text.len(), |found| i + 1 + found + close.len());
        return Some(end);
    }
    match text.get(i) {
        Some(b'"') => Some(quoted_end(text, i, true)),
        Some(b'\'') if i == at || text[at] == b'b' => {
            // `'x'`, `'\n'`, `'\u{1F600}'`; a lone `'` is a lifetime's.
            let rest = &text[i + 1..];
            let length = if rest.first() == Some(&b'\\') {
                rest.iter()
                    .take(12)
                    .skip(2)
                    .position(|byte| *byte == b'\'')?
                    + 2
            } else {
                let character = std::str::from_utf8(&rest[..rest.len().min(4)])
                    .or_else(|error| std::str::from_utf8(&rest[..error.valid_up_to()]))
                    .ok()?
                    .chars()
                    .next()?;
                if matches!(character, '\'' | '\n') {
                    return None;
                }
                character.len_utf8()
            };
            (rest.get(length) == Some(&b'\'')).then_some(i + 1 + length + 1)
        }
        _ => None,
    }
}

/// Where a literal starting at `at` (outside any comment) ends, by the
/// language's rules; `None` when none starts there.
fn literal_end(text: &[u8], at: usize, language: Language) -> Option<usize> {
    match (language, text[at]) {
        (Language::Rust, b'"' | b'\'' | b'b' | b'c' | b'r') => rust_literal_end(text, at),
        (Language::Script, b'\'' | b'"') => Some(quoted_end(text, at, false)),
        (Language::Script, b'`') => Some(template_end(text, at)),
        (Language::Python, b'\'' | b'"') => Some(python_string_end(text, at)),
        _ => None,
    }
}

/// A comment the lexer is inside.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Comment {
    None,
    /// To the end of the line.
    Line,
    /// A block comment, nested this deep (Rust nests them).
    Block(usize),
}

/// Each line of `text` (as `split_inclusive` on `\n` gives them) as its
/// tokens, in order, before any trailing comma is dropped. A literal that
/// spans lines gives each line the part of it on that line, its line break
/// included.
fn lex(text: &[u8], language: Language) -> Vec<Vec<&[u8]>> {
    let count = text.split_inclusive(|byte| *byte == b'\n').count();
    let mut lines: Vec<Vec<&[u8]>> = vec![Vec::new(); count];
    let mut line = 0;
    // The token `text[start..end]`, split where it spans lines; an empty
    // one (no indentation) is kept.
    let mut push = |line: &mut usize, start: usize, end: usize| {
        if start == end {
            lines[*line].push(&text[start..end]);
        }
        let mut from = start;
        while from < end {
            let to = text[from..end]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(end, |found| from + found + 1);
            lines[*line].push(&text[from..to]);
            if text[to - 1] == b'\n' {
                *line += 1;
            }
            from = to;
        }
    };
    let indented = matches!(language, Language::Python | Language::Indented);
    let block_comments = matches!(language, Language::Rust | Language::Script);
    let mut comment = Comment::None;
    // Python's open brackets: inside them a line's indentation is layout.
    let mut brackets = 0usize;
    let mut at = 0;
    while at < text.len() {
        let line_start = at == 0 || text[at - 1] == b'\n';
        if line_start && indented && brackets == 0 {
            let end = at
                + text[at..]
                    .iter()
                    .take_while(|byte| indentation(**byte))
                    .count();
            if text
                .get(end)
                .is_some_and(|byte| !byte.is_ascii_whitespace())
            {
                push(&mut line, at, end);
            }
        }
        let byte = text[at];
        if byte == b'\n' {
            line += 1;
            if comment == Comment::Line {
                comment = Comment::None;
            }
            at += 1;
            continue;
        }
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let pair = &text[at..text.len().min(at + 2)];
        match comment {
            Comment::None => {
                if let Some(end) = literal_end(text, at, language) {
                    push(&mut line, at, end);
                    at = end;
                    continue;
                }
                if (block_comments && pair == b"//")
                    || (language == Language::Python && byte == b'#')
                {
                    comment = Comment::Line;
                } else if block_comments && pair == b"/*" {
                    comment = Comment::Block(1);
                    push(&mut line, at, at + 2);
                    at += 2;
                    continue;
                } else if language == Language::Python {
                    match byte {
                        b'(' | b'[' | b'{' => brackets += 1,
                        b')' | b']' | b'}' => brackets = brackets.saturating_sub(1),
                        _ => {}
                    }
                }
            }
            Comment::Block(depth)
                if pair == b"*/" || (language == Language::Rust && pair == b"/*") =>
            {
                comment = match (pair, depth) {
                    (b"*/", 1) => Comment::None,
                    (b"*/", _) => Comment::Block(depth - 1),
                    _ => Comment::Block(depth + 1),
                };
                push(&mut line, at, at + 2);
                at += 2;
                continue;
            }
            _ => {}
        }
        let start = at;
        if word(byte) && language != Language::Other {
            while at < text.len() && word(text[at]) {
                at += 1;
            }
        } else {
            at += 1;
        }
        push(&mut line, start, at);
    }
    lines
}

fn closing(token: &[u8]) -> bool {
    matches!(token, b")" | b"]" | b"}")
}

fn opening(token: &[u8]) -> bool {
    matches!(token, b"(" | b"[" | b"{")
}

/// Rust keywords after which `(` starts a tuple or a parenthesized
/// expression or pattern, not an argument or parameter list.
const TUPLE_AFTER: &[&[u8]] = &[
    b"break", b"if", b"in", b"let", b"match", b"mut", b"ref", b"return", b"while", b"yield",
];

/// Whether the Rust `(` at `at` in `flat` opens a tuple rather than the
/// arguments of a call, the parameters of a function or a macro's input:
/// it is not just after a name (other than a keyword in `TUPLE_AFTER` or a
/// lifetime), a generic list's `>`, a closing bracket, `!` or `?`.
fn opens_tuple(flat: &[&[u8]], at: usize) -> bool {
    let Some(before) = at.checked_sub(1).map(|at| flat[at]) else {
        return true;
    };
    let earlier = at.checked_sub(2).map(|at| flat[at]);
    if before.first().copied().is_some_and(word) {
        return TUPLE_AFTER.contains(&before) || earlier == Some(b"'");
    }
    match before {
        // `->` and `=>` end in `>` too.
        b">" => matches!(earlier, Some(b"-" | b"=")),
        b")" | b"]" | b"!" | b"?" => false,
        _ => true,
    }
}

/// Each line of a version as its tokens, with a Rust, TypeScript or
/// JavaScript file's trailing commas dropped: a comma whose next token, on
/// its line or a later one, is a closing bracket. A Rust one-element
/// tuple's comma, `(u32,)`, is kept: it is the only comma in a tuple's
/// parentheses, and without it the type or value is not a tuple.
fn version_tokens(text: &[u8], language: Language) -> Vec<Vec<&[u8]>> {
    let mut lines = lex(text, language);
    if !matches!(language, Language::Rust | Language::Script) {
        return lines;
    }
    let flat: Vec<&[u8]> = lines.concat();
    let mut dropped = vec![false; flat.len()];
    // Each open bracket's position and the commas directly inside it.
    let mut open: Vec<(usize, usize)> = Vec::new();
    for (at, token) in flat.iter().enumerate() {
        if opening(token) {
            open.push((at, 0));
        } else if *token == b"," {
            if let Some((_, commas)) = open.last_mut() {
                *commas += 1;
            }
        } else if closing(token) {
            let group = open.pop();
            if at == 0 || flat[at - 1] != b"," {
                continue;
            }
            let tuple = language == Language::Rust
                && *token == b")"
                && group.is_some_and(|(start, commas)| {
                    commas == 1 && flat[start] == b"(" && opens_tuple(&flat, start)
                });
            dropped[at - 1] = !tuple;
        }
    }
    let mut at = 0;
    for line in &mut lines {
        let mut kept = Vec::with_capacity(line.len());
        for token in line.iter() {
            if !dropped[at] {
                kept.push(*token);
            }
            at += 1;
        }
        *line = kept;
    }
    lines
}

/// `text`'s tokens, in order, as this module compares them: what a
/// formatter may change (spacing, line breaks, a trailing comma) left out,
/// and string literals kept whole, whitespace and all.
pub fn tokens(text: &[u8], language: Language) -> Vec<&[u8]> {
    version_tokens(text, language).concat()
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
    let (old_tokens, new_tokens) = (
        version_tokens(change.before, change.language),
        version_tokens(change.after, change.language),
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
        let removed = old_tokens.get(rows)?;
        let added = new_tokens.get(cols)?;
        spend(
            &mut left,
            removed.iter().chain(added).map(Vec::len).sum::<usize>(),
        )
        .ok()?;
        let parts = split(removed, added, &mut left).ok()?;
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

    /// The tokens of a one-line text.
    fn tokens(line: &[u8], language: Language) -> Vec<&[u8]> {
        lex(line, language).concat()
    }

    /// Whether `before` and `after` have the same tokens, in order.
    fn same(before: &str, after: &str, language: Language) -> bool {
        version_tokens(before.as_bytes(), language).concat()
            == version_tokens(after.as_bytes(), language).concat()
    }

    #[test]
    fn tokens_ignore_whitespace_but_not_where_words_meet() {
        let code = Language::Rust;
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

    /// A Rust one-element tuple's comma is not a trailing comma: `(u32,)`
    /// is a tuple and `(u32)` is not (review comment 4704372475). A single
    /// wrapped argument or parameter, `f(\n    x,\n)`, still is.
    #[test]
    fn a_rust_one_element_tuples_comma_is_a_token() {
        let rust = Language::Rust;
        for (tuple, scalar) in [
            ("pub fn f(x: (u32,))", "pub fn f(x: (u32))"),
            ("fn f() -> (u32,)", "fn f() -> (u32)"),
            ("let (a,) = (b,);", "let (a) = (b);"),
            ("return (x,)", "return (x)"),
            ("match x { _ => (y,) }", "match x { _ => (y) }"),
            ("fn f<'a>(x: &'a (u32,))", "fn f<'a>(x: &'a (u32))"),
            ("type T = ((u32,),);", "type T = ((u32),);"),
            ("type T = ((u32,),);", "type T = ((u32,));"),
        ] {
            assert!(!same(tuple, scalar, rust), "{tuple} and {scalar}");
        }
        for (wrapped, joined) in [
            ("f(\n    x,\n)\n", "f(x)\n"),
            (
                "pub fn f(\n    x: (u32,),\n) -> u32",
                "pub fn f(x: (u32,)) -> u32",
            ),
            ("f::<T>(\n    x,\n)", "f::<T>(x)"),
            ("g()(\n    x,\n)", "g()(x)"),
            ("m!(\n    x,\n)", "m!(x)"),
            ("(a, b,)", "(a, b)"),
            ("[x,]", "[x]"),
            ("S { x, }", "S { x }"),
        ] {
            assert!(same(wrapped, joined, rust), "{wrapped} and {joined}");
        }
        // TypeScript and JavaScript have no one-element tuple syntax.
        assert!(same("let a = (x,)", "let a = (x)", Language::Script));
    }

    #[test]
    fn a_files_language_is_its_name_or_extension() {
        for path in ["src/a.rs", "lib/b.RS"] {
            assert_eq!(language(path), Language::Rust, "{path}");
        }
        for path in ["web/App.TSX", "x/y.mjs", "lib.cts", "a.js", "a.ts"] {
            assert_eq!(language(path), Language::Script, "{path}");
        }
        for path in ["README.md", "docs/a.markdown", "page.mdx"] {
            assert_eq!(language(path), Language::Prose, "{path}");
        }
        for path in ["a.py", "pkg/types.pyi", "tool.PY"] {
            assert_eq!(language(path), Language::Python, "{path}");
        }
        for path in [
            "config.yaml",
            ".github/workflows/check.yml",
            "Makefile",
            "sub/makefile",
            "GNUmakefile",
            "rules.mk",
            "view.haml",
            "page.pug",
            "style.sass",
            "style.styl",
            "app.nim",
            "Program.fs",
        ] {
            assert_eq!(language(path), Language::Indented, "{path}");
        }
        for path in [
            "Cargo.toml",
            ".rs",
            "dir.rs/notes",
            "a.txt",
            ".yml",
            "Dockerfile",
        ] {
            assert_eq!(language(path), Language::Other, "{path}");
        }
    }

    #[test]
    fn a_rust_or_script_string_is_one_token_whitespace_and_all() {
        for language in [Language::Rust, Language::Script] {
            assert!(!same("let s = \"a b\";\n", "let s = \"a  b\";\n", language));
            assert!(!same("f(\"x\")\n", "f(\" x\")\n", language));
            // Spacing outside the string is still layout.
            assert!(same("let s=\"a b\";\n", "let s = \"a b\" ;\n", language));
            // An escaped quote does not end it.
            assert!(!same("\"a\\\" b\"\n", "\"a\\\"  b\"\n", language));
        }
        let rust = Language::Rust;
        // Raw, byte and character literals, and lifetimes that are not.
        assert!(!same("r#\"a \"b\"#\n", "r#\"a  \"b\"#\n", rust));
        assert!(!same("b\"a b\"\n", "b\"a  b\"\n", rust));
        assert!(!same("x(' ')\n", "x('  ')\n", rust));
        assert!(!same("x(b' ')\n", "x(b'\\t')\n", rust));
        assert!(same(
            "fn f<'a>(x: &'a str) -> &'a str {x}\n",
            "fn f<'a>(x: &'a str) -> &'a str {\n    x\n}\n",
            rust
        ));
        // A string that spans lines: re-indenting its second line changes it.
        assert!(!same(
            "let s = \"one\n  two\";\n",
            "let s = \"one\n    two\";\n",
            rust
        ));
        assert!(same(
            "let s = \"one\n  two\";\n",
            "let s =\n    \"one\n  two\";\n",
            rust
        ));
        // A quote in a comment opens no string.
        assert!(same(
            "// say \"hi\na(x,y);\n",
            "// say \"hi\na(x, y);\n",
            rust
        ));
        assert!(same(
            "/* it's /* nested */ \" */ a(x,y);\n",
            "/* it's /* nested */ \" */\na(x, y);\n",
            rust
        ));
        let script = Language::Script;
        // A template literal, with its substitutions, is one token.
        assert!(!same("`a ${f(`b c`)} d`\n", "`a ${f(`b  c`)} d`\n", script));
        assert!(!same("`one\n  two`\n", "`one\n    two`\n", script));
        // An unterminated quote ends at its line.
        assert!(same("x = 'a\ny(a,b)\n", "x = 'a\ny(a, b)\n", script));
    }

    #[test]
    fn python_indentation_is_content_but_not_inside_brackets() {
        let python = Language::Python;
        // A statement moved into an `if`.
        assert!(!same(
            "if x:\n    a()\nb()\n",
            "if x:\n    a()\n    b()\n",
            python
        ));
        // Two statements joined, and a block re-indented as a whole.
        assert!(!same("a()\nb()\n", "a() b()\n", python));
        assert!(!same("if x:\n  a()\n", "if x:\n    a()\n", python));
        // Spacing within a line, trailing whitespace and blank lines are
        // layout.
        assert!(same(
            "if x :\n    a( 1,2 )  \n\n\nb()\n",
            "if x:\n    a(1, 2)\nb()\n",
            python
        ));
        // A call re-wrapped inside its brackets keeps its tokens.
        assert!(same(
            "def run():\n    result = compute(first, second)\n    return result\n",
            "def run():\n    result = compute(\n        first, second\n    )\n    return result\n",
            python
        ));
        assert!(same(
            "x = [1,\n       2]\n",
            "x = [\n    1,\n    2]\n",
            python
        ));
        // A bracket in a string or a comment opens nothing.
        assert!(!same(
            "s = '('\nif x:\n    a()\n",
            "s = '('\nif x:\n        a()\n",
            python
        ));
        assert!(!same(
            "a()  # (\nif x:\n    b()\n",
            "a()  # (\nif x:\nb()\n",
            python
        ));
        // A string is one token, triple-quoted ones across lines.
        assert!(!same("s = 'a b'\n", "s = 'a  b'\n", python));
        assert!(!same(
            "s = \"\"\"one\n  two\"\"\"\n",
            "s = \"\"\"one\n    two\"\"\"\n",
            python
        ));
    }

    #[test]
    fn yaml_and_makefile_indentation_is_content() {
        let indented = Language::Indented;
        // A list moved under another key.
        assert!(!same(
            "defaults:\n  retries: 3\nports:\n  - 80\n",
            "defaults:\n  retries: 3\n  ports:\n    - 80\n",
            indented
        ));
        // A recipe line's tab.
        assert!(!same("all:\n\tcc a.c\n", "all:\ncc a.c\n", indented));
        assert!(!same("a: 1\nb: 2\n", "a: 1 b: 2\n", indented));
        // Spacing within a line, trailing whitespace and blank lines.
        assert!(same(
            "key:   value  \n\n- a\n",
            "key: value\n- a\n",
            indented
        ));
        // Words that come to touch differ, as they do not in other files.
        assert!(!same("key: a b\n", "key: ab\n", indented));
    }

    #[test]
    fn rust_char_literal_and_raw_string_edges_do_not_panic() {
        for text in [
            "'",
            "'\\",
            "'\\u{",
            "b'",
            "r#",
            "r#\"",
            "br##\"x\"#",
            "c\"",
            "'é",
            "'\u{1F600}'",
            "\"\\",
            "`${",
            "`${'",
            "`${`",
            "/*",
            "/* /*",
            "\"\"\"",
            "'''a",
        ] {
            for language in [
                Language::Rust,
                Language::Script,
                Language::Python,
                Language::Indented,
                Language::Prose,
                Language::Other,
            ] {
                let lines = lex(text.as_bytes(), language);
                assert_eq!(lines.len(), 1, "{text:?} {language:?}");
            }
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
                Language::Rust,
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
            Language::Rust,
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
        let code = credit(before, after, Language::Rust, true, &alice(before));
        assert!(
            code.sessions["alice"].added.len() == 6 && code.sessions.len() == 1,
            "{code:?}"
        );
        // Wrapped with a trailing comma whose closing bracket did not change.
        let wrapped = credit(
            "f(\n    a,\n    b\n)\n",
            "f(\n    a,\n    b,\n)\n",
            Language::Rust,
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
            Language::Rust,
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
        let alone = credit(before, after, Language::Rust, false, &own);
        assert!(alone.whole, "{alone:?}");
        // While another session's command ran: no session's.
        let overlapped = credit(before, after, Language::Rust, true, &own);
        assert!(
            overlapped.sessions.is_empty() && !overlapped.whole,
            "{overlapped:?}"
        );
        // Rewriting another session's uncommitted line: no session's.
        let theirs = credit(
            before,
            after,
            Language::Rust,
            false,
            &[("let total = 1;\n", session("bob"))],
        );
        assert!(theirs.sessions.is_empty() && !theirs.whole, "{theirs:?}");
        // A pure addition while nothing ran is the command's.
        let added = credit("a\n", "a\nb\n", Language::Rust, false, &[]);
        assert!(added.whole && added.sessions[ME].added == keys(&["b\n"]));
    }

    #[test]
    fn committed_lines_count_as_the_commands_only_when_nothing_overlapped() {
        let before = "fn a(x,y) {}\n";
        let after = "fn a(x, y) {}\n";
        assert!(credit(before, after, Language::Rust, false, &[]).whole);
        let overlapped = credit(before, after, Language::Rust, true, &[]);
        assert!(overlapped.sessions.is_empty() && !overlapped.whole);
    }

    #[test]
    fn a_group_of_several_sessions_or_unknown_lines_is_no_ones() {
        // Alice's line and Bob's joined into one.
        let joined = credit(
            "call(a,\nb);\n",
            "call(a, b);\n",
            Language::Rust,
            false,
            &[("call(a,\n", session("alice")), ("b);\n", session("bob"))],
        );
        assert!(joined.sessions.is_empty() && !joined.whole, "{joined:?}");
        let unknown = credit(
            "f(a,b);\n",
            "f(a, b);\n",
            Language::Rust,
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
            Language::Rust,
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
            Language::Rust,
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
            language: Language::Rust,
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
            language: Language::Rust,
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
            language: Language::Rust,
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
