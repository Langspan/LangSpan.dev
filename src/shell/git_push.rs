//! The `git push` in a shell line
//! ([ADR 0007](../../docs/adr/0007-check-evidence-integration-and-landing.md#routing-an-agents-push)).
//!
//! LangSpan does not work out where a push goes: it asks Git, with a dry run
//! of the same push. So only a `git push` run on its own is read. A push
//! together with any other command, behind a prefix such as `env`, or with
//! words a dry run could not repeat exactly or safely, is an error: the
//! hook refuses it with what to run instead.

use super::git_commands::{Lexer, MAX_GIT_COMMAND_BYTES, Word, command_words};
use std::path::PathBuf;

/// One `git push`, as the dry run repeats it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GitPush {
    /// Each `git -C DIR`, joined in order; relative to the call's directory.
    pub(crate) cwd: Option<PathBuf>,
    /// Each `git -c KEY=VALUE`, in order.
    pub(crate) config: Vec<String>,
    /// The words after `push`, as Git receives them, without `-q`.
    pub(crate) args: Vec<String>,
}

/// Why LangSpan refuses a command with a `git push`, and what to run instead.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) String);

impl From<String> for Refused {
    fn from(why: String) -> Self {
        Self(why)
    }
}

impl From<&str> for Refused {
    fn from(why: &str) -> Self {
        Self(why.to_owned())
    }
}

const ALONE: &str = "run `git push` as a command of its own: no `&&`, `;`, `||`, pipe, subshell, `cd` or prefix such as `env`";

/// The `git push` that `command` runs; `None` when it runs none, or only a
/// dry run.
pub(crate) fn git_push(command: &str) -> Result<Option<GitPush>, Refused> {
    if !(command.contains("git") && command.contains("push")) {
        return Ok(None);
    }
    if command.len() > MAX_GIT_COMMAND_BYTES {
        return Err(
            format!("this command is longer than LangSpan reads and may push; {ALONE}").into(),
        );
    }
    let segments = Lexer::new(command)
        .segments()
        .map_err(|why| format!("LangSpan could not read this command ({why}); {ALONE}"))?;
    if !segments
        .iter()
        .any(|words| push_at(command_words(words)).is_some())
    {
        return Ok(None);
    }
    let [words] = segments.as_slice() else {
        return Err(format!(
            "this command runs `git push` together with another command, so LangSpan cannot check where it pushes; {ALONE}"
        ).into());
    };
    if command_words(words).len() != words.len() {
        return Err(format!(
            "this `git push` runs behind a prefix, which a dry run would not repeat; {ALONE}"
        )
        .into());
    }
    lone(words)
}

/// Where `push` is in a `git …` command's words, past Git's own options.
fn push_at(words: &[Word]) -> Option<usize> {
    let first = words.first()?;
    if !(first.text == "git" || first.text.ends_with("/git")) {
        return None;
    }
    let mut at = 1;
    while let Some(word) = words.get(at) {
        match word.text.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => at += 2,
            text if text.starts_with('-') => at += 1,
            "push" => return Some(at),
            _ => return None,
        }
    }
    None
}

/// A `-c` setting the dry run may carry: where the push goes, and nothing
/// that runs a program (`core.sshCommand`, `credential.helper`,
/// `remote.NAME.receivepack`, `protocol.*.allow`, `include.path`).
fn destination_setting(setting: &str) -> bool {
    let key = setting.split_once('=').map_or(setting, |(key, _)| key);
    let key = key.to_ascii_lowercase();
    let parts: Vec<&str> = key.split('.').collect();
    match (parts.first(), parts.len(), parts.last()) {
        (Some(&"push"), 2, _) | (Some(&"remote"), 2, Some(&"pushdefault")) => true,
        (Some(&"remote"), 3.., Some(name)) => {
            matches!(*name, "push" | "mirror" | "url" | "pushurl")
        }
        (Some(&"branch"), 3.., Some(name)) => matches!(*name, "remote" | "pushremote" | "merge"),
        _ => false,
    }
}

fn lone(words: &[Word]) -> Result<Option<GitPush>, Refused> {
    let Some(at) = push_at(words) else {
        return Ok(None);
    };
    let mut texts = Vec::with_capacity(words.len());
    for word in words {
        texts.push(word.literal().ok_or_else(|| {
            "a word of this `git push` holds an expansion (`$`, a backquote or an unquoted glob) whose value LangSpan does not know; write it out".to_owned()
        })?);
    }
    let mut push = GitPush::default();
    let mut global = texts.iter().take(at).skip(1);
    while let Some(option) = global.next() {
        let missing = || format!("`git {option}` without its value");
        match *option {
            "-C" => {
                let dir = global.next().ok_or_else(missing)?;
                push.cwd = Some(match push.cwd.take() {
                    Some(cwd) => cwd.join(dir),
                    None => PathBuf::from(dir),
                });
            }
            "-c" => {
                let setting = global.next().ok_or_else(missing)?;
                if !destination_setting(setting) {
                    return Err("this push sets something with `git -c` other than where it goes (push.*, remote.*, branch.*), and LangSpan runs a dry run of the push before it runs; set it with `git config`, or leave it out".into());
                }
                push.config.push((*setting).to_owned());
            }
            "--no-pager" | "-P" => {}
            _ => {
                return Err(
                    "this push has a Git option before `push` other than `-C` or `-c`; run it without".into(),
                );
            }
        }
    }
    let (mut options_end, mut value_next) = (false, false);
    for arg in texts.iter().skip(at + 1) {
        let arg = *arg;
        if options_end || value_next || !arg.starts_with('-') || arg == "-" {
            push.args.push(arg.to_owned());
            value_next = false;
            continue;
        }
        match arg {
            "--" => options_end = true,
            "-n" | "--dry-run" => return Ok(None),
            // A dry run must say which refs it updates.
            "-q" | "--quiet" => continue,
            "-o" | "--push-option" => value_next = true,
            _ if arg.starts_with("--receive-pack") || arg.starts_with("--exec") => {
                return Err("this push names the program to run on the remote side (`--receive-pack`, `--exec`), which LangSpan does not run in a dry run; set `remote.NAME.receivepack` with `git config` instead".into());
            }
            _ if arg.starts_with("--") => {}
            _ => {
                // A cluster of short options, such as `-fu` or `-qoci.skip`:
                // after `o`, the rest is its value.
                let letters = arg.trim_start_matches('-');
                let (flags, value) = letters.split_at(letters.find('o').unwrap_or(letters.len()));
                if flags.contains('n') {
                    return Ok(None);
                }
                let flags: String = flags.chars().filter(|letter| *letter != 'q').collect();
                value_next = value == "o";
                if !(flags.is_empty() && value.is_empty()) {
                    push.args.push(format!("-{flags}{value}"));
                }
                continue;
            }
        }
        push.args.push(arg.to_owned());
    }
    Ok(Some(push))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &str) -> Vec<String> {
        git_push(command).unwrap().unwrap().args
    }

    #[test]
    fn a_lone_push_keeps_its_words_for_the_dry_run() {
        assert_eq!(args("git push"), Vec::<String>::new());
        assert_eq!(
            args("git push --prune origin 'refs/heads/*:refs/heads/*'"),
            ["--prune", "origin", "refs/heads/*:refs/heads/*"]
        );
        assert_eq!(
            args("/usr/bin/git push -fu origin HEAD"),
            ["-fu", "origin", "HEAD"]
        );
        assert_eq!(args("git push origin main 2>&1"), ["origin", "main"]);
        assert_eq!(args("git push -o -q origin x"), ["-o", "-q", "origin", "x"]);
        assert_eq!(args("git push origin -- -q"), ["origin", "--", "-q"]);
        let push = git_push("git -C a -C b -c push.default=matching --no-pager push")
            .unwrap()
            .unwrap();
        assert_eq!(push.cwd, Some(PathBuf::from("a/b")));
        assert_eq!(push.config, ["push.default=matching"]);
    }

    /// A quiet dry run says nothing about the refs it would update.
    #[test]
    fn quiet_is_dropped_and_a_dry_run_is_no_push() {
        assert_eq!(args("git push -q origin main"), ["origin", "main"]);
        assert_eq!(args("git push --quiet origin main"), ["origin", "main"]);
        assert_eq!(args("git push -qf origin main"), ["-f", "origin", "main"]);
        assert_eq!(args("git push -qoci.skip origin"), ["-oci.skip", "origin"]);
        for command in [
            "git push -n origin main",
            "git push --dry-run origin main",
            "git push -fn origin main",
            "git status",
            "echo push",
            "git commit -m 'then push'",
            "git log --grep push && git status",
        ] {
            assert_eq!(git_push(command), Ok(None), "{command}");
        }
    }

    #[test]
    fn a_push_with_another_command_or_a_prefix_is_refused() {
        for command in [
            "git add -A && git push origin main",
            "git push origin feat && echo done",
            "git push origin main; true",
            "git push || true",
            "git push origin main | tee log",
            "cd sub && git push",
            "(cd sub; git push)",
            "{ git push; }",
            "env GIT_TRACE=1 git push origin main",
            "GIT_DIR=/x git push origin main",
            "if true; then git push origin main; fi",
        ] {
            let why = git_push(command).unwrap_err().0;
            assert!(why.contains("command of its own"), "{command}: {why}");
        }
    }

    #[test]
    fn what_a_dry_run_could_not_repeat_safely_is_refused() {
        for (command, says) in [
            ("git push origin $BRANCH", "expansion"),
            ("git push \"$(git remote)\" main", "expansion"),
            ("git -c core.sshCommand=evil push", "git config"),
            ("git -c credential.helper=x push", "git config"),
            ("git -c remote.origin.receivepack=x push", "git config"),
            ("git -c include.path=/x push", "git config"),
            (
                "git --git-dir=/elsewhere/.git push origin main",
                "other than `-C`",
            ),
            ("git push --receive-pack=evil origin main", "receivepack"),
            ("git push --exec evil origin main", "receivepack"),
            ("git push 'unterminated", "could not read"),
        ] {
            let why = git_push(command).unwrap_err().0;
            assert!(why.contains(says), "{command}: {why}");
        }
        for setting in [
            "push.default=current",
            "remote.pushDefault=fork",
            "remote.origin.push=+refs/heads/*:refs/heads/*",
            "remote.my.fork.mirror=true",
            "branch.feat.pushRemote=fork",
        ] {
            assert!(destination_setting(setting), "{setting}");
        }
        let long = format!("git push origin {}", "a".repeat(MAX_GIT_COMMAND_BYTES));
        assert!(git_push(&long).unwrap_err().0.contains("longer"));
    }
}
