# Codex, 26 November 2026: end-of-reply and first-patch events

Captured on 26 November 2026 by `scripts/capture-codex.sh` (issue #11), run by
the owner on macOS against a real, signed-in Codex. The owner trusted the
throwaway project and its hooks by hand in Codex's `/hooks` screen; the script
passed no trust, approval or bypass option. Each turn was a `codex exec` run
with `-s workspace-write`. The hook events name no Codex version, and the
script did not yet record `codex --version`, so the directory is named for
the date. The model the events name is `gpt-6.2-sol`, the one
`fixtures/codex-0.171.0` names.

[`events.jsonl`](events.jsonl) holds the 27 events Codex sent, one per line,
in the order it sent them. The coordinator's record of the same run is
[docs/evidence/codex-turn-events](../../docs/evidence/codex-turn-events/README.md).

| Lines | Turn | Events |
| --- | --- | --- |
| 1-7 | 1, session A | `SessionStart` (`source: "startup"`), `UserPromptSubmit`, `PreToolUse` and `PostToolUse` for `Bash` `cat notes.txt`, `PreToolUse` for an `apply_patch` whose context does not apply, `Stop`, `SessionEnd` |
| 8-20 | 2, session A resumed | `SessionStart` (`source: "resume"`), `UserPromptSubmit`, `cat notes.txt`, the `apply_patch` LangSpan refused as stale (line 12), `cat notes.txt` again, the identical `apply_patch` again with a new `tool_use_id` (line 15) and its `PostToolUse`, a last `cat notes.txt`, `Stop`, `SessionEnd` |
| 21-27 | 3, session B | `SessionStart`, `UserPromptSubmit`, `cat notes.txt`, the `apply_patch` LangSpan refused because a CLI session held `notes.txt` (line 25), `Stop`, `SessionEnd` |

Between turns 1 and 2 the script wrote `notes.txt` through `langspan hook
claude-code` as a peer session; before turn 3 it started a CLI session that
held `notes.txt`. Those calls are LangSpan's own, not Codex's, and are not in
this file. `tests/turn_events.rs` makes them where the script did.

What the events show beyond the documentation:

- `Stop` carries no `turn_id`. The prompt and tool events do.
- No `PostToolUse` came for the patch whose context did not apply, nor for the
  two patches LangSpan refused.
- `SessionEnd` (`reason: "other"`) arrives at the end of every `codex exec`
  run, a few milliseconds after `Stop`. A resumed session sends
  `SessionStart` with `source: "resume"` and the same `session_id`.
- `permission_mode` is `bypassPermissions` under `codex exec -s
  workspace-write`.

Changes from the raw events, and nothing else:

- The repository path is `/work/repo`, and `transcript_path` is under
  `/home/user/.codex/sessions/2026/01/01/`, with the date and time in the file
  name set to `2026-01-01T00-00-00` (session A) and `2026-01-01T00-00-01`
  (session B).
- Session IDs are `00000000-0000-7000-8000-00000000000a` and `...0b`, turn IDs
  `...0000000a0001`, `...0000000a0002` and `...0000000b0001`, and each
  `tool_use_id` is `exec-00000000-0000-4000-8000-00000000000N`, numbered in
  order of first use. A `PreToolUse` and its `PostToolUse` keep one ID.

Prompts, tool inputs and responses, the model name and `last_assistant_message`
(the one word each prompt asked for) are as Codex sent them.
