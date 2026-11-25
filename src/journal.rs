//! The line journal: which session wrote which lines of an uncommitted file
//! ([ADR 0026](../docs/adr/0026-commit-guard-and-commit-mine.md)).
//!
//! Several sessions in one checkout share one Git index, and a commit takes
//! whole files. The hooks see each write a session makes and attribute it
//! (ADR 0002, ADR 0011); at the same points each write is also kept here,
//! on this machine only: the file, the session, and the keys (truncated
//! SHA-256) of the lines it added and removed, compared with the file just
//! before the write. The commit guard and `langspan commit --mine` match
//! these keys against the lines a commit would take.
//!
//! The "before" of a write is exact or the write's lines are unknown: for an
//! edit tool, the file as the pre-tool hook read it; for a shell command,
//! the content of the version its snapshot recorded (kept here when it was
//! written or seen) or, for a file Git did not list, its `HEAD` content.
//!
//! Layout under `STATE/journal/`: `blobs/<sha256>` (file versions of at most
//! 512 KiB, the "before" of a later write), `pre/<operation>` (the versions
//! an edit tool call started from), and `<workspace>/<path>/<entry>` (one
//! entry per write and path, in a directory per path, so that a write hook
//! reads only the entries of the files it writes). Everything is owner-only.
//! Entries expire after a week; `commit --mine` removes the committing
//! session's entries for the files it committed.
//!
//! Entries written before the per-path directories, flat in
//! `<legacy workspace>/<entry>`, are moved into them on the first access,
//! keeping their names and modification times.

use crate::sha256;
use langspan_core::FileState;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Instant, UNIX_EPOCH},
};

/// A larger file's lines are not journaled; it is attributed whole.
pub const MAX_FILE_BYTES: u64 = 512 * 1024;
/// Line keys kept per side of one write; a write that changes more is
/// recorded with its lines unknown.
pub const MAX_KEYS: usize = 8192;
/// Entries kept per workspace, and file versions kept in all.
const MAX_ENTRIES: usize = 4096;
const MAX_BLOBS: usize = 4096;
const ENTRY_LIMIT_BYTES: u64 = 512 * 1024;
const WEEK_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// A write of more files than this is not journaled.
const MAX_PATHS_PER_WRITE: usize = 64;
/// Pruning runs at most this often.
const PRUNE_EVERY_MS: u64 = 60 * 1000;
/// File versions a shell command's pre-tool hook keeps, at most, in bytes.
const SHELL_KEEP_BYTES: u64 = 4 * 1024 * 1024;

/// The key of one line, terminator included, so that a last line that gains
/// a newline is a changed line.
pub fn line_key(line: &[u8]) -> String {
    sha256::hex(&sha256::digest(line))[..16].to_owned()
}

/// The keys of a file's lines, in order.
pub fn line_keys(content: &[u8]) -> Vec<String> {
    content
        .split_inclusive(|byte| *byte == b'\n')
        .map(line_key)
        .collect()
}

/// The lines one write added and removed, by key, counted as multisets: a
/// line moved within the file is neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lines {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// One session's write of one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub session_id: String,
    pub path: String,
    /// Wall-clock milliseconds, for expiry and for the latest write.
    pub at_ms: u64,
    /// The file as the write left it.
    pub after: FileState,
    /// `None` when the file before the write is not known exactly, or the
    /// write changed more than the journal keeps: the session changed this
    /// file, in lines that are not known.
    pub lines: Option<Lines>,
}

/// Who wrote which lines of one file, from its journal entries: the public
/// reading of the journal (the commit guard, `commit --mine`, and line-level
/// admission build on it).
#[derive(Debug, Default)]
pub struct FileLines<'a> {
    /// Line key -> sessions whose writes added that line.
    pub added: BTreeMap<&'a str, BTreeSet<&'a str>>,
    /// Line key -> sessions whose writes removed that line.
    pub removed: BTreeMap<&'a str, BTreeSet<&'a str>>,
    /// Sessions that changed the file in lines that are not known.
    pub unknown: BTreeSet<&'a str>,
    /// Each write: when (wall-clock ms), by whom, and the version it left.
    pub versions: Vec<(u64, &'a str, &'a FileState)>,
}

/// The entries of a workspace, by file.
pub fn by_file(entries: &[Entry]) -> BTreeMap<&str, FileLines<'_>> {
    let mut files: BTreeMap<&str, FileLines<'_>> = BTreeMap::new();
    for entry in entries {
        let file = files.entry(entry.path.as_str()).or_default();
        let session = entry.session_id.as_str();
        file.versions.push((entry.at_ms, session, &entry.after));
        match &entry.lines {
            None => {
                file.unknown.insert(session);
            }
            Some(lines) => {
                for key in &lines.added {
                    file.added.entry(key).or_default().insert(session);
                }
                for key in &lines.removed {
                    file.removed.entry(key).or_default().insert(session);
                }
            }
        }
    }
    files
}

/// One line of a file version and a session whose journaled write added it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WrittenLine {
    /// One-based line number in the version asked about.
    pub line: usize,
    pub session_id: String,
    /// When that session last wrote it (wall-clock ms).
    pub at_ms: u64,
}

/// The lines of `content` (a version of `path`) that journaled writes added,
/// with who wrote each and when, in line order. A line several sessions
/// added appears once per session. Lines no write added are absent.
pub fn written_lines(entries: &[Entry], path: &str, content: &[u8]) -> Vec<WrittenLine> {
    let mut writers: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.path == path) {
        for key in entry.lines.iter().flat_map(|lines| &lines.added) {
            let at = writers
                .entry(key)
                .or_default()
                .entry(&entry.session_id)
                .or_default();
            *at = (*at).max(entry.at_ms);
        }
    }
    line_keys(content)
        .iter()
        .enumerate()
        .flat_map(|(index, key)| {
            writers
                .get(key.as_str())
                .into_iter()
                .flatten()
                .map(move |(session, at)| WrittenLine {
                    line: index + 1,
                    session_id: (*session).to_owned(),
                    at_ms: *at,
                })
        })
        .collect()
}

/// The lines `after` has that `before` does not, and the reverse.
pub fn diff_lines(before: &[u8], after: &[u8]) -> Option<Lines> {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for key in line_keys(before) {
        *counts.entry(key).or_default() -= 1;
    }
    for key in line_keys(after) {
        *counts.entry(key).or_default() += 1;
    }
    let mut lines = Lines {
        added: Vec::new(),
        removed: Vec::new(),
    };
    for (key, count) in counts {
        let side = if count > 0 {
            &mut lines.added
        } else {
            &mut lines.removed
        };
        for _ in 0..count.unsigned_abs() {
            side.push(key.clone());
        }
    }
    (lines.added.len() <= MAX_KEYS && lines.removed.len() <= MAX_KEYS).then_some(lines)
}

fn now_ms() -> u64 {
    crate::shell::now_ms()
}

fn root(state_dir: &Path) -> PathBuf {
    state_dir.join("journal")
}

fn blobs(state_dir: &Path) -> PathBuf {
    root(state_dir).join("blobs")
}

/// The directory of one physical workspace's entries, after moving in any
/// entries an earlier version kept flat.
fn workspace_dir(state_dir: &Path, root_dir: &Path) -> PathBuf {
    let workspace = crate::shell::workspace_identity(root_dir);
    let workspace = workspace.to_string_lossy();
    let directory = root(state_dir).join(crate::hook::wire_id("journal-by-path", &[&workspace]));
    let legacy = root(state_dir).join(crate::hook::wire_id("journal-workspace", &[&workspace]));
    if fs::symlink_metadata(&legacy).is_ok_and(|m| m.is_dir()) {
        migrate(&legacy, &directory);
    }
    directory
}

/// The directory of one file's entries within a workspace's.
fn path_dir(directory: &Path, path: &str) -> PathBuf {
    directory.join(crate::hook::wire_id("journal-path", &[path]))
}

/// Move flat entries into per-path directories, under the same name and with
/// the same modification time. A file that is not a readable entry (a
/// leftover temporary file, say) is dropped, as no read would ever use it;
/// an entry that cannot be moved stays, for a later access to retry.
fn migrate(legacy: &Path, directory: &Path) {
    let Ok(files) = fs::read_dir(legacy) else {
        return;
    };
    for file in files.filter_map(Result::ok).take(MAX_ENTRIES * 2) {
        let from = file.path();
        let name = file.file_name();
        let entry = (!name.to_string_lossy().starts_with('.'))
            .then(|| read_entry(&from))
            .flatten();
        match entry {
            Some(entry) => {
                let to = path_dir(directory, &entry.path);
                if crate::local::private_directory(&to).is_ok() {
                    let _ = fs::rename(&from, to.join(&name));
                }
            }
            None => {
                let _ = fs::remove_file(&from);
            }
        }
    }
    // Fails while an entry is left or another version still writes there;
    // a later access retries.
    let _ = fs::remove_dir(legacy);
}

/// One entry file, read within its bound and parsed.
fn read_entry(path: &Path) -> Option<Entry> {
    #[cfg(test)]
    ENTRY_READS.with(|reads| reads.set(reads.get() + 1));
    serde_json::from_slice(&read_private(path, ENTRY_LIMIT_BYTES)?).ok()
}

/// Entry files this thread has read since the last call.
#[cfg(test)]
pub(crate) fn entry_reads() -> usize {
    ENTRY_READS.with(|reads| reads.replace(0))
}

#[cfg(test)]
thread_local! {
    /// Entry files this thread has read, for the tests that pin a write
    /// hook's cost.
    static ENTRY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(unix)]
fn write_private(directory: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    crate::local::private_directory(directory).map_err(|e| format!("journal: {e}"))?;
    let path = directory.join(name);
    let temporary = directory.join(format!(".{name}.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut file =
        crate::local::private_file(&temporary, true).map_err(|e| format!("journal: {e}"))?;
    file.write_all(bytes).map_err(|e| format!("journal: {e}"))?;
    fs::rename(&temporary, &path).map_err(|e| format!("journal: {e}"))
}

#[cfg(not(unix))]
fn write_private(_: &Path, _: &str, _: &[u8]) -> Result<(), String> {
    Err("the line journal requires Unix".into())
}

#[cfg(unix)]
fn read_private(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = crate::local::private_file(path, false).ok()?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

#[cfg(not(unix))]
fn read_private(_: &Path, _: u64) -> Option<Vec<u8>> {
    None
}

fn digest_of(state: &FileState) -> Option<&str> {
    match state {
        FileState::Present { digest, .. } => Some(digest),
        FileState::Missing => None,
    }
}

/// Keep a file version, by its SHA-256, unless it is kept already.
fn keep_blob(state_dir: &Path, content: &[u8]) -> Result<String, String> {
    let digest = sha256::hex(&sha256::digest(content));
    let path = blobs(state_dir).join(&digest);
    if path.is_file() {
        // Still wanted: a later write may start from it.
        let _ = fs::File::options()
            .append(true)
            .open(&path)
            .and_then(|file| file.set_modified(std::time::SystemTime::now()));
    } else if content.len() as u64 <= MAX_FILE_BYTES {
        write_private(&blobs(state_dir), &digest, content)?;
    }
    Ok(digest)
}

/// The content of a kept version whose SHA-256 is `digest`, checked.
fn blob(state_dir: &Path, digest: &str) -> Option<Vec<u8>> {
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = read_private(&blobs(state_dir).join(digest), MAX_FILE_BYTES)?;
    (sha256::hex(&sha256::digest(&bytes)) == digest).then_some(bytes)
}

/// A workspace file's content, `Some(None)` when it does not exist, `None`
/// when it is not a regular file within the bound.
fn read_file(root: &Path, path: &str) -> Option<Option<Vec<u8>>> {
    use std::io::Read;
    let full = root.join(path);
    let metadata = match fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(None),
        Err(_) => return None,
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(&full)
        .ok()?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(Some(bytes))
}

fn state_of(content: Option<&[u8]>) -> FileState {
    match content {
        None => FileState::Missing,
        Some(bytes) => FileState::Present {
            digest: sha256::hex(&sha256::digest(bytes)),
            kind: langspan_core::FileKind::Regular,
        },
    }
}

/// The versions an edit tool call starts from, kept by its pre-tool hook.
/// `None` for a file that cannot be read within the bound.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Before {
    files: BTreeMap<String, Option<FileState>>,
    at_ms: u64,
}

fn pre_dir(state_dir: &Path) -> PathBuf {
    root(state_dir).join("pre")
}

/// Before an edit tool runs: keep the files it names as they are now.
pub fn before_edit(state_dir: &Path, operation_id: &str, root: &Path, paths: &[String]) {
    let mut before = Before {
        at_ms: now_ms(),
        ..Before::default()
    };
    for path in paths {
        let state = read_file(root, path).and_then(|content| match content {
            None => Some(FileState::Missing),
            Some(bytes) => keep_blob(state_dir, &bytes)
                .ok()
                .map(|digest| FileState::Present {
                    digest,
                    kind: langspan_core::FileKind::Regular,
                }),
        });
        before.files.insert(path.clone(), state);
    }
    if let Ok(bytes) = serde_json::to_vec(&before) {
        let _ = write_private(&pre_dir(state_dir), operation_id, &bytes);
    }
}

/// Before a shell command runs: keep the content of each changed file its
/// snapshot lists that is not kept yet, within a byte bound and the deadline,
/// so that the command's changes to it can be told line by line.
pub fn before_shell(
    state_dir: &Path,
    root: &Path,
    snapshot: &crate::shell::Snapshot,
    deadline: Instant,
) {
    let mut kept = 0_u64;
    for (path, entry) in &snapshot.files {
        let Some(digest) = digest_of(&entry.state) else {
            continue;
        };
        if Instant::now() >= deadline || kept > SHELL_KEEP_BYTES {
            return;
        }
        if blobs(state_dir).join(digest).is_file() {
            continue;
        }
        if let Some(Some(bytes)) = read_file(root, path)
            && sha256::hex(&sha256::digest(&bytes)) == digest
        {
            kept += bytes.len() as u64;
            let _ = keep_blob(state_dir, &bytes);
        }
    }
}

/// Where the content before a write comes from.
pub enum Origin<'a> {
    /// An edit tool call, whose pre-tool hook kept its files.
    Edit { operation_id: &'a str },
    /// A shell command, with its snapshot from before it ran.
    Shell {
        snapshot: &'a crate::shell::Snapshot,
        deadline: Instant,
    },
}

/// After a write: record what `session_id` changed in each of `paths`. A
/// failure records nothing more and is returned for the hook's detail.
pub fn record(
    state_dir: &Path,
    root: &Path,
    session_id: &str,
    paths: &[String],
    origin: Origin<'_>,
) -> Result<usize, String> {
    let (before, operation) = match &origin {
        Origin::Edit { operation_id } => {
            let path = pre_dir(state_dir).join(operation_id);
            let kept: Option<Before> = read_private(&path, ENTRY_LIMIT_BYTES)
                .and_then(|b| serde_json::from_slice(&b).ok());
            let _ = fs::remove_file(path);
            (kept, (*operation_id).to_owned())
        }
        Origin::Shell { .. } => (None, String::new()),
    };
    let directory = workspace_dir(state_dir, root);
    // A bulk change (a checkout, generated files) is not journaled line by
    // line: it would crowd out every other session's entries.
    if paths.len() > MAX_PATHS_PER_WRITE {
        return Ok(0);
    }
    let mut recorded = 0;
    for path in paths {
        if let Origin::Shell { deadline, .. } = &origin
            && Instant::now() >= *deadline
        {
            break;
        }
        let Some(after) = read_file(root, path) else {
            // Over the bound: attributed whole, by its version. A file that
            // cannot be versioned at all is not recorded.
            let Some(after) = crate::hook::file_state(root, path) else {
                continue;
            };
            let entry = Entry {
                session_id: session_id.into(),
                path: path.clone(),
                at_ms: now_ms(),
                after,
                lines: None,
            };
            write_entry(&directory, &operation, &entry)?;
            recorded += 1;
            continue;
        };
        let start: Option<Vec<u8>> = match &origin {
            Origin::Edit { .. } => match before.as_ref().and_then(|b| b.files.get(path)) {
                Some(Some(FileState::Missing)) => Some(Vec::new()),
                Some(Some(FileState::Present { digest, .. })) => blob(state_dir, digest),
                _ => None,
            },
            Origin::Shell { snapshot, deadline } => match snapshot.files.get(path) {
                Some(entry) => match &entry.state {
                    FileState::Missing => Some(Vec::new()),
                    FileState::Present { digest, .. } => blob(state_dir, digest),
                },
                // Not listed: on disk as in HEAD, or absent from both.
                None => head_content(root, path, snapshot.head.as_deref(), *deadline),
            },
        };
        if let Some(bytes) = &after {
            let _ = keep_blob(state_dir, bytes);
        }
        let after_bytes = after.as_deref().unwrap_or_default();
        let entry = Entry {
            session_id: session_id.into(),
            path: path.clone(),
            at_ms: now_ms(),
            after: state_of(after.as_deref()),
            lines: start.and_then(|start| diff_lines(&start, after_bytes)),
        };
        write_entry(&directory, &operation, &entry)?;
        recorded += 1;
    }
    prune(state_dir, &directory);
    Ok(recorded)
}

/// A file's content in `HEAD`, empty when `HEAD` does not have it, `None`
/// when Git cannot say in time.
fn head_content(root: &Path, path: &str, head: Option<&str>, deadline: Instant) -> Option<Vec<u8>> {
    head_blob(root, path, head, deadline).map(Option::unwrap_or_default)
}

/// A file's version in `HEAD` (missing when `HEAD` does not have it), `None`
/// when Git cannot say in time or the file is over the journal's bound.
pub fn head_state(
    root: &Path,
    path: &str,
    head: Option<&str>,
    deadline: Instant,
) -> Option<FileState> {
    head_blob(root, path, head, deadline).map(|content| state_of(content.as_deref()))
}

/// A file's content in `HEAD`, `Some(None)` when `HEAD` does not have it.
fn head_blob(
    root: &Path,
    path: &str,
    head: Option<&str>,
    deadline: Instant,
) -> Option<Option<Vec<u8>>> {
    let Some(head) = head else {
        return Some(None);
    };
    let listed = crate::git::output(
        root,
        &["ls-tree", "-z", "--full-name", head, "--", path],
        deadline,
        64 * 1024,
    )
    .ok()?;
    if listed.is_empty() {
        return Some(None);
    }
    let text = std::str::from_utf8(&listed).ok()?;
    let (meta, _) = text.split_once('\t')?;
    let mut fields = meta.split(' ');
    let (_, kind, oid) = (fields.next()?, fields.next()?, fields.next()?);
    if kind != "blob" {
        return None;
    }
    let bytes = crate::git::output(
        root,
        &["cat-file", "blob", oid],
        deadline,
        MAX_FILE_BYTES as usize,
    )
    .ok()?;
    Some(Some(bytes))
}

fn write_entry(directory: &Path, operation: &str, entry: &Entry) -> Result<(), String> {
    // Named by the call and the path, so a repeated hook rewrites the same
    // entry; a shell entry is named by its content's time.
    let name = crate::hook::wire_id(
        "journal-entry",
        &[
            &entry.session_id,
            operation,
            &entry.path,
            &if operation.is_empty() {
                entry.at_ms.to_string()
            } else {
                String::new()
            },
        ],
    );
    let bytes = serde_json::to_vec(entry).map_err(|e| format!("journal: {e}"))?;
    let directory = path_dir(directory, &entry.path);
    // Once more if pruning removed the path's directory as the write began.
    write_private(&directory, &name, &bytes).or_else(|_| write_private(&directory, &name, &bytes))
}

/// Every kept entry file of a directory with its modification time.
fn listing(directory: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .take(MAX_ENTRIES * 2)
        .filter_map(|entry| {
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((entry.path(), modified))
        })
        .collect()
}

/// Every kept entry file of a workspace, across its path directories.
fn workspace_listing(directory: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(paths) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for path in paths.filter_map(Result::ok) {
        if files.len() >= MAX_ENTRIES * 2 {
            break;
        }
        if path.file_type().is_ok_and(|kind| kind.is_dir()) {
            files.extend(listing(&path.path()));
        }
    }
    files
}

/// Drop what is older than a week, then the oldest past the count bounds.
fn prune(state_dir: &Path, directory: &Path) {
    let now = now_ms();
    let marker = root(state_dir).join(".pruned");
    let recent = fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .is_some_and(|t| now.saturating_sub(t.as_millis() as u64) < PRUNE_EVERY_MS);
    if recent {
        return;
    }
    let _ = write_private(&root(state_dir), ".pruned", b"");
    for (mut files, limit, age) in [
        (workspace_listing(directory), MAX_ENTRIES, WEEK_MS),
        (listing(&blobs(state_dir)), MAX_BLOBS, WEEK_MS),
        (listing(&pre_dir(state_dir)), MAX_ENTRIES, DAY_MS),
    ] {
        files.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
        for (index, (path, modified)) in files.into_iter().enumerate() {
            if index >= limit || now.saturating_sub(modified) > age {
                let _ = fs::remove_file(path);
            }
        }
    }
    // A path directory left empty for a day goes (a write that finds it
    // gone as it starts writes again).
    for (path, modified) in listing(directory) {
        if now.saturating_sub(modified) > DAY_MS {
            let _ = fs::remove_dir(path);
        }
    }
}

/// Every entry kept for the physical workspace at `root`.
pub fn entries(state_dir: &Path, root: &Path) -> Vec<Entry> {
    read_entries(workspace_listing(&workspace_dir(state_dir, root)))
}

/// The entries kept for `paths` of the physical workspace at `root`: a write
/// hook's lookup, which reads no other file's entries.
pub fn entries_for(state_dir: &Path, root: &Path, paths: &[String]) -> Vec<Entry> {
    let directory = workspace_dir(state_dir, root);
    let unique: BTreeSet<&String> = paths.iter().collect();
    unique
        .into_iter()
        .flat_map(|path| {
            read_entries(listing(&path_dir(&directory, path)))
                .into_iter()
                .filter(move |entry| &entry.path == path)
        })
        .collect()
}

fn read_entries(files: Vec<(PathBuf, u64)>) -> Vec<Entry> {
    let now = now_ms();
    files
        .into_iter()
        .filter(|(_, modified)| now.saturating_sub(*modified) <= WEEK_MS)
        .filter_map(|(path, _)| read_entry(&path))
        .collect()
}

/// Remove the entries of `sessions` for `paths`: their lines are committed.
pub fn forget(
    state_dir: &Path,
    root: &Path,
    sessions: &BTreeSet<String>,
    paths: &BTreeSet<String>,
) {
    let directory = workspace_dir(state_dir, root);
    for path in paths {
        for (file, _) in listing(&path_dir(&directory, path)) {
            let committed = read_entry(&file)
                .is_some_and(|entry| sessions.contains(&entry.session_id) && &entry.path == path);
            if committed {
                let _ = fs::remove_file(file);
            }
        }
    }
}

/// Held commits a session was told about, by fingerprint: each is held once.
fn held_path(state_dir: &Path, session_id: &str) -> (PathBuf, String) {
    (
        root(state_dir).join("held"),
        crate::hook::wire_id("journal-held", &[session_id]),
    )
}

/// Whether `session_id` was already told about `fingerprint`; if not, it is
/// remembered as told.
pub fn told(state_dir: &Path, session_id: &str, fingerprint: &str) -> bool {
    let (directory, name) = held_path(state_dir, session_id);
    let mut known: Vec<String> = read_private(&directory.join(&name), 64 * 1024)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if known.iter().any(|seen| seen == fingerprint) {
        return true;
    }
    known.insert(0, fingerprint.to_owned());
    known.truncate(64);
    if let Ok(bytes) = serde_json::to_vec(&known) {
        let _ = write_private(&directory, &name, &bytes);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_adds_and_removes_lines_as_multisets() {
        let lines = diff_lines(b"a\nb\nc\n", b"a\nB\nc\nd").unwrap();
        let mut added = vec![line_key(b"B\n"), line_key(b"d")];
        added.sort();
        assert_eq!(lines.added, added);
        assert_eq!(lines.removed, vec![line_key(b"b\n")]);
        // A moved line is neither added nor removed.
        let moved = diff_lines(b"a\nb\n", b"b\na\n").unwrap();
        assert!(moved.added.is_empty() && moved.removed.is_empty());
        // A line repeated once more is added once.
        let repeated = diff_lines(b"}\n", b"}\n}\n").unwrap();
        assert_eq!(repeated.added, vec![line_key(b"}\n")]);
    }

    #[test]
    fn written_lines_say_who_wrote_each_line_of_a_version_and_when() {
        let entry = |session: &str, at_ms, before: &[u8], after: &[u8]| Entry {
            session_id: session.into(),
            path: "f".into(),
            at_ms,
            after: state_of(Some(after)),
            lines: diff_lines(before, after),
        };
        let entries = vec![
            entry("a", 1, b"x\ny\n", b"x\nA\ny\n"),
            entry("b", 2, b"x\nA\ny\n", b"x\nA\ny\nB\n"),
            entry("a", 3, b"", b"z\n"),
        ];
        let lines = written_lines(&entries, "f", b"x\nA\ny\nB\n");
        let found: Vec<(usize, &str, u64)> = lines
            .iter()
            .map(|l| (l.line, l.session_id.as_str(), l.at_ms))
            .collect();
        assert_eq!(found, vec![(2, "a", 1), (4, "b", 2)]);
        assert!(written_lines(&entries, "other", b"A\n").is_empty());
        let files = by_file(&entries);
        assert_eq!(files["f"].versions.len(), 3);
        assert!(files["f"].added[line_key(b"B\n").as_str()].contains("b"));
    }

    /// A state directory and a workspace root, removed when dropped.
    struct Dirs {
        base: PathBuf,
        state: PathBuf,
        root: PathBuf,
    }

    impl Dirs {
        fn new(name: &str) -> Self {
            let base =
                std::env::temp_dir().join(format!("ls-journal-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            let (state, root) = (base.join("state"), base.join("root"));
            fs::create_dir_all(&root).unwrap();
            crate::local::private_directory(&state).unwrap();
            Self { base, state, root }
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn entry_of(session: &str, path: &str, at_ms: u64) -> Entry {
        Entry {
            session_id: session.into(),
            path: path.into(),
            at_ms,
            after: state_of(Some(b"x\n")),
            lines: diff_lines(b"", b"x\n"),
        }
    }

    fn reads() -> usize {
        entry_reads()
    }

    #[test]
    fn a_write_hooks_lookup_reads_only_the_entries_of_its_own_paths() {
        let dirs = Dirs::new("lookup");
        let directory = workspace_dir(&dirs.state, &dirs.root);
        let now = now_ms();
        for index in 0..300 {
            let entry = entry_of("a", &format!("src/m{}.rs", index % 50), now);
            write_entry(&directory, &format!("op-{index}"), &entry).unwrap();
        }
        reads();
        // A fresh path: no other path's entry is opened.
        assert!(entries_for(&dirs.state, &dirs.root, &["src/fresh.rs".into()]).is_empty());
        assert_eq!(reads(), 0);
        // A path with entries: exactly its own, however many others exist.
        for index in 0..2 {
            let entry = entry_of("b", "src/fresh.rs", now);
            write_entry(&directory, &format!("fresh-{index}"), &entry).unwrap();
        }
        let paths = ["src/fresh.rs".to_owned(), "src/fresh.rs".to_owned()];
        let found = entries_for(&dirs.state, &dirs.root, &paths);
        assert_eq!(found.len(), 2);
        assert!(
            found
                .iter()
                .all(|e| e.path == "src/fresh.rs" && e.session_id == "b")
        );
        assert_eq!(reads(), 2);
        // `commit --mine` forgets one path's entries reading only those.
        let sessions = BTreeSet::from(["b".to_owned()]);
        forget(
            &dirs.state,
            &dirs.root,
            &sessions,
            &BTreeSet::from(["src/fresh.rs".to_owned()]),
        );
        assert_eq!(reads(), 2);
        assert!(entries_for(&dirs.state, &dirs.root, &paths[..1]).is_empty());
        // The whole journal is still there for the commit guard.
        assert_eq!(entries(&dirs.state, &dirs.root).len(), 300);
    }

    #[test]
    fn entries_an_earlier_version_kept_flat_are_moved_and_still_read() {
        let dirs = Dirs::new("legacy");
        let workspace = crate::shell::workspace_identity(&dirs.root);
        let legacy = root(&dirs.state).join(crate::hook::wire_id(
            "journal-workspace",
            &[&workspace.to_string_lossy()],
        ));
        let now = now_ms();
        // As the earlier version wrote them: one file per entry, flat.
        let old = [
            ("e1", entry_of("a", "src/a.rs", now - 1000)),
            ("e2", entry_of("b", "src/a.rs", now - 500)),
            ("e3", entry_of("a", "src/b.rs", now)),
        ];
        for (name, entry) in &old {
            write_private(&legacy, name, &serde_json::to_vec(entry).unwrap()).unwrap();
        }
        let a_day_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
        fs::File::options()
            .append(true)
            .open(legacy.join("e1"))
            .unwrap()
            .set_modified(a_day_ago)
            .unwrap();
        write_private(&legacy, "junk", b"not an entry").unwrap();

        let mut found = entries_for(&dirs.state, &dirs.root, &["src/a.rs".into()]);
        found.sort_by_key(|e| e.at_ms);
        assert_eq!(found, vec![old[0].1.clone(), old[1].1.clone()]);
        assert!(
            !legacy.exists(),
            "the flat directory is emptied and removed"
        );
        // Same name, same modification time (expiry is unchanged).
        let moved = path_dir(&workspace_dir(&dirs.state, &dirs.root), "src/a.rs").join("e1");
        assert_eq!(fs::metadata(moved).unwrap().modified().unwrap(), a_day_ago);
        assert_eq!(entries(&dirs.state, &dirs.root).len(), 3);
    }

    #[test]
    fn a_terminator_is_part_of_a_line() {
        assert_ne!(line_key(b"x"), line_key(b"x\n"));
        assert_eq!(line_keys(b"x\ny"), vec![line_key(b"x\n"), line_key(b"y")]);
        assert!(line_keys(b"").is_empty());
    }
}
