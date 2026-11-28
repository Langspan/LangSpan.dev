# Plan: split `src/store.rs` (part of #300, LAYOUT wave 2 C2)

`src/store.rs` is 3,358 lines, 2,442 of them production (ratchet baseline
`lines 2442`). LAYOUT.md places it at `store/{sql, scan, replay, reads,
messages, paging}`. `src/store.rs` is already the root of the `store`
namespace (it declares `mod retention;`, at `src/store/retention.rs`), so no
file changes namespace: nothing is `git mv`ed, `lib.rs` keeps `pub mod store;`
and needs no compatibility alias, and importers and tests don't change.

## What moves where

Every block is cut and pasted whole; `git diff --color-moved` must show only
moved lines, plus the edits listed under "Edits that aren't moves".

- `src/store/sql.rs`: the `macro_rules!` SQL fragments and the SQL constants
  built from them (lines 44-281 today: `GENERATION_MARKER_INDEX`,
  `ANCHOR_LOOKUP`, `INTENT_EVENT_INDEX`, `INTENT_EVENTS_BEFORE`,
  `COUNT_INDEXES`, `REFUSAL_ROWS`, `REFUSAL_EARLIER`, `HISTORY_INDEX`,
  `HISTORY_BEFORE`, `HISTORY_FOLLOWING`, `MOVES_INDEX`, `MOVES_LOOKUP`,
  `FORGOTTEN_INDEX`, `FORGETTING_LOOKUP`, `RETRACTED_INDEX`,
  `RETRACTION_LOOKUP`, `ANCHORS`, `GRANTS_AFTER`). The macros stay next to
  the constants that must match them, so each index keeps one owner with its
  lookup.
- `src/store/paging.rs`: `MAX_UNREAD_PER_PAGE`, `PART_OVERHEAD_BYTES`,
  `escaped_len`, `escaped_char_len`, `part_end`, `workspace_page`, `digest`,
  and `impl Store { status_page, status_records, events, event_page }`.
- `src/store/scan.rs`: `impl Store { record_scan }`,
  `CHECKOUT_RESTORED_FILES`, `observed_file_state`, `observed_branches`.
- `src/store/replay.rs`: `impl Store { replay }`.
- `src/store/reads.rs`: `ReportHistory`, `refusal_window_ms`, and
  `impl Store { denials, today, refused_window, since, newest_events,
  report_history_before, report, count, history, changes }`.
- `src/store/messages.rs`: `impl Store { sent, show_message, sent_event,
  retracted_after, message_status, first_kept_event }`.
- Stays in `src/store.rs`: the module docs, `IDENTITY_CHANGED`,
  `wall_now_ms`, `PAGE_BUDGET_BYTES`, `MIN_PAGE_BYTES`, `RETRY_LOOKAHEAD`,
  `HISTORY_BUDGET_BYTES` (these move later with LAYOUT's D1 time and D2
  protocol page constants, which haven't landed; moving them now would move
  them twice), `SCHEMA_VERSION` (read by `tests/versions_in_docs.rs`),
  `NEWEST_WRITER`, `release`, `StoreResult`, `Execution`, `Store`, `Tap`,
  `TAP_LIMIT`, `redact_event`, `shown_event`, and `impl Store { open,
  open_with_wall_clock, state, tap, take_tapped, refresh, execute, stored,
  execution, commit, commit_with_snapshot, recover_as_newest_writer,
  recover }`, and the whole `#[cfg(test)] mod tests`.

The write path stays one unit in the root, as the engineering standard
requires: `execute` -> `commit` -> `commit_with_snapshot`, with `open`'s
migrations and `recover`. `record_scan` (in `scan`) and `refresh` reach it
only through the root's private `commit_with_snapshot`/`commit`, which a
child module can call.

Expected production lines: `store.rs` about 700, `reads.rs` about 650, the
rest under 400 each. No file over 2,000.

## Edits that aren't moves

- Each new file gets a one-line `//!` doc and its own `use` lines (the names
  it uses from `langspan_core`, `rusqlite`, `std` and `super`), and the root's
  `use` list drops names it no longer uses.
- The root gains `mod sql; mod paging; mod scan; mod replay; mod reads;
  mod messages;` next to `mod retention;`, and imports what its own code
  uses (`sql::{..._INDEX, COUNT_INDEXES}`, `paging::workspace_page`).
- Visibility only, where an item is now used from a sibling or the root:
  the SQL constants become `pub(super) const`; `workspace_page`, and the
  methods the root's `execute`/`stored` call (`events`, `denials`, `since`,
  `changes`, `sent`, `show_message`, `message_status`) become `pub(super)`.
  Methods that were `pub` or `pub(crate)` keep it. `pub(super)` is not
  counted by the `pub` ratchet.
- The test module gains `use super::sql::*;` and
  `use langspan_core::HISTORY_EVENTS;` (the root no longer imports it), so
  its query-plan tests still name the same items. No test body changes.
- `mod sql;` carries `#[macro_use]` and comes before `mod retention;`:
  `retention.rs` uses `generation_marker!`, which now lives in `sql.rs`.

## Test-first order

A move adds no behavior, so there is no new failing test; the guard is the
existing suite passing unchanged:

1. `cargo build`, then `cargo nextest run -p langspan -E 'test(/store::/)'`
   and `--test store` (the store's unit and real-SQLite tests, including the
   `EXPLAIN QUERY PLAN` tests that pin each lookup to its index).
2. The golden, contract and property tests: `--test cli_golden`,
   `--test cli_transcript`, `--test protocol`, `--test versions_in_docs`.
3. `git diff --color-moved=dimmed-zebra` to confirm moves only.
4. Ratchets: `python3 -I -B scripts/ratchets.py --record` for each new file,
   `--tighten src/store.rs` (lines drop under the 2,000 limit, so the row
   goes; `record_scan`'s 269-line row moves to `scan.rs`), then `--check .`.
5. `cargo fmt --all`, both clippy runs, the full `cargo nextest run
   --workspace --locked` and `cargo test --doc --workspace --locked`.

## Shared contracts touched

None. No wire format, schema, CLI output, MCP tool or golden changes; the
schema marker stays `4` in `src/store.rs`; `lib.rs` and the namespace table
(`scripts/ratchets/namespaces.tsv`) are untouched. `scripts/coverage-baseline.tsv`
keeps its `src/store.rs` row (another session is editing that file); the
nightly report checks only the ten largest files, so the row may simply stop
applying once `store.rs` shrinks.

## Issue criteria

#300 "Done when" (no Rust file over 2,000 lines; features in different areas
don't touch the same file) needs the other splits too, so this PR says
"Refs #300" with what's still over 2,000 lines listed under "Not done yet".
