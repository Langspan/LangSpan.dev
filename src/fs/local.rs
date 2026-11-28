//! Owner-only filesystem primitives for the local transport.

#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File},
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

/// Whether `metadata`, read without following a link, is an owner-only
/// directory of this user: the check `private_directory` makes.
#[cfg(unix)]
pub fn check_private_directory(metadata: &fs::Metadata) -> Result<(), crate::Error> {
    check_owner(metadata, true)
}

#[cfg(any(unix, windows))]
pub fn prepare_directory(path: &Path) -> Result<PathBuf, crate::Error> {
    private_directory(path)?;
    let path = fs::canonicalize(path)?;
    check_socket_fits(&path)?;
    Ok(path)
}

/// Refuses a state directory whose `daemon.sock` would not fit in a Unix
/// socket address. `path` need not exist yet: its longest existing ancestor
/// is resolved first, as the daemon resolves the whole path once it exists
/// (on macOS `/var` becomes `/private/var`).
#[cfg(unix)]
pub fn check_socket_fits(path: &Path) -> Result<(), crate::Error> {
    let path = std::path::absolute(path)?;
    let mut existing = path.as_path();
    let mut rest = Vec::new();
    let resolved = loop {
        match fs::canonicalize(existing) {
            Ok(resolved) => break resolved,
            Err(_) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    rest.push(name);
                    existing = parent;
                }
                _ => break existing.to_path_buf(),
            },
        }
    };
    let socket = rest
        .iter()
        .rev()
        .fold(resolved, |path, name| path.join(name))
        .join("daemon.sock");
    if socket.as_os_str().as_encoded_bytes().len() > 100 {
        return Err(format!(
            "state directory {} is too long for a Unix socket; choose a shorter --state-dir",
            path.display()
        )
        .into());
    }
    Ok(())
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

#[cfg(any(unix, windows))]
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

/// Owner-only on Windows: owned by the current user, with a protected DACL
/// that grants access only to this user and SYSTEM (ADR 0039's 0700 and 0600).
#[cfg(windows)]
fn check_owner(file: &File, directory: bool) -> Result<(), crate::Error> {
    if !langspan_windows::is_owner_only(file)? {
        return Err("state files must belong to the current user and be owner-only (an ACL that grants only this user and SYSTEM, with nothing inherited)".into());
    }
    let metadata = file.metadata()?;
    if directory && !metadata.is_dir() || !directory && !metadata.is_file() {
        return Err("state path has an unexpected file type".into());
    }
    Ok(())
}

/// An owner-only directory, created with any missing parents if missing.
/// A symbolic link or junction, another owner or an ACL open to others is
/// refused.
#[cfg(windows)]
pub fn private_directory(path: &Path) -> Result<(), crate::Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => check_owner(&langspan_windows::open_no_follow(path)?, true)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directories(path)?;
            check_owner(&langspan_windows::open_no_follow(path)?, true)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Like `DirBuilder::recursive` with mode 0700: every directory created on
/// the way is owner-only, and one that appears meanwhile is accepted here
/// and checked by the caller.
#[cfg(windows)]
fn create_private_directories(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_private_directories(parent)?;
    }
    match langspan_windows::create_private_directory(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        result => result,
    }
}

/// No Unix socket on Windows yet: the local transport is slice (c) of #85,
/// which decides the endpoint and any limit on its path.
#[cfg(windows)]
pub fn check_socket_fits(_path: &Path) -> Result<(), crate::Error> {
    Ok(())
}

#[cfg(windows)]
pub fn private_file(path: &Path, create: bool) -> Result<File, crate::Error> {
    let file = langspan_windows::open_private_file(path, create)?;
    check_owner(&file, false)?;
    Ok(file)
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

/// Whether the peer of an accepted connection is this process's user, as the
/// kernel reports it; a peer that is not, or whose user cannot be read, is
/// logged as `peer_refused`. Another user cannot reach the owner-only socket;
/// were the directory opened, this still holds (ADR 0039).
#[cfg(unix)]
pub(crate) fn peer_is_owner(stream: &tokio::net::UnixStream) -> bool {
    let peer = stream
        .peer_cred()
        .map_err(crate::Error::from)
        .and_then(|peer| check_peer(peer.uid()));
    if let Err(error) = &peer {
        eprintln!(
            "{}",
            serde_json::json!({"level":"warn","event":"peer_refused","error":error.to_string()})
        );
    }
    peer.is_ok()
}

/// Refuses a socket peer whose effective user is not this process's: the
/// coordinator answers only its owner.
#[cfg(unix)]
fn check_peer(uid: u32) -> Result<(), crate::Error> {
    let owner = rustix::process::geteuid().as_raw();
    if uid != owner {
        return Err(format!(
            "refused a connection from user {uid}: the coordinator answers only its owner, user {owner}"
        )
        .into());
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "langspan-local-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_state_directory_is_created_owner_only_with_its_parents() {
        let scratch = Scratch::new("directory");
        let state = scratch.0.join("a").join("b").join("state");
        private_directory(&state).unwrap();
        private_directory(&state).unwrap();
        for directory in [
            scratch.0.join("a"),
            scratch.0.join("a").join("b"),
            state.clone(),
        ] {
            let handle = langspan_windows::open_no_follow(&directory).unwrap();
            assert!(
                langspan_windows::is_owner_only(&handle).unwrap(),
                "{}",
                directory.display()
            );
        }
        let prepared = prepare_directory(&state).unwrap();
        assert!(prepared.is_dir());
    }

    #[test]
    fn a_directory_open_to_others_is_refused() {
        let scratch = Scratch::new("open");
        let open = scratch.0.join("open");
        fs::create_dir(&open).unwrap();
        let error = private_directory(&open).unwrap_err();
        assert!(error.to_string().contains("owner-only"), "{error}");
    }

    #[test]
    fn a_state_file_is_created_owner_only_and_an_open_one_is_refused() {
        let scratch = Scratch::new("file");
        let state = scratch.0.join("state");
        private_directory(&state).unwrap();
        let path = state.join("graph.db");
        check_existing_file(&path).unwrap();
        private_file(&path, true).unwrap();
        private_file(&path, false).unwrap();
        check_existing_file(&path).unwrap();

        let open = scratch.0.join("open.json");
        fs::write(&open, b"{}").unwrap();
        let error = private_file(&open, false).unwrap_err();
        assert!(error.to_string().contains("owner-only"), "{error}");
        assert!(check_existing_file(&open).is_err());

        assert!(
            private_file(&state, false).is_err(),
            "a directory is not a state file"
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn a_peer_of_another_user_is_refused() {
        let owner = rustix::process::geteuid().as_raw();
        assert!(super::check_peer(owner).is_ok());
        let error = super::check_peer(owner.wrapping_add(1)).unwrap_err();
        assert!(
            error.to_string().contains("answers only its owner"),
            "{error}"
        );
    }
}
