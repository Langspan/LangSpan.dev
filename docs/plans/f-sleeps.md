# Plan: f-sleeps (finish #310, the fixed-sleep part of #311)

## What is left

`scripts/fixed-sleeps.tsv` allows 37 fixed sleeps of 200 ms or more in 14
files under `tests/` (the "39 in 15" of the task text is from an older
count; `scripts/fixed-sleeps.py tests` and a grep agree on 37). The check's
target is zero. Each changed file passing 20 times in a row under load, the
whole suite passing 20 times in a row, and the summed and lane times are not
shown yet.

## Rule for each sleep

Each sleep is one of three kinds, replaced with the existing helpers in
`tests/support/mod.rs` (no change to that file):

1. **A positive condition follows** (an outcome, a log line, a lease state):
   `wait_for` / `wait_until` on that condition.
2. **The subject is that nothing happens for a stated time** (no second
   notice, no second record, the process keeps running): `holds_for` with
   the same period and the assertion that followed the sleep checked
   throughout, which is stronger than checking it once at the end. Where a
   positive sign exists first, wait for it, then hold.
3. **Time itself must pass** (a gap, a wall-clock second, a redelivery later
   than the run): `next_wall_second`, or `wait_until` on `elapsed()` measured
   from the instant the product counts from, so the wait ends exactly when the
   condition holds.

No assertion is weakened, no period under test is shortened in a way that
makes another part of the same test more sensitive to load, and no `slow_`
name changes unless the test's duration changes class.

## Per file

| File | Lines | Kind | Replacement |
| --- | --- | --- | --- |
| `tests/between_steps.rs` | 314 | 2 | `holds_for(QUIET, watcher still running)` (the `slow_` test keeps its 30 s, its subject) |
| | 370, 381, 497, 638 | 2 | `holds_for(period, watcher still running)` before the event that must reach a running watcher |
| | 396, 628 | 1 | the 200 ms poll loops become `wait_until` |
| | 436, 464 | 2 | `holds_for(period, watcher retries within its grace)` |
| `tests/briefing.rs` | 368 | 3 | `wait_until` the 300 ms away gap has passed since the last prompt |
| | 1212, 1964 | 1 | the 200 ms poll loops become `wait_for` |
| `tests/codex.rs` | 313 | 3 | `wait_until` the 100 ms admission has expired since it was granted |
| `tests/coordinator_lifetime.rs` | 267 | — | a supervised child that hangs until killed: `loop { park() }` |
| | 308 | 2 | `holds_for(3 s, the coordinator answers)` |
| | 322 | 2 | `holds_for(2 s, the coordinator keeps running)` |
| | 424 | 1 | `settles` on the queued-connection count |
| `tests/hook.rs` | 267 | 1 | `wait_until` alice's lease reads `Expired` |
| `tests/landing.rs` | 515 | 2 | `holds_for(3 s, no late-write marker)` |
| `tests/mcp_hooked.rs` | 257, 314, 335, 378 | 2 | an async `holds_for` in this file (tokio current-thread: the sync helper would block the client's notifications), checking `seen() == 0` throughout |
| `tests/notify.rs` | 279, 319, 436, 486 | 2 | `holds_for(period, no further notifier call)` |
| `tests/refusal_outcomes.rs` | 290 | 1 | `wait_for` the outcome to read `abandoned` |
| `tests/report.rs` | 286, 288 | 3 | `support::next_wall_second()` twice |
| `tests/repository_shapes.rs` | 678 | 3 | `wait_until` the redelivery is later than the run by its length plus 2 s |
| `tests/retention.rs` | 1024 | 2 | `holds_for(1.5 s, no event deleted and no snapshot)` |
| `tests/status_line.rs` | 349 | 1 | `settles` on the state directory listing (500 ms quiet) |
| `tests/user_install.rs` | 1546, 1704 | 2 | `holds_for(period, one host_not_installed / no host_cleared)` |
| | 1687 | 1+2 | `wait_until` the `host_check_failed` line, then `holds_for` it stays one line |
| | 1990 | 1 | the release thread waits on a sign that uninstall is waiting for the lock, if uninstall leaves one; otherwise on `elapsed()` from the uninstall's start |

Then `scripts/fixed-sleeps.tsv` keeps only its header, and its comment and
the "Slow tests" section of `docs/development-process.md` say the count is
zero.

## Order and checks

1. Per file: make the change, run `cargo nextest run --test FILE`, then break
   the guarded condition once for one test of each kind (for example, let the
   notifier be called twice) and confirm the new wait fails with its message.
2. `python3 -I -B scripts/fixed-sleeps.py tests` reports 0 with the trimmed
   allowlist.
3. Under CPU load (`yes > /dev/null` on every core), each changed file 20
   times in a row:
   `for i in $(seq 20); do cargo nextest run --locked --test FILE --test-threads 2 || break; done`.
4. The whole suite 20 times in a row with `--test-threads 2` (a small CI
   runner), keeping each run's JUnit report; summed test time before
   (origin/main) and after, and the fast-lane (`--profile ci-fast`) and
   full-lane wall times, against the 3 and 8 minute targets.
5. `cargo fmt`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
   the full `cargo nextest run`, then the PR.

The figures and commands go in `docs/evidence/ci/fixed-sleeps.md` (new) and
the PR body.

## Shared contracts touched

None: no protocol, golden file, CLI output, daemon or product code changes.
`tests/support/mod.rs` is used, not changed. `docs/development-process.md`
is shared: one paragraph in "Slow tests".

## What this does not finish (#311)

"No untagged test takes over 10 s" (24 tests in other files, including the
benchmark harness that `t-bench-py` holds) and making the slow-test step in
`.github/workflows/check.yml` fail the job are outside this write set; the PR
says "Refs #311" with them under "Not done yet" unless the 20 runs show them
all under 10 s.
