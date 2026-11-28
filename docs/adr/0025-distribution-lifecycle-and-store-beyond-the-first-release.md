# ADR 0025: Distribution, lifecycle and store features beyond the first release

**Status:** Decided 25 November 2026. Each row is "not now" or
"not building", as stated, except G6 and J12, built and amended below, and
I5's version check, built below.

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
| I5 | Signed self-update and rollback | `doctor` compares the coordinator's version with each hook's binary | Self-update not building. Updates come through the release and the package manager. A binary that replaces itself would make a network call the docs do not otherwise need. The version check is built ([issue #84](https://github.com/Langspan/LangSpan.dev/issues/84)); see below |
| I6 (rest) | Windows, `npx`, plugin marketplaces | Source build; Unix sockets only | Not now. Windows needs a named-pipe transport and a process probe; marketplaces follow a first release |
| I10 | Setup and hook installation through MCP tools | CLI only | Not building. Installation changes the owner's host configuration, so it stays an owner command, as with `remote enable` ([ADR 0006](0006-remote-awareness-through-git-refs.md)) |
| G6 | Move and remove workspaces | `workspace add`, `list`, `move [--switch]`, `remove [--apply]` | Built 25 November 2026 ([issue #75](https://github.com/Langspan/LangSpan.dev/issues/75)), ahead of the condition below; see the amendment. Not yet run on a host |
| J7 | Retention limits for the event log and publications | `langspan compact` and `retention_days` | Built ([issue #90](https://github.com/Langspan/LangSpan.dev/issues/90)): compaction into a checked snapshot, below |
| J9 | A hash-chained ledger with replay | Append-only events, no chain | Planned since 28 November 2026 ([ADR 0043](0043-coordination-protocol.md#the-audit-log), [issue #91](https://github.com/Langspan/LangSpan.dev/issues/91)): a standard protocol needs a record a person or a second implementation can check. It was "not now" because the store has one writer and is owner-only; a chain still shows tampering and does not prevent it |
| J12 | Liveness on macOS | Process probe on Linux only, plus 30 minutes of activity | Not now. Activity age covers macOS until a probe is built |

**Moving, switching and removing a workspace (row G6), amended 25 November
2026.** G6 is built
([issue #75](https://github.com/Langspan/LangSpan.dev/issues/75)) as owner
commands of the CLI only, for every host's workspaces. There is no MCP tool:
moving or removing a workspace changes what every session is coordinated
against, so it stays an owner command, as with `remote enable`.

- `langspan workspace list [--after ID] [--json]` is a read (no receipt, no
  event): each workspace's ID and root, whether the root is a directory now
  (the daemon looks; the pure core cannot), its current sessions (this
  generation, not ended), notes, pending messages, check records, and whether
  an automatic check and remote sharing are on. It is paged so that any
  number of workspaces can be read: the core gives at most
  `MAX_WORKSPACE_PAGE` (100) in ID order after the cursor `after`, a
  workspace ID, and the store ends a page sooner to keep it within the same
  byte budget as a `status --all` page (a root is at most 4096 bytes, so a
  page always holds at least one). `next` is the last listed ID when more
  remain. A cursor is a position in ID order, not a reference, so one whose
  workspace was removed meanwhile still continues after it. No MCP tool lists
  workspaces.
- `langspan workspace move ID NEW_ROOT --request-id ID` is for a checkout
  the person moved. The daemon first checks, read-only and within one
  second, that NEW_ROOT is the top of a Git checkout (`not_checkout_top`
  otherwise) and that the old root no longer exists (`old_root_exists`
  otherwise). An old root that still resolves to NEW_ROOT, such as a
  symbolic link left where the checkout used to be, counts as moved; when
  the old root resolves to another directory through a link, the refusal
  names that directory. Every comparison of a registered root with a
  resolved one (the scan, a session's registration, admission) resolves
  links and also requires the same device and inode, so a link never turns
  one checkout into two, and two distinct checkouts are still told apart.
  Since a link resolves every path under it, a scan reached through a
  changed path also requires the checkout to hold the `HEAD` commit the
  previous scan saw; a link to another repository stops the scan as an
  identity change. The registered root is kept as given until a move
  records a new one. With `--switch`, the old root must still exist
  (`old_root_missing`) and its `HEAD` commit must be in NEW_ROOT's object
  database (`git cat-file -e`; `different_repository` otherwise); the old
  root then stops being coordinated under the ID. `--switch` is refused
  (`root_derived_id`) for an ID that user-level hooks derived from the old
  root: every new session there would derive and claim it again and be
  refused as `workspace_conflict`, leaving the still-existing checkout
  uncoordinated. Re-deriving a fresh ID for the old root instead would split
  its notes and history from it silently. The owner moves or deletes the old
  checkout and runs the move without `--switch`, or registers NEW_ROOT as a
  second workspace; a named ID (`workspace add`) can be switched. The same
  limit remains after a move without `--switch`: a new checkout later made
  at the old path derives the moved ID and is refused until it is
  registered under another ID. NEW_ROOT is
  then checked as
  `workspace add` checks a root: one registered under another ID, or
  overlapping one other than a verified linked worktree, is
  `workspace_conflict`. A refused move changes nothing and says which check
  failed. The core appends `workspace_moved` (the new registration and the
  old root): the ID, notes, pending messages, check records, landings, the
  automatic check and the remote setting stay, since they name
  workspace-relative paths. An automatic check waiting to run or running in
  the old root is cancelled when the move is recorded (as turning the check
  off cancels it), and a run that already ended is not reported: the worker
  asks the state owner, before it starts a run and before it records one,
  whether the workspace is still at the root the run was for. The setting
  applies to the next writes in NEW_ROOT.
- After a move, each session's recorded file versions in the workspace are
  dropped, so its next edit of an existing file has an unknown base until it
  reads the file again. The workspace's last Git snapshot is deleted in the
  same transaction as the event, so the next scan of NEW_ROOT starts a new
  baseline instead of reporting every file that differs between the two
  checkouts; its repository grouping, scan outcome, checkout changes and
  uncoordinated changes are derived again from that scan. The source index keeps the root it last
  indexed each workspace at and forgets a workspace's entries when the root
  changes, so a switch is not reported as interface changes. The line journal
  is keyed by physical root, so NEW_ROOT starts its own.
- The agent is told nothing new. A user-level hook from NEW_ROOT registers
  its derived ID, and the daemon answers with the moved workspace, as it
  already did for a root registered under another ID; the session joins the
  old ID. Project-level hooks name `--root` in host configuration, which the
  command never edits: it prints the `langspan hooks install HOST --workspace
  ID --root NEW_ROOT --apply` commands for the hosts whose project hooks, in
  NEW_ROOT or the old root, name this workspace and state directory. Until
  they are rerun, their events name the old root and are refused as
  `workspace_conflict`. With `--switch`, a session still running in the old
  root is no longer coordinated: its session ID stays bound to the moved
  workspace, so its next hook event is refused (`workspace_conflict` or
  `session_conflict`); a new session there registers the old root as a new
  workspace, as any unregistered checkout is.
- `langspan workspace remove ID [--request-id ID]` is a dry run, a read that
  lists what would end: current sessions (by ID, at most 32, the rest
  counted), intents, notes, pending messages and the automatic check.
  `--apply` (with `--request-id`) appends `workspace_removed`: the workspace,
  its sessions with their intents, admissions, activity, liveness and
  undelivered check results, and its checks, landings, notes, messages,
  interface notices, scan records, uncoordinated changes, automatic check, what its tests taught
  about flipping tests, teammates' received publications, its message
  history and the withdrawal notices its sessions were still owed leave the
  state, so `status`, `recap`, briefings and the dashboard no longer show
  it, and a session ID registered again is not told of an old withdrawal.
  Its Git snapshot is deleted with the event.
  The state counts each ID's moves and removals, its registration revision
  (derived on replay, not in the status reply). The ID, its root and that
  revision name one registration, so a switch to another checkout and back
  is a new registration too, and what a
  daemon worker learns from a registration carries it back to the state
  owner, which handles it only while the workspace still has that
  registration and otherwise refuses it (`workspace_changed`) unrecorded: a
  scan's result, each interface notice of an index pass, and an automatic
  check's reads of its setting and its check record. The source index keeps
  the revision beside the root and forgets the workspace's entries and
  baselines when either differs, or when it has no root record (an index
  written before roots were kept), so that workspace's first pass after the
  upgrade is a baseline. So an ID removed, changed while unregistered and
  registered again at the same root, switched to another checkout and back,
  or switched while an index pass of the old one was queued or running,
  starts a new index baseline instead of reporting interface changes from
  its earlier registration. An interface notice's request ID includes the
  root and the revision, so the same change of the same file in a later
  registration is a new notice, not a replay of the earlier one's receipt.
  Index answers (`graph`, code context, call breaks, the dashboard's
  coverage) read the index only when its root and revision match the
  workspace's current registration. Until the indexer starts that
  registration's index, or for good at a root it cannot scan, the answer is
  empty with `scanned: false`, never the earlier registration's symbols.
  Remote passes carry no registration: move and removal wait
  until publication is off and its ref withdrawn, which only the remote
  worker's own sequential pass completes, so no pass of it is in flight
  across either. The event log keeps every earlier event (`langspan events`). A disabled,
  withdrawn publication link stays, so the ID registered and published again
  continues its sequence for teammates. The ID can be registered again and
  starts empty; a hook event from the old root does exactly that.
- Move, switch and remove (dry run included) are refused while a session
  holds a current admission there (`workspace_busy`, naming the session),
  while a landing holds or waits for its push turn there (`workspace_busy`,
  naming the branch), or while remote sharing is on (`remote_sharing_on`,
  naming `langspan remote disable ID`, so the published ref is removed). Both
  also wait until a disabled publication's ref is gone from the remote, since
  the daemon removes it through the remote configured in the workspace's
  root: after a move it would ask the new root's remote, which need not be
  the one the ref was published to, and the ref would stay there.
- Both events are local only, never published. A binary from before them
  cannot read a store that holds one (it refuses the unknown event kind on
  start, [ADR 0015](0015-version-tolerance.md)).
- Failure modes: with the daemon down, nothing changes. A refused request is
  recorded with its receipt like any other refusal, so after the admission
  completes, the retry uses a new request ID.

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
- Cursor process names are not recognised yet (Gemini CLI support was removed,
  ADR 0033): they are added
  on both platforms once seen on a real host. Until then their sessions rely
  on hook runs alone.

A store holding `session_seen` is read by this build and later ones only: an
older build stops at the first, as for any event kind it does not know
([ADR 0015](0015-version-tolerance.md); what it reports is in
[ADR 0038](0038-store-rollback.md)). Activity recorded before
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

## I5: mixed versions, as built

Built 26 November 2026
([issue #84](https://github.com/Langspan/LangSpan.dev/issues/84)). Nothing
downloads, replaces or rolls back the binary; the owner upgrades through
Homebrew (`brew upgrade langspan`, once the tap is published) or a release archive checked with
`gh attestation verify` ([ADR 0031](0031-releases.md)), and rolls back the
same way or by keeping the previous archive. The earlier build reads the
store only until the later one records an event kind or field it doesn't
know; then it refuses with one line naming the build to start, or the owner
moves the state directory aside ([ADR 0038](0038-store-rollback.md), which
holds the compatibility policy).

- **The coordinator says its version.** A new read-only request, `version`,
  answers with the version the running daemon was built as and its process
  ID. It is answered by the daemon, never recorded and never replayed. It is
  an addition to wire protocol version 4; a daemon from before it answers
  with an error, which `doctor` reports as a build older than its own.
- **A binary's version is read, never run.** Every build carries one marker,
  `NUL langspan-binary-version=VERSION NUL`, in its read-only data.
  `doctor` reads each executable that an installed hook or MCP entry for its
  state directory names (project and user level, every hook event's own
  registration, since each can name a different file), at most 1 GiB in
  1 MiB pieces, and takes the version only when the file holds exactly one
  well-formed marker. It opens the file without blocking and reads it only
  when it is a regular file, so a FIFO, device or directory at that path is
  reported as unreadable instead of stalling the report. A project's own
  configuration can name that
  executable, so `doctor` does not run it: running `--version` would execute
  whatever a repository's settings point at. A file that holds no marker, or
  two different ones, has no version: a build from before this one, or not
  LangSpan.
- **What `doctor` reports.** One finding per executable, naming every hook
  and MCP entry that runs it: its path and version. When the coordinator
  reports a version and the executable's differs or is unknown, the finding
  needs attention and `doctor` exits 1. The text says which side is older and
  what to do: restart the coordinator through the login service (`launchctl
  kickstart -k` or `systemctl --user restart`) when `setup` installed one,
  otherwise `kill PID` and `langspan daemon`; or upgrade the binary the
  hooks name, or install them again from the coordinator's. A restart ends
  the leases sessions hold, so the text says to restart between agents'
  turns. `doctor` restarts nothing itself.
- **Homebrew.** Hooks and MCP entries name the stable
  `$(brew --prefix)/opt/langspan/bin/langspan`, so after `brew upgrade` they
  run the new keg while a coordinator started before the upgrade keeps the
  old one; `doctor` then names both versions until the coordinator restarts.

Tests: `tests/binary_versions.rs`, with a real daemon and a second binary,
a copy of this build whose marker is rewritten to a newer, then an older,
version (the copy is never run, so its broken code signature on macOS does
not matter); the same file without its marker; one hook event naming an
older binary than the others, and one naming a FIFO or a missing file, at
project and user level; `binary_version` unit tests
for a marker split across every read boundary, two markers, malformed
markers, a FIFO with and without a writer, a device, a directory, symlinks
and version order; and the `version` golden vectors
(`tests/protocol.rs`). Breaking the overlap between reads made the boundary
test fail. Still to retain: a real `brew upgrade` on macOS once the tap
exists, and the issue's test that runs every command with networking
blocked.

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
  reply: the clock anchor and the recent refusals of the status line, each
  workspace's registration revision, and the kept test results, first
  writes and runs of writes by which a check's failures are classified
  (ADR 0030).
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
LangSpan itself; the release does not wait for them. What would change it: Windows follows demand and a named-pipe transport design; G6, J7 and J12 are built, as recorded above; J9's threat was named on 28 November 2026 (ADR 0043). Self-update (I5) and I10 are not planned; I5's version check is built, as recorded above. A real-host Claude Code run after moving a checkout is still to be retained.
