# Plan: m4-state-encap (issue #373, first slice)

Issue #373: stop code outside `langspan-core` from writing `State` fields.
Done when no assignment to a `State` field exists outside the core, the
replay test and golden stores pass unchanged, and the compiler (field
visibility) plus a ratchet row counting `State`'s `pub` fields at zero keep
it so.

## Size, and why this is a slice

`State` has 43 fields. Outside the core they are read or written about 410
times in 70 files (production code, `src/` unit tests, `tests/`, and the
core's own integration tests in `crates/langspan-core/tests/`, which are a
separate crate and lose `pub(crate)` fields too). About 80 of those are test
fixtures that build a `State` by inserting into its maps. Turning every
field private in one pull request means ~400 mechanical `.x` to `.x()`
edits plus a new way for those fixtures to build a state: well over the
300-line limit. So this pull request lands one independent slice and says
"Refs #373".

## This slice

1. **Every production writer outside the core goes through the core.**
   - `src/store/retention.rs`: `SnapshotRecord` (the stored snapshot: the
     state plus its 11 `#[serde(skip)]` derived fields) moves, unchanged in
     shape, to the core as `langspan_core::SnapshotRecord` with `of(&State)`
     and `into_state(self)`: retention's constructor in the core. Same serde
     attributes, so stored snapshots and the golden stores read unchanged.
   - `src/client.rs` `full_state`: `state.generation = generation` becomes
     `State::set_generation`.
   - `src/protocol.rs` `reshape`: `state.activity.values_mut()` becomes
     `State::activities_mut()`.
2. **The 11 derived fields become `pub(crate)`**, each but one with a read
   accessor: `clock_anchor`, `refusals`, `path_refusals`,
   `workspace_revisions`, `kept_tests`, `first_writes`, `write_runs`,
   `unsettled`, `renames`, `trails`. `conflict_waits` gets none: nothing
   outside the core reads it except the snapshot. Their outside readers
   switch to the accessor; the three test helpers that clear or copy them
   (`observation.rs` `authority`, `tool_identity.rs`, `tests/retention.rs`
   `assert_same_state`) go through `SnapshotRecord::of(..)` / `into_state()`
   instead, so their assertions keep the same strength.
   Five `State { generation: N, ..State::default() }` literals stop
   compiling once any field is `pub(crate)` (functional update needs every
   field visible): `src/mcp.rs` `hooked_state`, core `tests/status.rs`
   (three) and `tests/graph_context.rs` `world`. Each becomes
   `State::default()` followed by `set_generation(N)`; same value.
3. **`State` moves out of `lib.rs`** into a new `crates/langspan-core/src/state.rs`,
   with its accessors and `SnapshotRecord`. `lib.rs` is at exactly the
   1500-line limit, so it cannot take the new code. A mechanical move,
   checked with `git diff --color-moved`; `lib.rs` re-exports `State`.
4. **Ratchet row** `fields STRUCT N` in `scripts/ratchets.py`: the number
   of `pub` (not `pub(...)`) fields of `struct STRUCT` in that file may not
   grow; a drop asks for `--tighten`; `--record` keeps the row. Baseline
   `fields State 32` in `state.rs.tsv` now; the next slice takes it to 0.

Remaining for the next slices (listed under "Not done yet" in the PR): the
32 serialized fields, their ~360 outside readers, and a fixture path for
the tests that build a `State` by inserting into it (`src/notify.rs`,
`src/mcp.rs`, `src/daemon/...` unit tests, `tests/protocol.rs`, core
`status.rs`, `dashboard.rs`, `graph_context.rs` and others).

## Test-first order

1. `scripts/ratchets/test_ratchets.py`: tests for the `fields` row (grows ->
   failure, drops -> improvement, `--tighten` lowers, `--record` keeps it,
   `pub(crate)` fields don't count, a struct that's gone fails). Run, see
   them fail, implement in `ratchets.py`.
2. New `crates/langspan-core/tests/state_snapshot.rs` (in-process, no
   store): a replayed state with every derived field set survives
   `SnapshotRecord::of(&s).into_state()`, and its JSON matches the shape
   retention writes. Fails to compile until the core type exists.
3. Move `State` to `state.rs`; move `SnapshotRecord`; flip the 11 fields;
   add accessors and the two functions; fix callers the compiler names.
4. Ratchets: `--record` the new `state.rs` (and `fields State 32`),
   `--tighten` `lib.rs`, `retention.rs`, `client.rs`, `protocol.rs`,
   `registration.rs`; regenerate the core API golden (`api.txt`: adds
   `SnapshotRecord`).
5. Unchanged and passing: `tests/golden_stores.rs`, the replay/snapshot
   tests (`crates/langspan-core/tests/support/snapshot.rs` users,
   `properties.rs`), `tests/retention.rs`, `tests/cli_transcript.rs`.

## Shared contracts touched

- Core public API: adds `SnapshotRecord`, `State::set_generation`,
  `State::activities_mut` and 11 read accessors; 11 fields leave the public
  surface. No wire format changes: `State`'s serde is untouched and
  `SnapshotRecord` keeps its JSON shape (golden stores prove it).
- `scripts/ratchets.py` baseline format gains one row type, documented in
  `docs/development-process.md#ratchets` (other sessions have edited that
  file; the edit is a few lines in the ratchets section only).
