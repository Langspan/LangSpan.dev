//! `langspan integrate check`, `land`, `land-queue` and `land-cancel`.

use crate::check::{check_name, unique};
use crate::flags::number;
use langspan::cli::options::no_unused_options;
use langspan::cli::render::{
    print_combined, print_conflict, print_inconclusive, print_not_rewritten, print_notes,
    print_renames, print_replay_conflict,
};
use std::collections::BTreeMap;

/// `integrate check` and `land`: the shared operations in
/// `langspan::landing`, with their results printed for a person.
pub(crate) fn integrate(
    state_dir: &std::path::Path,
    workspace: &str,
    branches: &[&str],
    landing: Option<bool>,
    command: &[String],
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    use langspan::landing::{self, Check, Integrated, Landed, Target};
    use langspan_core::CheckResult;
    let name = check_name(options)?;
    let onto = options
        .remove("--onto")
        .ok_or("--onto BRANCH is required: the branch the work is to be integrated into")?;
    let remote = options
        .remove("--remote")
        .unwrap_or_else(|| "origin".into());
    let timeout_s = number(options, "--timeout-s", 1800)?;
    let session = options.remove("--session");
    let request_id = options.remove("--request-id").unwrap_or_else(|| {
        unique(
            if landing.is_some() { "land" } else { "check" },
            &[workspace, &name],
        )
    });
    let check_again = options.remove("--check-again").is_some();
    let shape = langspan::integrate::Shape {
        rebase: options.remove("--rebase").is_some(),
        adapt_renames: options.remove("--adapt-renames").is_some(),
    };
    let symbols = options.remove("--symbols").is_some();
    if landing.is_some() && symbols {
        return Err("--symbols is valid only for `integrate check`".into());
    }
    if landing.is_none() && options.contains_key("--wait-s") {
        return Err("--wait-s is valid only for `land`".into());
    }
    let wait_s = number(options, "--wait-s", landing::DEFAULT_WAIT_S)?;
    no_unused_options(options)?;
    let target = Target {
        workspace_id: workspace.into(),
        remote: remote.clone(),
        onto: onto.clone(),
        name: name.clone(),
        branches: branches.iter().map(|branch| (*branch).to_owned()).collect(),
        shape,
    };
    let check = Check {
        argv: command.to_vec(),
        timeout_s,
        symbols,
    };
    let Some(apply) = landing else {
        if check_again {
            return Err("--check-again is valid only for `land`".into());
        }
        if command.is_empty() {
            return Err("give the check command after --, for example: -- cargo test".into());
        }
        let report = landing::integrate_check(state_dir, &request_id, &target, &check)?;
        if json {
            println!("{}", serde_json::to_string(&report)?);
            return Ok(report.exit_code());
        }
        match &report {
            Integrated::NotCombinable {
                branch,
                commit,
                paths,
                conflicts,
                ..
            } => {
                print_replay_conflict(commit.as_deref());
                print_conflict(branch, paths, conflicts, &onto, &remote)
            }
            Integrated::NotAdapted {
                branch,
                reason,
                notes,
                ..
            } => print_not_rewritten(false, branch, reason, notes),
            Integrated::NotRebased {
                branch,
                reason,
                notes,
                ..
            } => print_not_rewritten(true, branch, reason, notes),
            Integrated::Checked {
                target_oid,
                combined,
                check,
                inconclusive,
                ..
            } => {
                print_combined(&target, target_oid, combined);
                match check.result {
                    CheckResult::Passed {} => {
                        println!("{name} passed on the combined tree {}.", combined.tree)
                    }
                    CheckResult::Failed { exit_code } => println!(
                        "{name} failed (exit {exit_code}) on the combined tree {}.",
                        combined.tree
                    ),
                    CheckResult::Inconclusive {} => print_inconclusive(&name, inconclusive),
                }
                if let Some(found) = &check.symbol_findings {
                    println!("Definition findings (found by name; advisory):");
                    let lines = langspan::definitions::lines(found);
                    if lines.is_empty() {
                        println!(
                            "- none in the {} changed files indexed",
                            found.files_indexed
                        );
                    }
                    for line in lines {
                        println!("- {line}");
                    }
                }
            }
        }
        return Ok(report.exit_code());
    };
    if check_again == command.is_empty() {
        return Err(if check_again {
            "--check-again needs the check command after --, for example: -- cargo test"
        } else {
            "land runs nothing unless --check-again is given; run `langspan integrate check` first"
        }
        .into());
    }
    let report = landing::land(
        state_dir,
        &request_id,
        &target,
        apply,
        check_again.then_some(&check),
        session.as_deref(),
        wait_s,
    )?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(report.exit_code());
    }
    let attempts = |attempts: &[langspan_core::LandingAttempt]| {
        if attempts.len() > 1 {
            println!(
                "{} attempts: {onto:?} moved while landing, and the result was combined again each time.",
                attempts.len()
            );
        }
    };
    match &report {
        Landed::Busy {
            reason,
            holder,
            held_ms,
            waiting,
            ..
        } => {
            use langspan::landing::Unavailable;
            let held = match (holder, held_ms) {
                (Some(holder), Some(held_ms)) => format!(
                    "another landing is in progress by {} onto {remote}/{onto} (turn {}, held for {} s)",
                    holder
                        .session_id
                        .as_deref()
                        .map_or("a terminal".to_owned(), |session| format!(
                            "session {session}"
                        )),
                    holder.turn_id,
                    held_ms / 1000
                ),
                _ => format!("the push turn for {remote}/{onto} is free"),
            };
            let line = match waiting.len() {
                0 => String::new(),
                n => format!("; {n} landing(s) wait in line"),
            };
            println!(
                "Not landed: {}. Nothing was fetched or pushed.",
                match reason {
                    Unavailable::TurnHeld | Unavailable::OthersWaiting => format!(
                        "{held}{line}, and this landing was asked not to wait (--wait-s 0); try again when it ends"
                    ),
                    Unavailable::QueueFull =>
                        format!("{held}{line}: the line is full; try again when it is shorter"),
                    Unavailable::TimedOut => format!(
                        "waited {wait_s} s for the push turn, as long as --wait-s allows; {held}{line}"
                    ),
                    Unavailable::Cancelled => format!(
                        "this landing was cancelled while it waited for the push turn (or it stopped asking for longer than 15 s); {held}{line}"
                    ),
                }
            );
        }
        Landed::NotCombinable {
            branch,
            commit,
            paths,
            conflicts,
            attempts: tried,
            ..
        } => {
            attempts(tried);
            print_replay_conflict(commit.as_deref());
            print_conflict(branch, paths, conflicts, &onto, &remote)
        }
        Landed::NotAdapted {
            branch,
            reason,
            notes,
            attempts: tried,
            ..
        } => {
            attempts(tried);
            print_not_rewritten(false, branch, reason, notes);
        }
        Landed::NotRebased {
            branch,
            reason,
            notes,
            attempts: tried,
            ..
        } => {
            attempts(tried);
            print_not_rewritten(true, branch, reason, notes);
        }
        Landed::NothingToLand { .. } => {
            println!(
                "Nothing to land: {remote}/{onto} already contains these branches, or their changes."
            )
        }
        Landed::NotReady {
            target_oid,
            combined,
            reason,
            moved,
            attempts: tried,
            ..
        } => {
            attempts(tried);
            print_combined(&target, target_oid, combined);
            if *moved {
                println!(
                    "Not landed: {onto:?} moved during the landing, and the new combined tree has no passing check."
                );
            }
            println!("Not ready to land: {reason}.");
            println!(
                "Run: langspan integrate check {workspace} --onto {onto} --name {name} {} -- <command>",
                branches.join(" ")
            );
            println!("Or land with --check-again -- <command> to check each new combined tree.");
        }
        Landed::CheckFailed {
            combined,
            check,
            inconclusive,
            attempts: tried,
            notes,
        } => {
            attempts(tried);
            print_notes(notes);
            println!(
                "Not landed: {name} {} on the combined tree {}.",
                match check.result {
                    CheckResult::Failed { exit_code } => format!("failed (exit {exit_code})"),
                    _ => "was inconclusive".into(),
                },
                combined.tree
            );
            if check.result == (CheckResult::Inconclusive {}) {
                print_inconclusive(&name, inconclusive);
            }
        }
        Landed::Ready {
            target_oid,
            combined,
            check,
            ..
        } => {
            print_combined(&target, target_oid, combined);
            println!(
                "Ready: check {:?} ({}) passed on exactly this tree.",
                check.name, check.check_id
            );
            println!(
                "Dry run; nothing was pushed. Run the same command with --apply to push {} to {remote}/{onto}.",
                combined.commit
            );
        }
        Landed::Landed {
            landing,
            attempts: tried,
            renames,
            notes,
            waited_ms,
            replayed,
            unfinished_push,
        } => {
            attempts(tried);
            if *waited_ms >= 1000 {
                println!("Waited {} s in line for the push turn.", waited_ms / 1000);
            }
            print_renames(renames);
            print_notes(notes);
            if *replayed {
                println!("This request had already landed.");
            }
            if let Some(error) = unfinished_push {
                println!(
                    "The push did not finish ({error}), but {remote}/{onto} is at the pushed commit afterwards: it was pushed."
                );
            }
            println!(
                "Landed {} on {remote}/{onto}, on check {} for exactly its tree. Your local branches were not changed.",
                landing.commit_oid, landing.check_id
            );
        }
        Landed::Refused {
            attempts: tried, ..
        } => {
            attempts(tried);
            println!(
                "Not landed: {remote} refused the push, and {onto:?} had not moved. A hook, a permission or a protection rule refused it; this is not retried."
            );
        }
        Landed::GaveUp { attempts: tried } => println!(
            "Not landed: {onto:?} moved under each of {} attempts. Nothing was forced; try again later.",
            tried.len()
        ),
        Landed::PushedNotRecorded {
            combined,
            check_id,
            error,
            verify,
            attempts: tried,
            ..
        } => {
            attempts(tried);
            println!(
                "Pushed {} to {remote}/{onto} on check {check_id}, but LangSpan could not record the landing: {error}.",
                combined.commit
            );
            println!(
                "The remote branch changed; LangSpan's log does not show it. Verify with: {verify} (expect {}). Running this landing again does not push again: it finds nothing to land.",
                combined.commit
            );
        }
        Landed::NotPushed {
            target_oid,
            combined,
            error,
            attempts: tried,
            ..
        } => {
            attempts(tried);
            println!(
                "Not landed: the push of {} to {remote}/{onto} did not finish ({error}), and {onto:?} is still at {target_oid} on {remote}: nothing was pushed. This is not retried; a pre-push hook or the remote may be hanging.",
                combined.commit
            );
        }
        Landed::PushUnknown {
            target_oid,
            combined,
            error,
            remote_oid,
            verify,
            attempts: tried,
            ..
        } => {
            attempts(tried);
            println!(
                "The push of {} to {remote}/{onto} did not finish: {error}. Whether the remote branch changed is not known.",
                combined.commit
            );
            if let Some(now) = remote_oid {
                println!(
                    "Afterwards {onto:?} on {remote} is at {now}, neither the pushed commit nor the earlier tip: someone else pushed to it meanwhile."
                );
            }
            println!(
                "Check with: {verify}. {} means it landed and LangSpan has not recorded it; {target_oid} means nothing was pushed.",
                combined.commit
            );
        }
    }
    Ok(report.exit_code())
}

/// `land-queue`: who holds the push turn and who waits for it.
/// `land-cancel`: take a waiting landing out of the line.
pub(crate) fn landing_queue(
    state_dir: &std::path::Path,
    workspace: &str,
    cancel: bool,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    let onto = options
        .remove("--onto")
        .ok_or("--onto BRANCH is required: the branch whose push turn is meant")?;
    if cancel {
        let ticket = options
            .remove("--request-id")
            .ok_or("--request-id ID is required: the request ID of the waiting landing")?;
        no_unused_options(options)?;
        let left = langspan::landing::cancel(
            state_dir,
            workspace,
            &onto,
            &ticket,
            langspan_core::LeftQueue::Cancelled,
        )?;
        if json {
            println!("{}", serde_json::json!({ "ticket": ticket, "left": left }));
        } else if left {
            println!(
                "Landing {ticket} stopped waiting for the push turn of {onto:?}; it pushes nothing."
            );
        } else {
            println!(
                "Landing {ticket} was not waiting for the push turn of {onto:?}; nothing changed."
            );
        }
        return Ok(0);
    }
    no_unused_options(options)?;
    let view = langspan::landing::queue(state_dir, workspace, &onto)?;
    if json {
        println!("{}", serde_json::to_string(&view)?);
        return Ok(0);
    }
    match (&view.holder, view.held_ms) {
        (Some(holder), Some(held_ms)) => println!(
            "The push turn of {onto:?} is held by landing {} ({}) for {} s.",
            holder.turn_id,
            holder
                .session_id
                .as_deref()
                .map_or("a terminal".to_owned(), |session| format!(
                    "session {session}"
                )),
            held_ms / 1000
        ),
        _ => println!("The push turn of {onto:?} is free."),
    }
    if view.waiting.is_empty() {
        println!("Nobody is waiting for it.");
    }
    for waiter in &view.waiting {
        println!(
            "  {}. landing {} ({}), waiting {} s, gives up in {} s",
            waiter.position,
            waiter.ticket,
            waiter
                .session_id
                .as_deref()
                .map_or("a terminal".to_owned(), |session| format!(
                    "session {session}"
                )),
            waiter.waited_ms / 1000,
            waiter.wait_left_ms / 1000
        );
    }
    Ok(0)
}
