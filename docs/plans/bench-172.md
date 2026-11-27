# Plan: bench-172, the coordination benchmark's first publishable numbers

Refs #172, #494. Builds on #212 and #213 (both closed). Unblocks #495.

## What already exists (read, not rebuilt)

- Three families with oracles (`tasks.py`, `oracle.py`), the fixture, the
  three arms, `--stagger`, auto memory off, the stand-in agent and its modes,
  the repair round and the cost to a correct result (`driver.py`, record v8).
- Stale-name writes admitted and the time to the last stale caller fixed, per
  trial (`stale_callers`) and per arm group (`driver.py summarize`).
- #494: each task's `tokens` (`input`, the uncached input; `cache_read`;
  `cache_creation`, the cache write; `output`) is summed over the session's
  result lines from Claude Code's own `usage`. The trial has `tokens.total`
  and `cost.run/repair/total.tokens`, and `summarize` sums them per arm group.
  `test_the_record_sums_every_result_line_of_a_session` checks the totals on
  pilot-shaped transcript lines.
- A real run already requires `LANGSPAN_BENCH_CLAUDE_CONFIG_DIR` (a separate
  sign-in folder that the preflight check finds free of LangSpan) and runs
  `--permission-prompts none`, with no bypass flag.

## What is missing, and this slice

1. **A matrix run** that keeps every trial. New `benchmarks/coordination/matrix.py run`:
   given families, session counts, seeds and arms, it runs `driver.py` once per
   trial as a child process, each into `OUT/<family>-n<N>-s<seed>-<arm>/`. The
   arm order rotates with the seed (simple counterbalancing, as `runner.py`
   does). It continues past a failed, timed-out or refused trial and appends
   one line per trial to `OUT/matrix.jsonl` with the driver's exit code. It
   skips a directory that already holds a `record.json`, so an interrupted
   matrix resumes without repeating a trial; a directory without one is
   reported as incomplete and kept. With `--agent standin` (the default) no
   model is called. `--agent claude` passes through to the driver, which keeps
   its own sign-in folder and permission checks; the matrix adds no flag of
   its own to the session's command.
   `matrix.py run --plan-only` prints the trial list, the session count and
   the maximum number of repair sessions without running anything. This
   prints the numbers the owner approves.
2. **The results document.** `matrix.py report OUT... --output FILE.md`
   reads version 8 records and writes Markdown for `docs/evidence/`:
   - one row per trial, refused, failed, timed-out and `change_not_made`
     included: family, N, seed, arm, status, why the comparison excludes it,
     combined build (`integrated_tests`), tasks accepted of total, stale-name
     writes admitted, seconds to the last stale caller fixed, tokens (input,
     cache read, cache write, output), turns, elapsed seconds, repair outcome;
   - one row per condition (family, N, stagger, arm), from `driver.summarize`
     (the one owner of the grouping and exclusions): trials and exclusions,
     combined build passed k of n with a Wilson 95% interval, correct results
     k of n with a Wilson 95% interval, stale-name writes admitted, each
     trial's time to the last stale caller fixed (listed, not averaged), and
     the summed token figures for run, repair and total;
   - versions (agent CLI, models, LangSpan commit and binary digest, Git,
     Python), the seeds, whether the trials were dry runs, and the line "No
     result here is a LangSpan advantage unless the counts above show it".
     A dry-run report says in its first line that it measures the harness,
     not agents.
   No dollar figure appears in the document (#494).
3. **Runner tests for the new arms** (#172's last criterion): one test in
   `test_runner.py` runs a manifest with `worktrees`, `awareness_only` and
   `langspan` arms of real Python children and checks each arm's rows and
   `missing_comparison_arms == []`.
4. **README**: a section "Publishable runs", covering how to go from a clean
   checkout through the matrix to the results document, how failed runs are
   kept, and the live command with its counts. The Files list and Validation
   paragraph get the new file.

## Scenario set, arms, N and seeds for the live runs (owner approves; not run here)

- Stage 1 (the homepage proof, #495): `discovered_change`, arms `worktrees`,
  `awareness_only`, `langspan`, N = 2 and 5, seeds 1 to 5, no stagger (a
  stagger favours the LangSpan arm and stays out of the comparison).
  30 trials, 3 × 5 × (2 + 5) = 105 sessions, plus at most 30 repair sessions.
- Stage 2 (#172's second family): `rename_vs_new_caller`, the same arms, N and
  seeds: 30 more trials, 105 sessions, at most 30 repairs.
- Turn estimate per session: 10 to 30 turns. Turns per session are not
  measured yet (the 14 and 2 turns in the existing test are fixture values,
  not retained pilot evidence). A repair session takes about as many again.
  The PR labels this an estimate.
- What the oracle measures per trial: stale-name writes admitted
  (`stale_callers.writes_admitted`), combined build passed
  (`integrated_tests`), time to the last stale caller fixed
  (`stale_callers.fixed_after_s`), tasks accepted, the repair round, and
  tokens per session and per arm (#494).
- Failed runs: every trial directory and its `record.json` are kept whatever
  happened; `matrix.jsonl` holds one line per trial, including those that
  produced no record; the report lists each one and why the comparison
  excludes it.

## Test-first order

1. `test_matrix.py`, cheapest layer first, in process: the Wilson interval
   (0 of 5, 5 of 5, 3 of 5 against known values); the trial list and arm
   rotation of `--plan-only`; the report rendered from hand-built v8 records
   (copied from `driver.build_record` output in an existing test helper), with a
   refused and a `change_not_made` trial listed, per-arm token sums equal to
   the records' sums, and no `$` or `cost_usd` in the output. Watch each fail
   before writing `matrix.py`.
2. One process test (it verifies child processes, kept failures and resume):
   the dry-run matrix for `discovered_change`, N = 2, seed 1, all three arms,
   with the real `langspan` binary. Every arm writes a record; the report shows
   `langspan` passing and `awareness_only` and `worktrees` failing the combined
   build. Re-running skips all three. A planted directory without a record is
   reported as incomplete.
3. The runner arm test in `test_runner.py`.
4. Wire `test_matrix.py` into `tests/benchmark_harness.rs` (each suite is
   listed there by name).
5. Break the guarded code once (for example, have the matrix stop at the first
   failed trial) and confirm the process test catches it.

## Shared contracts touched

- None of the protocol, CLI dispatch, daemon or record schema. The run record
  stays v8; `matrix.py` only reads it, through `driver.summarize` and
  `driver.SCHEMA_VERSION`.
- `tests/benchmark_harness.rs` gains one `python_suite("test_matrix.py", None)`
  test.
- `docs/IMPLEMENTATION_STATUS.md`: one line saying that the matrix and the
  results document exist and that no live run has been kept yet.

## Open question for the owner

#494 says "No dollar amounts are recorded", but record v8 already keeps the
host's own `total_cost_usd` (`cost_usd`, `cost.*.cost_usd`). I read the
criterion as "LangSpan computes no price and the published results show
none". So this slice leaves the record unchanged and keeps dollars out of the
results document. Removing `cost_usd` from the record would be a schema v9
change with its own PR. Close #494 only if the owner accepts this reading;
otherwise "Refs #494".

## Not in this slice (stays open on #172)

- The live runs themselves and the committed results document under
  `docs/evidence/` (owner approval, then review of the transcripts).
- The cross-machine run at N = 2 through the Git-ref transport.
- N = 10 and the `contradictory_defaults` family in the publishable set.
