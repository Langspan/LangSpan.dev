# ADR 0002: Claude Code hook adapter

**Status:** Implemented in M2, following the owner's instruction on 24 November 2026 to begin M2 while the build contract stays Proposed. ADR 0001's guarantee is unchanged.

## Context

M2 connects one native host's edit path to the existing daemon. The contract asks for:

- phase-specific retry identities;
- observations that cannot revive leases;
- no `allow` from the adapter;
- an explicit choice of failure-record mechanism;
- a measured packaging decision.

Event shapes were captured from Claude Code 2.1.331 rather than assumed.

## Decision

- **Entry point:** `langspan hook claude-code --workspace ID --root DIR` handles PreToolUse, PostToolUse and PostToolUseFailure for `Edit` and `Write`, the tools whose shapes were captured. It runs without the async runtime.
  - Measured spawn-to-exit time with a warm daemon is 2.6 ms p50 and 3.7 ms p95 on Linux ([evidence](../evidence/m2-claude-code/README.md)).
  - On that measurement the hook stays in the existing executable. A separate binary would be reconsidered only if macOS or another host measures materially worse against its budget.
- **Retry identity:** `lsh1-<kind>-<32 hex>`, the first 128 bits of SHA-256 over length-prefixed fields: `"langspan-hook-id"`, `"v1"`, kind, adapter, host session ID, and host tool-call ID. Kinds `op`, `register`, `admit` and `complete` use the raw host fields above. The shipped M2 `observe-<phase>` derivation instead uses adapter, the derived session ID and the derived operation ID; this corrects the earlier prose without changing those IDs. M2 workspace registration uses `workspace` over its ID and canonical root. M3 scopes new workspace-registration attempts to the operation as recorded in ADR 0003, so a receipted temporary refusal does not block later attempts forever. Changing this derivation changes the identity of every in-flight retry, so it is versioned (`lsh1`) and pinned by golden vectors.
- **Missing identity:** an event without a usable `session_id` or `tool_use_id` is `unsupported`, and gets no decision. No identity is generated.
- **Host mapping:**
  - a conflict with a current holder → JSON `permissionDecision: "deny"`, exit 0;
  - everything else → no stdout, exit 0;
  - argument errors → exit 1, which the host treats as a non-blocking error (the between-step watcher exits 0 for them, and for every other failure, issue #288);
  - `allow` is never used, and the synchronous hook never exits 2. Only the between-step watcher below exits 2, to wake the model; it runs in the background, where Claude Code gives no exit code or decision field a blocking effect.

  The installer registers a 5 s host timeout and leaves `onFailure` unset. The adapter's own total deadline is 1.5 s.
- **Between-step interface notices (issue #214):** the installer adds a second entry to LangSpan's `UserPromptSubmit` group, the prompt hook's command with `--mode between-steps`, `"asyncRewake": true` and a 1,260 s timeout. Claude Code documents `asyncRewake` as running the hook in the background and waking Claude on exit code 2, with the hook's stderr shown to Claude as a system reminder ([hooks reference](https://code.claude.com/docs/en/hooks), command hook fields). The watcher asks the coordinator once a second with the `between_steps` command for interface notices relevant to the session that no briefing, write-time alert or earlier delivery told it. On one, it writes the notice to stderr in the briefing's line format and exits 2. It exits 0 without output when the session finishes the reply it started in, the session ends, or after 20 minutes. `Stop` reports the reply's `prompt_id`, and the watcher sends its own prompt's, so a reply that finished before the watcher's first answer still ends it; with no prompt ID, a stop after the first answer does. While the coordinator is unreachable or does not know the session, the watcher keeps asking for 10 seconds, since the prompt hook beside it may be starting the coordinator or registering the session, and exits after that. Each notice it tells is recorded as `interface_notices_told` with `via: between_steps`, and later briefings leave it out. A briefing's notices are recorded with `via: briefing` only when the prompt or tool hook reports that its capped text showed them (`interface_shown`). An ask that tells nothing writes no event and no receipt and is not the session's activity. Cursor runs Claude Code's hooks too: a Cursor event ends the watcher at once. Claude Code's channels, an MCP server pushing events into a session, are a research preview that needs a launch flag and can be switched off by an organization, so they are not used.
- **Observation:** the post-tool event records an `Observe` command, linked to the admission ID derived for the same tool call. Only while that admission is still valid does it then send `Complete`. Every field of the observe command comes from the host event, and the daemon assigns the time, so a retry is byte-identical.
- **Failure records:** best-effort diagnostics. Each run writes one structured JSON line to stderr with its coverage (`admitted`, `denied`, `observed`, `late_observation`, `observed_unadmitted`, `unavailable`, `timeout`, `unsupported`, `unknown`). Claude Code 2.1.331 keeps this line in its transcript. There is no local spool in this slice.
- **Installation:**
  - `hooks install|remove claude-code` is a dry run by default.
  - With `--apply` it writes `ROOT/.claude/settings.local.json`, a per-user file, not the team's committed `.claude/settings.json`.
  - It replaces only its own entries and keeps other settings and key order.

## Consequences

- With an unavailable daemon, LangSpan has no record of the call; only the host's transcript does. Coverage counts that need a durable denominator require a spool or another delivery mechanism later.
- A host-side permission denial emits no post-tool event, so its admission is held until the TTL ends (30 s by default).
- A prompt held past the TTL lets the write run unguarded after expiry. It is then recorded as a late observation.
- Duplicate host delivery, changed input under one identity, and an admission committed after the hook gave up are verified only against the real daemon in tests, not produced by the host on demand.
- Adding another host (Codex in M3) reuses the identity scheme with its own adapter name, and needs its own captured shapes and host mapping.
- The between-step watcher is built from Claude Code's documentation. Its tests run the real hook binary against a real coordinator and indexer, with a session that makes no hook call for 30 seconds; a live Claude Code run has not been retained. If a turn ends without a `Stop` hook (an interrupted reply, say), its watcher keeps running until the next `Stop` or for 20 minutes and can wake the idle session once with a relevant notice. A notice the coordinator recorded as told is lost if the watcher is killed before it writes stderr; it is never told twice.
