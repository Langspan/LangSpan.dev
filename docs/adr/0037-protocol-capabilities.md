# ADR 0037: Named protocol capabilities instead of one version per feature

**Status:** Implementation decision for
[issue #283](https://github.com/Langspan/LangSpan.dev/issues/283),
26 November 2026. Changes how [ADR 0015](0015-version-tolerance.md)'s mixed
versions reach the local protocol. Tested with a real daemon and real
sockets; not yet observed between two installed releases.

## Context

Every feature that added a wire field bumped the single protocol version.
On 26 November 2026 four pull requests took versions 6, 7, 8 and 9 within
hours. Each one that merged second had to resolve conflicts in
`src/protocol.rs`, its clients, golden vectors and docs, and renumber. A
field added without a bump broke older clients: issue #258 added
`symbol_findings` to a check record and to `record_check` while the
protocol was at version 5, so a version 3 to 5 client cannot read a reply
that carries findings (issue #265). One integer is a resource every feature
competes for.

Clients and the daemon read their structures with `deny_unknown_fields`,
so a field the other side does not know fails the whole message.

## Decision

**The version changes only for a breaking change**: a field removed or
redefined, or a capability withdrawn. It stays 10. Versions 3 to 10 are
frozen shapes: each keeps exactly the fields it had.

**A new wire field adds a named capability.** `CAPABILITIES` in
`src/protocol.rs` is one const array, one line per capability, in name
order:

| Capability | Fields it gates | Since version |
| --- | --- | --- |
| `anchored_notes` | a note's `anchor`, `anchor_state` and `tags`; `remember`'s `symbol` and `tags`; `recap`'s `tag` (roadmap E1) | 9 |
| `briefing_memory` | a session activity's `told`, `told_at_ms` and `context`; a briefing's `unchanged`; a delivery's `told` and `told_at_ms`; the `context_added` and `told_forgotten` events; `brief`'s `changes_only` (issue #222) | 7 |
| `inflight_partial` | a briefing's `inflight_partial` (issue #262) | 8 |
| `interface_notices` | a briefing's `interface_notice_ids` (issue #214) | 5 |
| `landing_rewrites` | a landing's `rebased` and `adapted`, in the landing and in `record_landing` (roadmap F5) | 6 |
| `outline_reads` | a session activity's `outlined` and the `read_outlined` event (roadmap A11) | 10 |
| `refusal_outcomes` | a report's `refusal_outcomes` (roadmap H9) | 5 |
| `renames` | the rename fields of interface changes, history entries and in-flight symbols, and private interface changes (roadmap D7) | 4 |
| `symbol_findings` | a check record's `symbol_findings`, and `record_check`'s (issue #265) | 6 |
| `trails` | a briefing's `trail` and a recap's `trails` (roadmap C9) | 4 |

The last column is the table that maps versions 3 to 10 to capability sets:
a client of version V that names no capabilities has every capability whose
"since" is at most V. Each frozen version keeps its exact shape through it.
A capability added from now on has no "since" (`None` in the array): only a
client that names it gets it.

`symbol_findings` gets version 6, the first version whose every build knows
the field. A version 3 to 5 client is now answered without findings. A
version 5 build made after #258 also knows the field; it loses only advisory
findings, never a check's result.

**The handshake.** A request may carry `capabilities`, a list of names.
`Request::new` names every capability this build knows, so no call site
changes. The daemon grants the intersection with its own table and ignores
names it does not know. When it grants fewer than the request named, the
response carries `capabilities`: the granted names. When it grants all of
them the field is left out, so a client and daemon of the same build
exchange the same bytes as before. A request without `capabilities` is
granted its version's set. The daemon answers only with names from its own
table, never with text the client sent.

**The fallback.** The client sends every request as `Request::new` builds
it. Two answers make it ask once more:

- A daemon of an earlier version refuses the request, with
  `unsupported_version` or, since it rejects the unknown `capabilities`,
  `invalid_request`, in its own version. The client asks again in that
  version, without `capabilities`, reshaped to that version's set.
- A daemon of the same version answers `invalid_request` and grants fewer
  capabilities than were named. The client asks again with the granted
  names, reshaped to them.

A request that never parsed never took effect, so asking again cannot
apply it twice. The daemon answers such a request without its request ID,
because it never read one; the client takes an error without a request ID
as the answer, and still refuses any other answer whose ID differs. Reshaping drops what the daemon cannot read when that is
safe: `changes_only` in a briefing, findings in `record_check` (advisory;
the check's result is recorded unchanged). It refuses, before sending, what
would lose meaning: a rebased or adapted landing, a note's symbol or tags,
and a recap by tag (`protocol::fits_with`). A version 10 daemon built before
this change rejects `capabilities`; restart it after upgrading. No release
shipped version 10 before the handshake.

**Golden vectors.** The existing `fixtures/*-vN.json` vectors stay
unchanged as the frozen shapes of versions 3 to 10, and every one of them
must keep passing. A capability's vectors live in
`fixtures/capabilities/<capability>/`: `with.json` is a response encoded for
a client that names it, and `without.json` the same response for one that
does not. `fixtures/capabilities/request.json` and `answer.json` pin the
handshake itself.

## Consequences

- Two features add fields in parallel by adding a line each to
  `CAPABILITIES`. The lines are in name order and carry no trailing comment
  (rustfmt would realign its neighbours), so two new names land in separate
  hunks unless no existing name sorts between them; then Git reports a
  conflict whose resolution is to keep both lines. A test checks the
  one-line-per-capability layout and that two such additions merge without
  conflict.
- Each capability's fields are still removed for a client without it by
  code in `src/protocol.rs` (`encode_with`); a new capability adds one
  `if !granted.has(..)` there, in its own hunk.
- Every request is larger by the list of names, about 200 bytes. The
  64 KiB frame bound is unchanged.
- A capability can be added but not withdrawn without a version bump.
- Event pages for an older client and tolerant readers in new clients are
  separate items of the same milestone; this decision does not change them.
