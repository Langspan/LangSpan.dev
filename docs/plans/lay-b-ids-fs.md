# Plan: LAYOUT wave 1, B2 (ids) and B3 (fs)

Part of #300 and #372 (refs, closes neither). Pure moves, no behavior change.
Two commits, one per step, each following the LAYOUT.md move recipe.

## B2: ids

- `git mv src/sha256.rs src/ids/sha256.rs`, `git mv src/registration.rs src/ids/registration.rs`,
  and their baselines to `scripts/ratchets/baseline/src/ids/`.
- `src/ids.rs`: `pub(crate) mod registration;` and `pub(crate) mod sha256;` (as
  `src/text.rs` declares `redact`).
- Items: `admission_id` and `record_id` move from `src/wait.rs`, and
  `user_workspace_id` from `src/hook.rs`, into `src/ids.rs`, cut and pasted with their
  doc comments. The old sites keep `pub use crate::ids::{...};`, so callers don't change.
- `src/lib.rs`: drop `mod registration;` and `mod sha256;` from the ids block; add
  `use ids::registration;` and `use ids::sha256;` to the compatibility block, one per
  line (the ratchet's alias pattern reads single-path `use` lines).
- `scripts/ratchets/namespaces.tsv`: drop the `sha256` and `registration` namespace
  rows and `allow ids sha256`; `allow regions sha256` becomes `allow regions ids`.
- `docs/dependencies.md`: the SHA-256 path becomes `src/ids/sha256.rs`.
- Baselines: `--record` for anything new, `--tighten` for wait.rs and hook.rs.

## B3: fs

- New `src/fs.rs` root: a one-line doc comment and `pub(crate) mod file_state; pub(crate)
  mod input; pub(crate) mod local;`.
- `git mv` `src/{local,file_state,input}.rs` to `src/fs/`, with the local and input
  baselines.
- `src/lib.rs`: the fs block becomes `mod fs;`; the compatibility block gets
  `use fs::file_state;`, `use fs::input;` and `use fs::local;`.
- `namespaces.tsv`: `local`, `file_state` and `input` rows become `namespace fs
  infrastructure`; `allow remote local`, `client local`, `autostart local`, `shell
  file_state` and `shell local` become `allow remote fs`, `client fs`, `autostart fs`
  and `shell fs`.
- `benchmarks/coordination/driver.py`: a comment's `src/local.rs` becomes `src/fs/local.rs`.

The moved files need no include_str!/#[path]/super:: fixes (their only `super::` are
their own test modules). None has a coverage-baseline row. No edge rows are tightened.

## Test order and checks

No new tests: this is a move. The guard is the unchanged suite. Per commit:
`git diff -M --color-moved` shows only moved lines; `cargo fmt --all -- --check`;
both clippy commands; `python3 -I -B scripts/ratchets.py --check .`; full
`cargo nextest run --workspace --locked` and `cargo test --doc`; goldens unchanged
(`git diff --stat tests/golden` empty).

## Shared contracts touched

None of the wire protocol, CLI, MCP, store or hook texts. Shared files: `src/lib.rs`
(module blocks), `src/hook.rs` (one function cut, one re-export added),
`src/wait.rs`, and `scripts/ratchets/namespaces.tsv`.
