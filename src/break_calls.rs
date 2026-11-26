//! The calls a write puts in its files, kept from its pre-tool hook run to
//! its post-tool one, for a host whose model reads hook context only with
//! the tool's result: Gemini CLI reads none on `BeforeTool` and adds
//! `AfterTool` context to the result
//! ([ADR 0029](../docs/adr/0029-break-alerts-at-write-time.md)).
//!
//! The calls are found before the write, against the file as it was, and
//! checked after it, so a write that fails or never completes uses up no
//! notice. One owner-only file per tool call under `STATE/break-calls/`,
//! named by a digest of the operation ID, holding the call sites only (file,
//! line, name, argument count), never the code. It is removed when it is
//! read; one whose post-tool run never came is removed at a later keep once
//! it is a day old.

use langspan_core::CallSite;
use std::path::{Path, PathBuf};

const DIR: &str = "break-calls";
/// 64 calls with paths of up to 4 KiB fit with room to spare.
const MAX_BYTES: u64 = 512 * 1024;
const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

fn path(state_dir: &Path, operation_id: &str) -> PathBuf {
    let name = crate::sha256::hex(&crate::sha256::digest(operation_id.as_bytes()));
    state_dir.join(DIR).join(format!("{}.json", &name[..32]))
}

/// Keep `calls` for the post-tool run of `operation_id`.
#[cfg(unix)]
pub fn keep(state_dir: &Path, operation_id: &str, calls: &[CallSite]) -> Result<(), String> {
    use std::io::Write;
    let dir = state_dir.join(DIR);
    crate::local::private_directory(&dir).map_err(|e| e.to_string())?;
    prune(&dir);
    let target = path(state_dir, operation_id);
    let staged = target.with_extension("staged");
    let _ = std::fs::remove_file(&staged);
    let text = serde_json::to_vec(calls).map_err(|e| e.to_string())?;
    let mut file = crate::local::private_file(&staged, true).map_err(|e| e.to_string())?;
    file.write_all(&text).map_err(|e| e.to_string())?;
    // Renamed into place whole, so a reader never sees half of it.
    std::fs::rename(&staged, &target).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
pub fn keep(_: &Path, _: &str, _: &[CallSite]) -> Result<(), String> {
    Err("kept break calls are implemented for Unix".into())
}

/// The calls kept for `operation_id`, removed as they are read. `None` when
/// none were kept or they cannot be read within bounds.
#[cfg(unix)]
pub fn take(state_dir: &Path, operation_id: &str) -> Option<Vec<CallSite>> {
    use std::io::Read;
    let target = path(state_dir, operation_id);
    if std::fs::symlink_metadata(&target).is_err() {
        return None;
    }
    let read = crate::local::private_file(&target, false)
        .ok()
        .and_then(|file| {
            let mut text = Vec::new();
            file.take(MAX_BYTES + 1).read_to_end(&mut text).ok()?;
            (text.len() as u64 <= MAX_BYTES).then_some(text)
        });
    let _ = std::fs::remove_file(&target);
    let calls: Vec<CallSite> = serde_json::from_slice(&read?).ok()?;
    (calls.len() <= langspan_core::MAX_CALL_SITES).then_some(calls)
}

#[cfg(not(unix))]
pub fn take(_: &Path, _: &str) -> Option<Vec<CallSite>> {
    None
}

/// Files whose post-tool run never came: the write was refused, or the
/// host stopped.
#[cfg(unix)]
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(10_000) {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > MAX_AGE);
        if old && entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn kept_calls_are_read_once_from_an_owner_only_file() {
        let state = std::env::temp_dir().join(format!("ls-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        crate::local::private_directory(&state).unwrap();
        let calls = vec![CallSite {
            path: "app/main.py".into(),
            line: 5,
            name: "scale".into(),
            args: 1,
            member: false,
        }];
        keep(&state, "op-1", &calls).unwrap();
        let file = path(&state, "op-1");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(take(&state, "op-2"), None, "another call's");
        assert_eq!(take(&state, "op-1"), Some(calls));
        assert_eq!(take(&state, "op-1"), None, "read once");
        // A file that is not the shape kept is nothing, and is removed.
        std::fs::write(&file, "not json").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(take(&state, "op-1"), None);
        assert!(!file.exists());
        std::fs::remove_dir_all(&state).unwrap();
    }
}
