# Local coordination threat model

**Scope:** the M3 working tree, extending [ADR 0001](../adr/0001-local-coordination.md),
[ADR 0002](../adr/0002-claude-code-adapter.md) and
[ADR 0003](../adr/0003-native-coordination-observation.md). Native-host M3
acceptance is pending. LangSpan is a cooperative developer tool, not an
isolation boundary between programs running as the same OS user.

## Data and boundaries

The daemon stores canonical workspace locations, logical session IDs, user-supplied
intent summaries, file keys, typed operation digests, grant/denial receipts,
process-incarnation observations, disk/branch observations and event history.
SQLite and its sidecars live in an owner-only state directory. A different state
directory remains an independent coordination domain.

M3 expands the read boundary. The Git observer reads bounded repository metadata
and file contents to compute digests, including symlink target text without
following the link. Full file contents and native edit bodies are not persisted
or relayed by the coordination engine. Process observation reads same-user Linux
process metadata and boot identity, not command arguments or environment. The
installer can modify the selected project hook settings only after explicit
apply. The engine does not apply a patch, run an agent or execute arbitrary
repository instructions.

| Boundary | Treatment |
| --- | --- |
| CLI/MCP JSON → daemon | Protocol 3, typed commands, strict internal request fields, bounded frames and durable mutation identity. MCP uses the SDK lifecycle and the same operations. |
| Native host event → hook | Bounded envelope, vendor-specific supported shapes, usable native IDs and a typed semantic input digest. Unknown host fields grant no authority. |
| Supplied paths → filesystem/Git identity | Canonical roots and paths, traversal/escape checks, verified worktree relationships and registered-descendant exclusions. |
| Git subprocess/content → observer | Fixed read-only commands, optional locks and lazy fetching disabled, bounded output/deadline and no interpretation of repository text as instructions. Incomplete scans fail without replacing the previous complete snapshot. A scan over its content bounds is partial and says so; what it left out is never reported as deleted. `git status` (partial scans) and `check run`'s private index run with every configured filter driver switched off and fsmonitor off, so no repository program runs; only the driver names are read from the configuration, never their commands, and a name containing `=` or a newline fails the operation. Submodules are not read into. A checkout change is built by the daemon's own scan; like `observe_disk`, the internal `observe_checkout` command is also accepted from any client of the owner-only socket, so a same-user process can fabricate one. It reaches briefings only, as cleaned and quoted branch names and hexadecimal commit IDs. |
| Process metadata → session evidence | Same-user process incarnation, alive/exited/unknown, explicit time and generation; no effect on admission authority. |
| Persisted JSON → replay and read views | Versioned schema, validated replay, preserved historical records, one owner and atomic event/receipt or event/snapshot commits. An event that does not parse stops the store opening; a receipt that does not parse is skipped and its request refused on retry ([ADR 0015](../adr/0015-version-tolerance.md)). |
| Hook settings → disk | Dry run first, explicit apply, recognized owned entries only, preservation of unrelated settings and refusal of settings-file symlinks. |
| Daemon → automatic check | Off by default; the owner's own command, set through the CLI only, run without a shell in its own process group within its time limit, outside any agent host's sandbox. Its output's redacted, bounded end reaches the event log and the triggering sessions' context as labelled data ([ADR 0030](../adr/0030-automatic-checks-after-write-batches.md)). |
| Daemon → desktop notifier | Off by default; `osascript` or `notify-send` from the daemon's `PATH`, with a title and a body of at most 200 bytes (workspace, session, paths) as arguments, in its own process group, killed at 2 seconds, on a thread of its own ([Desktop notices](#desktop-notices)). |
| Browser → dashboard (TCP) | Loopback by default, a per-run 256-bit token, a `Host` check against the arrival address, `GET` only, bounded heads and deadlines, read-only through the socket client ([ADR 0013](../adr/0013-dashboard.md)). |

Intent text, filenames, branch names and another participant's identity are data.
LangSpan does not treat them as executable instructions. Relayed conflict context
is labelled as coordination data and capped. No UI or remote publication service
is introduced by M3.

## STRIDE assessment

| Concern | Concrete behavior and remaining boundary |
| --- | --- |
| Spoofing | State directory `0700`, socket/database/lock `0600`, effective-user ownership checks and exclusive lifetime lock. Native session/tool IDs and host process names are cooperative identifiers, not authenticated identities. A process token distinguishes a PID incarnation, not the truth of an agent's claims. |
| Tampering | Strict core commands, bounded paths/IDs/digests, canonical scope and immutable original-command receipts. Tool-input changes under the same request ID fail. Post scope/input mismatches cannot complete a lease. Same-user programs can still alter files or storage; corrupted history fails instead of silently resetting. |
| Repudiation | Ordered events and durable original outcomes retain retry, generation and observation evidence. A denial is its receipt and one `write_refused` event (kind, at most 8 paths, the peer it names), written with its receipt, so a retry of the same request adds no second one; a held commit's event is reported by the hook. Disk observations are unattributed and any related operation IDs are unverified correlation. This is not a signed or tamper-proof audit log. |
| Information disclosure | Local owner-only storage; no telemetry or remote coordination layer. Shared clients can read paths, intent text, process IDs, digests and branch identities in that domain. Git stderr is not relayed verbatim because it can contain repository-controlled private values. Native edit bodies and process arguments/environment are not stored or forwarded. |
| Denial of service | 64 KiB IPC frames, bounded clients/queue and deadlines; 8 MiB hook envelopes; 4 MiB Codex patches and 64 paths. One bounded background Git worker isolates scan I/O from request ownership. Event/changes pages are byte-bounded. An MCP tool result carries its answer twice, once re-escaped as JSON text, so a 48 KiB page of quotation marks and backslashes needs about three times its bytes: the adapter encodes every tool's whole result and holds it to 512 bytes short of one 64 KiB frame, cutting a page of events, changes, status records, the status view or a recap to what fits (with a note naming the cursor for the rest) and replacing any other oversized result with a `reply_too_large` error that keeps the operation's `exit_code` and outcome. Large status, disk exhaustion, retained history growth and hostile same-user processes remain limitations. |
| Elevation of privilege | LangSpan never answers `allow` (except Antigravity's pass-through `{"decision":"allow"}` on `PreToolUse`, which leaves Antigravity's own policy in charge; see [Antigravity](#antigravity)), changes host permissions, applies code or grants OS access. Git observation does not mutate the index or working tree. Installer writes are explicit and scoped. Admission controls cooperative coordination, not filesystem writes. |

## Native adapters and installation

Claude Code covers `Edit`/`Write`; Codex covers canonical `apply_patch` additions,
updates, deletions and moves. A move claims source and destination in one set.
Both hosts' `Bash` calls are covered partly, as described under "Shell
commands" below. Shell wrappers around `apply_patch`, remote-environment selectors, unknown directives, unsupported
paths and Codex subagent envelopes receive no coordination decision. Missing or
unrecognized post results remain unknown. Real Codex host acceptance is not
inferred from the source-derived fixtures.

Only a newly returned conflict creates a JSON denial. Failures and unknown states
produce no permission decision, leaving the native host's ordinary controls in
place. LangSpan's own malformed-input, path, socket and state checks still reject
its operation; a failed coordination check does not become an `allow` result.
A replayed historical conflict is not reported as a freshly established holder.

One absolute deadline bounds stdin, workspace resolution and daemon calls
(1.5 seconds by default; installed host timeout 5 seconds). A permission prompt
or native refusal can hold an admission until its TTL (30 seconds by default,
maximum 60 seconds). Missing post callbacks do not cause implicit renewal. A
later callback may be expired or invalidated; it does not prove when the actual
write occurred. Optional liveness recording cannot suppress the main lifecycle.

The installer recognizes a stable `langspan-managed` marker plus the exact
expected generated command shape. That supports executable relocation without
duplicate hooks. The narrow legacy Claude format remains recognized; unrelated
commands remain user-owned. Reads use no-follow opens and bounded settings data;
writes preserve existing mode, use a new temporary file and atomic replacement,
and re-check that the destination is not a symlink. A pre-existing temporary
entry is never treated as the installer's own cleanup target. Same-user races
are not a claimed isolation boundary.

Host trust is still separate: writing configuration is not evidence that the
host loaded it. Historical Claude behavior is recorded in the
[M2 evidence](../evidence/m2-claude-code/README.md); current Codex source and
host-probe limits are in the [M3 evidence](../evidence/m3-codex/README.md).
With the daemon unavailable, failure evidence remains best-effort stderr or
host retention. There is no durable local failure spool or complete operation
denominator in M3.

## Cursor adapter

[ADR 0032](../adr/0032-cursor-adapter.md). Built from documentation, not yet verified on a real Cursor. The new boundaries are three:

- Cursor's event JSON on the hook's stdin;
- the command string Cursor runs through a shell, with that JSON appended as a heredoc;
- the `.cursor/hooks.json` and `.cursor/mcp.json` files LangSpan writes (project) and `~/.cursor/hooks.json` and `~/.cursor/mcp.json` (user).

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Spoofing**: another program's event, or another host's, is taken for a Cursor session | Identity comes only from `conversation_id`/`session_id` and `tool_use_id`, as `cursor.<id>`. An ID outside the grammar becomes a digest. A subagent call becomes its own session under its parent. The Claude Code adapter refuses any payload carrying `cursor_version`, so Cursor running Claude Code's hooks never coordinates one call under two identities. | Any local process that can run the hook command can claim a Cursor session. That is the same as for the other hosts, since the hook is not an authentication boundary. |
| **Tampering**: event text runs as shell code | Cursor CLI 2026.11.22 appends the event to the command as a heredoc. LangSpan's command therefore has no trailing comment: its marker is a leading `LANGSPAN_MANAGED=…` assignment, and every argument is single-quoted. A test runs the installed command that way under `sh`, `bash` and `zsh` with `$(touch …)` in a Write's content, and checks that the event is read as data. | The transport comes from the CLI's source. A Cursor version that builds the command differently has not been seen. The editor's transport is not known. |
| **Tampering**: the install damages the user's Cursor configuration | The same guarded writer as the other hosts: dry run first, every file planned before any write, a concurrent change never overwritten, mode kept, new files `0600`, symlinks refused, 1 MiB bound. Only entries with LangSpan's exact marked shape are replaced or removed. `version`, which Cursor requires, is added only if missing and never removed. | A hand-written entry in LangSpan's exact shape is treated as LangSpan's. |
| **Repudiation** | Each run writes one structured line to stderr with its coverage. Cursor keeps hook runs in its Hooks output channel. `doctor` reports what is installed. | Where Cursor keeps stderr is not documented. |
| **Information disclosure** | Fields that are read and never kept: `user_email`, `transcript_path`, `model`, a Write's `content` and a Shell's output. The prompt is used only as for the other hosts: a redacted first line if shared, and names for a local index lookup. A reply carries only the denial reason or briefing, both labelled as data and capped, and the briefing under Cursor's 10,000-character limit. | Project `.cursor/*.json` files are often committed, and LangSpan's entries hold this machine's executable and state-directory paths. The installer says so; it does not edit `.git/info/exclude`. `LANGSPAN_CAPTURE_DIR`, opt-in, saves raw events, which include the email and paths, as `0600` files. |
| **Denial of service**: LangSpan blocks Cursor's work | No `failClosed`, so a crash or timeout lets the action proceed. Every non-decision is an empty reply, which Cursor treats as a failed hook that does not block. Only documented fields are sent, because Cursor blocks a permission hook on a reply it cannot read. 5 s host timeout, 1.5 s own budget. | Each empty reply is logged by Cursor as a failed hook. A Cursor version that starts blocking on empty replies would block every covered call. The capture must confirm that it does not. |
| **Elevation of privilege** | The adapter never answers `allow`, which Cursor would merge as an approval, and never exits 2. Cursor's workspace trust, MCP approval and command permissions are not written or weakened. | Installing hooks is the owner's grant of a command that runs on every covered Cursor call. It is shown before it is written. |

## User-level install

`langspan install` writes to each host's user configuration
([ADR 0010](../adr/0010-user-level-install.md)): `~/.claude/settings.json`,
`~/.claude.json`, `~/.codex/hooks.json` and `~/.codex/config.toml`. This is a
wider boundary than a project install. A hook written there runs for every
repository the user opens, and `~/.claude.json` also holds the host's account
and per-project trust state.

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Spoofing**: an entry that looks like LangSpan's is treated as LangSpan's, or the reverse | Hooks carry a user-level marker distinct from the project marker and must match the exact generated shape. The JSON MCP entry is LangSpan's only in its exact shape; the TOML table only between LangSpan's marker lines. A `langspan` server LangSpan did not write stops `install` with nothing written and is never removed. | Someone who hand-writes the exact generated shape has written LangSpan's entry, and `uninstall` will remove it. |
| **Tampering**: the install damages or loses the user's configuration | Dry run by default; every file planned and every refusal raised before any write; JSON key order kept; TOML edited only inside LangSpan's lines; symlinked or non-regular files refused; 1 MiB bound; atomic replace keeping the file's mode, new files `0600`, new directories `0700`. `~/.claude.json` is never created. Just before each rename the file is read again and compared with the bytes the plan was made from; a change made meanwhile (Claude Code rewrites `~/.claude.json` often) is kept and planned against once more, and a second change refuses with nothing written. `uninstall` removes a file only when nothing but LangSpan's entries was in it (`{}`, empty text, or Cursor's bare `version: 1`), never `~/.claude.json`, and only while the file still holds the bytes the plan was made from, with the same replan and refusal. | A file the owner kept holding only `{}` or Cursor's bare `version` is removed by `uninstall`; it configured nothing. The moment between that last read and the rename (or removal) remains: with no lock the hosts honour it can be made short, not closed. Same-user races are not an isolation boundary. |
| **Tampering**: repository text switches coordination off | A LangSpan-shaped project hook in a repository makes the user-level hook defer to it. | A committed `.claude/settings.json` can opt a repository out of user-level coordination. The deferred-to hook still runs only if the host trusts the repository. Repository text is read only to look for that marker, never executed or relayed. |
| **Repudiation**: what was installed is unclear later | The dry run prints each file in full; `doctor` reports what is installed for each host, the executable and the state directory. | No install log is kept beyond the files themselves. |
| **Information disclosure** | Nothing is read from the configuration files beyond the keys LangSpan owns and the shape checks; no credential, token or account field is read, logged or printed except as part of a settings file the user asked to see in a dry run. For `~/.claude.json`, which holds the host's account and project state, the dry run shows only LangSpan's entry. The MCP entry carries only the executable path and state directory. | A dry run prints the whole planned `settings.json`, `hooks.json` and `config.toml`, including any value the user keeps in them (for example an `env` block), to the terminal that ran it, as the project install already does. |
| **Denial of service**: a broken user-level hook affects every repository | The hook keeps the project form's budget, gives no decision on any failure, exits 0, and prints nothing outside a Git repository or when a project install covers the repository. | A missing executable makes the host report a failed hook on every event until `uninstall` or a reinstall; `doctor` names it and the command to run again. Under Homebrew the entries name the `opt` link rather than the versioned keg, so `brew upgrade` does not cause this; that link is resolved on every run, and anyone who can change it can already replace the binary in the same Homebrew prefix. |
| **Elevation of privilege** | No permission, trust or approval setting of either host is written or weakened. Codex still requires its own review of the new hook; Claude Code's own permission flow still governs every tool call. The hook never answers `allow`. | Installing a user-level hook is the owner's grant of a command that runs on every session; it is shown before it is written. |

## Setup, the login service and the on-demand start

`langspan setup` ([ADR 0035](../adr/0035-one-command-setup.md)) runs the
user-level install with the skill, writes a per-user login service (a
LaunchAgent on macOS, a `systemd --user` unit on Linux) and runs the service
manager. A hook or `langspan mcp` that finds no coordinator starts one as a
child process. These are new boundaries: a file the service manager executes
at every login, external commands (`launchctl`, `systemctl`), and a
long-running process started from a host's hook.

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Spoofing**: a service file that is not LangSpan's is taken for one, so it is replaced, removed or trusted as covering a state directory | LangSpan's file carries a marker line and is recognized only with it. A file at the path without it stops setup with nothing written, and `uninstall` leaves it in place. | A hand-written file with the marker is treated as LangSpan's, as with hook entries. |
| **Tampering**: the service runs something other than the installed binary | The file names the absolute binary `install` resolves (the Homebrew `opt` link under Homebrew) and the absolute state directory; the unit refuses paths that systemd would split or expand (whitespace, quotes, `\`, `%`, `$`); the plist escapes XML markup. The file is written owner-only (`0600`) with an atomic rename, and shown in full before it is written. | Anyone who can write the user's `~/.config/systemd/user` or `~/Library/LaunchAgents`, or the binary, can already run code as the user. |
| **Tampering**: two hooks start two coordinators for one state directory | The daemon's own `daemon.lock` stops a second one before it touches the socket. A caller starts a process only under `autostart.lock`, after checking again, and not within 30 seconds of a recorded start. Tested with two concurrent real hooks (`tests/setup.rs`). | None known. |
| **Repudiation**: a coordinator runs and nobody knows who started it | The caller logs `coordinator_started` with the process ID to its stderr, and a line naming itself to `daemon.log`; the service manager's own logs record a service start. | `daemon.log` is plain text in the owner-only state directory, not tamper-evident. |
| **Information disclosure**: the coordinator inherits a host's secrets | The on-demand start clears the environment and keeps an allow-list (`HOME`, `PATH`, locale, `XDG_*`, `SSH_AUTH_SOCK`, `GIT_SSH*`, desktop session variables, `LANGSPAN_STATE_DIR`, and the host configuration directories `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`, which name directories, not secrets); API keys and tokens in the host's environment are dropped. The login service file names only `PATH` and those three host directories from setup's environment. Setup reads no host credential or sign-in file: it reads only the files `install` reads, with the same key-level rules. `daemon.log` is created `0600` inside the `0700` state directory. | `SSH_AUTH_SOCK` lets the coordinator use the user's SSH agent, which remote publication already needs. |
| **Denial of service**: starting the coordinator holds a hook past its budget, or a failing coordinator is restarted from every hook | A hook spends at most half of its remaining budget, including the lock wait, then answers as before; `langspan mcp` waits at most 2 seconds. A start within the last 30 seconds is not repeated. The started process has standard input and output on `/dev/null`, so the host never waits on it. Every `launchctl` and `systemctl` call is bounded to 10 seconds. | A coordinator that crashes at start is tried again every 30 seconds by hooks, or every 5 seconds (systemd) or 10 (launchd) by the service, each adding to `daemon.log`. The on-demand start moves a log over 8 MiB aside; the service's log is not rotated. |
| **Elevation of privilege** | The service is per user (`gui/UID`, `systemd --user`), never system-wide, and no `sudo` is run. `--home` installs no service, so a spare home cannot change the login user's manager. `LANGSPAN_AUTOSTART=off` turns the on-demand start off. | A user-level hook already runs code on every session; the on-demand start makes that code also leave a process running. |

## Host detection

`langspan hosts`, `doctor` and the daemon's hourly host check (roadmap A7,
[ADR 0018](../adr/0018-other-hosts-after-launch.md)) read each host's
configuration directory entry and LangSpan's own entries in its hook and MCP
files. The daemon now reads files in the user's home, which it did not before,
and records what it finds in the local store.

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Spoofing**: an entry that looks like LangSpan's is taken for an install | The same exact-shape recognition as `install` and `doctor`. | A hand-written entry of the generated shape counts as an install, as it does for `uninstall`. |
| **Tampering**: detection changes a host's configuration | Detection has no write path: it uses the read functions of `install` only (1 MiB bound, no symlinks or special files). A test compares every file under the home before and after `hosts`, the daemon's checks and `doctor`. | None known. |
| **Tampering**: a client records a false host | `observe_hosts` takes at most 16 names of 1–32 lower-case letters and hyphens; anything else is refused with nothing recorded. | Any same-user client can send it, as with every other observation; the next check replaces it. |
| **Repudiation** | Each change of a host's state is an event (`host_not_installed`, `host_cleared`) with the daemon's time. | Who installed a host later is not known. |
| **Information disclosure** | Only the host's name is stored; no path, setting or file content leaves the check. Agents are not told: the line is for the owner's `status`, dashboard and `doctor`. | `hosts --json` prints the configuration directory paths to the terminal that ran it. |
| **Denial of service** | One bounded check per interval (default one hour, at least 100 ms) in its own thread; unreadable directories are `unknown`, never absent, and a held host that is `unknown` keeps its notice rather than being cleared. A failed check or inspection is logged once per new failure. | A host directory on a hung network filesystem can stall that thread; it does not block the state owner or a hook. |
| **Elevation of privilege** | LangSpan never installs, upgrades or edits a host by itself; the note only names the command for the owner to run, with this coordinator's state directory single-quoted for the shell. | None. |

## The status line

`langspan statusline` (roadmap H2, [ADR 0019](../adr/0019-person-facing-surfaces-and-usage-accounting.md))
adds three boundaries: Claude Code's status-line input on stdin, the
system's time-zone file for the local offset (the file `TZ` or
`/etc/localtime` names, at most 256 KiB, parsed as TZif with bounds-checked
counts; anything else gives no offset and midnight UTC is used), and a `statusLine` entry in the user's `~/.claude/settings.json`. The coordinator gains a read (`status_line`) and
one reported event (`record_refusal`, a held commit).

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Spoofing**: a same-user process reports refusals that did not happen | `record_refusal` accepts only `commit_held`, for a registered session, with valid relative paths; the other kinds are recorded only by the admission that refuses them. | Any client of the owner-only socket can inflate the held-commit count, as it can already fabricate other cooperative observations. The count is for a person's glance, not a gate or a figure of saving. |
| **Tampering**: host input chooses what is counted, or the install damages settings | Stdin is read only when not a terminal and `--root` is absent, within the deadline and the hooks' 8 MiB bound, and ignored beyond 64 KiB; parsed as JSON for `workspace.current_dir` and `cwd` only (at most 4096 bytes each); the directory only selects which registered workspace's counts are shown. The `statusLine` is written by the installer's guarded writer only where none is set, with a marked command whose arguments are single-quoted; another is never replaced or removed. | A hand-written `statusLine` ending in LangSpan's marker is treated as LangSpan's and removed by `uninstall`. |
| **Repudiation** | Each refusal is an ordered event with its session, kind and time; a retried request is answered from its receipt and adds none. | The held-commit report can fail (the coordinator down); the hold still happens and the hook's diagnostic says the refusal was not recorded. |
| **Information disclosure**: the line shows another session's work | The line and `--json` carry counts and the workspace ID only, never a path, a session's task or a peer ID. Nothing is printed to the model: Claude Code shows the status line to the person only. Other stdin fields (transcript path, model, cost) are not read. | Anyone who can run the command as the owner sees the counts of every registered workspace by naming its directory, as `status` already shows. |
| **Denial of service**: the line slows the host | One socket call; the whole command is bounded by its deadline (200 ms, 100 to 5000); the zone file is read once, with no child process. On any failure it prints the unavailable line and exits 0. It never starts the coordinator or uses the network. The daemon answers from memory, keeping at most 1024 refusals from the last 48 hours. | Claude Code runs the command after each message; one small file read and one call each time. |
| **Elevation of privilege** | The command writes nothing to the state directory and records nothing; it grants no admission and changes no permission. | Installing the status line is the owner's grant of a command run after each message; it is shown before it is written. |

## Desktop notices

`langspan notify` (roadmap H3, [ADR 0019](../adr/0019-person-facing-surfaces-and-usage-accounting.md))
adds one boundary: the daemon starts the platform's notifier (`osascript` or
`notify-send`, found on its `PATH`) as a child process, with text built from
recorded events. It is off by default.

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Spoofing**: another program answers to the notifier's name | Only an absolute directory of the daemon's own `PATH` is searched, for a regular executable file; relative entries are skipped. | Whoever controls the owner's `PATH` can already run any program as the owner. A same-user socket client can cause notices by fabricating refusals or handoffs, as it can fabricate other cooperative records; the limits bound how many are shown. |
| **Tampering**: a path, workspace name or session ID changes what runs | Title and body are passed as arguments, never interpolated into AppleScript (`on run argv`) or a shell; `--` precedes them for `notify-send`, and the title comes first. Control characters are replaced. | `notify-send` servers may render simple markup in a body, so a file name holding tags can change how a notice looks, not what runs. |
| **Repudiation** | Each notice sent or dropped is a log line with its kind and reason. No event is recorded. | The log is stderr and is kept only as long as whoever started the daemon keeps it. |
| **Information disclosure**: a notice shows work on screen | Off unless the owner switches it on. The body names a directory name, short session IDs and at most three paths in 200 bytes; never a prompt, a message's or handoff's text, check output or file content. Nothing reaches the model and nothing leaves the machine. | Paths appear on the desktop and, depending on the system, in its notification history, where anyone at the screen can read them. |
| **Denial of service**: notices slow coordination | Notices are raised after commit and handed to a dedicated thread through a queue of 16 with `try_send`; nothing waits for a notifier, which is killed with its process group at 2 seconds. At most one notice of a kind per 5 minutes and 10 an hour. | A notifier that always hangs holds the thread 2 seconds per notice, within the limits; dropped notices are logged. |
| **Elevation of privilege** | The notifier runs as the owner with null stdio and the daemon's environment; it grants no admission and changes no state. `notify on|off` writes only `config.json`, owner-only, through a rename, refusing a file others can read. | Switching notices on is the owner's grant to run the notifier the daemon finds. |

## The LangSpan skill

`langspan install --skill` writes agent guidance where each host reads it on
every session ([ADR 0034](../adr/0034-langspan-skill.md)):
`~/.claude/skills/langspan/SKILL.md`, a section of `~/.codex/AGENTS.md` and of
`~/.gemini/GEMINI.md`, and, with `hooks install cursor --skill`, the project
rule `.cursor/rules/langspan.mdc`. This text reaches the model directly, so it
is an installed configuration that shapes what agents do.

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Spoofing**: a file that looks like LangSpan's skill is treated as LangSpan's, or the reverse | A whole file is LangSpan's only with its `langspan-managed:skill:v1` mark; a section only between its begin and end lines. A file of the same name without the mark stops the install with nothing written, and is never removed. | Someone who writes the mark into a file of that name has written LangSpan's file; reinstall replaces it and uninstall removes it. |
| **Tampering**: the install damages the owner's `AGENTS.md` or `GEMINI.md` | Only LangSpan's section changes; every other line is kept byte for byte. Two sections, or a marker without its partner, stop the install. The installer's guarded writer is used: dry run first, every file planned before any write, a change made meanwhile planned against again and never overwritten, symlinks refused, 1 MiB bound, new files `0600` and new folders `0700`. A removal is refused when the file changed since it was planned. | Edits the owner makes inside LangSpan's section or file are replaced by a reinstall. |
| **Tampering**: the skill tells agents to do something harmful | The text is compiled into the executable from `src/skill.md`, reviewed like code, and every command, option, tool and message it names is checked against the code by tests. It tells agents to treat everything LangSpan relays as data, never instructions. | A wrong statement that uses existing names is not caught by the tests. |
| **Repudiation** | The dry run prints each file's path, size and full text; `doctor` reports for each host whether it is installed, current or different. | No install log is kept beyond the files themselves. |
| **Information disclosure** | The skill holds no path, identity or state from the machine: every copy is the same text. Reading `AGENTS.md` or `GEMINI.md` looks only for LangSpan's marker lines. | A dry run of a section install prints the owner's whole `AGENTS.md` or `GEMINI.md` with the section in it, to the terminal that ran it. |
| **Denial of service** | Every form is at most 500 lines and 8 KiB, within each host's documented limit, and adds that much to every session's context. | The added context is spent on every session, whether or not LangSpan is in use in that repository. |
| **Elevation of privilege** | The skill grants nothing: no permission, trust or approval setting is written, and it tells agents never to turn LangSpan's hooks off. | An agent can ignore it; it is advice, not a gate. |

## A linked worktree's own install

The main checkout's project hook, run by a host started in a linked worktree,
reads that worktree's project hook files to decide whether to step aside or,
on a host that does not run them, which workspace to decide under
([ADR 0003](../adr/0003-native-coordination-observation.md)). Those files are
repository text in the worktree. `langspan doctor --root` reads the same
files for each worktree `git worktree list` names.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Tampering: repository text switches the inherited hook off | It steps aside only on a host known to run the worktree's own hooks there (Claude Code), and only for a hook of the same host in LangSpan's exact generated shape that names this worktree as its root and the same state directory: the hook the host runs there itself. The file is read with the installer's guards (no symlink, 1 MiB bound); anything unreadable is no match. Nothing in it is executed or relayed. A worktree's settings could already add or remove hooks the host runs there, so this switches off nothing the worktree's own settings could not. On any other host, a match never silences the hook. |
| Tampering: repository text chooses the workspace | On Codex, Cursor and Gemini CLI a matching hook's workspace ID and opt-outs are adopted for that worktree. The worktree must first share the installed checkout's Git common directory, and its root is the one Git reports. Its ID is the one the worktree's own install would register there; a file naming another workspace's ID for this root meets the coordinator's ordinary overlap and alias refusals. Opt-outs only ever narrow: either install's `off` holds. |
| Information disclosure | Only the hook command's own words are parsed; no other setting in the file is read or printed. `doctor` prints the worktree path, file path and workspace ID, as it already does for the main checkout. |
| Denial of service | Stepping aside costs a few `stat`s and one small JSON read, with no Git process and no coordinator call. Deciding instead costs two bounded `git rev-parse` calls within the hook's deadline, as covering a worktree without an install already did. `doctor` runs one `git worktree list` with a 5 s deadline and a 1 MiB output bound. |

## Session events and peer briefings

[ADR 0004](../adr/0004-automatic-peer-briefing.md) adds one new flow: text
derived from one agent session is placed in another session's model context.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | A briefing names sessions by the host-supplied session ID recorded through the owner-only socket. Any same-user process can register a session and claim work; LangSpan does not authenticate agents to each other. |
| Tampering (prompt injection) | A peer's task line is the first line of that owner's prompt; file paths come from the repository. Both are attacker-influenced if a peer session or the repository is. Control characters, Unicode line and paragraph separators, bidirectional overrides and zero-width characters become spaces; every such value is quoted with its own quotes escaped, capped at 200 bytes, each peer at 700 and the whole briefing at 2,400, under a fixed header stating that the lines are data, not instructions. This reduces, and cannot remove, the chance that a host model follows text in a briefing. A briefing never carries a permission decision, so it cannot allow or deny anything by itself. |
| Repudiation | Each delivery is an event (`briefing_delivered`) with the signature of what was delivered; prompt summaries and session ends are events. |
| Information disclosure | One line of each prompt, and no other line as text, is stored in the local event log, appears in `status`, and is shown to other sessions of the same user in the same repository: its first line marked `Task:`, else its first non-empty line, else, when a live peer's latest prompt in the repository begins with the same lines, its first line that differs (issue #114). To find that line the hook also sends a 32-hex-character SHA-256 prefix of each of the prompt's first 32 non-empty lines, computed after redaction. They are stored in the event log and the session's activity, visible to same-user clients of the socket through `status` and `events`, never in a briefing and never published; a short or common line can be guessed from its digest by someone who can already read the store. The core accepts at most 32 lower-case hex digests of at most 64 characters. Before that line, an intent summary, a note, a message or a handoff is sent to the daemon, one redaction function in the hook adapter replaces likely secrets with `[redacted]` (provider key shapes, JWTs, PEM blocks, URL passwords, secret-named assignments and query values, `--password`/`--token` arguments, and long high-entropy tokens under a conservative rule) and the owner's home prefix with `~`; the daemon applies it again on receipt. A prompt or intent summary is held to its byte limit after redaction: one that would outgrow it keeps its longest redacted prefix that fits, ending in `[redacted]`, so a summary is never refused, and its prompt shared without one, because it held a secret. A secret in a shape the rule does not know, such as an all-hexadecimal or all-lower-case token, is still shared. `hooks install --prompt-summary off` stops recording prompt text. Nothing leaves the machine unless publication is enabled. Workspaces in other repositories are never briefed about each other. |
| Denial of service | Session events share the 1.5 s hook budget and the daemon's bounded queue. Activity is 32 paths per session; a briefing reads at most 16 peers. Each prompt adds one receipt and at most three events; they are kept until compacted (see Retention). |
| Spoofing (repository grouping) | `observe_workspace_git` is accepted from any client of the owner-only socket, like the other observation commands. A same-user process can therefore place two workspaces in one repository and make their sessions briefed about each other. It cannot affect admission. |
| Elevation of privilege | Lifecycle hooks print context only. Every failure, including an unavailable daemon, prints nothing and exits 0. |
| Recent change at session start (roadmap C3, [ADR 0023](../adr/0023-messages-presence-and-session-continuity-after-launch.md)) | No new boundary: the section is derived from core state the daemon already holds (sessions' edited paths, refused writes, interface notices) and the line journal already on this machine, and reaches only the starting session's own model, as a briefing does. Tampering: its paths and signatures are repository- or peer-influenced; each is cleaned and quoted like peer text, under a heading that labels them coordination data, not instructions, in at most 600 bytes of the 2,400-byte briefing. Information disclosure: it names files ended sessions of the same user edited in the same repository and that their lines are uncommitted, never file content, prompt text beyond the existing task line, or any model output; nothing new is stored or published. Spoofing: a same-user client of the socket can fabricate the edits or notices it reports, as for peer lines. Denial of service: each list is capped (8, 4, 4, 4) and the journal confirmation has a quarter of the hook's budget, after which the section is left out. It never holds or refuses a write. |

Guarded by tests: unrelated repositories and idle, exited, stale-generation or
aged-out sessions are excluded; an unchanged situation is not delivered twice;
output stays within its cap; a switched-off prompt summary and a slash command
never reach a peer; subagent events and a working directory outside the
installed repository print nothing.

## Source versions

[ADR 0005](../adr/0005-source-versions-and-stale-writes.md): the hook reads a
workspace file to hash it, and a hook-supplied digest decides an admission.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing / tampering | Versions are supplied by clients of the owner-only socket. A same-user process can record a false read version for a session and so cause or avoid a `stale_base` for it. It cannot write files or grant admission to another session's paths. The daemon maps every observed path through the same canonical file-key checks as an admission. |
| Information disclosure | Only a SHA-256 of file content is stored, never content. A digest confirms a guess of a small file's content to anyone who can read the event log, which is owner-only. |
| Denial of service | The hook reads at most 4 MiB per file and at most 64 files, inside its existing deadline; larger files are not hashed. Records are capped at 128 per session. |
| Elevation of privilege | The only new decision is another `deny`. A failure to read or record a version never denies: the file is admitted as `base_unknown`. This is fail-open by design and is reported, because refusing every edit of an unreadable file would make LangSpan stricter than the host it sits beside. |
| Symlinks | A file key that is a symlink is not followed for hashing; it has no version. |

## End of a reply, tool batches and Codex snapshots

[ADR 0027](../adr/0027-turn-and-batch-events.md) adds four host events
(`Stop`, `StopFailure`, `SubagentStop`, `PostToolBatch`), registers Codex
`Stop` and `SessionEnd`, and adds one source-version origin. The input is the
host's event JSON, bounded at 8 MiB like every hook event; no new socket,
file or network path is added.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | A stop names its session by the host-supplied session ID, as every hook event does. Any same-user process that can reach the owner-only socket can stop a session and so complete its open tool admissions early; it could already complete them one by one with `complete`. A stop releases only the named session's tool admissions, never another session's or a manual admission. A Codex event that names a subagent is refused, so a Codex subagent's stop is never taken for its parent's. |
| Tampering (prompt injection) | Of the new payloads only `session_id`, `agent_id`, `cwd`, StopFailure's `error` and each batch call's `tool_use_id` are read. `last_assistant_message`, `error_details`, `background_tasks`, tool inputs and tool results are never read or stored. `error` is kept only as 1 to 40 lower-case letters or underscores, otherwise as `unknown`; the core refuses any other value. A batch's briefing is the existing bounded, labelled briefing. |
| Repudiation | Each stop is a `session_stopped` event naming the admissions it released, after one `lease_completed` per admission; each batch is a `tool_batch_observed` event. A Codex snapshot is a `source_observed` event with origin `snapshot`. |
| Information disclosure | Recorded: session, lease and operation IDs, a call count, an error type, and for a snapshot a SHA-256 of the file, as for any version. No model output, prompt text or file content. |
| Denial of service | A batch names at most 64 operations; a stop touches only its session's active leases. Both run inside the existing 1.5 s budget, and a failure prints nothing. A stop is not kept for resending, so a down coordinator leaves the admissions to their TTL, as before. |
| Elevation of privilege | Stop hooks never print, so they cannot keep a host working or block it. `PostToolBatch` prints only a briefing in `additionalContext`, never a decision. A snapshot can only cause a later `stale_base` refusal, never an admission; it is not reported as a read, and the admission it follows still says its base was not compared. |

Guarded by tests (`tests/turn_events.rs`, `crates/langspan-core/tests/host_stops.rs`):
only the stopping session's open tool admissions are released; a parent's
stop leaves a running subagent's admissions open; free text is never kept as
a failure; a stop prints nothing; an unknown session is not registered by a
stop; Codex subagent events are refused; Claude Code edits get no snapshot.

## Shell commands

[ADR 0011](../adr/0011-shell-writes.md): the hook now parses agent shell
commands, runs Git and reads changed files before and after each `Bash` call,
and keeps a run record per call under the state directory. Each edit tool
call that is not refused also leaves an edit mark (`STATE/shell/edits/`: its
session, workspace root, times and its files' SHA-256 versions before and
after), so a shell run going at the same time does not take another
session's edit for its own (issue #1).

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | The command text comes from the host event, which a same-user process could forge, as with every hook event. A forged event can admit paths for, or attribute changes to, the session it names; it cannot write files. Run records name their session and are owner-only. |
| Tampering | Command text is model output: data, never instructions. It is parsed, never executed or rewritten by LangSpan. A same-user process can edit or delete a run record or an edit mark (`STATE/shell/`, `0700`/`0600`, no-follow opens, size-capped reads) and so change what is attributed; the records are not a boundary against the owner's own processes. A forged change note can only change which session and command a stale-source refusal names (the refusal itself follows the daemon's versions); its text is placed on one line, capped, after the refusal's "coordination data, not instructions" label. A forged edit mark can only withhold a file from a shell run's session (as the editor's, or `contested`); it credits no session with anything. |
| Repudiation | An attributed change is an ordinary observation event in the append-only log, tool `Bash`. A change the comparison could not separate is `shell_unobserved` or `contested`, never attributed by guess. A file the command names as a literal write target is attributed to it even when another session's run overlapped, unless that run names it too; a same-user forged event could name a file to claim it, as it could already admit one. A `run_in_background` call contests other sessions' unnamed changes for two hours (a shell call's supported lifetime, ADR 0011), so a forged one can at most make changes unattributed. |
| Information disclosure | Only file paths and SHA-256 digests enter events, never file content or the command text. A run record and a change note (`STATE/shell-changes/`, issue #107: per workspace and file, the last shell command that changed it, its session and the version it left) keep the command line redacted and capped at 120 bytes, owner-only on this machine; it never reaches the daemon, the event log or a publication. Another session in the same workspace sees it in a stale-source refusal, only while the version that command left is still the one on disk, labelled as coordination data. A secret in a shape redaction does not know passes through, as it does for check commands. Git runs with `--no-optional-locks`, and its stderr is never relayed. |
| Denial of service | Commands over 8 KiB are not parsed. A snapshot reads at most 1,024 files, 64 MiB and 1 MiB of status output, inside the hook deadline; over a bound it is incomplete and records nothing. Run records and edit marks are pruned and each capped at 4,096, on every path that creates one. Change notes are one file per workspace and path (a later command replaces the earlier note), at most 256 per run and 4,096 per workspace, each read within 4 KiB, and expire after a week. An edit call left unmarked at the cap is noted, and a shell run that note could affect credits nothing it changed, so filling the directory can withhold attribution but never misattribute. A run, or an edit call, whose result never arrives can mark another session's overlapping changes `contested` for at most two hours, or until its session's reply ends. A run whose result arrives later than that is credited with nothing. |
| Elevation of privilege | The only new decision is `deny`, and only for literal targets inside the workspace with a current holder or a stale version. A parse failure, an incomplete snapshot, a merge or rebase in progress, or any other failure gives no decision; the host's own permission flow is untouched. |
| Isolation claim | Shell coverage is partial and cooperative. Writes outside the workspace, in ignored files, by background processes after the call returns, or by unhooked programs during it are not separated; see the ADR's "Never covered". |

## Commits in a shared checkout

[ADR 0026](../adr/0026-commit-guard-and-commit-mine.md): the hooks keep a line
journal under `STATE/journal/`; the `Bash` pre-tool hook reads the index and
working tree through Git before a staging or commit command and can deny it;
`langspan commit --mine` and `langspan_commit_mine` write a commit and the
shared index. `langspan status --mine` and `langspan_my_changes` (issue #40)
run the same reads and attribution and write nothing: no private index, no
commit, Git with `--no-optional-locks`. They add no boundary; their output,
like the deny reason, names other sessions' IDs, paths and line numbers, never
line content, and is labelled as data in the MCP tool's description.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Journal entries name the session the hook event named, which a same-user process could forge, as with every hook event. A forged entry can make a change look like another session's (so a commit is held once, or `commit --mine` leaves it out) or like the forger's own (so `commit --mine` takes it). The CLI's `--session` and the MCP tool's `session_id` are claims, like every session ID; without `--session`, the session comes from a running shell record its own pre-tool hook wrote, and two or none is an error. A session's open records are closed when its reply or the session ends, so a forged Stop or SessionEnd can at most make that session's own `commit --mine` ask for `--session`. |
| Tampering | Command text is model output: it is read to find Git steps, never executed or rewritten. Repository paths and diff text are data. When `commit --mine` finds no running session, the `--state-dir` a LangSpan hook in the checkout's settings names is only suggested in the error, quoted, never used, and dropped if it holds a control character. The guard's Git runs read-only with `--no-optional-locks` and bounded output. `commit --mine` builds the commit in a private index under the Git directory, runs the owner's commit hooks, and moves the branch with a compare-and-swap `git update-ref` from the `HEAD` it read, so a commit another session made meanwhile is never undone (it plans again on top once, or refuses); and writes the shared index only for the files it committed, applying exactly the hunks it attributed (`git apply --cached`, which checks the preimage). A same-user process can edit journal files (`0700`/`0600`, no-follow opens, size-capped reads, blob contents checked against their digests). Entries sit in one directory per path, named by a digest of the path; an entry is used only for the path it names, so one moved into another path's directory is ignored there, and one moved out of its own is missed, as a deleted one would be. Entries an earlier version kept flat are moved into those directories on first access; a file there that is not a readable entry is deleted. |
| Repudiation | A held command is a hook deny with coverage `denied_commit` and the named sessions in its reason; it is not an event. `commit --mine` makes an ordinary Git commit under the owner's identity and prints or returns which files it took and which it left. Nothing is attributed by guess: an unattributable hunk refuses the whole commit. |
| Information disclosure | The journal keeps copies of uncommitted file versions up to 512 KiB, line digests and paths, owner-only, for up to a week, on this machine only: not sent to the coordinator, briefings or publication. The deny reason names other sessions' IDs, paths and line numbers, not content. When a Git step of `commit --mine` or one of the guard's Git reads fails, the error (the CLI's stderr, the MCP error, or the hook's log line) names the step (at most 160 characters, redacted) and carries Git's standard error as `git::said` bounds it for a failed fetch or push: at most 1 KiB of its last six lines, control characters removed, passed through the same secret redaction, labelled `Git said:`. It is Git's text about the owner's own repository, shown to the owner's own agent; nothing is decided from it except whether to add the read-only Git directory hint. `git commit`'s own output goes to the command's stderr, as it would if the agent ran it. |
| Denial of service | Commands over 64 KiB are not read. The guard reads at most 16 MiB of diff and 1,024 untracked files, inside two thirds of the hook's remaining budget, and gives no decision past a bound. Journal entries are capped at 4,096 per workspace and kept file versions at 4,096, both expiring after a week. A forged entry can hold a command once. |
| Elevation of privilege | The only new decision is `deny`, once per command and foreign change set; the same command again proceeds, so the capability stays with the agent. A parse it cannot be sure of, an unreachable coordinator or a failed listing gives no decision. `commit --mine` runs as the owner with the owner's Git configuration and hooks (none is skipped), and never pushes. |
| Isolation claim | The guard is cooperative and reads only commands in the `Bash` tool's text: Git run by scripts, aliases or other programs is not seen. Line ownership is by content; see the ADR's "Never covered". |

**The formatter notice** (issue #126, [ADR 0011](../adr/0011-shell-writes.md)):
before a `Bash` command that runs a recognised formatter, the pre-tool hook
reads the same journal, the command's own workspace snapshot and the
coordinator's session list, and adds context naming other live sessions and
their uncommitted paths. It adds no boundary: the inputs are the guard's, and
the output goes to the session's own model, as a briefing does. Command text
is model output, read only to recognise a formatter, never run or changed. The
notice is labelled as coordination data, names session IDs and at most eight
paths (each cleaned of deceptive characters, the list capped at 600 bytes),
never content, and makes no permission decision: a forged journal entry can at
most add a path to one notice, and a missing one leaves a notice out. It is
given once per set of files (the guard's "told" record), so it cannot repeat
into every call; an unreachable coordinator gives no notice. Formatters run by
scripts or `npm run` are not recognised.

## Gemini CLI

[ADR 0033](../adr/0033-gemini-cli-adapter.md) adds a third host. Its hook is a
new command in `.gemini/settings.json` or `~/.gemini/settings.json`, and its
events are a new input. The other boundaries (socket, store, shell
comparison) are unchanged.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Gemini's events carry no tool-call ID. The adapter makes one from the session, the tool, the file or command, a count and a per-call nonce kept as marker files under `STATE/gemini-calls/<digest of the session ID>/` (a digest because the ID grammar admits `.` and `..`) (`0700`/`0600`, exclusive creation, linked into place). The nonce stops a resumed session, which keeps its session ID, from repeating an earlier call's ID. A same-user process can forge an event or create markers, as with every hook event: it can shift which number a call gets, and so make one call's report count as another's. It cannot write files or admit another session's paths. The session ID is the one Gemini sends; nothing authenticates it. |
| Tampering | Tool input and the prompt are model or user text: data, parsed into typed fields, never run. A deny's reason is the same labelled, capped text the other hosts get. Gemini replaces `<` and `>` in context it adds, so LangSpan's text cannot close the tags Gemini wraps around it. The installer refuses a path containing `$`, because Gemini expands `$NAME` in settings strings and in hook commands, which would run a different command than the one shown. |
| Repudiation | Gemini events produce the same events and receipts as the other hosts, under sessions named `gemini.<id>`. Each installed hook is named `langspan`, so Gemini's `/hooks` lists it under that name and `hooksConfig.disabled` can switch it off. |
| Information disclosure | Gemini keeps hooks and MCP servers in one user file, which may hold the owner's MCP server credentials in an `env` block. The dry run of `install` shows only LangSpan's hook and MCP entry, not the rest of the file. LangSpan reads that file only to plan its own entries. The marker files are named by a digest of the tool and its target and hold a random-looking nonce, no content. The hook does not read the transcript that `transcript_path` names. |
| Denial of service | The same 8 MiB event bound and 1.5 s budget; the installed timeout is 5,000 ms. At most 4,096 calls on the same target are numbered per session; the next gets no decision. A session's markers are removed at `SessionEnd`, and directories older than a week when another session first makes a call. |
| Elevation of privilege | The hook never answers `allow` and never exits 2. Every outcome other than a deny prints `{}`, including a hook that cannot start or panics: empty stdout would make Gemini parse the stderr log as the answer, and it reads plain text there as `allow`. The MCP entry does not set `trust`, so it bypasses none of Gemini's confirmations. With folder trust on, project hooks load only in a folder Gemini trusts; Gemini's documentation says it warns before a new or changed project hook first runs. |

## Antigravity

[ADR 0035](../adr/0035-antigravity-adapter.md) adds a fifth host. Its hook is
a new command in `.agents/hooks.json` or `~/.gemini/config/hooks.json`, its MCP
entry a new server in `.agents/mcp_config.json` or
`~/.gemini/config/mcp_config.json`, its skill a new rule file, and its events
a new input. Antigravity's App, CLI and IDE all read these files, so one
install reaches all three. The other boundaries (socket, store, shell
comparison) are unchanged.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | The installed command names its event with `--event`, and an event name LangSpan does not register stops the hook with `{}`; the `hookEventName` that Antigravity 1.5.14 also sends is not read. A tool call is identified by the conversation's `conversationId` and the call's `stepIdx`. Nothing authenticates either, as with every hook event: a same-user process can forge an event under another conversation's ID. If Antigravity were to give two calls of one conversation the same `stepIdx`, the second would be answered from the first's receipt; that `PreToolUse` and `PostToolUse` share it is unverified. |
| Tampering | Tool arguments and `lastUserInput` are model or user text: data, parsed into typed fields, never run. A deny's reason is the same labelled, capped text the other hosts get; the briefing goes out as a system message in `injectSteps`, labelled as data, not instructions. Antigravity keeps injected steps in the conversation's history, so a briefing is sent only when it changed. LangSpan's entries live in its own hook definition, named `langspan`; the owner's definitions are not read for anything but their entries' ownership markers, and are written back unchanged. |
| Repudiation | Antigravity events produce the same events and receipts as the other hosts, under sessions named `antigravity.<conversationId>`. `/hooks` in Antigravity's CLI lists LangSpan's definition by its name. |
| Information disclosure | The hook reads `lastUserInput` only on the first model call of an execution, for the prompt summary (unless `--prompt-summary off`) and the code briefing, as other hosts' prompts. It does not read the transcript or artifact directory the event names. The MCP entry carries no `env` or `headers`. |
| Denial of service | The same 8 MiB event bound and 1.5 s budget; the installed timeout is 5 seconds (Antigravity's default is 30). `PreInvocation` runs before every model call, so each costs one coordinator round trip within that budget. |
| Elevation of privilege | The hook never answers `auto_approve`, `ask` or any permission that skips Antigravity's own policy, never uses `overwrite` or `permissionOverrides`, and never exits 2. Antigravity 1.5.14 reads `{}` on `PreToolUse` as a refusal, so every `PreToolUse` outcome other than a deny prints `{"decision":"allow"}`, which the documentation describes as passing the call on to the next hook or the default policy; this includes a hook that cannot start or panics once its `--event` is known. That `allow` keeps Antigravity's own prompts in place is the documented meaning, and the repeat real-host run checks it. Every other event's outcome other than a briefing prints `{}`. The MCP entry is a plain stdio server, so Antigravity's own MCP permission rules apply to its tools. |

## Cross-machine publication

[ADR 0006](../adr/0006-remote-awareness-through-git-refs.md) is the first flow
in which data leaves the machine and data from another machine enters. It is
off until the owner runs `langspan remote enable` for a workspace.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Participation is whatever the Git remote authorizes; LangSpan adds no authentication of its own and handles no credential. A payload naming a participant other than its ref is discarded. Anyone with push access can publish under a new participant ID with any display name, including a teammate's name. A briefing shows the name as data and cannot vouch for it. |
| Tampering | A received `state.json` is at most 16 KiB (read bounded before parsing), typed, and validated whole by the core. Fields this build does not know are ignored and never kept, so a newer peer is still heard ([ADR 0015](../adr/0015-version-tolerance.md)); everything that is kept is validated: IDs, 64-byte single-line names, repository-relative paths without traversal, single-line task text, branch names. One invalid field rejects the publication. Rendering then applies the same cleaning, quoting and caps as local peer text. With push access, a peer's ref can be deleted or replaced. Refs under the namespace that are not participant refs are ignored and do not count against the 64-participant limit; a remote holding more than about 10,000 such refs makes the listing exceed its bound and stops receipt for that workspace. The remote name is validated so it cannot be read as a Git option. |
| Repudiation | Every publication that left is a local `remote_published` event with its sequence, content fingerprint, the exact `state.json` pushed and the time of the push; every accepted one is `remote_observed`. `langspan remote outbox` shows what would leave next, and `langspan remote sent` lists what did leave, payload for payload. Records from before payloads were kept are listed without one. |
| Information disclosure | Readable by everyone with read access to the repository: the chosen name, participant ID, host kinds, session ID prefixes, branch names, and paths being or recently edited. Task lines only with `--share-task on`, and only after redaction at the hook. Never file content, digests, other repositories' work or credentials. There is no MCP tool to enable publication. An agent with shell access can run the CLI command as the owner can; the host's permission prompt for that command is the control, and `langspan status` shows what is published. The ref has no history, so a task line the owner stops sharing is no longer reachable from it once replaced. |
| Denial of service | Network work runs on its own thread with a 20 s deadline per pass and never blocks the state owner. Every Git child runs in its own process group, and at a deadline the whole group is killed, so a hanging credential helper, SSH or remote helper cannot outlive it. After repeated failures of the same network operation (fetch, publish, withdraw) the workspace's passes wait 5 s, 30 s, 2 min, then every 10 min, reset by a success and logged once per change. At most 64 peers are read per pass. A peer can publish often; each accepted publication is one event, and retention is still open. |
| Elevation of privilege | Received data is advisory only. It cannot admit, deny, hold or release, and no remote input reaches a shell or a path on disk. |
| The owner's repository | Writes are limited to objects and refs under `refs/langspan/`, through a private index. Pushing that ref skips the owner's pre-push hooks. Network Git never waits for a person: `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=/usr/bin/false`, and `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` unless the owner set `GIT_SSH_COMMAND`, `GIT_SSH` or `core.sshCommand`, which then stays in charge and must not prompt itself. Credential helpers and an SSH agent still answer; LangSpan never sees what they return. Fetches pass an empty `--refmap`, so the owner's remote-tracking branches are not updated by them; when a branch publication's push makes Git create the owner's remote-tracking ref for it, that ref is deleted again (one the owner already had is left, updated by Git as for any push). |
| Branch namespace (`--publish-as-branch`) | Owner-chosen per workspace, never automatic. The publication becomes the branch `langspan/peers/ID`, visible in the branch list to everyone with read access and fetched by their plain `git fetch`. Its tree holds only `state.json`, so CI that reads its configuration from the pushed commit finds none; a host whose CI is configured elsewhere may run a pipeline for it. A hosting rule on branches (protection, required reviews, naming) applies to it. Readers accept both namespaces with the same validation; a participant in both is taken at the higher sequence. |
| Git's refusal text | A failed fetch or push now keeps the end of Git's standard error: at most 1 KiB of its last six lines, control characters removed, then passed through the same secret redaction as every recorded text (`src/hook/redact.rs`: a URL's `user:pass`, provider key shapes, JWTs, PEM blocks, secret-named assignments, high-entropy tokens). It is the server's text: it goes only into the error and the daemon's stderr log, labelled `Git said:`, and it decides nothing except whether a refusal counts as permanent (HTTP 401/403, "refusing", "denied", "protected"), which sets that one push's backoff to its longest wait (10 minutes). |
| Refused withdrawal | Where the host refuses deletes, the ref stays readable; LangSpan replaces it once with a publication marked withdrawn (no work listed) and tells the owner the exact ref to delete by hand. A receiver treats a withdrawn publication as the sender having left. A receiver running an older LangSpan rejects the unknown field and keeps the last publication until it goes stale (ten minutes). With push access anyone could publish a withdrawal for another participant's ref, as they could already delete or replace it. |

Guarded by tests: nothing is on the remote before enabling; another
repository's work is never in a publication; task lines are absent unless
shared; a secret on the first line of a shared prompt reaches neither the
event log, a teammate's briefing, the published `state.json` nor any file of
either daemon; replayed and out-of-order publications are ignored; eleven malformed
or hostile publications are rejected whole; an unreachable remote and a daemon
restart leave local coordination working; disabling removes the ref and the
teammate's view; branches, index and working tree are untouched; a hanging
credential helper is killed with its process group at the deadline while
local coordination goes on; `remote sent` lists exactly the bytes on the
remote. Against a hub
whose `update` hook refuses everything outside `refs/heads/`, every deletion
and every forced update: a branch publication is received by a default reader;
the default publisher's error carries Git's refusal and suggests
`--publish-as-branch`, and is not repeated every pass; a refused withdrawal is
reported with its ref, replaced by a withdrawn publication, and the teammate
stops showing the work. Against an unrestricted hub: branch and default
publishers see each other, a move between namespaces leaves one ref, and the
owner's remote-tracking branches stay untouched.

## Checks, combining and landing

[ADR 0007](../adr/0007-check-evidence-integration-and-landing.md) adds
commands that run an owner-supplied program and that push to a branch, from
the CLI and, through `langspan_integrate_check` and `langspan_land`, from an
agent over MCP. Both interfaces run the same operations in `src/landing.rs`, in
the calling process.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | A check record is whatever a client of the owner-only socket submits. A same-user process can record a pass it never ran, and `land` would accept it. The record is evidence for an honest owner, not a control against a hostile local process. The push turn's holder (`session_id`) is a cooperative label, as for every session ID: any local process can take a turn under any name, or release one whose turn ID it has read from `status`. The same holds for the line of landings waiting for a turn: a waiting landing's ticket is its request ID, visible in `status`, `land-queue` and the dashboard, and any local client can cancel it (`land-cancel`, `langspan_land_cancel`) or join the line under any session name. A cancelled landing pushes nothing; it is not a way to push. |
| Tampering | Branch, remote and target names are passed to Git as arguments, never through a shell, and are refused if they could be read as an option or a revision expression (leading `-`, `..`, `^`, `~`, `:` and glob characters). The check command is executed directly from its argument list; an MCP caller supplies it as a JSON array of 1 to 64 arguments, never as a shell string. Nothing is forced: a lost race is a refusal, and a retry combines the new target again and lands only a tree with its own passing check. **Conflict hunks are repository text relayed to an agent** (prompt injection): a branch can put any line into a conflicting region. The lines are returned as data in labelled fields (`target`, `branch`), at most 4 hunks for each of the first 8 files, 6 lines per side and 200 characters per line, with control characters replaced so a terminal escape is not passed to a terminal or a model verbatim; files over 1 MiB and binary files are named, not read; the CLI prints them under "repository text, shown as data" and the MCP tool descriptions say the same. An inconclusive check's reason names, in the same way, the paths that differed from the combined tree: at most 20, 256 characters each, control characters replaced, passed through secret redaction, with a `data` label in the result; no file content and no command output is in it. Git's conflict type per file is Git's own wording, cut to 40 characters. Combining sets `merge.renames`, `merge.renameLimit`, `diff.renameLimit` and `merge.conflictStyle` itself, so the owner's configuration cannot change what tree is combined (and checked), and conflict markers are parsed in one known style; a repository's `conflict-marker-size` attribute can still make markers unreadable, and then no hunks are shown, only paths and kinds. |
| Repudiation | Every check, with its tree, command line, result and input commits, and every landing, with the check it relied on, is an event. So is every push turn taken, renewed, released or expired, and each release records how each attempt ended. |
| Information disclosure | The check command line is passed through the same secret redaction as check output, then cut to 120 characters, before it leaves the calling process; the daemon redacts it again on receipt, so a `--token` value or a key-shaped argument never reaches the store, and the program name stays readable. A record written by an earlier build, which may hold the command as typed, is redacted as it is read back, so `check status`, the dashboard, briefings, recaps, event pages and a retried request's receipt show the redacted form; the earlier event row itself is not rewritten. Redaction never takes a command line past the 512-byte limit (a line that would outgrow it is cut after redaction and ends in `[redacted]`, so the cut reveals nothing redaction hid), so such a record still recovers instead of stopping the daemon; a retry of a request recorded by an earlier build is compared with its receipt after both are redacted, so it is answered from the receipt rather than refused. Redaction is a safety net: a secret in a shape it does not recognise is still stored. The check's output goes to the calling process's standard error (for MCP, the adapter's log), never into the tool result or the event log. Conflict hunks put a few lines of the conflicting files into the result (never into the event log); a secret committed to one of those lines would be shown to whoever ran the combine, who could already read the branch. |
| Denial of service | Git steps have a 120 s deadline each; the check command has its own limit (30 minutes by default on the CLI, at most 3600 s) and is killed at it with its whole process group. A temporary worktree is removed on every path, including failure. A push turn lasts one attempt's budget (Git steps, snapshots, the check limit, and 60 s) and is renewed before every attempt and every push, so a crashed landing blocks its target only until that deadline, and a daemon restart ends every turn. A same-user process can hold a turn and renew it indefinitely; that blocks landing through LangSpan, never a plain `git push`. A landing that finds the turn taken waits in line for at most its `--wait-s` (default 240 s, at most 3600 s; `wait_s` for MCP) and then exits 7, having fetched and pushed nothing; `--wait-s 0` returns at once. The line holds at most 8 landings per target (a ninth is told the line is full, exit 7), each asks every 0.5 s and writes one event about every 7.5 s while it waits, and one that stops asking for 15 s (killed, suspended) is dropped, so a dead waiter holds up the line for at most 15 s. A daemon restart empties every line. A same-user process can fill the line or stay at its head by asking; as with holding the turn, that blocks landing through LangSpan, never a plain `git push`. Each ask is a request with a receipt, so a long wait adds up to two small receipt rows a second to the store. Rename detection is bounded at 1000 files per side; conflict detail reads at most 8 files of at most 1 MiB, within the same 120 s Git deadline. |
| Elevation of privilege | The check command runs with the owner's privileges, because the owner ran it, or because an agent the owner runs called `langspan_integrate_check`. **That MCP tool is a new trust boundary:** whatever can call LangSpan's MCP tools can run any program as the owner, with the owner's environment, in a temporary checkout. This grants nothing an agent with a shell tool lacks, but it is a second way to run commands, and a host's permission prompt for MCP tools is the only gate on it; an owner who does not want agents to run commands this way should deny `langspan_integrate_check` in the host's settings. `langspan_land` with `apply` pushes, as the owner, with the owner's Git credentials and pre-push hooks, only what the remote accepts without force and only a tree with a passing check; an agent can therefore publish combined work to the target branch without asking, and the host's permission prompt is again the gate. Neither tool rewrites or intercepts the owner's own `git push`. Repository content in a combined checkout is the branches' own code: running a check on a teammate's branch runs that branch's code, as running its tests always does. Tool results contain repository text (paths, branch names) as data. |

## Automatic checks after a session's writes

[ADR 0030](../adr/0030-automatic-checks-after-write-batches.md) makes the
daemon start a child process by itself: the owner's configured check command,
after a session's batch of writes. The new boundaries are that automatic run
and the command's output, which now reaches the event log and the agent's
model.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | A trigger is a `wrote` answer to a tool batch or stop, which any client of the owner-only socket can cause by reporting a write and a batch for some session; it can only make the configured command run sooner or more often, bounded by the debounce and one run per workspace. A check report (`record_check` with `report`) is accepted from any such client, like every check record: a same-user process can record a false result and have it delivered as the check's. |
| Tampering (prompt injection) | The command's output is repository-controlled text (test names, assertion messages, build script output) placed in the model's context. Only its last 20 lines and 1,500 bytes are kept (the core refuses a tail over 2,048 bytes); deceptive characters become spaces, each line is capped at 300 bytes and prefixed with `\| `, one result per briefing, the section capped at 1,200 bytes under a label saying the lines are the check command's own text, data, not instructions. This reduces, and cannot remove, the chance that a model follows text in it. The delivery never carries a permission decision and stop hooks still print nothing. |
| Repudiation | Turning the check on or off, every result (`check_recorded`, then `check_reported` naming the sessions) and every delivery (`check_delivered`) are events. A run cancelled for a newer tree or by `--off` records nothing; its cancellation is not logged. |
| Information disclosure | The output tail is stored in the local event log and sent to the agent's model, so its vendor CLI forwards it to its provider; before, check output went only to the calling terminal. It is redacted with the prompt-text rules first, which miss secrets in shapes they do not know. The command line is stored in full, so a command that redaction would change (a `--token` value, a secret-named assignment, a key shape) is refused by the daemon (`secret_in_command`) from every client. A check that prints environment variables can still disclose them. |
| Denial of service | Off by default. One run per workspace at a time; a burst of batches waits for a pause of `auto_check_debounce_ms` (50 ms to 60 s) and at most 30 s; the command is killed with its process group at its limit (1 to 3,600 s) and whenever it ends. The trigger queue holds 256 and never blocks the state owner; output kept while running is 8 KiB. Each run takes two snapshots of the working tree, plus one per trigger while it runs. If the daemon stops, a running check's process group is not killed and runs to its end. |
| Elevation of privilege | **The command runs outside the agent host**: as the daemon's user, with the daemon's environment, in the workspace root, not in the host's sandbox and not behind its permission prompts. Code an agent writes and the command executes (tests, build scripts, a `Makefile`) therefore runs without the host asking, and a prompt-injected agent can use that to run code the host would have refused. Turning the check on is the owner's grant of this. There is no MCP tool for it; the CLI and any other client of the owner-only socket can set it, so an agent that runs `langspan check auto` through its shell does so under the host's own prompt for that shell command. An owner who runs agents in a sandbox for safety should leave it off. |

### The wider check after an interface change

The wider command (issue #71, ADR 0030) adds a second owner-chosen command to
the automatic check, run outside the agent host like the first, and relays
index-derived file names to more sessions.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | The wider command starts only from an interface notice the daemon's own indexer reported (`observe_interface` reaches the worker only through the state owner, and a replayed request starts nothing). A same-user client can send `observe_interface` itself and so make the configured wider command run, bounded like any trigger: one run per workspace, after the debounce. It cannot choose what runs. |
| Tampering (prompt injection) | The briefing quotes repository text: an export's name and signatures, cleaned and capped (128 and 160 bytes), and at most 8 file paths, cleaned and capped, labelled as a name lookup and not resolved uses, inside the check section's 1,200-byte cap with the output tail's existing label. A crafted signature can still put words in the model's context. |
| Repudiation | The wider result is an ordinary `check_recorded` and `check_reported` event, with the grade and the change; the sessions told are named in the event. |
| Information disclosure | The sessions working on a mentioning file now receive the wider check's output tail, which was before sent only to the sessions whose writes triggered the check. All are sessions of the same workspace on the same machine. Nothing is published to a remote. |
| Denial of service | At most 16 ungraded changes per workspace; the wider run shares the one-run-per-workspace rule, the debounce, its own 1 to 3,600 s limit and the tree-change cancellation, and is never rerun for flaky-test learning. Each boundary in a workspace with a wider command reads the index's coverage once, a bounded query of a local file. |
| Elevation of privilege | **The wider command runs outside the agent host**, as the regular command does: as the daemon's user, not in the host's sandbox. Setting it is the owner's grant, from the CLI or another client of the owner-only socket; there is no MCP tool, and a command that looks like it holds a secret is refused by the daemon. |

### Tests that flip on unchanged code

Learning which tests flip (issue #70, ADR 0030) widens three things: a quick
failing automatic check runs the owner's command a second time; test names
read from command output are stored in the event log and placed in the
model's context with a label that can change how it treats a failure; and
`check run` now reads its command's output instead of passing it straight to
the terminal.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Any client of the owner-only socket can send `record_check` with test names and a rerun's names, so a same-user process can make a test look flaky. It cannot turn a failure into a pass: the label never changes a record's result or what a landing accepts. Results are compared only when their command identities are equal, so two different commands under one check name and one shortened command line never count as each other's flips. |
| Tampering (prompt injection) | Test names are repository-controlled text. Each is at most 200 bytes on one line, without control characters or anything redaction would change, at most 100 and 10 KiB per run; the briefing cleans deceptive characters and shows at most 480 bytes of them, labelled as named in the check's output, before the output lines and their data-not-instructions label. A crafted output can make its own failures look known to flip only by flipping on unchanged code, which a real program can do on purpose: a test written to fail every other run is labelled flaky. The label says the check still failed, and the skill tells agents to read the output before dismissing it. |
| Repudiation | Each result's names are a `tests_recorded` event, and forgetting is `flaky_test_forgotten`; a flip is derived from two of those events, so the log shows why a test is labelled. A rerun that is stopped records nothing. |
| Information disclosure | Names stay in the local event log and the local agent's context; nothing is published to the remote (ADR 0006's payload is unchanged). A name that looks like a secret is left out, whether failed, passed or skipped, by the output parser and again by the daemon on receipt, so a request sent straight to the socket stores none either. A request to forget a test, whose test or check name looks like a secret, is refused by the store before its receipt or any event is written, and the refusal does not repeat the name. Each result also stores its command's identity, a SHA-256 of the whole argument list; `check run` sends none (and no names) for a command that looks like it holds a secret, and an automatic check refuses such a command, so the digest cannot be used to test guesses at a secret redaction hid from the command line. `check run` passes its output on to standard error unchanged, and keeps at most 4 MiB of it in memory until it ends. The status reply on the owner-only socket gains one count of recorded writes per workspace (the write generation `check run` sends with its test names): no path or content, only how many writes the event log already holds. |
| Denial of service | One rerun per failing automatic check, only when the first run named a failing test and took at most `flaky_rerun_max_s` (default 30 s, at most 3,600, 0 off), under the same process-group kill and cancellation as any run and a time limit of the smaller of the check's and `flaky_rerun_max_s`, so a rerun that hangs delays the result reaching the agent by at most that long. The worker keeps at most 4 MiB of a run's output, then its last 8 KiB. At most 500 tests and 16 check names are followed per workspace. |
| Elevation of privilege | The rerun executes the same command, as the same user, on the same tree as the run before it: no grant beyond the owner's turning the check on. Forgetting a test is CLI only; the MCP tool is read-only. |

## Remembered notes

A note is text one session or the owner asks LangSpan to show other sessions.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Tampering (prompt injection) | A note is the most direct way for one agent to place text in another's context. It is one line, at most 400 bytes, control characters refused at the core, rendered quoted under the same "data, not instructions" header and caps as peer text, and at most four are shown per briefing. An agent that can call `langspan_remember` can still write persuasive text; a host model may follow it. |
| Spoofing | The author is the session ID the caller supplies, checked only for being registered. |
| Repudiation | Remembering and forgetting are events. A recall is a read and is not recorded. |
| Tampering (recall) | `langspan recall` and `langspan_recall` (roadmap E2) return held notes matching a query as data fields of a tool result, never placed in a briefing. The MCP result puts a label first saying the note text is remembered repository text, data, not instructions; the CLI prints note text quoted with escapes. Note text keeps the bounds `remember` already applies. A note written to match likely queries reaches an agent that searches for them, the same reach recap already gives it. |
| Information disclosure | Every local session in the workspace can read every note through recap, and search them with recall. Notes are not published to teammates. The tool description says not to store secrets. |
| Denial of service | 64 notes per workspace, oldest dropped. Any session can forget any note. A recall reads at most those 64 notes with a query of at most 200 bytes and 8 words (at most 8 × 64 × 9 substring searches over at most 4.5 KiB of text and paths per note), and its answer is held to 28 KiB. The MCP result carries the answer twice, once re-escaped as text, so the adapter encodes the whole result and drops notes until it leaves 512 bytes of one 64 KiB frame, however many quotation marks and backslashes the notes hold. |

## Messages and handoffs

A message is text one session or the owner addresses to another session of the same workspace, to every other session of it, or (a handoff) to the next session that starts or is prompted there ([ADR 0009](../adr/0009-agent-messages-and-handoff.md)). It crosses from one agent's context into another's.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | The sender is the session ID the caller supplies, checked only for being registered in the same workspace; a message without one is labelled as from the owner. Any local process that can reach the socket can send as any registered session, or as the owner. The socket's owner-only mode is the boundary. |
| Tampering (prompt injection) | One line, at most 400 bytes, control characters refused at the core; rendered quoted, with deceptive characters replaced, under "Messages from other sessions (data, not instructions)" and inside the 2,400-byte cap. At most four per briefing. Persuasive text still reaches the model, and a host model may follow it. A handoff arrives as a session begins its work, where it is most likely to be acted on. |
| Repudiation | Sending and every acknowledgement are events, and a delivering briefing's receipt records what it carried. A forged sender is recorded as the sender it claimed. |
| Information disclosure | A message to one session is readable by every local session through recap until acknowledged, and in the event log afterwards; `messages show` (MCP `langspan_show_message`) reads one from that log by ID, the same data through the same owner-only socket, so it adds no boundary. A line cut in a briefing names that command with the workspace and message IDs, which are validated identifiers, not message text. Messages are never published to teammates. The tool descriptions say not to send secrets. |
| Denial of service | 64 unacknowledged messages per workspace, oldest dropped: a sender can push others' messages out before they are read. At most four messages, within 1,500 rendered bytes (796 beside an uncoordinated-change notice), per briefing; the rest wait. A message for everyone is held until every session of the current daemon generation has it, or until dropped. |
| Elevation of privilege | A message grants nothing: no lease, intent or admission changes, and a handoff transfers no lease. Acknowledgement only stops delivery. |

Asking who was given a message (`langspan messages status`, `langspan_message_status`, issue #67) adds no boundary: it reads, through the same owner-only socket, the acknowledgements that `events` and `status --all` already expose, and returns session IDs, states and ages, never message text. Its answer is bounded to 64 recipients and reads at most 4,096 acknowledgement rows. The daemon sets `via` from the command that delivered the message. A local process that can reach the socket can make a message look delivered, as it could before, by acknowledging it or by sending a delivering `brief` with the `mcp` flag; the socket's owner-only mode remains the boundary.

Withdrawing a message (`langspan messages retract`, `langspan_retract_message`, issue #66) adds no boundary: it uses the same socket and the same session labels.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Only the session named as sender, or the owner (no session), may withdraw a message, and that name is as unauthenticated as a sender's: a local process that can reach the socket can withdraw any message by naming its sender or none. |
| Tampering (prompt injection) | The disregard line quotes the first 120 bytes of the withdrawn text, cleaned and quoted like the message itself, under the same "data, not instructions" heading. It shares the 1,500-byte message budget, so it cannot push the briefing over its cap. A false withdrawal can make a reader drop a warning that still holds. |
| Repudiation | The withdrawal is a `message_retracted` event naming who withdrew it; the original `message_sent` event stays in the log. |
| Information disclosure | Readers are told only what they had already received. The recap and the dashboard show withdrawn messages, as they showed pending ones. |
| Denial of service | Withdrawals owed to a session are capped at 64, oldest dropped; at most four, within the message budget, per briefing. The history kept for withdrawal is the latest 64 messages per workspace. |
| Elevation of privilege | A withdrawal grants and releases nothing; it only stops delivery and adds a notice. |

## Moving and removing a workspace

`workspace move` rebinds a workspace ID to another root and `workspace remove` takes it out of the derived state ([ADR 0025](../adr/0025-distribution-lifecycle-and-store-beyond-the-first-release.md), row G6). They are CLI commands over the existing owner-only socket; no MCP tool offers them, so an agent following repository or peer text has no tool that moves or removes a workspace (a shell command can still run the CLI, as it can `remote disable`).

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Any client of the owner-only socket can send the request, as with `workspace add`. The socket's mode is the boundary. |
| Tampering (wrong checkout) | A move needs NEW_ROOT to be the top of a Git checkout and, without `--switch`, the old root to be gone; with `--switch`, NEW_ROOT's object database must hold the old root's `HEAD` commit. The check is a shared commit, not an identity: a fork or any clone that fetched that commit passes, and a repository can be given the commit on purpose. The new root then meets the same overlap and alias refusals as `workspace add`. A refused move changes nothing. |
| Tampering (symbolic link at a registered root) | A registered root and the root Git reports are the same checkout when they resolve to the same path and name the same device and inode, so a link left where a checkout used to be keeps it coordinated, and a move to the link's target needs no `--switch`. A link resolves everything under it, so on its own that comparison would accept a link to any checkout: the scan also requires the checkout now reached to hold the `HEAD` commit the previous scan saw (one bounded `git cat-file -e`, with the observer's safe options, once per change of path). A link to another repository stops the scan as an identity change, and admission through the link is refused while that failure holds. Until the first scan after the link changes, admission follows the link. A link to a clone of the same repository passes this check, as `--switch` would; a same-user process able to replace the registered root can already change the checkout's files. When that clone is itself registered under another ID, both IDs resolve to one root, so admission in either is refused until one of them moves or is removed. An accepted scan keeps the previous one as its baseline under the new paths, so a change made while the link replaced the root is still observed with its earlier version. Registration and `workspace move` compare every other workspace's root both as given and resolved, so the checkout a registered link leads to, or a directory inside it, cannot be registered under a second ID, where both IDs would admit the same file. A distinct linked worktree there still registers: the core accepts the adapter's verified overlap without the two roots overlapping as text, and refuses it while the linked workspace holds any current admission, since it cannot tell which paths that admission covers. |
| Tampering (repository-controlled Git) | NEW_ROOT's top is found with the same `git rev-parse` identity read `workspace add` uses. For `--switch`, `git rev-parse` of the old root's `HEAD` and `git cat-file -e` in NEW_ROOT run with `core.fsmonitor=false` and `--no-replace-objects`, as the observer's reads do. All of it is bounded by one second; no filter, hook or working-tree read runs. The project hook files of both roots are read only to print which `hooks install` commands to rerun; nothing in host configuration is written. |
| Repudiation | `workspace_moved` (with the old and new root) and `workspace_removed` are events; every earlier event stays in the log. |
| Information disclosure | `workspace list` shows roots, counts and two settings, as `status` already does. Both events are local only and never published. |
| Denial of service | A move or removal is refused while a session holds a current admission there or a landing holds or waits for its push turn, so it cannot end work in progress. A removal ends the workspace's sessions, notes, messages and automatic check in the derived state; a same-user process can do this to any idle workspace, as it could already forget its notes. |
| Elevation of privilege | A move or removal grants nothing. Remote sharing must be off first, and both wait until the published ref is gone, so neither leaves a published ref the daemon no longer withdraws, or withdraws through another checkout's remote. |

## Source index

[ADR 0008](../adr/0008-derived-source-index.md): repository source is parsed
inside the daemon, and names and signatures from it reach a model's context.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Tampering (memory safety) | Tree-sitter and the grammars are C. A crafted source file that triggers a parser bug could corrupt or crash the daemon, which holds coordination state. Inputs are bounded to 1 MiB, the tree is walked with an explicit stack, and extraction is bounded to 2,000 symbols and 20,000 references per file. LangSpan's own code remains `unsafe`-free. Running the indexer in a separate, unprivileged process would contain this and is not done. Each added grammar widens this surface by its own generated C parser: Java (`tree-sitter-java`, roadmap D6) now parses every `.java` file the scan reads, in the daemon and in the hook, under the same bounds. No grammar is downloaded or loaded at run time; each is compiled in and pinned in `Cargo.lock`. |
| Tampering (prompt injection) | Symbol names and signatures are repository text. In a briefing they pass through the same cleaning, quoting and caps as peer text; a signature is at most 240 bytes. In query results they are returned as data fields, and the tool descriptions say they are not instructions. |
| Information disclosure | The index holds names, signatures, line numbers and content digests, not file bodies. It is an owner-only file beside the event store. Nothing in it is published to teammates. |
| Denial of service | 200 files and 2 s per indexing pass, one pass at a time, on its own thread. Queries are bounded to 200 rows. A repository over the observer's 4,096-file bound is indexed for the files its partial scan versioned; the rest keep what was last indexed. A file between 1 and 4 MiB is marked too large for its digest and not retried, so such files cannot fill every pass. The index keeps at most 4,096 paths the last scan did not read, rewritten only when they change. |
| Tampering and disclosure (blind spots) | `graph blind-spots` and `langspan_blind_spots` return repository paths, extensions and counts from the index, no file content, as data fields of a tool result; the tool description says paths are data, not instructions, and the CLI prints them quoted with escapes. Five lists of at most 200 paths, cut to fit one 64 KiB frame. It reads the same owner-only index file as the other queries, adding no new boundary. |
| Spoofing | `observe_interface` is accepted from any client of the owner-only socket, so a same-user process can fabricate an interface notice. It reaches briefings only, never admission. |
| Information disclosure (code briefing) | At each prompt the hook takes up to 12 candidate names from the first 16 KiB of the owner's prompt and sends them over the owner-only socket in one read-only `graph_context` query. The daemon answers from the index and does not record, log or keep them; they are never an event or a receipt, and nothing leaves the machine. Any same-user client of the socket could observe the query in transit as it could any other. `--code-briefing off` stops the lookup. |
| Information disclosure (prompt targets) | With the prompt summary and code briefing both on and a completely scanned index, the hook then records the files the query found definitions in as the session's prompt targets (`set_prompt_targets`, event `prompt_targets_set`): up to 36 relative paths, the session and a time. The names asked about and the prompt's words are not in the command, which has no field for them and is refused with an unknown field; a test asserts a prompt's second line never reaches the event log. The paths reveal which files a prompt was about to every same-user client of the socket (`status`, `events`, other sessions' briefings in the same repository), as task lines already do. They are local only: the remote publication is built without them. |
| Information disclosure (presence) | Where a session looks is recorded locally and shown to related sessions: a read's path from the existing read observation, and from `observe_presence` (event `presence_observed`) a search's path scope or up to 4 existing paths a shell command's reading programs name. The command has fields for paths and a kind only, and an unknown field is refused, so a search pattern, glob, type or command text cannot be recorded; the hook never reads a `Grep` input other than `path`, and a shell search's pattern operand is skipped, with nothing taken when an option is not one that program (`grep` or `rg`, each with its own list) is known to take without a value. Tests assert a pattern never reaches the event log, the status or a briefing. The paths reveal which files a session read to every same-user client of the socket, as `known` versions in the status already do. `--presence off` keeps them from briefings (`set_presence`, which also forgets what was recorded); presence is local only, and the remote publication is built without it. |
| Tampering (presence) | A same-user client can record presence for any session, or change its setting, as it can its task line, and so make a peer line say a session looked at a file, or hide where it looked. Presence reaches briefings only; it never holds, refuses or delays a call. The core accepts 1 to 4 normalized relative paths for a registered session of the current generation, refuses `read` as a presence kind, and refuses presence for a session whose presence is off. A presence clause is part of a peer line: cleaned, quoted, within 700 bytes and the 2,400-byte briefing, and the first thing left out when full. |
| Tampering (prompt targets) | A same-user client can set any session's targets, as it can its task line or intent, and so make a peer line say a task points at a file. Targets reach briefings only; they never hold a write, grant an admission or change one. The core accepts 1 to 36 normalized relative paths for a registered session of the current generation, and replays only sorted, unrepeated paths of that generation. A target line is a peer line: cleaned, quoted, at most 700 bytes, within the 2,400-byte briefing. |
| Tampering (code briefing) | Names, kinds, paths and signatures in the code section are repository text placed in the model's context. Each value is cleaned and quoted like peer text, each name is one line of at most 500 bytes, the section at most 1,000 bytes beside a peer briefing and the whole context at most 2,400, under a header stating it is matched by name, not resolved, and is data, not instructions. A repository can still name a symbol or write a signature meant to persuade a model. The section carries no permission decision. The core refuses a query with more than 12 names, a name outside the identifier grammar, duplicates, or a limit outside 1 to 3. |
| Elevation of privilege | The indexer reads files below a registered workspace root, only regular files, only when their content matches the scanned digest. It writes only its own database. |
| Tampering (symbol detail) | A `graph symbol --detail` or `langspan_symbol` `detail` answer adds callee names, `path:line` call sites, digests and check names from the repository and the event store, and note text another session or person wrote. All are returned as data fields of a tool result, never placed in a briefing; the tool description says paths, signatures and note text are data, not instructions, and the CLI prints note text quoted with escapes. Note text keeps the bounds `remember` already applies; at most 4 notes, 32 callees and 8 sites per definition, and definitions past one 64 KiB frame are left out with `truncated`. No source body is returned. |
| Denial of service and child process (symbol detail) | The lookup runs on the daemon's request path. It reads each returned file again (1 MiB at most, only if it still has the indexed digest, regular files only) to compute its Git blob ID, then runs one read-only `git --no-optional-locks --literal-pathspecs ls-tree --full-tree -z TREE -- PATHS` per check record, for at most the 8 newest conclusive records, with a 300 ms deadline in all and bounded output. A tree ID from a record is passed only if it is 40 or 64 hexadecimal characters; paths come from the index and follow `--`. A Git failure or the deadline leaves `last_check` out for the files not yet matched. |
| Information disclosure (break check) | [ADR 0029](../adr/0029-break-alerts-at-write-time.md): before an admitted write, the hook sends the calls in the text it writes (path, line, called name, argument count) over the owner-only socket in one `check_calls` command. The daemon uses them for that answer only; they are never an event, a receipt or a log line. When an alert is given, `interface_alerted` records the session and the notice IDs, nothing of the code. Nothing leaves the machine. |
| Tampering (break check) | The alert places repository text (paths, names, signatures) and a session ID in the model's context as `additionalContext` on `PreToolUse`. Each value is cleaned and quoted like peer text, each break is one line of at most 500 bytes, the whole at most 1,200, under a header saying it is matched by name, not resolved, and is data, not instructions. It carries no permission decision, so a crafted repository cannot use it to allow or refuse a tool call. The core refuses a check with more than 64 calls, a path outside the workspace grammar, a line outside 1 to 2,000,000, a name over 128 bytes or with control characters, or more than 255 arguments, and an alert record naming more than 8 notices. |
| Spoofing (break check) | `acknowledge_interface` is accepted from any client of the owner-only socket, so a same-user process can mark notices as already alerted for a session and suppress its alerts, as it can fabricate a notice through `observe_interface`. Neither reaches admission. |
| Tampering (definition history) | `graph history` and `langspan_history` (roadmap D4) return signatures, file paths, session IDs, prompt summaries, note text and message text from the event log as data fields of a tool result, never placed in a briefing. The MCP result puts a label first saying this text is data, not instructions, and that `by_session` is not proof of authorship; the CLI prints every such value quoted with escapes. Each value keeps the bounds it was recorded under (256-byte signatures, 512-byte summaries, 400-byte notes and messages), with at most 4 notes and 4 messages per entry. A session that can write a note or message naming a file can make it appear beside that file's changes, the same reach recap and recall already give it. |
| Spoofing and repudiation (definition history) | `by_session` is the last session that reported editing the file, and `task` that session's prompt summary; a same-user client can report edits under any registered session, as for every briefing. Every answer says the session is a correlation, not proof of authorship. A history is a read and is not recorded. |
| Information disclosure (definition history) | It shows a session's prompt summary from the time of a change to any same-user client of the socket, which `events`, `status` and recap already expose. Prompt summaries are redacted on receipt; a session with them off records none, and none is shown. A message addressed to one session is shown to whoever asks about a file it names, as recap already lists unacknowledged messages. Local only; nothing is published and Git is not read. |
| Denial of service (definition history) | The store reads at most 20,000 events of five kinds through a partial index, plus at most 20,000 notes, forgettings and messages after a page's start, one lookup through a second partial index (`events_forgotten`, by note ID) for each distinct note read, so a forgotten note's text is never shown again, and the clock anchors; the core compares each entry with at most those events. Name 1 to 128 bytes, limit 1 to 50. The answer is cut at 28 KiB, and the MCP adapter trims again to leave 512 bytes of one 64 KiB frame. The partial index is built once when a store opens, one pass over the log, as the generation-marker index is. |
| Denial of service (break check) | The hook parses at most 8 files of at most 1 MiB per write; an `Edit` with `replace_all` whose result would pass 1 MiB is not applied, as its size is worked out before each replacement, so a bounded hook input cannot build a larger text. The check starts only while a quarter of its budget remains; the daemon call shares the hook's deadline. The daemon looks up only names present in both the write and an eligible notice, at most 64 calls against at most 32 notices. A parser failure or a timeout is a diagnostic note; the write proceeds. Parsing the written text runs the C grammars in the hook process, under the same memory-safety caveat as the indexer. |
| Tampering and information disclosure (kept break calls) | On Gemini CLI the hook's `BeforeTool` run keeps a write's call sites (path, line, called name, argument count; never the code) for its `AfterTool` run in `STATE/break-calls/`, one file per tool call named by a digest of the operation ID. The directory is created `0700` and checked for owner and mode; the file is created `0600` without following a symlink, written to a staged name and renamed into place, and read only when it is a regular owner-only file, at most 512 KiB and 64 calls. It is removed when read, whatever the write's result, and one left by an `AfterTool` that never came is removed after a day. A same-user process that writes such a file can make an alert name calls the write did not contain, as it can send `check_calls` itself; the daemon validates the calls as for any check, and the alert carries no permission decision. |
| Tampering (heredoc break check) | The hook reads the body of one exact quoted-heredoc form (`cat > PATH <<'EOF'`, a closing line that is exactly the delimiter, nothing after it) as a file's new content, for the break check only. The body is agent input already in the hook event (8 MiB at most) and is parsed with the same 1 MiB bound as any written file. It is never run, never read as shell, and admits or refuses nothing; any other shape is not read. |

## Dashboard

`langspan dashboard` ([ADR 0013](../adr/0013-dashboard.md)) is LangSpan's first
TCP listener. It serves one page and one read-only JSON view of every local
session and every teammate's published session, with task lines, file paths,
messages, notes and workspace roots.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Each run makes a 256-bit token from `/dev/urandom` and refuses any request without it (401) or with another (403), compared in constant time. The token travels in the URL's query or an `X-LangSpan-Token` header. Anyone who learns it can read the view until the server stops: it is printed to the owner's terminal, and with `--open` written to an owner-only file in the state directory, removed after 30 seconds, rather than passed in a process's arguments, which other users can list. A same-user process can read either, and the URL stays in the browser's history; a new run makes a new token. |
| Spoofing (DNS rebinding, other origins) | The `Host` header must name the address the connection arrived at, or `localhost` with its port on loopback; a hostile name that resolves to 127.0.0.1 is refused (421). No CORS headers are sent, so another origin's script cannot read a response even with a guessed URL. |
| Tampering | There is nothing to tamper with: only `GET /` and `GET /api/state` are served, any other method gets 405, any request with a body 413, and the server sends only `Command::Dashboard`, which records nothing. |
| Tampering (script injection) | Task lines, paths, branch names, messages and notes are other agents' and the repository's text. The page is static; values reach it only as JSON, with `<`, `>` and `&` escaped even inside strings, and are placed with `textContent`, never parsed as markup. The content security policy is `default-src 'none'` with the page's one script and one style allowed by SHA-256 hash, `connect-src 'self'`, no framing, no base URI and no form targets. A real-process test sends `<script>` and quotes through a real prompt and checks the JSON and the page. |
| Repudiation | Reads are not recorded: the dashboard leaves no trace in the event log. The server logs its start and address to stderr, not who read it. |
| Information disclosure | Loopback only unless `--allow-network` is given, which warns; a non-loopback address is refused otherwise, and an IPv4-mapped loopback address counts as non-loopback. Plain HTTP: off the machine, the token and view are only as private as the channel, so the documented way from a phone is an SSH tunnel or a VPN to the Mac. Responses are `no-store`, `no-referrer`, `nosniff` and not frameable. A teammate's work is shown as it was published to this machine; nothing new leaves the machine. |
| Denial of service | At most 16 connections at once, the rest closed unanswered; one thread each. A request head of at most 8 KiB and 64 headers, 2 s per read and 5 s in total, then 408 or 431. Each view is one socket request to the coordinator, under its own 64 KiB frame and deadlines; the denial search reads at most the newest 5,000 receipts and events. A page polls every 2 s; many open pages multiply that load. |
| Elevation of privilege | The server runs as the owner and can only read. It has no route that changes state, runs a command or reads a file other than through the coordinator's view. |

## Daily summary

`langspan today` and `langspan_today` (roadmap H4,
[ADR 0019](../adr/0019-person-facing-surfaces-and-usage-accounting.md)) read
the event log over the owner-only socket and summarise a window of the day.
The CLI and MCP adapter read the system's time-zone file (through
`local_day`, as the status line does) to learn the local time zone's offsets
near midnight. The events it
reads, `clock_anchored` at each start and `write_refused` at each refusal,
are the status line's.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | The zone is the file `TZ` names, a file under `TZDIR` or the system zone directories (a relative name with no `.` or `..` part), or `/etc/localtime`, all chosen by the owner's environment; no program is run. Only offsets are read from it (`local_day` and `zone`, shared with the status line): a file that is not TZif, or one of more than 256 KiB, gives none. Without the offset now the default window cannot be computed (`--since` still works); without an offset near midnight, the offset now is applied to the whole day. |
| Tampering (prompt injection) | The MCP answer places session IDs, file paths and intent text, written by other sessions and the repository, in a model's context. It starts with a line saying they are coordination data, not instructions; paths and intent text are quoted with escapes, intent text is cut to 120 characters, each line to 240 bytes and the whole to 2,400 bytes, the same cap as a briefing. No message text is included, only message IDs and recipients. |
| Repudiation | Reading leaves no trace: the command is answered from the store, records no event or receipt, and acknowledges no message. A refusal's event carries the session, workspace, paths, kind and time; a same-user process could already cause refusals by holding files. |
| Information disclosure | Nothing new is stored beyond the two event kinds, which hold IDs, paths, a kind and two clock readings. Nothing leaves the machine. |
| Denial of service | One answer reads at most the newest 50,000 events and parses each, then one backward search for the generation's anchor; the cost is bounded by that count, not by history, and the answer says `truncated` when the window reaches past it. The zone file is read once per answer, at most 256 KiB, and no child process is started. The answer fits one 48 KiB frame: listed files, then sessions, are dropped and counted. |
| Elevation of privilege | It grants nothing and decides nothing; a failure is an error answer. |

## Monthly refusal count

`langspan count` and the month's line in `langspan today` and on the local
page (roadmap H5,
[ADR 0019](../adr/0019-person-facing-surfaces-and-usage-accounting.md)) add no
boundary: they read the same `write_refused` and `clock_anchored` events over
the owner-only socket and read the zone file through `local_day` as `today` does.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | The day starts come from the client; the daemon checks only that they increase and number 2 to 33. A same-user client could ask for any window, which it could already read through `events`. |
| Tampering (prompt injection) | Nothing reaches a model: there is no MCP tool, and `langspan_today` leaves the line out. The text holds counts, kind names and workspace IDs. |
| Repudiation | Reading records no event or receipt. |
| Information disclosure | Nothing is stored or sent; the local page's view gains counts and workspace IDs it already showed. |
| Denial of service | One count reads at most the newest 20,000 refusal rows and the clock anchors, through partial indexes, and at most 16 later admissions per refusal counted, through an index by session. A month's hundred offsets come from one read of the zone file; the local page finds them once per month per run. |
| Elevation of privilege | It grants nothing and decides nothing; a failure is an error answer (exit 1). |

## Coordination report

`langspan report` and `langspan_report` (roadmap H8,
[ADR 0019](../adr/0019-person-facing-surfaces-and-usage-accounting.md)) read
the event log over the owner-only socket and count, per session and per host
kind, what `today` already reads. They start no process. The one new stored
field is the refused tool call's input digest on `write_refused`.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | A session's host kind is read from the prefix of its ID, which the hook sets from the adapter (`claude-code.`, `codex.`); a session registered by hand under such a prefix is counted as that host, which a same-user process could already do. |
| Tampering (prompt injection) | The MCP answer places session IDs, written by hooks and other callers, in a model's context. It starts with a line saying they are coordination data, not instructions; each ID is cut to 48 characters, each line to 240 bytes and the whole to 2,400 bytes. No path, intent text, prompt or message is included. |
| Repudiation | Reading leaves no trace: the command is answered from the store, records no event or receipt, refuses or holds nothing, and acknowledges no message. |
| Information disclosure | The input digest is the SHA-256 the admission's receipt and lease already keep; it holds no file content. Nothing leaves the machine; the digest is not published to a remote. |
| Denial of service | One answer reads at most the newest 50,000 events, as `today` does, and says `truncated` with the time it covered. Per-session state is bounded by the sessions in those events; the answer lists at most 16 sessions and fits one 48 KiB frame, dropping session rows and counting them. |
| Elevation of privilege | It grants nothing and decides nothing; a failure is an error answer. |

## Region admission

[ADR 0012](../adr/0012-region-admission.md): the hook reads and parses the file
being edited, and the regions it computes decide whether a second edit of the
same file is admitted while another is in progress.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Elevation of privilege (a wrong region) | A region computed wrongly could admit an edit that overlaps another in progress. The rules fall back to the whole file, the behaviour before regions, whenever the edit cannot be placed in exactly one way: no match or more than one, `replace_all`, a MultiEdit step matching text an earlier step wrote, an addition-only hunk, an added, deleted or moved file, a touched non-blank line in no definition, a file that parsed only partly, a file over 1 MiB or not UTF-8, or less than half the hook budget left. The host's own edit matching still applies when it writes: Claude Code replaces only a unique `old_string`, and Codex refuses a hunk whose context no longer matches. Keys match by name, so a mistaken key can only serialize more. |
| Spoofing / tampering | Regions are supplied by clients of the owner-only socket, like base versions. A same-user process can claim a region for its own session's admission and so be admitted beside another session's edit of the same file. It cannot change another session's lease. Admission was already cooperative, not a fence. The daemon maps each region's path through the same file-key checks as the admission, and the core keeps regions only for a path whose base in the same request is the version they name. |
| Tampering (memory safety) | The Tree-sitter parser of ADR 0008 now also runs in the hook process, on the file being edited, bounded to 1 MiB and to the index's symbol bounds. A parser bug triggered by a crafted file would affect the short-lived hook process, whose failure gives no decision. |
| Tampering (prompt injection) | Symbol names from repository text reach the model in a denial reason. They are cleaned, quoted and inside the existing 600-byte reason cap, labelled as coordination data, not instructions. The core refuses names with control characters or over 128 bytes. |
| Information disclosure | Leases store symbol names, line numbers and SHA-256 digests of files and definitions, not content, in the owner-only event store. A definition digest confirms a guess of that definition's text. |
| Denial of service | At most 64 regions per admission and 8 names per key, checked by the core on decision and on replay. Locating work is bounded (hunk lines times file lines, and touched lines times definitions, at most 4,000,000 each); beyond that the file is held whole. |

## Line ownership

[ADR 0028](../adr/0028-line-ownership.md): the hook compares the file being
edited with the edit applied in memory, looks the changed lines up in the line
journal of ADR 0026, and sends the core which other sessions wrote them. No new
boundary: the same hook reads the same file it already hashes and parses, the
journal it reads is the owner-only one the commit guard keeps, and it talks to
the same owner-only socket.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Elevation of privilege (wrong lines) | Lines computed wrongly could admit an unread edit over another session's lines, or refuse one that touches none. An edit that does not apply exactly as stated (an `old_string` missing or not unique, a hunk with no single match, a file over 1 MiB or not UTF-8, less than half the hook budget left) is not located and counts as changing any line. Lines are matched by content, so identical lines elsewhere can cause a refusal too many; lines of fewer than three letters or digits name nobody. The worst case of a wrong answer is one refusal too many or one write admitted as before this rule. |
| Spoofing / tampering | Written lines are supplied by clients of the owner-only socket, like base versions and regions, and read from the owner-only journal. A same-user process can claim false written lines for its own admission (causing its own refusal) or leave them out (avoiding one), or write journal entries. It cannot admit anything for another session. The core applies the rule only to the asking session, only for registered, live writers. |
| Tampering (prompt injection) | The denial relays paths (cleaned and quoted), session IDs, line numbers and ages, labelled as coordination data inside the 600-byte reason cap. No line text from the repository is relayed. |
| Information disclosure | Nothing new is stored: the journal's truncated line keys are ADR 0026's. Requests carry line numbers and ages only. |
| Denial of service | At most 64 written-lines entries of 32 ranges per admission, checked by the core. The comparison is bounded to 1 MiB, 4,000,000 line comparisons and 64 MiB of compared bytes, past which the changed middle is one change; the journal read is bounded by the journal's own entry limits. A refusal is once per change, so a false one costs one retry. |
| Repudiation | Each refusal is an appended `source_observed` event with origin `refused`; the writes it protects are in the journal. |

## Observation spool

[ADR 0014](../adr/0014-observation-spool.md): a completed tool call's report
that does not reach the coordinator is kept under `STATE/spool/` and sent again
by the next hook run that can connect.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Entries are read only from the owner-only state directory, through no-follow, owner-checked opens. A same-user process can write an entry naming any session and file, and so have an observation recorded for it; it could already send the same request over the socket. |
| Tampering | An entry is a typed record with unknown fields rejected; one that does not parse, is over 64 KiB, or is not named by its own request ID is deleted unread. A forged entry can only make a report the hook could have made: it reaches the core through the same commands and validation. |
| Repudiation | A resent report is an ordinary event in the append-only log under the request ID of the original report, so a resend and the original are one record. |
| Information disclosure | An entry holds IDs, a workspace root, file paths, SHA-256 digests and the tool input's digest: what the event store already records. No file content, command text, prompt or other free text is kept, so there is nothing further to redact. Directory `0700`, files `0600`. |
| Denial of service | At most 200 entries and 512 KiB, 3 tries each, an hour old at most; at most 20 sends per hook run, within half of the run's remaining budget, stopping at the first failure; a plainly down daemon costs one connection attempt. At most 800 directory entries are looked at per run. |
| Elevation of privilege | A resend makes no admission and no decision: it only records observations, versions and completions, as the original run would have. Against a restarted daemon it does not register the session again. |

## Retention

[ADR 0025](../adr/0025-distribution-lifecycle-and-store-beyond-the-first-release.md)
(J7): `langspan compact` and `config.json`'s `retention_days` replace old
events with a snapshot of the state they built, and delete old receipts. No
new listener, file outside the state directory or network call: the request
is an ordinary command on the owner-only socket, and the setting is read from
the owner-only `config.json`. `LANGSPAN_TEST_COMPACT_PAUSE_MS`, read from the
daemon's own environment, only delays the commit (at most 10 s) for a test.

| Concern | Behavior and remaining boundary |
| --- | --- |
| Spoofing | Any client of the owner-only socket can ask for a compaction, as it can for any command. It can only remove records older than at least 7 days, and only after the state they built is snapshotted. |
| Tampering | The snapshot is a row in the same owner-only file as the events. Before anything is deleted, the snapshot read back with the later events applied must equal the state the daemon holds; a same-user process that changed the event table first makes the compaction stop with nothing deleted. A same-user process that edits the snapshot row afterwards changes the state the next start loads, as editing an event row already could. |
| Repudiation | Removed events and receipts are gone: the log no longer shows what happened before the window, and a retry after it is a new request. The dry run is the default, the window is at least 7 days, and `remote sent` says from which date its list of publications is complete. |
| Information disclosure | Retention reduces what is kept. The snapshot holds the derived state only (what `status --all` shows), which the events already held. SQLite reuses the freed pages without overwriting them at once, so removed text can remain in the file's free space until it is reused. |
| Denial of service | A compaction runs in the single writer and blocks other requests while it replays the events after the previous snapshot (0.6 s for 500,000 small events in one measurement). The daemon runs it only when configured, once a day. An unacknowledged message, open admission or push turn holds the snapshot before it, so old events can remain; the report names what holds it. |
| Elevation of privilege | Compaction makes no admission or decision and changes no state the daemon holds: the snapshot must equal it. |

## Worktrees, processes and passive observations

**Worktree separation.** Ordinary overlapping roots and same-root aliases remain
rejected. Only the daemon may prepare a verified nested linked-worktree
registration after checking distinct Git toplevels and their common directory.
The wire command cannot assert that verification. An active parent lease over
child files blocks a new child registration. Once registered, descendant paths
remain excluded from parent admissions even if the `.git` marker disappears.
The daemon compares the resolved roots, so the exclusion still holds when the
parent's registered root is a link to its checkout.
Separate worktrees share repository lineage, not writable file identity.

**Process observations.** Linux PID, boot identity and process start metadata
identify an incarnation; on macOS, PID and start time. A missing or reused PID
is exited; an uncertain probe is unknown. A different binding in the current
generation is rejected. Restart makes previously projected liveness unknown
until re-observed. The one-second poll visits at most 64 bindings; unchanged
non-exited observations are coalesced for 15 seconds. These intervals do not
promise instantaneous liveness, and neither a dead process nor a live process
releases or extends any lease.

**macOS process probe (roadmap J12).** A new child process: the hook (to find
its host ancestor) and the daemon (to probe bindings) run `/bin/ps` by its
absolute path. STRIDE: *Spoofing*: a `ps` earlier in `PATH` is never used, and
the environment is cleared except `LC_ALL=C`. *Tampering*: the output is parsed
into typed fields and a line that does not parse is dropped; the start token is
hashed from it, so a crafted name cannot forge another incarnation's token.
*Repudiation*: every recorded result is a `session_liveness_observed` event.
*Information disclosure*: only ID, parent, owner, state, start time and
accounting name are asked for, never arguments or environment; the ancestor
walk lists only this user's processes, and another user's process is refused
and recorded `unknown`. *Denial of service*: each run has a two-second deadline
after which it is killed, its output is bounded to 1 MiB and read while it
runs, and the daemon probes a page of up to 64 bindings in one run and does
not re-probe one a hook saw alive less than 15 seconds before. *Elevation of
privilege*: `ps` runs with LangSpan's own rights and no input; its only
variable arguments are the user's numeric ID and a list of numeric PIDs; a failed or slow listing records `unknown`, never `exited`,
so it cannot drop a live session from briefings.

**Hook-run sightings (roadmap J12).** A `session_seen` event is evidence that
a session's hook ran, from a same-user client of the owner-only socket. It is
written at most once per 30 seconds per session, needs a session registered in
this generation, never registers one, and grants, renews or releases nothing.
A local client can keep a session it names in peers' briefings by sending
requests for it; that is the same trust any such client already has over a
session's prompts and edits. An unrecognised session is refused.

**Git observations.** Scans have a two-second budget and rotate across workspaces
on two-second opportunities. They bound file counts, content bytes, branches and
output; a warning, missing required data, metadata movement or exceeded bound
leaves the scan incomplete. File reads use workspace-relative no-follow access,
and regular files and symlinks have distinct content identities. All snapshot
paths and digests are validated, including a clean initial baseline. A failed
scan retains the previous complete snapshot and emits a diagnostic, not a
fabricated deletion or a fresh coverage claim.

Git runs with `GIT_NO_LAZY_FETCH=1`: a missing promisor object fails locally
instead of invoking a remote fetch or credential flow. A real partial-clone
regression checks that an owned test remote helper is not launched by this path.

Per-file reads are samples, not one atomic source snapshot. The first dirty
observation has an unknown prior version. Write-then-revert sequences can be
missed between scans. Branch-head commit/tree observations are not complete
change records, and content digests are not evidence of authorship. Local
monotonic observation times require their daemon generation; a prior-generation
record must not be presented as a current observation.

## Coordination benchmark driver

`benchmarks/coordination/driver.py` (issues #172, #188 and #193) is an
operator tool, not part of the product. It adds four boundaries: it launches
agent CLIs, it imports and runs the code those agents wrote, it reads the
operator's Claude Code configuration, and in the LangSpan arm it wraps
LangSpan's hook commands to log what they returned. Its test suite starts
only the stand-in agent, never a model.

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Elevation of privilege**: a benchmark session runs with more authority than the operator granted | Each session runs `claude -p --permission-mode default` with an explicit `--allowedTools` list: reads and edits under its own clone, the fixture's tests and a few Git commands (and, in the LangSpan arm, a few `langspan` commands). No flag or tool approves a permission prompt; a call outside the list is refused by Claude Code and counted from `permission_denials`. A test checks the command line. | `Glob` and `Grep` are not limited to the clone. An allowed `git commit` can run the fixture repository's hooks; the driver creates that repository and installs none. |
| **Tampering**: agent-written code runs during integration | The oracle runs in a separate interpreter (`-I` for the oracle, `-E -s` for the fixture tests) inside the integration worktree, with a time limit, after every session has stopped. | The code runs as the operator, like any agent's tests: run the benchmark in an account or machine where that is acceptable. |
| **Tampering**: one arm's coordination leaks into another | Each session works in its own clone, with its own objects and refs; the arms without LangSpan have no remote at all, and a test checks that a peer's commit is not visible. The driver refuses a real run when the configuration directory has user-level LangSpan hooks, the LangSpan skill or a LangSpan MCP server entry, and writes a `refused` record. The LangSpan arm's hooks and skill go into each clone's ignored `.claude/`; each coordinator has its own temporary state directory, removed after the run, and shares only through a bare remote inside the run's `work/`. | `Glob` and `Grep` can still read a peer's clone; such a call outside the allowed list is refused and counted as `outside_clone`. |
| **Tampering**: the hook-log wrapper changes what LangSpan decides | The wrapper runs the installed command through `/bin/sh -c` with the host's input, and passes its stdout, stderr and exit code through unchanged. Input over 64 MiB, or a hook that runs past 900 seconds, exits 2, which the host treats as a refusal, so a failed wrapper never admits an edit. | The wrapper adds an interpreter start (tens of milliseconds) to every hook call in the LangSpan arm, and two processes between the hook and the host, within the eight ancestors LangSpan searches for the host process. |
| **Information disclosure**: credentials or private text in a record | The driver never reads credentials. From the configuration directory it reads `settings.json` (its `hooks` and `mcpServers`), `.claude.json` (only its `mcpServers` tables; the rest is account state it never keeps) and whether `skills/langspan` exists, and passes the directory to Claude Code, which signs in itself. A refusal names only the kind of entry and the file, such as `hook in settings.json`, never a command or path from inside it; a test checks this. Records, transcripts and hook logs stay in the operator's output directory (`0700`, hook logs `0600`), outside the repository or in its ignored `runs/`. | Transcripts and hook logs can contain anything a model or a briefing wrote, including paths on the operator's machine; review them before committing any under `docs/evidence/`. |
| **Denial of service**: a session that never ends | `runner.py` bounds every session by `--time-limit` (at most 3,600 seconds) and stops its whole process group. A following stand-in waits for its peers at most 75 seconds per phase. | A process that leaves its group is not found. |

## Release pipeline

[ADR 0031](../adr/0031-releases.md) adds a new boundary: binaries built by
GitHub Actions and downloaded by users. It runs only in
`Langspan/LangSpan.dev`; no release has been made yet, so none of the controls
below has been observed in a real run.

| Threat | Control | Residual risk |
| --- | --- | --- |
| **Spoofing**: an archive that did not come from this repository's workflow | A build provenance attestation over every file in `SHA256SUMS`, signed with the run's OIDC identity through Sigstore, and checked with `gh attestation verify -R Langspan/LangSpan.dev`. | A user who skips the check gets no protection. macOS archives are not Developer ID signed. |
| **Tampering**: a dependency or action changes what is built | `--locked` builds on the pinned toolchain with no cache; every action pinned by commit SHA; `cargo deny` on every pull request; release jobs build from the tag release-please created. | A compromised crate already in `Cargo.lock`, or a compromised runner image, is built and attested like any other; the attestation proves where a file was built, not that its inputs were sound. |
| **Tampering**: a release changed after publication | Assets are attached to a draft and only then published, so the release can be immutable (a repository setting). | Until immutable releases are switched on, a maintainer can replace assets; the attestations would then no longer match. |
| **Repudiation** | The provenance names the workflow, commit and run; the run log is kept by GitHub. | Log retention is GitHub's. |
| **Information disclosure**: build paths or tokens in the binary or logs | Paths remapped with `--remap-path-prefix`; checkouts do not persist credentials; no secret is used. | None known. |
| **Elevation of privilege**: a pull request or a dependency gains write access | `permissions: {}` at the top of each workflow; each job names its own. Release jobs never run on `pull_request`. Signing permissions are only in jobs that compile nothing. | A maintainer merging the release pull request publishes whatever `main` holds. |

## Failure cases guarded by tests

Whole-set admission cannot partially hold free files after a conflict. A missing
reply cannot make an identical retry obtain a new grant. Reusing an ID for a
different command or semantic tool input is rejected. Restart advances generation
and invalidates old authority. Expired, invalidated and mismatched observations
cannot revive or complete leases. Another current lease from the same session
must remain visible as a concurrent holder.

A transaction failure must leave event history and published state unchanged.
A scan failure must not publish a partial snapshot, and a file-kind change must
remain visible even with the same content digest. Clean snapshot paths must pass
the same path grammar as later disk events. Process observations may not release,
renew or grant admissions. Verified workspace metadata must not be supplied over
the wire, and nested registration may not duplicate an active physical claim.
Installer regressions cover stable ownership and refusal to replace symlinks.
A message reaches each recipient once and never its sender, a look from outside
acknowledges nothing, a handoff goes to exactly one other session, and a
message whose rendered line could not fit is not selected for that briefing.

These tests establish specific component properties. Full current socket CI and
real native-host M3 acceptance remain separate checks in
[implementation status](../IMPLEMENTATION_STATUS.md).

## Deliberately uncovered guarantees

A path can be replaced after checking. Hard links are rejected when observed,
but filesystem races, mount aliases, independent coordinators, external editors
and delayed native tool actions can bypass admission. Cooperative leases do not
provide filesystem fencing.

The semantic input digest does not validate the source version previously read
by an agent. Once an earlier holder finishes, a stale whole-file replacement can
be admitted. Nor do queryable intents and changes prove the receiving model saw
them before choosing an edit. Source-base validation and verified context
delivery need their own host contracts and acceptance evidence.

`lease_valid` is a point-in-time result. An old `admitted` payload alone is not
current authority. Retry the same request ID to recover the original outcome;
use a new ID for a new operation. A transport error remains unknown/unavailable.
`status` answers within one frame however long the history: it lists open
admissions and live or recent sessions, counts closed admissions per session,
and names anything else it left out to fit. `status --all` pages through every
record after an opaque cursor, each page within the same byte budget as event
reads; a cursor the coordinator did not give is refused, never guessed at,
including a position at the integer limit. Records read across pages are each
as they were when their page was answered. A record larger than a page is sent
in parts that the reader joins, up to 8 MiB, and resumes inside a record only
if its JSON is unchanged; otherwise it starts that record again. Decisions that
need a part the bounded view may leave out (landing retries, doctor, automatic
checks) read it complete instead of treating it as absent.
Coverage aggregation is unfinished; retention is built (below). macOS path,
transport, sleep/resume and signing behavior, and Windows transport, need their
own validation. Remote publication requires a separate concrete owner-approved
data flow and threat-model update.
