# Gemini CLI protocol examples, not captured host events

These JSON files were written by hand for deterministic tests, from Gemini CLI
0.70.0's hooks reference and the code that builds hook input. They are **not**
a replay of a Gemini session. The installed CLI reports `0.70.0`, but on the
machine where they were written its headless runs stopped at sign-in, before
any hook fired ([ADR 0033](../../docs/adr/0033-gemini-cli-adapter.md#evidence)).

The session ID, paths, timestamps, transcript path, prompt, file contents and
tool output are invented. They hold no account data or transcript content.

Sources, at tag `v0.70.0` of `github.com/google-gemini/gemini-cli`:

- `docs/hooks/reference.md`: the base input fields, `BeforeTool` and
  `AfterTool` fields (`tool_name`, `tool_input`, `tool_response` with
  `llmContent`, `returnDisplay` and an optional `error`), `BeforeAgent`
  (`prompt`), `SessionStart` (`source`) and `SessionEnd` (`reason`).
- `docs/reference/tools.md`: tool names and argument keys (`write_file`:
  `file_path`, `content`; `replace`: `file_path`, `old_string`, `new_string`,
  `instruction`, `allow_multiple`; `read_file`: `file_path`;
  `run_shell_command`: `command`, `description`, `dir_path`, `is_background`).
- `packages/core/src/hooks/hookEventHandler.ts`: `createBaseInput` and the
  `fire*Event` methods that build each payload.
- `packages/core/src/core/coreToolHookTriggers.ts` and
  `packages/core/src/scheduler/hook-utils.ts`: `BeforeTool` gets a copy of the
  call's arguments before confirmation; `AfterTool` gets the arguments the
  tool ran with (changed only if the user or a hook modified them) and the
  tool's result.
- `packages/core/src/tools/edit.ts`, `write-file.ts`, `shell.ts`: the result
  text in `llmContent`.

## Capturing real events

Once Gemini CLI can sign in on the owner's machine, replace these with real
events in `fixtures/gemini-cli-<version>/`. With a release build and a
running daemon, a person runs, at a terminal:

```sh
GEMINI_CLI_HOME=DIR bash scripts/capture-gemini.sh OUT
```

`DIR` is a Gemini CLI home kept for captures, already signed in by running
`gemini` there. A relative `DIR`, `LANGSPAN_STATE_DIR` or `XDG_STATE_HOME`
is taken from the directory the script starts in, and the steps it writes
name the same state directory, so they work from any terminal. The script stops, writing nothing, unless Gemini CLI answers a
one-word prompt from that home. It then makes a throwaway repository under
`OUT` with LangSpan's project hooks and `LANGSPAN_CAPTURE_DIR` set, and starts
Gemini CLI there interactively with `--approval-mode default`. It prints four
prompts to type and the steps to run in a second terminal between them: a
`BeforeAgent` briefing, a `replace` denied while a CLI session holds the file,
a `read_file`, a peer's write, and a `replace` refused once as stale whose
retry goes through and is completed by its `AfterTool`. The hook writes each
event it receives to `OUT/capture` (owner-only files); `langspan status` and
`events` are saved beside it. Replace home paths, the transcript path and any
text beyond the prompts before committing them.

The capture uses no automatic approval: no `--approval-mode auto_edit` or
`yolo`, no policy file that allows a tool, and no `--skip-trust`. The person
approves each edit with "Allow once" and trusts the folder and its hooks
themselves. An earlier version of this recipe used `auto_edit` and an allow
policy. That captures a different path: Gemini CLI skips its confirmation, so
the capture would not show where LangSpan's `BeforeTool` decision falls
relative to the person's approval, nor how a denial reads in an ordinary
session. It would also have the script approve edits on the person's behalf,
which LangSpan does not do for any host.
