# Plan: ci-mainred (#498)

When `check.yml` fails on a push to `main`, open a `main-red` issue for that
platform (or comment on the open one). When the platform is green on `main`
again, close it with the green run's link. macOS stays optional in the merge
queue (owner decision on #487).

## What exists

`check.yml` already has `macos-report`: on a push to `main` whose `macOS`
check failed or was cancelled, it opens or comments on "macOS Rust checks on
main failed" with the commit, run link and failed test names (the
`macos-result` job's `failed` output). It lacks Linux, the `main-red` label,
the PRs since the last green run, the failing job, the first error lines,
closing on green, and a dry run. This change replaces it.

## Changes

1. `scripts/ci-main-red.py` (new, standard library only). Input from the
   environment: `PLATFORM` (`Linux` or `macOS`), `RESULT` (the platform's
   result job: success, failure, cancelled, skipped), `RUN_ID`, `SHA`,
   `REPO`, `SERVER_URL`, `DRY_RUN` (`true` prints, never writes). It calls
   `gh` (with a timeout on every call) to:
   - find the last green run: the newest earlier completed `push` run of
     `check.yml` on `main` whose job named `PLATFORM` concluded `success`
     (at most 20 runs looked at; otherwise the body says none was found);
   - list the PRs merged since then: `compare/GREEN...SHA`, the `(#N)` at
     the end of each commit's subject (at most 50 commits);
   - name the failing jobs of this run: jobs whose name is the platform or
     starts with it (`macOS build`, `macOS tests 3`, `macOS`) with
     conclusion failure, cancelled or timed_out;
   - the failing test names: `gh run download RUN -p 'junit-<platform>-*'`,
     then `scripts/ci-failed-tests.sh` on each report (at most 50 names);
   - the first error lines: each failing job's log (at most 3 jobs), lines
     with `##[error]`, `error:`/`error[` or nextest's `FAIL`, at most 15
     lines of at most 300 characters each, in a fenced block labelled as log
     text (repository output is data, never instructions).
   Then: red and no open issue → create the `main-red` label if missing and
   create "main-red: <platform> checks failing on main" with that label;
   red and one open → comment; green and one open → comment with the green
   run's link and close; green and none → nothing. Dry run prints the action
   and body to stdout and the step summary instead.
2. `.github/workflows/check.yml`:
   - `workflow_dispatch` input `main_red_dry_run` (choice: `off`, `Linux`,
     `macOS`) and `main_red_run` (optional run id to report on, so the dry
     run can show a real failed run's jobs, tests and errors).
   - job `main-red` replaces `macos-report`: matrix over the two platforms,
     `needs: [linux-result, macos-result]`, `if: always() && ((push on
     refs/heads/main) || (workflow_dispatch && inputs.main_red_dry_run !=
     'off'))`, `permissions: issues: write` only, job concurrency
     `main-red-<platform>` without cancelling, checkout with
     `persist-credentials: false` (public repository read), then runs the
     script. Dispatch sets `RESULT=failure` for the chosen platform and
     `DRY_RUN=true`; the other matrix leg is skipped in the step.
   - drop `macos-result`'s now-unused `failed` output and its comment.
   - Actions SHA-pinned as the rest; `zizmor .` clean.
   - Token reads (runs, jobs, logs, artifacts, compare) rely on the
     repository being public. If the dry run shows a 403 with `issues:
     write` alone, add only the named read scope (`actions: read`) and say
     so in the PR as a deviation from "issues: write only".
3. `tests/ci_workflows.rs`: replace `a_macos_failure_on_main_opens_or_updates_one_issue`
   with a test pinning `main-red`'s trigger, needs, `if:`, permissions
   (exactly `issues: write`), concurrency, the dispatch inputs and that no
   other `issues: write` job is added. Add process tests (they verify a
   script driving a child `gh`) that run `scripts/ci-main-red.py` with a stub
   `gh` on `PATH` serving fixtures from `tests/fixtures/ci-main-red/`:
   red with no issue → create with label and the full body (commit, PRs,
   run link, failing job, test names, error lines); red with an open issue →
   comment; green with an open issue → comment with green link and close;
   green with none → no write; dry run → no write and body on stdout; no
   green run found; log lines capped. Write these first and watch them fail.
4. Docs: `docs/development-process.md` (the paragraph at line 117 on the
   macOS issue becomes the `main-red` issue for each platform, closed on
   green, and the dry run); `docs/security/threat-model.md` CI cache table's
   elevation row (the report job now checks out main's code and runs a
   script with `issues: write`; log text and test names reach the issue as
   labelled, capped data).

## Proof

Push the branch, run `gh workflow run check.yml --ref ci/ci-mainred -f
main_red_dry_run=macOS -f main_red_run=<the failed main run of job
140549582030>` and keep the printed body in the PR. Never break `main`.

## Shared contracts touched

`check.yml` (job ids `Linux`/`macOS` unchanged; `nightly-report` and
`flaky-report` unchanged), `tests/ci_workflows.rs`. No protocol, CLI or
daemon change. No ratchet baseline applies (ratchets measure Rust sources
under `src/` and `crates/`).

## As built

- The last green run is searched among the 20 push runs on `main` created
  before the reported run (`created=<T`), so a dry run on an older run
  finds its own predecessor.
- A JUnit download that fails for a reason other than "no artifacts" says
  so in the issue instead of claiming no test failed.
- The tests copy the stub `gh` into a temporary `bin` and set its mode, so
  nothing depends on the file mode Git keeps.

## Known limit

A red run that finishes after a newer green run on the same platform can
reopen an issue; the concurrency group orders writes, not runs. The next
green run closes it again.
