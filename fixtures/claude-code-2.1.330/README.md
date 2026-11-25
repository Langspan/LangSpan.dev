# Claude Code 2.1.330: end-of-reply and tool-batch events

Captured on 25 November 2026 from real `claude -p` runs (Claude Code 2.1.330,
macOS) in a throwaway Git repository, with project-level hooks and
`LANGSPAN_CAPTURE_DIR` set. See
[the evidence record](../../docs/evidence/turn-events/README.md) for the runs.

| File | Event |
| --- | --- |
| `stop.json` | `Stop`, nothing in the background |
| `stop-with-running-subagent.json` | The parent's `Stop` while its `Explore` subagent was still running (`background_tasks`) |
| `subagent-stop.json` | `SubagentStop` for that subagent |
| `stop-failure.json` | `StopFailure`, `error: "model_not_found"` (an unknown model name) |
| `session-end.json` | `SessionEnd`, `reason: "other"` |
| `post-tool-batch-reads.json` | `PostToolBatch` after two parallel `Read` calls |
| `post-tool-batch-edit.json` | `PostToolBatch` after one `Edit` |
| `post-tool-batch-subagent.json` | A subagent's `PostToolBatch`, carrying its `agent_id` |
| `post-tool-batch-refused-edit.json` | `PostToolBatch` after an `Edit` Claude Code refused at its permission check (`--permission-mode default`); no post-tool event was sent for that call |

Changes from the raw events, and nothing else:

- The repository path is `/work/demo`, and the transcript paths are under
  `/home/user/.claude/projects/-work-demo/`.
- `last_assistant_message` of `Stop`, `SubagentStop` and
  `stop-with-running-subagent.json` is replaced by a short sentence: those
  are model output. The `StopFailure` value is Claude Code's own error text,
  kept as sent. File contents in `tool_response` are the test repository's
  (`# demo`, a three-line `greet` function).

Session, prompt, tool-use and agent IDs are the real ones from those runs.
