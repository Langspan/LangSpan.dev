# Plan: balance the CI test shards by measured time (#486)

Part of #308, epic #304. The ruleset is untouched (#487 is the owner's).

## Problem

The full lane's shards use nextest's `hash:K/N`, which spreads tests by
name and ignores their time. In queue run 46048404782 shard 3 took 3.5 min
(Linux) and 6.1 min (macOS) against 1.9–4.5 min for the others. The heavy
tests (benchmark dry runs at 30–162 s, `cli_golden`'s
`every_option_against_every_command` at 18–63 s, the 28–36 s coordinator
tests) land wherever their name hashes.

nextest 0.9.152 offers `hash`, `count` and `slice` partitions; none weighs
time. So the assignment comes from a checked-in timing list, applied with
nextest's own `-E` filtersets. No new tool.

## Design

- `scripts/ci-test-times.tsv` (new, data): per platform (`linux`,
  `macos`), the measured seconds of each test of 5 s or more, and the
  summed seconds and test count of each binary's other tests. Taken from the
  JUnit reports of a full-lane run with the dry runs on.
- `scripts/ci-shards.py` (new, standard library only):
  - `record --platform P JUNIT...` rewrites that platform's rows from a
    run's shard reports.
  - `assign --platform P --shard K/N [--no-dry-runs] LIST_JSON` reads
    `cargo nextest list --message-format json` of the extracted archive,
    builds units (each listed heavy test alone; each binary's remaining
    tests as one unit), weights them from the TSV (a binary or test the TSV
    lacks gets its test count times the platform's mean light-test time;
    without dry runs, the benchmark harness's `dry_run_` tests weigh 0),
    assigns them longest first to the lightest shard (ties by shard number,
    units ordered by weight then id, so every shard computes the same
    assignment), and prints shard K's filterset and its expected test count.
  - Every listed test is in exactly one unit by construction: a heavy test
    is `binary_id(=B) & test(=T)`, the rest of its binary is
    `binary_id(=B) & not (test(=T) | ...)`. A test added after the TSV was
    recorded goes with its binary's rest; a new binary is its own unit.
  - Bounds: list JSON and TSV sizes checked before reading; the output
    filterset is bounded (a few hundred terms, well under Linux's 128 KiB
    per argument).
- `scripts/test-foreign-home.sh`, shard stage: after extracting, run
  `cargo nextest list` on the archive, compute the filterset with
  `ci-shards.py assign`, list again with `-E` and fail (naming both counts)
  unless nextest selects exactly the expected count, then run with `-E`.
  A failure to compute denies: the shard fails rather than running all or
  nothing.
- `scripts/check.sh`: the shard lane takes
  `LANGSPAN_TEST_PARTITION=time:K/N` (replacing `hash:K/N`; message
  updated) and passes it and `LANGSPAN_BENCH_DRY_RUNS` through.
- `.github/workflows/check.yml`: both shard matrices set
  `time:${{ matrix.shard }}/${{ strategy.job-total }}`; comments updated.
- `.config/nextest.toml`: if 0.9.152 supports per-test `priority`, start
  the benchmark dry runs and `slow_` tests first, so a long test doesn't
  start last in its shard. Checked with `cargo nextest show-config` before
  relying on it; dropped from the change if unsupported.

## Test first

In `tests/ci_workflows.rs` (it runs the repository's CI scripts as child
processes, which is what these tests verify; it is in the fast lane):

1. `measured_shards_give_every_test_to_exactly_one_shard`: a fixture list
   (two binaries with heavy tests, one binary the TSV lacks, a test added
   after recording) and a fixture TSV; `assign` for K = 1..4 together cover
   every test once (the script's `--explain` JSON of test ids per shard),
   and the expected counts sum to the list.
2. `measured_shards_balance_the_recorded_times`: on the checked-in TSV and
   a list built from it, the heaviest shard's weight is within 20% of the
   mean on both platforms, with and without dry runs; `hash` order would
   not be (fails on a reverted assignment that ignores weights).
3. `measured_shards_are_the_same_on_every_shard`: two runs give identical
   output; units given in another order give the same assignment.
4. Changed assertions: the workflow's partition line (`time:`), check.sh's
   refused values (`hash:1/4` now refused, `time:0/4`, `time:1`) and its
   message. Not weakened.
5. Mutation: make `assign` ignore weights (round robin); test 2 must fail.
   Revert.

Then make them pass, then run the shard stage by hand against a local
archive (`cargo nextest archive`, extract, `assign`, list, run one shard).

## Measure

- Record the TSV from run 46048404782's `junit-linux-*` and
  `junit-macos-*` artifacts (needs `gh run download` approval).
- Three manual `gh workflow run check.yml --ref ci/ci-balance` runs with
  dry runs (warm), plus one cold. Per shard: job time and nextest's test
  time; slowest against mean, both platforms.
- Record before (hash, 46048404782 and the #483 runs) and after in
  `docs/evidence/ci/balance.md`, indexed from `docs/evidence/ci/README.md`.
- "Over 3 queue runs" in #486 can only be read after merge: the PR says so
  under "Not done yet" unless queue runs exist by then.

## Docs

`docs/development-process.md` (which tests run when: the shard partition)
and `docs/engineering.md` (the `hash:K/4` mention), plus how to refresh the
TSV.

## Shared contracts touched

The CI workflow and `check.sh`'s `LANGSPAN_TEST_PARTITION` value. No wire
format, protocol, CLI or daemon change. Required check names unchanged.
