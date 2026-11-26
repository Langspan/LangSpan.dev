# ADR 0033: Gemini CLI as a third host

**Status:** Removed, 26 November 2026 (issue #246). Google deprecated Gemini
CLI in favor of the Antigravity CLI (`agy`) in May 2026, and Gemini CLI
stopped serving most users in June 2026. Antigravity is LangSpan's Google host
([ADR 0035](0035-antigravity-adapter.md)). The adapter never ran in a live
Gemini session. One release keeps a compatibility path so that existing
installs keep working and can be removed; see [Removal](#removal).

The adapter was an implementation decision of 24 November 2026, built from
Gemini CLI 0.70.0's documentation and source. The sections after Removal
record that decision as it was made; none of them describes current support.

## Removal

LangSpan no longer installs, coordinates or briefs Gemini CLI sessions. The
adapter (`src/gemini.rs`), its contract fixtures, its tests and its capture
script are removed. For one release, LangSpan keeps this compatibility path:

- `langspan hook gemini-cli` (also `hook gemini`) does nothing. It prints `{}`,
  which gives no decision, so Gemini CLI's own approval decides. It exits 0,
  starts no coordinator and records nothing. On a `SessionStart` event, it
  prints one line on stderr that says Gemini CLI support was removed and how
  to remove the hook.
- `langspan hooks remove gemini-cli --root DIR [--apply]` removes LangSpan's
  project hooks from `.gemini/settings.json`.
- `langspan uninstall --hosts gemini-cli [--apply]` removes LangSpan's
  user-level hooks and its `langspan` MCP server entry from
  `~/.gemini/settings.json` (`$GEMINI_CLI_HOME/.gemini/settings.json` when
  set), and LangSpan's marked skill section from `~/.gemini/GEMINI.md`. Every
  other byte stays. LangSpan still reads `GEMINI_CLI_HOME` for this reason.
- `hooks install gemini-cli` and `install --hosts gemini-cli` are refused with
  a message that says support was removed.
- `langspan doctor` reports leftover LangSpan Gemini CLI hooks or MCP entries,
  at project and user level, as FIX, with the command that removes them.

`langspan guidance` no longer has a `gemini` format, `langspan hosts` no
longer lists Gemini CLI, and `langspan report` counts earlier Gemini CLI
sessions as `other`.

## Context

Gemini CLI has command hooks configured in `settings.json`, much like Claude
Code. Its events and fields have other names, and three differences matter to
LangSpan:

1. A tool event carries no tool-call ID. `BeforeTool` and `AfterTool` carry the
   base fields (`session_id`, `transcript_path`, `cwd`, `hook_event_name`,
   `timestamp`) plus `tool_name`, `tool_input` and, after the call,
   `tool_response` (`llmContent`, `returnDisplay`, optional `error`). The
   scheduler has a call ID, but it does not pass it to hooks
   (`packages/core/src/hooks/hookEventHandler.ts`, `fireBeforeToolEvent` and
   `fireAfterToolEvent`).
2. When a hook's stdout is empty, Gemini parses its stderr as the answer
   (`packages/core/src/hooks/hookRunner.ts`: `stdout.trim() || stderr.trim()`).
   Plain text there becomes `{"decision":"allow","systemMessage":...}`.
   LangSpan logs one JSON line to stderr after every hook run.
3. The hook timeout is in milliseconds. Claude Code and Codex use seconds.

Sources, all at tag `v0.70.0` of `github.com/google-gemini/gemini-cli`. The
installed package ships the same documentation files byte for byte (compared by
Git blob hash):

- `docs/hooks/reference.md`: input and output schemas, exit codes.
- `docs/hooks/index.md`: events, matchers, configuration layers, the hook
  environment, project-hook fingerprinting.
- `docs/hooks/best-practices.md`: project hook trust, environment redaction.
- `docs/reference/configuration.md`: settings file locations, `GEMINI_CLI_HOME`,
  `mcpServers`, `hooksConfig`, `$VAR` expansion in settings strings.
- `docs/reference/tools.md` and `docs/tools/file-system.md`, `docs/tools/shell.md`:
  tool names and argument keys.
- `docs/cli/trusted-folders.md`: an untrusted folder's `.gemini/settings.json`
  is not loaded.
- Source: `packages/core/src/hooks/hookEventHandler.ts`, `hookRunner.ts`,
  `hookPlanner.ts`, `types.ts` and `packages/core/src/core/coreToolHookTriggers.ts`
  (read in the installed bundle, which is built from them).

## Decision

**Events.** `src/gemini.rs` was the adapter. It mapped Gemini events onto the
lifecycle the other hosts share:

| Gemini CLI event | LangSpan |
| --- | --- |
| `SessionStart` (`startup`, `resume`, `clear`) | register the session and brief it |
| `BeforeAgent` (`prompt`) | the prompt: record its first line as the task (unless `--prompt-summary off`), brief, look up named symbols |
| `BeforeTool` for `write_file`, `replace` | admission of `tool_input.file_path` |
| `BeforeTool` for `run_shell_command` | shell admission and the before snapshot ([ADR 0011](0011-shell-writes.md)), run in `dir_path` |
| `AfterTool` for those tools | completion or failure (`tool_response.error` present), shell comparison |
| `AfterTool` for `read_file` | the version the session read |
| `SessionEnd` | the session ends |

A `replace` names its replacement (`old_string`, `new_string`,
`allow_multiple`), so it holds a region where the file parses
([ADR 0012](0012-region-admission.md)). A `write_file` holds the whole file.

Sessions are `gemini.<session_id>`, inside the existing ID grammar.

**Answers.** A conflict or stale source is
`{"decision":"deny","reason":...}`. The reason text is the same one the other
hosts get, labelled as data, not instructions. Gemini sends it to the model as
the tool's error, and the turn continues. Context is
`hookSpecificOutput.additionalContext`. Gemini adds it to the prompt on
`BeforeAgent`, to the session's first turn (or, headless, the prompt) on
`SessionStart`, and to the tool result on `AfterTool`, so mid-turn news is on.
Every other outcome is `{}`, never empty output, because empty stdout would
make Gemini read the stderr log as the answer. The hook never answers `allow`
and never exits 2. `BeforeTool` runs before Gemini's policy check and
confirmation, so a deny comes before the user is asked.

**Call identity.** The adapter derives a call ID from the session, the tool and
what the tool acts on (`file_path`, or `command` and `dir_path`), plus a count
of such calls and a nonce made at each `BeforeTool`. `BeforeTool` takes the
next number by linking an owner-only marker file holding the nonce into place
exclusively under `STATE/gemini-calls/<digest of the session ID>/`; a digest, because the ID grammar admits `.` and `..`. `AfterTool` takes the
latest marker's number and nonce. A `read_file` has no `BeforeTool`, so each
completed read takes a new number. The nonce matters because Gemini keeps the
session ID when a session is resumed (`--resume`, `/resume`, `--session-id`):
without it, a resumed session whose markers were removed would repeat earlier
IDs, and the coordinator would answer its calls from old receipts.
`SessionEnd` removes the session's markers. Because Gemini does not wait for
`SessionEnd`, a session's first call also removes marker directories older
than a week. The rest of the input is left out of the identity, so an edit the
user changes in Gemini's confirmation dialog keeps its ID. Its input digest
differs, though, so its admission is not completed and lapses at its TTL
(30 seconds by default).

**Installation.** `hooks install gemini-cli` writes `.gemini/settings.json` in
the project. `install --hosts gemini-cli` writes `~/.gemini/settings.json`
(`$GEMINI_CLI_HOME/.gemini/` when set). That one file holds both the hooks and
`mcpServers.langspan` (`command` and `args`, no `trust`), so they are planned
together and written in one guarded write. The rules from
[ADR 0010](0010-user-level-install.md) hold: a concurrent change is planned
again, never overwritten; reinstalling changes nothing; uninstall removes only
entries in the exact generated shape. Each hook is named `langspan` and has
`"timeout": 5000`. A dry run of the user file shows only LangSpan's entries,
because the rest can hold the owner's MCP server credentials. Gemini expands
`$NAME` in every settings string, so a path containing `$` is refused rather
than written. `doctor` reports both installs. `install` chooses Gemini CLI only when named
in `--hosts`: the adapter has no live evidence yet, and other Google tools
create `~/.gemini` too.

**Deferring to a project install.** Gemini reads project settings only from
the directory it was started in (`loadSettings(process.cwd())`), which is the
event's `cwd`. The user-level hook steps aside only when that directory's
`.gemini/settings.json` holds a LangSpan project hook, not for one further up
the repository. Whether Gemini trusts the folder, and so loads that file, is
not visible to the hook.

**Failures.** A hook that cannot start (a bad option) or panics still prints
`{}` first: Gemini would read plain text on stderr from a failed process as a
deny.

## What Gemini CLI cannot support here

- **No call ID.** Two calls on the same target in flight at once can share a
  number. The second one's report is then answered as the first's, so one of
  them is not recorded.
- **No subagent identity.** The events have no agent field. A call is
  attributed to the `session_id` it carries; whether Gemini's subagents carry
  their parent's was not checked.
- **`read_many_files`** names globs, not files, so it is not recorded as a
  read. Its files can still be refused as stale on the next edit.
- **No liveness.** Gemini runs as `node`, so no host process is recognised.
- **Project hooks need a trusted folder.** With folder trust on, an untrusted
  folder's `.gemini/settings.json` is not loaded, and headless runs stop. (The
  configuration reference gives `security.folderTrust.enabled` a default of
  `true`; the trusted-folders page says the feature is off by default.)
  Gemini warns once before it first runs a new project hook, and again
  whenever the command changes, so a reinstall that moves the executable is
  warned about again.
- **Settings with comments.** Gemini accepts comments in `settings.json`;
  LangSpan's JSON reader does not, so it refuses to install into such a file,
  and a commented project file makes the user-level hook give no decision in
  that directory.
- **`SessionEnd` is best effort.** Gemini does not wait for it, so a session
  may not be marked ended.

## Evidence

Verified:

- The installed CLI is 0.70.0 (`gemini --version`). Its `gemini hooks --help`
  lists only `migrate`; `gemini mcp add --help` shows `--scope user|project`.
- The documentation and source above, read for this version.
- The contract fixtures in `fixtures/gemini-cli-contract/` followed those
  shapes, and tests ran them through the real hook executable, daemon and Git.
  Both were removed with the adapter.

Not verified: a live Gemini session. Headless runs on this machine stopped
before any hook fired. Gemini's account sign-in is rejected ("This client is no
longer supported for Gemini Code Assist for individuals"), and no Vertex AI
settings were in effect. LangSpan does not change a host's sign-in. No live
session ran before the adapter was removed.

## Consequences

- A third host in `Vendor`, the installer, `doctor` and the hook command, with
  no change to the wire protocol or the store. After the removal, only the
  compatibility path in [Removal](#removal) remains.
- The shared lifecycle accepts `BeforeAgent` as the prompt event.
- Small marker files under the state directory are a new piece of local state.
  They hold no content: only a digest of the tool and its target.
- No between-step delivery (issue #214): the hooks above run at a session's
  start, an agent turn and a tool call, so an interface notice reaches a
  Gemini CLI session at its next hook call.
