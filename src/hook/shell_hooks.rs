//! The shell hooks: a shell tool call's operation, and the session's steps
//! before and after the command runs (what it may write, what it changed).
use super::{
    BreakCheck, Failure, Identities, Operation, Phase, Reply, Session, ShellCall, add_alert,
    failure_reply, file_key, file_state, input_fingerprint, looked_key, observed_files, wire_id,
};
use crate::protocol::Response;
use langspan_core::{Command, Outcome, ToolResult};
use serde_json::json;
use std::{path::Path, time::Instant};

/// The operation for a shell tool call (`Bash` on both hosts). Before it
/// runs, the files it will write are named only when the whole command
/// parsed and every target is a file inside this workspace; otherwise there
/// is no decision before it, and what it changed is compared after.
/// `pre_tool_context`: the host gives the model context from the pre-tool
/// reply, so a formatter notice can reach it (issue #126).
#[allow(clippy::too_many_arguments)]
pub fn shell_operation(
    source: &str,
    host_session: &str,
    parent: Option<String>,
    tool_use_id: &str,
    command: &str,
    cwd: Option<&str>,
    pre: bool,
    background: bool,
    root: &Path,
    mid_turn_context: bool,
    pre_tool_context: bool,
) -> Operation {
    let (paths, phase) = if pre {
        match shell_keys(root, cwd, command) {
            Ok(keys) => (
                keys,
                Phase::ShellPre {
                    unknown: None,
                    background,
                },
            ),
            Err(why) => (
                Vec::new(),
                Phase::ShellPre {
                    unknown: Some(why),
                    background,
                },
            ),
        }
    } else {
        (Vec::new(), Phase::ShellPost)
    };
    Operation {
        source: source.into(),
        host_session: host_session.into(),
        tool_use_id: tool_use_id.into(),
        tool: SHELL_TOOL.into(),
        paths,
        // A shell command says nothing about which part of a file it
        // changes: every target is held whole.
        edits: Vec::new(),
        // Only the command decides what runs; a description or timeout the
        // host adds does not make it a different call.
        input_fingerprint: input_fingerprint(SHELL_TOOL, &json!({ "command": command })),
        phase,
        mid_turn_context,
        parent,
        snapshot_unknown: false,
        shell: Some(ShellCall {
            command: command.into(),
            cwd: cwd.map(str::to_owned),
        }),
        contents: Vec::new(),
        // The context carries a formatter notice, and a break alert for a
        // quoted heredoc's calls (`shell_pre`).
        // A shell write's calls are checked only before it runs.
        pre_tool_context,
    }
}

/// The hook-facing name of a shell call on both hosts.
pub const SHELL_TOOL: &str = langspan_core::SHELL_TOOL;

/// The existing paths in this workspace that a shell command named as what
/// it reads (`shell::looked_at`), at most `PRESENCE_PATHS`. Never the
/// checkout itself, and nothing inside `.git`.
pub(crate) fn shell_looked_at(root: &Path, call: &ShellCall) -> Vec<String> {
    let mut keys = Vec::new();
    for word in crate::shell::looked_at(&call.command) {
        if let Some(key) = looked_key(root, call.cwd.as_deref(), &word)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
        if keys.len() == langspan_core::PRESENCE_PATHS {
            break;
        }
    }
    keys
}

fn shell_keys(root: &Path, cwd: Option<&str>, command: &str) -> Result<Vec<String>, &'static str> {
    let mut keys = std::collections::BTreeSet::new();
    for target in crate::shell::write_targets(command)? {
        let key = file_key(root, cwd, &target)
            .map_err(|_| "a write target outside this workspace or not resolvable")?;
        if root.join(&key).is_dir() {
            return Err("a write target that is a directory");
        }
        keys.insert(key);
    }
    Ok(keys.into_iter().collect())
}

impl Session<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn shell_pre(
        &mut self,
        ids: &Identities,
        operation: &langspan_core::ToolIdentity,
        keys: Vec<String>,
        unknown: Option<&'static str>,
        background: bool,
        call: Option<&ShellCall>,
        pre_tool_context: bool,
    ) -> Reply {
        // A commit that would carry another live session's lines is held
        // once, before anything else (ADR 0026).
        let mut guarded = None;
        if let Some(call) = call {
            match crate::commit::guard_hook(self.config, &ids.session_id, call, self.deadline) {
                crate::commit::Guarded::Held(reply) => return reply,
                crate::commit::Guarded::Passed(detail) => guarded = Some(detail),
                crate::commit::Guarded::NotACommit => {}
            }
        }
        // The snapshot may use two thirds of the time left; the rest is kept
        // for the admission decision.
        let now = Instant::now();
        let snapshot_deadline = now + self.deadline.saturating_duration_since(now) * 2 / 3;
        let mut record = crate::shell::Record {
            session_id: ids.session_id.clone(),
            workspace: Some(crate::shell::workspace_identity(self.root)),
            started_ms: crate::shell::now_ms(),
            started_clock_ms: Some(crate::clock::now_ms()),
            admitted: false,
            before: crate::shell::snapshot(self.root, snapshot_deadline),
            finished_ms: None,
            changed: None,
            unobserved: None,
            contested: Vec::new(),
            pending: false,
            named: if unknown.is_none() {
                keys.clone()
            } else {
                Vec::new()
            },
            background,
            commit_mine: call.is_some_and(|call| crate::shell::runs_commit_mine(&call.command)),
            edited_elsewhere: Vec::new(),
            reformatted: Vec::new(),
            command: call.map(|call| crate::shell::kept_command(&call.command)),
            moves_checkout: call.is_some_and(|call| crate::shell::moves_checkout(&call.command)),
            checkout: None,
            finished_clock_ms: None,
        };
        if let Ok(before) = &record.before {
            crate::journal::before_shell(
                &self.config.state_dir,
                self.root,
                before,
                snapshot_deadline,
            );
        }
        // Only a complete snapshot outside a merge or rebase allows a decision.
        let note = match (&record.before, unknown) {
            (Err(reason), _) => Some(format!(
                "no decision; workspace snapshot incomplete ({reason}), so this command's changes will not be observed"
            )),
            (Ok(before), _) if before.in_progress => Some(
                "no decision while a merge, rebase or conflict is in progress; changes compared after".into(),
            ),
            (Ok(_), Some(why)) => Some(format!(
                "no decision: write set unknown ({why}); changes compared after"
            )),
            (Ok(_), None) if keys.is_empty() => {
                Some("writes no file by its parse; changes compared after".into())
            }
            (Ok(_), None) => None,
        };
        let decided = match note {
            Some(_) => None,
            None => {
                let reply = self.pre(ids, operation, keys, (&[], &[]), false, BreakCheck::Off);
                // A denied command does not run, so no run is recorded.
                if matches!(reply.coverage, "denied" | "denied_stale" | "denied_unread") {
                    return reply;
                }
                record.admitted = reply.coverage == "admitted";
                Some(reply)
            }
        };
        let started = crate::shell::start(&self.config.state_dir, &ids.operation_id, &record);
        let mut reply = self.shell_decided(decided, note, started, &record);
        if let Some(detail) = guarded {
            reply.detail = format!("{}; {detail}", reply.detail);
        }
        // A formatter that may rewrite another session's uncommitted files:
        // said once, never refused (issue #126). Only where the host gives
        // the model pre-tool context, so the notice is not spent unread.
        let formatter = match (call, &record.before) {
            (Some(call), Ok(before)) if pre_tool_context => {
                crate::formatter::notice(self.config, &ids.session_id, call, before, self.deadline)
            }
            _ => None,
        };
        if let Some(notice) = formatter {
            reply.detail = format!("{}; {}", reply.detail, notice.detail);
            if let Some(context) = notice.context.filter(|_| reply.stdout.is_empty()) {
                reply.stdout = json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "additionalContext": context,
                    }
                })
                .to_string();
            }
        }
        // A quoted heredoc states the whole new content of its file, so its
        // calls are checked like a `Write` (ADR 0029). Nothing above denied
        // it, so it will run.
        if pre_tool_context
            && let Some(call) = call
            && let Some((target, body)) = crate::shell::heredoc_write(&call.command)
            && let Ok(key) = file_key(self.root, call.cwd.as_deref(), &target)
        {
            let alert = match self.attach(ids) {
                Ok(()) => {
                    let contents = [(key.clone(), body)];
                    self.break_alert(ids, std::slice::from_ref(&key), &[], &contents)
                }
                Err(failure) => Some((
                    None,
                    format!("break check unavailable: {}", failure_reply(failure).detail),
                )),
            };
            if let Some(alert) = alert {
                add_alert(&mut reply, "PreToolUse", alert);
            }
        }
        reply
    }

    fn shell_decided(
        &self,
        decided: Option<Reply>,
        note: Option<String>,
        started: Result<(), String>,
        record: &crate::shell::Record,
    ) -> Reply {
        match (decided, note) {
            (Some(mut reply), _) => {
                if let Err(error) = started {
                    reply.detail =
                        format!("{}; changes will not be observed: {error}", reply.detail);
                }
                reply
            }
            (None, note) => {
                let note = note.unwrap_or_default();
                match started {
                    Err(error) => Reply::none(
                        "shell_unobserved",
                        format!("{note}; changes will not be observed: {error}"),
                    ),
                    Ok(()) if record.before.is_err() => Reply::none("shell_unobserved", note),
                    Ok(()) => Reply::none("shell_observing", note),
                }
            }
        }
    }

    pub(crate) fn shell_post(
        &mut self,
        ids: &Identities,
        operation: &langspan_core::ToolIdentity,
    ) -> Reply {
        let state_dir = &self.config.state_dir;
        let Some(record) = crate::shell::load(state_dir, &ids.operation_id) else {
            return Reply::none(
                "shell_unobserved",
                "no workspace snapshot from before this command; nothing recorded",
            );
        };
        let (run, kept) = match record.finished_ms {
            // Compared before, but the daemon did not record it: record it now.
            Some(_) if record.pending => (record, Ok(())),
            Some(_) => {
                return Reply::none("unknown", "this command's changes were already handled");
            }
            None => {
                let mut checkout = None;
                let result = match &record.before {
                    Err(reason) => Err(reason.clone()),
                    Ok(before) => {
                        crate::shell::snapshot(self.root, self.deadline).and_then(|after| {
                            let changed = crate::shell::changed(before, &after, |path| {
                                file_state(self.root, path)
                            });
                            // The session's own Git command changed the
                            // checkout: not news to it (issue #139).
                            if record.moves_checkout {
                                checkout = crate::shell::checkout_left(
                                    before,
                                    &after,
                                    changed.as_ref().is_ok_and(|files| !files.is_empty()),
                                );
                            }
                            changed
                        })
                    }
                };
                let mut run = record;
                run.checkout = checkout;
                run.finished_ms = Some(crate::shell::now_ms());
                run.finished_clock_ms = Some(crate::clock::now_ms());
                match result {
                    Ok(changed) => {
                        run.contested =
                            crate::shell::contested(state_dir, &ids.operation_id, &run, &changed);
                        if let Ok(before) = &run.before {
                            // Another session's hooked edits while the
                            // command ran are that session's, or unknown.
                            let edits = crate::shell::overlapping_edits(state_dir, &run);
                            let (theirs, unknown) = crate::shell::classify_edits(
                                &edits.marks,
                                &changed,
                                |path| match before.files.get(path) {
                                    Some(entry) => Some(entry.state.clone()),
                                    None => crate::journal::head_state(
                                        self.root,
                                        path,
                                        before.head.as_deref(),
                                        self.deadline,
                                    ),
                                },
                                |path| file_state(self.root, path),
                            );
                            run.edited_elsewhere = theirs;
                            // Contested only because another session's shell
                            // run overlapped, so far.
                            let overlapped: Vec<String> = run
                                .contested
                                .iter()
                                .filter(|path| !unknown.contains(path))
                                .cloned()
                                .collect();
                            run.contested.extend(unknown);
                            // A rewrite of another session's uncommitted
                            // lines (a formatter's, say) is both sessions'.
                            // That file was still changed by this command,
                            // so it is noted as this command's change below.
                            let mut noted = run.attributed(&changed);
                            let rewritten = crate::journal::rewrites_others(
                                state_dir,
                                self.root,
                                &ids.session_id,
                                &noted,
                                before,
                                self.deadline,
                            );
                            run.contested.extend(rewritten.iter().cloned());
                            let mut partial = Vec::new();
                            // Past a shell call's supported lifetime, what
                            // other sessions did may no longer be known; nor
                            // when an edit may have gone unmarked at the cap.
                            if edits.incomplete || run.outlived(run.finished_ms.unwrap_or_default())
                            {
                                run.contested.extend(changed.iter().cloned());
                                noted.clear();
                            } else {
                                // A line the command only reformatted keeps
                                // its author, whatever ran alongside (issues
                                // #124 and #141); a change of tokens is
                                // still credited to no session.
                                let candidates: Vec<crate::journal::Candidate> = overlapped
                                    .into_iter()
                                    .map(|path| (path, true))
                                    .chain(rewritten.iter().map(|path| (path.clone(), false)))
                                    .filter(|(path, _)| !run.edited_elsewhere.contains(path))
                                    .map(|(path, overlapped)| crate::journal::Candidate {
                                        path,
                                        overlapped,
                                    })
                                    .collect();
                                let reformats = crate::journal::reformat_authors(
                                    state_dir,
                                    self.root,
                                    &ids.session_id,
                                    &candidates,
                                    before,
                                    self.deadline,
                                );
                                run.contested.retain(|path| !reformats.whole.contains(path));
                                // Now this session's, so this command's change.
                                noted.extend(reformats.whole.iter().cloned());
                                partial = reformats.partial;
                            }
                            run.contested.sort();
                            run.contested.dedup();
                            noted.sort();
                            noted.dedup();
                            self.note_shell_changes(&run, &noted);
                            run.reformatted =
                                partial.iter().map(|(path, _, _)| path.clone()).collect();
                            run.reformatted.dedup();
                            if let Err(error) = crate::journal::record_partial(
                                state_dir,
                                self.root,
                                &partial,
                                run.started_ms,
                            ) {
                                eprintln!(
                                    "{}",
                                    json!({"event":"journal_unavailable","detail":error})
                                );
                            }
                            let attributed = run.attributed(&changed);
                            if let Err(error) = crate::journal::record(
                                state_dir,
                                self.root,
                                &ids.session_id,
                                &attributed,
                                crate::journal::Origin::Shell {
                                    snapshot: before,
                                    deadline: self.deadline,
                                    started_ms: run.started_ms,
                                },
                            ) {
                                eprintln!(
                                    "{}",
                                    json!({"event":"journal_unavailable","detail":error})
                                );
                            }
                        }
                        run.changed = Some(changed);
                    }
                    Err(reason) => run.unobserved = Some(reason),
                }
                // Kept until the daemon has recorded it, so a redelivered
                // hook records this outcome rather than comparing again.
                let kept = crate::shell::computed(state_dir, &ids.operation_id, &run);
                (run, kept)
            }
        };
        let reply = match self.publish_shell(ids, operation, &run) {
            Ok(reply) => reply,
            Err(failure) => {
                let mut reply = failure_reply(failure);
                reply.detail = match kept {
                    Ok(()) => format!(
                        "{}; this command's changes are kept and are recorded when this hook is delivered again",
                        reply.detail
                    ),
                    Err(error) => format!(
                        "{}; this command's changes could not be kept for a retry ({error}), so a redelivered hook compares again",
                        reply.detail
                    ),
                };
                return reply;
            }
        };
        match crate::shell::finish(state_dir, &ids.operation_id, &run) {
            Ok(()) => reply,
            Err(error) => Reply {
                detail: format!("{}; run record not closed: {error}", reply.detail),
                ..reply
            },
        }
    }

    /// Note, on this machine, that this run's command made each of `paths`
    /// the version on disk now, so that another session's refusal for one of
    /// them can name the command (issue #107). A file that cannot be read
    /// within the deadline is not noted; a failure is logged, not fatal.
    fn note_shell_changes(&self, run: &crate::shell::Record, paths: &[String]) {
        let files: Vec<(String, langspan_core::FileState)> = paths
            .iter()
            .take(crate::shell::MAX_NOTED_PER_RUN)
            .take_while(|_| Instant::now() < self.deadline)
            .filter_map(|path| Some((path.clone(), file_state(self.root, path)?)))
            .collect();
        if let Err(error) = crate::shell::note_changes(
            &self.config.state_dir,
            self.root,
            &run.session_id,
            run.command.as_deref(),
            &files,
        ) {
            eprintln!(
                "{}",
                json!({"event":"change_notes_unavailable","detail":error})
            );
        }
    }

    /// Record a finished run's outcome with the daemon and complete its
    /// admission. Every step has a request ID derived from the tool call, so
    /// a retry after a failure repeats it without recording twice.
    fn publish_shell(
        &self,
        ids: &Identities,
        operation: &langspan_core::ToolIdentity,
        run: &crate::shell::Record,
    ) -> Result<Reply, Failure> {
        // First, so that this hook's own briefing already leaves it out.
        if let Some(left) = &run.checkout {
            self.attach(ids)?;
            // Durations on the daemon's clock, read from the record written
            // when the command finished, so a redelivery keeps its window.
            let (ran_ms, finished_ago_ms) = run.checkout_timing(crate::clock::now_ms());
            match self.call(
                wire_id(
                    "checkout-run",
                    &[self.source, &ids.session_id, &ids.operation_id],
                ),
                Command::ObserveCheckoutRun {
                    session_id: ids.session_id.clone(),
                    command: run.command.clone(),
                    to_branch: left.branch.clone(),
                    to_head: left.head.clone(),
                    ran_ms,
                    finished_ago_ms,
                    files_unchanged: left.files_unchanged,
                },
            ) {
                Ok(_) => {}
                // Refused as invalid: the checkout change stays news to
                // this session, as before; nothing else depends on it.
                Err(Failure::Rejected(reason)) => eprintln!(
                    "{}",
                    json!({"event":"checkout_run_refused","detail":reason})
                ),
                Err(failure) => return Err(failure),
            }
        }
        let changed = match (&run.changed, &run.unobserved) {
            (Some(changed), _) => changed,
            (None, reason) => {
                self.release(ids, run)?;
                return Ok(Reply::none(
                    "shell_unobserved",
                    format!(
                        "{}; nothing recorded",
                        reason.as_deref().unwrap_or("the change set is unknown")
                    ),
                ));
            }
        };
        if changed.is_empty() {
            self.release(ids, run)?;
            return Ok(Reply::none(
                "shell_unchanged",
                "the command changed no file; nothing recorded",
            ));
        }
        let attributed = run.attributed(changed);
        if attributed.len() > 64 {
            self.release(ids, run)?;
            return Ok(Reply::none(
                "shell_unobserved",
                "more than 64 files changed; nothing recorded",
            ));
        }
        let mut relation = String::new();
        if !attributed.is_empty() {
            self.attach(ids)?;
            match self.call(
                wire_id(
                    "observe-shell",
                    &[self.source, &ids.session_id, &ids.operation_id],
                ),
                Command::ObserveTool {
                    session_id: ids.session_id.clone(),
                    operation: operation.clone(),
                    paths: attributed.clone(),
                    // The changes happened whatever the command's exit status.
                    result: ToolResult::Succeeded,
                    lease_id: Some(crate::wait::admission_of(
                        &self.config.state_dir,
                        &ids.operation_id,
                        &ids.admit,
                    )),
                },
            )? {
                Response {
                    outcome: Outcome::Observed { observation },
                    ..
                } => {
                    relation = format!(
                        "; admission {}",
                        format!("{:?}", observation.lease_relation).to_lowercase()
                    );
                }
                other => {
                    return Ok(Reply::none(
                        "unknown",
                        format!("unexpected outcome {other:?}"),
                    ));
                }
            }
            // What the session just wrote is the version it now knows.
            let _ = self.record_source(
                ids,
                observed_files(self.root, &attributed),
                langspan_core::SourceOrigin::Write,
            );
        }
        self.release(ids, run)?;
        let elsewhere = match run.edited_elsewhere.len() {
            0 => String::new(),
            theirs => format!(
                "; {theirs} to another session's edit made while the command ran, recorded by that session's hooks"
            ),
        };
        let partly = match run.reformatted.len() {
            0 => String::new(),
            partly => format!(
                " ({partly} of them keep the authors of the lines the command only reformatted)"
            ),
        };
        Ok(if run.contested.is_empty() {
            Reply::none(
                "shell_observed",
                format!(
                    "{} changed file(s) attributed to this session{elsewhere}{relation}",
                    attributed.len()
                ),
            )
        } else {
            Reply::none(
                "contested",
                format!(
                    "{} changed file(s) attributed to this session{elsewhere}; {} attributed to no session{partly}, because another session's shell run or edit overlapped and could have written them, or the command rewrote another session's uncommitted lines{relation}",
                    attributed.len(),
                    run.contested.len()
                ),
            )
        })
    }

    /// The command has finished, so an admission made for it is over. A
    /// daemon that cannot be reached is a failure to retry; a refusal (the
    /// admission already ended) is not.
    fn release(&self, ids: &Identities, record: &crate::shell::Record) -> Result<(), Failure> {
        if record.admitted {
            match self.call(
                ids.complete.clone(),
                Command::Complete {
                    session_id: ids.session_id.clone(),
                    lease_id: crate::wait::admission_of(
                        &self.config.state_dir,
                        &ids.operation_id,
                        &ids.admit,
                    ),
                },
            ) {
                Ok(_) | Err(Failure::Rejected(_)) => {}
                Err(failure) => return Err(failure),
            }
        }
        Ok(())
    }
}
