# ADR 0015: Mixed versions: what an older or newer build must still read

**Status:** Implementation decision, 24 November 2026. The principle: a hook
or server of another version keeps working with what it can read.

## Context

Two places read data that another build of LangSpan wrote:

- **The store.** A coordinator restarted on a new build replays the event log
  written by the old one. `recover` failed closed on any event or receipt it
  could not parse, so one unreadable receipt stopped the coordinator from
  starting at all. That happened once already: a receipt written before a
  field became required (fixed by giving the field a default).
- **Teammates' publications.** A teammate may run a newer build. The received
  `state.json` was parsed with `deny_unknown_fields`, so a newer peer that
  added any field was silently not heard by every older one.

## Decision

**Events stay fail-closed.** Events are the state; replaying around one that
does not parse would build a state that never existed. A store whose events do
not all parse does not open, and says which sequence failed.

**Receipts that do not parse are skipped.** A receipt only answers a retry of
its own request. One this build cannot read is counted at open, logged once to
stderr (`receipts_unreadable`, with the count), and otherwise left as it is. A
retry of its request is answered with the error `receipt_unreadable`, telling
the client to send the operation with a new request ID; it is never executed
again under the same ID, since that request may already have taken effect. The
dashboard's denial search skips such receipts. Test:
`a_receipt_this_build_cannot_read_is_skipped_and_only_its_request_is_not_replayed`
(`tests/store.rs`), which failed before the change (the store did not open).

**Golden stores.** `tests/fixtures/store-0.1.0/` holds the two coordinators'
state files from a run of `scripts/demo-two-developers.sh` on release 0.1.0
(compacted with `VACUUM INTO`): 115 events of 27 types and 177 receipts,
including publication, checks, landing and push-turn events. Every later build
must open them (`stores_written_by_earlier_releases_still_open`). Renaming the
`turn_renewed` event made that test fail. A release that changes the event
format adds a directory of its own stores; earlier ones are never replaced.
Event types the demonstration does not produce (for example `scan_failed`,
`message_sent`, `checkout_changed`) are not covered by these fixtures.

**Peers' publications ignore fields this build does not know.** The receiving
side parses into its own structures without `deny_unknown_fields` and keeps
only the fields it knows. Everything else holds as before: at most 16 KiB read
from the blob before parsing, the payload version must be 1, the publication
must name the participant its ref names, and the core validates every value it
keeps (names, sizes, paths, branch names) in `ObserveRemote`. A change that
removes or redefines a field bumps the payload version, which an older build
does not read. What this build writes is unchanged, and the internal protocol
and the event store keep rejecting unknown fields. Test:
`a_teammates_publication_with_fields_this_build_does_not_know_is_still_shown`
(`tests/remote.rs`): a publication with extra fields at three levels is
received and briefed; one over 16 KiB and one with a 33-byte host name are
not. It failed before the change.

## Consequences

- A coordinator restarted on another build starts whatever its receipts hold;
  only retries of unreadable receipts' requests are refused.
- A field a newer peer adds is invisible to an older one until it upgrades.
- The golden stores are binary SQLite files in the repository, 188 KiB in
  all; they hold temporary demo paths and no personal data.
