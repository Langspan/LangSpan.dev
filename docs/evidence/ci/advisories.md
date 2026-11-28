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

PENDING: the dispatched run.
