# Plan: one exhaustive `Command::route()` (issue #371, M4)

## Today's routing, read from the three sites

- Core `decide_checked` (`crates/langspan-core/src/lib.rs`): the last arm names
  24 commands and refuses them with `storage_operation`.
- `Store::execute` (`src/store.rs`): a list of reads decided by the core and
  never recorded; arms for the commands answered from the event log; a `_` arm
  that records. `Store::replay` holds the same read list again (no receipt is
  looked up for a read).
- Daemon `answer` (`src/daemon.rs`): `Version`, then `CheckCalls`,
  `CheckWrite` and eight `Graph*` queries answered with the source index before
  the store is asked.

From these, each command's route:

| Route | Commands |
| --- | --- |
| `Internal` (answered by the coordinator about itself) | `Version` |
| `Index` (answered by the daemon with its source index; never decided by the core) | the ten `Graph*` commands, `CheckWrite`, `CheckCalls` |
| `Store` (answered by the store from the event log) | `Events`, `Today`, `Report`, `Count`, `History`, `StatusRecords`, `Changes`, `RemoteSent`, `Compact`, `ShowMessage`, `MessageStatus` |
| `Read` (decided by the core, never recorded) | `Status`, `Dashboard`, `StatusLine`, `RemoteOutbox`, `CheckStatus`, `FlakyTests`, `LandingReadiness`, `TurnQueue`, `WorkspaceList`, `WorkspaceRemove` without `apply`, `Recap`, `Trails`, `Policy`, `Holders`, `Recall` |
| `Decide` (decided by the core and recorded) | every other command, the five prepared adapter inputs and `WorkspaceRemove` with `apply` |

`OutlineRead`, `Brief` and `Remember` consult the index but are decided and
recorded by the core, so they are `Decide`; their index preparation in `answer`
stays as it is. `GraphExport` and `GraphImport` go to the indexer before
`answer` (`is_transfer`); `answer` keeps letting them fall through as today.

## Changes

1. `crates/langspan-core/src/route.rs` (new, like `authority.rs`): `pub enum
   Route { Decide, Read, Store, Index, Internal }` and `impl Command { pub fn
   route(&self) -> Route }`, one match naming every variant, no wildcard.
   `lib.rs`: `mod route; pub use route::Route;`.
2. `decide_checked`: an exhaustive `match command.route()` at the top returns
   the existing `storage_operation` error for `Store | Index | Internal`. The
   24-name arm has to stay for the decision match to stay exhaustive (a
   wildcard there would let a new decided command compile without a decision);
   it now returns the same error through one helper, and the PR says so.
3. `Store::execute`: `match original.route()`: `Read` (with `Status` and
   `Dashboard`'s post-processing kept), `Store` (the event-log arms, moved into
   one function), `Decide | Index | Internal` (the record arms, with the
   three skip-empty commands kept). `Store::replay`: its list becomes
   `matches!(original.route(), Read | Store | Internal)`.
4. Daemon `answer`: `match command.route()`: `Internal` answers `Version`;
   `Index` (not a transfer) dispatches to `checked_calls`, `check_write` and the
   graph queries; the rest continue as today. The index dispatch names
   `GraphSymbol | GraphReferences | GraphImpact` instead of its `_` arm.

No wire, receipt, event or outcome shape changes. No protocol version bump.

## Test first

1. `tests/command_vectors.rs`: `every_command_routes_as_recorded`: for each
   golden vector (every on-wire variant) plus `WorkspaceRemove` with `apply`
   and the five prepared inputs, the expected route from the table above.
   It also checks, against today's core, that `decide` refuses with
   `storage_operation` exactly the `Store`, `Index` and `Internal` commands.
   Written before `route()` exists (fails to compile), then passes; break one
   entry of `route()` once and watch it fail.
2. `tests/store.rs`: `store_records_and_replays_by_route` (real SQLite, since
   whether a receipt is written is persistence): each vector executed on a
   fresh store; a receipt is written exactly for `Decide`, `Index` and
   `Internal` commands that recorded today, and a second execution replays
   exactly those not `Read`, `Store` or `Internal`. Run against the old code
   first so it records today's behavior, then unchanged after.
3. Unchanged and must pass: `decision_golden`, `command_vectors`,
   `cli_transcript`, the daemon contract tests and the property tests.

## Shared contracts touched

The core's public surface gains `Route` and `Command::route` (ratchet
baseline recorded for `route.rs`; `lib.rs`, `store.rs` and `daemon.rs`
tightened if they shrank). The routing in `src/store.rs` and `src/daemon.rs`
(the daemon's request handler) changes form only.

## Left as they are (candidates in the PR)

`acting_session` (core), `reads_blind_in_scope` and `coverages` (daemon),
`protocol::fits`, the redaction arms in `src/hook/redact.rs`, `is_transfer`,
and `apply: bool` on `WorkspaceRemove`, `GraphImport` and `Compact`.
