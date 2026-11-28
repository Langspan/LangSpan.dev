# ADR 0043: The LangSpan coordination protocol, version 1

**Status:** Accepted direction. On 28 November 2026 the owner decided that
LangSpan aims to be the standard protocol for coordinating coding agents
([epic #626](https://github.com/Langspan/LangSpan.dev/issues/626)). The design
details in this record are open for review. Written 28 November 2026 for
[issue #627](https://github.com/Langspan/LangSpan.dev/issues/627). Nothing in it
is built. It builds on the wire protocol of
[ADR 0037](0037-protocol-capabilities.md), region admission
([ADR 0012](0012-region-admission.md)) and stale reads
([ADR 0005](0005-source-versions-and-stale-writes.md),
[ADR 0022](0022-a-conflicted-file-is-refused-once.md)). It changes the decision
on row J9 of [ADR 0025](0025-distribution-lifecycle-and-store-beyond-the-first-release.md)
(see [the audit log](#the-audit-log)).

## Context

LangSpan already speaks a protocol. It is the reference implementation's
private one:

- The daemon listens on an owner-only Unix socket. Each connection carries
  one newline-terminated JSON request of at most 64 KiB and one answer
  (`src/protocol.rs`, `MAX_FRAME_BYTES`).
- Wire version 10 is current. Versions 3 to 10 are frozen shapes, and a new
  field adds a named capability, not a version (ADR 0037). Golden vectors in
  `fixtures/*-vN.json` and `fixtures/capabilities/` pin the shapes.
- The pure core decides and replays. `langspan_core::decide(state, command,
  request_id, now_ms)` returns the events and the outcome without changing
  state. `apply(state, event)` folds one event into the state. The caller
  injects time and identifiers. The store has one writer and persists the
  events before they are applied.
- Admission holds files, symbols or line ranges (ADR 0012). A symbol region
  carries a key (its name below the names that enclose it) and the digest of
  its text. The digest is recorded and not used in a decision.
- A stale base is decided per file. If the file changed since the session's
  recorded version, the whole set is refused once (ADR 0005). Two sessions
  that edit different functions of one file still each re-read after the
  other's write (ADR 0012, "After one region edit completes").
- A session learns of peer work only when a hook runs or it asks.

The protocol stays private for three reasons. No document says what a
compatible coordinator must do. The conflict rules are code in
`crates/langspan-core/src/admission.rs`, not data that another
implementation can share. A tool that is not one of our hook adapters has no
supported way to take part. To become the standard, the protocol needs a
specification that someone outside the project can implement and check.

The rules this has to meet:

- Enforcement fails closed where it guards the default branch.
- No model judgment in a decision.
- Nothing leaves the machine unless the owner turns it on.
- No new network listener.
- The coordinator never needs source text.
- Existing clients and stored records keep working.

## Decision

### Scope

Protocol version 1 is the contract between a **coordinator** (the daemon, or
an embedded engine) and its **clients** (hooks, the CLI, the MCP adapter, a
TypeScript client, an editor). It covers the following:

| Area | In protocol 1 |
| --- | --- |
| Framing and the handshake | Newline-terminated JSON frames of at most 64 KiB; `version` and `capabilities` as in ADR 0037. |
| Sessions | Registration, identity and liveness. Liveness never extends or releases a lease. |
| The unit | [Versioned keys](#the-unit-versioned-keys-with-typed-changes) for symbols, line regions and files, with a base and a change kind. |
| Admission | All-or-none admission of a set of keys, leases, release and expiry, refusals with a stable code. |
| Conflict rules | The [composition table](#the-conflict-table), as data. |
| Change reports | A client's report of a completed write, with the resulting hashes and kinds. |
| Delivery | Event pages (`events --after SEQ`) and [subscriptions](#transport). |
| The log | The ordered, [hash-chained](#the-audit-log) event log and replay. |

Everything else the daemon does stays a feature of the reference
implementation, outside the core protocol. It keeps its own capabilities:
briefings and their rendering, messages, notes, checks and landing, the source
index and interface notices, remote awareness through Git
([ADR 0006](0006-remote-awareness-through-git-refs.md),
[ADR 0041](0041-cross-machine-messages.md)), the dashboard and reports. A
coordinator can conform to protocol 1 without them.

### Transport

**Keep the socket and the hooks.** The transport stays the owner-only local
Unix socket: mode `0600` in a `0700` state directory, and the daemon checks
the peer's user. Every request still works one per connection, so a hook, a
shell script or a host that cannot hold a connection takes part as it does
today. Hook-only hosts keep learning at the next hook.

**Add subscriptions.** A new command, `subscribe`, keeps its connection open
after the answer. The command names a session, a set of key patterns (an
exact key, every key below a symbol key, or every key of a file) and
optionally dependents through the source index, at most 2 edges deep. It
also names `after_seq`, the last event the client has. The coordinator then
writes one frame per committed event that touches a subscribed key, in log
order, each with its `seq` and chain digest.

The delivery rules:

- **After the commit, never inside it.** A frame goes out only after its
  event is stored. The fan-out reads committed events off the writer's
  thread, so a slow subscriber never delays a decision.
- **At least once, in order, resumable.** A client that reconnects with
  `after_seq` receives every matching event it missed. A client ignores a
  `seq` it already has.
- **Bounded.** Each subscription has a queue of at most 256 frames, and the
  daemon allows at most 64 subscriptions. When a queue fills, the daemon
  writes one `resync` frame with the last `seq` it sent and closes the
  connection. The client then reads the gap through `events --after`. Every
  write to a subscriber has the daemon's I/O timeout. A subscriber that
  misses it is closed in the same way.
- **Not presence.** An open subscription is not liveness. It neither extends
  nor releases a lease.
- **Latency.** **Target:** a frame leaves the daemon within 50 ms of the
  commit on the same machine. Nothing measured yet.

A push reaches the client process, not the model. A host puts the update in
front of the model at its own next point: a hook, an MCP result, or an
embedding that injects context. The briefing's existing per-session record of
what a session was told
([ADR 0037](0037-protocol-capabilities.md), `briefing_memory`) stays the one
place that decides what a session still needs to hear.

**Rejected for protocol 1:** a TCP, HTTP or WebSocket listener, even on
loopback. A browser client reaches the coordinator only through a host that
already runs locally as the owner and forwards frames; that bridge belongs to
the embedder, not to the coordinator.

### The unit: versioned keys with typed changes

**Keys.** Every coordinated thing is a key. On the wire a key is a
structured value, never a string to split:

| Key | Fields | Display form |
| --- | --- | --- |
| Symbol | `path`, `symbol` (1 to 8 names, outermost first, as in ADR 0012) | `code:src/store.rs::Store::open` |
| Line region | `path`, `start_line`, `end_line`, `file_hash` | `lines:docs/notes.txt:12-30` |
| File | `path` | `file:src/store.rs` |

A path is the path below the workspace root as the filesystem resolves it
(ADR 0005, "One file, one key"). A file key encloses every symbol and line
region of that file. A symbol key encloses the keys below it. The bounds of
ADR 0012 stay: at most 64 keys per request, names of 1 to 128 bytes without
control characters, lines 1 to 2,000,000.

**Versions.** The coordinator keeps, for each key it has seen, a **version**
and a **hash**:

- The version is the `seq` of the event that last recorded a change to the
  key. Only the coordinator assigns it, so it orders changes without
  comparing clocks.
- The hash is reported by the client. For a symbol, it is SHA-256 of the
  definition's lines with trailing whitespace removed (ADR 0012's digest).
  For a file, it is SHA-256 of the bytes, or `missing`. A line region uses
  its `file_hash`. Line numbers mean nothing in another version of the file,
  so a line region is current only while the file is, and its kind is
  always `replace`.

**A write's base.** A write names, for each key, the `base_version` and
`base_hash` it was made from, and a **change kind**:

| Kind | Meaning |
| --- | --- |
| `body_edit` | The definition's text changes; its name and signature stay. |
| `signature_change` | Its signature changes; the name stays. |
| `rename` | Its name changes; the signature stays. The write names the new key. |
| `add` | A key that did not exist at the base comes into being. |
| `remove` | The key stops existing. |
| `replace` | A whole-file write, or a line region; there is no finer kind. |

The client works out the kind on its own machine. It parses the file before
and after the edit, applied in memory, as the hook already does to place a
`MultiEdit` (ADR 0012). When it cannot tell, it sends `replace` for the file.
That choice can only hold more, never less.

**Staleness per key.** The coordinator compares each written key's
`base_hash` with the key's current hash:

- The hashes are equal. The key is current, whatever else changed in the
  file.
- The hashes differ. The [conflict table](#the-conflict-table) decides,
  given each change recorded for the key, or for a key that encloses or is
  enclosed by it, after `base_version`.
- The hashes differ, and no recorded change explains the difference. A
  shell command, an editor or a checkout changed the key outside any
  reported write. That is a conflict, refused once as "changed outside any
  reported edit" (ADR 0005).
- The coordinator has no record of the key. The base is unknown. The key is
  admitted and listed under `base_unknown`, as for a file today (ADR 0005).

This removes the refusal that follows every peer write to another function of
the same file. It needs no global lock. The coordinator's one writer orders
the decisions, and no client holds anything while it waits. The refuse-once
rule of ADR 0005 and ADR 0022 stays for a conflict. A refusal records the
current hash as the session's known base, so a session that cannot report
reads (Codex) is not refused for ever.

**Compatibility.** A request without the new fields is decided as today: file
versions under ADR 0005, regions under ADR 0012. Only a client that names the
`versioned_keys` capability sends a per-key base and a kind.

### The conflict table

The rules for a stale key are data: one table that the coordinator loads at
start. The table has one cell for each combination of these three values:

- the **relation** between the written key and the changed key: `same`,
  `encloses` (the changed key encloses the written one), `enclosed` (the
  written key encloses the changed one) or `disjoint`;
- the **recorded change**'s kind;
- the **stale write**'s kind.

Each cell holds one of three results:

- `commute`: the write is admitted. The answer lists what it was rebased
  over: each key, its versions from and to, the kind and the session.
- `rekey`: a rename moved the key. The write's key is replaced by the new
  key. When the write's base hash is the one the rename started from, its
  base becomes the rename's result. Then the write is decided again against
  the new key's later changes, through at most 8 renames.
- `conflict`: the write is refused with `stale_base`. The refusal carries the
  other side's change kind, session and intent.

The loader refuses a table with a gap (a missing cell) or a contradiction (a
cell given twice, or `rekey` where the recorded change is not a `rename`).
The coordinator does not start with a refused table. It never falls back to
an unchecked rule. The table's SHA-256 is recorded in the `booted` event that
starts each daemon generation, so a replay knows which rules decided it.

The first table chooses conflict wherever the composition is not known to be
safe. The cells for the `same` relation:

| Recorded change \ stale write | `body_edit` | `signature_change` | `rename` | `add` | `remove` | `replace` |
| --- | --- | --- | --- | --- | --- | --- |
| `body_edit` | conflict | conflict | conflict | conflict | conflict | conflict |
| `signature_change` | conflict | conflict | conflict | conflict | conflict | conflict |
| `rename` | rekey | conflict | conflict | conflict | conflict | conflict |
| `add` | conflict | conflict | conflict | conflict | conflict | conflict |
| `remove` | conflict | conflict | conflict | conflict | commute | conflict |
| `replace` | conflict | conflict | conflict | conflict | conflict | conflict |

The other relations follow these rules:

- `disjoint`: every cell is `commute`. A change to `Store::close` does not
  make a write to `Store::open` stale.
- `encloses`: `rekey` when the recorded change is a `rename` (renaming `Store`
  moves `Store::open` to `NewStore::open`). Otherwise the cell is `conflict`.
- `enclosed`: `conflict`. The written key's text contains the changed one.

The cells are deliberately cautious. A `body_edit` over a peer's `body_edit`
of the same symbol conflicts because the coordinator never sees text and
cannot merge it. A `body_edit` over a peer's `signature_change` conflicts
because the body may use the old parameters. A cell moves from `conflict` to
`commute` only with a test and evidence that the two changes compose. That
change edits the table file and this ADR.

Concurrent leases keep the overlap rule of ADR 0012. Two active leases conflict
when one key equals or encloses the other. The table decides only what a
**stale** write meets after the fact.

### Determinism and replay

The protocol's decisions are a pure function:

- `decide` receives the state, the command, the request ID and the time, and
  returns events and an outcome. It reads no clock, no randomness and no
  file.
- `apply` folds one event into the state. The state at `seq` N is the fold of
  events 1 to N, from a snapshot when there is one (J7 of ADR 0025).
- Iteration order is defined: keys and maps are ordered (`BTreeMap` in the
  reference), never hash order.
- The composition table and the protocol version are inputs, recorded in the
  log.

Two conforming coordinators that replay the same events answer every later
command alike. Given the same log, command, request ID and time, each returns
the same events and outcome. The spec defines the state only through those
answers, so an implementation can store it however it likes.
`langspan-core` is the reference. Golden scenario vectors record a command
sequence with its request IDs and times, then the expected events and
outcomes. A conformance suite replays them against any implementation
([#631](https://github.com/Langspan/LangSpan.dev/issues/631)).

The client's classification of a change kind is not part of this guarantee.
Two parser versions can classify one edit differently. The coordinator
decides deterministically from what it is told, and the spec says how a
client must classify.

### The audit log

[Issue #91](https://github.com/Langspan/LangSpan.dev/issues/91) adds the chain.
Each event row carries `digest`: SHA-256 of the previous row's digest and this
row's event JSON. `langspan events verify` names the first sequence that does
not match, and `langspan replay --at SEQ` rebuilds the state at a sequence
without writing to the store. In the protocol, an event page and a
subscription frame carry each event's digest under the capability
`chained_events`, so a client can check that what it received is continuous.

ADR 0025 decided "not now" for row J9: the store has one writer and is
owner-only, and a chain shows tampering without preventing it. The owner's
decision of 28 November 2026 names the threat that a chain answers. A
standard needs a record that a person, or a second implementation replaying
the same log, can check after the fact. The limits of ADR 0025 still hold.
The chain detects and does not prevent. Nobody signs it. LangSpan sends the
head digest nowhere. The daemon keeps running when the chain breaks.

### Versioning

Protocol version 1 is wire version 10 plus a named set of capabilities. No
wire version 11 is needed, because each slice adds fields, and ADR 0037 gives
an addition a capability:

| Capability | Adds | Slice |
| --- | --- | --- |
| `subscriptions` | `subscribe`, its event, `resync` frames | [#628](https://github.com/Langspan/LangSpan.dev/issues/628) |
| `versioned_keys` | structured keys, `base_version`, `base_hash`, change kinds | [#629](https://github.com/Langspan/LangSpan.dev/issues/629) |
| `composition_table` | `rebased_over`, `rekeyed`, the conflicting change in `stale_base` | [#630](https://github.com/Langspan/LangSpan.dev/issues/630) |
| `chained_events` | `digest` on events in pages and frames | [#91](https://github.com/Langspan/LangSpan.dev/issues/91) |

None of the four has a "since" version, so only a client that names it gets
it. Versions 3 to 10 keep their frozen shapes and vectors. The spec defines
**protocol 1** as wire version 10 with those four capabilities. A
coordinator conforms when it grants all four and passes the suite. A breaking
change, such as withdrawing a capability or redefining a field, becomes
protocol 2 and wire version 11.

The composition table carries its own `table_version`. A cell change that
only relaxes a conflict keeps protocol 1. The log records the table's hash,
so a replay is exact.

### Clients

1. **The reference clients.** The hooks, the CLI and the MCP adapter already
   build every request through `Request::new` and keep working unchanged.
2. **TypeScript first**
   ([#632](https://github.com/Langspan/LangSpan.dev/issues/632)). Most agent
   hosts and editors are written in it. It is a Node client over the Unix
   socket, plus a transport-agnostic build for a browser behind an embedder's
   bridge. It is published under the project's license, with the conformance
   suite in its CI.
3. **An embedding API.** A host links `langspan-core` and drives `decide` and
   `apply` with its own store. It must persist the events before it applies
   them, and it must have one writer. The conformance suite checks it.
4. Other languages follow on demand. Each one passes the same suite.

### Security

The trust model does not change:

- **Owner-only.** The socket and state directory belong to the owner, and the
  daemon checks the peer's user. A subscription is a longer connection by the
  same user. It grants nothing a request does not grant. Owner grants stay
  behind `"owner_grants": true` ([ADR 0039](0039-caller-authority.md)).
- **No source text in a decision.** The coordinator decides from paths,
  symbol names, line numbers, hashes and kinds. A client parses and hashes
  on its own machine. Names and paths are metadata taken from the
  repository. They stay on the machine unless the owner turns on sharing
  (ADR 0006). Interface notices still carry signatures for briefings, under
  their existing capability; no decision reads them.
- **Repository and peer text is data.** A symbol name, an intent or a note
  that arrives in a frame is bounded, carries a label and grants nothing. A
  client that shows it to a model shows it as data.
- **Fail closed.** A table with gaps keeps the coordinator from starting. A
  write the client cannot classify is a `replace`. Landing on the default
  branch keeps its own checks ([ADR 0007](0007-check-evidence-integration-and-landing.md)).
- **Bounded.** The limits are the frame size, the keys per request, the
  subscriptions, the queue per subscription and the I/O timeouts.

Each slice that adds or widens a boundary updates
[the threat model](../security/threat-model.md) in its own pull request:
subscriptions (#628) and the embedding API (#632). This record changes no
code and no boundary.

### Out of scope

- Merging or rewriting source text, and any coordinator that reads it.
- Model judgment in any decision.
- A network listener, a hosted service, or a transport across machines.
  Remote awareness stays on Git refs (ADR 0006, ADR 0041).
- Filesystem fencing. Admission stays cooperative, and a write outside the
  hooks can bypass it.
- Authentication beyond the operating system's user, and signing the log.
- How a host shows an update to its model, and the text of briefings.
- Windows named pipes, which follow demand (ADR 0025).

## Alternatives considered

- **Use MCP as the coordination protocol.** MCP connects one host to its
  tools. It has no shared order of events, no replay and no conflict rules
  between peers, and those would still have to be specified. MCP stays an
  adapter onto this protocol.
- **Merge text at the coordinator (operational transforms or CRDTs).** That
  needs source text at the coordinator, and it merges without review. Both
  break the rules this record has to meet.
- **A loopback HTTP or WebSocket service.** It widens the trust boundary to
  anything that can reach the port, and it needs authentication that the
  socket's user check already gives. An embedder that needs a browser bridge
  can run one.
- **Wire version 11 for protocol 1.** ADR 0037 exists to avoid one version
  per feature. Every slice here adds fields, so capabilities carry it, and
  versions 3 to 10 lose nothing.
- **Conflict rules as code.** That is today's state, and Rust's exhaustive
  `match` already catches a missing case. But another implementation cannot
  share a `match`, and nothing checks that two implementations agree. A table
  in data is one owner of the rule for every implementation, with its gaps
  checked at load.
- **Keep the per-file stale rule.** It is simple and already shipped. But
  every peer write to any function refuses every other session's next edit
  of that file, and that cost grows with the number of agents.
- **Poll the event log faster instead of pushing.** A hook runs only at a
  tool call. Polling between tool calls costs work on every client for every
  idle interval and still arrives late.

## Consequences

- An outside implementer gets a specification, a schema, vectors and a suite
  to check a coordinator against. LangSpan becomes one implementation, the
  reference.
- Sessions that edit different functions of one file stop refusing each
  other after every write, when both clients send per-key bases. Older
  clients keep today's behavior.
- The first table refuses more than a human might, because it is cautious by
  construction. Relaxing a cell needs evidence.
- The client takes on more work: it parses before and after, hashes and
  classifies. A wrong classification can admit a write that should conflict,
  but only within one key's text. A client that is unsure sends `replace`.
- A subscriber costs the daemon a queue and a connection, both bounded.
- The log gains a digest per event, and a replay records the table it used.
- Every new frame needs a JSON Schema, golden vectors and conformance
  scenarios before it counts as part of protocol 1.

## Migration path

Each slice lands in its own pull request, behind its capability, with golden
vectors and tests. The slices:

1. **Transport, [#628](https://github.com/Langspan/LangSpan.dev/issues/628).**
   `subscribe`, its frames and limits, and the `subscriptions` capability,
   with a threat-model update. Done when a subscribed session receives a
   peer's change without polling, in an integration test with two real
   sessions.
2. **The unit, [#629](https://github.com/Langspan/LangSpan.dev/issues/629).**
   Structured keys, per-key versions and hashes, change kinds classified in
   the hook, and the `versioned_keys` capability. A request without it is
   decided as today.
3. **Conflict rules, [#630](https://github.com/Langspan/LangSpan.dev/issues/630).**
   The table file, its loader with gap and contradiction checks, a test for
   every cell, and `rebased_over` and `rekeyed` in the answers. Starts after
   #629.
4. **Audit log, [#91](https://github.com/Langspan/LangSpan.dev/issues/91).**
   The chain, `events verify`, `replay --at`, and `chained_events` in pages and
   frames. It is independent of 1 to 3.
5. **Spec and conformance, [#631](https://github.com/Langspan/LangSpan.dev/issues/631).**
   The normative spec, a JSON Schema per frame, scenario vectors from
   `langspan-core`, and a suite that runs against LangSpan and any other
   coordinator. Starts after 1 to 4 have their frames.
6. **Reach, [#632](https://github.com/Langspan/LangSpan.dev/issues/632).** The
   TypeScript client and the embedding API, each with the suite in its CI.
   Starts after #631.

The token cost work
([#624](https://github.com/Langspan/LangSpan.dev/issues/624)) is a child of the
same epic and runs alongside, with no dependency on this order.
