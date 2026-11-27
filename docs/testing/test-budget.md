# Test budget

This page records where the test suite's time goes in CI, which tests set
that time and why, and the issue that addresses each group. It is the
per-test view; lane and cache times across many runs are measured in pull
request #449. Issue #311 owns the rule that no untagged test takes over
10 s.

## Measured runs

The numbers below come from three runs on GitHub's standard runners
(`ubuntu-24.04`, `macos-26-arm64`). Per-test times are nextest's own
times from each job's log; step times come from the job's step records.

| Run | Event | Commit | Lane |
| --- | --- | --- | --- |
| 46008001615 | push to `main` | `379a712` | full, Linux and macOS, with the release build |
| 46020312574 | pull request | `f628c5d` | fast, Linux |

A local run of the whole suite on an Apple M6 with 12 cores, at `3bb8a30`
under the `ci` profile, ran 2,085 tests in 158.0 s.

## Where the time goes

In the full lane on Linux (job 13 min 6 s), the check step took 12 min
25 s:

| Part | Time |
| --- | --- |
| Format, dependency policy, unused dependencies, lint, docs | 56 s |
| Build the test binaries | 2 min 7 s |
| Run 2,087 tests | 5 min 20 s (319.7 s) |
| Doc tests and the demonstration on the debug binary | 13 s |
| Release build | 3 min 37 s |
| Demonstration on the release binary | 10 s |

On macOS (job 29 min 10 s), installing the tools took 7 min 17 s, building
the test binaries 3 min 58 s, running 2,084 tests 10 min 23 s (623.0 s) and
the release build 3 min 44 s.

In the fast lane (Linux job 4 min 26 s), the check step took 4 min 6 s.
Building the test binaries took 2 min 15 s of that and running 1,194
tests 47.7 s. One unit test took 29.2 s of the 47.7 s.

The test run is bounded by its longest tests, not by their count. Of the
2,087 tests in the full lane on Linux, 65 took 5 s or more and 19 took
10 s or more; the longest took 36.4 s.

## Budget

These are **Targets**, not measurements:

- A unit test, or a test of `langspan-core` or `langspan-graph`, takes
  under 1 s. The fast lane runs all of them.
- An integration test takes under 5 s in the full lane on Linux.
- A test that must take longer is named `slow_…` with a comment saying
  why (see [slow tests](../development-process.md#slow-tests)). The fast
  lane leaves it out.
- A test that waits on a product delay shortens that delay with a
  `LANGSPAN_TEST_*` setting, and one test keeps the production value.

## Why the slow tests are slow

Each test over 5 s on Linux, or over 10 s on macOS, falls into one of these
groups. Times are Linux / macOS in seconds from run 46008001615, or the
fast lane where noted.

### A fixed product delay with no test setting (#455, #456)

The coordinator scans each workspace on a fixed 2 s tick
(`src/daemon.rs`), and a rescan a hook asks for waits at least 5 s
(`URGENT_AFTER`). A test that writes a file and waits for the coordinator
to see it pays several scans. This puts a floor of about 6 to 10 s under 20
tests in `tests/graph.rs`, the slowest at 16.6 / 17.7, and under tests in
`tests/anchored_notes.rs`, `tests/wider_checks.rs` and
`tests/auto_checks.rs`.

The between-steps watcher waits a fixed 10 s start grace
(`START_GRACE`, `src/between_steps.rs`). Three tests in
`tests/between_steps.rs` take 8 to 11 s because of it.
`a_notice_reaches_a_session_that_makes_no_hook_call_for_30_seconds` waits
30 s by design (36.4 / 36.2) and is not named `slow_…`.

#456 lets a test shorten the grace with `LANGSPAN_TEST_WATCH_GRACE_MS`
(500 ms up to the 10 s it can only shorten). The watcher tests use 1 s,
and the one that starts the coordinator inside the grace uses 5 s. A unit
test pins that without the setting the grace is 10 s. On an Apple
M-series laptop the two tests that wait out the grace took 11.2 s each
before and 2.1 s after; the third took 5.1 s both times. The 30 s test is
now `slow_a_notice_reaches_…`: its quiet period spans one 30 s
`SESSION_SEEN_INTERVAL_MS`, so it keeps the real timing.

### A larger input than the assertion needs (#457, #459)

- `store::tests::a_report_on_a_long_history_folds_at_most_the_budget_and_says_partial`
  built 200,000 events where about 61,000 cross the bound: 29.2 in the
  fast lane, 17.2 / 16.2 in the full lane.
- `breaks::tests::a_replacement_that_would_pass_the_parser_bound_is_not_applied`
  made 1,000 replacements on a 1 MiB string: 13.4 in the fast lane.
- `store::tests::a_session_registered_again_elsewhere_keeps_its_earlier_read_in_its_old_workspace`
  committed 50,000 events: 8.2 in the fast lane.
- `graph::review_regressions::failing_head_blobs_beyond_the_pass_budget_do_not_starve_a_later_file`
  wrote about 100 MiB to Git: 6.3 / 29.4.
- `capture_codex`, `status_history`, `hook_replies`, `retention` and
  `binary_versions` each build their state through hundreds or thousands of
  process starts or socket requests: 9 to 24 s each.

#457 shrank the first four: their tests now lower the bound they cross
(the report's window and fold budget, the indexer's files per pass) or
size the input from it. On an Apple M-series laptop under the `ci-fast`
profile they took 5.6, 4.4, 1.6 and 5.9 s before and each under 0.2 s
after.

#459 changed how the other five build their state; each still calls the
coordinator, the CLI or the hook it checks:

- `capture_codex` writes its 1,001 earlier sessions to the store before the
  daemon starts. The stand-in Codex registers each turn's sessions through
  one `langspan mcp`, 16 calls at a time, not one `session start` each. With
  IDs near the 128-byte limit, 240 registrations (not 450) still fill more
  than one page of `langspan events`.
- The two `status_history` tests make the bounded view leave out
  `landings` and `workspace_git` with one automatic check at its limits,
  written to the store, in place of 800 sessions registered over the socket.
- `binary_versions` reads the real copied binary once, then replaces it
  with a small file holding another version's marker.
- `retention`'s daemon test keeps 100 days of 10 steps, not 150 of 40.
- `hook_replies` still builds a new coordinator for each recorded event:
  the briefing names a peer of the coordinator's current generation, so a
  copied state would change the pinned replies. It runs four fixtures at a
  time.

With the five files run together on an Apple M-series laptop (debug, default
profile), the six tests #459 names took 27.2, 23.3, 23.8, 7.8, 4.4 and
21.6 s before and 4.3, 1.6, 1.9, 2.0, 0.8 and 3.1 s after (in the order
capture, the two status tests, Codex replies, retention, versions); the
five files took 28.8 s before and 5.1 s after.

### Process starts where an in-process check would do (#458)

`cli_golden::every_option_against_every_command_matches_the_table` starts
the binary 6,156 times: 12.4 / 62.9, and 20.0 in the fast lane. It is the
slowest test on macOS.

### Test harness timeouts (#460)

The benchmark harness's Python tests wait on real hook timeouts and fixed
sleeps. `driver_planning_oracle_and_refusals` runs about 50 tests one after
another (35.1 / 43.6). `standin_hooks` and
`a_real_refusal_reaches_the_run_record` take 16 to 19 s each.

#460 found that 20 s of `driver_planning_oracle_and_refusals` came after
its Python tests had finished: the tool-guard test's agent was a shell
running `sleep 20`, the guard stopped the shell, and the orphaned `sleep`
kept the suite's stderr open, so `Command::output` waited for it. The agent
now `exec`s one process that waits and then writes, so stopping it leaves
nothing behind. #460 also:

- waits for the hook-log test's hooks to have no process left (each names
  its `late` file in its arguments) instead of sleeping 3 s;
- sets the stand-in's bound to 0.5 s (was 2 s) in the bound test, and a
  0.6 s hook against a 0.3 s host timeout (was 2 s against 1 s);
- cuts the late-hook test's copied host timeout from 5 s to 1 s, with its
  hooks starting 1.5 s and 0.5 s late (were 6 s and 4 s);
- splits the driver tests into `DriverUnitTests`, `HookLogWrapper` and
  `RepairAndSummary`, each its own Rust test, and runs the two
  `RefusalCollection` tests as two. `every_driver_test_is_run_by_a_test_here`
  fails if a Python test has no Rust test that selects it.

Each changed test still fails when the code it guards is broken: the
hook-log wrapper that leaves its hook's group running on a signal, a tool
guard that never stops the session, a stand-in that gives up on a hook at
its host timeout (#200), and a stand-in that never ends a hook's group (the
bound test then hangs until nextest's 480 s timeout).

On an Apple M-series laptop (debug, default profile, the non-dry-run harness
tests run together, other builds running on the machine), in seconds:

| Rust test | Before (`fc0c8fcf`) | After |
| --- | --- | --- |
| `driver_planning_oracle_and_refusals` | 34.1 | 3.3 |
| `the_hook_log_wrapper` | (in the above) | 7.0 |
| `repair_records_and_summary` | (in the above) | 1.4 |
| `a_real_refusal_reaches_the_run_record` | 16.8 | 2.1 |
| `a_pre_tool_hook_that_starts_late_refuses_nothing` | (in the above) | 4.3 |
| `standin_hooks` | 16.1 | 8.7 |
| `runner_mechanics` | 5.9 | 5.9 |
| `holder_wait_comparison` | 4.2 | 4.1 |
| All of them, wall clock | 34.1 | 8.7 |

### Slow for a stated reason (keep)

These tests' subject is the time or the size, and they are already named
`slow_…` or are performance acceptance tests:

| Test | Linux / macOS s | Reason |
| --- | --- | --- |
| `report::slow_an_admission_completes_promptly_while_reports_read_a_long_history` | 19.1 / 17.9 | 800,000 events; the size is the subject |
| `landing::slow_a_rebase_landing_bounds_the_rewritten_commits_of_all_its_branches_together` | – / 23.0 | 128 commits, twice the rewrite bound |
| `auto_checks::slow_a_burst_of_batches_runs_one_check_on_the_tree_it_left` | 12.8 / 14.0 | the 5 s debounce is the subject |
| `commit_guard::the_guard_for_a_two_file_commit_stays_within_the_hook_budget_over_a_large_journal` | – / 12.0 | a hook latency figure recorded in ADR 0026, over a large journal |
| `auto_checks::real_go_test_output_is_classified` | 10.1 / 14.5 | a real `go test` run, compiled twice |
| `benchmark_harness::runner_mechanics` | 5.9 / – | process-group timeouts are the subject |

The last three are not named `slow_…` yet; #311 decides their tags.

## Measure again

To find the slowest tests in a CI job, read its log:

```sh
gh run view --repo Langspan/LangSpan.dev --job JOB_ID --log
```

Each test's line reads `PASS [   6.791s] (n/total) binary test_name`. Each
full-lane job keeps its JUnit report as the `junit-linux` or `junit-macos`
artifact for 14 days, and `scripts/slow-tests.py` names the untagged tests
over 10 s in the Linux job's summary.
