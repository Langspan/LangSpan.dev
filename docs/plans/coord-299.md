# Plan: coord-299, reserve the next number of a named counter

Issue #299 (and the narrower first slice of roadmap B16, #32). ADR 0017's
condition for B16, "a run shows two sessions taking the same number", was met
on 2026-11-26: five pull requests claimed protocol versions 6 to 10 and each
one that merged second renumbered. This is a coordination primitive, not the
orchestration layer of #384.

## Done when (from #299)

Two sessions in separate worktrees that each need the next number get
different numbers, and each one's briefing names the other's claim.

## Decisions (new ADR 0040, `docs/adr/0040-reserved-numbers.md`)

- **Resources: counters only.** A counter is a free-form key the caller names
  (`protocol-version`, `adr`, `migration`): 1 to 64 bytes of `[a-z0-9._-]`,
  so a key shown in a peer's briefing cannot carry quoting or instructions.
  Names (a file or identifier) are left for later.
- **The number.** `reserve SESSION KEY [--after N]` answers one more than the
  larger of `N` (the highest number the caller sees in its own checkout;
  default 0) and the highest number ever handed out for that key. LangSpan
  does not read repository files to find `N`: no new trust boundary.
- **Scope: the coordination domain**, one daemon and state directory, so every
  worktree and checkout registered with it. Not the cross-machine metadata.
- **Release.** A claim is shown while its session has not ended. A number is
  never handed out twice in the domain: the highest per key is kept after its
  holder ends, so a session that ends before its pull request merges cannot
  cause a reuse, and no takeover is needed. An abandoned number leaves a gap.
- **Retry.** The store's request-id replay returns the same number.
- **Advisory**, like intents: it holds and refuses nothing.
- **Bounds.** At most 256 keys; each key keeps its newest 16 claims (the
  highest is always kept). A briefing lists at most 8 peer claims, newest first.
- **ADR 0017** row B16 points at ADR 0040 for this slice.

## Wire (capability `reservations`, ADR 0037; no version bump)

- Command `reserve {session_id, key, after}`; event `reserved {reservation}`;
  outcome `reserved {reservation, held}` where `held` is the other live
  claims of the key.
- Briefing field `reserved`: live claims of other sessions, skipped when empty.
- A client without `reservations`: the briefing's `reserved` is stripped, the
  `reserved` event is withheld from event pages (`event_needs`), and the
  client refuses to send `reserve` to a daemon that lacks it (`fits_with`,
  `older_daemon` text). Frozen versions 3 to 10 keep their exact bytes.
- Status view leaves reservations out (briefings carry them), so the status
  shape is unchanged.
- Golden vectors: `reserve-command-v10.json`, `reserved-event-v10.json`,
  `reserved-outcome-v10.json`, `fixtures/capabilities/reservations/{with,without}.json`,
  and the handshake fixtures gain the name.

## Test-first order

1. `crates/langspan-core/tests/reservations.rs` (in-process, no daemon):
   two sessions in two workspaces of one repository reserve `adr` after 39;
   they get 40 and 41; the second's outcome names the first's; each one's
   briefing `reserved` names the other's claim and not its own. Then: a
   claim of an ended session leaves briefings but its number is not reused;
   `--after` above the highest wins; invalid key and too many keys are refused
   with errors that say what to do; replaying the events rebuilds the state.
   Watch each fail, then build `crates/langspan-core/src/reservations.rs` and
   wire it in `lib.rs`, `command.rs`, `route.rs` (the exhaustive
   `Command::route()` from #484), `event.rs`, `outcome.rs`, `authority.rs`,
   `status.rs`, `briefing.rs` (field, byte budget, signature, changes-only
   keys).
2. Protocol: vectors in `tests/command_vectors.rs`, `tests/event_vectors.rs`,
   `tests/outcome_vectors.rs`; a capability test in `tests/protocol.rs`
   (stripped without it, withheld event, `fits_with`). Then `src/protocol.rs`.
3. CLI: `tests/cli_transcript.rs` case and `tests/golden/transcript/coordination.txt`;
   help and options goldens. Then `src/cli/request.rs` (argument parsing
   moved there), `src/cli/help.rs`, `src/cli/options.rs`, `src/cli/render.rs`.
   `src/daemon/prepare.rs` and `src/text/redact.rs` pass an unlisted command
   through unchanged (`_ =>`), and a key is charset-bound, so neither changes.
4. Hook text: a render test in `src/hook/render.rs` (not `src/hook.rs`, which
   lay-b-ids-fs is editing) that a briefing's peer claim appears as one
   quoted, labelled line, then the rendering there.
5. Docs: ADR 0040, ADR 0017 row, ADR 0037 table,
   `docs/usage/try-the-coordination-loop-with-the-cli.md`,
   `src/skill.md`, `docs/roadmap.md` B16 line, `docs/IMPLEMENTATION_STATUS.md`,
   threat model line (peer-supplied key shown to other sessions: charset-bound).
6. Ratchet baselines recorded or tightened for each changed or new file.

## Re-checked against main after #213 (#476, #482, #497)

ADR 0040 is still the next free number (0039 is the last on main).
`rename_holds` (#476) is the template for a capability with no version: its
fixtures under `fixtures/capabilities/` and the handshake `request.json` and
`answer.json`. Vectors live in `fixtures/` at the root. dash-feed is editing
`crates/langspan-core/src/lib.rs`; this plan adds only a decide arm and an
apply arm there.

## Changed during the first implementation run

- **Scope is the repository, not the whole domain.** One daemon serves every
  repository on the machine, so a domain-wide `adr` counter would give a
  repository at ADR 5 the number 40. A counter counts across every worktree
  of one repository (`workspace_git.repository_id`), or one workspace
  outside Git. The Done-when case (separate worktrees) is unchanged.
- **`after` is a positional argument** (`reserve SESSION KEY [AFTER]`), so
  the per-command flag table in `src/cli/flags.rs` does not change.
- **The counters are a serialized `State` field** left out of the status
  view, so the compaction snapshot (`src/store/retention.rs`) keeps them
  without a change there.
- **Files added to the reservation**, found by compiling: the exhaustive
  matches in `crates/langspan-core/tests/replay.rs` (with its replay
  fixtures) and `crates/langspan-core/tests/decision_golden.rs` (with the
  admission golden steps), the three `Briefing` literals in `src/hook.rs`'s
  tests, `docs/tables/capabilities/reservations.txt` (generated ADR 0037
  table, from #486), and ratchet baselines of the other touched core files.

## Changed during the second run (after merging main at 18f4354c)

- The hook split (#501 and after) moved the `Briefing` test literals into
  `src/hook/briefing_text.rs`, `src/hook/code_briefing_text.rs` and
  `src/hook/placed_briefing.rs`, and the CLI option matrix's command forms
  into `src/cli/run/option_table.rs`. Each needs a one-line addition (and
  `briefing_text.rs` the hook render test), so they were added to the
  reservation with their ratchet baselines.
- `crates/langspan-core/src/lib.rs` was at the 1,500-line limit on main; the
  new command adds 11 lines there. Its baseline is recorded with that reason
  in the pull request.
- The briefing line is rendered in `src/hook/render.rs`; it also shows in
  `langspan brief`'s human output, pinned in the coordination transcript.

## Changed during the third run (after merging main at e245e5f2)

- #533 moved `State` out of `crates/langspan-core/src/lib.rs` into
  `crates/langspan-core/src/state.rs`. The `reservations` field (three
  lines) and the `Counter` import move with it, and its `fields State`
  ratchet goes from 32 to 33, so `state.rs` and its baseline were added to
  the reservation. The field is serialized, so `SnapshotRecord` there does
  not change.

## Changed during the fourth run (after merging main at a9dbdae8)

- `reservations` in `State` is `pub(crate)` and `Counter` crate-private, so
  the `fields State` ratchet stays at 32 and the core's public names grow
  by two (`Reservation`, `MAX_RESERVATION_KEYS`).
- Files outside the reservation, found by the suite after the merge:
  - `src/briefing/render.rs` and `scripts/ratchets/baseline/src/briefing/render.rs.tsv`:
    main moved `src/hook/render.rs` there (#526 and after); the briefing
    line this branch added moved with it, so only its baseline is recorded.
  - `tests/tolerant_responses.rs`: a `Reservation` is stored in the
    `reserved` event, so it keeps `deny_unknown_fields` and the two places
    in `reserved-outcome-v10.json` join `STRICT`.
  - `crates/langspan-core/src/error_code.rs`,
    `crates/langspan-core/tests/golden/error_codes.txt`, `docs/usage.md`:
    #367 (#612) made a committed list of error codes; the three this
    branch adds (`invalid_key`, `invalid_number`, `too_many_keys`) join it
    and the usage guide's table, and `reservations.rs` writes them through
    `ErrorCode`.

## Changed during the fifth run (after merging main at 6517267b)

- The briefing line moved into its own function, `place_reserved` in
  `src/briefing/render.rs`, so `render_peers_placed` grows by one line; the
  file's 36 new lines are recorded in its baseline.
- The three error codes were not yet in the committed list; they are now,
  with their usage-guide rows, and `reservations.rs` writes every code it
  sends through `ErrorCode`.

## Shared contracts touched

`Command`, `Event`, `Outcome`, `Briefing`, `State` (one field each, additive),
`CAPABILITIES`, the CLI dispatch and help, the hook's briefing text.

## Not in this slice (stays on #32)

Sequences defined by a directory and pattern, reading worktrees and the
target branch for the highest number, expiry by lifetime or by a file
appearing, the MCP tool `langspan_reserve`, names as resources, and
real-host evidence (a follow-up issue under #110).
