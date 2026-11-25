# ADR 0025: Distribution, lifecycle and store features beyond the first release

**Status:** Decided 25 November 2026. Each row is "not now" or
"not building", as stated.

## Context

The launch bar includes a release pipeline (track L8 in the
[roadmap](../roadmap.md)): versioned releases with prebuilt
binaries, designed in the public launch checklist. LangSpan's transport is a
Unix socket ([ADR 0001](0001-local-coordination.md)) and its store is one
SQLite event log under an owner-only state directory. The capability review of
25 November 2026 lists the remaining distribution, lifecycle and store rows
below as Missing or Partial.

## Decision

| Row | Capability | LangSpan today | Decision and reason |
| --- | --- | --- | --- |
| I5 | Signed self-update and rollback | None | Not building. Updates come through the release and the package manager. A binary that replaces itself would make a network call the docs do not otherwise need |
| I6 (rest) | Windows, `npx`, plugin marketplaces | Source build; Unix sockets only | Not now. Windows needs a named-pipe transport and a process probe; marketplaces follow a first release |
| I10 | Setup and hook installation through MCP tools | CLI only | Not building. Installation changes the owner's host configuration, so it stays an owner command, as with `remote enable` ([ADR 0006](0006-remote-awareness-through-git-refs.md)) |
| G6 | Move and remove workspaces | `workspace add` only | Not now |
| J7 | Retention limits for the event log and publications | No retention policy | Not now. Retention needs compaction that keeps derived state correct |
| J9 | A hash-chained ledger with replay | Append-only events, no chain | Not now. The store has one writer and is owner-only; a chain would show tampering, not prevent it |
| J12 | Liveness on macOS | Process probe on Linux only, plus 30 minutes of activity | Not now. Activity age covers macOS until a probe is built |

## Consequences

- The first release targets the platforms the release pipeline builds; Windows
  is not one of them.
- An upgrade is a new binary from the release; hooks that name a binary path
  are handled by the pipeline's upgrade path, not by self-update.
- Event logs grow without a bound until J7 is built. The
  [usage guide](../usage.md) already says events and receipts have no
  retention policy.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: Windows follows demand and a named-pipe transport design; G6, J7 and J12 are expected soon after the first release; J9 follows a threat that a chain would answer. I5 and I10 are not planned.
