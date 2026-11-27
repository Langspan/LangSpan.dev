//! Wire and host identifiers, and workspace file keys.
pub(crate) mod registration;
pub(crate) mod sha256;

use std::{
    fs, io,
    path::{Component, Path},
};

/// Versioned, length-prefixed digest: `lsh1-<kind>-<first 128 bits, hex>`.
pub fn wire_id(kind: &str, fields: &[&str]) -> String {
    let mut message = Vec::new();
    for field in [&"langspan-hook-id", &"v1", &kind]
        .into_iter()
        .chain(fields)
    {
        message.extend_from_slice(&(field.len() as u64).to_be_bytes());
        message.extend_from_slice(field.as_bytes());
    }
    format!(
        "lsh1-{kind}-{}",
        &sha256::hex(&sha256::digest(&message))[..32]
    )
}

pub fn workspace_request_id(workspace_id: &str, root: &str) -> String {
    wire_id("workspace", &[workspace_id, root])
}

/// The workspace ID a user-level hook uses for a repository root: the same
/// root always gets the same ID, on every run and after every restart.
pub fn user_workspace_id(root: &Path) -> String {
    wire_id("root", &[&root.to_string_lossy()])
}

/// The request ID, and so the admission ID, of the admission asked for
/// after a wait.
pub fn admission_id(first: &str) -> String {
    crate::ids::wire_id("admit-waited", &[first])
}

/// The request ID of the record of how the wait that `first` started
/// ended.
pub fn record_id(first: &str) -> String {
    crate::ids::wire_id("waited", &[first])
}

pub fn valid_host_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// Map a host path to a workspace-relative key. The daemon repeats the
/// canonical checks; this only decides whether the path is inside the root.
pub fn file_key(root: &Path, cwd: Option<&str>, file_path: &str) -> Result<String, String> {
    if file_path.is_empty() || file_path.len() > 4096 || file_path.contains('\0') {
        return Err("file_path is empty, too long or contains NUL".into());
    }
    let path = Path::new(file_path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(cwd.ok_or("relative file_path without cwd")?).join(path)
    };
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("file_path has dot components".into());
    }
    let mut existing = path.clone();
    let mut suffix = Vec::new();
    let resolved = loop {
        match fs::canonicalize(&existing) {
            Ok(resolved) => break resolved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                suffix.push(existing.file_name().ok_or("unresolvable path")?.to_owned());
                if !existing.pop() {
                    return Err("unresolvable path".into());
                }
            }
            Err(error) => return Err(format!("cannot resolve file_path: {error}")),
        }
    };
    let mut resolved = resolved;
    for part in suffix.iter().rev() {
        resolved.push(part);
    }
    if resolved.parent().is_some_and(|parent| {
        parent
            .ancestors()
            .take_while(|ancestor| *ancestor != root)
            .any(|ancestor| ancestor.join(".git").try_exists().unwrap_or(true))
    }) {
        return Err("file_path crosses a nested Git workspace boundary".into());
    }
    let relative = resolved
        .strip_prefix(root)
        .map_err(|_| "file_path is outside the registered workspace root".to_string())?;
    relative
        .to_str()
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "file_path is not a UTF-8 file inside the root".into())
}
