//! An agent's `git push` to the default branch is routed to `langspan land`
//! ([ADR 0007](../../docs/adr/0007-check-evidence-integration-and-landing.md#routing-an-agents-push)):
//! the push is refused with the exact landing command, which rebases onto the
//! current target, runs the check on the combined tree and pushes only on a
//! pass, never forced. Pushes to other branches pass untouched.
//!
//! Where a push goes is Git's answer, not LangSpan's reading: the hook runs
//! `git push --dry-run --porcelain` with the same arguments and reads which
//! refs it would update. A push it cannot check that way (one combined with
//! another command, one with an option outside a short allowlist, or a dry
//! run that fails or runs out of time) is refused.

use super::{Config, Reply, ShellCall};
use crate::shell::GitPush;
use serde_json::json;
use std::{path::Path, time::Instant};

/// The start of every routed push's reason.
pub(crate) const PUSH_ROUTED: &str = "LangSpan: push routed to landing.";
/// The start of the reason for a push that could not be checked.
pub(crate) const PUSH_REFUSED: &str = "LangSpan: push refused.";
/// `git config langspan.defaultBranchPush direct`: the repository allows
/// direct pushes to its default branch, and none is routed.
pub(crate) const DIRECT_SETTING: &str = "langspan.defaultBranchPush";
/// Bytes read from one Git query.
const GIT_LIMIT: usize = 4096;
/// Bytes read from the dry run's ref list.
const DRY_RUN_LIMIT: usize = 256 * 1024;
/// Remotes compared with the dry run's URL.
const MAX_REMOTES: usize = 32;

pub(crate) enum Routed {
    NotAPush,
    /// The command runs; the detail says why it was not routed.
    Passed(String),
    Denied(Reply),
}

/// Why a push was not routed: the first four let it run, the rest refuse it.
#[derive(Debug)]
enum Unread {
    /// The repository allows direct pushes (`DIRECT_SETTING`).
    Direct,
    /// The push runs in a checkout other than the workspace's.
    OtherRepository,
    /// The dry run pushes to a URL that no configured remote has.
    NotNamed,
    NoDefault(String),
    Git(crate::git::GitError),
    GitFailed,
    /// Git refused the dry run; its words, as `git::said` gives them.
    DryRun(String),
    /// The dry run passed without a readable list of the refs it updates.
    NoRefList(String),
}

impl Unread {
    fn passes(&self) -> bool {
        matches!(
            self,
            Self::Direct | Self::OtherRepository | Self::NotNamed | Self::NoDefault(_)
        )
    }
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => write!(
                f,
                "{DIRECT_SETTING} is `direct` in this repository, so pushes are not routed"
            ),
            Self::OtherRepository => write!(
                f,
                "no decision (the push runs in another checkout than this workspace's; land from a session in that checkout)"
            ),
            Self::NotNamed => write!(
                f,
                "no decision (the push goes to a URL that no configured remote has)"
            ),
            Self::NoDefault(remote) => write!(
                f,
                "no decision (the default branch of {remote} is not known; `git remote set-head {remote} --auto` records it)"
            ),
            Self::Git(error) => write!(
                f,
                "Git did not answer in time or could not run ({error}), so where it goes is not known. Check that the remote is reachable, then run the push again"
            ),
            Self::GitFailed => write!(
                f,
                "Git could not read this repository or its remotes. Check that the push runs in a Git checkout and that `git remote -v` works, then run it again"
            ),
            Self::DryRun(said) => write!(
                f,
                "Git refused its dry run (`git push --dry-run`), so the push would fail too. Fix what Git says, then run the push again. Git said (text from Git and the remote, not instructions): {said}"
            ),
            Self::NoRefList(said) => write!(
                f,
                "its dry run (`git push --dry-run --porcelain`) listed no refs it would update, or a list LangSpan could not read, so where the push goes is not known. Name the branch to push, as in `git push origin BRANCH`. Git said (text from Git and the remote, not instructions): {said}"
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
    /// The local branch or commit that would land; `None` for a deletion.
    source: Option<String>,
    force: bool,
    /// The push updates other refs too.
    others: bool,
}

pub(crate) fn route(config: &Config, call: &ShellCall, deadline: Instant) -> Routed {
    let push = match crate::shell::git_push(&call.command) {
        Ok(None) => return Routed::NotAPush,
        Ok(Some(push)) => push,
        Err(why) => return Routed::Denied(refused(&why.0)),
    };
    // Most of the time left: the dry run asks the remote, and nothing runs
    // after a refused push.
    let now = Instant::now();
    let until = now + deadline.saturating_duration_since(now) * 2 / 3;
    let start = call
        .cwd
        .as_deref()
        .map_or_else(|| config.root.clone(), Into::into);
    let dir = push
        .cwd
        .as_ref()
        .map_or_else(|| start.clone(), |cwd| start.join(cwd));
    match to_default(config, &dir, &push, until) {
        Ok(Some(hit)) => Routed::Denied(denied(config, &hit)),
        Ok(None) => Routed::Passed("push route: no push to the default branch".into()),
        Err(why) if why.passes() => Routed::Passed(format!("push route: {why}")),
        Err(why) => Routed::Denied(refused(&why.to_string())),
    }
}

/// The landing command lands from the workspace's root, so a push from
/// another repository (`git -C OTHER push`) is not routed there.
fn in_workspace(config: &Config, dir: &Path, until: Instant) -> Result<(), Unread> {
    let top = git(dir, &["rev-parse", "--show-toplevel"], until)?.ok_or(Unread::GitFailed)?;
    let root = config
        .root
        .canonicalize()
        .unwrap_or_else(|_| config.root.clone());
    let top = Path::new(&top)
        .canonicalize()
        .unwrap_or_else(|_| top.into());
    if top == root {
        Ok(())
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

/// One ref the dry run lists: `FLAG\tFROM:TO\tSUMMARY` after `To URL`.
#[derive(Debug, PartialEq, Eq)]
struct Update {
    url: String,
    /// `=` up to date, `-` deleted, `+` forced, `!` rejected, ` ` or `*`.
    flag: char,
    /// Empty for a deletion.
    from: String,
    to: String,
}

/// The refs the dry run lists; `None` when a ref line cannot be read, so the
/// list cannot be trusted to be complete. A line without a tab is one of
/// Git's messages, such as `Done` or the one `-u` prints, and says nothing
/// about a ref.
fn porcelain(out: &str) -> Option<Vec<Update>> {
    let mut url: Option<&str> = None;
    let mut updates = Vec::new();
    for line in out.lines() {
        if let Some(to) = line.strip_prefix("To ") {
            url = Some(to);
            continue;
        }
        if !line.contains('\t') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(flag), Some(refs), Some(_summary), Some(url)) =
            (fields.next(), fields.next(), fields.next(), url)
        else {
            return None;
        };
        let mut letters = flag.chars();
        let (Some(flag), None) = (letters.next(), letters.next()) else {
            return None;
        };
        let (from, to) = refs.split_once(':').unwrap_or(("", refs));
        updates.push(Update {
            url: url.to_owned(),
            flag,
            from: from.to_owned(),
            to: to.to_owned(),
        });
    }
    Some(updates)
}

fn to_default(
    config: &Config,
    dir: &Path,
    push: &GitPush,
    until: Instant,
) -> Result<Option<ToDefault>, Unread> {
    // `git_push` refuses `git -c`, so the push cannot set it.
    if git(dir, &["config", "--get", DIRECT_SETTING], until)?.as_deref() == Some("direct") {
        return Err(Unread::Direct);
    }
    in_workspace(config, dir, until)?;
    // Git runs `pre-push` even for a dry run; only the push itself may. The
    // probe's options come first, and `git_push` lets through only options
    // that take no value and cannot undo them (its `ALLOWED`).
    let mut args = vec![
        "-c",
        "core.hooksPath=/dev/null",
        "push",
        "--dry-run",
        "--porcelain",
    ];
    args.extend(push.args.iter().map(String::as_str));
    let (status, out, said) =
        crate::git::network_output(dir, &args, until, DRY_RUN_LIMIT).map_err(Unread::Git)?;
    match porcelain(&String::from_utf8_lossy(&out)) {
        Some(updates) if !updates.is_empty() => match onto_default(dir, &updates, until) {
            Ok(None) if !status.success() => Err(Unread::DryRun(said)),
            decided => decided,
        },
        _ if !status.success() => Err(Unread::DryRun(said)),
        _ => Err(Unread::NoRefList(said)),
    }
}

/// The update of a remote's default branch among `updates`.
fn onto_default(
    dir: &Path,
    updates: &[Update],
    until: Instant,
) -> Result<Option<ToDefault>, Unread> {
    let changed: Vec<&Update> = updates.iter().filter(|u| u.flag != '=').collect();
    if changed.is_empty() {
        return Ok(None);
    }
    let names = git(dir, &["remote"], until)?.unwrap_or_default();
    let mut unread = None;
    let mut urls: Vec<&str> = changed.iter().map(|u| u.url.as_str()).collect();
    urls.dedup();
    for url in urls {
        let mut named = false;
        for name in names.lines().filter(|name| plain(name)).take(MAX_REMOTES) {
            let pushes_to = git(dir, &["remote", "get-url", "--push", "--all", name], until)?;
            if !pushes_to
                .unwrap_or_default()
                .lines()
                .any(|pushed| anonymous(pushed) == anonymous(url))
            {
                continue;
            }
            named = true;
            let Some(onto) = default_branch(dir, name, until)? else {
                unread = Some(Unread::NoDefault(name.to_owned()));
                continue;
            };
            let target = format!("refs/heads/{onto}");
            if let Some(update) = changed.iter().find(|u| u.url == url && u.to == target) {
                return Ok(Some(ToDefault {
                    remote: name.to_owned(),
                    onto,
                    source: source(dir, update, until)?,
                    force: update.flag == '+',
                    others: changed.len() > 1,
                }));
            }
        }
        if !named {
            unread.get_or_insert(Unread::NotNamed);
        }
    }
    unread.map_or(Ok(None), Err)
}

/// The branch `refs/remotes/REMOTE/HEAD` names.
fn default_branch(dir: &Path, remote: &str, until: Instant) -> Result<Option<String>, Unread> {
    let head = format!("refs/remotes/{remote}/HEAD");
    let named = git(dir, &["symbolic-ref", "--quiet", &head], until)?;
    Ok(named.and_then(|named| {
        named
            .strip_prefix(&format!("refs/remotes/{remote}/"))
            .map(str::to_owned)
    }))
}

/// What would land: the local ref, the current branch's for `HEAD`. It stays
/// fully qualified (`refs/heads/feat`), since a tag of the same name comes
/// first when Git resolves a short one and would land other commits.
fn source(dir: &Path, update: &Update, until: Instant) -> Result<Option<String>, Unread> {
    Ok(match update.from.as_str() {
        "" => None,
        "HEAD" => Some(
            git(dir, &["symbolic-ref", "--quiet", "HEAD"], until)?.unwrap_or_else(|| "HEAD".into()),
        ),
        from => Some(from.to_owned()),
    })
}

/// `url` as Git shows it after `To`: without a user or password.
fn anonymous(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
        let slash = if rest.contains('/') { "/" } else { "" };
        return format!("{scheme}://{host}{slash}{path}");
    }
    // `user@host:path`, as `scp` writes it; a local path stays as it is.
    match url.split_once(':') {
        Some((host, path)) if !host.contains('/') => {
            let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
            format!("{host}:{path}")
        }
        _ => url.to_owned(),
    }
}

/// `text` quoted for a POSIX shell.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn deny(reason: String, detail: String) -> Reply {
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
        detail,
    }
}

fn refused(why: &str) -> Reply {
    deny(
        format!(
            "{PUSH_REFUSED} LangSpan checks where an agent's push goes before it runs, and could not check this one: {why}."
        ),
        "a push that could not be checked, refused".into(),
    )
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
    let reason = if hit.source.is_none() {
        format!(
            "{PUSH_ROUTED} This push would delete {onto}, the default branch of the remote. LangSpan does not delete it; ask the owner."
        )
    } else {
        let force = if hit.force {
            " This push forces; landing never does, so a forced push to the default branch is never made for an agent."
        } else {
            ""
        };
        let others = if hit.others {
            " This push updates other refs too: push the other branches by name."
        } else {
            ""
        };
        format!(
            "{PUSH_ROUTED} {onto} is the default branch of the remote, and a push to it goes through landing: it fetches {onto}, rebases your commits onto it, runs the check on that exact combined tree and pushes only when it passes. A failing check pushes nothing and says why.{force}{others} Run `{command}`, with CHECK a name for the check and TEST_COMMAND the project's test command (MCP: langspan_integrate_check, then langspan_land, both with rebase). Push other branches with `git push` as before."
        )
    };
    deny(
        reason,
        format!("a push to the default branch {onto}, routed to landing"),
    )
}

#[cfg(test)]
mod tests {
    //! Git's own answer is what is verified, so most of these run the shell
    //! hook against real temporary repositories with a local bare remote;
    //! the ref list's and the URL's reading are pure.
    use super::*;
    use std::{
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

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

    fn commit(dir: &Path, message: &str) {
        git_ok(dir, &["commit", "-q", "--allow-empty", "-m", message]);
    }

    /// `work` on `feat`, one commit ahead of `origin/feat`; `origin/main`
    /// has a commit `feat` lacks, so `feat:main` would be rejected without
    /// a force. `origin/HEAD` names `main`.
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
        commit(&work, "one");
        git_ok(&work, &["push", "-q", "-u", "origin", "main"]);
        git_ok(&work, &["remote", "set-head", "origin", "main"]);
        git_ok(&work, &["switch", "-q", "-c", "feat"]);
        git_ok(&work, &["push", "-q", "-u", "origin", "feat"]);
        git_ok(&work, &["switch", "-q", "main"]);
        commit(&work, "two");
        git_ok(&work, &["push", "-q"]);
        git_ok(&work, &["switch", "-q", "feat"]);
        commit(&work, "three");
        Repo { base, work }
    }

    /// `main` one commit ahead of `origin/main`, `feat` checked out.
    fn main_ahead(repo: &Repo) {
        git_ok(&repo.work, &["switch", "-q", "main"]);
        commit(&repo.work, "four");
        git_ok(&repo.work, &["switch", "-q", "feat"]);
    }

    fn config(repo: &Repo) -> Config {
        Config::new(
            repo.base.join("state"),
            "app",
            repo.work.clone(),
            crate::vendor::Vendor::ClaudeCode,
        )
    }

    /// The shell hook's reply to `command` run in `cwd`, and whether it
    /// recorded a run.
    fn hook_in(repo: &Repo, cwd: Option<&Path>, command: &str) -> (Reply, bool) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let config = config(repo);
        crate::local::private_directory(&config.state_dir).unwrap();
        let id = format!("p{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let ids = super::super::identities_for("claude-code", "alice", &id);
        let operation = langspan_core::ToolIdentity {
            operation_id: ids.operation_id.clone(),
            source: "claude-code".into(),
            tool: "Bash".into(),
            input_digest: "0".repeat(64),
        };
        let call = ShellCall {
            command: command.into(),
            cwd: cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
        };
        let mut session = super::super::Session {
            config: &config,
            deadline: Instant::now() + Duration::from_secs(20),
            root: &repo.work,
            source: "claude-code",
            parent: None,
            headless: false,
            unreachable: Default::default(),
            reached: Default::default(),
        };
        let reply = session.shell_pre(&ids, &operation, Vec::new(), None, false, Some(&call), true);
        let recorded = crate::shell::load(&config.state_dir, &ids.operation_id).is_some();
        (reply, recorded)
    }

    fn hook(repo: &Repo, command: &str) -> (Reply, bool) {
        hook_in(repo, None, command)
    }

    /// The refusal's reason; a refused command records no run.
    fn denial(command: &str, (reply, recorded): (Reply, bool)) -> String {
        assert_eq!(reply.coverage, "denied_push", "{command}: {reply:?}");
        assert!(!recorded, "{command}");
        let stdout: serde_json::Value = serde_json::from_str(&reply.stdout).unwrap();
        let output = &stdout["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "deny");
        output["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn routed(repo: &Repo, command: &str) -> String {
        let reason = denial(command, hook(repo, command));
        assert!(reason.starts_with(PUSH_ROUTED), "{command}: {reason}");
        reason
    }

    fn refused(repo: &Repo, command: &str) -> String {
        let reason = denial(command, hook(repo, command));
        assert!(reason.starts_with(PUSH_REFUSED), "{command}: {reason}");
        reason
    }

    /// The route's detail of a command that runs.
    fn passed(repo: &Repo, command: &str) -> String {
        let (reply, recorded) = hook(repo, command);
        assert_ne!(reply.coverage, "denied_push", "{command}: {reply:?}");
        assert!(recorded, "{command}");
        let at = reply.detail.find("push route: ").unwrap();
        reply.detail.get(at..).unwrap().to_owned()
    }

    const NOT_DEFAULT: &str = "push route: no push to the default branch";

    /// The landing command for the local branch `branch`.
    fn lands(branch: &str) -> String {
        format!(
            "land 'app' --onto main --name CHECK refs/heads/{branch} --apply --rebase --check-again -- TEST_COMMAND`"
        )
    }

    /// Review of PR #576: the landing command named `feat`, and a tag `feat`
    /// comes first when Git resolves that, so landing would have published
    /// the tag's commit. The command names `refs/heads/feat`, and `land`'s
    /// own resolution of it gives the branch's commit. Git's resolution of
    /// an ambiguous name is what is verified, so this runs Git.
    #[test]
    fn the_landing_command_names_the_branch_even_when_a_tag_shares_its_name() {
        let repo = repo("tag-named-like-branch");
        git_ok(&repo.work, &["tag", "feat", "main"]);
        let reason = routed(&repo, "git push origin refs/heads/feat:refs/heads/main");
        assert!(reason.contains(&lands("feat")), "{reason}");
        // As `integrate::commit` resolves the source: `rev-parse NAME^{commit}`.
        let resolve = |name: &str| {
            let out = Command::new("git")
                .args([
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{name}^{{commit}}"),
                ])
                .current_dir(&repo.work)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let branch = resolve("refs/heads/feat");
        assert_ne!(branch, resolve("refs/tags/feat"), "the fixture's tag");
        assert_ne!(
            resolve("feat"),
            branch,
            "Git resolves the short name to the tag"
        );
    }

    #[test]
    fn a_push_to_the_default_branch_is_routed_to_the_landing_command() {
        let repo = repo("routed");
        let marker = repo.base.join("pre-push-ran");
        let pre_push = repo.work.join(".git/hooks/pre-push");
        std::fs::write(
            &pre_push,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(
            &pre_push,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        for command in [
            "git push origin feat:main",
            "git push origin HEAD:refs/heads/main",
            // The remote's URL is the remote.
            "git push ../origin.git feat:main",
        ] {
            let reason = routed(&repo, command);
            assert!(reason.contains(&lands("feat")), "{command}: {reason}");
            assert!(reason.contains("pushes only when it passes"), "{reason}");
            assert!(!reason.contains(DIRECT_SETTING), "{reason}");
            assert!(!reason.contains("forces"), "{reason}");
            // The same push is refused every time.
            assert_eq!(routed(&repo, command), reason);
        }
        main_ahead(&repo);
        git_ok(&repo.work, &["switch", "-q", "main"]);
        for command in ["git push", "git push -q", "git push origin main"] {
            let reason = routed(&repo, command);
            assert!(reason.contains(&lands("main")), "{command}: {reason}");
        }
        assert!(!marker.exists(), "the dry run ran the pre-push hook");
    }

    #[test]
    fn a_push_to_another_branch_passes_and_so_does_what_is_not_a_push() {
        let repo = repo("other");
        for command in [
            "git push",
            "git push -u origin feat",
            "git push origin HEAD",
            "git push origin +feat",
            "git push origin feat:refs/tags/main",
            // Up to date: the default branch is not updated.
            "git push origin main",
        ] {
            assert_eq!(passed(&repo, command), NOT_DEFAULT, "{command}");
        }
        let call = ShellCall {
            command: "git status".into(),
            cwd: None,
        };
        let routed = route(
            &config(&repo),
            &call,
            Instant::now() + Duration::from_secs(20),
        );
        assert!(matches!(routed, Routed::NotAPush));
    }

    /// Review of PR #576: `--no-dry-run` after the probe's `--dry-run`, or a
    /// trailing `--repo` taking it as its value, made the probe a real push,
    /// with the pre-push hook off. Only allowed options reach the probe, after
    /// its own. Git is what is verified: the bare remote's `main` must not
    /// move, with a remote named `--dry-run` configured as the review found.
    #[test]
    fn no_word_of_the_push_turns_its_dry_run_into_a_push() {
        let repo = repo("no-dry-run");
        main_ahead(&repo);
        git_ok(
            &repo.work,
            &["config", "remote.--dry-run.url", "../origin.git"],
        );
        git_ok(&repo.work, &["config", "push.default", "current"]);
        let remote_main = || {
            let out = Command::new("git")
                .args(["rev-parse", "main"])
                .current_dir(repo.base.join("origin.git"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let before = remote_main();
        for (command, word) in [
            ("git push origin main --repo", "--repo"),
            ("git push --repo origin main", "--repo"),
            ("git push --no-dry-run origin main", "--no-dry-run"),
            ("git push --no-dry origin main", "--no-dry"),
            ("git push --no-porcelain origin main", "--no-porcelain"),
            ("git push --qui origin main", "--qui"),
            ("git push origin -- main", "--"),
            ("git push origin main -o", "-o"),
            (
                "git push --prune origin 'refs/heads/*:refs/heads/*'",
                "--prune",
            ),
            ("git -c remote.--dry-run.url=../origin.git push", "-c"),
        ] {
            let reason = refused(&repo, command);
            assert!(reason.contains(&format!("`{word}`")), "{command}: {reason}");
            assert_eq!(remote_main(), before, "{command} pushed");
        }
        for command in [
            "git push -q origin main",
            "git push origin main --quiet",
            "git push -u --set-upstream -v --verbose origin main",
            "git push --tags --follow-tags --atomic --progress origin main",
            "git push --no-follow-tags --no-progress origin main",
        ] {
            assert!(routed(&repo, command).contains(&lands("main")), "{command}");
            assert_eq!(remote_main(), before, "{command} pushed");
        }
    }

    /// Review of PR #576: a dry run that passes and lists no refs, or
    /// lines LangSpan cannot read, is no answer, and the push is refused.
    #[test]
    fn a_dry_run_without_a_ref_list_refuses_the_push() {
        let repo = repo("no-refs");
        let reason = refused(&repo, "git push origin 'refs/heads/nope*:refs/heads/nope*'");
        assert!(reason.contains("listed no refs"), "{reason}");
    }

    #[test]
    fn forcing_or_deleting_the_default_branch_is_routed_and_says_so() {
        let repo = repo("force");
        let forced = routed(&repo, "git push origin +feat:main");
        assert!(forced.contains("landing never does"), "{forced}");
        assert!(forced.contains(&lands("feat")), "{forced}");
        let delete = routed(&repo, "git push origin :main");
        assert!(delete.contains("would delete main"), "{delete}");
        assert!(!delete.contains("land "), "{delete}");
        // The options are refused; the refusal says to use a refspec.
        for command in [
            "git push -f origin feat:main",
            "git push --force-with-lease origin feat:main",
            "git push origin --delete main",
        ] {
            let reason = refused(&repo, command);
            assert!(reason.contains("`+SRC:DST`, `:DST`"), "{command}: {reason}");
        }
    }

    /// Review of PR #576: a push combined with other commands was read in
    /// part. Now any `git push` with another command is refused, even to a
    /// feature branch, and runs nothing.
    #[test]
    fn a_push_combined_with_another_command_is_refused() {
        let repo = repo("compound");
        for command in [
            "git add -A && git push origin feat:main",
            "git push origin feat && echo done",
            "git push origin feat; git status",
            "git push origin feat | tee log",
            "(cd . ; git push origin feat)",
            "cd . && git push origin feat",
            "env GIT_TRACE=1 git push origin feat",
        ] {
            let reason = refused(&repo, command);
            assert!(reason.contains("command of its own"), "{command}: {reason}");
        }
        assert_eq!(passed(&repo, "git push origin feat"), NOT_DEFAULT);
    }

    /// Review of PR #576: `remote.origin.mirror` makes a plain `git push`
    /// update every ref, the default branch with them.
    #[test]
    fn a_mirror_remote_pushes_the_default_branch_too() {
        let repo = repo("mirror");
        main_ahead(&repo);
        assert_eq!(passed(&repo, "git push origin"), NOT_DEFAULT);
        git_ok(&repo.work, &["config", "remote.origin.mirror", "true"]);
        for command in ["git push", "git push origin"] {
            let reason = routed(&repo, command);
            assert!(reason.contains(&lands("main")), "{command}: {reason}");
            assert!(reason.contains("other refs too"), "{command}: {reason}");
        }
    }

    /// Review of PR #576: a configured refspec and wildcard refspecs decide
    /// where a push goes, and Git applies them. `git -c` is refused, since a
    /// setting can change what the dry run does.
    #[test]
    fn configured_and_wildcard_refspecs_are_gits_to_apply_and_git_c_is_refused() {
        let repo = repo("dash-c");
        git_ok(
            &repo.work,
            &[
                "config",
                "remote.origin.push",
                "refs/heads/feat:refs/heads/main",
            ],
        );
        assert!(routed(&repo, "git push origin").contains(&lands("feat")));
        git_ok(&repo.work, &["config", "--unset", "remote.origin.push"]);
        main_ahead(&repo);
        let glob = "git push origin 'refs/heads/*:refs/heads/*'";
        assert!(routed(&repo, glob).contains(&lands("main")));
        for command in [
            "git -c core.sshCommand=false push origin feat",
            "git -c push.default=matching push",
            "git -c langspan.defaultBranchPush=direct push origin feat:main",
        ] {
            let reason = refused(&repo, command);
            assert!(reason.contains("`-c` before `push`"), "{command}: {reason}");
        }
    }

    /// Review of PR #576: `push.default` and `remote.pushDefault` decide
    /// where a plain `git push` goes, and Git applies them.
    #[test]
    fn push_default_and_push_remote_settings_are_gits_to_apply() {
        let repo = repo("push-default");
        git_ok(&repo.work, &["config", "push.default", "upstream"]);
        git_ok(&repo.work, &["branch", "-q", "-u", "origin/main"]);
        assert!(routed(&repo, "git push").contains(&lands("feat")));
        git_ok(&repo.work, &["config", "push.default", "current"]);
        assert_eq!(passed(&repo, "git push"), NOT_DEFAULT);
        git_ok(&repo.work, &["remote", "add", "fork", "../origin.git"]);
        git_ok(&repo.work, &["fetch", "-q", "fork"]);
        git_ok(&repo.work, &["remote", "set-head", "fork", "main"]);
        git_ok(&repo.work, &["config", "remote.pushDefault", "fork"]);
        git_ok(&repo.work, &["config", "push.default", "matching"]);
        main_ahead(&repo);
        let reason = routed(&repo, "git push");
        assert!(
            reason.contains("--onto main --remote fork --name CHECK refs/heads/main"),
            "{reason}"
        );
    }

    #[test]
    fn a_dry_run_that_fails_or_runs_out_of_time_refuses_the_push() {
        let repo = repo("fails");
        let reason = refused(&repo, "git push nowhere feat");
        assert!(reason.contains("Git refused its dry run"), "{reason}");
        assert!(reason.contains("not instructions"), "{reason}");
        let call = ShellCall {
            command: "git push origin feat".into(),
            cwd: None,
        };
        let Routed::Denied(reply) = route(&config(&repo), &call, Instant::now()) else {
            panic!("a push past its deadline ran");
        };
        assert!(
            reply.stdout.contains("did not answer in time"),
            "{}",
            reply.stdout
        );
    }

    /// The landing command lands from the workspace's root, so a push run in
    /// another checkout is not routed there; one run from a subdirectory of
    /// the workspace is.
    #[test]
    fn a_push_in_another_checkout_passes_and_one_in_a_subdirectory_is_routed() {
        let repo = repo("elsewhere");
        git_ok(&repo.base, &["clone", "-q", "origin.git", "other"]);
        git_ok(&repo.base.join("other"), &["switch", "-q", "feat"]);
        let detail = passed(&repo, "git -C ../other push origin feat:main");
        assert!(detail.contains("another checkout"), "{detail}");
        let sub = repo.work.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let command = "git push origin feat:main";
        let reason = denial(command, hook_in(&repo, Some(&sub), command));
        assert!(reason.contains(&lands("feat")), "{reason}");
    }

    #[test]
    fn the_repositorys_setting_an_unknown_default_or_an_unknown_url_passes_with_a_detail() {
        let repo = repo("setting");
        git_ok(&repo.work, &["config", DIRECT_SETTING, "direct"]);
        let detail = passed(&repo, "git push origin feat:main");
        assert!(detail.contains("is `direct`"), "{detail}");
        git_ok(&repo.work, &["config", "--unset", DIRECT_SETTING]);
        git_ok(
            &repo.base,
            &["clone", "-q", "--bare", "origin.git", "copy.git"],
        );
        let detail = passed(&repo, "git push ../copy.git feat:main");
        assert!(detail.contains("no configured remote"), "{detail}");
        git_ok(&repo.work, &["remote", "set-head", "origin", "-d"]);
        let detail = passed(&repo, "git push origin feat:main");
        assert!(
            detail.contains("git remote set-head origin --auto"),
            "{detail}"
        );
    }

    #[test]
    fn the_ref_list_is_read_per_remote_url() {
        let out = "To ../o.git\n!\trefs/heads/feat:refs/heads/main\t[rejected] (non-fast-forward)\n-\t:refs/heads/old\t[deleted]\n=\trefs/heads/x:refs/heads/x\t[up to date]\nDone\n";
        let update = |flag, from: &str, to: &str| Update {
            url: "../o.git".into(),
            flag,
            from: from.into(),
            to: to.into(),
        };
        assert_eq!(
            porcelain(out),
            Some(vec![
                update('!', "refs/heads/feat", "refs/heads/main"),
                update('-', "", "refs/heads/old"),
                update('=', "refs/heads/x", "refs/heads/x"),
            ])
        );
        // Git's own messages hold no ref, and `--no-porcelain`'s list (which
        // `git_push` refuses) is read as none.
        for empty in [
            "",
            "Everything up-to-date\n",
            "To ../o.git\n   abc..def  main -> main\nDone\n",
        ] {
            assert_eq!(porcelain(empty), Some(vec![]), "{empty}");
        }
        for unread in [
            "*\trefs/heads/x:refs/heads/x\t[new branch]\n",
            "To ../o.git\n**\trefs/heads/x:refs/heads/x\t[new branch]\n",
            "To ../o.git\n*\trefs/heads/x:refs/heads/x\n",
        ] {
            assert_eq!(porcelain(unread), None, "{unread}");
        }
    }

    /// Git shows a push URL without its user or password.
    #[test]
    fn a_url_is_compared_without_its_user() {
        for (url, shown) in [
            ("git@github.com:o/r.git", "github.com:o/r.git"),
            ("https://u:p@example.com/o/r", "https://example.com/o/r"),
            ("ssh://git@example.com", "ssh://example.com"),
            ("../origin.git", "../origin.git"),
            ("/srv/a:b@c", "/srv/a:b@c"),
        ] {
            assert_eq!(anonymous(url), shown, "{url}");
            assert_eq!(anonymous(shown), shown, "{shown}");
        }
    }
}
