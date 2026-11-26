# ADR 0007: Check evidence, combined checks and landing

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026; push turn, retries and MCP tools added the same day; the
landing queue, conflict hunks, followed renames and the merge-versus-rebase and
attempt-count decisions added 24 November 2026; identifier renames, opt-in
rename adaptation and opt-in rebase landings (roadmap F5) added 26 November
2026. Independent of the admission guarantee in
[ADR 0001](0001-local-coordination.md).

## Context

"The tests passed" is a claim about some version of the code. Without the
version it is not evidence about any other. Two branches that each pass can
fail together, and a branch that passed yesterday can be landed onto a target
that moved this morning.

## Decision

**A check record names its tree.** `langspan check run WORKSPACE --name NAME
-- COMMAND...` records the command, its exit status, its duration and the Git
tree of everything the command could read. The command line is recorded with
the same secret redaction as check output, so a token passed on it is not
stored; the check's name and program still identify it, and a record stored
before this redaction is redacted when it is shown. Redaction never makes a
record longer than the 512-byte command limit: a line that would outgrow it
keeps its longest redacted prefix that fits, ending in `[redacted]`, so a
store written by an earlier build still opens, and a request it recorded is
still answered when retried with its request ID. For a working directory that tree
is built through a private index from every tracked and every untracked,
unignored file, so it is exact even with uncommitted changes. The tree is taken
before and after the command. If they differ, or the command did not finish,
the result is **inconclusive**: it is not about any one tree. An inconclusive
combined check says why, as one reason the CLI prints and the MCP result
carries in `inconclusive`: the cause, the files that differed from the combined
tree (at most 20 named, each added, modified or deleted and whether it is
untracked; paths redacted, as data) and the next step. The reason is part of
the result, not the record: a replay of the same request says only that.

**Applicability is a comparison, not a memory.** `langspan check status`
builds the tree as it is now and compares. A record applies only if its tree
is that tree. A pass on any other tree is shown, labelled as not applying.
Independently, when the Git observer sees a file change, working-tree records
for that workspace are marked changed-since. History is kept; nothing is
rewritten.

**Combining without a checkout.** `langspan integrate check WORKSPACE --onto
BRANCH --name NAME BRANCH... -- COMMAND...` fetches the remote's current
`BRANCH`, combines the named branches onto it in order inside the object
database (`git merge-tree`), and reports the conflict (paths and hunks,
below) instead of proceeding if they do not combine. Otherwise it checks the combined commit out
into a temporary detached worktree in the system temporary directory, runs the
command there, removes the worktree, and records the result against the
combined tree with the input commits. The owner's checkout is not involved.

**Landing needs current evidence, and the core decides.** `langspan land
WORKSPACE --onto BRANCH --name NAME BRANCH... [--apply]` fetches the target
again, recombines, and asks the core whether a passing record of that name
exists for exactly the resulting tree. If the target moved, a branch moved or
the check failed, the tree differs or the record is not a pass, and landing is
refused with the command that would produce the evidence. The core also
refuses to record a landing that does not name such a record.

**The push turn.** `land --apply` first takes the push turn for its
workspace and target branch, a lock held in the core as state like a lease:
`TakeTurn` and `ReleaseTurn` commands, `turn_taken`, `turn_renewed`,
`turn_released` and `turn_expired` events, a deadline in the daemon's
injected clock, and every turn ended when the daemon restarts. If another
landing holds it, this landing waits in line for it (below).
The turn lasts one attempt's budget: three Git steps of 120 s, two snapshots,
the check's time limit when one may run, and 60 s. It is renewed by its holder
before every attempt and before every push, so a slow check never outlives
it and a crashed landing blocks its target only until that deadline. A
landing whose turn has ended (it expired, or the daemon restarted) is refused
at the next renewal and pushes nothing more. If the daemon cannot be reached,
there is no turn and nothing is pushed. The turn is released on every return;
the release records how each attempt ended (landed, moved, refused, not
ready, check failed, not combinable, nothing to land, error, push unknown). The key is the
workspace and target, not the repository: two clones of one repository have
separate turns, and the remote's refusal of a non-fast-forward still keeps
them from overwriting each other.

**The landing queue.** A landing that finds the turn held waits for it in a
line kept in the core with the turn, keyed the same way: first come, first
served, at most `MAX_TURN_QUEUE` (8) landings per target. The landing's request
ID is its ticket. Its process asks the coordinator every 0.5 s (`TakeTurn` with
`queue`); each ask either takes the turn, which only the first in line may do
when it is free, or says where it stands. A landing that did not ask to wait
(or an older client) never takes a free turn while others are in line. It
waits at most `--wait-s` (CLI) or `wait_s` (MCP): default 240 s, at most 3600 s,
0 meaning it does not wait. Leaving the line is an event with a reason:
`timed_out` (its wait ran out; the client leaves at its own deadline, and the
coordinator's deadline, counted from when it joined, is a backstop),
`cancelled` (`land-cancel`, `langspan_land_cancel`), or `stopped` (it did not
ask for 15 s: the process died or was suspended). Taking the turn removes it
from the line. A daemon restart empties every line, as it ends every turn.
Events: `turn_queue_joined`, `turn_queue_seen` (an ask that extends its 15 s,
written at most every 7.5 s rather than on every ask), `turn_queue_left`. The
line is part of `status`, of the `TurnQueue` read (`land-queue`,
`langspan_land_queue`) and of the dashboard's push-turn section, each with
position, time waited and time left. A landing that does not get the turn (it
did not wait and the turn was held or others were in line, the line was full,
its wait ran out, or it was cancelled) is `busy` with that `reason`, the holder
and the line, exit code 7, as before the queue: nothing was fetched or pushed.
A cancelled landing that asks again is told it is no longer in line even when
the turn is free, so cancelling never lets it push. Once a landing has the turn
it fetches the target, so it combines onto whatever the landings before it
pushed, and with `--check-again` checks that new tree before pushing
(`a_second_landing_waits_its_turn_and_lands_on_the_new_target_with_its_check_run_again`).

**The push and its retries.** Without `--apply` nothing is pushed and no turn
is taken. With it, the target is fetched after the turn is held, so whatever
landed while this landing waited is combined in. The combined commit is
pushed to the remote branch, never by force; the owner's pre-push hooks run,
as for any push of their code. If the remote refuses, the target is fetched
again. If it moved, the refusal was a lost race: the branches are combined
onto the new target and the attempt is repeated, at most three attempts in
all. A new combined tree lands only with a passing check for exactly that
tree: with `land --check-again -- COMMAND...` the check is run on it inside
the turn; without it, the landing stops as not ready (exit 6) and says the
target moved. If the target had not moved, the refusal came from a hook, a
permission or a protection rule; it is reported (exit 2) and not retried.
The remote's own answer decides, never a match on its message text. After
three moves the landing gives up (exit 2). Branch history is preserved: a
branch that already contains the target is pushed as it is, and anything else
becomes an ordinary merge commit with both parents, authored with the owner's
Git identity. A merge whose tree would equal the result so far is not made:
the branch's changes are all there already, as after a rebase landing of it.
Without `--rebase` or `--adapt-renames` the branches' commits are never
rewritten.

**Merge, not rebase (decided 24 November 2026).** A lander could rebase the
agent's commits onto the moved target; LangSpan keeps merging, with no rebase
option.
The evidence is bound to a tree, and a rebase and a merge of the same work
usually give the same final tree, so a rebase would not weaken the check
itself. It would weaken everything around it. Rebasing rewrites every branch
commit, so the pushed commits are not the ones the agent's branch, the check
record's input commits or a teammate's checkout hold. Containment stops
working: a rebased branch is no longer an ancestor of the target, so a retry or
a second landing of the same branch would combine it again instead of finding
nothing to land, and a branch stacked on it would no longer fast-forward. A
rebase replays commit by commit, so it can stop at a conflict in an
intermediate commit that neither final tree has, and every replayed commit is
a tree that was never checked. A merge is one combination whose single result
is the tree that was checked, the same thing `git merge` and a hosted merge
button do. Merge stays the default. An opt-in rebase was added on 26 November
2026 (roadmap F5, below) for a team that wants linear history; it records the
rewritten commits and keeps the check on the final tree, and it answers the
containment problem with `git cherry`.

**Three attempts, not four (decided 24 November 2026).** A lander without a
queue needs more attempts, because every lap of its loop is a race with the
other agents of the same repository. Here the queue serialises every landing made through LangSpan on
the machine, so a refused push means something outside it moved the target: a
teammate on another machine, or a plain `git push`. Three attempts already
absorb two such moves inside one landing. Each further attempt can run the
whole check again (`--check-again`) while every waiting landing waits behind
it, and its time limit adds to the turn's budget. A target that moves three
times during one landing is busy enough that the owner should hear about it
(`gave_up`, exit 2) rather than have the line held longer.

**Conflict detail.** When a branch does not combine, the result names the
conflicting paths (at most 64) and, for the first 8 files, the conflicting
hunks read from the conflicted tree `git merge-tree` writes: at most 4 hunks
per file, each with the target's side (the target and the branches before it)
and the branch's side, each side's first line number in that version of the
file, its line count and its first 6 lines of at most 200 characters, control
characters replaced. Git's conflict type for each path (`contents`,
`modify/delete`, `rename/delete` and so on) comes from the messages of
`merge-tree -z`. A binary file, a file over 1 MiB, or a conflict about a file
rather than its lines is named with why no lines are shown. The text is the
repository's: data for the agent, labelled as such
(`a_conflict_reports_its_hunks_per_file_bounded_and_as_data`). Nothing is
resolved or left in a checkout; the agent resolves the conflict on its own
branch and lands again.

**Renames that already landed.** Combining always runs with
`merge.renames=true`, `merge.renameLimit=1000`, `diff.renameLimit=1000` and
`merge.conflictStyle=merge`, whatever the owner's configuration, so the
combined tree, and the evidence for it, does not depend on local settings.
When the target renamed a file after a branch started, the branch's edits to
the old name are combined into the new file, and the result's `renames` lists
each such rename (`branch`, `from`, `to`): the renames between the merge base
and the target whose old path the branch changed. Inexact renames are looked
for among at most 1000 files on each side; exact renames are always found.
This is Git's file rename detection (`a_rename_that_already_landed_is_followed_and_said`).

**Identifier renames (roadmap F5, added 26 November 2026).** For each branch
that diverged from the result so far, the combination compares every source
file the target changed between the merge base and the target (at most 64,
in the languages of the source index), parsed from Git's objects at both
versions with the source index's extractor and body digests (ADR 0008, D7).
A function or method that the extractor's `diff` reports as renamed with the
**same body** (`same_body` evidence: one exported definition went, one came
with the same signature but for the name and the same block digest, each the
other's only candidate) is a candidate. It is reported when the branch's
added lines (Git's diff from the merge base to the branch, no context) still
mention the old name as a whole word: `identifier_renames`, each with
`branch`, `from`, `to`, `path` and `uses` (path and line in the branch's own
version), at most 16 renames and 32 uses each, on `checked`, `not_ready`,
`check_failed`, `ready` and `landed` results. A `not_ready` reason says to
land again with `--adapt-renames`. A rename with the same lines but a changed
body (`same_signature`), a rename across files, and a private definition are
not candidates. When renames cannot all be looked for (more than 64 changed
source files, a file over 1 MiB or one that does not parse completely, a
branch diff over 8 MiB), none are reported and
`identifier_renames_incomplete` says why
(`a_changed_file_that_does_not_parse_makes_rename_detection_say_so`). Test:
`a_target_rename_is_reported_with_each_use_the_branch_adds`.

**Adapting a branch to them (`--adapt-renames`, opt-in).** LangSpan adds one
commit on top of the branch, written to the object database through a private
index, never checked out: on exactly each listed line, every whole-word
`from` becomes `to`. The combination then uses that commit, so the landed
tree needs its own passing check (`--check-again`, or an
`integrate check --adapt-renames` of the same tree first); the result's
`adapted` names the commit, and the landing record keeps it. The agent's
branch, checkout and index are not changed; the agent can fetch the commit.
The rewrite is made only when it is certain by these rules, and otherwise the
landing stops as `not_adapted` (exit 2) with the reason, having rewritten,
checked and pushed nothing, and each rename's `uncertain` says why:

- renames were not all looked for, or the landing's branches together use
  more than 16
  (`renames_past_the_bound_across_branches_stop_an_adapting_landing`);
- the branch adds more than 32 lines that mention the old name;
- the branch's added lines already mention the new name;
- the branch changed, moved or removed the old definition itself (its body
  digest on the branch differs from the merge base's);
- a listed line is itself a definition of the old name that the branch adds;
- the target still defines the old name in some source file (`git grep -w`,
  at most 16 files parsed), so a use could mean that one;
- a file to rewrite is not UTF-8 text, not a regular file, or over 1 MiB, or
  a listed line no longer holds the old name.

Names are matched as words, not resolved: a use of another definition with the
same name in a scope the rules cannot see would be rewritten too. The check of
the adapted tree is what stands between such a rewrite and the target. Tests:
`adapt_renames_pushes_the_new_name_at_each_use_only_on_its_own_passing_check`,
`a_rename_that_cannot_be_applied_with_certainty_stops_the_landing`,
`without_adapt_renames_the_branch_is_pushed_unchanged_or_not_at_all`.

**Rebase landings (`--rebase`, opt-in).** Each branch's commits that the
result so far lacks are replayed onto it, oldest first, entirely in the object
database: each is `git merge-tree --write-tree --merge-base=PARENT` of the
commit onto the replayed tip, committed with `git commit-tree` keeping the
commit's author, date and message (the committer is the owner's Git
identity, as for a merge; a signature is not kept). A commit whose change the
result already has (`git cherry` marks it `-`), or that changes nothing once
replayed, is left out. A replay that conflicts stops with `not_combinable`
naming that `commit` and its hunks, and nothing is pushed. A branch with merge
commits (even one that would fast-forward), a root commit, a commit encoded
other than as UTF-8, or more commits to replay than the 64 a landing records
across all its branches is not rebased (`not_rebased`, exit 2), before anything
is checked or pushed. Only the final tree
is checked; the intermediate commits are trees that were never checked, which
the result says by listing them in `rebased` (`from`, `to`) and the landing
record keeps. The push is a fast-forward of the target, never forced. A
later landing of the same branch finds every patch already on the target and
reports `nothing_to_land`, with or without `--rebase`. Without `--rebase`, a
merge that changes nothing is still made while later branches are combined,
so a branch stacked on it merges from its history and keeps its own changes
(a deletion is not undone from an older base); merges after the last change
are left out of the result, so such merges alone are `nothing_to_land`.
Tests: `a_branch_stacked_on_a_no_op_merge_still_applies_its_own_change`,
`a_no_op_merge_is_left_out_when_nothing_after_it_changes_the_result`,
`a_rebase_landing_pushes_linear_history_whose_tree_is_the_checked_tree`,
`a_rebase_that_conflicts_in_an_intermediate_commit_names_it_and_pushes_nothing`,
`a_branch_with_a_merge_commit_is_not_rebased_and_nothing_is_pushed`.

**Safety rules for both.** No force push, ever: a rewritten result reaches the
remote only as a fast-forward of the fetched target. No rewrite without its
flag. No push without a passing check of exactly the pushed tree, the same
rule as every landing. The owner's checkout, index, `HEAD`, branches and
configuration are not changed, and no ref is written for a rewritten commit;
the new commits are objects until a landing pushes them. Published history is
never rewritten: the target only moves forward, and the agent's own branch
stays where it was. The MCP tools take `rebase` and `adapt_renames`; a landing
through MCP checks the rewritten tree first with `langspan_integrate_check`
and the same values. Verified with real Git, a real daemon and local file
remotes only; no agent has called these on a host yet.

**Wire protocol version 6.** A landing's `rebased` and `adapted`, and the
request that records them, are what version 6 adds to version 5. A daemon of
an earlier build reads that request with `deny_unknown_fields` and would
refuse it after the push, leaving the landing `pushed_not_recorded`. So the
landing learns the daemon's version from its last renewal of the push turn,
before it pushes: a rebased or adapted landing that daemon could not record
stops with an error naming the daemon's version and saying nothing was
pushed, and the turn is released (`protocol::fits` owns the rule). The
client never sends that request in an earlier version. A landing that
rewrote nothing is the same request in every version and still lands through
an earlier daemon. To a client of version 5, 4 or 3 the daemon gives every
landing, in a landed answer, the status view and the journal, without
`rebased` and `adapted` (`protocol_5_shape`). Tests:
`a_rebased_or_adapted_landing_is_not_pushed_through_a_daemon_that_cannot_record_it`,
`a_merge_landing_still_lands_through_a_daemon_of_the_build_before`
(`tests/landing.rs`, through a socket that answers as a version 5 daemon),
`v6_landed_response_and_its_earlier_forms_match_the_golden_vectors`,
`recording_a_rebased_or_adapted_landing_fits_only_version_6`
(`tests/protocol.rs`).

**One implementation, two interfaces.** `integrate check` and `land` are
application operations in `src/landing.rs`. The CLI and the MCP tools
`langspan_integrate_check`, `langspan_land`, `langspan_land_queue` and
`langspan_land_cancel` call them; each runs in the
calling process (for MCP, the `langspan mcp` adapter, a separate process from
the daemon) and asks the daemon for state, because a check can take minutes
and the daemon answers each request within seconds. Both tools need a
`request_id`. For a check it is the record's ID, and a retry on the same
combined tree returns that record without running the command again. For a
landing it is the landing's ID, and a retry after it landed reports that
landing; any other earlier result needs a new ID. An MCP check gives the
command as an argument array, run without a shell, with a time limit of 1 to
3600 s that the caller keeps within its own tool-call limit. The MCP landing
has no `--check-again`: an agent checks with `langspan_integrate_check` first.
A retry of a landing that is still waiting keeps its place in line (the ticket
is the request ID).
LangSpan never rewrites or intercepts the owner's own `git push`.

**Choices in the land flow.** The turn is keyed by workspace and target, not
by repository. A moved target needs a passing check on exactly the new combined tree before
it is pushed, not merely a test run. The owner's `git push` is never
rewritten. Without the coordinator there is no turn and no landing, rather
than a landing without a turn. The turn outlives a slow check because it is
sized to the check's limit and renewed before each push; it is always
fetched after it is taken; and only a refusal after which the target is found
to have moved is retried, so a hook or protection rule's refusal is final.
A landing that polls a server for the turn and then goes on without it is not
the design; here a landing that does not get the turn pushes nothing, and the
line is first come, first served, bounded in length and time, visible,
cancellable, and kept across retries by request ID.

**What is touched.** Git objects (with `--rebase` and `--adapt-renames`, new
commits, trees and blobs, written through a private index in a private
temporary directory); refs under `refs/langspan/`; a temporary
worktree that is removed; on landing, the remote branch, and the
remote-tracking ref Git itself updates after a push. Never the working tree,
index, `HEAD`, local branches or configuration.

## Consequences and limits

- A check command is the owner's own command, run without a shell, with the
  owner's environment. Its environment is **not** part of the recorded
  identity: the same tree can pass on one machine and fail on another, and the
  record does not say which machine or toolchain. Records are local to one
  daemon and are not yet published to teammates.
- Ignored files are not part of the tree. A check that reads an ignored file
  (a local `.env`, a build cache) depends on something the record does not
  name.
- Snapshotting hashes every changed file and can take seconds on a very large
  working tree. Submodules are recorded by commit, not content.
- A combined check runs in a fresh checkout without ignored files or
  installed dependencies. The command must set up what it needs.
- Combining is in the order given and stops at the first conflict. There is no
  conflict resolution: branches that do not combine are reported with their
  hunks. Hunks are read only from conflict markers of Git's default size; a
  repository whose `conflict-marker-size` attribute makes them shorter gets
  paths and conflict types only.
- "Dependency-aware" here means the order the owner gives and containment: a
  branch already contained in the result is skipped and a stacked branch
  fast-forwards. Dependencies are not inferred.
- Landing is not transactional across a failure after the push. Once a
  push has started, no failure is reported as an error that could be read as
  "nothing was pushed". If the remote accepted it and recording the landing
  fails, the result is `pushed_not_recorded` (exit 8): the commit, the
  target's earlier tip, why the record failed, and a `git -C ROOT ls-remote
  REMOTE refs/heads/TARGET` command to verify. The log lacks the record, and
  running the landing again does not record it: it finds nothing to land. If
  the push itself did not finish (its 120 s budget ran out), the result is
  `push_unknown` (exit 9), with the same command and the two tips that tell
  the outcomes apart, and the attempt is recorded as `push_unknown`. An
  error result means no push was started or the remote refused it. Tested by
  a pre-push hook that stops the daemon, so the real push succeeds and the
  record fails (`tests/landing.rs`).
- **An unfinished push is settled when it can be** (added 24 November 2026).
  After the push's budget runs out, LangSpan asks the
  remote once, `git ls-remote REMOTE refs/heads/TARGET` within 30 s, without
  fetching or changing a local ref. At the pushed commit: it was pushed, and
  the landing is recorded as usual, the result saying the push did not finish
  (`unfinished_push`). At the earlier tip: nothing was pushed, `not_pushed`
  (exit 2), not retried, since a hanging hook or remote would hang again.
  Anything else (another push moved the target meanwhile, or the remote could
  not be asked): `push_unknown` as before, now with the tip it saw. Tests, with
  a local file remote and the test-only `LANGSPAN_TEST_PUSH_BUDGET_MS` (it can
  only shorten the 120 s budget, to no less than 100 ms):
  `a_push_that_timed_out_after_the_remote_took_it_is_settled_as_landed` (a
  slow `post-receive` on the remote),
  `a_push_that_timed_out_before_reaching_the_remote_is_settled_as_not_pushed`
  (a slow `pre-push`),
  `a_push_that_timed_out_while_the_target_moved_stays_unknown_with_how_to_check`.
  A remote that updated the branch to the pushed commit and then rolled it back
  in a hook would be reported as not pushed; that is what it holds.
- The turn coordinates landings made through LangSpan on one machine. It does
  not stop a plain `git push`, a teammate on another machine, or a same-user
  process that ignores it; the remote's non-fast-forward refusal is what keeps
  those from losing an update. The queue is local to one daemon, like the
  turn. Its tickets and sessions are labels any local client can use: one can
  cancel another's wait, or fill the line, which blocks landing through
  LangSpan and never a plain `git push`. Each ask while waiting is a request
  with a stored receipt (two a second). Two processes landing under the same
  request ID share one place and one turn, as they shared one turn before.
- An MCP check's output goes to the adapter's log, not to the tool result.
- **Filters are not run for a snapshot.** The private index is built with
  every configured filter driver's `clean`, `smudge` and `process` set empty
  and `required` false, and with fsmonitor off, for `read-tree`, `add` and
  `write-tree` alike (any of them can rehash a racily clean entry). A
  repository program therefore never runs to take a snapshot, and a required
  filter that is not installed (Git LFS configured, LFS absent) no longer
  stops `check run`. The cost: a filtered file that changed locally is
  recorded as its raw bytes, not as the object the filter would store. An
  unchanged file keeps HEAD's blob. Test:
  `check_run_with_a_required_filter_that_is_not_installed_records_the_raw_tree`.
- **Sparse checkouts.** Paths outside a sparse checkout stay as they are in
  HEAD: `git add -A` itself leaves them alone, even through the private index
  that has no skip-worktree bits. Observed with Git 2.56.0 in cone, non-cone
  and sparse-index modes; an older Git that does not apply the sparse
  patterns to `add` would record them as deleted. Test:
  `check_run_in_a_sparse_checkout_records_heads_tree_when_nothing_changed`.
- **Shallow clones are refused.** `integrate check` and `land` stop before
  fetching when `git rev-parse --is-shallow-repository` is not `false`: the
  merge base of a cut-off history can be missing or wrong, and a wrong combine
  would be checked and landed as if it were right. The message says to run
  `git fetch --unshallow`. `check run` still works in a shallow clone; it
  needs no history. Test:
  `integrate_and_land_in_a_shallow_clone_are_refused_with_what_to_do`.
- The temporary checkout of a combined result (`git worktree add`) still runs
  the repository's smudge filters. A combined check in a repository whose
  required filter is not installed is therefore expected to fail to start;
  this is not tested.
- Verified with local file remotes. A hosted remote with branch protection,
  required reviews or merge queues decides for itself whether to accept the
  push; LangSpan reports a refusal and never works around one.
