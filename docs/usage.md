# Using LangSpan

Build, connect native sessions, inspect coordination, and integrate through MCP.
For the product overview and support summary, see [the README](../README.md).
For a visual introduction, open the [interactive workflow](https://langspan.dev/#demo),
[launch film](https://langspan.dev/#film) or [recorded dashboard evidence](https://langspan.dev/#evidence).
Examples below use the binary built from the LangSpan repository root.
If you installed a release binary on your PATH, replace
`./target/release/langspan` with `langspan`.

## Install a release

[v0.1.0](https://github.com/Langspan/LangSpan.dev/releases/tag/v0.1.0) provides
macOS and Linux binaries. Follow the [platform downloads, verification and
installation instructions](../README.md#install) before starting the coordinator.
A source build is optional.

## Build from source

From this repository, use Rust's installed toolchain manager and a C compiler for
bundled SQLite and Tree-sitter grammars. The repository pins Rust 1.100.0 and
commits the dependency lockfile.

```sh
cargo build --workspace --release --locked
./target/release/langspan --help
```

For development checks:

```sh
cargo install --locked cargo-deny --version 0.22.1
cargo install --locked cargo-machete --version 0.10.1
sh scripts/check.sh
```

That script checks formatting, lints, runs the core and real SQLite/socket/CLI/MCP tests,
builds the release binary, and checks dependencies against `deny.toml`: advisories, licences, bans and sources. The integration
suite needs permission to bind a Unix socket. The
[validation record](IMPLEMENTATION_STATUS.md) distinguishes observed passes,
blocked runs, and host acceptance. Build and test passes are recorded on Linux
and macOS arm64. Process probing is Linux-only; Windows transport is not implemented. Exact commands live in [engineering practice](engineering.md).

## Start the coordinator

In one terminal, from the LangSpan repository root:

```sh
./target/release/langspan daemon
```

The default state directory is `$XDG_STATE_HOME/langspan` or
`$HOME/.local/state/langspan`. Choose another with `--state-dir DIR` or
`LANGSPAN_STATE_DIR`. Use that same directory for every hook, CLI, and MCP client
coordinating local edits. Each machine runs its own daemon; remote sharing is a
separate setup below. The daemon requires owner-only permissions and does not
change workspace files.

## Connect a native agent

### All repositories for this user

Keep the coordinator running as described above. `install` writes host
configuration; it does not start the daemon. Preview the user-level hooks and
MCP registration with:

```sh
./target/release/langspan install
```

This is a dry run. It shows the planned files, except that Claude Code's
`.claude.json` preview shows only the LangSpan entry, leaving account and project
state out of the output. Close Claude Code before adding `--apply`: it also
writes `.claude.json`, and concurrent changes can overwrite one another.

With the default host configuration locations, installation updates:

| Host | Hooks | MCP server |
| --- | --- | --- |
| Claude Code | `~/.claude/settings.json` | `mcpServers.langspan` in `~/.claude.json` |
| Codex | `~/.codex/hooks.json` | A marked `[mcp_servers.langspan]` table in `~/.codex/config.toml` |
| Cursor (built from documentation, not yet verified on a real Cursor) | `~/.cursor/hooks.json` | `mcpServers.langspan` in `~/.cursor/mcp.json` |
| Gemini CLI (built from documentation, not yet verified on a real Gemini CLI) | `~/.gemini/settings.json` | `mcpServers.langspan` in the same file |

By default, the installer selects hosts whose configuration directory exists.
Gemini CLI is installed only when named: `--hosts gemini-cli` (or
`--hosts claude-code,codex,cursor,gemini-cli`);
`CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `GEMINI_CLI_HOME` are honored. Gemini CLI
keeps hooks and MCP servers in one file, so LangSpan plans both together and
writes that file once; its dry run shows only LangSpan's entries, because the
rest of the file can hold MCP server credentials. A path containing `$` is
refused for Gemini CLI, which expands `$NAME` in its settings. The generated hooks and MCP entries carry the same
explicit state directory. Other settings and hooks are preserved. An existing
`langspan` MCP entry the installer does not recognize stops installation before
any file is written. Codex runs new or changed hooks only after you review and
trust them with `/hooks`.

LangSpan does not create `~/.claude.json`. If it is missing, installation writes
the Claude hooks but skips MCP registration. Start Claude Code once, close it,
then preview and apply the install again.

User-level hooks find the Git repository from the agent's working directory and
do nothing outside a repository. An existing LangSpan project install takes
precedence. `langspan doctor --root /path/to/checkout` reports both setups;
`install` and `hooks install` write `--state-dir` as given, and `doctor` and
`hosts` compare state directories by their canonical form, so one reached
through a symbolic link (`/tmp` and `/private/tmp` on macOS) is the same
coordinator, and only a genuinely different directory is reported.
`langspan uninstall --apply` removes the user-level entries it recognizes and
leaves the coordination state directory in place. A hooks or MCP file left
holding nothing else (`{}`, or empty text) is removed rather than left behind.
Running it again after an upgrade also removes such a file an earlier
version left behind. A file with any of your own settings in it keeps them
byte for byte.
`~/.claude.json` is never removed.

#### An agent tool installed later

Install chooses hosts once, when it runs. A host installed afterwards has no
LangSpan hooks, so its edits are neither checked nor shown to other sessions.
`langspan hosts [--home DIR] [--json]` lists each host LangSpan knows, with
`name`, `config_dir`, `present` (its configuration directory exists; `null`
with an `error` when it could not be looked at), `user_install` (`none`,
`current`, `outdated`, `other_state_dir` or `unknown`) and `next_step`, the
install command, where one is proposed. It only reads. Gemini CLI is listed but
never proposed: install it by naming it, as above.

The coordinator checks the same at start and then every hour
(`host_check_ms` in `config.json`). A host that is present, has no user-level
install and has no project install in any registered workspace is recorded once
as a `host_not_installed` event, with the host name only, and recorded again
only after it was covered (`host_cleared`). A host whose directory or files
cannot be read at a check keeps the notice it had; the reason is logged as
`host_check_failed`, and nothing is recorded until an install or a removal is
seen. `langspan status` then shows:

```text
hosts: cursor has a configuration directory but no LangSpan hooks; run `langspan install --hosts cursor --state-dir '/path/to/state'` to preview, then add --apply
```

The command always names this coordinator's state directory, so the hooks it
installs talk to the coordinator that reported the host, whatever the shell's
own default. The dashboard shows the same command under Health. `langspan doctor` marks the host as
needing attention once sessions of another host use this coordinator. Agents
are not told. LangSpan never installs or edits a host's configuration by itself;
after you run the install, the line is gone at the next check.

#### A status line in Claude Code

`langspan install --statusline on` also sets Claude Code's `statusLine` in
`~/.claude/settings.json`, so the person sees, under the prompt after each
message:

```text
langspan · 3 sessions here · 1 refusal today
```

"Sessions here" counts the conversations (not host subagents) of this
checkout's repository, including its linked worktrees, that are of the
running coordinator, not ended and active within the last 30 minutes, this
one included. "Refusals today" counts what LangSpan refused in that
repository since local midnight: a write held by another session, a stale or
unread edit, and a held commit. Each refusal counts once; a retry of the same
call adds none. Refusals recorded by a build without this feature, or before
the coordinator first started with it, are not counted. The line shows counts,
never paths, and nothing of it reaches the model.

The setting is written only where no `statusLine` is set: a status line of
your own is never replaced, and the dry run says so. `install` without
`--statusline` leaves the setting as it is; `--statusline off` and
`uninstall --apply` remove LangSpan's line and nothing else. Other hosts have
no status-line command; a shell prompt or terminal multiplexer can show the
same line with:

```sh
langspan statusline --root /path/to/checkout
langspan statusline --root /path/to/checkout --json
# {"workspace_id":"...","sessions_here":3,"refusals_today":1,"coordinator":"running"}
```

Without `--root` it takes the directory from the status-line input Claude Code
sends on stdin (`workspace.current_dir`, then `cwd`), or else the current
directory. It asks the coordinator once with a 200 ms deadline
(`--deadline-ms`, 100 to 5000). When the coordinator is not running or does
not answer in time it prints `langspan · coordinator not running` and exits 0;
it never starts the coordinator, never uses the network, and writes nothing.
"Local midnight" follows the system's time zone (honouring `TZ`): `date`
gives the offset at the instants around midnight, so on a day the clocks
change the count still starts at that day's 00:00. If `date` fails, midnight
UTC is used. A day of more than 1024 refusals shows 1024.

User-level installation has been tested with a temporary home and real daemon,
Git and hook executable. It has not been observed in a native Claude Code or
Codex session. Claude's user-scope MCP key remains inferred from its documentation;
`doctor` inspecting the entry does not prove the host loaded it. A real Claude
session listing `langspan` in `/mcp` remains an acceptance check.
[ADR 0010](adr/0010-user-level-install.md) records the host configuration details
and remaining verification.

### One project

First inspect the proposed project settings. Replace `/path/to/checkout` with
the project your agent is editing:

```sh
./target/release/langspan hooks install claude-code --workspace demo --root /path/to/checkout
./target/release/langspan hooks install codex --workspace demo --root /path/to/checkout
./target/release/langspan hooks install cursor --workspace demo --root /path/to/checkout
./target/release/langspan hooks install gemini-cli --workspace demo --root /path/to/checkout
```

These commands are dry runs. Once reviewed, add `--apply` to write the entries.
The Claude installer uses `.claude/settings.local.json`; Codex uses
`.codex/hooks.json`; Gemini CLI uses `.gemini/settings.json`, which Gemini loads
only in a folder it trusts (Gemini CLI: built from documentation, not yet
verified on a real Gemini CLI; [ADR 0033](adr/0033-gemini-cli-adapter.md)). All three include session-start and prompt briefing hooks as well
as the supported edit hooks; Claude Code, Gemini CLI and Codex also have
session-end handling. Claude Code and Codex get end-of-reply handling
(`Stop`), and Claude Code also gets `StopFailure`, `SubagentStop` and
`PostToolBatch` ([ADR 0027](adr/0027-turn-and-batch-events.md)). Existing unrelated settings and hooks are preserved. The
host's own folder and hook trust requirements still apply. For Codex, review the
project hooks through its native hook-trust flow.

#### Codex: start and resume at the resolved path

Codex keys its hook trust to the exact path of the project's hooks file. A
session whose hooks were trusted with Codex started at `/tmp/work` and that is
later reopened with `codex exec resume` at `/private/tmp/work` (the same
directory on macOS, by another path) counts them as untrusted there: Codex runs
none of LangSpan's hooks, prints no warning, and the session edits
uncoordinated. A symbolic link anywhere in the path does the same. To avoid it:

- Start Codex, trust LangSpan's hooks, and resume Codex in the checkout's
  resolved path, with no symbolic links. `langspan hooks install codex` prints
  it:

  ```text
  Codex: start and resume Codex in '/private/tmp/work', the resolved root with no symbolic links. ...
  ```

- If a session must be resumed under another path, trust the hooks again there
  with `/hooks` before it edits.

LangSpan cannot see a hook that never runs, but it notices the result. When
the coordinator's scan sees a file's content change in a checkout and no
session of the checkout reported it, while a Codex session is registered there, it records an
`uncoordinated_changed` event naming up to two of the checkout's Codex
sessions, most recently active first, as the likely cause (not a proven
author: a hand edit looks the same). Files held by an admitted edit, versions a
session read or wrote, and files a checkout change (a branch switch, reset or
stash) accounts for are left out. The next briefing to each session of that
checkout carries one line, once per session:

```text
LangSpan: edits not coordinated. Files in this checkout changed 12s ago and no session reported the edit. codex session 0198c0de may be running without LangSpan's hooks: ... Read these files again before changing them: "src/a.rs"
```

A Codex session named is not named again while that notice is held (the
briefing horizon), and a file a session reports at that version afterwards
drops out of the line. Without a registered Codex session nothing is recorded.
`langspan status --json` shows the held notices under `uncoordinated`.

Cursor: built from documentation, not yet verified on a real Cursor. Its
installer writes `.cursor/hooks.json` (`preToolUse`, `postToolUse` and
`postToolUseFailure` for `Write`, `Delete` and `Shell`, `postToolUse` also for
`Read`; `sessionStart`, `beforeSubmitPrompt` and `sessionEnd`) and registers
`langspan mcp` in `.cursor/mcp.json`. Cursor runs project hooks only in a
trusted workspace and asks before it first uses an MCP server. Both files are
often committed, and LangSpan's entries hold paths on this machine: keep them
out of a commit. A Cursor `Write` states a whole file, so the file is held
whole. Cursor also runs Claude Code's hooks; LangSpan's Claude Code hook does
nothing on a Cursor event, so a call is coordinated once.
[ADR 0032](adr/0032-cursor-adapter.md) lists the sources, what was not
verifiable and how to capture a real run. Consult the [availability table](../README.md#current-availability)
before treating an adapter as validated for your host version.

Remove the managed entries with the matching vendor:

```sh
./target/release/langspan hooks remove claude-code --root /path/to/checkout --apply
./target/release/langspan hooks remove codex --root /path/to/checkout --apply
./target/release/langspan hooks remove cursor --root /path/to/checkout --apply
./target/release/langspan hooks remove gemini-cli --root /path/to/checkout --apply
```

A hook returns a labelled denial for a fresh current conflict or a fresh
`stale_base` result. Admission, unsupported input, unavailable coordination, and
unknown outcomes return no permission decision. The agent's existing prompts
and rules remain in control.
A permission refusal or cancellation may produce no supported post callback.
Its admission then ends when the reply does: `Stop` (and Claude Code's
`StopFailure`) completes every admission the session's own tool calls still
hold, and `SubagentStop` does the same for that subagent and ends its session.
An admission requested by hand is left alone. A turn the owner interrupts
sends no `Stop`, so its admissions expire normally. A stop hook never prints
anything, because output there would keep the agent working.

After each batch of tool calls, Claude Code's `PostToolBatch` records the
batch in the event log and, as after a completed tool call, delivers news
from peers that arrived since the session was last told.

Reinstalling over an install made by an earlier version adds the events it
lacks; `doctor` names any that are missing. `hooks remove` takes an event's
list out of the file once LangSpan's entry was its only entry, so an empty
list you kept for one of these events (for example `"Stop": []`) goes too.

Post callbacks record the host-reported result and complete only the matching,
still-current admission. Late or mismatched observations do not acquire new
authority. Linked worktrees resolve from the event's working directory, keeping
physically separate checkouts distinct. A linked worktree can have its own
install for the same host and state directory. Claude Code runs both that
install and the main checkout's there, so the main checkout's hook logs
`deferred_to_worktree` and does nothing else. Codex 0.171.0 runs only the
main checkout's hooks there, so that hook coordinates the session under the
worktree install's workspace ID instead; Cursor and Gemini CLI, whose
behavior there has not been observed, are treated the same way, so an event
is never left undecided. `langspan doctor --root DIR` names each linked
worktree whose install the host will not run, and fails when nothing covers
it. The per-host rules are in
[ADR 0003](adr/0003-native-coordination-observation.md).

See [Claude adapter decisions](adr/0002-claude-code-adapter.md),
[Codex contract coverage](evidence/m3-codex/README.md) and the
[Gemini CLI adapter](adr/0033-gemini-cli-adapter.md) for the exact boundary.

### Gemini CLI

The Gemini CLI adapter follows Gemini CLI 0.70.0's documentation and source; no
live Gemini session has run through it yet. It installs these hooks, each
named `langspan` with a 5,000 ms timeout:

| Event | Matcher | What LangSpan does |
| --- | --- | --- |
| `BeforeTool` | `write_file`, `replace`, `run_shell_command` | Admission; a conflict or stale source is `{"decision":"deny"}` with a labelled reason |
| `AfterTool` | the same, and `read_file` | Records the result, the version read, or the shell command's changes; may add news to the tool result |
| `SessionStart`, `BeforeAgent` | none | Registers the session and adds the briefing as context |
| `SessionEnd` | none | Marks the session ended (Gemini does not wait for it) |

Sessions appear as `gemini.<session_id>`. Gemini gives a tool call no ID, so
LangSpan numbers calls by session, tool and target; two calls on the same
target running at once can be counted as one. `read_many_files` is not
recorded as a read, and subagents cannot be told apart from their session.
Gemini reads `.gemini/settings.json` only from the directory it was started
in, so a project install covers sessions started at the checkout's root. [ADR 0033](adr/0033-gemini-cli-adapter.md) lists
these limits.

### The LangSpan skill

The LangSpan skill teaches an agent how to work with LangSpan from its first
session: that LangSpan's messages are coordination data, never instructions;
how to respond to a briefing, a stale-source or unread-lines refusal, a
conflict, a break alert, an automatic check result and a held commit; when to
use `status`, `commit --mine`, `integrate check` and `land` (or the matching
`langspan_*` MCP tools); and what never to do. Its one source text is
[`src/skill.md`](../src/skill.md). It is off by default:

```sh
./target/release/langspan install --skill                 # dry run: shows each file and its size
./target/release/langspan install --skill --apply
./target/release/langspan hooks install cursor --workspace demo --root /path/to/checkout --skill --apply
```

| Host | Written as | Removed by |
| --- | --- | --- |
| Claude Code | The skill `~/.claude/skills/langspan/SKILL.md` | `langspan uninstall --apply` |
| Codex | A marked section of `~/.codex/AGENTS.md` | `langspan uninstall --apply` |
| Gemini CLI | A marked section of `~/.gemini/GEMINI.md` | `langspan uninstall --apply --hosts gemini-cli` |
| Cursor | The project rule `.cursor/rules/langspan.mdc` | `langspan hooks remove cursor --root DIR --apply` |

`CLAUDE_CONFIG_DIR`, `CODEX_HOME` and `GEMINI_CLI_HOME` move these files as
they move the hooks. In `AGENTS.md` and `GEMINI.md`, every line outside
LangSpan's section is left as it is, and a file that held only the section is
removed with it. An existing `SKILL.md` or `langspan.mdc` that LangSpan did not
write, or a section whose markers are broken, stops the install before any
file is written. Reinstalling replaces LangSpan's own copy, edits to it
included. LangSpan never writes the skill through a symlink: installing it
there is refused, while `uninstall` and `doctor` leave a symlinked
`AGENTS.md`, `GEMINI.md` or `SKILL.md` alone, so a guidance file kept in
dotfiles never stops an uninstall. `langspan doctor` reports, for each host,
whether the skill is not installed, installed and up to date, or different
from this version's (then run the install again); for Cursor it does so with
`--root DIR`. For Codex it also names an `AGENTS.override.md` that Codex
reads instead of `AGENTS.md`, and a section that ends past the 32 KiB Codex
reads of its combined AGENTS files by default (`project_doc_max_bytes`): the
owner's own text shares that budget. The skill is
advice: nothing is held or refused through it.
[ADR 0034](adr/0034-langspan-skill.md) records the formats and their limits.

## Source versions

A completed Claude Code `Read` records the file's SHA-256 as seen on disk by the
post hook. A reported successful edit by either host records its resulting
version. LangSpan retains the 128 most recent records per session across daemon
restarts. A partial read records the whole file's version.

Before a supported edit, the hook supplies current file versions. After checking
for current holders, the core compares them with that session's records:

| Recorded version | Admission result |
| --- | --- |
| Matches the current file | The source comparison passes. |
| Differs from the current file | `stale_base`; no files in the set are admitted. A fresh result makes the hook answer `deny`, naming the changed paths and, for each, what changed it: a shell command that left the version now on disk ("changed by a shell command of claude-code session `<id>`: cargo fmt --all", its command line redacted and capped), the last peer that reported editing it, or "changed outside any reported edit" when no hook reported the change. |
| No record for an existing file | Admission can proceed with the path in `base_unknown`; its version was not compared. |
| No record and the file is missing | Admission can proceed as a new file. |

A stale change is refused once. The refusal records the current version as
learned from a refusal; it does not establish that the agent read it. A new tool
attempt against that same version can proceed without a new read. A further
change can trigger another refusal. This lets Codex recover without a reported
read and lets an agent recreate a deleted file; it also lets an agent that
ignores the notice repeat an outdated whole-file write. A new attempt needs a
new tool-call identity, rather than replaying the original request.

Codex reads are not reported through a supported hook, so its first edit of an
existing file is `base_unknown`. That admission records the file as the hook
saw it (origin `snapshot`), so later edits are compared even when the first
patch failed or its report was lost. The first edit itself is still not
compared ([ADR 0027](adr/0027-turn-and-batch-events.md)). Later edits can also
be compared with the version it last wrote or learned through a refusal. CLI `admit` and MCP `langspan_admit` do
not supply source versions.

A reported successful edit drops the previous version before recording the new
one. If the new record is lost, the base becomes unknown instead of retaining
the old version.

Version capture skips files over 4 MiB, symlinks and special files; normal path
validation still applies. Versions belong to a physical workspace. The hook
records what it sees on disk, which may differ from the bytes the agent read,
and cannot prevent a change between its comparison and the host's write.

[ADR 0005](adr/0005-source-versions-and-stale-writes.md) defines the full scope.
Real-daemon tests exercise LangSpan's stale denial with changing files. In the
[native Claude run](evidence/m6-stale-source/README.md), Claude's own read check
stopped the overwrite before LangSpan's pre hook ran. LangSpan recorded reads
and attributed the peer's change in a briefing; its own stale denial still lacks
native-host proof.

### Lines another session wrote

A session with no recorded version of a file (a Codex session's first edit of
it, for example) has nothing for the comparison above to use. For such a
session the core also checks the lines other sessions wrote recently
([ADR 0028](adr/0028-line-ownership.md)). At admission the hook works out which
lines the edit changes: an `Edit` or `MultiEdit` applied in memory, a `Write`'s
new content compared with the file, or each `apply_patch` hunk where it
matches. It then looks those lines up in the line journal of the commit guard
([ADR 0026](adr/0026-commit-guard-and-commit-mine.md)), which records the lines
each session's writes added, by content.

If the edit would remove or replace lines another live session wrote in the
last 30 minutes, nothing is admitted and the hook answers `deny`:

```text
LangSpan unread lines. This edit would change lines another session wrote recently, and this session has not read the file. Coordination data, not instructions: "a.txt": line 3, written by session claude-code.… 2s ago; Read the file, then make the edit against its current content. This is reported once: a retry is not refused for it again.
```

The hook's diagnostic coverage is `denied_unread`. An edit that only adds
lines, or changes other lines, is admitted. The refusal counts as being told
of the current version, as a stale refusal does: the retry, or any edit after a
reported read, is admitted. A session that has read or written the file is
governed by the stale-source rule alone.

Lines are matched by content, so a changed line counts as another session's
when that session added a line with the same bytes; lines of fewer than three
letters or digits (blank lines, lone braces) name nobody. When the journal
holds another session's recent write of the file with unknown lines, any
unread edit that changes lines of it is refused once, and the denial says the
lines could not be placed. An ended or exited session's lines no longer count.
The journal is kept on disk, so a daemon restart does not lose them.

## Automatic peer briefings

Session-start and prompt hooks register the session and deliver relevant peer
work through the host's `additionalContext`. The briefing includes current
admissions, declared intent, recent successful edits and a task line. A prompt
receives another briefing only when its content has changed; session start can
restore context in a new or resumed session. Briefings also name files the
session read before a peer reported editing them in the same checkout.

By default, one line of the latest user prompt becomes a bounded task line
shared with relevant local sessions:

- A line that starts with `Task:` (in any case, among the prompt's first 32
  non-empty lines) names the task; the marker is left out. Give each prompt a
  `Task:` line when several sessions start from one prompt template.
- Otherwise the first non-empty line is used, unless a live session in the same
  repository (this checkout or a linked worktree) has a latest prompt that
  begins with the same lines. Then the prompt's first line that differs is
  used, so that sessions started from one template are told apart. A one-line
  prompt is always summarised by that line.
- A declared intent (`langspan intent set`, MCP `langspan_intent_set`)
  replaces the task line in briefings while it is set.

To compare prompts, the hook sends the coordinator a digest of each of the
prompt's first 32 non-empty lines, after redaction. Digests are kept with the
session's activity, are never shown or published, and say only whether two
prompts begin with the same lines. The line chosen keeps the same redaction and
200-byte limit as before. To leave prompt text out, install with
`--prompt-summary off`:

```sh
./target/release/langspan hooks install claude-code \
  --workspace demo --root /path/to/checkout --prompt-summary off
```

Inspect the dry run and add `--apply` to write it. The option also applies to the
Codex installer. File activity and admission information remain available when
prompt summaries are off.

Two tasks aimed at the same code are flagged before either session edits it.
At each recorded prompt the files where the code briefing found the prompt's
names defined (at most 12 names and 3 definitions each) become the session's
prompt targets: paths only, never the prompt's words, replaced at its next
prompt that finds a definition and ignored after 30 minutes or once the session
ends. When another session's targets share a file with your targets, admissions,
recent edits or declared intent, its peer line says
`task points at your files: "src/http.rs";`, and your own targets count as your
work for `shares files with your work`. The prompting session is told in that
prompt's briefing, the other at its next delivery point (after a tool batch on
Claude Code, otherwise its next prompt), once. A target never holds a write,
never counts as an admission and is never published through the remote setup.
Nothing is recorded with `--prompt-summary off` or `--code-briefing off`, or
while the source index has no complete scan. `langspan status --json` shows
each session's `prompt_targets` under its activity.

A session that has read and searched but not written yet is still shown: its
peer line ends with where it is looking, for example
`task: "Rework token refresh"; looking at "src/auth.rs", "src/session.rs";`.
This presence comes from Claude Code `Read` and `Grep`, Cursor `Read` and
`Grep`, Gemini CLI `read_file`, and on every host the files a shell command
reads: each operand of `cat`, `head`, `tail`, `less`, `more`, `wc`, `nl`,
`ls`, `stat`, `file`, `diff`, `tree` or `du`, and the operands after the
pattern of a `grep`, `egrep`, `fgrep` or `rg` whose options are all flags
that program takes without a value (any other option, such as `rg -E` or
`grep -e`, records nothing for that command). After `cd`, `pushd`, `popd`,
`source`, `.` or `eval`, only absolute paths are taken. Only paths that exist inside the checkout are kept, at most 4
per command. A search records only the path it was scoped to (its `path`
input, else its working directory, never the whole checkout); its pattern,
glob and type, and a shell command's text, are never recorded. Codex reports
no reads, so a Codex session is shown only through its shell commands.

At most 4 paths are shown per peer, newest first, within the 700-byte peer line
and the 2,400-byte briefing, and only paths looked at in the last 10 minutes
that the peer is not editing and has not edited. A peer known only by where it
looked is listed after every peer with work. When the briefing is full, the
clause is the first thing left out: every peer's line, local and remote, is
placed without it, and it is added only where room is left. A peer seen
reading a path it had not been shown reading is news at a session start or
prompt, never mid-turn after a tool call; paths ageing out, or read again,
are not news. Presence never
holds, refuses or delays a read, search or shell call, and it is not published
through the remote setup. The briefing JSON (`langspan brief --json`,
`langspan_brief`) gives each peer a `looking` list of
`{"path": "src/auth.rs", "kind": "read"|"search"|"shell", "age_s": 42}`, and
`langspan status --json` (`langspan_status`) each session's `looking` with the
times it was recorded.

To keep a session's reads, searches and shell paths from peers, install with
`--presence off` (for `install` and `hooks install`). Its reads are still
recorded as versions for its own stale checks; nothing looked at while it is
off is shown later. `--presence-minutes N` (1 to 30, default 10) sets how old a
peer's presence that session is shown. When the coordinator is down, nothing
is recorded and the host's call is unchanged.

Relevant peers share the same checkout or a Git repository identified by the
observer. Linked worktrees can share briefings while keeping separate physical
admission domains. Another worktree's file is reported as a logical overlap,
not a file held in the current checkout. Separate clones and machines can
exchange published activity through the opt-in remote setup below.

Hook briefings contain at most six peers and 2,400 bytes. A peer must be current
and relevant; stale-generation, exited and aged-out work is excluded. New work
that begins during a model turn reaches the peer at its next prompt. Briefings
label peer and repository text as data. Full selection and delivery details are
in [ADR 0004](adr/0004-automatic-peer-briefing.md).

Inspect a registered session's briefing from the terminal:

```sh
./target/release/langspan brief bob --max-peers 8 --request-id bob-brief-1
```

This terminal view does not mark the briefing as delivered to Bob or suppress
his next hook briefing. It still requires a request ID in the current CLI;
use a fresh ID for a new observation, or retry the identical ID and arguments
if the outcome is unknown.

[Native-host evidence](evidence/m4-briefing/README.md) demonstrates Claude Code
receiving a SessionStart briefing before its first model turn. Codex native
briefing delivery, prompt delivery on the host, the mixed-host/worktree pair,
and an observed change in an agent's approach remain acceptance tasks.

## Share context through a Git remote

The preview can exchange work summaries through an existing Git remote. Each
machine runs its own daemon with its clone registered as a workspace. Git uses
its existing authentication. The project owner's choice of transport and payload
remains open; [ADR 0006](adr/0006-remote-awareness-through-git-refs.md) records the
implemented option and its limits.

To start sharing, replace `PROJECT` with a registered workspace ID and choose the
name teammates will see. `origin` is the Git remote name; choose another with
`--remote` if needed:

```sh
./target/release/langspan remote enable PROJECT \
  --name "Your name" --remote origin --request-id remote-enable-1
./target/release/langspan remote outbox PROJECT --json
./target/release/langspan remote sent PROJECT --limit 5
```

`remote sent` lists what has already left the machine, newest first: the time,
the sequence and the exact payload that was pushed.

Enabling starts background publication and receipt; it has no dry-run or
`--apply` step. The outbox command works only after sharing is enabled. It shows
the participant name and ID, session prefixes, host kinds, branches, and current
and recent file paths. Source contents stay out of this payload. The first line
of a session's prompt is included only with `--share-task on`; changing that
setting requires another enable command with a new request ID. One publishing
workspace covers its related worktrees.

Remote work appears in briefings as advice and does not hold files or deny local
edits. Its age is measured from receipt on this machine; work disappears from
briefings after ten minutes without a new publication. Remote failures leave
local coordination running. This exchange has been tested with two daemons and
two clones on one machine; hosted remotes and two-machine use remain unverified.

### Hosts that accept pushes only to branches

By default each machine publishes to `refs/langspan/peers/ID`, outside the
repository's branch list. Some hosts refuse every push outside `refs/heads/`; a
Claude Code cloud environment's Git proxy answered HTTP 403. The coordinator's
log then shows Git's own refusal text and suggests the option below. To publish
as a branch instead:

```sh
./target/release/langspan remote enable PROJECT \
  --name "Your name" --publish-as-branch --request-id remote-enable-2
```

The ref becomes the branch `langspan/peers/ID`. Everyone in the repository sees
it in the branch list, and a teammate's plain `git fetch` copies it into their
remote-tracking branches. Each `remote enable` states every choice again, so
leaving out `--publish-as-branch` (or `--share-task on`) turns it off. Moving
between the two removes the ref in the namespace no longer used. Every machine
reads both, so teammates who publish either way see each other.

Stop sharing with:

```sh
./target/release/langspan remote disable PROJECT --request-id remote-disable-1
```

The daemon stops publishing and retries removal of its remote ref until the
remote confirms it is gone. If the host refuses to delete it, the daemon says so
in its log with the exact ref to delete by hand, and replaces the ref once with
a final publication marked as withdrawn, so teammates stop showing your work.
`langspan doctor` keeps reporting the ref as not removed until it is gone; the
coordinator checks again about every ten minutes. Remote setup has no MCP tool. An agent with shell
access can run these CLI commands under its host's permission controls.

## Try the coordination loop with the CLI

This standalone example uses a fresh state directory, so its demo workspace
cannot collide with an existing hook setup. In a new terminal at the LangSpan
repository root, create the private directory, note its printed path, and run a
separate coordinator:

```sh
LANGSPAN_DEMO_STATE="$(mktemp -d)"
printf '%s\n' "$LANGSPAN_DEMO_STATE"
./target/release/langspan daemon --state-dir "$LANGSPAN_DEMO_STATE"
```

In another terminal at the same repository root, set `LANGSPAN_STATE_DIR` to the
path printed above, then register two logical sessions:

```sh
export LANGSPAN_STATE_DIR=/path/printed/by/the/first/terminal
./target/release/langspan workspace add demo "$PWD" --request-id workspace-1
./target/release/langspan session start alice demo --request-id alice-session-1
./target/release/langspan session start bob demo --request-id bob-session-1
./target/release/langspan intent set alice src/lib.rs --summary "Review shared operations" --request-id alice-intent-1
./target/release/langspan status
./target/release/langspan brief bob --request-id bob-brief-demo-1
```

Request an overlapping file, then unrelated work:

```sh
./target/release/langspan admit alice src/lib.rs --ttl-ms 60000 --request-id alice-edit-1
./target/release/langspan admit bob src/lib.rs --ttl-ms 60000 --request-id bob-edit-1
# The second request reports a conflict and exits with code 2.

./target/release/langspan admit bob src/main.rs --ttl-ms 60000 --request-id bob-other-1
# The unrelated file can be admitted.

./target/release/langspan complete alice alice-edit-1 --request-id alice-complete-1
./target/release/langspan complete bob bob-other-1 --request-id bob-other-complete-1
./target/release/langspan admit bob src/lib.rs --ttl-ms 60000 --request-id bob-edit-2
./target/release/langspan complete bob bob-edit-2 --request-id bob-complete-2
./target/release/langspan events --after 0 --limit 20 --json
./target/release/langspan changes demo --after 0 --limit 20 --json
```

Run admission and completion within their requested lifetime. Stop the demo
coordinator with Ctrl+C when finished. These commands coordinate logical
participants; they do not edit the files or launch agents.
Use a fresh request ID for a new attempt after a conflict. If repeating the
example against an existing state directory, use fresh demo identities rather
than expecting historical receipts to become new work.

`status` shows what matters now, in one answer however long the coordinator has
run: workspaces, sessions that hold an open admission, were active in the last
hour, are seen alive or have just registered, and the admissions still open.
Completed, expired and invalidated admissions are counted per session in the
`summary` (`closed_admissions`), not listed. If the current state is still too
large for one 64 KiB answer, the summary names what it left out (`omitted`,
`omitted_sessions`); workspaces, links, turns and open admissions stay, and an
automatic check is left out only when its command alone is near a frame.
LangSpan's own commands that decide from one of these parts (`land` recognising
a retried landing, `doctor` judging a workspace's scan, `auto-check`) read it
complete when the view left it out. To read every record, history included,
page through it:

```sh
./target/release/langspan status --all --json
# Each page's outcome holds `records` and `next`; pass `next` back for the following page.
./target/release/langspan status --all --after 'lease:0:alice-edit-1' --json
```

Each record is `{"kind": ..., "key": ..., ...}` with the record's state-map key.
One record larger than a page (an entry at its limits, such as a session's 128
known file versions with long paths) comes as `{"kind": "part", "text": ...,
"offset": ..., "last": ...}` records instead: the `text` of its parts, joined in
order, is that record's JSON. A part at offset 0 starts the record again,
because it changed while it was being read. The cursor is opaque: pass back only
a `next` value. A record changed between pages is read as it is when its page
is answered. MCP's `langspan_status` takes the same `all` and `after` arguments.

`changes` reads persisted file and local-branch observations, with scan freshness
information. A disk change is unattributed; related operation IDs are a
correlation, not proof of authorship. Initial baselines and incomplete scans are
kept distinct from newly observed changes. Process registration alone is not
proof a process is alive; Linux process observation is available through
`session attach-pid SESSION PID --request-id ID`.

## Recap, notes and messages

`recap` gives a bounded account of one workspace's recorded state, including
sessions, edits, checks, remembered notes, unacknowledged messages and recently
withdrawn ones (`"retracted": true`). Using the
workspace and sessions from the CLI example:

```sh
./target/release/langspan recap demo --json
./target/release/langspan remember demo "The response field is named upload_id." src/lib.rs --session alice --request-id note-1
./target/release/langspan message demo "The response field changed to upload_id." src/lib.rs --from alice --to bob --request-id message-1
./target/release/langspan handoff alice "Review the response parser next." src/lib.rs --request-id handoff-1
```

Notes reach sessions working on their named files; omit paths for a workspace-wide
note. They survive sessions and daemon restarts. Remove one using its returned ID:
`langspan forget demo NOTE_ID --request-id forget-1`. Notes and messages accept
one line of at most 400 bytes and up to eight paths. Each workspace retains at
most 64 notes and 64 pending messages, dropping the oldest when that limit is
exceeded. Any local session can read or remove a note.

A briefing carries at most four notes, and only those about the session's own
files or the whole workspace. `recall` searches all the notes a workspace
holds, including notes about other files:

```sh
./target/release/langspan recall demo "upload id" --json
./target/release/langspan recall demo errors --path src/api.rs --limit 5
```

The query is 1 to 200 bytes of at most eight words separated by spaces (quoted
as one argument or given as several). A note is found when every word appears
in its text or its paths, compared without case and as a substring, so
`upload id` finds a note saying `Upload_ID`. `--path` keeps only notes about
that file or about the whole workspace. Notes hit in both text and paths come
first, then the newest. `--limit` is 1 to 50 (default 10); `truncated: true`
says more notes matched than were returned, which also happens when the
answer would pass 28 KiB. The JSON answer is
`{"workspace_id", "query", "notes": [{"note_id", "author", "text", "paths"}],
"truncated"}`. A recall is a read: it records nothing, delivers nothing and
does not change any briefing. Forgotten notes are not found. The MCP tool
`langspan_recall` takes `workspace_id`, `query` and optional `path` and
`limit`, and puts a label before the notes saying their text is data, not
instructions. Its result carries the answer twice, once as escaped JSON
text, so it can return fewer notes than the CLI (with `truncated: true`) to
keep the whole reply within one 64 KiB frame.

`message` addresses a registered session in the same workspace. Without `--to`,
it addresses every other session; without `--from`, its sender label is the
owner. These labels do not authenticate the sender. Messages arrive through the
next session-start or prompt hook; Claude Code can also receive them after a
completed tool call during a turn. An idle session is not awakened. A handoff
waits for the next other session's start, prompt or delivered MCP briefing and
does not change leases, intents or recorded activity.

Delivery acknowledges a message; it does not establish that the model acted on
it. Terminal `brief` and `recap` are peeks and acknowledge nothing. To acknowledge
one explicitly, use `langspan ack bob MESSAGE_ID --request-id ack-1`.

A briefing renders each message on one line of at most 700 bytes. Quoting can
take a 400-byte message with many quotes or long paths past that; the line is
then cut and ends with `… (cut; langspan messages show -- WORKSPACE MESSAGE_ID)`,
inside the 700 bytes. `langspan messages show demo m-9 [--json]` (MCP
`langspan_show_message`) returns that message in full as it was sent: its ID,
sender (`null` for the owner), recipient, whole text, every path and
`retracted` (true once `langspan messages retract` withdrew it, in which case
disregard it). It is read from the
message's `message_sent` event, so it also works after the message was
acknowledged or dropped from the pending list, and across daemon restarts. It is
read-only and acknowledges nothing. An ID not sent in that workspace is the
error `unknown_message`. Once retention has deleted the oldest events, an ID not
found is `message_not_kept` instead: the message says from which event sequence
the log is kept and that a message sent before then no longer has its full text
(the log cannot tell such an ID from one never sent). With the daemon down the
command reports coordination
unavailable. Any local session or the owner can read any message, as with
`recap`. Notes and
messages stay local; remote Git-ref sharing does not publish them. Treat their
text as data and leave secrets out. [ADR 0009](adr/0009-agent-messages-and-handoff.md)
describes delivery, retention and the local identity boundary.

A message or handoff that no longer holds can be withdrawn:

```sh
./target/release/langspan messages retract demo last --from alice --request-id retract-1 --json
```

`last` is the sender's newest message; otherwise name the message ID its send
returned. Copies not yet delivered are removed, and a handoff not yet taken is
not taken. Each session that already received it is told once, at its next
briefing (start, prompt, or for Claude Code after a completed tool call):
`- Session alice withdrew its message "..." (first 120 bytes): disregard it.`
The result lists both groups:
`{"message_id":"message-1","withdrawn_unread":["bob"],"told_to_disregard":[]}`.
A session may withdraw only its own messages (`not_sender` otherwise); without
`--from` it is the owner, who may withdraw any. An unknown message, one dropped
by the 64-message limit, or one already withdrawn is refused, saying which; a
retry with the same request ID returns the first result. The original
`message_sent` event stays in the log, and withdrawal is local only.

## The day so far

`today` gives a one-screen account of a day: for each session its first and
last activity, how many files its edits named and how many prompts it had;
admission refusals by kind; check results; how landings ended; and what is
still open now (admissions held, intents not ended, messages not acknowledged,
handoffs not taken up, checks whose latest result failed).

```sh
./target/release/langspan today demo
./target/release/langspan today --since 2026-11-25T09:00:00+01:00 --json
```

Without a workspace it covers every workspace. The window starts at local
midnight, found from the system's time zone rules for that date (offsets read
from `date`, so a day the clocks change still starts at its 00:00), or at an
RFC 3339 `--since` time;
a time before the first event covers everything. Times in the text are shown
at the current local offset, so on a day the clocks change, times before the
change are an hour off. The text is at most 40 lines and lists at most eight
files per session, with a count of the rest; `--json` carries `window`
(`from_ms`, `to_ms`, Unix milliseconds), `sessions`, `refusals`, `checks`,
`landings`, `open`, `truncated` and, when there are any, `undated` and
`omitted_sessions`. The MCP tool `langspan_today` (`workspace_id` and `since`,
both optional) returns the same text, labelled as data, within 2,400 bytes.

It is a peek, like `recap`: it writes nothing and acknowledges no message.
Refusals are counted from `write_refused` events, the status line's, by kind:
a refused write (conflict, stale base or unread lines) or a held commit the
hook reported (`commit_held`). Each daemon start records a `clock_anchored` event
that places that run's times on the wall clock; events recorded by a build
before this one have no such anchor and are counted only as `undated`. A
landing is placed at the time it took its turn to land. A
window with more than 50,000 events is summarised from the newest 50,000 and
says `truncated: true`; when the oldest of those carries no time of its own,
the first that does decides whether the window may reach past them. With the
daemon down it exits 1 with "coordination unavailable".

## Desktop notices

When a session is refused, an automatic check fails or a handoff waits, the
person is often looking at something else. The coordinator can show a desktop
notice for these (roadmap H3). It is off until switched on:

```sh
langspan notify on                 # notices on, sound as it was (off by default)
langspan notify on --sound on      # with a sound
langspan notify status             # on or off, and which notifier was found
langspan notify off
```

A notice looks like this:

```text
LangSpan
demo: codex.7c2e9a1b… refused on src/api.rs (held by claude-code.4f1a0c3d…)
```

Three things raise one: a refusal (any `write_refused` event: a write held by
another session, a stale or unread edit, a held commit), an automatic check
that failed ([ADR 0030](adr/0030-automatic-checks-after-write-batches.md)),
and a handoff no session has taken up for 10 minutes. The body names the
workspace's directory, the session (its host and the first eight characters of
its ID) and at most three paths with a count of the rest, and is at most 200
bytes. It never carries a prompt, a message's or handoff's text, a check's
output or file content.

The notice is shown by `osascript` on macOS and `notify-send` on other
systems, the first one found on the coordinator's `PATH`, as a child process
the coordinator kills after 2 seconds. With sound on, `notify-send` asks for
the `message-new-instant` sound and `osascript` for `Glass`. With sound off,
`notify-send` passes the `suppress-sound` hint, so a desktop set to play a
default sound for every notice stays quiet for these (a notification server
that ignores the hint may still play one), and `osascript` names no sound,
which macOS shows silently. It runs on a thread of its own, so no
hook or request waits for it, and nothing about it reaches the model, holds or
refuses anything, or uses the network. Where neither program is found,
nothing is shown, and `notify status` says so (it looks on its own `PATH`,
which is normally the coordinator's). A `notify-send` started by a
coordinator that has no desktop session in its environment (no
`DBUS_SESSION_BUS_ADDRESS`) fails; the coordinator logs that once and goes
on.

At most one notice of a kind is shown per 5 minutes and 10 per hour in total;
the rest are dropped. The coordinator's log (stderr) records each notice as
`notice_sent` or `notice_dropped` with its kind and reason (`kind_limit`,
`hourly_limit`, `queue_full`, `notifier_missing`, `notifier_failed`,
`notifier_timed_out`), and a notifier that is missing, fails or times out is
also logged once per coordinator run. No event is recorded.

`notify on|off` writes `notify` and, with `--sound`, `notify_sound` into
`config.json`, keeping every other setting; the coordinator reads them again
at each notice, so no restart is needed. The limits are set in the same file
(see [Configuration and limits](#configuration-and-limits)). A handoff that is
already waiting when the coordinator starts is timed from that start.

## Look up symbols and affected files

The source index finds definitions and name mentions in Rust, Python,
TypeScript/JavaScript (including TSX/JSX), and Go. It updates after complete Git
scans and can lag changes on disk. Replace `PROJECT` with a registered workspace,
`upload` with the name to look up, and `src/api.rs` with the file to inspect:

```sh
./target/release/langspan graph symbol PROJECT upload --limit 50 --json
./target/release/langspan graph symbol PROJECT upload --detail --json
./target/release/langspan graph impact PROJECT src/api.rs --depth 2 --json
./target/release/langspan graph references PROJECT upload --limit 50 --json
./target/release/langspan graph impact PROJECT src/api.rs --limit 50 --json
```

`symbol` returns definitions with their location, signature, export status and
source digest. With `--detail` (MCP: `langspan_symbol` with `detail: true`),
each definition also has:

- `body_digest`: the SHA-256 of the definition's own bytes, which an edit
  elsewhere in the file does not change;
- `calls`: up to 32 names called within the definition, each with a count and
  the line of the first call;
- `callers`: how many call sites outside the definition use its name, and up
  to 8 of them as `path:line`;
- `notes`: up to 4 remembered notes on its file (workspace-wide notes are not
  included);
- `last_check`: the newest passed or failed check record, of the 8 newest,
  whose tree holds the file's indexed content. It is left out when none does,
  when the file is stored through a Git clean filter, or when the comparison
  does not finish within 300 ms.

The answer has `resolution: "name"`: calls and callers are matched by name,
not resolved, so a call of another function with the same name counts. No
source body is returned. Definitions that would make the answer larger than
64 KiB are left out and `truncated` is true. `references` finds mentions of the name. `impact` lists the file's
exports and other files mentioning those names, one level deep.

With `--depth N` (1 to 3, default 1; MCP: `langspan_impact` with `depth`),
`impact` also returns `levels`: level 1 is the files mentioning an export of
the file, and each further level the files mentioning an export of a file on
the level before. Each file appears once, at its first level, with `via` (the
name it mentions) and `from` (the file on the previous level). At most 200
files in all and 50 exported names per level are followed; a level that a
bound, the 64 KiB answer size or the 500 ms time budget cut has `truncated:
true`. Levels are found by name, not resolved, and each level adds false
positives. An empty level is no claim that nothing is affected; check
`coverage` first. `--depth 1` gives exactly the answer without the flag; a
depth outside 1 to 3 is refused:

```text
$ langspan graph impact PROJECT src/store.rs --depth 2
Level 1 (mentions an export of "src/store.rs"): "src/api.rs" via "save", "src/cli.rs" via "load"
Level 2: "src/server.rs" via "router" from "src/api.rs", "tests/api.rs" via "router" from "src/api.rs"
Found by name, not resolved; each level adds false positives. Index: 120 of 120 supported files at their current content.
```

These queries
match names without resolving scopes, types, modules or imports. Common names
can produce unrelated matches; dynamic uses can be missed.

Results include `coverage` and `truncated`. Coverage is relative to supported
files in the last complete scan, so an empty answer does not establish that no
code is affected. Queries return at most 200 results. Files over 1 MiB are not
parsed, syntax errors can leave partial results, and other languages are outside
the index. The observer's 4,096-file limit can prevent a larger repository from
producing the complete scan needed for indexing.

When `coverage.blind_in_scope` is above zero, that many supported files could
not be read in full, so an empty or short answer is weak. Ask which ones:

```sh
./target/release/langspan graph blind-spots PROJECT --limit 50 --json
```

```text
partial (2): "src/legacy/parse.rs" (since 3h), "web/src/app.tsx" (since 10m)
waiting (1): "src/api.rs"
too large (1): "src/generated/schema.rs"
not indexed, other languages: .java 10, .rb 3
```

The lists are `partial` (a syntax error or a bound reached, with since when),
`never_clean` (never parsed cleanly, so no interface notice can come from it),
`waiting` (its current content is not indexed yet), `too_large` (over 1 MiB),
`unscanned` (left out by the scan's bound) and `unsupported_languages` (other
source files by extension). Each is in path order and holds at most `--limit`
paths (1 to 200), so a capped list is the same on every call; `totals` has the
full counts. MCP: `langspan_blind_spots`. Read those files directly when the
index is silent; nothing guesses what they contain. `langspan doctor` also
says when a workspace has blind files.

When an indexed export disappears or its signature changes, relevant sessions
receive an interface notice at their next prompt. The notice names files they
work on or have read that mention the changed name. Notices stay within one
physical workspace. Body-only changes produce no notice, and a rename appears
as a removal. A first indexing produces no change notice. When a file holds
several definitions of one name (a method of that name on two types, say), a
definition added or removed is told as such, with how many there are now:
"a second method "text" was added … (now 2 definitions with that name)".

A write is also checked as it is made. When an Edit, MultiEdit or Write (Claude
Code) or an `apply_patch` (Codex) is admitted, the hook parses the calls in the
text it writes. If a call's argument count does not fit a function signature
that another live session changed in the last 30 minutes, the write's reply
carries an alert in the model's context: the call's file and line, the
function, the count it now takes, the new and old signatures, and the session
that changed it. The write itself is not refused or changed, and the host's
permission prompt is unaffected. Claude Code shows the alert to the model
with the write's result. Each session is alerted once per change; a call with
the new count gets nothing. Only argument counts are compared, matched by
name, so a different function of the same name can be named; a type or order
change that keeps the count, removals and renames are left to the prompt
notice. Gemini CLI and Cursor writes are not checked: Gemini reads no context
before a tool runs, and Cursor's write input has not yet been seen from a real
Cursor. Nothing is checked until
the index has a complete scan of the workspace, and the hook's diagnostic
says so. [ADR 0029](adr/0029-break-alerts-at-write-time.md) has the rules.

Extraction and real-daemon tests exercise files changing on disk. Native-agent
acceptance and an improvement in a real agent's work have not been demonstrated.
[ADR 0008](adr/0008-derived-source-index.md) describes extraction, coverage and
notice delivery.

## Checks and landing

The CLI implements `check run`, `check status`, `integrate check`, and `land`.
A working-directory check records its command and result with a Git-tree ID,
including tracked files and untracked files that Git does not ignore. Different
before-and-after trees or an unfinished command produce an inconclusive result.
`check status` compares the recorded tree with the current one.

`integrate check` combines branches in the order given and runs the command in
a temporary checkout. `land` recombines them and requires a passing check of the
chosen name for the resulting tree. It pushes only with `--apply`, using a normal
non-forced push and the repository's pre-push hooks. With `--apply` it first
takes the push turn for the workspace and target. If another landing has it,
it waits in line for it, first come first served, for up to `--wait-s`
seconds (default 240, at most 3600; `--wait-s 0` returns at once, as before).
At most 8 landings wait per target. A landing that does not get the turn
(it did not wait, the line was full, its wait ran out, or it was cancelled)
exits 7 with the holder and the line, having fetched and pushed nothing. Only
once it has the turn does it fetch the target, so it combines onto whatever
the landings before it pushed and, with `--check-again`, checks that new tree.
`langspan land-queue WORKSPACE --onto BRANCH` shows who holds the turn and who
waits (also in `status` and on the dashboard);
`langspan land-cancel WORKSPACE --onto BRANCH --request-id ID` stops a waiting
landing. A waiting landing that is killed leaves the line once it has not asked
for 15 s; a daemon restart empties every line. If the push is refused
because the target moved, it combines again and retries, at most three
attempts, and lands a new tree only with a passing check for it:
`--check-again -- COMMAND...` runs the check inside the turn to get one. A
refusal by a hook or a protection rule is not retried. If the push went
through but recording the landing failed, exit code 8 names the pushed commit;
if the push did not finish, LangSpan asks the remote where the branch is: at
the pushed commit it is landed and recorded; still at the earlier tip, exit
code 2 says nothing was pushed; otherwise exit code 9 says whether it happened
is not known. Exit codes 8 and 9 print the `git ls-remote` command that shows
the remote branch. `--json` prints the
whole result, including every attempt.

When branches do not combine, `integrate check` and `land` name the conflicting
paths and, for the first 8 files, up to 4 conflicting hunks each: the target's
side and the branch's side, each with its first line number in that version,
its line count and its first 6 lines (at most 200 characters each, control
characters replaced). Git's conflict type is given per file, such as `contents`
or `modify/delete`. These lines are the repository's text, shown as data.

An inconclusive `integrate check` says why, in the same words on the CLI and in
the MCP result's `inconclusive` field: its `cause` (`changed_during_run`,
`checked_out_differently`, `did_not_finish`, or `replayed` for a retry of a
request whose record does not keep the reason), a `message`, up to 20 of the
`changed` files with whether each was added, modified or deleted and whether
it is untracked (`changed_total` counts them all), and what to do `next`. A
check command that writes a file the branch does not track, such as a lock
file a build tool creates, is the usual cause: commit the file on the branch or
add it to `.gitignore`, then run the check again under a new request ID. The
paths are the repository's text, redacted and without control characters.

Combining always follows renames, whatever `merge.renames` is set to: when the
target renamed a file after a branch started, the branch's edits to the old name
are combined into the new file, and the result's `renames` (and the printed
output) says which. Inexact renames are looked for among at most 1000 files on
each side; exact renames are always found. Combining is a merge, never a rebase:
the branches' commits are pushed as they are (see ADR 0007). Check commands run with your
environment; environment identity and ignored files are outside the record.
A check's recorded command line is redacted like its output: a `--token`
value or a key-shaped argument is stored and shown as `[redacted]`, while the
check's name and program stay readable. Prefer passing secrets through the
environment, which is never recorded.

Tests use real daemons, CLI commands and local file remotes. Hosted branch
protection and native-agent workflows remain unverified. See
[ADR 0007](adr/0007-check-evidence-integration-and-landing.md) for command syntax,
input limits, and the gap between a successful push and recording it locally.
`langspan_integrate_check` and `langspan_land` expose the same operations to agents (below).

### Automatic checks after a session's writes

Off by default. Turn it on for one workspace:

```sh
langspan check auto WORKSPACE --name tests --timeout-s 600 -- cargo test --workspace
langspan check auto WORKSPACE          # show the setting
langspan check auto WORKSPACE --off
```

After a session writes files and reaches the end of a tool batch (Claude
Code's `PostToolBatch`) or of its reply (`Stop` on Claude Code and Codex), the
daemon runs the command. The hook never waits for it. Cursor and Gemini CLI
have no such event hooked, so their sessions start no automatic check. The daemon waits until
the workspace's sessions have paused for `auto_check_debounce_ms` (default
2000) and at most 30 s, so a burst of batches runs one check, then takes the
exact working tree as `check run` does and runs the command there, without a
shell, in its own process group, within the time limit (default 1800 s, at
most 3600). If a session writes again while it runs and the tree changed, it
is stopped and the new tree is checked. Changing the command or `--off` stops
a running check.

The result is an ordinary check record of that tree (`check status` shows it,
`land` can use it), and the sessions it ran after are told once, at their next
briefing point (a completed tool call, the next batch, or the next prompt):
passed, failed with the exit code, stopped at its time limit, or inconclusive,
with the tree ID and, unless it passed, up to 20 of the last output lines,
redacted and labelled as the command's own text. Failures are not classified
as new or already failing, but when the output is that of `cargo test`,
`go test -v` or `pytest`, the failing tests it named are listed. The command runs as you, with
the daemon's environment, outside the agent host's sandbox and permission
prompts: code an agent writes runs without the host asking. A command that
looks like it holds a secret is refused; pass secrets through the environment.
See [ADR 0030](adr/0030-automatic-checks-after-write-batches.md).

#### Tests that flip on unchanged code

Two results of the same check (name and command, compared by its whole
argument list, not the shortened command line `check status` shows) on the
same tree, with no write recorded in the workspace between them, that
disagree about a test are one flip of that test. `check run` learns nothing
from a command that looks like it holds a secret. A failing automatic check that named a failing test and
took at most `flaky_rerun_max_s` (`config.json`, default 30, 0 turns it off)
is run once more on the same tree, stopped after at most that long, to get
that second result; a longer one is not run again, and a write recorded while
either run goes, even one undone, means neither is compared. The rerun is
not a record: the first
run's failure is the result, in `check status` and to `land`, whatever the
rerun found. Two `check run`s of one tree with no write between or during
them are compared too. A test the output names as skipped (`ignored` in
`cargo test`, `--- SKIP:` in `go test -v`, `SKIPPED`, `XFAIL` or `XPASS` in
`pytest -v`) neither passed nor failed in that run, even when the run passed,
and is not compared. A test that flipped at least twice in its last 20 such runs is known to
flip, until it has gone 50 runs without one. Its failures are then labelled in
the briefing, and still count as failures:

```text
1 failure named in its output: net::times_out (known to flip on unchanged code, 2 of 14 runs). Every one is of a test known to flip on unchanged code; the check still failed.
```

```sh
langspan check flaky WORKSPACE [--name NAME] [--json]
tests  "net::times_out"  runs 14  flips 2  last flip 3h ago
langspan check flaky WORKSPACE --forget net::times_out [--name NAME] --request-id ID
```

`--forget` clears what was learned about a test, including its last result,
so its next failure is not labelled. A test or check name that looks like a
secret is refused (`secret_in_test_name`) before anything is recorded: such a
name is never learned, so there is nothing to forget. The MCP tool `langspan_flaky_tests` reads the same list. Only test
names are kept, in the local event log; no output, and nothing is published.
Names are read from at most 4 MiB of output (at most 100 names and 10 KiB per
run); a runner LangSpan does not recognise teaches nothing.

## Commits in a shared checkout

When several sessions work in one checkout they share one Git index, and a
commit takes whole files. The `Bash` hook holds a command that would stage or
commit another live session's uncommitted changes: `git add -A`, `git add .`,
`git add -u`, `git add PATH…`, `git commit -a`, `git commit PATH…`, or a plain
`git commit` while the index already holds such changes. The reason names the
other session and its files and lines, and gives the options. The same command
run again is not held: each is held once, so the choice stays with the agent
and the owner. Changes no session made (your own editor), this session's own
changes, and changes of sessions that have ended never hold a commit.

To commit only what this session wrote:

```sh
langspan commit --mine -m "Fix the parser" [--session ID] [--root DIR] [--json]
```

Files whose every change is the session's are committed whole; in a file
that also holds another session's changes, only its own hunks. The commit is
built in a private index from `HEAD`, so nothing anyone else staged rides
along, and made with your identity and Git hooks. The branch moves only if no
other commit landed meanwhile; if one did, the session's changes are planned
again on top of it once, or the command refuses and nothing is lost. Everything else stays as it
was, staged or not, and is listed. When Git joins the session's lines and
another session's into one hunk (two entries added one below the other to a
list), the hunk is split between them and only the session's own lines are
committed. If a hunk mixes the session's lines with lines no session wrote,
or one session's line there replaced another's, or the two sessions' lines
there could be in more than one order (one session changed "A B C" to "B X"
and the other removed B: X could go above B or below it), nothing is
committed (exit 1) and the hunk is named, with the reason. Nothing is
committed either if the partial patch would not keep the session's lines in
their one place among the other session's, or does not apply exactly to the
version in the index. Lines are matched by content, not position, so sessions
editing one file in turn keep their own hunks however their edits shift each
other's lines. When another session changed the file in a write whose lines
are not known, the session's hunks are still committed unless one of their
lines appears in the file's changes since `HEAD` more often than the session
wrote it, or the session's earlier write of that line may already have been
committed through Git; that hunk is then named, with the reason. Run from an agent's shell, the session is the one running
the command; from a terminal, pass `--session` (a briefing says "Your session
ID is …"). It looks for the running command in its own state directory, so
with hooks installed under `--state-dir`, pass the same `--state-dir`; when
it finds no running session, the error names the directory it looked in and,
if a LangSpan hook installed in the checkout names another one, that one.
The MCP tool `langspan_commit_mine` does the same.

To see first what `commit --mine` would take:

```sh
langspan status --mine [--session ID] [--root DIR] [--json]
```

```text
shared.txt  mixed  lines 10-14 mine, line 22 no session's, lines 40-41 claude-code.…
src/new.rs  mine   lines 1-30
3 changed file(s) hold other sessions' lines.
```

It lists, by path, each changed file that holds this session's lines:
`mine` (`commit --mine` would commit all of it), `mixed` (only its hunks
marked mine) or `unknown` (it would refuse, and why), whether some of it is
`staged` in the shared index, and each hunk's first line, line count and
owner: `mine`, another session's (`others`, with `sessions`), `none` (no
session wrote it) or `unknown` (with `reason`). A file attributed as a whole
(binary, over 512 KiB, a mode change) is `whole`, with no hunks. `others`
counts the changed files that hold other sessions' lines, also where they
share an `unknown` hunk with this session's. At most 200 files
are listed (`truncated` when there are more). It runs the same attribution as
`commit --mine`, finds its session the same way, and changes nothing: no
private index, no commit, and Git reads run with `--no-optional-locks`. The
MCP tool `langspan_my_changes` does the same. Other worktrees and machines
are not included.

A formatter or other workspace-wide command (`cargo fmt --all`) does not
change who wrote a line it only reformats. Where the command kept a line's
tokens in order and changed only whitespace and line breaks (re-indented,
re-wrapped, joined or split it), each line it left keeps the author of the
uncommitted lines it came from: the session that ran the formatter, another
session, or both sessions' own lines in one file. This holds whoever ran the
command and whether or not another session's command ran at the same time,
so `cargo fmt --all` during another session's test run leaves each session's
`commit --mine` able to take its own hunks, and a new file one session wrote
stays that session's after another session's formatter rewrites it. Tokens
are compared by language: Rust, TypeScript and JavaScript as words and
punctuation, each string or character literal one token whitespace and all,
with a comma just before a closing `)`, `]` or `}` ignored (a formatter adds
and drops those when it wraps a list); Markdown as words and punctuation;
Python as words, punctuation and strings, with the indentation of each line
that starts a statement as content, so re-wrapping a call inside its
parentheses is formatting but re-indenting a block is not; YAML, Makefiles
and other indentation-sensitive files (`.haml`, `.pug`, `.sass`, `.styl`,
`.nim`, `.fs` and the others ADR 0011 lists) as words and punctuation with
every line's indentation as content; any other file as its text with all
whitespace removed. Trailing whitespace and blank lines are always
formatting.

A change that alters tokens (a rename, an added line) is credited as before:
to the command's session when nothing else could have made it and it
rewrote no other session's uncommitted line, and otherwise to no session, so
`commit --mine` leaves or refuses it. The same holds for a file another
session edited through its edit tool while the command ran
([ADR 0011](adr/0011-shell-writes.md)). A line two sessions both wrote (a
blank line, a closing brace) goes with the lines around it in a hunk, so it
does not stop either session's `commit --mine`
([ADR 0026](adr/0026-commit-guard-and-commit-mine.md)).

Before such a command runs, the Bash pre-tool hook on Claude Code and Codex
tells the session, once, which other live sessions have uncommitted changes in
files it may rewrite: "LangSpan: this command may reformat files that another
session in this checkout has not committed.", then each session and at most
eight of its files, then how many more. The command is not refused, and the
host's own permission prompt is unchanged. The hook recognises `cargo fmt` in
any form (it formats every file of the workspace, even when files follow
`--`), `rustfmt FILE...` (those files and the modules they declare) and
`prettier --write PATH...` (directly or through `npx`, `pnpm exec` or `yarn`).
A check such as `cargo fmt --check` or `prettier --check .` names nothing.
The same files are not named again whichever formatter runs; a new file is.
The hook's log line says why no notice was given (`formatter notice: …`). In a
shared checkout, format only the files you changed, by name: `rustfmt FILE...`
or `prettier --write FILE...`. The LangSpan skill says so too.

When a Git step fails, the error names the step, its exit status and what Git
said (its last lines, at most 1 KiB, redacted as other relayed text is). The
commit is built in the Git directory, so it needs to write there: inside an
agent's workspace sandbox that directory is often read-only, and the error then
says so. Run `commit --mine` outside the sandbox (from a terminal, or with the
host's permission to run it unsandboxed), or make the Git directory writable.
The commit guard's own Git reads report a failure the same way, in the hook's
log line, and give no decision.

Which lines are whose comes from a journal the hooks keep on this machine
(`STATE/journal/`, owner-only): for each reported edit and observed shell
change, the keys of the lines it added and removed, and a copy of file
versions up to 512 KiB. Nothing of it leaves the machine. See
[ADR 0026](adr/0026-commit-guard-and-commit-mine.md) for the rules and what
is not covered (identical lines, moved lines, changes LangSpan did not see).

## MCP

Configure an MCP client to launch the built executable with `mcp`:

```sh
./target/release/langspan mcp --state-dir /path/to/private/shared-state
```

Start the daemon separately with the same directory. MCP stdout is protocol
only. The official Rust SDK handles initialization and stdio transport.

| Tool | Operation |
| --- | --- |
| `langspan_workspace_register` | Register a canonical physical workspace |
| `langspan_session_register` | Register or reattach a logical participant |
| `langspan_intent_set` | Declare/update advisory work and file scope |
| `langspan_intent_end` | End advisory work independently of admission |
| `langspan_admit` | Request an entire file set or inspect conflicts |
| `langspan_complete` | Complete a currently valid held admission |
| `langspan_status` | Inspect open admissions, live or recent sessions, liveness observations and each session's last hook run (`summary.last_hook_age_ms`); `all` and `after` page through every record |
| `langspan_events` | Read an ordered page of persisted events |
| `langspan_changes` | Read file and local-branch observations with scan freshness |
| `langspan_brief` | Deliver bounded, relevant peer work to the asking session |
| `langspan_symbol` | Find indexed definitions by name, with source digests and coverage; with `detail`, also body digest, calls, callers, notes and last check |
| `langspan_references` | Find name mentions, with coverage and truncation |
| `langspan_impact` | Find a file's exports and files mentioning those names; with `depth` 2 or 3, those files level by level |
| `langspan_blind_spots` | List the files the index cannot answer for (partial, never parsed cleanly, waiting, too large, unscanned) and other languages by extension |
| `langspan_integrate_check` | Combine branches onto the remote's target and run a command (an argument list, no shell, 1 to 3600 s) on exactly that tree; record the result |
| `langspan_land` | Dry run, or with `apply` take the push turn (waiting in line for up to `wait_s`, default 240) and push a combined result that has a passing check, retrying a moved target up to 3 attempts; conflicts come with their hunks |
| `langspan_land_queue` | Read who holds a target's push turn and who waits for it, in order |
| `langspan_flaky_tests` | Read which tests are known to flip on unchanged code, with runs, flips and the age of the last flip |
| `langspan_land_cancel` | Stop a landing waiting for the push turn, by its `request_id` |
| `langspan_recap` | Summarize one workspace's recorded activity, checks, notes and pending messages |
| `langspan_today` | The day so far from the event log: sessions, refusals, checks, landings and what is still open, within 2,400 bytes |
| `langspan_remember` | Keep a local note about the workspace or named files |
| `langspan_forget` | Remove a remembered note by ID |
| `langspan_recall` | Search a workspace's remembered notes: every query word in a note's text or paths, without case; best first, at most 50 |
| `langspan_send_message` | Send a local message to one session or every other session in the workspace |
| `langspan_show_message` | Read one message in full as it was sent, also after acknowledgement; for a briefing line marked `(cut; …)` |
| `langspan_handoff` | Leave a message for the next other session to take up |
| `langspan_retract_message` | Withdraw a message this session sent, by ID or `"last"`; its readers are told to disregard it |
| `langspan_commit_mine` | Commit only this session's own uncommitted changes in a shared checkout, with the owner's identity and hooks |
| `langspan_my_changes` | List this session's own uncommitted files and hunks in a shared checkout, as `langspan_commit_mine` would split them; changes nothing |

Every mutation tool requires a caller-owned `request_id` (for
`langspan_land_cancel`, it names the waiting landing). Conflicts are normal
results; unavailable coordination and domain errors are explicit tool errors.
The MCP briefing tool counts as delivery; the terminal `brief` command is a
peek. No tool launches agents, reads credentials, or changes workspace files.
`langspan_integrate_check` runs the given command as you, in a temporary
checkout, inside the `langspan mcp` process rather than the daemon; its output
goes to that process's standard error. `langspan_land` with `apply` pushes as
you, and `langspan_commit_mine` commits as you, running your Git hooks. Deny
any of them in your host's permission settings if agents should not do this.

## Move, switch and remove a workspace

These are owner commands of the CLI only; no MCP tool moves or removes a
workspace ([ADR 0025](adr/0025-distribution-lifecycle-and-store-beyond-the-first-release.md)).

```sh
langspan workspace list
mv /path/to/src/app /path/to/work/app
langspan workspace move app /path/to/work/app --request-id ws-move-1
```

```text
Workspace app now at /path/to/work/app. Kept: 6 notes, 2 pending messages, 14 check records, 3 landings.
Project hooks name the old root; rerun:
  langspan hooks install claude-code --workspace 'app' --root '/path/to/work/app' --state-dir '…' --apply
```

`workspace list [--after ID] [--json]` shows every workspace with its root and
whether it exists, its current sessions, notes, pending messages, check
records, and whether its automatic check and remote sharing are on. It answers
a page at a time, in workspace ID order: at most 100 workspaces, and fewer when
long roots would not fit one frame. When more remain, the page ends with
`More: langspan workspace list --after ID` (`next` in the JSON); run that for
the next page. A cursor whose workspace was removed since still continues after
it.

`workspace move ID NEW_ROOT` is for a checkout you moved: the old root must be
gone and NEW_ROOT must be the top of a Git checkout. With `--switch`, the old
root still exists and NEW_ROOT is another checkout of the same repository
(its object database holds the old root's `HEAD` commit), for example a fresh
clone that replaces a broken one; the old root then stops being coordinated
under the ID. `--switch` is refused (`root_derived_id`) when the ID is the one
user-level hooks derived from the old root, since new sessions there would
claim it again; move or delete the old checkout and run the move without
`--switch`, or register NEW_ROOT with `workspace add`. The ID, notes, pending
messages, check records, landings, the automatic check and the remote setting
are kept; an automatic check waiting or running in the old root is cancelled,
and the next writes in NEW_ROOT are checked there. File versions sessions read
under the old root are dropped, so a session's next edit of an existing file
has an unknown base until it reads it again; the next scan and the source
index start again from NEW_ROOT. A refused move changes nothing and says
which check failed: `not_checkout_top`, `old_root_exists`,
`old_root_missing`, `root_derived_id`, `different_repository` or
`workspace_conflict` (NEW_ROOT
is registered already or overlaps another workspace).

A checkout reached through a symbolic link is the same checkout. LangSpan
compares a registered root with the root Git reports by resolving links and
checking that both name the same directory (same device and inode). If you
move a checkout and leave a link at its old path, the workspace keeps
scanning, and sessions started through the link join it, under the
registered path. A link to a checkout of another repository (one without the
commit the last scan saw) stops the scan with `Git identity changed`.
`workspace move ID NEW_ROOT` to the link's target succeeds
without `--switch` and records NEW_ROOT. When the old root is a link to a
different directory, the `old_root_exists` refusal names the directory it
leads to; use `--switch` only if that is another checkout you mean to leave.
The checkout a registered link leads to cannot be registered, or receive a
moved workspace, under another ID. Its linked worktrees still can, at their
resolved paths: register a new one under the checkout's new place, or move
one registered under the old root to its resolved path. While the workspace
at the link holds an admission, such a registration is refused with
`workspace_in_use`; complete the admission or wait for it to expire.

User-level hooks need nothing: an event from NEW_ROOT joins the moved ID.
Project-level hooks name their root in the host's configuration, which the
command does not edit; it prints the `hooks install` commands to rerun for the
hosts whose project hooks name this workspace.

`workspace remove ID` is a dry run that lists what would end: sessions,
intents, notes, pending messages and the automatic check. `workspace remove ID
--apply --request-id ID` takes the workspace out of `status`, `recap`,
`workspace list`, briefings and the dashboard; `langspan events` keeps its
history. A hook event from that root later registers it again, empty.

Moving and removing are refused while a session holds an admission there or a
landing holds or waits for its push turn (`workspace_busy`), and while remote
sharing is on (`remote_sharing_on`): run `langspan remote disable ID` first,
so the published ref is removed, and wait until that ref is gone (`langspan
doctor` shows its progress): the daemon removes it through the old checkout's
remote.
A refusal is recorded under its request ID; retry with a new one.

## Retries and recovery

Local coordination mutations and remote enable/disable require `--request-id`.
An identical retry returns its original outcome; changing the command under that
ID returns `request_id_conflict`. Path/transport validation failures occur before
a receipt is recorded. A lost reply or timeout is an unknown outcome: retry
the original ID and arguments.

The CLI generates IDs for check and landing records unless `--request-id` is
given. Running a check command again under a new ID runs the command again; a
retry under the same ID on the same combined tree returns the recorded result.

Use `--json` to inspect `replayed`, `generation`, `lease_valid`, and the full
outcome. A historical `admitted` response can have `lease_valid: false` after
completion, expiry, or restart. It is not a fresh admission. Validity is observed
when the response is produced and cannot fence a later write.

The current wire protocol and SQLite schema marker are version 3. Upgrade the
daemon and clients together. Earlier persisted records remain readable.

A daemon restart advances the generation and invalidates outstanding grants.
Sessions and intents remain visible as stale. Reattach with a new registration
request ID, then explicitly refresh intent. Process liveness observations never
renew an admission or replace this recovery step.

A session counts as current for peers' briefings, the status line and the
dashboard while it is in this generation, not ended, not known to have exited,
and its newest edit, prompt or hook run is at most 30 minutes old. Any hook run
that reaches the daemon counts, reads, failed tool calls, tool batches, stops
and shell commands included; the daemon records one `session_seen` event per
session at most every 30 seconds. `langspan status` prints each session's last
hook run and process observation, and its `--json` form and `langspan_status`
give them as `summary.last_hook_age_ms` and `state.session_liveness`. Process
observations are made on Linux and macOS for Claude Code and Codex hosts; a
probe that fails, or of another user's process, records `unknown`, which does
not drop a session.

CLI exit codes: `0` successful result, `1` error/unavailable coordination or a
failed check, `2` conflict or refused push, `3` an inactive historical admission,
`4` stale source or unread lines, `5` inconclusive check, `6` not ready to
land, `7` the landing did not get the push turn (nothing was done), `8`
pushed but not recorded, and `9` whether a push happened is not known. The stale
result applies to versioned hook requests, including those sent through
`request JSON`; the plain `admit` command does not compare versions. Hook
processes exit `0` and convey denials through their JSON reply.

## Configuration and limits

Optional `config.json` in the state directory must be owner-only (`0600`):

```json
{"io_timeout_ms":2000,"request_timeout_ms":5000,"max_clients":32,"auto_check_debounce_ms":2000,
 "flaky_rerun_max_s":30,"host_check_ms":3600000,"notify":false,"notify_sound":false,
 "notify_kind_interval_ms":300000,"notify_hourly_limit":10,"notify_handoff_after_ms":600000}
```

Timeout values accept 100–5000 ms; client count accepts 1–128;
`auto_check_debounce_ms` accepts 50–60000; `flaky_rerun_max_s` accepts
0–3600 (0: a failing automatic check is never run again); `host_check_ms`,
how often the coordinator looks for agent hosts with no LangSpan install,
accepts 100–86400000. For [desktop notices](#desktop-notices),
`notify_kind_interval_ms` and `notify_handoff_after_ms` accept 1000–86400000
and `notify_hourly_limit` 1–60; `notify`, `notify_sound` and the two notice
limits are read again at each notice, `notify_handoff_after_ms` when the
coordinator starts. A build before notices refuses to start with a
`config.json` that has these keys. IPC and MCP frames
are limited to 64 KiB. The MCP SDK discards oversized input lines without replying
to that line. An MCP tool result carries each answer twice, as structured
content and as JSON text that escapes every quotation mark and backslash
again, so the adapter measures the whole encoded result and keeps it, with
512 bytes left for the JSON-RPC envelope, within one frame. An MCP page can
therefore be shorter than the CLI's: a page of `langspan_events`,
`langspan_changes` or `langspan_status` with `all` that would not fit is cut,
and the result ends with a text note starting "LangSpan cut this page to fit
one MCP reply" that names the `after` to call again with for the rest. The
`status` view and `langspan_recap` leave more out instead, counted as the
CLI's answer counts it (`summary`, `omitted_items`); every record is in
`langspan_status` with `all`. A result no rule can cut, such as an oversized
landing report, is replaced by the error `reply_too_large`, which keeps the
operation's `exit_code` and outcome; the CLI's `--json` gives the whole
answer. The CLI's answers are unchanged. File admission accepts up to 64 distinct normalized keys, 512 bytes
per key, with a TTL of 1–60000 ms. Intents have a 2048-byte summary limit
and prompt summaries a 512-byte one. A presence record names 1 to 4 paths, and
each session keeps its 16 most recent looked-at paths. Summaries are redacted on receipt, and
redaction never makes one longer than its limit: a summary that would outgrow
it keeps its longest redacted prefix that fits, cut at a character boundary
and ending in `[redacted]`, so a summary within its limit is never refused
because it held a secret.
Canonical path checks reject escapes, special files, and observed hard links.

Events and receipts do not yet have a retention policy. History is read in
bounded pages; large status snapshots remain subject to the IPC response limit.
Git observation also has explicit file, byte, branch, and deadline bounds; an
incomplete scan does not become a complete snapshot. See
[implementation status](IMPLEMENTATION_STATUS.md) and the
[threat model](security/threat-model.md) for current coverage,
platform limits, and the cooperative boundary.
