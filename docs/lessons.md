# Lessons

Patterns from the owner's corrections, recorded so they don't repeat.

## 24 November 2026

- **Check a new identifier format against the validator that will receive it.** The build contract's first lifecycle IDs used `/` separators, which `validate_request_id` rejects. Specify the logical form separately from the encoded wire form, and pin the encoding with golden vectors.
- **Never generate a retry identity the host didn't supply.** An ID made up on each delivery cannot connect admission, retry and completion. Treat the call as unsupported until the host provides a stable identity.
- **Keep retried commands byte-identical.** Fields that change on every attempt, such as timestamps, belong to the owner that records them, not to the request. Otherwise the same request ID becomes different command content.
- **Scope a number before calling it wrong.** A count that doesn't match the full suite may be a real subset ("21 tests" was the core behaviour suite plus the architecture test). Say what it measures instead of calling it fabricated.
- **Verify vendor behaviour against primary documentation, then against the pinned host.** Claude Code's `onFailure` accepts `continue` or `block`, not `allow`. PreToolUse `additionalContext` arrives with the tool result, not before the call. Both corrected earlier drafts.

- **Lead with the finished product in public-facing copy.** The owner wants the README and marketing page to explain what LangSpan unlocks, rather than present another planning memo. Keep runnable commands accurate and put detailed current evidence and availability in implementation status.
- **Use the supplied identity consistently.** The owner supplied the LangSpan SVG kit and exact palette; use those assets across the code repository and marketing site.
- **Compare the product outcome, not just the plumbing.** A working hook and a durable event store do not prove that agents receive useful context before choosing edits. Keep that ordering in the two-developer acceptance test and measure the resulting integrated work.

- Check public copy against the product before revising it. Keep one clear headline; use the surrounding sentences to name the agent, file and action instead of repeating slogan fragments. Recheck product facts against the implementation as it changes.

- Keep the mission tied to the team's ability to build related work together. The owner's examples include teammates stepping aside because a swarm moves faster than they can coordinate, and solo worktrees meeting conflicts at integration. Treat those experiences as motivation for a measurable hypothesis, not proof of reduced rework. Keep “As a / I want / so that / we know this is true when” in internal product notes; public copy should tell the problem and the team story.

- Check the shipped vector against the owner's reference image. The earlier LangSpan SVG changed the center ribbon, crossings and corners, and substituted a font for the wordmark. After flagging the mismatch, the owner supplied an updated SVG kit with filled ribbon outlines, outlined lettering and a revised palette. Those supplied files are the source of truth across the READMEs, site, social preview and organization avatar.

## 28 November 2026

- **Allowlist what a safety probe passes through; never denylist it.** The push-routing dry run passed the agent's push words to Git and refused bad options one at a time. Each review round found another (`--no-dry-run`, `--no-porcelain`, an abbreviation, a trailing `--repo` that took the probe's own `--dry-run` as its value), and each one turned the probe into a real push. When untrusted words reach a command that must stay harmless, accept only a short list of known-safe forms, refuse the rest by name, and put the probe's own options where nothing can consume or undo them.
