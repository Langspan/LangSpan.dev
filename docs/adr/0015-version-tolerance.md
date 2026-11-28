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
not all parse does not open, and says which sequence failed. When the store
records a newer build than this one, it names that build in one line instead
([ADR 0038](0038-store-rollback.md)).

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
Every release has one, `store-0.1.0/` through `store-0.8.0/`; each was made
by `scripts/golden-stores.sh VERSION`, which builds `langspan` from tag
`vVERSION` with its lockfile and runs that tag's own demonstration with it.
`every_release_has_a_golden_store_directory` fails for a changelog version
older than the current one without a directory; the release workflow opens
the pull request that adds it (issue #566).
Event types the demonstration does not produce (for example `scan_failed`,
`message_sent`, `checkout_changed`) are not covered by these fixtures.
Each store's events, folded by the core's `apply`, must also give the state
committed beside it, `<store>.state.json`
(`every_golden_store_replays_to_its_committed_state`, `tests/golden_stores.rs`).
A synthetic log in the core's suite applies every event type
(`crates/langspan-core/tests/replay.rs`, issue #317).

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

**Clients read responses tolerantly (issue #332, 27 November 2026).** This
changes the rule above that the internal protocol rejects unknown fields,
for responses only. From this build on, a client ignores the fields it does
not know in a response, in its outcome and in the 43 objects of the core's
`lib.rs` that only responses carry (a briefing, a recap, the graph and
impact reads, and the like), and names `tolerant_responses` to say so
([ADR 0037](0037-protocol-capabilities.md)). The response objects of the
core's other modules (a report, `today`, a recall, a definition's history,
and the like) are still strict; they follow in a later change.
No wire-only mirror type is added: those core types simply no longer carry
`deny_unknown_fields`. What stays strict:

- requests, which the daemon reads, and every object a request carries;
- events, which the store replays (above), and every object an event or the
  derived state carries, including in a response, since the same types are
  stored. A response holding one of them with a field this build lacks still
  fails to read.

When an addition needs what:

- A field added to a response-only object needs no version and no capability
  of its own. A client without `tolerant_responses` (every release up to
  0.5.1) is strict, so the daemon leaves the field out for it, in the one
  place `encode_with` shapes such clients' answers.
- A field added to an object that is stored or sent in a request, a new event
  type, and a request a daemon must understand each still add a capability.
- The version changes only when a field is removed or changes meaning, or a
  capability is withdrawn. A request that needs something an older daemon
  cannot do is a capability checked before it is sent (`fits_with`), not a
  version.
- Versions 3 to 10 stay frozen: their shapes come from the capability table,
  and no function per version (`protocol_N_shape`) is added again. A test
  checks for one.

An unknown event type in a page is not yet kept as an opaque record: the
core's `Event` has no variant to hold one, and adding it needs a JSON value
type in the core. Until then a new event type gets a capability, and a
client without it is not sent the event (ADR 0037).

## Consequences

- A coordinator restarted on another build starts whatever its receipts hold;
  only retries of unreadable receipts' requests are refused.
- A field a newer peer adds is invisible to an older one until it upgrades.
- On the internal protocol, a client and daemon of different builds agree
  on named capabilities, not one version per feature
  ([ADR 0037](0037-protocol-capabilities.md)).
- A field a later daemon adds to a response-only object is invisible to this
  client, not an error. One it adds to a stored type still needs a
  capability, until those readers are tolerant too.
- The golden stores are binary SQLite files in the repository, 188 KiB for
  0.1.0 and 2.4 MiB through 0.8.0, about 280 KiB more for each release;
  they hold temporary demo paths and no personal data.
