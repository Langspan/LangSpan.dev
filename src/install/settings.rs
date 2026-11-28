//! Reading and writing hosts' settings files: the symlink, type and size
//! refusals, and replacement that never overwrites a concurrent change.

use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use super::{CURSOR_CONFIG_VERSION, Vendor};

pub(super) const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

fn settings_metadata(path: &Path) -> Result<Option<fs::Metadata>, crate::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => Err(format!(
            "refusing settings symlink {}; choose its intended target explicitly with --settings; nothing was changed",
            path.display()
        )
        .into()),
        Ok(metadata) if !metadata.is_file() => Err(format!(
            "settings path {} is not a regular file; nothing was changed",
            path.display()
        )
        .into()),
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect {}: {error}", path.display()).into()),
    }
}

pub fn read(path: &Path) -> Result<Option<Value>, crate::Error> {
    parse_json(path, read_bytes(path)?.as_deref())
}

/// Read `vendor`'s MCP configuration file, as `parse_mcp_json` does.
pub fn read_mcp(vendor: Vendor, path: &Path) -> Result<Option<Value>, crate::Error> {
    parse_mcp_json(vendor, path, read_bytes(path)?.as_deref())
}

/// `bytes` of `vendor`'s MCP configuration file as JSON. Antigravity leaves
/// its `mcp_config.json` empty, at project and at user level (1.5.14), so an
/// empty or blank one is the empty configuration, `{}`. Any other host's,
/// and any other bytes, are parsed as `parse_json` does.
pub fn parse_mcp_json(
    vendor: Vendor,
    path: &Path,
    bytes: Option<&[u8]>,
) -> Result<Option<Value>, crate::Error> {
    match bytes {
        Some(bytes)
            if vendor == Vendor::Antigravity && bytes.iter().all(u8::is_ascii_whitespace) =>
        {
            Ok(Some(json!({})))
        }
        bytes => parse_json(path, bytes),
    }
}

/// `bytes` read from `path` as JSON.
pub fn parse_json(path: &Path, bytes: Option<&[u8]>) -> Result<Option<Value>, crate::Error> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_slice(bytes).map_err(|error| {
        format!(
            "{} is not valid JSON ({error}); nothing was changed",
            path.display()
        )
    })?))
}

/// Read a configuration file that is not JSON, with the same refusals.
pub fn read_text(path: &Path) -> Result<Option<String>, crate::Error> {
    parse_text(path, read_bytes(path)?.as_deref())
}

/// `bytes` read from `path` as UTF-8 text.
pub fn parse_text(path: &Path, bytes: Option<&[u8]>) -> Result<Option<String>, crate::Error> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    Ok(Some(String::from_utf8(bytes.to_vec()).map_err(|_| {
        format!("{} is not UTF-8; nothing was changed", path.display())
    })?))
}

/// The file's exact bytes, with the symlink, type and size refusals.
pub fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, crate::Error> {
    if settings_metadata(path)?.is_none() {
        return Ok(None);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display()).into()),
    };
    if !file.metadata()?.is_file() {
        return Err(format!("settings path {} is not a regular file", path.display()).into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(format!("{} exceeds 1 MiB; nothing was changed", path.display()).into());
    }
    Ok(Some(bytes))
}

/// The bytes LangSpan writes for a planned JSON file.
pub fn json_bytes(value: &Value) -> Result<Vec<u8>, crate::Error> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// A file's new content, planned from what it holds now (`None`: absent).
pub type Planner<'a> = dyn Fn(Option<&[u8]>) -> Result<Vec<u8>, crate::Error> + 'a;

/// Replace the file atomically with `plan(from)`, keeping an existing file's
/// mode; new files are 0600. `from` is what the file held when the plan was
/// shown. Just before the rename the file is read again: a host such as
/// Claude Code may have rewritten it meanwhile, and that change is never
/// overwritten. The plan is made once more against the new content; if the
/// file changes again, nothing is written and an error says so. Returns
/// false when the new content already matched, so nothing was written.
///
/// What is left is the moment between that last read and the rename. With
/// no lock that the hosts honour, it can be made short but not closed.
pub fn write_planned(
    path: &Path,
    from: Option<&[u8]>,
    plan: &Planner,
) -> Result<bool, crate::Error> {
    write_planned_with(path, from, plan, &mut || {})
}

pub(super) fn write_planned_with(
    path: &Path,
    from: Option<&[u8]>,
    plan: &Planner,
    before_rename: &mut dyn FnMut(),
) -> Result<bool, crate::Error> {
    let bytes = plan(from)?;
    let now = match write_bytes(path, from, &bytes, before_rename)? {
        Written::Replaced => return Ok(true),
        Written::Changed(now) => now,
    };
    let bytes = plan(now.as_deref())?;
    if now.as_deref() == Some(bytes.as_slice()) {
        return Ok(false);
    }
    match write_bytes(path, now.as_deref(), &bytes, before_rename)? {
        Written::Replaced => Ok(true),
        Written::Changed(_) => Err(format!(
            "{} changed while LangSpan was writing it, twice; nothing was written. Close the program that keeps changing it (for example Claude Code) and run this again",
            path.display()
        )
        .into()),
    }
}

/// Whether a JSON file LangSpan may create holds nothing once LangSpan's
/// entries are removed: an empty object, or for Cursor's `hooks.json` only
/// the `version` install puts in a new one. Such a file changes nothing for
/// its host, so `uninstall` removes it rather than leave `{}` behind.
pub fn json_left_empty(vendor: Vendor, bytes: &[u8]) -> bool {
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    map.is_empty()
        || vendor == Vendor::Cursor
            && map.len() == 1
            && map.get("version") == Some(&json!(CURSOR_CONFIG_VERSION))
}

/// Which planned bytes leave a file holding nothing, so it is removed.
pub type LeftEmpty<'a> = dyn Fn(&[u8]) -> bool + 'a;

/// What `write_or_remove_planned` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    Written,
    Removed,
    /// The file already held the planned content (or was already absent).
    Unchanged,
}

/// `write_planned`, except that a planned content for which `empty` holds
/// removes the file instead of writing it. The file is removed only while
/// it still holds the bytes the plan was made from; if it changed, the plan
/// is made again against the change, and a second change refuses with
/// nothing removed. As with the rename, the moment between the last read
/// and the removal can be made short but not closed.
pub fn write_or_remove_planned(
    path: &Path,
    from: Option<&[u8]>,
    plan: &Planner,
    empty: &LeftEmpty,
) -> Result<Applied, crate::Error> {
    write_or_remove_with(path, from, plan, empty, &mut || {})
}

pub(super) fn write_or_remove_with(
    path: &Path,
    from: Option<&[u8]>,
    plan: &Planner,
    empty: &LeftEmpty,
    before_change: &mut dyn FnMut(),
) -> Result<Applied, crate::Error> {
    let mut from = from.map(<[u8]>::to_vec);
    for _ in 0..2 {
        if !empty(&plan(from.as_deref())?) {
            return Ok(
                match write_planned_with(path, from.as_deref(), plan, before_change)? {
                    true => Applied::Written,
                    false => Applied::Unchanged,
                },
            );
        }
        before_change();
        let now = read_bytes(path)?;
        if now == from {
            if now.is_none() {
                return Ok(Applied::Unchanged);
            }
            fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
            return Ok(Applied::Removed);
        }
        from = now;
    }
    Err(format!(
        "{} changed while LangSpan was removing it, twice; nothing was removed. Close the program that keeps changing it and run this again",
        path.display()
    )
    .into())
}

enum Written {
    Replaced,
    /// The file no longer held what was expected; this is what it holds.
    Changed(Option<Vec<u8>>),
}

#[cfg(unix)]
fn write_bytes(
    path: &Path,
    from: Option<&[u8]>,
    bytes: &[u8],
    before_rename: &mut dyn FnMut(),
) -> Result<Written, crate::Error> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let parent = path.parent().ok_or("settings path has no parent")?;
    let mode = settings_metadata(path)?
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .unwrap_or(0o600);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let temporary = parent.join(format!(
        ".{}.langspan-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("settings"),
        std::process::id()
    ));
    // Creation errors must not enter cleanup: the preexisting entry is not ours.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<Written, crate::Error> {
        file.write_all(bytes)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        before_rename();
        let now = read_bytes(path)?;
        if now.as_deref() != from {
            return Ok(Written::Changed(now));
        }
        fs::rename(&temporary, path)?;
        Ok(Written::Replaced)
    })();
    if !matches!(result, Ok(Written::Replaced)) {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(unix))]
fn write_bytes(
    _path: &Path,
    _from: Option<&[u8]>,
    _bytes: &[u8],
    _before_rename: &mut dyn FnMut(),
) -> Result<Written, crate::Error> {
    Err("hook installation is implemented for Unix in this slice".into())
}

/// The project file in `dir` (canonical) holding a LangSpan hook for
/// `vendor` that names `dir` as its root and `state_dir`, with that hook's
/// registration, its root canonical. Also used by `doctor`.
pub fn install_at(
    vendor: crate::install::Vendor,
    state_dir: &Path,
    dir: &Path,
) -> Option<(PathBuf, crate::install::Registration)> {
    let canonical = |dir: &Path| fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    vendor.project_hook_files().iter().find_map(|file| {
        let path = dir.join(file);
        let Ok(Some(settings)) = crate::install::read(&path) else {
            return None;
        };
        crate::install::managed_hooks(&settings, |_| true)
            .iter()
            .filter_map(|hook| crate::install::parse_project_hook(vendor, hook))
            .map(|mut registration| {
                registration.root = canonical(&registration.root);
                registration
            })
            .find(|registration| {
                registration.root == dir
                    && crate::install::same_directory(&registration.state_dir, state_dir)
            })
            .map(|registration| (path, registration))
    })
}
