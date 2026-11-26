//! The file format of an exported source index (roadmap D5, ADR 0008).
//!
//! JSON Lines: one header line, then one line per indexed file. An import
//! reads a file someone else may have written, so every line is parsed into
//! typed structures within the same bounds the extractor keeps, and nothing
//! in it is trusted until the importer has compared its digest with the
//! file on disk. No I/O here beyond reading lines from a reader it is given.

use langspan_graph::{
    MAX_CALLS, MAX_IMPORTS, MAX_REFERENCES, MAX_SYMBOLS, NAME_BYTES, SIGNATURE_BYTES, SymbolKind,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io::BufRead};

/// The `format` of every header this build writes and reads.
pub const FORMAT: &str = "langspan-source-index";
/// The largest file an import reads, and an export writes.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// The longest line read. A file at every parse bound, with JSON escapes,
/// fits well inside it; a longer line is skipped without being held.
pub const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
/// A repository-relative path longer than this is malformed.
const PATH_BYTES: usize = 4096;
/// A signature or import target cut at its bound ends with this.
const CUT: &str = "…";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub format: String,
    /// The index schema the rows were written in. Only the same schema is
    /// read.
    pub schema: i64,
    /// The last component of the exporting workspace's root, for the owner
    /// to recognize. Never used to decide anything.
    pub workspace_root: String,
    /// Wall-clock milliseconds since the Unix epoch, for the owner to read.
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    /// Repository-relative, `/`-separated, in a supported language.
    pub path: String,
    /// SHA-256 of the exact content the rows were extracted from.
    pub digest: String,
    pub partial: bool,
    pub symbols: Vec<SymbolRow>,
    pub refs: Vec<NameLine>,
    pub imports: Vec<ImportRow>,
    pub calls: Vec<CallRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolRow {
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub end_line: u32,
    pub signature: String,
    pub exported: bool,
    pub body_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameLine {
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRow {
    pub target: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRow {
    /// The innermost definition the call is in, if any.
    pub caller: Option<String>,
    pub callee: String,
    pub line: u32,
    pub args: Option<u32>,
}

/// Why a file line was not used. Both are counted; neither stops the import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// More rows, or a longer name, signature or line, than the extractor
    /// would ever produce.
    OverLimits(String),
    /// Not a file record this build understands.
    Malformed(String),
}

/// A header, or the reason the whole import is refused.
pub fn parse_header(line: &[u8], schema: i64) -> Result<Header, String> {
    let header: Header = serde_json::from_slice(line)
        .map_err(|error| format!("the first line is not an index export header: {error}"))?;
    if header.format != FORMAT {
        return Err(format!(
            "the header's format is {:?}, not {FORMAT:?}",
            truncate(&header.format)
        ));
    }
    if header.schema != schema {
        return Err(format!(
            "the export is of index schema {}, and this build reads schema {schema}; export it again with this build",
            header.schema
        ));
    }
    if header.workspace_root.len() > PATH_BYTES
        || header.workspace_root.chars().any(char::is_control)
    {
        return Err("the header's workspace_root is not a plain name".into());
    }
    Ok(header)
}

/// One file line, checked against every parse bound and field rule.
pub fn parse_record(line: &[u8]) -> Result<FileRecord, Skip> {
    let record: FileRecord = serde_json::from_slice(line)
        .map_err(|error| Skip::Malformed(format!("not a file record: {error}")))?;
    let over = |what: &str, count: usize, max: usize| {
        (count > max).then(|| Skip::OverLimits(format!("{count} {what}, more than {max}")))
    };
    if let Some(skip) = over("symbols", record.symbols.len(), MAX_SYMBOLS)
        .or_else(|| over("references", record.refs.len(), MAX_REFERENCES))
        .or_else(|| over("imports", record.imports.len(), MAX_IMPORTS))
        .or_else(|| over("calls", record.calls.len(), MAX_CALLS))
    {
        return Err(skip);
    }
    if !valid_path(&record.path) {
        return Err(Skip::Malformed(
            "the path is not a relative path to a supported source file".into(),
        ));
    }
    if !is_digest(&record.digest) {
        return Err(Skip::Malformed("the digest is not a SHA-256 in hex".into()));
    }
    let names = record
        .symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .chain(record.refs.iter().map(|reference| reference.name.as_str()))
        .chain(record.calls.iter().map(|call| call.callee.as_str()))
        .chain(
            record
                .calls
                .iter()
                .filter_map(|call| call.caller.as_deref()),
        );
    for name in names {
        if name.len() > NAME_BYTES {
            return Err(Skip::OverLimits(format!(
                "a name of {} bytes, more than {NAME_BYTES}",
                name.len()
            )));
        }
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(Skip::Malformed(
                "a name is empty or has control characters".into(),
            ));
        }
    }
    let texts = record
        .symbols
        .iter()
        .map(|symbol| symbol.signature.as_str())
        .chain(record.imports.iter().map(|import| import.target.as_str()));
    for text in texts {
        if !within_signature_bound(text) {
            return Err(Skip::OverLimits(format!(
                "a signature or import of {} bytes, more than {SIGNATURE_BYTES}",
                text.len()
            )));
        }
        if text.chars().any(char::is_control) {
            return Err(Skip::Malformed(
                "a signature or import has control characters".into(),
            ));
        }
    }
    for symbol in &record.symbols {
        if SymbolKind::parse(&symbol.kind).is_none() {
            return Err(Skip::Malformed(format!(
                "unknown symbol kind {:?}",
                truncate(&symbol.kind)
            )));
        }
        if symbol.line == 0 || symbol.end_line < symbol.line {
            return Err(Skip::Malformed("a symbol's lines are out of order".into()));
        }
        if !is_digest(&symbol.body_digest) {
            return Err(Skip::Malformed(
                "a symbol's body digest is not a SHA-256 in hex".into(),
            ));
        }
    }
    let lines = record
        .refs
        .iter()
        .map(|reference| reference.line)
        .chain(record.imports.iter().map(|import| import.line))
        .chain(record.calls.iter().map(|call| call.line));
    if lines.into_iter().any(|line| line == 0) {
        return Err(Skip::Malformed("a line number is 0".into()));
    }
    Ok(record)
}

/// What the extractor can write: at most `SIGNATURE_BYTES`, or a text cut
/// there that ends with `…`.
fn within_signature_bound(text: &str) -> bool {
    text.len() <= SIGNATURE_BYTES
        || (text.len() <= SIGNATURE_BYTES + CUT.len() && text.ends_with(CUT))
}

/// A path the observer could have listed: relative, `/`-separated, with no
/// empty, `.` or `..` component, and in a language the index parses.
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= PATH_BYTES
        && !path.chars().any(|c| c.is_control() || c == '\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && langspan_graph::Language::for_path(path).is_some()
}

fn is_digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn truncate(text: &str) -> &str {
    let mut end = text.len().min(64);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// One line read by `read_line`.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// In the buffer, without its newline.
    Read,
    /// Longer than the bound: consumed up to its newline and discarded.
    TooLong,
    End,
}

/// Read one line of at most `max` bytes into `buffer`, never holding more.
pub fn read_line(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Line> {
    buffer.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        any = true;
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(at) => (&available[..at], Some(at + 1)),
            None => (available, None),
        };
        if !too_long {
            if buffer.len() + chunk.len() > max {
                too_long = true;
                buffer.clear();
            } else {
                buffer.extend_from_slice(chunk);
            }
        }
        let used = done.unwrap_or(available.len());
        reader.consume(used);
        if done.is_some() {
            break;
        }
    }
    Ok(if !any {
        Line::End
    } else if too_long {
        Line::TooLong
    } else {
        Line::Read
    })
}

/// Paths already seen in one import, so a second line for a path is
/// malformed instead of replacing the first.
#[derive(Default)]
pub struct Seen(BTreeSet<String>);

impl Seen {
    /// True the first time `path` is offered.
    pub fn first(&mut self, path: &str) -> bool {
        self.0.insert(path.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn record() -> FileRecord {
        FileRecord {
            path: "src/api.rs".into(),
            digest: D.into(),
            partial: false,
            symbols: vec![SymbolRow {
                name: "upload".into(),
                kind: "function".into(),
                line: 1,
                end_line: 3,
                signature: "pub fn upload(path: &str) -> bool".into(),
                exported: true,
                body_digest: D.into(),
            }],
            refs: vec![NameLine {
                name: "open".into(),
                line: 2,
            }],
            imports: vec![ImportRow {
                target: "std::fs".into(),
                line: 1,
            }],
            calls: vec![CallRow {
                caller: Some("upload".into()),
                callee: "open".into(),
                line: 2,
                args: Some(1),
            }],
        }
    }

    fn line(record: &FileRecord) -> Vec<u8> {
        serde_json::to_vec(record).unwrap()
    }

    #[test]
    fn a_record_at_every_bound_is_read_and_one_past_any_is_over_limits() {
        let mut at = record();
        at.symbols = vec![at.symbols[0].clone(); MAX_SYMBOLS];
        at.refs = vec![at.refs[0].clone(); MAX_REFERENCES];
        at.imports = vec![at.imports[0].clone(); MAX_IMPORTS];
        at.calls = vec![at.calls[0].clone(); MAX_CALLS];
        at.symbols[0].name = "n".repeat(NAME_BYTES);
        at.symbols[0].signature = format!("{}{CUT}", "s".repeat(SIGNATURE_BYTES));
        let bytes = line(&at);
        assert!(bytes.len() < MAX_LINE_BYTES);
        assert_eq!(parse_record(&bytes), Ok(at.clone()));
        let past: [fn(&mut FileRecord); 6] = [
            |r| r.symbols.push(r.symbols[1].clone()),
            |r| r.refs.push(r.refs[0].clone()),
            |r| r.imports.push(r.imports[0].clone()),
            |r| r.calls.push(r.calls[0].clone()),
            |r| r.symbols[0].name.push('n'),
            |r| r.symbols[0].signature = "s".repeat(SIGNATURE_BYTES + 1),
        ];
        for change in past {
            let mut over = at.clone();
            change(&mut over);
            assert!(
                matches!(parse_record(&line(&over)), Err(Skip::OverLimits(_))),
                "past a bound"
            );
        }
    }

    #[test]
    fn paths_digests_kinds_and_lines_are_checked() {
        let bad: [fn(&mut FileRecord); 11] = [
            |r| r.path = "/etc/passwd.rs".into(),
            |r| r.path = "src/../../x.rs".into(),
            |r| r.path = "src//x.rs".into(),
            |r| r.path = "README.md".into(),
            |r| r.path = "src/a\nb.rs".into(),
            |r| r.digest = D.to_uppercase(),
            |r| r.symbols[0].kind = "macro".into(),
            |r| r.symbols[0].end_line = 0,
            |r| r.symbols[0].body_digest = "x".into(),
            |r| r.refs[0].name = String::new(),
            |r| r.calls[0].line = 0,
        ];
        for change in bad {
            let mut record = record();
            change(&mut record);
            assert!(
                matches!(parse_record(&line(&record)), Err(Skip::Malformed(_))),
                "{record:?}"
            );
        }
        let unknown =
            String::from_utf8(line(&record()))
                .unwrap()
                .replacen('{', r#"{"content":"secret","#, 1);
        assert!(matches!(
            parse_record(unknown.as_bytes()),
            Err(Skip::Malformed(_))
        ));
    }

    #[test]
    fn a_header_of_another_format_or_schema_is_refused_with_the_reason() {
        let header = |format: &str, schema: i64| {
            serde_json::to_vec(&Header {
                format: format.into(),
                schema,
                workspace_root: "app".into(),
                created_at: 1,
            })
            .unwrap()
        };
        assert!(parse_header(&header(FORMAT, 4), 4).is_ok());
        assert!(
            parse_header(&header("other", 4), 4)
                .unwrap_err()
                .contains("format")
        );
        assert!(
            parse_header(&header(FORMAT, 3), 4)
                .unwrap_err()
                .contains("schema 3")
        );
        assert!(parse_header(b"not json", 4).is_err());
        assert!(parse_header(&line(&record()), 4).is_err());
    }

    #[test]
    fn a_long_line_is_discarded_without_being_held_and_the_next_is_read() {
        let text = format!("{}\nshort\nlast", "x".repeat(100));
        let mut reader = std::io::BufReader::with_capacity(7, text.as_bytes());
        let mut buffer = Vec::new();
        assert_eq!(
            read_line(&mut reader, &mut buffer, 10).unwrap(),
            Line::TooLong
        );
        assert!(buffer.len() <= 10);
        assert_eq!(read_line(&mut reader, &mut buffer, 10).unwrap(), Line::Read);
        assert_eq!(buffer, b"short");
        assert_eq!(read_line(&mut reader, &mut buffer, 10).unwrap(), Line::Read);
        assert_eq!(buffer, b"last");
        assert_eq!(read_line(&mut reader, &mut buffer, 10).unwrap(), Line::End);
    }

    /// A deterministic generator, so a failure names its seed.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, below: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % below.max(1)
        }
    }

    /// Property: any bytes, including valid records with random edits,
    /// truncations and splices, parse to a record within every bound or
    /// are refused; nothing panics, and a record that parses serializes
    /// back to a line that parses to the same record.
    #[test]
    fn any_bytes_parse_within_bounds_or_are_refused() {
        let base = line(&record());
        let alphabet = b"{}[]\":,0123456789-.eE truefalsnul\\u\x00\xff\n/abc";
        for seed in 0..20_000u64 {
            let mut random = Lcg(seed);
            let mut bytes = base.clone();
            for _ in 0..1 + random.next(4) {
                match random.next(4) {
                    0 | 3 if bytes.is_empty() => {}
                    0 => {
                        let at = random.next(bytes.len());
                        bytes[at] = alphabet[random.next(alphabet.len())];
                    }
                    1 => bytes.truncate(random.next(bytes.len() + 1)),
                    2 => {
                        let at = random.next(bytes.len() + 1);
                        let from = random.next(base.len());
                        let to = from + random.next(base.len() - from);
                        bytes.splice(at..at, base[from..to].iter().copied());
                    }
                    _ => {
                        let at = random.next(bytes.len());
                        bytes.remove(at);
                    }
                }
            }
            match parse_record(&bytes) {
                Ok(parsed) => {
                    assert!(valid_path(&parsed.path), "seed {seed}");
                    assert!(parsed.symbols.len() <= MAX_SYMBOLS, "seed {seed}");
                    assert_eq!(parse_record(&line(&parsed)), Ok(parsed), "seed {seed}");
                }
                Err(Skip::Malformed(_) | Skip::OverLimits(_)) => {}
            }
            let _ = parse_header(&bytes, 4);
        }
    }
}
