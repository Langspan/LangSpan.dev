# ADR 0035: The semantic layer: deterministic awareness of how changes relate

**Status:** Accepted. Decided by the owner, 25 November 2026. Places the
derived source index ([ADR 0008](0008-derived-source-index.md)) and its
roadmap rows in the architecture.

## Context

Git and worktrees keep sessions' files and branches apart, but two clean
changes can still break each other. One session renames `parse_config` to
`load_config`, another adds a caller of `parse_config` in a different file;
each branch passes its tests and the merge does not compile
([issue #172](https://github.com/Langspan/LangSpan.dev/issues/172)). Catching
that before the second session writes the call needs knowledge of what the
changes mean for each other, not only which files they touch.

`langspan-graph` holds part of that knowledge: it parses Rust, Python,
TypeScript/JavaScript, Go and Java with Tree-sitter and extracts definitions,
signatures, mentions, imports and call sites with argument counts, matched by
name and not resolved. Interface notices (ADR 0008) and write-time break
alerts ([ADR 0029](0029-break-alerts-at-write-time.md)) use it. More of this
work waits behind [ADR 0017](0017-large-coordination-features-wait-for-measurement.md)
and [ADR 0020](0020-source-graph-depth-after-launch.md). This ADR records the
role it plays and the rules it follows.

## Decision

**Four layers.** Each layer has one job:

| Layer | Job | Built today |
| --- | --- | --- |
| Git and worktrees | Isolate files and branches. | Git itself; linked-worktree registration ([ADR 0003](0003-native-coordination-observation.md)) |
| Semantic awareness | Relate changes, interfaces, dependencies and work in progress to each other. | The source index, interface notices, break alerts |
| Coordination | Deliver relevant context at decision points, enforce supported edit boundaries and track ownership. | Briefings, holds, stale-source refusal, line ownership, `commit --mine` |
| Verification | Check that the combined changes work. | Checks bound to exact trees and landing ([ADR 0007](0007-check-evidence-integration-and-landing.md)) |

The semantic layer makes coordination relevant: it works out which peer work
matters to which session, and why. It does not deliver, hold or refuse
anything itself.

**Deterministic first.** The semantic layer is built from syntax trees,
symbol relationships and dependency graphs. It adds a language server only
where a measured gap shows the index insufficient (see consequences).

**AI assistance only where it adds value.** Two uses qualify: relating one
session's task intent to another's work, and explaining what a change implies
for a receiving session. Assisted output is advisory, labeled as such, capped
in length and never an input to enforcement. LangSpan makes no model call
today. A model call that LangSpan makes itself needs its own ADR, which names
the network call or the local model, its timeout and its failure mode.

**Enforcement only from deterministic rules.** LangSpan refuses or holds work
only on an established rule with a reproducible answer, such as:

- a stale source version ([ADR 0005](0005-source-versions-and-stale-writes.md));
- a held region ([ADR 0012](0012-region-admission.md));
- a hunk `commit --mine` cannot attribute ([ADR 0026](0026-commit-guard-and-commit-mine.md));
- a failed check of the combined tree (ADR 0007).

[ADR 0022](0022-a-conflicted-file-is-refused-once.md) and
[ADR 0028](0028-line-ownership.md) add rules of the same kind. A new refusal
needs such a rule and its own ADR. A model's
judgment, and a name match from the index, inform; they never refuse.

**No planning or reasoning engine.** The agents own execution: they choose
tasks, plan and write code. LangSpan owns coordination. LangSpan does not
assign, sequence or run agent work, and it does not host an agent runtime.

**Grow from demonstrated failures.** Each semantic capability names the
coordination failure it addresses and the test or benchmark scenario that
reproduces it. The first is issue #172's rename-plus-new-caller task family,
served by three rows:

- [B9 (#28)](https://github.com/Langspan/LangSpan.dev/issues/28): a write
  whose calls no longer fit a peer's new argument count is told at write
  time. Built for Claude Code and Codex.
- [B10 (#29)](https://github.com/Langspan/LangSpan.dev/issues/29): a write
  calling a renamed or removed function is told of the rename or removal.
- [C7 (#44)](https://github.com/Langspan/LangSpan.dev/issues/44): a session
  in another worktree learns of in-flight signatures before they are
  committed. The benchmark gives each session its own worktree, and notices
  stay within one physical workspace today, so the benchmark's LangSpan
  condition sees the rename only through a cross-worktree row. C7 as written
  covers added and changed signatures; it must also carry removals and
  renames to serve this family.

[B26 (#39)](https://github.com/Langspan/LangSpan.dev/issues/39) serves the
same family on a host without hooks: the agent asks before it writes
(`langspan_check_write`). The answer follows the rules above. It reports a
signature change or removal only while the index still shows it, and a body
change only from lines the line journal attributes, in a file indexed at its
current content. Every symbol carries `confidence: "name_match"`. The answer
is bounded, counts what it left out and refuses nothing.

**The semantic index never leaves the machine.** Cross-machine publications
([ADR 0006](0006-remote-awareness-through-git-refs.md)) carry activity
metadata only: the participant's ID and chosen name, and per session the host
kind, a short session ID, the branch and paths, plus the task line when the
owner opts in. They never carry source, signatures, symbols, digests or
any other part of the structural map. Linked worktrees on one machine may
share the index's answers through the local daemon, as C7 proposes.

## Consequences

**Ruled in**, each still waiting on its own condition in its issue and ADR:

- B10 renames and removals at write time
  ([#29](https://github.com/Langspan/LangSpan.dev/issues/29)) and D7 rename
  tracking ([#56](https://github.com/Langspan/LangSpan.dev/issues/56)).
- C7 in-flight signatures across linked worktrees
  ([#44](https://github.com/Langspan/LangSpan.dev/issues/44)).
- D3 typed edges, paths and a repository map
  ([#52](https://github.com/Langspan/LangSpan.dev/issues/52)).
- D6 more languages, one grammar at a time
  ([#55](https://github.com/Langspan/LangSpan.dev/issues/55)).

**Ruled out:**

- A model deciding to refuse, hold or rewrite a write, or to block a commit or
  a landing.
- An agent runtime, a task planner or a scheduler that directs agents. B17
  and B18 stay held by ADR 0017; if built, they report collisions predicted
  by deterministic rules and never assign or order work.
- Sending the structural map, signatures or source to another machine through
  LangSpan. A teammate on another machine learns of an interface change only
  when the source reaches their checkout through Git, and a check of the
  combined tree catches what remains. Changing this needs an ADR that
  supersedes this one and reviews the threat model.

**When to add a language server.** Only when a retained run or benchmark
record shows the index measurably insufficient for a named failure: for
example, missed or false notices that resolving the name would fix, counted
against the index's own answers on the same scenario. Then it is added for
that language only, behind a bound and a timeout, with a fallback to the
index when it fails. A language server is a child process that may run code
the repository configures, so that ADR also updates the
[threat model](../security/threat-model.md).

No semantic capability is claimed to reduce repair until the benchmark of
issue #172 shows it, with sample sizes.
