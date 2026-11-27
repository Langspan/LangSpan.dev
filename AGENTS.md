# LangSpan project context

LangSpan helps teams keep building together as their agents write more code. It gives people and their coding agents a shared view of work in progress. The hypothesis: relevant information delivered before the next affected decision reduces avoidable rework and manual relay, while preserving useful parallel work and human review. [The mission](docs/mission.md) owns the direction and the evidence needed to establish that benefit.

The product name is **LangSpan** and the CLI is **`langspan`**.

## Before any change

- Read and follow [docs/engineering.md](docs/engineering.md) and [docs/engineering-standard.md](docs/engineering-standard.md): commands, workflow, design principles and the check behind each, security, testing and Git rules for every agent and person here.
- Write docs, comments, CLI messages, commits and pull request text to [docs/writing-style.md](docs/writing-style.md). Keep public claims within [docs/positioning.md](docs/positioning.md).

## What to work on

Every capability still to build is a GitHub issue labeled `roadmap` ([the list](https://github.com/Langspan/LangSpan.dev/issues?q=label%3Aroadmap)); the issue is the full specification and names the ADR that decides it. [docs/roadmap.md](docs/roadmap.md) indexes them by area.

- Pick an issue whose ADR says it is planned, or whose condition for revisiting has been met. Read the issue and that ADR.
- Propose any change of decision as an ADR change in the same pull request, and update the issue.
- A pull request that finishes a capability says "Fixes #N". It doesn't edit the index, which is [generated](docs/development-process.md#generated-files) from the open issues, nor any other generated table.

## Constraints the code doesn't show

- All participants in one local coordination domain use the same daemon and state directory.
- Admission is cooperative, not filesystem fencing. Writes outside the hooks can bypass it.
- Tool-input digests protect retry identity; they don't validate the source version an agent previously read.
- Git observations identify file-content and branch-head changes without claiming an author or a complete logical change record.
- Liveness never extends or releases a lease.
- Distinct worktrees remain distinct physical admission domains.
- Awareness must include uncommitted and unpushed local work; this doesn't prescribe a shared transport.

## Evidence

- Retain actual native-host evidence. Parser fixtures and manually issued CLI calls establish narrower results.
- A delivered message or a successful status query shows receipt, not an observable change in approach. The [mission](docs/mission.md#what-success-means) defines the measures.

## Where things are

- [docs/architecture.md](docs/architecture.md): components, protocol and schema versions, decisions by milestone, and the first useful result.
- [docs/IMPLEMENTATION_STATUS.md](docs/IMPLEMENTATION_STATUS.md): what is built, retained evidence and pending acceptance.
- [docs/usage.md](docs/usage.md): setup, hooks, CLI, MCP and configuration.
- [docs/development-process.md](docs/development-process.md): backlog labels, work-in-progress limits, Definition of Done, review, integration, releases and metrics.
- [docs/adr/](docs/adr/0001-local-coordination.md): architecture decisions, starting with ADR 0001.
- [docs/security/threat-model.md](docs/security/threat-model.md): trust boundaries and threats.
- [benchmarks/coordination/README.md](benchmarks/coordination/README.md): the coordination benchmark harness.
- [README.md](README.md): the product overview for people deciding whether to try it.

## How to work

**Think before coding.** State your assumptions. If you're uncertain, ask. If a request has more than one reading, say which ones and don't pick silently. If a simpler approach exists, say so, and push back when it's warranted.

**Simplicity first.** Write the least code that solves the problem, with nothing speculative: no abstraction for single-use code and no configurability nobody asked for. If 200 lines could be 50, rewrite it.

**Surgical changes.** Don't improve adjacent code or formatting, and don't refactor what isn't broken. Every changed line traces to the task. (A task that is itself a refactor, such as an extraction in the engineering remediation, is the change; keep it to moves.)

**Goal-driven execution.** Define what success looks like and loop until it's verified. For a multi-step task, state a short plan with a check for each step.

**No session links in public text.** Don't put a link to an agent session (a `Claude-Session:` trailer or any `claude.ai/code/session` URL) in commits, pull requests, issues or comments. A `Co-Authored-By:` trailer and a "Generated with Claude Code" line are fine.
