//! The commands sent to the coordinator as one request, `request JSON`
//! included: the request each builds, its reply printed, and the exit code.

use crate::flags::{absolute_option, number};
use langspan::cli::ask;
use langspan::cli::options::no_unused_options;
use langspan::cli::render::{print_human, print_project_hook_reruns};
use langspan::{client, protocol::Request};
use langspan_core::{Command, Outcome};
use std::{collections::BTreeMap, path::PathBuf};

/// One coordinator request from the command's words and options, sent, its
/// reply printed, and the exit code it maps to.
pub(crate) fn request(
    state_dir: PathBuf,
    words: Vec<&str>,
    mut options: BTreeMap<String, String>,
    symbols: Vec<String>,
    tags: Vec<String>,
    apply: bool,
    json: bool,
) -> Result<u8, langspan::Error> {
    let mut enable_root: Option<String> = None;
    let request = if let ["request", body] = words.as_slice() {
        no_unused_options(&options)?;
        if body.len() >= langspan::protocol::MAX_FRAME_BYTES {
            return Err("request exceeds 64 KiB".into());
        }
        serde_json::from_str::<Request>(body)?
    } else {
        enable_root = if words.starts_with(&["remote", "enable"]) {
            options.remove("--root")
        } else {
            None
        };
        let command = match words.as_slice() {
            ["workspace", "add", id, root] => Command::WorkspaceRegister {
                workspace_id: (*id).into(),
                root: (*root).into(),
            },
            ["workspace", "list"] => Command::WorkspaceList {
                after: options.remove("--after"),
            },
            ["workspace", "move", id, root] => Command::WorkspaceMove {
                workspace_id: (*id).into(),
                // The daemon runs elsewhere: a relative root is this shell's.
                root: std::path::absolute(root)?.to_string_lossy().into_owned(),
                switch: options.remove("--switch").is_some(),
            },
            ["workspace", "remove", id] => Command::WorkspaceRemove {
                workspace_id: (*id).into(),
                apply,
            },
            ["session", "start", id, workspace] => Command::SessionRegister {
                session_id: (*id).into(),
                workspace_id: (*workspace).into(),
                parent: None,
                headless: false,
            },
            ["session", "attach-pid", session, pid] => Command::ObserveSessionLiveness {
                session_id: (*session).into(),
                process: langspan::process::identify(
                    pid.parse().map_err(|_| "PID must be an unsigned integer")?,
                )?,
                status: langspan_core::LivenessStatus::Alive,
            },
            ["intent", "set", session, paths @ ..] if !paths.is_empty() => Command::IntentSet {
                session_id: (*session).into(),
                paths: paths.iter().map(|p| (*p).into()).collect(),
                summary: options.remove("--summary").unwrap_or_default(),
            },
            ["intent", "end", session] => Command::IntentEnd {
                session_id: (*session).into(),
            },
            ["admit", session, paths @ ..] if !paths.is_empty() => Command::Admit {
                session_id: (*session).into(),
                paths: paths.iter().map(|p| (*p).into()).collect(),
                ttl_ms: options
                    .remove("--ttl-ms")
                    .as_deref()
                    .unwrap_or("5000")
                    .parse()
                    .map_err(|_| "--ttl-ms must be an integer")?,
            },
            ["complete", session, lease] => Command::Complete {
                session_id: (*session).into(),
                lease_id: (*lease).into(),
            },
            ["brief", session] => Command::Brief {
                session_id: (*session).into(),
                max_peers: u32::try_from(number(&mut options, "--max-peers", 8)?)
                    .map_err(|_| "--max-peers is too large")?,
                // A look from the terminal is not a delivery to the session.
                deliver: false,
                mid_turn: false,
                left: Vec::new(),
                recent: options.remove("--recent").is_some(),
                recent_window_ms: None,
                mcp: false,
                // Asked for: the full view (issue #222).
                changes_only: false,
            },
            ["remote", "enable", workspace] => Command::RemoteEnable {
                workspace_id: (*workspace).into(),
                remote: options
                    .remove("--remote")
                    .unwrap_or_else(|| "origin".into()),
                participant: langspan::remote::participant_id(&state_dir)?,
                name: options
                    .remove("--name")
                    .ok_or("--name NAME is required: it is what teammates will see")?,
                share_task: match options.remove("--share-task").as_deref() {
                    None | Some("off") => false,
                    Some("on") => true,
                    Some(_) => return Err("--share-task must be on or off".into()),
                },
                publish_as_branch: options.remove("--publish-as-branch").is_some(),
            },
            ["remote", "disable", workspace] => Command::RemoteDisable {
                workspace_id: (*workspace).into(),
            },
            ["remote", "outbox", workspace] => Command::RemoteOutbox {
                workspace_id: (*workspace).into(),
            },
            ["remote", "sent", workspace] => Command::RemoteSent {
                workspace_id: (*workspace).into(),
                limit: u32::try_from(number(&mut options, "--limit", 10)?)
                    .map_err(|_| "--limit is too large")?,
            },
            [
                "graph",
                query @ ("symbol" | "references" | "impact"),
                workspace,
                subject,
            ] => {
                let workspace_id: String = (*workspace).into();
                let limit = u32::try_from(number(&mut options, "--limit", 50)?)
                    .map_err(|_| "--limit is too large")?;
                match *query {
                    "symbol" => Command::GraphSymbol {
                        workspace_id,
                        name: (*subject).into(),
                        limit,
                        detail: options.remove("--detail").is_some(),
                        blind_in_scope: true,
                        rank: options.remove("--rank").is_some(),
                        session_id: options.remove("--session"),
                        renamed: options.remove("--renamed").is_some(),
                    },
                    "references" => Command::GraphReferences {
                        workspace_id,
                        name: (*subject).into(),
                        limit,
                        blind_in_scope: true,
                    },
                    _ => Command::GraphImpact {
                        workspace_id,
                        path: (*subject).into(),
                        limit,
                        blind_in_scope: true,
                        // The daemon checks the range, so every client is
                        // told the same.
                        depth: u32::try_from(number(&mut options, "--depth", 1)?)
                            .map_err(|_| "--depth must be between 1 and 3")?,
                    },
                }
            }
            ["graph", "inflight", workspace] => Command::GraphInflight {
                workspace_id: (*workspace).into(),
            },
            // The daemon checks both ranges, so every client is told the same.
            ["graph", "path", workspace, from, to] => Command::GraphPath {
                workspace_id: (*workspace).into(),
                from: (*from).into(),
                to: (*to).into(),
                max_hops: u32::try_from(number(
                    &mut options,
                    "--max-hops",
                    langspan_core::DEFAULT_PATH_HOPS.into(),
                )?)
                .map_err(|_| "--max-hops must be between 1 and 6")?,
            },
            ["graph", "map", workspace] => Command::GraphMap {
                workspace_id: (*workspace).into(),
                depth: u32::try_from(number(&mut options, "--depth", 1)?)
                    .map_err(|_| "--depth must be between 1 and 3")?,
            },
            ["graph", "export", workspace] => Command::GraphExport {
                workspace_id: (*workspace).into(),
                path: absolute_option(&mut options, "--out")?,
            },
            ["graph", "import", workspace] => Command::GraphImport {
                workspace_id: (*workspace).into(),
                path: absolute_option(&mut options, "--in")?,
                apply,
            },
            ["graph", "history", workspace, name] => Command::History {
                workspace_id: (*workspace).into(),
                name: (*name).into(),
                path: options.remove("--path"),
                limit: u32::try_from(number(&mut options, "--limit", 10)?)
                    .map_err(|_| "--limit is too large")?,
                before_seq: options
                    .remove("--before")
                    .map(|seq| seq.parse().map_err(|_| "--before must be an integer"))
                    .transpose()?,
            },
            ["check-write", session, paths @ ..] if !paths.is_empty() => Command::CheckWrite {
                session_id: (*session).into(),
                paths: paths.iter().map(|path| (*path).into()).collect(),
                symbols: symbols.clone(),
            },
            ["graph", "blind-spots", workspace] => Command::GraphBlindSpots {
                workspace_id: (*workspace).into(),
                limit: u32::try_from(number(&mut options, "--limit", 50)?)
                    .map_err(|_| "--limit is too large")?,
            },
            ["recap", workspace] => Command::Recap {
                workspace_id: (*workspace).into(),
                tag: tags.first().cloned(),
                since: langspan::since::asked(
                    options
                        .remove("--since-seq")
                        .map(|seq| {
                            seq.parse()
                                .map_err(|_| "--since-seq must be a whole event sequence number, as `langspan events` lists them")
                        })
                        .transpose()?,
                    options.remove("--since-away").is_some(),
                )?,
            },
            ["trail", workspace] => Command::Trails {
                workspace_id: (*workspace).into(),
                limit: Some(
                    u32::try_from(number(&mut options, "--limit", 4)?)
                        .map_err(|_| "--limit is too large")?,
                ),
            },
            // The words may come quoted as one argument or as several.
            ["recall", workspace, query @ ..] if !query.is_empty() => Command::Recall {
                workspace_id: (*workspace).into(),
                query: query.join(" "),
                path: options.remove("--path"),
                limit: u32::try_from(number(&mut options, "--limit", 10)?)
                    .map_err(|_| "--limit is too large")?,
            },
            ["today", workspace @ ..] if workspace.len() <= 1 => Command::Today {
                workspace_id: workspace.first().map(|id| (*id).into()),
                from_ms: langspan::today::window_start(
                    options.remove("--since").as_deref(),
                    langspan::store::wall_now_ms(),
                )?,
            },
            ["report", workspace] => Command::Report {
                workspace_id: (*workspace).into(),
                from_ms: match options.remove("--since") {
                    Some(since) => langspan::today::parse_rfc3339(&since)?,
                    None => 0,
                },
            },
            ["policy", workspace] => Command::Policy {
                workspace_id: (*workspace).into(),
            },
            ["policy", "set", workspace, case, policy] => Command::SetPolicy {
                workspace_id: (*workspace).into(),
                case: langspan_core::PolicyCase::parse(case).ok_or_else(|| {
                    format!(
                        "unknown case {case:?}; the cases are {}",
                        langspan_core::PolicyCase::ALL
                            .iter()
                            .map(|case| case.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?,
                policy: match *policy {
                    "refuse" => langspan_core::Policy::Refuse,
                    "wait" => langspan_core::Policy::Wait,
                    "advisory" => langspan_core::Policy::Advisory,
                    _ => return Err("the policy is refuse, wait or advisory".into()),
                },
                wait_ms: options
                    .remove("--wait-ms")
                    .map(|ms| {
                        ms.parse()
                            .map_err(|_| "--wait-ms must be an integer from 0 to 2000")
                    })
                    .transpose()?,
            },
            ["remember", workspace, text, paths @ ..] => Command::Remember {
                workspace_id: (*workspace).into(),
                author: options.remove("--session"),
                text: (*text).into(),
                paths: paths.iter().map(|path| (*path).into()).collect(),
                symbol: symbols.first().cloned(),
                tags: tags.clone(),
            },
            ["forget", workspace, note] => Command::Forget {
                workspace_id: (*workspace).into(),
                note_id: (*note).into(),
            },
            ["message", workspace, text, paths @ ..] => Command::SendMessage {
                workspace_id: (*workspace).into(),
                from: options.remove("--from"),
                to: options.remove("--to"),
                text: (*text).into(),
                paths: paths.iter().map(|path| (*path).into()).collect(),
            },
            ["handoff", session, text, paths @ ..] => Command::Handoff {
                session_id: (*session).into(),
                text: (*text).into(),
                paths: paths.iter().map(|path| (*path).into()).collect(),
            },
            ["ack", session, message] => Command::AckMessage {
                session_id: (*session).into(),
                message_id: (*message).into(),
            },
            ["messages", "show", workspace, message] => Command::ShowMessage {
                workspace_id: (*workspace).into(),
                message_id: (*message).into(),
            },
            ["messages", "status", workspace, message] => Command::MessageStatus {
                workspace_id: (*workspace).into(),
                from: options.remove("--from"),
                message_id: (*message != "last").then(|| (*message).into()),
            },
            ["messages", "retract", workspace, message] => Command::RetractMessage {
                workspace_id: (*workspace).into(),
                from: options.remove("--from"),
                message_id: (*message != "last").then(|| (*message).into()),
            },
            ["status"] if options.remove("--all").is_some() => Command::StatusRecords {
                after: options.remove("--after"),
                page_bytes: None,
            },
            ["status"] => Command::Status,
            ["changes", workspace] => Command::Changes {
                workspace_id: (*workspace).into(),
                after: number(&mut options, "--after", 0)?,
                limit: u32::try_from(number(&mut options, "--limit", 100)?)
                    .map_err(|_| "--limit is too large")?,
            },
            ["compact"] => Command::Compact {
                keep_days: u32::try_from(number(&mut options, "--keep-days", 90)?)
                    .map_err(|_| "--keep-days is too large")?,
                apply,
            },
            ["events"] => Command::Events {
                after: options
                    .remove("--after")
                    .as_deref()
                    .unwrap_or("0")
                    .parse()
                    .map_err(|_| "--after must be an integer")?,
                limit: options
                    .remove("--limit")
                    .as_deref()
                    .unwrap_or("100")
                    .parse()
                    .map_err(|_| "--limit must be an integer")?,
            },
            _ => return Err("unknown or incomplete command; run `langspan --help`".into()),
        };
        let request_id = if matches!(
            command,
            Command::Status
                | Command::StatusRecords { .. }
                | Command::Events { .. }
                | Command::Changes { .. }
                | Command::RemoteOutbox { .. }
                | Command::RemoteSent { .. }
                | Command::GraphSymbol { .. }
                | Command::GraphReferences { .. }
                | Command::GraphImpact { .. }
                | Command::GraphBlindSpots { .. }
                | Command::GraphPath { .. }
                | Command::GraphMap { .. }
                | Command::GraphInflight { .. }
                | Command::GraphExport { .. }
                | Command::GraphImport { .. }
                | Command::CheckWrite { .. }
                | Command::Recap { .. }
                | Command::Trails { .. }
                | Command::ShowMessage { .. }
                | Command::MessageStatus { .. }
                | Command::Recall { .. }
                | Command::Today { .. }
                | Command::Report { .. }
                | Command::Compact { .. }
                | Command::History { .. }
                | Command::Policy { .. }
                | Command::WorkspaceList { .. }
                | Command::WorkspaceRemove { apply: false, .. }
        ) {
            // Compaction is never recorded or replayed: running it again
            // finds what is left to remove.
            options
                .remove("--request-id")
                .unwrap_or_else(|| "read".into())
        } else {
            options
                .remove("--request-id")
                .ok_or("mutation requires --request-id ID; reuse it only for an identical retry")?
        };
        no_unused_options(&options)?;
        Request::new(request_id, command)
    };
    // `remote enable --root DIR` registers the workspace first, so a machine
    // where no hook has run yet can join in one step.
    if let (Command::RemoteEnable { workspace_id, .. }, Some(root)) =
        (&request.command, enable_root.as_ref())
    {
        let root = std::fs::canonicalize(root)
            .map_err(|e| format!("cannot resolve --root: {e}"))?
            .to_string_lossy()
            .into_owned();
        ask(
            &state_dir,
            langspan::hook::workspace_request_id(workspace_id, &root),
            Command::WorkspaceRegister {
                workspace_id: workspace_id.clone(),
                root,
            },
        )?;
    }
    let mut response = if matches!(
        request.command,
        Command::GraphExport { .. } | Command::GraphImport { .. }
    ) {
        // The daemon allows an index transfer up to two minutes.
        client::call_until(
            &state_dir,
            &request,
            std::time::Instant::now() + std::time::Duration::from_secs(125),
        )?
    } else {
        client::call(&state_dir, &request)?
    };
    // As at a session's start: work ended sessions left is named only while
    // the line journal shows it still uncommitted (roadmap C3).
    if let (
        Command::Brief { session_id, .. },
        Outcome::Briefing {
            briefing:
                langspan_core::Briefing {
                    recent: recent @ Some(_),
                    ..
                },
        },
    ) = (&request.command, &mut response.outcome)
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        if let Some(found) = recent.as_mut()
            && let Err(reason) =
                langspan::recent::confirm_left(&state_dir, session_id, found, deadline)
        {
            eprintln!("langspan: what changed recently is left out: {reason}");
            *recent = None;
        }
    }
    if json {
        println!("{}", serde_json::to_string(&response)?);
    } else if let Outcome::Today { today } = &response.outcome {
        // The month's refusal count, one line, for the person only (H5).
        let month = langspan::count::today_line(
            &state_dir,
            today.workspace_id.as_deref(),
            langspan::store::wall_now_ms(),
        );
        println!(
            "{}",
            langspan::today::text(today, &langspan::today::shown(&today.window), Some(&month))
        );
    } else {
        print_human(&response, &state_dir)?;
        if let Outcome::WorkspaceMoved {
            workspace,
            from_root,
            ..
        } = &response.outcome
        {
            print_project_hook_reruns(&state_dir, workspace, from_root);
        }
    }
    Ok(match response.outcome {
        Outcome::Error { .. } => 1,
        Outcome::Conflict { .. } => 2,
        Outcome::StaleBase { .. } | Outcome::UnreadLines { .. } => 4,
        Outcome::Admitted { .. } if response.lease_valid != Some(true) => 3,
        _ => 0,
    })
}
