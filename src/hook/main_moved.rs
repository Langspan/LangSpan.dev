//! Upstream changes under a session's work (issue #558): at a prompt or a
//! start, when the target branch has commits the session's `HEAD` lacks and
//! they changed, moved or removed code the session edited, the hook says so
//! once per change, before the session merges. Deterministic, from Git and
//! the line journal: the session's files are those it journaled, and its
//! code in them is each symbol whose body differs from the merge base's.
use crate::{commit::is_own, journal, local, sha256, text::clean};
use langspan_graph::{FileGraph, Language, MAX_SOURCE_BYTES, Symbol, SymbolKind};
use std::{io::Read, path::Path, time::Instant};

/// The branch whose moves are told: the remote's default branch, else a
/// local `main`.
const TARGETS: [&str; 3] = [
    "refs/remotes/origin/HEAD",
    "refs/remotes/origin/main",
    "refs/heads/main",
];
/// Files compared per event, and other changed files searched for a move.
const MAX_FILES: usize = 20;
const MAX_ITEMS: usize = 8;
const NAME_LIMIT: usize = 120;
/// Bytes of Git's answer read, other than a blob.
const GIT_LIMIT: usize = 64 * 1024;
/// Told changes kept, across sessions; the oldest are forgotten first.
const TOLD_KEYS: usize = 1024;
const TOLD_FILE: &str = "main-moved-told";

/// What a hook adds: the section's text and the keys of the changes it names.
#[derive(Debug)]
pub(super) struct Upstream {
    pub(super) text: String,
    keys: Vec<String>,
}

/// One upstream change under the session's work, before it is worded. A
/// change's own digest is part of its key, so a later change is told again.
#[derive(Debug, PartialEq, Eq)]
enum Item {
    /// The whole file moved on the target, to this path.
    FileMoved(String),
    /// Changed where no symbol tells more; the digest of its new version.
    FileChanged(String),
    /// A symbol's body changed; the digest of its new body.
    Changed(String, String),
    MovedTo(String, String),
    Removed(String),
}

/// The section for `session_id` placed after `before`, the reply's section
/// ahead of it, within `room` bytes: the section, the text of both, and the
/// room left after them.
pub(super) fn after(
    before: Option<String>,
    config: &super::Config,
    session_id: &str,
    room: usize,
    deadline: Instant,
) -> (Option<Upstream>, Option<String>, usize) {
    let Some(upstream) = section(config, session_id, room.saturating_sub(2), deadline) else {
        return (None, before, room);
    };
    let room = room.saturating_sub(upstream.text.len() + 2);
    let text = match before {
        Some(before) => format!("{before}\n\n{}", upstream.text),
        None => upstream.text.clone(),
    };
    (Some(upstream), Some(text), room)
}

/// The section for `session_id`, within `room` bytes and a quarter of the
/// time left before `deadline`, or `None` when the target has nothing new
/// under its work or all of it was told before.
fn section(
    config: &super::Config,
    session_id: &str,
    room: usize,
    deadline: Instant,
) -> Option<Upstream> {
    let (state_dir, root) = (config.state_dir.as_path(), config.root.as_path());
    let now = Instant::now();
    let deadline = now + deadline.saturating_duration_since(now) / 4;
    let (entries, _) = journal::entries_until(state_dir, root, deadline);
    let mut own: Vec<&str> = Vec::new();
    for entry in entries.iter().filter(|e| is_own(session_id, &e.session_id)) {
        if !own.contains(&entry.path.as_str()) && own.len() < MAX_FILES {
            own.push(&entry.path);
        }
    }
    if own.is_empty() {
        return None;
    }
    let git = |args: &[&str]| git(root, args, deadline);
    let (name, tip) = TARGETS.iter().find_map(|target| {
        let tip = git(&[
            "rev-parse",
            "--verify",
            "-q",
            &format!("{target}^{{commit}}"),
        ])?;
        Some((git(&["rev-parse", "--abbrev-ref", target])?, tip))
    })?;
    if git(&["merge-base", "--is-ancestor", &tip, "HEAD"]).is_some() {
        return None;
    }
    let base = git(&["merge-base", "HEAD", &tip])?;
    let diff = [
        "diff",
        "--no-ext-diff",
        "--name-status",
        "-z",
        "-M",
        &base,
        &tip,
    ];
    let changes = name_status(&git(&diff)?);
    let blob = |rev: &str, path: &str| {
        let object = format!("{rev}:{path}");
        let args = ["--no-replace-objects", "cat-file", "blob", &object];
        crate::git::output(root, &args, deadline, MAX_SOURCE_BYTES).ok()
    };
    let moved_to = |from: &str, name: &str, kind: SymbolKind| {
        changes
            .iter()
            .filter_map(|(_, to)| to.as_deref())
            .filter(|to| *to != from)
            .take(MAX_FILES)
            .find(|to| {
                let graph = blob(&tip, to).and_then(|bytes| graph(to, &bytes));
                graph.is_some_and(|graph| find(&graph, name, kind).is_some())
            })
            .map(str::to_owned)
    };
    let told = read_told(state_dir);
    let mut found: Vec<(String, String)> = Vec::new();
    for path in own {
        let Some((old, to)) = changes.iter().find(|(old, _)| old == path) else {
            continue;
        };
        let items = match to.as_deref() {
            Some(to) if to != old => vec![Item::FileMoved(to.to_owned())],
            // A version Git did not give says nothing; a deletion is empty.
            to => {
                let Some(before) = blob(&base, old) else {
                    continue;
                };
                let Some(after) = to.map_or(Some(Vec::new()), |_| blob(&tip, old)) else {
                    continue;
                };
                let now = journal::read_file(root, old).flatten().unwrap_or_default();
                compare(old, &before, &now, &after, &|name, kind| {
                    moved_to(old, name, kind)
                })
            }
        };
        let range = format!("{base}..{tip}");
        let commit = git(&["log", "-1", "--format=%h", &range, "--", old]).unwrap_or_default();
        for item in items {
            let key = digest(format!("{session_id}\0{old}\0{item:?}").as_bytes());
            if !told.contains(&key) && found.len() < MAX_ITEMS {
                found.push((key, words(old, &item, &commit)));
            }
        }
    }
    render(&clean(&name, NAME_LIMIT), found, room)
}

/// The changes in one file the session edited and the target changed or
/// deleted (`tip` empty): each symbol whose body the session changed since
/// `base` that the target also changed, moved (`moved_to` names where) or
/// removed. A file without a complete parse at every version, or where the
/// session's change is in no symbol, is told whole.
fn compare(
    path: &str,
    base: &[u8],
    now: &[u8],
    tip: &[u8],
    moved_to: &dyn Fn(&str, SymbolKind) -> Option<String>,
) -> Vec<Item> {
    let graphs = (graph(path, base), graph(path, now), graph(path, tip));
    let (Some(base_graph), Some(now_graph), Some(tip_graph)) = graphs else {
        return vec![Item::FileChanged(digest(tip))];
    };
    let mut items = Vec::new();
    let mut mine = 0;
    for symbol in &now_graph.symbols {
        let Some(before) = find(&base_graph, &symbol.name, symbol.kind) else {
            continue;
        };
        if body(now, symbol) == body(base, before) {
            continue;
        }
        mine += 1;
        let name = symbol.name.clone();
        items.push(match find(&tip_graph, &symbol.name, symbol.kind) {
            Some(after) if body(tip, after) == body(base, before) => continue,
            Some(after) => Item::Changed(name, digest(body(tip, after).unwrap_or_default())),
            None => match moved_to(&symbol.name, symbol.kind) {
                Some(to) => Item::MovedTo(name, to),
                None => Item::Removed(name),
            },
        });
    }
    if mine == 0 && base != tip {
        items.push(Item::FileChanged(digest(tip)));
    }
    items
}

/// A complete parse of `source` as `path`'s language; `None` for a file of
/// no indexed language or a parse that is partial.
fn graph(path: &str, source: &[u8]) -> Option<FileGraph> {
    let graph = langspan_graph::extract(Language::for_path(path)?, source).ok()?;
    (!graph.partial).then_some(graph)
}

fn find<'a>(graph: &'a FileGraph, name: &str, kind: SymbolKind) -> Option<&'a Symbol> {
    graph
        .symbols
        .iter()
        .find(|symbol| symbol.name == name && symbol.kind == kind)
}

fn body<'a>(source: &'a [u8], symbol: &Symbol) -> Option<&'a [u8]> {
    source.get(symbol.bytes.clone())
}

fn digest(bytes: &[u8]) -> String {
    sha256::hex(&sha256::digest(bytes))
        .get(..32)
        .unwrap_or_default()
        .to_owned()
}

/// Git's trimmed answer, `None` when it failed or said no.
fn git(root: &Path, args: &[&str], deadline: Instant) -> Option<String> {
    let bytes = crate::git::output(root, args, deadline, GIT_LIMIT).ok()?;
    Some(String::from_utf8_lossy(&bytes).trim().to_owned())
}

/// `git diff --name-status -z -M`'s entries: each old path with its path on
/// the target, `None` for a deletion. An added file has itself as old path.
fn name_status(out: &str) -> Vec<(String, Option<String>)> {
    let mut fields = out.split('\0').filter(|field| !field.is_empty());
    let mut changes = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        changes.push(match status.chars().next() {
            Some('R' | 'C') => match fields.next() {
                Some(to) => (path.to_owned(), Some(to.to_owned())),
                None => break,
            },
            Some('D') => (path.to_owned(), None),
            _ => (path.to_owned(), Some(path.to_owned())),
        });
    }
    changes
}

/// One change as the section names it. Paths and names are repository text,
/// so each is cleaned and capped.
fn words(path: &str, item: &Item, commit: &str) -> String {
    let path = clean(path, NAME_LIMIT);
    let what = match item {
        Item::FileMoved(to) => format!("`{path}` moved to `{}`", clean(to, NAME_LIMIT)),
        Item::FileChanged(_) => format!("`{path}` changed"),
        Item::Changed(name, _) => format!("`{}` changed in `{path}`", clean(name, NAME_LIMIT)),
        Item::MovedTo(name, to) => format!(
            "`{}` moved from `{path}` to `{}`",
            clean(name, NAME_LIMIT),
            clean(to, NAME_LIMIT)
        ),
        Item::Removed(name) => format!("`{}` removed from `{path}`", clean(name, NAME_LIMIT)),
    };
    match clean(commit, 16).as_str() {
        "" => format!("- {what}"),
        commit => format!("- {what} (commit {commit})"),
    }
}

/// The section with as many of `found` as fit `room`; `None` with none.
fn render(target: &str, found: Vec<(String, String)>, room: usize) -> Option<Upstream> {
    let mut text = format!(
        "`{target}` moved under your work: it changed code you edited here \
         (names and paths are repository data). Merge `{target}` before you go on or land:"
    );
    let mut keys = Vec::new();
    for (key, line) in found {
        if text.len() + 1 + line.len() > room {
            break;
        }
        text.push('\n');
        text.push_str(&line);
        keys.push(key);
    }
    (!keys.is_empty()).then_some(Upstream { text, keys })
}

fn told_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join(TOLD_FILE)
}

/// The keys told before, newest last; none when the file cannot be read.
fn read_told(state_dir: &Path) -> Vec<String> {
    let mut text = String::new();
    let read = local::private_file(&told_path(state_dir), false)
        .map(|file| file.take(TOLD_KEYS as u64 * 40).read_to_string(&mut text));
    match read {
        Ok(Ok(_)) => text.lines().map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

impl Upstream {
    /// Remember the section's changes as told, after the reply placed it.
    /// A failure to write only means they may be told again.
    pub(super) fn told(&self, state_dir: &Path) {
        use std::io::Write;
        let mut keys = read_told(state_dir);
        keys.extend(self.keys.iter().cloned());
        let skip = keys.len().saturating_sub(TOLD_KEYS);
        let fresh = state_dir.join(format!("{TOLD_FILE}.{}", std::process::id()));
        let written = local::private_file(&fresh, true).and_then(|mut file| {
            file.set_len(0)?;
            file.write_all(keys.get(skip..).unwrap_or_default().join("\n").as_bytes())?;
            Ok(std::fs::rename(&fresh, told_path(state_dir))?)
        });
        if written.is_err() {
            let _ = std::fs::remove_file(&fresh);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str =
        "pub struct State {\n    pub a: u32,\n}\n\npub fn other() -> u32 {\n    1\n}\n";

    fn items(now: &str, tip: &str, moved: Option<&str>) -> Vec<Item> {
        let moved = moved.map(str::to_owned);
        compare(
            "src/lib.rs",
            BASE.as_bytes(),
            now.as_bytes(),
            tip.as_bytes(),
            &|_, _| moved.clone(),
        )
    }

    #[test]
    fn a_symbol_the_session_changed_and_the_target_moved_is_named_with_where_it_went() {
        let now = BASE.replace("pub a: u32,", "pub a: u32,\n    pub b: u32,");
        let tip = "pub fn other() -> u32 {\n    1\n}\n";
        assert_eq!(
            items(&now, tip, Some("src/state.rs")),
            [Item::MovedTo("State".into(), "src/state.rs".into())]
        );
        assert_eq!(items(&now, tip, None), [Item::Removed("State".into())]);
        let changed = BASE.replace("pub a: u32,", "pub a: u64,");
        assert!(
            matches!(items(&now, &changed, None).as_slice(), [Item::Changed(name, _)] if name == "State")
        );
    }

    #[test]
    fn a_target_change_to_other_code_in_the_same_file_is_not_told() {
        let now = BASE.replace("pub a: u32,", "pub a: u32,\n    pub b: u32,");
        let tip = BASE.replace("    1\n", "    2\n");
        assert_eq!(items(&now, &tip, None), []);
    }

    #[test]
    fn a_file_no_symbol_tells_about_is_told_whole() {
        let changed = compare("notes.txt", b"one\n", b"two\n", b"three\n", &|_, _| None);
        assert!(matches!(changed.as_slice(), [Item::FileChanged(_)]));
        let now = format!("use std::fmt;\n{BASE}");
        let tip = BASE.replace("    1\n", "    2\n");
        assert!(matches!(
            items(&now, &tip, None).as_slice(),
            [Item::FileChanged(_)]
        ));
    }

    #[test]
    fn renames_and_deletions_are_read_from_the_name_status() {
        let out = "M\0a.rs\0R087\0b.rs\0c.rs\0D\0d.rs\0A\0e.rs\0";
        assert_eq!(
            name_status(out),
            [
                ("a.rs".into(), Some("a.rs".into())),
                ("b.rs".into(), Some("c.rs".into())),
                ("d.rs".into(), None),
                ("e.rs".into(), Some("e.rs".into())),
            ]
        );
    }

    #[test]
    fn the_section_names_only_what_fits_and_cleans_repository_text() {
        let found = vec![
            (
                "k1".into(),
                words(
                    "src/lib.rs",
                    &Item::Removed("St\u{202e}ate".into()),
                    "abc1234",
                ),
            ),
            ("k2".into(), "- ".to_owned() + &"x".repeat(400)),
        ];
        let upstream = render("main", found, 300).unwrap();
        assert_eq!(upstream.keys, ["k1"]);
        assert!(
            upstream
                .text
                .ends_with("\n- `St ate` removed from `src/lib.rs` (commit abc1234)"),
            "{}",
            upstream.text
        );
        assert!(render("main", vec![("k".into(), "- x".into())], 10).is_none());
    }
}
