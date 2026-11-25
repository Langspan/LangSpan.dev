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
| B26 | `check_collisions`: an agent-called check before a write, per symbol, with the kind of change, author and freshness (added by the second review) | `langspan_admit` and `langspan_status` say whether a path is held; briefings carry peers' recent changes | Not now. On the four hooked hosts the hook already checks every write; an agent-called check is for hosts without hooks ([ADR 0018](0018-other-hosts-after-launch.md)), like B21 |
| B27 | `status --mine`: this session's uncommitted written lines (added by the second review) | Built (issue #40): `langspan status --mine` and MCP `langspan_my_changes` list this session's files and hunks with the attribution of `commit --mine`, read-only ([ADR 0026](0026-commit-guard-and-commit-mine.md), section 3) | Built after launch as the first candidate; a real-host run is pending |
| F2 | Flaky-test learning shared with the team | None | Not now. Needs L6 results to learn from |
| F3 | Dependents' tests run after an interface change, and the change graded | None | Not now. Follows L5 and L6 |
| F5 (rebase only) | Rebase onto the target when landing | Merge ([ADR 0007](0007-check-evidence-integration-and-landing.md)) | Not now. The queue, hunk-level conflicts and rename adaptation are L7 |
| F5 (identifier renames) | Rewrite a landing's uses of an identifier another landing renamed | File renames that landed are followed and reported (L7, `src/landing.rs`) | Not now. Rewriting code the session did not write changes its meaning, and the source index is not complete enough to find every use; the landing reports the conflict instead |
| F8 | Trunk mode, `branches`, `promote` to one pull request | None | Not now. LangSpan does not open pull requests |

## Consequences

- At launch, a shell script's writes are found after it runs, not decided
  before. The README's shell-command row keeps saying so.
- No Git hook, timer or pull request is added to the owner's repository by
  LangSpan.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: F2 and F3 follow once L5 and L6 have results to work from; B8, rebase and identifier renames follow a real-host run of L4 and L7 that shows the gap; B5, B21, B25, B26 and F8 follow a user need that the launch behaviour does not meet. B27 is built; its Claude Code run before `commit --mine` is still to be retained.
