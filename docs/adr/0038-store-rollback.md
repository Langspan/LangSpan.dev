# ADR 0038: Rolling back a build, and the compatibility policy

**Status:** Implementation decision, 27 November 2026
([issue #333](https://github.com/Langspan/LangSpan.dev/issues/333)). Amends
the rollback sentence of [ADR 0025](0025-distribution-lifecycle-and-store-beyond-the-first-release.md)'s
I5 and adds to [ADR 0015](0015-version-tolerance.md).

## Context

The usage guide said the store "stays readable by the earlier build while the
schema marker is unchanged". It isn't: the marker has been 4 since 0.2.0 while
event kinds were added, and events are read strictly (ADR 0015), so an earlier
build stops at the first event kind it doesn't know, with a parse error that
lists every kind it does know. A new build records a new kind within minutes,
so "keep the previous archive" stopped working soon after an upgrade, and
nothing told the owner what to do.

Two choices: bump the schema marker with every new kind, so every earlier
build refuses every store a later build opened; or record the newest build
that opened the store, and refuse only when the earlier build actually can't
read it.

## Decision

**The store records the newest build that opened it.** A one-row table,
`newest_writer`, holds that build's release version. Opening raises it to this
build's version and never lowers it. It is a table, not a schema change: the
marker stays 4 and builds without the table ignore it. Nothing else in the
write path changes.

**An earlier build that can read the store opens it.** Rolling back works for
as long as the later build has written nothing the earlier one can't read.

**An earlier build that can't read it refuses with one line.** When the
events or the snapshot don't read or apply and the store names a newer build, the
store doesn't open and the coordinator reports:

```text
open coordination store: this store was last written by LangSpan 0.6.0 and holds records this build (0.5.2) cannot read; start LangSpan 0.6.0 or later on this state directory, or move the state directory aside to start with an empty store
```

Without a newer build named, the error is the existing one naming the
sequence that failed: the store is damaged, not newer. No event is appended
and nothing is reset in either case. Tests (`tests/store.rs`):
`an_earlier_build_refuses_a_newer_store_with_one_line_naming_the_build_needed`
(a recorded 99.1.0 and an event kind this build has no variant for),
`the_newest_writer_is_kept_by_an_earlier_build_and_raised_by_a_later_one`,
and `stores_written_by_earlier_releases_still_open`, which checks that a
golden store from before the table gains it.

## Compatibility policy

What a build promises to builds before and after it.

- **Clients.** The daemon answers clients of protocol version 3 to the
  current one (10), each in its own shape, and agrees on features by named
  capabilities ([ADR 0037](0037-protocol-capabilities.md)). An event a client
  can't read is left out of its pages. A client of this build ignores fields
  it doesn't know in responses ([ADR 0015](0015-version-tolerance.md)).
- **The store is forward-compatible, and backward only while readable.**
  Every build opens every store an earlier release wrote; the golden stores in
  `tests/fixtures/store-VERSION/` hold that. An earlier build opens a later
  build's store until the later build writes an event kind or field it lacks;
  from then on, a build with this ADR refuses with the line above. Releases up
  to 0.5.1 have no `newest_writer` table and can't be changed: they stop with
  `invalid persisted event at sequence N` and a list of known kinds. In both
  cases, roll back by starting the later build again, or by stopping the
  coordinator and moving the state directory aside, which starts an empty
  store and keeps the old history in the moved directory.
- **CLI output.** `--json` output, exit codes and the hook replies are the
  stable interface; a change to them is a breaking change in the release
  notes. Human-readable text is pinned in `tests/golden/` so every change is
  reviewed, but may change in any release.
- **`config.json`.** Every release reads it with unknown keys rejected, so a
  key written for a later build stops an earlier one from starting; remove the
  key to roll back. A key is retired by first accepting and ignoring it for at
  least one release, with a line in the release notes, and only then
  rejecting it.
- **Release notes.** A release that adds or changes a protocol capability or
  version, an event kind or a stored field, or the schema marker says so in
  its notes, since each limits which earlier build can read what it writes.

## Consequences

- Rolling back is safe to try: the earlier build either opens the store or
  says which build to start instead.
- The refusal covers events and the snapshot. A receipt an earlier build
  can't read is still skipped (ADR 0015).
- A build with no release version (not `X.Y.Z`) never records itself.
- Releases up to 0.5.1 still fail with a parse error on a newer store; the
  usage guide says so.
