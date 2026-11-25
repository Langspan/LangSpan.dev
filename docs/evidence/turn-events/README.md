# End of a reply and of a tool batch: native-host evidence

**Recorded:** 25 November 2026, macOS 27.2 arm64, Claude Code 2.1.330
(`claude -p`, model `haiku`), Codex 0.171.0 (`codex exec`). One throwaway Git
repository per host. LangSpan debug build of `feat/more-host-events`. Paths
are rewritten to `/work/demo`. [ADR 0027](../../adr/0027-turn-and-batch-events.md)
records the decisions this exercises.

## Captures with the earlier build

Project hooks written by hand for every event, with `LANGSPAN_CAPTURE_DIR`
set, so the raw events were saved whatever the hook did with them. Two runs:

1. A prompt asking for two parallel `Read` calls, an `Edit`, and an `Explore`
   subagent. Claude Code sent `PostToolBatch` after each batch, the
   subagent's `PostToolBatch` with its `agent_id`, the parent's `Stop` while
   the subagent was still listed as running in `background_tasks`, then
   `SubagentStop`, a second prompt and `Stop` for the subagent's result, and
   `SessionEnd`.
2. `--model claude-langspan-nonexistent-1`: `StopFailure` with
   `error: "model_not_found"`, then `SessionEnd`.

Sanitized events: [fixtures/claude-code-2.1.330](../../../fixtures/claude-code-2.1.330/README.md).

## This build, installed by `hooks install claude-code --apply`

The installed file registered ten events: the three tool events and
`SessionStart`, `UserPromptSubmit`, `SessionEnd`, `Stop`, `StopFailure`,
`SubagentStop`, `PostToolBatch`. `doctor --root` reported "claude-code hooks
installed for 10 event(s)." The coordinator's full log is
[langspan-events.json](langspan-events.json).

| Run | Prompt (test text) | What the log shows |
| --- | --- | --- |
| 3 | A Bash write, a `Read` and `Edit` of `src/lib.rs`, a foreground `Explore` subagent | A `tool_batch_observed` per batch (seq 11, 13, 19, 27), each naming the call's operation ID. The `Edit` admitted (15) and completed by its post-tool event (18). The subagent registered with its parent (21), its own batch (24), `session_stopped` with nothing open (25) and `session_ended` (26) from `SubagentStop`. The parent's `Stop` (28) and `SessionEnd` (29) |
| 4 | One `Edit`, with `--permission-mode default`, so Claude Code refuses it without asking | `Edit` admitted (34). No post-tool event came: Claude Code sent `PostToolBatch` with the refusal as the tool's result (35). `Stop` completed the open admission (37) and recorded it as released (38), then `SessionEnd` (39) |

Run 4 is the case this change is for: before it, that admission stayed open
until its 30-second TTL.

In run 4 the batch hook also delivered a briefing (`briefing_delivered`,
seq 36, right after the batch). Whether the model received it was not
checked: the host's transcript was not read.

## Codex

`codex exec` in a new throwaway repository with project hooks for
`PreToolUse`, `PostToolUse` (all tools), `SessionStart`, `UserPromptSubmit`,
`Stop`, `SessionEnd` and `SubagentStop` printed:

> warning: Untrusted project-local configuration is disabled. Run `codex exec --trust` to allow project configuration and MCP servers for this and future chats in the project, including MCP commands outside the sandbox.

No hook ran and nothing was captured. `--trust` would record trust in the
owner's user-level Codex configuration, and no bypass flag was used. Codex
`Stop` and `SessionEnd` are therefore tested in their documented shape only
(`tests/turn_events.rs`), and the earlier real Codex captures
(`fixtures/codex-0.171.0`) cover the tool and session-start events.

## Observed, not changed here

In runs 1 and 3 the subagent was briefed about its own parent as a peer, with
the parent's task line (the first line of the test prompt). In both runs the
subagent's report described that text as a note asking it to run commands,
and the parent ignored it. The briefing labels peer text as data; a subagent
being told its parent's task as another session's is a separate question,
not addressed by this change.
