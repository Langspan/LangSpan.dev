# Plan: split src/mcp.rs, src/shell.rs and src/install.rs (f-split-mcp-shell-install, part of #300)

LAYOUT.md wave 2: C3 `mcp.rs -> mcp/{tool_list, args, frame} + capability
routers`, C4 `install.rs -> install/{hooks, registration, plan, settings}`,
C5 `shell.rs -> shell/{parse, snapshot, changes, record}`. LAYOUT.md has no
section "Where the pending splits land"; these C3 to C5 lines are the
destinations used here. Only the parts needed to bring each file under 2,000
lines move now; each moved part goes to its final file, so nothing moves twice.

Moves only, no behavior change. The three roots stay at their paths, so
`lib.rs` and every importer are untouched: each root re-exports the moved
`pub` items under their old names (`pub use child::X;`). Tests stay where
they are and still compile through `use super::*`.

## Commits (one per file)

1. `refactor: split src/mcp.rs into mcp/{tool_list, args, frame}` (2,472 lines
   today, about 1,540 after)
   - `src/mcp/tool_list.rs`: `HIDDEN_WHEN_HOOKED`, `HOOK_QUIET_MS`,
     `ToolList` and its impl, `hooks_reporting`, `compare_tool_names`
     (lines 24-163).
   - `src/mcp/args.rs`: every `*Args` struct and `HISTORY_LABEL`,
     `RECALL_LABEL` (lines 170-700). The root's tool methods read their
     fields, so each struct and field gains `pub(super)`; no other edit.
   - `src/mcp/frame.rs`: `ENVELOPE_ROOM_BYTES`, `CUT_NOTE`, `REPLY_BYTES`,
     `encoded_len`, `fits_one_frame`, `tool_result`, `with_note`,
     `bounded_result`, `continue_after`, `fitting_prefix`,
     `within_one_frame`, `recall_result`, `history_result`, `labelled`
     (lines 164-169, 870-1129). Functions the root calls gain `pub(super)`.
   - Root keeps `McpAdapter`, its watcher, `INSTRUCTIONS`, the
     `#[tool_router]` impl (the capability-router split is a later slice:
     it changes how the router is built, not a move), `ServerHandler`,
     `serve` and the tests.
2. `refactor: split src/shell.rs into shell/{parse, snapshot}` (2,445, about
   1,580 after)
   - `src/shell/parse.rs`: `MAX_COMMAND_BYTES`, `MAX_TARGETS`, `Token`,
     `write_targets`, `MAX_LOOKED_WORDS`, `looked_at`, the tokenizer and
     the per-command target parsers, `heredoc_write` (lines 25-640). No
     test names a private item here.
   - `src/shell/snapshot.rs`: `MAX_SNAPSHOT_FILES`, `MAX_SNAPSHOT_BYTES`,
     `MAX_STATUS_BYTES`, `Entry`, `Snapshot`, `git_line`, `snapshot`,
     `checked_out_branch`, `CheckoutLeft`, `checkout_left`, `unversioned`,
     `changed` (lines 641-890).
   - Root keeps run records (`record`, later), change notes and edit marks
     (`changes`, later) and the test modules.
3. `refactor: split src/install.rs into install/{plan, settings}` (2,386,
   about 1,860 after)
   - `src/install/plan.rs`: `remove_managed`, `plan`, `plan_for`,
     `plan_user_for`, `WATCH_WORDS`, the watcher helpers, `plan_hooks`
     (lines 800-1003).
   - `src/install/settings.rs`: `MAX_SETTINGS_BYTES`, `settings_metadata`,
     `read`, `read_mcp`, `parse_mcp_json`, `parse_json`, `read_text`,
     `parse_text`, `read_bytes`, `json_bytes`, `Planner`, `write_planned`,
     `write_planned_with`, `json_left_empty`, `LeftEmpty`, `Applied`,
     `write_or_remove_planned`, `write_or_remove_with`, `Written`,
     `write_bytes`, `install_at` (line 67, lines 1004-1326). The tests use
     `write_planned_with`, `write_or_remove_with` and `MAX_SETTINGS_BYTES`,
     so those gain `pub(super)` and the root imports them under
     `#[cfg(test)]`.
   - Root keeps the host constants, `Vendor`'s impl, `Registration` and
     `UserRegistration` (`registration`, later), the hook parsers
     (`hooks`, later) and the tests.

A child reaches the root's private items through `super::`, which Rust
allows for descendants, so only the child-to-root direction needs
`pub(super)`.

## Ratchets, coverage and docs

- `python3 -I -B scripts/ratchets.py --record` for each new file and
  `--tighten` for each root, in the same commit as its split. pub(super)
  doesn't count as pub, and `pub use` isn't counted, so the crate's pub
  total is unchanged.
- `scripts/coverage-baseline.tsv` keeps its `src/mcp.rs` and `src/shell.rs`
  rows (the files keep their paths); not edited.
- `docs/development-process.md`: the `install` hot component lists
  `src/install/` instead of only `src/install/detect.rs`.

## Checks, in order

No new behavior, so no new test: the existing unit, golden, contract and
property tests are the guard and must pass unchanged. For each commit:
`cargo build`, `git diff --color-moved=dimmed-zebra
--color-moved-ws=allow-indentation-change` (only `mod`/`use` lines and the
named `pub(super)` edits are not moved), the ratchet check, then the related
tests (`cargo nextest run --lib -E 'test(mcp::) | test(shell::) |
test(install::)'`, `--test cli_transcript`, `--test tool_count_in_docs`).
Before pushing: `cargo fmt`, both clippy runs, `cargo nextest run
--workspace --locked`, `cargo test --doc`.

## Shared contracts touched

None: no wire field, MCP tool name or schema, CLI text, settings format or
library path changes. The MCP tool schemas come from the same `*Args`
derives; field visibility doesn't reach the schema.

## Not done here (Refs #300)

The rest of C3 (capability routers), C4 (`hooks`, `registration`) and C5
(`changes`, `record`), and the other files #300 lists.
