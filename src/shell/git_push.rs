//! The `git push` commands in a shell line, and where each would push
//! ([ADR 0007](../../docs/adr/0007-check-evidence-integration-and-landing.md#routing-an-agents-push)).
//!
//! It uses the lenient reader of the commit guard: quotes, escapes and
//! substitutions are skipped, and what it cannot be sure of (an expansion
//! where a remote or refspec is needed, `cd` to an unknown directory,
//! another repository) is an error, which never holds a command.

use super::git_commands::{Lexer, MAX_GIT_COMMAND_BYTES, Word, command_words};
use std::path::PathBuf;

/// One `git push`. `cwd` is as in [`super::GitStep`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GitPush {
    pub(crate) cwd: Option<PathBuf>,
    /// The remote as written, `None` for Git's choice.
    pub(crate) remote: Option<String>,
    /// The refspecs as written, without a leading `+`; a deletion is
    /// `:BRANCH`. Empty for Git's choice of the current branch.
    pub(crate) refspecs: Vec<String>,
    /// `-f`, `--force`, `--force-with-lease`, `--force-if-includes` or a `+`.
    pub(crate) force: bool,
    /// `--all`, `--branches` or `--mirror`.
    pub(crate) every_branch: bool,
}

/// The `git push` commands of `command` that would update a branch, in
/// order. A dry run, or a push of tags alone, is not one.
pub(crate) fn git_pushes(command: &str) -> Result<Vec<GitPush>, &'static str> {
    if command.len() > MAX_GIT_COMMAND_BYTES {
        return Err("command longer than the read limit");
    }
    if !command.contains("push") {
        return Ok(Vec::new());
    }
    let mut pushes = Vec::new();
    let mut cwd: Option<PathBuf> = None;
    let mut cwd_known = true;
    for words in Lexer::new(command).segments()? {
        let words = command_words(&words);
        let Some(first) = words.first() else {
            continue;
        };
        if first.text == "cd" {
            match words {
                [_, dir]
                    if dir
                        .literal()
                        .is_some_and(|d| d != "-" && !d.starts_with('~')) =>
                {
                    cwd = Some(join(cwd, &dir.text));
                }
                _ => cwd_known = false,
            }
            continue;
        }
        if !(first.text == "git" || first.text.ends_with("/git")) {
            continue;
        }
        let Some(push) = git_push(words.get(1..).unwrap_or_default(), cwd.clone())? else {
            continue;
        };
        if !cwd_known {
            return Err("a `git push` after `cd` to a directory that is not known");
        }
        pushes.push(push);
    }
    Ok(pushes)
}

fn join(cwd: Option<PathBuf>, dir: &str) -> PathBuf {
    match cwd {
        Some(cwd) => cwd.join(dir),
        None => PathBuf::from(dir),
    }
}

/// One `git …` command's push, after the word `git`.
fn git_push(words: &[Word], mut cwd: Option<PathBuf>) -> Result<Option<GitPush>, &'static str> {
    let mut at = 0;
    while let Some(word) = words.get(at) {
        match word.text.as_str() {
            "-C" => {
                let dir = words
                    .get(at + 1)
                    .and_then(Word::literal)
                    .ok_or("`git -C` with a directory that is not known")?;
                cwd = Some(join(cwd, dir));
                at += 2;
            }
            "-c" => at += 2,
            "--git-dir" | "--work-tree" | "--namespace" => {
                return Err("a Git option naming another repository");
            }
            text if text.starts_with("--git-dir=")
                || text.starts_with("--work-tree=")
                || text.starts_with("--namespace=") =>
            {
                return Err("a Git option naming another repository");
            }
            text if text.starts_with('-') => at += 1,
            "push" => return push(words.get(at + 1..).unwrap_or_default(), cwd),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn push(words: &[Word], cwd: Option<PathBuf>) -> Result<Option<GitPush>, &'static str> {
    let mut push = GitPush {
        cwd,
        ..GitPush::default()
    };
    let (mut delete, mut tags, mut options_end) = (false, false, false);
    let mut operands = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at) {
        at += 1;
        let text = word.text.as_str();
        if options_end || !text.starts_with('-') || text == "-" {
            let operand = word
                .literal()
                .ok_or("a `git push` remote or refspec with an expansion")?;
            operands.push(operand.to_owned());
            continue;
        }
        match text {
            "--" => options_end = true,
            "-n" | "--dry-run" => return Ok(None),
            "--all" | "--branches" | "--mirror" => push.every_branch = true,
            "-d" | "--delete" => delete = true,
            "--tags" => tags = true,
            "-f" | "--force" | "--force-if-includes" => push.force = true,
            "-o" | "--push-option" | "--receive-pack" | "--exec" => at += 1,
            "--repo" => return Err("`git push --repo`"),
            _ if text.starts_with("--repo=") => return Err("`git push --repo`"),
            _ if text.starts_with("--force-with-lease") => push.force = true,
            _ if text.starts_with("--") => {}
            _ => {
                // A cluster of short options, such as `-fu`.
                let letters: Vec<char> = text.chars().skip(1).collect();
                for (index, letter) in letters.iter().enumerate() {
                    match letter {
                        'f' => push.force = true,
                        'd' => delete = true,
                        'n' => return Ok(None),
                        'o' => {
                            // `-o VALUE`, or the rest of the cluster is it.
                            if index + 1 == letters.len() {
                                at += 1;
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let mut operands = operands.into_iter();
    push.remote = operands.next();
    for refspec in operands {
        let refspec = match refspec.strip_prefix('+') {
            Some(rest) => {
                push.force = true;
                rest.to_owned()
            }
            None => refspec,
        };
        push.refspecs.push(if delete && !refspec.contains(':') {
            format!(":{refspec}")
        } else {
            refspec
        });
    }
    if tags && push.refspecs.is_empty() && !push.every_branch {
        return Ok(None);
    }
    Ok(Some(push))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(command: &str) -> GitPush {
        let pushes = git_pushes(command).unwrap();
        assert_eq!(pushes.len(), 1, "{command}: {pushes:?}");
        pushes.into_iter().next().unwrap()
    }

    fn to(remote: &str, refspecs: &[&str]) -> GitPush {
        GitPush {
            remote: Some(remote.into()),
            refspecs: refspecs.iter().map(|r| (*r).to_owned()).collect(),
            ..GitPush::default()
        }
    }

    #[test]
    fn each_form_names_its_remote_and_refspecs() {
        assert_eq!(one("git push"), GitPush::default());
        assert_eq!(one("git push origin"), to("origin", &[]));
        assert_eq!(one("git push -u origin HEAD"), to("origin", &["HEAD"]));
        assert_eq!(
            one("git push origin HEAD:main"),
            to("origin", &["HEAD:main"])
        );
        assert_eq!(
            one("git push --quiet origin feat:refs/heads/main other"),
            to("origin", &["feat:refs/heads/main", "other"])
        );
        assert_eq!(
            one("git push -o ci.skip origin main"),
            to("origin", &["main"])
        );
        assert_eq!(one("git push origin -- main"), to("origin", &["main"]));
        assert_eq!(
            one("git push origin --delete main"),
            to("origin", &[":main"])
        );
        assert_eq!(one("git push -d origin main"), to("origin", &[":main"]));
    }

    #[test]
    fn every_way_of_forcing_is_a_force() {
        for command in [
            "git push -f origin main",
            "git push --force origin main",
            "git push -fu origin main",
            "git push --force-with-lease origin main",
            "git push --force-with-lease=main:abc origin main",
            "git push --force-if-includes origin main",
            "git push origin +main",
        ] {
            let push = one(command);
            assert!(push.force, "{command}: {push:?}");
            assert_eq!(push.refspecs, ["main"], "{command}");
        }
        assert!(!one("git push -u origin main").force);
    }

    #[test]
    fn every_branch_is_said_and_tags_alone_or_a_dry_run_is_no_push() {
        assert!(one("git push --all origin").every_branch);
        assert!(one("git push --mirror").every_branch);
        for command in [
            "git push --tags",
            "git push origin --tags",
            "git push -n origin main",
            "git push --dry-run origin main",
            "git status",
            "echo git push",
            "git log --grep push",
            "git commit -m 'then push'",
        ] {
            assert_eq!(git_pushes(command), Ok(Vec::new()), "{command}");
        }
        assert_eq!(one("git push origin --tags main").refspecs, ["main"]);
    }

    #[test]
    fn the_directory_follows_cd_and_git_c_and_wrappers_do_not_hide_a_push() {
        assert_eq!(one("cd sub && git push").cwd, Some(PathBuf::from("sub")));
        assert_eq!(
            one("git -C /work/app push origin main").cwd,
            Some(PathBuf::from("/work/app"))
        );
        for command in [
            "git add -A && git commit -m x && git push origin main",
            "env GIT_TRACE=1 git push origin main",
            "if true; then git push origin main; fi",
            "/usr/bin/git push origin main",
            "git --no-pager push origin main",
            "git -c push.default=simple push origin main",
        ] {
            assert_eq!(one(command).refspecs, ["main"], "{command}");
        }
        let both = git_pushes("git push origin a; git push upstream b").unwrap();
        assert_eq!(both, [to("origin", &["a"]), to("upstream", &["b"])]);
    }

    #[test]
    fn what_cannot_be_known_is_an_error() {
        for command in [
            "git push origin $BRANCH",
            "git push \"$REMOTE\" main",
            "git push origin \"$(git branch --show-current)\":main",
            "cd $DIR && git push origin main",
            "git --git-dir=/elsewhere/.git push origin main",
            "git --work-tree /elsewhere push origin main",
            "git push --repo=origin main",
        ] {
            assert!(git_pushes(command).is_err(), "{command}");
        }
        let long = format!("git push origin {}", "a".repeat(MAX_GIT_COMMAND_BYTES));
        assert!(git_pushes(&long).is_err());
    }
}
