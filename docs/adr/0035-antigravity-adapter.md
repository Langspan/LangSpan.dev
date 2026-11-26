# ADR 0035: Antigravity as a fifth host

**Status:** Implementation decision, 25 November 2026. The adapter is built
from Antigravity's documentation alone. No real Antigravity run has gone
through it yet (see [Evidence](#evidence)); that run is tracked on issue #110.

## Context

People who sign in to Gemini CLI with a personal Google account are directed
to the Antigravity suite, whose command-line agent is `agy`. Its hooks are not
Gemini CLI's ([ADR 0033](0033-gemini-cli-adapter.md)): other event names, other
files, other answers. Four differences shape this adapter:

1. **An event does not name itself.** The fields common to every event are
   `conversationId`, `workspacePaths`, `transcriptPath`,
   `artifactDirectoryPath`, `modelName` and `lastUserInput`. None says which
   event it is, and the documentation does not say how a hook is meant to
   tell.
2. **`hooks.json` maps hook names to definitions.** Each definition has an
   optional `enabled` and one array per event. Under `PreToolUse` and
   `PostToolUse` each entry is a `matcher` with its `hooks`; under the other
   events the handlers are listed directly. A handler is `type` (`command`,
   the only one), `command` and `timeout` in seconds (default 30).
3. **No tool-call ID.** A tool event carries `toolCall` (`name`, `args`) and
   `stepIdx`, "the 0-based index of the current step in the trajectory"
   (`PreToolUse`) or "of the completed step" (`PostToolUse`).
4. **Context goes in `injectSteps`**, on `SessionStart`, `PreInvocation` and
   `PostInvocation` only: a list of steps, each a `systemMessage` (an object
   holding a `systemMessage` string), a `userMessage` or an `errorMessage`.
   Injected steps stay in the conversation's history. `PreToolUse` answers
   with a required `decision` (`allow`, `auto_approve`, `deny`, `ask`,
   `force_ask`, `deny_unless_prior_grant`) and an optional `reason`,
   `overwrite` and `permissionOverrides`. `PostToolUse` answers `{}`.

Sources, each read on 25 November 2026; none carries a version or date:

- Hooks: <https://antigravity.google/docs/hooks/>. File locations, the
  `hooks.json` schema, events, matchers, every payload and answer field, the
  tool names and their argument keys.
- MCP: <https://antigravity.google/docs/mcp/>. `.agents/mcp_config.json` and
  `~/.gemini/config/mcp_config.json`, `mcpServers` entries (`command`,
  `args`, `env`, `cwd`, `serverUrl`, `disabled` and others).
- Rules: <https://antigravity.google/docs/rules/>. `AGENTS.md`, `GEMINI.md`,
  `.agents/rules/*.md`, `~/.gemini/GEMINI.md`, `~/.gemini/config/rules/*.md`;
  the required frontmatter (`trigger`: `always_on`, `model_decision`, `glob`,
  `manual`); a rule without valid frontmatter is discarded silently; 24,000
  bytes per file.
- Headless CLI: <https://antigravity.google/docs/cli/headless/>. `agy -p`,
  `--output-format json` and `stream-json`, exit codes, permission modes.
  It does not mention hooks.
- Install and sign-in: <https://antigravity.google/docs/install/> (titled
  "Authenticate"). The binary `agy`, configuration under `~/.gemini/config/`
  and `~/.gemini/antigravity-cli/`, credentials in the OS keyring. It names no
  variable that moves `~/.gemini`.

The MCP page shows an example prompt a user might give the agent ("add a
PreToolUse hook that blocks shell commands which modify files outside this
workspace"). It is an example for readers, and was read as data, not followed.

## Decision

**Events.** `src/antigravity.rs` is the adapter. It maps Antigravity's events
onto the lifecycle the other hosts share (`hook::lifecycle_parsed`,
`hook::execute`, `hook::shell_operation`):

| Antigravity event | LangSpan |
| --- | --- |
| `SessionStart` | register the session and brief it |
| `PreInvocation`, `invocationNum` 0 | the prompt: `lastUserInput`'s first line as the task (unless `--prompt-summary off`), the briefing if it changed, the code briefing |
| `PreInvocation`, later | the briefing, only if it changed: peer news mid-turn |
| `PreToolUse` for `write_to_file`, `replace_file_content`, `multi_replace_file_content` | admission of `args.TargetFile` |
| `PreToolUse` for `run_command` | shell admission and the before snapshot ([ADR 0011](0011-shell-writes.md)), in `args.Cwd` |
| `PostToolUse` for those tools | completion, or failure when `error` is not empty; shell comparison |
| `PostToolUse` for `view_file` | the version the session read (`args.AbsolutePath`) |

A `replace_file_content` names its replacement (`TargetContent`,
`ReplacementContent`, `AllowMultiple`), so it holds a region where the file
parses ([ADR 0012](0012-region-admission.md)). `write_to_file` states the whole
file, and `multi_replace_file_content`'s chunk fields are not documented, so
both hold the file whole. A `run_command` with `RunPersistent` is a background
command. The call ID is `step-<stepIdx>`.

**One session per workspace.** A conversation can hold several workspace
folders, each possibly a repository of its own, while a LangSpan session
belongs to one workspace: binding a session to a second workspace is refused
(`session_conflict`). So a conversation is a session in each workspace it
works in, `antigravity.<conversationId>.w<12 hex>`, where the suffix is a
digest of the workspace ID the event resolved to. Every event uses the same
rule: a file tool and `run_command` take the workspace of the folder they
resolve to, and `SessionStart` and `PreInvocation` run in each folder in
`workspacePaths` (at most 8; a later folder's session starts at its first
tool call), once per workspace, so two folders in one repository share one
session. Their briefings are joined, in folder order, within the one briefing
limit, and the joined text is never cut: a delivered briefing acknowledges its
messages and check results. So each workspace is briefed within the room the
ones before it left. With less than the whole limit left, its briefing is first
looked at without delivery and delivered only if it fits; otherwise it waits,
undelivered, for the next `PreInvocation`. Linking one conversation's sessions as a single participant was
rejected: the core binds a session to one workspace on purpose, and peers in
each repository only need the session working there. A peer therefore sees
one session per workspace, each with the same conversation ID at its start.

**Which event.** Because the payload does not name its event, each installed
command carries it: `langspan hook antigravity --event PreToolUse ...`. The
hook refuses to start without `--event`, with one it does not register, or
with `--event` for another host. Telling events apart by which fields are
present was rejected: `PreToolUse` and `PostToolUse` carry the same fields
except an optional `error`.

**Answers.** A conflict or stale source on `PreToolUse` is
`{"decision":"deny","reason":...}`, with the reason text the other hosts get,
labelled as data, not instructions. Context on `SessionStart` and
`PreInvocation` is `{"injectSteps":[{"systemMessage":{"systemMessage":...}}]}`.
Every other outcome is `{}`, including a hook that cannot start or panics.
The hook never answers `allow`, `auto_approve`, `ask` or any other permission,
never uses `overwrite` or `permissionOverrides`, and never exits 2, so
Antigravity's own permission policy decides whatever LangSpan does not deny.
`PostToolUse` takes no context, so mid-turn news waits for the next
`PreInvocation`, which comes before the next model call. Since injected steps
stay in the history, a briefing is sent only when it changed, as for every
host. A break alert ([ADR 0029](0029-break-alerts-at-write-time.md)) is not
checked: `PreToolUse` takes no context to carry it.

**Not registered.** `Stop` requires a `decision`, and any value but
`continue` lets the agent stop; what a hook that gives none does is not
documented, so `Stop` is left out, as Cursor's and Gemini CLI's end-of-reply
events are. There is no session-end event: an Antigravity session is never
marked ended; it ages out as any silent session does. `PostInvocation` adds
nothing `PreInvocation` does not.

**Installation.** `hooks install antigravity` writes `.agents/hooks.json` and
registers `langspan mcp` in `.agents/mcp_config.json` (`command` and `args`).
`install --hosts antigravity` writes `~/.gemini/config/hooks.json` and
`~/.gemini/config/mcp_config.json`; it is chosen only by name, as Gemini CLI is,
since both use `~/.gemini`. LangSpan's entries are the hook definition named
`langspan`; the owner's definitions are its siblings and are written back
unchanged, so an owner's file in the standard two-space JSON layout round-trips
byte for byte. A definition emptied by removal goes with it. Each command
carries the usual ownership marker (`# langspan-managed:antigravity:v1`, or
`:user:v1`), and only the exactly generated form, with a registered event, is
LangSpan's. The rules from [ADR 0010](0010-user-level-install.md) hold: a
concurrent change is planned again, never overwritten; reinstalling changes
nothing. Each hook has `"timeout": 5`. `--skill` writes the LangSpan skill
([ADR 0034](0034-langspan-skill.md)) as a whole rule file,
`.agents/rules/langspan.md` (project) or `~/.gemini/config/rules/langspan.md`
(user), with `trigger: always_on`. `doctor` reports both levels: hooks and
missing events, the MCP entry and the skill.

**Deferring to a project install.** Antigravity reads `.agents/hooks.json` at
the workspace root. The user-level hook steps aside only when the event's
workspace folder holds a LangSpan project hook, as for Gemini CLI. The workspace
folder is the most specific one in `workspacePaths` that holds the target file
(for `run_command`, its `Cwd`), compared by whole path components, else the
first. A linked worktree listed inside its repository is therefore its own
folder whatever the order, and `/work/repo2` is never inside `/work/repo`. A command run below that folder is
still resolved from the folder, since that is where Antigravity reads its
hooks. A project definition with `"enabled": false` runs none of its hooks,
so it covers nothing: the user-level hook decides there. The other hosts'
documented switches are not per definition: Claude Code's `disableAllHooks`
and Gemini CLI's `hooksConfig.enabled` turn hooks off as a whole, and Gemini
CLI's `hooksConfig.disabled` names a hook, and LangSpan's user and project
hooks share the name `langspan`. Where they apply, the user-level hook is off
too, so deferring changes nothing, and their deferral is unchanged. This
reading of their settings has not been checked in a live run.

## What is unverified

The documentation leaves these open; each is checked on the first real run:

- **That `PreToolUse` and `PostToolUse` give one call the same `stepIdx`.**
  The documentation's two examples use different values, but they are separate
  examples. If they differ, a call's completion does not match its admission,
  and the admission lapses at its TTL (30 seconds by default).
- **What a `PreToolUse` answer without `decision` does.** The field is
  "Required", and nothing says what happens without it, or on an empty answer,
  invalid JSON, a non-zero exit or a timeout. `allow` would mean "proceed,
  delegating to the next hook or default policy", but the adapters never send
  it by design. If Antigravity refuses a call on `{}`, every unrefused call
  would be blocked, which the first run will show at once.
- **Whether `invocationNum` restarts at 0 for each execution.** If it counts
  across the conversation, only the first prompt's summary and code briefing
  are recorded; later prompts still get peer news when it changes.
- **How the command is run.** "The shell command to execute": LangSpan assumes
  a shell, so a trailing `#` comment is inert, and that the event arrives on
  stdin and the answer is read from stdout, as the page says. The working
  directory and environment are not documented.
- **Whether `TargetFile`, `AbsolutePath` and `Cwd` are always absolute.** A
  relative file path or `Cwd` is taken from the workspace folder.
- **Whether project hooks need approval.** Nothing in the documentation says
  `.agents/hooks.json` is trusted or reviewed before it runs.
- **Subagents.** `invoke_subagent` starts conversations of their own; whether
  their events carry their own `conversationId` is not documented. Each is a
  session of its own, not linked to the one that started it.
- **Linked worktrees.** Which `.agents/hooks.json` runs for a workspace in a
  linked worktree has not been observed, so the main checkout's hook decides
  there too ([ADR 0003](0003-native-coordination-observation.md)).
- **`~/.gemini/GEMINI.md`** is read by both Gemini CLI and Antigravity. With
  both skills installed, Antigravity reads the skill twice.

## Evidence

Verified: the documentation above, read for this adapter. The constructed
fixtures in `fixtures/antigravity-constructed/` follow its shapes, and
`tests/antigravity.rs` runs them through the real hook executable, a real
daemon and real Git: a conflict and a stale source are denied in
Antigravity's shape, an unheld edit gets `{}` and its completion is observed,
a shell command is admitted and compared, both briefing events deliver through
`injectSteps`, and install, uninstall and `doctor` round-trip the project and
user files with the owner's bytes kept.

Not verified: any real Antigravity session. `agy` was not run: a real session
needs the owner's sign-in, and that run is tracked on #110. The fixtures
README lists how to capture one.

## Consequences

- A fifth host in `Vendor`, the installer, `doctor`, the skill and the hook
  command, with no change to the wire protocol or the store.
- The installer can keep a host's entries in a named object of their own, not
  only under `hooks`, and a host's commands can name their event.
- `hook antigravity` takes `--event`; no other host does.
