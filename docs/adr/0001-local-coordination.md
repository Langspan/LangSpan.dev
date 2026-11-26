# ADR 0001: First local coordination implementation

**Status:** Implemented by the first build slice, following the owner's instruction
on 24 November 2026 to begin implementation. This chooses the minimum needed to
build; it does not select every future milestone from any one proposal.

## Context

LangSpan is a headless coordination tool. The repository contains three useful
implementation proposals, but no running coordination loop. The shared engineering
framework requires one contract, a pure core, explicit time, durable retry
identities, and observable recovery. The owner has authorized implementation.

## Decision

- Use Rust, with a pure `langspan-core` crate and OS/SQLite/CLI/MCP adapters in the
  root package. Use the command name `langspan`. The first executable combines
  the daemon, CLI and stdio MCP entry points. A separately measured hook binary
  remains a later packaging choice.
- Use one local SQLite file, WAL and `synchronous=FULL`. The daemon holds an
  exclusive lifetime file lock and owns all writes. Persist an event sequence
  and the original command/outcome receipt in the same transaction before
  publishing a mutation or acknowledging it. `FULL` is chosen for the receipt
  acknowledgement contract; no latency figure from the research is claimed for
  these durable commits.
- Require a caller-supplied request ID for mutations. Persist conflicts and core
  rejections too. Identical retries return the original outcome. Reusing an ID
  for different command content is an error. Current lease validity is separate
  from the historical outcome. Path/transport validation failures occur before
  the transaction and do not reserve a request ID.
- Register physical workspace roots, logical session identities and advisory
  file-scoped intents. Admit a complete sorted set of file keys atomically or
  return its conflicting holders. Intent does not reserve a file. Completion
  and expiry release admission independently of intent.
- Use process-monotonic time for lease deadlines. Persist a new generation at
  daemon startup; invalidate outstanding grants and mark prior sessions/intents
  stale by generation. Re-register a session and explicitly refresh its intent
  to reattach. Retain receipts and events across restart.
- Resolve existing symlinks and nearest existing parents for new files. Reject
  escapes, overlapping registered roots, special files and hard-linked files.
  This narrows alias ambiguity; it does not fence filesystem operations.
- Use owner-only Unix sockets, a bounded versioned JSON-line protocol, and the
  official Rust MCP SDK. Both interfaces call the same daemon operations. No
  network listener, account, credential access, telemetry or UI is required.
- A daemon serves only the state directory it started with. Its state owner
  checks it every second, between requests, and exits with a logged reason
  once the directory is removed, replaced at the same path, or no longer
  owner-only (issue #239). A debug build also exits after an idle period, so
  a coordinator a killed test left behind ends; a release build has no idle
  exit.

## Guarantee and failure behavior

Admission is **cooperative coordination**, not a filesystem security control.
LangSpan serializes decisions for registered participants. It cannot prevent an
editor, shell, delayed hook action, or a process using a different state directory
from writing. A request transport error or timeout is unavailable coordination,
never a grant. A positive historical outcome is usable only when the same reply
also reports `lease_valid: true`; that is a point-in-time observation, not a fence
on a later write. An operation that outlives its lease must obtain a new admission
before proceeding. Permission prompts and native hook coverage are not integrated
in this slice.

No installed security control is changed to fail open. A later vendor adapter
must document its host's failure and permission behavior before claiming coverage.

## Consequences and limits

This slice can be run and tested on Linux with two logical sessions and through
stdio MCP. These tests are not evidence of real Claude/Codex/Gemini hook sessions,
macOS signing behavior or Windows named-pipe behavior. The code uses Unix sockets;
Windows transport, signed distributions and vendor adapters remain work to do.

Events and receipts grow without compaction for now. Status and event responses
must fit the 64 KiB transport frame; oversized responses fail explicitly. An MCP
tool result carries its answer twice, once re-escaped as text, so the adapter
measures the whole encoded result against that frame and cuts a page to what
fits, saying so and naming the cursor for the rest; an MCP page can be shorter
than the CLI's. Large
history pagination/projections, retention policy and graph indexing are later
work. SQLite is for a local filesystem and local owner, not a database file on a
shared network volume. Same-user processes are inside the trust boundary.

A future graph may be rebuildable and revision-scoped; no graph facts are in this
implementation. Cross-machine intent publication, source/check evidence, code
indexing and an optional UI remain separate slices.
