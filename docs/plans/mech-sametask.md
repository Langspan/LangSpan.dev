# Plan: tell two sessions when their prompts are the same task (#537)

## What exists

`brief` (`crates/langspan-core/src/briefing.rs`) already reads each running
session's recorded prompt targets (`prompt_targets_set`) and lists a peer's
targets that meet the asking session's work or targets as `target_overlap`
("task points at your files"). Nothing says "this is the same task", and
nothing names it as such on both sides.

## Rule (deterministic, no model judgment)

For sessions A and B with prompt targets T (within `BRIEF_HORIZON_MS`, this
generation, not ended) and claims C (held write admissions):

    same_task(A, B) = T(A)∩T(B) ≠ ∅  or  T(A)∩C(B) ≠ ∅  or  C(A)∩T(B) ≠ ∅

Symmetric, so both sessions get it. Claims alone (both edit one file) stay
plain `overlap`, not same task. A subagent's line for its parent
(`task_known`) never carries it, as with `target_overlap`.

## Changes

- `crates/langspan-core/src/briefing/same_task.rs` (new, `mod same_task;` in
  `briefing.rs`): the pure predicate over two (targets, claims) pairs, and
  the item key `peer|<id>|same_task`.
- `crates/langspan-core/src/briefing.rs`:
  - `PeerWork.same_task: bool`, `#[serde(default, skip_serializing_if = "is_false")]`.
  - `brief`: compute it per peer (my claims from `work_paths`).
  - signature: append `|same_task` only when true, so older signatures are
    unchanged and the notice makes a briefing new once.
  - `briefing_keys` / `keep_changes`: its own key, told once; while it is
    fresh the peer's task line is kept even if the header was told, so the
    notice names the other session's task line.
- `src/briefing/render.rs`: the peer line starts with
  `same task as yours: take a different part or agree who does it;` right
  after the header, so a shortened line keeps it.
- PeerWork struct literals in `src/hook/{briefing_text,code_briefing_text,placed_briefing}.rs`
  get `same_task: false` (plus a render test in `briefing_text.rs`).
- Wire: new golden `fixtures/same-task-briefing-response-v3.json`; test in
  `tests/protocol.rs` that it round-trips and that the shape without the
  field (older daemon) still reads as `false`. No version bump: additive,
  defaulted field, same as `target_overlap`.
- `docs/adr/0004-automatic-peer-briefing.md`: the rule and its limits.
- Ratchets: `briefing.rs` is over the 1,500-line file limit at baseline and
  `brief`/`render_peers_placed` over the function limit; logic goes in the
  new file, and any remaining few lines of growth are recorded with
  `--record` and justified in the PR.

## Test-first order

1. Core (`crates/langspan-core/tests/briefing.rs`, in-process, no daemon):
   - two sessions whose targets share a file: each is told `same_task` once
     (first briefing changed, the next unchanged, both sides);
   - a session whose target is a file the peer holds: both sides told;
   - disjoint targets: no `same_task` either side (no false match);
   - two sessions only editing one file, no targets: no `same_task`;
   - `changes_only`: told once, then counted unchanged.
   Watch them fail (field missing / false), then implement.
2. Render test in `src/hook/briefing_text.rs`.
3. Protocol golden vector in `tests/protocol.rs`.

## Shared contracts touched

The briefing wire shape (`PeerWork`, additive field) and the rendered peer
line. No CLI dispatch, daemon or store change.

## Not in this slice

"A recorded real-host session shows it" needs a retained native-host run;
listed under "Not done yet" in the PR (Refs #537) unless one can be retained.
