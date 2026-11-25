# ADR 0025: Distribution, lifecycle and store features beyond the first release

**Status:** Decided 25 November 2026. Each row is "not now" or
"not building", as stated, except G6, built and amended below.

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
| G6 | Move and remove workspaces | `workspace add`, `list`, `move [--switch]`, `remove [--apply]` | Built 25 November 2026 ([issue #75](https://github.com/Langspan/LangSpan.dev/issues/75)), ahead of the condition below; see the amendment. Not yet run on a host |
| J7 | Retention limits for the event log and publications | No retention policy | Not now. Retention needs compaction that keeps derived state correct |
| J9 | A hash-chained ledger with replay | Append-only events, no chain | Not now. The store has one writer and is owner-only; a chain would show tampering, not prevent it |
| J12 | Liveness on macOS | Process probe on Linux only, plus 30 minutes of activity | Not now. Activity age covers macOS until a probe is built |

**Moving, switching and removing a workspace (row G6), amended 25 November
2026.** G6 is built
([issue #75](https://github.com/Langspan/LangSpan.dev/issues/75)) as owner
commands of the CLI only, for every host's workspaces. There is no MCP tool:
moving or removing a workspace changes what every session is coordinated
against, so it stays an owner command, as with `remote enable`.

- `langspan workspace list [--json]` is a read (no receipt, no event): each
  workspace's ID and root, whether the root is a directory now (the daemon
  looks; the pure core cannot), its current sessions (this generation, not
  ended), notes, pending messages, check records, and whether an automatic
  check and remote sharing are on.
- `langspan workspace move ID NEW_ROOT --request-id ID` is for a checkout
  the person moved. The daemon first checks, read-only and within one
  second, that NEW_ROOT is the top of a Git checkout (`not_checkout_top`
  otherwise) and that the old root no longer exists (`old_root_exists`
  otherwise). With `--switch`, the old root must still exist
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
  checkouts; its repository grouping, scan outcome and checkout changes are
  derived again from that scan. The source index keeps the root it last
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
  interface notices, scan records, automatic check and teammates' received
  publications leave the state, so `status`, `recap`, briefings and the
  dashboard no longer show it. Its Git snapshot is deleted with the event.
  The state counts each ID's removals (derived on replay, not in the status
  reply), and every scan and index pass carries the count it was asked for
  under: the source index keeps the count beside the root and forgets the
  workspace's entries and baselines when either differs, and a scan asked
  for before a removal is not recorded after it. So an ID removed, changed
  while unregistered and registered again at the same root starts a new
  index baseline instead of reporting interface changes from its earlier
  lifetime. The event log keeps every earlier event (`langspan events`). A disabled,
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
LangSpan itself; the release does not wait for them. What would change it: Windows follows demand and a named-pipe transport design; J7 and J12 are expected soon after the first release; J9 follows a threat that a chain would answer. I5 and I10 are not planned. G6 is built; a real-host Claude Code run after moving a checkout is still to be retained.
