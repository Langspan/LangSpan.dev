# ADR 0017: Large coordination features wait for measurement

**Status:** Not now. Decided 25 November 2026.

## Context

Coordination tools can offer features that change how agents plan and record
work, not only what they are told. The capability review of 25 November 2026
([roadmap](../roadmap.md)) lists them as Missing or Partial.
Each is a sizeable piece of work, and each adds new state that every host
adapter, briefing and recap has to carry. None has been shown, in a
measurement, to reduce rework for agents that already get
briefings, stale-source refusals and region admission.

## Decision

These rows are not built for the launch. They are reconsidered only when a
retained run (a real-host demonstration or the benchmark re-run) shows
agents hitting the problem the feature solves, and the measurement says how
often.

| Row | Capability | LangSpan today | Revisit when |
| --- | --- | --- | --- |
| B11 | Standing decisions that hold a write which would reverse them | Notes (`remember`) inform; they never hold a write | A run shows an agent undoing a recorded decision |
| B16 | `claim`: the next free number in a sequence (migrations, ADRs) | None | A run shows two sessions taking the same number |
| B17 | Typed intents (rename, add a parameter) with conflict prediction | `intent set` on paths with a summary | Path-level intents are shown to miss conflicts that typed ones would predict |
| B18 | `plan_work`: work units split into independent groups, checked live | None | Measurement shows planning collisions that briefings do not prevent |
| B19 | A queue of collided work to reconcile | None | Collisions are frequent enough to need a queue |
| B20 | Typed refactor operations over definitions and call sites, and recipes | None | Graph depth ([ADR 0020](0020-source-graph-depth-after-launch.md)) exists to resolve call sites |
| C5 | Briefings that learn from hits and misses per repository | Fixed bounds and relevance rules ([ADR 0004](0004-automatic-peer-briefing.md)) | Hit and miss rates are measured |
| C11 | Worked examples from finished, tested tasks | None | A run shows agents repeating solved tasks |

## Consequences

- The roadmap lists these rows as not built at launch, each pointing here. Public copy does not claim them.
- The core and its event schema stay smaller until there is evidence. Adding
  any of them later is an additive event type, within the version tolerance
  of [ADR 0015](0015-version-tolerance.md).

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. The "Revisit when" column says what would change each decision.
