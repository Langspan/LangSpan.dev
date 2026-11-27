# Plan: generate the shared docs tables and the roadmap index (issue #298, M4)

## What conflicts today

- `docs/usage/mcp.md`: the tool table. Every pull request that adds an MCP
  tool (most that add a command) adds a row here.
- `docs/adr/0037-protocol-capabilities.md`: the capability table. Every pull
  request that adds a wire field adds a row (since ADR 0037 a new field adds
  a capability, not a version, so the version line in `docs/architecture.md`
  no longer changes per feature; it stays hand-written).
- `docs/roadmap.md`: the index lines A to J. A finishing pull request removes
  its line by hand; one that adds an item adds a line.
- There is no CLI command table in the docs (`docs/usage.md` is now a page
  index), so "add a command" conflicts in docs only through the MCP table.

## Design

One source entry per item, so two pull requests adding items add two files
and never touch the same source line:

- `docs/tables/mcp-tools/<tool>.txt`: the tool's one-line summary (today's
  "Operation" cell, moved unchanged).
- `docs/tables/capabilities/<name>.txt`: the "Fields it gates" cell, moved
  unchanged. "Since version" is not repeated: the generator reads it from
  `CAPABILITIES` in `src/protocol.rs` (its one owner), and fails naming any
  capability without a file or file without a capability.
- `.txt`, not `.md`: `scripts/ci-changes.sh` skips the checks for a
  Markdown-only change, and a change to a source entry must be checked.

`scripts/gen-docs.py` (standard library only, Python 3.9):

- default: rewrite the region between `<!-- generated ... -->` and
  `<!-- end generated -->` markers in `docs/usage/mcp.md` and ADR 0037,
  rows in name order.
- `--check`: write nothing; exit 1 naming each stale file and the command
  that fixes it.
- `--roadmap ISSUES.json [--date "27 November 2026"]`: rewrite the A to J
  region of `docs/roadmap.md` from the output of
  `gh issue list --label roadmap --state open --limit 500 --json number,title,body,labels,url`.
  Sections come from the `area: X Name` labels, items sorted by their ID
  (`B10` after `B9`), items without an ID after them by number. Each line is
  `- [title](url): summary`, the summary being the issue's `**Index.**`
  paragraph, else the first sentence of its `**Problem.**` (and a warning
  naming the issue). On demand only: CI never needs the network.

Generated rows sort by name, so two pull requests' rows conflict only when
both land in the same gap between existing rows; `gen-docs.py` resolves it
from the merged entries. Ordering changes: the MCP table becomes
alphabetical instead of grouped.

Changed while building: no `merge=union` in `.gitattributes`. It applies
to the whole file, so a real conflict in the prose of `docs/usage/mcp.md`
would merge silently with both sides kept. The same-gap case keeps a
visible conflict, as ADR 0037 accepts for the `CAPABILITIES` lines.

## Changes, test first

1. `scripts/gen-docs/test_gen_docs.py` (unittest, temp directories, a
   recorded issues JSON fixture in `scripts/gen-docs/fixtures/`): region
   rewrite, `--check` failing on a stale table and naming the fix, the
   capability cross-check both ways, roadmap sections, ID ordering, summary
   fallback. Fails first (no module).
2. `tests/tool_count_in_docs.rs` (already in nextest's `ci-fast` profile, so
   it runs on every pull request without touching `.config/nextest.toml` or
   `scripts/check.sh`, which ci-balance holds): two tests, one running
   `gen-docs.py --check` (fails before the markers exist) and one running the
   generator's unit tests. A process test: what is verified is the script.
   The issue's "check in `scripts/check.sh`" is met through the suite
   `check.sh` runs, the way `tests/ratchets.rs` runs the ratchets.
3. `scripts/gen-docs.py`, then the data files (cells moved from the
   tables), markers in both docs, regenerate; `--check` passes.
4. `docs/roadmap.md`: intro says the index is generated and a pull request
   doesn't edit it; markers around A to J; regenerate from the issues.
   `AGENTS.md` line 18 and `docs/development-process.md` (Backlog, Definition
   of Done, a "Generated files" list) say the same; `docs/engineering.md`
   Commands gains the generator line.
5. Evidence for "Done when": two scratch branches each adding a tool entry
   and a capability entry, regenerated, merged locally without a conflict;
   output in the PR.

## Shared contracts touched

None of the wire protocol, CLI dispatch or daemon. Docs tables:
`docs/usage/mcp.md` (row format unchanged, so `tool_count_in_docs.rs`'s
name check still reads it), ADR 0037's table, `docs/roadmap.md` (p-213-3 is
editing it now; I re-read and merge before editing), and
`docs/development-process.md` (f-sleeps is editing it).

## Today's status text

Changed while building: no issue edits. A line's summary is the issue's
`**Index.**` paragraph, else the summary the index already gives that
issue, else the first sentence of its `**Problem.**`. Regenerating keeps
today's status text ("Built as ...; a real-host run remains") without
editing about 70 public issues; a status change goes into the issue's
`**Index.**` paragraph from then on.

The first regeneration from the live issues needs `gh issue list`, which
this session could not run; it is listed as not done in the pull request.

## Not in scope

- The brief's one summary read by the help text and the MCP description too:
  those texts differ from the table's today, and serving new text changes
  the CLI golden output and the MCP tool list, beyond issue #298.
- The launch bar table in `docs/roadmap.md` stays hand-written.
