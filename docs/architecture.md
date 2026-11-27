# Architecture and decisions at a glance

How LangSpan is built and which decision records shaped it. This page holds
the descriptive project context that used to sit in [AGENTS.md](../AGENTS.md);
the rules agents must follow stay there. For current verification, prefer
[implementation status](IMPLEMENTATION_STATUS.md); the history in the
[decisions](#decisions-by-milestone) section records what was true when each
milestone merged.

## Shape of the product

The implementation is a headless tool that fits existing agent sessions, with
CLI and MCP access and an optional UI. A solo developer with several agents is
a useful starting case; the broader product outcome is several developers
contributing to related work with their own agents. Awareness of uncommitted
and unpushed local work is a requirement; it does not prescribe a particular
shared transport.

## Components

The implementation is Rust: a pure `langspan-core` crate, a single SQLite
event/receipt store, a Unix-socket daemon, the `langspan` CLI and an
official-SDK stdio MCP adapter. [ADR 0001](adr/0001-local-coordination.md)
records the local foundation. The derived source index lives in
`crates/langspan-graph`.

Wire protocol is version 10 (version 9, 8, 7, 6, 5, 4 and 3 clients are still
answered, in their shapes; ADR 0004, ADR 0007, ADR 0008, ADR 0019, ADR 0020,
ADR 0023; each version maps to a set of named capabilities, and a new wire
field adds a capability, not a version, ADR 0037) and the SQLite schema
marker version 4 (retention, J7 in
[ADR 0025](adr/0025-distribution-lifecycle-and-store-beyond-the-first-release.md));
older persisted records remain readable. An earlier build reads a later
build's store until it meets a record it doesn't know, then refuses with one
line naming the build needed; the
[compatibility policy](adr/0038-store-rollback.md#compatibility-policy) says
what each interface promises across builds.

## Decisions by milestone

Moved unchanged in substance from AGENTS.md on 26 November 2026. Later
evidence supersedes some of the pending items; [implementation
status](IMPLEMENTATION_STATUS.md) and the
[detailed product reference](reference-overview.md#current-availability) hold
the current state.

- The M2 Claude Code adapter covers Edit and Write
  ([ADR 0002](adr/0002-claude-code-adapter.md)).
- M3 added shared typed hook identities, a Codex `apply_patch` adapter,
  linked-worktree registration, Linux process observations, a bounded Git
  observer, and an MCP tool, `langspan_changes`.
  [ADR 0003](adr/0003-native-coordination-observation.md) records these
  choices and their limits.
- [ADR 0004](adr/0004-automatic-peer-briefing.md) adds automatic peer
  briefings: session-start and prompt hooks register each session and place
  bounded, relevant peer work in the model's context, also available as
  `langspan brief` and the MCP tool `langspan_brief`.
- [ADR 0005](adr/0005-source-versions-and-stale-writes.md) records source
  comparisons: stale edits are refused once per changed version, while
  unrecorded versions can proceed.
- [ADR 0006](adr/0006-remote-awareness-through-git-refs.md) adds opt-in
  cross-machine awareness through LangSpan-owned refs on the Git remote.
- [ADR 0007](adr/0007-check-evidence-integration-and-landing.md) binds checks
  to exact trees and lands only on current evidence.
- [ADR 0008](adr/0008-derived-source-index.md) adds a derived, name-matching
  source index with interface-change notices.
- [ADR 0033](adr/0033-gemini-cli-adapter.md) adds a Gemini CLI adapter built
  from Gemini CLI 0.70.0's documentation and source; its removal is tracked in
  [#246](https://github.com/Langspan/LangSpan.dev/issues/246).

When M3 and M4 merged, the record of pending acceptance read:

- The Codex fixtures were source-derived against an unverified alpha-host
  contract; mixed-host and linked-worktree acceptance was pending,
  independently of unit tests and CI. A real Codex run on 25 November 2026
  has since been recorded ([Codex real-host
  evidence](evidence/codex-real-host/README.md)).
- Briefing delivery was observed on Claude Code only, with Codex delivery
  awaiting the owner's hook-trust approval.
- Claude completed-Read capture had native evidence; LangSpan's own
  stale-denial path still needed live host verification.
- Hosted remotes and two-machine runs were unverified.
- Checks, landing and source-index use still needed native-agent evidence.
- Gemini CLI fixtures were source-derived, and live Gemini acceptance was
  pending.

## The first useful result

The immediate work is to verify Claude Code and Codex together in one checkout
and in linked worktrees, including conflicts, unrelated work, ordinary
permission behavior, post observations and bypass edits. Retain actual
native-host evidence; parser fixtures and manually issued CLI calls establish
narrower results. The product benchmark starts alongside this work.

The broader product proof is two developers' existing agents coordinating
across machines without their owners relaying context. Shared facts must
reach the receiving agent before it chooses the relevant edit, with an
observable change in approach. A successful status query alone does not
establish that outcome. The [mission](mission.md#what-success-means) defines
the measures.
