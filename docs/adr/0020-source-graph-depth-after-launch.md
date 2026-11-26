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
| D4 | A definition's history with rationales and reverted attempts | `graph history` and `langspan_history`: recorded interface changes newest first, with the last reported editor, its task line, nearby notes and messages, and `reverted` | Built 25 November 2026 ([issue #53](https://github.com/Langspan/LangSpan.dev/issues/53)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet called on a host |
| D5 | Graph export and import | `graph export` and `graph import` | Built 26 November 2026 ([issue #54](https://github.com/Langspan/LangSpan.dev/issues/54)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet run between two machines |
| D6 | 11 languages | 5 languages | Java built 25 November 2026 and C# 26 November 2026 at the owner's request ([issue #55](https://github.com/Langspan/LangSpan.dev/issues/55)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Neither yet run on a host. Other languages: not now. A grammar is a new C-built dependency, each with a row in [dependencies](../dependencies.md) |
| D7, B10 (renames) | Renames that keep history | A rename in one file with the same signature apart from the name and the same body or lines is one `renamed` notice with its `evidence`, told at a prompt and at a write calling the old name; a file moved whole keeps its definitions, notes and history under the new path; `graph symbol OLD --renamed` answers with the new name; any other rename is a removal and an addition | B10 built 25 November 2026 ([issue #29](https://github.com/Langspan/LangSpan.dev/issues/29)) and D7 built 26 November 2026 ([issue #56](https://github.com/Langspan/LangSpan.dev/issues/56)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md) and [ADR 0029](0029-break-alerts-at-write-time.md). Not yet observed on a host. A rename across files other than a whole-file move is never inferred. Landing's adaptation to landed renames (track L7) is separate and in the bar |
| D8 | Parse states and a blind-spots query | Coverage on every answer, with `blind_in_scope`; whole-file fallback; `doctor`; `graph blind-spots` | Built 25 November 2026 ([issue #57](https://github.com/Langspan/LangSpan.dev/issues/57)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet called on a host |
| D9 | Symbol-level merge simulation | `integrate check --symbols` and `symbols` on `langspan_integrate_check`: before the command runs, each branch's changed files in indexed languages are indexed in memory from Git's objects, and the record keeps at most 32 `symbol_findings` (`both_changed`, `changed_and_called` by ADR 0029's arity rule, `removed_and_used`), skipped with a note past 10 seconds | Built 26 November 2026 ([issue #58](https://github.com/Langspan/LangSpan.dev/issues/58)), ahead of the condition below; see [usage](../usage.md). Advisory: a finding never changes the check's result or what `land` requires, and Git's combination stays the tree that is checked and landed; no symbol-level merge is written. Name matches only. Not yet called on a host |
| C2 | Prompt briefing ranked with dependents | Definitions ranked by fixed rules (the session's own files, a live peer's, exported, named in the prompt, path order), each with up to 5 ranked dependents | Built 26 November 2026 at the owner's request ([issue #41](https://github.com/Langspan/LangSpan.dev/issues/41)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet run on a host |
| C7 | In-flight unpushed signatures told to dependents | Exported names another worktree of the repository added or changed and not committed, told by name to sessions whose files mention them; `graph inflight` | Built 25 November 2026 at the owner's request ([issue #44](https://github.com/Langspan/LangSpan.dev/issues/44)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Linked worktrees on one machine only. Not yet run on a host |
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
