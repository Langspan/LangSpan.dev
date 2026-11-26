# ADR 0035: Antigravity as a fifth host

**Status:** Implementation decision, 25 November 2026. The adapter was built
from Antigravity's documentation and then corrected against the first real
run, on Antigravity CLI (`agy`) 1.5.14 (issue #110), which showed that the
documented tool names and the empty `PreToolUse` answer do not hold there
(issue #187). A repeat real-host run of the corrected adapter on `agy` 1.5.14
verified admission, conflict and stale-source refusals, and the install
([issue #110 evidence](https://github.com/Langspan/LangSpan.dev/issues/110#issuecomment-7182408496)).
See [Antigravity 1.5.14](#antigravity-1514), [What is unverified](#what-is-unverified)
and [Evidence](#evidence). Antigravity is LangSpan's Google host: Gemini CLI
support was removed (issue #246, [ADR 0033](0033-gemini-cli-adapter.md)).

## Context

Google deprecated Gemini CLI in favor of the Antigravity suite, whose
command-line agent is `agy`. Antigravity's hooks are not Gemini CLI's: other
event names, other files, other answers. Four differences shape this adapter:

1. **An event does not name itself, as documented.** The documented fields
   common to every event are `conversationId`, `workspacePaths`,
   `transcriptPath`, `artifactDirectoryPath`, `modelName` and
   `lastUserInput`. None says which event it is, and the documentation does
   not say how a hook is meant to tell. (1.5.14 also sends `hookEventName`.)
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
| `PreToolUse` for `edit_file` | admission of `args.Path` (1.5.14) |
| `PreToolUse` for `write_to_file`, `replace_file_content`, `multi_replace_file_content` | admission of `args.TargetFile` (documented) |
| `PreToolUse` for `run_command` | shell admission and the before snapshot ([ADR 0011](0011-shell-writes.md)), in `args.Cwd`, else the workspace folder |
| `PostToolUse` for those tools | completion, or failure when `error` is not empty; shell comparison |
| `PostToolUse` for `read_file` | the version the session read (`args.Path`, 1.5.14) |
| `PostToolUse` for `view_file` | the version the session read (`args.AbsolutePath`, documented) |

An `edit_file` names its replacement (`Target`, `Replacement`), as a
`replace_file_content` does (`TargetContent`, `ReplacementContent`,
`AllowMultiple`), so each holds a region where the file parses
([ADR 0012](0012-region-admission.md)). `write_to_file` states the whole
file, and `multi_replace_file_content`'s chunk fields are not documented, so
both hold the file whole. A `run_command` with `RunPersistent` is a background
command. The call ID is `step-<stepIdx>`. The documented tool names stay in
the matchers beside the observed ones: they cost nothing where unused and
cover a version that uses them.

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
labelled as data, not instructions. Any other `PreToolUse` outcome is
`{"decision":"allow"}`, including a hook that cannot decide, cannot start or
panics once its `--event` is known: Antigravity 1.5.14 reads `{}` there as a
refusal (answer (a) below). The documentation describes `allow` as "proceed,
delegating to the next hook or default policy", unlike `auto_approve`, so
Antigravity's own permission policy still decides whatever LangSpan does not
deny. Context on `SessionStart` and `PreInvocation` is
`{"injectSteps":[{"systemMessage":{"systemMessage":...}}]}`. Every other
outcome on those events and on `PostToolUse` is `{}`. The hook never answers
`auto_approve`, `ask` or any other permission, never uses `overwrite` or
`permissionOverrides`, and never exits 2. Other hosts' adapters still never
answer allow: their empty answer means no decision.
`PostToolUse` takes no context, so mid-turn news waits for the next
`PreInvocation`, which comes before the next model call. Since injected steps
stay in the history, a briefing is sent only when it changed, as for every
host. A break alert ([ADR 0029](0029-break-alerts-at-write-time.md)) is not
checked: `PreToolUse` takes no context to carry it.

**Not registered.** `Stop` requires a `decision`, and any value but
`continue` lets the agent stop; what a hook that gives none does is not
documented, so `Stop` is left out, as Cursor's end-of-reply event is. There is no session-end event: an Antigravity session is never
marked ended; it ages out as any silent session does. `PostInvocation` adds
nothing `PreInvocation` does not.

**Installation.** `hooks install antigravity` writes `.agents/hooks.json` and
registers `langspan mcp` in `.agents/mcp_config.json` (`command` and `args`).
`install --hosts antigravity` writes `~/.gemini/config/hooks.json` and
`~/.gemini/config/mcp_config.json`; it is chosen only by name, because other
Google tools, the earlier Gemini CLI among them, also use `~/.gemini`. LangSpan's entries are the hook definition named
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
workspace folder holds a LangSpan project hook. The workspace
folder is the most specific one in `workspacePaths` that holds the target file
(for `run_command`, its `Cwd`), compared by whole path components, else the
first. A linked worktree listed inside its repository is therefore its own
folder whatever the order, and `/work/repo2` is never inside `/work/repo`. A command run below that folder is
still resolved from the folder, since that is where Antigravity reads its
hooks. A project definition with `"enabled": false` runs none of its hooks,
so it covers nothing: the user-level hook decides there. Claude Code's
documented switch is not per definition: `disableAllHooks` turns hooks off as
a whole. Where it applies, the user-level hook is off too, so deferring
changes nothing, and Claude Code's deferral is unchanged. This reading of its
settings has not been checked in a live run.

## Antigravity 1.5.14

The first real run (Antigravity CLI `agy` 1.5.14, 25 November 2026, default
permission mode, project hooks; issue #110) answered the questions the
documentation left open. Its events, with paths and identifiers replaced, are
in `fixtures/antigravity-1.5.14/`.

- **(a) `PreToolUse` without a decision is a refusal.** Every `run_command`
  the hook answered `{}` failed with `tool call denied by pre-tool hook:` and
  an empty reason. `{}` on `PostToolUse` has no effect. So the hook answers
  the explicit `{"decision":"allow"}`. In the repeat real-host run, 1.5.14
  honored all 7 allow answers, with no flag or mode that skips approvals in
  effect.
- **(b) `invocationNum` restarts at 0 each execution** and counts up once per
  model call within it. A resumed conversation (`--conversation`) starts at 0
  again, with `initialNumSteps` counting the steps already in it, and sends no
  `SessionStart`: its first event is `PreInvocation`, which registers its
  session. So the `invocationNum == 0` rule picks each prompt.
- **(c) How the command runs.** Through a shell: the trailing `#` marker is
  inert and the quoted paths work. The event arrives on stdin and the answer
  is read from stdout. The working directory is the workspace's `.agents`
  folder, not the workspace root, so the hook never resolves anything from its
  own working directory: the workspace comes from `workspacePaths`, else the
  event's `cwd`, and a project hook from its `--root`. The environment is
  Antigravity's own, plus `ANTIGRAVITY_CONVERSATION_ID`. Project hooks in
  `.agents/hooks.json` ran with no approval prompt.
- **Tool names and arguments.** Edits are `edit_file` (`Path`, `Target`,
  `Replacement`), reads are `read_file` (`Path`), and shell commands are
  `run_command` (`CommandLine`, `WaitMsBeforeAsync`, `toolSummary`, no
  `Cwd`). No documented edit or read tool name appeared. All paths
  (`Path`, `cwd`, `workspacePaths`) were absolute.
- **Fields the documentation does not list.** Every event carries
  `hookEventName` and `cwd`; a tool event carries `toolCall.id`. The hook
  still takes its event from `--event` and its call ID from `stepIdx`, which
  equals the call's step in the run's own output for both `PreToolUse` and
  `PostToolUse`. `PostToolUse` carries Antigravity's error text in `error`
  when a tool fails and `""` when it succeeds.
- **An empty `mcp_config.json`.** Antigravity leaves `.agents/mcp_config.json`
  and `~/.gemini/config/mcp_config.json` empty (0 bytes). An empty or blank
  Antigravity MCP file is therefore read as `{}` by the installer, `doctor`
  and the coordinator's host check; any other host's file must still be JSON.
- **Briefing label.** A peer's briefing named an Antigravity session by its
  conversation, not its workspace suffix: `antigravity session 00000000`, not
  `... subagent w17e6174`.
- **`doctor` and old matchers.** An install whose matcher for a tool event is
  not the one this version writes misses tools LangSpan handles, as the first
  install missed `edit_file` and `read_file`. `doctor` reports FIX for it, at
  project and user level, for every host; installing again writes the current
  matchers.

## What is unverified

- **Invalid JSON, a non-zero exit or a timeout** on `PreToolUse`. Not
  observed; the hook prints the allow answer before it exits for any of its
  own failures.
- **Subagents.** `invoke_subagent` starts conversations of their own; whether
  their events carry their own `conversationId` is not documented. Each is a
  session of its own, not linked to the one that started it.
- **Linked worktrees.** Which `.agents/hooks.json` runs for a workspace in a
  linked worktree has not been observed, so the main checkout's hook decides
  there too ([ADR 0003](0003-native-coordination-observation.md)).
- **Mixed-host runs** with Antigravity and another host in one checkout, and
  whether a briefing changes what the receiving Antigravity session does.

Known limits:

- **`admit --ttl-ms` is capped at 60 seconds.**
- **A resumed conversation (`--conversation`) sends no `SessionStart`.** Its
  first `PreInvocation` registers its session, as
  [Antigravity 1.5.14](#antigravity-1514) explains.
- **`~/.gemini/GEMINI.md`.** An earlier Gemini CLI install may have written
  LangSpan's skill section there, and Antigravity also reads that file.
  `langspan uninstall --hosts gemini-cli --apply` removes the section.

## Evidence

Verified: the documentation above, read for this adapter, and two real runs
on Antigravity 1.5.14.

In the first run, `injectSteps` delivered briefings at `SessionStart` and
`PreInvocation`, and a shell write to a held file was refused with LangSpan's
reason, which Antigravity showed and the model heeded. Edits and reads were
not seen and harmless commands were refused, as
[Antigravity 1.5.14](#antigravity-1514) explains.

The repeat run used the corrected adapter on main after PR #191, in a separate
Antigravity profile with Keychain access denied, with no flag or mode that
skips approvals, in a throwaway repository
([issue #110 evidence](https://github.com/Langspan/LangSpan.dev/issues/110#issuecomment-7182408496)).
It showed:

- **Admission.** An `edit_file` got `{"decision":"allow"}`, then its lease was
  granted, the edit completed successfully and the lease was completed. The
  `read_file` before it was recorded.
- **A harmless command.** `git diff` got allow and ran, with exit code 0.
- **Conflict.** An `edit_file` of a file another session held was refused
  with the conflict reason, and the file was unchanged. A shell append to the
  same file was refused too. The model reported the hold and stopped.
- **Stale source.** The file changed outside Antigravity between its
  `read_file` and `edit_file`. LangSpan refused the edit as stale; the model
  read the file again, and the retry was admitted and completed.
- **Install.** An empty `mcp_config.json` at project and user level installed
  cleanly, and `doctor` reported every Antigravity line OK.
- **Briefing label.** A peer's briefing read
  "antigravity session … in this checkout".
- **Allow and `stepIdx`.** Antigravity honored all 7 allow answers, and each
  `PostToolUse` carried the same `stepIdx` as its `PreToolUse`.

In that run, the first `run_command` of a turn sometimes failed with
Antigravity's own terminal sandbox error inside the confined profile. The
error came from Antigravity, not LangSpan, and Antigravity's retry worked.

`tests/antigravity.rs` replays the captured 1.5.14 events through the real
hook executable, a real daemon and real Git: an `edit_file` is allowed,
admitted and completed; an `edit_file` and a `run_command` write of a held
file are refused with the conflict; a `read_file`, an outside change and an
`edit_file` are refused as stale; a harmless `run_command` is allowed; a
resumed conversation registers at its first `PreInvocation`; the installed
command run in `.agents` resolves the workspace from the event; an empty
`mcp_config.json` installs cleanly at both levels with `doctor` OK; and
`doctor` reports FIX for the first install's matchers. The constructed
fixtures in `fixtures/antigravity-constructed/` still cover the documented
tool names, and install, uninstall and `doctor` round-trip the project and
user files with the owner's bytes kept. The first run captured no `edit_file`
`PreToolUse`, so the tests build it from the captured `PostToolUse`.

Not verified: the items in [What is unverified](#what-is-unverified).

## Consequences

- A fifth host in `Vendor`, the installer, `doctor`, the skill and the hook
  command, with no change to the wire protocol or the store.
- The installer can keep a host's entries in a named object of their own, not
  only under `hooks`, and a host's commands can name their event.
- `hook antigravity` takes `--event`; no other host does.
- No between-step delivery (issue #214): the hooks above run at a session's
  start, an invocation and a tool call, so an interface notice reaches an
  Antigravity session at its next hook call.
