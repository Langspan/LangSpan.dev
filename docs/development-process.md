# Development process

How work moves from the backlog to a release in LangSpan, for every person
and agent working here. [Engineering practice](engineering.md) owns how to
build each change; this document owns how changes are chosen, limited,
reviewed, merged and released. The owner approved it on 2026-11-26.

## Backlog

Every item of work is one GitHub issue. Labels on the issue carry its type,
priority and size:

- **Type.** Label a capability `roadmap` and a defect `bug`. The
  [roadmap index](roadmap.md) lists the open `roadmap` issues by area; it
  is [generated](#generated-files) from them, not edited by hand.
- **Priority.** The MoSCoW labels are the source of truth for priority:
  `moscow: must`, `moscow: should`, `moscow: could` and `moscow: later`.
  Take a `must` before a `should`, and so on down.
- **Size.** Set `size: S`, `size: M` or `size: L` when work starts. Compare
  the size with the actual cycle time when the pull request merges, to
  calibrate later estimates.

If a decision is costly to reverse, record it in an ADR in
[`docs/adr/`](adr/0001-local-coordination.md) before or with the change.

A new wire field adds a named capability, one line in `CAPABILITIES` in
`src/protocol.rs` with vectors in `fixtures/capabilities/<name>/`, never a
protocol version. The version changes only for a breaking change
([ADR 0037](adr/0037-protocol-capabilities.md)).

## Work in progress

Limit open work so that each pull request merges quickly:

- Keep at most 5 pull requests open at once. Release pull requests don't
  count.
- Keep at most one open pull request per hot component. The hot components
  are the following namespaces of the
  [module map](architecture.md#module-map), with the files that hold them
  today:
  - `protocol`, the wire protocol: `src/protocol.rs` and its clients;
  - `briefing`: `src/briefing.rs` and `src/briefing/`;
  - `install`: `src/install.rs`, `src/install/` and `src/user_install.rs`;
  - `cli`, its argument list and flags table: `src/main.rs` and `src/cli/`;
  - `graph`, the source graph: `src/graph.rs`, `src/graph/`,
    `src/index_file.rs` and `crates/langspan-graph`.
- Give at least one of the 5 slots to bugs, review follow-ups or flaky
  tests. This slot is the debt slot.
- Aim to merge each pull request within an hour of opening it. If a pull
  request is older than that, attend to it before you start new work.

## Definition of Done

A pull request is done when all of the following are true:

- Its tests were written first, and they pass.
- `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets --locked -- -D warnings` are clean.
- The docs and the roadmap issue match the change, and the
  [generated tables](#generated-files) are up to date.
- CI's fast lane is green on the pull request, and the full lane on Linux
  passes on its merge-queue commit, on top of the latest `main`. The macOS
  full lane runs there too; it isn't a required check, so read its result
  and fix a macOS failure before it reaches `main`'s macOS issue.
- No P0 or P1 review finding is open.

Write "Fixes #N" only when the pull request meets every build criterion of
issue N. If the issue also asks for evidence from a real host, track that
evidence in its own "Verify ... on a real host" issue, labeled `real-host`
and listed under [#110](https://github.com/Langspan/LangSpan.dev/issues/110).
Building a capability and verifying it on a host stay separate.

## Review

Automated review runs on every new head of a pull request. Treat its findings
by priority:

- A P0 or P1 finding always blocks the merge.
- After two review rounds, move the remaining P2 findings to one issue
  titled "Review follow-ups from #N" instead of starting more rounds.
- Work follow-up issues in the debt slot.

A pull request that changes a committed state snapshot (the `state.json`
files the replay tests compare, ADR 0015) says in its body which snapshot
changed and why. Rewrite one with `LANGSPAN_UPDATE_GOLDEN=1` on the test
that compares it, then read `git diff` before committing: an unexplained
refresh hides the regression the snapshot exists to catch.

## Integration

Pull requests merge through GitHub's merge queue. Exactly these gate a
merge, as the ruleset for `main` requires them
([`.github/desired-settings.json`](../.github/desired-settings.json)):

1. **To join the queue**, the pull request's own `Linux` (the fast lane),
   `Workflows` (zizmor) and `Content` checks pass. Its `macOS` check passes
   at once on a pull request and gates nothing.
2. **To merge**, the same three checks pass again on the queue's commit:
   the pull request on top of `main` and every pull request queued ahead
   of it, with `Linux` in the full lane. A pull request whose checks fail
   there leaves the queue, and the ones behind it are tested again without
   it. The Content check on a queued commit checks the tree only; the
   pull request's title and body passed it before the pull request was
   queued.

`Linux` and `macOS` are the jobs that collect each platform's build and
test shards ([which tests run when](#which-tests-run-when)), so a ruleset
lists those two names and none of `Linux build` or `Linux tests 1`.

`macOS` isn't required: the full macOS lane runs on the queue's commit and
again on `main`, but the queue doesn't wait for it, because waiting doubled
the time a pull request spent in the queue before the tests ran in shards.
Requiring it now, so that the ruleset lists `Linux`, `macOS`, `Workflows`
and `Content`, is the owner's decision. Nor is coverage, the nightly
run, or the release build. When the `Linux` or `macOS` check fails on a
push to `main`, job `main-red` in `check.yml` opens the issue "main-red:
macOS checks failing on main" (or Linux), labelled `main-red`, or comments
on it while it is open. The issue names the commit, the run, the failing jobs,
the tests that failed every try (at most 50), the first error lines of the
failing jobs' logs (at most 15) and the pull requests merged since that
platform's last green run on `main`. When the platform is green on `main`
again, the job closes the issue with the green run's link. When no test
failed and every failing job failed on GitHub's side (an artifact upload or
cache error, a timed-out connection, a lost runner, by its log's error
lines), the run's first attempt opens no issue. Instead it starts
`rerun-failed.yml`, which waits for the run to end and reruns its failed
jobs once; the rerun reports in the same way, and opens the issue if it
fails too. Nothing is reverted automatically. Treat the issue like a failing check on `main` and
fix it the same day. To see the issue a run would file without filing it,
start `check.yml` by hand with `main_red_dry_run` set to the platform and,
optionally, `main_red_run` set to the run to report on; the job prints the
body in its log and summary.

While the Linux check on `main` fails, merge only the fix for that failure.

### Adding a response field

[ADR 0015](adr/0015-version-tolerance.md) decides what an addition needs.
A field of an object only responses carry needs no new version and no
capability: clients from 0.6.0 (`tolerant_responses`) ignore a field they
don't know, and `encode_with` leaves it out for earlier ones. A field of an
object that is stored or sent in a request, or a new event type, needs a
capability. Removing a field or changing its meaning needs a new version.
Add the field to the list of response fields added without a version in
[Retries and recovery](usage/retries-and-recovery.md), with the release
that adds it and what an earlier client gets instead.

### Generated files

These are generated by `scripts/gen-docs.py`, so two pull requests that
add items add separate source entries instead of editing the same lines.
Edit the source, never the region between the `<!-- generated` and
`<!-- end generated -->` lines:

| File | Generated region | Source, one entry per item |
| --- | --- | --- |
| `docs/usage/mcp.md` | the tool table | `docs/tables/mcp-tools/<tool>.txt` |
| `docs/adr/0037-protocol-capabilities.md` | the capability table | `docs/tables/capabilities/<name>.txt`, with the "since" version from `CAPABILITIES` in `src/protocol.rs` |
| `docs/roadmap.md` | the A to J index | the open `roadmap` issues: area label, title and `**Index.**` paragraph |
| `docs/usage/retries-and-recovery.md` | the event types each served release reads | `fixtures/event-types/<release>.txt`, in version order |

A pull request that adds an MCP tool or a capability adds its `.txt` entry
and runs `python3 scripts/gen-docs.py`; `tests/tool_count_in_docs.rs` fails
when a table differs from its entries (`--check`), and a capability without
an entry fails the generator. Rows are in name order, so two new rows
land in separate hunks unless no existing name sorts between them, the
same rule as the `CAPABILITIES` lines ([ADR 0037](adr/0037-protocol-capabilities.md#consequences)).
Then Git reports a conflict; run the generator on the merged entries to
resolve it.

The roadmap index needs the network, so CI never builds it. Regenerate it
on demand and commit the result in its own pull request:

```sh
gh issue list --label roadmap --state open --limit 500 \
  --json number,title,body,labels,url > target/roadmap-issues.json
python3 scripts/gen-docs.py --roadmap target/roadmap-issues.json
```

A line's summary is the issue's `**Index.**` paragraph, else the line's
current summary, else the first sentence of its `**Problem.**` paragraph
(the generator names each issue that took it from there). An issue without
exactly one `area:` label stops the run and changes nothing.

### Which tests run when

The Rust checks ([`check.yml`](../.github/workflows/check.yml)) run
`scripts/check.sh` on Linux and macOS at the same time. The tests run under
cargo-nextest, locally and in CI: `cargo nextest run --workspace --locked`
runs them in parallel with the settings in
[`.config/nextest.toml`](../.config/nextest.toml), and
`cargo test --doc --workspace --locked` runs the doc tests. A test still
running after 3 minutes is killed and fails, and so is one that leaves a
child process holding its output. The checks run in two lanes, chosen by
the event (`LANGSPAN_CHECK_LANE`):

- **The fast lane: a pull request.** On Linux only: format, dependency
  policy and advisories (cargo-deny and cargo-audit, with the one ignore
  list in `deny.toml`), unused dependencies, lint, and the tests nextest's `ci-fast`
  profile selects: every unit test, every test of `langspan-core` and
  `langspan-graph`, and the integration tests of the protocol, the store,
  Git output, MCP framing, the workflows and the docs. None of those
  starts a coordinator, except the CLI's golden transcripts
  (`tests/cli_transcript.rs`, `tests/cli_golden.rs`), so that output that
  differs on Linux shows on the pull request. The `macOS` check passes at once and says that the
  full run happens in the merge queue. Target: a result **under 3
  minutes** from push. The job stops at 6 minutes.
- **The full lane: a merge-queue commit, a push to `main`, the nightly run
  and a manual run.** On Linux and macOS side by side: everything in the
  fast lane, every other test (the `ci` profile, which leaves none out),
  and the two-developer demonstration on the debug binary the tests built.
  Format, dependency policy, advisories, unused dependencies and the docs
  don't depend on the platform, so only Linux runs them
  (`LANGSPAN_CHECK_PORTABLE=false` on macOS); macOS runs lint, the build
  and every test. Each platform builds once and tests in parallel: its
  build job (`Linux build`, `macOS build`; `check.sh`'s `build` lane) runs
  every check but the tests and builds them into a
  [nextest archive](https://nexte.st/docs/ci-features/archiving/), and
  four shard jobs (`Linux tests 1` to `4`, `macOS tests 1` to `4`; the
  `shard` lane, `time:K/4`) each run the tests `scripts/ci-shards.py`
  gives them, from that archive, under the same foreign home, retries and
  reports. The script splits the measured times in
  `scripts/ci-test-times.tsv` evenly; runner variance still moves a
  shard by 15–20% from run to run ([evidence](evidence/ci/balance.md)).
  Refresh the file from several full runs' JUnit reports, as its header
  says, when the shards drift apart. Every test
  runs in exactly one shard: a shard fails unless nextest selects the
  count the script expects. The `Linux` and `macOS`
  checks are jobs of their own that pass only when the build job and
  every shard passed (`scripts/ci-shard-results.sh`); they name the
  shards' flaky and failed tests for the issues below. Four shards come
  from the measured test time
  ([evidence](evidence/ci/shards.md)). Target: **under 8 minutes** in the
  merge queue. A build job stops at 15 minutes there and at 30 on `main`
  and nightly, and a shard at 15. A build that restores no cache takes
  longer than 6 minutes and can take longer than 15, so a pull request
  that changes `rust-toolchain.toml` or a tool version in
  `scripts/install-ci-tools.sh` raises both limits in `check.yml`, and the
  next pull request lowers them once `main` has saved the new cache
  ([evidence](evidence/ci/lanes.md)). Only the
  nightly run also builds the release binary and runs the demonstration on
  it; nothing else reads it, and the release workflow builds its own. The queue
  merges a pull request only when its full Linux lane passes, so nothing
  reaches `main` untested on Linux. A macOS failure doesn't block the
  merge; it shows on the queue's run, and on `main` in its own issue
  ([integration](#integration)).

Each event and commit has its own concurrency group, so the push to `main`
starts at once instead of waiting for the merge-queue run of the same
commit, and neither cancels the other. Only a newer push to a pull request
cancels its older run.

Every run restores the latest build cache `main` saved for its platform,
job and toolchain, its exact key first. `main` saves under its own key
after a run whose checks passed or failed, but not after a cancelled or
timed-out run, or one that built no tests
([threat model](security/threat-model.md#ci-build-cache)). A manual run
with `cold` set restores nothing, to measure a build from nothing; on
`main`, it still saves an entry that `main` lacks.

The benchmark harness's dry runs (the `dry_run_*` tests in
`tests/benchmark_harness.rs`, 92 to 132 s each on macOS) never run in
`check.yml`, so the merge queue never waits on them; there the harness's
other tests, about 11 s on an Apple M-series laptop, are its smoke test.
The workflow `bench.yml` ("Benchmark dry runs", not a required check) runs
them on Linux and macOS for a pull request or push to `main` that touches
one of the following paths, and otherwise passes at once and says why: `benchmarks/`, `tests/benchmark_harness.rs`, the
daemon, hook and briefing code they drive (`src/daemon.rs`, `src/daemon/`,
`src/hook.rs`, `src/hook/`, `src/briefing.rs`, `src/briefing/`,
`src/text.rs`, `src/text/`, `src/claude_code.rs` and all of
`crates/langspan-core/src/`, so code moved into a new module there still
counts), or what builds and runs the suite
(`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.config/nextest.toml`,
`check.yml`, `bench.yml`, `scripts/check.sh`, `scripts/test-foreign-home.sh`,
`scripts/ci-changes.sh`). A change that touches only Markdown builds
nothing in either lane of `check.yml`.

- **A push to `main`** that adds one commit, as a squash merge does,
  follows the same rule for the dry runs. A push of several commits runs
  them. Since the queue doesn't run them, `main` can hold a change that
  breaks them; this run and the nightly one show it.
- **The nightly runs** on `main` (`check.yml` at 03:23 UTC, `bench.yml`
  with the dry runs at 03:53) and **a manual run** run everything. If one
  fails, it opens an issue titled "Nightly Rust checks on main failed" or
  "Nightly benchmark dry runs on main failed", or comments on that issue
  while it is open. Treat that issue like a failing check on `main`.
- **An advisory dry run** shows that path without an advisory on `main`:
  `gh workflow run check.yml --ref BRANCH -f advisory_dry_run=true`. The
  Linux build audits the real lockfile, then
  `tests/fixtures/advisory-dry-run/known-advisory.lock` (RUSTSEC-2021-0003),
  and fails on it; `nightly-report` then opens or comments on an issue
  titled "Advisory dry run of the nightly report", never the real one.
  Close that issue afterwards ([evidence](evidence/ci/advisories.md)).

The two times are targets the lanes should meet, not gates: a run over
them still passes. `bash scripts/measure-ci.sh` reports them from GitHub's
run data: the median and 90th percentile of workflow completion per event,
the merge-queue gate (run creation to the end of the last required check,
which leaves out macOS), the Linux and macOS build and test-shard job
times by cache state, and how many dependency crates each build job
compiled. It reads runs from before the shards too. `--since` and `--until`
bound the runs, and `--rows DIR` keeps one row per run, per job and per
Content run. The latest figures, the gap to each
target and what would close it are in
[docs/evidence/ci](evidence/ci/README.md).

Locally, `sh scripts/check.sh` runs the full lane, and
`LANGSPAN_CHECK_LANE=fast sh scripts/check.sh` the fast one. The runners
come from the repository variables `LINUX_RUNNER` and `MACOS_RUNNER`;
unset, each job uses GitHub's standard runner. The Linux benchmark dry
runs use the [self-hosted runner](#self-hosted-runner) instead.

When a dry run is skipped, the test log shows
`skipped: no benchmark or briefing change`. A local `sh scripts/check.sh`
runs every test unless `LANGSPAN_BENCH_DRY_RUNS=skip` is set, which
`check.yml` always sets.
[`scripts/ci-changes.sh`](../scripts/ci-changes.sh) owns the path list;
change the list there and here together.

A failing test isn't retried on a pull request, so a flaky test shows
there. In the merge queue and on `main` it gets two more tries, so one
flaky test doesn't eject a healthy pull request; a test that fails every
try still fails the check. Each job's summary names the tests that passed
only on retry, and on `main` they are added to the issue "Flaky tests seen
on main". Every job keeps its JUnit report as an artifact for 14 days.

A test seen flaky, in that issue, a summary or anywhere else, gets an issue
and a deterministic fix the same day. Never rerun the job and move on, and
never weaken an assertion to make it pass.

The core's golden decision test
(`crates/langspan-core/tests/decision_golden.rs`) records the events and
outcome `decide` returns for every command. A new command doesn't compile
there until it has a case, and fails the test until it has a step. A changed
decision fails with a diff of each step that changed. To accept an intended
change, run `cargo nextest run -p langspan-core --test decision_golden
--run-ignored only -E 'test(bless)'` and review the golden diff in the pull
request.

### Slow tests

A test waits on a condition, not a fixed time: the helpers in
[`tests/support/mod.rs`](../tests/support/mod.rs) poll toward a deadline and
fail naming what they waited for. Where a test's subject is that nothing
happens within a stated time, `holds_for` checks it throughout that time.
`python3 -I -B scripts/fixed-sleeps.py tests`, run on Linux in every lane,
fails when a file holds more fixed sleeps of 200 ms or more than
[`scripts/fixed-sleeps.tsv`](../scripts/fixed-sleeps.tsv) allows it, or
fewer. That list is empty, so no test may hold one.

Where a product delay is the subject, a test shortens it with the
`LANGSPAN_TEST_*_MS` setting the code already reads. A coordinator started
with `LANGSPAN_TEST_SCAN_EVERY_MS` (50 to 2000 ms) scans Git at that interval
instead of every 2 s, and lets a hook's urgent rescan through after 5/2 of
it instead of 5 s. A test that must still
take long is named `slow_…`, with a comment saying why. The fast lane leaves
it out (a filter in `.config/nextest.toml`); the full lane runs it. In the
full lane on Linux, `scripts/slow-tests.py` names every test not so named
that took over 10 s. It doesn't fail the job yet: tests in files no change
has reached still take longer (issue #311).

### Coverage

The `Coverage` workflow ([`coverage.yml`](../.github/workflows/coverage.yml))
runs `bash scripts/coverage.sh` on every push to `main`, every night on
`main` (04:41 UTC), and on a pull request that changes the measurement. It
isn't a required check. The
script runs the suite once under cargo-llvm-cov and cargo-nextest, with the
profile written continuously, so coordinators that tests stop with SIGKILL
still count. It counts production lines only (`src/` and `crates/*/src/`,
without `#[cfg(test)]` code, even where an integration test includes a
source with `#[path]`); in-file unit tests still cover them. The exports
name sources relative to the checkout, so a downloaded artifact reads the
same in yours. The run summary has the per-file table.

The floor: production lines 90% covered or more, and no file more than 2
points under its row in
[`scripts/coverage-baseline.tsv`](../scripts/coverage-baseline.tsv), one
row per file. Every row is checked. The run also fails for a row whose file
no longer exists, a row whose file measures no production lines, and a file
of 100 production lines or more without a row. Under 100 lines one line is
more than a point, so the 2-point margin would be a line or two of noise.
A nightly run under the floor, or with a failing test, opens or comments on
the issue "Nightly Rust checks on main failed".

To raise the baseline, run `bash scripts/coverage.sh` (or download the
`coverage` artifact of a run on `main` into `target/coverage`), then
`python3 -I -B scripts/coverage-report.py --write-baseline . target/coverage`,
which rewrites one row per measured file of 100 production lines or more.
Commit only the rows of files your change raised or added. A pull request
that splits or moves a file moves its rows with it: the old row goes, and
each new file of 100 lines or more gets a row. Lowering a row needs the
reason in the pull request.
A refactor that moves or renames files moves their rows in the same pull
request; `tests/coverage_report.rs` fails on a row whose file is gone.

### Ratchets

`tests/ratchets.rs` runs `python3 -I -B scripts/ratchets.py --check .` in
the full lane. It measures production code only: `#[cfg(test)]` items and
files outside `src/` and `crates/*/src/` don't count. It checks six
numbers, each against `scripts/ratchets/baseline/<source path>.tsv`:

- **File size.** A file may not grow past its `lines` row. A file without a
  row may have up to 1,500 lines.
- **Function length.** A function may not grow past its `fn` row. A
  function without a row may have up to 150 lines.
- **Public surface.** A crate may not have more declared `pub` items than
  the sum of its files' `pub` rows. `pub use` and `pub(crate)` don't count,
  so moving an item between files of a crate passes.
- **Public fields.** A struct named in a `fields STRUCT N` row of its
  file may not have more than N `pub` fields; `pub(crate)` fields don't
  count. A row is added by hand, `--record` keeps it, and a struct the row
  names that is gone fails. `State`'s row in
  `crates/langspan-core/src/state.rs` heads for zero (issue #373), so code
  outside the core reads its fields through accessors and cannot write
  them.
- **Index and slice expressions.** A crate may not have more `x[i]`
  expressions than the sum of its files' `index` rows, or more `x[a..b]`
  expressions than the sum of their `slice` rows. Either can panic on
  input it wasn't written for; use `.get(i)` or `.get(a..b)` and handle the
  missing value. Like `pub`, moving one between files of a crate passes.
  The `indexing_slicing` lint stays off until the recorded count is low
  enough to turn it on.
- **Module dependencies** in the `langspan` crate. An edge from one
  top-level module to another may not join a cycle, or point outward,
  unless the module's root file lists it as an `edge` row. Each module's
  layer, and the edges allowed within a layer, are rows of
  `scripts/ratchets/namespaces.tsv` (the
  [module map](architecture.md#module-map)). A module whose row says `-`
  can state its layer with a
  `// layer: utility|infrastructure|application|interface` comment in its
  root file; edges between modules without a layer are checked for cycles
  only.

A failure says what grew, by how much, and what to do: split the code, or
justify the growth in the pull request and run
`python3 -I -B scripts/ratchets.py --record FILE`. A number that improves
passes, and the check names the file; run
`python3 -I -B scripts/ratchets.py --tighten FILE` for that file only, in
the pull request that moved the code. `--tighten` never raises a number,
and it deletes a baseline that has nothing left to record. The checker's
own tests are in `scripts/ratchets/test_*.py`.

One more number is kept per crate rather than per file: the count of
production function and closure signatures that return
`Result<_, String>`, in `scripts/ratchets/string-errors.tsv` (rows
`CRATE<TAB>N`). A crate may not go above its row. When the count falls,
the check prints the new number; lower the row by hand in the same pull
request. Return a typed error instead, as `client::ClientError` and
`git::GitError` do (issue #368).

### Self-hosted runner

The owner runs one Linux machine (8 cores, 16 GB) as a self-hosted runner
with the labels `self-hosted` and `langspan-trusted`. One slow Linux job
uses it: the Linux benchmark dry runs (`bench.yml`). Everything in
`check.yml`, coverage, macOS, releases, fuzzing, Scorecard and the agentic
workflows stay on GitHub's runners. A runner process takes one job at a
time, so the four Linux test shards would wait for each other there; on
GitHub's runners they run side by side. Coverage stays there until a timing
test it runs passes on the machine: in `tests/report.rs` an admission waits
about 1.8 s while a report reads (issue #581).

The runner is used only for a trusted event: any event except
`pull_request`, or a pull request whose branch is in this repository
(`github.event.pull_request.head.repo.full_name == github.repository`).
The job picks its runner with one expression, and a pull request from a
fork gets `ubuntu-24.04`. A fork's pull request runs code from anyone on
GitHub, and a self-hosted runner keeps its disk, home directory and tool
installs from one job to the next. Code from a fork could leave something
behind that a later run on `main` executes, or read what other jobs left on
the machine. A GitHub runner is a fresh virtual machine for every job, so a
fork's code can't reach anything after its own job. `tests/ci_workflows.rs`
checks that only this job names the runner, with that expression, and that
coverage and the `check.yml` shards don't.

Because the machine keeps its state, the job doesn't assume a fresh one.
It installs nothing with `sudo` or system-wide. The toolchain comes
from the runner user's `rustup` (`rustup toolchain install` reads
`rust-toolchain.toml`). CI tools are downloaded and checked again on every
run into `~/.cache/langspan-ci-tools`. `actions/checkout` removes untracked
files, `target/` included, before each job. The dry runs restore
`check.yml`'s Linux entry with the same key as on GitHub's runners and save
no cache, so a trusted pull request's run on the machine can't write an
entry that `main` restores. The runner needs
`rustup`, `python3`, `curl` and `sha256sum` for its user. It also needs a
glibc at least as new as Ubuntu 24.04's, because the dry runs restore
build scripts and dependencies compiled on `ubuntu-24.04`.

### Repository settings

[`.github/desired-settings.json`](../.github/desired-settings.json) records
the GitHub settings the owner chose: merge methods, branch deletion, Actions
permissions, the security features and both rulesets. To see whether GitHub
still matches it, run:

```sh
python3 -I -B scripts/github-drift.py
```

It reads the live values with `gh api` and prints each difference, and
changes nothing. It exits 0 when they match and 1 when they differ. Some
values need an admin's `gh` login to read. To change a setting, change it
on GitHub and in the file in the same pull request.

## Releases

release-please keeps a release pull request open on `main`, built from the
Conventional Commits titles of merged pull requests
([`release.yml`](../.github/workflows/release.yml)). Merge the release pull
request as soon as its checks pass; merging it tags and publishes the
release. While its checks run, hold other merges so they don't restart the
checks. The project's release app opens the release pull request, so its
checks run like any other pull request's.

Each release adds its golden stores (ADR 0015) and its frozen event-type
list to the tests. Both are made from the tag, so the release pull request
can't hold them, and the tests never ask them of the version `main` carries
(`.release-please-manifest.json`, `Cargo.toml`): only of older releases.
Publishing a release changes no commit, so it turns no pull request red
(issue #566). The next release pull request raises the version, and from
then on it fails until the previous release's stores and list are in.

After publishing, the release workflow's `release-fixtures` job runs
`bash scripts/release-fixtures.sh VERSION` on the tag, and
`release-fixtures-pr` opens a draft pull request with the result. The
script runs `scripts/golden-stores.sh VERSION`, which builds `langspan` from
the tag with `--locked` and runs that tag's demonstration with it; writes the
stores' state snapshots with `LANGSPAN_UPDATE_GOLDEN=1` on
`tests/golden_stores.rs`; copies the sorted `type` of each
`fixtures/*-event-v10.json` into `fixtures/event-types/VERSION.txt`; and runs
`python3 scripts/gen-docs.py`. To finish the pull request, add the release's
row to `RELEASES` in `tests/protocol.rs` with the capabilities its requests
name, review the new snapshots, and mark it ready. If the job fails, run the
script by hand at the tag (`git checkout vVERSION`) and open the pull request
yourself, saying how the stores were made.

`every_release_has_a_golden_store_directory` fails for a release older than
the current version with no `tests/fixtures/store-VERSION/`, whether or not
it changed the event set or the schema. `every_published_release_has_a_frozen_list`
fails for one without `fixtures/event-types/VERSION.txt` or a `RELEASES`
row, and for a list without a row. Neither reads the tags, so a run gives the
same result with or without them.

Before 1.0, a breaking change (`feat!:`) raises the minor version.
[CONTRIBUTING.md](../CONTRIBUTING.md) lists how each title type affects the
version.

Each build job runs `scripts/demo-two-developers.sh` on its target's
binary, so a failure on any target stops the release while it is a draft.
After publishing, `verify-published` downloads each target's archive on that
target's runner, checks `SHA256SUMS` and the attestations, installs it with
the published `install.sh` and runs the demonstration again.

### Withdraw a release

When `verify-published` fails, the `withdraw` job marks the release a
prerelease, makes the previous release latest again and opens an issue that
links the run. The install script's `latest` URL then serves the previous
release. For a fault found later, do the same by hand:

1. `gh release edit vX.Y.Z --prerelease --latest=false`, then
   `gh release edit vPREVIOUS --latest`.
2. Put a first line in the release notes saying it is withdrawn, why, and
   which version to use. If it changed the store's schema marker, say that
   the earlier version can't read a store this one wrote, and fix forward.
3. Keep the release and its tag: pinned installs and the golden stores
   refer to them.
4. Land the fix with a `fix:` pull request and merge the next release pull
   request as usual. Close the issue with a link to the fixed release.

If the failure was the runner or the network, re-run the failed jobs; once
they pass, `gh release edit vX.Y.Z --prerelease=false --latest` and close
the issue.

## Metrics

Review every metric in each status report. The metrics come in three
groups.

The four DORA (DevOps Research and Assessment) metrics:

- **Deployment frequency:** how often a release is published.
- **Lead time:** the time from a pull request's merge to the release that
  contains it.
- **Change failure rate:** the share of merges after which the check on
  `main` fails.
- **Time to restore:** the time from `main` failing its check to `main`
  passing again.

Flow:

- **Throughput:** the number of pull requests merged in the period.
- **Cycle time:** the time from opening a pull request to merging it.
- **Work in progress:** the number of open pull requests, against the limit
  of 5.
- **Aging:** the age of each open pull request, against the one-hour aim.
- **CI feedback time:** the median and 90th percentile time from push to
  result for the fast lane on pull requests, against its 3-minute target,
  and for the full lane in the merge queue, against its 8-minute target.

Quality:

- **Bugs per feature:** the number of `bug` issues filed for each merged
  `feat:` pull request.
- **Escaped defects:** the number of bugs found after a release that
  contains the faulty change.
- **Flaky tests:** the number of tests that failed only sometimes in the
  period.
- **Open review follow-ups:** the number of open "Review follow-ups from #N"
  issues.
