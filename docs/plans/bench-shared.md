# Plan: shared-checkout arm in the coordination benchmark (#543)

Refs #543, part of #172 and epic #538.

## What this pull request does

Adds the `shared` arm to `benchmarks/coordination/`, run with the stand-in
agent only (no live model run; that needs the owner's approval, as in #513):
every session works in one checkout of the fixture, `work/trunk`, on `main`,
cloned from a bare integration remote `work/origin.git`. No coordination
tool. Each session commits only its own files (`git commit -- PATHS`) and
pushes to `origin main`, fetching and merging then pushing again on a
rejection, at most a fixed number of attempts.

Recorded per task and per trial: push attempts and rejections, counted from
the session's transcript (each Bash `git push` call and whether its result
shows Git's rejection). The stand-in makes its commits and pushes as Bash tool
calls in the same stream-json shape, so one counter covers stand-in and real
sessions. Arms that do not push record `pushes.status: "not_applicable"`
(null counts, never zero). The combined result is what `origin main` holds
when every session has ended; work left unpushed did not land.

Tokens, turns and elapsed time come from the existing cost section;
person-touches from the record's `stops` and `questions`.

## Narrowed after reading the code

`langspan_shared` is left for a follow-up. In one checkout every session has
the same LangSpan workspace, and the harness attributes hook logs, refusals,
notices and context to a session by workspace (`bench-sN`); the stand-in also
waits for peers keyed by workspace, so it would wait out its time limit.
Fixing that attribution is its own change.

## Files

| File | Change |
| --- | --- |
| `benchmarks/coordination/shared_checkout.py` (new) | Prepare the bare remote and the one checkout; count pushes from a transcript; the stand-in's commit-and-push with retries |
| `benchmarks/coordination/driver.py` | `SHARED_ARMS`; `isolation` gains `shared`; shared prompt text; push/pull/fetch/merge allowed in the shared arm; integrate and repair from `origin main`; `pushes` in the record; record v9; summary reads v8 and v9 |
| `benchmarks/coordination/standin_agent.py` | In a shared checkout, commit its own edited paths and push |
| `benchmarks/coordination/matrix.py` | `--arms` accepts `shared` (default stays the three compared arms); the results document gains push attempts and rejections per trial and per condition; reads v8 and v9 |
| `benchmarks/coordination/runner.py`, `manifest.schema.json`, `records.schema.json` | Accept the arm name `shared` (added to the reservation after admission: the runner refuses an arm it does not list) |
| `benchmarks/coordination/run-record.schema.json` | v9: the arm, `isolation: shared`, `pushes` per task and per trial |
| `benchmarks/coordination/README.md` | The arm, its checkout, what is recorded, and what is pending |
| `benchmarks/coordination/test_shared.py` (new) | Tests below |
| `tests/benchmark_harness.rs` | One selector for the unit tests and one for the dry runs |

## Test-first order

1. `push_counts` on transcripts (in process): no push, a rejected call, a push
   inside a compound command, a commit message that says "push".
2. Real Git (Git's own output is what's verified): two clones of one bare
   remote; the second push is rejected, counted, and the retry lands both.
3. `make_plan` for `shared`, `claude_argv`'s push tools, `matrix.trials` and
   `--arms`, the summary and the report's push columns, v8 records read.
4. Dry runs, N = 2 and 5, `rename_vs_new_caller`: a schema-valid v9 record,
   every session's cwd the one checkout, at least one push per session,
   rejections at most attempts, and `origin main` holding the sessions' commits.

## Shared contracts touched

- The run-record schema (v8 → v9); the summary and report still read v8.
- The runner's arm list and its two schemas: one name added.
- No Rust protocol, CLI, hook or daemon code changes.

## Not in this pull request

- `langspan_shared`, as above; then one-step landing once #536 lands.
- The pilot (N = 2 and 5, one seed): needs #534 fixed and the owner's live-run
  approval as in #513.
- Stale-name writes and duplicate work in the shared arm: one tree holds every
  session's edits, so they are not attributed per session (`duplicate_work`
  null; the stale-caller measure applies only to `discovered_change`).
