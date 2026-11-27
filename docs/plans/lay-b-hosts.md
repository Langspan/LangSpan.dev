# Plan: LAYOUT wave 1, B4 (install::detect) and B5 (hosts)

Part of #300 and #372 (refs, closes neither). Two commits, one per step, each following
the LAYOUT.md move recipe. B4 is a pure move; B5 is moves plus one rename.

## B4: install::detect

- `git mv src/hosts.rs src/install/detect.rs` and
  `scripts/ratchets/baseline/src/hosts.rs.tsv` to `.../src/install/detect.rs.tsv`.
- `src/install.rs`: `pub mod detect;`. The file stays a flat root (as `src/fs.rs`).
- `src/lib.rs`: drop `pub mod hosts;` from the install block. No alias (B5 reuses the
  name `hosts`).
- Repoint every importer from `hosts::` to `install::detect::` (paths only):
  `src/cli/render/state.rs`, `src/cli/doctor.rs`, `src/cli/setup.rs`, `src/cli/run.rs`
  (binary, `langspan::`), `src/service.rs`, `src/daemon/remote_worker.rs`,
  `src/dashboard.rs`, `tests/doctor_library.rs`.
- `namespaces.tsv`: drop `namespace hosts infrastructure`, `allow hosts install`,
  `allow hosts user_install` and `allow service hosts` (service -> install is allowed);
  the header comment's `hosts -> hook` debt becomes install's.
- Docs: `docs/architecture.md` module map (install row: `install::detect`) and
  `docs/development-process.md` hot components (`src/install/detect.rs`).
- The moved file has no `super::` outside its tests and no `include_str!`; no
  coverage-baseline row.
- Found while doing it: folding detection into the `install` node closes a cycle,
  install (detect) -> user_install -> install (`GEMINI_REMOVED`, `Vendor`). Both ends
  fail `--check` until they carry an `edge` row: `edge user_install` in
  `install.rs.tsv` (recorded) and `edge install` in `user_install.rs.tsv` (outside the
  first reservation; requested). Both modules join the `install` namespace in the target
  layout, so the cycle disappears when user_install moves.

## B5: hosts

- New `src/hosts.rs` root, `pub mod hosts;` in lib.rs's hosts block replacing
  `mod tool_names; mod vendor;`; compatibility block gets `use hosts::tool_names;` and
  `use hosts::vendor;`, so their importers don't change.
- `git mv src/vendor.rs src/hosts/vendor.rs`, `src/tool_names.rs src/hosts/tool_names.rs`,
  and both baselines; declared `pub(crate) mod` in `src/hosts.rs`.
- SOURCE ids: the four `pub const SOURCE` lines in claude_code, codex, cursor and
  antigravity move (with doc comments) to `hosts` as `CLAUDE_CODE_SOURCE`,
  `CODEX_SOURCE`, `CURSOR_SOURCE`, `ANTIGRAVITY_SOURCE`; each old site keeps
  `pub use crate::hosts::X_SOURCE as SOURCE;`, so no importer changes.
- `HOST_DIRS` moves from `src/service.rs`; service keeps `pub use crate::hosts::HOST_DIRS;`.
- The rename: `between_steps::WATCH_LIMIT` and `between_steps::HOST_TIMEOUT_SECONDS` move
  to `hosts`, the second renamed `WATCHER_TIMEOUT_SECONDS` (in `hosts` a bare
  `HOST_TIMEOUT_SECONDS` would read as the 5-second `install::HOST_TIMEOUT_SECONDS`).
  between_steps keeps `pub use crate::hosts::{WATCH_LIMIT, WATCHER_TIMEOUT_SECONDS};`;
  its test and `src/install.rs`'s two uses take the new name. That removes install's
  only edge into between_steps (an application module). Value unchanged (1260).
  Assumption: LAYOUT.md doesn't name which constant is renamed; this is my reading.
- `namespaces.tsv`: `vendor` and `tool_names` rows become `namespace hosts utility`.
- Baselines: `--record` for `src/hosts.rs`; `--tighten` where counts shrank. Edge rows
  that stop being debt (install -> between_steps) are left for wave 3, per the recipe,
  unless the check requires otherwise.

## Test order and checks

No new tests: the moves and the rename are guarded by the unchanged suite (the
between_steps test asserting WATCH_LIMIT + overrun < the watcher timeout, and the install
tests pinning the watcher entry's `timeout` JSON). Per commit: `git diff -M
--color-moved`, `cargo fmt --all -- --check`, both clippy commands, `python3 -I -B
scripts/ratchets.py --check .`, full `cargo nextest run --workspace --locked` and
`cargo test --doc --workspace --locked`; `git diff --stat tests/golden` empty.

## Shared contracts touched

No wire protocol, CLI text, MCP schema, store or hook text. Shared files: `src/lib.rs`,
`src/install.rs` (one `pub mod` line, two renamed uses), `src/dashboard.rs` (one path;
the dash-feed session edits that file), the four host adapters (one line each),
`scripts/ratchets/namespaces.tsv`, and two docs tables.
