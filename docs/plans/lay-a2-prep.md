# Plan: lay-a2-prep (LAYOUT.md wave 0, step A2)

Refs #300, #372 (preparation only; neither closes). No behaviour change: no
Rust item changes, goldens byte-identical.

## What I found

- A1 (#471) already landed `scripts/ratchets/namespaces.tsv`, and in it
  every top-level module of `src/` carries the layer of the namespace it
  moves into, except `graph`, `hook` and `wait` (`-`).
- `tests/ratchets.rs` and `ratchets.py --check` require the `namespace`
  rows to be exactly the top-level modules of `src/`. Rows for target
  namespaces that have no root yet (`time`, `fs`, `ipc`, `ownership`,
  `checks`, `briefing`, `person`) would fail the check, so each of those
  rows arrives with the move that creates its root (B2 to D9).
- `ratchets.py` reads a namespace's layer from its tsv row and falls back to
  a `// layer:` marker only when the row says `-`. A marker on a root that
  has a tsv layer is never read, and nothing checks that the two agree.
- `hook/redact.rs` already moved to `text/redact.rs`, and `BENCH_PATHS`
  doesn't name `text`, so a change there already skips the dry runs.

## Decision: layers in the tsv, not in markers

The task asks for `// layer:` markers on roots that don't move. Because the
tsv already names those layers and wins over a marker, markers would be a
second, unchecked copy (DRY). The three missing layers go in the tsv
instead (`graph infrastructure`, `hook application`, `wait application`),
which exposes the same edges the markers were meant to expose, and no
markers are added. Only the `allow` rows the check actually asked for were
added (graph -> git, index_file, observer; cursor -> hook; hook -> spool,
wait), not the speculative list below.

## Changes, test first

1. `tests/ci_workflows.rs`
   (`code_split_into_new_core_daemon_or_hook_modules_runs_the_dry_runs`):
   add `src/text/redact.rs`, `src/text.rs`, `src/briefing.rs`,
   `src/briefing/render.rs` to the paths that run the dry runs, and
   `src/texts.rs`, `src/briefings.rs` to those that skip. Run it, watch the
   new paths fail. Then widen `scripts/ci-changes.sh`:
   `^src/(daemon|hook|briefing|text)(/|\.rs$)`, and update its comment.
   (This test runs the real script in a temporary Git repository because
   Git's diff of the commit range is what it verifies; it is already that
   kind of test.)
2. `scripts/ratchets/namespaces.tsv`: set the three layers; add `allow`
   rows for same-layer edges LAYOUT.md allows that now show up (hook and its
   future members claude_code, codex, cursor, antigravity, gemini_removed,
   between_steps, spool, wait; hook -> ownership members journal, lines,
   reformat, formatter, adopt, commit; hook -> briefing members since,
   recent, trail; person members -> briefing members). Update the header
   comment that says hook, wait and graph have no layer yet.
   `python3 -I -B scripts/ratchets.py --check .`, then `--record` each root
   whose debt grew: the edges LAYOUT.md forbids (checks and person modules
   -> hook, hosts/store -> hook, store -> wait, graph -> definitions and
   reformat). Exact rows come from the check's output; the reservation lists
   every root baseline that can gain one.
3. `src/lib.rs`: reorder the `mod` lines into one block per target namespace
   (utility, infrastructure, application, interface, each headed by a
   comment naming the namespace), then the existing marked compatibility
   block. Same set of `mod` lines and the same visibility, so
   `the_top_level_modules_are_the_listed_namespaces` passes unchanged.
   `--record src/lib.rs` if its line count grows past its row.
4. `docs/architecture.md`: a "Module map" section under Components: the four
   layers, the namespaces in each, today's modules each will hold, and the
   allowed edges, pointing to namespaces.tsv as the owner.
5. `docs/development-process.md`: hot components named by namespace
   (protocol, briefing, install and hosts, cli, graph) with today's files;
   the Ratchets paragraph says layers come from namespaces.tsv (the marker
   is the fallback for a `-` row). Another session (f-sleeps) is editing this
   file in its own worktree; I'll re-read it from origin/main first and keep
   my edit to those two passages.

## Shared contracts touched

None of the wire protocol, CLI, daemon, store or goldens. Shared files: the
ratchet tsv and baselines, `scripts/ci-changes.sh`, two docs.

## Checks

cargo fmt, clippy `-D warnings`, `cargo nextest run --workspace --locked`
(includes `tests/ratchets.rs`, `tests/ci_workflows.rs`, golden tests),
`cargo test --doc`, `git diff --color-moved` on lib.rs to show only moves.
