# Lane times against the targets, and what runs only at night

Evidence for #308 (epic #304): each lane's median against its target since
the full lane was built once and sharded (#483), the time limits the lanes
now have, and which checks run only in the nightly run.

## Method

The window is every run of `check.yml` created from 2026-11-27T18:24:58Z,
when #483 merged, to 21:32:00Z. Counted: completed first attempts on a pull
request or a merge-queue commit that passed or failed. Runners: GitHub's
standard `ubuntu-24.04` and `macos-26` (arm64). The rows came from

    gh run list -R Langspan/LangSpan.dev --workflow check.yml --limit 300 \
      --created "2026-11-27T18:24:58Z..2026-11-27T21:32:00Z" \
      --json databaseId,event,status,conclusion,createdAt,updatedAt,attempt
    gh run view RUN -R Langspan/LangSpan.dev --json jobs

one row per run in [rows/lanes-runs.tsv](rows/lanes-runs.tsv) and one per
build or shard job that ran in [rows/lanes-jobs.tsv](rows/lanes-jobs.tsv).
`scripts/measure-ci.sh` wasn't used, because it wasn't approved to run in
this session; the figures are computed from these rows alone, so the
cache state and the Content workflow aren't in them.

Workflow completion is run creation to the run's last update, so it
includes time waiting for a runner. Four runs that finished in under a
minute built nothing (Markdown only) and are left out: 24 pull-request
runs and 27 merge-queue runs remain. One pull-request run and two
merge-queue runs failed; they are counted.

## Against the targets

| Lane | n | Median min | p90 min | Max min | Target | Met |
| --- | --- | --- | --- | --- | --- | --- |
| Fast (pull request), workflow completion | 24 | 3.8 | 4.6 | 4.7 | under 3 | no, 0.8 over |
| Full (merge queue), run created to the last Linux shard's end | 27 | 7.0 | 7.8 | 15.7 | under 8 | yes for Linux |
| Full (merge queue), run created to the last macOS shard's end | 27 | 10.5 | 11.9 | 14.0 | under 8 | no, 2.5 over |
| Full (merge queue), workflow completion | 27 | 10.8 | 12.1 | 15.8 | under 8 | no, 2.8 over |

The target asks for the full suite on Linux and macOS under 8 minutes, so
the full lane meets it only when macOS does. The Linux maximum of 15.7 is a
failed run whose shard retried its failing tests.

Job time is job start to job end:

| Lane | Job | n | Median min | p90 min | Max min | Time limit |
| --- | --- | --- | --- | --- | --- | --- |
| Fast | `Linux build` | 24 | 3.6 | 4.4 | 4.5 | 6 |
| Full | `Linux build` | 27 | 4.0 | 4.4 | 4.8 | 15 |
| Full | `macOS build` | 27 | 5.6 | 6.3 | 6.8 | 15 |
| Full | `Linux tests K`, passed | 105 | 2.8 | 3.3 | 3.9 | 15 |
| Full | `macOS tests K`, passed | 108 | 4.0 | 5.0 | 8.1 | 15 |

The three failed Linux shards took 9.8 to 11.2 minutes, retries included.
macOS finished after Linux in 25 of the 27 merge-queue runs: its build plus
its slowest shard took a median of 10.4 minutes, against 6.9 on Linux.

## Time limits

A build job stops at 6 minutes on a pull request and at 15 in the merge
queue; a shard stops at 15, and a build job at 30 on `main` and nightly.
Every job in the window fits with room to spare: the slowest pull-request
build took 4.5 minutes and the slowest merge-queue job 11.2.

A build that restores no cache doesn't fit 6, and can miss 15: cold
manual runs took 9m23s and 15m04s for the Linux and macOS build jobs
([shards.md](shards.md)), and 11m15s and 12m43s in another
([balance.md](balance.md)). Apart from GitHub evicting main's entry, only
a change of `rust-toolchain.toml` leaves a pull request or queue run with
nothing to restore, since the cache key names the toolchain, and a new cargo-nextest or cargo-audit version in
`scripts/install-ci-tools.sh` compiles that tool again. A job's
`timeout-minutes` can't read what the change touches, so such a pull
request raises the two limits in `check.yml` itself, and the next one
lowers them once `main` has saved the new cache
([which tests run when](../../development-process.md#which-tests-run-when)).

## What runs only at night

Every test still runs in the full lane or the nightly run. On a pull
request the fast lane runs nextest's `ci-fast` selection; the merge queue
runs every test exactly once across its shards (#483). What runs only at
night, or in the full lane only for a change that needs it:

- **The ten benchmark dry runs**, the `dry_run_*` tests in
  `tests/benchmark_harness.rs` (`worktrees_arm`, `awareness_only_arm`,
  `langspan_arm_ignoring_briefings`, `langspan_arm_following_briefings`,
  `hedging_fails_in_every_arm`,
  `a_changer_that_works_around_the_change_is_change_not_made`,
  `with_stagger_tells_dependents_before_their_first_write`,
  `with_the_watcher_installed_a_session_takes_its_turn`,
  `records_an_ignored_option`, `matrix_keeps_every_trial_and_resumes`).
  The full lane runs them only when the change touches the benchmark,
  daemon, hook, briefing or build paths `scripts/ci-changes.sh` lists; the
  nightly run always does.
- **The release build and the demonstration on the release binary**
  (`LANGSPAN_CHECK_RELEASE` is true only on the nightly schedule). The
  release workflow builds and checks its own.
- **Coverage**, which isn't a test but runs only nightly
  ([coverage](../../development-process.md#coverage)).

No other test was moved out of the full lane.

## What remains

- **Fast lane, 0.8 minute over 3.** The `Linux build` job alone takes 3.6
  minutes at the median, so no amount of queueing removes the gap. Either
  a larger runner through `LINUX_RUNNER` (billed, the owner's decision) or
  a smaller `ci-fast` selection in `.config/nextest.toml`.
- **Full lane, 2.5 minutes over 8 on macOS.** Linux meets the target. On
  macOS the build job takes a median of 5.6 minutes and the slowest shard
  about 4.8 more. A larger runner through `MACOS_RUNNER` (billed), a
  smaller macOS build, or running on macOS only the tests whose result
  depends on the platform would close it.
- **macOS isn't a required check.** The owner decided in #487 to keep it
  optional, so a queue commit can merge with the macOS half failed.
