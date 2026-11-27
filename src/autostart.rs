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
    ffi::OsString,
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
const KEPT_ENV: [&str; 20] = [
    "HOME",
    // Where the hosts keep their configuration: the coordinator's host
    // checks read the same directories as the session that started it.
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GEMINI_CLI_HOME",
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
/// The variables `UserHome` reads to find directories in a home. An
/// explicit home ignores them, so a coordinator started for one gets none.
const HOME_OVERRIDES: [&str; 6] = [
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GEMINI_CLI_HOME",
    "LANGSPAN_STATE_DIR",
    "XDG_STATE_HOME",
    "XDG_CONFIG_HOME",
];

/// The environment the started coordinator gets from `vars`, its caller's:
/// the variables in `KEPT_ENV` and every `XDG_*` and `LC_*`. With
/// `home`, an explicit `--home`, `HOME` is that directory and the variables
/// in `HOME_OVERRIDES` are dropped, so the coordinator's
/// `UserHome::new(None)` finds the same directories as `UserHome::new(home)`.
/// A relative override in `service::HOST_DIRS` is made absolute against this
/// process's working directory, since the coordinator runs in `/`.
pub fn child_environment(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    home: Option<&Path>,
) -> Vec<(OsString, OsString)> {
    let mut kept: Vec<(OsString, OsString)> = vars
        .into_iter()
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                (KEPT_ENV.contains(&name) || name.starts_with("XDG_") || name.starts_with("LC_"))
                    && !(home.is_some() && (name == "HOME" || HOME_OVERRIDES.contains(&name)))
            })
        })
        .map(|(name, value)| {
            // The coordinator runs in `/`: a relative host directory
            // resolves here, against the caller's working directory, as
            // `UserHome` resolves it for setup and the hooks. A value that
            // cannot be resolved is kept as written.
            let relative = !value.is_empty() && !Path::new(&value).is_absolute();
            if relative
                && crate::service::HOST_DIRS.iter().any(|dir| name == *dir)
                && let Ok(absolute) = std::path::absolute(&value)
            {
                return (name, absolute.into_os_string());
            }
            (name, value)
        })
        .collect();
    if let Some(home) = home {
        kept.push(("HOME".into(), home.as_os_str().to_owned()));
    }
    kept
}

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
/// past it. `executable` is the `langspan` binary to start; `home` is an
/// explicit `--home`, which the coordinator's host checks then read instead
/// of `HOME`.
pub fn ensure(
    state_dir: &Path,
    executable: &Path,
    home: Option<&Path>,
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
    match start(state_dir, executable, home, deadline) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

/// Stops the coordinator for `state_dir`, for `uninstall` (issue #292): asks
/// it for its process id, sends that process SIGTERM, and waits until
/// `deadline` for it to stop accepting connections. Returns false when none
/// was running.
pub fn stop(state_dir: &Path, deadline: Instant) -> Result<bool, crate::Error> {
    use rustix::process::{Pid, Signal, kill_process};
    if !crate::client::accepts(state_dir, probe_deadline(deadline)) {
        return Ok(false);
    }
    let asked = crate::protocol::Request::new("uninstall", langspan_core::Command::Version);
    let pid = match crate::client::call_until(state_dir, &asked, deadline)?.outcome {
        langspan_core::Outcome::Version { pid, .. } => pid,
        other => return Err(format!("the coordinator answered {other:?} when asked for its process id; stop it yourself and run this again").into()),
    };
    let process = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| format!("the coordinator gave process id {pid}, which is not one"))?;
    // A coordinator holds `daemon.lock` while it runs. A free lock means
    // none does, and the id may now name another process: send nothing.
    let lock = local::private_file(&fs::canonicalize(state_dir)?.join("daemon.lock"), true)?;
    if lock.try_lock().is_ok() {
        return Ok(false);
    }
    kill_process(process, Signal::TERM)
        .map_err(|error| format!("cannot stop the coordinator, process {pid}: {error}"))?;
    wait_for_exit(&lock, pid, deadline)?;
    Ok(true)
}

/// Waits until `deadline` for the coordinator, process `pid`, to free
/// `daemon.lock`, which it holds until its process ends. A connect that
/// fails is no such proof: one bounded by a deadline already past fails too.
fn wait_for_exit(lock: &fs::File, pid: u32, deadline: Instant) -> Result<(), crate::Error> {
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(error)) => {
                return Err(format!(
                    "cannot tell whether the coordinator, process {pid}, stopped: {error}"
                )
                .into());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the coordinator, process {pid}, did not stop in time; run `kill {pid}` and run this again"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A connect bounded to a small part of the caller's wait.
fn probe_deadline(deadline: Instant) -> Instant {
    deadline.min(Instant::now() + Duration::from_millis(100))
}

fn start(
    state_dir: &Path,
    executable: &Path,
    home: Option<&Path>,
    deadline: Instant,
) -> Result<Outcome, crate::Error> {
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
        .envs(child_environment(std::env::vars_os(), home))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> Vec<(OsString, OsString)> {
        [
            ("HOME", "/home/u"),
            ("PATH", "/home/u/.cargo/bin:/usr/bin"),
            ("CLAUDE_CONFIG_DIR", "/cfg/claude"),
            ("CODEX_HOME", "/cfg/codex"),
            ("GEMINI_CLI_HOME", "/cfg/gemini"),
            ("XDG_STATE_HOME", "/home/u/state"),
            ("XDG_RUNTIME_DIR", "/run/user/1"),
            ("ANTHROPIC_API_KEY", "secret"),
            ("OPENAI_API_KEY", "secret"),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect()
    }

    fn value<'a>(env: &'a [(OsString, OsString)], name: &str) -> Option<&'a str> {
        env.iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.to_str())
    }

    /// Issue #292: a coordinator that outlives the wait, here one that still
    /// holds `daemon.lock`, is reported as not stopped; a wait whose
    /// deadline passed before it started reports the same, never success.
    /// A free lock means it stopped.
    #[test]
    fn the_wait_for_the_coordinator_to_stop_ends_in_an_error_at_its_deadline() {
        let directory = std::env::temp_dir().join(format!("ls-stop-wait-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("daemon.lock");
        let held = local::private_file(&path, true).unwrap();
        held.try_lock().unwrap();
        let waiting = local::private_file(&path, true).unwrap();
        let start = Instant::now();
        for deadline in [start + Duration::from_millis(300), start] {
            let error = wait_for_exit(&waiting, 42, deadline)
                .unwrap_err()
                .to_string();
            assert!(error.contains("did not stop in time"), "{error}");
            assert!(error.contains("kill 42"), "{error}");
        }
        assert!(start.elapsed() >= Duration::from_millis(300));
        held.unlock().unwrap();
        wait_for_exit(&waiting, 42, start).unwrap();
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn the_coordinator_keeps_host_configuration_directories_and_drops_keys() {
        let env = child_environment(caller(), None);
        assert_eq!(value(&env, "HOME"), Some("/home/u"));
        assert_eq!(value(&env, "CLAUDE_CONFIG_DIR"), Some("/cfg/claude"));
        assert_eq!(value(&env, "CODEX_HOME"), Some("/cfg/codex"));
        assert_eq!(value(&env, "GEMINI_CLI_HOME"), Some("/cfg/gemini"));
        assert_eq!(value(&env, "XDG_STATE_HOME"), Some("/home/u/state"));
        assert_eq!(value(&env, "ANTHROPIC_API_KEY"), None);
        assert_eq!(value(&env, "OPENAI_API_KEY"), None);
    }

    #[test]
    fn a_relative_host_directory_resolves_against_the_callers_directory() {
        // The coordinator runs in `/`, so `./config/codex` kept as written
        // would point it at `/config/codex`.
        let mut vars = caller();
        vars.retain(|(name, _)| name != "CODEX_HOME" && name != "GEMINI_CLI_HOME");
        vars.push(("CODEX_HOME".into(), "./config/codex".into()));
        vars.push(("GEMINI_CLI_HOME".into(), "".into()));
        let env = child_environment(vars, None);
        let expected = std::path::absolute("./config/codex").unwrap();
        assert!(expected.is_absolute());
        assert_eq!(value(&env, "CODEX_HOME"), expected.to_str());
        assert_eq!(value(&env, "CLAUDE_CONFIG_DIR"), Some("/cfg/claude"));
        // An empty override means none, as `UserHome` reads it.
        assert_eq!(value(&env, "GEMINI_CLI_HOME"), Some(""));
    }

    #[test]
    fn an_explicit_home_replaces_home_and_drops_its_overrides() {
        let env = child_environment(caller(), Some(Path::new("/spare")));
        assert_eq!(value(&env, "HOME"), Some("/spare"));
        assert_eq!(env.iter().filter(|(key, _)| key == "HOME").count(), 1);
        for name in HOME_OVERRIDES {
            assert_eq!(value(&env, name), None, "{name}");
        }
        assert_eq!(value(&env, "PATH"), Some("/home/u/.cargo/bin:/usr/bin"));
        assert_eq!(value(&env, "XDG_RUNTIME_DIR"), Some("/run/user/1"));
        assert_eq!(value(&env, "ANTHROPIC_API_KEY"), None);
    }
}
