use langspan::cli::render::{
    print_combined, print_conflict, print_human, print_inconclusive, print_not_rewritten,
    print_notes, print_project_hook_reruns, print_renames, print_replay_conflict,
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

fn hooks(
    vendor: langspan::install::Vendor,
    install: bool,
    state_dir: PathBuf,
    options: &mut BTreeMap<String, String>,
    apply: bool,
) -> Result<u8, langspan::Error> {
    use langspan::install;
    if install && vendor.removed() {
        return Err(install::GEMINI_REMOVED.into());
    }
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
            presence: presence(options)?,
            outline_reads: outline_reads(options)?,
        })
    } else {
        None
    };
    let with_skill = options.remove("--skill").is_some();
    let project_skill = langspan::skill::PROJECT_HOSTS.contains(&vendor);
    if with_skill && !(install && project_skill) {
        return Err("--skill is valid for `hooks install cursor` and `hooks install antigravity` only: they read the LangSpan skill as a project rule. For claude-code and codex, run `langspan install --skill`; `hooks remove` removes the rule without it".into());
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
            hooked_host: None,
        });
    let registration = registration.as_ref();
    let server = server.as_ref();
    type Plan<'a> =
        Box<dyn Fn(Option<serde_json::Value>) -> Result<serde_json::Value, String> + 'a>;
    // Each file with its plan and whether it is the MCP configuration, which
    // Antigravity leaves empty.
    let mut files: Vec<(PathBuf, Plan, bool)> = vec![(
        settings,
        Box::new(move |existing| install::plan_for(vendor, existing, registration)),
        false,
    )];
    if let Some(mcp) = vendor.project_mcp_file() {
        files.push((
            root.join(mcp),
            Box::new(move |existing| {
                if vendor == install::Vendor::Antigravity {
                    langspan::user_install::plan_antigravity(existing, server)
                } else {
                    langspan::user_install::plan_claude(
                        existing.unwrap_or_else(|| serde_json::json!({})),
                        server,
                    )
                }
            }),
            true,
        ));
    }
    let parse = |path: &std::path::Path, mcp: bool, bytes: Option<&[u8]>| {
        if mcp {
            install::parse_mcp_json(vendor, path, bytes)
        } else {
            install::parse_json(path, bytes)
        }
    };
    // Every file is planned, and every refusal raised, before any is written.
    let mut planned_files = Vec::new();
    for (path, plan, mcp) in &files {
        let original = install::read_bytes(path)?;
        let existing = parse(path, *mcp, original.as_deref())?;
        let planned = plan(existing.clone()).map_err(|e| format!("{}: {e}", path.display()))?;
        let unchanged = existing.as_ref() == Some(&planned) || (existing.is_none() && !install);
        planned_files.push((path, plan, *mcp, original, planned, unchanged));
    }
    // Cursor's and Antigravity's project rule: written with --skill, removed
    // with the hooks wherever LangSpan's is found.
    let rule = if project_skill && (with_skill || !install) {
        let path = langspan::skill::project_path(vendor, &root);
        Some(langspan::skill::plan_file(vendor, &path, install)?)
    } else {
        None
    };
    let mut changed = false;
    for (path, plan, mcp, original, planned, unchanged) in planned_files {
        if unchanged {
            println!("{} already matches; nothing to change.", path.display());
            continue;
        }
        changed = true;
        if apply {
            let replan = |now: Option<&[u8]>| -> Result<Vec<u8>, langspan::Error> {
                let existing = parse(path, mcp, now)?;
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
            if vendor.removed() {
                // Gemini CLI's project settings can hold MCP server
                // credentials too.
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
    if vendor == install::Vendor::Codex && install {
        // Codex trusts project hooks by the exact path of their file: a
        // session resumed under another path for the same directory runs
        // none of them (issue #129).
        println!(
            "Codex: start and resume Codex in '{}', the resolved root with no symbolic links. Codex trusts a project's hooks only at the exact path they were trusted at, so a session started or resumed under another path for the same directory (a symbolic link, or /tmp for /private/tmp on macOS) runs none of LangSpan's hooks, without a warning.",
            root.display()
        );
    }
    if vendor == install::Vendor::Antigravity && install {
        println!(
            "Antigravity: {} Antigravity's App, CLI and IDE all read these files; `/hooks` in the CLI lists the hooks it loaded. They hold paths on this machine, so keep LangSpan's entries out of a commit.",
            langspan::antigravity::VERIFIED
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

/// `guidance`: print LangSpan's block for AGENTS.md and similar files, or
/// write it into (`install`) or take it out of (`remove`) one file of the
/// owner's repository. A dry run unless `--apply`. Needs no coordinator.
fn guidance(
    args: &[String],
    options: &mut BTreeMap<String, String>,
    json: bool,
    apply: bool,
) -> Result<u8, langspan::Error> {
    use langspan::guidance::{self, Format};
    if json {
        return Err("--json is not valid for guidance; it prints text".into());
    }
    // Accepted for every command; the guidance never uses the coordinator.
    options.remove("--state-dir");
    let install = match args {
        [] => {
            let format = match options.remove("--format") {
                None => Format::Agents,
                Some(name) => Format::from_name(&name)
                    .ok_or_else(|| format!("--format {name}: expected {}", Format::USAGE))?,
            };
            no_unused_options(options)?;
            if apply {
                return Err("--apply is valid for `guidance install` and `guidance remove`".into());
            }
            print!("{}", guidance::render(format));
            return Ok(0);
        }
        [action] if action == "install" => true,
        [action] if action == "remove" => false,
        _ => {
            return Err(format!(
                "usage: langspan guidance [--format {}] | guidance install --root DIR --file FILE [--create] [--apply] | guidance remove --root DIR --file FILE [--apply]",
                Format::USAGE
            )
            .into());
        }
    };
    let root = options
        .remove("--root")
        .ok_or("guidance install and remove need --root DIR, the repository")?;
    let file = options
        .remove("--file")
        .ok_or("guidance install and remove need --file FILE, such as AGENTS.md")?;
    let create = options.remove("--create").is_some();
    if create && !install {
        return Err("--create is valid only for `guidance install`".into());
    }
    no_unused_options(options)?;
    let path = guidance::target(std::path::Path::new(&root), &file)?;
    let plan = guidance::plan_file(&path, install, create)?;
    if plan.unchanged() {
        println!(
            "{}",
            if install {
                format!("{file} already has LangSpan's block; nothing changed.")
            } else {
                format!("{file} has no LangSpan block; nothing changed.")
            }
        );
        return Ok(0);
    }
    if !apply {
        print!("{}", plan.diff());
        if plan.planned.is_none() {
            println!("{file} would be removed: it held only LangSpan's block.");
        }
        println!(
            "dry run: add --apply to {} {file}",
            if install { "write" } else { "change" }
        );
        return Ok(0);
    }
    plan.apply()?;
    println!(
        "{}",
        match (install, plan.planned.is_some()) {
            (true, _) => format!("wrote LangSpan's block to {file}"),
            (false, true) => format!("removed LangSpan's block from {file}"),
            (false, false) => format!("removed {file}: it held only LangSpan's block"),
        }
    );
    Ok(0)
}

/// How `user_install` runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Show the plan and how to apply it.
    DryRun,
    /// Show the plan only: `setup --verbose` says how to apply it.
    Plan,
    /// One line per file, no file's text: `setup` says how to apply it.
    Brief,
    Apply,
}

/// The hosts `install` and `setup` choose without --hosts: those whose
/// configuration directory exists. Antigravity is chosen only by name:
/// `~/.gemini` is also made by other tools, Gemini CLI among them.
fn found_hosts(home: &UserHome) -> Vec<langspan::install::Vendor> {
    use langspan::install::Vendor;
    [Vendor::ClaudeCode, Vendor::Codex, Vendor::Cursor]
        .into_iter()
        .filter(|vendor| home.files(*vendor).config_dir.is_dir())
        .collect()
}

fn no_host(home: &UserHome) -> langspan::Error {
    format!(
        "No Claude Code, Codex or Cursor found in {}. Start one of them once, then run this again, or name a host with --hosts claude-code,codex,cursor,antigravity.",
        home.home.display()
    )
    .into()
}

/// A brief plan's line for one file.
fn would_be(path: &std::path::Path, exists: bool, removed: bool) -> String {
    let what = match (removed, exists) {
        (true, _) => "removed",
        (false, true) => "changed",
        (false, false) => "created",
    };
    format!("{} would be {what}.", path.display())
}

/// `install` / `uninstall`: LangSpan's user-level hooks and MCP server entry
/// for each chosen host, and on uninstall the coordinator's login service.
/// Every file is planned, and every refusal raised, before anything is
/// written. Returns how many files would change or changed.
fn user_install(
    install: bool,
    options: &mut BTreeMap<String, String>,
    mode: Mode,
) -> Result<usize, langspan::Error> {
    use langspan::{install as hooks, install::Vendor, user_install};
    let apply = mode == Mode::Apply;
    let home = UserHome::new(options.remove("--home").map(PathBuf::from))?;
    let state_dir = std::path::absolute(home.state_dir(options.remove("--state-dir")))?;
    let share_prompt = switch(options, "--prompt-summary")?;
    let presence = presence(options)?;
    let outline_reads = outline_reads(options)?;
    let hosts = options.remove("--hosts");
    let purge = options.remove("--purge").is_some();
    let leftover = user_install::Leftover::check(&state_dir, purge, install, hosts.is_none())?;
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
    let hooked_tools = match options.remove("--mcp-tools").as_deref() {
        Some(_) if !install => {
            return Err("--mcp-tools is valid only for `install`; `uninstall` removes the whole MCP entry, flag included".into());
        }
        None | Some("all") => false,
        Some("hooked") => true,
        Some(other) => {
            return Err(format!("--mcp-tools takes all or hooked, not {other:?}").into());
        }
    };
    no_unused_options(options)?;
    let vendors: Vec<Vendor> = match hosts {
        Some(list) => list
            .split(',')
            .map(|name| {
                Vendor::from_name(name.trim()).ok_or_else(|| {
                    format!(
                        "unknown host {:?}; choose from claude-code, codex, cursor and antigravity",
                        name.trim()
                    )
                })
            })
            .collect::<Result<_, _>>()?,
        None => found_hosts(&home),
    };
    // Gemini CLI is named only to remove an earlier install.
    if install && vendors.iter().any(|vendor| vendor.removed()) {
        return Err(langspan::install::GEMINI_REMOVED.into());
    }
    if vendors.is_empty() && install {
        return Err(no_host(&home));
    }
    if vendors.is_empty() {
        println!(
            "No host configuration found under {} (.claude, .codex or .cursor), so no host's files change.",
            home.home.display()
        );
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
        presence,
        outline_reads,
    });
    // Each host's entry: with `--mcp-tools hooked`, the Claude Code and Codex
    // entries list fewer tools while their hooks report (roadmap I11).
    let servers: Vec<(Vendor, user_install::McpServer)> = if install {
        vendors
            .iter()
            .map(|&vendor| {
                let hooked_host = (hooked_tools
                    && langspan::mcp::HOOKED_HOSTS.contains(&vendor.name()))
                .then(|| vendor.name().to_owned());
                (
                    vendor,
                    user_install::McpServer {
                        executable: executable.clone(),
                        state_dir: state_dir.clone(),
                        hooked_host,
                    },
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    // A host may rewrite its file between this plan and the write.
    let mut plans: Vec<FilePlan> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if hooked_tools {
        let others: Vec<&str> = vendors
            .iter()
            .map(|vendor| vendor.name())
            .filter(|name| !langspan::mcp::HOOKED_HOSTS.contains(name))
            .collect();
        if !others.is_empty() {
            notes.push(format!(
                "--mcp-tools hooked applies to claude-code and codex; {} keep every tool until their hooks are verified on a real host.",
                others.join(" and ")
            ));
        }
    }
    let registration = registration.as_ref();
    // On uninstall, a file LangSpan may have created is removed when nothing
    // but LangSpan's entries was in it, rather than left holding `{}`.
    let emptied = |vendor: Vendor| {
        (!install).then(|| -> Box<hooks::LeftEmpty> {
            Box::new(move |bytes: &[u8]| hooks::json_left_empty(vendor, bytes))
        })
    };
    for &vendor in &vendors {
        let server = servers
            .iter()
            .find(|(each, _)| *each == vendor)
            .map(|(_, server)| server);
        let files = home.files(vendor);
        let original = hooks::read_bytes(&files.hooks)?;
        let existing = hooks::parse_json(&files.hooks, original.as_deref())?;
        if vendor.removed() {
            // Only `uninstall` gets here (install refused it above). Gemini
            // CLI kept hooks and MCP servers in one settings file: one plan,
            // one guarded write.
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
            // Cursor's mcp.json and Antigravity's mcp_config.json hold only
            // MCP servers; LangSpan may create them.
            Vendor::Cursor | Vendor::Antigravity => {
                let plan_mcp = move |existing: Option<serde_json::Value>| {
                    if vendor == Vendor::Antigravity {
                        user_install::plan_antigravity(existing, server)
                    } else {
                        user_install::plan_claude(
                            existing.unwrap_or_else(|| serde_json::json!({})),
                            server,
                        )
                    }
                };
                let existing = hooks::parse_mcp_json(vendor, &files.mcp, original.as_deref())?;
                let planned = plan_mcp(existing.clone())
                    .map_err(|e| format!("{}: {e}", files.mcp.display()))?;
                let path = files.mcp.clone();
                let replan = move |now: Option<&[u8]>| -> Result<Vec<u8>, langspan::Error> {
                    let existing = hooks::parse_mcp_json(vendor, &path, now)?;
                    let planned =
                        plan_mcp(existing).map_err(|e| format!("{}: {e}", path.display()))?;
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
                if install && vendor == Vendor::Cursor {
                    notes.push("Cursor: built from documentation, not yet verified on a real Cursor (ADR 0032). Cursor asks before it first uses an MCP server; approve `langspan` there.".into());
                }
                if install && vendor == Vendor::Antigravity {
                    notes.push(format!(
                        "Antigravity: {} Its App, CLI and IDE all read these files.",
                        langspan::antigravity::VERIFIED
                    ));
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
        // `uninstall --hosts gemini-cli` takes out the section an earlier
        // `install --skill` put in ~/.gemini/GEMINI.md, which Antigravity
        // reads too.
        if vendor.removed() {
            let path = langspan::skill::path(vendor, &home.files(vendor).config_dir);
            skills.push(langspan::skill::plan_file(vendor, &path, false)?);
            continue;
        }
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
        } else if removed || mode == Mode::Brief {
            println!("{}", would_be(path, original.is_some(), removed));
        } else {
            println!("{} would become:", path.display());
            println!("{text}");
        }
    }
    // Install shows the skill once; uninstall shows each file's own remainder.
    let mut shown: Option<&std::path::Path> = None;
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
        if mode == Mode::Brief {
            println!(
                "{}",
                would_be(&plan.path, plan.original.is_some(), plan.planned.is_none())
            );
        } else if !apply {
            match shown {
                Some(first) if plan.planned.is_some() => println!(
                    "{} would get the same skill text as {}.",
                    plan.path.display(),
                    first.display()
                ),
                _ => {
                    println!("{}", plan.shown());
                    if install && plan.planned.is_some() {
                        shown = Some(&plan.path);
                    }
                }
            }
        } else if plan.apply()? {
            match plan.planned {
                Some(_) => println!("Wrote {}.", plan.path.display()),
                None => println!("Removed {}.", plan.path.display()),
            }
            // Claude Code's `skills` folder, when the skill was all it held.
            if plan.planned.is_none() && plan.vendor == Vendor::ClaudeCode {
                let _ = plan.path.ancestors().nth(2).map(std::fs::remove_dir);
            }
        } else {
            println!(
                "{} already matches; nothing to change.",
                plan.path.display()
            );
        }
    }
    if !install {
        changed += remove_service(&home, apply)?;
        changed += stop_coordinator(&state_dir, apply)?;
    }
    if let Some(leftover) = &leftover {
        let (removed, lines) = leftover.finish(&executable, apply)?;
        changed += removed;
        lines.iter().for_each(|line| println!("{line}"));
    }
    if changed > 0 && mode == Mode::DryRun {
        println!("Dry run; nothing was written. Run the same command with --apply to write it.");
    }
    for note in notes {
        println!("{note}");
    }
    Ok(changed)
}

/// Starts a coordinator for `state_dir` if none runs and no login service is
/// installed for it, waiting at most until `until`. Never fails: what
/// happened goes to stderr as one structured line.
fn start_coordinator(state_dir: &std::path::Path, until: std::time::Instant) {
    if langspan::autostart::disabled() {
        return;
    }
    let Ok(state_dir) = std::path::absolute(state_dir) else {
        return;
    };
    // The usual case, before anything else is looked up.
    if langspan::client::accepts(
        &state_dir,
        until.min(std::time::Instant::now() + std::time::Duration::from_millis(100)),
    ) {
        return;
    }
    let executable = std::env::current_exe()
        .map_err(langspan::Error::from)
        .and_then(|exe| Ok(langspan::install::installed_executable(&exe)?));
    let outcome = match executable {
        Ok(executable) => {
            let service = UserHome::new(None)
                .is_ok_and(|home| langspan::service::installed_for(&home, &state_dir));
            langspan::autostart::ensure(&state_dir, &executable, None, service, until)
        }
        Err(error) => {
            langspan::autostart::Outcome::Failed(format!("cannot find this executable: {error}"))
        }
    };
    if let Some(line) = outcome.log_line(&state_dir) {
        eprintln!("{line}");
    }
}

/// The login service `setup` would install, as planned.
enum ServicePlan {
    /// No service: why, and so the coordinator is started on demand.
    None(String),
    /// The file and the manager already match.
    Unchanged(PathBuf),
    /// The file to write, its text, and what is there now.
    Install {
        manager: langspan::service::Manager,
        path: PathBuf,
        text: String,
        existing: Option<String>,
    },
}

fn plan_service(
    home: &UserHome,
    executable: &std::path::Path,
    state_dir: &std::path::Path,
) -> Result<ServicePlan, langspan::Error> {
    use langspan::service::{self, Manager};
    let Some(manager) = Manager::current() else {
        return Ok(ServicePlan::None(
            "this platform has no supported login service".into(),
        ));
    };
    if home.explicit() {
        return Ok(ServicePlan::None(
            "--home names a home other than the login user's, whose service manager setup does not change".into(),
        ));
    }
    let path = manager.file(home);
    let existing = service::read(&path)?;
    if let Some(existing) = &existing
        && manager.state_dir_of(existing).is_none()
    {
        return Err(format!(
            "{} exists and LangSpan did not write it; move it aside, then run setup again",
            path.display()
        )
        .into());
    }
    if let Err(reason) = service::manager_available(manager) {
        return Ok(ServicePlan::None(reason));
    }
    let text = manager.render(executable, state_dir, &|name| std::env::var(name).ok())?;
    if existing.as_deref() == Some(text.as_str()) && service::running(manager) {
        return Ok(ServicePlan::Unchanged(path));
    }
    Ok(ServicePlan::Install {
        manager,
        path,
        text,
        existing,
    })
}

/// `setup`: one plan for the user-level install with the skill, the
/// coordinator's login service and the state directory; one question; then
/// the install, the service, a running coordinator and `doctor`.
fn setup(options: &mut BTreeMap<String, String>, json: bool) -> Result<u8, langspan::Error> {
    use std::io::{BufRead, IsTerminal, Read, Write};
    if json {
        return Err(
            "--json is not valid for `setup`; run `langspan doctor --json` after it".into(),
        );
    }
    let yes = options.remove("--yes").is_some();
    let verbose = options.remove("--verbose").is_some();
    if options.contains_key("--skill") {
        return Err("`setup` always installs the skill; leave out --skill".into());
    }
    options.insert("--skill".into(), String::new());
    let home = UserHome::new(options.get("--home").map(PathBuf::from))?;
    // Said before any plan is printed (issue #291).
    if !options.contains_key("--hosts") && found_hosts(&home).is_empty() {
        return Err(no_host(&home));
    }
    let state_dir = std::path::absolute(home.state_dir(options.get("--state-dir").cloned()))?;
    langspan::autostart::check_state_dir(&state_dir)?;
    let executable = langspan::install::installed_executable(&std::env::current_exe()?)?;
    println!("LangSpan setup for {}", home.home.display());
    println!("State directory: {}", state_dir.display());
    println!("Hook and MCP files:");
    let mut plan_options = options.clone();
    let mode = if verbose { Mode::Plan } else { Mode::Brief };
    let files = user_install(true, &mut plan_options, mode)?;
    let service = plan_service(&home, &executable, &state_dir)?;
    println!("Coordinator:");
    match &service {
        ServicePlan::None(reason) => println!(
            "  No login service: {reason}. Hooks and `langspan mcp` start the coordinator when they find none, and setup starts one now."
        ),
        ServicePlan::Unchanged(path) => {
            println!(
                "  Login service {} is installed and running; nothing to change.",
                path.display()
            )
        }
        ServicePlan::Install {
            path,
            text,
            existing,
            ..
        } => {
            if existing.as_deref() == Some(text.as_str()) {
                println!(
                    "  Start login service {}, which is installed but not running.",
                    path.display()
                );
            } else {
                println!(
                    "  {} {} and start it, so the coordinator runs at every login{}",
                    if existing.is_some() {
                        "Replace"
                    } else {
                        "Write"
                    },
                    path.display(),
                    if verbose { ":" } else { "." }
                );
                if verbose {
                    for line in text.lines() {
                        println!("    {line}");
                    }
                }
            }
        }
    }
    let service_changes = matches!(service, ServicePlan::Install { .. });
    let running = langspan::client::accepts(
        &state_dir,
        std::time::Instant::now() + std::time::Duration::from_millis(500),
    );
    if files == 0 && !service_changes && running {
        println!("Already set up; nothing to change.");
        println!("{TRY_IT}");
        return Ok(0);
    }
    if !verbose {
        println!("Run `langspan setup --verbose` to see each file's full text first.");
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            println!(
                "Not run from a terminal and no --yes, so nothing was changed. Run `langspan setup --yes` to apply this plan."
            );
            return Ok(0);
        }
        eprint!("Set this up? [Y/n] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin().lock().take(256).read_line(&mut answer)?;
        if !matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "" | "y" | "yes"
        ) {
            println!("Nothing was changed.");
            return Ok(0);
        }
    }
    if files > 0 {
        user_install(true, options, Mode::Apply)?;
    }
    langspan::autostart::prepare_state_dir(&state_dir)?;
    match service {
        ServicePlan::Install {
            manager,
            path,
            text,
            existing,
        } => {
            install_service(manager, &path, existing.as_deref(), &text, &state_dir)?;
        }
        ServicePlan::Unchanged(_) => {}
        ServicePlan::None(_) => {
            let outcome = langspan::autostart::ensure(
                &state_dir,
                &executable,
                home.explicit().then_some(home.home.as_path()),
                false,
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            );
            match outcome {
                langspan::autostart::Outcome::Running => {
                    println!("A coordinator is already running.")
                }
                langspan::autostart::Outcome::Started { pid, ready: true } => println!(
                    "Started a coordinator in the background (process {pid}); it logs to {}.",
                    langspan::autostart::log_path(&state_dir).display()
                ),
                langspan::autostart::Outcome::Started { pid, ready: false } => println!(
                    "Started a coordinator (process {pid}), but it did not accept connections within 5 seconds; read {} for why.",
                    langspan::autostart::log_path(&state_dir).display()
                ),
                langspan::autostart::Outcome::Skipped(reason) => {
                    println!("No coordinator was started ({reason}).")
                }
                langspan::autostart::Outcome::Failed(error) => {
                    println!("No coordinator was started: {error}.")
                }
            }
        }
    }
    println!("langspan doctor:");
    // Setup itself succeeded; what doctor finds is reported, not its exit.
    doctor(&state_dir, None, &home, false)?;
    println!("{TRY_IT}");
    Ok(0)
}

/// Setup's last line: the next step, which needs no agent (issue #290).
const TRY_IT: &str = "Try it: langspan demo";

/// Writes the login service file, unless it changed from `planned` since
/// the plan was shown, and starts it, then waits up to five seconds for the
/// coordinator to accept connections.
fn install_service(
    manager: langspan::service::Manager,
    path: &std::path::Path,
    planned: Option<&str>,
    text: &str,
    state_dir: &std::path::Path,
) -> Result<(), langspan::Error> {
    use langspan::service;
    // The manager creates the log as it starts the service: owner-only first.
    drop(langspan::autostart::open_log(&std::fs::canonicalize(
        state_dir,
    )?)?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    langspan::service::write(path, planned, text)?;
    println!("Wrote {}.", path.display());
    let commands = service::start_commands(manager, path);
    let last = commands.len() - 1;
    for (index, (program, args)) in commands.iter().enumerate() {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let ran = service::run(program, &args)?;
        // launchd's first step unloads an earlier load, and fails when there
        // was none.
        let optional = manager == service::Manager::Launchd && index < last;
        if !ran.success && !optional {
            return Err(format!(
                "`{program} {}` failed: {}. {} is written; run that command again, or `langspan daemon` to start the coordinator by hand",
                args.join(" "),
                ran.said,
                path.display()
            )
            .into());
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        if langspan::client::accepts(state_dir, deadline) {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if ready {
        println!("Started the login service; the coordinator accepts connections.");
    } else {
        println!(
            "Started the login service, but the coordinator did not accept connections within 5 seconds; see {}.",
            langspan::autostart::log_path(state_dir).display()
        );
    }
    Ok(())
}

/// On uninstall, stops and removes the login service LangSpan wrote, if
/// any. Returns 1 when there is one to remove, else 0.
fn remove_service(home: &UserHome, apply: bool) -> Result<usize, langspan::Error> {
    use langspan::service::{self, Manager};
    let Some(manager) = Manager::current() else {
        return Ok(0);
    };
    let path = manager.file(home);
    match service::read(&path)? {
        None => return Ok(0),
        Some(text) if manager.state_dir_of(&text).is_none() => {
            println!(
                "{} was not written by LangSpan; it was left in place.",
                path.display()
            );
            return Ok(0);
        }
        Some(_) => {}
    }
    if !apply {
        println!(
            "The coordinator's login service {} would be stopped and removed.",
            path.display()
        );
        return Ok(1);
    }
    // Another home's manager is not this login's: only the file goes.
    if !home.explicit() {
        for (program, args) in service::stop_commands(manager) {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match service::run(&program, &args) {
                Ok(ran) if ran.success => {}
                Ok(ran) => println!("`{program} {}` said: {}", args.join(" "), ran.said),
                Err(error) => println!("{error}"),
            }
        }
    }
    std::fs::remove_file(&path)?;
    if !home.explicit() && manager == Manager::Systemd {
        let _ = service::run("systemctl", &["--user", "daemon-reload"]);
    }
    println!(
        "Stopped and removed the coordinator's login service {}.",
        path.display()
    );
    Ok(1)
}

/// On uninstall, stops the coordinator for `state_dir`, however it was
/// started. Returns 1 when one runs, else 0.
fn stop_coordinator(state_dir: &std::path::Path, apply: bool) -> Result<usize, langspan::Error> {
    use std::time::{Duration, Instant};
    if !apply {
        let runs = langspan::autostart::running(state_dir)?;
        if runs {
            println!("The coordinator would be stopped.");
        }
        return Ok(usize::from(runs));
    }
    let stopped = langspan::autostart::stop(state_dir, Instant::now() + Duration::from_secs(5))?;
    if stopped {
        println!("Stopped the coordinator.");
    }
    Ok(usize::from(stopped))
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

/// An `on`/`off` option that is on unless switched off.
/// `--outline-reads on|off` (roadmap A11): off unless switched on, because
/// it changes what a host's first whole read of a large file returns.
fn outline_reads(options: &mut BTreeMap<String, String>) -> Result<bool, langspan::Error> {
    match options.remove("--outline-reads").as_deref() {
        None | Some("off") => Ok(false),
        Some("on") => Ok(true),
        Some(_) => Err("--outline-reads must be on or off".into()),
    }
}

fn switch(options: &mut BTreeMap<String, String>, name: &str) -> Result<bool, langspan::Error> {
    match options.remove(name).as_deref() {
        None | Some("on") => Ok(true),
        Some("off") => Ok(false),
        Some(_) => Err(format!("{name} must be on or off").into()),
    }
}

/// `--presence on|off` and `--presence-minutes N` (roadmap A13).
fn presence(
    options: &mut BTreeMap<String, String>,
) -> Result<langspan::install::Presence, langspan::Error> {
    Ok(langspan::install::Presence {
        shown: switch(options, "--presence")?,
        minutes: match options.remove("--presence-minutes") {
            Some(value) => langspan::install::Presence::minutes(&value)?,
            None => None,
        },
    })
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
fn no_unused_options(options: &BTreeMap<String, String>) -> Result<(), langspan::Error> {
    if let Some((name, _)) = options.first_key_value() {
        return Err(format!("option {name} is not valid for this command").into());
    }
    Ok(())
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
