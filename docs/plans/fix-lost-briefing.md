# Plan: a briefing item is told only once the hook has it (#531)

## Cause

`Brief { deliver: true }` records `BriefingDelivered` with the told keys when
the core decides (`crates/langspan-core/src/briefing.rs`, `decide_brief`).
Under load the hook's call times out, or its process is descheduled past its
deadline, so it drops the reply (`src/hook/lifecycle.rs`, `news`'s `.ok()?`
and the prompt path's `Err(reply) => return reply`). The keys stay told, and
every later `changes_only` briefing leaves them out: the notice is lost.

A daemon-side fix (forget the keys when writing the reply fails) was
considered and rejected: a reply written into the socket buffer before a
descheduled hook wakes past its deadline is written successfully and still
lost, which is exactly the load case.

## Design: two-phase told keys

- `Command::Brief` gains `confirm: bool` (serde default, skipped when false).
  With `deliver` and `confirm`, the core records the delivery as now
  (signature, `told_at_ms`, messages, checks and the rest unchanged), but
  `told` keeps only the keys that were already told: the newly shown items
  stay untold. One line in `decide_brief`.
- New command `ConfirmTold { session_id, keys }` (at most `MAX_TOLD`). The
  hook sends the keys its text placed, right before it returns its output.
  The core adds the ones not yet told (this generation only, capped at
  `MAX_TOLD`) and records them with the existing `BriefingDelivered` event
  (same signature, `told_at_ms: None`). No new event type, so the stored log
  stays readable by earlier builds and replay is unchanged. A retry, or keys
  already told, records nothing. Outcome `ToldConfirmed { session_id,
  confirmed }`.
- A hook that times out never confirms, so the next briefing tells the items
  again. A confirm that fails costs an item told twice, the safe direction.
- Older shapes kept: a `Brief` without `confirm` (CLI, MCP, an earlier hook)
  tells at decision as today. `ForgetTold` stays, and the hook still sends it
  for unplaced keys so a daemon without the new capability behaves as now.
- Protocol: new capability `confirmed_briefings` (`None`: only a client that
  names it). `for_capabilities` clears `confirm` for a daemon without it;
  `fits_with` refuses `ConfirmTold` to one. Golden vectors for the command,
  the outcome and a `brief` with `confirm`.

Remaining window, stated in the PR: the hook confirms just before it prints;
a host that kills the hook between the two loses that one briefing's items.
Messages, checks and other one-time items are still recorded at decision;
not in #531's scope, listed as follow-up.

#489 (delivery records): this PR does not touch `InterfaceNoticesTold` or add
an event. `ConfirmTold` is the point where #489's per-notice record belongs;
noted in the PR.

## Test-first order

1. Core, in-process (`crates/langspan-core/tests/briefing_changes.rs`): a
   session is briefed with `confirm` about a peer's interface change, the
   hook "times out" (no `ConfirmTold`), and the next prompt's briefing
   carries the notice again; after `ConfirmTold` the following prompt does
   not. Also: a full view with `confirm`, a retried confirm records nothing,
   an unknown session and more than 256 keys are refused, a `Brief` without
   `confirm` still tells at once. Watch them fail before the change.
2. Decision golden: `confirm_told` steps in `briefing.steps.jsonl`, blessed.
3. Protocol (`tests/protocol.rs`, fixtures): vectors, capability reshaping.
4. Hook (`src/hook/lifecycle.rs`): `confirm: deliver` on both delivering
   briefs, `confirm_placed` before returning. Break it once and confirm a
   hook test in `tests/briefing.rs` catches it; add one if none does.
5. Loop the two named tests under load:
   `briefing::a_codex_session_is_told_both_names_of_a_rename_a_claude_session_made`
   and `benchmark_harness::dry_run_langspan_arm_following_briefings`
   (`--stress-count`, with the workspace suite as load).

## Progress

Steps 1 to 4 done. The core tests fail with the `decide_brief` filter
broken; with either hook `confirm_placed` call disabled, two hook tests each
fail (`briefing::a_starting_session_is_told_who_is_editing_what_and_only_again_when_it_changes`,
`briefing::news_since_the_last_prompt_reaches_the_model_after_its_next_tool_call`
and their neighbours). Step 5 and the PR remain.

## Shared contracts touched

The wire protocol (a `brief` field, a command, an outcome, a capability),
the core's command dispatch, the hook. Every `Command::Brief` literal gains
`confirm: false` (mechanical, in the test files listed in the reservation).
Docs: ADR 0004's told-keys paragraph, ADR 0037's generated table, the
threat model's briefing-memory row, the retries-and-recovery table.
