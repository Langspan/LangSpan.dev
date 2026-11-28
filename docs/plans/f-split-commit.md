# Plan: split src/commit.rs into the ownership namespace (LAYOUT C6)

Part of #300 (and #372). `src/commit.rs` is 3,508 lines (2,866 production lines
in its ratchet baseline). LAYOUT.md wave 2, C6, puts it at
`ownership/{commit, commit_diff, attribution, commit_mine, status_mine}`. D3
(the ownership namespace) hasn't landed, so this PR creates the root with the
move recipe and moves only commit; journal, lines, reformat, formatter and
adopt stay where they are for D3.

## Destination (line ranges of today's src/commit.rs)

| File | Contents | About |
| --- | --- | --- |
| `src/ownership.rs` (new root) | module doc; `pub mod commit;` and the four private siblings | 10 |
| `src/ownership/commit.rs` (`git mv` of src/commit.rs) | module doc (ADR link fixed to `../../docs/...`), the guard: `diff`, `untracked`, `taken`, `Guarded`, `live_sessions`, `guard_hook`, `record_held`, `held_reply`, `quote`, `truncate` (1338-1727), the shared constants, re-exports of every item callers name at `commit::`, and the unit-test module (2875-3508) | ~1,150 |
| `src/ownership/commit_diff.rs` | Diffs: `Hunk`, `Piece`, `FileDiff`, `parse_diff`, `patch`, `apply_exactly`, `in_one_order` (61-430), plus `span`/`lines_of` if only diff code uses them | ~380 |
| `src/ownership/attribution.rs` | Whose lines: `Owner`, `is_own`, `hunk_owner`, sides, runs, pieces, `split_hunk`, `Copies`, `committed_since`, `FileOwners`, `file_owners`, `is_live` (430-1335) | ~900 |
| `src/ownership/commit_mine.rs` | `Left`, `Refusal`, `MineReport`, `plan`, `git_in`/`git_bounded`, `stage`, `Hooks`, `commit_on`, `running_session`, `checkout_paths`, `commit_mine`, `update_shared_index` (1728-2575) | ~850 |
| `src/ownership/status_mine.rs` | `MINE_LISTED`, `MineHunk`, `MineFile`, `MineStatus`, `mine_file`, `staged_paths`, `status_mine`, `current_head`, `private_name` (2576-2874), each helper going to the file whose code calls it | ~300 |

Every file ends under 1,500 lines (the ratchet's limit), so no file needs a
size row and #300's 2,000-line bar holds for all five.

## Rules followed

- Lines are cut and pasted, never retyped; checked with `git diff --color-moved`.
  The only edits on moved lines: a private item another sibling or the tests use
  becomes `pub(super)`; each new file gets a one-line `//!` doc and its own `use`
  block; relative doc links fixed.
- Old paths unchanged: lib.rs replaces `pub mod commit;` with `mod ownership;`
  (private, as `mod briefing;` is, so the crate's pub count holds) and adds
  `pub use ownership::commit;` to the compatibility block.
  `ownership/commit.rs` re-exports every item callers reach as `crate::commit::X` /
  `langspan::commit::X` (`HELD`, `Guarded`, `guard_hook`, `is_own`, `is_live`,
  `live_sessions`, `commit_mine`, `running_session`, `status_mine`, and every
  formerly `pub` type: `Hunk`, `FileDiff`, `parse_diff`, `Owner`, `Left`,
  `Refusal`, `MineReport`, `MineHunk`, `MineFile`, `MineStatus`, `MINE_LISTED`).
  No importer changes (adopt, journal, formatter, briefing/trail, hook/shell_hooks,
  skill, mcp, cli/local).
- Tests stay together in `ownership/commit.rs` (`use super::*` plus the siblings'
  items); none is edited.
- Ratchets: `git mv` `baseline/src/commit.rs.tsv` to
  `baseline/src/ownership/commit.rs.tsv` (keeps `edge install`, `edge journal`),
  `--record` the four new files, `--tighten` ownership/commit.rs, in the same
  commit, keeping its `edge install` and `edge journal` rows as they were
  (recipe step 6; `--tighten` would drop `edge install`). The root needs no
  baseline: lib.rs's still counts the one `pub mod`. namespaces.tsv:
  `namespace commit application` becomes `namespace ownership application`.
  Edge rows naming `commit` in other baselines (adopt, journal, formatter,
  hook, briefing, briefing/trail) resolve through the alias and stay unedited
  (recipe step 6).
- `scripts/coverage-baseline.tsv`: row `src/commit.rs` renamed to
  `src/ownership/commit.rs` in the move commit; after the split, each file
  gets the row `scripts/coverage.sh` measures for it (every one has 100
  production lines or more, so each needs one).
- `docs/IMPLEMENTATION_STATUS.md:164`: path `src/commit.rs` updated.

## Order and checks

1. `git mv src/commit.rs src/ownership/commit.rs`, the baseline the same way, root,
   lib.rs, namespaces.tsv. `cargo build`; ratchets `--check`. Commit (pure move).
2. Cut each block into its sibling, one at a time, `cargo check --all-targets`
   after each.
3. Ratchet `--record`/`--tighten`; coverage row; status doc.
4. `cargo fmt --all -- --check`, both clippy commands, `RUSTDOCFLAGS='-D warnings'
   cargo doc`, full `cargo nextest run --workspace --locked`, `cargo test --doc`,
   `python3 -I -B scripts/ratchets.py --check .`. Goldens unchanged
   (`git diff --stat tests/golden` empty).

No test is added: behavior doesn't change, and the existing unit, golden,
contract and property tests are the check.

## Shared contracts touched

None of the protocol, CLI, MCP, store or hook texts. Shared files: `src/lib.rs`
(two lines), `scripts/ratchets/namespaces.tsv` (one row, which v-363 is also
editing nearby), `scripts/coverage-baseline.tsv` (one row),
`docs/IMPLEMENTATION_STATUS.md` (one path).

## Size

About 3,500 moved lines and under 80 lines of new or edited code (re-exports,
`use` blocks, visibility, root). Issue #300 asks for one file per PR, so this is
one PR; `--color-moved` shows the rest is moves.

## Done when (#300)

This PR takes `src/commit.rs` off #300's list. #300 stays open (hook.rs, store.rs,
mcp.rs, shell.rs, main.rs, install.rs, index_queries.rs, core checks.rs and the
docs splits remain), so the PR says "Refs #300".
