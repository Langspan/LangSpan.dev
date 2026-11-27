//! Prompt analysis: what a hook takes from the owner's prompt (its summary,
//! line digests, candidate identifiers and qualifiers). Pure text handling.
use super::redact::redact;
use crate::sha256;
use crate::text::{SUMMARY_LIMIT, clean};

/// The marker of a prompt's own task line, compared without regard to case.
const TASK_MARKER: &str = "task:";

/// The owner's prompt in one line: its task line, the first of its leading
/// `MAX_PROMPT_LINES` non-empty lines that starts with `Task:`, without the
/// marker; otherwise its first non-empty line. Likely secrets redacted,
/// whitespace collapsed, control and invisible characters dropped, capped at
/// `SUMMARY_LIMIT` bytes. Redaction comes before the cap, so a cut never
/// leaves part of a secret behind. Other lines never leave the session,
/// except the one `prompt_line` picks when a live peer's prompt began the
/// same way. A slash command yields nothing.
pub fn prompt_summary(prompt: &str) -> Option<String> {
    PromptLines::new(prompt).summary()
}

/// Digests of the summaries of `prompt`'s leading non-empty lines, at most
/// `MAX_PROMPT_LINES`: what the coordinator compares with live peers'
/// prompts, never their words. Nothing for a slash command.
pub fn prompt_line_digests(prompt: &str) -> Vec<String> {
    PromptLines::new(prompt).digests()
}

/// The summary of `prompt`'s non-empty line at `index`, counting from zero.
pub fn prompt_line(prompt: &str, index: usize) -> Option<String> {
    PromptLines::new(prompt).0.into_iter().nth(index)
}

/// The summaries of a prompt's leading non-empty lines, each redacted,
/// collapsed and capped: at most `MAX_PROMPT_LINES` of them, and lines after
/// the first only within `PROMPT_SCAN_BYTES` of the prompt. None for a
/// prompt that starts with a slash command.
pub(crate) struct PromptLines(pub(crate) Vec<String>);

impl PromptLines {
    pub(crate) fn new(prompt: &str) -> Self {
        let mut scanned = 0;
        let lines: Vec<String> = prompt
            .lines()
            .take_while(|line| {
                let first = scanned == 0;
                scanned += line.len() + 1;
                first || scanned <= PROMPT_SCAN_BYTES
            })
            .filter(|line| !line.trim().is_empty())
            .take(langspan_core::MAX_PROMPT_LINES)
            .map(|line| {
                clean(
                    &redact(line)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    SUMMARY_LIMIT,
                )
            })
            .filter(|line| !line.is_empty())
            .collect();
        let command = lines.first().is_some_and(|line| {
            line.split(' ')
                .next()
                .is_some_and(|word| word.starts_with('/') && word.matches('/').count() == 1)
        });
        Self(if command { Vec::new() } else { lines })
    }

    /// The text of the first line marked `Task:`, without the marker.
    pub(crate) fn task_line(&self) -> Option<String> {
        self.0.iter().find_map(|line| {
            let head = line.get(..TASK_MARKER.len())?;
            let task = line[TASK_MARKER.len()..].trim_start();
            (head.eq_ignore_ascii_case(TASK_MARKER) && !task.is_empty()).then(|| task.to_owned())
        })
    }

    pub(crate) fn summary(&self) -> Option<String> {
        self.task_line().or_else(|| self.0.first().cloned())
    }

    pub(crate) fn digests(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|line| sha256::hex(&sha256::digest(line.as_bytes()))[..32].to_owned())
            .collect()
    }
}

/// Only this much of a prompt is searched for identifiers.
pub(crate) const PROMPT_SCAN_BYTES: usize = 16 * 1024;

/// Words that match the identifier grammar but are almost always prose or
/// keywords. Compared in lower case.
const STOP_WORDS: &[&str] = &[
    "about",
    "add",
    "after",
    "again",
    "all",
    "also",
    "and",
    "any",
    "are",
    "been",
    "before",
    "both",
    "bug",
    "but",
    "call",
    "can",
    "change",
    "check",
    "class",
    "code",
    "const",
    "continue",
    "could",
    "create",
    "def",
    "delete",
    "did",
    "does",
    "done",
    "each",
    "else",
    "error",
    "false",
    "file",
    "files",
    "first",
    "fix",
    "for",
    "from",
    "func",
    "function",
    "had",
    "has",
    "have",
    "here",
    "how",
    "impl",
    "implement",
    "into",
    "its",
    "just",
    "last",
    "let",
    "like",
    "line",
    "look",
    "make",
    "may",
    "method",
    "more",
    "move",
    "must",
    "mut",
    "need",
    "new",
    "next",
    "none",
    "not",
    "now",
    "null",
    "only",
    "other",
    "our",
    "out",
    "please",
    "pub",
    "refactor",
    "remove",
    "rename",
    "return",
    "run",
    "same",
    "see",
    "self",
    "should",
    "some",
    "struct",
    "test",
    "tests",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "true",
    "type",
    "update",
    "use",
    "using",
    "very",
    "want",
    "was",
    "way",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "why",
    "will",
    "with",
    "work",
    "would",
    "write",
    "yes",
    "you",
    "your",
];

/// Candidate identifiers in a prompt, for a source-index lookup: ASCII words
/// in the index's name grammar, common words skipped, distinct, at most
/// `CONTEXT_NAMES`. Code-shaped words (with an underscore, a digit, an inner
/// capital, a call, a qualified path or backticks) come first, then the
/// rest, each in prompt order. A word touching a non-ASCII letter is not a
/// candidate. Only the first `PROMPT_SCAN_BYTES` are read.
pub fn prompt_identifiers(prompt: &str) -> Vec<String> {
    let mut end = prompt.len().min(PROMPT_SCAN_BYTES);
    while !prompt.is_char_boundary(end) {
        end -= 1;
    }
    let text = &prompt[..end];
    let word_char = |c: char| c.is_alphanumeric() || c == '_';
    let mut code = Vec::new();
    let mut plain = Vec::new();
    let mut rest = text;
    while code.len() < langspan_core::CONTEXT_NAMES {
        let Some(start) = rest.find(word_char) else {
            break;
        };
        let before = rest[..start].chars().next_back();
        let tail = &rest[start..];
        let len = tail.find(|c| !word_char(c)).unwrap_or(tail.len());
        let word = &tail[..len];
        let after = &tail[len..];
        rest = after;
        if !langspan_core::is_graph_name(word)
            || STOP_WORDS.contains(&word.to_ascii_lowercase().as_str())
        {
            continue;
        }
        let shaped = word.bytes().any(|b| b == b'_' || b.is_ascii_digit())
            || word.chars().skip(1).any(|c| c.is_ascii_uppercase());
        let next_is_word = |skip: usize| after[skip..].starts_with(word_char);
        let joined = matches!(before, Some('`' | '.' | ':'))
            || after.starts_with(['`', '('])
            || (after.starts_with('.') && next_is_word(1))
            || (after.starts_with("::") && next_is_word(2));
        if code.iter().chain(&plain).any(|seen| seen == word) {
            continue;
        }
        if shaped || joined {
            code.push(word.to_owned());
        } else if plain.len() < langspan_core::CONTEXT_NAMES {
            plain.push(word.to_owned());
        }
    }
    code.extend(plain);
    code.truncate(langspan_core::CONTEXT_NAMES);
    code
}

/// Source file extensions a prompt's file name may end with.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java",
];

/// Paths and qualifiers the prompt names, which rank a name's definitions
/// (roadmap C2): a relative path (`src/store/upload.rs`), a source file
/// name (`upload.rs`), and the leading parts of a qualified name (`store`
/// in `store::upload` or `store.upload`). From the same bytes as
/// `prompt_identifiers`, distinct, at most `CONTEXT_NAMES`, each passing
/// `is_qualifier`. Used for one query only, never recorded.
pub fn prompt_qualifiers(prompt: &str) -> Vec<String> {
    let mut end = prompt.len().min(PROMPT_SCAN_BYTES);
    while !prompt.is_char_boundary(end) {
        end -= 1;
    }
    let part = |c: char| c.is_ascii_alphanumeric() || "_./:-".contains(c);
    let mut found: Vec<String> = Vec::new();
    let mut add = |qualifier: &str| {
        let plain = qualifier.bytes().all(|b| b.is_ascii_digit());
        if found.len() < langspan_core::CONTEXT_NAMES
            && !plain
            && !["self", "this", "super"].contains(&qualifier)
            && langspan_core::is_qualifier(qualifier)
            && !found.iter().any(|seen| seen == qualifier)
        {
            found.push(qualifier.to_owned());
        }
    };
    for token in prompt[..end].split(|c: char| !part(c)) {
        let token = token.trim_end_matches(|c: char| ".:-".contains(c));
        let token = token.strip_prefix("./").unwrap_or(token);
        let token = token.trim_start_matches([':', '-']);
        if token.contains('/') {
            add(token);
        } else if token.contains("::") {
            let parts: Vec<&str> = token.split("::").collect();
            parts[..parts.len() - 1].iter().for_each(|part| add(part));
        } else if let Some((head, extension)) = token.rsplit_once('.') {
            if SOURCE_EXTENSIONS.contains(&extension) {
                add(token);
            } else {
                head.split('.').for_each(&mut add);
            }
        }
    }
    found
}

/// The distinct files the code briefing found definitions in, in the order
/// found, at most `MAX_PROMPT_TARGETS`: a prompt's targets. Paths only.
pub fn prompt_target_paths(matches: &[langspan_core::NameContext]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for symbol in matches.iter().flat_map(|context| &context.definitions) {
        if !paths.contains(&symbol.path) {
            paths.push(symbol.path.clone());
        }
    }
    paths.truncate(langspan_core::MAX_PROMPT_TARGETS);
    paths
}
