# ADR 0019: Person-facing surfaces and usage accounting wait

**Status:** Not now. Decided 25 November 2026.

## Context

LangSpan is headless: its users are agents, through hooks and MCP, and the
owner, through the CLI and an optional read-only local page
([ADR 0013](0013-dashboard.md)). Other surfaces for the person are possible (a
status line, desktop notices, a daily summary, counters), as is reading host
transcripts for token, cost and quota figures. The capability review of 25 November 2026
([roadmap](../roadmap.md)) lists these as Missing or Partial.

Reading a host's transcript files is a new trust boundary: they hold the
owner's prompts and the model's output. Any "saved" figure shown to a person
is a performance claim, and [engineering practice](../engineering.md) requires
such numbers to be measured.

## Decision

| Row | Capability | Decision | Reason |
| --- | --- | --- | --- |
| H2 | Status line: agents here, catches today | Not now | The launch is about coordination between agents; `status` and the local page cover the owner |
| H3 | Desktop pings and a sound | Not now | Needs OS integration outside the state directory |
| H4, C10 | A daily summary and a "since you left" briefing | Not now | Can be derived from events later; briefings already carry current peer work |
| H5 | Monthly catch counter | Not now | A count of refusals is not evidence of rework avoided |
| H6 | Daily "tokens saved" line | Not now | No such number is shown until it is measured |
| E3 | Recap with tokens, model and cost | Not now | Depends on J10 |
| J10 | Token and cost accounting from host transcripts | Not now | New trust boundary over private transcripts; would need a STRIDE review. Subscription sessions would store no dollars |
| J11 | Detecting that an agent ran out of quota | Not now | Depends on J10. Claude Code's StopFailure event is hooked (below), but nothing reads it for quota or usage |
| H8 | `compliance_report`: how often each person's and agent's writes were checked first, intents completed or abandoned, collisions, blocks and overrides (added by the second review) | Not now | On a hooked host every write is checked by construction, so the rate says little; the rest would be counted from LangSpan's own events once someone needs it |
| H9 | Block outcomes: each refusal settled as adapted, overrode, waited or abandoned (added by the second review) | Not now | Whether a refusal changed what the agent did is judged in retained runs for now (the launch validation, PR #56); a product figure would need the benchmark's definition first, as for H5 |
| H10 | Git trouble baseline: conflicts, rejected pushes and reverts seen in agents' shells, and 26 weeks of `git log` counts (added by the second review) | Not now | A new read of the owner's Git history for a figure nothing uses yet. A report built from the counts by a hosted service is out of scope ([ADR 0016](0016-hosted-service-features-out-of-scope.md)) |

**StopFailure (row A2), amended 25 November 2026.** This record first left
the Claude Code StopFailure hook out, since its only uses were J10 and J11.
The launch bar includes it, so it is hooked with Stop, SubagentStop and
PostToolBatch in launch track L3
([ADR 0027](0027-turn-and-batch-events.md)): a StopFailure is recorded as the
end of a turn, which releases that reply's open admissions as Stop does, and
only its `error` type is kept. Detecting quota or usage from it is J11 and
stays Not now.

## Consequences

- At launch the owner sees coordination through `status`, `recap`, `doctor`
  and `langspan dashboard`, and no usage or savings figure.
- Adding transcript reading later needs its own ADR and an update to the
  [threat model](../security/threat-model.md).

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: users asking for a notice or summary that `status`, `recap` and the local page do not give; for J10, J11 and E3, an accepted STRIDE review of reading transcripts; for H5 and H6, a measurement that ties the count to avoided rework.
