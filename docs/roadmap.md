# Roadmap

Updated 25 November 2026. This is an index of what LangSpan still has to
build. Each capability is a GitHub issue labelled `roadmap`, and the issue is
its specification: the problem it solves, what exists today, the behaviour to
build, the acceptance criteria that make it done, an example, the outcome and
the limits, and the decision that governs it (a launch track and the ADR,
which says what is planned, what waits and what would change the decision).
The full list is the [roadmap issues](https://github.com/Langspan/LangSpan.dev/issues?q=label%3Aroadmap).
Pick work from there.

Items carry an ID, grouped by area (A to J below); each issue carries the
matching `area:` label. The ADRs refer to capabilities by these IDs. A pull
request that finishes a capability says "Fixes #N" for its issue and removes
its line here in the same change; one that changes a decision updates the
issue and the ADR. A capability that is complete has no line here; its
behaviour is in the [usage guide](usage.md) and its evidence in
[implementation status](IMPLEMENTATION_STATUS.md). Names marked "proposed" in
an issue do not exist yet; an implementation may choose better ones and
updates the issue.

A capability counts as done when it has an implementation, a repeatable
acceptance case and retained results. Native-agent scenarios are completed as
native runs; source fixtures and hand-issued CLI calls prove narrower
properties. Team scenarios are completed across two developers and machines,
including unrelated work and failed runs. Performance claims need
measurements taken the same way for every arm: correct integrated
completion, elapsed time, human intervention, unnecessary denials,
stale-write misses and coverage, alongside hook latency. Every run is kept,
including failures and timeouts.

**Decision** in each issue names the launch track or the ADR that governs it:
*planned* (decided, waiting for someone to build it), *not now* (the ADR names
what would bring it forward), or *not building*.

## Launch bar

The launch hosts are Claude Code, Codex, Gemini CLI and Cursor. Every track
below is built and tested on `main`; what each still needs before it counts
as done is in its row.

| Track | Rows | Status, 25 November 2026 |
| --- | --- | --- |
| L1. Line protection without a recorded read: a per-session journal of written lines; a write that changes another live session's recent lines is refused unless that session read them since | B4 | Built ([ADR 0028](adr/0028-line-ownership.md)). Shell writes, `NotebookEdit` and Codex add, delete and move are not located. Not yet observed on a host |
| L2. Codex reads, Stop and SessionEnd | A3 | Built: Stop and SessionEnd registered; Codex reports no reads, so a first patch records a `snapshot` version instead ([ADR 0027](adr/0027-turn-and-batch-events.md)). Not yet observed on Codex |
| L3. Claude Code Stop, StopFailure, SubagentStop and PostToolBatch hooks | A2 | Done: observed on Claude Code 2.1.330 ([ADR 0027](adr/0027-turn-and-batch-events.md)). Quota and usage detection from StopFailure is J11 |
| L4. Shared-checkout commit guard and commit of only this session's hunks | B6, B7 | Built ([ADR 0026](adr/0026-commit-guard-and-commit-mine.md)); observed with real Claude Code sessions. Not yet observed with Codex |
| L5. Interface-break alerts at write time, with the lines to fix and the argument count | B9, B10 | Built for Claude Code and Codex writes ([ADR 0029](adr/0029-break-alerts-at-write-time.md)); observed once on Claude Code. Not yet observed on Codex; Gemini CLI and Cursor do not receive it yet |
| L6. Checks after write batches, run detached and reported in the next briefing | F1 | Built, off by default ([ADR 0030](adr/0030-automatic-checks-after-write-batches.md)). Not yet observed on a host |
| L7. Landing depth: a bounded queue for the push turn, hunk-level conflict reports, and following renames that landed | F5 | Built ([ADR 0007](adr/0007-check-evidence-integration-and-landing.md)). Not yet called by an agent on a host |
| L8. Release pipeline: versioned releases with prebuilt binaries for the supported platforms | I6 | Built ([ADR 0031](adr/0031-releases.md)); no release has been produced yet |
| L9. Cursor adapter: hooks and MCP registration | A4 | Built from documentation ([ADR 0032](adr/0032-cursor-adapter.md)); waits for a capture from a real Cursor (`scripts/capture-cursor.sh`) |
| L10. Gemini CLI adapter: hooks and MCP registration | A6 | Built from documentation ([ADR 0033](adr/0033-gemini-cli-adapter.md)); no live Gemini CLI session yet |

Windows, `npx` and plugin marketplaces are not in L8 ([ADR 0025](adr/0025-distribution-lifecycle-and-store-beyond-the-first-release.md)).

## A. Hosts and hook events

- [A3: Verify Codex end-of-reply events and the first-patch snapshot on a real Codex](https://github.com/Langspan/LangSpan.dev/issues/11): admission release at end of reply and the first-patch snapshot are built but no real Codex has run them.
- [A4: Verify the Cursor adapter on a real Cursor](https://github.com/Langspan/LangSpan.dev/issues/12): the Cursor adapter rests on documentation alone, so Cursor cannot yet be relied on to be coordinated.
- [A5: A plugin adapter for the Hermes agent](https://github.com/Langspan/LangSpan.dev/issues/13): a Hermes agent beside other hosts gets no admission or briefing, and its edits go unrecorded.
- [A6: Gemini CLI verified on a real session, and guidance for rules-file hosts](https://github.com/Langspan/LangSpan.dev/issues/14): the Gemini CLI adapter has never run live, and editors without hooks get nothing at all.
- [A11: Outline first for whole reads of large files](https://github.com/Langspan/LangSpan.dev/issues/16): whole reads of large files spend context to find one function; nothing offers the file's shape first.
- [A12: Index answers next to a search result](https://github.com/Langspan/LangSpan.dev/issues/17): a search result does not say where a name is defined, what depends on it or whether a peer is changing it.
- [A13: Show where peers are looking](https://github.com/Langspan/LangSpan.dev/issues/18): a session that has read and planned but not written yet is missing from peers' briefings. Built as presence; a real-host run remains.
- [A14: Brief a session when it first reaches another repository](https://github.com/Langspan/LangSpan.dev/issues/19): a session that moves into a second repository is never briefed about the peers working there.
- [A15: Deliver peers' changes during a Codex turn](https://github.com/Langspan/LangSpan.dev/issues/20): a long Codex turn hears about peers' changes only at its next prompt, after building on old state.
- [A16: Push a message into an open session at once](https://github.com/Langspan/LangSpan.dev/issues/21): an urgent message waits for the receiving session's next hook, which may come after the harm.
- [A17: Start an agent for a message nobody has taken](https://github.com/Langspan/LangSpan.dev/issues/22): a handoff left for an ended session can wait for hours with nobody noticing.

## B. Write coordination

- [B4: Protect another session's recent lines from an unread write](https://github.com/Langspan/LangSpan.dev/issues/23): a session that never read a file can replace lines a peer wrote seconds ago, silently.
- [B5: Know a shell command's writes before it runs](https://github.com/Langspan/LangSpan.dev/issues/24): a heredoc rewrite or script can overwrite a peer's newer change with no refusal.
- [B6: Hold a broad commit that would take another session's changes](https://github.com/Langspan/LangSpan.dev/issues/25): `git commit -a` in a shared checkout takes another live session's half-finished edits.
- [B7: Commit only this session's hunks](https://github.com/Langspan/LangSpan.dev/issues/26): no ordinary Git command commits one session's hunks and leaves another's untouched.
- [B8: Guard commits made outside agent shells](https://github.com/Langspan/LangSpan.dev/issues/27): commits from a terminal, IDE or script bypass the agent-shell commit guard.
- [B9: Alert a write whose calls no longer fit a peer's new signature](https://github.com/Langspan/LangSpan.dev/issues/28): a session keeps writing calls in a function's old shape after a peer changed its parameters.
- [B10: Tell a write about renames and same-count signature changes](https://github.com/Langspan/LangSpan.dev/issues/29): a peer's rename or same-count parameter swap is not reported at the write that uses it.
- [B11: Hold a write that reverses a recorded decision](https://github.com/Langspan/LangSpan.dev/issues/30): a later session that missed a team decision can write it back out.
- [B12: A note that fires into the next editor's tool result](https://github.com/Langspan/LangSpan.dev/issues/31): a note about a file arrives at the next prompt, possibly after the wrong edit.
- [B16: Claim the next number in a sequence](https://github.com/Langspan/LangSpan.dev/issues/32): two sessions take the same ADR or migration number and the clash shows only at landing.
- [B17: Typed intents that predict conflicts](https://github.com/Langspan/LangSpan.dev/issues/33): "working on a file" does not reveal a rename colliding with new calls elsewhere.
- [B18: Split planned work into independent groups](https://github.com/Langspan/LangSpan.dev/issues/34): a person splitting a task cannot see which units share files or depend on each other.
- [B19: A queue of collided work to reconcile](https://github.com/Langspan/LangSpan.dev/issues/35): refusals and landing conflicts scroll past with no single list of what needs reconciling.
- [B20: Typed refactor operations and recipes](https://github.com/Langspan/LangSpan.dev/issues/36): a hand-made rename across many files misses call sites and collides with peers.
- [B21: Expected-version checks over MCP](https://github.com/Langspan/LangSpan.dev/issues/37): `langspan_admit` on a host without hooks does not check the file is the version the agent read.
- [B25: Opt-in fast-forward of an idle checkout](https://github.com/Langspan/LangSpan.dev/issues/38): a shared checkout falls behind its target branch and every session works on an old base.
- [B26: Ask before a write what changed in its symbols](https://github.com/Langspan/LangSpan.dev/issues/39): an agent without hooks cannot ask whether the functions it will change were just changed.
- [B27: Show this session's uncommitted lines](https://github.com/Langspan/LangSpan.dev/issues/40): in a shared checkout a session cannot tell which uncommitted lines are its own.

## C. Briefings and context

- [C2: Rank the code briefing and name each definition's dependents](https://github.com/Langspan/LangSpan.dev/issues/41): code briefings list definitions in path order and do not mark the files that will break.
- [C3: Tell a new session what changed recently in the repository](https://github.com/Langspan/LangSpan.dev/issues/42): a new session hears only about active peers, not what changed in the last hours.
- [C5: Learn which briefed files agents use](https://github.com/Langspan/LangSpan.dev/issues/43): briefing relevance never adjusts to what agents in a repository actually use.
- [C7: Tell dependents about another worktree's new or changed signatures before they are pushed](https://github.com/Langspan/LangSpan.dev/issues/44): a session cannot see a function another worktree just added, so it duplicates or stalls.
- [C8: Tell two sessions their tasks point at the same code before either writes](https://github.com/Langspan/LangSpan.dev/issues/45): two tasks aimed at the same function are not flagged before either session edits. Built as prompt targets; a real-host run remains.
- [C9: Leave a trail and a handoff when a session ends](https://github.com/Langspan/LangSpan.dev/issues/46): what a finished session learned is lost unless someone ran `handoff` by hand.
- [C10: Brief the owner's next session on what happened since the last one](https://github.com/Langspan/LangSpan.dev/issues/47): overnight landings, failed checks and unread messages are scattered across several commands.
- [C11: Offer a finished, checked task as a worked example](https://github.com/Langspan/LangSpan.dev/issues/48): agents rediscover the same files and helpers for a recurring kind of task.
- [C12: Advise on worktrees and tidy finished ones on request](https://github.com/Langspan/LangSpan.dev/issues/49): linked worktrees pile up and the owner cannot tell which are safe to remove.

## D. Source graph and queries

- [D1: Look up a symbol with its calls, callers, digest and notes](https://github.com/Langspan/LangSpan.dev/issues/50): a symbol lookup gives its definition but not its calls, callers, changes or notes. Built with `--detail`; a real-host run remains.
- [D2: Show a change's blast radius level by level](https://github.com/Langspan/LangSpan.dev/issues/51): `graph impact` stops at direct dependents, hiding breaks two steps away. Built with `--depth`; a real-host run remains.
- [D3: Typed edges, paths between symbols and a repository map](https://github.com/Langspan/LangSpan.dev/issues/52): name mentions alone cannot follow a call chain or show how modules relate.
- [D4: Show the history of a definition](https://github.com/Langspan/LangSpan.dev/issues/53): an agent cannot see who changed a definition recently, why, or who reverted the same change.
- [D5: Export and import the source index](https://github.com/Langspan/LangSpan.dev/issues/54): a large repository's first indexing takes many passes, and the index cannot be inspected outside LangSpan.
- [D6: Index more languages, one grammar at a time](https://github.com/Langspan/LangSpan.dev/issues/55): Java, C#, Ruby and C++ repositories get no source-index answers or interface notices.
- [D7: Track renames so history and notices follow the new name](https://github.com/Langspan/LangSpan.dev/issues/56): a rename is reported as a removal with no new name, and history stays with the old one.
- [D8: Report where the index is blind](https://github.com/Langspan/LangSpan.dev/issues/57): an empty impact answer does not say which files the index could not read. Built as `graph blind-spots` and `coverage.blind_in_scope`; a real-host run remains.
- [D9: Check a combination at the level of definitions](https://github.com/Langspan/LangSpan.dev/issues/58): a clean Git merge can pair a changed signature with a new call in the old shape.
- [D10: Offer live activity as an MCP resource](https://github.com/Langspan/LangSpan.dev/issues/59): a live view of who is doing what takes three tool calls, and there is no resource to read.

## E. Memory and messages

- [E1: Notes anchored to a definition, with staleness and tags](https://github.com/Langspan/LangSpan.dev/issues/60): notes attach only to files, keep being told after their code changes, and cannot be grouped.
- [E2: Search remembered notes](https://github.com/Langspan/LangSpan.dev/issues/61): a note about another file stays invisible to an agent that needs its decision. Built as `langspan recall` and `langspan_recall`; a real-host run remains.
- [E3: Token, model and cost lines in the recap](https://github.com/Langspan/LangSpan.dev/issues/62): the owner cannot see which session used most tokens or which model it ran.
- [E4: Messages addressed to a file or a definition](https://github.com/Langspan/LangSpan.dev/issues/63): a message can go only to everyone or to one session ID, not to whoever uses some code.
- [E7: Sender recorded from the running session](https://github.com/Langspan/LangSpan.dev/issues/64): a shell-sent message needs `--from`, and a wrong or missing value misattributes it.
- [E9: Messages to sessions on other machines](https://github.com/Langspan/LangSpan.dev/issues/65): agents on two machines cannot message each other, so their owners relay by hand.
- [E12: Ask who received a message](https://github.com/Langspan/LangSpan.dev/issues/67): a sender cannot tell whether its message has reached the other session yet.
- [E13: Show a shortened message in full](https://github.com/Langspan/LangSpan.dev/issues/68): a message cut short in a briefing cannot be read in full.

## F. Checks and landing

- [F1: Say which tests a write broke](https://github.com/Langspan/LangSpan.dev/issues/69): a failed automatic check does not say whether this write, an earlier state or a peer broke the test.
- [F3: Run the wider check when an interface changes](https://github.com/Langspan/LangSpan.dev/issues/71): the fast check misses breaks in packages that call a changed exported function.
- [F5: Rebase landings and adapt identifier renames](https://github.com/Langspan/LangSpan.dev/issues/72): a landing refused over a rename leaves every use to fix by hand, and there is no linear-history landing.
- [F8: An integration branch every session lands on](https://github.com/Langspan/LangSpan.dev/issues/73): each agent's own branch leaves the person merging six branches by hand to see the work together.
- [F9: A LangSpan mark in pull requests](https://github.com/Langspan/LangSpan.dev/issues/74): a reviewer might want to know a pull request's tree was checked through LangSpan.

## H. The person's view

- [H5: A monthly refusal count](https://github.com/Langspan/LangSpan.dev/issues/79): a person deciding whether to keep LangSpan has no count of how often it intervened.
- [H6: A daily "work saved" line, once measured](https://github.com/Langspan/LangSpan.dev/issues/80): people want a savings figure, which needs measurement rather than assumptions.
- [H8: A coordination report per session and host](https://github.com/Langspan/LangSpan.dev/issues/81): a person cannot tell whether a new host version or model still follows coordination well.
- [H9: What an agent did after a refusal](https://github.com/Langspan/LangSpan.dev/issues/82): nobody can tell whether a refused agent reread, waited, retried blindly or gave up.
- [H10: Local counts of Git trouble](https://github.com/Langspan/LangSpan.dev/issues/83): there is no local baseline of merge conflicts, rejected pushes and reverts.

## I. Install and lifecycle

- [I5: Self-update with rollback](https://github.com/Langspan/LangSpan.dev/issues/84): a hand-installed release can fall behind and run an old daemon against newer hooks.
- [I6: More ways to install, and Windows](https://github.com/Langspan/LangSpan.dev/issues/85): a source build needs Rust and a C compiler, and Windows is not supported.
- [I7: A LangSpan block for AGENTS.md and rules files](https://github.com/Langspan/LangSpan.dev/issues/86): an agent without trusted hooks does not know LangSpan coordinates the repository.
- [I8: A LangSpan skill that teaches agents how to work with it](https://github.com/Langspan/LangSpan.dev/issues/87): part 1, the general skill for every host, is built ([ADR 0034](adr/0034-langspan-skill.md), [usage](usage.md#the-langspan-skill)) and still needs a real-host run; part 2, task skills for landing, recovery and shared commits, is planned.
- [I10: Setup through MCP tools](https://github.com/Langspan/LangSpan.dev/issues/88): an agent asked to set up LangSpan has no MCP tool for it.
- [I11: A shorter MCP tool list on hooked hosts](https://github.com/Langspan/LangSpan.dev/issues/89): every session carries all tool descriptions, including bookkeeping tools the hooks already call.

## J. Privacy, audit, configuration and robustness

- [J7: Retention limits for the event log](https://github.com/Langspan/LangSpan.dev/issues/90): the event log grows without bound, slowing start and keeping old data on disk.
- [J9: A hash-chained event log with replay](https://github.com/Langspan/LangSpan.dev/issues/91): a person cannot show what LangSpan told an agent at a moment, or that the record is unchanged.
- [J10: Token accounting from host transcripts](https://github.com/Langspan/LangSpan.dev/issues/92): token spending per session cannot be seen or related to refusals and rework.
- [J11: Telling peers that a session ran out of quota](https://github.com/Langspan/LangSpan.dev/issues/93): a session stopped by its usage limit still looks active to peers and the person.

## Not building: hosted-service features

LangSpan is local and has no server ([ADR 0016](adr/0016-hosted-service-features-out-of-scope.md)). These capabilities need one,
so they are not built: E8 (people messaging agents from a hosted dashboard),
F10 (a GitHub App's checks and comments), G2 (accounts, workspaces, roles,
seats), G3 (channels, uploads, email, a REST API, OAuth, analytics), G4
(prompts matched to code by meaning), G5 (real-time leases across machines),
H7 (streaks, badges, leaderboards), H11 and H12 (browser onboarding and a
hosted dashboard's editor detection), and I9 (login, upgrades, sharing a
repository through a service).

## Design choices that stay

LangSpan stays a headless CLI and MCP tool with an optional local page, with
its own implementation. Native permissions stay authoritative. Related remote
work is advisory. Publication and landing remain explicit actions, and stored
facts carry their uncertainty.
