# Product marketing context

Document version: 1.0 — 25 November 2026

## Product and offer
LangSpan is an Apache-2.0 coordination layer for existing coding-agent sessions.
The primary action is to download v0.1.0, connect one checkout and observe work
from two sessions. Source builds remain an option; Homebrew and Windows are not
available. The user keeps the agent, its permissions and code-review process.

## Audience and jobs
Primary: developers using multiple sessions on connected changes with teammates.
Secondary: solo developers using concurrent sessions and engineering leads
evaluating agent-assisted collaboration. Hypotheses, not customer research.
Jobs: discover related in-progress work before editing; identify supported local
overlaps; inspect shared work and check evidence before integrating.

## Alternatives and differentiation
Manual chat/status relay, issue assignment, Git branches/worktrees and agent
orchestrators are relevant alternatives or complements. LangSpan operates in
existing sessions via hooks, CLI and MCP, with optional Git-based awareness.
Worktree isolation remains useful; it does not share local holds across
checkouts. Do not claim unique market position without competitive validation.

## Objections and evidence
- Does it replace my agent? No; native hooks integrate supported sessions.
- Does it lock files everywhere? No; active holds apply to supported local
  operations. Remote sharing is advisory, opt-in; source content is excluded,
  and task text requires its own opt-in.
- Is it proven across the team? Retained native Claude Code and Codex evidence,
  and two-machine briefing delivery exist. A measured reduction in repair or
  relay has not been established. Cursor/Gemini live acceptance remains open.

## Voice
Concrete verbs, short sentences, mechanism before hype. Keep the established
“Your agents. On the same page.” identity and the mission's collaborative
direction. No invented customers, testimonials, metrics, or safety guarantees.

## Claim owners
[Mission](../docs/mission.md), [implementation status](../docs/IMPLEMENTATION_STATUS.md),
[real Codex](../docs/evidence/codex-real-host/README.md),
[two machines](../docs/evidence/two-machine/README.md),
[scripted scenario](../docs/evidence/demo/README.md), and the
[published release](https://github.com/Langspan/LangSpan.dev/releases/tag/v0.1.0).
The source and evidence take precedence over marketing.

## Learnings
The first release makes source-only calls to action obsolete. Primary adoption
now starts with an archive; preserve the source route. No conversion analytics
or customer research was available to validate lift.

## Changelog
- 1.0: initial source-based context, release and claim review.
