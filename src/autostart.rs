//! Starting a coordinator on demand (issue #159). A hook or `langspan mcp`
//! that finds no coordinator for its state directory, where no login service
//! is installed for that directory, starts `langspan daemon` detached in the
//! background and goes on within its own budget.
//!
//! Exactly one coordinator per state directory: the daemon's own lock on
//! `daemon.lock` already makes a second one exit before it touches the
//! socket. This module also keeps concurrent callers from starting more than
//! one process: a caller starts one only while it holds `autostart.lock`,
//! after checking again under that lock, and records when it did. Another
//! caller within `RETRY_AFTER_MS` of that start does not start one, so a
//! coordinator that cannot start (a damaged store, a full disk) costs each
//! hook a lock and a file read, not a process and a wait.

use crate::local;
use std::{
    fs,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Turns the on-demand start off when set to `off`.
pub const ENV: &str = "LANGSPAN_AUTOSTART";
/// The coordinator's log in the state directory, shared with the login
/// service.
pub const LOG: &str = "daemon.log";
const LOCK: &str = "autostart.lock";
/// How long after one start no caller starts another.
pub const RETRY_AFTER_MS: u64 = 30_000;
/// A log larger than this is moved to `daemon.log.1` before a start.
const LOG_LIMIT: u64 = 8 << 20;
/// The variables the started coordinator keeps from its caller's
/// environment. Everything else, such as a host's API keys, is dropped.
const KEPT_ENV: [&str; 17] = [
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "HOSTNAME",
    "SSH_AUTH_SOCK",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "LANGSPAN_STATE_DIR",
];

/// What `ensure` found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A coordinator accepts connections.
    Running,
    /// This call started one, with this process ID; `ready` says whether it
    /// accepted connections before the wait ended.
    Started { pid: u32, ready: bool },
    /// Not started: the reason.
    Skipped(&'static str),
    /// Starting failed: why.
    Failed(String),
}

impl Outcome {
    /// One structured log line for stderr, or none when nothing happened.
    pub fn log_line(&self, state_dir: &Path) -> Option<String> {
        let line = match self {
            Outcome::Running | Outcome::Skipped("disabled") => return None,
            Outcome::Started { pid, ready } => serde_json::json!({
                "level": "info",
                "event": "coordinator_started",
                "pid": pid,
                "ready": ready,
                "state_dir": state_dir,
                "log": state_dir.join(LOG),
            }),
            Outcome::Skipped(reason) => serde_json::json!({
                "level": "info",
                "event": "coordinator_not_started",
                "reason": reason,
                "state_dir": state_dir,
            }),
            Outcome::Failed(error) => serde_json::json!({
                "level": "error",
                "event": "coordinator_not_started",
                "reason": "failed",
                "message": error,
                "state_dir": state_dir,
            }),
        };
        Some(line.to_string())
    }
}

/// Whether `LANGSPAN_AUTOSTART` turns the on-demand start off.
pub fn disabled() -> bool {
    std::env::var_os(ENV).is_some_and(|value| value == "off")
}

/// Makes sure a coordinator runs for `state_dir`, starting one if none
/// accepts connections and `service_installed` is false. Waits at most until
/// `deadline`, and returns at once when another caller holds the start lock
/// past it. `executable` is the `langspan` binary to start.
pub fn ensure(
    state_dir: &Path,
    executable: &Path,
    service_installed: bool,
    deadline: Instant,
) -> Outcome {
    if Instant::now() >= deadline {
        return Outcome::Skipped("no_time_left");
    }
    if crate::client::accepts(state_dir, probe_deadline(deadline)) {
        return Outcome::Running;
    }
    if disabled() {
        return Outcome::Skipped("disabled");
    }
    if service_installed {
        return Outcome::Skipped("service_installed");
    }
    match start(state_dir, executable, deadline) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

/// A connect bounded to a small part of the caller's wait.
fn probe_deadline(deadline: Instant) -> Instant {
    deadline.min(Instant::now() + Duration::from_millis(100))
}

fn start(state_dir: &Path, executable: &Path, deadline: Instant) -> Result<Outcome, crate::Error> {
    // A coordinator started here would exit at once.
    local::check_socket_fits(state_dir)?;
    local::private_directory(state_dir)?;
    let directory = fs::canonicalize(state_dir)?;
    let mut lock = local::private_file(&directory.join(LOCK), true)?;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) => {
                // Another caller is starting one: wait for it, within bound.
                if Instant::now() >= deadline {
                    return Ok(Outcome::Skipped("another_caller_starting"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    // Checked again under the lock: the caller that held it may have
    // started one meanwhile.
    if crate::client::accepts(&directory, probe_deadline(deadline)) {
        return Ok(Outcome::Running);
    }
    let now = crate::clock::now_ms();
    let mut recorded = String::new();
    (&lock).take(64).read_to_string(&mut recorded)?;
    // A record from before a reboot is larger than the clock now and is
    // ignored, as is one that is not a number.
    if let Ok(last) = recorded.trim().parse::<u64>()
        && last <= now
        && now - last < RETRY_AFTER_MS
    {
        return Ok(Outcome::Skipped("started_recently"));
    }
    lock.set_len(0)?;
    lock.rewind()?;
    lock.write_all(now.to_string().as_bytes())?;
    let log = open_log(&directory)?;
    let mut note = log.try_clone()?;
    let mut command = Command::new(executable);
    command
        .arg("daemon")
        .arg("--state-dir")
        .arg(&directory)
        .env_clear()
        .envs(std::env::vars_os().filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                KEPT_ENV.contains(&name) || name.starts_with("XDG_") || name.starts_with("LC_")
            })
        }))
        .current_dir("/")
        .stdin(Stdio::null())
        // Never the hook's stdout: the host reads it until every writer
        // closes it.
        .stdout(Stdio::null())
        .stderr(log);
    detach(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", executable.display()))?;
    let pid = child.id();
    // Not waited for: it outlives this process, which leaves it to init.
    drop(child);
    let _ = writeln!(
        note,
        "{}",
        serde_json::json!({"level":"info","event":"coordinator_started_on_demand","pid":pid,"by":std::process::id()})
    );
    let ready = loop {
        if crate::client::accepts(&directory, probe_deadline(deadline)) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Outcome::Started { pid, ready })
}

/// Its own process group, so a signal to the host's group (Ctrl-C in the
/// host's terminal) does not stop it.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// `daemon.log` in `directory`, owner-only, opened to append. One that has
/// grown past `LOG_LIMIT` is moved aside first, so it stays bounded across
/// starts.
pub fn open_log(directory: &Path) -> Result<fs::File, crate::Error> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = directory.join(LOG);
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
        fs::rename(&path, directory.join(format!("{LOG}.1")))?;
    }
    local::check_existing_file(&path)?;
    Ok(fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&path)?)
}

/// Refuses a state directory too long for the coordinator's socket, before
/// anything is changed for it.
pub fn check_state_dir(state_dir: &Path) -> Result<(), crate::Error> {
    local::check_socket_fits(state_dir)
}

/// Creates `state_dir` owner-only if it is missing, and refuses one that is
/// not owner-only.
pub fn prepare_state_dir(state_dir: &Path) -> Result<(), crate::Error> {
    local::private_directory(state_dir)
}

/// The coordinator's log for `state_dir`.
pub fn log_path(state_dir: &Path) -> PathBuf {
    state_dir.join(LOG)
}
