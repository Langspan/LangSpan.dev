# Shards by measured test time: before and after

Evidence for #486 (part of #308, epic #304): what assigning the full lane's
test shards by measured test time (`scripts/ci-shards.py`, weights in
`scripts/ci-test-times.tsv`) does to how evenly the four shards finish,
measured on GitHub's standard runners (`ubuntu-24.04`, 4 threads;
`macos-26`, arm64, 3 threads) on 2026-11-27.

## Method

Each row is one GitHub Actions run of `check.yml`, full lane, benchmark dry
runs on. "Before" is nextest's `hash:K/4` partition: queue run
[46048404782](https://github.com/Langspan/LangSpan.dev/actions/runs/46048404782),
#483's first, and the manual run
[46044043275](https://github.com/Langspan/LangSpan.dev/actions/runs/46044043275)
from [shards.md](shards.md). "After" is this change, started with
`gh workflow run check.yml --ref ci/ci-balance` and restoring `main`'s build
cache (warm) unless marked cold.

- **Test time** is the `time` of each shard's JUnit report
  (`<testsuites time=…>`): nextest's own wall time for the shard's tests,
  retries included. The #486 criterion is measured on this: the slowest
  shard against the mean of the four.
- **Job time** is the shard job's start to end, and **download** its
  `actions/download-artifact` step (the test archive). Job time also holds
  the checkout, the toolchain, Go and pytest, the extraction and listing,
  so it is reported apart.
- "Platform done" is run creation to the end of that platform's check job.

Two weightings were run. The first (v1, commit `1eec9552`) weighed each
test from the one queue run 46048404782. Its two runs left Linux 23–25%
over the mean, with the same shard heavy each time: the benchmark dry runs
in it ran up to twice their recorded time. The second (commit `f98f816b`)
weighs each test by its mean over runs 46048404782, 46053197733 and
46054527495 (`record` given all three runs' reports), and is what this
change ships. Runs A, B and C below are three warm runs of it, and
"cold" one with `-f cold=true`, which restores no cache. The last commit
here, which fixes the threads the assignment assumes at 4 on Linux and 3
on macOS instead of the runner's CPU count, assigns the same tests on
these runners, so these runs measure it too.

## Test time per shard

Seconds, shards 1 to 4, then the slowest shard over the mean.

| Run | Assignment | Linux | Slowest | macOS | Slowest |
| --- | --- | --- | --- | --- | --- |
| 46048404782 (queue) | hash | 61.0, 90.1, 83.6, 54.5 | +24.6% | 115.3, 177.4, 248.0, 108.9 | +52.7% |
| 46044043275 (manual) | hash | 69, 91, 78, 56 | +23.8% | 138, 181, 138, 125 | +24.4% |
| [46053197733](https://github.com/Langspan/LangSpan.dev/actions/runs/46053197733) | v1, one run's times | 83.1, 73.7, 98.1, 58.5 | +25.3% | 202.0, 171.1, 166.7, 125.1 | +21.5% |
| [46054527495](https://github.com/Langspan/LangSpan.dev/actions/runs/46054527495) | v1, one run's times | 80.8, 67.6, 94.3, 62.8 | +23.4% | failed (below) | – |
| A [46056339579](https://github.com/Langspan/LangSpan.dev/actions/runs/46056339579) | mean of three runs | 77.1, 86.0, 86.5, 75.5 | **+6.4%** | 229.7, 181.8, 161.7, 173.7 | +23.0% |
| B [46057973393](https://github.com/Langspan/LangSpan.dev/actions/runs/46057973393) | mean of three runs | 72.6, 101.1, 87.5, 65.9 | +23.6% | 171.6, 184.0, 165.9, 103.3 | **+17.8%** |
| C [46059455302](https://github.com/Langspan/LangSpan.dev/actions/runs/46059455302) | mean of three runs | 79.2, 83.0, 74.2, 72.4 | **+7.5%** | 190.2, 204.6, 137.9, 142.3 | +21.2% |
| Cold [46061169788](https://github.com/Langspan/LangSpan.dev/actions/runs/46061169788) | mean of three runs | 67.9, 84.0, 77.5, 70.6 | **+12.0%** | 185.1, 172.8, 118.0, 154.7 | **+17.4%** |

Each shard's mean over the runs of one assignment:

| Assignment | Runs | Linux | Slowest | macOS | Slowest |
| --- | --- | --- | --- | --- | --- |
| hash | the two before runs | 65.0, 90.6, 80.8, 55.3 | +24.2% | 126.7, 179.2, 193.0, 117.0 | +25.4% |
| mean of three runs | A, B, C, cold | 74.2, 88.5, 81.4, 71.1 | **+12.3%** | 194.2, 185.8, 145.9, 143.5 | **+16.0%** |

Every shard ran its tests once: 887 + 241 + 739 + 282 = 2,149 on Linux and
350 + 376 + 919 + 501 = 2,146 on macOS in each of A, B, C and cold, each count
checked by the shard against what `ci-shards.py` assigned it before the
tests ran. All passed.

## Job time and the download

Seconds, shards 1 to 4: job time, and in brackets the archive download.

| Run | Linux | macOS | Linux done | macOS done |
| --- | --- | --- | --- | --- |
| 46048404782 (hash) | 154 (28), 166 (12), 213 (61), 117 (12) | 190 (33), 270 (36), 366 (43), 194 (35) | 6m57s | 12m45s |
| A | 169 (26), 174 (22), 170 (14), 139 (15) | 369 (45), 290 (35), 273 (32), 273 (32) | 6m36s | 12m32s |
| B | 142 (13), 193 (26), 154 (16), 177 (61) | 265 (31), 269 (16), 245 (17), 170 (30) | 7m33s | 9m45s |
| C | 179 (47), 241 (108), 158 (23), 142 (18) | 315 (35), 308 (20), 214 (19), 225 (8) | 8m19s | 12m12s |
| Cold | 139 (11), 187 (41), 162 (22), 165 (29) | 300 (35), 283 (34), 166 (6), 247 (21) | 14m36s | 18m01s |

The slowest job over the mean, with and without its download:

| Run | Linux job | Linux job less download | macOS job | macOS job less download |
| --- | --- | --- | --- | --- |
| 46048404782 (hash) | +31.1% | +14.7% | +43.5% | +48.0% |
| A | +6.7% | +8.5% | +22.5% | +22.1% |
| B | +15.9% | +21.5% | +13.4% | +18.4% |
| C | +33.9% | +3.1% | +18.6% | +17.6% |
| Cold | +14.5% | +6.2% | +20.5% | +17.8% |

The four Linux shards' downloads of the same archive took 13 to 61 s in
run B and 18 to 108 s in run C. In run C the download alone put Linux shard 2's job 34% over the mean while its tests were
7.5% over. No assignment of tests can remove that.

"Linux done" and "macOS done" did not improve: the build job before the
shards took 3m17s to 6m39s on macOS and 3m27s to 4m05s on Linux across
the warm runs, and it and the download vary by more than the shards' test
times now differ. The cold run's build took 11m15s on Linux and 12m43s on
macOS, compiling every dependency and the CI tools; its shards' test times
are no different from the warm runs', as nothing they run is cached.

## What the figures show

- **Linux meets the criterion in three of four runs** (+6.4%, +7.5%,
  +12.0%) and misses in one (+23.6%), against +23.8% and +24.6% before.
  Over the four runs the slowest shard's mean is 12.3% over.
- **macOS meets it in two of four runs** (+17.8%, +17.4%) and misses in
  two (+23.0%, +21.2%), against +24.4% and +52.7% before. Over the four
  runs the slowest shard's mean is 16.0% over.
- **What is left is run-to-run variance, not the assignment.** Runs A, B,
  C and cold give every shard the same tests. Linux shard 2 took 86.0,
  101.1, 83.0 and 84.0 s; macOS shard 4 173.7, 103.3, 142.3 and 154.7 s. The longest test,
  `dry_run_langspan_arm_following_briefings`, took 126 s on macOS on
  average over the recorded runs and 197 s in run A, where its shard was
  the slowest.
- **A failure in run 46054527495, not caused by the assignment.**
  `dry_run_langspan_arm_ignoring_briefings` failed all three tries on
  macOS shard 1 (`no context added`, `tests/benchmark_harness.rs:59`), the
  last after the rest of the shard had finished. Run 46053197733 gave that
  shard the same tests, and it passed.

## What would close the gap

Three follow-ups, none in this change:

- **Weigh the multi-process tests by the CPU they use.** The benchmark dry
  runs start daemons and agents, so a dry run slows its shard's other
  tests by more than its own wall time. nextest's `threads-required` for
  them, and a weight to match in `ci-shards.py`, would count that.
- **The download.** Up to 108 s for the same archive, more than the
  imbalance left in the tests. A smaller archive would narrow it; why the
  same download varies this much on GitHub's runners wasn't measured.
- **The longest dry run.** On macOS it alone is 74% of a shard's mean test
  time on average and was 115% in run A; split into smaller runs it would
  stop setting its shard's time.

The #486 criterion is three merge-queue runs; those can be read only after
this merges.
