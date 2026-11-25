//! Read-only Git operations with bounded output and a caller-owned deadline.
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub root: PathBuf,
    pub common_dir: PathBuf,
}

pub fn identity(cwd: &Path, deadline: Instant) -> Result<Identity, String> {
    let root = output(cwd, &["rev-parse", "--show-toplevel"], deadline, 8192)?;
    let common = output(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        deadline,
        8192,
    )?;
    let path = |bytes: Vec<u8>| -> Result<PathBuf, String> {
        let text = String::from_utf8(bytes).map_err(|_| "Git path is not UTF-8")?;
        let text = text.strip_suffix('\n').unwrap_or(&text);
        fs::canonicalize(text).map_err(|e| format!("cannot resolve Git identity: {e}"))
    };
    Ok(Identity {
        root: path(root)?,
        common_dir: path(common)?,
    })
}

/// `-c` arguments that switch off every filter driver configured for the
/// repository, so a Git command that may hash working files (`status`,
/// `add`) does not run a repository-specified clean or process program, and
/// a required filter that is not installed (Git LFS absent) is not an error.
/// Content is then hashed as the raw bytes on disk, as the observer reads it.
/// Only the driver names are read from the configuration, never the values.
pub fn filter_overrides(cwd: &Path, deadline: Instant) -> Result<Vec<String>, String> {
    let (status, bytes) = run_with_env(
        cwd,
        &[
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\.",
        ],
        &[],
        deadline,
        64 * 1024,
    )?;
    match status.code() {
        Some(0) => {}
        // No such key.
        Some(1) => return Ok(Vec::new()),
        _ => return Err("Git could not list the configured filters".into()),
    }
    let mut names = std::collections::BTreeSet::new();
    for record in bytes.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let key = std::str::from_utf8(record).map_err(|_| "Git filter name is not UTF-8")?;
        let name = key
            .strip_prefix("filter.")
            .and_then(|rest| rest.rsplit_once('.'))
            .map(|(name, _)| name)
            .filter(|name| !name.is_empty() && !name.contains(['\n', '=']))
            .ok_or("Git filter configuration is malformed")?;
        names.insert(name.to_owned());
        if names.len() > 64 {
            return Err("more than 64 Git filter drivers are configured".into());
        }
    }
    let mut arguments = Vec::new();
    for name in names {
        for setting in ["clean=", "smudge=", "process=", "required=false"] {
            arguments.push("-c".to_owned());
            arguments.push(format!("filter.{name}.{setting}"));
        }
    }
    Ok(arguments)
}

pub fn output(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<Vec<u8>, String> {
    output_with_env(cwd, args, &[], deadline, limit)
}

/// As `output`, with extra environment for the Git process. Used only by the
/// paths that write LangSpan's own objects and refs.
pub fn output_with_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
    deadline: Instant,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let (status, bytes) = run_with_env(cwd, args, env, deadline, limit)?;
    if status.success() {
        return Ok(bytes);
    }
    // Repository-controlled stderr can contain private values; do not relay it.
    Err(format!(
        "read-only Git operation failed ({status}); check repository state"
    ))
}

/// Run Git and return its exit status with its bounded standard output, for
/// the commands whose non-zero exit is an answer rather than a failure
/// (`merge-tree` reporting conflicts, `merge-base --is-ancestor`).
pub fn run_with_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
    deadline: Instant,
    limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>), String> {
    let (status, bytes, errors) = run_with_errors(cwd, args, env, deadline, limit)?;
    if status.success() && !errors.is_empty() {
        return Err("read-only Git operation emitted a warning; observation is incomplete, check repository state".into());
    }
    Ok((status, bytes))
}

/// As `run_with_env`, also returning the end of Git's standard error, for a
/// caller that relays it on failure through `said`. A warning on success is
/// returned, not an error.
pub fn run_with_errors(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
    deadline: Instant,
    limit: usize,
) -> Result<Ran, String> {
    run(cwd, args, env, deadline, limit)
}

/// The SSH command used for network Git when the owner has not chosen one:
/// fail instead of asking for a passphrase or a host-key confirmation.
/// Keys from an agent still work.
pub const BATCH_SSH: &str = "ssh -o BatchMode=yes";
/// What Git runs when it would ask a person for a username or password: a
/// command that fails, so Git gives up at once. Credential helpers are asked
/// before it and still answer.
const NO_ASKPASS: &str = "/usr/bin/false";

/// As `run_with_env`, for commands that talk to a remote. A server may print
/// progress or notices on standard error when nothing is wrong, so only the
/// exit status decides.
///
/// Nothing may wait for a person: there is no terminal prompt, no askpass
/// program, and SSH runs in batch mode unless the owner set their own
/// `GIT_SSH_COMMAND`, `GIT_SSH` or `core.sshCommand`, which is then left in
/// charge (and is responsible for not prompting).
pub fn run_network(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
) -> Result<std::process::ExitStatus, String> {
    network(cwd, args, deadline).map(|(status, _)| status)
}

/// The most of Git's own words kept from a failed network command.
pub const SAID_LIMIT: usize = 1024;

/// As `run_network`, also returning the end of what Git and the server wrote
/// on standard error (see `said`). It is the server's text, so a caller
/// relays it labelled as Git's and decides nothing from it but whether a
/// refusal is permanent.
pub fn network(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
) -> Result<(std::process::ExitStatus, String), String> {
    network_run(cwd, args, deadline, 64 * 1024).map(|(status, _, errors)| (status, said(&errors)))
}

/// As `run_network`, with Git's standard output, bounded to `limit` bytes.
pub fn run_network_output(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>), String> {
    network_run(cwd, args, deadline, limit).map(|(status, output, _)| (status, output))
}

/// One network Git command: no credential prompt, SSH in batch mode unless
/// the owner chose their own command.
fn network_run(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
    let mut env: Vec<(&str, &std::ffi::OsStr)> = vec![("GIT_ASKPASS", NO_ASKPASS.as_ref())];
    if !owner_ssh(cwd, deadline)? {
        env.push(("GIT_SSH_COMMAND", BATCH_SSH.as_ref()));
    }
    run(cwd, args, &env, deadline, limit)
}

/// Git's refusal text in a form fit for a log line and an error message: its
/// last six non-empty lines joined with ` | `, control characters removed,
/// likely secrets (a URL's `user:pass`, token shapes) replaced by the same
/// redaction every recorded text gets, and at most `SAID_LIMIT` bytes from
/// the end.
pub fn said(errors: &[u8]) -> String {
    const LINES: usize = 6;
    let text = String::from_utf8_lossy(errors);
    let lines: Vec<String> = text
        .split(['\n', '\r'])
        .map(|line| {
            line.chars()
                .filter(|c| !c.is_control() || *c == '\t')
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .filter(|line| !line.is_empty())
        .collect();
    let mut kept = crate::hook::redact(&lines[lines.len().saturating_sub(LINES)..].join(" | "));
    if kept.len() > SAID_LIMIT {
        let mut start = kept.len() - SAID_LIMIT;
        while !kept.is_char_boundary(start) {
            start += 1;
        }
        kept = format!("...{}", &kept[start..]);
    }
    kept
}

/// Whether the owner chose the SSH command Git uses in this repository.
fn owner_ssh(cwd: &Path, deadline: Instant) -> Result<bool, String> {
    if std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some() {
        return Ok(true);
    }
    let (status, value) = run_with_env(
        cwd,
        &["config", "--get", "core.sshCommand"],
        &[],
        deadline,
        8192,
    )?;
    Ok(status.success() && !value.trim_ascii().is_empty())
}

/// Exit status, standard output and standard error of one Git process.
pub type Ran = (std::process::ExitStatus, Vec<u8>, Vec<u8>);

/// One Git process. Standard error keeps only its last 8 KiB, so a chatty
/// command cannot fill the pipe or memory; the end is where Git says why it
/// failed. The caller decides what a non-empty one means.
fn run(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
    deadline: Instant,
    limit: usize,
) -> Result<Ran, String> {
    #[cfg(not(unix))]
    {
        let _ = (cwd, args, env, deadline, limit);
        Err("Git observer currently requires Unix".into())
    }
    #[cfg(unix)]
    {
        use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
        use std::os::unix::process::CommandExt;
        if Instant::now() >= deadline {
            return Err("Git operation deadline elapsed".into());
        }
        // Its own process group, so a credential helper, SSH or remote
        // helper it started is stopped with it.
        let mut child = Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot start read-only Git operation: {e}"))?;
        let group = rustix::process::Pid::from_child(&child);
        let result = (|| {
            let mut stdout = child.stdout.take().ok_or("Git stdout pipe missing")?;
            let mut stderr = child.stderr.take().ok_or("Git stderr pipe missing")?;
            fcntl_setfl(
                &stdout,
                fcntl_getfl(&stdout).map_err(|e| e.to_string())? | OFlags::NONBLOCK,
            )
            .map_err(|e| e.to_string())?;
            fcntl_setfl(
                &stderr,
                fcntl_getfl(&stderr).map_err(|e| e.to_string())? | OFlags::NONBLOCK,
            )
            .map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            let mut errors = Vec::new();
            let mut stdout_done = false;
            let mut stderr_done = false;
            loop {
                if Instant::now() >= deadline {
                    return Err("Git operation deadline elapsed".into());
                }
                drain(&mut stdout, &mut bytes, limit, &mut stdout_done)?;
                drain_tail(&mut stderr, &mut errors, 8192, &mut stderr_done)?;
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())?
                    && stdout_done
                    && stderr_done
                {
                    return Ok((status, bytes, errors));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        })();
        // Only on failure: after a normal exit the group may hold a helper
        // meant to outlive it, such as `git credential-cache--daemon`.
        if result.is_err() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            let _ = child.kill();
        }
        let _ = child.wait();
        result
    }
}

/// As `drain`, keeping only the last `limit` bytes instead of failing.
fn drain_tail(
    reader: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    done: &mut bool,
) -> Result<(), String> {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *done = true;
                return Ok(());
            }
            Ok(n) => {
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.len() > limit {
                    bytes.drain(..bytes.len() - limit);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("cannot read Git output: {e}")),
        }
    }
}

fn drain(
    reader: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    done: &mut bool,
) -> Result<(), String> {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *done = true;
                return Ok(());
            }
            Ok(n) => {
                if bytes.len() + n > limit {
                    return Err(
                        "Git output exceeded its size bound; observation is incomplete".into(),
                    );
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("cannot read Git output: {e}")),
        }
    }
}
