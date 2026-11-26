# ADR 0020: Source-graph depth beyond four languages and one hop waits

**Status:** Not now. Decided 25 November 2026.

## Context

The derived source index ([ADR 0008](0008-derived-source-index.md)) extracts
definitions, mentions and imports for Rust, Python, TypeScript/JavaScript and
Go with Tree-sitter. Answers are name matches, one level deep, with coverage
and content digests. A deeper index would cover more languages, typed edges,
multi-hop impact, rename tracking and more query tools. The capability review of
25 November 2026 ([roadmap](../roadmap.md)) lists these as
Missing or Partial.

The launch bar takes one graph-dependent item: interface-break alerts at write
time, with the argument count (rows B9 and B10, track L5). Everything else
about graph depth waits.

## Decision

| Row | Capability | LangSpan today | Decision |
| --- | --- | --- | --- |
| D1 | Symbol lookup with calls, span, hash, notes and check state | Path, line, kind, signature | Built 25 November 2026 at the owner's request ([issue #50](https://github.com/Langspan/LangSpan.dev/issues/50)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet called on a host |
| D2 | Blast radius level by level | One level | Built 25 November 2026 at the owner's request ([issue #51](https://github.com/Langspan/LangSpan.dev/issues/51)), ahead of the condition below: `graph impact --depth N` (1 to 3) and `langspan_impact` with `depth`; see [ADR 0008](0008-derived-source-index.md). Not yet called on a host |
| D3 | Typed edges, paths between symbols, a repository map | Name mentions | Not now |
| D4 | A definition's history with rationales and reverted attempts | Raw `events` and `recap` | Not now |
| D5 | Graph export and import | `graph export` and `graph import` | Built 26 November 2026 ([issue #54](https://github.com/Langspan/LangSpan.dev/issues/54)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet run between two machines |
| D6 | 11 languages | 4 languages | Not now. A grammar is a new C-built dependency, each with a row in [dependencies](../dependencies.md) |
| D7, B10 (renames) | Renames that keep history | A rename is a removal | Not now. Landing's adaptation to landed renames (track L7) is separate and in the bar |
| D8 | Parse states and a blind-spots query | Coverage on every answer, with `blind_in_scope`; whole-file fallback; `doctor`; `graph blind-spots` | Built 25 November 2026 ([issue #57](https://github.com/Langspan/LangSpan.dev/issues/57)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet called on a host |
| D9 | Symbol-level merge simulation | Git-level combine plus a real check (ADR 0007) | Not now. A real check of the combined tree is the stronger evidence |
| C2 | Prompt briefing ranked with dependents | Unranked names | Not now |
| C7 | In-flight unpushed signatures told to dependents | Peers' files | Not now |
| A11 | Declining a whole-file read once and returning an outline | None | Not now. It changes what the host's read returns |
| A12 | The index's answer attached to a grep result | None | Not now |

Revisit after the launch, ordered by what the benchmark re-run and the
real-host demonstration show agents missing.

## Consequences

- Source context stays a name match without type, scope or import resolution,
  as the README already says.
- Repositories in other languages get file- and line-level coordination and
  briefings, but no source-index answers or interface notices.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: the benchmark re-run or a real-host run showing agents missing a dependent, a rename or a language that the index does not cover. Languages are added by demand, one grammar at a time.
