//! Read-only Git operations with bounded output and a caller-owned deadline.
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Why a Git operation failed, in the classes a caller acts on. Its text is
/// the message the operation gives people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// Git could not be started, or does not run on this platform.
    Unavailable(String),
    /// The caller's deadline passed before Git finished.
    TimedOut,
    /// Git ran and failed, or warned; its own words are not kept.
    Failed(String),
    /// Git's answer was not of the form asked for, or over its bound.
    Corrupt(String),
    /// A worktree file or Git's output could not be read.
    Io(String),
    /// The caller asked for something Git would read as something else.
    Invalid(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => f.write_str("Git operation deadline elapsed"),
            Self::Unavailable(message)
            | Self::Failed(message)
            | Self::Corrupt(message)
            | Self::Io(message)
            | Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GitError {}

impl From<GitError> for String {
    fn from(error: GitError) -> Self {
        error.to_string()
    }
}

fn corrupt(message: &str) -> GitError {
    GitError::Corrupt(message.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub root: PathBuf,
    pub common_dir: PathBuf,
}

pub fn identity(cwd: &Path, deadline: Instant) -> Result<Identity, GitError> {
    let root = output(cwd, &["rev-parse", "--show-toplevel"], deadline, 8192)?;
    let common = output(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        deadline,
        8192,
    )?;
    let path = |bytes: Vec<u8>| -> Result<PathBuf, GitError> {
        let text = String::from_utf8(bytes).map_err(|_| corrupt("Git path is not UTF-8"))?;
        let text = text.strip_suffix('\n').unwrap_or(&text);
        fs::canonicalize(text)
            .map_err(|e| GitError::Io(format!("cannot resolve Git identity: {e}")))
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
pub fn filter_overrides(cwd: &Path, deadline: Instant) -> Result<Vec<String>, GitError> {
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
        _ => {
            return Err(GitError::Failed(
                "Git could not list the configured filters".into(),
            ));
        }
    }
    let mut names = std::collections::BTreeSet::new();
    for record in bytes.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let key =
            std::str::from_utf8(record).map_err(|_| corrupt("Git filter name is not UTF-8"))?;
        let name = key
            .strip_prefix("filter.")
            .and_then(|rest| rest.rsplit_once('.'))
            .map(|(name, _)| name)
            .filter(|name| !name.is_empty() && !name.contains(['\n', '=']))
            .ok_or_else(|| corrupt("Git filter configuration is malformed"))?;
        names.insert(name.to_owned());
        if names.len() > 64 {
            return Err(corrupt("more than 64 Git filter drivers are configured"));
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
) -> Result<Vec<u8>, GitError> {
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
) -> Result<Vec<u8>, GitError> {
    let (status, bytes) = run_with_env(cwd, args, env, deadline, limit)?;
    if status.success() {
        return Ok(bytes);
    }
    // Repository-controlled stderr can contain private values; do not relay it.
    Err(GitError::Failed(format!(
        "read-only Git operation failed ({status}); check repository state"
    )))
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
) -> Result<(std::process::ExitStatus, Vec<u8>), GitError> {
    let (status, bytes, errors) = run_with_errors(cwd, args, env, deadline, limit)?;
    if status.success() && !errors.is_empty() {
        return Err(GitError::Failed("read-only Git operation emitted a warning; observation is incomplete, check repository state".into()));
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
) -> Result<Ran, GitError> {
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
) -> Result<std::process::ExitStatus, GitError> {
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
) -> Result<(std::process::ExitStatus, String), GitError> {
    network_run(cwd, args, deadline, 64 * 1024).map(|(status, _, errors)| (status, said(&errors)))
}

/// As `run_network`, with Git's standard output, bounded to `limit` bytes.
pub fn run_network_output(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>), GitError> {
    network_run(cwd, args, deadline, limit).map(|(status, output, _)| (status, output))
}

/// As `run_network_output`, also with the end of Git's standard error, as
/// `said` gives it.
pub(crate) fn network_output(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>, String), GitError> {
    network_run(cwd, args, deadline, limit)
        .map(|(status, output, errors)| (status, output, said(&errors)))
}

/// One network Git command: no credential prompt, SSH in batch mode unless
/// the owner chose their own command.
fn network_run(
    cwd: &Path,
    args: &[&str],
    deadline: Instant,
    limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), GitError> {
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
    let mut kept =
        crate::text::redact::redact(&lines[lines.len().saturating_sub(LINES)..].join(" | "));
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
fn owner_ssh(cwd: &Path, deadline: Instant) -> Result<bool, GitError> {
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

/// Where the version of `path` in the worktree at `changed` stands against
/// the checkout at `base` (issue #217): `Uncommitted` while the working file
/// differs from that worktree's `HEAD`; otherwise the newest commit of its
/// `HEAD` that touches `path` is `Merged` when `base`'s `HEAD` contains it
/// (`merge-base --is-ancestor`) and `Unmerged` when not. The file is hashed
/// as its raw bytes (`hash-object --no-filters`), so no repository-configured
/// filter runs; a file differing only by CRLF line endings is compared by
/// `crlf_committed`. At most five read-only Git processes, all within
/// `deadline`.
pub fn standing(
    changed: &Path,
    path: &str,
    base: &Path,
    deadline: Instant,
) -> Result<langspan_core::Standing, GitError> {
    use langspan_core::Standing;
    // A path for Git's own syntax only: relative, no option or revision
    // punctuation Git would read as something else.
    if path.is_empty() || path.starts_with(['-', '/']) || path.contains(['\n', '\0']) {
        return Err(GitError::Invalid("not a worktree-relative path".into()));
    }
    let object = |bytes: Vec<u8>| -> Result<String, GitError> {
        let text = String::from_utf8(bytes).map_err(|_| corrupt("Git object ID is not UTF-8"))?;
        let id = text.trim_end_matches('\n');
        if (id.len() == 40 || id.len() == 64) && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(id.to_owned())
        } else {
            Err(corrupt("Git did not answer with an object ID"))
        }
    };
    let (status, committed) = run_with_env(
        changed,
        &["rev-parse", "--verify", "--quiet", &format!("HEAD:{path}")],
        &[],
        deadline,
        256,
    )?;
    let committed = if status.success() {
        Some(object(committed)?)
    } else if status.code() == Some(1) {
        None
    } else {
        return Err(GitError::Failed(
            "Git could not read the worktree's HEAD".into(),
        ));
    };
    let working = match fs::symlink_metadata(changed.join(path)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(GitError::Io(format!(
                "cannot read the worktree's file: {error}"
            )));
        }
        // `hash-object` would follow a symbolic link, and a device or FIFO
        // has no content to compare.
        Ok(metadata) if !metadata.is_file() => {
            return Err(GitError::Invalid(
                "the worktree's file is not a regular file".into(),
            ));
        }
        Ok(_) => Some(object(output(
            changed,
            &["hash-object", "--no-filters", "--", path],
            deadline,
            256,
        )?)?),
    };
    if committed != working {
        match (&committed, &working) {
            (Some(blob), Some(_)) if crlf_committed(changed, path, blob, deadline)? => {}
            _ => return Ok(Standing::Uncommitted),
        }
    }
    let commit = object(output(
        changed,
        // Literal, so `src/[id]/a.ts` does not also select `src/i/a.ts`.
        &["--literal-pathspecs", "rev-list", "-1", "HEAD", "--", path],
        deadline,
        256,
    )?)?;
    let (status, _) = run_with_env(
        base,
        &["merge-base", "--is-ancestor", &commit, "HEAD"],
        &[],
        deadline,
        256,
    )?;
    match status.code() {
        Some(0) => Ok(Standing::Merged),
        Some(1) => Ok(Standing::Unmerged),
        _ => Err(GitError::Failed(
            "Git could not compare the commit with the checkout's HEAD".into(),
        )),
    }
}

/// The most bytes `crlf_committed` reads from the working file and the blob.
const CRLF_COMPARE_LIMIT: usize = 1 << 20;

/// Whether the working file at `path` equals the `HEAD` blob `blob` once its
/// CRLF line endings become LF: Git's built-in normalization under
/// `core.autocrlf` or a `text`/`eol=crlf` attribute stores LF while the clean
/// file on disk keeps CRLF. Done here rather than by Git, so no
/// repository-configured filter program runs. A file without CRLF, or either
/// side over `CRLF_COMPARE_LIMIT`, is not equal. At most one Git process
/// (`cat-file blob`), within `deadline`.
fn crlf_committed(
    changed: &Path,
    path: &str,
    blob: &str,
    deadline: Instant,
) -> Result<bool, GitError> {
    let mut working = Vec::new();
    let mut options = fs::OpenOptions::new();
    options.read(true);
    // `standing` saw a regular file; do not follow a link swapped in since.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        rustix::fs::OFlags::NOFOLLOW.bits() as i32,
    );
    options
        .open(changed.join(path))
        .and_then(|file| {
            file.take(CRLF_COMPARE_LIMIT as u64 + 1)
                .read_to_end(&mut working)
        })
        .map_err(|error| GitError::Io(format!("cannot read the worktree's file: {error}")))?;
    if working.len() > CRLF_COMPARE_LIMIT || !working.windows(2).any(|w| w == b"\r\n") {
        return Ok(false);
    }
    let mut normalized = Vec::with_capacity(working.len());
    let mut bytes = working.iter().peekable();
    while let Some(&byte) = bytes.next() {
        if byte == b'\r' && bytes.peek() == Some(&&b'\n') {
            continue;
        }
        normalized.push(byte);
    }
    // One byte over the normalized length: a longer blob cannot be equal,
    // and `output` fails on it rather than reading it all.
    match output(
        changed,
        &["cat-file", "blob", blob],
        deadline,
        normalized.len() + 1,
    ) {
        Ok(committed) => Ok(committed == normalized),
        Err(_) if Instant::now() < deadline => Ok(false),
        Err(error) => Err(error),
    }
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
) -> Result<Ran, GitError> {
    #[cfg(not(unix))]
    {
        let _ = (cwd, args, env, deadline, limit);
        Err(GitError::Unavailable(
            "Git observer currently requires Unix".into(),
        ))
    }
    #[cfg(unix)]
    {
        use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
        use std::os::unix::process::CommandExt;
        let io = |e: &dyn std::fmt::Display| GitError::Io(e.to_string());
        if Instant::now() >= deadline {
            return Err(GitError::TimedOut);
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
            .map_err(|e| {
                GitError::Unavailable(format!("cannot start read-only Git operation: {e}"))
            })?;
        let group = rustix::process::Pid::from_child(&child);
        let result = (|| {
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| io(&"Git stdout pipe missing"))?;
            let mut stderr = child
                .stderr
                .take()
                .ok_or_else(|| io(&"Git stderr pipe missing"))?;
            fcntl_setfl(
                &stdout,
                fcntl_getfl(&stdout).map_err(|e| io(&e))? | OFlags::NONBLOCK,
            )
            .map_err(|e| io(&e))?;
            fcntl_setfl(
                &stderr,
                fcntl_getfl(&stderr).map_err(|e| io(&e))? | OFlags::NONBLOCK,
            )
            .map_err(|e| io(&e))?;
            let mut bytes = Vec::new();
            let mut errors = Vec::new();
            let mut stdout_done = false;
            let mut stderr_done = false;
            loop {
                if Instant::now() >= deadline {
                    return Err(GitError::TimedOut);
                }
                drain(&mut stdout, &mut bytes, limit, &mut stdout_done)?;
                drain_tail(&mut stderr, &mut errors, 8192, &mut stderr_done)?;
                if let Some(status) = child.try_wait().map_err(|e| io(&e))?
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
) -> Result<(), GitError> {
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
            Err(e) => return Err(GitError::Io(format!("cannot read Git output: {e}"))),
        }
    }
}

fn drain(
    reader: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    done: &mut bool,
) -> Result<(), GitError> {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *done = true;
                return Ok(());
            }
            Ok(n) => {
                if bytes.len() + n > limit {
                    return Err(corrupt(
                        "Git output exceeded its size bound; observation is incomplete",
                    ));
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(GitError::Io(format!("cannot read Git output: {e}"))),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::standing;
    use langspan_core::Standing;
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
        time::{Duration, Instant},
    };

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository on `main` with `a.rs` and `b.rs` committed, and a linked
    /// worktree of it on `feature`. Removed when dropped.
    struct Repo {
        base: PathBuf,
    }

    impl Repo {
        fn new(name: &str) -> Self {
            let base = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("ls-standing-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            let main = base.join("main");
            fs::create_dir_all(&main).unwrap();
            fs::write(main.join("a.rs"), "pub fn a() {}\n").unwrap();
            fs::write(main.join("b.rs"), "pub fn b() {}\n").unwrap();
            git(&main, &["init", "--quiet", "--initial-branch=main"]);
            git(&main, &["add", "."]);
            git(&main, &["commit", "--quiet", "-m", "init"]);
            let tree = base.join("tree");
            git(
                &main,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "feature",
                    tree.to_str().unwrap(),
                ],
            );
            Self { base }
        }
        fn main(&self) -> PathBuf {
            self.base.join("main")
        }
        fn tree(&self) -> PathBuf {
            self.base.join("tree")
        }
        fn of(&self, path: &str) -> Result<Standing, super::GitError> {
            standing(
                &self.tree(),
                path,
                &self.main(),
                Instant::now() + Duration::from_secs(10),
            )
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn a_change_is_uncommitted_then_unmerged_then_merged() {
        let repo = Repo::new("cycle");
        let tree = repo.tree();
        // Unchanged since the common commit: already in the base.
        assert_eq!(repo.of("a.rs"), Ok(Standing::Merged));
        fs::write(tree.join("a.rs"), "pub fn renamed() {}\n").unwrap();
        assert_eq!(repo.of("a.rs"), Ok(Standing::Uncommitted));
        // Staged is not committed.
        git(&tree, &["add", "a.rs"]);
        assert_eq!(repo.of("a.rs"), Ok(Standing::Uncommitted));
        git(&tree, &["commit", "--quiet", "-m", "rename"]);
        assert_eq!(repo.of("a.rs"), Ok(Standing::Unmerged));
        // A later commit of another file leaves the file's commit as it is.
        fs::write(tree.join("b.rs"), "pub fn b(x: u8) {}\n").unwrap();
        git(&tree, &["commit", "--quiet", "-am", "b"]);
        assert_eq!(repo.of("a.rs"), Ok(Standing::Unmerged));
        // Merging the branch into the base puts the commit in its HEAD.
        git(&repo.main(), &["merge", "--quiet", "--no-edit", "feature"]);
        assert_eq!(repo.of("a.rs"), Ok(Standing::Merged));
    }

    #[test]
    fn a_new_or_deleted_file_is_judged_by_its_commit() {
        let repo = Repo::new("files");
        let tree = repo.tree();
        // Never committed.
        fs::write(tree.join("new.rs"), "pub fn new() {}\n").unwrap();
        assert_eq!(repo.of("new.rs"), Ok(Standing::Uncommitted));
        // Deleted and not committed.
        fs::remove_file(tree.join("b.rs")).unwrap();
        assert_eq!(repo.of("b.rs"), Ok(Standing::Uncommitted));
        // Deleted and committed: judged by the deleting commit.
        git(&tree, &["commit", "--quiet", "-am", "remove b"]);
        assert_eq!(repo.of("b.rs"), Ok(Standing::Unmerged));
        // A path that never existed has no commit to compare.
        assert!(repo.of("never.rs").is_err());
    }

    #[test]
    fn a_committed_file_with_crlf_on_disk_and_lf_in_head_is_committed() {
        let repo = Repo::new("crlf");
        let tree = repo.tree();
        // `core.autocrlf=true` stores LF in the blob and keeps CRLF on disk.
        git(&tree, &["config", "core.autocrlf", "true"]);
        fs::write(tree.join("a.rs"), "pub fn renamed() {}\r\nfn x() {}\r\n").unwrap();
        git(&tree, &["commit", "--quiet", "-am", "rename"]);
        assert_eq!(repo.of("a.rs"), Ok(Standing::Unmerged));
        // A change beyond line endings is still uncommitted.
        fs::write(tree.join("a.rs"), "pub fn again() {}\r\nfn x() {}\r\n").unwrap();
        assert_eq!(repo.of("a.rs"), Ok(Standing::Uncommitted));
        // So is a working file that is only a prefix of the blob.
        fs::write(tree.join("a.rs"), "pub fn renamed() {}\r\n").unwrap();
        assert_eq!(repo.of("a.rs"), Ok(Standing::Uncommitted));
    }

    #[test]
    fn a_path_with_glob_characters_is_matched_literally() {
        let repo = Repo::new("glob");
        let tree = repo.tree();
        // `src/[id]/a.ts` as a glob also matches `src/i/a.ts`.
        fs::create_dir_all(tree.join("src/[id]")).unwrap();
        fs::create_dir_all(tree.join("src/i")).unwrap();
        fs::write(tree.join("src/[id]/a.ts"), "export function renamed() {}\n").unwrap();
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "--quiet", "-m", "rename"]);
        git(&repo.main(), &["merge", "--quiet", "--no-edit", "feature"]);
        // A later, unmerged commit of the other file is not the notice's.
        fs::write(tree.join("src/i/a.ts"), "export const other = 1;\n").unwrap();
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "--quiet", "-m", "other"]);
        assert_eq!(repo.of("src/[id]/a.ts"), Ok(Standing::Merged));
    }

    #[test]
    fn a_path_git_would_read_as_something_else_is_refused() {
        let repo = Repo::new("paths");
        for path in ["", "-a.rs", "/a.rs", "a\n.rs"] {
            assert!(repo.of(path).is_err(), "{path:?}");
        }
        // A symbolic link is not followed.
        std::os::unix::fs::symlink("/etc/hosts", repo.tree().join("link.rs")).unwrap();
        assert!(repo.of("link.rs").is_err());
        // A deadline already past runs nothing.
        assert!(standing(&repo.tree(), "a.rs", &repo.main(), Instant::now()).is_err());
    }

    /// A process test: what it checks is the deadline of the Git child.
    #[test]
    fn a_past_deadline_is_timed_out_with_the_same_words() {
        let error = super::output(Path::new("."), &["version"], Instant::now(), 64).unwrap_err();
        assert_eq!(error, super::GitError::TimedOut);
        assert_eq!(error.to_string(), "Git operation deadline elapsed");
        let invalid = standing(Path::new("."), "-a", Path::new("."), Instant::now()).unwrap_err();
        assert!(
            matches!(&invalid, super::GitError::Invalid(_)),
            "{invalid:?}"
        );
        assert_eq!(invalid.to_string(), "not a worktree-relative path");
    }
}
