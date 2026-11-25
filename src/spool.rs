//! Observations the coordinator never got, kept and sent again
//! ([ADR 0014](../docs/adr/0014-observation-spool.md)).
//!
//! When a completed tool call's report (an edit's PostToolUse, a read, a
//! shell command's comparison) cannot reach the daemon, the hook keeps what
//! it would have sent, here, and the next hook run that reaches the daemon
//! sends it first. Each entry is one file named by its request ID, the
//! deterministic `wire_id` of the report, so a resend that the daemon already
//! recorded is answered from its receipt and recorded once.
//!
//! Bounded every way: at most 3 tries per entry, counted before sending; an
//! hour old, it is dropped unsent; at most 200 entries and 512 KiB in all,
//! the oldest dropped first; at most 20 resends per hook run, within half of
//! what is left of the hook's budget, stopping at the first that does not
//! reach the daemon. A newer report of the same session and files replaces
//! an older one.
//!
//! Concurrent hook runs share the directory without a lock. Every write is a
//! private temporary file renamed into place, and a try is counted only when
//! the entry still reads as it did when this run chose it.

use langspan_core::{ObservedFile, ToolResult};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAX_ENTRIES: usize = 200;
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024;
/// One entry: 64 paths and their digests fit with room to spare.
pub const MAX_ENTRY_BYTES: u64 = 64 * 1024;
pub const MAX_AGE_MS: u64 = 60 * 60 * 1000;
pub const MAX_TRIES: u32 = 3;
pub const MAX_RESENDS: usize = 20;

/// What a kept report needs to be sent again as it would have been.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The report's request ID, and the entry's file name.
    pub request_id: String,
    /// When it was kept, on `crate::clock` (sleep-inclusive, since boot).
    pub created_ms: u64,
    /// The same moment on the wall clock: a reboot restarts the other one.
    pub created_wall_ms: u64,
    pub tries: u32,
    pub workspace_id: String,
    pub root: PathBuf,
    pub source: String,
    pub host_session: String,
    pub tool_use_id: String,
    pub tool: String,
    pub input_digest: String,
    pub parent: Option<String>,
    pub report: Report,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Report {
    /// An edit tool's PostToolUse or PostToolUseFailure. `files` are the
    /// versions on disk when the report failed, not when it is resent.
    Post {
        result: ToolResult,
        paths: Vec<String>,
        files: Vec<ObservedFile>,
    },
    /// A completed read, with the versions read.
    Read { files: Vec<ObservedFile> },
    /// A shell command whose comparison is kept in its run record
    /// (`crate::shell`); the resend records that outcome.
    Shell,
}

impl Entry {
    pub fn session_id(&self) -> String {
        format!("{}.{}", self.source, self.host_session)
    }

    /// Whether a newer report by `session` of `paths` makes this one owed no
    /// longer. A newer edit report replaces an older edit or read of the
    /// same files; a newer read replaces only an older read, since an edit
    /// also tells peers who changed the file. A shell report is never
    /// replaced: its changes are attributed by comparison.
    pub fn superseded_by(&self, session: &str, newer: Superseding<'_>) -> bool {
        if self.session_id() != session {
            return false;
        }
        let (mine, allowed): (Vec<&String>, bool) = match &self.report {
            Report::Post { paths, .. } => (paths.iter().collect(), newer.edit),
            Report::Read { files } => (files.iter().map(|f| &f.path).collect(), true),
            Report::Shell => return false,
        };
        allowed && !mine.is_empty() && mine.iter().all(|path| newer.paths.contains(path))
    }

    /// The files this report names, and whether it is an edit's, for what
    /// it supersedes.
    fn superseding(&self) -> Option<(Vec<String>, bool)> {
        match &self.report {
            Report::Post { paths, .. } => Some((paths.clone(), true)),
            Report::Read { files } => Some((files.iter().map(|f| f.path.clone()).collect(), false)),
            Report::Shell => None,
        }
    }
}

/// A newer report: the files it names, and whether it is an edit's.
#[derive(Debug, Clone, Copy)]
pub struct Superseding<'a> {
    pub paths: &'a [String],
    pub edit: bool,
}

pub fn directory(state_dir: &Path) -> PathBuf {
    state_dir.join("spool")
}

/// Both clocks now.
pub fn clocks() -> (u64, u64) {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    (crate::clock::now_ms(), wall)
}

/// Too old to send, by either clock, or from before a reboot.
fn expired(entry: &Entry, (now, wall): (u64, u64)) -> bool {
    entry.created_ms > now
        || now - entry.created_ms > MAX_AGE_MS
        || wall.saturating_sub(entry.created_wall_ms) > MAX_AGE_MS
        || entry.created_wall_ms > wall.saturating_add(MAX_AGE_MS)
}

fn valid_name(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A kept entry, as read: its file, its bytes on disk.
#[derive(Debug, Clone)]
pub struct Kept {
    pub path: PathBuf,
    pub bytes: u64,
    pub entry: Entry,
}

#[cfg(unix)]
fn read_entry(path: &Path) -> Option<(Entry, u64)> {
    use std::io::Read;
    let file = crate::local::private_file(path, false).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return None;
    }
    let entry: Entry = serde_json::from_slice(&bytes).ok()?;
    let named = path.file_stem().and_then(|s| s.to_str()) == Some(entry.request_id.as_str());
    named.then_some((entry, bytes.len() as u64))
}

#[cfg(unix)]
fn write_entry(path: &Path, entry: &Entry) -> Result<(), String> {
    use std::io::Write;
    let bytes = serde_json::to_vec(entry).map_err(|e| format!("spool entry: {e}"))?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return Err(format!(
            "spool entry of {} bytes is over the {MAX_ENTRY_BYTES}-byte bound",
            bytes.len()
        ));
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut file =
        crate::local::private_file(&temporary, true).map_err(|e| format!("spool entry: {e}"))?;
    let written = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path));
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(format!("spool entry: {error}"));
    }
    Ok(())
}

/// Every entry that reads back, oldest first. A file that does not (another
/// format, over its bound, not owner-only, misnamed) is removed: it can never
/// be sent. Temporary files older than a minute are left-overs of a run that
/// died between writing and renaming.
#[cfg(unix)]
pub fn entries(state_dir: &Path) -> Vec<Kept> {
    let Ok(listing) = fs::read_dir(directory(state_dir)) else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    // Bounded: anything past this many files is not looked at in this run.
    for item in listing.filter_map(Result::ok).take(4 * MAX_ENTRIES) {
        let path = item.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => match read_entry(&path) {
                Some((entry, bytes)) => kept.push(Kept { path, bytes, entry }),
                None => {
                    let _ = fs::remove_file(&path);
                }
            },
            Some(extension) if extension.starts_with("tmp-") => {
                let stale = item
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age.as_secs() > 60);
                if stale {
                    let _ = fs::remove_file(&path);
                }
            }
            _ => {}
        }
    }
    kept.sort_by(|a, b| {
        (a.entry.created_wall_ms, &a.entry.request_id)
            .cmp(&(b.entry.created_wall_ms, &b.entry.request_id))
    });
    kept
}

/// Keep `entry` to send again. An entry already kept under the same request
/// ID stays as it is, tries and all: a report is owed once however often it
/// failed. Older reports it supersedes and expired ones are dropped, then the
/// oldest until the bounds hold.
#[cfg(unix)]
pub fn keep(state_dir: &Path, entry: &Entry) -> Result<(), String> {
    if !valid_name(&entry.request_id) {
        return Err("spool entry request ID is outside the file-name grammar".into());
    }
    let dir = directory(state_dir);
    crate::local::private_directory(&dir).map_err(|e| format!("spool: {e}"))?;
    let path = dir.join(format!("{}.json", entry.request_id));
    let now = clocks();
    let session = entry.session_id();
    let newer = entry.superseding();
    let mut others = Vec::new();
    for kept in entries(state_dir) {
        if kept.entry.request_id == entry.request_id {
            return Ok(());
        }
        let superseded = newer.as_ref().is_some_and(|(paths, edit)| {
            kept.entry
                .superseded_by(&session, Superseding { paths, edit: *edit })
        });
        if superseded || expired(&kept.entry, now) {
            let _ = fs::remove_file(&kept.path);
        } else {
            others.push(kept);
        }
    }
    let size = serde_json::to_vec(entry).map_or(MAX_ENTRY_BYTES, |b| b.len() as u64);
    let mut total: u64 = others.iter().map(|k| k.bytes).sum::<u64>() + size;
    let mut count = others.len() + 1;
    // Oldest first.
    for kept in &others {
        if count <= MAX_ENTRIES && total <= MAX_TOTAL_BYTES {
            break;
        }
        let _ = fs::remove_file(&kept.path);
        count -= 1;
        total -= kept.bytes;
    }
    write_entry(&path, entry)
}

#[cfg(not(unix))]
pub fn keep(_: &Path, _: &Entry) -> Result<(), String> {
    Err("the observation spool requires Unix".into())
}

#[cfg(not(unix))]
pub fn entries(_: &Path) -> Vec<Kept> {
    Vec::new()
}

/// How one resend went.
pub enum Sent {
    /// The daemon answered: recorded, answered from its receipt, or refused.
    /// Either way nothing more is owed.
    Answered,
    /// It did not reach the daemon, or no answer came in time.
    Unreachable,
}

/// What a resend pass did, for the hook's diagnostic.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub sent: usize,
    pub expired: usize,
    pub superseded: usize,
    pub spent: usize,
}

impl Pass {
    pub fn describe(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.sent > 0 {
            parts.push(format!("{} earlier observation(s) sent again", self.sent));
        }
        if self.superseded > 0 {
            parts.push(format!(
                "{} superseded by this report and dropped unsent",
                self.superseded
            ));
        }
        if self.expired > 0 {
            parts.push(format!("{} too old to send, dropped", self.expired));
        }
        if self.spent > 0 {
            parts.push(format!("{} dropped after {MAX_TRIES} tries", self.spent));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// The report a hook run makes after sending what is owed.
#[derive(Debug, Clone, Copy)]
pub struct Live<'a> {
    pub session: &'a str,
    /// Its request ID: a redelivered hook makes the same report again.
    pub request_id: Option<&'a str>,
    /// The files it names, when it is an edit's or a read's.
    pub newer: Option<Superseding<'a>>,
}

/// Send what is owed, least-tried and oldest first, through `send`, at most
/// `MAX_RESENDS` and stopping at the first that does not reach the daemon.
/// What `live`, the report this run makes right after, makes again or
/// supersedes is dropped unsent. The caller checks that the daemon is
/// reachable first, so a daemon that is plainly down spends no tries.
#[cfg(unix)]
pub fn resend(state_dir: &Path, live: &Live<'_>, mut send: impl FnMut(&Entry) -> Sent) -> Pass {
    let mut pass = Pass::default();
    let mut owed = entries(state_dir);
    if owed.is_empty() {
        return pass;
    }
    let now = clocks();
    owed.sort_by_key(|kept| kept.entry.tries);
    let mut stopped = false;
    let mut attempts = 0;
    for kept in owed {
        let entry = &kept.entry;
        if expired(entry, now) {
            let _ = fs::remove_file(&kept.path);
            pass.expired += 1;
            continue;
        }
        if live.request_id == Some(entry.request_id.as_str())
            || live
                .newer
                .is_some_and(|newer| entry.superseded_by(live.session, newer))
        {
            let _ = fs::remove_file(&kept.path);
            pass.superseded += 1;
            continue;
        }
        if entry.tries >= MAX_TRIES {
            let _ = fs::remove_file(&kept.path);
            pass.spent += 1;
            continue;
        }
        if stopped || attempts >= MAX_RESENDS {
            continue;
        }
        // Counted before sending, and only if no other run changed it since
        // it was read: a run killed mid-send still spends a try.
        match read_entry(&kept.path) {
            Some((current, _)) if current == *entry => {}
            _ => continue,
        }
        let mut counted = entry.clone();
        counted.tries += 1;
        if write_entry(&kept.path, &counted).is_err() {
            continue;
        }
        attempts += 1;
        match send(entry) {
            Sent::Answered => {
                let _ = fs::remove_file(&kept.path);
                pass.sent += 1;
            }
            Sent::Unreachable => {
                if counted.tries >= MAX_TRIES {
                    let _ = fs::remove_file(&kept.path);
                    pass.spent += 1;
                }
                stopped = true;
            }
        }
    }
    pass
}

#[cfg(not(unix))]
pub fn resend(_: &Path, _: Option<(&str, Superseding<'_>)>, _: impl FnMut(&Entry) -> Sent) -> Pass {
    Pass::default()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ls-spool-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn entry(id: &str, session: &str, path: &str) -> Entry {
        let (created_ms, created_wall_ms) = clocks();
        Entry {
            request_id: id.into(),
            created_ms,
            created_wall_ms,
            tries: 0,
            workspace_id: "work".into(),
            root: "/repos/work".into(),
            source: "claude-code".into(),
            host_session: session.into(),
            tool_use_id: id.into(),
            tool: "Write".into(),
            input_digest: "0".repeat(64),
            parent: None,
            report: Report::Post {
                result: ToolResult::Succeeded,
                paths: vec![path.into()],
                files: Vec::new(),
            },
        }
    }

    #[test]
    fn the_spool_is_bounded_in_entries_and_bytes_and_owner_only() {
        let state = scratch("bounds");
        for n in 0..(MAX_ENTRIES + 10) {
            let mut e = entry(&format!("e{n:04}"), "s", &format!("f{n}.txt"));
            e.created_wall_ms += n as u64; // a strict age order
            keep(&state, &e).unwrap();
        }
        let kept = entries(&state);
        assert_eq!(kept.len(), MAX_ENTRIES);
        assert_eq!(kept[0].entry.request_id, "e0010", "the oldest went first");
        let mode = fs::metadata(directory(&state))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        let mode = fs::metadata(&kept[0].path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        // Large entries: the byte bound holds before the count does.
        let state = scratch("bytes");
        for n in 0..40 {
            let mut e = entry(&format!("b{n:04}"), "s", "x");
            e.created_wall_ms += n as u64;
            e.report = Report::Post {
                result: ToolResult::Succeeded,
                paths: (0..64).map(|p| format!("{p:0>200}")).collect(),
                files: Vec::new(),
            };
            keep(&state, &e).unwrap();
        }
        let kept = entries(&state);
        let total: u64 = kept.iter().map(|k| k.bytes).sum();
        assert!(total <= MAX_TOTAL_BYTES, "{total}");
        assert!(kept.len() < 40 && kept.last().unwrap().entry.request_id == "b0039");

        // A file that does not read back as an entry is removed, not sent.
        fs::write(directory(&state).join("junk.json"), "{}").unwrap();
        fs::write(
            directory(&state).join("big.json"),
            vec![b' '; MAX_ENTRY_BYTES as usize + 1],
        )
        .unwrap();
        let before = entries(&state).len();
        assert!(!directory(&state).join("junk.json").exists());
        assert!(!directory(&state).join("big.json").exists());
        assert_eq!(entries(&state).len(), before);
        let _ = fs::remove_dir_all(scratch("bounds"));
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn a_report_is_kept_once_and_a_newer_one_of_the_same_files_replaces_it() {
        let state = scratch("once");
        keep(&state, &entry("a1", "alice", "b.txt")).unwrap();
        let mut tried = entry("a1", "alice", "b.txt");
        tried.tries = 2;
        write_entry(&directory(&state).join("a1.json"), &tried).unwrap();
        keep(&state, &entry("a1", "alice", "b.txt")).unwrap();
        assert_eq!(
            entries(&state)[0].entry.tries,
            2,
            "kept once, tries and all"
        );
        // Another session's report of the same file does not replace it.
        keep(&state, &entry("b1", "bob", "b.txt")).unwrap();
        assert_eq!(entries(&state).len(), 2);
        // Alice's newer report of b.txt does.
        keep(&state, &entry("a2", "alice", "b.txt")).unwrap();
        let ids: Vec<_> = entries(&state)
            .into_iter()
            .map(|k| k.entry.request_id)
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a2".to_string()) && ids.contains(&"b1".to_string()));
        let _ = fs::remove_dir_all(&state);
    }
}
