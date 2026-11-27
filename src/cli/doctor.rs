//! `langspan doctor`: what would explain why nothing is being coordinated.

use super::{ask, render::coverage_text};
#[cfg(unix)]
use crate::client;
use crate::{hosts::UserHome, protocol::PROTOCOL_VERSION};
use langspan_core::{Command, Outcome};
use std::path::PathBuf;

/// What `doctor` learned from the coordinator about where LangSpan is in use.
struct InUse {
    /// Registered workspace roots.
    roots: Vec<PathBuf>,
    /// The hosts named by registered sessions' IDs.
    hosts: std::collections::BTreeSet<String>,
}

/// Everything that would explain "why is nothing being coordinated": is the
/// coordinator reachable, are hooks installed and pointing at it, is each
/// workspace being scanned and indexed, is publication working. Exit 0 only
/// when nothing needs attention.
pub fn doctor(
    state_dir: &std::path::Path,
    root: Option<&std::path::Path>,
    home: &UserHome,
    json: bool,
) -> Result<u8, crate::Error> {
    let mut findings: Vec<(bool, String)> = Vec::new();
    let mut note = |ok: bool, text: String| findings.push((ok, text));
    note(
        true,
        format!(
            "Source index languages: {}. Other source files are listed by `langspan graph blind-spots`.",
            langspan_graph::Language::ALL
                .iter()
                .map(|language| language.name())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    // Registered workspace roots and the hosts whose sessions work in them.
    let mut in_use: Option<InUse> = None;
    // The running coordinator's build, when it says (roadmap I5).
    let mut running: Option<(String, u32)> = None;
    // Each executable an installed hook or MCP entry for this state
    // directory names, with what names it, in the order found.
    let mut binaries: Vec<(String, PathBuf)> = Vec::new();
    // What doctor judges a workspace by must be complete, even when the
    // bounded view leaves it out to fit a frame.
    match crate::client::status_from(state_dir, &["workspace_git", "remote_peers"]) {
        // A socket that accepts is a coordinator that runs but is stuck
        // (issue #329): starting another would find it holding the lock.
        Err(error) if accepts_connections(state_dir) => note(
            false,
            format!(
                "Coordinator at {} accepts connections but does not answer: {error}. It is running but stuck; restart it: {}.",
                state_dir.display(),
                restart_hint(home, state_dir, None)
            ),
        ),
        Err(error) => note(
            false,
            format!(
                "Coordinator not reachable at {}: {error}. Run `langspan setup` to install it as a login service, or start it with `langspan daemon`. Until then a hook starts one when it finds none, unless LANGSPAN_AUTOSTART=off or a login service is installed for it.",
                state_dir.display()
            ),
        ),
        Ok((state, _)) => {
            in_use = Some(InUse {
                roots: state
                    .workspaces
                    .values()
                    .map(|workspace| PathBuf::from(&workspace.root))
                    .collect(),
                hosts: state
                    .sessions
                    .keys()
                    .filter_map(|id| id.split_once('.').map(|(host, _)| host.to_owned()))
                    .collect(),
            });
            note(
                true,
                format!(
                    "Coordinator reachable, generation {}, protocol {PROTOCOL_VERSION}.",
                    state.generation
                ),
            );
            match ask(state_dir, "read".into(), Command::Version) {
                Ok(Outcome::Version { version, pid }) => {
                    note(
                        true,
                        format!("Coordinator version {version}, process {pid}."),
                    );
                    running = Some((version, pid));
                }
                // A coordinator from before `version` refuses it as a
                // request it cannot parse.
                answer => note(
                    false,
                    format!(
                        "The coordinator does not report its version ({}), so it runs a build from before this one, {}; restart it: {}.",
                        match answer {
                            Ok(Outcome::Error { message, .. }) => message,
                            Ok(other) => format!("unexpected answer {other:?}"),
                            Err(error) => error.to_string(),
                        },
                        crate::binary_version::VERSION,
                        restart_hint(home, state_dir, None)
                    ),
                ),
            }
            let stale = state
                .sessions
                .values()
                .filter(|session| session.generation != state.generation)
                .count();
            note(
                true,
                format!(
                    "{} live or recent session(s) registered since it started; {stale} from before it restarted are not coordinated until they act again.",
                    state.sessions.len() - stale
                ),
            );
            // The normal state of a fresh install, not a fault (issue #291).
            if state.workspaces.is_empty() {
                note(
                    true,
                    "No workspace yet. Start an agent session in a Git repository; its first hook registers one."
                        .into(),
                );
            }
            for workspace in state.workspaces.values() {
                let id = &workspace.workspace_id;
                if !std::path::Path::new(&workspace.root).is_dir() {
                    note(
                        false,
                        format!(
                            "Workspace {id}: its root {:?} no longer exists.",
                            workspace.root
                        ),
                    );
                    continue;
                }
                match state.scan_failures.get(id) {
                    Some(failure) => note(
                        false,
                        format!(
                            "Workspace {id}: the last scan failed ({}). File observations, check staleness and the index are as of the last scan that completed.",
                            failure.detail
                        ),
                    ),
                    None if state.workspace_git.contains_key(id) => {
                        match state.scan_coverage.get(id) {
                            // A stated limit of the scan, not a fault the owner
                            // can fix: reported, without failing the check.
                            Some(coverage) => note(
                                true,
                                format!(
                                    "Workspace {id}: scanned; repository and branch known. {}",
                                    coverage_text(coverage)
                                ),
                            ),
                            None => note(
                                true,
                                format!(
                                    "Workspace {id}: scanned; repository and branch known; every file versioned."
                                ),
                            ),
                        }
                    }
                    None => note(
                        false,
                        format!(
                            "Workspace {id}: no scan has completed yet, so worktrees are not grouped and nothing is indexed."
                        ),
                    ),
                }
                if let Ok(Outcome::Graph { coverage, .. }) = ask(
                    state_dir,
                    "read".into(),
                    Command::GraphSymbol {
                        workspace_id: id.clone(),
                        name: "_".into(),
                        limit: 1,
                        detail: false,
                        blind_in_scope: true,
                        rank: false,
                        session_id: None,
                        renamed: false,
                    },
                ) {
                    let behind = coverage.indexable_files - coverage.current_files;
                    note(
                        behind == 0,
                        format!(
                            "Workspace {id}: index holds {} of {} supported source files at their current content{}.",
                            coverage.current_files,
                            coverage.indexable_files,
                            if coverage.partial_files > 0 {
                                format!("; {} only partly parsed", coverage.partial_files)
                            } else {
                                String::new()
                            }
                        ),
                    );
                    if coverage.blind_in_scope > 0 {
                        note(
                            false,
                            format!(
                                "Workspace {id}: the index cannot read {} supported file(s) in full; `langspan graph blind-spots {id}` names them.",
                                coverage.blind_in_scope
                            ),
                        );
                    }
                }
                if let Some(link) = state.remote_links.get(id) {
                    let heard = state
                        .remote_peers
                        .values()
                        .filter(|peer| &peer.workspace_id == id)
                        .count();
                    note(
                        link.enabled || link.withdrawn,
                        match (link.enabled, link.withdrawn) {
                            (true, _) => format!(
                                "Workspace {id}: publishing to {:?} at {} as {:?}; {} publication(s) recorded; {heard} teammate(s) heard.{}",
                                link.remote,
                                crate::remote::peer_ref(link),
                                link.name,
                                link.sequence,
                                if link.sequence == 0 {
                                    " Nothing has been published yet: either there is no work to publish or the remote is not accepting it; see the coordinator's log."
                                } else {
                                    ""
                                }
                            ),
                            (false, true) => format!(
                                "Workspace {id}: publication is off and its ref is removed."
                            ),
                            (false, false) => format!(
                                "Workspace {id}: publication is off but its ref {} has not yet been removed from {:?}. The coordinator keeps trying; if the host refuses deletions (see its log), delete the ref there by hand.",
                                crate::remote::peer_ref(link),
                                link.remote
                            ),
                        },
                    );
                }
            }
        }
    }
    if let Some(root) = root {
        use crate::install::{self, Vendor};
        let mut shell_hooks = false;
        for vendor in Vendor::ALL {
            let path = root.join(vendor.project_hook_files()[0]);
            let (managed, missing, stale): (Vec<install::Registration>, Vec<&str>, Vec<&str>) =
                match install::read(&path) {
                    Err(error) => {
                        note(false, format!("{}: {error}", path.display()));
                        continue;
                    }
                    Ok(None) => (Vec::new(), Vec::new(), Vec::new()),
                    Ok(Some(settings)) => {
                        let ours = |hook: &serde_json::Value| install::is_managed_for(vendor, hook);
                        if install::switched_off(vendor, &settings) {
                            note(false, switched_off_finding(vendor, &path));
                        }
                        (
                            install::managed_hooks(&settings, ours)
                                .iter()
                                .filter_map(|hook| install::parse_project_hook(vendor, hook))
                                .collect(),
                            install::missing_events(vendor, &settings, ours),
                            install::stale_matchers(vendor, &settings, ours),
                        )
                    }
                };
            // Cursor and Antigravity are newer and optional: their absence
            // is not a problem.
            if managed.is_empty() && matches!(vendor, Vendor::Cursor | Vendor::Antigravity) {
                continue;
            }
            if managed.is_empty() {
                note(
                    false,
                    format!(
                        "{} hooks are not installed in {}. Run `langspan hooks install {} --workspace ID --root {} --apply`.",
                        vendor.name(),
                        root.display(),
                        vendor.name(),
                        root.display()
                    ),
                );
                continue;
            }
            // Each event's registration is read on its own, so each may name
            // its own executable and state directory: all of them count.
            let points_here = managed
                .iter()
                .all(|hook| install::same_directory(&hook.state_dir, state_dir));
            let exists = managed.iter().all(|hook| hook.executable.is_file());
            for hook in &managed {
                if install::same_directory(&hook.state_dir, state_dir) {
                    binaries.push((
                        format!("Hooks ({}) in {}", vendor.name(), root.display()),
                        hook.executable.clone(),
                    ));
                }
            }
            note(
                points_here && exists && missing.is_empty() && stale.is_empty(),
                format!(
                    "{} hooks installed for {} event(s){}{}{}{}{}",
                    vendor.name(),
                    managed.len(),
                    if missing.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; not for {}, which an install from an earlier version leaves out: run `langspan hooks install {} --workspace ID --root {} --apply` again",
                            missing.join(", "),
                            vendor.name(),
                            root.display()
                        )
                    },
                    if stale.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; {}: {}, so tools LangSpan handles are not coordinated: run `langspan hooks install {} --workspace ID --root {} --apply` again",
                            stale.join(", "),
                            STALE_MATCHER,
                            vendor.name(),
                            root.display()
                        )
                    },
                    if exists {
                        String::new()
                    } else {
                        format!(
                            "; the installed executable no longer exists, so they do nothing: run `langspan hooks install {} --workspace ID --root {} --apply` again",
                            vendor.name(),
                            root.display()
                        )
                    },
                    if points_here {
                        ""
                    } else {
                        "; they use a different state directory than this command, so they talk to a different coordinator"
                    },
                    match vendor {
                        Vendor::Codex =>
                            ". Codex runs project hooks only after you approve them in its own review.",
                        Vendor::Cursor =>
                            ". Cursor: built from documentation, not yet verified on a real Cursor (ADR 0032); Cursor runs project hooks only in a trusted workspace.",
                        Vendor::Antigravity =>
                            ". Antigravity: hooks verified on a real host, agy 1.5.14 (ADR 0035); `/hooks` in its CLI lists the hooks it loaded.",
                        // Not in `Vendor::ALL`: see `gemini_leftovers`.
                        Vendor::ClaudeCode | Vendor::Gemini => ".",
                    }
                ),
            );
            if let Some(mcp) = vendor.project_mcp_file() {
                let name = vendor.name();
                let mcp = root.join(mcp);
                match install::read_mcp(vendor, &mcp).map(|config| {
                    config
                        .as_ref()
                        .and_then(|config| config.get("mcpServers"))
                        .and_then(|servers| servers.get(crate::user_install::SERVER_NAME))
                        .map(crate::user_install::parse_claude_entry)
                }) {
                    Err(error) => note(false, format!("{}: {error}", mcp.display())),
                    Ok(None) => note(
                        false,
                        format!(
                            "{name}: the MCP server is not registered in {}; run `langspan hooks install {name} --workspace ID --root {} --apply`.",
                            mcp.display(),
                            root.display()
                        ),
                    ),
                    Ok(Some(None)) => note(
                        false,
                        format!(
                            "{name}: an MCP server named `langspan` is registered in {} but LangSpan did not write it.",
                            mcp.display()
                        ),
                    ),
                    Ok(Some(Some(server))) => note(
                        {
                            let here = install::same_directory(&server.state_dir, state_dir);
                            if here {
                                binaries.push((
                                    format!("MCP server ({name}) in {}", mcp.display()),
                                    server.executable.clone(),
                                ));
                            }
                            here && server.executable.is_file()
                        },
                        format!(
                            "{name}: MCP server `langspan` registered in {}, state directory {}.",
                            mcp.display(),
                            server.state_dir.display()
                        ),
                    ),
                }
                let (ok, text) = skill_finding(
                    vendor,
                    &crate::skill::project_path(vendor, root),
                    &format!(
                        "langspan hooks install {name} --workspace ID --root {} --skill --apply",
                        root.display()
                    ),
                );
                note(ok, text);
            }
            shell_hooks = true;
        }
        if shell_hooks {
            note(
                true,
                "Shell commands (Bash) are covered partly: a write is refused before it runs only for literal forms such as `> file`, `sed -i` or `mv`; other commands' changes are found afterwards by comparing Git status, and are not recorded when that comparison is incomplete (ADR 0011).".into(),
            );
        }
        for (ok, text) in worktree_install_findings(state_dir, root) {
            note(ok, text);
        }
    }
    for (ok, text) in user_level_findings(state_dir, home, in_use.as_ref(), &mut binaries)? {
        note(ok, text);
    }
    let restart = running
        .as_ref()
        .map(|(_, pid)| restart_hint(home, state_dir, Some(*pid)));
    for (ok, text) in version_findings(
        running
            .as_ref()
            .map(|(version, _)| version.as_str())
            .zip(restart.as_deref()),
        &binaries,
    ) {
        note(ok, text);
    }
    for text in gemini_leftovers(home, root) {
        note(false, text);
    }
    let problems = findings.iter().filter(|(ok, _)| !ok).count();
    if json {
        let items: Vec<serde_json::Value> = findings
            .iter()
            .map(|(ok, text)| serde_json::json!({"ok": ok, "detail": text}))
            .collect();
        println!(
            "{}",
            serde_json::json!({"problems": problems, "findings": items})
        );
    } else {
        for (ok, text) in &findings {
            println!("{} {text}", if *ok { "ok" } else { "FIX" });
        }
        println!(
            "{}",
            if problems == 0 {
                "Nothing needs attention.".to_owned()
            } else {
                format!("{problems} thing(s) need attention.")
            }
        );
    }
    Ok(u8::from(problems > 0))
}

/// What `doctor` says about each linked worktree of `root`'s repository with
/// a LangSpan install of its own that the host may not run there
/// (`Vendor::worktree_hooks`, ADR 0003).
fn worktree_install_findings(
    state_dir: &std::path::Path,
    root: &std::path::Path,
) -> Vec<(bool, String)> {
    use crate::{
        hook::install_at,
        install::{Vendor, WorktreeHooks},
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let listed = match crate::git::output(
        root,
        &["worktree", "list", "--porcelain", "-z"],
        deadline,
        1 << 20,
    ) {
        Ok(listed) => listed,
        Err(error) => {
            return vec![(
                true,
                format!(
                    "Linked worktrees of {} were not checked: {error}.",
                    root.display()
                ),
            )];
        }
    };
    // The main worktree comes first; a listed worktree that no longer
    // exists on disk has no install to run.
    let mut trees = listed
        .split(|byte| *byte == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .filter_map(|path| std::fs::canonicalize(String::from_utf8_lossy(path).as_ref()).ok());
    let Some(main) = trees.next() else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for tree in trees {
        for vendor in Vendor::ALL {
            let Some((file, own)) = install_at(vendor, state_dir, &tree) else {
                continue;
            };
            let name = vendor.name();
            match vendor.worktree_hooks() {
                WorktreeHooks::Both => {}
                WorktreeHooks::MainOnly => {
                    let fix = format!(
                        "run `langspan hooks install {name} --workspace ID --root {} --apply`",
                        main.display()
                    );
                    let coverage = match install_at(vendor, state_dir, &main) {
                        None => Err(format!("Nothing coordinates it: {fix}.")),
                        Some((main_file, registration)) => {
                            match main_install_problem(vendor, &main_file, &registration) {
                                Some(problem) => Err(format!(
                                    "Nothing coordinates it: the main checkout's hook in {} {problem}; {fix} again.",
                                    main_file.display()
                                )),
                                None => Ok(format!(
                                    "The main checkout's hook coordinates it as workspace {}.",
                                    own.workspace_id
                                )),
                            }
                        }
                    };
                    findings.push((
                        coverage.is_ok(),
                        format!(
                            "{name}: a session started in linked worktree {} does not run the hooks installed in {} (observed on {}); it runs the main checkout's project hooks. {}",
                            tree.display(),
                            file.display(),
                            vendor.worktree_hooks_observed().unwrap_or("this host"),
                            coverage.unwrap_or_else(|problem| problem)
                        ),
                    ));
                }
                WorktreeHooks::Unconfirmed => findings.push((
                    true,
                    format!(
                        "{name}: whether a session started in linked worktree {} runs the hooks installed in {} has not been observed on a real host. The main checkout's hook, where the host runs it there, also decides, as workspace {}, so it does not step aside.",
                        tree.display(),
                        file.display(),
                        own.workspace_id
                    ),
                )),
            }
        }
    }
    findings
}

/// Why the main checkout's hook in `file`, with `registration`, cannot run
/// for a linked worktree, if it cannot: its executable is gone or it lacks
/// an event the host sends. Checked as `doctor` checks the root it is given,
/// since for a host that runs only the main checkout's hooks it is the one
/// hook a worktree session has.
fn main_install_problem(
    vendor: crate::install::Vendor,
    file: &std::path::Path,
    registration: &crate::install::Registration,
) -> Option<String> {
    use crate::install;
    let mut problems = Vec::new();
    if !registration.executable.is_file() {
        problems.push("cannot start: its executable no longer exists".to_owned());
    }
    match install::read(file) {
        Ok(Some(settings)) => {
            let missing = install::missing_events(vendor, &settings, |hook| {
                install::is_managed_for(vendor, hook)
            });
            if !missing.is_empty() {
                problems.push(format!("is not installed for {}", missing.join(", ")));
            }
            let stale = install::stale_matchers(vendor, &settings, |hook| {
                install::is_managed_for(vendor, hook)
            });
            if !stale.is_empty() {
                problems.push(format!("has, for {}, {STALE_MATCHER}", stale.join(", ")));
            }
        }
        Ok(None) => problems.push("could not be read".to_owned()),
        Err(error) => problems.push(format!("could not be read: {error}")),
    }
    (!problems.is_empty()).then(|| problems.join(" and "))
}

/// What `doctor` says of the tool events whose installed matcher is not the
/// one this version writes (`install::stale_matchers`).
const STALE_MATCHER: &str = "the matcher does not name the tools this version handles";

/// Whether `state_dir`'s socket accepts a connection within a second.
#[cfg(unix)]
fn accepts_connections(state_dir: &std::path::Path) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    client::accepts(state_dir, deadline)
}
#[cfg(not(unix))]
fn accepts_connections(_: &std::path::Path) -> bool {
    false
}

/// How to restart the coordinator for `state_dir`: through the login
/// service when `setup` installed one, otherwise by stopping process `pid`
/// (when known) and starting it again. Never run by `doctor` itself.
fn restart_hint(home: &UserHome, state_dir: &std::path::Path, pid: Option<u32>) -> String {
    use crate::service::{self, Manager};
    let then = "do it between agents' turns: a restart ends the leases they hold, and each session is coordinated again when it next acts";
    if service::installed_for(home, state_dir) {
        match Manager::current() {
            Some(Manager::Launchd) => {
                return format!(
                    "`launchctl kickstart -k gui/{}/{}` ({then})",
                    rustix::process::getuid().as_raw(),
                    service::LABEL
                );
            }
            Some(Manager::Systemd) => {
                return format!("`systemctl --user restart {}` ({then})", service::UNIT);
            }
            None => {}
        }
    }
    format!(
        "stop it ({}) and start it with `langspan daemon --state-dir {}` ({then})",
        match pid {
            Some(pid) => format!("`kill {pid}`"),
            None => "the `langspan daemon` process".to_owned(),
        },
        state_dir.display()
    )
}

/// What `doctor` says of each executable in `binaries` (what names it, and
/// its path), against the coordinator's version and how to restart it in
/// `running`, when it says. The version is read from the file
/// (`binary_version::of_file`), never by running it: a project's
/// configuration names it. One finding per executable, naming everything
/// that runs it.
fn version_findings(
    running: Option<(&str, &str)>,
    binaries: &[(String, PathBuf)],
) -> Vec<(bool, String)> {
    use crate::binary_version;
    use std::cmp::Ordering;
    let mut grouped: Vec<(Vec<&str>, &std::path::Path)> = Vec::new();
    for (label, path) in binaries {
        match grouped.iter_mut().find(|(_, seen)| *seen == path.as_path()) {
            // Each hook event of one install adds the same label.
            Some((labels, _)) if labels.contains(&label.as_str()) => {}
            Some((labels, _)) => labels.push(label),
            None => grouped.push((vec![label], path)),
        }
    }
    let mut findings = Vec::new();
    for (labels, path) in grouped {
        let labels = labels.join(", ");
        let path_text = path.display();
        let version = match binary_version::of_file(path) {
            // A missing executable is already reported with its install.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                findings.push((
                    false,
                    format!("{labels}: {path_text} could not be read for its version: {error}."),
                ));
                continue;
            }
            Ok(version) => version,
        };
        let finding = match (version, running) {
            (None, None) => (
                true,
                format!(
                    "{labels}: {path_text} does not record its version: a LangSpan build from before versions were recorded, or not LangSpan."
                ),
            ),
            (None, Some((coordinator, _))) => (
                false,
                format!(
                    "{labels}: {path_text} does not record its version, so it is a LangSpan build from before versions were recorded, or not LangSpan; the coordinator runs version {coordinator}. Upgrade the binary at that path, or install these again from the coordinator's."
                ),
            ),
            (Some(version), None) => (true, format!("{labels}: {path_text}, version {version}.")),
            (Some(version), Some((coordinator, _))) if version == coordinator => (
                true,
                format!("{labels}: {path_text}, version {version}, as the coordinator's."),
            ),
            (Some(version), Some((coordinator, restart))) => (
                false,
                match binary_version::order(coordinator, &version) {
                    Some(Ordering::Less) => format!(
                        "{labels}: {path_text}, version {version}; the coordinator, version {coordinator}, is older than this binary; restart it: {restart}."
                    ),
                    Some(Ordering::Greater) => format!(
                        "{labels}: {path_text}, version {version}; the coordinator, version {coordinator}, is newer than this binary. Upgrade the binary at that path, or install these again from the coordinator's."
                    ),
                    _ => format!(
                        "{labels}: {path_text}, version {version}; the coordinator runs version {coordinator}. Run one version: upgrade the binary at that path, then restart the coordinator: {restart}."
                    ),
                },
            ),
        };
        findings.push(finding);
    }
    findings
}

/// What `doctor` says about the user-level install of each host present
/// under the home: hooks, MCP registration, the executable both run, and the
/// state directory both point at.
/// What an install made before Gemini CLI support was removed left behind:
/// LangSpan's project hooks in `root`'s `.gemini/settings.json`, and its
/// user-level hooks, MCP server entry and skill section under the home.
/// Each is a problem, with the command that removes it. A file LangSpan
/// cannot read (Gemini CLI accepts comments in its settings) holds nothing
/// LangSpan wrote, so it says nothing.
fn gemini_leftovers(home: &UserHome, root: Option<&std::path::Path>) -> Vec<String> {
    use crate::{install, install::Vendor, user_install};
    let vendor = Vendor::Gemini;
    let name = vendor.name();
    let mut found = Vec::new();
    if let Some(root) = root {
        let path = root.join(vendor.project_hook_files()[0]);
        let hooks = match install::read(&path) {
            Ok(Some(settings)) => {
                install::managed_hooks(&settings, |hook| install::is_managed_for(vendor, hook))
                    .len()
            }
            _ => 0,
        };
        if hooks > 0 {
            found.push(format!(
                "{name}: {} holds {hooks} LangSpan hook(s) from an earlier install. Gemini CLI support was removed, so they do nothing; run `langspan hooks remove {name} --root {} --apply`.",
                path.display(),
                root.display()
            ));
        }
    }
    let files = home.files(vendor);
    let mut parts = Vec::new();
    if let Ok(Some(settings)) = install::read(&files.hooks) {
        let hooks =
            install::managed_hooks(&settings, |hook| install::is_user_managed_for(vendor, hook))
                .len();
        if hooks > 0 {
            parts.push(format!("{hooks} LangSpan hook(s)"));
        }
        if settings["mcpServers"]
            .get(user_install::SERVER_NAME)
            .and_then(user_install::parse_claude_entry)
            .is_some()
        {
            parts.push("the MCP server `langspan`".to_owned());
        }
    }
    let skill = crate::skill::path(vendor, &files.config_dir);
    if install::read_text(&skill)
        .ok()
        .flatten()
        .is_some_and(|text| {
            crate::skill::state(vendor, Some(&text))
                .is_ok_and(|state| state != crate::skill::State::Absent)
        })
    {
        parts.push(format!("the LangSpan skill section in {}", skill.display()));
    }
    if !parts.is_empty() {
        found.push(format!(
            "{name}: {} from an earlier install {} under {}. Gemini CLI support was removed, so they do nothing; run `langspan uninstall --hosts {name} --apply`.",
            parts.join(", "),
            if parts.len() == 1 { "is" } else { "are" },
            files.config_dir.display()
        ));
    }
    found
}

fn user_level_findings(
    state_dir: &std::path::Path,
    home: &UserHome,
    in_use: Option<&InUse>,
    binaries: &mut Vec<(String, PathBuf)>,
) -> Result<Vec<(bool, String)>, crate::Error> {
    use crate::{install, install::Vendor, user_install};
    let mut findings = Vec::new();
    let check = |executable: &std::path::Path, dir: &std::path::Path| -> String {
        let mut problems = String::new();
        if !executable.is_file() {
            problems.push_str("; its executable no longer exists, so it does nothing: run `langspan install --apply` again");
        }
        if !install::same_directory(dir, state_dir) {
            problems.push_str("; it uses a different state directory than this command, so it talks to a different coordinator");
        }
        problems
    };
    for vendor in Vendor::ALL {
        let files = home.files(vendor);
        if !files.config_dir.is_dir() {
            continue;
        }
        let name = vendor.name();
        if crate::skill::USER_HOSTS.contains(&vendor) {
            let path = crate::skill::path(vendor, &files.config_dir);
            findings.push(skill_finding(
                vendor,
                &path,
                &format!("langspan install --skill --hosts {name} --apply"),
            ));
            if vendor == Vendor::Codex
                && let Some(problem) = crate::skill::codex_unread(&files.config_dir)
            {
                findings.push((false, format!("{name}: {problem}.")));
            }
        }
        let (hooks, missing, stale): (Vec<install::UserRegistration>, Vec<&str>, Vec<&str>) =
            match install::read(&files.hooks) {
                Err(error) => {
                    findings.push((false, format!("{}: {error}", files.hooks.display())));
                    (Vec::new(), Vec::new(), Vec::new())
                }
                Ok(None) => (Vec::new(), Vec::new(), Vec::new()),
                Ok(Some(settings)) => {
                    let ours =
                        |hook: &serde_json::Value| install::is_user_managed_for(vendor, hook);
                    if install::switched_off(vendor, &settings) {
                        findings.push((false, switched_off_finding(vendor, &files.hooks)));
                    }
                    (
                        install::managed_hooks(&settings, ours)
                            .iter()
                            .filter_map(|hook| install::parse_user_hook(vendor, hook))
                            .collect(),
                        install::missing_events(vendor, &settings, ours),
                        install::stale_matchers(vendor, &settings, ours),
                    )
                }
            };
        let server: Result<Option<user_install::McpServer>, String> = match vendor {
            // Gemini CLI is not in `Vendor::ALL`: see `gemini_leftovers`.
            Vendor::ClaudeCode | Vendor::Cursor | Vendor::Gemini | Vendor::Antigravity => match install::read_mcp(vendor, &files.mcp) {
                Err(error) => Err(error.to_string()),
                Ok(config) => match config
                    .as_ref()
                    .and_then(|config| config.get("mcpServers"))
                    .and_then(|servers| servers.get(user_install::SERVER_NAME))
                {
                    None => Ok(None),
                    Some(entry) => user_install::parse_claude_entry(entry).map(Some).ok_or_else(|| {
                        "an MCP server named `langspan` is registered but LangSpan did not write it".into()
                    }),
                },
            },
            Vendor::Codex => install::read_text(&files.mcp)
                .map_err(|e| e.to_string())
                .and_then(|text| user_install::parse_codex(text.as_deref().unwrap_or(""))),
        };
        let server = match server {
            Ok(server) => server,
            Err(error) => {
                findings.push((false, format!("{}: {error}", files.mcp.display())));
                None
            }
        };
        if hooks.is_empty() && server.is_none() {
            // Needs attention when the coordinator is in use by another
            // host's sessions and nothing covers this one (roadmap A7).
            let uncovered = in_use.filter(|in_use| {
                in_use.hosts.iter().any(|host| host != name)
                    && !crate::hosts::not_installed(
                        &[crate::hosts::inspect(vendor, home, state_dir)],
                        &in_use.roots,
                        state_dir,
                    )
                    .is_empty()
            });
            findings.push((
                uncovered.is_none(),
                format!(
                    "{name}: no user-level install under {}; repositories need a project install, or run `{}`.{}",
                    home.home.display(),
                    crate::hosts::install_command(vendor, home, state_dir),
                    if uncovered.is_some() {
                        " Sessions of another host use this coordinator and no registered workspace has a project install for this one, so its edits are not coordinated."
                    } else {
                        ""
                    }
                ),
            ));
            continue;
        }
        // Every event's registration, as for a project install.
        for hook in &hooks {
            if install::same_directory(&hook.state_dir, state_dir) {
                binaries.push((
                    format!("User-level hooks ({name})"),
                    hook.executable.clone(),
                ));
            }
        }
        if let Some(server) = &server
            && install::same_directory(&server.state_dir, state_dir)
        {
            binaries.push((
                format!("User-level MCP server ({name})"),
                server.executable.clone(),
            ));
        }
        match hooks.first() {
            Some(first) => {
                let mut problems = String::new();
                if !missing.is_empty() {
                    problems.push_str(&format!(
                        "; not for {}, which an install from an earlier version leaves out: run `langspan install --hosts {name} --apply` again",
                        missing.join(", ")
                    ));
                }
                if !stale.is_empty() {
                    problems.push_str(&format!(
                        "; {}: {STALE_MATCHER}, so tools LangSpan handles are not coordinated: run `langspan install --hosts {name} --apply` again",
                        stale.join(", ")
                    ));
                }
                for hook in &hooks {
                    let problem = check(&hook.executable, &hook.state_dir);
                    if !problems.contains(&problem) {
                        problems.push_str(&problem);
                    }
                }
                findings.push((
                    problems.is_empty(),
                    format!(
                        "{name}: user-level hooks installed in {} for {} event(s), state directory {}{problems}{}",
                        files.hooks.display(),
                        hooks.len(),
                        first.state_dir.display(),
                        if vendor == Vendor::Codex {
                            ". Codex runs them only after you review and trust them with /hooks."
                        } else {
                            "."
                        }
                    ),
                ));
            }
            None => findings.push((
                false,
                format!(
                    "{name}: the MCP server is registered but the user-level hooks are not installed in {}; run `langspan install --hosts {name} --apply`.",
                    files.hooks.display()
                ),
            )),
        }
        match server {
            Some(server) => {
                let problems = check(&server.executable, &server.state_dir);
                findings.push((
                    problems.is_empty(),
                    format!(
                        "{name}: MCP server `langspan` registered in {}, state directory {}{problems}.",
                        files.mcp.display(),
                        server.state_dir.display()
                    ),
                ));
            }
            None => findings.push((
                false,
                format!(
                    "{name}: user-level hooks are installed but the MCP server is not registered in {}; run `langspan install --hosts {name} --apply`.",
                    files.mcp.display()
                ),
            )),
        }
    }
    Ok(findings)
}

/// What `doctor` says when the host is told not to run LangSpan's hooks in
/// the file at `path` at all.
fn switched_off_finding(vendor: crate::install::Vendor, path: &std::path::Path) -> String {
    format!(
        "{}: the `{}` hook definition in {} has \"enabled\": false, so none of LangSpan's hooks there run; remove that line or set it to true.",
        vendor.name(),
        crate::install::ANTIGRAVITY_HOOK_NAME,
        path.display()
    )
}

/// What `doctor` says about the LangSpan skill in the file at `path`. Not
/// installing it is a choice (it is off by default), not a problem.
fn skill_finding(
    vendor: crate::install::Vendor,
    path: &std::path::Path,
    install: &str,
) -> (bool, String) {
    use crate::skill::{State, file_state};
    let name = vendor.name();
    match file_state(vendor, path) {
        Ok(State::Absent) => (
            true,
            format!(
                "{name}: the LangSpan skill is not installed in {} (optional; `{install}` writes it).",
                path.display()
            ),
        ),
        Ok(State::Current) => (
            true,
            format!(
                "{name}: the LangSpan skill is installed and up to date in {}.",
                path.display()
            ),
        ),
        Ok(State::Outdated) => (
            false,
            format!(
                "{name}: the LangSpan skill in {} differs from this version's; run `{install}` to replace it.",
                path.display()
            ),
        ),
        Ok(State::Unmanaged) => (
            true,
            format!(
                "{name}: {} is a symlink or not a regular file, so the LangSpan skill is not installed there; LangSpan leaves it alone and will not write its skill through it.",
                path.display()
            ),
        ),
        Err(error) => (false, error),
    }
}
