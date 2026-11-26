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
| J7 | Retention limits for the event log and publications | `langspan compact` and `retention_days` | Built ([issue #90](https://github.com/Langspan/LangSpan.dev/issues/90)): compaction into a checked snapshot, below |
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
  another user is refused and recorded `unknown`; a failed listing (one that
  times out, is killed, or exits unsuccessfully, except `ps`'s own exit 1
  with nothing listed when none of the asked-for processes exists) records
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
- Event logs grow without a bound unless the owner compacts them or sets
  `retention_days`; with it unset nothing is ever deleted.

## J7: retention, as built

Decided and built 25 November 2026.

- **Snapshot, then delete, in one transaction.** `langspan compact
  [--keep-days N] [--apply]` (default 90 days, accepted 7 to 3650) writes the
  derived state at sequence N into a `snapshots` table and deletes the events
  at or below N recorded before the window, and the receipts recorded before
  it. Only the newest snapshot is kept. The daemon starts from it and replays
  only the events after N. Without `--apply` it deletes nothing and reports
  the counts, the sequence range, the snapshot sequence and the bytes the
  removed records hold. `config.json`'s `retention_days` makes the daemon do
  the same when it starts and then once a day; unset, nothing is deleted.
- **Ages are wall-clock.** Every event and receipt now records its
  wall-clock time (`recorded_at_ms`). The daemon's monotonic clock starts
  again at each boot, so it cannot date a record across days. Records
  written before this build have no time of their own and count from the
  upgrade. A wall clock set back keeps records longer; one set forward removes
  them sooner, but never below what the snapshot holds.
- **The snapshot is checked before anything goes.** The snapshot is written
  to JSON and read back, the events after N are applied to it, and the result
  must equal the state the daemon holds, field for field. Otherwise the
  compaction aborts with `snapshot_mismatch`, logs the fields that differ and
  deletes nothing. A crash before the commit leaves the previous log.
- **N never cuts inside open work.** N stops before the event that opened
  any admission, push turn, waiting landing or unacknowledged message still
  open; the report names it (`held_by`). An unacknowledged message therefore
  holds retention until it is acknowledged or leaves the workspace's 64.
- **The clock anchor of what is left stays.** A daemon start writes
  `booted` and `clock_anchored`, by which `today` places a generation's
  events on a day. A compaction keeps the start of the generation of the
  oldest event it leaves (or the newest start, if it leaves none) and every
  start after it, even when they are older than the window; older starts go.
  The snapshot also records the derived fields the state leaves out of every
  reply (the clock anchor and the recent refusals of the status line).
- **Retries and publications.** A retry whose receipt was removed is a new
  request, which is why the window is at least 7 days. `remote sent` keeps
  every `remote_published` record within the window and says from which date
  it is complete (`since_ms`).
- **Schema marker 4.** Version 4 adds the time columns and the `snapshots`
  table. An older build refuses the file, as it refuses any marker above its
  own, instead of replaying events without the snapshot they follow. Like
  events, a snapshot this build cannot read stops the store opening
  ([ADR 0015](0015-version-tolerance.md)): it is the state. The wire protocol
  stays version 3: `compact` and the `since_ms` field are additions.
- **Cost.** A compaction replays the events after the previous snapshot once,
  in the daemon's single writer, which waits meanwhile. Measured once on an
  arm64 macOS laptop, release build, a log of 500,000 small events (73 MiB):
  start with full replay 220 ms, dry run 254 ms, applied compaction 633 ms,
  start afterwards under 1 ms. Larger or more varied events take longer; a
  request that waits past its 5 s timeout is answered as unavailable and can
  be sent again.
- **Not included.** SQLite reuses the freed pages; the file is not shrunk
  (no `VACUUM`). Exporting removed events, the source index and remote refs
  are out of scope.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: Windows follows demand and a named-pipe transport design; G6 is expected soon after the first release; J7 and J12 are built, as recorded above; J9 follows a threat that a chain would answer. I5 and I10 are not planned.
