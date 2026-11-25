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
| E2 | `recall` search | None | Not now |
| E4 | Messages addressed by path and symbol | To one session or all | Not now |
| E7 | Sender signed automatically | `--from SESSION` | Not now |
| E9 | Messages across machines | Local only | Not now. Could ride the Git-ref transport (ADR 0006), which would widen its payload and needs that ADR's review |
| D10 | `list_activity` and an MCP activity resource an agent can read (added by the second review) | MCP tools `langspan_status`, `langspan_changes`, `langspan_events` | Not now. The tools answer the same reads; a resource adds a surface no host has been shown to use |
| E11 | Retract a sent message, telling readers to disregard it (added by the second review) | None | Not now. Needs a withdrawal event that every briefing and the recap honour; no run has shown the need |
| E12 | Delivery and seen receipts the sender can ask for (added by the second review) | Acknowledgements are kept and counted on the local page | Not now. The data is partly there; a sender query follows E11, since both read the same message state |
| E13 | Show a shortened message in full (added by the second review) | None | Not now. Messages are capped at 400 bytes when sent, so a delivered line is cut only when quoting takes it past 700 bytes |

## Consequences

- At launch, messages and notes stay on one machine, and an idle session
  hears nothing until its host runs a hook.
- C9 is the first candidate after launch, since its prerequisite events are in
  the bar.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: C9 starts once tracks L2 and L3 deliver the end events; A15 once Codex is confirmed to honour added context on PostToolUse; A16, A17 and E9 each need a reviewed ADR for their new boundary; the rest follow runs that show agents missing what these would have told them.
