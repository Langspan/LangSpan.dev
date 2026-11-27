#[path = "cli/check.rs"]
mod check;
#[path = "cli/flags.rs"]
mod flags;
#[path = "cli/hook.rs"]
mod hook;
#[path = "cli/land.rs"]
mod land;

use check::{auto_check, evidence, flaky_tests, unique};
use flags::{ParsedOptions, absolute_option, flags_of, number, parse_options, state_dir};
use land::{integrate, landing_queue};
use langspan::cli::options::no_unused_options;
use langspan::cli::render::{print_human, print_project_hook_reruns};
use langspan::cli::setup::{
    Mode, guidance, hooks, hosts_report, setup, start_coordinator, user_install,
};
use langspan::cli::{ask, doctor::doctor};
use langspan::{client, hosts::UserHome, protocol::Request};
use langspan_core::{Command, Outcome};
use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

const HELP: &str = r#"LangSpan — local cooperative coordination for AI coding agents

Usage: langspan [--state-dir DIR] COMMAND [--json]

  setup [--home DIR] [--hosts ...] [--yes] [--verbose]
                                 From nothing to coordinated sessions: shows one plan
                                 (the hosts found, the files `install --skill` changes,
                                 the coordinator's login service and the state
                                 directory), asks once, applies it, starts the
                                 coordinator and runs `doctor`. --yes applies without
                                 asking; without a terminal and without --yes it only
                                 prints the plan. The plan lists each file; --verbose
                                 shows each file's full text. A second run changes nothing
  demo                           Show a real refusal with no agent: two example
                                 sessions edit one file in a temporary workspace,
                                 with a coordinator of the demo's own. Removes
                                 everything it made
  daemon                         Run the local coordinator in the foreground
  mcp [--tools all|hooked --host claude-code|codex]
                                 Serve MCP over stdin/stdout using the coordinator.
                                 --tools hooked leaves the four tools the hooks call
                                 out of the list while that host's hooks report here;
                                 each still answers when called
  dashboard [--listen 127.0.0.1:7311] [--open] [--allow-network] [--demo]
                                 Serve a live, read-only page of every agent on this
                                 machine and every teammate heard from, on loopback.
                                 Prints the URL with this run's access token; --open
                                 also opens it in the browser. A non-loopback address
                                 is refused without --allow-network; from a phone, use
                                 an SSH tunnel or a VPN to this machine instead.
                                 --demo serves the same page with fictional, labeled
                                 simulated data and reads nothing from the coordinator
  workspace add ID ROOT          Register a physical workspace
  workspace list [--after ID]    Every workspace: root and whether it exists, current
                                 sessions, notes, pending messages, check records,
                                 automatic check and remote sharing; a page at a
                                 time, in ID order, naming the next page's command
  workspace move ID NEW_ROOT [--switch] --request-id ID
                                 Rebind a workspace to a checkout you moved (the old
                                 root is gone), or with --switch to another checkout of
                                 the same repository. Notes, messages, checks,
                                 landings and settings are kept. Prints the project
                                 hook installs to rerun; edits no host configuration
  workspace remove ID [--apply] [--request-id ID]
                                 Dry run: what would end. With --apply, take the
                                 workspace out of status, recap, briefings and the
                                 dashboard; its events stay. Refused while a session
                                 holds an admission there, a landing holds or waits
                                 for its push turn, or remote sharing is on
  session start ID WORKSPACE      Register or reattach a logical session
  session attach-pid SESSION PID  Observe an owned process incarnation (Linux, macOS)
  intent set SESSION PATH...     Declare work; optional --summary TEXT
  intent end SESSION             End a work intention
  admit SESSION PATH...          Request a whole file set; --ttl-ms defaults to 5000
  complete SESSION LEASE         Complete a current admission
  brief SESSION                  Look at the peer work a session would be told about;
                                 optional --max-peers N (1..16); --recent adds what changed
                                 recently, as told at a session's start. Does not count as delivery
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
    [COMMAND... --on-interface-change NAME2 [--timeout-s N] -- COMMAND2...]
                                 When the writes changed an indexed export that other
                                 files mention, COMMAND2 runs whole after COMMAND on the
                                 same tree, recorded as NAME2 and graded kept or broke
  check flaky WORKSPACE [--name NAME] | --forget TEST [--name NAME] --request-id ID
                                 Tests that flipped on unchanged code (two results on
                                 one tree, no write between); --forget clears one
  integrate check WORKSPACE --onto BRANCH --name NAME [--symbols] BRANCH...
       -- COMMAND...
                                 Combine branches onto the remote's BRANCH without
                                 touching your checkout, run the command on that exact
                                 result, and record it. Reports conflicts instead,
                                 and functions BRANCH renamed that a branch still
                                 calls. [--rebase] [--adapt-renames]: as for land.
                                 --symbols also names definitions the branches changed
                                 in ways that disagree (advisory; never the result)
  land WORKSPACE --onto BRANCH --name NAME BRANCH... [--apply] [--session ID]
       [--wait-s N] [--rebase] [--adapt-renames] [--check-again -- COMMAND...]
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
                                 --rebase replays the branch's commits onto BRANCH
                                 instead of merging (linear history). --adapt-renames
                                 rewrites the branch's uses of names BRANCH renamed in
                                 one new commit on top of it, only when that is
                                 certain. Your branches are never changed.
                                 Optional --remote (default origin), --request-id
  land-queue WORKSPACE --onto BRANCH
                                 Who holds BRANCH's push turn and who waits for it
  land-cancel WORKSPACE --onto BRANCH --request-id ID
                                 Stop the waiting landing ID; it pushes nothing
  graph symbol WORKSPACE NAME    Where a name is defined (Rust, Python, TypeScript, Go, Java, C#)
                                 [--detail]: with its body digest, calls, callers
                                 by name, notes on its file and last check;
                                 [--renamed]: under the newest name a recorded
                                 rename of NAME leads to
  graph references WORKSPACE NAME  Where a name is mentioned
  graph impact WORKSPACE PATH    What a file exports and which files mention those names.
                                 [--depth N]: 1 to 3 levels, each the files that
                                 mention an export of the level before
                                 All three match by name and report index coverage
  graph path WORKSPACE FROM TO [--max-hops N]
                                 The shortest chains of calls from FROM to TO, at
                                 most 5, each N (1 to 6, default 4) calls at most;
                                 found by name and by import path, not type-checked
  graph map WORKSPACE [--depth D]
                                 Areas at depth D (1 to 3): files, exports and
                                 languages, and the imports and calls between them
  graph blind-spots WORKSPACE [--limit N]
                                 Files the index cannot answer for: partial, never
                                 parsed cleanly, waiting, too large, left out of the
                                 scan, and other languages by extension
  graph inflight WORKSPACE       Exported names other worktrees of the repository
                                 added or changed and not committed, that this
                                 workspace's files mention or import
  graph export WORKSPACE --out FILE
                                 Write the source index to a new owner-only FILE
                                 (JSON Lines); it never replaces an existing file
  graph import WORKSPACE --in FILE [--apply]
                                 Dry run: how many exported files have the same
                                 content here. --apply loads those the index does
                                 not hold yet; the rest are indexed from disk
  check-write SESSION PATH... [--symbol NAME]...
                                 Before a write, for a host without hooks: per PATH
                                 (1 to 8) who holds it, lines other sessions wrote in
                                 the last 30 minutes and whether it changed since
                                 SESSION read it; per NAME (up to 16, or without any,
                                 the definitions in PATH) a peer's recent signature
                                 change, removal or body lines, matched by name.
                                 Records nothing
  graph history WORKSPACE NAME [--path PATH] [--limit N] [--before SEQ]
                                 The recorded interface changes of definitions named
                                 NAME, newest first (--limit 1 to 50, default 10):
                                 before and after, the last session that reported
                                 editing the file (not proof of authorship), its task
                                 line, notes and messages naming the file within 30
                                 minutes, and changes back to an earlier signature.
                                 Reads only
  recap WORKSPACE [--tag TAG]    A bounded account of one workspace: sessions, recent
                                 edits, interface changes, checks, landings, notes and
                                 messages not yet acknowledged, then withdrawn ones.
                                 --tag lists only the notes carrying TAG.
                                 --since-away (or --since-seq SEQ) adds what happened
                                 since the last interactive session before a gap of
                                 2 hours (LANGSPAN_AWAY_AFTER_MS), or since event SEQ
  trail WORKSPACE [--limit N]    Trails ended sessions left, newest first (--limit 1
                                 to 16, default 4): task line, files read and edited
                                 with line ranges, checks after the last write, files
                                 left uncommitted, and who was told. Reads only
  today [WORKSPACE] [--since RFC3339]
                                 What each session did since local midnight (or the
                                 time given), refusals by kind, checks, landings, and
                                 what is still open. Reads only; acknowledges nothing
  report WORKSPACE [--since RFC3339]
                                 Per session and per host kind: edits admitted, those
                                 not compared with a read, shell writes seen only
                                 after, reads, intents set/ended/left open, refusals,
                                 unchanged retries, briefings. Counts of recorded
                                 events (all of them without --since). Reads only
  count [--month YYYY-MM]        Refusals in one local month (default: this one), by
                                 kind and workspace, once per session, kind and files
                                 a day, and how many were retried and let through.
                                 Says what LangSpan did, not what it saved. Reads only
  policy WORKSPACE               The coordination policy table in effect: each kind
                                 of case, advisory, wait or refuse, and whether it can
                                 be set. Reads only
  policy set WORKSPACE conflict refuse|wait [--wait-ms N] --request-id ID
                                 What a write meets when another session holds its file
                                 now: refuse at once (the default) or wait for the
                                 holder, up to N ms (0 to 2000, default 2000; 0 is
                                 refuse), never past half the hook's remaining budget,
                                 then compare the file again. Every other kind is fixed
  remember WORKSPACE TEXT [PATH...] [--session ID] [--symbol NAME] [--tag TAG]...
                                 Keep a one-line note for the repository or for files;
                                 sessions working there are told. Survives restarts.
                                 --symbol anchors it to the one indexed definition of
                                 NAME in PATH (or the workspace); briefings then say
                                 whether that code changed since. Up to 4 tags, each
                                 1 to 32 bytes of a-z, 0-9 and '-'
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
  adopt SESSION [--session ID] [--root DIR]
                                 Take over the uncommitted lines an ended session left
                                 in this workspace: status --mine and commit --mine then
                                 treat them as this session's; lines another session
                                 also wrote stay shared. Refused for a live session and
                                 for one of another workspace. Recorded as an event.
                                 Session found as for commit --mine
  message WORKSPACE TEXT [PATH...] [--to SESSION] [--from SESSION]
                                 Send one line to a session, or without --to to every
                                 other session of the workspace. Without --from it is
                                 from the owner. Each recipient is told once, at its
                                 next prompt or start (Claude Code: also mid-turn)
  handoff SESSION TEXT [PATH...] Leave one line for the next other session that starts
                                 or is prompted in SESSION's workspace
  ack SESSION MESSAGE            Mark a message seen so it is not delivered again
  messages show WORKSPACE MESSAGE
                                 One message in full, as it was sent, also after it
                                 was acknowledged: what a briefing line marked
                                 "(cut; ...)" left out. Read-only
  messages status WORKSPACE MESSAGE|last [--from SESSION]
                                 Which sessions were given a message, and how; `last`
                                 is the newest one SESSION (or the owner) sent.
                                 Delivered means placed in a session's context, not
                                 that it acted on it. Read-only
  messages retract WORKSPACE MESSAGE|last [--from SESSION]
                                 Withdraw a message or handoff: copies not yet
                                 delivered are removed, and each session that received
                                 it is told once, at its next briefing, to disregard
                                 it. `last` is the sender's newest message. Without
                                 --from it is the owner, who may withdraw any message;
                                 a session may withdraw only its own
  doctor [--root DIR] [--home DIR]
                                 Say what is and is not working: coordinator, scans,
                                 index, publication, the hooks installed in DIR, and
                                 the user-level install under the home, each with
                                 whether the LangSpan skill is installed and current,
                                 and whether the coordinator's version matches the
                                 binary each hook names (read, never run)
  hosts [--home DIR]             List each agent host LangSpan knows (Claude Code,
                                 Codex, Cursor, Antigravity): whether its configuration
                                 directory exists, its user-level install (none,
                                 current, outdated, other_state_dir, unknown) and the
                                 command that installs it. Reads only
  guidance [--format agents|cursor-rules]
                                 Print LangSpan's block for AGENTS.md or a
                                 Cursor rule: which MCP tools an agent calls to
                                 coordinate. Without hooks it is advice, not a gate
  guidance install --root DIR --file FILE [--create] [--apply]
  guidance remove --root DIR --file FILE [--apply]
                                 Show (default) or write the block into FILE under DIR
                                 between <!-- langspan:begin --> and <!-- langspan:end
                                 -->, or take out only that block; every other byte of
                                 FILE stays. A missing FILE is created only with
                                 --create. A hand-edited block stops it, with its lines
  status                         Show workspaces, live or recent sessions, intents and
                                 open admissions; closed ones are counted per session
  status --mine [--session ID] [--root DIR]
                                 This session's uncommitted changes in a shared
                                 checkout, file by file and hunk by hunk: each file
                                 mine, mixed or unknown, each hunk mine, another
                                 session's, no session's or not known; at most 200
                                 files. What commit --mine would commit and refuse;
                                 changes nothing. Session found as for commit --mine
  notify on|off|status [--sound on|off]
                                 Desktop notices for the person (off by default): a
                                 refusal, a failed automatic check, a handoff not taken
                                 up for 10 minutes. Shown by osascript (macOS) or
                                 notify-send, at most one of a kind per 5 minutes and
                                 10 an hour; workspace, session and paths only, at most
                                 200 bytes. Sound off unless --sound on. Kept as notify
                                 and notify_sound in config.json; status names the
                                 notifier found on this PATH, or says none was
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
  compact [--keep-days N] [--apply]
                                 Delete events and receipts older than N days (default
                                 90, 7..3650) after snapshotting the state they built.
                                 A dry run unless --apply: shows what would go
  changes WORKSPACE [--after SEQ] [--limit N]
                                 Read disk and local-branch observations and scan freshness
  request JSON                   Send a complete version 3 request envelope
  hook claude-code|codex|cursor|antigravity [--event NAME]
       [--workspace ID --root DIR]
                                 PreToolUse/PostToolUse(Failure) adapter for Edit,
                                 Write, MultiEdit, NotebookEdit, Read, Grep and Bash
                                 (Codex: apply_patch and Bash; Cursor: Write, Delete,
                                 Shell, Read and Grep, built from documentation and
                                 not yet verified on a real host; Antigravity:
                                 PreToolUse and PostToolUse for edit_file,
                                 write_to_file, replace_file_content,
                                 multi_replace_file_content, run_command, read_file
                                 and view_file, whose events do not name themselves,
                                 so --event NAME is required for it alone); reads one
                                 event on stdin. Shell commands are covered partly
                                 (ADR 0011). `hook gemini-cli`, from an install made
                                 before Gemini CLI support was removed, does nothing
                                 and says how to remove it.
                                 Without --workspace and --root (the user-level form),
                                 the repository is the Git toplevel of the event's cwd;
                                 outside a repository, or where a project-level LangSpan
                                 hook covers it, the hook does nothing.
                                 Optional --ttl-ms (default 30000), --budget-ms (1500)
                                 SessionStart and UserPromptSubmit (Antigravity:
                                 PreInvocation) register the
                                 session and print relevant peer work for the model;
                                 --prompt-summary off keeps the prompt's first line
                                 out of what peers are told. At a prompt, names it
                                 mentions that the source index defines are listed
                                 with their files and signatures (matched by name;
                                 the prompt is used for that local lookup only and
                                 is not stored); --code-briefing off turns this off.
                                 Reads, a search's path and the files a shell command
                                 reads are shown to peers as where the session is
                                 looking (never a pattern or command text);
                                 --presence off stops that, --presence-minutes N
                                 (1-30, default 10) is how old a peer's is shown.
                                 --outline-reads on (off by default; Claude Code
                                 and Cursor) refuses a session's first whole read
                                 of a large indexed file once, with the file's
                                 definitions and their lines as the reason
  hooks install claude-code|codex|cursor|antigravity --workspace ID --root DIR
                                 [--settings FILE] [--prompt-summary off]
                                 [--code-briefing off] [--presence off]
                                 [--presence-minutes N] [--outline-reads on]
                                 [--skill] [--apply]
  hooks remove claude-code|codex|cursor|antigravity|gemini-cli --root DIR
               [--settings FILE] [--apply]
                                 Show (default) or apply the change to project Claude
                                 settings, Codex hooks, Cursor's .cursor/hooks.json and
                                 .cursor/mcp.json, or Antigravity's
                                 .agents/hooks.json and .agents/mcp_config.json,
                                 preserving other settings. gemini-cli removes an
                                 earlier install's hooks from .gemini/settings.json;
                                 Gemini CLI support was removed. Cursor and Antigravity
                                 only: --skill also writes the LangSpan skill as the
                                 project rule .cursor/rules/langspan.mdc or
                                 .agents/rules/langspan.md, which hooks remove removes
  install [--home DIR] [--hosts claude-code,codex,cursor,antigravity]
          [--prompt-summary off] [--presence off] [--presence-minutes N]
          [--outline-reads on] [--skill] [--statusline on|off]
          [--mcp-tools all|hooked] [--apply]
                                 Install the hooks once for every repository, in each
                                 host's user configuration, and register `langspan mcp`
                                 as the MCP server `langspan`. Hosts default to those
                                 whose configuration directory exists. Dry run unless
                                 --apply. --home DIR ignores HOME, CLAUDE_CONFIG_DIR,
                                 CODEX_HOME, GEMINI_CLI_HOME, LANGSPAN_STATE_DIR and
                                 XDG_STATE_HOME. --skill (off by default) also writes
                                 the LangSpan skill, which teaches an agent how to work
                                 with LangSpan: Claude Code skills/langspan/SKILL.md, a
                                 marked section of Codex's AGENTS.md, Antigravity's
                                 rule ~/.gemini/config/rules/langspan.md.
                                 Antigravity is chosen only by name in --hosts.
                                 --statusline on sets Claude Code's
                                 statusLine to `langspan statusline` where none is
                                 set (never replacing one); off removes LangSpan's.
                                 --mcp-tools hooked writes `mcp --tools hooked` for
                                 Claude Code and Codex (default all)
  uninstall [--home DIR] [--hosts ...] [--purge] [--apply]
                                 Remove only what install and setup wrote, the skill
                                 and the coordinator's login service included, and
                                 stop the coordinator. The state directory stays
                                 unless --purge. --hosts gemini-cli removes an
                                 earlier Gemini CLI install from ~/.gemini/settings.json
                                 and ~/.gemini/GEMINI.md

A hook or `mcp` that finds no coordinator, where no login service is installed for
its state directory, starts `langspan daemon` in the background and logs it to
daemon.log in the state directory. LANGSPAN_AUTOSTART=off turns that off.

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
        return hook::hook(input);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{}", error_line(&error.to_string(), &input));
            return ExitCode::FAILURE;
        }
    };
    let error = match runtime.block_on(run(input.clone())) {
        Ok(code) => return ExitCode::from(code),
        Err(error) => error.to_string(),
    };
    eprintln!("{}", error_line(&error, &input));
    ExitCode::FAILURE
}

/// A command's error for stderr: plain text for a person at a terminal,
/// one JSON line for a program or with --json (issue #291).
fn error_line(message: &str, input: &[String]) -> String {
    use std::io::IsTerminal;
    error_text(
        message,
        std::io::stderr().is_terminal() && !input.iter().any(|word| word == "--json"),
    )
}

fn error_text(message: &str, plain: bool) -> String {
    if plain {
        format!("langspan: {message}")
    } else {
        serde_json::json!({"level":"error","message":message}).to_string()
    }
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

/// `count`: refusals in one local month, read from the event log (H5).
/// The coordinator being down is an error (exit 1); no earlier figure is
/// shown instead.
fn refusal_count(
    state_dir: &std::path::Path,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    use langspan::count;
    let month = count::month(
        options.remove("--month").as_deref(),
        langspan::store::wall_now_ms(),
    )?;
    no_unused_options(options)?;
    let answer = count::ask(state_dir, month, count::day_starts(month)?)?;
    if json {
        println!("{}", serde_json::to_string(&answer)?);
    } else {
        println!("{}", answer.text(&month.label()));
    }
    Ok(0)
}

/// `notify on|off|status [--sound on|off]`: desktop notices (H3). Writes
/// only `notify` and `notify_sound` in `config.json`; the daemon reads them
/// at each notice.
fn notify(
    state_dir: &std::path::Path,
    action: &str,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    use langspan::notify;
    let sound = match options.remove("--sound").as_deref() {
        None => None,
        Some("on") => Some(true),
        Some("off") => Some(false),
        Some(other) => return Err(format!("--sound takes on or off, not {other:?}").into()),
    };
    no_unused_options(options)?;
    let (on, sound) = match (action, sound) {
        ("on" | "off", sound) => notify::configure(state_dir, action == "on", sound)?,
        ("status", None) => notify::configured(state_dir)?,
        ("status", Some(_)) => {
            return Err("--sound is valid with `notify on` and `notify off`".into());
        }
        _ => return Err(format!("notify takes on, off or status, not {action:?}").into()),
    };
    let found = notify::find(std::env::var_os("PATH").as_deref());
    let program = notify::Program::here().name();
    if json {
        let notifier = found.as_ref().map(|(program, path)| {
            serde_json::json!({"name": program.name(), "path": path.to_string_lossy()})
        });
        println!(
            "{}",
            serde_json::json!({"notify": on, "notify_sound": sound, "notifier": notifier})
        );
        return Ok(0);
    }
    let word = |on: bool| if on { "on" } else { "off" };
    println!("notices: {}", word(on));
    println!("sound: {}", word(sound));
    match &found {
        Some((program, path)) => println!("notifier: {} ({})", program.name(), path.display()),
        None => println!("notifier: none found (no {program} on PATH); no notice can be shown"),
    }
    Ok(0)
}

/// `adopt SESSION`: take over an ended session's uncommitted lines (issue
/// #161). The adopting session is `--session`, or the one running this
/// command through its shell tool.
fn adopt(
    state_dir: &std::path::Path,
    from: &str,
    options: &mut BTreeMap<String, String>,
    json: bool,
) -> Result<u8, langspan::Error> {
    let root = match options.remove("--root") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()?,
    };
    let session = options.remove("--session");
    let request_id = options.remove("--request-id");
    no_unused_options(options)?;
    let session = match session {
        Some(session) => session,
        None => langspan::commit::running_session(state_dir, &root)?,
    };
    let request_id = request_id.unwrap_or_else(|| unique("adopt", &[&session, from]));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let report = langspan::adopt::adopt(state_dir, &request_id, &session, from, deadline)?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", report.text());
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
        if json {
            return Err("--json is not valid for `demo`".into());
        }
        if options.contains_key("--state-dir") {
            return Err(
                "`demo` uses a temporary state directory of its own; leave out --state-dir".into(),
            );
        }
        no_unused_options(&options)?;
        let executable = std::env::current_exe()?;
        let mut stdout = std::io::stdout().lock();
        langspan::demo::run(&executable, &mut stdout)?;
        return Ok(0);
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
        let demo = options.remove("--demo").is_some();
        no_unused_options(&options)?;
        let options = langspan::dashboard::Options {
            listen,
            open,
            allow_network,
            demo,
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
    if words == ["daemon"] {
        no_unused_options(&options)?;
        langspan::daemon::run(state_dir).await?;
        return Ok(0);
    }
    if words == ["mcp"] {
        let list = langspan::mcp::ToolList::parse(
            options.remove("--tools").as_deref(),
            options.remove("--host").as_deref(),
        )?;
        no_unused_options(&options)?;
        let dir = state_dir.clone();
        tokio::task::spawn_blocking(move || {
            start_coordinator(
                &dir,
                std::time::Instant::now() + std::time::Duration::from_secs(2),
            )
        })
        .await?;
        langspan::mcp::serve(state_dir, list).await?;
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

#[cfg(test)]
mod time_tests {
    #[test]
    fn an_error_is_plain_text_for_a_person_and_json_for_a_program() {
        assert_eq!(
            super::error_text("no \"x\" here; do y", true),
            "langspan: no \"x\" here; do y"
        );
        assert_eq!(
            super::error_text("no \"x\" here; do y", false),
            r#"{"level":"error","message":"no \"x\" here; do y"}"#
        );
    }
}
