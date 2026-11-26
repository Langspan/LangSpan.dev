//! Where in a file an edit falls, for region admission ([ADR 0012]).
//!
//! Pure: the caller reads the file once and passes its bytes; nothing here
//! touches the disk. Every answer is `None`, meaning "hold the whole file",
//! unless the edit can be placed exactly and every line it touches belongs to
//! a known definition (parsed languages) or the language is not parsed (line
//! ranges with a margin). Ambiguity always falls back to the whole file.
//!
//! [ADR 0012]: ../docs/adr/0012-region-admission.md

use crate::sha256;
use langspan_core::{MAX_KEY_DEPTH, MAX_REGIONS, PathRegions, Region};
use langspan_graph::{Language, Symbol};

/// Larger files are held whole: the parser's own bound.
pub const MAX_LOCATED_BYTES: usize = langspan_graph::MAX_SOURCE_BYTES;
/// Lines added on each side of a range in a file whose language is not parsed.
pub const LINE_MARGIN: u32 = 3;
/// Lines of a hunk or replacement times definitions examined, at most.
const MAX_WORK: usize = 4_000_000;

/// What a tool call will change in one file, as the host's input states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Claude Code string replacements, applied in order (`Edit`, `MultiEdit`).
    Replace(Vec<Replacement>),
    /// Codex `apply_patch` hunks of one `Update File` section.
    Hunks(Vec<Hunk>),
    /// Claude Code `Write`: the file's whole new content. Never placed in a
    /// region; its changed lines are found by comparison.
    Write(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pub old: String,
    pub new: String,
    pub replace_all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkLine {
    Context,
    Removed,
    Added,
}

/// One hunk: its lines in order, each with its kind and text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hunk {
    pub lines: Vec<(HunkLine, String)>,
}

/// The regions of `path` that `edit` touches in `bytes`, or `None` for the
/// whole file.
pub fn locate(path: &str, bytes: &[u8], edit: &Edit) -> Option<PathRegions> {
    if bytes.len() > MAX_LOCATED_BYTES {
        return None;
    }
    let content = std::str::from_utf8(bytes).ok()?;
    let ranges = match edit {
        Edit::Replace(replacements) => replacement_ranges(content, replacements)?,
        Edit::Hunks(hunks) => hunk_ranges(content, hunks)?,
        Edit::Write(_) => return None,
    };
    if ranges.is_empty() {
        return None;
    }
    let lines: Vec<&str> = file_lines(content);
    let regions = match Language::for_path(path) {
        Some(language) => {
            let graph = langspan_graph::extract(language, bytes).ok()?;
            // A file that did not parse cleanly may misplace definitions.
            if graph.partial {
                return None;
            }
            symbol_regions(&lines, &graph.symbols, &ranges)?
        }
        None => {
            let last = u32::try_from(lines.len().max(1)).ok()?;
            let mut regions: Vec<Region> = ranges
                .iter()
                .map(|&(start, end)| Region::Lines {
                    start_line: start.saturating_sub(LINE_MARGIN).max(1),
                    end_line: end.saturating_add(LINE_MARGIN).min(last).max(start),
                })
                .collect();
            regions.sort();
            regions.dedup();
            regions
        }
    };
    if regions.is_empty() || regions.len() > MAX_REGIONS {
        return None;
    }
    Some(PathRegions {
        path: path.into(),
        digest: sha256::hex(&sha256::digest(bytes)),
        regions,
    })
}

/// Lines as `apply_patch` sees them: split on `\n`, without the empty piece
/// after a final newline.
pub(crate) fn file_lines(content: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

fn line_of(content: &str, offset: usize) -> Option<u32> {
    u32::try_from(
        content.as_bytes()[..offset]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
            + 1,
    )
    .ok()
}

/// One-based inclusive line ranges, in the original content, of the text the
/// replacements remove. Each replacement must match exactly once in the text
/// as the earlier ones left it, and must not touch text an earlier one wrote:
/// otherwise its place in the original cannot be stated, and the answer is
/// the whole file. `replace_all` and an empty `old` are the whole file.
fn replacement_ranges(content: &str, replacements: &[Replacement]) -> Option<Vec<(u32, u32)>> {
    if replacements.is_empty() || replacements.len() > MAX_REGIONS {
        return None;
    }
    // Spans of text written by earlier replacements, in current offsets, and
    // the change in length each made.
    struct Written {
        start: usize,
        end: usize,
        delta: isize,
    }
    let mut current = content.to_owned();
    let mut written: Vec<Written> = Vec::new();
    let mut ranges = Vec::new();
    for replacement in replacements {
        let old = replacement.old.as_str();
        if replacement.replace_all || old.is_empty() {
            return None;
        }
        let mut found = current.match_indices(old);
        let (start, _) = found.next()?;
        if found.next().is_some() {
            return None;
        }
        let end = start + old.len();
        if written
            .iter()
            .any(|span| start <= span.end && end >= span.start)
        {
            return None;
        }
        let shift: isize = written
            .iter()
            .filter(|span| span.end <= start)
            .map(|span| span.delta)
            .sum();
        let original = usize::try_from(isize::try_from(start).ok()? - shift).ok()?;
        let first = line_of(content, original)?;
        let last = line_of(content, original + old.len() - 1)?;
        ranges.push((first, last));
        let delta =
            isize::try_from(replacement.new.len()).ok()? - isize::try_from(old.len()).ok()?;
        for span in written.iter_mut().filter(|span| span.start >= end) {
            span.start = usize::try_from(isize::try_from(span.start).ok()? + delta).ok()?;
            span.end = usize::try_from(isize::try_from(span.end).ok()? + delta).ok()?;
        }
        current.replace_range(start..end, &replacement.new);
        written.push(Written {
            start,
            end: start + replacement.new.len(),
            delta,
        });
    }
    Some(ranges)
}

/// A comparison deliberately looser than any `apply_patch` applies: ends
/// trimmed, and every character that is not an ASCII letter or digit
/// treated as the same character. A hunk that matches this way in exactly
/// one place can only be applied there.
pub(crate) fn loose(line: &str) -> String {
    line.trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// One-based inclusive line ranges of what each hunk changes: its removed
/// lines, and for an insertion the lines on either side of it. Context lines
/// are not part of the range: the host refuses a hunk whose context no longer
/// matches. Each hunk's context and removed lines must match exactly one place
/// in the file; a hunk with nothing to match is the whole file.
fn hunk_ranges(content: &str, hunks: &[Hunk]) -> Option<Vec<(u32, u32)>> {
    let mut ranges = Vec::new();
    for (hunk, (at, old_len)) in hunks.iter().zip(hunk_places(content, hunks)?) {
        // Positions, within the matched lines, that the hunk changes.
        let mut position = 0usize;
        let mut changed: Vec<usize> = Vec::new();
        for (kind, _) in &hunk.lines {
            match kind {
                HunkLine::Context => position += 1,
                HunkLine::Removed => {
                    changed.push(position);
                    position += 1;
                }
                HunkLine::Added => {
                    if position > 0 {
                        changed.push(position - 1);
                    }
                    changed.push(position);
                }
            }
        }
        let limit = old_len - 1;
        let (Some(&low), Some(&high)) = (changed.iter().min(), changed.iter().max()) else {
            return None;
        };
        let first = u32::try_from(at + low.min(limit) + 1).ok()?;
        let last = u32::try_from(at + high.min(limit) + 1).ok()?;
        ranges.push((first, last));
    }
    Some(ranges)
}

/// Where each hunk applies: the zero-based line its context and removed
/// lines start at, and how many lines they are, when they match exactly one
/// place in the file under `loose`. `None` if any hunk does not.
pub(crate) fn hunk_places(content: &str, hunks: &[Hunk]) -> Option<Vec<(usize, usize)>> {
    if hunks.is_empty() || hunks.len() > MAX_REGIONS {
        return None;
    }
    let file: Vec<String> = file_lines(content).iter().map(|line| loose(line)).collect();
    let mut places = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        let mut old: Vec<String> = hunk
            .lines
            .iter()
            .filter(|(kind, _)| *kind != HunkLine::Added)
            .map(|(_, text)| loose(text))
            .collect();
        // The host retries a hunk without its trailing empty lines; a match of
        // the shorter form is the one that must be unique.
        while old.last().is_some_and(String::is_empty) {
            old.pop();
        }
        if old.is_empty() || old.len() > file.len() {
            return None;
        }
        if file.len().checked_mul(old.len())? > MAX_WORK {
            return None;
        }
        let mut matches =
            (0..=file.len() - old.len()).filter(|&at| file[at..at + old.len()] == old[..]);
        let at = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        places.push((at, old.len()));
    }
    Some(places)
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Lines directly above a definition that belong to it: attributes,
/// decorators and comments, with no blank line between. A C# attribute
/// (`[Obsolete]`) is a whole line in brackets, so a statement such as
/// JavaScript's `[a, b] = f();` is not one.
fn leading(line: &str) -> bool {
    let line = line.trim();
    ["#", "@", "//", "/*", "*"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
        || (line.starts_with('[') && line.ends_with(']'))
}

struct Definition<'a> {
    symbol: &'a Symbol,
    /// First line including attributes, decorators and leading comments.
    start: u32,
}

impl Definition<'_> {
    fn contains(&self, line: u32) -> bool {
        self.start <= line && line <= self.symbol.end_line
    }
    fn span(&self) -> u32 {
        self.symbol.end_line - self.start
    }
}

/// The definitions the ranges fall in, keyed by name below the enclosing
/// definitions, so that two methods of one type are different symbols while
/// an edit anywhere in one function is the same symbol. A non-blank line in
/// no definition (an import, a top-level statement, an `impl` header) makes
/// the answer the whole file.
fn symbol_regions(
    lines: &[&str],
    symbols: &[Symbol],
    ranges: &[(u32, u32)],
) -> Option<Vec<Region>> {
    let text = |line: u32| lines.get(line.checked_sub(1)? as usize).copied();
    let definitions: Vec<Definition> = symbols
        .iter()
        .filter(|symbol| symbol.line >= 1 && symbol.line <= symbol.end_line)
        .map(|symbol| {
            let mut start = symbol.line;
            while start > 1
                && text(start - 1).is_some_and(|line| !is_blank(line) && leading(line))
                // A line of another definition, other than one enclosing this.
                && !symbols.iter().any(|other| {
                    other.line < start
                        && start - 1 <= other.end_line
                        && !(other.line <= symbol.line && symbol.end_line <= other.end_line)
                })
            {
                start -= 1;
            }
            Definition { symbol, start }
        })
        .collect();
    let touched: usize = ranges
        .iter()
        .map(|&(first, last)| (last - first + 1) as usize)
        .sum();
    if touched.checked_mul(definitions.len().max(1))? > MAX_WORK {
        return None;
    }
    let mut chosen: Vec<usize> = Vec::new();
    for &(first, last) in ranges {
        let mut keyed = false;
        for line in first..=last {
            // The innermost definition holding the line; ties by line, name.
            let innermost = definitions
                .iter()
                .enumerate()
                .filter(|(_, definition)| definition.contains(line))
                .min_by(|(_, a), (_, b)| {
                    (a.span(), std::cmp::Reverse(a.start), &a.symbol.name).cmp(&(
                        b.span(),
                        std::cmp::Reverse(b.start),
                        &b.symbol.name,
                    ))
                });
            match innermost {
                Some((index, _)) => {
                    keyed = true;
                    if !chosen.contains(&index) {
                        chosen.push(index);
                    }
                }
                None if text(line).is_none_or(is_blank) => {}
                None => return None,
            }
        }
        if !keyed {
            return None;
        }
    }
    let mut regions: Vec<Region> = chosen
        .into_iter()
        .map(|index| {
            let definition = &definitions[index];
            Region::Symbol {
                key: key(&definitions, definition),
                digest: symbol_digest(lines, definition),
                start_line: definition.start,
                end_line: definition.symbol.end_line,
            }
        })
        .collect();
    regions.sort();
    regions.dedup();
    Some(regions)
}

/// The definition's name below the names of the definitions enclosing it,
/// outermost first. A key cut at the depth bound is a prefix, so it can
/// only conflict more.
fn key(definitions: &[Definition], definition: &Definition) -> Vec<String> {
    let inner = definition.symbol;
    let mut enclosing: Vec<&Definition> = definitions
        .iter()
        .filter(|outer| {
            let outer = outer.symbol;
            outer.line <= inner.line
                && inner.end_line <= outer.end_line
                && outer.end_line - outer.line > inner.end_line - inner.line
        })
        .collect();
    enclosing.sort_by_key(|outer| std::cmp::Reverse(outer.symbol.end_line - outer.symbol.line));
    let mut key: Vec<String> = enclosing
        .iter()
        .map(|outer| outer.symbol.name.clone())
        .collect();
    key.push(inner.name.clone());
    key.truncate(MAX_KEY_DEPTH);
    key
}

/// SHA-256 of the definition's lines, each without trailing whitespace (and
/// so without a carriage return), joined by newlines.
fn symbol_digest(lines: &[&str], definition: &Definition) -> String {
    let first = definition.start as usize - 1;
    let last = (definition.symbol.end_line as usize).min(lines.len());
    let text = lines
        .get(first..last.max(first))
        .unwrap_or_default()
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    sha256::hex(&sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = "use std::fmt;

/// The first.
#[inline]
fn alpha() -> u32 {
    let one = 1;
    one
}

fn beta() -> u32 {
    2
}

struct Store;

impl Store {
    fn open(&self) -> u32 {
        3
    }

    fn close(&self) -> u32 {
        4
    }
}

fn outer() -> u32 {
    fn inner() -> u32 {
        5
    }
    inner()
}
";

    const PYTHON: &str = "import os


class Store:
    limit = 3

    def open(self):
        return 1

    @staticmethod
    def close():
        return 2


def helper():
    return os.sep
";

    fn replace(old: &str, new: &str) -> Edit {
        Edit::Replace(vec![Replacement {
            old: old.into(),
            new: new.into(),
            replace_all: false,
        }])
    }

    fn keys(located: &PathRegions) -> Vec<String> {
        located
            .regions
            .iter()
            .map(|region| match region {
                Region::Symbol { key, .. } => key.join("::"),
                Region::Lines {
                    start_line,
                    end_line,
                } => format!("{start_line}-{end_line}"),
            })
            .collect()
    }

    fn rust(edit: &Edit) -> Option<Vec<String>> {
        locate("src/lib.rs", RUST.as_bytes(), edit).map(|located| keys(&located))
    }

    #[test]
    fn a_claude_edit_is_the_definition_holding_its_unique_old_string() {
        assert_eq!(
            rust(&replace("    one\n", "    one + 1\n")).unwrap(),
            ["alpha"]
        );
        // A different line of the same function is the same symbol.
        assert_eq!(
            rust(&replace("let one = 1;", "let one = 2;")).unwrap(),
            ["alpha"]
        );
        // Its attributes and doc comment belong to it.
        assert_eq!(
            rust(&replace("#[inline]", "#[inline(always)]")).unwrap(),
            ["alpha"]
        );
        assert_eq!(rust(&replace("    2\n", "    20\n")).unwrap(), ["beta"]);
        // Across two functions: both, and the blank line between is no symbol.
        assert_eq!(
            rust(&replace("    one\n}\n\nfn beta", "    one\n}\n\nfn gamma")).unwrap(),
            ["alpha", "beta"]
        );
    }

    #[test]
    fn methods_and_nested_functions_are_keyed_below_their_enclosing_definitions() {
        // Methods of one impl are siblings (an impl block is not a symbol).
        assert_eq!(
            rust(&replace("        3\n", "        30\n")).unwrap(),
            ["open"]
        );
        assert_eq!(
            rust(&replace("        4\n", "        40\n")).unwrap(),
            ["close"]
        );
        assert_eq!(
            rust(&replace("        5\n", "        50\n")).unwrap(),
            ["outer::inner"]
        );
        assert_eq!(
            rust(&replace("    inner()\n", "    inner() + 1\n")).unwrap(),
            ["outer"]
        );
        let python =
            |edit: &Edit| locate("app.py", PYTHON.as_bytes(), edit).map(|located| keys(&located));
        assert_eq!(
            python(&replace("return 1", "return 10")).unwrap(),
            ["Store::open"]
        );
        assert_eq!(
            python(&replace("return 2", "return 20")).unwrap(),
            ["Store::close"]
        );
        // A decorator belongs to the method it decorates.
        assert_eq!(
            python(&replace("@staticmethod", "@classmethod")).unwrap(),
            ["Store::close"]
        );
        // A class attribute is the class itself, which encloses every method.
        assert_eq!(
            python(&replace("limit = 3", "limit = 4")).unwrap(),
            ["Store"]
        );
        assert_eq!(
            python(&replace("return os.sep", "return os.pathsep")).unwrap(),
            ["helper"]
        );
        // An import is outside every definition: the whole file.
        assert_eq!(python(&replace("import os", "import sys")), None);
    }

    #[test]
    fn a_java_method_holds_its_annotations_and_javadoc() {
        const JAVA: &str = "package shop;

public class Orders {
    /** Places one order. */
    @Override
    public Order place(Cart cart) {
        return null;
    }

    private int count() {
        return 0;
    }
}
";
        let java = |edit: &Edit| {
            locate("src/main/java/shop/Orders.java", JAVA.as_bytes(), edit)
                .map(|located| keys(&located))
        };
        assert_eq!(
            java(&replace("return null;", "return new Order();")).unwrap(),
            ["Orders::place"]
        );
        assert_eq!(
            java(&replace("@Override", "@Deprecated")).unwrap(),
            ["Orders::place"]
        );
        assert_eq!(
            java(&replace("Places one order.", "Places an order.")).unwrap(),
            ["Orders::place"]
        );
        assert_eq!(
            java(&replace("return 0;", "return 1;")).unwrap(),
            ["Orders::count"]
        );
        assert_eq!(java(&replace("package shop;", "package store;")), None);
    }

    #[test]
    fn a_csharp_method_holds_its_attributes_and_doc_comment() {
        const CSHARP: &str = "namespace Shop;

public class Orders
{
    /// <summary>Places one order.</summary>
    [Obsolete]
    public Order Place(Cart cart)
    {
        return null;
    }

    private int Count()
    {
        return 0;
    }
}
";
        let csharp = |edit: &Edit| {
            locate("src/Shop/Orders.cs", CSHARP.as_bytes(), edit).map(|located| keys(&located))
        };
        assert_eq!(
            csharp(&replace("return null;", "return new Order();")).unwrap(),
            ["Orders::Place"]
        );
        assert_eq!(
            csharp(&replace("[Obsolete]", "[Obsolete(\"no\")]")).unwrap(),
            ["Orders::Place"]
        );
        assert_eq!(
            csharp(&replace("Places one order.", "Places an order.")).unwrap(),
            ["Orders::Place"]
        );
        assert_eq!(
            csharp(&replace("return 0;", "return 1;")).unwrap(),
            ["Orders::Count"]
        );
        assert_eq!(
            csharp(&replace("namespace Shop;", "namespace Store;")),
            None
        );
    }

    #[test]
    fn a_bracketed_statement_above_a_function_is_not_its_attribute() {
        const SCRIPT: &str =
            "let a, b;\n[a, b] = pair();\nexport function run() {\n  return a;\n}\n";
        let located = locate(
            "web/run.ts",
            SCRIPT.as_bytes(),
            &replace("[a, b] = pair();", "[b, a] = pair();"),
        );
        assert_eq!(located, None, "a statement outside any definition");
    }

    #[test]
    fn ambiguity_or_an_edit_outside_any_definition_is_the_whole_file() {
        for edit in [
            replace("absent text", "x"),
            // `}` occurs many times.
            replace("}\n", "};\n"),
            replace("", "x"),
            Edit::Replace(vec![Replacement {
                old: "    2\n".into(),
                new: "    20\n".into(),
                replace_all: true,
            }]),
            replace("use std::fmt;", "use std::io;"),
            replace("impl Store {", "impl Store where Self: Sized {"),
        ] {
            assert_eq!(rust(&edit), None, "{edit:?}");
        }
        // A file that does not parse cleanly, and one too large to parse.
        let broken = format!("{RUST}\nfn broken( {{\n");
        assert_eq!(
            locate(
                "src/lib.rs",
                broken.as_bytes(),
                &replace("    2\n", "    3\n")
            ),
            None
        );
        let large = format!("{RUST}{}", " ".repeat(MAX_LOCATED_BYTES));
        assert_eq!(
            locate(
                "src/lib.rs",
                large.as_bytes(),
                &replace("    2\n", "    3\n")
            ),
            None
        );
        assert_eq!(locate("src/lib.rs", b"\xff\xfe", &replace("x", "y")), None);
    }

    #[test]
    fn a_multi_edit_is_the_union_and_any_ambiguity_is_the_whole_file() {
        let edits = |pairs: &[(&str, &str)]| {
            Edit::Replace(
                pairs
                    .iter()
                    .map(|(old, new)| Replacement {
                        old: (*old).into(),
                        new: (*new).into(),
                        replace_all: false,
                    })
                    .collect(),
            )
        };
        assert_eq!(
            rust(&edits(&[
                ("    2\n", "    20\n"),
                ("        4\n", "        40\n")
            ]))
            .unwrap(),
            ["beta", "close"]
        );
        // Matched in the text an earlier edit wrote: its original place is
        // not a line of the file, so the whole file.
        assert_eq!(
            rust(&edits(&[("    2\n", "    marker\n"), ("marker", "two")])),
            None
        );
        // An earlier edit makes a later one ambiguous.
        assert_eq!(
            rust(&edits(&[
                ("    2\n", "        4\n"),
                ("        4\n", "x\n")
            ])),
            None
        );
        // An earlier edit that changes lengths shifts nothing in the answer.
        assert_eq!(
            rust(&edits(&[
                ("    one\n", "    one\n    + 0\n    + 0\n"),
                ("        5\n", "        6\n")
            ]))
            .unwrap(),
            ["alpha", "outer::inner"]
        );
    }

    fn hunk(lines: &[(HunkLine, &str)]) -> Hunk {
        Hunk {
            lines: lines
                .iter()
                .map(|(kind, text)| (*kind, (*text).into()))
                .collect(),
        }
    }

    #[test]
    fn a_codex_hunk_is_located_by_its_context_and_removed_lines() {
        use HunkLine::*;
        let change = hunk(&[
            (Context, "fn beta() -> u32 {"),
            (Removed, "    2"),
            (Added, "    20"),
            (Context, "}"),
        ]);
        assert_eq!(rust(&Edit::Hunks(vec![change.clone()])).unwrap(), ["beta"]);
        // The context's own definition is not claimed: only what changes.
        let edge = hunk(&[
            (Context, "}"),
            (Context, ""),
            (Context, "fn beta() -> u32 {"),
            (Removed, "    2"),
            (Added, "    20"),
        ]);
        assert_eq!(rust(&Edit::Hunks(vec![edge])).unwrap(), ["beta"]);
        // An insertion is the lines on either side of it.
        let insert = hunk(&[
            (Context, "    let one = 1;"),
            (Added, "    let two = 2;"),
            (Context, "    one"),
        ]);
        assert_eq!(rust(&Edit::Hunks(vec![insert])).unwrap(), ["alpha"]);
        // Two hunks: their union.
        let close = hunk(&[(Removed, "        4"), (Added, "        40")]);
        assert_eq!(
            rust(&Edit::Hunks(vec![change, close])).unwrap(),
            ["beta", "close"]
        );
        // Whitespace and punctuation variants still count as the same line.
        let loose = hunk(&[
            (Context, "fn beta() -> u32 {   "),
            (Removed, "\t2"),
            (Added, "    20"),
        ]);
        assert_eq!(rust(&Edit::Hunks(vec![loose])).unwrap(), ["beta"]);
    }

    #[test]
    fn an_ambiguous_missing_or_context_free_hunk_is_the_whole_file() {
        use HunkLine::*;
        for hunks in [
            vec![hunk(&[(Removed, "}"), (Added, "};")])],
            vec![hunk(&[(Removed, "    absent"), (Added, "x")])],
            vec![hunk(&[(Added, "fn appended() {}")])],
            vec![hunk(&[(Context, "use std::fmt;"), (Added, "use std::io;")])],
            vec![],
        ] {
            assert_eq!(rust(&Edit::Hunks(hunks.clone())), None, "{hunks:?}");
        }
    }

    #[test]
    fn an_unparsed_language_gets_line_ranges_with_a_margin_and_the_file_digest() {
        let text = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
        let located = locate("notes.txt", text.as_bytes(), &replace("line 10\n", "ten\n")).unwrap();
        assert_eq!(
            located.digest,
            sha256::hex(&sha256::digest(text.as_bytes()))
        );
        assert_eq!(
            located.regions,
            [Region::Lines {
                start_line: 7,
                end_line: 13
            }]
        );
        let edge = locate("notes.txt", text.as_bytes(), &replace("line 1\n", "one\n")).unwrap();
        assert_eq!(
            edge.regions,
            [Region::Lines {
                start_line: 1,
                end_line: 4
            }]
        );
        let end = locate(
            "notes.txt",
            text.as_bytes(),
            &replace("line 20\n", "twenty\n"),
        )
        .unwrap();
        assert_eq!(
            end.regions,
            [Region::Lines {
                start_line: 17,
                end_line: 20
            }]
        );
    }

    #[test]
    fn a_symbol_digest_ignores_line_endings_and_trailing_whitespace_but_not_content() {
        let digest = |source: &str| match &locate(
            "src/lib.rs",
            source.as_bytes(),
            &replace("    2", "    3"),
        )
        .unwrap()
        .regions[0]
        {
            Region::Symbol { digest, .. } => digest.clone(),
            other => panic!("{other:?}"),
        };
        let plain = "fn beta() -> u32 {\n    2\n}\n";
        assert_eq!(
            digest(plain),
            digest("fn beta() -> u32 {  \r\n    2\r\n}\r\n")
        );
        assert_ne!(
            digest(plain),
            digest("fn beta() -> u32 {\n    2 \n    + 1\n}\n")
        );
        // Another definition's change leaves this one's digest as it was.
        assert_eq!(
            digest(plain),
            digest("fn alpha() {}\n\nfn beta() -> u32 {\n    2\n}\n")
        );
    }
}
