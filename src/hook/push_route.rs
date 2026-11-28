//! An agent's `git push` to the default branch is routed to `langspan land`
//! ([ADR 0007](../../docs/adr/0007-check-evidence-integration-and-landing.md#routing-an-agents-push)):
//! the push is refused with the exact landing command, which rebases onto the
//! current target, runs the check on the combined tree and pushes only on a
//! pass, never forced. Pushes to other branches pass untouched. What cannot
//! be read passes with a detail: routing is cooperative.

use super::{Config, Reply, ShellCall};
use crate::shell::GitPush;
use serde_json::json;
use std::{path::Path, time::Instant};

/// The start of every routed push's reason.
pub(crate) const PUSH_ROUTED: &str = "LangSpan: push routed to landing.";
/// `git config langspan.defaultBranchPush direct`: the repository allows
/// direct pushes to its default branch, and none is routed.
pub(crate) const DIRECT_SETTING: &str = "langspan.defaultBranchPush";
/// Bytes read from one Git query.
const GIT_LIMIT: usize = 4096;

pub(crate) enum Routed {
    NotAPush,
    /// The command runs; the detail says why it was not routed.
    Passed(String),
    Denied(Reply),
}

/// Why a push's destination was not decided.
#[derive(Debug)]
enum Unread {
    /// The repository allows direct pushes (`DIRECT_SETTING`).
    Direct,
    Git(crate::git::GitError),
    GitFailed,
    NotNamed,
    NoDefault(String),
    /// The push runs in a checkout other than the workspace's.
    OtherRepository,
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => write!(
                f,
                "{DIRECT_SETTING} is `direct` in this repository, so pushes are not routed"
            ),
            Self::Git(error) => write!(f, "no decision ({error})"),
            Self::GitFailed => write!(f, "no decision (Git could not read the push's destination)"),
            Self::NotNamed => write!(
                f,
                "no decision (a push to a URL, a path or an unusual remote name)"
            ),
            Self::OtherRepository => write!(
                f,
                "no decision (the push runs in another checkout than this workspace's; land from a session in that checkout)"
            ),
            Self::NoDefault(remote) => write!(
                f,
                "no decision (the default branch of {remote} is not known; `git remote set-head {remote} --auto` records it)"
            ),
        }
    }
}

/// A plain branch or remote name, safe to show in a shell command: letters,
/// digits and `-_./`, not starting with `-`, `.` or `/`, at most 255 bytes.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with(['-', '.', '/'])
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
}

/// A push that would update the default branch.
#[derive(Debug, PartialEq, Eq)]
struct ToDefault {
    remote: String,
    onto: String,
    /// The local branch or commit that would land; `None` for a deletion or
    /// a push of every branch.
    source: Option<String>,
    force: bool,
    delete: bool,
}

pub(crate) fn route(config: &Config, call: &ShellCall, deadline: Instant) -> Routed {
    let pushes = match crate::shell::git_pushes(&call.command) {
        Ok(pushes) if pushes.is_empty() => return Routed::NotAPush,
        Ok(pushes) => pushes,
        Err(why) => return Routed::Passed(format!("push route: no decision ({why})")),
    };
    // A third of the time left: the commit guard and the snapshot follow.
    let now = Instant::now();
    let until = now + deadline.saturating_duration_since(now) / 3;
    let start = call
        .cwd
        .as_deref()
        .map_or_else(|| config.root.clone(), Into::into);
    let mut why = "no push to the default branch".to_owned();
    for push in &pushes {
        let dir = push
            .cwd
            .as_ref()
            .map_or_else(|| start.clone(), |cwd| start.join(cwd));
        match to_default(&dir, push, until).and_then(|hit| in_workspace(config, &dir, hit, until)) {
            Ok(Some(hit)) => return Routed::Denied(denied(config, &hit)),
            Ok(None) => {}
            Err(error) => why = error.to_string(),
        }
    }
    Routed::Passed(format!("push route: {why}"))
}

/// The hit, when the push runs in the workspace's own checkout: the landing
/// command lands from the workspace's root, so a push from another
/// repository (`git -C OTHER push`) is not routed there.
fn in_workspace(
    config: &Config,
    dir: &Path,
    hit: Option<ToDefault>,
    until: Instant,
) -> Result<Option<ToDefault>, Unread> {
    let Some(hit) = hit else {
        return Ok(None);
    };
    let top = git(dir, &["rev-parse", "--show-toplevel"], until)?.ok_or(Unread::GitFailed)?;
    let root = config
        .root
        .canonicalize()
        .unwrap_or_else(|_| config.root.clone());
    let top = Path::new(&top)
        .canonicalize()
        .unwrap_or_else(|_| top.into());
    if top == root {
        Ok(Some(hit))
    } else {
        Err(Unread::OtherRepository)
    }
}

/// Git's answer, `None` when it says no (exit 1); a failure otherwise.
fn git(dir: &Path, args: &[&str], until: Instant) -> Result<Option<String>, Unread> {
    let (status, bytes) =
        crate::git::run_with_env(dir, args, &[], until, GIT_LIMIT).map_err(Unread::Git)?;
    match status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&bytes).trim().to_owned())),
        Some(1) => Ok(None),
        _ => Err(Unread::GitFailed),
    }
}

fn to_default(dir: &Path, push: &GitPush, until: Instant) -> Result<Option<ToDefault>, Unread> {
    let setting = git(dir, &["config", "--get", DIRECT_SETTING], until)?;
    if setting.as_deref() == Some("direct") {
        return Err(Unread::Direct);
    }
    let current = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"], until)?;
    let mut remote = push.remote.clone();
    // Without refspecs, Git pushes to the current branch's push remote,
    // whether it pushes one branch or every branch.
    let pushed = match &current {
        Some(current) if push.refspecs.is_empty() => {
            let format = "--format=%(push:remotename) %(push:remoteref)";
            let line = git(
                dir,
                &["for-each-ref", format, &format!("refs/heads/{current}")],
                until,
            )?
            .unwrap_or_default();
            let (to_remote, to_ref) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            let (to_remote, to_ref) = (to_remote.to_owned(), to_ref.to_owned());
            if remote.is_none() {
                remote = Some(to_remote.clone());
            }
            Some((current, to_remote, to_ref))
        }
        _ => None,
    };
    let remote = remote
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "origin".into());
    if !plain(&remote) {
        return Err(Unread::NotNamed);
    }
    let (refspecs, force) = refspecs(dir, push, &remote, until)?;
    // `:` (or `+:`, the `+` is dropped) pushes the matching branches.
    let matching = refspecs.iter().any(|refspec| refspec == ":")
        || refspecs.is_empty()
            && git(dir, &["config", "--get", "push.default"], until)?.as_deref()
                == Some("matching");
    let every_branch = push.every_branch || matching;
    // Each (source, destination) the push would update.
    let mut updates: Vec<(Option<String>, String)> = Vec::new();
    if refspecs.is_empty() && !every_branch {
        // Git's choice: the current branch, to its push destination.
        let Some((current, to_remote, to_ref)) = pushed else {
            return Ok(None);
        };
        let destination = if remote == to_remote && !to_ref.is_empty() {
            to_ref
        } else {
            current.clone()
        };
        updates.push((Some(current.clone()), destination));
    }
    // A negative refspec (`^SOURCE`) only excludes sources.
    let excluded = |source: &str| {
        refspecs
            .iter()
            .filter_map(|r| r.strip_prefix('^'))
            .any(|negative| negative == source || wildcard_source("*", negative, source).is_some())
    };
    for refspec in refspecs.iter().filter(|r| !r.starts_with('^')) {
        let (source, destination) = match refspec.split_once(':') {
            Some((source, destination)) => (source, destination.to_owned()),
            None if refspec == "HEAD" => match &current {
                Some(current) => ("HEAD", current.clone()),
                None => continue,
            },
            None => (refspec.as_str(), refspec.clone()),
        };
        let source = match source {
            "" => None,
            "HEAD" => Some(current.clone().unwrap_or_else(|| "HEAD".into())),
            source => Some(source.to_owned()),
        };
        updates.push((source, destination));
    }
    let head = git(
        dir,
        &[
            "symbolic-ref",
            "--quiet",
            &format!("refs/remotes/{remote}/HEAD"),
        ],
        until,
    )?
    .ok_or_else(|| Unread::NoDefault(remote.clone()))?;
    let Some(onto) = head.strip_prefix(&format!("refs/remotes/{remote}/")) else {
        return Err(Unread::NoDefault(remote));
    };
    let onto = onto.to_owned();
    let hit = |source: Option<String>, delete: bool| ToDefault {
        remote: remote.clone(),
        onto: onto.clone(),
        source,
        force,
        delete,
    };
    if every_branch {
        return Ok(Some(hit(None, false)));
    }
    for (source, destination) in updates {
        if destination.contains('*') {
            // A wildcard refspec: the default branch is updated when the
            // source its pattern maps onto it is an existing local ref.
            let Some(source) = source.as_deref().and_then(|source| {
                wildcard_source(source, &destination, &format!("refs/heads/{onto}"))
                    .or_else(|| wildcard_source(source, &destination, &onto))
            }) else {
                continue;
            };
            if !plain(&source) || excluded(&source) {
                continue;
            }
            let commit = format!("{source}^{{commit}}");
            if git(dir, &["rev-parse", "--verify", "--quiet", &commit], until)?.is_some() {
                let source = source.strip_prefix("refs/heads/").unwrap_or(&source);
                return Ok(Some(hit(Some(source.to_owned()), false)));
            }
            continue;
        }
        let branch = match destination.strip_prefix("refs/heads/") {
            Some(branch) => branch,
            // A tag or another namespace.
            None if destination.starts_with("refs/") => continue,
            None => destination.as_str(),
        };
        if branch == onto {
            let delete = source.is_none();
            return Ok(Some(hit(source, delete)));
        }
    }
    Ok(None)
}

/// The push's refspecs, and whether it forces. Without refspecs on the
/// command line, Git pushes every `remote.NAME.push` refspec the remote has.
fn refspecs(
    dir: &Path,
    push: &GitPush,
    remote: &str,
    until: Instant,
) -> Result<(Vec<String>, bool), Unread> {
    if !push.refspecs.is_empty() {
        return Ok((push.refspecs.clone(), push.force));
    }
    let key = format!("remote.{remote}.push");
    let configured = git(dir, &["config", "--get-all", &key], until)?.unwrap_or_default();
    let mut force = push.force;
    let refspecs = configured
        .lines()
        .map(|line| {
            let line = line.trim();
            force |= line.starts_with('+');
            line.trim_start_matches('+').to_owned()
        })
        .filter(|line| !line.is_empty())
        .collect();
    Ok((refspecs, force))
}

/// The source a wildcard refspec `source:destination` pushes to `target`:
/// the part of `target` that the destination's `*` matches, put in place of
/// the source's `*`. `None` when the destination does not match `target`.
fn wildcard_source(source: &str, destination: &str, target: &str) -> Option<String> {
    let (before, after) = destination.split_once('*')?;
    let star = target.strip_prefix(before)?.strip_suffix(after)?;
    let (source_before, source_after) = source.split_once('*')?;
    Some(format!("{source_before}{star}{source_after}"))
}

/// `text` quoted for a POSIX shell.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn denied(config: &Config, hit: &ToDefault) -> Reply {
    // Only plain names are shown; anything else is left as a placeholder.
    let name =
        |text: &str, placeholder: &str| if plain(text) { text } else { placeholder }.to_owned();
    let onto = name(&hit.onto, "DEFAULT_BRANCH");
    let source = hit
        .source
        .as_deref()
        .map_or_else(|| "BRANCH".to_owned(), |source| name(source, "BRANCH"));
    let executable = std::env::current_exe()
        .map(|path| quote(&path.to_string_lossy()))
        .unwrap_or_else(|_| "langspan".into());
    let remote = match hit.remote.as_str() {
        "origin" => String::new(),
        remote => format!(" --remote {}", name(remote, "REMOTE")),
    };
    let command = format!(
        "{executable} --state-dir {} land {} --onto {onto}{remote} --name CHECK {source} --apply --rebase --check-again -- TEST_COMMAND",
        quote(&config.state_dir.to_string_lossy()),
        quote(&config.workspace_id),
    );
    let reason = if hit.delete {
        format!(
            "{PUSH_ROUTED} This push would delete {onto}, the default branch of the remote. LangSpan does not delete it; ask the owner."
        )
    } else {
        let force = if hit.force {
            " This push forces; landing never does, so a forced push to the default branch is never made for an agent."
        } else {
            ""
        };
        let every = if hit.source.is_none() {
            " This push would update every branch, the default branch too: push the other branches by name."
        } else {
            ""
        };
        format!(
            "{PUSH_ROUTED} {onto} is the default branch of the remote, and a push to it goes through landing: it fetches {onto}, rebases your commits onto it, runs the check on that exact combined tree and pushes only when it passes. A failing check pushes nothing and says why.{force}{every} Run `{command}`, with CHECK a name for the check and TEST_COMMAND the project's test command (MCP: langspan_integrate_check, then langspan_land, both with rebase). Push other branches with `git push` as before."
        )
    };
    Reply {
        stdout: json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        })
        .to_string(),
        coverage: "denied_push",
        detail: format!("a push to the default branch {onto}, routed to landing"),
    }
}

#[cfg(test)]
mod tests {
    //! Git's own resolution of a remote's default branch and of a plain
    //! push's destination is what is verified, so these use real temporary
    //! repositories with a local bare remote.
    use super::*;
    use std::{path::PathBuf, process::Command, time::Duration};

    struct Repo {
        base: PathBuf,
        work: PathBuf,
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// `work` on `feat`, with `main` and `feat` pushed to `origin` and
    /// `origin/HEAD` naming `main`.
    fn repo(name: &str) -> Repo {
        let base =
            std::env::temp_dir().join(format!("ls-push-route-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // The snapshot wants the top level as Git names it, without links.
        let base = base.canonicalize().unwrap();
        let work = base.join("work");
        git_ok(&base, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git_ok(&base, &["init", "-q", "-b", "main", "work"]);
        git_ok(&work, &["remote", "add", "origin", "../origin.git"]);
        git_ok(&work, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git_ok(&work, &["push", "-q", "-u", "origin", "main"]);
        git_ok(&work, &["remote", "set-head", "origin", "main"]);
        git_ok(&work, &["switch", "-q", "-c", "feat"]);
        git_ok(&work, &["push", "-q", "-u", "origin", "feat"]);
        Repo { base, work }
    }

    fn run(repo: &Repo, command: &str) -> Routed {
        let config = Config::new(
            repo.base.join("state"),
            "app",
            repo.work.clone(),
            crate::vendor::Vendor::ClaudeCode,
        );
        let call = ShellCall {
            command: command.into(),
            cwd: None,
        };
        route(&config, &call, Instant::now() + Duration::from_secs(20))
    }

    fn reason(routed: Routed) -> String {
        match routed {
            Routed::Denied(reply) => {
                assert_eq!(reply.coverage, "denied_push");
                let stdout: serde_json::Value = serde_json::from_str(&reply.stdout).unwrap();
                let output = &stdout["hookSpecificOutput"];
                assert_eq!(output["permissionDecision"], "deny");
                output["permissionDecisionReason"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
            Routed::Passed(detail) => panic!("passed: {detail}"),
            Routed::NotAPush => panic!("not a push"),
        }
    }

    fn passed(routed: Routed) -> String {
        match routed {
            Routed::Passed(detail) => detail,
            Routed::Denied(reply) => panic!("denied: {}", reply.stdout),
            Routed::NotAPush => panic!("not a push"),
        }
    }

    #[test]
    fn a_push_to_the_default_branch_is_routed_to_the_landing_command() {
        let repo = repo("routed");
        for (command, source) in [
            ("git push origin feat:main", "feat"),
            ("git push origin HEAD:refs/heads/main", "feat"),
            ("git push origin main", "main"),
        ] {
            let reason = reason(run(&repo, command));
            assert!(reason.starts_with(PUSH_ROUTED), "{reason}");
            assert!(
                reason.contains(&format!(
                    "land 'app' --onto main --name CHECK {source} --apply --rebase --check-again -- TEST_COMMAND`"
                )),
                "{command}: {reason}"
            );
            assert!(reason.contains("pushes only when it passes"), "{reason}");
            assert!(!reason.contains(DIRECT_SETTING), "{reason}");
            // The same push is refused every time.
            assert_eq!(super::tests::reason(run(&repo, command)), reason);
        }
        git_ok(&repo.work, &["switch", "-q", "main"]);
        let reason = reason(run(&repo, "git push"));
        assert!(reason.contains("--name CHECK main --apply"), "{reason}");
    }

    #[test]
    fn a_push_to_another_branch_passes_and_so_does_what_is_not_a_push() {
        let repo = repo("other");
        for command in [
            "git push",
            "git push -u origin feat",
            "git push origin HEAD",
            "git push -f origin feat",
            "git push origin feat:refs/tags/main",
        ] {
            let detail = passed(run(&repo, command));
            assert_eq!(
                detail, "push route: no push to the default branch",
                "{command}"
            );
        }
        assert!(matches!(run(&repo, "git status"), Routed::NotAPush));
    }

    #[test]
    fn forcing_deleting_or_every_branch_is_routed_and_says_so() {
        let repo = repo("force");
        let forced = reason(run(&repo, "git push --force-with-lease origin feat:main"));
        assert!(forced.contains("landing never does"), "{forced}");
        assert!(forced.contains("--name CHECK feat --apply"), "{forced}");
        let every = reason(run(&repo, "git push --all origin"));
        assert!(every.contains("push the other branches by name"), "{every}");
        assert!(every.contains("--name CHECK BRANCH --apply"), "{every}");
        let delete = reason(run(&repo, "git push origin --delete main"));
        assert!(delete.contains("would delete main"), "{delete}");
        assert!(!delete.contains("land "), "{delete}");
    }

    #[test]
    fn the_matching_refspec_is_routed_as_a_push_of_every_branch() {
        let repo = repo("matching");
        for command in ["git push origin :", "git push origin +:"] {
            let every = reason(run(&repo, command));
            assert!(
                every.contains("push the other branches by name"),
                "{command}: {every}"
            );
        }
    }

    /// A wildcard refspec updates the default branch when its source side
    /// names an existing local branch that maps onto it.
    #[test]
    fn a_wildcard_refspec_onto_the_default_branch_is_routed() {
        let repo = repo("wildcard");
        for command in [
            "git push origin 'refs/heads/*:refs/heads/*'",
            "git push origin '+refs/heads/*:refs/heads/*'",
            "git push origin 'refs/heads/ma*:refs/heads/ma*'",
        ] {
            let reason = reason(run(&repo, command));
            assert!(
                reason.contains("--name CHECK main --apply"),
                "{command}: {reason}"
            );
        }
        for command in [
            "git push origin 'refs/heads/f*:refs/heads/f*'",
            "git push origin 'refs/heads/*:refs/heads/x/*'",
            "git push origin 'refs/heads/nope/*:refs/heads/*'",
            "git push origin 'refs/heads/*:refs/tags/*'",
        ] {
            let detail = passed(run(&repo, command));
            assert_eq!(
                detail, "push route: no push to the default branch",
                "{command}"
            );
        }
    }

    #[test]
    fn a_wildcard_source_is_the_destinations_match_put_in_the_sources_star() {
        let main = "refs/heads/main";
        assert_eq!(
            wildcard_source("refs/heads/*", "refs/heads/*", main).as_deref(),
            Some(main)
        );
        assert_eq!(
            wildcard_source("refs/remotes/o/*-x", "refs/heads/m*", main).as_deref(),
            Some("refs/remotes/o/ain-x")
        );
        assert_eq!(
            wildcard_source("refs/heads/*", "refs/heads/x/*", main),
            None
        );
        assert_eq!(wildcard_source("refs/heads/a", "refs/heads/*", main), None);
    }

    /// Without refspecs on the command line, Git pushes the remote's
    /// configured `remote.NAME.push` refspecs, all of them.
    #[test]
    fn a_push_without_refspecs_uses_every_configured_push_refspec() {
        let repo = repo("configured");
        git_ok(
            &repo.work,
            &[
                "config",
                "remote.origin.push",
                "refs/heads/feat:refs/heads/feat",
            ],
        );
        for command in ["git push", "git push origin"] {
            let detail = passed(run(&repo, command));
            assert_eq!(
                detail, "push route: no push to the default branch",
                "{command}"
            );
        }
        git_ok(
            &repo.work,
            &[
                "config",
                "--add",
                "remote.origin.push",
                "+refs/heads/*:refs/heads/*",
            ],
        );
        for command in ["git push", "git push origin"] {
            let reason = reason(run(&repo, command));
            assert!(
                reason.contains("--name CHECK main --apply"),
                "{command}: {reason}"
            );
            assert!(reason.contains("landing never does"), "{command}: {reason}");
        }
        // A negative refspec excludes the default branch from the wildcard.
        git_ok(
            &repo.work,
            &["config", "--add", "remote.origin.push", "^refs/heads/main"],
        );
        let detail = passed(run(&repo, "git push"));
        assert_eq!(detail, "push route: no push to the default branch");
        git_ok(&repo.work, &["config", "--unset-all", "remote.origin.push"]);
        git_ok(&repo.work, &["config", "remote.origin.push", ":"]);
        let every = reason(run(&repo, "git push"));
        assert!(every.contains("push the other branches by name"), "{every}");
    }

    /// Git picks the current branch's push remote for a push without
    /// refspecs, even of every branch; `origin` is not assumed.
    #[test]
    fn a_push_of_every_branch_goes_to_the_current_branchs_remote() {
        let repo = repo("remote");
        git_ok(&repo.work, &["remote", "add", "fork", "../origin.git"]);
        git_ok(&repo.work, &["fetch", "-q", "fork"]);
        git_ok(&repo.work, &["remote", "set-head", "fork", "main"]);
        git_ok(&repo.work, &["branch", "-q", "-u", "fork/feat"]);
        git_ok(&repo.work, &["remote", "set-head", "origin", "-d"]);
        let every = reason(run(&repo, "git push --all"));
        assert!(
            every.contains("--onto main --remote fork --name CHECK BRANCH"),
            "{every}"
        );
        git_ok(&repo.work, &["config", "push.default", "matching"]);
        let matching = reason(run(&repo, "git push"));
        assert!(matching.contains("--remote fork"), "{matching}");
    }

    /// The landing command lands from the workspace's root, so a push run in
    /// another checkout is not routed there; one run from a subdirectory of
    /// the workspace is.
    #[test]
    fn a_push_in_another_checkout_passes_and_one_in_a_subdirectory_is_routed() {
        let repo = repo("elsewhere");
        let other = repo.base.join("other");
        git_ok(&repo.base, &["clone", "-q", "origin.git", "other"]);
        git_ok(&other, &["switch", "-q", "feat"]);
        let detail = passed(run(&repo, "git -C ../other push origin feat:main"));
        assert!(detail.contains("another checkout"), "{detail}");
        std::fs::create_dir(repo.work.join("sub")).unwrap();
        let reason = reason(run(&repo, "cd sub && git push origin feat:main"));
        assert!(reason.contains("land 'app' --onto main"), "{reason}");
    }

    /// The shell hook refuses before it records a run, and a push that
    /// passes runs with the route's detail. No daemon is needed: neither
    /// command writes a file by its parse, so nothing is admitted.
    #[test]
    fn the_shell_hook_refuses_a_routed_push_before_recording_a_run() {
        let repo = repo("hook");
        let config = Config::new(
            repo.base.join("state"),
            "app",
            repo.work.clone(),
            crate::vendor::Vendor::ClaudeCode,
        );
        crate::local::private_directory(&config.state_dir).unwrap();
        let session = |deadline| super::super::Session {
            config: &config,
            deadline,
            root: &repo.work,
            source: "claude-code",
            parent: None,
            headless: false,
            unreachable: Default::default(),
            reached: Default::default(),
        };
        for (id, command, coverage) in [
            ("p1", "git push origin feat:main", "denied_push"),
            ("p2", "git push -u origin feat", "shell_observing"),
        ] {
            let ids = super::super::identities_for("claude-code", "alice", id);
            let operation = langspan_core::ToolIdentity {
                operation_id: ids.operation_id.clone(),
                source: "claude-code".into(),
                tool: "Bash".into(),
                input_digest: "0".repeat(64),
            };
            let call = ShellCall {
                command: command.into(),
                cwd: None,
            };
            let deadline = Instant::now() + Duration::from_secs(20);
            let reply = session(deadline).shell_pre(
                &ids,
                &operation,
                Vec::new(),
                None,
                false,
                Some(&call),
                true,
            );
            assert_eq!(reply.coverage, coverage, "{command}: {reply:?}");
            let recorded = crate::shell::load(&config.state_dir, &ids.operation_id);
            assert_eq!(recorded.is_some(), coverage != "denied_push", "{command}");
            if coverage != "denied_push" {
                assert!(
                    reply
                        .detail
                        .contains("push route: no push to the default branch"),
                    "{reply:?}"
                );
            }
        }
    }

    #[test]
    fn the_repositorys_setting_or_an_unknown_default_branch_passes_with_a_detail() {
        let repo = repo("setting");
        git_ok(&repo.work, &["config", DIRECT_SETTING, "direct"]);
        let detail = passed(run(&repo, "git push origin feat:main"));
        assert!(detail.contains("is `direct`"), "{detail}");
        git_ok(&repo.work, &["config", "--unset", DIRECT_SETTING]);
        git_ok(&repo.work, &["remote", "set-head", "origin", "-d"]);
        let detail = passed(run(&repo, "git push origin feat:main"));
        assert!(
            detail.contains("git remote set-head origin --auto"),
            "{detail}"
        );
        let detail = passed(run(&repo, "git push ../origin.git feat:main"));
        assert!(detail.contains("URL, a path"), "{detail}");
    }
}
