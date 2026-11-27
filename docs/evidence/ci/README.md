# CI times and the release gate, measured

Evidence for #305, #308, #309 and #315 (epic #304): how long the Rust checks
take now that they run in two lanes with a build cache, how far that is from
the targets, and a dry run showing that a broken release binary stops the
release before `publish`.

## Push to result and job times

Measured on 2026-11-27 at 14:36 UTC with

    bash scripts/measure-ci.sh --since 2026-11-27T04:06:11Z --until 2026-11-27T15:00:00Z --logs 20 --rows docs/evidence/ci/rows

The window starts just after #376 (nextest, two lanes, build cache) merged
at 04:06:10 UTC. It ends when the script ran, so rerun with
`--until 2026-11-27T14:36:00Z` to get the same runs. Runners: GitHub's
standard `ubuntu-24.04` and `macos-26`. 215 of the 255 runs in the window
were completed first attempts that passed or failed and are counted; the
other 40 were cancelled or reruns. One row per run is in
[rows/runs.tsv](rows/runs.tsv) and one per job in [rows/jobs.tsv](rows/jobs.tsv).

Push to result is run creation to the run's last update, so it includes the
time spent queued. The three Markdown-only runs, which built nothing, are
left out.

| Event | n | Median min | p90 min | Max min | Target | Before (#305, n = 28) |
| --- | --- | --- | --- | --- | --- | --- |
| pull request (fast lane) | 80 | 4.0 | 8.8 | 17.1 | under 3 | 27.6 median, 31.2 p90 |
| merge queue (full lane) | 106 | 18.5 | 25.3 | 31.1 | under 8 | none (no merge queue) |
| push to main (full lane) | 26 | 26.3 | 35.9 | 41.5 | none | 27.6 (same as a pull request) |

Job time is job start to job end. Cache state comes from the job log, so it
is known only for the 20 newest pull-request runs and 20 newest
merge-queue runs whose logs were read. "Exact" means the entry `main` saved
under the job's own key, and "fallback" means an older entry found by
prefix. No log that was read showed a cold start.

| Event | Job | Cache | n | Median job min | Median `check.sh` step min | Median dependency crates compiled |
| --- | --- | --- | --- | --- | --- | --- |
| pull request | Linux | all | 80 | 3.7 | 3.1 | – |
| pull request | Linux | exact | 13 | 3.9 | 3.5 | 0 |
| pull request | Linux | fallback | 7 | 4.4 | 4.0 | 5 |
| merge queue | Linux | all | 106 | 9.5 | 8.8 | – |
| merge queue | Linux | exact | 13 | 10.1 | 9.2 | 0 |
| merge queue | Linux | fallback | 7 | 11.0 | 10.4 | 5 |
| merge queue | macOS | all | 106 | 18.3 | 16.4 | – |
| merge queue | macOS | fallback | 20 | 23.2 | 18.1 | 9 |
| push to main | Linux | all | 26 | 11.3 | 9.1 | – |
| push to main | macOS | all | 25 | 21.7 | 16.3 | – |

Before #376, the Linux job took a median of 9.5 minutes and macOS 16.7
minutes, run one after the other, on every pull request push (#305).

## What the figures show

- **The fast lane (#308).** The median is 4.0 minutes against a target of
  under 3, and the p90 is 8.8. The Linux job takes a median of 3.7 minutes,
  so the job alone is over the target before any queueing. Its `check.sh`
  step takes 3.1 minutes even with an exact cache hit that compiles no
  dependency crate, so what remains is compiling the workspace's three
  crates and running the fast tests. The gap between the 3.7-minute job and
  the 4.0-minute median, and the long p90 tail, is time spent waiting for a
  runner.
- **The full lane (#308).** The merge-queue median is 18.5 minutes against
  a target of under 8. macOS sets it: a median job of 18.3 minutes, against
  9.5 on Linux. The two jobs now start together (#305), so the merge queue
  waits only for the slower one.
- **The cache (#309).** On Linux, 21 of the 26 jobs read with an exact hit
  compiled no dependency crate, including every run created after 11:13 UTC.
  That meets the acceptance criterion. The other 5 were runs created
  between 10:24 and 11:13 UTC, the oldest whose logs were read, and compiled 5 crates (first `tokio`) despite the exact key;
  the logs don't show why. A fallback entry also recompiled 5, which added
  about 0.5 minute to a pull-request job and 0.9 to a merge-queue job. On
  macOS, all 20 merge-queue jobs read used a fallback
  entry and compiled 9 dependency crates (first `libc`). None found an
  entry under its own key, so the macOS cache is not working as intended.
  This measurement doesn't show why: the cache list wasn't read.
- **The release gate (#315).** See the dry run below.

## What would close each gap

- **Fast lane, about 1 minute over.** Two levers, neither tried here:
  - A larger Linux runner, set through the `LINUX_RUNNER` variable. This is
    billed and is the owner's decision.
  - A smaller fast test set. Move the slowest fast tests to the full lane
    or nightly; `ci-fast` in `.config/nextest.toml` selects them.

  Queueing can't be fixed from the workflow.
- **Full lane, about 10 minutes over.** macOS is the critical path. The
  options:
  - First, make main's macOS job save an entry the merge queue finds under
    its own key. That would remove the 9 recompiled crates and, judging by
    Linux, about 1 minute.
  - A larger macOS runner through `MACOS_RUNNER`, which is billed.
  - Run on macOS only the tests that depend on the platform, and leave the
    rest to Linux.

  Even the Linux job, at 9.5 minutes, is over 8, so the full suite's test
  time needs cutting as well (#308 lists `tests/graph.rs` at 266 s).
- **Cache.** Linux meets the criterion. macOS needs the save problem above
  found and fixed. Start with the save step's log on a push to main, and
  check whether `scripts/ci-cache-size.sh` stops the save.
- **Release gate.** Still to do: a real release run showing the
  demonstration on all four targets before publish, and again from the
  downloaded archives. Only a release can show that.

## Release gate dry run

[broken-binary-dry-run.sh](broken-binary-dry-run.sh) runs the build job's
"Version" and "Two-developer demonstration on the release binary" steps,
read from `.github/workflows/release.yml`. It runs them against the real
release binary, then against a wrapper that answers `--version` correctly
but crashes with SIGSEGV on `langspan hook`. Then it checks that `publish`
needs `build`. Run on 2026-11-27, macOS arm64, langspan 0.7.0 built with
`cargo build --release --locked --bin langspan`:

    $ bash docs/evidence/ci/broken-binary-dry-run.sh target/release/langspan
    target aarch64-apple-darwin, tag v0.7.0, steps from .github/workflows/release.yml
         | langspan 0.7.0
         | == 8. A coordinator restarts
         | == 9. The network goes away and comes back
         | == 10. A session that ended, and one that went quiet
    real binary: Version pass (want pass), demonstration pass (want pass)
         | langspan 0.7.0
         | == Setup: one repository, a shared remote, two developers with their own coordinators
         | == 1. Automatic discovery and awareness in one checkout (Claude Code and Codex)
    broken binary (crashes on hook): Version pass (want pass), demonstration fail (want fail)
    publish     needs: [release-please, build, sbom-attestation]: a failed build job leaves the release a draft
    build matrix      fail-fast: true

The old `--version` smoke test passes the broken binary. The demonstration
fails on it at the first hook call, and because `publish` needs `build`,
that failure leaves the release a draft. This is a dry run on one target,
not a release run.
