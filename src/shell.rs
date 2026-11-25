//! Shell commands run by an agent: the files a few literal forms will write,
//! and what a command really changed, found by comparing the workspace before
//! and after it ([ADR 0011](../docs/adr/0011-shell-writes.md)).
//!
//! The parser is pure and conservative: anything outside its small grammar is
//! "unknown", never a guess. The snapshot reads Git and file contents within
//! bounds and a deadline; when a bound is hit it is incomplete, and nothing is
//! inferred from it.

use langspan_core::FileState;
use serde::{Deserialize, Serialize};

mod formatters;
mod git_commands;
pub use formatters::{Formatting, Reach, formatters};
pub use git_commands::{GitStep, git_steps, moves_checkout};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// A longer command is not parsed.
pub const MAX_COMMAND_BYTES: usize = 8 * 1024;
/// At most this many write targets, the core's limit for one admission.
pub const MAX_TARGETS: usize = 64;

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    /// `;`, `&&` or `|`: the parts all run, so their writes are a union.
    Separator,
    /// `>` or `>>`, with the file descriptor written before it, if any.
    Redirect(Option<u32>),
    /// `N>&M`: a descriptor copied, no file named.
    Duplicate,
}

/// The files a command will write, when the whole command is a sequence of
/// supported forms whose targets are literal paths. `Ok(empty)` means it
/// writes no file (`echo`, `cat a`); `Err` means the write set is unknown.
/// Paths are as written (`./` segments removed), not yet resolved.
pub fn write_targets(command: &str) -> Result<Vec<String>, &'static str> {
    if command.len() > MAX_COMMAND_BYTES {
        return Err("command longer than the parsed limit");
    }
    let tokens = tokenize(command)?;
    let mut targets = BTreeSet::new();
    for part in tokens.split(|token| *token == Token::Separator) {
        simple_command(part, &mut targets)?;
        if targets.len() > MAX_TARGETS {
            return Err("command names more than 64 write targets");
        }
    }
    Ok(targets.into_iter().collect())
}

fn tokenize(command: &str) -> Result<Vec<Token>, &'static str> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    // A word exists once any of its characters or quotes has been read, so
    // `''` is an empty word rather than nothing.
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = command.chars().peekable();
    let end_word =
        |tokens: &mut Vec<Token>, word: &mut String, in_word: &mut bool, quoted: &mut bool| {
            if *in_word {
                tokens.push(Token::Word(std::mem::take(word)));
            }
            *in_word = false;
            *quoted = false;
        };
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => end_word(&mut tokens, &mut word, &mut in_word, &mut quoted),
            '\n' | '\r' => return Err("a command of more than one line"),
            ';' => {
                end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
                tokens.push(Token::Separator);
            }
            '&' => {
                end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
                if chars.next_if_eq(&'&').is_none() {
                    return Err("a background command or `&>`");
                }
                tokens.push(Token::Separator);
            }
            '|' => {
                end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
                if chars.peek().is_some_and(|next| matches!(next, '|' | '&')) {
                    return Err("`||` or `|&`");
                }
                tokens.push(Token::Separator);
            }
            '>' => {
                // `2>` names a descriptor only when the digits stand alone.
                let fd = if in_word && !quoted && word.bytes().all(|b| b.is_ascii_digit()) {
                    let fd = word.parse::<u32>().map_err(|_| "a malformed redirection")?;
                    word.clear();
                    in_word = false;
                    Some(fd)
                } else {
                    end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
                    None
                };
                chars.next_if_eq(&'>');
                if chars.next_if_eq(&'&').is_some() {
                    let mut digits = 0;
                    while chars.next_if(char::is_ascii_digit).is_some() {
                        digits += 1;
                    }
                    if digits == 0 {
                        return Err("a malformed descriptor copy");
                    }
                    tokens.push(Token::Duplicate);
                } else if chars.peek() == Some(&'|') {
                    return Err("`>|`");
                } else {
                    tokens.push(Token::Redirect(fd));
                }
            }
            '\'' => {
                in_word = true;
                quoted = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("an unterminated quote"),
                    }
                }
            }
            '"' => {
                in_word = true;
                quoted = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('$' | '`' | '\\') => {
                            return Err("an expansion or escape inside double quotes");
                        }
                        Some(c) => word.push(c),
                        None => return Err("an unterminated quote"),
                    }
                }
            }
            // A heredoc, input redirection or process substitution stops the
            // parse here, before any body that follows is read as shell.
            '<' => return Err("input redirection, a heredoc or process substitution"),
            '$' | '`' => return Err("a variable or command substitution"),
            '\\' => return Err("a backslash escape"),
            '*' | '?' | '[' | ']' | '{' | '}' | '~' => return Err("a glob or expansion"),
            '(' | ')' => return Err("a subshell or function"),
            '!' | '#' => return Err("history expansion or a comment"),
            c if c.is_control() => return Err("a control character"),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut tokens, &mut word, &mut in_word, &mut quoted);
    Ok(tokens)
}

fn simple_command(tokens: &[Token], targets: &mut BTreeSet<String>) -> Result<(), &'static str> {
    let mut words = Vec::new();
    let mut tokens = tokens.iter();
    while let Some(token) = tokens.next() {
        match token {
            Token::Word(word) => words.push(word.as_str()),
            Token::Duplicate => {}
            Token::Redirect(fd) => {
                let Some(Token::Word(target)) = tokens.next() else {
                    return Err("a redirection without a file");
                };
                if !matches!(fd, None | Some(1) | Some(2)) {
                    return Err("a redirection of an unusual descriptor");
                }
                if target != "/dev/null" {
                    targets.insert(literal_path(target)?);
                }
            }
            Token::Separator => unreachable!("split on separators"),
        }
    }
    let Some((&name, args)) = words.split_first() else {
        // `> file` alone truncates it; an empty part is a syntax error.
        return if targets.is_empty() {
            Err("an empty command")
        } else {
            Ok(())
        };
    };
    if name.contains('=') {
        return Err("a variable assignment");
    }
    let mut add = |paths: &[&str]| -> Result<(), &'static str> {
        for path in paths {
            targets.insert(literal_path(path)?);
        }
        Ok(())
    };
    match name {
        "echo" | "printf" | "cat" | "true" => Ok(()),
        "tee" => add(&operands(
            args,
            "aip",
            &["--append", "--ignore-interrupts"],
        )?),
        "rm" => add(&nonempty(operands(
            args,
            "fiv",
            &["--force", "--verbose"],
        )?)?),
        "touch" => add(&nonempty(operands(args, "acm", &["--no-create"])?)?),
        "mv" | "cp" => {
            let flags = if name == "mv" { "fniv" } else { "fnipv" };
            let files = operands(args, flags, &["--force", "--verbose"])?;
            let [source, destination] = files.as_slice() else {
                return Err("mv or cp with other than one source and one destination");
            };
            if name == "mv" {
                add(&[source, destination])
            } else {
                add(&[destination])
            }
        }
        "truncate" => add(&truncate_operands(args)?),
        "sed" => {
            let files = sed_targets(args)?;
            add(&files.iter().map(String::as_str).collect::<Vec<_>>())
        }
        _ => Err("a command outside the supported set"),
    }
}

/// Non-option arguments, after checking every option is one of `flags`
/// (single letters, combinable) or `long`. `--` ends the options.
fn operands<'a>(
    args: &[&'a str],
    flags: &str,
    long: &[&str],
) -> Result<Vec<&'a str>, &'static str> {
    let mut files = Vec::new();
    let mut options = true;
    for &arg in args {
        if options && arg == "--" {
            options = false;
        } else if options && arg.starts_with("--") {
            if !long.contains(&arg) {
                return Err("an unsupported option");
            }
        } else if options && arg.starts_with('-') && arg.len() > 1 {
            if !arg[1..].chars().all(|c| flags.contains(c)) {
                return Err("an unsupported option");
            }
        } else {
            files.push(arg);
        }
    }
    Ok(files)
}

fn nonempty(files: Vec<&str>) -> Result<Vec<&str>, &'static str> {
    if files.is_empty() {
        return Err("a command with no file operand");
    }
    Ok(files)
}

fn truncate_operands<'a>(args: &[&'a str]) -> Result<Vec<&'a str>, &'static str> {
    let mut files = Vec::new();
    let mut sized = false;
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        if arg == "-s" {
            args.next().ok_or("truncate -s without a size")?;
            sized = true;
        } else if arg.starts_with("--size=") || (arg.starts_with("-s") && arg.len() > 2) {
            sized = true;
        } else if arg == "-c" || arg == "--no-create" {
        } else if arg.starts_with('-') {
            return Err("an unsupported option");
        } else {
            files.push(arg);
        }
    }
    if !sized {
        return Err("truncate without a size");
    }
    nonempty(files)
}

/// `sed` writes files only with `-i`, and then only those it is given, when
/// its script is made of commands that write nowhere else.
fn sed_targets(args: &[&str]) -> Result<Vec<String>, &'static str> {
    let mut in_place = false;
    let mut suffix = String::new();
    let mut script = false;
    let mut files = Vec::new();
    let mut args = args.iter().peekable();
    let mut options = true;
    while let Some(&arg) = args.next() {
        match arg {
            "--" if options => options = false,
            "-i" | "--in-place" if options => {
                in_place = true;
                // BSD sed takes the backup suffix as the next argument; an
                // empty one is the usual `sed -i '' …`.
                args.next_if(|next| next.is_empty());
            }
            "-e" if options => {
                sed_script(args.next().ok_or("sed -e without a script")?)?;
                script = true;
            }
            _ if options && arg.starts_with("--in-place=") => {
                in_place = true;
                suffix = arg["--in-place=".len()..].into();
            }
            _ if options && arg.starts_with("--expression=") => {
                sed_script(&arg["--expression=".len()..])?;
                script = true;
            }
            _ if options && arg.starts_with("-i") => {
                in_place = true;
                suffix = arg[2..].into();
            }
            "--quiet" | "--silent" | "--regexp-extended" | "--posix" | "--separate"
            | "--null-data"
                if options => {}
            _ if options && arg.starts_with('-') && arg.len() > 1 => {
                if !arg[1..].chars().all(|c| "nErsuz".contains(c)) {
                    return Err("an unsupported sed option");
                }
            }
            _ if !script => {
                sed_script(arg)?;
                script = true;
            }
            _ => files.push(arg),
        }
    }
    if !script {
        return Err("sed without a script");
    }
    if !in_place {
        return Ok(Vec::new());
    }
    if files.is_empty() {
        return Err("sed -i without a file");
    }
    if suffix.contains(['/', '*']) {
        return Err("a sed backup suffix naming another place");
    }
    let mut targets: Vec<String> = files.iter().map(|file| (*file).to_owned()).collect();
    if !suffix.is_empty() {
        targets.extend(files.iter().map(|file| format!("{file}{suffix}")));
    }
    Ok(targets)
}

/// Accept a script of `s`, `y`, `d` and `p` commands with simple addresses.
/// Anything that could write or run elsewhere (`w`, `W`, `r`, `e`, an `s`
/// flag of `w` or `e`, a script file) is refused.
fn sed_script(script: &str) -> Result<(), &'static str> {
    const UNSUPPORTED: &str = "a sed script with commands outside s, y, d and p";
    let mut chars = script.chars().peekable();
    let skip_blank = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        while chars
            .next_if(|c| matches!(c, ' ' | '\t' | '\n' | ';'))
            .is_some()
        {}
    };
    // Read up to an unescaped `delimiter`, returning false if it never comes.
    let delimited = |chars: &mut std::iter::Peekable<std::str::Chars>, delimiter: char| loop {
        match chars.next() {
            Some('\\') => {
                if chars.next().is_none() {
                    return false;
                }
            }
            Some('\n') | None => return false,
            Some(c) if c == delimiter => return true,
            Some(_) => {}
        }
    };
    let address = |chars: &mut std::iter::Peekable<std::str::Chars>| -> Result<(), &'static str> {
        if chars.next_if(char::is_ascii_digit).is_some() {
            while chars.next_if(char::is_ascii_digit).is_some() {}
        } else if chars.next_if_eq(&'$').is_some() {
        } else if chars.next_if_eq(&'/').is_some() && !delimited(chars, '/') {
            return Err(UNSUPPORTED);
        }
        Ok(())
    };
    skip_blank(&mut chars);
    while chars.peek().is_some() {
        address(&mut chars)?;
        if chars.next_if_eq(&',').is_some() {
            address(&mut chars)?;
        }
        chars.next_if_eq(&'!');
        match chars.next() {
            Some(command @ ('s' | 'y')) => {
                let delimiter = chars.next().ok_or(UNSUPPORTED)?;
                if matches!(delimiter, '\\' | '\n' | ' ') {
                    return Err(UNSUPPORTED);
                }
                if !delimited(&mut chars, delimiter) || !delimited(&mut chars, delimiter) {
                    return Err(UNSUPPORTED);
                }
                if command == 's' {
                    while chars
                        .next_if(|c| c.is_ascii_digit() || matches!(c, 'g' | 'i' | 'I' | 'p'))
                        .is_some()
                    {}
                }
            }
            Some('d' | 'p') => {}
            _ => return Err(UNSUPPORTED),
        }
        while chars.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        match chars.peek() {
            None | Some(';' | '\n') => skip_blank(&mut chars),
            Some(_) => return Err(UNSUPPORTED),
        }
    }
    Ok(())
}

/// A literal path with `.` segments removed. `..`, an empty path and one
/// that names only `.` are refused.
fn literal_path(path: &str) -> Result<String, &'static str> {
    if path.is_empty() || path.len() > 4096 {
        return Err("an empty or overlong path");
    }
    let absolute = path.starts_with('/');
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("a path with `..`"),
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err("a path naming a directory");
    }
    let joined = parts.join("/");
    Ok(if absolute {
        format!("/{joined}")
    } else {
        joined
    })
}

// ---------------------------------------------------------------------------
// Workspace snapshot
// ---------------------------------------------------------------------------

/// At most this many changed paths in one snapshot.
pub const MAX_SNAPSHOT_FILES: usize = 1024;
/// At most this many bytes of changed files are read for one snapshot.
pub const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STATUS_BYTES: usize = 1024 * 1024;

/// One path Git reports as differing from `HEAD` or the index: its status
/// codes (two with `--no-renames` when, say, it is both deleted from the
/// index and untracked) and its content version on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub codes: Vec<String>,
    pub state: FileState,
}

/// What Git reports as differing, with each such file's content version. A
/// file not listed is, on disk, as it is in `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub head: Option<String>,
    /// The branch checked out, as `refs/heads/NAME`; `None` when `HEAD` is
    /// detached, or in a record written before this was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub files: BTreeMap<String, Entry>,
    /// A merge, rebase, cherry-pick or revert is under way, or a file is
    /// conflicted. Nothing is denied then.
    pub in_progress: bool,
}

fn git_line(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec())
        .map(|text| text.trim_end_matches('\n').to_owned())
        .map_err(|_| "Git printed a path that is not UTF-8".into())
}

/// Snapshot the workspace at `root`, which must be the top level of a Git
/// working tree. Only files Git lists are read, so the cost follows the
/// number of changed files, not the size of the repository. Git's own
/// racy-clean check compares the content of a file modified too recently to
/// trust its timestamp. Every failure and every bound is an error: an
/// incomplete snapshot is never used.
pub fn snapshot(root: &Path, deadline: Instant) -> Result<Snapshot, String> {
    let paths = crate::git::output(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--absolute-git-dir",
        ],
        deadline,
        16 * 1024,
    )
    .map_err(|_| "the workspace is not a Git working tree".to_owned())?;
    let text = git_line(&paths)?;
    let lines: Vec<&str> = text.lines().collect();
    let [top, git_dir] = lines.as_slice() else {
        return Err("Git did not report the working tree's paths".into());
    };
    let top = fs::canonicalize(top).map_err(|e| format!("cannot resolve the working tree: {e}"))?;
    if top != root {
        return Err("the workspace root is not the top level of its Git working tree".into());
    }
    let git_dir = PathBuf::from(git_dir);
    let in_progress = [
        "MERGE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ]
    .iter()
    .any(|marker| git_dir.join(marker).symlink_metadata().is_ok());
    let (status, oid) = crate::git::run_with_env(
        root,
        &["rev-parse", "-q", "--verify", "HEAD^{commit}"],
        &[],
        deadline,
        1024,
    )?;
    let head = status.success().then(|| git_line(&oid)).transpose()?;
    let listing = crate::git::output(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=all",
            "--no-renames",
        ],
        deadline,
        MAX_STATUS_BYTES,
    )
    .map_err(|e| format!("git status: {e}"))?;
    let mut snapshot = Snapshot {
        head,
        branch: checked_out_branch(&git_dir),
        files: BTreeMap::new(),
        in_progress,
    };
    let mut bytes = 0_u64;
    for entry in listing.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|_| "a changed path is not UTF-8")?;
        let (code, path) = entry
            .split_at_checked(3)
            .filter(|(code, _)| code.ends_with(' '))
            .ok_or("an unexpected git status entry")?;
        let code = &code[..2];
        if matches!(code, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
            snapshot.in_progress = true;
        }
        // A nested repository is listed as a directory; it is not covered.
        if path.ends_with('/') {
            continue;
        }
        if path.len() > 512 || path.contains('\\') || path.chars().any(char::is_control) {
            return Err("a changed path cannot be named as a workspace file".into());
        }
        if let Some(seen) = snapshot.files.get_mut(path) {
            seen.codes.push(code.into());
            continue;
        }
        if snapshot.files.len() >= MAX_SNAPSHOT_FILES {
            return Err(format!(
                "more than {MAX_SNAPSHOT_FILES} files differ from HEAD"
            ));
        }
        if Instant::now() >= deadline {
            return Err("the hook deadline elapsed during the snapshot".into());
        }
        bytes += fs::symlink_metadata(root.join(path)).map_or(0, |m| m.len());
        if bytes > MAX_SNAPSHOT_BYTES {
            return Err("the changed files are larger than the snapshot bound".into());
        }
        let state = crate::hook::file_state(root, path).ok_or_else(unversioned)?;
        snapshot.files.insert(
            path.to_owned(),
            Entry {
                codes: vec![code.into()],
                state,
            },
        );
    }
    Ok(snapshot)
}

/// The branch `HEAD` names in the Git directory `git_dir`, as
/// `refs/heads/NAME`, read from its `HEAD` file (at most 1 KiB). `None` when
/// it is detached or cannot be read.
fn checked_out_branch(git_dir: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    fs::File::open(git_dir.join("HEAD"))
        .ok()?
        .take(1024)
        .read_to_string(&mut text)
        .ok()?;
    let reference = text.strip_prefix("ref: ")?.trim_end();
    (reference.len() > "refs/heads/".len()
        && reference.starts_with("refs/heads/")
        && !reference.chars().any(char::is_control))
    .then(|| reference.to_owned())
}

/// The checkout a run's Git command left (`Record::checkout`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutLeft {
    pub branch: Option<String>,
    pub head: Option<String>,
}

/// What a run whose command runs a Git operation that changes the checkout
/// (`moves_checkout`) left, when the checkout changed while it ran: another
/// branch or HEAD, or files changed with them in place (a restore or stash).
pub fn checkout_left(
    before: &Snapshot,
    after: &Snapshot,
    changed_files: bool,
) -> Option<CheckoutLeft> {
    (before.head != after.head || before.branch != after.branch || changed_files).then(|| {
        CheckoutLeft {
            branch: after.branch.clone(),
            head: after.head.clone(),
        }
    })
}

fn unversioned() -> String {
    "a changed file cannot be versioned (over 4 MiB, a symlink or not a regular file)".into()
}

/// The files whose content differs between two snapshots of one workspace.
///
/// - Listed in both: their content versions are compared.
/// - Listed only before: the file is now as in `HEAD`, and `current` (the
///   file on disk now) is compared with the version before.
/// - Listed only after: it was as in `HEAD` before. It changed when Git says
///   the file on disk now differs from `HEAD`: untracked, or one status
///   column clean. When both columns changed the index moved too, and
///   whether the file differs from `HEAD` is not known.
///
/// A moved `HEAD` changes what "as in `HEAD`" means, so the set is unknown.
pub fn changed(
    before: &Snapshot,
    after: &Snapshot,
    current: impl Fn(&str) -> Option<FileState>,
) -> Result<Vec<String>, String> {
    if before.head != after.head {
        return Err("HEAD moved during the command".into());
    }
    let paths: BTreeSet<&String> = before.files.keys().chain(after.files.keys()).collect();
    let mut changed = Vec::new();
    for path in paths {
        let differs = match (before.files.get(path), after.files.get(path)) {
            (Some(old), Some(new)) => old.state != new.state,
            (Some(old), None) => current(path).ok_or_else(unversioned)? != old.state,
            (None, Some(new)) => match new.codes.as_slice() {
                [code] if code == "??" || code.starts_with(' ') || code.ends_with(' ') => true,
                _ => {
                    return Err(
                        "Git's index changed for a file during the command, so whether its content changed is not known"
                            .into(),
                    );
                }
            },
            (None, None) => false,
        };
        if differs {
            changed.push(path.clone());
        }
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Run records: the "before" of each command, and what each run changed
// ---------------------------------------------------------------------------

/// The longest a shell call is supported to run: two hours, the longest
/// Claude Code lets a `run_in_background` command run (a foreground call
/// stops at ten minutes). Until then an unfinished run counts as running,
/// so the run records and edit marks it needs are kept; after it, a run
/// whose result never arrived is taken as abandoned, and a run whose result
/// does arrive is credited with nothing (`outlived`).
pub const RUN_LIFETIME_MS: u64 = 2 * 60 * 60 * 1000;
const RECORD_LIMIT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RECORDS: usize = 4096;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub session_id: String,
    /// The physical workspace the command ran in: its canonical root. Only
    /// runs of the same workspace can contest each other's changes, since
    /// changed paths are relative to it. `None` in a record written before
    /// this was kept, which is compared with every run.
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    pub started_ms: u64,
    pub admitted: bool,
    /// The workspace before the command, or why it could not be taken.
    pub before: Result<Snapshot, String>,
    pub finished_ms: Option<u64>,
    /// What the run changed, once finished; `None` when that is unknown.
    pub changed: Option<Vec<String>>,
    /// Once finished: why the change set is unknown, when it is.
    #[serde(default)]
    pub unobserved: Option<String>,
    /// Once finished: of `changed`, the paths attributed to no session.
    #[serde(default)]
    pub contested: Vec<String>,
    /// Finished and compared, but the daemon has not yet recorded the
    /// outcome. A redelivered post-tool hook records it then.
    #[serde(default)]
    pub pending: bool,
    /// The files the command names as literal write targets, when its whole
    /// write set parsed. Another session's overlapping run does not contest
    /// a change to one of these unless it names the file too.
    #[serde(default)]
    pub named: Vec<String>,
    /// Started in the background (Claude Code's `run_in_background`): it
    /// may still be running after its call returns, so until
    /// `RUN_LIFETIME_MS` after it started it contests other sessions' changes
    /// as a running command does.
    #[serde(default)]
    pub background: bool,
    /// The command runs `langspan commit --mine` or `langspan status
    /// --mine`: while it runs, that command may take its session from this
    /// record (ADR 0026).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub commit_mine: bool,
    /// Once finished: of `changed`, the paths another session's hooked edit
    /// tool made what they are now while this run was going. They are that
    /// session's (its own hooks recorded them), not this run's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited_elsewhere: Vec<String>,
    /// Once finished: of `contested`, the paths part of whose change only
    /// reformatted this session's own lines, journaled as this session's
    /// (issue #124).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reformatted: Vec<String>,
    /// The command line, redacted and capped (`kept_command`), for a later
    /// refusal to name. `None` in a record written before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The command runs a Git operation that changes the checkout
    /// (`moves_checkout`), read from the whole command line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moves_checkout: bool,
    /// Once finished: the checkout such a command left, when the checkout
    /// changed while it ran (issue #139).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<CheckoutLeft>,
}

impl Record {
    /// Of `changed`, the paths this run's session is credited with: neither
    /// contested nor another session's edit.
    pub fn attributed(&self, changed: &[String]) -> Vec<String> {
        changed
            .iter()
            .filter(|path| !self.contested.contains(path) && !self.edited_elsewhere.contains(path))
            .cloned()
            .collect()
    }

    /// Whether the run started more than `RUN_LIFETIME_MS` before `now`:
    /// the edit marks and run records it needed may have been pruned, so
    /// what it changed cannot be told from other sessions' changes.
    pub fn outlived(&self, now: u64) -> bool {
        now.saturating_sub(self.started_ms) >= RUN_LIFETIME_MS
    }
}

/// Whether a shell command runs `langspan commit --mine` or `langspan status
/// --mine`, which take their session from the running call.
pub fn runs_commit_mine(command: &str) -> bool {
    command.contains("langspan")
        && (command.contains("commit") || command.contains("status"))
        && command.contains("--mine")
}

/// The sessions with a `langspan commit --mine` (or `status --mine`)
/// command running now in the physical workspace `workspace`, from their
/// run records. A session's open
/// records are closed when its reply or the session ends (`close_session`),
/// so an ended session is not among them.
pub fn committing_sessions(state_dir: &Path, workspace: &Path) -> BTreeSet<String> {
    let now = now_ms();
    listing(state_dir)
        .into_iter()
        .filter(|(_, done, modified)| !done && now.saturating_sub(*modified) < RUN_LIFETIME_MS)
        .filter_map(|(path, _, _)| read_record(&path))
        .filter(|record| {
            record.commit_mine
                && record.finished_ms.is_none()
                && record.workspace.as_deref() == Some(workspace)
        })
        .map(|record| record.session_id)
        .collect()
}

/// Close the runs of `session_id` whose result never arrived, when the
/// session's reply or the session itself ends: a call the host refused after
/// the pre-tool hook (its permission check) sends no post event, and no call
/// of a session's reply is still running once the reply has ended. Each is
/// finished with its changes unknown, so until it is old it still contests
/// overlapping runs as an unobserved run does, but it no longer counts as
/// running (`committing_sessions`). Returns how many were closed.
pub fn close_session(state_dir: &Path, session_id: &str, why: &str) -> usize {
    let now = now_ms();
    let mut closed = 0;
    for (path, done, _) in listing(state_dir) {
        if done {
            continue;
        }
        let Some(record) = read_record(&path) else {
            continue;
        };
        if record.session_id != session_id || record.finished_ms.is_some() || record.pending {
            continue;
        }
        let Some(operation_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let mut run = record;
        run.finished_ms = Some(now);
        run.unobserved = Some(format!("the host sent no result for this command; {why}"));
        if finish(state_dir, operation_id, &run).is_ok() {
            closed += 1;
        }
    }
    closed
}

/// The identity of the physical workspace at `root`: its canonical path, or
/// `root` itself when it cannot be resolved.
pub fn workspace_identity(root: &Path) -> PathBuf {
    fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn records(state_dir: &Path) -> PathBuf {
    state_dir.join("shell")
}

fn record_path(state_dir: &Path, operation_id: &str, done: bool) -> PathBuf {
    records(state_dir).join(format!(
        "{operation_id}.{}",
        if done { "done" } else { "run" }
    ))
}

#[cfg(unix)]
fn write_record(path: &Path, record: &Record) -> Result<(), String> {
    use std::io::Write;
    let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut file =
        crate::local::private_file(&temporary, true).map_err(|e| format!("run record: {e}"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("run record: {e}"))?;
    fs::rename(&temporary, path).map_err(|e| format!("run record: {e}"))
}

#[cfg(not(unix))]
fn write_record(_: &Path, _: &Record) -> Result<(), String> {
    Err("shell observation requires Unix".into())
}

#[cfg(unix)]
fn read_record(path: &Path) -> Option<Record> {
    use std::io::Read;
    let file = crate::local::private_file(path, false).ok()?;
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > RECORD_LIMIT_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

#[cfg(not(unix))]
fn read_record(_: &Path) -> Option<Record> {
    None
}

/// Every record file in the directory, with its kind and modification time,
/// up to `MAX_RECORDS + 1` of them.
fn listing(state_dir: &Path) -> Vec<(PathBuf, bool, u64)> {
    let Ok(entries) = fs::read_dir(records(state_dir)) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .take(MAX_RECORDS + 1)
        .filter_map(|entry| {
            let path = entry.path();
            let done = match path.extension()?.to_str()? {
                "done" => true,
                "run" => false,
                _ => return None,
            };
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((path, done, modified))
        })
        .collect()
}

/// Record the start of a run. Old records go first: a finished run no
/// running one overlaps, and anything older than a day.
pub fn start(state_dir: &Path, operation_id: &str, record: &Record) -> Result<(), String> {
    #[cfg(unix)]
    crate::local::private_directory(&records(state_dir))
        .map_err(|e| format!("run records: {e}"))?;
    let now = now_ms();
    let entries = listing(state_dir);
    let earliest_running = entries
        .iter()
        .filter(|(_, done, modified)| !done && now.saturating_sub(*modified) < RUN_LIFETIME_MS)
        .map(|(_, _, modified)| *modified)
        .min()
        .unwrap_or(now);
    let mut kept = 0;
    for (path, done, modified) in entries {
        // A finished background command may still be running: its record is
        // kept while it can still contest.
        let background = done
            && now.saturating_sub(modified) < RUN_LIFETIME_MS
            && read_record(&path).is_some_and(|record| record.background);
        let expired = now.saturating_sub(modified) > DAY_MS
            || (done
                && !background
                && modified + 2_000 < earliest_running.min(now.saturating_sub(60_000)));
        if expired {
            let _ = fs::remove_file(path);
        } else {
            kept += 1;
        }
    }
    if kept >= MAX_RECORDS {
        return Err("too many shell runs are recorded".into());
    }
    write_record(&record_path(state_dir, operation_id, false), record)
}

/// The record of a run, running or finished.
pub fn load(state_dir: &Path, operation_id: &str) -> Option<Record> {
    read_record(&record_path(state_dir, operation_id, true))
        .or_else(|| read_record(&record_path(state_dir, operation_id, false)))
}

/// Keep a finished run's outcome, compared but not yet recorded by the
/// daemon, so that a retry records it instead of comparing again. Its
/// snapshot is dropped; the change set it keeps is bounded by the snapshot's.
pub fn computed(state_dir: &Path, operation_id: &str, record: &Record) -> Result<(), String> {
    let mut pending = record.clone();
    pending.before = Err("finished".into());
    pending.pending = true;
    write_record(&record_path(state_dir, operation_id, false), &pending)
}

/// Mark a run finished and its outcome recorded. Its snapshot is dropped.
pub fn finish(state_dir: &Path, operation_id: &str, record: &Record) -> Result<(), String> {
    let mut done = record.clone();
    done.before = Err("finished".into());
    done.pending = false;
    write_record(&record_path(state_dir, operation_id, true), &done)?;
    let _ = fs::remove_file(record_path(state_dir, operation_id, false));
    Ok(())
}

/// Of `changed`, the paths another session's shell run in the same physical
/// workspace could also have written: one that overlapped this run in time
/// and changed the same path, or whose changes are unknown (still running,
/// or not observed). Runs in other workspaces sharing the state directory
/// never contest: their paths name other files.
pub fn contested(
    state_dir: &Path,
    operation_id: &str,
    run: &Record,
    changed: &[String],
) -> Vec<String> {
    let now = now_ms();
    let mut contested = BTreeSet::new();
    for (path, _, _) in listing(state_dir) {
        if path.file_stem().and_then(|s| s.to_str()) == Some(operation_id) {
            continue;
        }
        let Some(other) = read_record(&path) else {
            continue;
        };
        if other.session_id == run.session_id || other.started_ms > now {
            continue;
        }
        if let (Some(theirs), Some(ours)) = (&other.workspace, &run.workspace)
            && theirs != ours
        {
            continue;
        }
        // A background command's call returning does not mean it stopped.
        let still_running = |started: u64| now.saturating_sub(started) < RUN_LIFETIME_MS;
        let overlapping = match other.finished_ms {
            Some(_) if other.background => still_running(other.started_ms),
            Some(finished) => finished >= run.started_ms,
            None => still_running(other.started_ms),
        };
        if !overlapping {
            continue;
        }
        // What this command names and the other does not is this command's.
        let theirs_too = |path: &&String| !run.named.contains(path) || other.named.contains(path);
        match (&other.finished_ms, &other.changed) {
            (Some(_), Some(theirs)) if !other.background => contested.extend(
                changed
                    .iter()
                    .filter(|path| theirs.contains(path))
                    .filter(theirs_too)
                    .cloned(),
            ),
            _ => contested.extend(changed.iter().filter(theirs_too).cloned()),
        }
    }
    contested.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Change notes: the last shell command that changed each file (issue #107)
// ---------------------------------------------------------------------------

/// The command text a run record and a change note keep, at most, in bytes,
/// after redaction.
pub const NOTE_COMMAND_BYTES: usize = 120;
/// Change notes kept per workspace, and files noted per run, at most.
const MAX_NOTES: usize = 4096;
pub const MAX_NOTED_PER_RUN: usize = 256;
const NOTE_LIMIT_BYTES: u64 = 4096;
const NOTE_LIFETIME_MS: u64 = 7 * DAY_MS;

/// The last shell command known to have changed one file: whose it was,
/// its command line (redacted and capped), and the version it left. A
/// refusal names it only while that version is still the one on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeNote {
    pub session_id: String,
    #[serde(default)]
    pub command: Option<String>,
    pub after: FileState,
    pub at_ms: u64,
}

/// A shell command line as a run record and change note keep it: likely
/// secrets redacted, deceptive characters replaced, at most
/// `NOTE_COMMAND_BYTES` bytes.
pub fn kept_command(command: &str) -> String {
    crate::hook::clean(&crate::hook::redact(command), NOTE_COMMAND_BYTES)
}

fn notes_dir(state_dir: &Path, root: &Path) -> PathBuf {
    let workspace = workspace_identity(root);
    state_dir.join("shell-changes").join(crate::hook::wire_id(
        "shell-changes",
        &[&workspace.to_string_lossy()],
    ))
}

fn note_path(directory: &Path, path: &str) -> PathBuf {
    directory.join(crate::hook::wire_id("shell-change", &[path]))
}

/// Note that `session_id`'s command changed each of `files` to the version
/// given, replacing any earlier note of the file. At most
/// `MAX_NOTED_PER_RUN` files are noted; the oldest notes beyond
/// `MAX_NOTES`, and any older than a week, are removed.
#[cfg(unix)]
pub fn note_changes(
    state_dir: &Path,
    root: &Path,
    session_id: &str,
    command: Option<&str>,
    files: &[(String, FileState)],
) -> Result<(), String> {
    use std::io::Write;
    if files.is_empty() {
        return Ok(());
    }
    let directory = notes_dir(state_dir, root);
    crate::local::private_directory(&state_dir.join("shell-changes"))
        .and_then(|()| crate::local::private_directory(&directory))
        .map_err(|e| format!("change notes: {e}"))?;
    let now = now_ms();
    for (path, after) in files.iter().take(MAX_NOTED_PER_RUN) {
        let note = ChangeNote {
            session_id: session_id.to_owned(),
            command: command.map(kept_command),
            after: after.clone(),
            at_ms: now,
        };
        let bytes = serde_json::to_vec(&note).map_err(|e| e.to_string())?;
        let target = note_path(&directory, path);
        let temporary = target.with_extension(format!("tmp-{}", std::process::id()));
        let _ = fs::remove_file(&temporary);
        let mut file = crate::local::private_file(&temporary, true)
            .map_err(|e| format!("change note: {e}"))?;
        file.write_all(&bytes)
            .map_err(|e| format!("change note: {e}"))?;
        fs::rename(&temporary, &target).map_err(|e| format!("change note: {e}"))?;
    }
    prune_notes(&directory, now);
    Ok(())
}

#[cfg(not(unix))]
pub fn note_changes(
    _: &Path,
    _: &Path,
    _: &str,
    _: Option<&str>,
    _: &[(String, FileState)],
) -> Result<(), String> {
    Err("shell observation requires Unix".into())
}

fn prune_notes(directory: &Path, now: u64) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut notes: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .take(MAX_NOTES * 2)
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            let at = modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
            Some((at, entry.path()))
        })
        .collect();
    notes.sort();
    let excess = notes.len().saturating_sub(MAX_NOTES);
    for (index, (at, path)) in notes.into_iter().enumerate() {
        if index < excess || now.saturating_sub(at) > NOTE_LIFETIME_MS {
            let _ = fs::remove_file(path);
        }
    }
}

/// The last shell command noted as changing `path` in the workspace at
/// `root`, when it is noted, readable and less than a week old.
pub fn change_note(state_dir: &Path, root: &Path, path: &str) -> Option<ChangeNote> {
    let file = note_path(&notes_dir(state_dir, root), path);
    #[cfg(unix)]
    let bytes = {
        use std::io::Read;
        let file = crate::local::private_file(&file, false).ok()?;
        let mut bytes = Vec::new();
        file.take(NOTE_LIMIT_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= NOTE_LIMIT_BYTES).then_some(bytes)?
    };
    #[cfg(not(unix))]
    let bytes: Vec<u8> = {
        let _ = file;
        return None;
    };
    let note: ChangeNote = serde_json::from_slice(&bytes).ok()?;
    (now_ms().saturating_sub(note.at_ms) <= NOTE_LIFETIME_MS).then_some(note)
}

// ---------------------------------------------------------------------------
// Edit marks: hooked edits a shell run must not take for its own
// ---------------------------------------------------------------------------

/// One file of an edit tool call: its version just before the call and, once
/// it finished, after it. `None` is a version that is not known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkedFile {
    pub before: Option<FileState>,
    pub after: Option<FileState>,
}

/// An edit tool call (Edit, Write, a patch) admitted in a workspace, kept so
/// that a shell run going at the same time can tell that call's changes from
/// its command's. Written by the call's pre-tool hook and finished by its
/// post-tool hook; a call whose post never arrives is closed with its result
/// unknown when its session's reply or the session ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditMark {
    pub session_id: String,
    pub workspace: PathBuf,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub files: BTreeMap<String, MarkedFile>,
}

fn marks(state_dir: &Path) -> PathBuf {
    records(state_dir).join("edits")
}

fn mark_path(state_dir: &Path, operation_id: &str) -> PathBuf {
    marks(state_dir).join(format!("{operation_id}.json"))
}

#[cfg(unix)]
fn write_mark(path: &Path, mark: &EditMark) -> Result<(), String> {
    use std::io::Write;
    let bytes = serde_json::to_vec(mark).map_err(|e| e.to_string())?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut file =
        crate::local::private_file(&temporary, true).map_err(|e| format!("edit mark: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("edit mark: {e}"))?;
    fs::rename(&temporary, path).map_err(|e| format!("edit mark: {e}"))
}

#[cfg(not(unix))]
fn write_mark(_: &Path, _: &EditMark) -> Result<(), String> {
    Err("edit marks require Unix".into())
}

#[cfg(unix)]
fn read_mark(path: &Path) -> Option<EditMark> {
    use std::io::Read;
    let file = crate::local::private_file(path, false).ok()?;
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > RECORD_LIMIT_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

#[cfg(not(unix))]
fn read_mark(_: &Path) -> Option<EditMark> {
    None
}

/// Every mark file with its modification time, up to `MAX_RECORDS + 1`.
fn mark_listing(state_dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(marks(state_dir)) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
        .take(MAX_RECORDS + 1)
        .filter_map(|entry| {
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((entry.path(), modified))
        })
        .collect()
}

/// Before an edit tool call runs, once it is not refused: keep the versions
/// of its files. Old marks go first: those no running shell run can still
/// need, and anything older than a day.
pub fn edit_started(
    state_dir: &Path,
    operation_id: &str,
    session_id: &str,
    root: &Path,
    before: BTreeMap<String, Option<FileState>>,
) -> Result<(), String> {
    #[cfg(unix)]
    crate::local::private_directory(&marks(state_dir)).map_err(|e| format!("edit marks: {e}"))?;
    let now = now_ms();
    // The earliest start of a shell run that may still be going.
    let earliest_running = listing(state_dir)
        .iter()
        .filter(|(_, _, modified)| now.saturating_sub(*modified) < RUN_LIFETIME_MS)
        .filter_map(|(path, _, _)| read_record(path))
        .filter(|record| record.finished_ms.is_none() || record.background)
        .map(|record| record.started_ms)
        .min()
        .unwrap_or(now);
    let mut kept = 0;
    for (path, modified) in mark_listing(state_dir) {
        let expired = now.saturating_sub(modified) > DAY_MS
            || (modified + 2_000 < earliest_running.min(now.saturating_sub(60_000))
                && read_mark(&path).is_none_or(|mark| mark.finished_ms.is_some()));
        if expired {
            let _ = fs::remove_file(path);
        } else {
            kept += 1;
        }
    }
    if kept >= MAX_RECORDS {
        return Err(mark_overflowed(state_dir, now));
    }
    let mark = EditMark {
        session_id: session_id.into(),
        workspace: workspace_identity(root),
        started_ms: now,
        finished_ms: None,
        files: before
            .into_iter()
            .map(|(path, before)| {
                (
                    path,
                    MarkedFile {
                        before,
                        after: None,
                    },
                )
            })
            .collect(),
    };
    write_mark(&mark_path(state_dir, operation_id), &mark)
}

/// Note that an edit call went unmarked because the marks are at their cap,
/// and say so. Until `RUN_LIFETIME_MS` after this, a shell run's overlapping
/// edits are incomplete (`overlapping_edits`). If even this note cannot be
/// written, the cap error says that too.
fn mark_overflowed(state_dir: &Path, now: u64) -> String {
    let refused = "too many edit tool calls are recorded";
    match write_overflow(&marks(state_dir).join(OVERFLOW), now) {
        Ok(()) => refused.into(),
        Err(error) => format!("{refused}; the overflow note failed too: {error}"),
    }
}

/// The file naming the last time an edit call went unmarked. It has no
/// `.json` extension, so it is never listed or counted as a mark.
const OVERFLOW: &str = "overflow";

#[cfg(unix)]
fn write_overflow(path: &Path, now: u64) -> Result<(), String> {
    use std::io::Write;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut file = crate::local::private_file(&temporary, true).map_err(|e| e.to_string())?;
    file.write_all(now.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    fs::rename(&temporary, path).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn write_overflow(_: &Path, _: u64) -> Result<(), String> {
    Err("edit marks require Unix".into())
}

/// When an edit call last went unmarked: `Ok(None)` if none has, `Err` when
/// the note exists but cannot be read (taken as a recent overflow).
#[cfg(unix)]
fn read_overflow(state_dir: &Path) -> Result<Option<u64>, ()> {
    use std::io::Read;
    let path = marks(state_dir).join(OVERFLOW);
    if fs::symlink_metadata(&path).is_err() {
        return Ok(None);
    }
    let file = crate::local::private_file(&path, false).map_err(|_| ())?;
    let mut text = String::new();
    file.take(32).read_to_string(&mut text).map_err(|_| ())?;
    text.trim().parse().map(Some).map_err(|_| ())
}

#[cfg(not(unix))]
fn read_overflow(_: &Path) -> Result<Option<u64>, ()> {
    Ok(None)
}

/// After an edit tool call: its files as they are now. A call whose start was
/// not kept is marked as starting now from versions that are not known,
/// unless the marks are at their cap: then it is not marked, and the
/// overflow is noted instead.
pub fn edit_finished(
    state_dir: &Path,
    operation_id: &str,
    session_id: &str,
    root: &Path,
    after: BTreeMap<String, Option<FileState>>,
) -> Result<(), String> {
    let path = mark_path(state_dir, operation_id);
    let now = now_ms();
    let mut mark = match read_mark(&path).filter(|mark| mark.session_id == session_id) {
        Some(mark) => mark,
        None => {
            #[cfg(unix)]
            crate::local::private_directory(&marks(state_dir))
                .map_err(|e| format!("edit marks: {e}"))?;
            // A new mark file: bounded like `edit_started`, so that the
            // directory never holds more marks than a listing reads.
            if !path.exists() && mark_listing(state_dir).len() >= MAX_RECORDS {
                return Err(mark_overflowed(state_dir, now));
            }
            EditMark {
                session_id: session_id.into(),
                workspace: workspace_identity(root),
                started_ms: now,
                finished_ms: None,
                files: BTreeMap::new(),
            }
        }
    };
    if mark.finished_ms.is_some() {
        return Ok(());
    }
    mark.finished_ms = Some(now);
    for (file, state) in after {
        mark.files
            .entry(file)
            .or_insert(MarkedFile {
                before: None,
                after: None,
            })
            .after = state;
    }
    #[cfg(unix)]
    crate::local::private_directory(&marks(state_dir)).map_err(|e| format!("edit marks: {e}"))?;
    write_mark(&path, &mark)
}

/// Close `session_id`'s edit calls whose result never arrived: finished now,
/// with what they left unknown. Returns how many were closed.
pub fn close_edits(state_dir: &Path, session_id: &str) -> usize {
    let now = now_ms();
    let mut closed = 0;
    for (path, _) in mark_listing(state_dir) {
        let Some(mut mark) = read_mark(&path) else {
            continue;
        };
        if mark.session_id != session_id || mark.finished_ms.is_some() {
            continue;
        }
        mark.finished_ms = Some(now);
        if write_mark(&path, &mark).is_ok() {
            closed += 1;
        }
    }
    closed
}

/// The edit marks a shell run is compared with, and whether they can be
/// missing an edit that overlapped it.
#[derive(Debug, Default)]
pub struct OverlappingEdits {
    pub marks: Vec<EditMark>,
    /// An edit call may have gone unmarked while the run was going, or more
    /// marks are kept than were read: a changed file no mark names may still
    /// be another session's, so none of the run's changes can be vouched for.
    pub incomplete: bool,
}

/// Every kept edit mark of another session than `run`'s, in its workspace,
/// that overlapped it in time: finished after it started, or still going
/// and not abandoned. Incomplete when the directory holds more marks than
/// are read, or an edit call went unmarked at the cap recently enough to
/// have been going during the run (within `RUN_LIFETIME_MS` before its start,
/// as an unfinished mark would count).
pub fn overlapping_edits(state_dir: &Path, run: &Record) -> OverlappingEdits {
    let now = now_ms();
    let listing = mark_listing(state_dir);
    let overflowed = match read_overflow(state_dir) {
        Ok(None) => false,
        Ok(Some(at)) => at.saturating_add(RUN_LIFETIME_MS) > run.started_ms,
        Err(()) => true,
    };
    let incomplete = overflowed || listing.len() > MAX_RECORDS;
    let marks = listing
        .into_iter()
        .filter_map(|(path, _)| read_mark(&path))
        .filter(|mark| {
            mark.session_id != run.session_id
                && run
                    .workspace
                    .as_ref()
                    .is_none_or(|ours| *ours == mark.workspace)
                && match mark.finished_ms {
                    Some(finished) => finished >= run.started_ms,
                    None => now.saturating_sub(mark.started_ms) < RUN_LIFETIME_MS,
                }
        })
        .collect();
    OverlappingEdits { marks, incomplete }
}

/// Of `changed`, the paths other sessions' edit calls (`marks`) touched while
/// the run was going: `(theirs, unknown)`.
///
/// A path is theirs when its edits, in the order they started, form one
/// chain of known versions from the file before the run (`before`) to the
/// file now (`current`): no other change came between them, so the command
/// did not change it. Otherwise both the command and an edit may have
/// changed it, or an edit is still going and the order cannot be known: it
/// is unknown, and the run's session is not credited with it.
pub fn classify_edits(
    marks: &[EditMark],
    changed: &[String],
    before: impl Fn(&str) -> Option<FileState>,
    current: impl Fn(&str) -> Option<FileState>,
) -> (Vec<String>, Vec<String>) {
    let mut theirs = Vec::new();
    let mut unknown = Vec::new();
    for path in changed {
        let mut edits: Vec<(&EditMark, &MarkedFile)> = marks
            .iter()
            .filter_map(|mark| mark.files.get(path).map(|file| (mark, file)))
            .collect();
        if edits.is_empty() {
            continue;
        }
        edits.sort_by_key(|(mark, _)| mark.started_ms);
        let chain = || -> Option<()> {
            let mut version = before(path)?;
            for (mark, file) in &edits {
                mark.finished_ms?;
                if file.before.as_ref()? != &version {
                    return None;
                }
                version = file.after.clone()?;
            }
            (current(path)? == version).then_some(())
        };
        if chain().is_some() {
            theirs.push(path.clone());
        } else {
            unknown.push(path.clone());
        }
    }
    (theirs, unknown)
}

#[cfg(test)]
mod parser {
    use super::*;

    fn targets(command: &str) -> Vec<String> {
        write_targets(command).unwrap_or_else(|e| panic!("{command}: {e}"))
    }

    fn unknown(command: &str) {
        assert!(
            write_targets(command).is_err(),
            "{command} should be unknown, got {:?}",
            write_targets(command)
        );
    }

    #[test]
    fn each_supported_form_names_its_targets() {
        assert_eq!(targets("printf x > b.txt"), ["b.txt"]);
        assert_eq!(targets("echo hi >> log.txt"), ["log.txt"]);
        assert_eq!(targets("echo hi>b.txt"), ["b.txt"]);
        assert_eq!(targets("> empty.txt"), ["empty.txt"]);
        assert_eq!(
            targets("cat a.txt | tee -a b.txt c.txt"),
            ["b.txt", "c.txt"]
        );
        assert_eq!(targets("sed -i 's/a/b/' a.txt"), ["a.txt"]);
        assert_eq!(targets("sed -i '' 's/a/b/' a.txt"), ["a.txt"]);
        assert_eq!(
            targets("sed -i.bak -e 's/a/b/g' a.txt"),
            ["a.txt", "a.txt.bak"]
        );
        assert_eq!(targets("sed -n -i '1,3d;$p' a.txt"), ["a.txt"]);
        assert_eq!(targets("mv old.rs new.rs"), ["new.rs", "old.rs"]);
        assert_eq!(targets("cp -f a.txt b.txt"), ["b.txt"]);
        assert_eq!(targets("rm -f a.txt b.txt"), ["a.txt", "b.txt"]);
        assert_eq!(targets("touch -c a.txt"), ["a.txt"]);
        assert_eq!(targets("truncate -s 0 a.txt"), ["a.txt"]);
        assert_eq!(targets("truncate --size=10 a.txt"), ["a.txt"]);
        assert!(targets("true; echo done").is_empty());
    }

    #[test]
    fn chains_of_supported_forms_and_read_only_commands_are_a_union() {
        assert_eq!(
            targets("echo a > x.txt && sed -i s/a/b/ y.txt; cat x.txt | tee z.txt"),
            ["x.txt", "y.txt", "z.txt"]
        );
        assert!(targets("echo hello").is_empty());
        assert!(targets("cat a.txt | sed s/a/b/").is_empty());
        assert_eq!(targets("sed -i s/a/b/ a.txt 2>/dev/null"), ["a.txt"]);
        assert_eq!(targets("echo x > out.txt 2>&1"), ["out.txt"]);
    }

    #[test]
    fn quoting_is_literal_and_dot_segments_are_removed() {
        assert_eq!(targets("echo x > 'my file.txt'"), ["my file.txt"]);
        assert_eq!(targets(r#"echo "a;b" > "c d.txt""#), ["c d.txt"]);
        assert_eq!(targets("echo x > ./src/./a.txt"), ["src/a.txt"]);
        assert_eq!(targets("echo x > /tmp/abs.txt"), ["/tmp/abs.txt"]);
        // A quoted `2` is a word, not a descriptor.
        assert_eq!(targets("echo '2'> f.txt"), ["f.txt"]);
        // Single quotes keep `$` and `*` literal, so the script is fine.
        assert_eq!(targets("sed -i 's/a*$/b/' a.txt"), ["a.txt"]);
    }

    #[test]
    fn every_disqualifier_gives_no_decision() {
        for command in [
            "echo $HOME > a.txt",
            "echo x > $OUT",
            "echo x > \"$OUT\"",
            "echo $(date) > a.txt",
            "echo `date` > a.txt",
            "rm *.txt",
            "rm a?.txt",
            "rm a[12].txt",
            "touch {a,b}.txt",
            "echo x > ~/a.txt",
            "cd src && echo x > a.txt",
            "(echo x > a.txt)",
            "eval 'echo x > a.txt'",
            "ls | xargs rm",
            "for f in *.txt; do sed -i s/a/b/ $f; done",
            "echo x > a.txt &",
            "false || echo x > a.txt",
            "echo x |& tee a.txt",
            "echo x &> a.txt",
            "cat < a.txt > b.txt",
            "cat <<EOF > a.txt\nit's here\nEOF",
            "echo x > a.txt\necho y > b.txt",
            "echo x > ../outside.txt",
            "echo x > a\\ b.txt",
            "echo 'unterminated > a.txt",
            "FOO=1 echo x > a.txt",
            "echo x >",
            "echo x 3> a.txt",
            "echo x >| a.txt",
            "python3 fix.py",
            "./x.sh",
            "npm run format",
            "cargo fmt",
            "rm -rf build",
            "cp -r a b",
            "mv a b c/",
            "rm",
            "sed -i 's/a/b/w other.txt' a.txt",
            "sed -i 's/a/b/e' a.txt",
            "sed -i 'w other.txt' a.txt",
            "sed -i -f script.sed a.txt",
            "sed -i 's/a/b/'",
            "sed -i.x/y s/a/b/ a.txt",
            "truncate a.txt",
            "touch -t 202601010000 a.txt",
            "echo x > a.txt # comment",
            "! echo x > a.txt",
            ";",
            "",
        ] {
            unknown(command);
        }
    }

    #[test]
    fn a_heredoc_body_is_never_read_as_shell() {
        // An apostrophe in the body must not open a quote that swallows the
        // rest: the parse stops at `<<`, before the body.
        assert_eq!(
            write_targets("cat <<'EOF' > a.txt\ndon't\nEOF"),
            Err("input redirection, a heredoc or process substitution")
        );
    }

    #[test]
    fn very_long_input_is_bounded_and_never_panics() {
        unknown(&format!("echo {} > a.txt", "x".repeat(MAX_COMMAND_BYTES)));
        let many: String = (0..65).map(|n| format!("touch f{n}.txt; ")).collect();
        assert!(many.len() < MAX_COMMAND_BYTES);
        unknown(&many);
        for probe in ["'", "\"", ">", ">>", "2>&", "&", "|", "sed", "sed -i", "s/"] {
            let _ = write_targets(probe);
            let _ = write_targets(&probe.repeat(3000));
        }
        let _ = write_targets("é\u{2028}> \u{0}");
    }
}

#[cfg(all(test, unix))]
mod change_notes {
    use super::*;

    fn present(digest: &str) -> FileState {
        FileState::Present {
            digest: digest.repeat(64),
            kind: langspan_core::FileKind::Regular,
        }
    }

    fn temporary() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "ls-change-notes-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    /// A note is kept per workspace and file, the latest replaces the
    /// last, its command is redacted and capped, and a week-old note is
    /// neither read nor kept.
    #[test]
    fn a_note_is_the_latest_per_file_and_workspace_redacted_and_expires() {
        let base = temporary();
        let (state, one, two) = (base.join("state"), base.join("one"), base.join("two"));
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        let long = format!(
            "TOKEN=ghp_0123456789abcdefghijABCDEFGHIJ012345 {}",
            "y".repeat(300)
        );
        note_changes(
            &state,
            &one,
            "s.a",
            Some("first"),
            &[("f".into(), present("a"))],
        )
        .unwrap();
        note_changes(
            &state,
            &one,
            "s.b",
            Some(&long),
            &[("f".into(), present("b"))],
        )
        .unwrap();
        let note = change_note(&state, &one, "f").unwrap();
        assert_eq!(
            (note.session_id.as_str(), &note.after),
            ("s.b", &present("b"))
        );
        let command = note.command.unwrap();
        assert!(command.starts_with("TOKEN=[redacted] y"), "{command}");
        assert!(command.len() <= NOTE_COMMAND_BYTES && command.ends_with('…'));
        assert!(
            change_note(&state, &two, "f").is_none(),
            "another workspace"
        );
        assert!(change_note(&state, &one, "g").is_none(), "another file");

        // An expired note is not read, and the next write removes it.
        let directory = notes_dir(&state, &one);
        let path = note_path(&directory, "f");
        let old = ChangeNote {
            at_ms: now_ms() - NOTE_LIFETIME_MS - 1,
            ..change_note(&state, &one, "f").unwrap()
        };
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(change_note(&state, &one, "f").is_none());
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(
                SystemTime::now() - std::time::Duration::from_millis(NOTE_LIFETIME_MS + 60_000),
            )
            .unwrap();
        note_changes(&state, &one, "s.a", None, &[("g".into(), present("c"))]).unwrap();
        assert!(!path.exists());
        assert_eq!(change_note(&state, &one, "g").unwrap().command, None);
        let _ = fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod diff {
    use super::*;

    fn present(digest: &str) -> FileState {
        FileState::Present {
            digest: digest.into(),
            kind: langspan_core::FileKind::Regular,
        }
    }

    fn snap(files: &[(&str, &str, FileState)]) -> Snapshot {
        Snapshot {
            head: Some("h".into()),
            branch: None,
            files: files
                .iter()
                .map(|(path, code, state)| {
                    (
                        (*path).to_owned(),
                        Entry {
                            codes: vec![(*code).into()],
                            state: state.clone(),
                        },
                    )
                })
                .collect(),
            in_progress: false,
        }
    }

    #[test]
    fn content_decides_and_a_file_git_now_lists_differs_from_head() {
        let before = snap(&[
            ("dirty.txt", " M", present("1")),
            ("same.txt", " M", present("2")),
            ("staged.txt", " M", present("5")),
        ]);
        let after = snap(&[
            ("dirty.txt", " M", present("3")),
            ("same.txt", " M", present("2")),
            // Only the index moved: `git add` changes no content.
            ("staged.txt", "M ", present("5")),
            // A clean tracked file deleted, a clean one edited, a new file,
            // and a staged-then-written one.
            ("gone.txt", " D", FileState::Missing),
            ("edited.txt", " M", present("6")),
            ("new.txt", "??", present("4")),
            ("added.txt", "A ", present("7")),
        ]);
        let disk = |_: &str| None;
        assert_eq!(
            changed(&before, &after, disk).unwrap(),
            [
                "added.txt",
                "dirty.txt",
                "edited.txt",
                "gone.txt",
                "new.txt"
            ]
        );
        assert!(changed(&before, &before, disk).unwrap().is_empty());
    }

    #[test]
    fn a_file_no_longer_listed_is_compared_with_its_content_on_disk() {
        let before = snap(&[
            ("reverted.txt", " M", present("1")),
            ("new.txt", "??", present("2")),
        ]);
        let after = snap(&[]);
        // reverted.txt is back to HEAD's content; new.txt became ignored
        // with the same content, so it did not change.
        let disk = |path: &str| {
            Some(if path == "new.txt" {
                present("2")
            } else {
                present("head")
            })
        };
        assert_eq!(changed(&before, &after, disk).unwrap(), ["reverted.txt"]);
        assert!(
            changed(&before, &after, |_| None).is_err(),
            "unreadable now: unknown"
        );
    }

    fn edit(
        started_ms: u64,
        finished: Option<u64>,
        files: &[(&str, Option<&str>, Option<&str>)],
    ) -> EditMark {
        EditMark {
            session_id: "peer".into(),
            workspace: PathBuf::from("/w"),
            started_ms,
            finished_ms: finished,
            files: files
                .iter()
                .map(|(path, before, after)| {
                    (
                        (*path).to_owned(),
                        MarkedFile {
                            before: before.map(present),
                            after: after.map(present),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn an_edit_that_alone_explains_a_change_is_the_editors_and_anything_else_is_unknown() {
        let marks = [
            // Edited from the run's "before" to what is on disk now.
            edit(1, Some(2), &[("theirs.txt", Some("0"), Some("1"))]),
            // Two edits in a row, each from the last one's result.
            edit(1, Some(2), &[("chain.txt", Some("0"), Some("1"))]),
            edit(3, Some(4), &[("chain.txt", Some("1"), Some("2"))]),
            // Changed again after the edit: by the command, or someone.
            edit(1, Some(2), &[("after.txt", Some("0"), Some("1"))]),
            // Changed before the edit started.
            edit(1, Some(2), &[("before.txt", Some("5"), Some("1"))]),
            // Still going.
            edit(1, None, &[("going.txt", Some("0"), None)]),
            // Finished with what it left unknown (closed with no result).
            edit(1, Some(2), &[("closed.txt", Some("0"), None)]),
            // A start that was not kept.
            edit(1, Some(2), &[("unkept.txt", None, Some("1"))]),
        ];
        let changed: Vec<String> = [
            "after.txt",
            "before.txt",
            "chain.txt",
            "closed.txt",
            "going.txt",
            "mine.txt",
            "theirs.txt",
            "unkept.txt",
        ]
        .map(String::from)
        .into();
        let now = |path: &str| {
            Some(present(match path {
                "chain.txt" => "2",
                "after.txt" => "9",
                _ => "1",
            }))
        };
        let (theirs, unknown) = classify_edits(&marks, &changed, |_| Some(present("0")), now);
        assert_eq!(theirs, ["chain.txt", "theirs.txt"]);
        assert_eq!(
            unknown,
            [
                "after.txt",
                "before.txt",
                "closed.txt",
                "going.txt",
                "unkept.txt"
            ]
        );
        // The version before the run is not known: no chain can be shown.
        let (theirs, unknown) = classify_edits(&marks[..1], &changed, |_| None, now);
        assert!(theirs.is_empty());
        assert_eq!(unknown, ["theirs.txt"]);
        // No edit touched a path: nothing is said about it.
        assert_eq!(
            classify_edits(&[], &changed, |_| None, now),
            (vec![], vec![])
        );
    }

    #[test]
    fn a_moved_head_or_an_index_only_listing_makes_the_change_set_unknown() {
        let before = snap(&[]);
        let mut after = snap(&[]);
        after.head = Some("other".into());
        assert!(changed(&before, &after, |_| None).is_err());
        // Both columns changed: content compared with HEAD is unknown.
        let after = snap(&[("both.txt", "MM", present("1"))]);
        assert!(changed(&before, &after, |_| None).is_err());
        // `git rm --cached`: deleted from the index and now untracked.
        let mut after = snap(&[("kept.txt", "D ", present("1"))]);
        after
            .files
            .get_mut("kept.txt")
            .unwrap()
            .codes
            .push("??".into());
        assert!(changed(&before, &after, |_| None).is_err());
    }

    /// The marks a shell run is compared with are incomplete when an edit
    /// went unmarked at the cap within a shell call's lifetime of the run,
    /// or the directory holds more marks than a listing reads.
    #[cfg(unix)]
    #[test]
    fn overflowing_edit_marks_make_a_runs_overlapping_edits_incomplete() {
        let state = std::env::temp_dir().join(format!(
            "ls-marks-overflow-{}-{}",
            std::process::id(),
            now_ms()
        ));
        crate::local::private_directory(&state).unwrap();
        crate::local::private_directory(&marks(&state)).unwrap();
        let now = now_ms();
        let run = Record {
            session_id: "shell".into(),
            workspace: Some(PathBuf::from("/w")),
            started_ms: now,
            admitted: false,
            before: Err("test".into()),
            finished_ms: None,
            changed: None,
            unobserved: None,
            contested: Vec::new(),
            pending: false,
            named: Vec::new(),
            background: false,
            commit_mine: false,
            edited_elsewhere: Vec::new(),
            reformatted: Vec::new(),
            command: None,
            moves_checkout: false,
            checkout: None,
        };
        let note = marks(&state).join(OVERFLOW);
        assert!(!overlapping_edits(&state, &run).incomplete);

        // An overflow longer than a shell call's lifetime before the run.
        write_overflow(&note, now - RUN_LIFETIME_MS - 1).unwrap();
        assert!(!overlapping_edits(&state, &run).incomplete);
        // One the run may have overlapped, and one while it ran.
        write_overflow(&note, now - RUN_LIFETIME_MS + 1_000).unwrap();
        assert!(overlapping_edits(&state, &run).incomplete);
        write_overflow(&note, now + 1).unwrap();
        assert!(overlapping_edits(&state, &run).incomplete);
        // An unreadable note counts as a recent overflow.
        fs::write(&note, "not a time").unwrap();
        assert!(overlapping_edits(&state, &run).incomplete);
        fs::remove_file(&note).unwrap();

        // More marks than a listing reads, as an older build could leave.
        let mark = edit(now, None, &[]);
        for n in 0..=MAX_RECORDS {
            write_mark(&mark_path(&state, &format!("m{n}")), &mark).unwrap();
        }
        assert!(overlapping_edits(&state, &run).incomplete);
        // A late post hook at the cap adds no mark and notes the overflow.
        fs::remove_file(mark_path(&state, "m0")).unwrap();
        let after = BTreeMap::from([("a.txt".to_owned(), Some(present("1")))]);
        let error = edit_finished(&state, "late", "peer", Path::new("/w"), after).unwrap_err();
        assert!(error.contains("too many edit tool calls"), "{error}");
        assert!(!mark_path(&state, "late").exists());
        assert!(read_overflow(&state).unwrap().is_some_and(|at| at >= now));
        fs::remove_dir_all(&state).unwrap();
    }
}
