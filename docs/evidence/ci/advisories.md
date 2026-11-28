# The advisory gate and the nightly report, shown by a dry run

Evidence for #312 (epic #304): a known advisory fails the `Linux` job, and
a failed nightly run reaches the issue through `nightly-report`. Neither
needs a vulnerable dependency on `main`. A manual run of `Rust checks` with
`advisory_dry_run` audits the real lockfile and then
`tests/fixtures/advisory-dry-run/known-advisory.lock`, which holds only
`smallvec 1.6.0` (RUSTSEC-2021-0003, fixed in 1.6.1). The same
`cargo audit --deny warnings` command and the same ignore flags from
`deny.toml` run on both.

## Locally

Run on 2026-11-27, macOS arm64, cargo-audit 0.23.0, advisory database of
that day (1571 advisories):

    $ cargo audit --deny warnings --file tests/fixtures/advisory-dry-run/known-advisory.lock
    Crate:     smallvec
    Version:   1.6.0
    Title:     Buffer overflow in SmallVec::insert_many
    ID:        RUSTSEC-2021-0003
    error: 1 vulnerability found!

The command exits with status 1.

## In CI

Not run yet. Dispatching a workflow needs the owner's approval, which the
change's author didn't have; on 2026-11-28 the dispatch was refused again
for the same reason. The `advisory_dry_run` input reached `main` in #550,
so run it there:

    gh workflow run check.yml --ref main -f advisory_dry_run=true

then record here the run's link, the `Linux build` step that failed and the
advisory it names, and the "Advisory dry run of the nightly report" issue
`nightly-report` opened (close it afterwards).

## The scheduled nightly run

On 2026-11-27 no scheduled `Rust checks` run had completed: the only one,
[46001221022](https://github.com/Langspan/LangSpan.dev/actions/runs/46001221022),
was cancelled. On 2026-11-28 at 06:27 UTC that was still the only
scheduled run: none had started for that day's 03:23 UTC schedule.
