# ADR 0016: Hosted-service features are out of scope

**Status:** Not building. Decided 25 November 2026.

## Context

The capability review of 25 November 2026 ([roadmap](../roadmap.md))
found eight capabilities that coordination products provide through a hosted
service. A second review the same day found two more, H11 and H12, both
serving browser onboarding and a hosted dashboard. The report half of row H10
(the "waste report") is hosted too; its local counts are in
[ADR 0019](0019-person-facing-surfaces-and-usage-accounting.md). LangSpan is a local tool: one daemon per machine, an owner-only
Unix socket ([ADR 0001](0001-local-coordination.md)), and cross-machine
awareness through refs on the repository's own Git remote
([ADR 0006](0006-remote-awareness-through-git-refs.md)). It has no server,
no accounts and no outbound calls beyond that opt-in remote.

## Decision

LangSpan does not build these rows. The table records what stands in their
place.

| Row | Hosted capability | What LangSpan has instead |
| --- | --- | --- |
| E8 | People message agents from the hosted dashboard | `langspan message` from the owner's terminal ([ADR 0009](0009-agent-messages-and-handoff.md)) |
| F10 | GitHub App checks and comments on pull requests | Checks bound to an exact tree, recorded locally ([ADR 0007](0007-check-evidence-integration-and-landing.md)) |
| G2 | Accounts, workspaces, members, roles, seats | Whoever may push to the repository may take part (ADR 0006) |
| G3 | Channels, hubs, uploads, unread counts, CRM, email, status page, REST API, OAuth, analytics, rate limits | None |
| G4 | Prompts matched to code by meaning (embeddings, paid) | Name matching ([ADR 0008](0008-derived-source-index.md)) |
| G5 | Real-time leases across machines over a hosted service | Nothing is held across machines; the local half is region admission ([ADR 0012](0012-region-admission.md)) |
| H7 | Streaks, badges, "Wrapped", leaderboards | None |
| I9 | Login, billing upgrade, sharing a repository through the service | `langspan remote enable` per workspace (ADR 0006) |
| H11 | Bringing the editor back to the front after the browser step of adding a repository | None: LangSpan has no browser onboarding; `hooks install` and `install` run in the terminal |
| H12 | Editor and workspace detection for the hosted dashboard's breadcrumb | None: the local page ([ADR 0013](0013-dashboard.md)) names workspaces by their checkout |

## Consequences

- LangSpan keeps no credentials, runs no service and sends nothing that the
  docs do not name. The trust boundaries in the
  [threat model](../security/threat-model.md) do not grow.
- Teams get no hosted view, central access control or cross-machine holds.
  A team that needs those needs a hosted product.
- Cross-machine messages (E9) are not in this list. They could ride Git refs
  and are deferred in [ADR 0023](0023-messages-presence-and-session-continuity-after-launch.md).
- Reopening any row here would mean a server and accounts, which needs a new
  ADR that replaces this one.
