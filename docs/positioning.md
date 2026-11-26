# Product positioning

The public messaging contract for LangSpan, updated 26 November 2026 for v0.2.0.
Use it for the website, repository descriptions, READMEs, organization profile,
and project references. The [mission](mission.md) owns the product hypothesis.
[Implementation status](IMPLEMENTATION_STATUS.md), the published release, and
retained evidence determine which capabilities and outcomes can be claimed.

## Message hierarchy

- Category: The open-source coordination layer for parallel agentic development.
- Problem: Parallel execution is easy. Coordinated development is hard.
- Product headline: Run agents in parallel. Build as a team.
- Brand line: Your agents. On the same page.
- Differentiation: Keep your tools. Connect their work.
- One sentence: LangSpan brings relevant work into existing coding-agent
  sessions, coordinates supported local edits, and shares opt-in work
  updates across machines. Verify combined changes against the exact Git
  tree before landing.

## The problem and the layer

Worktrees isolate checkout contents, not the meaning of changes. One agent
can rename a function while another adds a caller using its old name. The
branches can merge without a textual conflict and fail the combined checks.
Shared notes can help, but agents must maintain them and read them at the
right time. Manual updates make people carry context between sessions.

LangSpan complements agents, Git, branches, worktrees and review. It is not a
replacement for them or a general-purpose agent orchestrator. It brings
relevant briefings into supported events, checks supported local edits,
tracks session-attributed changes and records check evidence against an exact
tree. People choose priorities and review the result.

The [semantic example](https://langspan.dev/#why) reproduces a clean Git merge with a failing
combined check, without a model or LangSpan. Its proposed briefing-assisted
recovery is an illustration, not an observed outcome.

## Semantic awareness within coordination

The coordination layer uses source relationships as well as file activity.
The v0.2.0 local Tree-sitter index extracts definitions, name mentions,
imports and signatures from Rust, Python, TypeScript/JavaScript, Go, Java and C#.
It matches names, not resolved scopes, types or imports. Interface notices
can cross linked worktrees on the same machine, including relevant
uncommitted exported signatures. Related sessions can receive a notice at
their next prompt. Supported Claude Code and Codex writes can receive
advisory context about a changed function's argument count with the tool
result, without an edit refusal on that basis.

The [product illustration](https://langspan.dev/#semantic-awareness) uses a
signature change, not an inferred rename. The
[released-binary fixture](https://github.com/Langspan/langspan.github.io/blob/main/review/source-awareness-proof.mjs)
exercises the actual advisory response with constructed hook events, real
Git and the local coordinator. No model runs. It does not prove that an
agent changes its approach or automatically repairs the caller.

Use the four-part architecture to explain the responsibilities: existing
agent execution, semantic awareness, coordination, and verification and
integration. The middle two belong to LangSpan; checks bind to exact Git
trees. Semantic awareness is a mechanism, not a new product category.
[Source index](adr/0008-derived-source-index.md) and
[argument-count alerts](adr/0029-break-alerts-at-write-time.md) document the
current limits. Rename and removal alerts and linked-worktree semantic notices ship in
v0.2.0. Same-count type/order checks remain roadmap work; cross-machine
symbol publication is not a released capability. See
[B10](https://github.com/Langspan/LangSpan.dev/issues/29) and
[C7](https://github.com/Langspan/LangSpan.dev/issues/44).

## Claim boundaries

- The coordinator, hooks, CLI, MCP and SQLite run locally. LangSpan Live is
  a read-only dashboard. No LangSpan hosted account is required. The released
  `langspan setup` command connects detected hosts, starts the coordinator
  and installs a login service where supported. The install script and
  Homebrew tap remain unpublished.
- Remote awareness is opt-in through Git metadata publications. Source
  contents are excluded from these publications; task text requires its own
  opt-in. This does not describe the coding-agent provider's data handling.
  Remote activity is advisory and never locks another machine's files.
- Supported local holds are scoped and temporary. Writes outside hooks and
  late host-approved edits can bypass them. Host permissions remain
  authoritative. A hold does not establish cross-file semantic compatibility.
- Refusals for stale source, active overlaps and unsafe commit requests have
  different purposes. An agent can reread, retry or choose another task.
  Waiting on an active edit hold is an opt-in workspace policy, off by
  default: a wait lasts at most part of one hook call and admits only an
  unchanged file. Do not promise an edit queue or that waiting reduces stops;
  see [B28: coordination policy](https://github.com/Langspan/LangSpan.dev/issues/171).
- Native Claude Code and Codex observations, real two-machine briefing delivery,
  scripted coordinator tests, and authored illustrations are different kinds
  of evidence. Cursor and Gemini CLI adapters have no retained live-host verification.
  Antigravity was corrected against an initial real run; repeat validation is
  pending. The 5+5 topology is illustrative, not a scale test.
- The goal is compatible parallel work with less avoidable repair and manual
  relay, not a measured reduction. Track comparative results in
  [K1: coordination benchmark](https://github.com/Langspan/LangSpan.dev/issues/172).
  External research motivates the problem, not LangSpan's efficacy.
- The current core is Apache-2.0. Do not announce enterprise features, a
  hosted service, a pricing commitment, or unshipped roadmap work.

## Public entry points

Use [the product website](https://langspan.dev/) for the overview,
[parallel-work illustration](https://langspan.dev/#scale),
[semantic conflict](https://langspan.dev/#why),
[semantic awareness](https://langspan.dev/#semantic-awareness),
[capability explorer](https://langspan.dev/#demo),
[research](https://langspan.dev/#research) and
[retained evidence](https://langspan.dev/#evidence).
Link the existing local edit animation to
[its matching scene](https://langspan.dev/#edit-coordination).

Start adoption with the
[latest stable release](https://github.com/Langspan/LangSpan.dev/releases/latest)
for macOS and Linux, or the source build instructions in the README.
Update the availability statement only after checking the release assets.
