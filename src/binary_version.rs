//! The version a LangSpan binary was built as, read from the file without
//! running it (roadmap I5, ADR 0025). Every build carries `MARKER` once; a
//! hook's or MCP entry's executable named in a host's configuration, which
//! a repository can supply, is only read, never executed.

use std::{
    cmp::Ordering,
    io::{self, Read},
    path::Path,
};

/// This build's version, between a fixed prefix and a NUL, kept in the
/// binary even though nothing but `of_file` reads it. The search prefix is
/// cut from this one string at run time, so no second copy of it is
/// compiled in.
#[used]
pub static MARKER: &str = concat!(
    "\0langspan-binary-version=",
    env!("CARGO_PKG_VERSION"),
    "\0"
);

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The longest version a marker may hold; a longer run is not a marker.
const MAX_VERSION_BYTES: usize = 64;
/// The most of a file `of_file` reads. A release binary is tens of MiB and a
/// debug one a few hundred.
const MAX_FILE_BYTES: u64 = 1 << 30;
const CHUNK_BYTES: usize = 1 << 20;

fn prefix() -> &'static [u8] {
    // Opaque to the optimizer, so it cannot fold the slice below into a
    // second, shorter literal that a scan would also find.
    let bytes = std::hint::black_box(MARKER).as_bytes();
    let end = bytes
        .iter()
        .position(|byte| *byte == b'=')
        .map_or(bytes.len(), |at| at + 1);
    // Without the leading NUL, which is common in a binary.
    &bytes[1..end]
}

/// The version recorded in the file at `path`: `None` when it records none,
/// or more than one, as a build from before the marker, or a file that is
/// not LangSpan, does. Reads at most 1 GiB, in 1 MiB pieces, and only from a
/// regular file: the path comes from a configuration a repository can
/// supply, so a FIFO, device or directory there is refused with
/// `InvalidInput` before any read. The open does not block, so a FIFO with
/// no writer cannot hold it either. A symlink is followed.
pub fn of_file(path: &Path) -> io::Result<Option<String>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    // Nonblocking has no effect on a regular file's reads.
    scan(file.take(MAX_FILE_BYTES), CHUNK_BYTES)
}

fn valid_version_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+')
}

/// The single version marked in `reader`, read `chunk` bytes at a time.
/// Each piece keeps the tail of the one before it, long enough for a whole
/// marker, so a marker split across two reads is still found.
fn scan(mut reader: impl Read, chunk: usize) -> io::Result<Option<String>> {
    let prefix = prefix();
    let keep = prefix.len() + MAX_VERSION_BYTES + 1;
    let mut found: Option<String> = None;
    let mut buffer: Vec<u8> = Vec::with_capacity(chunk + keep);
    let mut piece = vec![0; chunk.max(1)];
    let mut done = false;
    while !done {
        let read = reader.read(&mut piece)?;
        done = read == 0;
        buffer.extend_from_slice(&piece[..read]);
        // A marker that starts this close to the end may still be cut off.
        let complete = if done {
            buffer.len()
        } else {
            buffer.len().saturating_sub(keep)
        };
        let mut at = 0;
        while at < complete {
            let Some(offset) = buffer[at..complete]
                .iter()
                .position(|byte| *byte == prefix[0])
            else {
                break;
            };
            let start = at + offset;
            at = start + 1;
            if !buffer[start..].starts_with(prefix) {
                continue;
            }
            let value = &buffer[start + prefix.len()..];
            let Some(length) = value.iter().position(|byte| !valid_version_byte(*byte)) else {
                continue;
            };
            if length == 0 || length > MAX_VERSION_BYTES || value[length] != 0 {
                continue;
            }
            let version = String::from_utf8_lossy(&value[..length]).into_owned();
            match &found {
                Some(earlier) if *earlier != version => return Ok(None),
                _ => found = Some(version),
            }
        }
        buffer.drain(..complete);
    }
    Ok(found)
}

/// How `a` orders against `b` as `MAJOR.MINOR.PATCH`; `None` when either is
/// not of that form or both are but differ only after it (a pre-release).
pub fn order(a: &str, b: &str) -> Option<Ordering> {
    fn parts(version: &str) -> Option<(u64, u64, u64, &str)> {
        let core_end = version.find(['-', '+']).unwrap_or(version.len());
        let mut numbers = version[..core_end].split('.').map(|n| n.parse().ok());
        let parsed = (numbers.next()??, numbers.next()??, numbers.next()??);
        numbers
            .next()
            .is_none()
            .then_some((parsed.0, parsed.1, parsed.2, &version[core_end..]))
    }
    let (a, b) = (parts(a)?, parts(b)?);
    match (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)) {
        Ordering::Equal if a.3 != b.3 => None,
        ordering => Some(ordering),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built from `prefix()`, as `of_file` is: a literal copy here would
    /// put a second marker prefix in this test binary.
    fn marked(version: &str) -> Vec<u8> {
        let mut bytes = vec![0];
        bytes.extend_from_slice(prefix());
        bytes.extend_from_slice(version.as_bytes());
        bytes.push(0);
        bytes
    }

    #[test]
    fn a_marker_is_found_wherever_it_falls_against_the_reads() {
        let mut file = vec![b'l'; 300];
        file.extend(marked("1.2.3"));
        file.extend(vec![0; 300]);
        // Every chunk size from one byte puts a read boundary at each point
        // of the marker in turn.
        for chunk in 1..80 {
            assert_eq!(
                scan(file.as_slice(), chunk).unwrap().as_deref(),
                Some("1.2.3"),
                "chunk {chunk}"
            );
        }
        // At the very end of the file.
        assert_eq!(
            scan(marked("0.9.0").as_slice(), 4).unwrap().as_deref(),
            Some("0.9.0")
        );
    }

    #[test]
    fn no_marker_two_markers_or_a_malformed_one_is_no_version() {
        let unfinished = [b"langspan 0.2.0\0", &prefix()[..prefix().len() - 1]].concat();
        assert_eq!(scan(unfinished.as_slice(), 7).unwrap(), None);
        let mut two = marked("1.0.0");
        two.extend(marked("2.0.0"));
        assert_eq!(scan(two.as_slice(), 1 << 10).unwrap(), None);
        let mut same = marked("1.0.0");
        same.extend(marked("1.0.0"));
        assert_eq!(scan(same.as_slice(), 3).unwrap().as_deref(), Some("1.0.0"));
        // Not NUL-terminated, empty, or longer than a version may be.
        for bad in [
            [prefix(), b"1.0.0 "].concat(),
            [prefix(), b"\0"].concat(),
            marked(&"9".repeat(MAX_VERSION_BYTES + 1))[1..].to_vec(),
        ] {
            assert_eq!(scan(bad.as_slice(), 5).unwrap(), None, "{bad:?}");
        }
        assert_eq!(scan(&b""[..], 5).unwrap(), None);
    }

    #[test]
    fn this_build_carries_its_own_marker() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(of_file(&exe).unwrap().as_deref(), Some(VERSION));
    }

    /// `of_file(path)` on another thread, failing the test instead of hanging
    /// it when the call blocks.
    fn of_file_within_a_bound(path: &Path) -> io::Result<Option<String>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let owned = path.to_path_buf();
        std::thread::spawn(move || {
            let _ = sender.send(of_file(&owned));
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("of_file({}) blocked", path.display()))
    }

    fn temporary_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ls-bv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fifo(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("langspan");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        path
    }

    #[test]
    fn a_fifo_with_no_writer_is_refused_without_waiting() {
        let dir = temporary_dir("fifo");
        let path = fifo(&dir);
        let error = of_file_within_a_bound(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        assert!(error.to_string().contains("not a regular file"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_fifo_with_an_idle_writer_is_refused_without_waiting() {
        let dir = temporary_dir("idle");
        let path = fifo(&dir);
        // Read and write: opens at once, and holds a writer that never writes.
        let _writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let error = of_file_within_a_bound(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_device_or_a_directory_is_refused_and_a_symlink_to_a_file_is_followed() {
        // Endless zeros: refused, never read for a gigabyte.
        let error = of_file_within_a_bound(Path::new("/dev/zero")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        let dir = temporary_dir("kinds");
        let error = of_file_within_a_bound(&dir).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        // A package manager's `bin/langspan` is a symlink to the keg's file.
        let file = dir.join("real");
        std::fs::write(&file, marked("1.2.3")).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(
            of_file_within_a_bound(&link).unwrap().as_deref(),
            Some("1.2.3")
        );
        // A symlink to a FIFO is the FIFO.
        let fifo = fifo(&dir);
        let to_fifo = dir.join("to-fifo");
        std::os::unix::fs::symlink(&fifo, &to_fifo).unwrap();
        let error = of_file_within_a_bound(&to_fifo).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        // A missing file stays NotFound, which doctor reports elsewhere.
        let error = of_file_within_a_bound(&dir.join("missing")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn versions_order_by_number_and_a_pre_release_is_not_ordered() {
        assert_eq!(order("0.10.0", "0.9.9"), Some(Ordering::Greater));
        assert_eq!(order("1.0.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(order("0.4.0", "0.5.0"), Some(Ordering::Less));
        assert_eq!(order("1.0.0-rc.1", "1.0.0"), None);
        assert_eq!(order("1.0.0-rc.1", "0.9.0"), Some(Ordering::Greater));
        assert_eq!(order("1.0", "1.0.0"), None);
        assert_eq!(order("1.0.0.0", "1.0.0"), None);
        assert_eq!(order("x", "1.0.0"), None);
    }
}
