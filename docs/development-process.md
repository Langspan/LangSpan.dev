# Development process

How work moves from the backlog to a release in LangSpan, for every person
and agent working here. [Engineering practice](engineering.md) owns how to
build each change; this document owns how changes are chosen, limited,
reviewed, merged and released. The owner approved it on 2026-11-26.

## Backlog

Every item of work is one GitHub issue. Labels on the issue carry its type,
priority and size:

- **Type.** Label a capability `roadmap` and a defect `bug`. The
  [roadmap index](roadmap.md) lists the open `roadmap` issues by area.
- **Priority.** The MoSCoW labels are the source of truth for priority:
  `moscow: must`, `moscow: should`, `moscow: could` and `moscow: later`.
  Take a `must` before a `should`, and so on down.
- **Size.** Set `size: S`, `size: M` or `size: L` when work starts. Compare
  the size with the actual cycle time when the pull request merges, to
  calibrate later estimates.

If a decision is costly to reverse, record it in an ADR in
[`docs/adr/`](adr/0001-local-coordination.md) before or with the change.

## Work in progress

Limit open work so that each pull request merges quickly:

- Keep at most 5 pull requests open at once. Release pull requests don't
  count.
- Keep at most one open pull request per hot component. The hot components
  are the following:
  - the wire protocol: `src/protocol.rs` and its clients;
  - briefings;
  - installation: `src/install.rs`, `src/user_install.rs` and
    `src/hosts.rs`;
  - the CLI argument list in `src/main.rs`;
  - the source graph.
- Give at least one of the 5 slots to bugs, review follow-ups or flaky
  tests. This slot is the debt slot.
- Aim to merge each pull request within an hour of opening it. If a pull
  request is older than that, attend to it before you start new work.

## Definition of Done

A pull request is done when all of the following are true:

- Its tests were written first, and they pass.
- `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets --locked -- -D warnings` are clean.
- The docs and the [roadmap index](roadmap.md) match the change.
- CI is green on Linux and macOS against the latest `main`.
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

Pull requests merge through GitHub's merge queue. The queue tests each pull
request on top of `main` and every pull request queued ahead of it, under
the same required checks (Linux, macOS, Workflows and Content), and merges
it only if they pass. A pull request whose checks fail there leaves the
queue, and the ones behind it are tested again without it. The Content
check on a queued commit checks the tree only; the pull request's title and
body passed it before the pull request was queued.

While the Linux check on `main` fails, merge only the fix for that failure.

### Which tests run when

The Rust checks ([`check.yml`](../.github/workflows/check.yml)) run
`scripts/check.sh` on Linux, then on macOS. What they run depends on the
event:

- **A pull request** runs the whole suite, except for the benchmark
  harness's dry runs (the `dry_run_*` tests in `tests/benchmark_harness.rs`).
  Those run only when the change touches one of the following paths:
  `benchmarks/`, `tests/benchmark_harness.rs`, the daemon, hook and
  briefing code they drive (`src/daemon.rs`, `src/hook.rs`, `src/hook/`,
  `src/claude_code.rs`, `src/recent.rs`, `crates/langspan-core/src/lib.rs`),
  or what builds and runs the suite (`Cargo.toml`, `Cargo.lock`,
  `rust-toolchain.toml`, `check.yml`, `scripts/check.sh`,
  `scripts/test-foreign-home.sh`). The dry runs took about 11 minutes of
  an 18-minute macOS job. A pull request that changes only Markdown builds
  nothing.
- **A merge-queue commit** follows the pull request's rules, judged on
  every change from the queue's base on `main` to that commit, so a
  benchmark change in a pull request queued ahead runs the dry runs.
- **A push to `main`** that adds one commit, as a squash merge does,
  follows the same rule for the dry runs. A push of several commits runs
  them, and it always builds and runs the release binary.
- **The nightly run** (03:23 UTC on `main`) and **a manual run** run
  everything. If the nightly run fails, it opens an issue titled "Nightly
  Rust checks on main failed", or comments on that issue while it is open.
  Treat that issue like a failing check on `main`.

When a dry run is skipped, the test log shows
`skipped: no benchmark or briefing change`. A local `sh scripts/check.sh`
runs every test unless `LANGSPAN_BENCH_DRY_RUNS=skip` is set. The workflow
owns the path list; change the list there and here together.

If a test fails only sometimes, open an issue and land a deterministic fix
the same day. Never rerun the job and move on.

## Releases

release-please keeps a release pull request open on `main`, built from the
Conventional Commits titles of merged pull requests
([`release.yml`](../.github/workflows/release.yml)). Merge the release pull
request as soon as its checks pass; merging it tags and publishes the
release. While its checks run, hold other merges so they don't restart the
checks. The project's release app opens the release pull request, so its
checks run like any other pull request's.

After a release, add its golden stores (ADR 0015): build `langspan` from
the new tag with `--locked`, run `bash scripts/golden-stores.sh VERSION BINARY`,
write the stores' state snapshots with `LANGSPAN_UPDATE_GOLDEN=1` on
`tests/golden_stores.rs`, and say in the pull request how the stores were
made. `every_release_has_a_golden_store_directory` fails for a changelog
version with no `tests/fixtures/store-VERSION/`, whether or not the release
changed the event set or the schema.

Before 1.0, a breaking change (`feat!:`) raises the minor version.
[CONTRIBUTING.md](../CONTRIBUTING.md) lists how each title type affects the
version.

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

Quality:

- **Bugs per feature:** the number of `bug` issues filed for each merged
  `feat:` pull request.
- **Escaped defects:** the number of bugs found after a release that
  contains the faulty change.
- **Flaky tests:** the number of tests that failed only sometimes in the
  period.
- **Open review follow-ups:** the number of open "Review follow-ups from #N"
  issues.
