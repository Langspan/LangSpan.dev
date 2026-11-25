# Cursor hook events: constructed, not captured

**These files were written by hand. No Cursor run produced them.** They stand in until a real capture exists ([ADR 0032](../../docs/adr/0032-cursor-adapter.md) says how to take one). Tests built on them show that LangSpan handles this shape. They do not show that Cursor sends it.

The shape was built from two sources, both read on 24 November 2026:

1. Cursor's documentation. None of these pages carries a date or a version.
   - Hooks: <https://cursor.com/docs/agent/hooks>. Event names, the common input fields, per-event input and output fields, `hooks.json`, exit codes, fail-open behaviour.
   - Claude Code hooks loaded by Cursor: <https://cursor.com/docs/reference/third-party-hooks>.
   - MCP: <https://cursor.com/docs/context/mcp> and <https://cursor.com/docs/cli/mcp>.
2. The installed Cursor CLI, `cursor-agent` 2026.11.22-e9042b2 (Homebrew cask `cursor-cli`). Its bundled JavaScript was read where the documentation is silent. It is not copied here; it is summarised in the ADR.
   - The `tool_input` of each tool. The documentation gives Shell's only. From the CLI: Write is `{file_path, content}`, Delete and Read are `{file_path}`, Shell is `{command, cwd}`.
   - `postToolUseFailure` carries no `cwd`.
   - A `session_id` equal to `conversation_id` is added to every agent event.

Values that are placeholders, not observed:

- `cursor_version` is the documentation's example, `1.7.2`. What a real Cursor reports is unknown.
- `model` is `default`.
- The IDs are made up. `tool_use_id` is written in the hook ID grammar. The adapter replaces a real ID that falls outside it with a digest, because the real format is unknown.
- `/work/demo` is a placeholder root. The tests replace it with a temporary repository.
- `user_email` and `transcript_path` are `null`, which the documentation allows.

| File | Event | Used for |
|---|---|---|
| `session-start.json` | `sessionStart` | registration and the start briefing |
| `before-submit-prompt.json` | `beforeSubmitPrompt` | the prompt summary and briefing |
| `pre-write.json`, `post-write.json`, `post-failure-write.json` | `preToolUse`, `postToolUse`, `postToolUseFailure` for `Write` | admission and observation |
| `pre-delete.json` | `preToolUse` for `Delete` | admission |
| `post-read.json` | `postToolUse` for `Read` | the read version |
| `pre-shell.json`, `post-shell.json` | `preToolUse`, `postToolUse` for `Shell` | shell admission and observation |
| `session-end.json` | `sessionEnd` | the end of a session |

When a capture exists, put it in `fixtures/cursor-<version>/` with its own README. Then delete each file here that the capture replaces.
