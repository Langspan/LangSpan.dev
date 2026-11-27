use super::*;

/// The host identity of a subagent: the parent's host session joined to the
/// subagent's ID, so each subagent is its own LangSpan session with its own
/// read and written versions. An agent ID outside the grammar, or long
/// enough to push the joined ID past 100 bytes, is replaced by a digest, so
/// two subagents never share an identity.
pub fn subagent_session(host_session: &str, agent_id: &str) -> Option<String> {
    if !valid_host_id(host_session) || agent_id.is_empty() || agent_id.len() > 4096 {
        return None;
    }
    let plain = agent_id.len() <= 40
        && agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte));
    let agent = if plain {
        agent_id.to_owned()
    } else {
        format!(
            "h{}",
            &sha256::hex(&sha256::digest(agent_id.as_bytes()))[..24]
        )
    };
    let joined = format!("{host_session}.{agent}");
    valid_host_id(&joined).then_some(joined)
}

/// Whether the host was started without a person at its terminal. Claude
/// Code's hook payloads do not say: a `claude -p` SessionStart has the same
/// fields as an interactive one (`source` is `startup`, a transcript path is
/// present). Claude Code does set `CLAUDE_CODE_SESSION_ATTENDED` in every
/// hook's environment: `0` under `-p`, `1` in a terminal session (observed on
/// 2.1.322; not in its documentation). Only an explicit `0` counts, so a
/// version that drops the variable falls back to treating every session as
/// attended, which is the behaviour before this check existed. Codex gives
/// no such signal, in its payloads or its documentation: `codex exec` is not
/// detected.
pub fn headless_host(source: &str) -> bool {
    source == crate::claude_code::SOURCE
        && std::env::var("CLAUDE_CODE_SESSION_ATTENDED").is_ok_and(|value| value == "0")
}

pub fn execute(operation: Operation, config: &Config, deadline: Instant) -> Reply {
    if !valid_host_id(&operation.host_session) || !valid_host_id(&operation.tool_use_id) {
        return Reply::none("unsupported", "missing or unusable native tool identity");
    }
    if Instant::now() >= deadline {
        return Reply::none("timeout", "hook deadline elapsed before coordination");
    }
    let ids = identities_for(
        &operation.source,
        &operation.host_session,
        &operation.tool_use_id,
    );
    let identity = langspan_core::ToolIdentity {
        operation_id: ids.operation_id.clone(),
        source: operation.source,
        tool: operation.tool,
        input_digest: operation.input_fingerprint,
    };
    // An edit's files, for a shell run of another session going at the same
    // time (ADR 0011): before a call, the versions it starts from; after
    // one, what it left. Both are read here, on entry and before any daemon
    // call, so that a shell write landing while this hook waits on the
    // daemon is not taken for the edit's result.
    let versions = |paths: &[String]| -> std::collections::BTreeMap<_, _> {
        paths
            .iter()
            .map(|path| (path.clone(), file_state(&config.root, path)))
            .collect()
    };
    let marked_before = matches!(operation.phase, Phase::Pre).then(|| versions(&operation.paths));
    let marked_after =
        matches!(operation.phase, Phase::Post(_)).then(|| versions(&operation.paths));
    // The versions a completed edit or read left on disk, read now: they are
    // what is recorded, now or when a report that fails is sent again.
    let files = match operation.phase {
        Phase::Post(ToolResult::Succeeded) | Phase::Read => {
            observed_files(&config.root, &operation.paths)
        }
        _ => Vec::new(),
    };
    let report = match operation.phase {
        Phase::Post(result) => Some(crate::spool::Report::Post {
            result,
            paths: operation.paths.clone(),
            files: files.clone(),
        }),
        Phase::Read => Some(crate::spool::Report::Read {
            files: files.clone(),
        }),
        Phase::ShellPost => Some(crate::spool::Report::Shell),
        Phase::Pre | Phase::ShellPre { .. } => None,
    };
    let report_id = report
        .as_ref()
        .map(|report| report_request_id(&identity.source, &ids, report));
    let resent = resend_owed(
        config,
        deadline,
        &crate::spool::Live {
            session: &ids.session_id,
            request_id: report_id.as_deref(),
            newer: matches!(operation.phase, Phase::Post(_) | Phase::Read).then_some(
                crate::spool::Superseding {
                    paths: &operation.paths,
                    edit: matches!(operation.phase, Phase::Post(_)),
                },
            ),
        },
    );
    let mut session = Session {
        config,
        deadline,
        root: &config.root,
        source: &identity.source,
        parent: operation.parent.as_deref(),
        headless: headless_host(&identity.source),
        unreachable: Default::default(),
        reached: Default::default(),
    };
    // The line journal (ADR 0026): the files an edit starts from, and what
    // it changed in them. Kept on this machine whatever the daemon answers.
    let mut journaled = None;
    match operation.phase {
        Phase::Pre => crate::journal::before_edit(
            &config.state_dir,
            &ids.operation_id,
            &config.root,
            &operation.paths,
        ),
        Phase::Post(ToolResult::Succeeded) => {
            journaled = Some(crate::journal::record(
                &config.state_dir,
                &config.root,
                &ids.session_id,
                &operation.paths,
                crate::journal::Origin::Edit {
                    operation_id: &ids.operation_id,
                },
            ));
        }
        _ => {}
    }
    let mut reply = match operation.phase {
        Phase::Pre => session.pre(
            &ids,
            &identity,
            operation.paths,
            (&operation.edits, &operation.contents),
            operation.snapshot_unknown,
            if operation.pre_tool_context {
                BreakCheck::Now
            } else {
                BreakCheck::Off
            },
        ),
        Phase::Post(result) => session.observe(&ids, &identity, operation.paths, result, files),
        Phase::Read => session.read(&ids, files),
        Phase::ShellPre {
            unknown,
            background,
        } => session.shell_pre(
            &ids,
            &identity,
            operation.paths,
            unknown,
            background,
            operation.shell.as_ref(),
            operation.pre_tool_context,
        ),
        Phase::ShellPost => session.shell_post(&ids, &identity),
    };
    // The paths a completed shell command read are where its session is
    // looking (roadmap A13). Advisory: the reply is the same either way.
    if matches!(operation.phase, Phase::ShellPost)
        && config.presence
        && !matches!(
            reply.coverage,
            "timeout" | "unavailable" | "unknown" | "unsupported"
        )
        && let Some(call) = operation.shell.as_ref()
    {
        let paths = shell_looked_at(&config.root, call);
        // A command that changed nothing has not attached the session.
        let looked = if paths.is_empty() {
            Ok(0)
        } else {
            session
                .attach(&ids)
                .and_then(|()| session.look(&ids, langspan_core::PresenceKind::Shell, paths))
        };
        match looked {
            Ok(0) => {}
            Ok(count) => reply.detail = format!("{}; {count} path(s) looked at", reply.detail),
            Err(failure) => {
                reply.detail = format!(
                    "{}; presence not recorded: {}",
                    reply.detail,
                    failure_reply(failure).detail
                )
            }
        }
    }
    if let Some(Err(error)) = journaled {
        reply.detail = format!("{}; line journal: {error}", reply.detail);
    }
    // An edit that is refused does not run, so it is not marked.
    let marked = match (operation.phase, marked_before, marked_after) {
        (Phase::Pre, Some(before), _) if !reply.coverage.starts_with("denied") => {
            Some(crate::shell::edit_started(
                &config.state_dir,
                &ids.operation_id,
                &ids.session_id,
                &config.root,
                before,
            ))
        }
        (Phase::Post(_), _, Some(after)) => Some(crate::shell::edit_finished(
            &config.state_dir,
            &ids.operation_id,
            &ids.session_id,
            &config.root,
            after,
        )),
        _ => None,
    };
    if let Some(Err(error)) = marked {
        reply.detail = format!("{}; edit mark: {error}", reply.detail);
    }
    if let (Some(report), Some(request_id)) = (report, report_id)
        && session.unreachable.get()
    {
        let (created_ms, created_wall_ms) = crate::spool::clocks();
        let entry = crate::spool::Entry {
            request_id,
            created_ms,
            created_wall_ms,
            tries: 0,
            workspace_id: config.workspace_id.clone(),
            root: config.root.clone(),
            source: identity.source.clone(),
            host_session: operation.host_session.clone(),
            tool_use_id: operation.tool_use_id.clone(),
            tool: identity.tool.clone(),
            input_digest: identity.input_digest.clone(),
            parent: operation.parent.clone(),
            presence: crate::spool::Presence {
                shown: config.presence,
                window_ms: config.presence_window_ms,
            },
            report,
        };
        reply.detail = match crate::spool::keep(&config.state_dir, &entry) {
            Ok(()) => format!(
                "{}; kept to send again at the next hook run that reaches the coordinator",
                reply.detail
            ),
            Err(error) => format!("{}; could not be kept to send again: {error}", reply.detail),
        };
    }
    if let Some(resent) = resent {
        reply.detail = format!("{}; {resent}", reply.detail);
    }
    // After a completed tool call, tell the model what changed around it
    // since it was last told. Only when something did, and never in place
    // of a decision.
    let completed = matches!(
        operation.phase,
        Phase::Read | Phase::Post(ToolResult::Succeeded) | Phase::ShellPost
    );
    if operation.mid_turn_context
        && completed
        && reply.stdout.is_empty()
        && !matches!(
            reply.coverage,
            "timeout" | "unavailable" | "unknown" | "unsupported"
        )
        && let Some(context) = session.news(&ids)
    {
        reply.stdout = json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": context,
            }
        })
        .to_string();
        reply.detail = format!("{}; briefing delivered mid-turn", reply.detail);
    }
    // Whatever this reply places in the model's context is counted: a
    // mid-turn briefing, an alert at a write, a formatter's notice (review
    // comment 4711488164). One record per hook run and phase.
    session.record_context(
        wire_id(
            "context-tool",
            &[
                &identity.source,
                &ids.operation_id,
                &format!("{:?}", operation.phase),
            ],
        ),
        &ids.session_id,
        reply_context_bytes(&reply.stdout),
        false,
    );
    // Process evidence is advisory. Record it only after the coordination
    // lifecycle, so a rejected attachment cannot suppress the tool result.
    if !matches!(
        reply.coverage,
        "timeout" | "unavailable" | "unknown" | "unsupported"
    ) {
        session.report_liveness(&ids);
    }
    reply
}
/// The request ID of the report a spool entry stands for: the one its first
/// call to record the observation carries, so a resend is the same request.
fn report_request_id(source: &str, ids: &Identities, report: &crate::spool::Report) -> String {
    match report {
        crate::spool::Report::Post { result, .. } => post_request_id(source, ids, *result),
        crate::spool::Report::Read { .. } => {
            wire_id("source-read", &[source, &ids.session_id, &ids.operation_id])
        }
        crate::spool::Report::Shell => wire_id(
            "observe-shell",
            &[source, &ids.session_id, &ids.operation_id],
        ),
    }
}

pub(super) fn post_request_id(source: &str, ids: &Identities, result: ToolResult) -> String {
    let phase = match result {
        ToolResult::Succeeded => "post_tool_use",
        _ => "post_tool_use_failure",
    };
    wire_id(
        &format!("observe-{phase}"),
        &[source, &ids.session_id, &ids.operation_id],
    )
}

/// Send what earlier hook runs could not, before this run's own report
/// ([ADR 0014](../docs/adr/0014-observation-spool.md)). Only when the daemon
/// accepts a connection (the client's nonblocking connect, bounded like the
/// rest), so a daemon that is down or not accepting spends no tries,
/// and within half of what is left of this run's budget. `live` is this run's
/// own report: the same report kept earlier, and older ones of the same
/// files, are owed no longer.
fn resend_owed(
    config: &Config,
    deadline: Instant,
    live: &crate::spool::Live<'_>,
) -> Option<String> {
    #[cfg(unix)]
    {
        let now = Instant::now();
        let until = now + deadline.saturating_duration_since(now) / 2;
        if !crate::spool::directory(&config.state_dir).is_dir()
            || !crate::client::accepts(&config.state_dir, until)
        {
            return None;
        }
        crate::spool::resend(&config.state_dir, live, |entry| {
            resend(config, until, entry)
        })
        .describe()
    }
    #[cfg(not(unix))]
    {
        let _ = (config, deadline, live);
        None
    }
}

/// Make the report an entry stands for, as the run that kept it would have,
/// with the versions it read then. Every call has the request ID that run
/// used, so whatever the daemon already recorded is answered from its
/// receipt. Against a restarted daemon the session's registration is answered
/// from its receipt too, so the session stays from the earlier generation:
/// the observation is recorded as history (its admission ended with the
/// restart), and neither a written version nor a completion is recorded.
fn resend(config: &Config, until: Instant, entry: &crate::spool::Entry) -> crate::spool::Sent {
    let config = Config {
        workspace_id: entry.workspace_id.clone(),
        root: entry.root.clone(),
        presence: entry.presence.shown,
        presence_window_ms: entry.presence.window_ms,
        ..config.clone()
    };
    let ids = identities_for(&entry.source, &entry.host_session, &entry.tool_use_id);
    let identity = langspan_core::ToolIdentity {
        operation_id: ids.operation_id.clone(),
        source: entry.source.clone(),
        tool: entry.tool.clone(),
        input_digest: entry.input_digest.clone(),
    };
    let mut session = Session {
        config: &config,
        deadline: until,
        root: &config.root,
        source: &entry.source,
        parent: entry.parent.as_deref(),
        headless: headless_host(&entry.source),
        unreachable: Default::default(),
        reached: Default::default(),
    };
    match &entry.report {
        crate::spool::Report::Post {
            result,
            paths,
            files,
        } => {
            // A report sent again is late: no break alert is due with it.
            session.observe(&ids, &identity, paths.clone(), *result, files.clone());
        }
        crate::spool::Report::Read { files } => {
            session.read(&ids, files.clone());
        }
        crate::spool::Report::Shell => {
            session.shell_post(&ids, &identity);
        }
    }
    if session.unreachable.get() {
        crate::spool::Sent::Unreachable
    } else {
        crate::spool::Sent::Answered
    }
}

/// Stable map ordering; only the digest enters storage, never edit text.
pub fn input_fingerprint(tool: &str, input: &Value) -> String {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|key| (key.clone(), canonical(&map[key])))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            value => value.clone(),
        }
    }
    let bytes = serde_json::to_vec(&json!(["langspan-tool-input-v1", tool, canonical(input)]))
        .unwrap_or_default();
    sha256::hex(&sha256::digest(&bytes))
}
