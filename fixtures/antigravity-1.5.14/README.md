# Antigravity 1.5.14: captured hook events

Captured on 25 November 2026 from real headless `agy` runs (Antigravity CLI
1.5.14, macOS, default permission mode) in a throwaway Git repository with
`calc.py` and `main.py`. Project hooks in `.agents/hooks.json` ran the
installed LangSpan commands through a wrapper that saved each event, and a
second, capture-only `PostToolUse` hook with matcher `.*` saved every tool's
completion. The run is recorded on issue #110 and the fixes on issue #187.

| File | Event |
| --- | --- |
| `session-start.json` | `SessionStart` of a new conversation |
| `pre-invocation.json` | Its first `PreInvocation` (`invocationNum` 0) |
| `pre-invocation-resumed.json` | The first `PreInvocation` of a resumed conversation: `invocationNum` 0 again, `initialNumSteps` 7, and no `SessionStart` before it |
| `pre-run-command.json` | `PreToolUse` for `run_command` `git diff calc.py` |
| `pre-run-command-append.json` | `PreToolUse` for `run_command` `echo '# note' >> main.py` |
| `post-read-file.json` | `PostToolUse` for `read_file` of `calc.py` |
| `post-edit-file.json` | `PostToolUse` for `edit_file` of `main.py` |
| `post-edit-file-failed.json` | `PostToolUse` for an `edit_file` whose `Target` was no longer in `calc.py`, with Antigravity's `error` text |

No `PreToolUse` for `edit_file` or `read_file` was captured: the installed
matchers did not name those tools. The tests build an `edit_file`
`PreToolUse` from `post-edit-file.json` by removing `error` and setting
`hookEventName`, the only differences between the two `run_command` events'
shapes and the `PostToolUse` ones.

Changes from the raw events, and nothing else:

- The repository path is `/work/repo`; `artifactDirectoryPath` and
  `transcriptPath` are under `/home/user/.gemini/antigravity-cli/brain/`.
- Each conversation ID is a fixed fake UUID (`...a1`, `...b1`, `...b2`,
  `...c1`, one per conversation), and each `toolCall.id` a fixed fake
  `call_` number.

Prompts, model name, step indexes, tool names, arguments and error text are
as Antigravity sent them.
