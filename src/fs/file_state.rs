//! The version of a workspace file as it is on disk, read within bounds.
use crate::sha256;
use std::{fs, io, path::Path};

pub const MAX_VERSION_BYTES: u64 = 4 * 1024 * 1024;

/// The version of one workspace file as it is on disk now: its SHA-256, or
/// missing. `None` when it cannot be established within bounds (a symlink, a
/// special file, more than 4 MiB, unreadable); such a file's base is unknown.
pub fn file_state(root: &Path, key: &str) -> Option<langspan_core::FileState> {
    file_content(root, key).map(|content| version_of(content.as_deref()))
}

/// A workspace file's bytes, `Some(None)` when it does not exist, or `None`
/// when it cannot be read within the version bounds.
pub(crate) fn file_content(root: &Path, key: &str) -> Option<Option<Vec<u8>>> {
    use std::io::Read;
    let path = root.join(key);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Some(None),
        Err(_) => return None,
    };
    if !metadata.is_file() || metadata.len() > MAX_VERSION_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .ok()?
        .take(MAX_VERSION_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_VERSION_BYTES {
        return None;
    }
    Some(Some(bytes))
}

pub(crate) fn version_of(bytes: Option<&[u8]>) -> langspan_core::FileState {
    match bytes {
        None => langspan_core::FileState::Missing,
        Some(bytes) => langspan_core::FileState::Present {
            digest: sha256::hex(&sha256::digest(bytes)),
            kind: langspan_core::FileKind::Regular,
        },
    }
}
