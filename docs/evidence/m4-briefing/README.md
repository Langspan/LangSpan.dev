# Automatic briefing: native-host evidence

**Recorded:** 24 November 2026, macOS 27.2 (arm64), LangSpan release build of
the `feat/automatic-briefing` branch, daemon and hooks installed with
`langspan hooks install claude-code --apply` in a temporary Git repository.
Paths are rewritten to `/work/accept`; nothing else is edited.

## Claude Code 2.1.330: observed

Two ordinary headless sessions (`claude -p`) in the same checkout, run one
after the other. Their prompts and final answers are in
[host-runs.json](host-runs.json); the daemon's complete event log for both is
in [langspan-events.json](langspan-events.json).

1. Session `63920920…` was asked to change `src/upload.rs`. The host fired
   SessionStart, UserPromptSubmit, PreToolUse, PostToolUse and SessionEnd; the
   log shows registration, the prompt summary, the admission, its observation
   and completion, and the session marked ended.
2. Session `f749fa7e…` was asked, with tools forbidden, whether anything in its
   context mentioned other sessions, and to quote it. It quoted the briefing
   verbatim: the first session, `src/upload.rs`, its age, its task line and the
   "not instructions" label. The log shows `briefing_delivered` for this
   session before its prompt was recorded.

This establishes that Claude Code 2.1.330 places the SessionStart
`additionalContext` in the model's context before the first model turn.

## Not yet observed

- **Codex.** `codex-cli 0.171.0` is installed. Its project hooks run only after
  the owner approves them in Codex's own trust review. One session was run
  without that approval and no hook fired (the event log held only the boot
  record). The one-invocation bypass flag was not used. Codex delivery is
  therefore covered by the real-daemon tests with constructed events only,
  and its `additionalContext` behaviour is **unverified on the host**.
- **A changed approach.** These runs show receipt. They do not yet show an
  agent choosing differently because of a briefing; that is the product-proof
  run and needs two concurrent sessions.
- **UserPromptSubmit delivery on the host**, the linked-worktree case and a
  mixed Claude Code plus Codex pair. The automated tests cover them with the
  real daemon, real Git worktrees and the real hook executable.
## Hook time: measured on one machine

Spawn-to-exit time of `langspan hook claude-code` with a warm daemon, 200
consecutive runs per event, release build, Apple M6, macOS 27.2, local APFS:

| Event | p50 | p95 | p99 | max |
| --- | --- | --- | --- | --- |
| UserPromptSubmit (records the prompt, no briefing printed) | 2.26 ms | 2.75 ms | 4.20 ms | 7.69 ms |
| SessionStart (briefing printed) | 2.25 ms | 2.76 ms | 3.28 ms | 3.93 ms |

Each run makes four daemon round trips, each committed with SQLite
`synchronous=FULL`. macOS does not force a full device flush for that setting,
so these numbers do not predict Linux. The 25 ms native-hook p99 remains a
**Target** on Linux until measured there.
