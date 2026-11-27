# Plan: repoint the hook importers (f-imports, LAYOUT wave 3; #343, #344)

Refactor with no behavior change, by the LAYOUT.md move recipe: moved lines
are cut and pasted whole, the old site keeps a `pub use`, goldens and every
existing test pass unchanged (`git diff --stat tests/` empty).

## Where things stand (origin/main e81189e7)

Largest dependency cycle, from the cycle `edge` rows (the ratchet records
every cycle edge; the stale rows hook -> antigravity and
install -> between_steps left out): 13 modules, 34 edges: hook, adopt,
antigravity, briefing, claude_code, codex, commit, formatter, install,
journal, lines, reformat, user_install. #343's target (14 or fewer) already
holds; this task lowers it further and reports both numbers.

What still names `hook` outside its namespace, and who changes it:

| Importer | What it takes | Who |
| --- | --- | --- |
| briefing/{recent,since,trail}, formatter, report, today, daemon/*, install, install/detect, hook/workspace | text and ids helpers, `BRIEFING_LIMIT`, `wire_id`, `install_at` | v-363 (reserved; not this task) |
| commit | `Config`, `Reply`, `ShellCall`, `REASON_LIMIT` (port types) | this task |
| cli/{check,request,hook}, cli/render{,/messages,/recaps}, skill, demo | `wire_id`, `workspace_request_id`, `read_stdin_until`, `age`, briefing render items, port types | this task |
| claude_code, codex, antigravity, cursor, between_steps | ids, input and briefing items through `hook::` | this task (adapters may keep using the hook root's own items) |

`hook` names `install` in `hook.rs` (`Config` holds `install::Vendor` and
calls `install::Presence`) and in `hook/workspace.rs` (worktree install,
v-363's file). `commit` names `install` for `install::project_registrations`.

## Changes

1. Port types to `src/hosts/port.rs` (LAYOUT C1 "port -> hosts/port.rs"; no
   one else holds it). Move from `src/hook.rs`, unchanged: `REASON_LIMIT`,
   `Config` and its `impl`, `Reply` and its `impl`, `Identities`, `Phase`,
   `Operation`, `ShellCall`, `LifecycleKind`. `identities_for` stays in hook
   (it calls `ids::wire_id`, and hosts -> ids is not an allowed edge).
   `hook.rs` gets `pub use crate::hosts::port::{...}` (and
   `pub(crate) use` for `REASON_LIMIT`), so every path keeps working.
   `crate::install::Vendor` inside the moved lines becomes
   `crate::hosts::vendor::Vendor` (the type's home; same type).
2. `Presence` and `PRESENCE_MINUTES` to `src/hosts/port.rs`, so the port
   doesn't name install: move the struct, the const, `Default` and the
   `window_ms` and `minutes` methods from `src/install.rs`; install keeps
   its private `words` and `parse` in its own `impl Presence {` block and
   `pub use crate::hosts::port::{PRESENCE_MINUTES, Presence};`.
   `src/hosts.rs` gets `pub(crate) mod port;`.
3. Repoint importers to the leaf modules (use lines and paths only):
   commit -> `hosts::port`; cli/check, cli/request -> `ids`; cli/hook,
   claude_code -> `fs::input`; cli/render/messages -> `text`; cli/render,
   cli/render/recaps, skill, between_steps -> `briefing::render`; demo ->
   `hosts::port`; the adapters' `wire_id`, `file_key`, `valid_host_id`,
   `MAX_EVENT_BYTES`, `read_stdin_until` -> `ids` / `fs::input`, and
   `BRIEFING_LIMIT` -> `briefing`. Their public re-exports
   (`claude_code::{wire_id, ...}`, `codex::MAX_EVENT_BYTES`) keep their
   names and now point at the leaf.
4. Delete the hook re-exports that lose their last user: in the hook root, a
   `pub use` of a leaf item that nothing outside `hook` (src/ and tests/)
   names any more becomes a private `use` (the submodules' `use super::*`
   still sees it), or goes when nothing uses it. Kept public because tests/
   name them: `BRIEFING_LIMIT`, `user_workspace_id`, `wire_id`,
   `UNCOORDINATED`, `render_briefing`, `render_code_briefing`,
   `identities_for`, `input_fingerprint`, `CONTEXT_BUDGET_BYTES_PER_TURN`.
   lib.rs's compatibility aliases are not touched (other wave 3 tasks).

Not done here, and why:

- `commit -> install` (`project_registrations`): it needs install's whole
  settings parser (`read`, `managed_hooks`, `parse_project_hook`,
  `Registration::command_body`, about 250 lines). LAYOUT keeps that in
  `install` (C4), and commit (application) -> install (infrastructure) is an
  inward edge LAYOUT allows. It stops being cycle debt once install no
  longer names hook (v-363). Moving the parser to a leaf would contradict
  LAYOUT, so this criterion of #344 is left for the owner to decide.
- `hook/workspace.rs -> install`: v-363's file; LAYOUT moves the worktree
  install to `install/worktree.rs`, after which hook -> install is the same
  allowed inward edge.

## What changed from this plan while implementing

- `Operation` stays in `hook`: it names `regions::Edit`, and `hosts` and
  `regions` are both utility namespaces with no `allow hosts regions` row,
  so moving it would add a same-layer debt edge.
- The binary's files (`cli/check.rs`, `cli/request.rs`, `cli/hook.rs`) keep
  `langspan::hook::` paths: `ids`, `input` and `hosts` are private modules,
  and the ratchet doesn't count `langspan::` paths there as edges.
- `antigravity` keeps `hook::BRIEFING_LIMIT` and `between_steps` keeps
  `hook::BRIEFING` and `hook::interface_impact_line`: naming `briefing`
  directly is a new cycle edge (antigravity) or a same-layer edge
  (between_steps), which the ratchet refuses.
- `compose_context` stays a public hook re-export: it has no other path, so
  making it private leaves it dead code in `briefing/render.rs`.

## Test-first order

A pure move adds no behavior, so there is no new failing test; the guard is
the suite unchanged.

1. Baseline: `cargo nextest run --workspace --locked` on the merged base;
   record the count.
2. Step 1+2 (port move), then `cargo build`, `cargo nextest run -p langspan
   -E 'test(/hook|install|commit/)'`.
3. Step 3+4 (repoints, re-export removal), then `cargo build`; an unused
   import or a private item a test needs fails here.
4. `cargo fmt --all`, both clippy commands, `RUSTDOCFLAGS='-D warnings' cargo
   doc`, `python3 -I -B scripts/ratchets.py --check .`, full nextest and doc
   tests; `git diff --color-moved=dimmed-zebra` shows the moves as moves.
5. Ratchets in the same commit: `--record src/hosts/port.rs`, `--tighten`
   for hook.rs, install.rs, commit.rs and each touched file that shrank
   (commit's `edge hook` row goes). Recompute the cycle from the rows and
   report before and after.

## Shared contracts touched

None: no wire format, CLI text, hook reply, MCP schema, store, golden or
public library path changes (`langspan::hook::Config` and the rest stay
re-exported). Shared files: `src/install.rs` (also reserved by v-363; my
edit is lines 316-352, the `Presence` block), `src/commit.rs` (c10-rebuild),
`src/hook.rs` and `src/cli/*` (older reservations).

## Size

About 200 moved lines (step 1 and 2) and 40 to 60 non-move lines. Refs #343
until v-363's importers land; Refs #344 (commit -> install as above).
