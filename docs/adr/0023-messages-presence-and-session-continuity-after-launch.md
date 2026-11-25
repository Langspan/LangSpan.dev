# ADR 0023: Richer messages, presence, wake and session continuity wait

**Status:** Not now. Decided 25 November 2026.

## Context

LangSpan delivers peer work at session start, at each prompt and, on Claude
Code, after each completed tool call ([ADR 0004](0004-automatic-peer-briefing.md)).
Messages, handoffs and notes are local to one machine and arrive in the next
briefing ([ADR 0009](0009-agent-messages-and-handoff.md)). A richer design would push
messages into open sessions, start an agent to take a message, show where
agents are looking, and build handoffs and notes with more structure.
The capability review of 25 November 2026 ([roadmap](../roadmap.md))
lists these as Missing or Partial.

The launch bar includes Codex reads, Stop and SessionEnd (L2) and Claude Code
Stop, SubagentStop and PostToolBatch (L3). Several rows here depend on those
events and can follow them.

## Decision

| Row | Capability | LangSpan today | Decision and reason |
| --- | --- | --- | --- |
| A13 | Reads, greps and shell calls shown to peers as presence | Reads recorded as versions only | Not now. Adds briefing volume without evidence it changes decisions |
| A14 | Briefing for a repository first reached mid-session | Workspace from the event's working directory | Not now |
| A15 | Mid-turn delivery on Codex | Next prompt only | Not now. Waits for confirmation that Codex honours added context on PostToolUse |
| A16 | Instant message push into an open session | Next hook | Not now. Uses a host messaging socket: a new trust boundary |
| A17 | Wake: an unattended agent started for an untaken message | None | Not now. Starting agents without the owner is a large step in autonomy and needs its own ADR |
| B12 | Tripwire that fires into the next editor's tool result | Notes told at the next prompt | Not now |
| C3 | Start-of-session recency: changed modules, renames, hotspots, abandoned work | Peers' recent edits and task lines | Not now |
| C8 | Two tasks on the same code told before either writes | Peers ranked by overlap | Built 25 November 2026 ([issue #45](https://github.com/Langspan/LangSpan.dev/issues/45)), ahead of the condition below: prompt targets, see [ADR 0004](0004-automatic-peer-briefing.md). Not yet run on a host |
| C9 | Automatic handoff and trail at session end | Manual `handoff` | Not now. Can be built from recorded edits once L2 and L3 deliver the end events |
| C12 | Worktree advice and tidying | None | Not now. Tidying would remove the owner's worktrees |
| E1 | Notes anchored to a definition with staleness, tags and confidence | One-line notes on a workspace or files | Not now |
| E2 | `recall` search | Word search over held notes | Built 25 November 2026 ([issue #61](https://github.com/Langspan/LangSpan.dev/issues/61)), ahead of the condition below: `langspan recall` and MCP `langspan_recall`, see "Note search (E2)" below. Not yet run on a host |
| E4 | Messages addressed by path and symbol | To one session or all | Not now |
| E7 | Sender signed automatically | `--from SESSION` | Not now |
| E9 | Messages across machines | Local only | Not now. Could ride the Git-ref transport (ADR 0006), which would widen its payload and needs that ADR's review |
| D10 | `list_activity` and an MCP activity resource an agent can read (added by the second review) | MCP tools `langspan_status`, `langspan_changes`, `langspan_events` | Not now. The tools answer the same reads; a resource adds a surface no host has been shown to use |
| E11 | Retract a sent message, telling readers to disregard it (added by the second review) | `langspan messages retract`, MCP `langspan_retract_message` | Built 25 November 2026 ([issue #66](https://github.com/Langspan/LangSpan.dev/issues/66)), ahead of the condition below: a `message_retracted` event that briefings, the recap and the dashboard honour, see [ADR 0009](0009-agent-messages-and-handoff.md). Not yet run on a host |
| E12 | Delivery and seen receipts the sender can ask for (added by the second review) | Acknowledgements are kept and counted on the local page | Not now. The data is partly there; a sender query follows E11, since both read the same message state |
| E13 | Show a shortened message in full (added by the second review) | None | Not now. Messages are capped at 400 bytes when sent, so a delivered line is cut only when quoting takes it past 700 bytes |

## Note search (E2)

`langspan recall WORKSPACE QUERY [--path PATH] [--limit N] [--json]` and the
read-only MCP tool `langspan_recall` (`workspace_id`, `query`, optional `path`
and `limit`) search the notes a workspace holds (at most 64, from core state).
A query is 1 to 200 bytes, split on whitespace into at most 8 words; a note is
found when every word appears, compared without case and as a substring, in
its text or its paths. With `path`, only notes about that file or about the
whole workspace count. Results are ordered by how many fields hold a query
word, then newest first; the limit is 1 to 50, default 10, and `truncated`
says more matched. The answer is `{workspace_id, query, notes: [{note_id,
author, text, paths}], truncated}`, held to 28 KiB by dropping the last
notes (and setting `truncated`). The MCP result carries it twice, as
structured content and as JSON text that escapes every quotation mark and
backslash again, so notes full of them can more than double it; the MCP
adapter encodes the whole tool result and drops further notes (setting
`truncated`) until it leaves 512 bytes of one 64 KiB frame for the JSON-RPC
envelope. The MCP result puts a label first saying note text is remembered repository text: data, not
instructions.

A recall is a read answered by the core: no event, no receipt, no delivery,
and no change to any briefing. Forgotten notes are not held, so they are not
found. There is no embedding, model call or network use. Daemon down is an
explicit failure (the CLI exits with status 1; over MCP it is the tool error
`daemon_request_failed`, as for every other tool); an unknown workspace is the domain error
`workspace_not_found`.

Notes have no tags or anchors yet (E1), so the proposed `--tag` / `tag`
argument and the `anchor` and `tags` fields are not offered; they arrive
with E1, which adds them as further fields to match. Not yet observed: a
real Claude Code session calling `langspan_recall` and acting on the result.

## Consequences

- At launch, messages and notes stay on one machine, and an idle session
  hears nothing until its host runs a hook.
- C9 is the first candidate after launch, since its prerequisite events are in
  the bar.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: C9 starts once tracks L2 and L3 deliver the end events; A15 once Codex is confirmed to honour added context on PostToolUse; A16, A17 and E9 each need a reviewed ADR for their new boundary; the rest follow runs that show agents missing what these would have told them.
