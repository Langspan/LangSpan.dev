# Plan: LAYOUT wave 1, B6 (briefing) and the rest of B1 (session_label to text)

Part of #300 and #372 (refs, closes neither). Pure moves, no behavior change.
Two commits, one per step, each following the LAYOUT.md move recipe.

## B1 rest: session_label into text

- Cut `session_label` (with its doc comment) from `src/hook/render.rs` and paste it
  into `src/text.rs`, after `clean`, which it calls.
- The old site keeps `pub(crate) use crate::text::session_label;` in `render.rs`, so
  `hook.rs`'s `pub(crate) use render::{.., session_label, ..}` and every caller
  (`since`, `trail`, `formatter`, `hook`) stay unchanged.
- Baselines: `--tighten src/hook/render.rs`; `--record src/text.rs` if its line count
  needs it (it has no `lines` row today, so it is held to the file limit only).

## B6: briefing namespace

- New root `src/briefing.rs`: a one-line doc comment, `pub const BRIEFING_LIMIT`
  moved from `src/hook.rs` (cut and paste), and `pub(crate) mod render;`,
  `pub mod recent;`, `pub mod since;`, `pub mod trail;`.
- `git mv src/hook/render.rs src/briefing/render.rs`, and `src/{since,recent,trail}.rs`
  to `src/briefing/`, with their baselines to `scripts/ratchets/baseline/src/briefing/`.
- `src/hook.rs`: `mod render;` becomes `use crate::briefing::render;`, so its `use
  render::{..}` lines don't change; the BRIEFING_LIMIT line becomes `pub use
  crate::briefing::BRIEFING_LIMIT;` (callers such as `report`, `antigravity` and the
  hook tests keep `hook::BRIEFING_LIMIT`).
- `render.rs` keeps `use super::BRIEFING_LIMIT;`: `super` is now `briefing`, which owns
  the constant, so the line stays as it is. The moved files have no include_str! and
  no other `super::` outside their own test modules; their `crate::hook::..` and
  `crate::trail`/`crate::recent` paths resolve through hook's re-exports and the aliases.
- `src/lib.rs`: the briefing block becomes `pub mod briefing;`; the compatibility block
  gets `pub use briefing::recent;`, `pub use briefing::since;`, `pub use briefing::trail;`
  (one per line, as the ratchet's alias pattern reads them).
- `scripts/ratchets/namespaces.tsv`: the `since`, `recent`, `trail` rows become one
  `namespace briefing application` row. Edge rows aren't tightened (recipe step 6):
  `hook.rs.tsv`'s `edge since/recent/trail` rows resolve through the aliases to
  `briefing`, and the moved files' `edge hook` rows cover briefing -> hook.
- `scripts/ci-changes.sh`: `recent` leaves the `(claude_code|recent)\.rs` pattern, since
  `src/briefing/` already matches it; `docs/development-process.md` updates the hot
  `briefing` file list and the BENCH_PATHS sentence to the new paths.
- Baselines: `--record src/briefing.rs`; `--tighten` / `--record` for `hook.rs` and
  `lib.rs` as the check reports. No coverage-baseline rows exist for these files.

### Update after merging main (f-split-hook landed)

`BRIEFING_LIMIT` is no longer in `src/hook.rs`: the hook split moved it to
`src/hook/lifecycle.rs:3`. `src/briefing.rs` now owns it, and `hook.rs` re-exports
it with `pub use crate::briefing::BRIEFING_LIMIT;`. That explicit re-export shadows
lifecycle's glob, but the old definition is still there, so the value is defined twice.
Finishing the move means deleting that one line from `src/hook/lifecycle.rs`.
`lifecycle.rs` uses `use super::*`, so it then resolves through hook's re-export.
It also means tightening `scripts/ratchets/baseline/src/hook/lifecycle.rs.tsv`
(`pub` 6 -> 5). Both files were added to the reservation for this.

## Test order and checks

No new tests: this is a move. The guard is the unchanged suite (the briefing render
tests in `hook.rs`, `since`/`recent`/`trail` unit tests, `cli_golden`, `cli_transcript`,
contract and property tests). Per commit: `git diff -M --color-moved` shows only moved
lines; `cargo fmt --all -- --check`; both clippy commands; `python3 -I -B
scripts/ratchets.py --check .`; full `cargo nextest run --workspace --locked` and
`cargo test --doc`; `git diff --stat tests/golden` empty.

## Shared contracts touched

None of the wire protocol, CLI, MCP, store or hook/briefing texts. Shared files:
`src/lib.rs` (module blocks), `src/hook.rs` (two lines), `scripts/ratchets/namespaces.tsv`.

`src/hook.rs` and `scripts/ratchets/baseline/src/hook.rs.tsv` are in f-split-hook's
reservation (the hook.rs body split, C1). My edits there are two lines near the top
(`mod render;`) and the `BRIEFING_LIMIT` line, so this should land before or after
f-split-hook, not alongside it; whichever lands second rebases its two-line change.
