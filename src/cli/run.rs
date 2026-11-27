//! `langspan COMMAND`: which driver each command line goes to.

use crate::check::{auto_check, evidence, flaky_tests};
use crate::flags::{ParsedOptions, flags_of, parse_options, state_dir};
use crate::help::HELP;
use crate::land::{integrate, landing_queue};
use crate::local::{adopt, commit_mine, demo, notify, refusal_count, status_line, status_mine};
use crate::request::request;
use crate::serve::{daemon, dashboard, mcp};
use langspan::cli::doctor::doctor;
use langspan::cli::options::no_unused_options;
use langspan::cli::setup::{Mode, guidance, hooks, hosts_report, setup, user_install};
use langspan::install::detect::UserHome;
use std::path::PathBuf;

#[cfg(test)]
#[path = "run/option_matrix.rs"]
mod option_matrix;
#[cfg(test)]
#[path = "run/option_table.rs"]
mod option_table;

pub(crate) async fn run(input: Vec<String>) -> Result<u8, langspan::Error> {
    if input.is_empty() || input.iter().any(|v| v == "--help" || v == "-h") {
        print!("{HELP}");
        return Ok(0);
    }
    if input == ["--version"] {
        println!("langspan {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    let ParsedOptions {
        args,
        command,
        mut options,
        symbols,
        tags,
        json,
        apply,
    } = parse_options(input)?;
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let before_command = &words[..words.len() - command.len()];
    let flags = flags_of(&words, before_command);
    flags.symbol.check("--symbol", symbols.len())?;
    flags.tag.check("--tag", tags.len())?;
    if let [action] = args.as_slice()
        && (action == "install" || action == "uninstall")
    {
        user_install(
            action == "install",
            &mut options,
            if apply { Mode::Apply } else { Mode::DryRun },
        )?;
        return Ok(0);
    }
    if args == ["setup"] {
        if apply {
            return Err(
                "`setup` asks before it changes anything; use --yes to skip the question".into(),
            );
        }
        return setup(&mut options, json);
    }
    if args == ["demo"] {
        return demo(&options, json);
    }
    if args.first().map(String::as_str) == Some("guidance") && command.is_empty() {
        return guidance(&args[1..], &mut options, json, apply);
    }
    let home_option = options.remove("--home").map(PathBuf::from);
    let state_option = options.get("--state-dir").cloned();
    let state_dir = state_dir(&mut options)?;
    match before_command {
        ["doctor"] => {
            let root = options.remove("--root").map(PathBuf::from);
            no_unused_options(&options)?;
            let home = UserHome::new(home_option)?;
            return doctor(&state_dir, root.as_deref(), &home, json);
        }
        ["hosts"] => {
            no_unused_options(&options)?;
            let home = UserHome::new(home_option)?;
            // The state directory `install` would use in the same home.
            let state_dir = std::path::absolute(home.state_dir(state_option))?;
            return hosts_report(&home, &state_dir, json);
        }
        ["check", "auto", workspace] => {
            return auto_check(&state_dir, workspace, &command, &mut options, json);
        }
        ["check", "flaky", workspace] if command.is_empty() => {
            return flaky_tests(&state_dir, workspace, &mut options, json);
        }
        ["check", "run", workspace] | ["check", "status", workspace] => {
            return evidence(
                &state_dir,
                workspace,
                before_command[1] == "run",
                &command,
                &mut options,
                json,
            );
        }
        ["land-queue", workspace] | ["land-cancel", workspace] if command.is_empty() => {
            return landing_queue(
                &state_dir,
                workspace,
                before_command[0] == "land-cancel",
                &mut options,
                json,
            );
        }
        ["statusline"] => return status_line(&state_dir, &mut options, json),
        ["count"] => return refusal_count(&state_dir, &mut options, json),
        ["notify", action] if command.is_empty() => {
            return notify(&state_dir, action, &mut options, json);
        }
        ["commit"] => return commit_mine(&state_dir, &mut options, json),
        // `adopt SESSION` or `adopt -- SESSION`, for an ID that begins with
        // a hyphen.
        ["adopt"] | ["adopt", _] if words.len() == 2 => {
            return adopt(&state_dir, words[1], &mut options, json);
        }
        ["status"] if options.remove("--mine").is_some() => {
            return status_mine(&state_dir, &mut options, json);
        }
        ["integrate", "check", workspace, branches @ ..] | ["land", workspace, branches @ ..] => {
            return integrate(
                &state_dir,
                workspace,
                branches,
                (before_command[0] == "land").then_some(apply),
                &command,
                &mut options,
                json,
            );
        }
        _ => {}
    }
    if home_option.is_some() {
        return Err("--home is valid only for `install`, `uninstall`, `doctor` and `hosts`".into());
    }
    if let ["hooks", action @ ("install" | "remove"), vendor] = words.as_slice()
        && let Some(vendor) = langspan::install::Vendor::from_name(vendor)
    {
        return hooks(vendor, *action == "install", state_dir, &mut options, apply);
    }
    if apply && !flags.apply {
        return Err(
            "--apply is valid only for `install`, `uninstall`, `hooks install|remove`, `land`, `compact`, `workspace remove` and `graph import`"
                .into(),
        );
    }
    if words == ["dashboard"] {
        return dashboard(state_dir, options, json).await;
    }
    if words == ["daemon"] {
        return daemon(state_dir, &options).await;
    }
    if words == ["mcp"] {
        return mcp(state_dir, options).await;
    }
    request(state_dir, words, options, symbols, tags, apply, json)
}
