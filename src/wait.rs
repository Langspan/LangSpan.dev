//! A tool call admitted after a wait for a current holder (roadmap B28,
//! [ADR 0036](../docs/adr/0036-coordination-policies.md)) holds its
//! admission under a second request ID: the first one's receipt is the
//! conflict that started the wait. Its pre-tool hook run leaves a mark for
//! the post-tool run, which then observes and completes that admission.
//!
//! One empty owner-only file per tool call under `STATE/waited/`, named by a
//! digest of the operation ID. It is read, not removed, so a post-tool event
//! the host delivers twice finds the same admission; marks a day old are
//! removed at a later mark.

pub use crate::ids::{admission_id, record_id};
use std::path::{Path, PathBuf};

const DIR: &str = "waited";
const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

fn path(state_dir: &Path, operation_id: &str) -> PathBuf {
    let name = crate::sha256::hex(&crate::sha256::digest(operation_id.as_bytes()));
    state_dir.join(DIR).join(&name[..32])
}

/// The admission the post-tool run of `operation_id` observes: the one
/// asked for after a wait when the pre-tool run left a mark, else `first`.
pub fn admission_of(state_dir: &Path, operation_id: &str, first: &str) -> String {
    if std::fs::symlink_metadata(path(state_dir, operation_id)).is_ok_and(|meta| meta.is_file()) {
        admission_id(first)
    } else {
        first.to_owned()
    }
}

/// Mark `operation_id` as admitted after a wait.
#[cfg(unix)]
pub fn mark(state_dir: &Path, operation_id: &str) -> Result<(), String> {
    let dir = state_dir.join(DIR);
    crate::local::private_directory(&dir).map_err(|e| e.to_string())?;
    prune(&dir);
    crate::local::private_file(&path(state_dir, operation_id), true)
        .map(drop)
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
pub fn mark(_: &Path, _: &str) -> Result<(), String> {
    Err("waits are implemented for Unix".into())
}

#[cfg(unix)]
pub(crate) fn prune(dir: &Path) {
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
    fn a_marked_call_observes_the_admission_asked_for_after_its_wait() {
        let state = std::env::temp_dir().join(format!("ls-waited-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        crate::local::private_directory(&state).unwrap();
        assert_eq!(admission_of(&state, "op-1", "first"), "first");
        mark(&state, "op-1").unwrap();
        let mode = std::fs::metadata(path(&state, "op-1"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(admission_of(&state, "op-1", "first"), admission_id("first"));
        assert_eq!(
            admission_of(&state, "op-1", "first"),
            admission_of(&state, "op-1", "first"),
            "read twice, the same admission"
        );
        assert_eq!(admission_of(&state, "op-2", "first"), "first");
        assert_ne!(admission_id("first"), "first");
        std::fs::remove_dir_all(&state).unwrap();
    }
}
