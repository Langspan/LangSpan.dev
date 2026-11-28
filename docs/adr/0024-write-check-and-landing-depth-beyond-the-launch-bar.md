# ADR 0024: Write, check and branch workflow beyond the launch bar

**Status:** Not now. Decided 25 November 2026.

## Context

The launch bar ([roadmap](../roadmap.md)) takes these write
and landing gaps: line protection without a recorded read (L1), the commit
guard and commit of only this session's hunks (L4), checks after write
batches (L6), and landing depth: a queue, hunk-level conflicts and adapting
to renames that landed (L7). The capability review of 25 November 2026 lists
further Missing and Partial rows in the same area.

## Decision

| Row | Capability | LangSpan today | Decision and reason |
| --- | --- | --- | --- |
| B5 | A write set for scripts, loops and variables before they run | Literal forms decided before; others compared after ([ADR 0011](0011-shell-writes.md)) | Not now. A command's writes cannot be known before it runs in general; L1's line journal narrows the after-the-fact case. A heredoc that rewrites a named file (`cat > path <<'EOF'`) is not in this row: its target is literal, and it is a planned fix in ADR 0011 |
| B8 | A Git pre-commit hook against in-flight collisions | None | Not now. L4 guards agents' commits at the host's shell hook. A Git hook would also cover commits outside agents, but it is installed into the owner's repository; reconsider after L4 is observed |
| B21 | Compare-and-swap on expected file hashes over MCP | Leases from `langspan_admit` and `langspan_complete` | Not now. MCP clients without hooks are rules-file hosts ([ADR 0018](0018-other-hosts-after-launch.md)) |
| B25 | Opt-in continuous fast-forward of the checkout | None | Not now. It changes the owner's working tree on a timer |
| B26 | `check_collisions`: an agent-called check before a write, per symbol, with the kind of change, author and freshness (added by the second review) | `langspan_admit` and `langspan_status` say whether a path is held; briefings carry peers' recent changes | Built on 26 November 2026 (issue #39), when the owner took it up for hosts without hooks ([ADR 0018](0018-other-hosts-after-launch.md)): `langspan check-write` and MCP `langspan_check_write` report per path the holder, other sessions' recent journaled lines and whether it is stale, and per symbol a peer's recent signature change, removal or body lines with session, age and intents, matched by name ([ADR 0042](0042-semantic-layer.md)). Read-only: it records and refuses nothing. On the hooked hosts the hook still checks every write. A hookless host run is pending |
| B27 | `status --mine`: this session's uncommitted written lines (added by the second review) | Built (issue #40): `langspan status --mine` and MCP `langspan_my_changes` list this session's files and hunks with the attribution of `commit --mine`, read-only ([ADR 0026](0026-commit-guard-and-commit-mine.md), section 3) | Built after launch as the first candidate; a real-host run is pending |
| F2 | Flaky-test learning shared with the team | None | Built on 25 November 2026, local to each machine: tests that flip on unchanged code are learned from reruns of quick failing automatic checks and from repeated `check run`s, and a failure of one is labelled in the briefing ([ADR 0030](0030-automatic-checks-after-write-batches.md)). Sharing the list with teammates is not built: it would widen the remote payload of ADR 0006. No real-host run yet |
| F3 | Dependents' tests run after an interface change, and the change graded | None | Built on 25 November 2026 (issue #71): an automatic check can carry the owner's wider command, run whole on the same tree after the regular one when a session's writes changed an indexed export that other files mention, graded `kept` or `broke` and told to the sessions on the mentioning files ([ADR 0030](0030-automatic-checks-after-write-batches.md)). Tests are not chosen from the index. No real-host run yet |
| F5 (rebase only) | Rebase onto the target when landing | Merge by default ([ADR 0007](0007-check-evidence-integration-and-landing.md)) | Built on 26 November 2026 (issue #72), ahead of the condition below, when the owner took it up: opt-in `land --rebase` and MCP `rebase` replay the branch's commits in the object database, check only the final tree, list each rewritten commit, stop at a conflicting intermediate commit, and push only a fast-forward. Merge stays the default. No real-host run yet |
| F5 (identifier renames) | Rewrite a landing's uses of an identifier another landing renamed | File renames that landed are followed and reported (L7, `src/landing.rs`) | Built on 26 November 2026 (issue #72): renames the target made in one file with the same body are reported with the branch's uses (`identifier_renames`); opt-in `--adapt-renames` rewrites those uses in one new commit on top of the branch only when ADR 0007's certainty rules hold, and lands it only on its own passing check. The reason given before still limits it: names are matched, not resolved, so the check of the adapted tree is required. No real-host run yet |
| F8 | Trunk mode, `branches`, `promote` to one pull request | None | Not now. LangSpan does not open pull requests |

## Consequences

- At launch, a shell script's writes are found after it runs, not decided
  before. The README's shell-command row keeps saying so.
- No Git hook, timer or pull request is added to the owner's repository by
  LangSpan.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: F2, F3 and F5 are built (F5's real-host run with Claude Code and Codex calling the landing tools is still to be retained); B8 follows a real-host run of L4 that shows the gap; B5, B21, B25 and F8 follow a user need that the launch behaviour does not meet. B26 and B27 are built; B26's run on a host without hooks, and B27's Claude Code run before `commit --mine`, are still to be retained.
