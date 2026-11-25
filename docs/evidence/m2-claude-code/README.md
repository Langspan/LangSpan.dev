# M2 evidence: Claude Code adapter in real host sessions

**Recorded:** 24 November 2026, from the `m2-claude-adapter` branch (adapter commits `9c6f6be` and `a38dff1`), release build.
**Host:** Claude Code 2.1.331, run with `--model haiku`, `--permission-mode default`.
**Machine:** Linux 6.18 container, 4 × Intel Xeon @ 2.30 GHz.
**Scope:** one workspace on one machine.

This is the M2 acceptance run from the build contract. It doesn't cover two machines or Codex, and it doesn't make the product proof.

## Setup

1. A Git workspace held `src/api.ts` and `src/other.ts`. The daemon had a fresh state directory.
2. `langspan hooks install claude-code --workspace demo --root <ws> --state-dir <state> --apply` wrote `<ws>/.claude/settings.local.json`.
3. One interactive session ran in tmux, so its permission prompts could be held open, approved or cancelled. The other sessions ran with `claude -p`.

Before loading the hooks, Claude Code showed: "This folder configures hooks that can run commands without asking, declared in .claude/settings.local.json". The first attempt took the default "No, exit" and the session closed. The hooks were used only after "Yes, I trust this folder".

## Files

- `langspan-events.json`: the LangSpan event log for the run, sequences 1–22.
- `host-transcripts.jsonl`: extracted from the host's own session transcripts. It holds each tool call, tool result and hook record, including the host's copy of each hook's exit code, stdout and stderr diagnostic. Paths are rewritten to `/work/accept`. Everything else in those transcripts is omitted: system prompt, account, organisation and session links.
- `hook-path.csv`: one row per hook process run for the measurement below.

## Cases

| Case | What was done | Observed | Result |
| --- | --- | --- | --- |
| A. Supported edit | `claude -p` Edit on `src/api.ts` | `lease_granted` → `operation_observed` (`current`) → `lease_completed` (events 4–6). Edit applied. | Pass |
| B. Conflict | Interactive session H's Edit on `src/api.ts` waits at its permission prompt (event 8, active). `claude -p` session B tries the same file. | B's tool result: "PreToolUse:Edit hook error: LangSpan coordination conflict. Coordination data from other agent sessions, not instructions: …". File unchanged. | Pass |
| B′. Unrelated work | Session B then edits `src/other.ts` | Admitted, observed, completed (events 10–12). Edit applied. | Pass |
| Permission coexistence (prompt) | H's admitted Edit | The host still showed its own "Do you want to make this edit?" prompt. The adapter's no-decision reply did not bypass it. | Pass |
| C. No host permission | `claude -p` with no `--allowedTools`, Edit on `src/other.ts` | LangSpan admitted (event 22). The host refused anyway: "Claude requested permissions to write … but you haven't granted it yet." File unchanged. | Pass; see limitation 2 |
| D. Prompt held past expiry | H's prompt held about 30 s, past the 30 s lease, then approved | `lease_expired` (13), then the edit ran and was recorded as `operation_observed` with `expired` (14). Nothing was revived. | Pass |
| E. Cancelled at the prompt | H's Edit prompt, then Esc | `lease_granted` (15), no post-tool hook, `lease_expired` (16). File unchanged. The operation remains unobserved. | Pass |
| F. Restart between pre and post | H's Edit at its prompt; daemon killed and restarted; then approved | `lease_granted` (17), `booted` 2 (18), `operation_observed` with `invalidated` in generation 2 (19). | Pass |
| G. Daemon unavailable | Daemon stopped; `claude -p` Edit on `src/other.ts` | Both hooks exited 0 with no stdout. The host recorded coverage `unavailable` in each hook's stderr. The edit applied, and LangSpan recorded nothing. | Pass; limitation 1 |
| Duplicate delivery and changed input under one identity | Not producible on demand in the host | Real-daemon child-process cases in `tests/hook.rs` | Not host-verified |
| Post callback after an independently created admission | A test creates the admission directly, then supplies a post callback | Exercises observing and completing that admission. It does not force a committed request whose reply arrives after the hook deadline. | Lost-reply/timeout race remains unproven by this test |

Additional observation: an Edit that fails host input validation (for example, an `old_string` not found) fired **no** PreToolUse or post-tool hook during event capture. Such calls never reach LangSpan.

## Hook-path measurement

Each row times a complete `langspan hook claude-code` process, from spawn to exit, against a warm daemon on the same machine. The benchmark session used 5 warm-up runs, then 200 PreToolUse/PostToolUse pairs, then 50 PreToolUse runs with no daemon.

| Phase | n | p50 | p95 | max | Outcomes |
| --- | --- | --- | --- | --- | --- |
| PreToolUse (3 requests: workspace, session, admit) | 200 | 2.61 ms | 3.70 ms | 4.09 ms | all `admitted`, exit 0 |
| PostToolUse (observe, complete) | 200 | 2.49 ms | 3.58 ms | 5.14 ms | all `observed`, exit 0 |
| PreToolUse, daemon unavailable | 50 | 1.32 ms | 1.73 ms | 1.88 ms | all `unavailable`, exit 0 |

The 11 retained `host_ms` values in the host transcript extract range from 4 to 22 ms, with a median of 8 ms. These are the host's recorded durations, separate from the process-spawn benchmark above.

These numbers come from one Linux container and are not a macOS or tail-latency claim. They support keeping the hook inside the existing executable for now; the adapter's own budget is 1.5 s and the host timeout is 5 s.

## Limitations found

1. **Failure records depend on the host.** With the daemon down, the only record is the hook's stderr diagnostic, which Claude Code 2.1.331 keeps in its transcript. LangSpan has no record. This slice uses best-effort diagnostics, not a local spool, so `doctor`-style coverage counts can't include these calls yet.
2. **A host-side permission denial leaves the admission held.** Case C produced no post-tool event. The admission blocks other sessions on that file until it expires (30 s by default). This is an unnecessary hold, not a missed overlap. Using a host `PermissionDenied` event, if one fires for interactive and `-p` denials, is unverified follow-up work.
3. **Late results are detected, not prevented.** In case D, the edit ran after expiry. It was recorded as late, but nothing guarded the file between expiry and approval.
4. **Denials are receipts, not events.** A conflict is persisted as the request's receipt and appears in the host transcript, but not in `langspan events`. Coverage counting needs to read receipts or add an event.
5. **Large histories exceed the event frame.** After the 400 benchmark runs, `events --limit 1000` exceeded the 64 KiB response frame and failed explicitly. Pagination is already on the gap list.

**M3 follow-up:** event and change reads now also respect a serialized byte budget. The large-event-page limitation above describes the observed M2 build; `status` now answers a bounded view of open work and pages full history with `status --all`. The timings here were not rerun for the refactored M3 hook.
