# Plan: wire error codes as an enum, with a committed list (issue #367, M4)

## What exists today

- `CoreError { code: String, message: String }` (`crates/langspan-core/src/lib.rs`)
  and `Outcome::Error { code: String, message: String }` carry the code as a
  string. No list of codes exists.
- About 100 distinct codes are written as string literals at about 320 sites:
  258 calls of `validate::error("code", ...)` across 27 core files, and about
  60 in `src/` (the daemon's `error(id, generation, "code", ...)`, the index
  worker's `refuse("code", ...)` and `Err(("code", ...))` tuples, `CoreError {
  code: "code".into() }` in the store, retention and redaction, and the
  comparisons `code == "..."` in `src/client.rs`, `src/hook.rs` and
  `src/landing.rs`).
- `src/daemon.rs` drops the result of the scan-outcome write
  (`let _ = store.execute(&id, &command, &command, now)` after a scan).

The `(&'static str, String)` pairs from `Anchor::lost` in `src/daemon.rs`
(`state_directory_removed`, `state_directory_replaced`) and `Reply::none`
coverage values in the hooks are log reasons and hook coverage, not wire error
codes; they stay as they are.

## Decision: the wire fields stay strings

`CoreError.code` and `Outcome::Error.code` stay `String`. A client that reads a
code it doesn't know (a newer daemon) must still parse the reply and show the
message; a typed field would make that a decode failure, which is a behavior
change the issue doesn't ask for. The enum is the only way to *write* a code:
every constructor takes `ErrorCode`, and comparisons use
`ErrorCode::X.as_str()`. No wire, receipt, event or protocol version change.

## Size: two slices

All of it is about 320 one-token call-site lines plus the enum, its list, the
docs table and the ratchet: well over the 300-line limit, and the core half
touches 27 core files other tasks also edit. So:

**Slice 1 (this pull request, "Refs #367")**

1. `crates/langspan-core/src/error_code.rs` (new): one `macro_rules!` table,
   one line per code, generating `pub enum ErrorCode` (serde
   `rename_all = "snake_case"`), `ErrorCode::ALL`, `as_str()`, `Display` and
   `From<ErrorCode> for String`. `lib.rs`: `mod error_code; pub use
   error_code::ErrorCode;`. Every code the daemon, store, client or core
   writes today is in it.
2. `crates/langspan-core/tests/golden/error_codes.txt` (new): the committed
   list, one code per line, sorted.
3. `src/`: every production string-literal code goes through `ErrorCode`:
   the daemon's `error()` takes `ErrorCode`; `refuse()` and the
   `(code, message)` tuples in the index worker, index queries and prepare take
   `ErrorCode`; the store, retention and redaction build `CoreError` with
   `ErrorCode::X.into()`; the client's version fallback and the hook and
   landing comparisons use `ErrorCode::X.as_str()`. Test fixtures that pin the
   wire string (`"code": "invalid_request"` JSON) stay literal on purpose:
   they are the wire vectors.
4. `src/daemon.rs`: the scan-outcome write logs a structured `warn` line
   (`scan_outcome_not_recorded`, workspace id, detail) to stderr when the store
   returns an error, instead of dropping it.
5. `docs/usage.md`: a "Error codes" section with a table of every code, what
   it means, and whether it is retryable (the same request can succeed later
   with no change: for example `workspace_busy`, `index_unavailable`,
   `storage_error`, `coordination_unavailable`, `read_timeout`,
   `workspace_changed`), with each code's class decided from the message the
   code is written with.

As landed, step 3 covers only the files in this task's reservation. Main
added `src/daemon/report_worker.rs` (#581) during the wait for admission; it
calls the daemon's `error()` with string codes, so `error()` takes
`impl Into<String>` until that file changes, and the codes it writes
(`report_busy`, `report_unavailable`) and `owner_grant_off` are in the enum
and the table. Core codes the daemon relays (`validate_request_id`'s) go
through `relayed()` unchanged.

**Slice 2 (next pull request, "Fixes #367")**

- `validate::error` takes `ErrorCode`; the 258 core sites change mechanically
  (`"invalid_event"` becomes `ErrorCode::InvalidEvent`, nothing else).
- The string codes left in `src/`: `src/daemon/report_worker.rs` (then
  `error()` takes `ErrorCode` again), and `src/store/messages.rs`,
  `src/store/paging.rs`, `src/store/reads.rs`, `src/store/replay.rs` and any
  other `CoreError { code: "..." }` or `code == "..."` the ratchet finds.
- `scripts/ratchets.py`: a count row of string literals equal to a committed
  code in production code outside `error_code.rs`, held at zero with no
  baseline rows (it lands when the count is already zero, so no baseline file
  of another task changes).

## Test first (slice 1)

1. `crates/langspan-core/tests/error_codes.rs` (new, in-process, no daemon):
   - `error_codes_match_the_committed_list`: the serde name of each
     `ErrorCode::ALL` entry equals its `as_str()`, the list has no duplicates,
     and it equals `tests/golden/error_codes.txt` line for line. Written first
     (fails to compile), then passes; delete one code once and watch it fail.
   - `usage_guide_lists_every_code`: the table in `docs/usage.md` names
     exactly the committed codes, each marked retryable or not.
2. `src/daemon.rs` unit test for the scan-outcome log is not cheap (the write
   only fails on a storage fault); the log line is checked by reading the
   change, and the existing storage-fault tests must stay green. Said so in
   the PR.
3. Unchanged and must pass: every golden wire vector (`tests/golden/`,
   `tests/protocol.rs`, `tests/command_vectors.rs`, `decision_golden`,
   `cli_transcript`), the daemon contract tests and the property tests.

## Shared contracts touched

- The core's public surface gains `ErrorCode` (ratchet baseline recorded for
  `error_code.rs`; `lib.rs` and the changed `src/` files tightened if they
  shrank, recorded with a reason if rustfmt reflow adds lines).
- The daemon's internal `error()` helper signature (`src/daemon.rs`) and the
  index worker's `refuse()`; no wire shape.
- `docs/usage.md` gains one section.
