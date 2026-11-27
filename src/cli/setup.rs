//! `langspan install`, `uninstall`, `hooks`, `guidance`, `hosts` and `setup`: the user-level and project installs, and the coordinator's login service.

use super::doctor::doctor;
use super::options::{no_unused_options, outline_reads, presence, switch};
use crate::hosts::UserHome;
use std::{collections::BTreeMap, path::PathBuf};

pub fn hooks(
    vendor: crate::install::Vendor,
    install: bool,
    state_dir: PathBuf,
    options: &mut BTreeMap<String, String>,
    apply: bool,
) -> Result<u8, crate::Error> {
    use crate::install;
    if install && vendor.removed() {
        return Err(install::GEMINI_REMOVED.into());
    }
    let root = std::fs::canonicalize(options.remove("--root").ok_or("--root DIR is required")?)
        .map_err(|e| format!("cannot resolve --root: {e}"))?;
    let settings = options
        .remove("--settings")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(vendor.project_hook_files()[0]));
    let registration = if install {
        Some(install::Registration {
            executable: install::installed_executable(&std::env::current_exe()?)?,
            state_dir: std::path::absolute(&state_dir)?,
            workspace_id: options
                .remove("--workspace")
                .ok_or("--workspace ID is required")?,
            root: root.clone(),
            share_prompt: switch(options, "--prompt-summary")?,
            code_briefing: switch(options, "--code-briefing")?,
            presence: presence(options)?,
            outline_reads: outline_reads(options)?,
        })
    } else {
        None
    };
    let with_skill = options.remove("--skill").is_some();
    let project_skill = crate::skill::PROJECT_HOSTS.contains(&vendor);
    if with_skill && !(install && project_skill) {
        return Err("--skill is valid for `hooks install cursor` and `hooks install antigravity` only: they read the LangSpan skill as a project rule. For claude-code and codex, run `langspan install --skill`; `hooks remove` removes the rule without it".into());
    }
    no_unused_options(options)?;
    if let Some(registration) = &registration {
        langspan_core::validate_request_id(&registration.workspace_id)
            .map_err(|_| "--workspace must be 1..128 ASCII letters, digits, '.', '_' or '-'")?;
    }
    // Cursor reads MCP servers from the project too: register `langspan
    // mcp` beside the hooks.
    let server = registration
        .as_ref()
        .map(|registration| crate::user_install::McpServer {
            executable: registration.executable.clone(),
            state_dir: registration.state_dir.clone(),
            hooked_host: None,
        });
    let registration = registration.as_ref();
    let server = server.as_ref();
    type Plan<'a> =
        Box<dyn Fn(Option<serde_json::Value>) -> Result<serde_json::Value, String> + 'a>;
    // Each file with its plan and whether it is the MCP configuration, which
    // Antigravity leaves empty.
    let mut files: Vec<(PathBuf, Plan, bool)> = vec![(
        settings,
        Box::new(move |existing| install::plan_for(vendor, existing, registration)),
        false,
    )];
    if let Some(mcp) = vendor.project_mcp_file() {
        files.push((
            root.join(mcp),
            Box::new(move |existing| {
                if vendor == install::Vendor::Antigravity {
                    crate::user_install::plan_antigravity(existing, server)
                } else {
                    crate::user_install::plan_claude(
                        existing.unwrap_or_else(|| serde_json::json!({})),
                        server,
                    )
                }
            }),
            true,
        ));
    }
    let parse = |path: &std::path::Path, mcp: bool, bytes: Option<&[u8]>| {
        if mcp {
            install::parse_mcp_json(vendor, path, bytes)
        } else {
            install::parse_json(path, bytes)
        }
    };
    // Every file is planned, and every refusal raised, before any is written.
    let mut planned_files = Vec::new();
    for (path, plan, mcp) in &files {
        let original = install::read_bytes(path)?;
        let existing = parse(path, *mcp, original.as_deref())?;
        let planned = plan(existing.clone()).map_err(|e| format!("{}: {e}", path.display()))?;
        let unchanged = existing.as_ref() == Some(&planned) || (existing.is_none() && !install);
        planned_files.push((path, plan, *mcp, original, planned, unchanged));
    }
    // Cursor's and Antigravity's project rule: written with --skill, removed
    // with the hooks wherever LangSpan's is found.
    let rule = if project_skill && (with_skill || !install) {
        let path = crate::skill::project_path(vendor, &root);
        Some(crate::skill::plan_file(vendor, &path, install)?)
    } else {
        None
    };
    let mut changed = false;
    for (path, plan, mcp, original, planned, unchanged) in planned_files {
        if unchanged {
            println!("{} already matches; nothing to change.", path.display());
            continue;
        }
        changed = true;
        if apply {
            let replan = |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
                let existing = parse(path, mcp, now)?;
                if existing.is_none() && !install {
                    return Err(format!(
                        "{} was removed meanwhile; nothing to remove",
                        path.display()
                    )
                    .into());
                }
                install::json_bytes(
                    &plan(existing).map_err(|e| format!("{}: {e}", path.display()))?,
                )
            };
            install::write_planned(path, original.as_deref(), &replan)?;
            println!("Wrote {}.", path.display());
        } else {
            println!(
                "Dry run; nothing was written. {} would become:",
                path.display()
            );
            if vendor.removed() {
                // Gemini CLI's project settings can hold MCP server
                // credentials too.
                println!("{}", gemini_shown(&planned, false)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&planned)?);
            }
        }
    }
    if let Some(rule) = rule.filter(|rule| install || !rule.unchanged()) {
        if rule.unchanged() {
            println!(
                "{} already matches; nothing to change.",
                rule.path.display()
            );
        } else if !apply {
            changed = true;
            println!("Dry run; nothing was written. {}", rule.shown());
        } else if rule.apply()? {
            match rule.planned {
                Some(_) => println!("Wrote {}.", rule.path.display()),
                None => println!("Removed {}.", rule.path.display()),
            }
        }
    }
    if changed && !apply {
        println!("Run the same command with --apply to write it.");
    }
    if vendor == install::Vendor::Cursor && install {
        println!(
            "Cursor: built from documentation, not yet verified on a real Cursor (ADR 0032). Cursor runs project hooks only in a trusted workspace. These files hold paths on this machine; Cursor's project files are often committed, so keep LangSpan's entries out of a commit."
        );
    }
    if vendor == install::Vendor::Codex && install {
        // Codex trusts project hooks by the exact path of their file: a
        // session resumed under another path for the same directory runs
        // none of them (issue #129).
        println!(
            "Codex: start and resume Codex in '{}', the resolved root with no symbolic links. Codex trusts a project's hooks only at the exact path they were trusted at, so a session started or resumed under another path for the same directory (a symbolic link, or /tmp for /private/tmp on macOS) runs none of LangSpan's hooks, without a warning.",
            root.display()
        );
    }
    if vendor == install::Vendor::Antigravity && install {
        println!(
            "Antigravity: {} Antigravity's App, CLI and IDE all read these files; `/hooks` in the CLI lists the hooks it loaded. They hold paths on this machine, so keep LangSpan's entries out of a commit.",
            crate::antigravity::VERIFIED
        );
    }
    Ok(0)
}

/// `hosts`: each known host, whether its configuration directory exists and
/// what LangSpan install it has. Reads only.
pub fn hosts_report(
    home: &UserHome,
    state_dir: &std::path::Path,
    json: bool,
) -> Result<u8, crate::Error> {
    let reports = crate::hosts::report(home, state_dir);
    if json {
        println!("{}", serde_json::json!({ "hosts": reports }));
        return Ok(0);
    }
    for host in &reports {
        let state = match (host.present, host.user_install) {
            (None, _) => format!(
                "unknown: {}",
                host.error.as_deref().unwrap_or("not readable")
            ),
            (Some(false), _) => "not present".to_owned(),
            (Some(true), crate::hosts::UserInstall::Unknown) => format!(
                "present; install unknown: {}",
                host.error.as_deref().unwrap_or("not readable")
            ),
            (Some(true), install) => format!(
                "present; user-level install: {}",
                serde_json::to_value(install)?
                    .as_str()
                    .unwrap_or_default()
                    .replace('_', " ")
            ),
        };
        println!("{} ({}): {state}", host.name, host.config_dir.display());
        if let Some(next) = &host.next_step {
            println!("  to install: {next}");
        } else if host.not_installed() {
            println!(
                "  chosen only by name: langspan install --hosts {} --apply",
                host.name
            );
        }
    }
    Ok(0)
}

enum Planned {
    /// The existing value, the planned value, and what a dry run shows in
    /// place of the whole planned file, if not all of it.
    Json(Option<serde_json::Value>, serde_json::Value, Option<String>),
    Text(Option<String>, String),
}

/// A file's path, its plan, the bytes the plan was made from, how to make
/// it again from other bytes, and, on uninstall of a file LangSpan may have
/// created, which planned bytes mean nothing is left so the file is removed.
type FilePlan<'a> = (
    PathBuf,
    Planned,
    Option<Vec<u8>>,
    Box<crate::install::Planner<'a>>,
    Option<Box<crate::install::LeftEmpty<'a>>>,
);

/// `guidance`: print LangSpan's block for AGENTS.md and similar files, or
/// write it into (`install`) or take it out of (`remove`) one file of the
/// owner's repository. A dry run unless `--apply`. Needs no coordinator.
pub fn guidance(
    args: &[String],
    options: &mut BTreeMap<String, String>,
    json: bool,
    apply: bool,
) -> Result<u8, crate::Error> {
    use crate::guidance::{self, Format};
    if json {
        return Err("--json is not valid for guidance; it prints text".into());
    }
    // Accepted for every command; the guidance never uses the coordinator.
    options.remove("--state-dir");
    let install = match args {
        [] => {
            let format = match options.remove("--format") {
                None => Format::Agents,
                Some(name) => Format::from_name(&name)
                    .ok_or_else(|| format!("--format {name}: expected {}", Format::USAGE))?,
            };
            no_unused_options(options)?;
            if apply {
                return Err("--apply is valid for `guidance install` and `guidance remove`".into());
            }
            print!("{}", guidance::render(format));
            return Ok(0);
        }
        [action] if action == "install" => true,
        [action] if action == "remove" => false,
        _ => {
            return Err(format!(
                "usage: langspan guidance [--format {}] | guidance install --root DIR --file FILE [--create] [--apply] | guidance remove --root DIR --file FILE [--apply]",
                Format::USAGE
            )
            .into());
        }
    };
    let root = options
        .remove("--root")
        .ok_or("guidance install and remove need --root DIR, the repository")?;
    let file = options
        .remove("--file")
        .ok_or("guidance install and remove need --file FILE, such as AGENTS.md")?;
    let create = options.remove("--create").is_some();
    if create && !install {
        return Err("--create is valid only for `guidance install`".into());
    }
    no_unused_options(options)?;
    let path = guidance::target(std::path::Path::new(&root), &file)?;
    let plan = guidance::plan_file(&path, install, create)?;
    if plan.unchanged() {
        println!(
            "{}",
            if install {
                format!("{file} already has LangSpan's block; nothing changed.")
            } else {
                format!("{file} has no LangSpan block; nothing changed.")
            }
        );
        return Ok(0);
    }
    if !apply {
        print!("{}", plan.diff());
        if plan.planned.is_none() {
            println!("{file} would be removed: it held only LangSpan's block.");
        }
        println!(
            "dry run: add --apply to {} {file}",
            if install { "write" } else { "change" }
        );
        return Ok(0);
    }
    plan.apply()?;
    println!(
        "{}",
        match (install, plan.planned.is_some()) {
            (true, _) => format!("wrote LangSpan's block to {file}"),
            (false, true) => format!("removed LangSpan's block from {file}"),
            (false, false) => format!("removed {file}: it held only LangSpan's block"),
        }
    );
    Ok(0)
}

/// How `user_install` runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Show the plan and how to apply it.
    DryRun,
    /// Show the plan only: `setup --verbose` says how to apply it.
    Plan,
    /// One line per file, no file's text: `setup` says how to apply it.
    Brief,
    Apply,
}

/// The hosts `install` and `setup` choose without --hosts: those whose
/// configuration directory exists. Antigravity is chosen only by name:
/// `~/.gemini` is also made by other tools, Gemini CLI among them.
fn found_hosts(home: &UserHome) -> Vec<crate::install::Vendor> {
    use crate::install::Vendor;
    [Vendor::ClaudeCode, Vendor::Codex, Vendor::Cursor]
        .into_iter()
        .filter(|vendor| home.files(*vendor).config_dir.is_dir())
        .collect()
}

fn no_host(home: &UserHome) -> crate::Error {
    format!(
        "No Claude Code, Codex or Cursor found in {}. Start one of them once, then run this again, or name a host with --hosts claude-code,codex,cursor,antigravity.",
        home.home.display()
    )
    .into()
}

/// A brief plan's line for one file.
fn would_be(path: &std::path::Path, exists: bool, removed: bool) -> String {
    let what = match (removed, exists) {
        (true, _) => "removed",
        (false, true) => "changed",
        (false, false) => "created",
    };
    format!("{} would be {what}.", path.display())
}

/// `install` / `uninstall`: LangSpan's user-level hooks and MCP server entry
/// for each chosen host, and on uninstall the coordinator's login service.
/// Every file is planned, and every refusal raised, before anything is
/// written. Returns how many files would change or changed.
pub fn user_install(
    install: bool,
    options: &mut BTreeMap<String, String>,
    mode: Mode,
) -> Result<usize, crate::Error> {
    use crate::{install as hooks, install::Vendor, user_install};
    let apply = mode == Mode::Apply;
    let home = UserHome::new(options.remove("--home").map(PathBuf::from))?;
    let state_dir = std::path::absolute(home.state_dir(options.remove("--state-dir")))?;
    let share_prompt = switch(options, "--prompt-summary")?;
    let presence = presence(options)?;
    let outline_reads = outline_reads(options)?;
    let hosts = options.remove("--hosts");
    let purge = options.remove("--purge").is_some();
    let leftover = user_install::Leftover::check(&state_dir, purge, install, hosts.is_none())?;
    let with_skill = options.remove("--skill").is_some();
    if with_skill && !install {
        return Err(
            "--skill is valid only for `install`; `uninstall` always removes LangSpan's skill"
                .into(),
        );
    }
    let status_line = options.remove("--statusline");
    if status_line.is_some() && !install {
        return Err(
            "--statusline is valid only for `install`; `uninstall` always removes LangSpan's status line"
                .into(),
        );
    }
    let hooked_tools = match options.remove("--mcp-tools").as_deref() {
        Some(_) if !install => {
            return Err("--mcp-tools is valid only for `install`; `uninstall` removes the whole MCP entry, flag included".into());
        }
        None | Some("all") => false,
        Some("hooked") => true,
        Some(other) => {
            return Err(format!("--mcp-tools takes all or hooked, not {other:?}").into());
        }
    };
    no_unused_options(options)?;
    let vendors: Vec<Vendor> = match hosts {
        Some(list) => list
            .split(',')
            .map(|name| {
                Vendor::from_name(name.trim()).ok_or_else(|| {
                    format!(
                        "unknown host {:?}; choose from claude-code, codex, cursor and antigravity",
                        name.trim()
                    )
                })
            })
            .collect::<Result<_, _>>()?,
        None => found_hosts(&home),
    };
    // Gemini CLI is named only to remove an earlier install.
    if install && vendors.iter().any(|vendor| vendor.removed()) {
        return Err(crate::install::GEMINI_REMOVED.into());
    }
    if vendors.is_empty() && install {
        return Err(no_host(&home));
    }
    if vendors.is_empty() {
        println!(
            "No host configuration found under {} (.claude, .codex or .cursor), so no host's files change.",
            home.home.display()
        );
    }
    let executable = hooks::installed_executable(&std::env::current_exe()?)?;
    // Claude Code's `statusLine` (roadmap H2): only where none is set, and
    // removed by `uninstall` only where it is LangSpan's.
    let status_line = match (install, status_line.as_deref()) {
        (false, _) => user_install::StatusLineChoice::Off,
        (true, None) => user_install::StatusLineChoice::Unchanged,
        (true, Some("on")) => user_install::StatusLineChoice::On {
            executable: executable.clone(),
            state_dir: state_dir.clone(),
        },
        (true, Some("off")) => user_install::StatusLineChoice::Off,
        (true, Some(other)) => {
            return Err(format!("--statusline takes on or off, not {other:?}").into());
        }
    };
    if status_line != user_install::StatusLineChoice::Unchanged
        && install
        && !vendors.contains(&Vendor::ClaudeCode)
    {
        return Err("--statusline is for Claude Code, the one host with a status-line command; add claude-code to --hosts".into());
    }
    let status_line = &status_line;
    let registration = install.then(|| hooks::UserRegistration {
        executable: executable.clone(),
        state_dir: state_dir.clone(),
        share_prompt,
        presence,
        outline_reads,
    });
    // Each host's entry: with `--mcp-tools hooked`, the Claude Code and Codex
    // entries list fewer tools while their hooks report (roadmap I11).
    let servers: Vec<(Vendor, user_install::McpServer)> = if install {
        vendors
            .iter()
            .map(|&vendor| {
                let hooked_host = (hooked_tools
                    && crate::mcp::HOOKED_HOSTS.contains(&vendor.name()))
                .then(|| vendor.name().to_owned());
                (
                    vendor,
                    user_install::McpServer {
                        executable: executable.clone(),
                        state_dir: state_dir.clone(),
                        hooked_host,
                    },
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    // A host may rewrite its file between this plan and the write.
    let mut plans: Vec<FilePlan> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if hooked_tools {
        let others: Vec<&str> = vendors
            .iter()
            .map(|vendor| vendor.name())
            .filter(|name| !crate::mcp::HOOKED_HOSTS.contains(name))
            .collect();
        if !others.is_empty() {
            notes.push(format!(
                "--mcp-tools hooked applies to claude-code and codex; {} keep every tool until their hooks are verified on a real host.",
                others.join(" and ")
            ));
        }
    }
    let registration = registration.as_ref();
    // On uninstall, a file LangSpan may have created is removed when nothing
    // but LangSpan's entries was in it, rather than left holding `{}`.
    let emptied = |vendor: Vendor| {
        (!install).then(|| -> Box<hooks::LeftEmpty> {
            Box::new(move |bytes: &[u8]| hooks::json_left_empty(vendor, bytes))
        })
    };
    for &vendor in &vendors {
        let server = servers
            .iter()
            .find(|(each, _)| *each == vendor)
            .map(|(_, server)| server);
        let files = home.files(vendor);
        let original = hooks::read_bytes(&files.hooks)?;
        let existing = hooks::parse_json(&files.hooks, original.as_deref())?;
        if vendor.removed() {
            // Only `uninstall` gets here (install refused it above). Gemini
            // CLI kept hooks and MCP servers in one settings file: one plan,
            // one guarded write.
            let plan_both = move |existing: Option<serde_json::Value>| {
                let hooked = hooks::plan_user_for(vendor, existing, registration)?;
                user_install::plan_gemini(hooked, server)
            };
            let planned = plan_both(existing.clone())
                .map_err(|e| format!("{}: {e}", files.hooks.display()))?;
            let shown = gemini_shown(&planned, true)?;
            let path = files.hooks.clone();
            let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
                let existing = hooks::parse_json(&path, now)?;
                if existing.is_none() && registration.is_none() {
                    return Err(format!(
                        "{} was removed meanwhile; nothing to remove",
                        path.display()
                    )
                    .into());
                }
                let planned =
                    plan_both(existing).map_err(|e| format!("{}: {e}", path.display()))?;
                hooks::json_bytes(&planned)
            };
            plans.push((
                files.hooks.clone(),
                Planned::Json(existing, planned, Some(shown)),
                original,
                Box::new(replan),
                emptied(vendor),
            ));
            continue;
        }
        // Claude Code's status line lives in the same settings file as its
        // hooks: one plan, one guarded write.
        let plan_file = move |existing: Option<serde_json::Value>| {
            let planned = hooks::plan_user_for(vendor, existing, registration)?;
            if vendor == Vendor::ClaudeCode {
                user_install::plan_status_line(planned, status_line)
            } else {
                Ok((planned, None))
            }
        };
        let (planned, note) =
            plan_file(existing.clone()).map_err(|e| format!("{}: {e}", files.hooks.display()))?;
        notes.extend(note);
        let path = files.hooks.clone();
        let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
            let existing = hooks::parse_json(&path, now)?;
            if existing.is_none() && registration.is_none() {
                return Err(format!(
                    "{} was removed meanwhile; nothing to remove",
                    path.display()
                )
                .into());
            }
            let (planned, _) =
                plan_file(existing).map_err(|e| format!("{}: {e}", path.display()))?;
            hooks::json_bytes(&planned)
        };
        plans.push((
            files.hooks.clone(),
            Planned::Json(existing, planned, None),
            original,
            Box::new(replan),
            emptied(vendor),
        ));
        let original = hooks::read_bytes(&files.mcp)?;
        match vendor {
            Vendor::ClaudeCode => match hooks::parse_json(&files.mcp, original.as_deref())? {
                // Claude Code creates this file itself and keeps its account
                // and project state in it; LangSpan never creates it.
                None if install => notes.push(format!(
                    "{} does not exist yet, so the MCP server was not registered. Start Claude Code once, then run this again.",
                    files.mcp.display()
                )),
                None => {}
                Some(existing) => {
                    let planned = user_install::plan_claude(existing.clone(), server)
                        .map_err(|e| format!("{}: {e}", files.mcp.display()))?;
                    // The rest of this file is the host's account and project
                    // state: a dry run shows only the entry that changes.
                    let shown = match planned
                        .get("mcpServers")
                        .and_then(|servers| servers.get(user_install::SERVER_NAME))
                    {
                        Some(entry) => format!(
                            "  \"mcpServers\": {{ \"{}\": {} }}\n(only this entry changes; the rest of the file is kept as it is and not shown)",
                            user_install::SERVER_NAME,
                            serde_json::to_string(entry)?
                        ),
                        None => format!(
                            "  without \"mcpServers\".\"{}\"\n(only this entry changes; the rest of the file is kept as it is and not shown)",
                            user_install::SERVER_NAME
                        ),
                    };
                    let path = files.mcp.clone();
                    let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
                        let existing = hooks::parse_json(&path, now)?.ok_or_else(|| {
                            format!("{} was removed while LangSpan was writing it; nothing was written", path.display())
                        })?;
                        let planned = user_install::plan_claude(existing, server)
                            .map_err(|e| format!("{}: {e}", path.display()))?;
                        hooks::json_bytes(&planned)
                    };
                    plans.push((
                        files.mcp.clone(),
                        Planned::Json(Some(existing), planned, Some(shown)),
                        original,
                        Box::new(replan),
                        // Claude Code's own file: never removed.
                        None,
                    ));
                }
            },
            // Cursor's mcp.json and Antigravity's mcp_config.json hold only
            // MCP servers; LangSpan may create them.
            Vendor::Cursor | Vendor::Antigravity => {
                let plan_mcp = move |existing: Option<serde_json::Value>| {
                    if vendor == Vendor::Antigravity {
                        user_install::plan_antigravity(existing, server)
                    } else {
                        user_install::plan_claude(
                            existing.unwrap_or_else(|| serde_json::json!({})),
                            server,
                        )
                    }
                };
                let existing = hooks::parse_mcp_json(vendor, &files.mcp, original.as_deref())?;
                let planned = plan_mcp(existing.clone())
                    .map_err(|e| format!("{}: {e}", files.mcp.display()))?;
                let path = files.mcp.clone();
                let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
                    let existing = hooks::parse_mcp_json(vendor, &path, now)?;
                    let planned =
                        plan_mcp(existing).map_err(|e| format!("{}: {e}", path.display()))?;
                    hooks::json_bytes(&planned)
                };
                plans.push((
                    files.mcp.clone(),
                    Planned::Json(existing, planned, None),
                    original,
                    Box::new(replan),
                    // `{"version": 1}` is not Cursor's MCP shape: only `{}`.
                    emptied(Vendor::ClaudeCode),
                ));
                if install && vendor == Vendor::Cursor {
                    notes.push("Cursor: built from documentation, not yet verified on a real Cursor (ADR 0032). Cursor asks before it first uses an MCP server; approve `langspan` there.".into());
                }
                if install && vendor == Vendor::Antigravity {
                    notes.push(format!(
                        "Antigravity: {} Its App, CLI and IDE all read these files.",
                        crate::antigravity::VERIFIED
                    ));
                }
            }
            Vendor::Codex => {
                let existing = hooks::parse_text(&files.mcp, original.as_deref())?;
                let planned = user_install::plan_codex(existing.as_deref(), server)
                    .map_err(|e| format!("{}: {e}", files.mcp.display()))?;
                let path = files.mcp.clone();
                let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, crate::Error> {
                    let existing = hooks::parse_text(&path, now)?;
                    Ok(user_install::plan_codex(existing.as_deref(), server)
                        .map_err(|e| format!("{}: {e}", path.display()))?
                        .into_bytes())
                };
                plans.push((
                    files.mcp.clone(),
                    Planned::Text(existing, planned),
                    original,
                    Box::new(replan),
                    (!install).then(|| -> Box<hooks::LeftEmpty> {
                        Box::new(|bytes: &[u8]| bytes.is_empty())
                    }),
                ));
                if install {
                    notes.push("Codex runs a new or changed hook only after you review and trust it with /hooks.".into());
                }
            }
            // Planned with its hooks, above.
            Vendor::Gemini => {}
        }
    }
    // The skill: written only with --skill, removed by uninstall wherever
    // LangSpan's is found. Someone else's file of the same name stops an
    // install here, before anything is written.
    let mut skills: Vec<crate::skill::FilePlan> = Vec::new();
    for &vendor in &vendors {
        // `uninstall --hosts gemini-cli` takes out the section an earlier
        // `install --skill` put in ~/.gemini/GEMINI.md, which Antigravity
        // reads too.
        if vendor.removed() {
            let path = crate::skill::path(vendor, &home.files(vendor).config_dir);
            skills.push(crate::skill::plan_file(vendor, &path, false)?);
            continue;
        }
        if !crate::skill::USER_HOSTS.contains(&vendor) {
            if with_skill {
                notes.push("Cursor reads the LangSpan skill as a project rule: run `langspan hooks install cursor --workspace ID --root DIR --skill --apply` in each project.".into());
            }
            continue;
        }
        if install && !with_skill {
            continue;
        }
        let path = crate::skill::path(vendor, &home.files(vendor).config_dir);
        skills.push(crate::skill::plan_file(vendor, &path, install)?);
    }
    let mut changed = 0;
    for (path, plan, original, replan, empty) in &plans {
        let planned_bytes = match plan {
            Planned::Json(_, planned, _) => hooks::json_bytes(planned)?,
            Planned::Text(_, planned) => planned.clone().into_bytes(),
        };
        // A planned removal of a file that is there is a change even when
        // its content would not change, as with the `{}` an earlier
        // uninstall left; an absent file stays unchanged.
        let removed =
            original.is_some() && empty.as_ref().is_some_and(|empty| empty(&planned_bytes));
        let (unchanged, text) = match plan {
            Planned::Json(existing, planned, shown) => (
                existing.as_ref() == Some(planned) || (existing.is_none() && !install),
                match shown {
                    Some(shown) => shown.clone(),
                    None => serde_json::to_string_pretty(planned)?,
                },
            ),
            Planned::Text(existing, planned) => (
                existing.as_deref() == Some(planned.as_str()) || (existing.is_none() && !install),
                planned.trim_end().to_owned(),
            ),
        };
        if unchanged && !removed {
            println!("{} already matches; nothing to change.", path.display());
            continue;
        }
        changed += 1;
        if apply {
            // The plan is made again from the bytes it was shown from, and
            // written (or removed) only if the file still holds them.
            let never = |_: &[u8]| false;
            let empty = empty.as_deref().unwrap_or(&never);
            match hooks::write_or_remove_planned(path, original.as_deref(), &**replan, empty)? {
                hooks::Applied::Written => println!("Wrote {}.", path.display()),
                hooks::Applied::Removed => println!("Removed {}.", path.display()),
                hooks::Applied::Unchanged => {
                    println!("{} already matches; nothing to change.", path.display())
                }
            }
        } else if removed || mode == Mode::Brief {
            println!("{}", would_be(path, original.is_some(), removed));
        } else {
            println!("{} would become:", path.display());
            println!("{text}");
        }
    }
    // Install shows the skill once; uninstall shows each file's own remainder.
    let mut shown: Option<&std::path::Path> = None;
    for plan in &skills {
        if plan.unchanged() {
            // Uninstall says nothing about a skill that is not there.
            if install {
                println!(
                    "{} already matches; nothing to change.",
                    plan.path.display()
                );
            }
            continue;
        }
        changed += 1;
        if mode == Mode::Brief {
            println!(
                "{}",
                would_be(&plan.path, plan.original.is_some(), plan.planned.is_none())
            );
        } else if !apply {
            match shown {
                Some(first) if plan.planned.is_some() => println!(
                    "{} would get the same skill text as {}.",
                    plan.path.display(),
                    first.display()
                ),
                _ => {
                    println!("{}", plan.shown());
                    if install && plan.planned.is_some() {
                        shown = Some(&plan.path);
                    }
                }
            }
        } else if plan.apply()? {
            match plan.planned {
                Some(_) => println!("Wrote {}.", plan.path.display()),
                None => println!("Removed {}.", plan.path.display()),
            }
            // Claude Code's `skills` folder, when the skill was all it held.
            if plan.planned.is_none() && plan.vendor == Vendor::ClaudeCode {
                let _ = plan.path.ancestors().nth(2).map(std::fs::remove_dir);
            }
        } else {
            println!(
                "{} already matches; nothing to change.",
                plan.path.display()
            );
        }
    }
    if !install {
        changed += remove_service(&home, apply)?;
        changed += stop_coordinator(&state_dir, apply)?;
    }
    if let Some(leftover) = &leftover {
        let (removed, lines) = leftover.finish(&executable, apply)?;
        changed += removed;
        lines.iter().for_each(|line| println!("{line}"));
    }
    if changed > 0 && mode == Mode::DryRun {
        println!("Dry run; nothing was written. Run the same command with --apply to write it.");
    }
    for note in notes {
        println!("{note}");
    }
    Ok(changed)
}

/// Starts a coordinator for `state_dir` if none runs and no login service is
/// installed for it, waiting at most until `until`. Never fails: what
/// happened goes to stderr as one structured line.
pub fn start_coordinator(state_dir: &std::path::Path, until: std::time::Instant) {
    if crate::autostart::disabled() {
        return;
    }
    let Ok(state_dir) = std::path::absolute(state_dir) else {
        return;
    };
    // The usual case, before anything else is looked up.
    if crate::client::accepts(
        &state_dir,
        until.min(std::time::Instant::now() + std::time::Duration::from_millis(100)),
    ) {
        return;
    }
    let executable = std::env::current_exe()
        .map_err(crate::Error::from)
        .and_then(|exe| Ok(crate::install::installed_executable(&exe)?));
    let outcome = match executable {
        Ok(executable) => {
            let service = UserHome::new(None)
                .is_ok_and(|home| crate::service::installed_for(&home, &state_dir));
            crate::autostart::ensure(&state_dir, &executable, None, service, until)
        }
        Err(error) => {
            crate::autostart::Outcome::Failed(format!("cannot find this executable: {error}"))
        }
    };
    if let Some(line) = outcome.log_line(&state_dir) {
        eprintln!("{line}");
    }
}

/// The login service `setup` would install, as planned.
enum ServicePlan {
    /// No service: why, and so the coordinator is started on demand.
    None(String),
    /// The file and the manager already match.
    Unchanged(PathBuf),
    /// The file to write, its text, and what is there now.
    Install {
        manager: crate::service::Manager,
        path: PathBuf,
        text: String,
        existing: Option<String>,
    },
}

fn plan_service(
    home: &UserHome,
    executable: &std::path::Path,
    state_dir: &std::path::Path,
) -> Result<ServicePlan, crate::Error> {
    use crate::service::{self, Manager};
    let Some(manager) = Manager::current() else {
        return Ok(ServicePlan::None(
            "this platform has no supported login service".into(),
        ));
    };
    if home.explicit() {
        return Ok(ServicePlan::None(
            "--home names a home other than the login user's, whose service manager setup does not change".into(),
        ));
    }
    let path = manager.file(home);
    let existing = service::read(&path)?;
    if let Some(existing) = &existing
        && manager.state_dir_of(existing).is_none()
    {
        return Err(format!(
            "{} exists and LangSpan did not write it; move it aside, then run setup again",
            path.display()
        )
        .into());
    }
    if let Err(reason) = service::manager_available(manager) {
        return Ok(ServicePlan::None(reason));
    }
    let text = manager.render(executable, state_dir, &|name| std::env::var(name).ok())?;
    if existing.as_deref() == Some(text.as_str()) && service::running(manager) {
        return Ok(ServicePlan::Unchanged(path));
    }
    Ok(ServicePlan::Install {
        manager,
        path,
        text,
        existing,
    })
}

/// `setup`: one plan for the user-level install with the skill, the
/// coordinator's login service and the state directory; one question; then
/// the install, the service, a running coordinator and `doctor`.
pub fn setup(options: &mut BTreeMap<String, String>, json: bool) -> Result<u8, crate::Error> {
    use std::io::{BufRead, IsTerminal, Read, Write};
    if json {
        return Err(
            "--json is not valid for `setup`; run `langspan doctor --json` after it".into(),
        );
    }
    let yes = options.remove("--yes").is_some();
    let verbose = options.remove("--verbose").is_some();
    if options.contains_key("--skill") {
        return Err("`setup` always installs the skill; leave out --skill".into());
    }
    options.insert("--skill".into(), String::new());
    let home = UserHome::new(options.get("--home").map(PathBuf::from))?;
    // Said before any plan is printed (issue #291).
    if !options.contains_key("--hosts") && found_hosts(&home).is_empty() {
        return Err(no_host(&home));
    }
    let state_dir = std::path::absolute(home.state_dir(options.get("--state-dir").cloned()))?;
    crate::autostart::check_state_dir(&state_dir)?;
    let executable = crate::install::installed_executable(&std::env::current_exe()?)?;
    println!("LangSpan setup for {}", home.home.display());
    println!("State directory: {}", state_dir.display());
    println!("Hook and MCP files:");
    let mut plan_options = options.clone();
    let mode = if verbose { Mode::Plan } else { Mode::Brief };
    let files = user_install(true, &mut plan_options, mode)?;
    let service = plan_service(&home, &executable, &state_dir)?;
    println!("Coordinator:");
    match &service {
        ServicePlan::None(reason) => println!(
            "  No login service: {reason}. Hooks and `langspan mcp` start the coordinator when they find none, and setup starts one now."
        ),
        ServicePlan::Unchanged(path) => {
            println!(
                "  Login service {} is installed and running; nothing to change.",
                path.display()
            )
        }
        ServicePlan::Install {
            path,
            text,
            existing,
            ..
        } => {
            if existing.as_deref() == Some(text.as_str()) {
                println!(
                    "  Start login service {}, which is installed but not running.",
                    path.display()
                );
            } else {
                println!(
                    "  {} {} and start it, so the coordinator runs at every login{}",
                    if existing.is_some() {
                        "Replace"
                    } else {
                        "Write"
                    },
                    path.display(),
                    if verbose { ":" } else { "." }
                );
                if verbose {
                    for line in text.lines() {
                        println!("    {line}");
                    }
                }
            }
        }
    }
    let service_changes = matches!(service, ServicePlan::Install { .. });
    let running = crate::client::accepts(
        &state_dir,
        std::time::Instant::now() + std::time::Duration::from_millis(500),
    );
    if files == 0 && !service_changes && running {
        println!("Already set up; nothing to change.");
        println!("{TRY_IT}");
        return Ok(0);
    }
    if !verbose {
        println!("Run `langspan setup --verbose` to see each file's full text first.");
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            println!(
                "Not run from a terminal and no --yes, so nothing was changed. Run `langspan setup --yes` to apply this plan."
            );
            return Ok(0);
        }
        eprint!("Set this up? [Y/n] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin().lock().take(256).read_line(&mut answer)?;
        if !matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "" | "y" | "yes"
        ) {
            println!("Nothing was changed.");
            return Ok(0);
        }
    }
    if files > 0 {
        user_install(true, options, Mode::Apply)?;
    }
    crate::autostart::prepare_state_dir(&state_dir)?;
    match service {
        ServicePlan::Install {
            manager,
            path,
            text,
            existing,
        } => {
            install_service(manager, &path, existing.as_deref(), &text, &state_dir)?;
        }
        ServicePlan::Unchanged(_) => {}
        ServicePlan::None(_) => {
            let outcome = crate::autostart::ensure(
                &state_dir,
                &executable,
                home.explicit().then_some(home.home.as_path()),
                false,
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            );
            match outcome {
                crate::autostart::Outcome::Running => {
                    println!("A coordinator is already running.")
                }
                crate::autostart::Outcome::Started { pid, ready: true } => println!(
                    "Started a coordinator in the background (process {pid}); it logs to {}.",
                    crate::autostart::log_path(&state_dir).display()
                ),
                crate::autostart::Outcome::Started { pid, ready: false } => println!(
                    "Started a coordinator (process {pid}), but it did not accept connections within 5 seconds; read {} for why.",
                    crate::autostart::log_path(&state_dir).display()
                ),
                crate::autostart::Outcome::Skipped(reason) => {
                    println!("No coordinator was started ({reason}).")
                }
                crate::autostart::Outcome::Failed(error) => {
                    println!("No coordinator was started: {error}.")
                }
            }
        }
    }
    println!("langspan doctor:");
    // Setup itself succeeded; what doctor finds is reported, not its exit.
    doctor(&state_dir, None, &home, false)?;
    println!("{TRY_IT}");
    Ok(0)
}

/// Setup's last line: the next step, which needs no agent (issue #290).
const TRY_IT: &str = "Try it: langspan demo";

/// Writes the login service file, unless it changed from `planned` since
/// the plan was shown, and starts it, then waits up to five seconds for the
/// coordinator to accept connections.
fn install_service(
    manager: crate::service::Manager,
    path: &std::path::Path,
    planned: Option<&str>,
    text: &str,
    state_dir: &std::path::Path,
) -> Result<(), crate::Error> {
    use crate::service;
    // The manager creates the log as it starts the service: owner-only first.
    drop(crate::autostart::open_log(&std::fs::canonicalize(
        state_dir,
    )?)?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::service::write(path, planned, text)?;
    println!("Wrote {}.", path.display());
    let commands = service::start_commands(manager, path);
    let last = commands.len() - 1;
    for (index, (program, args)) in commands.iter().enumerate() {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let ran = service::run(program, &args)?;
        // launchd's first step unloads an earlier load, and fails when there
        // was none.
        let optional = manager == service::Manager::Launchd && index < last;
        if !ran.success && !optional {
            return Err(format!(
                "`{program} {}` failed: {}. {} is written; run that command again, or `langspan daemon` to start the coordinator by hand",
                args.join(" "),
                ran.said,
                path.display()
            )
            .into());
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        if crate::client::accepts(state_dir, deadline) {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if ready {
        println!("Started the login service; the coordinator accepts connections.");
    } else {
        println!(
            "Started the login service, but the coordinator did not accept connections within 5 seconds; see {}.",
            crate::autostart::log_path(state_dir).display()
        );
    }
    Ok(())
}

/// On uninstall, stops and removes the login service LangSpan wrote, if
/// any. Returns 1 when there is one to remove, else 0.
fn remove_service(home: &UserHome, apply: bool) -> Result<usize, crate::Error> {
    use crate::service::{self, Manager};
    let Some(manager) = Manager::current() else {
        return Ok(0);
    };
    let path = manager.file(home);
    match service::read(&path)? {
        None => return Ok(0),
        Some(text) if manager.state_dir_of(&text).is_none() => {
            println!(
                "{} was not written by LangSpan; it was left in place.",
                path.display()
            );
            return Ok(0);
        }
        Some(_) => {}
    }
    if !apply {
        println!(
            "The coordinator's login service {} would be stopped and removed.",
            path.display()
        );
        return Ok(1);
    }
    // Another home's manager is not this login's: only the file goes.
    if !home.explicit() {
        for (program, args) in service::stop_commands(manager) {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match service::run(&program, &args) {
                Ok(ran) if ran.success => {}
                Ok(ran) => println!("`{program} {}` said: {}", args.join(" "), ran.said),
                Err(error) => println!("{error}"),
            }
        }
    }
    std::fs::remove_file(&path)?;
    if !home.explicit() && manager == Manager::Systemd {
        let _ = service::run("systemctl", &["--user", "daemon-reload"]);
    }
    println!(
        "Stopped and removed the coordinator's login service {}.",
        path.display()
    );
    Ok(1)
}

/// On uninstall, stops the coordinator for `state_dir`, however it was
/// started. Returns 1 when one runs, else 0.
fn stop_coordinator(state_dir: &std::path::Path, apply: bool) -> Result<usize, crate::Error> {
    use std::time::{Duration, Instant};
    if !apply {
        let runs = crate::autostart::running(state_dir)?;
        if runs {
            println!("The coordinator would be stopped.");
        }
        return Ok(usize::from(runs));
    }
    let stopped = crate::autostart::stop(state_dir, Instant::now() + Duration::from_secs(5))?;
    if stopped {
        println!("Stopped the coordinator.");
    }
    Ok(usize::from(stopped))
}

/// What a dry run shows of Gemini CLI's settings file: LangSpan's entries
/// only. The rest of the file can hold the owner's MCP server credentials
/// and is neither shown nor changed.
fn gemini_shown(planned: &serde_json::Value, user: bool) -> Result<String, crate::Error> {
    use crate::{install, user_install};
    let ours = install::managed_hooks(planned, |hook| {
        if user {
            install::is_user_managed_for(install::Vendor::Gemini, hook)
        } else {
            install::is_managed_for(install::Vendor::Gemini, hook)
        }
    });
    let hooks = match ours.first() {
        Some(hook) => format!(
            "  LangSpan's hook on {} event(s): {}",
            ours.len(),
            serde_json::to_string(hook)?
        ),
        None => "  without LangSpan's hooks".into(),
    };
    let server = match planned
        .get("mcpServers")
        .and_then(|servers| servers.get(user_install::SERVER_NAME))
    {
        Some(entry) if user_install::parse_claude_entry(entry).is_some() => format!(
            "  \"mcpServers\": {{ \"{}\": {} }}",
            user_install::SERVER_NAME,
            serde_json::to_string(entry)?
        ),
        // Not LangSpan's: kept as it is, and its contents are not shown.
        Some(_) => format!(
            "  \"mcpServers\".\"{}\" kept as it is: LangSpan did not write it",
            user_install::SERVER_NAME
        ),
        None => format!("  without \"mcpServers\".\"{}\"", user_install::SERVER_NAME),
    };
    Ok(if user {
        format!(
            "{hooks}\n{server}\n(only these entries change; the rest of the file is kept as it is and not shown)"
        )
    } else {
        format!(
            "{hooks}\n(only these entries change; the rest of the file is kept as it is and not shown)"
        )
    })
}
