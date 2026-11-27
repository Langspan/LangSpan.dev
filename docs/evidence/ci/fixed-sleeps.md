# The last fixed sleeps: repeated runs and times

Evidence for #310 and #311 (epic #304): the 37 fixed sleeps of 200 ms or
more left in 14 test files replaced with waits on a condition, measured on
an Apple M-series laptop (macOS, aarch64, debug build, cargo-nextest
0.9.152) on 2026-11-27. Other agent sessions were building and testing in
other worktrees on the same machine throughout, so every time below was
taken under load from them as well; each figure is one sample.

## What changed

Each sleep became one of the waits in `tests/support/mod.rs`, by what the
test needs:

- **A positive condition follows:** `wait_for` or `wait_until` on it. The
  Codex and Claude Code admissions now wait for the lease to read expired
  (the coordinator expires leases on its own timer), not a guessed 200 or
  400 ms. The prompt-polling loops in `between_steps.rs` and `briefing.rs`,
  the abandoned refusal in `refusal_outcomes.rs` and the inspection error in
  `user_install.rs` are waits too.
- **The subject is that nothing happens for a stated time:** `holds_for`
  with the same period, checking the assertion that used to follow the
  sleep throughout it (no second notifier call, no list-changed
  notification, the watcher still running, no event deleted). Periods are
  unchanged, including the 30 s of the `slow_` test in `between_steps.rs`,
  so no `slow_` name changes. `mcp_hooked.rs` has an async `holds_for` of
  its own: its client runs on tokio's current-thread runtime, where the
  sync helper would block its notifications.
- **A count or directory that a burst changes:** `settles` (the queued
  connections in `coordinator_lifetime.rs`, the state directory in
  `status_line.rs`).
- **Time itself must pass:** `next_wall_second` for the RFC 3339 window in
  `report.rs`; `wait_until` on elapsed time, counted from the instant the
  product counts from, for the away gap in `briefing.rs`, the redelivery in
  `repository_shapes.rs` and the lock release in `user_install.rs`, which
  leaves no sign that it is waiting.
- The supervised child in `coordinator_lifetime.rs` that hangs until it is
  killed parks instead of sleeping for 600 s.

`scripts/fixed-sleeps.tsv` lists no file, so `scripts/fixed-sleeps.py`
holds every test file at zero. The check's rule, applied with a grep
(`sleep(` whose argument is not a literal `Duration::from_millis` under
200), finds none.

Two waits were broken on purpose once (the async `holds_for` in
`mcp_hooked.rs`, and `notify.rs` expecting a third notifier call): each
failed at once, naming what no longer held.

## The changed files, 20 times in a row

`cargo nextest run --locked --stress-count 20 --test-threads 2 -E
'binary(between_steps) | … | binary(user_install)'` (the 14 files, 209
tests), while the whole suite ran 20 times beside it (below), so the
machine was loaded throughout.

| Iterations | Passed | Failed | Each iteration | Total |
| --- | --- | --- | --- | --- |
| 20 | 17 | 3 | 133.5–221.9 s | 3,260.9 s |

The three failures are in two tests whose code this change did not touch,
and neither is a wait:

| Iteration | Test | What failed |
| --- | --- | --- |
| 14, 19 | `briefing::a_file_too_large_for_the_journal_is_left_uncommitted_only_while_it_differs_from_head` | a hook answered `timeout` instead of `admitted` (line 1256) or `observed` (line 1259): the hook's own deadline ran out under load |
| 16 | `report::slow_an_admission_completes_promptly_while_reports_read_a_long_history` | the admission took 1.34 s while a report ran, over `ADMISSION_LIMIT` (line 760) |

Both are latency budgets of the product, and under this load they were
exceeded. Raising a budget or making the product faster is a decision for
their own issues, not part of replacing sleeps, so they are left as they
are and reported here. With them, the criterion "each changed file passes
20 times in a row under load" is **not met**: 12 of the 14 files passed all
20 iterations; `briefing.rs` and `report.rs` did not.

## The whole suite, 20 times in a row

`cargo nextest run --workspace --locked --stress-count 20 --test-threads 4`
(4 threads, as on a 4-vCPU CI runner), beside the run above for its first
54 minutes.

| Iterations | Passed | Failed | Each iteration | Total |
| --- | --- | --- | --- | --- |
| 20 | 16 | 4 | 247.8–395.3 s | 5,984.4 s |

| Iteration | Test | What failed |
| --- | --- | --- |
| 2, 9 | `briefing::a_file_too_large_for_the_journal_is_left_uncommitted_only_while_it_differs_from_head` | the hook answered `timeout` (lines 1256 and 1259), as above |
| 17 | `stale_source::a_file_is_refused_once_per_teammate_change_and_the_next_write_goes_through` | the hook answered `timeout` instead of `admitted` (line 705) |
| 18 | `langspan store::tests::a_session_registered_again_elsewhere_keeps_its_earlier_read_in_its_old_workspace` (unit test) | its report, over a 6 ms wall-clock window, held no session (`src/store.rs:2674`) |

None of them waits on a fixed sleep. Three are the hook's deadline running
out under load, one is a wall-clock window. The criterion "the whole suite
passes 20 times in a row" is **not met**.

## Times

| Measure | Command | Result |
| --- | --- | --- |
| Full lane, this change | `cargo nextest run --workspace --locked --profile ci --test-threads 4` | 2,136 tests passed, 7 skipped, in 267.9 s |
| The 14 files' summed test time, this change | the 14 files with `--test-threads 1`, so wall time is summed time | 209 tests in 254.0 s |

Not measured here: the same 14 files on `origin/main` (summed time before),
and the fast lane (`--profile ci-fast`). The session that made this change
could not check out `main`'s copies of the files or start a second worktree,
so the fall in summed time is not stated as a measurement. Most of the
replaced sleeps keep their period as a `holds_for`, so the expected fall is
small: only the waits that end once their condition holds (about a dozen
of the 37) can be shorter, by at most the margin each sleep had over its
condition. That is an estimate, not a measurement.
