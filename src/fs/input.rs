//! A host event read from stdin, bounded in size and time.
use std::{io, time::Instant};

pub const MAX_EVENT_BYTES: u64 = 8 * 1024 * 1024;

/// Bound the actual CLI's stdin pipe by the same absolute deadline as IPC.
#[cfg(unix)]
pub fn read_stdin_until(deadline: Instant) -> io::Result<Vec<u8>> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::io::Errno;
    let input = io::stdin();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "hook stdin deadline elapsed")
            })?;
        let wait = Timespec::try_from(remaining).map_err(io::Error::other)?;
        let mut fds = [PollFd::new(&input, PollFlags::IN)];
        match poll(&mut fds, Some(&wait)) {
            Ok(0) | Err(Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        match rustix::io::read(&input, &mut buffer) {
            Ok(0) => return Ok(bytes),
            Ok(n) => {
                if bytes.len() + n > MAX_EVENT_BYTES as usize {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "host event exceeds 8 MiB",
                    ));
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(Errno::INTR | Errno::AGAIN) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
#[cfg(not(unix))]
pub fn read_stdin_until(_: Instant) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native hooks require Unix transport",
    ))
}
