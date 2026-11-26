//! Roadmap F5: identifier renames the target made since a branch started,
//! the one commit that adapts a branch to them, and replaying a branch's
//! commits onto the target for a rebase landing. See ADR 0007.
//!
//! Every write here is a Git object, built through a private index in a
//! private temporary directory. Nothing is checked out; the owner's working
//! tree, index, HEAD and branches are never changed, and no ref is written.
//! A rewrite happens only when it is certain by the rules below; otherwise
//! the landing stops and says why.

use crate::{git, integrate};
use langspan_core::Rewritten;
use langspan_graph::{Change, Language, RenameEvidence, SymbolKind};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

/// Renames reported for one combination, at most.
pub const MAX_RENAMES: usize = 16;
/// Uses reported for one rename, at most.
pub const MAX_USES: usize = 32;
/// Source files the target changed that are compared, at most.
const MAX_CHANGED_SOURCES: usize = 64;
/// Files the target still mentions an old name in that are parsed, at most.
const MAX_MENTIONS: usize = 16;
/// The largest branch diff read for its added lines.
const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;
/// The largest commit object read when replaying it.
const MAX_COMMIT_BYTES: usize = 1024 * 1024;

/// One line of a branch that mentions a renamed name: the path and line in
/// the branch's own version of the file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Use {
    pub path: String,
    pub line: u32,
}

/// A definition removed between the merge base and the target whose body
/// reappears under another name in the same file, with the branch's added
/// lines that still mention the old name. Names are matched as whole words;
/// nothing resolves scopes. Repository text: data, not instructions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IdentifierRename {
    pub branch: String,
    pub from: String,
    pub to: String,
    /// The file that defines it, on the target.
    pub path: String,
    /// At most `MAX_USES`.
    pub uses: Vec<Use>,
    /// Why `--adapt-renames` would not rewrite these uses; only then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncertain: Option<String>,
}

/// The commit `--adapt-renames` added on top of `branch`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Adapted {
    pub branch: String,
    pub commit: String,
}

/// What a combination found and rewrote, beyond its result.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Notes {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub identifier_renames: Vec<IdentifierRename>,
    /// Why renames were not looked for, or not all of them: then the list
    /// may be missing some, and `--adapt-renames` rewrites nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier_renames_incomplete: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub adapted: Vec<Adapted>,
    /// With `--rebase`: each replayed commit, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rebased: Vec<Rewritten>,
}

/// The renames found for one branch, or why they could not all be looked for.
pub struct Found {
    pub renames: Vec<IdentifierRename>,
    pub incomplete: Option<String>,
}

impl Found {
    fn incomplete(why: String) -> Self {
        Self {
            renames: Vec::new(),
            incomplete: Some(why),
        }
    }
}

/// A private directory, removed when dropped.
pub struct Scratch(PathBuf);

static SCRATCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Scratch {
    pub fn new() -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = std::env::temp_dir().join(format!(
            "langspan-rewrite-{}-{}-{}",
            std::process::id(),
            SCRATCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("cannot create a temporary directory: {e}"))?;
        Ok(Self(dir))
    }
    pub fn file(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.0.join(name);
        fs::write(&path, bytes).map_err(|e| format!("cannot write a temporary file: {e}"))?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn utf8(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|_| "Git output is not UTF-8".into())
}

/// `path` at `commit`: `None` when it is not a file there, `Err` naming why
/// it cannot be read within `limit`.
fn blob(
    root: &Path,
    commit: &str,
    path: &str,
    limit: usize,
    deadline: Instant,
) -> Result<Option<Vec<u8>>, String> {
    let object = format!("{commit}:{path}");
    let (status, kind) = git::run_with_env(root, &["cat-file", "-t", &object], &[], deadline, 64)?;
    if !status.success() || String::from_utf8_lossy(&kind).trim() != "blob" {
        return Ok(None);
    }
    let size = utf8(git::output(
        root,
        &["cat-file", "-s", &object],
        deadline,
        64,
    )?)?;
    if size
        .trim()
        .parse::<usize>()
        .map_or(true, |size| size > limit)
    {
        return Err(format!("{path:?} is larger than {limit} bytes"));
    }
    git::output(root, &["cat-file", "blob", &object], deadline, limit).map(Some)
}

/// The structure of `path` at `commit`, with body digests. `Ok(None)` when
/// it is not a file there; `Err` says why it could not be read completely.
fn graph(
    root: &Path,
    commit: &str,
    path: &str,
    language: Language,
    deadline: Instant,
) -> Result<Option<langspan_graph::FileGraph>, String> {
    let Some(bytes) = blob(
        root,
        commit,
        path,
        langspan_graph::MAX_SOURCE_BYTES,
        deadline,
    )?
    else {
        return Ok(None);
    };
    let mut graph = langspan_graph::extract(language, &bytes)?;
    if graph.partial {
        return Err(format!("{path:?} did not parse completely at {commit}"));
    }
    langspan_graph::digest_blocks(&mut graph, &bytes, |block| {
        crate::sha256::hex(&crate::sha256::digest(block))
    });
    Ok(Some(graph))
}

/// Whether `text` holds `name` as a whole word: not next to a letter, digit
/// or underscore.
fn mentions(text: &str, name: &str) -> bool {
    word_matches(text, name).next().is_some()
}

fn word_matches<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name).filter_map(move |(at, _)| {
        let end = at + name.len();
        (!text[..at].ends_with(word) && !text[end..].starts_with(word)).then_some(at)
    })
}

/// `line` with each whole-word `from` replaced by `to`.
fn replace_words(line: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for at in word_matches(line, from) {
        out.push_str(&line[last..at]);
        out.push_str(to);
        last = at + from.len();
    }
    out.push_str(&line[last..]);
    out
}

/// One line the branch added: its path and line in the branch's version.
struct Added {
    path: String,
    line: u32,
    text: String,
}

/// The lines the branch added between `base` and `branch`, from Git's diff
/// with no context. `Err` when the diff cannot be read completely.
fn added_lines(
    root: &Path,
    base: &str,
    branch: &str,
    deadline: Instant,
) -> Result<Vec<Added>, String> {
    let bytes = git::output(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "-U0",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            base,
            branch,
        ],
        deadline,
        MAX_DIFF_BYTES,
    )
    .map_err(|_| {
        format!("the branch's changes could not be read (larger than {MAX_DIFF_BYTES} bytes, or Git failed)")
    })?;
    parse_added(&String::from_utf8_lossy(&bytes))
}

/// The added lines of a `git diff -U0` with `a/` and `b/` prefixes. Inside a
/// hunk, its header's line counts say which lines are content, so an added
/// line that itself starts with `++ ` is not read as a file header.
fn parse_added(diff: &str) -> Result<Vec<Added>, String> {
    const UNREADABLE: &str = "Git's diff of the branch was not readable";
    let range = |text: &str| -> Option<(u32, u32)> {
        let (start, count) = text.split_once(',').unwrap_or((text, "1"));
        Some((start.parse().ok()?, count.parse().ok()?))
    };
    let mut added = Vec::new();
    let mut path = None::<String>;
    // The next new line's number, and the old and new lines left in the hunk.
    let (mut next, mut old_left, mut new_left) = (0_u32, 0_u32, 0_u32);
    for line in diff.lines() {
        if old_left > 0 || new_left > 0 {
            if let Some(text) = line.strip_prefix('+') {
                new_left = new_left.checked_sub(1).ok_or(UNREADABLE)?;
                if let Some(path) = &path {
                    added.push(Added {
                        path: path.clone(),
                        line: next,
                        text: text.to_owned(),
                    });
                }
                next += 1;
            } else if line.starts_with('-') {
                old_left = old_left.checked_sub(1).ok_or(UNREADABLE)?;
            } else if !line.starts_with('\\') {
                return Err(UNREADABLE.into());
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            path = match rest.strip_prefix("b/") {
                Some(name) => Some(name.to_owned()),
                None if rest == "/dev/null" => None,
                None => return Err(format!("a changed path could not be read: {rest}")),
            };
        } else if line.starts_with("diff --git ") {
            path = None;
        } else if let Some(header) = line.strip_prefix("@@ -") {
            let mut ranges = header.split(' ');
            let (Some(old), Some(new)) = (
                ranges.next().and_then(range),
                ranges.next().and_then(|new| range(new.strip_prefix('+')?)),
            ) else {
                return Err(UNREADABLE.into());
            };
            (next, old_left, new_left) = (new.0, old.1, new.1);
        }
        // Anything else outside a hunk ("index", "---", "Binary files",
        // "\ No newline at end of file") says nothing about added lines.
    }
    Ok(added)
}

/// The renames the target made between `base` and `target` that `branch`'s
/// added lines (between `base` and `branch`) still mention. See ADR 0007.
pub fn find(
    root: &Path,
    base: &str,
    target: &str,
    branch_name: &str,
    branch: &str,
    deadline: Instant,
) -> Result<Found, String> {
    let listing = utf8(git::output(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--name-only",
            "-z",
            "--no-renames",
            base,
            target,
        ],
        deadline,
        1024 * 1024,
    )?)?;
    let sources: Vec<(&str, Language)> = listing
        .split('\0')
        .filter_map(|path| Language::for_path(path).map(|language| (path, language)))
        .collect();
    if sources.len() > MAX_CHANGED_SOURCES {
        return Ok(Found::incomplete(format!(
            "the target changed more than {MAX_CHANGED_SOURCES} source files since {branch_name} started; renames were not looked for"
        )));
    }
    // Each definition removed whose body reappears under a new name.
    let mut candidates = Vec::new();
    for (path, language) in sources {
        let (before, after) = match (
            graph(root, base, path, language, deadline),
            graph(root, target, path, language, deadline),
        ) {
            (Ok(Some(before)), Ok(Some(after))) => (before, after),
            (Ok(_), Ok(_)) => continue,
            (Err(why), _) | (_, Err(why)) => {
                return Ok(Found::incomplete(format!(
                    "{why}; renames were not looked for"
                )));
            }
        };
        for change in langspan_graph::diff(&before, &after) {
            if let Change::Renamed {
                to,
                evidence: RenameEvidence::SameBody,
                ..
            } = change.change
            {
                let digest = before
                    .symbols
                    .iter()
                    .find(|symbol| symbol.name == change.name && symbol.kind == change.kind)
                    .and_then(|symbol| symbol.block_digest.clone());
                candidates.push((
                    change.name,
                    change.kind,
                    to,
                    path.to_owned(),
                    language,
                    digest,
                ));
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Found {
            renames: Vec::new(),
            incomplete: None,
        });
    }
    let added = match added_lines(root, base, branch, deadline) {
        Ok(added) => added,
        Err(why) => {
            return Ok(Found::incomplete(format!(
                "{why}; renames were not looked for"
            )));
        }
    };
    let mut renames = Vec::new();
    let mut incomplete = None;
    for (from, kind, to, path, language, digest) in candidates {
        let mut uses: Vec<Use> = added
            .iter()
            .filter(|line| mentions(&line.text, &from))
            .map(|line| Use {
                path: line.path.clone(),
                line: line.line,
            })
            .collect();
        if uses.is_empty() {
            continue;
        }
        if renames.len() == MAX_RENAMES {
            incomplete = Some(format!(
                "more than {MAX_RENAMES} renames on the target are still used on {branch_name}; only the first {MAX_RENAMES} are listed"
            ));
            break;
        }
        let total = uses.len();
        uses.truncate(MAX_USES);
        let mut rename = IdentifierRename {
            branch: branch_name.to_owned(),
            from,
            to,
            path,
            uses,
            uncertain: None,
        };
        rename.uncertain = uncertain(
            root,
            target,
            branch,
            &rename,
            (kind, language, digest.as_deref()),
            total,
            &added,
            deadline,
        )?;
        renames.push(rename);
    }
    Ok(Found {
        renames,
        incomplete,
    })
}

/// Why rewriting `rename`'s uses would not be certain, if it would not be.
#[allow(clippy::too_many_arguments)]
fn uncertain(
    root: &Path,
    target: &str,
    branch: &str,
    rename: &IdentifierRename,
    (kind, language, digest): (SymbolKind, Language, Option<&str>),
    total: usize,
    added: &[Added],
    deadline: Instant,
) -> Result<Option<String>, String> {
    let (from, to) = (&rename.from, &rename.to);
    if total > MAX_USES {
        return Ok(Some(format!(
            "this branch adds {total} lines that mention {from}, more than the {MAX_USES} that are listed"
        )));
    }
    if let Some(line) = added.iter().find(|line| mentions(&line.text, to)) {
        return Ok(Some(format!(
            "this branch already mentions {to} itself ({}:{}), so which {to} a rewritten use would reach is not certain",
            line.path, line.line
        )));
    }
    // The branch must leave the old definition as it found it.
    let same = match graph(root, branch, &rename.path, language, deadline) {
        Ok(Some(graph)) => graph.symbols.iter().any(|symbol| {
            symbol.name == *from && symbol.kind == kind && symbol.block_digest.as_deref() == digest
        }),
        Ok(None) => false,
        Err(why) => return Ok(Some(why)),
    };
    if !same {
        return Ok(Some(format!(
            "this branch changes or moves {from}'s own definition in {}",
            rename.path
        )));
    }
    // No use may be a definition of the old name the branch adds.
    let mut paths: Vec<&str> = rename.uses.iter().map(|u| u.path.as_str()).collect();
    paths.dedup();
    for path in paths {
        let Some(language) = Language::for_path(path) else {
            continue;
        };
        match graph(root, branch, path, language, deadline) {
            Ok(Some(graph)) => {
                if let Some(symbol) = graph.symbols.iter().find(|symbol| {
                    symbol.name == *from
                        && rename
                            .uses
                            .iter()
                            .any(|u| u.path == path && u.line == symbol.line)
                }) {
                    return Ok(Some(format!(
                        "this branch defines its own {from} at {path}:{}",
                        symbol.line
                    )));
                }
            }
            Ok(None) => {}
            Err(why) => return Ok(Some(why)),
        }
    }
    // The target must not still define the old name anywhere.
    let (status, listing) = git::run_with_env(
        root,
        &[
            "grep", "-l", "-z", "-w", "-F", "-I", "-e", from, target, "--",
        ],
        &[],
        deadline,
        1024 * 1024,
    )?;
    if !matches!(status.code(), Some(0 | 1)) {
        return Ok(Some(format!("the target could not be searched for {from}")));
    }
    let listing = String::from_utf8_lossy(&listing);
    let prefix = format!("{target}:");
    let mentioned: Vec<(&str, Language)> = listing
        .split('\0')
        .filter_map(|entry| entry.strip_prefix(&prefix))
        .filter_map(|path| Language::for_path(path).map(|language| (path, language)))
        .collect();
    if mentioned.len() > MAX_MENTIONS {
        return Ok(Some(format!(
            "the target still mentions {from} in more than {MAX_MENTIONS} source files"
        )));
    }
    for (path, language) in mentioned {
        match graph(root, target, path, language, deadline) {
            Ok(Some(graph)) if graph.symbols.iter().any(|symbol| symbol.name == *from) => {
                return Ok(Some(format!("the target still defines {from} in {path}")));
            }
            Ok(_) => {}
            Err(why) => return Ok(Some(why)),
        }
    }
    Ok(None)
}

/// Why `--adapt-renames` cannot rewrite `found`'s uses, if it cannot.
pub fn refusal(found: &Found) -> Option<String> {
    const HAND: &str = "nothing was rewritten or pushed; change these uses by hand on the branch, or land without --adapt-renames";
    if let Some(why) = &found.incomplete {
        return Some(format!(
            "not every rename on the target could be looked for ({why}); {HAND}"
        ));
    }
    found.renames.iter().find_map(|rename| {
        rename.uncertain.as_ref().map(|why| {
            format!(
                "the target renamed {} to {}, but {why}; {HAND}",
                rename.from, rename.to
            )
        })
    })
}

/// Write one commit on top of `branch` that replaces each listed use of
/// every rename's `from` with its `to`, as a whole word, on exactly the
/// listed lines. `Err` when a listed line no longer holds the old name.
pub fn adapt(
    root: &Path,
    branch: &str,
    renames: &[IdentifierRename],
    deadline: Instant,
) -> Result<String, String> {
    let scratch = Scratch::new()?;
    // A private index: the owner's is never read into or written.
    let index = scratch.0.join("index");
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    git::output_with_env(root, &["read-tree", branch], &env, deadline, 1024)?;
    let mut paths: Vec<&str> = renames
        .iter()
        .flat_map(|rename| rename.uses.iter().map(|u| u.path.as_str()))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    for (number, path) in paths.iter().enumerate() {
        let bytes = blob(root, branch, path, 1024 * 1024, deadline)?
            .ok_or_else(|| format!("{path:?} is not a file on the branch"))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{path:?} is not UTF-8 text, so it is not rewritten"))?;
        let mut out = String::with_capacity(text.len());
        for (index, line) in text.split_inclusive('\n').enumerate() {
            let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let mut line = line.to_owned();
            for rename in renames {
                if rename
                    .uses
                    .iter()
                    .any(|u| u.path == *path && u.line == number)
                {
                    let rewritten = replace_words(&line, &rename.from, &rename.to);
                    if rewritten == line {
                        return Err(format!(
                            "{path}:{number} no longer mentions {}; nothing was rewritten",
                            rename.from
                        ));
                    }
                    line = rewritten;
                }
            }
            out.push_str(&line);
        }
        let file = scratch.file(&format!("blob-{number}"), out.as_bytes())?;
        let oid = utf8(git::output(
            root,
            &[
                "hash-object",
                "-w",
                "--no-filters",
                "--",
                file.to_str().ok_or("temporary path is not UTF-8")?,
            ],
            deadline,
            128,
        )?)?;
        let entry = utf8(git::output(
            root,
            &["ls-tree", "-z", branch, "--", path],
            deadline,
            4096,
        )?)?;
        let mode = entry
            .split(' ')
            .next()
            .filter(|mode| matches!(*mode, "100644" | "100755"))
            .ok_or_else(|| format!("{path:?} is not a regular file on the branch"))?;
        git::output_with_env(
            root,
            &[
                "update-index",
                "--cacheinfo",
                &format!("{mode},{},{path}", oid.trim()),
            ],
            &env,
            deadline,
            1024,
        )?;
    }
    let tree = utf8(git::output_with_env(
        root,
        &["write-tree"],
        &env,
        deadline,
        128,
    )?)?;
    let mut message = String::from("Use the target's new names\n\n");
    for rename in renames {
        message.push_str(&format!(
            "{} is now {} ({}): {} use(s) rewritten.\n",
            rename.from,
            rename.to,
            rename.path,
            rename.uses.len()
        ));
    }
    message.push_str("\nWritten by langspan land --adapt-renames.\n");
    let file = scratch.file("message", message.as_bytes())?;
    integrate::new_commit(root, tree.trim(), branch, &file, &[], deadline)
}

/// A commit's author and message, for replaying it.
pub struct Original {
    pub author: [(&'static str, String); 3],
    pub message: Vec<u8>,
}

/// `commit`'s author and message, or why it is not replayed.
pub fn original(root: &Path, commit: &str, deadline: Instant) -> Result<Original, String> {
    let raw = git::output(
        root,
        &["cat-file", "commit", commit],
        deadline,
        MAX_COMMIT_BYTES,
    )?;
    let split = raw
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .ok_or("a commit without a message")?;
    let headers = String::from_utf8_lossy(&raw[..split]);
    let mut author = None;
    for header in headers.lines() {
        if let Some(rest) = header.strip_prefix("author ") {
            let close = rest.rfind('>').ok_or("an unreadable author")?;
            let open = rest[..close].rfind('<').ok_or("an unreadable author")?;
            author = Some([
                ("GIT_AUTHOR_NAME", rest[..open].trim_end().to_owned()),
                ("GIT_AUTHOR_EMAIL", rest[open + 1..close].to_owned()),
                ("GIT_AUTHOR_DATE", rest[close + 1..].trim().to_owned()),
            ]);
        } else if let Some(encoding) = header.strip_prefix("encoding ")
            && !encoding.eq_ignore_ascii_case("utf-8")
        {
            return Err(format!(
                "commit {commit} is encoded as {encoding:?}, which a rebase landing does not replay"
            ));
        }
    }
    Ok(Original {
        author: author.ok_or("a commit without an author")?,
        message: raw[split + 2..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_added_line_that_looks_like_a_header_is_still_an_added_line() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,0 +2,2 @@\n+++ fetch_user()\n+fetch_user()\n\\ No newline at end of file\ndiff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-fetch_user()\n";
        let added = parse_added(diff).unwrap();
        let found: Vec<(&str, u32, &str)> = added
            .iter()
            .map(|line| (line.path.as_str(), line.line, line.text.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("src/a.rs", 2, "++ fetch_user()"),
                ("src/a.rs", 3, "fetch_user()")
            ]
        );
    }

    #[test]
    fn replacing_words_leaves_longer_names_alone() {
        assert_eq!(
            replace_words(
                "fetch_user(fetch_users, fetch_user2) + fetch_user",
                "fetch_user",
                "load_user"
            ),
            "load_user(fetch_users, fetch_user2) + load_user"
        );
        assert!(!mentions("prefetch_user()", "fetch_user"));
        assert!(mentions("x.fetch_user()", "fetch_user"));
    }
}
