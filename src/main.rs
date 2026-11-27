use langspan::cli::options::{no_unused_options, outline_reads, presence, switch};
use langspan::cli::render::{
    print_combined, print_conflict, print_human, print_inconclusive, print_not_rewritten,
    print_notes, print_project_hook_reruns, print_renames, print_replay_conflict,
};
use langspan::cli::setup::{
    Mode, guidance, hooks, hosts_report, setup, start_coordinator, user_install,
};
use langspan::cli::{ask, doctor::doctor};
use langspan::{
    client,
    hosts::UserHome,
    protocol::{PROTOCOL_VERSION, Request, Response},
};
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
        return hook(input);
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

fn hook(input: Vec<String>) -> ExitCode {
    use std::os::unix::fs::OpenOptionsExt;
    // An earlier release's Gemini CLI hook: allowed, nothing coordinated or
    // recorded, whatever its options say (issue #246).
    if input
        .get(1)
        .and_then(|name| langspan::install::Vendor::from_name(name))
        .is_some_and(langspan::install::Vendor::removed)
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        let event = langspan::hook::read_stdin_until(deadline).unwrap_or_default();
        let root = input
            .windows(2)
            .find(|pair| pair[0] == "--root")
            .map(|pair| pair[1].as_str());
        print!("{}", langspan::gemini_removed::ANSWER);
        if let Some(notice) = langspan::gemini_removed::notice_for(&event, root) {
            eprintln!("{notice}");
        }
        return ExitCode::SUCCESS;
    }
    // Antigravity documents a JSON answer for every event, and 1.5.14 reads
    // `{}` on `PreToolUse` as a refusal, so it gets its event's fallback:
    // the explicit allow there, else `{}`.
    let fallback: Option<&'static str> = match input.get(1).map(String::as_str) {
        Some("antigravity") => Some(langspan::antigravity::fallback(
            input
                .windows(2)
                .find(|pair| pair[0] == "--event")
                .map(|pair| pair[1].as_str()),
        )),
        _ => None,
    };
    // The between-step watcher runs in the background, where exit 2 wakes
    // the model: it exits 2 only to tell a notice, and 0 for every failure,
    // its options' included (issue #288).
    let background_watcher = input.get(1).map(String::as_str) == Some("claude-code")
        && input
            .windows(2)
            .any(|pair| pair[0] == "--mode" && pair[1] == langspan::between_steps::MODE);
    if let Some(fallback) = fallback {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = stdout
                .write_all(fallback.as_bytes())
                .and_then(|()| stdout.flush());
            default(info);
        }));
    }
    let result = (|| -> Result<u8, langspan::Error> {
        let ParsedOptions {
            args, mut options, ..
        } = parse_options(input)?;
        let vendor = match args.as_slice() {
            [hook, name] if hook == "hook" => langspan::install::Vendor::from_name(name),
            _ => None,
        }
        .ok_or(
            "usage: langspan hook claude-code|codex|cursor|antigravity [--event NAME] [--workspace ID --root DIR]",
        )?;
        // Only a host whose events do not name themselves takes `--event`,
        // and it must be one the install registers.
        let event = match (vendor.names_event(), options.remove("--event")) {
            (true, Some(event)) if vendor.hook_events().contains(&event.as_str()) => Some(event),
            (true, Some(event)) => {
                return Err(format!(
                    "--event {event:?} is not an event LangSpan handles for {}",
                    vendor.name()
                )
                .into());
            }
            (true, None) => {
                return Err(format!("{} hooks need --event NAME", vendor.name()).into());
            }
            (false, Some(_)) => {
                return Err(format!("--event is not used for {}", vendor.name()).into());
            }
            (false, None) => None,
        };
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
        let presence = presence(&mut options)?;
        // The between-step watcher (issue #214): Claude Code only.
        let watcher = match options.remove("--mode") {
            None => false,
            Some(mode)
                if mode == langspan::between_steps::MODE
                    && vendor == langspan::install::Vendor::ClaudeCode =>
            {
                true
            }
            Some(mode) => {
                return Err(format!(
                    "--mode {mode:?} is not a hook mode for {}; only claude-code takes --mode {}",
                    vendor.name(),
                    langspan::between_steps::MODE
                )
                .into());
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
            presence: presence.shown,
            presence_window_ms: presence.window_ms(),
            outline_reads: outline_reads(&mut options)?,
        };
        no_unused_options(&options)?;
        if !(1..=60_000).contains(&config.ttl_ms)
            || !(1..=4000).contains(&config.budget.as_millis())
        {
            return Err("hook limits: --ttl-ms 1..60000 and --budget-ms 1..4000".into());
        }
        let deadline = std::time::Instant::now() + config.budget;
        let input = langspan::hook::read_stdin_until(deadline);
        // A watcher starts no coordinator and writes nothing to stdout. On
        // exit 2 its stderr is the notice Claude Code shows the model, so
        // the log line is written only when it does not wake the session.
        if watcher {
            let watched = match &input {
                // A panic is a bug, but must not become the host's error.
                Ok(bytes) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    langspan::between_steps::watch(bytes, &config)
                }))
                .unwrap_or_else(|_| langspan::between_steps::Watched {
                    exit: 0,
                    stderr: String::new(),
                    detail: "the watcher stopped on an internal error (a panic); report it".into(),
                }),
                Err(error) => langspan::between_steps::Watched {
                    exit: 0,
                    stderr: String::new(),
                    detail: error.to_string(),
                },
            };
            if watched.exit == langspan::between_steps::WAKE {
                eprint!("{}", watched.stderr);
                return Ok(langspan::between_steps::WAKE);
            }
            eprintln!(
                "{}",
                serde_json::json!({"level":"info","event":"langspan_between_steps","detail":watched.detail})
            );
            return Ok(0);
        }
        // At most half of what is left, so the event itself keeps the rest.
        let now = std::time::Instant::now();
        start_coordinator(
            &config.state_dir,
            now + deadline.saturating_duration_since(now) / 2,
        );
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
                // Answered before any of this, above.
                langspan::install::Vendor::Gemini => langspan::hook::Reply {
                    stdout: langspan::gemini_removed::ANSWER.into(),
                    ..langspan::hook::Reply::none("unsupported", "Gemini CLI support was removed")
                },
                langspan::install::Vendor::Antigravity => langspan::antigravity::run_until(
                    bytes.as_slice(),
                    &config,
                    event.as_deref().unwrap_or_default(),
                    deadline,
                ),
                langspan::install::Vendor::ClaudeCode => {
                    langspan::claude_code::run_until(bytes.as_slice(), &config, deadline)
                }
            },
            Err(error) => langspan::hook::Reply {
                stdout: fallback.unwrap_or_default().into(),
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
        Ok(0)
    })();
    match result {
        Ok(code) => ExitCode::from(code),
        // Exit 1 is a non-blocking error for Claude Code. Only the background
        // between-step watcher exits 2, to wake the model (above).
        Err(error) if background_watcher => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","event":"langspan_between_steps","detail":error.to_string()})
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            if let Some(fallback) = fallback {
                print!("{fallback}");
            }
            eprintln!(
                "{}",
                serde_json::json!({"level":"error","event":"langspan_hook","coverage":"unknown","message":error.to_string()})
            );
            ExitCode::FAILURE
        }
    }
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

/// `check flaky`: the tests known to flip on unchanged code, or, with
/// `--forget`, clear what was learned about one of them.
fn flaky_tests(
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

/// The flags one command takes, read once its words are known: a new flag
/// is added to its command's entry in [`COMMANDS`].
struct Flags {
    /// Whether the command's words name this entry: all of them, and
    /// those before a `--`.
    form: fn(&[&str], &[&str]) -> bool,
    /// Flags without a value.
    switches: &'static [&'static str],
    /// Flags that take a value.
    values: &'static [&'static str],
    /// Whether `run` lets `--apply` through to the command.
    apply: bool,
    symbol: Repeat,
    tag: Repeat,
}

/// How often a command takes `--symbol` or `--tag`.
#[derive(Clone, Copy)]
enum Repeat {
    No,
    /// At most once; the text says why.
    Once(&'static str),
    Any,
}

impl Repeat {
    fn check(self, name: &str, given: usize) -> Result<(), langspan::Error> {
        match self {
            Repeat::No if given > 0 => {
                Err(format!("option {name} is not valid for this command").into())
            }
            Repeat::Once(why) if given > 1 => Err(why.into()),
            _ => Ok(()),
        }
    }
}

impl Flags {
    /// A command with no flags of its own.
    const NONE: Flags = Flags {
        form: |_, _| false,
        switches: &[],
        values: &[],
        apply: false,
        symbol: Repeat::No,
        tag: Repeat::No,
    };
}

/// Flags every command reads: `--state-dir`, and the `--request-id` of a
/// request to the coordinator. `--json`, `--apply`, `--symbol` and
/// `--tag` are the parser's own; `-m` is `--message`.
const EVERY_COMMAND: &[&str] = &["--state-dir", "--request-id"];

/// The installers' options for the hosts they set up.
const INSTALL_VALUES: &[&str] = &[
    "--home",
    "--hosts",
    "--prompt-summary",
    "--presence",
    "--presence-minutes",
    "--outline-reads",
    "--statusline",
    "--mcp-tools",
];

/// What each command takes. A form that decides `--apply`, `--symbol` or
/// `--tag` matches the words exactly as the command's dispatch in `run`
/// does; the first match wins.
const COMMANDS: &[Flags] = &[
    Flags {
        form: |words, _| words.first() == Some(&"hook"),
        values: &[
            "--event",
            "--workspace",
            "--root",
            "--mode",
            "--ttl-ms",
            "--budget-ms",
            "--prompt-summary",
            "--code-briefing",
            "--presence",
            "--presence-minutes",
            "--outline-reads",
        ],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["install"],
        switches: &["--skill"],
        values: INSTALL_VALUES,
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["uninstall"],
        switches: &["--skill", "--purge"],
        values: INSTALL_VALUES,
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| {
            matches!(words, ["hooks", "install" | "remove", host]
                if langspan::install::Vendor::from_name(host).is_some())
        },
        switches: &["--skill"],
        values: &[
            "--root",
            "--settings",
            "--workspace",
            "--prompt-summary",
            "--code-briefing",
            "--presence",
            "--presence-minutes",
            "--outline-reads",
        ],
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["setup"],
        switches: &["--yes", "--skill", "--verbose"],
        values: INSTALL_VALUES,
        ..Flags::NONE
    },
    Flags {
        form: |words, before| words.first() == Some(&"guidance") && words.len() == before.len(),
        switches: &["--create"],
        values: &["--format", "--root", "--file"],
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["doctor"],
        values: &["--root", "--home"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["hosts"],
        values: &["--home"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["check", "auto", _]),
        switches: &["--off"],
        values: &["--name", "--timeout-s"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["check", "flaky", _]),
        values: &["--name", "--forget"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["check", "run" | "status", _]),
        values: &["--name", "--timeout-s"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["integrate", "check", _, ..]),
        switches: &["--rebase", "--adapt-renames", "--symbols"],
        values: &["--name", "--onto", "--remote", "--timeout-s", "--session"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["land", _, ..]),
        switches: &["--check-again", "--rebase", "--adapt-renames"],
        values: &[
            "--name",
            "--onto",
            "--remote",
            "--timeout-s",
            "--session",
            "--wait-s",
        ],
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["land-queue" | "land-cancel", _]),
        values: &["--onto"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["statusline"],
        values: &["--root", "--deadline-ms"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["count"],
        values: &["--month"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| matches!(before, ["notify", _]),
        values: &["--sound"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["commit"],
        switches: &["--mine"],
        values: &["--message", "--root", "--session"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"adopt"),
        values: &["--root", "--session"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["status"],
        switches: &["--mine", "--all"],
        values: &["--root", "--session", "--after"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["dashboard"],
        switches: &["--open", "--allow-network", "--demo"],
        values: &["--listen"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["mcp"],
        values: &["--tools", "--host"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words == ["workspace", "list"],
        values: &["--after"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["workspace", "move"]),
        switches: &["--switch"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| matches!(words, ["workspace", "remove", _]),
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["intent", "set"]),
        values: &["--summary"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"admit"),
        values: &["--ttl-ms"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"brief"),
        switches: &["--recent"],
        values: &["--max-peers"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["remote", "enable"]),
        switches: &["--publish-as-branch"],
        values: &["--root", "--remote", "--name", "--share-task"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["remote", "sent"]),
        values: &["--limit"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "symbol"]),
        switches: &["--detail", "--rank", "--renamed"],
        values: &["--limit", "--session"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| {
            words.starts_with(&["graph", "references"])
                || words.starts_with(&["graph", "blind-spots"])
        },
        values: &["--limit"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "impact"]),
        values: &["--limit", "--depth"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "path"]),
        values: &["--max-hops"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "map"]),
        values: &["--depth"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "export"]),
        values: &["--out"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| matches!(words, ["graph", "import", _]),
        values: &["--in"],
        apply: true,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["graph", "history"]),
        values: &["--path", "--limit", "--before"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"check-write"),
        symbol: Repeat::Any,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"recap"),
        switches: &["--since-away"],
        values: &["--since-seq"],
        tag: Repeat::Once("recap filters by one tag: give --tag once"),
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"remember"),
        values: &["--session"],
        symbol: Repeat::Once("a note is anchored to one definition: give --symbol once"),
        tag: Repeat::Any,
        ..Flags::NONE
    },
    Flags {
        form: |words, _| {
            matches!(
                words.first(),
                Some(&"trail" | &"events" | &"changes" | &"recall")
            )
        },
        values: &["--after", "--limit", "--path"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| matches!(words.first(), Some(&"today" | &"report")),
        values: &["--since"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.starts_with(&["policy", "set"]),
        values: &["--wait-ms"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| words.first() == Some(&"message"),
        values: &["--from", "--to"],
        ..Flags::NONE
    },
    Flags {
        form: |words, _| {
            words.starts_with(&["messages", "status"])
                || words.starts_with(&["messages", "retract"])
        },
        values: &["--from"],
        ..Flags::NONE
    },
    Flags {
        form: |_, before| before == ["compact"],
        values: &["--keep-days"],
        apply: true,
        ..Flags::NONE
    },
];

/// The flags of the command `words` name: all the words, and `before`,
/// those before a `--`.
fn flags_of(words: &[&str], before: &[&str]) -> &'static Flags {
    COMMANDS
        .iter()
        .find(|flags| (flags.form)(words, before))
        .unwrap_or(&Flags::NONE)
}

/// Whether some command declares `name` as a flag that takes a value
/// (`Some(true)`) or none (`Some(false)`). The parser reads this before
/// the command's words are known.
fn takes_value(name: &str) -> Option<bool> {
    let name = if name == "-m" { "--message" } else { name };
    if EVERY_COMMAND.contains(&name) || COMMANDS.iter().any(|flags| flags.values.contains(&name)) {
        Some(true)
    } else if COMMANDS.iter().any(|flags| flags.switches.contains(&name)) {
        Some(false)
    } else {
        None
    }
}

struct ParsedOptions {
    args: Vec<String>,
    /// Everything after a literal `--`: a check's command line.
    command: Vec<String>,
    options: BTreeMap<String, String>,
    /// Each `--symbol NAME`, in order: one of the two options that may
    /// repeat.
    symbols: Vec<String>,
    /// Each `--tag TAG`, in order (`remember`; `recap` takes one).
    tags: Vec<String>,
    json: bool,
    apply: bool,
}

fn parse_options(input: Vec<String>) -> Result<ParsedOptions, langspan::Error> {
    let mut positionals = Vec::new();
    let mut command = Vec::new();
    let mut options = BTreeMap::new();
    let mut symbols = Vec::new();
    let mut tags = Vec::new();
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
            "--symbol" => symbols.push(input.next().ok_or("--symbol needs a value")?),
            "--tag" => tags.push(input.next().ok_or("--tag needs a value")?),
            "--apply" => apply = true,
            // A flag some command declares. Its words are not known yet, so
            // these are kept with the options, and only the commands that
            // read them accept them.
            name if takes_value(name) == Some(false) => {
                if options.insert(arg.clone(), String::new()).is_some() {
                    return Err(format!("duplicate option {arg}").into());
                }
            }
            name if takes_value(name) == Some(true) => {
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
        symbols,
        tags,
        json,
        apply,
    })
}
/// A required file option as an absolute path, so the daemon, which runs
/// in another directory, opens the file the owner named.
fn absolute_option(
    options: &mut BTreeMap<String, String>,
    name: &str,
) -> Result<String, langspan::Error> {
    let value = options
        .remove(name)
        .ok_or_else(|| format!("{name} FILE is required"))?;
    let path = std::path::absolute(&value).map_err(|e| format!("cannot resolve {name}: {e}"))?;
    path.into_os_string()
        .into_string()
        .map_err(|_| format!("{name} must be a UTF-8 path").into())
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

#[cfg(test)]
mod option_tests {
    fn parsed(input: &[&str]) -> std::collections::BTreeMap<String, String> {
        super::parse_options(input.iter().map(|s| s.to_string()).collect())
            .unwrap()
            .options
    }

    #[test]
    fn flags_and_values_from_setup_brief_retention_and_guidance_all_parse() {
        let options = parsed(&[
            "x",
            "--yes",
            "--recent",
            "--create",
            "--event",
            "e",
            "--before",
            "b",
            "--keep-days",
            "7",
            "--month",
            "2026-11",
            "--format",
            "cursor",
            "--file",
            "AGENTS.md",
            "--wait-ms",
            "300",
        ]);
        for flag in ["--yes", "--recent", "--create"] {
            assert_eq!(options[flag], "", "{flag}");
        }
        for (key, value) in [
            ("--event", "e"),
            ("--before", "b"),
            ("--keep-days", "7"),
            ("--month", "2026-11"),
            ("--format", "cursor"),
            ("--file", "AGENTS.md"),
            ("--wait-ms", "300"),
        ] {
            assert_eq!(options[key], value, "{key}");
        }
    }
}
