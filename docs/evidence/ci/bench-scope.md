# The benchmark dry runs off the merge queue's path

Evidence for moving the benchmark harness's dry runs (the `dry_run_*` tests
in `tests/benchmark_harness.rs`) out of `check.yml` into `bench.yml`
(epic #304; keeps the intent of #286). Before, a merge-queue commit whose
group touched a path in `scripts/ci-changes.sh`'s `BENCH_PATHS` ran them in
the required `Linux` check and in `macOS`. After, `check.yml` always skips
them, and `bench.yml` runs them for such a pull request or push to `main`,
every night and on demand, never in the merge queue.

## Before: sharded runs that ran or skipped them

Read with `gh run view` from the Rust checks runs between 2026-11-27 18:31
and 23:34 UTC (every merge-queue run since #483's shards merged at 18:24;
10 of the 30 pushes to `main`). GitHub's standard `ubuntu-24.04` and
`macos-26` runners, warm build cache. All times in seconds, medians with
max; n is runs timed. Run and skip aren't randomized: the changes differ
between the groups, and the sample is one afternoon.

| | merge queue, ran | merge queue, skipped | push, ran | push, skipped |
| --- | --- | --- | --- | --- |
| Runs (n timed) | 29 (26) | 13 (11) | 7 (7) | 3 (3) |
| Slowest Linux shard job | 199.5 / 234 | 169 / 204 | 193 / 278 | 147 / 163 |
| Slowest macOS shard job | 301 / 372 | 242 / 273 | 321 / 327 | 236 / 236 |
| Slowest Linux nextest run | 92.5 | 62.6 | 94.9 | 63.2 |
| Slowest macOS nextest run | 193.6 | 132.6 | 189.9 | 125.6 |
| `Linux` check ends after run creation | 441.5 / 500 | 425 / 473 | 420 / 543 | 405 / 432 |
| `macOS` check ends after run creation | 661.5 / 755 | 591 / 720 | 625 / 671 | 614 / 629 |

29 of 42 merge-queue runs (69 percent) ran them. Where they ran, the
slowest shard took about 30 s longer on Linux and 60 s on macOS (median
job time), and the `Linux` check, which the queue waits on, ended about
17 s later; `macOS`, which it doesn't wait on, about 70 s later. All 7
sampled pushes that ran them did so even where the queue had skipped them
for the same commit (for example push 46083486555, queue run 46083261467).

Per-test times, from two runs that ran them (46082674004 / 46080196723),
passes only, with failed tries in brackets:

| | Linux | macOS |
| --- | --- | --- |
| Sum of the 10 dry runs | 296.8 (314.6) / 274.3 (298.9) | 382.0 (499.2) / 518.3 (644.5) |
| Slowest, `langspan_arm_following_briefings` | 90.0 / 79.8 | 111.6 / 150.5 |

One or two dry runs failed a try and passed on retry in every run read
closely; on macOS a retry of `langspan_arm_ignoring_briefings` added 87 to
117 s to its shard. Removing them doesn't free their whole time from a
shard, since the other tests slow down while they share the CPU, so the
"skipped" columns, not a subtraction, give the time without them.

## After

Locally, on an Apple M-series laptop with one thread per core:

| | Tests | Wall time |
| --- | --- | --- |
| The harness's smoke test, every test but the dry runs (`check.yml` runs these) | 13 | 10.8 s |
| `bench.yml`'s command, the dry runs alone | 10 | 82.8 s |
| The whole suite, dry runs included | 2,189 | 196.7 s |

**Target** for the merge queue after this change: every run's slowest
shard at the "skipped" median above (about 169 s Linux, 242 s macOS)
instead of 69 percent of runs at the "ran" median, and no dry-run retry in
the queue. The GitHub Actions runs of this pull request (`bench.yml` on
both platforms, since it changes `tests/benchmark_harness.rs`) and of its
merge-queue commit (no dry runs) are the first measured after; add their
times here once they have run.
