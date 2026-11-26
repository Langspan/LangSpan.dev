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
| D3 | Typed edges, paths between symbols, a repository map | `graph path` and `langspan_path`: the shortest chains of calls between two names, each step found by import path or by name and marked `ambiguous` when the name has several definitions; `graph map` and `langspan_map`: areas with their files, exports and languages, and the imports and calls between them | Built 26 November 2026 at the owner's request ([issue #52](https://github.com/Langspan/LangSpan.dev/issues/52)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Imports are resolved by path for TypeScript and JavaScript, Python and Rust only. Not yet called on a host |
| D4 | A definition's history with rationales and reverted attempts | `graph history` and `langspan_history`: recorded interface changes newest first, with the last reported editor, its task line, nearby notes and messages, and `reverted` | Built 25 November 2026 ([issue #53](https://github.com/Langspan/LangSpan.dev/issues/53)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet called on a host |
| D5 | Graph export and import | `graph export` and `graph import` | Built 26 November 2026 ([issue #54](https://github.com/Langspan/LangSpan.dev/issues/54)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet run between two machines |
| D6 | 11 languages | 5 languages | Java built 25 November 2026 and C# 26 November 2026 at the owner's request ([issue #55](https://github.com/Langspan/LangSpan.dev/issues/55)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Neither yet run on a host. Other languages: not now. A grammar is a new C-built dependency, each with a row in [dependencies](../dependencies.md) |
| D7, B10 (renames) | Renames that keep history | A rename in one file with the same signature apart from the name and the same body or lines is one `renamed` notice with its `evidence`, told at a prompt and at a write calling the old name; a file moved whole keeps its definitions, notes and history under the new path; `graph symbol OLD --renamed` answers with the new name; any other rename is a removal and an addition | B10 built 25 November 2026 ([issue #29](https://github.com/Langspan/LangSpan.dev/issues/29)) and D7 built 26 November 2026 ([issue #56](https://github.com/Langspan/LangSpan.dev/issues/56)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md) and [ADR 0029](0029-break-alerts-at-write-time.md). Not yet observed on a host. A rename across files other than a whole-file move is never inferred. Landing's adaptation to landed renames (track L7) is separate and in the bar |
| D8 | Parse states and a blind-spots query | Coverage on every answer, with `blind_in_scope`; whole-file fallback; `doctor`; `graph blind-spots` | Built 25 November 2026 ([issue #57](https://github.com/Langspan/LangSpan.dev/issues/57)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet called on a host |
| D9 | Symbol-level merge simulation | `integrate check --symbols` and `symbols` on `langspan_integrate_check`: before the command runs, each branch's changed files in indexed languages are indexed in memory from Git's objects, and the record keeps at most 32 `symbol_findings` (`both_changed`, `changed_and_called` by ADR 0029's arity rule, `removed_and_used`), skipped with a note past 10 seconds | Built 26 November 2026 ([issue #58](https://github.com/Langspan/LangSpan.dev/issues/58)), ahead of the condition below; see [usage](../usage.md). Advisory: a finding never changes the check's result or what `land` requires, and Git's combination stays the tree that is checked and landed; no symbol-level merge is written. Name matches only. Not yet called on a host |
| C2 | Prompt briefing ranked with dependents | Definitions ranked by fixed rules (the session's own files, a live peer's, exported, named in the prompt, path order), each with up to 5 ranked dependents | Built 26 November 2026 at the owner's request ([issue #41](https://github.com/Langspan/LangSpan.dev/issues/41)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Not yet run on a host |
| C7 | In-flight unpushed signatures told to dependents | Exported names another worktree of the repository added or changed and not committed, told by name to sessions whose files mention them; `graph inflight` | Built 25 November 2026 at the owner's request ([issue #44](https://github.com/Langspan/LangSpan.dev/issues/44)), ahead of the condition below; see [ADR 0008](0008-derived-source-index.md). Linked worktrees on one machine only. Not yet run on a host |
| A11 | Declining a whole-file read once and returning an outline | `--outline-reads on` (off by default): a session's first whole read of a large file the index holds whole at its disk content is refused once with the file's outline; see [usage](../usage.md#outline-first-for-whole-reads-of-large-files) | Built 26 November 2026 ([issue #16](https://github.com/Langspan/LangSpan.dev/issues/16)), ahead of the condition below, opt-in because it changes what the host's read returns. Tested with Claude Code-shaped and constructed Cursor events through a real daemon and index; not yet run on a host. Decisions the issue left open are below |
| A12 | The index's answer attached to a grep result | None | Not now |

Revisit after the launch, ordered by what the benchmark re-run and the
real-host demonstration show agents missing.

### A11: choices the issue left open

Each is the smallest that meets the issue's criteria.

- The option is `--outline-reads on|off` on `hooks install`, `install` and
  `hook`, written into the installed command only when on. On, it also adds
  the read tool to the pre-tool matcher of Claude Code (`PreToolUse`) and
  Cursor (`preToolUse`), so with it off no
  read starts a hook process before it runs. Codex and Antigravity commands
  carry the option and ignore it.
- The thresholds are `outline_min_lines` (1,500) and `outline_min_bytes`
  (65,536) in `config.json`, read by the coordinator at start; either one
  reached is enough. A file over the index's 1 MiB bound is never outlined.
- A read names part of the file when its input has a non-null `offset`,
  `limit` or `pages`.
- The hook reads the file, measures it and sends its SHA-256 with one new
  command, `outline_read`. The coordinator answers with an outline only
  when the index row for the path has that digest and no partial parse, the
  scan is complete, the file has at least one definition, and the session's
  record (`read_outlined`, at most 64 versions per session, one per path)
  does not hold that version. Only then is `read_outlined` appended. An
  older coordinator answers the unknown command with an error, which is no
  decision.
- Wire protocol version 8 (review comment 4714078108). A client of an
  earlier build reads session activity and events with
  `deny_unknown_fields`, so a session activity's `outlined` and the
  `read_outlined` event would break its status and event queries. To a
  version 7 or earlier client, the coordinator answers without them,
  including in state records given in parts.
- Each outline line is `KIND NAME lines A-B`, with ` (exported)` for an
  exported definition. `KIND` is the index's own name (`function`,
  `method`, `type`, `interface`, `constant`, `module`), not the language's
  keyword. Signatures are left out, so the outline holds names alone.
- The coordinator sends at most 200 definitions, the largest by lines
  spanned when a file has more (the index ranks them before it limits
  them, review comment 4713905468), with how many the index holds. The hook
  lists them in line order when all fit in 2,400 bytes; otherwise largest
  first. Either way, a last line counts the definitions not listed.
- Session identity is the host's, so a subagent with its own identity gets
  its own outline.

## Consequences

- Source context stays a name match without type, scope or import resolution,
  as the README already says. The one exception is D3: `graph path` and
  `graph map` resolve imports by path, and say so on each step and edge.
- Repositories in other languages get file- and line-level coordination and
  briefings, but no source-index answers or interface notices.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: the benchmark re-run or a real-host run showing agents missing a dependent, a rename or a language that the index does not cover. Languages are added by demand, one grammar at a time.
