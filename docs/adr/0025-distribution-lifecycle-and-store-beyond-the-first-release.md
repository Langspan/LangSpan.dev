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

**Liveness from recent hook runs (row J12), amended 25 November 2026.** J12 is
built ([issue #94](https://github.com/Langspan/LangSpan.dev/issues/94)):

- Every request a session makes at work (`langspan_core::acting_session`:
  each request a hook run sends for it, of any event, and the same requests
  from the CLI or MCP; not a `brief` without delivery, a read, or the end of
  a session) is followed in the daemon by a `session_seen` event carrying the
  session, the time and the generation, at most once per
  `SESSION_SEEN_INTERVAL_MS` (30 seconds) per session. A hook run that sent
  nothing else for the session (a shell command that writes no file by its
  parse, with no recognised host) sends `session_seen` itself; inside the
  interval it writes nothing, not even a receipt. It never registers a
  session, and it grants, renews and releases nothing.
- The event advances the session's `last_hook_ms`. A session is current
  when it is in this generation, not ended, not known to have exited, and the
  newer of `last_ms` (edits, prompts) and `last_hook_ms` is within
  `BRIEF_HORIZON_MS`. Briefings, publications to teammates, the status line,
  the dashboard and the bounded status view all use that one rule
  (`SessionActivity::last_seen_ms`).
- macOS has a process probe. LangSpan's own code stays free of `unsafe`, so
  instead of calling `proc_pidinfo` it reads the kernel's process table
  through `/bin/ps` with a clean environment, no input, a two-second deadline
  and a 1 MiB output bound, asking only for ID, parent, owner, state, start
  time and accounting name, never arguments or environment. The start token
  hashes the PID with the start time (one-second resolution; macOS hands out
  PIDs in sequence, so a PID is not reused within a second). A process of
  another user is refused and recorded `unknown`; a failed listing records
  `unknown`, never `exited`. The hook's ancestor walk lists only this user's
  processes in one run; the daemon's poll probes each page of up to 64
  bindings in one run, and does not re-probe a binding a hook reports that it
  saw alive less than 15 seconds before.
- `status` and `langspan_status` give `summary.last_hook_age_ms` per shown
  session, next to the probe result in `session_liveness`. No new command.
- Cursor and Gemini CLI process names are not recognised yet: they are added
  on both platforms once seen on a real host. Until then their sessions rely
  on hook runs alone.

A store holding `session_seen` is read by this build and later ones only: an
older build stops at the first, as for any event kind it does not know
([ADR 0015](0015-version-tolerance.md)). Activity recorded before
`last_hook_ms` existed reads as zero. Tests:
`crates/langspan-core/tests/hook_liveness.rs` (injected clocks: 40 minutes of
reads, 31 quiet minutes, a burst, a lease, a restart) and
`tests/hook_liveness.rs` (real daemon and hook processes: a burst of 100 hook
runs, a killed host process, another user's process, the status output).
A real Claude Code and Codex run on macOS where one host is closed is still to
be retained.

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
LangSpan itself; the release does not wait for them. What would change it: Windows follows demand and a named-pipe transport design; G6 and J7 are expected soon after the first release; J12 is built, as recorded above; J9 follows a threat that a chain would answer. I5 and I10 are not planned.
