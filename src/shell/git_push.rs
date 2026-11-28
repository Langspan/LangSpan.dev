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
    /// The words after `push`, as Git receives them, without `-q`: remote
    /// names, refspecs and only the options in `ALLOWED`.
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

/// How deep in substitutions within substitutions LangSpan reads; a push
/// any deeper is refused.
const MAX_NESTING: usize = 8;

/// The `git push` that `command` runs; `None` when it runs none.
pub(crate) fn git_push(command: &str) -> Result<Option<GitPush>, Refused> {
    // Without its line continuations, as Bash reads it: `git pu\⏎sh` pushes.
    let joined = command.replace("\\\n", "");
    if !(joined.contains("git") && joined.contains("push")) {
        return Ok(None);
    }
    read(command, &joined, 0)
}

fn read(command: &str, joined: &str, nesting: usize) -> Result<Option<GitPush>, Refused> {
    if command.len() > MAX_GIT_COMMAND_BYTES {
        return Err(
            format!("this command is longer than LangSpan reads and may push; {ALONE}").into(),
        );
    }
    let (segments, substituted) = Lexer::new(command)
        .read()
        .map_err(|why| format!("LangSpan could not read this command ({why}); {ALONE}"))?;
    // Issue #610: the shell runs a substitution before the command it is in,
    // so a push there runs before any word below is read. Its text is read
    // as a command of its own, with `g""it` and the like, so the cheap check
    // in `git_push` is not repeated.
    for text in &substituted {
        let pushes = || !matches!(read(text, &text.replace("\\\n", ""), nesting + 1), Ok(None));
        if nesting >= MAX_NESTING || pushes() {
            return Err(format!(
                "this command runs `git` and `push` inside a substitution (`$(…)`, a backquote, `<(…)` or an unquoted heredoc), or in an `eval` or `sh -c` there, which the shell runs before LangSpan could check where it pushes; {ALONE}"
            )
            .into());
        }
    }
    if !segments
        .iter()
        .any(|words| push_at(command_words(words)).is_some())
    {
        if segments.iter().any(|words| unread(words, joined)) {
            return Err(format!(
                "this command names `git` and `push` in a command that does not read as a Git command (a shell, `xargs` or a substitution, say), so it may push where LangSpan cannot check; {ALONE}"
            )
            .into());
        }
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

/// Whether the words `git` and `push` appear, in `text`.
fn names_a_push(text: &str) -> bool {
    let tokens = || text.split(|c: char| !c.is_ascii_alphanumeric());
    tokens().any(|token| token == "git") && tokens().any(|token| token == "push")
}

/// A command that may run a push LangSpan cannot read, in a `command` that
/// names one: not a `git …` command, and with both words among its own, or
/// with an expansion whose value is not known (`bash -c 'git push'`,
/// `sh -c "$CMD"`). A `git …` command that is not a push is read.
fn unread(words: &[Word], command: &str) -> bool {
    let run = command_words(words);
    if run
        .first()
        .is_some_and(|first| first.text == "git" || first.text.ends_with("/git"))
    {
        return false;
    }
    let texts: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
    names_a_push(&texts.join(" "))
        || (words.iter().any(|word| word.expanded) && names_a_push(command))
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

/// The only options a hooked `git push` may have. None takes a value, so no
/// word of the push can consume one of the dry run's own; anything else is
/// refused, an abbreviation of these too. `-q` is dropped from the dry run,
/// where it would hide the ref list.
const ALLOWED: &[&str] = &[
    "-u",
    "--set-upstream",
    "-q",
    "--quiet",
    "-v",
    "--verbose",
    "--tags",
    "--follow-tags",
    "--no-follow-tags",
    "--atomic",
    "--progress",
    "--no-progress",
];

const NAMES: &str = "-u, -q, -v, --tags, --[no-]follow-tags, --atomic, --[no-]progress";

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
            "--no-pager" | "-P" => {}
            _ => {
                return Err(format!(
                    "this push has `{option}` before `push`, and LangSpan accepts only `-C` there (a setting such as `-c` changes what its dry run of the push does); set it with `git config`, or leave it out"
                ).into());
            }
        }
    }
    for arg in texts.iter().skip(at + 1).copied() {
        if !arg.starts_with('-') {
            push.args.push(arg.to_owned());
        } else if !ALLOWED.contains(&arg) {
            return Err(format!(
                "this push has `{arg}`, and LangSpan runs a dry run of every push before it runs, which accepts only remote names, refspecs and the options {NAMES}; run the push without it, name a ref that starts with `-` as `refs/heads/NAME`, and force or delete with a refspec (`+SRC:DST`, `:DST`)"
            ).into());
        } else if !matches!(arg, "-q" | "--quiet") {
            push.args.push(arg.to_owned());
        }
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
    fn a_lone_push_keeps_its_allowed_words_for_the_dry_run() {
        assert_eq!(args("git push"), Vec::<String>::new());
        assert_eq!(
            args("git push origin 'refs/heads/*:refs/heads/*' +feat:main :old"),
            ["origin", "refs/heads/*:refs/heads/*", "+feat:main", ":old"]
        );
        assert_eq!(
            args("/usr/bin/git push -u --set-upstream origin HEAD 2>&1"),
            ["-u", "--set-upstream", "origin", "HEAD"]
        );
        let every =
            "-v --verbose --tags --follow-tags --no-follow-tags --atomic --progress --no-progress";
        let mut expected: Vec<&str> = every.split(' ').collect();
        expected.push("origin");
        assert_eq!(args(&format!("git push {every} origin")), expected);
        let push = git_push("git -C a -C b --no-pager push").unwrap().unwrap();
        assert_eq!(push.cwd, Some(PathBuf::from("a/b")));
    }

    /// A quiet dry run says nothing about the refs it would update.
    #[test]
    fn quiet_is_dropped() {
        assert_eq!(args("git push -q origin main"), ["origin", "main"]);
        assert_eq!(args("git push origin main --quiet"), ["origin", "main"]);
        for command in [
            "git status",
            "echo push",
            "git commit -m 'then push'",
            "git log --grep push && git status",
        ] {
            assert_eq!(git_push(command), Ok(None), "{command}");
        }
    }

    /// Review of PR #576: a backslash-newline right after `git` made an empty
    /// word, so the push was not seen; one after `push` gave the dry run an
    /// empty argument. Bash removes the pair, and so does the lexer.
    #[test]
    fn a_line_continuation_is_removed_as_bash_removes_it() {
        for command in [
            "git \\\npush origin main",
            "git \\\n  push origin main",
            "git push \\\n origin \\\nmain",
            "git pu\\\nsh origin ma\\\nin",
            "git push origin main \\\n",
        ] {
            assert_eq!(args(command), ["origin", "main"], "{command:?}");
        }
    }

    /// Review of PR #576: `git` and `push` in a command the parser does not
    /// read as a Git command are refused, not allowed.
    #[test]
    fn git_and_push_in_a_command_that_is_not_read_as_git_are_refused() {
        for command in [
            "bash -c 'git push origin main'",
            "sh -c \"git push\"",
            "xargs git push origin",
            "echo x | xargs /usr/bin/git push",
            "git status; nice git push origin main",
            "eval 'git push origin main'",
        ] {
            let why = git_push(command).unwrap_err().0;
            assert!(
                why.contains("does not read as a Git command"),
                "{command}: {why}"
            );
        }
        // A Git command that is not a push, and words that only resemble.
        for command in [
            "git commit -m \"$(cat <<'EOF'\nfix: push routing\nEOF\n)\"",
            "git log --grep push && git status",
            "echo pushed by git",
            "gitk --all; pushd x",
        ] {
            assert_eq!(git_push(command), Ok(None), "{command}");
        }
    }

    /// Issue #610: a push in a substitution inside a `git …` command's own
    /// words runs before the hook sees it. Each substitution's text is read
    /// as a command of its own.
    #[test]
    fn a_push_hidden_in_a_substitution_is_refused() {
        for command in [
            "$(git push origin main)",
            "git status && echo `git push`",
            "git push origin feat <<EOF\n$(git push origin HEAD:main)\nEOF",
            "git log -1 --format=\"$(git push origin HEAD:main)\"",
            "git log -1 --format=`git push origin HEAD:main`",
            "git log -1 --format=\"`git push origin HEAD:main`\"",
            "git diff <(git push origin HEAD:main)",
            "git diff --output >(git push origin HEAD:main)",
            "git log --format=\"$(echo \"$(git push origin HEAD:main)\")\"",
            "git log --format=\"$(eval 'git push origin HEAD:main')\"",
            "git log --format=\"$(sh -c 'git push origin HEAD:main')\"",
            "git log --format=\"$(bash -c 'git push origin HEAD:main')\"",
            "git log --format=\"$(zsh -c 'git push origin HEAD:main')\"",
            "git log --format=\"$(g\"\"it push origin HEAD:main)\"",
            "git commit -F - <<EOF\n$(git push origin HEAD:main)\nEOF",
            "git commit -F - <<EOF\n`git push origin HEAD:main`\nEOF",
            // Review of PR #613: an unquoted heredoc body is not shell, so
            // its quotes do not stop the substitutions in it.
            "git commit -F - <<EOF\ngit log '$(git push origin HEAD:main)'\nEOF",
            "git commit -F - <<EOF\nnote: '`git push origin HEAD:main`'\nEOF",
            "git commit -F - <<EOF\n\"it's\" $'$(git push origin HEAD:main)'\nEOF",
            "git commit -F - <<EOF\n# $(git push origin HEAD:main)\nEOF",
            "git commit -F - <<EOF\n\\\\$(git push origin HEAD:main)\nEOF",
            // A substitution in a parameter expansion's default runs too.
            "git commit -F - <<EOF\n${x:-$(git push origin HEAD:main)}\nEOF",
            "git log --format=\"${x:-$(git push origin HEAD:main)}\"",
            "git log --format=${x:-`git push origin HEAD:main`}",
        ] {
            let why = git_push(command).unwrap_err().0;
            assert!(why.contains("inside a substitution"), "{command:?}: {why}");
            assert!(why.contains("command of its own"), "{command:?}: {why}");
        }
        // Words only, in a quoted heredoc or a substitution that runs no push.
        for command in [
            "git commit -m \"$(cat <<'EOF'\nfix: refuse a hidden git push\nEOF\n)\"",
            "git commit -F - <<'EOF'\nsee $(git push) in the docs\nEOF",
            "git commit -F - <<EOF\nan escaped \\$(git push) and \\`git push\\` in $USER's note\nEOF",
            "git commit -F - <<EOF\n'unbalanced \"quotes in $HOME: git push\nEOF",
            "git log --format=\"$(git config user.name)\" --grep push",
        ] {
            assert_eq!(git_push(command), Ok(None), "{command:?}");
        }
        // Nested deeper than LangSpan reads: refused, not read.
        let deep = format!(
            "git log {}git push{}",
            "$(echo ".repeat(MAX_NESTING + 1),
            ")".repeat(MAX_NESTING + 1)
        );
        assert!(git_push(&deep).is_err());
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

    /// Review of PR #576: options that took a value or undid the dry run
    /// were refused one by one; now only `ALLOWED` passes, and the refusal
    /// names the word.
    #[test]
    fn every_option_outside_the_allowlist_is_refused_by_name() {
        for (command, word) in [
            ("git push origin main --repo", "--repo"),
            ("git push --repo origin main", "--repo"),
            ("git push --repo=origin main", "--repo=origin"),
            ("git push --no-dry-run origin main", "--no-dry-run"),
            ("git push --no-dry origin main", "--no-dry"),
            ("git push -n origin main", "-n"),
            ("git push --dry-run origin main", "--dry-run"),
            ("git push --no-porcelain origin main", "--no-porcelain"),
            ("git push --qui origin main", "--qui"),
            ("git push -qu origin main", "-qu"),
            ("git push -f origin main", "-f"),
            (
                "git push --force-with-lease origin main",
                "--force-with-lease",
            ),
            ("git push --delete origin main", "--delete"),
            ("git push --prune origin main", "--prune"),
            ("git push --mirror origin", "--mirror"),
            ("git push --all origin", "--all"),
            ("git push origin main -o", "-o"),
            ("git push -o x origin main", "-o"),
            (
                "git push --receive-pack=evil origin main",
                "--receive-pack=evil",
            ),
            ("git push --exec evil origin main", "--exec"),
            ("git push origin -- main", "--"),
            ("git push origin main --", "--"),
            ("git push - main", "-"),
        ] {
            let why = git_push(command).unwrap_err().0;
            assert!(why.contains(&format!("has `{word}`,")), "{command}: {why}");
            assert!(why.contains("--[no-]follow-tags"), "{command}: {why}");
        }
        for (command, word) in [
            ("git -c core.sshCommand=evil push", "-c"),
            ("git -c remote.--dry-run.url=../o.git push", "-c"),
            ("git -c push.default=matching push", "-c"),
            ("git --config-env=a=B push", "--config-env=a=B"),
            (
                "git --git-dir=/x/.git push origin main",
                "--git-dir=/x/.git",
            ),
        ] {
            let why = git_push(command).unwrap_err().0;
            let named = format!("has `{word}` before `push`");
            assert!(why.contains(&named), "{command}: {why}");
        }
        for (command, says) in [
            ("git push origin $BRANCH", "expansion"),
            ("git push \"$(git remote)\" main", "expansion"),
            ("git push 'unterminated", "could not read"),
        ] {
            let why = git_push(command).unwrap_err().0;
            assert!(why.contains(says), "{command}: {why}");
        }
        let long = format!("git push origin {}", "a".repeat(MAX_GIT_COMMAND_BYTES));
        assert!(git_push(&long).unwrap_err().0.contains("longer"));
    }
}
