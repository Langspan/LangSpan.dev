# Plan: split src/hook.rs (f-split-hook, part of #300)

LAYOUT.md wave 2, C1: the body of `src/hook.rs` goes to
`hook/{tool_call, lifecycle, workspace, admit, observe, reads, deny}`.
LAYOUT.md has no section "Where the pending splits land"; the C1 line in
"Waves" is the destination used here.

## Scope of this slice

Moves only, no behavior change. `hook` stays at its path, so `lib.rs` and
every importer are untouched: the root re-exports each moved `pub` and
`pub(crate)` item under its old name (`hook::execute`, `hook::deny_stale`,
`hook::BRIEFING_LIMIT`, ...).

Not in this slice, and why:

- port types (`Config`, `Reply`, `Identities`, `Phase`, `Operation`,
  `ShellCall`, `LifecycleKind`, `REASON_LIMIT`) -> `hosts/port.rs`: the
  `hosts` namespace root is being created by lay-b-hosts (B5, branch
  `lay/lay-b-hosts`, not merged). They stay in the hook root until then.
- worktree install (`worktree_install`, `install_at`, `project_install`,
  `covered`) -> `install/worktree.rs`: `src/install.rs` is changed by the
  same in-flight B4/B5 branch. They go to `hook/workspace.rs` now with the
  rest of workspace resolution; the later move is one file to one file.
- adapters (`claude_code.rs`, `codex.rs`, ...) into `hook/`: touched by B5.
- `hook/render.rs` -> `briefing/render.rs` is B6. coord-299 reserves
  `src/hook/render.rs`; this slice doesn't edit it.

## Where each part of hook.rs goes (line numbers at 18f4354c)

| Destination | Items |
| --- | --- |
| `hook.rs` (root) | module doc, imports, `mod` lines, re-exports; port types (54-271, `LifecycleKind`); `Session`, `Failure` and the coordinator calls `call`, `call_by`, `attach`, `default_presence`, `set_presence`, `look`, `report_liveness` (1798-1982 less the wait types) |
| `hook/tool_call.rs` | `subagent_session`, `headless_host`, `execute`, `report_request_id`, `post_request_id`, `resend_owed`, `resend`, `input_fingerprint`, `canonical` |
| `hook/lifecycle.rs` | `BRIEFING_LIMIT`, `CONTEXT_BUDGET_BYTES_PER_TURN`, `BRIEF_PEERS`, `LifecycleEvent`, `TurnKind`, `failure_type`, `lifecycle_event`, `prompt_parts`, `CodeAsk`, `record_prompt`, `lifecycle_parsed`, `lifecycle_resolved`, `no_session_identity`, `lifecycle`, `since_section`, `own_session`, `closed_runs`, `turn_event`, `Session::{stop, batch}`, `report_shown`; the briefing delivery `Session::{news, forget_unplaced, record_context, code_context}` |
| `hook/workspace.rs` | `resolve_workspace`, `worktree_install`, `install_at`, `resolve_user_workspace`, `adopted_workspace_id`, `project_install`, `covered` |
| `hook/admit.rs` | `WAIT_POLL`, `Waited`, `WaitEnd`, `Session::{pre, wait_for_holders, record_wait}`, the break check (`BREAK_FILES`, `BREAK_LATE`, `add_alert`, `Session::{break_alert, written_calls, written_text, break_start_by, checked_calls}`), `Located`, `located_files`; test module `journal_lookup` |
| `hook/observe.rs` | `Session::{observe, record_source, read}`, `observed_files` |
| `hook/reads.rs` | `looked_key`, `search`, `READ_RANGE_FIELDS`, `OUTLINE_LIMIT`, `OUTLINE_ENDING`, `outline_read`, `read_bounded`, `line_count`, `grouped`, `outline_text`; test module `outline_text_tests` |
| `hook/deny.rs` | `deny_stale`, `deny_stale_with`, `deny_unread`, `failure_reply`, `deny`, `deny_after_wait`, `deny_held`, `overlap`; the stale-refusal test (`version`, `stale_reason`, `a_stale_refusal_names_...`) from `briefing_text` |
| `hook/briefing_text.rs`, `hook/code_briefing_text.rs`, `hook/placed_briefing.rs` | the three briefing-text test modules, declared `#[cfg(test)] mod X;` in the root so their `use super::*` keeps its meaning and their bodies move unchanged |

Expected sizes: root about 450 lines, the largest new file `lifecycle.rs`
about 1,000; the three briefing test files are about 1,070, 720 and 250.
No file over 2,000.

The briefing test modules test `render.rs` and `prompt.rs` through the hook
root's imports. Putting them inside `render.rs` would take it to about
3,700 lines and edit a file coord-299 holds; they sit beside it as separate
test files, and move with `render.rs` to `briefing/` in B6.

## Mechanics

- Each new file starts with `use super::*;` so the moved lines compile
  unchanged against the root's imports. Each move is cut-and-paste of whole
  line ranges (scripted with `sed -n`, never retyped), checked with
  `git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change`.
- The only non-move edits: `mod` and `pub use`/`use` lines in the root;
  `pub(super)` on items and `Session` methods a sibling now calls; an
  `impl Session<'_> {` header and `}` where an impl block is divided;
  test modules lose one indentation level when they become files.
- Ratchets: `--record` for the new files, `--tighten` for `hook.rs`
  (its `fn` rows for `execute`, `lifecycle` and `pre` move to the new
  files' baselines; edge rows are per namespace and stay in the root's),
  in the same commit. The three briefing test files are test-only
  (`#[cfg(test)] mod`), so the ratchets don't measure them.
  `scripts/coverage-baseline.tsv` keeps its `src/hook.rs` row (the file
  still exists; the report only checks the ten largest files).

## Test-first order

A pure move adds no behavior, so there is no new failing test. The guard is
the existing suite unchanged:

1. Before: `cargo nextest run --workspace --locked` on the merged base, record the count.
2. Move one destination at a time; after each, `cargo build` and
   `cargo nextest run -p langspan -E 'test(/hook::/)'`.
3. After all: `cargo fmt --all -- --check`, both clippy commands,
   `python3 -I -B scripts/ratchets.py --check .`, the full nextest run and
   doc tests; the same test count, goldens (`tests/golden/`) byte-identical
   (`git diff --stat tests/` empty).

## Shared contracts touched

None. No wire format, CLI text, hook reply, MCP schema, store or `lib.rs`
path changes. Shared files edited: `src/hook.rs` (also reserved by
coord-299, p-213-2 and lay-b-ids-fs; a later merge will conflict there if
they edit the moved lines) and the ratchet baselines listed in the
reservation.

## PR size

About 3,500 moved lines and an estimated 100 to 150 non-move lines. A split
to get under 2,000 lines can't be smaller than the moves; the PR body gives
the `--color-moved` evidence so review covers only the non-move lines.
Refs #300 (it stays open: the other large files and the docs splits).
