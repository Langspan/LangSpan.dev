# Plan: t-bench-scope — benchmark dry runs off the merge-queue critical path

Epic #304. Keeps the intent of #286 (closed): the dry runs still run for a
change to the benchmark, briefing, hook or daemon code and every night, and a
nightly failure still opens or updates one issue.

## Today

- `scripts/ci-changes.sh` writes `dry_runs=run|skip` from `BENCH_PATHS`; the
  `check.yml` build jobs pass it to `scripts/check.sh` and the shards as
  `LANGSPAN_BENCH_DRY_RUNS`, so a merge-queue commit whose group touches a
  bench path runs eight 92–132 s dry runs inside the required `Linux` check
  and the `macOS` check (about 6 of 16 queue jobs).
- `dry_run_with_the_watcher_installed_a_session_takes_its_turn` calls
  `python_suite` directly, so it runs even when the others skip.

## Change

1. **New `.github/workflows/bench.yml`** ("Benchmark dry runs"), not a
   required check:
   - `pull_request` and `push` to main: runs `scripts/ci-changes.sh` (the one
     owner of `BENCH_PATHS`) and runs the dry runs only on `dry_runs=run`;
     otherwise the job passes at once and says why (no workflow-level `paths`
     filter, so the list stays in one place and the result always reports).
   - `schedule` (nightly) and `workflow_dispatch`: always run them.
   - No `merge_group` trigger: the queue never waits on them.
   - Linux and macOS matrix; `cargo nextest run --locked --profile ci
     --test benchmark_harness -E 'test(/^dry_run_/)'`, with Go/pytest not
     needed. Restores `check.yml`'s main build-cache entry read-only (prefix
     of the `linux`/`macos` job keys), saves nothing; installs only
     cargo-nextest via `scripts/install-ci-tools.sh`.
   - `permissions: {}`, job `contents: read`; a `nightly-report` job
     (`issues: write`, no checkout, fixed title "Nightly benchmark dry runs on
     main failed") opens or comments on one issue.
   - `concurrency` per workflow and ref, cancelling superseded PR runs.
2. **`check.yml`**: every `LANGSPAN_BENCH_DRY_RUNS` becomes the literal
   `skip` (build and shard jobs, Linux and macOS), and the `dry_runs` job
   outputs go. The nightly-report text says the dry runs moved to
   `bench.yml`.
3. **`tests/benchmark_harness.rs`**: the watcher dry run goes through
   `dry_run()`, so it skips with the others; module doc points at
   `bench.yml`. The non-dry-run tests (runner, driver units, hook-log
   wrapper, repair/summary, the two real-coordinator refusal tests, stand-in
   hooks, holder wait) stay in the full lane as the harness's smoke test;
   measure their time locally and in CI (target: well under the dry runs).
4. **`scripts/ci-changes.sh`**: comment only (who reads `dry_runs` now).
5. **Docs**: `docs/development-process.md#which-tests-run-when`,
   `docs/engineering.md` (the dry-run sentence in Commands),
   `docs/security/threat-model.md` (the new workflow's issue writer in the
   CI row), and `docs/evidence/ci/bench-scope.md` with the before/after
   timings.

## Test first (`tests/ci_workflows.rs`, string checks of the workflow files)

- `check_yml_never_runs_the_dry_runs`: every `LANGSPAN_BENCH_DRY_RUNS:` line
  in `check.yml` is `skip`. Fails today (it reads the job output).
- `bench_yml_runs_the_dry_runs_off_the_queue`: `bench.yml` has
  `pull_request`, `push` to main, `schedule`, `workflow_dispatch`, no
  `merge_group`; runs `scripts/ci-changes.sh`; gates the run on
  `dry_runs == 'run'` or the schedule/dispatch events; runs on Linux and
  macOS; the report job has `issues: write` and no checkout. Fails today (no
  file).
- Update the existing shard assertion that expects
  `needs.{build}.outputs.dry_runs` to expect `skip`.
- The `ci-changes.sh` decision tests stay unchanged (behaviour unchanged).
- Break check: point one `check.yml` line back at the output and confirm the
  first test fails.

## Measure

Before: `scripts/measure-ci.sh` / `gh run view` over recent queue runs that
ran the dry runs vs ones that skipped (Linux and macOS job times). After: the
PR's own bench.yml run (it touches `tests/benchmark_harness.rs`, so it runs
them) and the queue run of this PR (no dry runs). Record cold and warm
separately in `docs/evidence/ci/bench-scope.md`.

## Shared contracts touched

`check.yml` (also edited by ci-balance), `tests/ci_workflows.rs` and
`scripts/ci-changes.sh` (also edited by lay-a2-prep). No protocol, CLI,
daemon or golden output changes.
