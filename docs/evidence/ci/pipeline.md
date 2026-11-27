# Redundant and cold CI work, before and after

Evidence for the pull request that keys concurrency by event, lets `main`
restore and save its build cache, drops the release build from the merge
queue and `main`, and leaves the platform-independent checks to Linux
(#305, #307, #309; epic #304). Runners: GitHub's standard `ubuntu-24.04`
and `macos-26`.

Each row comes from `gh run view RUN --json jobs` through
`bash scripts/ci-step-times.sh RUN...`, one row per job, in
[rows/steps-before.tsv](rows/steps-before.tsv) and
[rows/steps-after.tsv](rows/steps-after.tsv). Times are in seconds. *Wait*
is the run's creation to the job's start; *tools* is the step that installs
cargo-nextest and the other checkers; *check* is the `scripts/check.sh`
step.

## Before: main at `075ee3a5` and earlier, 2026-11-27

Eight pushes to `main` (14:48 to 16:01 UTC) and four merge-queue runs
(16:05 to 16:19 UTC).

| Event | Job | n | Median wait | Median job | Median check | Ended |
| --- | --- | --- | --- | --- | --- | --- |
| push to main | Linux | 8 | 430 | 795 | 756 | 8 passed |
| push to main | macOS | 8 | 430 | 1,660 | 1,264 | 4 passed, 4 timed out at 30 min |
| merge queue | Linux | 4 | 3 | 501 | 463 | 4 passed |
| merge queue | macOS | 4 | 3 | 1,026 | 952 | 4 passed |

- **The wait.** Six of the eight pushes to `main` waited 409 to 564 s
  before any job started: the run shared the concurrency group
  `check-<sha>` with the merge-queue run of the same commit, and waited
  for it to end. The two that didn't wait (2 and 3 s) had no queue run
  left in that group.
- **The tools.** All four macOS timeouts spent 344 to 550 s installing the
  tools, because the cargo-audit and cargo-nextest caches missed and both
  were compiled. The four that passed took 1 s for the step. The entry
  they lacked is saved only by a run that ends: a timed-out run never
  reached the save, so the next run missed it again.
- **The release build.** In run 46029986247, the release build after the
  tests took 3 min 40 s on Linux and 4 min 27 s on macOS, and the second
  demonstration on it 11 s and 19 s. Nothing reads that binary; the
  release workflow builds its own.
- **The platform-independent checks on macOS.** In merge-queue run
  46031003210, format, cargo-deny, cargo-audit and cargo-machete took
  14 s on macOS and the docs 13 s: about 27 s of a 1,025 s step, run again
  after Linux had run them on the same commit.
- **The tests themselves** dominate what is left: 701 s of that step
  (2,099 tests in 140 binaries). This pull request doesn't change them.

## After

Measured on this branch with manual runs (`workflow_dispatch`), one with
the build cache (`cold` unset) and one without (`cold` set). A manual run
takes the full lane on both platforms with the merge queue's settings and,
since this change, no release build.

### Warm: run 46035930668 at `07b956a2`, 2026-11-27 16:43 UTC

Both jobs restored an entry under their exact key, and compiled only the
workspace's three crates (`langspan`, `langspan-core`, `langspan-graph`),
no dependency crate.

| Job | Wait | Job | Cache | Tools | Check | Tests (nextest summary) | Before, queue median job / check | Before, main median job / check |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Linux | 3 | 505 | 2 | 1 | 472 | 305 s, 2,109 passed, 7 skipped | 501 / 463 | 795 / 756 |
| macOS | 3 | 755 | 6 | 0 | 714 | 541 s, 2,106 passed, 6 skipped | 1,026 / 952 | 1,660 / 1,264 |

- **Linux** is unchanged against the queue, as expected: the queue already
  skipped the release build, and Linux keeps every check. Against a push
  to `main`, the run no longer waits behind the queue run (430 s median
  before) or builds the release binary (about 3 min 51 s with its
  demonstration).
- **macOS** took 271 s less than the queue median. About 27 s of that is
  the portable checks left to Linux. The rest is the exact cache hit (the
  queue runs before compiled 9 dependency crates from a fallback entry)
  and test time, which varies from run to run: 541 s here against 701 s
  in run 46031003210. One run can't separate the two. Against a push to
  `main`, it also drops the 430 s wait and the release build (about
  4 min 46 s).

A manual run has the settings of a push to `main` except that it saves no
cache, so these stand for `main` too. The figures from `main` itself come
after this merges.

### Cold

Run 46035934855, with `cold` set, was still running when this was
written; its rows are added when it ends.

## What the targets need beyond this

The targets are PR feedback within 90 s, the full merge-queue verification
within 5 min on Linux and macOS, full Linux under 3 min warm and full
macOS under 5 min warm. After this change the warm full lane is still
mostly test time: a 472 s Linux check step (305 s of tests) and a 714 s
macOS check step (541 s of tests) in the warm run above, against 180 s
and 300 s for the whole job. Closing that needs the
slow tests shortened or sharded, or larger runners, which other tasks of
epic #304 work on. The cargo-nextest and cargo-audit builds remain until
both install as prebuilt binaries with pinned digests; this change didn't
do that (see the pull request).
