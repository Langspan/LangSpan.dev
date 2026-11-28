# Plan: f-coverage (#325, #317, #327)

What is still open on each issue, from its latest comment:

- #325: `src/main.rs` at 90% line coverage. `main.rs` is now 79 lines; the
  CLI it calls is `src/cli/` (about 9,000 lines). The nightly baseline row
  (84.8%) predates #435 and #438.
- #317: line coverage of `apply` above 95%. `apply` is now a dispatcher in
  `crates/langspan-core/src/lib.rs` plus `apply_*` functions in the domain
  modules (`admission.rs`, `checks.rs`, `sessions.rs` and others).
- #327: `forward_transfer` (`src/daemon/index_worker.rs`), `serve_connection`
  (`src/daemon.rs`) and `prepare_move` (`src/daemon/prepare.rs`) each above
  85% line coverage. The `Full` and `Disconnected` branches of
  `forward_transfer` are reached by no test.

## Measuring

Rustup's `llvm-tools` component isn't installed here, so the figures come
from the coverage job's own exports on main (run 46084017995, commit
0e32e861, Linux): `report.md` for files, and the lcov `DA` lines over each
function's span for functions. This pull request changes
`scripts/coverage-baseline.tsv`, so the coverage job runs on it too and
measures the result.

Measured on main, before this change:

- `src/main.rs` 92.9% (26 of 28); `src/main.rs` with `src/cli/` 89.0%
  (4331 of 4865).
- `apply` 100% (148 of 148); `apply` with its 74 `apply_*` functions 88.9%
  (1480 of 1665). The missed lines are `invalid_event` guards that reject
  a malformed event.
- `forward_transfer` 73.5% (36 of 49), `serve_connection` 75.0% (69 of 92),
  `prepare_move` 92.7% (89 of 96).

What this slice leaves: `serve_connection` (needs a test in
`src/daemon.rs`) and the `apply_*` guards (a new
`crates/langspan-core/tests/` file), both outside this reservation.

## Steps

1. Measure: `bash scripts/coverage.sh target/coverage` (the nightly job's
   own script, `cargo llvm-cov` + `cargo nextest`). File figures come from
   `target/coverage/report.md`. Function figures come from
   `llvm-cov report -show-functions` over the same profile and binaries,
   for `apply` and every `apply_*`, and for the three daemon functions.
   Check: the report lists `src/main.rs`, every `src/cli/` file and the
   three daemon files.
2. #327, test first: an in-file `#[cfg(test)]` module in
   `src/daemon/index_worker.rs` calls `forward_transfer` in process, with no
   socket or daemon: a `State` with one registered workspace, a `Job` whose
   reply is a `oneshot`, and
   - a `sync_channel(0)` with no receiver waiting: the reply is
     `index_unavailable` with the "handling another export" message (`Full`);
   - a channel whose receiver is dropped: `index_unavailable` with the
     "index is not available" message (`Disconnected`);
   - an unknown workspace, a relative path, and a version the daemon doesn't
     answer, each with its code.
   Break each branch once (swap the two messages) and confirm a test fails.
   `serve_connection` and `prepare_move`: add tests only if step 1 shows
   them under 85%; they would then need a reservation update (see below).
3. #325: from step 1, list the uncovered lines of `src/main.rs` and
   `src/cli/`. Cover the error paths through the binary in a new
   `tests/cli_coverage.rs` only where what is verified is argv, exit status
   and stdout/stderr of the process; pure rendering is tested in process
   against `langspan::cli` instead. If reaching 90% needs in-file tests in
   specific `src/cli/*.rs` files, I write an updated reservation naming
   them and stop.
4. #317: report the measured figure. If `apply` plus its `apply_*`
   functions are under 95%, the gaps go into
   `crates/langspan-core/tests/replay.rs`, which needs a reservation update
   (and `crates/langspan-core/src/` is being changed by `m4-state-encap`
   now, so I check the briefing first).
5. Re-measure; raise the `scripts/coverage-baseline.tsv` rows that rose
   (`--write-baseline`, then keep only the rows already present plus any
   file now among the ten largest). Record ratchet baselines for the new
   test file and tighten `index_worker.rs`'s.
6. `cargo fmt`, `cargo clippy --workspace --all-targets --locked -- -D
   warnings`, full `cargo nextest run`, doc tests. Post each measured figure
   as a comment on its issue. PR: "Fixes" only for an issue whose every
   criterion holds, otherwise "Refs" with "### Not done yet".

## Shared contracts touched

None. No wire format, CLI output or golden file changes; tests only, plus
the coverage baseline (a nightly floor) and ratchet baselines.

## Size

Expected well under 300 changed lines: one in-file test module, at most one
new integration test file, baseline rows.
