use langspan::{
    client,
    hosts::UserHome,
    protocol::{PROTOCOL_VERSION, Request, Response},
};
use langspan_core::{Command, LeaseStatus, Outcome};
use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

const HELP: &str = r#"LangSpan — local cooperative coordination for AI coding agents

Usage: langspan [--state-dir DIR] COMMAND [--json]

  daemon                         Run the local coordinator in the foreground
  mcp                            Serve MCP over stdin/stdout using the coordinator
  dashboard [--listen 127.0.0.1:7311] [--open] [--allow-network]
                                 Serve a live, read-only page of every agent on this
                                 machine and every teammate heard from, on loopback.
                                 Prints the URL with this run's access token; --open
                                 also opens it in the browser. A non-loopback address
                                 is refused without --allow-network; from a phone, use
                                 an SSH tunnel or a VPN to this machine instead
  workspace add ID ROOT          Register a physical workspace
  session start ID WORKSPACE      Register or reattach a logical session
  session attach-pid SESSION PID  Observe an owned process incarnation (Linux)
  intent set SESSION PATH...     Declare work; optional --summary TEXT
  intent end SESSION             End a work intention
  admit SESSION PATH...          Request a whole file set; --ttl-ms defaults to 5000
  complete SESSION LEASE         Complete a current admission
  brief SESSION                  Look at the peer work a session would be told about;
                                 optional --max-peers N (1..16). Does not count as delivery
  remote enable WORKSPACE --name NAME [--remote origin] [--share-task on] [--root DIR]
                [--publish-as-branch]
                                 Publish this workspace's work summary to teammates
                                 through the Git remote, and receive theirs. Off until
                                 enabled. Task lines leave only with --share-task on.
                                 --publish-as-branch publishes as the branch
                                 langspan/peers/ID, visible to everyone in the
                                 repository, for hosts that accept pushes only to
                                 branches. Each enable states every choice again
  remote disable WORKSPACE       Stop publishing and remove the published ref
  remote outbox WORKSPACE        Show exactly what is, or would be, published
  remote sent WORKSPACE [--limit N]
                                 Every publication that left the machine for the
                                 workspace, newest first (default 10, at most 100):
                                 time, sequence and the exact payload pushed
  check run WORKSPACE --name NAME [--timeout-s N] -- COMMAND...
                                 Run a command and record its result for the exact
                                 working tree it ran on
  check status WORKSPACE         Which recorded results apply to the tree as it is now
  check auto WORKSPACE --name NAME [--timeout-s N] -- COMMAND... | --off
                                 Off by default. When on, the daemon runs COMMAND after
                                 a session's batch of writes (Claude Code PostToolBatch,
                                 Claude Code and Codex Stop), on the exact working tree,
                                 records it as check NAME and tells that session the
                                 result at its next briefing. Without options: show it
  integrate check WORKSPACE --onto BRANCH --name NAME BRANCH... -- COMMAND...
                                 Combine branches onto the remote's BRANCH without
                                 touching your checkout, run the command on that exact
                                 result, and record it. Reports conflicts instead
  land WORKSPACE --onto BRANCH --name NAME BRANCH... [--apply] [--session ID]
       [--wait-s N] [--check-again -- COMMAND...]
                                 Push that combined result, only if check NAME passed
                                 on exactly its tree. Never forced. Dry run unless --apply.
                                 --apply takes the push turn for BRANCH first; if another
                                 landing has it, waits in line for it, first come first
                                 served, up to --wait-s (default 240, at most 3600; 0: do
                                 not wait; exit 7 if it does not get the turn). It then
                                 fetches, and if the push is refused because BRANCH
                                 moved, combines again and retries (3 attempts). A new
                                 tree lands only with its own passing check;
                                 --check-again runs COMMAND on it to get one. Conflicts
                                 are shown as hunks; renames that landed are followed.
                                 Optional --remote (default origin), --request-id
  land-queue WORKSPACE --onto BRANCH
                                 Who holds BRANCH's push turn and who waits for it
  land-cancel WORKSPACE --onto BRANCH --request-id ID
                                 Stop the waiting landing ID; it pushes nothing
  graph symbol WORKSPACE NAME    Where a name is defined (Rust, Python, TypeScript, Go)
                                 [--detail]: with its body digest, calls, callers
                                 by name, notes on its file and last check
  graph references WORKSPACE NAME  Where a name is mentioned
  graph impact WORKSPACE PATH    What a file exports and which files mention those names.
                                 All three match by name and report index coverage
  graph blind-spots WORKSPACE [--limit N]
                                 Files the index cannot answer for: partial, never
                                 parsed cleanly, waiting, too large, left out of the
                                 scan, and other languages by extension
  recap WORKSPACE               A bounded account of one workspace: sessions, recent
                                 edits, interface changes, checks, landings, notes and
                                 messages not yet acknowledged
  today [WORKSPACE] [--since RFC3339]
                                 What each session did since local midnight (or the
                                 time given), refusals by kind, checks, landings, and
                                 what is still open. Reads only; acknowledges nothing
  remember WORKSPACE TEXT [PATH...] [--session ID]
                                 Keep a one-line note for the repository or for files;
                                 sessions working there are told. Survives restarts
  forget WORKSPACE NOTE          Remove a note
  recall WORKSPACE QUERY [--path PATH] [--limit N]
                                 Find held notes holding every word of QUERY (1 to 200
                                 bytes, at most 8 words) in their text or paths, without
                                 case; with --path, only notes about PATH or the whole
                                 workspace. Best first, --limit 1 to 50 (default 10).
                                 Reads only; no briefing changes
  commit --mine -m MESSAGE [--session ID] [--root DIR]
                                 Commit only this session's own uncommitted changes in
                                 a checkout other sessions share: whole files that are
                                 all its own, its hunks of files that hold others'
                                 too. Staged in a private index, committed with your
                                 identity and hooks; other changes stay as they are,
                                 staged or not. Refuses (exit 1) a hunk whose author is
                                 not known. Without --session, the session whose shell
                                 call is running it now
  message WORKSPACE TEXT [PATH...] [--to SESSION] [--from SESSION]
                                 Send one line to a session, or without --to to every
                                 other session of the workspace. Without --from it is
                                 from the owner. Each recipient is told once, at its
                                 next prompt or start (Claude Code: also mid-turn)
  handoff SESSION TEXT [PATH...] Leave one line for the next other session that starts
                                 or is prompted in SESSION's workspace
  ack SESSION MESSAGE            Mark a message seen so it is not delivered again
  doctor [--root DIR] [--home DIR]
                                 Say what is and is not working: coordinator, scans,
                                 index, publication, the hooks installed in DIR, and
                                 the user-level install under the home, each with
                                 whether the LangSpan skill is installed and current
  hosts [--home DIR]             List each agent host LangSpan knows (Claude Code,
                                 Codex, Cursor, Gemini CLI): whether its configuration
                                 directory exists, its user-level install (none,
                                 current, outdated, other_state_dir, unknown) and the
                                 command that installs it. Reads only
  status                         Show workspaces, live or recent sessions, intents and
                                 open admissions; closed ones are counted per session
  status --mine [--session ID] [--root DIR]
                                 This session's uncommitted changes in a shared
                                 checkout, file by file and hunk by hunk: each file
                                 mine, mixed or unknown, each hunk mine, another
                                 session's, no session's or not known; at most 200
                                 files. What commit --mine would commit and refuse;
                                 changes nothing. Session found as for commit --mine
  statusline [--root DIR] [--deadline-ms N]
                                 One line for the person: current sessions of this
                                 repository and refusals since local midnight, as
                                 Claude Code's statusLine (directory from its input
                                 on stdin) or a shell prompt (--root). Never starts
                                 the coordinator; within 200 ms (100-5000) or it
                                 prints "coordinator not running" and exits 0
  status --all [--after CURSOR]  Read every record, history included, one page at a
                                 time; pass the page's `next` as --after for the next
  events [--after SEQ] [--limit N] Read ordered events (default limit 100; maximum 1000)
  changes WORKSPACE [--after SEQ] [--limit N]
                                 Read disk and local-branch observations and scan freshness
  request JSON                   Send a complete version 3 request envelope
  hook claude-code|codex|cursor|gemini-cli [--workspace ID --root DIR]
                                 PreToolUse/PostToolUse(Failure) adapter for Edit,
                                 Write, MultiEdit, NotebookEdit, Read and Bash
                                 (Codex: apply_patch and Bash; Cursor: Write, Delete,
                                 Shell and Read; Gemini CLI: BeforeTool and AfterTool
                                 for write_file, replace, read_file and
                                 run_shell_command. Cursor and Gemini CLI are built
                                 from documentation and not yet verified on a real
                                 host); reads one event on stdin. Shell commands are
                                 covered partly (ADR 0011).
                                 Without --workspace and --root (the user-level form),
                                 the repository is the Git toplevel of the event's cwd;
                                 outside a repository, or where a project-level LangSpan
                                 hook covers it, the hook does nothing.
                                 Optional --ttl-ms (default 30000), --budget-ms (1500)
                                 SessionStart and UserPromptSubmit (Gemini CLI:
                                 BeforeAgent) register the
                                 session and print relevant peer work for the model;
                                 --prompt-summary off keeps the prompt's first line
                                 out of what peers are told. At a prompt, names it
                                 mentions that the source index defines are listed
                                 with their files and signatures (matched by name;
                                 the prompt is used for that local lookup only and
                                 is not stored); --code-briefing off turns this off
  hooks install claude-code|codex|cursor|gemini-cli --workspace ID --root DIR
                                 [--settings FILE] [--prompt-summary off]
                                 [--code-briefing off] [--skill] [--apply]
  hooks remove claude-code|codex|cursor|gemini-cli --root DIR [--settings FILE] [--apply]
                                 Show (default) or apply the change to project Claude
                                 settings, Codex hooks, Cursor's .cursor/hooks.json and
                                 .cursor/mcp.json, or Gemini CLI settings
                                 (.gemini/settings.json), preserving other settings.
                                 Cursor only: --skill also writes the LangSpan skill as
                                 the project rule .cursor/rules/langspan.mdc, which
                                 hooks remove cursor removes
  install [--home DIR] [--hosts claude-code,codex,cursor,gemini-cli] [--prompt-summary off]
          [--skill] [--statusline on|off] [--apply]
                                 Install the hooks once for every repository, in each
                                 host's user configuration, and register `langspan mcp`
                                 as the MCP server `langspan`. Hosts default to those
                                 whose configuration directory exists. Dry run unless
                                 --apply. --home DIR ignores HOME, CLAUDE_CONFIG_DIR,
                                 CODEX_HOME, GEMINI_CLI_HOME, LANGSPAN_STATE_DIR and
                                 XDG_STATE_HOME. --skill (off by default) also writes
                                 the LangSpan skill, which teaches an agent how to work
                                 with LangSpan: Claude Code skills/langspan/SKILL.md, a
                                 marked section of Codex's AGENTS.md and of Gemini
                                 CLI's GEMINI.md. --statusline on sets Claude Code's
                                 statusLine to `langspan statusline` where none is
                                 set (never replacing one); off removes LangSpan's
  uninstall [--home DIR] [--hosts ...] [--apply]
                                 Remove only what install wrote, the skill included;
                                 the state directory stays

Every mutation requires --request-id ID. Retry the identical ID and command after
an unknown outcome; a new operation needs a new ID. Old grants never become fresh
on retry. Admission is cooperative and does not fence filesystem writes.

State directory: --state-dir, then LANGSPAN_STATE_DIR, then XDG_STATE_HOME/langspan
or $HOME/.local/state/langspan. Optional owner-only config.json in that directory.
Use --json for the complete response, including replayed, generation, lease_valid.
Exit codes: 0 success, 1 error/unavailable or a failed check, 2 conflict, a refused
push or a target that kept moving, 3 inactive historical grant, 4 stale source (a file
changed since the session read it), 5 inconclusive check, 6 not ready to land (no
current evidence), 7 the landing did not get the push turn: it did not wait, the line
was full, its wait ran out or it was cancelled (nothing was done), 8 pushed
but the landing not recorded, 9 whether a push happened is not known (see its output).
"#;

fn main() -> ExitCode {
    let input: Vec<String> = std::env::args().skip(1).collect();
    // The hook runs on the host's hot path: no async runtime, one event, exit 0.
    if input.first().map(String::as_str) == Some("hook") {
        return hook(input);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","message":error.to_string()})
            );
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(input)) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","message":error.to_string()})
            );
            ExitCode::FAILURE
        }
    }
}

fn hook(input: Vec<String>) -> ExitCode {
    use std::os::unix::fs::OpenOptionsExt;
    // Gemini CLI reads stderr as the answer when stdout is empty, and takes
    // plain text there from a failed process as a deny: whatever happens,
    // it gets the empty answer first.
    let gemini = input.get(1).map(String::as_str) == Some("gemini-cli");
    if gemini {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(b"{}").and_then(|()| stdout.flush());
            default(info);
        }));
    }
    let result = (|| -> Result<(), langspan::Error> {
        let ParsedOptions {
            args, mut options, ..
        } = parse_options(input)?;
        let vendor = match args.as_slice() {
            [hook, name] if hook == "hook" => langspan::install::Vendor::from_name(name),
            _ => None,
        }
        .ok_or(
            "usage: langspan hook claude-code|codex|cursor|gemini-cli [--workspace ID --root DIR]",
        )?;
        // Without a workspace and root, the hook is the user-level form and
        // takes both from each event's working directory.
        let (workspace_id, root, user_level) =
            match (options.remove("--workspace"), options.remove("--root")) {
                (Some(workspace), Some(root)) => (workspace, root.into(), false),
                (None, None) => (String::new(), PathBuf::new(), true),
                _ => {
                    return Err(
                        "--workspace and --root go together; omit both for the user-level form"
                            .into(),
                    );
                }
            };
        let config = langspan::claude_code::Config {
            state_dir: state_dir(&mut options)?,
            workspace_id,
            root,
            vendor,
            user_level,
            ttl_ms: number(&mut options, "--ttl-ms", 30_000)?,
            budget: std::time::Duration::from_millis(number(&mut options, "--budget-ms", 1500)?),
            share_prompt: switch(&mut options, "--prompt-summary")?,
            code_briefing: switch(&mut options, "--code-briefing")?,
        };
        no_unused_options(&options)?;
        if !(1..=60_000).contains(&config.ttl_ms)
            || !(1..=4000).contains(&config.budget.as_millis())
        {
            return Err("hook limits: --ttl-ms 1..60000 and --budget-ms 1..4000".into());
        }
        let deadline = std::time::Instant::now() + config.budget;
        let input = langspan::hook::read_stdin_until(deadline);
        // Opt-in, for capturing what a real host sends so it can become a
        // fixture. Off unless the variable names an existing directory; a
        // failure to write is ignored and never changes the hook's answer.
        if let (Ok(bytes), Some(dir)) = (&input, std::env::var_os("LANGSPAN_CAPTURE_DIR")) {
            let dir = PathBuf::from(dir);
            if dir.is_dir() {
                use std::io::Write;
                let name = format!(
                    "{}-{}-{}.json",
                    args[1],
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(dir.join(name))
                    .and_then(|mut file| file.write_all(bytes));
            }
        }
        let reply = match input {
            Ok(bytes) => match vendor {
                langspan::install::Vendor::Codex => {
                    langspan::codex::run_until(bytes.as_slice(), &config, deadline)
                }
                langspan::install::Vendor::Cursor => {
                    langspan::cursor::run_until(bytes.as_slice(), &config, deadline)
                }
                langspan::install::Vendor::Gemini => {
                    langspan::gemini::run_until(bytes.as_slice(), &config, deadline)
                }
                langspan::install::Vendor::ClaudeCode => {
                    langspan::claude_code::run_until(bytes.as_slice(), &config, deadline)
                }
            },
            Err(error) => langspan::hook::Reply {
                // Gemini CLI reads stderr as the answer when stdout is empty.
                stdout: if vendor == langspan::install::Vendor::Gemini {
                    "{}".into()
                } else {
                    String::new()
                },
                ..langspan::hook::Reply::none(
                    if error.kind() == std::io::ErrorKind::TimedOut {
                        "timeout"
                    } else {
                        "unknown"
                    },
                    error.to_string(),
                )
            },
        };
        print!("{}", reply.stdout);
        eprintln!(
            "{}",
            serde_json::json!({"level":"info","event":"langspan_hook","coverage":reply.coverage,"detail":reply.detail})
        );
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // Exit 1 is a non-blocking error for Claude Code; this adapter never exits 2.
        Err(error) => {
            if gemini {
                print!("{{}}");
            }
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","event":"langspan_hook","coverage":"unknown","message":error.to_string()})
            );
            ExitCode::FAILURE
        }
    }
}

fn hooks(
    vendor: langspan::install::Vendor,
    install: bool,
    state_dir: PathBuf,
    options: &mut BTreeMap<String, String>,
    apply: bool,
) -> Result<u8, langspan::Error> {
    use langspan::install;
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
        })
    } else {
        None
    };
    let with_skill = options.remove("--skill").is_some();
    if with_skill && !(install && vendor == install::Vendor::Cursor) {
        return Err("--skill is valid for `hooks install cursor` only: Cursor reads the LangSpan skill as a project rule. For claude-code, codex and gemini-cli, run `langspan install --skill`; `hooks remove cursor` removes the rule without it".into());
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
        .map(|registration| langspan::user_install::McpServer {
            executable: registration.executable.clone(),
            state_dir: registration.state_dir.clone(),
        });
    let registration = registration.as_ref();
    let server = server.as_ref();
    type Plan<'a> =
        Box<dyn Fn(Option<serde_json::Value>) -> Result<serde_json::Value, String> + 'a>;
    let mut files: Vec<(PathBuf, Plan)> = vec![(
        settings,
        Box::new(move |existing| install::plan_for(vendor, existing, registration)),
    )];
    if vendor == install::Vendor::Cursor {
        files.push((
            root.join(".cursor/mcp.json"),
            Box::new(move |existing| {
                langspan::user_install::plan_claude(
                    existing.unwrap_or_else(|| serde_json::json!({})),
                    server,
                )
            }),
        ));
    }
    // Every file is planned, and every refusal raised, before any is written.
    let mut planned_files = Vec::new();
    for (path, plan) in &files {
        let original = install::read_bytes(path)?;
        let existing = install::parse_json(path, original.as_deref())?;
        let planned = plan(existing.clone()).map_err(|e| format!("{}: {e}", path.display()))?;
        let unchanged = existing.as_ref() == Some(&planned) || (existing.is_none() && !install);
        planned_files.push((path, plan, original, planned, unchanged));
    }
    // Cursor's project rule: written with --skill, removed with the hooks
    // wherever LangSpan's is found.
    let rule = if vendor == install::Vendor::Cursor && (with_skill || !install) {
        let path = langspan::skill::path(vendor, &root);
        Some(langspan::skill::plan_file(vendor, &path, install)?)
    } else {
        None
    };
    let mut changed = false;
    for (path, plan, original, planned, unchanged) in planned_files {
        if unchanged {
            println!("{} already matches; nothing to change.", path.display());
            continue;
        }
        changed = true;
        if apply {
            let replan = |now: Option<&[u8]>| -> Result<Vec<u8>, langspan::Error> {
                let existing = install::parse_json(path, now)?;
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
            if vendor == install::Vendor::Gemini {
                // Project settings can hold MCP server credentials too.
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
    if vendor == install::Vendor::Gemini && install {
        println!(
            "Gemini CLI: built from documentation, not yet verified on a real Gemini CLI (ADR 0033). Gemini CLI loads project settings only in a folder it trusts."
        );
    }
    Ok(0)
}

/// `hosts`: each known host, whether its configuration directory exists and
/// what LangSpan install it has. Reads only.
fn hosts_report(
    home: &UserHome,
    state_dir: &std::path::Path,
    json: bool,
) -> Result<u8, langspan::Error> {
    let reports = langspan::hosts::report(home, state_dir);
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
            (Some(true), langspan::hosts::UserInstall::Unknown) => format!(
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
    Box<langspan::install::Planner<'a>>,
    Option<Box<langspan::install::LeftEmpty<'a>>>,
);

/// `install` / `uninstall`: LangSpan's user-level hooks and MCP server entry
/// for each chosen host. Every file is planned, and every refusal raised,
/// before anything is written.
fn user_install(
    install: bool,
    options: &mut BTreeMap<String, String>,
    apply: bool,
) -> Result<u8, langspan::Error> {
    use langspan::{install as hooks, install::Vendor, user_install};
    let home = UserHome::new(options.remove("--home").map(PathBuf::from))?;
    let state_dir = std::path::absolute(home.state_dir(options.remove("--state-dir")))?;
    let share_prompt = switch(options, "--prompt-summary")?;
    let hosts = options.remove("--hosts");
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
    no_unused_options(options)?;
    let vendors: Vec<Vendor> = match hosts {
        Some(list) => list
            .split(',')
            .map(|name| {
                Vendor::from_name(name.trim()).ok_or_else(|| {
                    format!(
                        "unknown host {:?}; choose from claude-code, codex, cursor and gemini-cli",
                        name.trim()
                    )
                })
            })
            .collect::<Result<_, _>>()?,
        // Gemini CLI is chosen only by name: its adapter has no live-host
        // evidence yet, and `~/.gemini` is also made by other Google tools.
        None => [Vendor::ClaudeCode, Vendor::Codex, Vendor::Cursor]
            .into_iter()
            .filter(|vendor| home.files(*vendor).config_dir.is_dir())
            .collect(),
    };
    if vendors.is_empty() {
        return Err(format!(
            "no host configuration found under {} (.claude, .codex or .cursor); choose --hosts claude-code,codex,cursor,gemini-cli",
            home.home.display()
        )
        .into());
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
    });
    let server = install.then(|| user_install::McpServer {
        executable: executable.clone(),
        state_dir: state_dir.clone(),
    });
    // A host may rewrite its file between this plan and the write.
    let mut plans: Vec<FilePlan> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let registration = registration.as_ref();
    let server = server.as_ref();
    // On uninstall, a file LangSpan may have created is removed when nothing
    // but LangSpan's entries was in it, rather than left holding `{}`.
    let emptied = |vendor: Vendor| {
        (!install).then(|| -> Box<hooks::LeftEmpty> {
            Box::new(move |bytes: &[u8]| hooks::json_left_empty(vendor, bytes))
        })
    };
    for &vendor in &vendors {
        let files = home.files(vendor);
        let original = hooks::read_bytes(&files.hooks)?;
        let existing = hooks::parse_json(&files.hooks, original.as_deref())?;
        if vendor == Vendor::Gemini {
            // Gemini CLI keeps hooks and MCP servers in one settings file:
            // one plan, one guarded write.
            let plan_both = move |existing: Option<serde_json::Value>| {
                let hooked = hooks::plan_user_for(vendor, existing, registration)?;
                user_install::plan_gemini(hooked, server)
            };
            let planned = plan_both(existing.clone())
                .map_err(|e| format!("{}: {e}", files.hooks.display()))?;
            let shown = gemini_shown(&planned, true)?;
            let path = files.hooks.clone();
            let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, langspan::Error> {
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
            if install {
                notes.push("Gemini CLI: built from documentation, not yet verified on a real Gemini CLI (ADR 0033). Gemini CLI runs user-level hooks in every folder; it loads a project's own .gemini/settings.json only once the folder is trusted.".into());
            }
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
        let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, langspan::Error> {
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
                    let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, langspan::Error> {
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
            // Cursor's mcp.json holds only MCP servers; LangSpan may create it.
            Vendor::Cursor => {
                let existing = hooks::parse_json(&files.mcp, original.as_deref())?;
                let planned = user_install::plan_claude(
                    existing.clone().unwrap_or_else(|| serde_json::json!({})),
                    server,
                )
                .map_err(|e| format!("{}: {e}", files.mcp.display()))?;
                let path = files.mcp.clone();
                let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, langspan::Error> {
                    let existing = hooks::parse_json(&path, now)?;
                    let planned = user_install::plan_claude(
                        existing.unwrap_or_else(|| serde_json::json!({})),
                        server,
                    )
                    .map_err(|e| format!("{}: {e}", path.display()))?;
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
                if install {
                    notes.push("Cursor: built from documentation, not yet verified on a real Cursor (ADR 0032). Cursor asks before it first uses an MCP server; approve `langspan` there.".into());
                }
            }
            Vendor::Codex => {
                let existing = hooks::parse_text(&files.mcp, original.as_deref())?;
                let planned = user_install::plan_codex(existing.as_deref(), server)
                    .map_err(|e| format!("{}: {e}", files.mcp.display()))?;
                let path = files.mcp.clone();
                let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, langspan::Error> {
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
    let mut skills: Vec<langspan::skill::FilePlan> = Vec::new();
    for &vendor in &vendors {
        if !langspan::skill::USER_HOSTS.contains(&vendor) {
            if with_skill {
                notes.push("Cursor reads the LangSpan skill as a project rule: run `langspan hooks install cursor --workspace ID --root DIR --skill --apply` in each project.".into());
            }
            continue;
        }
        if install && !with_skill {
            continue;
        }
        let path = langspan::skill::path(vendor, &home.files(vendor).config_dir);
        skills.push(langspan::skill::plan_file(vendor, &path, install)?);
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
        } else if removed {
            println!("{} would be removed.", path.display());
        } else {
            println!("{} would become:", path.display());
            println!("{text}");
        }
    }
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
        if !apply {
            println!("{}", plan.shown());
        } else if plan.apply()? {
            match plan.planned {
                Some(_) => println!("Wrote {}.", plan.path.display()),
                None => println!("Removed {}.", plan.path.display()),
            }
        } else {
            println!(
                "{} already matches; nothing to change.",
                plan.path.display()
            );
        }
    }
    if changed > 0 && !apply {
        println!("Dry run; nothing was written. Run the same command with --apply to write it.");
    }
    if !install && apply {
        println!("LangSpan's state directory was left in place.");
    }
    for note in notes {
        println!("{note}");
    }
    Ok(0)
}

/// What a dry run shows of Gemini CLI's settings file: LangSpan's entries
/// only. The rest of the file can hold the owner's MCP server credentials
/// and is neither shown nor changed.
fn gemini_shown(planned: &serde_json::Value, user: bool) -> Result<String, langspan::Error> {
    use langspan::{install, user_install};
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

fn ask(
    state_dir: &std::path::Path,
    id: String,
    command: Command,
) -> Result<Outcome, langspan::Error> {
    langspan::landing::ask(state_dir, id, command).map(|response| response.outcome)
}

fn check_name(options: &mut BTreeMap<String, String>) -> Result<String, langspan::Error> {
    Ok(options
        .remove("--name")
        .ok_or("--name NAME is required: it is how this check's evidence is found again")?)
}

fn unique(kind: &str, fields: &[&str]) -> String {
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
fn evidence(
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
    let (head, before) = snapshot(&root)?;
    let (ran, duration_ms) = integrate::run_command(&root, command, limit)?;
    let (_, after) = snapshot(&root)?;
    // A result is about a tree only if the tree held still while it ran.
    let result = match ran {
        _ if before != after => CheckResult::Inconclusive {},
        integrate::Ran::Exited(0) => CheckResult::Passed {},
        integrate::Ran::Exited(exit_code) => CheckResult::Failed { exit_code },
        integrate::Ran::Unfinished => CheckResult::Inconclusive {},
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

/// `commit --mine`: commit only this session's own changes (ADR 0026).
fn commit_mine(
    state_dir: &std::path::Path,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    if options.remove("--mine").is_none() {
        return Err(
            "`commit` needs --mine: langspan commit --mine -m MESSAGE [--session ID] [--root DIR]"
                .into(),
        );
    }
    let message = options
        .remove("--message")
        .ok_or("a commit message is required: -m MESSAGE")?;
    let root = match options.remove("--root") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()?,
    };
    let session = options.remove("--session");
    no_unused_options(options)?;
    let session = match session {
        Some(session) => session,
        None => langspan::commit::running_session(state_dir, &root)?,
    };
    let report = langspan::commit::commit_mine(state_dir, &root, &session, &message)?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", report.text());
    }
    Ok(report.exit_code())
}

/// `statusline`: sessions here and refusals today, for the person (H2).
/// Exits 0 with the unavailable line when the coordinator cannot answer.
fn status_line(
    state_dir: &std::path::Path,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    use langspan::status_line::{DEADLINE_RANGE_MS, DEFAULT_DEADLINE_MS, run};
    let started = std::time::Instant::now();
    let root = options.remove("--root").map(PathBuf::from);
    let deadline_ms = number(options, "--deadline-ms", DEFAULT_DEADLINE_MS)?;
    no_unused_options(options)?;
    if !DEADLINE_RANGE_MS.contains(&deadline_ms) {
        return Err(format!(
            "--deadline-ms must be between {} and {}",
            DEADLINE_RANGE_MS.start(),
            DEADLINE_RANGE_MS.end()
        )
        .into());
    }
    let answer = run(
        state_dir,
        root,
        std::time::Duration::from_millis(deadline_ms),
        started,
    );
    if json {
        println!("{}", answer.json());
    } else {
        println!("{}", answer.text());
    }
    Ok(0)
}

/// `status --mine`: this session's uncommitted lines, read-only (issue #40).
fn status_mine(
    state_dir: &std::path::Path,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    let root = match options.remove("--root") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()?,
    };
    let session = options.remove("--session");
    no_unused_options(options)?;
    let session = match session {
        Some(session) => session,
        None => langspan::commit::running_session(state_dir, &root)?,
    };
    let status = langspan::commit::status_mine(state_dir, &root, &session)?;
    if json {
        println!("{}", serde_json::to_string(&status)?);
    } else {
        println!("{}", status.text());
    }
    Ok(0)
}

/// `check auto`: turn the automatic check on, off, or show it.
fn auto_check(
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
        if command.is_empty() {
            return Err("give the check command after --, for example: -- cargo test".into());
        }
        langspan::auto_check::refuse_secrets(command)?;
        ask(
            state_dir,
            unique("auto-check", &[workspace, &name]),
            Command::AutoCheckEnable {
                workspace_id: workspace.into(),
                name,
                argv: command.to_vec(),
                timeout_s,
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

/// An inconclusive check's reason and next step, as the MCP result has them.
fn print_inconclusive(name: &str, reason: &Option<langspan::landing::Inconclusive>) {
    match reason {
        Some(reason) => {
            println!("{}", reason.message);
            println!("Next: {}", reason.next);
        }
        None => println!("{name} is inconclusive; nothing is known about the combined tree."),
    }
}

/// `integrate check` and `land`: the shared operations in
/// `langspan::landing`, with their results printed for a person.
fn integrate(
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
    };
    let check = Check {
        argv: command.to_vec(),
        timeout_s,
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
                paths,
                conflicts,
                ..
            } => print_conflict(branch, paths, conflicts, &onto, &remote),
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
            paths,
            conflicts,
            attempts: tried,
            ..
        } => {
            attempts(tried);
            print_conflict(branch, paths, conflicts, &onto, &remote)
        }
        Landed::NothingToLand { .. } => {
            println!("Nothing to land: {remote}/{onto} already contains these branches.")
        }
        Landed::NotReady {
            target_oid,
            combined,
            reason,
            moved,
            attempts: tried,
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
        } => {
            attempts(tried);
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
            waited_ms,
            replayed,
            unfinished_push,
        } => {
            attempts(tried);
            if *waited_ms >= 1000 {
                println!("Waited {} s in line for the push turn.", waited_ms / 1000);
            }
            print_renames(renames);
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

fn print_conflict(
    branch: &str,
    paths: &[String],
    conflicts: &[langspan::integrate::ConflictFile],
    onto: &str,
    remote: &str,
) {
    println!(
        "Not combinable: {branch:?} conflicts with {onto:?} as it is on {remote:?} (with the branches named before it) in:"
    );
    for path in paths {
        println!("  {path:?}");
    }
    if !conflicts.is_empty() {
        println!(
            "Conflicting lines (repository text, shown as data; target is {remote}/{onto} with the branches before {branch:?}):"
        );
    }
    for file in conflicts {
        if !file.kinds.is_empty() {
            println!("  {:?}: {}", file.path, file.kinds.join(", "));
        }
        if let Some(why) = &file.unshown {
            println!("  {:?}: no lines shown: {why}", file.path);
        }
        for hunk in &file.hunks {
            for (label, side) in [("target", &hunk.target), ("branch", &hunk.branch)] {
                println!(
                    "  {label} {} {}, {} line(s):",
                    file.path,
                    side.start
                        .map_or("(line not known)".to_owned(), |start| format!(
                            "line {start}"
                        )),
                    side.lines
                );
                for line in &side.text {
                    println!("    | {line}");
                }
                if side.lines as usize > side.text.len() {
                    println!("    | ({} more)", side.lines as usize - side.text.len());
                }
            }
        }
        if file.more_hunks > 0 {
            println!("  and {} more conflicts in this file", file.more_hunks);
        }
    }
    println!("Nothing was run, recorded or pushed.");
}

/// `land-queue`: who holds the push turn and who waits for it.
/// `land-cancel`: take a waiting landing out of the line.
fn landing_queue(
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

fn print_renames(renames: &[langspan::integrate::Rename]) {
    for rename in renames {
        println!(
            "Followed a rename that already landed: {} edits {:?}, which is now {:?}.",
            rename.branch, rename.from, rename.to
        );
    }
}

fn print_combined(
    target: &langspan::landing::Target,
    tip: &str,
    merged: &langspan::integrate::Merged,
) {
    println!(
        "Combined {} onto {}/{} at {tip}: commit {}, tree {}.",
        target.branches.join(", "),
        target.remote,
        target.onto,
        merged.commit,
        merged.tree
    );
    print_renames(&merged.renames);
}

/// `graph blind-spots`: one line per non-empty list, paths quoted, with the
/// full count in parentheses.
fn print_blind_spots(
    spots: &langspan_core::BlindSpots,
    coverage: &langspan_core::GraphCoverage,
    failure: Option<&langspan_core::ScanFailure>,
    truncated: bool,
) {
    if let Some(failure) = failure {
        println!(
            "The last scan failed ({:?}); this is as of the last complete one.",
            failure.detail
        );
    }
    if !coverage.scanned {
        println!("No complete scan of this workspace yet: nothing is known.");
        return;
    }
    let now = langspan::clock::now_ms();
    let since = |at: u64| {
        let seconds = now.saturating_sub(at) / 1000;
        match seconds {
            0..=89 => format!("{seconds}s"),
            90..=5399 => format!("{}m", seconds / 60),
            5400..=129_599 => format!("{}h", seconds / 3600),
            _ => format!("{}d", seconds / 86_400),
        }
    };
    let line = |label: &str, total: u32, paths: Vec<String>| {
        if total > 0 {
            let more = total as usize - paths.len().min(total as usize);
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            println!("{label} ({total}): {}{more}", paths.join(", "));
        }
    };
    let quoted = |paths: &[String]| paths.iter().map(|path| format!("{path:?}")).collect();
    let totals = &spots.totals;
    line(
        "partial",
        totals.partial,
        spots
            .partial
            .iter()
            .map(|file| format!("{:?} (since {})", file.path, since(file.since_ms)))
            .collect(),
    );
    line(
        "never parsed cleanly",
        totals.never_clean,
        quoted(&spots.never_clean),
    );
    line("waiting", totals.waiting, quoted(&spots.waiting));
    line("too large", totals.too_large, quoted(&spots.too_large));
    line(
        "left out of the scan",
        totals.unscanned,
        quoted(&spots.unscanned),
    );
    if !spots.unsupported_languages.is_empty() {
        println!(
            "not indexed, other languages: {}",
            spots
                .unsupported_languages
                .iter()
                .map(|count| format!("{} {}", count.extension, count.files))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if coverage.blind_in_scope == 0 && spots.unsupported_languages.is_empty() {
        println!("The index reads every supported file at its current content.");
    }
    if truncated {
        println!("More paths exist than the limit allows.");
    }
}

/// What the last completed scan did not version, in one paragraph. A
/// partial scan is always called partial.
fn coverage_text(coverage: &langspan_core::ScanCoverage) -> String {
    let named = |paths: &[String], count: u64| {
        let mut text = paths
            .iter()
            .map(|path| format!("{path:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        if count > paths.len() as u64 {
            text.push_str(&format!(" and {} more", count - paths.len() as u64));
        }
        text
    };
    let mut parts = Vec::new();
    if coverage.omitted > 0 {
        parts.push(format!(
            "{} file(s) left out at the scan's file, byte or time bound (changed files are versioned first)",
            coverage.omitted
        ));
    }
    if coverage.too_large > 0 {
        parts.push(format!(
            "{} file(s) not versioned because they are over {} MiB: {}",
            coverage.too_large,
            langspan::observer::Limits::default().max_file_bytes / (1024 * 1024),
            named(&coverage.too_large_paths, coverage.too_large)
        ));
    }
    if coverage.submodules > 0 {
        parts.push(format!(
            "{} submodule(s) present and not read into: {}",
            coverage.submodules,
            named(&coverage.submodule_paths, coverage.submodules)
        ));
    }
    if coverage.skip_worktree > 0 {
        parts.push(format!(
            "{} file(s) outside the sparse checkout, not observed",
            coverage.skip_worktree
        ));
    }
    let lead = if coverage.is_partial() {
        "The last scan was partial: file observations, check staleness marks and the index do not cover the files it did not version."
    } else {
        "The last scan versioned every file in the working tree."
    };
    format!("{lead} {}.", parts.join("; "))
}

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
fn doctor(
    state_dir: &std::path::Path,
    root: Option<&std::path::Path>,
    home: &UserHome,
    json: bool,
) -> Result<u8, langspan::Error> {
    let mut findings: Vec<(bool, String)> = Vec::new();
    let mut note = |ok: bool, text: String| findings.push((ok, text));
    // Registered workspace roots and the hosts whose sessions work in them.
    let mut in_use: Option<InUse> = None;
    // What doctor judges a workspace by must be complete, even when the
    // bounded view leaves it out to fit a frame.
    match langspan::client::status_from(state_dir, &["workspace_git", "remote_peers"]) {
        Err(error) => note(
            false,
            format!(
                "Coordinator not reachable at {}: {error}. Start it with `langspan daemon`. Until then every hook gives no decision and no briefing.",
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
            if state.workspaces.is_empty() {
                note(
                    false,
                    "No workspace is registered. A hook registers one on first use; none has run."
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
                                langspan::remote::peer_ref(link),
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
                                langspan::remote::peer_ref(link),
                                link.remote
                            ),
                        },
                    );
                }
            }
        }
    }
    if let Some(root) = root {
        use langspan::install::{self, Vendor};
        let mut shell_hooks = false;
        for vendor in Vendor::ALL {
            let path = root.join(vendor.project_hook_files()[0]);
            let (managed, missing): (Vec<String>, Vec<&str>) = match install::read(&path) {
                Err(error) => {
                    note(false, format!("{}: {error}", path.display()));
                    continue;
                }
                Ok(None) => (Vec::new(), Vec::new()),
                Ok(Some(settings)) => {
                    let ours = |hook: &serde_json::Value| install::is_managed_for(vendor, hook);
                    (
                        install::managed_hooks(&settings, ours)
                            .iter()
                            .filter_map(|hook| hook["command"].as_str().map(str::to_owned))
                            .collect(),
                        install::missing_events(vendor, &settings, ours),
                    )
                }
            };
            // Cursor and Gemini CLI are newer and optional: their absence is
            // not a problem.
            if managed.is_empty() && matches!(vendor, Vendor::Cursor | Vendor::Gemini) {
                continue;
            }
            let Some(command) = managed.first() else {
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
            };
            let here = std::path::absolute(state_dir)?;
            let points_here = command.contains(&install::quote(&here.to_string_lossy()));
            let exists = install::executable_of(vendor, command).is_some_and(|path| path.is_file());
            note(
                points_here && exists && missing.is_empty(),
                format!(
                    "{} hooks installed for {} event(s){}{}{}{}",
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
                        Vendor::Gemini =>
                            ". Gemini CLI: built from documentation, not yet verified on a real Gemini CLI (ADR 0033); it loads them only in a folder it trusts, and warns once before it first runs them.",
                        Vendor::ClaudeCode => ".",
                    }
                ),
            );
            if vendor == Vendor::Cursor {
                let mcp = root.join(".cursor/mcp.json");
                match install::read(&mcp).map(|config| {
                    config
                        .as_ref()
                        .and_then(|config| config.get("mcpServers"))
                        .and_then(|servers| servers.get(langspan::user_install::SERVER_NAME))
                        .map(langspan::user_install::parse_claude_entry)
                }) {
                    Err(error) => note(false, format!("{}: {error}", mcp.display())),
                    Ok(None) => note(
                        false,
                        format!(
                            "cursor: the MCP server is not registered in {}; run `langspan hooks install cursor --workspace ID --root {} --apply`.",
                            mcp.display(),
                            root.display()
                        ),
                    ),
                    Ok(Some(None)) => note(
                        false,
                        format!(
                            "cursor: an MCP server named `langspan` is registered in {} but LangSpan did not write it.",
                            mcp.display()
                        ),
                    ),
                    Ok(Some(Some(server))) => note(
                        server.state_dir == here && server.executable.is_file(),
                        format!(
                            "cursor: MCP server `langspan` registered in {}, state directory {}.",
                            mcp.display(),
                            server.state_dir.display()
                        ),
                    ),
                }
                let (ok, text) = skill_finding(
                    vendor,
                    &langspan::skill::path(vendor, root),
                    &format!(
                        "langspan hooks install cursor --workspace ID --root {} --skill --apply",
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
    for (ok, text) in user_level_findings(state_dir, home, in_use.as_ref())? {
        note(ok, text);
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
    use langspan::{
        hook::install_at,
        install::{Vendor, WorktreeHooks},
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let listed = match langspan::git::output(
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
    vendor: langspan::install::Vendor,
    file: &std::path::Path,
    registration: &langspan::install::Registration,
) -> Option<String> {
    use langspan::install;
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
        }
        Ok(None) => problems.push("could not be read".to_owned()),
        Err(error) => problems.push(format!("could not be read: {error}")),
    }
    (!problems.is_empty()).then(|| problems.join(" and "))
}

/// What `doctor` says about the user-level install of each host present
/// under the home: hooks, MCP registration, the executable both run, and the
/// state directory both point at.
fn user_level_findings(
    state_dir: &std::path::Path,
    home: &UserHome,
    in_use: Option<&InUse>,
) -> Result<Vec<(bool, String)>, langspan::Error> {
    use langspan::{install, install::Vendor, user_install};
    let here = std::path::absolute(state_dir)?;
    let mut findings = Vec::new();
    let check = |executable: &std::path::Path, dir: &std::path::Path| -> String {
        let mut problems = String::new();
        if !executable.is_file() {
            problems.push_str("; its executable no longer exists, so it does nothing: run `langspan install --apply` again");
        }
        if dir != here {
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
        if langspan::skill::USER_HOSTS.contains(&vendor) {
            let path = langspan::skill::path(vendor, &files.config_dir);
            findings.push(skill_finding(
                vendor,
                &path,
                &format!("langspan install --skill --hosts {name} --apply"),
            ));
            if vendor == Vendor::Codex
                && let Some(problem) = langspan::skill::codex_unread(&files.config_dir)
            {
                findings.push((false, format!("{name}: {problem}.")));
            }
        }
        let (hooks, missing): (Vec<install::UserRegistration>, Vec<&str>) = match install::read(
            &files.hooks,
        ) {
            // Gemini CLI accepts comments in its settings; LangSpan never
            // writes a file it cannot parse, so it cannot have installed there.
            Err(error) if vendor == Vendor::Gemini => {
                findings.push((
                        true,
                        format!(
                            "{name}: {error}, so no LangSpan install is there and none can be made until it is plain JSON."
                        ),
                    ));
                continue;
            }
            Err(error) => {
                findings.push((false, format!("{}: {error}", files.hooks.display())));
                (Vec::new(), Vec::new())
            }
            Ok(None) => (Vec::new(), Vec::new()),
            Ok(Some(settings)) => {
                let ours = |hook: &serde_json::Value| install::is_user_managed_for(vendor, hook);
                (
                    install::managed_hooks(&settings, ours)
                        .iter()
                        .filter_map(|hook| install::parse_user_hook(vendor, hook))
                        .collect(),
                    install::missing_events(vendor, &settings, ours),
                )
            }
        };
        let server: Result<Option<user_install::McpServer>, String> = match vendor {
            Vendor::ClaudeCode | Vendor::Cursor | Vendor::Gemini => match install::read(&files.mcp) {
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
                    && !langspan::hosts::not_installed(
                        &[langspan::hosts::inspect(vendor, home, state_dir)],
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
                    langspan::hosts::install_command(vendor, home, state_dir),
                    if uncovered.is_some() {
                        " Sessions of another host use this coordinator and no registered workspace has a project install for this one, so its edits are not coordinated."
                    } else {
                        ""
                    }
                ),
            ));
            continue;
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
                problems.push_str(&check(&first.executable, &first.state_dir));
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

/// What `doctor` says about the LangSpan skill in the file at `path`. Not
/// installing it is a choice (it is off by default), not a problem.
fn skill_finding(
    vendor: langspan::install::Vendor,
    path: &std::path::Path,
    install: &str,
) -> (bool, String) {
    use langspan::skill::{State, file_state};
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

/// An `on`/`off` option that is on unless switched off.
fn switch(options: &mut BTreeMap<String, String>, name: &str) -> Result<bool, langspan::Error> {
    match options.remove(name).as_deref() {
        None | Some("on") => Ok(true),
        Some("off") => Ok(false),
        Some(_) => Err(format!("{name} must be on or off").into()),
    }
}

fn number(
    options: &mut BTreeMap<String, String>,
    name: &str,
    default: u64,
) -> Result<u64, langspan::Error> {
    match options.remove(name) {
        None => Ok(default),
        Some(value) => value
            .parse()
            .map_err(|_| format!("{name} must be an integer").into()),
    }
}

fn state_dir(options: &mut BTreeMap<String, String>) -> Result<PathBuf, langspan::Error> {
    Ok(options
        .remove("--state-dir")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LANGSPAN_STATE_DIR").map(PathBuf::from))
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(|p| PathBuf::from(p).join("langspan")))
        .or_else(|| {
            std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/langspan"))
        })
        .ok_or("choose --state-dir; no state directory environment is available")?)
}

async fn run(input: Vec<String>) -> Result<u8, langspan::Error> {
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
        json,
        apply,
    } = parse_options(input)?;
    if let [action] = args.as_slice()
        && (action == "install" || action == "uninstall")
    {
        return user_install(action == "install", &mut options, apply);
    }
    let home_option = options.remove("--home").map(PathBuf::from);
    let state_option = options.get("--state-dir").cloned();
    let state_dir = state_dir(&mut options)?;
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let before_command = &words[..words.len() - command.len()];
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
        ["commit"] => return commit_mine(&state_dir, &mut options, json),
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
    if apply {
        return Err(
            "--apply is valid only for `install`, `uninstall`, `hooks install|remove` and `land`"
                .into(),
        );
    }
    if words == ["dashboard"] {
        let listen = match options.remove("--listen") {
            None => langspan::dashboard::default_listen(),
            Some(value) => value.parse().map_err(|_| {
                format!(
                    "--listen must be an IP address and port, such as 127.0.0.1:{}",
                    langspan::dashboard::DEFAULT_PORT
                )
            })?,
        };
        let open = options.remove("--open").is_some();
        let allow_network = options.remove("--allow-network").is_some();
        no_unused_options(&options)?;
        let options = langspan::dashboard::Options {
            listen,
            open,
            allow_network,
        };
        if json {
            return Err(
                "--json is not valid for `dashboard`; its JSON is served at /api/state".into(),
            );
        }
        tokio::task::spawn_blocking(move || langspan::dashboard::serve(state_dir, options))
            .await??;
        return Ok(0);
    }
    if words == ["daemon"] || words == ["mcp"] {
        no_unused_options(&options)?;
        if words[0] == "daemon" {
            langspan::daemon::run(state_dir).await?;
        } else {
            langspan::mcp::serve(state_dir).await?;
        }
        return Ok(0);
    }
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
                    },
                }
            }
            ["graph", "blind-spots", workspace] => Command::GraphBlindSpots {
                workspace_id: (*workspace).into(),
                limit: u32::try_from(number(&mut options, "--limit", 50)?)
                    .map_err(|_| "--limit is too large")?,
            },
            ["recap", workspace] => Command::Recap {
                workspace_id: (*workspace).into(),
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
            ["remember", workspace, text, paths @ ..] => Command::Remember {
                workspace_id: (*workspace).into(),
                author: options.remove("--session"),
                text: (*text).into(),
                paths: paths.iter().map(|path| (*path).into()).collect(),
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
            ["status"] if options.remove("--all").is_some() => Command::StatusRecords {
                after: options.remove("--after"),
            },
            ["status"] => Command::Status,
            ["changes", workspace] => Command::Changes {
                workspace_id: (*workspace).into(),
                after: number(&mut options, "--after", 0)?,
                limit: u32::try_from(number(&mut options, "--limit", 100)?)
                    .map_err(|_| "--limit is too large")?,
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
                | Command::Recap { .. }
                | Command::Recall { .. }
                | Command::Today { .. }
        ) {
            options
                .remove("--request-id")
                .unwrap_or_else(|| "read".into())
        } else {
            options
                .remove("--request-id")
                .ok_or("mutation requires --request-id ID; reuse it only for an identical retry")?
        };
        no_unused_options(&options)?;
        Request {
            version: PROTOCOL_VERSION,
            request_id,
            command,
        }
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
    let response = client::call(&state_dir, &request)?;
    if json {
        println!("{}", serde_json::to_string(&response)?);
    } else {
        print_human(&response, &state_dir)?;
    }
    Ok(match response.outcome {
        Outcome::Error { .. } => 1,
        Outcome::Conflict { .. } => 2,
        Outcome::StaleBase { .. } | Outcome::UnreadLines { .. } => 4,
        Outcome::Admitted { .. } if response.lease_valid != Some(true) => 3,
        _ => 0,
    })
}

struct ParsedOptions {
    args: Vec<String>,
    /// Everything after a literal `--`: a check's command line.
    command: Vec<String>,
    options: BTreeMap<String, String>,
    json: bool,
    apply: bool,
}

fn parse_options(input: Vec<String>) -> Result<ParsedOptions, langspan::Error> {
    let mut positionals = Vec::new();
    let mut command = Vec::new();
    let mut options = BTreeMap::new();
    let mut json = false;
    let mut apply = false;
    let mut raw = false;
    let mut input = input.into_iter();
    while let Some(arg) = input.next() {
        if raw {
            command.push(arg.clone());
            positionals.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => raw = true,
            "--json" => json = true,
            "--apply" => apply = true,
            // Flags, kept with the options so that only the commands that take
            // them accept them (`dashboard`: `--open`, `--allow-network`;
            // `land`: `--check-again`; `remote enable`: `--publish-as-branch`;
            // `install` and `hooks install cursor`: `--skill`; `status`: `--all`;
            // `graph symbol`: `--detail`).
            "--open"
            | "--allow-network"
            | "--check-again"
            | "--publish-as-branch"
            | "--mine"
            | "--detail"
            | "--all"
            | "--skill"
            | "--off" => {
                if options.insert(arg.clone(), String::new()).is_some() {
                    return Err(format!("duplicate option {arg}").into());
                }
            }
            "--state-dir" | "--request-id" | "--summary" | "--ttl-ms" | "--after" | "--limit"
            | "--workspace" | "--root" | "--budget-ms" | "--settings" | "--prompt-summary"
            | "--code-briefing" | "--max-peers" | "--remote" | "--name" | "--share-task"
            | "--onto" | "--timeout-s" | "--wait-s" | "--session" | "--to" | "--from"
            | "--home" | "--hosts" | "--listen" | "-m" | "--message" | "--statusline"
            | "--deadline-ms" | "--since" | "--path" => {
                let arg = if arg == "-m" {
                    "--message".to_owned()
                } else {
                    arg
                };
                let value = input.next().ok_or_else(|| format!("{arg} needs a value"))?;
                if options.insert(arg.clone(), value).is_some() {
                    return Err(format!("duplicate option {arg}").into());
                }
            }
            _ if arg.starts_with('-') => {
                return Err(format!(
                    "unknown option {arg}; use -- before a filename beginning with '-' "
                )
                .into());
            }
            _ => positionals.push(arg),
        }
    }
    Ok(ParsedOptions {
        args: positionals,
        command,
        options,
        json,
        apply,
    })
}
fn no_unused_options(options: &BTreeMap<String, String>) -> Result<(), langspan::Error> {
    if let Some((name, _)) = options.first_key_value() {
        return Err(format!("option {name} is not valid for this command").into());
    }
    Ok(())
}
/// The lines `graph symbol --detail` adds under a definition. Note text is
/// another session's: quoted, so it reads as data.
fn print_symbol_detail(symbol: &langspan_core::SymbolRef) {
    if let Some(digest) = &symbol.body_digest {
        println!("  body digest {digest}");
    }
    if let Some(calls) = &symbol.calls {
        let shown: Vec<String> = calls
            .iter()
            .map(|call| format!("{} ×{} (line {})", call.name, call.count, call.line))
            .collect();
        println!(
            "  calls: {}",
            if shown.is_empty() {
                "none found".into()
            } else {
                shown.join(", ")
            }
        );
    }
    if let Some(callers) = &symbol.callers {
        let more = callers.count as usize > callers.sites.len();
        println!(
            "  called by name at {} site(s){}{}{}",
            callers.count,
            if callers.sites.is_empty() { "" } else { ": " },
            callers.sites.join(", "),
            if more { ", …" } else { "" }
        );
    }
    for note in symbol.notes.iter().flatten() {
        println!("  note {}: {:?}", note.id, note.text);
    }
    if let Some(check) = &symbol.last_check {
        println!(
            "  last check on this file's content: {} {} on tree {}",
            check.name, check.result, check.tree
        );
    }
}

fn print_human(response: &Response, state_dir: &std::path::Path) -> Result<(), langspan::Error> {
    if response.replayed {
        println!(
            "Original result replayed; current daemon generation {}.",
            response.generation
        );
    }
    match &response.outcome {
        Outcome::Workspace { workspace } => println!(
            "Workspace {} registered at {:?}.",
            workspace.workspace_id, workspace.root
        ),
        Outcome::Session { session } => println!(
            "Session {} attached to {} in generation {}.",
            session.session_id, session.workspace_id, session.generation
        ),
        Outcome::SessionLiveness { liveness } => println!(
            "Session {} process {}: {:?}, observed at {} ms in generation {}.",
            liveness.session_id,
            liveness.process.pid,
            liveness.status,
            liveness.observed_at_ms,
            liveness.generation
        ),
        Outcome::Intent { intent } => println!(
            "Intent for {}: {} on {:?}.",
            intent.session_id,
            serde_json::to_string(&intent.summary)?,
            intent.paths
        ),
        Outcome::IntentEnded { session_id } => println!("Intent ended for {session_id}."),
        Outcome::Admitted { lease } => println!(
            "Admission {}: holder {}, files {:?}, generation {}, current validity {}. Expires at {} ms of that daemon generation.",
            lease.lease_id,
            lease.session_id,
            lease.paths,
            lease.generation,
            response.lease_valid == Some(true),
            lease.expires_at_ms
        ),
        Outcome::Conflict { holders } => {
            println!("Conflict; no files admitted.");
            for holder in holders {
                println!(
                    "  {} holds {:?} through admission {}.",
                    holder.session_id, holder.paths, holder.lease_id
                );
            }
        }
        Outcome::Completed { lease_id } => println!("Admission {lease_id} completed."),
        Outcome::Observed { observation } => println!(
            "Observation {}: {} {:?} reported {:?} for {:?}; admission relation {:?}.",
            observation.observation_id,
            observation.source,
            observation.tool,
            observation.result,
            observation.paths,
            observation.lease_relation
        ),
        Outcome::DiskObserved { observation } => println!(
            "Unattributed disk observation {}: {:?}, {:?} -> {:?}.",
            observation.observation_id, observation.path, observation.before, observation.after
        ),
        Outcome::BranchObserved { observation } => println!(
            "Local branch observation {}: {:?} -> {:?}.",
            observation.observation_id, observation.branch_ref, observation.head_oid
        ),
        Outcome::Changes { events, scan } => {
            match scan {
                Some(scan) => println!(
                    "Latest complete scan: {} files, {} local branches, generation {}, observed at {} ms. Disk changes are unattributed.",
                    scan.file_count, scan.branch_count, scan.generation, scan.observed_at_ms
                ),
                None => println!("No complete Git baseline yet; coverage is unknown."),
            }
            for event in events {
                println!("{} {}", event.seq, serde_json::to_string(&event.event)?);
            }
        }
        Outcome::WorkspaceGit { git } => println!(
            "Workspace {} is in repository {} on {:?}.",
            git.workspace_id, git.repository_id, git.branch_ref
        ),
        Outcome::SessionPrompted { session_id } => {
            println!("Task summary recorded for {session_id}.")
        }
        Outcome::PromptTargets { session_id, paths } => println!(
            "Prompt targets recorded for {session_id}: {} file(s).",
            paths.len()
        ),
        Outcome::SessionEnded { session_id } => {
            println!("Session {session_id} marked ended.")
        }
        Outcome::AutoCheck { check } => println!(
            "Automatic check {} is on for workspace {}: after a session's writes, {:?} runs on the exact working tree (time limit {} s).",
            check.name,
            check.workspace_id,
            langspan::landing::command_line(&check.argv),
            check.timeout_s
        ),
        Outcome::AutoCheckDisabled { workspace_id } => {
            println!("Automatic check is off for workspace {workspace_id}.")
        }
        Outcome::SessionStopped {
            session_id,
            released,
            ..
        } => println!(
            "Session {session_id} finished its reply; {} open admission(s) released.",
            released.len()
        ),
        Outcome::ToolBatchObserved {
            session_id, calls, ..
        } => {
            println!("Batch of {calls} tool call(s) recorded for {session_id}.")
        }
        Outcome::Briefing { briefing } => {
            if !langspan::hook::has_content(briefing) {
                println!("No relevant peer work.");
            } else {
                println!("{}", langspan::hook::render_briefing(briefing));
            }
        }
        Outcome::StaleBase { stale } => {
            println!("Stale source; no files admitted. Read these again first:");
            for file in stale {
                println!(
                    "  {:?} changed since this session read it{}.",
                    file.path,
                    file.changed_by
                        .as_ref()
                        .map(|session| format!(" (last reported edit by {session})"))
                        .unwrap_or_default()
                );
            }
        }
        Outcome::UnreadLines { lines } => {
            println!("Unread lines; no files admitted. Read these first:");
            for entry in lines {
                println!(
                    "  {:?}: lines {} written by {} and not read since.",
                    entry.path,
                    if entry.lines.is_empty() {
                        "that could not be placed".to_owned()
                    } else {
                        entry
                            .lines
                            .iter()
                            .map(|range| format!("{}-{}", range.start_line, range.end_line))
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    entry.session_id
                );
            }
        }
        Outcome::SourceObserved { session_id, files } => {
            println!("Recorded {files} file version(s) for {session_id}.")
        }
        Outcome::RemoteLink { link } => println!(
            "Workspace {} publishes to Git remote {:?} as {:?} (participant {}) at {}{}; task lines {}; last publication {}.",
            link.workspace_id,
            link.remote,
            link.name,
            link.participant,
            langspan::remote::peer_ref(link),
            if link.publish_as_branch {
                ", a branch everyone in the repository can see"
            } else {
                ""
            },
            if link.share_task {
                "shared"
            } else {
                "not shared"
            },
            link.sequence
        ),
        Outcome::RemoteDisabled { workspace_id } => println!(
            "Workspace {workspace_id} no longer publishes. The daemon removes its ref from the remote on its next pass; `langspan doctor` shows whether the remote has let it."
        ),
        Outcome::RemoteOutbox {
            publication,
            changed,
            ..
        } => {
            println!(
                "{} This is everything that leaves the machine for this workspace:",
                if *changed {
                    "Not yet published."
                } else {
                    "Matches the last publication."
                }
            );
            println!("{}", serde_json::to_string_pretty(publication)?);
        }
        Outcome::RemoteSent { sent, more } => {
            if sent.is_empty() {
                println!("Nothing has been published for this workspace.");
            }
            for record in sent {
                println!(
                    "Sequence {} at {} (event {}):",
                    record.sequence,
                    record
                        .published_at_ms
                        .map_or_else(|| "an unrecorded time".into(), utc),
                    record.seq
                );
                println!(
                    "{}",
                    record
                        .payload
                        .as_deref()
                        .unwrap_or("(payload not recorded: published before payloads were kept)")
                );
            }
            if *more {
                println!("Older publications were left out; ask for fewer with --limit.");
            }
        }
        Outcome::RemoteObserved {
            participant,
            accepted,
        } => println!("Publication from {participant}: accepted {accepted}."),
        Outcome::CheckRecorded { check } => {
            println!("Check {} recorded for tree {}.", check.name, check.tree_oid)
        }
        Outcome::Checks { checks } => println!("{} check record(s).", checks.len()),
        Outcome::Readiness { ready, reason, .. } => println!("Ready: {ready}. {reason}."),
        Outcome::Landed { landing } => println!(
            "Landing {} on {} recorded.",
            landing.commit_oid, landing.onto
        ),
        Outcome::Turn { turn } => println!(
            "Push turn {} for {} held until {} ms.",
            turn.turn_id, turn.onto, turn.expires_at_ms
        ),
        Outcome::TurnBusy { holder, held_ms } => println!(
            "Another landing holds the push turn for {}: turn {}, for {held_ms} ms.",
            holder.onto, holder.turn_id
        ),
        Outcome::TurnWaiting {
            position, waiting, ..
        } => println!(
            "Waiting for the push turn: number {position} of {} in line.",
            waiting.len()
        ),
        Outcome::TurnUnavailable {
            waiting,
            queue_full,
            ..
        } => println!(
            "The push turn is not available: {} landing(s) in line{}.",
            waiting.len(),
            if *queue_full {
                ", and the line is full"
            } else {
                ""
            }
        ),
        Outcome::TurnQueue {
            holder, waiting, ..
        } => println!(
            "Push turn {}; {} landing(s) in line.",
            holder.as_ref().map_or("free".to_owned(), |turn| format!(
                "held by {}",
                turn.turn_id
            )),
            waiting.len()
        ),
        Outcome::TurnQueueLeft { ticket, left } => println!(
            "Landing {ticket} {}.",
            if *left {
                "stopped waiting for the push turn"
            } else {
                "was not waiting for the push turn"
            }
        ),
        Outcome::TurnReleased { turn_id, released } => println!(
            "Push turn {turn_id} {}.",
            if *released {
                "released"
            } else {
                "had already ended"
            }
        ),
        Outcome::InterfaceObserved { notice } => println!(
            "Interface change in {:?}: {} symbol(s), {} file(s) mention them.",
            notice.path,
            notice.changes.len(),
            notice.dependents.len()
        ),
        Outcome::CallBreaks { ready: false, .. } => {
            println!("Not checked: the source index has no complete scan of this workspace yet.")
        }
        Outcome::CallBreaks { breaks, .. } => {
            for found in breaks {
                println!(
                    "{}:{} {} passes {} argument(s); {} in {} changed by {}: {}",
                    found.call.path,
                    found.call.line,
                    found.call.name,
                    found.call.args,
                    found.call.name,
                    found.defined_in,
                    found.by_session,
                    found.after
                );
            }
            println!(
                "{} call(s) that may not fit; matched by name, not resolved.",
                breaks.len()
            );
        }
        Outcome::InterfaceAcknowledged {
            session_id,
            notice_ids,
        } => println!(
            "Session {session_id} will not be alerted again to {} notice(s).",
            notice_ids.len()
        ),
        Outcome::GraphContext { matches, coverage } => {
            for context in matches {
                for symbol in &context.definitions {
                    println!(
                        "{}:{} {} {} — {}",
                        symbol.path, symbol.line, symbol.kind, symbol.name, symbol.signature
                    );
                }
                println!(
                    "{} is mentioned in: {}{}",
                    context.name,
                    context.mentioned_in.join(", "),
                    if context.truncated { " (and more)" } else { "" }
                );
            }
            println!(
                "Matches are by name, not resolved. Index: {} of {} supported files at their current content.",
                coverage.current_files, coverage.indexable_files
            );
        }
        Outcome::GraphBlindSpots {
            blind_spots,
            coverage,
            scan_failure,
            truncated,
        } => print_blind_spots(blind_spots, coverage, scan_failure.as_ref(), *truncated),
        Outcome::Graph {
            symbols,
            references,
            coverage,
            truncated,
            ..
        } => {
            for symbol in symbols {
                println!(
                    "{}:{} {} {}{} — {}",
                    symbol.path,
                    symbol.line,
                    symbol.kind,
                    symbol.name,
                    if symbol.exported { " (exported)" } else { "" },
                    symbol.signature
                );
                print_symbol_detail(symbol);
            }
            for reference in references {
                println!(
                    "{}:{} mentions {}",
                    reference.path, reference.line, reference.name
                );
            }
            if *truncated {
                println!("More results exist than the limit allows.");
            }
            if !coverage.scanned {
                println!(
                    "Nothing is indexed for this workspace yet; an empty answer means nothing."
                );
            } else {
                println!(
                    "Matches are by name, not resolved uses. Index: {} of {} supported files at their current content{}.",
                    coverage.current_files,
                    coverage.indexable_files,
                    if coverage.partial_files > 0 {
                        format!(", {} only partly parsed", coverage.partial_files)
                    } else {
                        String::new()
                    }
                );
                if coverage.blind_in_scope > 0 {
                    println!(
                        "{} supported file(s) cannot be read in full, so an empty answer is weak; `langspan graph blind-spots` names them.",
                        coverage.blind_in_scope
                    );
                }
            }
        }
        Outcome::Remembered { note } => println!(
            "Remembered as {}. Forget it with `langspan forget {} {}`.",
            note.note_id, note.workspace_id, note.note_id
        ),
        Outcome::Forgotten { note_id, removed } => println!(
            "{}",
            if *removed {
                format!("Forgot {note_id}.")
            } else {
                format!("No note {note_id} to forget.")
            }
        ),
        Outcome::MessageSent { message } => println!(
            "{} {}. {}",
            match message.to {
                langspan_core::Recipient::NextSession {} => "Handed off as",
                _ => "Sent as",
            },
            message.message_id,
            match &message.to {
                langspan_core::Recipient::Session { session_id } => format!(
                    "Session {session_id} is told at its next prompt, or after its next tool call on a host that accepts context mid-turn."
                ),
                langspan_core::Recipient::Everyone {} => format!(
                    "Every other session of {} is told once, at its next prompt or start.",
                    message.workspace_id
                ),
                langspan_core::Recipient::NextSession {} => format!(
                    "The next other session to start or be prompted in {} is told.",
                    message.workspace_id
                ),
            }
        ),
        Outcome::MessageAcknowledged {
            message_id,
            acknowledged,
        } => println!(
            "{}",
            if *acknowledged {
                format!("Acknowledged {message_id}; it is not delivered to this session again.")
            } else {
                format!("Message {message_id} was not waiting for this session.")
            }
        ),
        Outcome::ScanObserved {
            workspace_id,
            failed,
        } => println!("Scan of {workspace_id} recorded; failed: {failed}."),
        Outcome::HostsObserved { not_installed } => {
            println!("Hosts not installed: {}.", not_installed.join(", "))
        }
        Outcome::CheckoutObserved { change } => println!(
            "Checkout change recorded for {}: {}.",
            change.workspace_id,
            langspan::hook::checkout_text(
                change.from_branch.as_deref(),
                change.to_branch.as_deref(),
                change.from_head.as_deref(),
                change.to_head.as_deref(),
                change.files
            )
        ),
        Outcome::Today { today } => {
            println!(
                "{}",
                langspan::today::text(today, &langspan::today::shown(&today.window))
            );
        }
        Outcome::StatusLine { line } => println!("{}", line.text()),
        Outcome::RefusalRecorded { session_id } => {
            println!("Refusal recorded for session {session_id}.")
        }
        Outcome::Recap { recap } => {
            println!(
                "Workspace {} at {:?}{}.",
                recap.workspace_id,
                recap.root,
                recap
                    .branch_ref
                    .as_ref()
                    .map(|branch| format!(" on {branch}"))
                    .unwrap_or_default()
            );
            if let Some(failure) = &recap.scan_failure {
                println!(
                    "The last scan failed ({}); file facts and the index below are as of the last complete scan.",
                    failure.detail
                );
            }
            if let Some(coverage) = &recap.scan_coverage {
                println!("{}", coverage_text(coverage));
            }
            for session in &recap.sessions {
                println!(
                    "Session {}{}{}:{}{}{}",
                    session.session_id,
                    if session.current {
                        ""
                    } else {
                        " (from before the coordinator restarted)"
                    },
                    if session.ended { " (ended)" } else { "" },
                    if session.editing.is_empty() {
                        String::new()
                    } else {
                        format!(" editing {:?};", session.editing)
                    },
                    if session.recent.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " edited {:?};",
                            session
                                .recent
                                .iter()
                                .map(|recent| &recent.path)
                                .collect::<Vec<_>>()
                        )
                    },
                    session
                        .summary
                        .as_ref()
                        .map(|summary| format!(
                            " task {}",
                            serde_json::to_string(summary).unwrap_or_default()
                        ))
                        .unwrap_or_default()
                );
            }
            if recap.omitted_sessions > 0 {
                println!("({} more session(s) not shown.)", recap.omitted_sessions);
            }
            if recap.omitted_items > 0 {
                println!(
                    "({} more item(s) left out to fit one response; see `langspan events` for all.)",
                    recap.omitted_items
                );
            }
            for notice in &recap.interface_changes {
                println!(
                    "Interface change in {:?}: {} symbol(s); {} file(s) mention them.",
                    notice.path,
                    notice.changes.len(),
                    notice.dependents.len()
                );
            }
            for check in &recap.checks {
                println!(
                    "Check {} on tree {}: {:?}{}.",
                    check.name,
                    check.tree_oid,
                    check.result,
                    if check.changed_since {
                        "; the working tree has changed since"
                    } else {
                        ""
                    }
                );
            }
            for landing in &recap.landings {
                println!("Landed {} on {}.", landing.commit_oid, landing.onto);
            }
            for peer in &recap.teammates {
                println!(
                    "Teammate {:?}: {} session(s) published.",
                    peer.publication.name,
                    peer.publication.work.len()
                );
            }
            for message in &recap.messages {
                println!(
                    "Message {} from {} to {}: {}{}",
                    message.message_id,
                    message
                        .from
                        .as_ref()
                        .map(|session| format!("session {session}"))
                        .unwrap_or_else(|| "the owner".into()),
                    match &message.to {
                        langspan_core::Recipient::Session { session_id } =>
                            format!("session {session_id}"),
                        langspan_core::Recipient::Everyone {} => format!(
                            "every session ({} acknowledged so far)",
                            message.acknowledged.len()
                        ),
                        langspan_core::Recipient::NextSession {} =>
                            "the next session (handoff)".into(),
                    },
                    serde_json::to_string(&message.text)?,
                    if message.paths.is_empty() {
                        String::new()
                    } else {
                        format!(" about {:?}", message.paths)
                    }
                );
            }
            for note in &recap.notes {
                println!(
                    "Remembered ({}): {}{}",
                    note.note_id,
                    serde_json::to_string(&note.text)?,
                    if note.paths.is_empty() {
                        String::new()
                    } else {
                        format!(" about {:?}", note.paths)
                    }
                );
            }
        }
        Outcome::Recall { recall } => {
            if recall.notes.is_empty() {
                println!(
                    "No held note in {} holds every word of {}.",
                    recall.workspace_id,
                    serde_json::to_string(&recall.query)?
                );
            }
            // Note text is remembered repository text: quoted, so it reads
            // as data.
            for note in &recall.notes {
                println!(
                    "Remembered ({}{}): {}{}",
                    note.note_id,
                    note.author
                        .as_ref()
                        .map(|author| format!(", by {author}"))
                        .unwrap_or_default(),
                    serde_json::to_string(&note.text)?,
                    if note.paths.is_empty() {
                        String::new()
                    } else {
                        format!(" about {:?}", note.paths)
                    }
                );
            }
            if recall.truncated {
                println!("(More notes match; raise --limit, up to 50, or narrow the query.)");
            }
        }
        Outcome::Error { code, message } => println!("{code}: {}", serde_json::to_string(message)?),
        // The view is for the dashboard page; a terminal gets it as data.
        Outcome::Dashboard { dashboard } => println!("{}", serde_json::to_string(dashboard)?),
        Outcome::Events { events } => {
            for event in events {
                println!("{} {}", event.seq, serde_json::to_string(&event.event)?);
            }
        }
        Outcome::StatusRecords { records, next } => {
            for record in records {
                println!("{}", serde_json::to_string(record)?);
            }
            if let Some(next) = next {
                println!(
                    "More: langspan status --all --after {}",
                    serde_json::to_string(next)?
                );
            }
        }
        Outcome::Status { state, summary } => {
            println!("LangSpan generation {}", state.generation);
            for workspace in state.workspaces.values() {
                println!("Workspace {}: {:?}", workspace.workspace_id, workspace.root);
            }
            for session in state.sessions.values() {
                println!(
                    "Session {} in {}: {}",
                    session.session_id,
                    session.workspace_id,
                    if session.generation == state.generation {
                        "registered in this generation"
                    } else {
                        "stale; reattach with a new request ID"
                    }
                );
                match state.session_liveness.get(&session.session_id) {
                    Some(liveness) => println!(
                        "  Process {}: {:?} (observation generation {}, {} ms).",
                        liveness.process.pid,
                        liveness.status,
                        liveness.generation,
                        liveness.observed_at_ms
                    ),
                    None => {
                        println!("  Process liveness: unknown; registration is not a heartbeat.")
                    }
                }
            }
            for intent in state.intents.values() {
                println!(
                    "Intent {} [{}]: {} {:?}",
                    intent.session_id,
                    if intent.generation == state.generation {
                        "current"
                    } else {
                        "stale"
                    },
                    serde_json::to_string(&intent.summary)?,
                    intent.paths
                );
            }
            for lease in state.leases.values() {
                println!(
                    "Admission {}: {} {:?} {:?}",
                    lease.lease_id, lease.session_id, lease.status, lease.paths
                );
            }
            let active = state
                .leases
                .values()
                .filter(|l| l.status == LeaseStatus::Active)
                .count();
            println!(
                "{active} active admission(s). Session registration is not a process-liveness signal."
            );
            for host in state.uninstalled_hosts.keys() {
                println!("{}", langspan::hosts::status_line(host, state_dir));
            }
            if let Some(summary) = summary {
                let closed = summary.closed_admissions.values().sum::<u64>() + summary.other_closed;
                if closed > 0 {
                    let mut counts: Vec<String> = summary
                        .closed_admissions
                        .iter()
                        .map(|(session, count)| format!("{session} {count}"))
                        .collect();
                    if summary.other_closed > 0 {
                        counts.push(format!("other sessions {}", summary.other_closed));
                    }
                    println!(
                        "{closed} closed admission(s), not listed: {}.",
                        counts.join(", ")
                    );
                }
                if summary.omitted_sessions > 0 {
                    println!(
                        "{} session(s) not shown: nothing open and idle for over {} minutes, or left out to fit.",
                        summary.omitted_sessions,
                        langspan_core::STATUS_RECENT_MS / 60_000
                    );
                }
                if summary.omitted_open > 0 {
                    println!(
                        "{} open admission(s) left out to fit one answer.",
                        summary.omitted_open
                    );
                }
                if !summary.omitted.is_empty() {
                    println!("Left out to fit: {}.", summary.omitted.join(", "));
                }
                if closed > 0
                    || summary.omitted_sessions > 0
                    || summary.omitted_open > 0
                    || !summary.omitted.is_empty()
                {
                    println!("Every record, history included: langspan status --all");
                }
            }
            for link in state.remote_links.values() {
                println!(
                    "{} {} to {:?} at {} as {:?}; last publication {}.",
                    match (link.enabled, link.withdrawn) {
                        (true, _) => "Publishing",
                        (false, true) => "No longer publishing",
                        (false, false) =>
                            "Stopped publishing; its ref is still to be removed from the remote:",
                    },
                    link.workspace_id,
                    link.remote,
                    langspan::remote::peer_ref(link),
                    link.name,
                    link.sequence
                );
            }
            for peer in state.remote_peers.values() {
                println!(
                    "Teammate {:?} ({}): publication {}, {} session(s), received in generation {} at {} ms.",
                    peer.publication.name,
                    peer.publication.participant,
                    peer.publication.sequence,
                    peer.publication.work.len(),
                    peer.generation,
                    peer.received_ms
                );
            }
        }
    }
    Ok(())
}

/// Unix milliseconds as an ISO 8601 UTC time, for people.
fn utc(ms: u64) -> String {
    let seconds = ms / 1000;
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = langspan::today::civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod time_tests {
    #[test]
    fn unix_milliseconds_are_shown_as_utc() {
        assert_eq!(super::utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::utc(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(super::utc(1_795_521_906_123), "2026-11-24T12:05:06Z");
    }
}
