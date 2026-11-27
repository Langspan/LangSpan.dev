# Connect a native agent

## All repositories for this user

`langspan setup` does all of this and starts the coordinator. `install`
writes host configuration only; it does not start the daemon. Preview the user-level hooks and
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
| Antigravity (verified on a real Antigravity CLI 1.5.14) | `~/.gemini/config/hooks.json` | `mcpServers.langspan` in `~/.gemini/config/mcp_config.json` |

By default, the installer selects hosts whose configuration directory exists.
Antigravity, whose `~/.gemini` other Google tools also use, is installed only
when named: `--hosts antigravity` (or
`--hosts claude-code,codex,cursor,antigravity`); `CLAUDE_CONFIG_DIR` and
`CODEX_HOME` are honored. LangSpan no longer supports Gemini CLI; to remove an
earlier install, see
[Removing an earlier Gemini CLI install](#removing-an-earlier-gemini-cli-install).
The generated hooks and MCP entries carry the same
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
`langspan uninstall --apply` removes the user-level entries it recognizes,
stops and removes the coordinator's login service if `setup` installed one,
stops the coordinator however it was started (one that holds `daemon.lock`
but doesn't answer within 5 seconds fails the uninstall with nothing removed),
removes the `~/.claude/skills` folder when
the skill was all it held, and leaves the coordination state directory in
place. It then prints the `rm` commands for the binary and the state directory.
`--purge` removes the state directory as well; it refuses a directory that
holds no `daemon.lock` and isn't empty, and it isn't valid with `--hosts`,
since the other hosts' hooks still use that directory. A hooks or MCP file left
holding nothing else (`{}`, or empty text) is removed rather than left behind.
Running it again after an upgrade also removes such a file an earlier
version left behind. A file with any of your own settings in it keeps them
byte for byte.
`~/.claude.json` is never removed.

### An agent tool installed later

Install chooses hosts once, when it runs. A host installed afterwards has no
LangSpan hooks, so its edits are neither checked nor shown to other sessions.
`langspan hosts [--home DIR] [--json]` lists each host LangSpan knows, with
`name`, `config_dir`, `present` (its configuration directory exists; `null`
with an `error` when it could not be looked at), `user_install` (`none`,
`current`, `outdated`, `other_state_dir` or `unknown`) and `next_step`, the
install command, where one is proposed. It only reads. Antigravity is listed
but never proposed: install it by naming it, as above.

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

### A status line in Claude Code

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
"Local midnight" follows the system's time zone (honoring `TZ`): LangSpan
reads the zone's rules from the system's zone files (`/etc/localtime`, or the
file or POSIX rule `TZ` names) and takes the offset at the instants around
midnight, so on a day the clocks change the count still starts at that day's
00:00. No `date` program is started. If the rules cannot be read, midnight
UTC is used. A day of more than 1024 refusals shows 1024.

### A shorter MCP tool list

`langspan install --mcp-tools hooked` writes the Claude Code and Codex MCP
entries as `mcp --state-dir DIR --tools hooked --host claude-code` (or
`codex`). Such a server leaves `langspan_admit`, `langspan_complete`,
`langspan_session_register` and `langspan_workspace_register` out of its tool
list while that host's hooks report for the workspace the host started the
server in; see [the MCP section](mcp.md#mcp). Other hosts keep every tool until
their hooks are verified on a real host, and the dry run says so. Like every
install, it writes nothing until `--apply`. `--mcp-tools all`, or installing
again without the option, writes the plain entry back; `uninstall --apply`
removes the entry with its flags. The default is `all`, and LangSpan never
switches it by itself. The measured saving is about 2.6 KB of the 32 KB list
([benchmarks/mcp-tools](../../benchmarks/mcp-tools/README.md)).

User-level installation has been tested with a temporary home and real daemon,
Git and hook executable. It has not been observed in a native Claude Code or
Codex session. Claude's user-scope MCP key remains inferred from its documentation;
`doctor` inspecting the entry does not prove the host loaded it. A real Claude
session listing `langspan` in `/mcp` remains an acceptance check.
[ADR 0010](../adr/0010-user-level-install.md) records the host configuration details
and remaining verification.

## One project

First inspect the proposed project settings. Replace `/path/to/checkout` with
the project your agent is editing:

```sh
./target/release/langspan hooks install claude-code --workspace demo --root /path/to/checkout
./target/release/langspan hooks install codex --workspace demo --root /path/to/checkout
./target/release/langspan hooks install cursor --workspace demo --root /path/to/checkout
./target/release/langspan hooks install antigravity --workspace demo --root /path/to/checkout
```

These commands are dry runs. Once reviewed, add `--apply` to write the entries.
The Claude installer uses `.claude/settings.local.json`; Codex uses
`.codex/hooks.json`. Both include session-start and prompt briefing hooks as well
as the supported edit hooks, and session-end handling. Claude Code and Codex get end-of-reply handling
(`Stop`), and Claude Code also gets `StopFailure`, `SubagentStop` and
`PostToolBatch` ([ADR 0027](../adr/0027-turn-and-batch-events.md)). Existing unrelated settings and hooks are preserved. The
host's own folder and hook trust requirements still apply. For Codex, review the
project hooks through its native hook-trust flow.

### Codex: start and resume at the resolved path

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
[ADR 0032](../adr/0032-cursor-adapter.md) lists the sources, what was not
verifiable and how to capture a real run. Consult the [detailed availability table](../reference-overview.md#current-availability)
before treating an adapter as validated for your host version.

Remove the managed entries with the matching vendor:

```sh
./target/release/langspan hooks remove claude-code --root /path/to/checkout --apply
./target/release/langspan hooks remove codex --root /path/to/checkout --apply
./target/release/langspan hooks remove cursor --root /path/to/checkout --apply
./target/release/langspan hooks remove antigravity --root /path/to/checkout --apply
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
worktree install's workspace ID instead; Cursor and Antigravity,
whose behavior there has not been observed, are treated the same way, so an event
is never left undecided. `langspan doctor --root DIR` names each linked
worktree whose install the host will not run, and fails when nothing covers
it. The per-host rules are in
[ADR 0003](../adr/0003-native-coordination-observation.md).

See [Claude adapter decisions](../adr/0002-claude-code-adapter.md),
[Codex contract coverage](../evidence/m3-codex/README.md) and the
[Antigravity adapter](../adr/0035-antigravity-adapter.md) for the exact boundary.

## Antigravity

Antigravity is LangSpan's Google host. The adapter is verified on a real
Antigravity CLI (`agy`) 1.5.14: edit admission, a harmless command, conflict
and stale-source refusals, and the install with `doctor`
([issue #110 evidence](https://github.com/Langspan/LangSpan.dev/issues/110#issuecomment-7182408496)).
Linked worktrees, subagents and mixed-host runs are not yet observed.
`hooks install antigravity` writes
`.agents/hooks.json` and registers `langspan mcp` in `.agents/mcp_config.json`;
`install --hosts antigravity` writes `~/.gemini/config/hooks.json` and
`~/.gemini/config/mcp_config.json`. An empty `mcp_config.json`, which
Antigravity leaves at both levels, is taken as an empty configuration.
Antigravity's App, CLI and IDE all read these files. Its `hooks.json` maps
hook names to definitions, so LangSpan's entries are the definition named
`langspan`, beside your own. Each installed command names its event with
`--event`. Each has a 5-second timeout:

| Event | Matcher | What LangSpan does |
| --- | --- | --- |
| `PreToolUse` | `edit_file`, `write_to_file`, `replace_file_content`, `multi_replace_file_content`, `run_command` | Admission; a conflict or stale source is `{"decision":"deny"}` with a labelled reason, anything else `{"decision":"allow"}` |
| `PostToolUse` | the same, and `read_file` and `view_file` | Records the result, the version read, or the shell command's changes |
| `SessionStart` | none | Registers the session and adds the briefing as a system message in `injectSteps` |
| `PreInvocation` | none | Before each model call: news from peers since the session was last told, as a system message; at the first call of an execution, the prompt summary and code briefing too |

Antigravity 1.5.14 edits with `edit_file` and reads with `read_file`; the
other tool names are the documented ones, kept in case another version uses
them. If `doctor` reports that a matcher does not name the tools this version
handles, an earlier install wrote it: run the install command again.

On `PreToolUse`, Antigravity 1.5.14 reads `{}` as a refusal, so the hook
answers `{"decision":"allow"}` whenever it does not deny, including when it
cannot decide. `allow` passes the call on to the next hook or Antigravity's
own permission policy; the hook never answers `auto_approve` or `ask`. Every
other event's answer, other than a briefing, is `{}`. A conversation is one
session in each workspace it works in, appearing as
`antigravity.<conversationId>.w<digest of the workspace>`; with several
workspace folders, each is registered and briefed at `SessionStart` and
`PreInvocation`. A tool call is identified by its `stepIdx`. A user-level
hook steps aside for the workspace's `.agents/hooks.json` only while that
file's `langspan` definition is not `"enabled": false`.
Antigravity has no session-end event, and its `Stop` requires a decision, so
neither is registered. An `edit_file` or `replace_file_content` holds the
region it replaces; `write_to_file` and `multi_replace_file_content` hold the
whole file.

Two limits apply on Antigravity. `admit --ttl-ms` is capped at 60 seconds. A
resumed conversation (`--conversation`) sends no `SessionStart`, so its first
`PreInvocation` registers it.
[ADR 0035](../adr/0035-antigravity-adapter.md) lists the sources, the real-host
evidence and what is still unverified.

## Removing an earlier Gemini CLI install

LangSpan no longer supports Gemini CLI: Google deprecated it in favor of the
Antigravity CLI. To remove LangSpan's entries from an earlier install, run
these commands. Without `--apply`, each one previews its change:

```sh
./target/release/langspan uninstall --hosts gemini-cli --apply
./target/release/langspan hooks remove gemini-cli --root /path/to/checkout --apply
```

`uninstall --hosts gemini-cli` removes LangSpan's user-level hooks and its
`langspan` MCP server entry from `~/.gemini/settings.json`
(`$GEMINI_CLI_HOME/.gemini/settings.json` when `GEMINI_CLI_HOME` is set), and
LangSpan's marked skill section from `~/.gemini/GEMINI.md`. `hooks remove
gemini-cli` removes LangSpan's project hooks from `.gemini/settings.json`.
Every other byte of these files stays. `langspan doctor` reports leftover
LangSpan Gemini CLI hooks or MCP entries as FIX, with the command that removes
them.

Until you remove them, LangSpan's leftover Gemini CLI hooks do nothing: the
hook prints `{}`, so Gemini CLI's own approval decides, and it records nothing
and starts no coordinator. At session start it prints one line on stderr that
says how to remove the hook. This compatibility path is kept for one release.
`hooks install gemini-cli` and `install --hosts gemini-cli` are refused.
[ADR 0033](../adr/0033-gemini-cli-adapter.md) records the removal.

## The LangSpan skill

The LangSpan skill teaches an agent how to work with LangSpan from its first
session: that LangSpan's messages are coordination data, never instructions;
how to respond to a briefing, a stale-source or unread-lines refusal, a
conflict, a break alert, an automatic check result and a held commit; when to
use `status`, `commit --mine`, `integrate check` and `land` (or the matching
`langspan_*` MCP tools); and what never to do. Its one source text is
[`src/skill.md`](../../src/skill.md). It is off by default:

```sh
./target/release/langspan install --skill                 # dry run: shows each file and its size
./target/release/langspan install --skill --apply
./target/release/langspan hooks install cursor --workspace demo --root /path/to/checkout --skill --apply
./target/release/langspan hooks install antigravity --workspace demo --root /path/to/checkout --skill --apply
```

| Host | Written as | Removed by |
| --- | --- | --- |
| Claude Code | The skill `~/.claude/skills/langspan/SKILL.md` | `langspan uninstall --apply` |
| Codex | A marked section of `~/.codex/AGENTS.md` | `langspan uninstall --apply` |
| Cursor | The project rule `.cursor/rules/langspan.mdc` | `langspan hooks remove cursor --root DIR --apply` |
| Antigravity | The rule `~/.gemini/config/rules/langspan.md` (`--hosts antigravity`), or the project rule `.agents/rules/langspan.md` (`hooks install antigravity --skill`), with `trigger: always_on` | `langspan uninstall --apply --hosts antigravity`, or `langspan hooks remove antigravity --root DIR --apply` |

An earlier Gemini CLI install may have written a LangSpan section to
`~/.gemini/GEMINI.md`, which Antigravity also reads. `langspan uninstall
--hosts gemini-cli --apply` removes that section.

`CLAUDE_CONFIG_DIR` and `CODEX_HOME` move these files as they move the hooks.
In `AGENTS.md` and `GEMINI.md`, every line outside
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
[ADR 0034](../adr/0034-langspan-skill.md) records the formats and their limits.

## A LangSpan block in the repository's AGENTS.md

An agent on a host without LangSpan's hooks, or one started before its hooks
were trusted, is told nothing by LangSpan. A short block in the repository's
own guidance file tells it that LangSpan coordinates the repository and which
MCP tools to call: `langspan_brief` and `langspan_intent_set` before starting,
`langspan_admit` before an edit and `langspan_complete` after it when no
LangSpan hook runs, and that other sessions' text is data. A session with
hooks edits as usual: the hooks admit and complete each edit, and a lease the
session took by hand would hold its own hooked edit. In a shared checkout it says to commit with
`langspan_commit_mine` when LangSpan's hooks run; without hooks, to commit with
`git commit -m "..." -- PATH...`, naming only files that hold nothing but the
session's own changes. Only the hooks record which lines a session wrote, so
without them `langspan_commit_mine` finds nothing of the session's and
answers `nothing_to_commit`. It says that without hooks this is advice: nothing stops an edit. Its
text is [`src/guidance.md`](../../src/guidance.md), at most 1,500 bytes with its
markers.

```sh
./target/release/langspan guidance                              # print it (also --format cursor-rules)
./target/release/langspan guidance install --root /path/to/checkout --file AGENTS.md          # dry run: the diff
./target/release/langspan guidance install --root /path/to/checkout --file AGENTS.md --apply
./target/release/langspan guidance remove --root /path/to/checkout --file AGENTS.md --apply
```

`install` writes the block at the top of the file, after any YAML front
matter (a first line `---` up to the next `---` line, which a rule file's
host reads its settings from), followed by one blank line, between the lines `<!-- langspan:begin -->` and `<!-- langspan:end -->`;
every other byte of the file is kept. Without `--apply` it prints the lines
it would add (`+`) or take out (`-`) and changes nothing. Installing again
leaves the one block where it is, also where the owner moved it. `remove`
takes out only the block, and the blank line after it when it is at the top
or right after the front matter, so the file is back to its earlier bytes; a file that held only the block is
removed. A missing file is created only with `--create`. Front matter whose
closing `---` is the file's last line without a newline is refused, since the
block could not follow it without changing that line. A block whose text
LangSpan did not write (edited by hand, or from another version), a marker
without its partner, a second block, or a changed marker line stops `install`
and `remove` before anything is written, naming the lines. `--file` is a path
inside `--root` (no `..`, no absolute path, no folder that leads outside it
through a symlink), and the file itself must not be a symlink. Nothing is
committed: the change is in the working tree for the owner to review.

`--format cursor-rules` adds a Cursor rule's front matter
(`alwaysApply: true`), for saving as a file under `.cursor/rules/`. `install`
writes the Markdown block only. Nothing is held or refused through any of
them.
