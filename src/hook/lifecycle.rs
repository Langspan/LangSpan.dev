use super::*;

/// The context the hooks may add to a typical session per turn, on average
/// over its turns (issue #222): about 64 tokens. `langspan status` reports
/// what a session actually received; a test holds a typical session to it.
pub const CONTEXT_BUDGET_BYTES_PER_TURN: u64 = 256;
const BRIEF_PEERS: u32 = 6;

#[derive(serde::Deserialize)]
struct LifecycleEvent {
    hook_event_name: String,
    session_id: Option<String>,
    agent_id: Option<String>,
    cwd: Option<String>,
    prompt: Option<String>,
    /// Claude Code numbers prompts; Codex numbers turns. Either makes a
    /// redelivered prompt event the same request.
    prompt_id: Option<String>,
    turn_id: Option<String>,
    /// StopFailure's error type. Other fields of that event, including the
    /// rendered error text, are never read.
    error: Option<Value>,
    /// PostToolBatch's calls. Only each call's `tool_use_id` is read.
    tool_calls: Option<Value>,
}

/// The end of a reply or of a tool batch: bookkeeping, never a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TurnKind {
    /// Stop, or StopFailure with the host's error type.
    Stop { failure: Option<String> },
    /// The subagent finished: its reply ends, and so does its session.
    SubagentStop,
    /// PostToolBatch, with the tool-call IDs it names, in order.
    Batch { calls: Vec<String>, total: usize },
}

/// Claude Code's documented StopFailure error types are lower-case words
/// joined by underscores. Anything else is recorded as `unknown`.
fn failure_type(error: Option<&Value>) -> String {
    match error.and_then(Value::as_str) {
        Some(kind)
            if (1..=40).contains(&kind.len())
                && kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
        {
            kind.to_owned()
        }
        _ => "unknown".into(),
    }
}

/// Handle a session lifecycle event, or return `None` for any other event so
/// the vendor adapter parses it as a tool call.
pub fn lifecycle_event(
    body: &[u8],
    source: &str,
    config: &Config,
    deadline: Instant,
) -> Option<Reply> {
    let event: LifecycleEvent = serde_json::from_slice(body).ok()?;
    let kind = match event.hook_event_name.as_str() {
        "SessionStart" => LifecycleKind::Start,
        "UserPromptSubmit" => LifecycleKind::Prompt {
            prompt: event.prompt.clone(),
        },
        "SessionEnd" => LifecycleKind::End,
        "Stop" | "StopFailure" | "SubagentStop" | "PostToolBatch" => {
            return Some(turn_event(event, source, config, deadline));
        }
        _ => return None,
    };
    if event.agent_id.is_some() {
        return Some(Reply::none(
            "unsupported",
            "subagent lifecycle events are not covered",
        ));
    }
    let Some(host_session) = event.session_id.filter(|id| valid_host_id(id)) else {
        return Some(Reply::none(
            "unsupported",
            "lifecycle event lacks a usable session identity",
        ));
    };
    let config = match resolve_workspace(config, event.cwd.as_deref(), deadline) {
        Ok(config) => config,
        Err(reply) => return Some(reply),
    };
    let event_id = prompt_event_id(event.prompt_id.or(event.turn_id).as_deref())
        .filter(|_| matches!(kind, LifecycleKind::Prompt { .. }))
        .map(str::to_owned);
    Some(lifecycle(
        source,
        &host_session,
        kind,
        (&event.hook_event_name, event_id.as_deref()),
        &config,
        deadline,
        BRIEFING_LIMIT,
    ))
}

/// What a submitted prompt contributes, within the settings of the workspace
/// it resolved to: its summary, if shared, and the names to look up, if code
/// briefing is on.
fn prompt_parts(prompt: Option<&str>, config: &Config) -> (Option<PromptLines>, CodeAsk) {
    let code = prompt.filter(|_| config.code_briefing);
    (
        prompt
            .filter(|_| config.share_prompt)
            .map(PromptLines::new)
            .filter(|lines| lines.summary().is_some()),
        CodeAsk {
            names: code.map(prompt_identifiers).unwrap_or_default(),
            qualifiers: code.map(prompt_qualifiers).unwrap_or_default(),
        },
    )
}

/// What a prompt asks the source index about: its candidate names and the
/// paths and qualifiers that rank their definitions.
#[derive(Default)]
struct CodeAsk {
    names: Vec<String>,
    qualifiers: Vec<String>,
}

/// Record a prompt's summary and line digests. A prompt without a task line
/// whose leading lines a live peer's prompt in the repository begins with
/// too, as when several sessions start from one template, is recorded again
/// under its first line that differs, so that briefings tell them apart.
fn record_prompt(
    step: &impl Fn(&str, Command) -> Result<Response, Failure>,
    session_id: &str,
    lines: &PromptLines,
) -> Result<(), Failure> {
    let Some(summary) = lines.summary() else {
        return Ok(());
    };
    let line_digests = lines.digests();
    let command = |summary: String| Command::SessionPrompt {
        session_id: session_id.to_owned(),
        summary,
        line_digests: line_digests.clone(),
    };
    let response = step("prompt", command(summary.clone()))?;
    let Outcome::SessionPrompted { shared_lines, .. } = response.outcome else {
        return Ok(());
    };
    if lines.task_line().is_some() || shared_lines == 0 {
        return Ok(());
    }
    match lines.0.get(shared_lines as usize) {
        Some(differs) if *differs != summary => {
            step("prompt-differs", command(differs.clone())).map(drop)
        }
        _ => Ok(()),
    }
}

/// A lifecycle event a vendor adapter has already parsed, for a host whose
/// envelope differs from the one `lifecycle_event` reads. `event_id` makes a
/// redelivered prompt the same request. The reply's context is in the shape
/// `lifecycle_event` writes; the adapter translates it.
pub fn lifecycle_parsed(
    source: &str,
    host_session: &str,
    kind: LifecycleKind,
    (event_name, event_id): (&str, Option<&str>),
    cwd: Option<&str>,
    config: &Config,
    deadline: Instant,
) -> Reply {
    if !valid_host_id(host_session) {
        return no_session_identity();
    }
    match resolve_workspace(config, cwd, deadline) {
        Ok(config) => lifecycle_resolved(
            source,
            host_session,
            kind,
            (event_name, event_id),
            &config,
            deadline,
            BRIEFING_LIMIT,
        ),
        Err(reply) => reply,
    }
}

/// `lifecycle_parsed` for a workspace the adapter has already resolved
/// (`resolve_workspace`), for a host whose session identity depends on it.
/// `room` is what is left of the briefing limit for this workspace's
/// context when one reply joins several (at most `BRIEFING_LIMIT`). With
/// less than the whole limit, the briefing is first looked at without
/// delivery and delivered only if it fits `room`; otherwise nothing is
/// delivered, and its messages and check results wait for the next event.
pub fn lifecycle_resolved(
    source: &str,
    host_session: &str,
    kind: LifecycleKind,
    (event_name, event_id): (&str, Option<&str>),
    config: &Config,
    deadline: Instant,
    room: usize,
) -> Reply {
    if !valid_host_id(host_session) {
        return no_session_identity();
    }
    let event_id =
        prompt_event_id(event_id).filter(|_| matches!(kind, LifecycleKind::Prompt { .. }));
    lifecycle(
        source,
        host_session,
        kind,
        (event_name, event_id),
        config,
        deadline,
        room.min(BRIEFING_LIMIT),
    )
}

fn no_session_identity() -> Reply {
    Reply::none(
        "unsupported",
        "lifecycle event lacks a usable session identity",
    )
}

fn lifecycle(
    source: &str,
    host_session: &str,
    kind: LifecycleKind,
    (event_name, event_id): (&str, Option<&str>),
    config: &Config,
    deadline: Instant,
    room: usize,
) -> Reply {
    if Instant::now() >= deadline {
        return Reply::none("timeout", "hook deadline elapsed before coordination");
    }
    // A prompt the host numbered is one request however often it is
    // delivered. Other lifecycle events carry no identity, so each delivery
    // is a new request; repeating one only repeats a registration.
    let nonce = match event_id {
        Some(id) => format!("prompt-{id}"),
        None => format!(
            "life-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ),
    };
    let ids = identities_for(source, host_session, &nonce);
    let starting = kind == LifecycleKind::Start;
    // An ended session runs nothing: its shell runs whose result never came
    // are closed, whatever the coordinator answers.
    let closed = if kind == LifecycleKind::End {
        closed_runs(&config.state_dir, &ids.session_id, "the session ended")
    } else {
        String::new()
    };
    let session = Session {
        config,
        deadline,
        root: &config.root,
        source,
        parent: None,
        headless: headless_host(source),
        unreachable: Default::default(),
        reached: Default::default(),
    };
    let failed = |failure| {
        let mut reply = failure_reply(failure);
        reply.detail.push_str(&closed);
        reply
    };
    if let Err(failure) = session.attach(&ids) {
        return failed(failure);
    }
    // The default is sent here, at each session event, so that a session
    // whose hook turned presence back on is shown again.
    if session.default_presence()
        && let Err(failure) = session.set_presence(&ids)
    {
        return failed(failure);
    }
    session.report_liveness(&ids);
    let step = |kind: &str, command: Command| {
        session.call(wire_id(kind, &[source, &ids.operation_id]), command)
    };
    let (ask, prompt_recorded) = match kind {
        LifecycleKind::End => {
            // The trail's line ranges and uncommitted files (roadmap C9),
            // read within half the time left; past it, what was read goes
            // with the end, marked partial.
            let now = Instant::now();
            let until = now + deadline.saturating_duration_since(now) / 2;
            let trail = crate::trail::facts(
                &config.state_dir,
                &config.root,
                &ids.session_id,
                config.share_prompt,
                until,
            );
            return match step(
                "session-end",
                Command::SessionEnd {
                    session_id: ids.session_id.clone(),
                    trail: Some(trail),
                },
            ) {
                Ok(_) => Reply::none("session_ended", format!("session marked ended{closed}")),
                Err(failure) => failed(failure),
            };
        }
        LifecycleKind::Prompt { prompt } => {
            let (lines, ask) = prompt_parts(prompt.as_deref(), config);
            let recorded = lines.is_some();
            if let Some(lines) = lines
                && let Err(failure) = record_prompt(&step, &ids.session_id, &lines)
            {
                return failure_reply(failure);
            }
            (ask, recorded)
        }
        LifecycleKind::Start => (CodeAsk::default(), false),
    };
    // At a start after the workspace was away (roadmap C10): what happened
    // since its last interactive session, first, within the same room. A
    // reply joining several workspaces may leave this one too little room
    // for it: it is cut to fit, or left out.
    let mut away_note = String::new();
    let since_section = starting
        .then(|| since_section(&step, &config.workspace_id, room, &mut away_note))
        .flatten();
    let room = since_section
        .as_ref()
        .map_or(room, |text| room.saturating_sub(text.len() + 2));
    // The code section is looked up before the briefing, so that the files
    // this prompt points at are compared with other sessions' in it.
    let code = session.code_context(&ids, ask);
    // Prompt targets (roadmap C8) go with a recorded prompt: only with the
    // prompt summary and the code briefing on, and only from an index with
    // a complete scan. Paths only; a failure to record them is noted and
    // leaves the briefing as it would have been.
    let mut targets_note = String::new();
    if prompt_recorded
        && let Some((matches, coverage)) = code.as_ref()
        && coverage.scanned
    {
        let paths = prompt_target_paths(matches);
        if !paths.is_empty()
            && let Err(failure) = step(
                "prompt-targets",
                Command::SetPromptTargets {
                    session_id: ids.session_id.clone(),
                    paths,
                },
            )
        {
            targets_note = format!(
                "; prompt targets not recorded: {}",
                failure_reply(failure).detail
            );
        }
    }
    // At a start, the sessions whose journaled lines are still uncommitted
    // here: the coordinator names the ended ones nobody adopted (issue
    // #161). Half the time left at most, so the briefing still has its own.
    let left = if starting {
        let now = Instant::now();
        let until = now + deadline.saturating_duration_since(now) / 2;
        crate::adopt::left_sessions(&config.state_dir, &config.root, &ids.session_id, until)
    } else {
        Vec::new()
    };
    let code_part = || {
        code.as_ref()
            .map(|(matches, coverage)| (matches.as_slice(), coverage))
    };
    let brief = |kind: &str, deliver: bool| match step(
        kind,
        Command::Brief {
            session_id: ids.session_id.clone(),
            max_peers: BRIEF_PEERS,
            deliver,
            mid_turn: false,
            left: left.clone(),
            // What changed recently goes only into a starting context.
            recent: starting,
            recent_window_ms: None,
            mcp: false,
            // A new or resumed context gets the full view; a prompt in a
            // continuing one only what changed since it was last told.
            changes_only: !starting,
            // Told only once this hook confirms it placed it (issue #531).
            confirm: deliver,
        },
    ) {
        Ok(Response {
            outcome: Outcome::Briefing { briefing },
            ..
        }) => Ok(briefing),
        Ok(other) => Err(Reply::none(
            "unknown",
            format!("unexpected outcome {other:?}"),
        )),
        Err(failure) => Err(failure_reply(failure)),
    };
    // The work ended sessions left uncommitted is confirmed against the line
    // journal within a quarter of the budget; past it, the recent section
    // is left out and the rest of the briefing is sent. A new or resumed
    // context has lost any earlier briefing; a prompt in a continuing
    // context gets one only when the situation changed.
    let confirmed = |mut briefing: langspan_core::Briefing, note: &mut String| {
        if let Some(recent) = briefing.recent.as_mut() {
            let share = (Instant::now() + config.budget / 4).min(deadline);
            if let Err(reason) =
                crate::recent::confirm_left(&config.state_dir, &ids.session_id, recent, share)
            {
                briefing.recent = None;
                note.push_str(&format!("; recent changes left out: {reason}"));
            }
        }
        (has_content(&briefing) && (starting || briefing.changed)).then_some(briefing)
    };
    // Delivering a briefing acknowledges its messages and check results, so
    // with less than the whole limit left (a reply joining several
    // workspaces) it is first looked at without delivery, and delivered
    // only if it fits the room, every in-flight line included. Otherwise it
    // waits for the next event. A delivered briefing that has grown since its preview is shown whole.
    let preview = if room >= BRIEFING_LIMIT {
        None
    } else {
        match brief("brief-preview", false) {
            // The preview only sizes the reply; a note about it is not kept.
            Ok(preview) => Some(confirmed(preview, &mut String::new())),
            Err(reply) => return reply,
        }
    };
    // What the core recorded as told: what the text did not place is
    // forgotten below (review comment 4711375989).
    let mut delivered = None;
    let briefing = match preview {
        Some(None) => None,
        Some(Some(preview))
            if compose_context_within(Some(&preview), code_part(), room)
                .is_none_or(|text| text.len() > room || !shows_all_inflight(&preview, &text)) =>
        {
            targets_note.push_str("; peer briefing deferred: no room left in this reply");
            None
        }
        _ => match brief("brief", true) {
            Ok(briefing) => {
                delivered = Some(briefing.clone());
                confirmed(briefing, &mut targets_note)
            }
            Err(reply) => return reply,
        },
    };
    let deliver = briefing.is_some();
    // The code section is about this prompt, so it is sent whether or not
    // the peer briefing changed. It is never remembered as delivered.
    let (context, placed) = match compose_context_placed(briefing.as_ref(), code_part(), room) {
        Some((text, placed)) => (Some(text), placed),
        None => (None, None),
    };
    targets_note.push_str(&away_note);
    let told_since = since_section.is_some();
    let context = match (since_section, context) {
        (Some(since), Some(rest)) => Some(format!("{since}\n\n{rest}")),
        (since, rest) => since.or(rest),
    };
    if let Some(delivered) = &delivered {
        session.forget_unplaced(
            wire_id("forget", &[source, &ids.operation_id]),
            &ids.session_id,
            delivered,
            placed.as_ref().filter(|_| deliver),
        );
    }
    // Every turn is counted, one that adds nothing too (issue #222).
    session.record_context(
        wire_id("context", &[source, &ids.operation_id]),
        &ids.session_id,
        context.as_ref().map_or(0, String::len),
        true,
    );
    let Some(context) = context else {
        return Reply::none(
            "no_briefing",
            format!("no new relevant peer work{targets_note}"),
        );
    };
    if let Some(briefing) = &briefing {
        targets_note.push_str(&report_shown(&step, briefing, &context));
    }
    // Last before the reply: a hook that timed out before this leaves what
    // it would have shown untold (issue #531).
    if let Some(placed) = placed.as_ref().filter(|_| deliver) {
        session.confirm_placed(
            wire_id("confirm", &[source, &ids.operation_id]),
            &ids.session_id,
            placed,
        );
    }
    let stdout = json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": context,
        }
    })
    .to_string();
    let matched = code.map_or(0, |(matches, _)| matches.len());
    Reply {
        stdout,
        coverage: if deliver || told_since {
            "briefed"
        } else {
            "code_briefed"
        },
        detail: format!(
            "{} peer session(s); {matched} prompt name(s) found in the source index{targets_note}",
            briefing.as_ref().map_or(0, |b| b.peers.len())
        ),
    }
}

/// The "since the last session here" section for a starting session
/// (roadmap C10): only while the workspace has been away, with no
/// interactive session at work since, and only when something happened.
/// A failure to ask is noted and leaves the briefing as it would have been.
/// The section and the separator after it fit `room`; with too little room
/// for even its shortest form, it is left out and the note says so.
fn since_section(
    step: &dyn Fn(&str, Command) -> Result<Response, Failure>,
    workspace_id: &str,
    room: usize,
    note: &mut String,
) -> Option<String> {
    let after_ms = match crate::since::away_after_ms() {
        Ok(after_ms) => after_ms,
        Err(message) => {
            note.push_str(&format!("; since-away section left out: {message}"));
            return None;
        }
    };
    let asked = Command::Recap {
        workspace_id: workspace_id.into(),
        tag: None,
        since: Some(langspan_core::SinceFrom::Away { after_ms }),
    };
    let since = match step("since-away", asked) {
        Ok(Response {
            outcome: Outcome::Recap { recap },
            ..
        }) => recap.since?,
        Ok(_) => {
            note.push_str(
                "; since-away section left out: the daemon answered with another outcome",
            );
            return None;
        }
        Err(failure) => {
            note.push_str(&format!(
                "; since-away section left out: {}",
                failure_reply(failure).detail
            ));
            return None;
        }
    };
    if !since.ongoing {
        return None;
    }
    let section = crate::since::section(&since, workspace_id, room.saturating_sub(2));
    if section.is_none()
        && crate::since::section(&since, workspace_id, crate::since::SECTION_BYTES).is_some()
    {
        note.push_str(&format!(
            "; since-away section deferred: no room left in this reply; {}",
            crate::since::full_command(workspace_id)
        ));
    }
    section
}

/// The LangSpan host identity an event belongs to, and the parent's
/// LangSpan session ID for a subagent. A Claude Code subagent's events carry
/// the parent's `session_id` and the subagent's own `agent_id`; each
/// subagent is a session of its own. `None` when the agent ID cannot make one.
pub fn own_session(
    source: &str,
    host_session: &str,
    agent: Option<&str>,
) -> Option<(String, Option<String>)> {
    match agent {
        None => Some((host_session.to_owned(), None)),
        Some(agent) => subagent_session(host_session, agent)
            .map(|own| (own, Some(format!("{source}.{host_session}")))),
    }
}

/// Close `session_id`'s shell runs that got no result, for a hook's detail:
/// empty when there were none.
fn closed_runs(state_dir: &Path, session_id: &str, why: &str) -> String {
    let edits = match crate::shell::close_edits(state_dir, session_id) {
        0 => String::new(),
        closed => format!("; {closed} edit call(s) that got no result closed"),
    };
    match crate::shell::close_session(state_dir, session_id, why) {
        0 => edits,
        closed => format!("; {closed} shell run(s) that got no result closed{edits}"),
    }
}

/// Stop, StopFailure, SubagentStop and PostToolBatch
/// ([ADR 0027](../docs/adr/0027-turn-and-batch-events.md)). Nothing is ever
/// printed for a stop: on Claude Code, output from a Stop or SubagentStop
/// hook keeps the agent going, and StopFailure output is ignored.
fn turn_event(event: LifecycleEvent, source: &str, config: &Config, deadline: Instant) -> Reply {
    let codex = source == crate::codex::SOURCE;
    // Codex gives a subagent's events the parent's session ID, so a Codex
    // event that names an agent is never taken for the parent's.
    if codex && event.agent_id.is_some() {
        return Reply::none(
            "unsupported",
            "Codex subagent identity is not covered by this adapter",
        );
    }
    let kind = match event.hook_event_name.as_str() {
        "Stop" => TurnKind::Stop { failure: None },
        "StopFailure" if !codex => TurnKind::Stop {
            failure: Some(failure_type(event.error.as_ref())),
        },
        "SubagentStop" if !codex => TurnKind::SubagentStop,
        "PostToolBatch" if !codex => {
            let Some(calls) = event.tool_calls.as_ref().and_then(Value::as_array) else {
                return Reply::none("unsupported", "PostToolBatch event has no tool_calls");
            };
            TurnKind::Batch {
                total: calls.len(),
                calls: calls
                    .iter()
                    .filter_map(|call| call.get("tool_use_id")?.as_str())
                    .filter(|id| valid_host_id(id))
                    .fold(Vec::new(), |mut seen: Vec<String>, id| {
                        if !seen.iter().any(|other| other == id) {
                            seen.push(id.to_owned());
                        }
                        seen
                    }),
            }
        }
        _ => return Reply::none("unsupported", "this host does not send that event"),
    };
    if kind == TurnKind::SubagentStop && event.agent_id.is_none() {
        return Reply::none("unsupported", "SubagentStop event names no agent");
    }
    let Some(host_session) = event.session_id.filter(|id| valid_host_id(id)) else {
        return Reply::none(
            "unsupported",
            "lifecycle event lacks a usable session identity",
        );
    };
    let Some((own, parent)) = own_session(source, &host_session, event.agent_id.as_deref()) else {
        return Reply::none(
            "unsupported",
            "subagent identity is outside supported grammar",
        );
    };
    let config = match resolve_workspace(config, event.cwd.as_deref(), deadline) {
        Ok(config) => config,
        Err(reply) => return reply,
    };
    if Instant::now() >= deadline {
        return Reply::none("timeout", "hook deadline elapsed before coordination");
    }
    let session = Session {
        config: &config,
        deadline,
        root: &config.root,
        source,
        parent: parent.as_deref(),
        headless: headless_host(source),
        unreachable: Default::default(),
        reached: Default::default(),
    };
    // The prompt whose reply finished, which ends a between-step watcher
    // started for it (issue #214). A subagent's stop ends no prompt.
    let prompt = prompt_event_id(event.prompt_id.or(event.turn_id).as_deref()).map(str::to_owned);
    match kind {
        TurnKind::Batch { calls, total } => session.batch(&own, &calls, total),
        TurnKind::Stop { failure } => session.stop(&own, failure, prompt, false),
        TurnKind::SubagentStop => session.stop(&own, None, None, true),
    }
}

impl Session<'_> {
    /// End the reply: complete the tool admissions still open, and for a
    /// subagent end its session. A stop carries no identity of its own and
    /// may come more than once for one prompt, so each is a new request;
    /// repeating one releases nothing more. A session the coordinator never
    /// saw is not registered just to be stopped.
    fn stop(
        &self,
        own: &str,
        failure: Option<String>,
        prompt_id: Option<String>,
        end_session: bool,
    ) -> Reply {
        let nonce = format!(
            "stop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let ids = identities_for(self.source, own, &nonce);
        // No call of an ended reply is still running: a shell run whose
        // result never came (the host refused it) is closed here, whatever
        // the coordinator answers.
        let closed = closed_runs(&self.config.state_dir, &ids.session_id, "the reply ended");
        let stopped = self.call(
            wire_id("session-stop", &[self.source, &ids.operation_id]),
            Command::SessionStop {
                session_id: ids.session_id.clone(),
                failure: failure.clone(),
                prompt_id,
            },
        );
        let released = match stopped {
            Ok(Response {
                outcome: Outcome::SessionStopped { released, .. },
                ..
            }) => released.len(),
            Ok(other) => {
                return Reply::none("unknown", format!("unexpected outcome {other:?}"));
            }
            Err(Failure::Rejected(message)) if message.starts_with("session_not_found") => {
                return Reply::none(
                    "no_session",
                    format!(
                        "the coordinator has no record of this session; nothing to release{closed}"
                    ),
                );
            }
            Err(failure) => {
                let mut reply = failure_reply(failure);
                reply.detail.push_str(&closed);
                return reply;
            }
        };
        let mut detail = format!("reply ended; {released} open admission(s) released{closed}");
        if let Some(failure) = failure {
            detail.push_str(&format!("; the host reported error type {failure}"));
        }
        if !end_session {
            return Reply::none("stopped", detail);
        }
        match self.call(
            wire_id("session-end", &[self.source, &ids.operation_id]),
            // A subagent leaves no trail of its own: its parent's has its
            // work (roadmap C9).
            Command::SessionEnd {
                session_id: ids.session_id.clone(),
                trail: None,
            },
        ) {
            Ok(_) => Reply::none(
                "subagent_ended",
                format!("{detail}; subagent session ended"),
            ),
            Err(failure) => {
                let mut reply = failure_reply(failure);
                reply.detail = format!("{detail}; ending the subagent session: {}", reply.detail);
                reply
            }
        }
    }

    /// Record the end of a tool batch, then deliver what changed around the
    /// session since it was last told, if anything did. The request ID comes
    /// from the batch's tool calls, so a redelivered batch is one record.
    fn batch(&self, own: &str, calls: &[String], total: usize) -> Reply {
        if calls.is_empty() {
            return Reply::none(
                "unsupported",
                "PostToolBatch names no tool call with a usable identity",
            );
        }
        let named = &calls[..calls.len().min(langspan_core::MAX_BATCH_OPERATIONS)];
        let key = named.join("\n");
        let ids = identities_for(self.source, own, &wire_id("batch", &[&key]));
        if let Err(failure) = self.attach(&ids) {
            return failure_reply(failure);
        }
        let operation_ids: Vec<String> = named
            .iter()
            .map(|call| identities_for(self.source, own, call).operation_id)
            .collect();
        if let Err(failure) = self.call(
            wire_id("tool-batch", &[self.source, &ids.session_id, &key]),
            Command::ObserveToolBatch {
                session_id: ids.session_id.clone(),
                operation_ids,
                calls: u32::try_from(total).unwrap_or(u32::MAX),
            },
        ) {
            return failure_reply(failure);
        }
        let detail = format!("batch of {total} tool call(s) recorded");
        let news = self.news(&ids);
        self.record_context(
            wire_id("context-batch", &[self.source, &ids.session_id, &key]),
            &ids.session_id,
            news.as_ref().map_or(0, String::len),
            false,
        );
        match news {
            Some(context) => Reply {
                stdout: json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolBatch",
                        "additionalContext": context,
                    }
                })
                .to_string(),
                coverage: "batch_briefed",
                detail: format!("{detail}; briefing delivered mid-turn"),
            },
            None => Reply::none("batch_recorded", detail),
        }
    }
}

/// Tell the coordinator which interface notices of `briefing` the context
/// `text` placed showed, so a between-step watcher does not tell them
/// again (issue #214). Returns a note for the hook's diagnostic when that
/// fails; the briefing is delivered either way, and the cost of a failure
/// is one notice told twice.
fn report_shown(
    step: &impl Fn(&str, Command) -> Result<Response, Failure>,
    briefing: &langspan_core::Briefing,
    text: &str,
) -> String {
    let notice_ids = shown_notice_ids(briefing, text);
    if notice_ids.is_empty() {
        return String::new();
    }
    match step(
        "notices-shown",
        Command::InterfaceShown {
            session_id: briefing.session_id.clone(),
            notice_ids,
        },
    ) {
        Ok(_) => String::new(),
        Err(failure) => format!(
            "; shown interface notices not recorded: {}",
            failure_reply(failure).detail
        ),
    }
}

impl Session<'_> {
    /// The briefing, if it differs from the last one delivered to this
    /// session. Any failure is no news.
    pub(super) fn news(&self, ids: &Identities) -> Option<String> {
        if Instant::now() >= self.deadline {
            return None;
        }
        let response = self
            .call(
                wire_id("brief-mid-turn", &[self.source, &ids.operation_id]),
                Command::Brief {
                    session_id: ids.session_id.clone(),
                    max_peers: BRIEF_PEERS,
                    deliver: true,
                    mid_turn: true,
                    left: Vec::new(),
                    recent: false,
                    recent_window_ms: None,
                    mcp: false,
                    changes_only: true,
                    confirm: true,
                },
            )
            .ok()?;
        let Outcome::Briefing { briefing } = response.outcome else {
            return None;
        };
        let shown = briefing.changed && has_content(&briefing);
        let (text, placed) = if shown {
            let (text, placed) = render_briefing_placed(&briefing, BRIEFING_LIMIT);
            (Some(text), Some(placed))
        } else {
            (None, None)
        };
        // What the text did not place was recorded as told: it is not.
        self.forget_unplaced(
            wire_id("forget-mid-turn", &[self.source, &ids.operation_id]),
            &ids.session_id,
            &briefing,
            placed.as_ref(),
        );
        if let Some(text) = &text {
            // A failure costs at most a notice told twice (issue #214).
            let _ = report_shown(
                &|kind: &str, command| {
                    self.call(wire_id(kind, &[self.source, &ids.operation_id]), command)
                },
                &briefing,
                text,
            );
        }
        if let Some(placed) = &placed {
            self.confirm_placed(
                wire_id("confirm-mid-turn", &[self.source, &ids.operation_id]),
                &ids.session_id,
                placed,
            );
        }
        // Its caller counts the reply that carries it, with anything else
        // the reply adds.
        text
    }

    /// Have the core record as told what `placed`, this hook's text, shows
    /// (issue #531). Within the deadline; a failure, or a daemon of an
    /// earlier build, leaves the items untold, so they are told again.
    fn confirm_placed(
        &self,
        request_id: String,
        session_id: &str,
        placed: &langspan_core::Briefing,
    ) {
        let mut keys = langspan_core::briefing_keys(placed);
        keys.truncate(langspan_core::MAX_TOLD);
        if keys.is_empty() || Instant::now() >= self.deadline {
            return;
        }
        let _ = self.call(
            request_id,
            Command::ConfirmTold {
                session_id: session_id.to_owned(),
                keys,
            },
        );
    }

    /// Have the core forget the items of `delivered` that `placed`, what
    /// this hook's text showed in full, does not hold (review comment
    /// 4711375989), so the next briefing tells them. Within the deadline;
    /// a failure leaves them told, as before this record existed.
    fn forget_unplaced(
        &self,
        request_id: String,
        session_id: &str,
        delivered: &langspan_core::Briefing,
        placed: Option<&langspan_core::Briefing>,
    ) {
        let keys = unplaced_keys(delivered, placed);
        if keys.is_empty() || Instant::now() >= self.deadline {
            return;
        }
        let _ = self.call(
            request_id,
            Command::ForgetTold {
                session_id: session_id.to_owned(),
                keys,
            },
        );
    }

    /// Record that `bytes` of text went into the session's context, at a
    /// turn's start when `turn` (issue #222). A measurement only: within
    /// the hook's deadline, and any failure is left unsaid.
    pub(super) fn record_context(
        &self,
        request_id: String,
        session_id: &str,
        bytes: usize,
        turn: bool,
    ) {
        if (bytes == 0 && !turn) || Instant::now() >= self.deadline {
            return;
        }
        let _ = self.call(
            request_id,
            Command::RecordContext {
                session_id: session_id.to_owned(),
                bytes: u64::try_from(bytes).unwrap_or(u64::MAX),
                turn,
            },
        );
    }

    /// Definitions of names from the owner's prompt, from the source index.
    /// A read-only query that the daemon never records. Any failure is
    /// nothing to say.
    fn code_context(
        &self,
        ids: &Identities,
        ask: CodeAsk,
    ) -> Option<(
        Vec<langspan_core::NameContext>,
        langspan_core::GraphCoverage,
    )> {
        if ask.names.is_empty() || Instant::now() >= self.deadline {
            return None;
        }
        // Ranked for this session (roadmap C2): its own files first.
        let response = self
            .call(
                wire_id("code-context", &[self.source, &ids.operation_id]),
                Command::GraphContext {
                    workspace_id: self.config.workspace_id.clone(),
                    names: ask.names,
                    limit: langspan_core::CONTEXT_DEFINITIONS,
                    rank: true,
                    session_id: Some(ids.session_id.clone()),
                    qualifiers: ask.qualifiers,
                },
            )
            .ok()?;
        match response.outcome {
            Outcome::GraphContext { matches, coverage } => Some((matches, coverage)),
            _ => None,
        }
    }
}
