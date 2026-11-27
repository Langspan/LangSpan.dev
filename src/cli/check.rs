//! `langspan check run|status|flaky|auto`: the drivers for recorded checks.

use crate::flags::number;
use langspan::cli::ask;
use langspan::cli::options::no_unused_options;
use langspan::cli::render::print_human;
use langspan::protocol::{PROTOCOL_VERSION, Response};
use langspan_core::{Command, Outcome};
use std::collections::BTreeMap;

pub(crate) fn check_name(
    options: &mut BTreeMap<String, String>,
) -> Result<String, langspan::Error> {
    Ok(options
        .remove("--name")
        .ok_or("--name NAME is required: it is how this check's evidence is found again")?)
}

pub(crate) fn unique(kind: &str, fields: &[&str]) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_string();
    let mut all = fields.to_vec();
    all.push(&now);
    langspan::hook::wire_id(kind, &all)
}

/// `check run`: bind one command's result to the exact working tree it ran
/// on. `check status`: say which records apply to the tree as it is now.
pub(crate) fn evidence(
    state_dir: &std::path::Path,
    workspace: &str,
    running: bool,
    command: &[String],
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    use langspan::integrate;
    use langspan_core::{Applicability, CheckResult};
    use std::time::{Duration, Instant};
    let root = langspan::landing::workspace_root(state_dir, workspace)?;
    let snapshot = |root: &std::path::Path| {
        integrate::working_tree(root, Instant::now() + Duration::from_secs(120))
    };
    if !running {
        no_unused_options(options)?;
        let (_, tree) = snapshot(&root)?;
        let outcome = ask(
            state_dir,
            "read".into(),
            Command::CheckStatus {
                workspace_id: workspace.into(),
                tree_oid: Some(tree.clone()),
            },
        )?;
        if json {
            println!("{}", serde_json::to_string(&outcome)?);
            return Ok(0);
        }
        let Outcome::Checks { checks } = outcome else {
            return Err("unexpected reply to check status".into());
        };
        println!("Working tree {tree}.");
        if checks.is_empty() {
            println!("No check has been recorded for this workspace.");
        }
        for view in checks {
            let record = view.record;
            println!(
                "{} {:?}: {} on tree {}{}. {}",
                record.name,
                record.command,
                match record.result {
                    CheckResult::Passed {} => "passed".to_owned(),
                    CheckResult::Failed { exit_code } => format!("failed (exit {exit_code})"),
                    CheckResult::Inconclusive {} => "inconclusive".to_owned(),
                },
                record.tree_oid,
                if record.inputs.is_empty() {
                    String::new()
                } else {
                    format!(" (combination of {} commits)", record.inputs.len())
                },
                match view.applicability {
                    Applicability::Current => "Applies to the working tree as it is now.",
                    _ => "Does NOT apply to the working tree as it is now: the tree differs.",
                }
            );
        }
        return Ok(0);
    }
    let name = check_name(options)?;
    let limit = Duration::from_secs(number(options, "--timeout-s", 1800)?);
    no_unused_options(options)?;
    if command.is_empty() {
        return Err("give the check command after --, for example: -- cargo test".into());
    }
    // The workspace's write generation, taken before the tree: a write
    // recorded while the command runs, even one undone before it ends,
    // keeps its test results from being compared. Unreadable, nothing is
    // learned from them.
    let writes_before = langspan::client::status_from(state_dir, &[])
        .ok()
        .map(|(state, _)| state.writes.get(workspace).copied().unwrap_or(0));
    let (head, before) = snapshot(&root)?;
    let (ran, duration_ms, output) = integrate::run_command_reading(&root, command, limit)?;
    let (_, after) = snapshot(&root)?;
    // A result is about a tree only if the tree held still while it ran.
    let result = match ran {
        _ if before != after => CheckResult::Inconclusive {},
        integrate::Ran::Exited(0) => CheckResult::Passed {},
        integrate::Ran::Exited(exit_code) => CheckResult::Failed { exit_code },
        integrate::Ran::Unfinished => CheckResult::Inconclusive {},
    };
    // The tests its output named, to learn which flip on unchanged code;
    // none for a command that looks like it holds a secret, whose identity
    // (a digest of it) is not sent.
    let tests = match (&result, &output) {
        (CheckResult::Inconclusive {}, _) | (_, None) => None,
        _ if writes_before.is_none() => None,
        _ if langspan::hook::contains_secret(&command.join(" ")) => None,
        (result, Some(bytes)) => {
            langspan::test_output::read(bytes, false, matches!(result, CheckResult::Passed {}), &[])
        }
    };
    let outcome = ask(
        state_dir,
        unique("check", &[workspace, &name, &before]),
        Command::RecordCheck {
            workspace_id: workspace.into(),
            name: name.clone(),
            command: langspan::landing::command_line(command),
            tree_oid: before.clone(),
            head_oid: head,
            inputs: Vec::new(),
            result: result.clone(),
            duration_ms,
            report: None,
            writes_before: tests.as_ref().and(writes_before),
            command_id: tests
                .as_ref()
                .map(|_| langspan::landing::command_id(command)),
            tests,
            rerun: None,
            symbol_findings: None,
        },
    )?;
    if json {
        println!("{}", serde_json::to_string(&outcome)?);
    } else {
        match &result {
            CheckResult::Passed {} => println!("{name} passed on tree {before}."),
            CheckResult::Failed { exit_code } => {
                println!("{name} failed (exit {exit_code}) on tree {before}.")
            }
            CheckResult::Inconclusive {} if before != after => println!(
                "{name} is inconclusive: files changed while it ran, so its result is not about any one tree. Run it again."
            ),
            CheckResult::Inconclusive {} => {
                println!("{name} is inconclusive: the command did not finish.")
            }
        }
    }
    Ok(match result {
        CheckResult::Passed {} => 0,
        CheckResult::Failed { .. } => 1,
        CheckResult::Inconclusive {} => 5,
    })
}

/// `check flaky`: the tests known to flip on unchanged code, or, with
/// `--forget`, clear what was learned about one of them.
pub(crate) fn flaky_tests(
    state_dir: &std::path::Path,
    workspace: &str,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    let name = options.remove("--name");
    let (request_id, command) = match options.remove("--forget") {
        Some(test) => (
            options
                .remove("--request-id")
                .ok_or("--forget requires --request-id ID; reuse it only for an identical retry")?,
            Command::ForgetFlakyTest {
                workspace_id: workspace.into(),
                name,
                test,
            },
        ),
        None => (
            "read".to_owned(),
            Command::FlakyTests {
                workspace_id: workspace.into(),
                name,
            },
        ),
    };
    no_unused_options(options)?;
    let outcome = ask(state_dir, request_id, command)?;
    if json {
        println!("{}", serde_json::to_string(&outcome)?);
        return Ok(0);
    }
    match outcome {
        Outcome::FlakyTests { tests, more, .. } => {
            if tests.is_empty() {
                println!("No test is known to flip on unchanged code in {workspace}.");
            }
            let ago = |ms: u64| match ms / 1000 {
                seconds @ 0..=89 => format!("{seconds}s"),
                seconds @ 90..=5399 => format!("{}m", seconds / 60),
                seconds @ 5400..=172_799 => format!("{}h", seconds / 3600),
                seconds => format!("{}d", seconds / 86_400),
            };
            for test in &tests {
                // Test names come from the check command's output.
                println!(
                    "{}  {:?}  runs {}  flips {}  last flip {} ago",
                    test.check_name,
                    test.test,
                    test.runs,
                    test.flips,
                    ago(test.last_flip_age_ms),
                );
            }
            if more {
                println!("(More are known; use --name NAME to list one check's, or --json.)");
            }
        }
        Outcome::FlakyTestForgotten {
            test, forgotten, ..
        } => {
            if forgotten {
                println!("Forgot what was learned about {test}; its next failure is not labelled.");
            } else {
                println!("Nothing was learned about {test}; nothing changed.");
            }
        }
        other => return Err(format!("unexpected reply: {other:?}").into()),
    }
    Ok(0)
}

/// `check auto`: turn the automatic check on, off, or show it.
pub(crate) fn auto_check(
    state_dir: &std::path::Path,
    workspace: &str,
    command: &[String],
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    let off = options.remove("--off").is_some();
    let outcome = if off {
        no_unused_options(options)?;
        if !command.is_empty() {
            return Err("--off takes no command".into());
        }
        ask(
            state_dir,
            unique("auto-check-off", &[workspace]),
            Command::AutoCheckDisable {
                workspace_id: workspace.into(),
            },
        )?
    } else if command.is_empty() && options.is_empty() {
        let (state, _) = langspan::client::status_from(state_dir, &["auto_checks"])?;
        if !state.workspaces.contains_key(workspace) {
            return Err(format!("workspace {workspace:?} is not registered").into());
        }
        match state.auto_checks.get(workspace) {
            Some(check) => Outcome::AutoCheck {
                check: check.clone(),
            },
            None => Outcome::AutoCheckDisabled {
                workspace_id: workspace.into(),
            },
        }
    } else {
        let name = check_name(options)?;
        let timeout_s = u32::try_from(number(options, "--timeout-s", 1800)?)
            .map_err(|_| "--timeout-s is at most 3600")?;
        no_unused_options(options)?;
        let (command, wider) = wider_check(command)?;
        if command.is_empty() {
            return Err("give the check command after --, for example: -- cargo test".into());
        }
        langspan::auto_check::refuse_secrets(command)?;
        if let Some(wider) = &wider {
            langspan::auto_check::refuse_secrets(&wider.argv)?;
        }
        ask(
            state_dir,
            unique("auto-check", &[workspace, &name]),
            Command::AutoCheckEnable {
                workspace_id: workspace.into(),
                name,
                argv: command.to_vec(),
                timeout_s,
                on_interface_change: wider.map(Box::new),
            },
        )?
    };
    if json {
        println!("{}", serde_json::to_string(&outcome)?);
        return Ok(0);
    }
    print_human(
        &Response {
            version: PROTOCOL_VERSION,
            request_id: String::new(),
            replayed: false,
            generation: 0,
            lease_valid: None,
            outcome,
        },
        state_dir,
    )?;
    Ok(0)
}

/// `COMMAND... --on-interface-change NAME [--timeout-s N] -- COMMAND2...`:
/// the regular command and the wider one, which runs whole after it when a
/// session's writes changed an indexed interface that other files mention.
fn wider_check(
    command: &[String],
) -> Result<(&[String], Option<langspan_core::WiderCheck>), langspan::Error> {
    const USAGE: &str = "give the wider check as --on-interface-change NAME [--timeout-s N] -- COMMAND..., after the regular command";
    let Some(at) = command
        .iter()
        .position(|arg| arg == "--on-interface-change")
    else {
        return Ok((command, None));
    };
    let (regular, rest) = command.split_at(at);
    let (name, mut rest) = match rest {
        [_, name, rest @ ..] if !name.starts_with("--") => (name.clone(), rest),
        _ => return Err(USAGE.into()),
    };
    let mut timeout_s = 1800;
    if let [flag, value, after @ ..] = rest
        && flag == "--timeout-s"
    {
        timeout_s = value
            .parse::<u32>()
            .ok()
            .filter(|seconds| (1..=langspan_core::MAX_AUTO_CHECK_TIMEOUT_S).contains(seconds))
            .ok_or("--timeout-s of the wider check is 1 to 3600 seconds")?;
        rest = after;
    }
    match rest {
        [separator, argv @ ..] if separator == "--" && !argv.is_empty() => Ok((
            regular,
            Some(langspan_core::WiderCheck {
                name,
                argv: argv.to_vec(),
                timeout_s,
            }),
        )),
        _ => Err(USAGE.into()),
    }
}
