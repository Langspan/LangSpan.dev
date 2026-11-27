# Plan: speed up the benchmark harness's Python tests (#460)

Part of #311 (epic #304). Only test code and the test budget page change;
`driver.py`, `standin_agent.py` and every product file stay as they are.

## Before

Local, Apple M-series, debug binary, `cargo nextest run --test
benchmark_harness`, at `fc0c8fcf`:

| Rust test | Time |
| --- | --- |
| `driver_planning_oracle_and_refusals` | 40.4 s |
| `a_real_refusal_reaches_the_run_record` | 17.9 s |
| `standin_hooks` | 16.2 s |
| `runner_mechanics` | 6.3 s |
| `holder_wait_comparison` | 5.0 s |

CI (run 46008001615, from the issue): 35.1 / 43.6, 16.1 / 18.7,
16.0 / 18.8, 5.9 / – s on Linux / macOS.

## Changes

1. **Run the driver tests in parallel** (`test_driver.py`,
   `tests/benchmark_harness.rs`). Split `DriverUnitTests` into several
   classes by subject (planning and prompts, the tool guard, the oracles,
   the hook-log wrapper, repair and merge, records and the summary). The
   methods and their helpers move unchanged; only new `class` lines are
   added, checked with `git diff --color-moved`. Each class gets its own
   Rust test, filtered with `-k ClassName`, so nextest runs them side by
   side. The two `RefusalCollection` tests (one coordinator each) become
   two Rust tests the same way. Group boundaries are set from measured
   per-test times so no group is over about 8 s on the M-series laptop.
2. **Keep every Python test running.** A new in-process Rust test reads
   `test_driver.py` and fails, naming the class, when a `TestCase` class
   there has no Rust test that selects it. No process is started: it
   checks the wiring, not the harness. Written first and watched fail
   against the split before the Rust tests are added.
3. **Replace `time.sleep(3)`** in
   `test_the_hook_log_ends_the_hooks_children_at_its_bound` with a wait on
   the condition. Each hook writes its process group (`echo $$`), as the
   `StandinHooks` tests already do; after the wrapper returns the test
   waits, within a 10 s bound, until that group has no live process, then
   asserts no `late` file exists. A child that survives the wrapper now
   fails the test as soon as the bound passes (it did before only if it
   wrote within 3 s). Checked by breaking the group kill in a scratch copy
   of `hook_log_main` once and watching the test fail.
4. **Shorten the stand-in bound** in
   `test_a_hook_past_the_standin_bound_leaves_no_process_running`:
   `HOOK_SECONDS` 2 to 0.5 (it is a float timeout to `communicate`), and
   `test_a_hook_slower_than_its_host_timeout_still_decides_the_call`
   to a hook of `sleep 0.6` against a host timeout of 0.3 s, keeping
   `elapsed_ms >= 600` and `late`.
5. **Shorten the late-hook test**
   (`test_a_pre_tool_hook_that_starts_late_refuses_nothing`): keep the
   assertion that `install_hooks` writes a 5 s timeout, then set the
   copied settings' timeout to 1 s and the delays to 1.5 s (PreToolUse,
   past it) and 0.5 s (PostToolUse, within it). The test still asserts
   `late_hooks >= 1`, the edit admitted and no refusal. Checked by making
   the stand-in apply the edit at the host timeout (the #200 bug) in a
   scratch run and watching it fail.
6. **Record the results** in `docs/testing/test-budget.md`: before and
   after per Rust test, locally and from the pull request's CI runs
   (Linux full lane and macOS), cold and warm.

Order: 2 (fails), 1 (passes), then 3, 4, 5 each with its break-once
check, then 6. `cargo fmt`, clippy `-D warnings`, the full `cargo nextest
run` and the ratchets before pushing; a ratchet baseline is recorded only
if `tests/benchmark_harness.rs` crosses one.

## Changed during implementation

- The largest cost was none of the above: Python finished in about 14 s
  but the Rust test took 34 s, because the tool-guard test's orphaned
  `sleep 20` held stderr open. Fixed in the test (the agent `exec`s one
  process); `driver.py` is unchanged.
- Three classes, not six, were enough; `option` and `transcript` moved
  unchanged into `Temporary` so the split classes share them.
- Change 3 waits on the hooks' process listing (each names its `late`
  file) rather than an `echo $$` group file.
- The late-hook test asserts the installed 5 s timeout, then sets 1 s.

## Shared contracts touched

None: no protocol, CLI, golden file or daemon. `docs/testing/test-budget.md`
is shared with #459 and #456 (non-exclusive); I re-read it after merging
main and add one section.

## Acceptance criteria

- No benchmark harness test over 10 s in the full lane on Linux outside
  `dry_run_*`: checked from the pull request's merge-queue or main CI log;
  if no full-lane run is available before review, reported as not yet
  verified with the local times.
- Every Python test still runs in the full lane (change 2 guards it), and
  each changed timing test still fails when its timeout is broken (the
  break-once checks in 3 to 5).
