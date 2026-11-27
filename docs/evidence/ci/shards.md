# Build once, test in shards: before and after

Evidence for #308 (epic #304): what building the tests once per platform and
running them in four parallel shards does to the full lane's time, measured
on GitHub's standard runners (`ubuntu-24.04`; `macos-26`, arm64) on
2026-11-27.

## Method

Each row is one GitHub Actions run of `check.yml`. The "before" rows run
the workflow as `main` had it at `fc0c8fcf`; the "after" rows run this
change at `8341b9f3` (the same code under test, plus this change's three
tests). The manual runs were started with
`gh workflow run check.yml --ref ci/ci-shard -f cold=…` and take the full
lane with the benchmark dry runs, as a merge-queue commit that touches the
benchmark or daemon code does. The queue run is the newest successful one
on `main` when this was measured. Times come from
`gh run view RUN --json jobs` (job and step start and end) and the job
logs (nextest's per-test lines and summaries).

"Warm" restores `main`'s build cache. "Cold" (`cold=true`) restores
nothing, so the run also compiles every dependency, cargo-nextest and, on
Linux, cargo-audit. Each row is a single sample, not a median: the
merge-queue median over 20 runs can only be read after this merges.

"Linux" and "macOS" below are the time from the run's creation to the end
of that platform's last job: the unsharded job before, the last shard
after. "Run" is creation to the run's last update.

## Results

| Run | Workflow | Cache | Linux | macOS | Run |
| --- | --- | --- | --- | --- | --- |
| [46039907276](https://github.com/Langspan/LangSpan.dev/actions/runs/46039907276) (merge queue) | before | warm | 9m08s | 16m15s | 16m15s |
| [46044043275](https://github.com/Langspan/LangSpan.dev/actions/runs/46044043275) (manual) | after | warm | 6m28s | 9m40s | 9m51s |
| [46042358881](https://github.com/Langspan/LangSpan.dev/actions/runs/46042358881) (manual) | before | cold | 13m17s | 23m33s | 23m34s |

### Where the warm time goes, after

| Platform | Build job | Archive upload | Shard setup to tests starting | Slowest shard's tests | Shards' tests (s) |
| --- | --- | --- | --- | --- | --- |
| Linux | 3m26s | 8 s | 60–80 s, 16–32 s of it extracting the archive | 91 s | 69, 91, 78, 56 |
| macOS | 5m26s | 7 s | 41–63 s, 5–6 s of it extracting | 181 s | 138, 181, 138, 125 |

Before, the same warm tests took 310 s on Linux (4 threads) and 671 s on
macOS (3 threads) in one job each (queue run 46039907276). The build job
now runs everything the unsharded job ran except the tests: format, the
dependency policy and advisories, unused dependencies (Linux), lint, the
docs (Linux), the test build, the doc tests and the demonstration.

Each shard ran its share once. Linux: 521 + 518 + 554 + 529 = 2,122 tests
of the 2,130 listed, 8 skipped by design as before. macOS: 519 + 555 +
517 + 528 = 2,119 of 2,126, 7 skipped. All passed. On Linux shard 1, two
tests in `tests/auto_checks.rs` failed their first try at
`f.prompt("p1").context().expect("delivered")` (lines 667 and 941) and
passed their second, so they were reported as flaky.

## Why four shards

The slowest single tests set a floor no partition can go under: the
benchmark harness's dry run `dry_run_langspan_arm_following_briefings`
took 80 s on Linux and 162 s on macOS, and its siblings 30–141 s. At four
shards the slowest shard is that floor plus about a minute (Linux 91 s,
macOS 181 s), so a fifth shard would save at most a few tens of seconds
and cost another runner's setup, and on macOS another of the runners a run
can hold at once. `hash` partitioning gives each test to a shard by its
name, so the shards' sizes differ by up to about 7 percent.

## Against the targets

| Target | Measured, after | Met |
| --- | --- | --- |
| Full Linux verification under 3 min warm | 6m28s | no |
| Full macOS verification under 5 min warm | 9m40s | no |
| Full merge-queue verification within 5 min | the `Linux` check ended 6m38s after the run's creation, `macOS` 9m50s | no |
| PR feedback within 90 s | unchanged: the fast lane is not sharded (about 4 min, README.md) | no |
| Pure unit and property tests under 15 s warm | not measured here | – |

What remains, in order of size:

- **The build job is now the critical path:** 3m26s on Linux and 5m26s on
  macOS warm, of which the test build alone took 2m05s and 2m44s before
  this change. Lint, format, the dependency checks and the docs could run
  in a job beside it instead of before the archive. Beyond that, only a
  faster build (a larger runner through `LINUX_RUNNER` / `MACOS_RUNNER`,
  billed, or a smaller test build) shortens it.
- **Linux shard setup:** extracting the archive took 16–32 s on Linux
  against 5–6 s on macOS, and installing the toolchain, Go and pytest
  another 25 s.
- **The slowest tests:** the benchmark dry runs (up to 162 s on macOS)
  bound every shard that holds one.
