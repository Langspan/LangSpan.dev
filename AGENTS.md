# LangSpan project context

LangSpan's mission is to help teams keep building together as their agents write more code. It gives people and their coding agents a shared view of work in progress. The product hypothesis is that relevant information delivered before the next affected decision reduces avoidable rework and manual state relay while preserving useful parallel work and human review.

[Mission and success criteria](docs/mission.md) records the product direction and the evidence needed to establish that benefit.

The implementation is a headless tool that fits existing agent sessions, with CLI and MCP access and an optional UI. A solo developer with several agents is a useful starting case; the broader product outcome is several developers contributing to related work with their own agents. Awareness of uncommitted and unpushed local work is a requirement; it does not prescribe a particular shared transport.

## Engineering practice

Before any change, read and follow [docs/engineering.md](docs/engineering.md). It is the shared engineering framework for every agent and person working here. This file supplies project context and navigation; the engineering practices live in that one document.

## What to work on

Every capability still to build is a GitHub issue labelled `roadmap` ([the list](https://github.com/Langspan/LangSpan.dev/issues?q=label%3Aroadmap)); the issue is the item's full specification and names the ADR that decides it. [docs/roadmap.md](docs/roadmap.md) is a short index of those issues by area. Take work from there: pick an issue whose ADR says it is planned or whose condition for revisiting has been met, read the issue and that ADR, and propose any change of decision as an ADR change in the same pull request. A pull request that finishes a capability says "Fixes #N" for its issue and removes its line from the index; a decision that changes updates the issue and the ADR.

## Current state

The implementation is Rust: a pure `langspan-core` crate, a single SQLite event/receipt store, a Unix-socket daemon, the `langspan` CLI and an official-SDK stdio MCP adapter. [ADR 0001](docs/adr/0001-local-coordination.md) records the local foundation; [implementation status](docs/IMPLEMENTATION_STATUS.md) distinguishes historical verification, current checks and pending acceptance. The exact build and check commands are in the shared engineering document.

The product name is **LangSpan** and this implementation's CLI is **`langspan`**. All participants in one local coordination domain use the same daemon/state directory. The M2 Claude Code adapter covers Edit and Write ([ADR 0002](docs/adr/0002-claude-code-adapter.md)). **M3 is merged:** shared typed hook identities, a Codex `apply_patch` adapter, linked-worktree registration, Linux process observations, a bounded Git observer, and a ninth MCP tool, `langspan_changes`. [ADR 0003](docs/adr/0003-native-coordination-observation.md) records these choices and their limits. [ADR 0004](docs/adr/0004-automatic-peer-briefing.md) adds automatic peer briefings: session-start and prompt hooks register each session and place bounded, relevant peer work in the model's context, also available as `langspan brief` and a tenth MCP tool. Wire protocol and SQLite schema marker are version 3; older persisted records remain readable.

Admission is cooperative, not filesystem fencing. Tool-input digests protect retry identity; they do not validate the source version an agent previously read. Git observations identify file-content and branch-head changes without claiming an author or a complete logical change record. Liveness never extends or releases a lease. Distinct worktrees remain distinct physical admission domains.

The current Codex fixtures are source-derived against an unverified alpha-host contract. New mixed-host and linked-worktree acceptance remains pending, independently of unit tests and CI. Briefing delivery is observed on Claude Code only; Codex delivery awaits the owner's hook-trust approval. [ADR 0005](docs/adr/0005-source-versions-and-stale-writes.md) records source comparisons: stale edits are refused once per changed version, while unrecorded versions can proceed. Claude completed-Read capture has native evidence; LangSpan's own stale-denial path still needs live host verification. [ADR 0006](docs/adr/0006-remote-awareness-through-git-refs.md) adds opt-in cross-machine awareness through LangSpan-owned refs on the Git remote; hosted remotes and two-machine runs remain unverified. [ADR 0007](docs/adr/0007-check-evidence-integration-and-landing.md) binds checks to exact trees and lands only on current evidence. [ADR 0008](docs/adr/0008-derived-source-index.md) adds a derived, name-matching source index (`crates/langspan-graph`) with interface-change notices. Checks, landing and source-index use still need native-agent evidence. [ADR 0033](docs/adr/0033-gemini-cli-adapter.md) adds a Gemini CLI adapter built from Gemini CLI 0.70.0's documentation and source; its fixtures are source-derived, and live Gemini acceptance is pending.

## First useful result

The immediate work is to verify Claude Code and Codex together in one checkout and in linked worktrees, including conflicts, unrelated work, ordinary permission behavior, post observations and bypass edits. Retain actual native-host evidence; parser fixtures and manually issued CLI calls establish narrower results. The product benchmark starts alongside this work.

The broader product proof is two developers' existing agents coordinating across machines without their owners relaying context. Shared facts must reach the receiving agent before it chooses the relevant edit, with an observable change in approach. A successful status query alone does not establish that outcome.

## Project map

- [docs/mission.md](docs/mission.md): mission, problem, hypothesis, intended workflow and team-level success criteria.
- [README.md](README.md): product overview and repository entry point.
- [docs/usage.md](docs/usage.md): setup, native hooks, CLI, MCP and configuration.
- [docs/IMPLEMENTATION_STATUS.md](docs/IMPLEMENTATION_STATUS.md): what is built, retained evidence and pending acceptance.
- [docs/roadmap.md](docs/roadmap.md): the launch bar and an index of the [roadmap issues](https://github.com/Langspan/LangSpan.dev/issues?q=label%3Aroadmap), which hold each item's specification.
- [docs/adr/](docs/adr/0001-local-coordination.md): architecture decisions, starting with ADR 0001.
- [docs/security/threat-model.md](docs/security/threat-model.md): trust boundaries and threats.
- [benchmarks/coordination/README.md](benchmarks/coordination/README.md): the coordination benchmark harness.
