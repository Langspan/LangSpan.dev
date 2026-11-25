# Claude Code and Codex together: native-host evidence

**Recorded:** 24 November 2026, macOS 27.2 arm64. Claude Code 2.1.330 and
Codex 0.171.0, both real, both unmodified, in one throwaway Git repository with
`langspan hooks install claude-code|codex --apply`. Codex's project and hook
trust were granted by the owner in Codex's own review; no bypass flag was
used. LangSpan release build of `feat/mid-turn-and-tool-coverage` plus the
Codex response fix in this commit. Paths are rewritten to `/work/demo`.

Prompts and final answers: [host-runs.json](host-runs.json). The coordinator's
complete event log: [langspan-events.json](langspan-events.json). Raw hook
events Codex sent: [fixtures/codex-0.171.0](../../../fixtures/codex-0.171.0).

## What happened

| Step | Host | What was asked | What was observed |
| --- | --- | --- | --- |
| 1 | Codex | Change `src/upload.rs` | Session registered, prompt recorded, edit admitted, observed and completed |
| 2 | Claude Code | No tools: quote anything about other sessions | Quoted the SessionStart briefing: the Codex session, `src/upload.rs`, its age and its task |
| 3 | Claude Code | Read and change `src/upload.rs` | Built on the Codex change and said so: "Another agent session working in this checkout had already added the check…" |
| 4 | Codex, resumed | Rename the function without re-reading the file | **Did not edit.** "The workspace requires me to reread `src/upload.rs` before editing it because another session changed it." |
| 5 | Codex, resumed | Make the call anyway | **Did not edit**, citing the same rule. No `apply_patch` call was made |

Steps 3 to 5 are the product-proof property the plan asks for: an agent
received another agent's facts before choosing its edit and changed its
approach, with no human relaying anything. Both directions were seen,
Codex → Claude Code (step 3) and Claude Code → Codex (steps 4 and 5).

## A defect this found

Before this run, LangSpan never completed a real Codex edit. Real Codex wraps
the `apply_patch` result as `Exit code: 0`, `Wall time: …`, `Output:` and then
the success line; the adapter accepted only the bare success line, so every
real post-edit event was "unknown" and its admission stayed open until it
expired. The source-derived fixture did not have this shape. Fixed in
`src/codex.rs` (`patch_result`), with the captured events as fixtures.

The capture was made with a new opt-in setting, `LANGSPAN_CAPTURE_DIR`: when
it names a directory, the hook saves each event it receives there, owner-only.
It is off unless set and never changes the hook's answer.

## Not yet observed

- **LangSpan's own `denied_stale` reply on a real host.** In steps 4 and 5
  Codex obeyed the briefing and made no call, so the refusal never had to fire.
  It is exercised with real files by `tests/stale_source.rs`.
- **A conflicting edit denied between the two hosts at the same moment.** Both
  hosts here ran one turn at a time; two concurrent turns were not arranged.
- Codex receiving context mid-turn: not sent to Codex yet.
- Two machines.
