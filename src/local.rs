//! Owner-only filesystem primitives for the local transport.

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
fn check_owner(metadata: &fs::Metadata, directory: bool) -> Result<(), crate::Error> {
    if metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("state files must belong to the current user and be owner-only (directory 0700, files 0600)".into());
    }
    if directory && !metadata.is_dir() || !directory && !metadata.is_file() {
        return Err("state path has an unexpected file type".into());
    }
    Ok(())
}

/// An owner-only directory, created if missing. A symlink, another owner or
/// a mode open to others is refused.
#[cfg(unix)]
pub fn private_directory(path: &Path) -> Result<(), crate::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => check_owner(&metadata, true)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
            check_owner(&fs::symlink_metadata(path)?, true)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(unix)]
pub fn prepare_directory(path: &Path) -> Result<PathBuf, crate::Error> {
    private_directory(path)?;
    let path = fs::canonicalize(path)?;
    if path
        .join("daemon.sock")
        .as_os_str()
        .as_encoded_bytes()
        .len()
        > 100
    {
        return Err(
            "state directory is too long for a Unix socket; choose a shorter --state-dir".into(),
        );
    }
    Ok(path)
}

#[cfg(unix)]
pub fn private_file(path: &Path, create: bool) -> Result<File, crate::Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    check_owner(&file.metadata()?, false)?;
    Ok(file)
}

#[cfg(unix)]
pub fn check_existing_file(path: &Path) -> Result<(), crate::Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            private_file(path, false)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
pub fn check_endpoint(state_dir: &Path) -> Result<(), crate::Error> {
    use std::os::unix::fs::FileTypeExt;
    let dir = fs::symlink_metadata(state_dir)?;
    check_owner(&dir, true)?;
    let socket = fs::symlink_metadata(state_dir.join("daemon.sock"))?;
    if !socket.file_type().is_socket()
        || socket.uid() != rustix::process::geteuid().as_raw()
        || socket.permissions().mode() & 0o077 != 0
    {
        return Err(
            "daemon socket must be an owner-only socket belonging to the current user".into(),
        );
    }
    Ok(())
}
