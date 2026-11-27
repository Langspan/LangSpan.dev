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

The daemon answers only its owner's user, and every command is either
coordination or an owner grant, the commands that act with the owner's
authority beyond the coordination state
([ADR 0039](adr/0039-caller-authority.md)).

### Module map

The `langspan` crate is moving from flat top-level modules into namespaces,
grouped by capability, in four layers. A namespace may depend on namespaces in
lower layers. Within one layer it may depend only on the namespaces listed
below. `src/lib.rs` keeps one block per namespace, and
[`scripts/ratchets/namespaces.tsv`](../scripts/ratchets/namespaces.tsv) gives
each of today's modules its layer. An edge that breaks these rules is recorded
debt in that module's ratchet baseline
([ratchets](development-process.md#ratchets)). A module that has moved keeps
its old path as a re-export in the compatibility block at the end of
`src/lib.rs` until its importers are repointed.

| Layer | Namespace | Holds |
| --- | --- | --- |
| utility | `text` | text helpers and redaction |
| | `ids` | ids, `sha256`, `registration` |
| | `time` | `clock`, `local_day`, `zone` |
| | `protocol` | the wire protocol and page budgets |
| | `regions` | `regions`, `breaks` |
| | `hosts` | the host catalog and the hook's port types: `vendor`, `tool_names` |
| infrastructure | `fs` | `local`, `file_state`, `input` |
| | `git` | `git`, `sha1`, `observer`, `remote` |
| | `process` | child processes |
| | `ipc` | `client`, `autostart` |
| | `store` | the SQLite event store |
| | `graph` | the source index: `graph`, `index_file` |
| | `shell` | shell command parsing and snapshots |
| | `install` | `install`, install detection (today's `hosts`), `user_install`, `guidance`, `service`, `binary_version` |
| application | `ownership` | `journal`, `lines`, `reformat`, `formatter`, `adopt`, `commit` |
| | `checks` | `integrate`, `landing`, `rewrite`, `auto_check`, `test_output`, `definitions` |
| | `briefing` | the briefing and its render: `since`, `recent`, `trail` |
| | `person` | `today`, `count`, `report`, `status_line`, `notify`, `dashboard` |
| | `hook` | the hook pipeline and its host adapters: `claude_code`, `codex`, `cursor`, `antigravity`, `gemini_removed`, `between_steps`, `spool`, `wait` |
| interface | `daemon` | the daemon |
| | `mcp` | the MCP adapter |
| | `cli` | the CLI, `demo`, `skill` |

The edges allowed within a layer are: `regions` to `text` and `ids`;
`hosts` to `regions`; `git`, `ipc`, `shell` and `install` to `fs`; `shell`,
`store` and `graph` to `git`; `install` to `ipc`; `briefing` to `ownership`;
`person` to `briefing`; `hook` to `ownership` and `briefing`; and `cli` to
`mcp` and `daemon`. Only `daemon` may name `store` and `graph`, and only `cli`
may name `hook`, `daemon` and `mcp`. A host adapter in `hook` depends only on
the `hook` root, `hosts`, `regions`, `ids`, `text` and `fs`.

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
