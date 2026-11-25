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

Once `gemini -p` can sign in on the owner's machine, replace these with real
events in `fixtures/gemini-cli-0.70.0/`. In a throwaway Git repository with a
running daemon:

```sh
langspan hooks install gemini-cli --workspace capture --root "$PWD" --apply
mkdir -p /private/tmp/gemini-capture/events
cat > policy.toml <<'EOF'
[[rule]]
toolName = "run_shell_command"
commandPrefix = "ls"
decision = "allow"
priority = 500
EOF
LANGSPAN_CAPTURE_DIR=/private/tmp/gemini-capture/events \
  gemini --skip-trust --approval-mode auto_edit --policy ./policy.toml -p \
  'Read notes.txt with read_file, change the line beta to BETA with replace, then run ls.'
```

`--approval-mode auto_edit` approves edit tools only; the policy file allows
`ls` and nothing else. The hook writes each event it receives to
`LANGSPAN_CAPTURE_DIR` (owner-only files). Replace home paths, the transcript
path and any text beyond the test prompt before committing them.
