# Coordination benchmark scenarios

These are workload and evidence requirements, not results. Use owner-supplied
native commands and task-specific checkers. Keep each scenario/item, arm, and
repetition separate in the [raw schema](records.schema.json). The three comparison
arms are no tool, coarse file claims, and the full **currently implemented**
LangSpan configuration. Another coordination tool is an optional, separately
configured arm (`comparable_tool`).

## Common experimental record

For every trial retain the initial commit/tree and dirty-file identities, task
inputs, exact model/host versions and permission settings, coordination
configuration, both native transcripts, observed host tool-call IDs, complete
available coordination diagnostics/events, actual file changes, final integrated
checks, and a timestamped human-intervention log. Record unsupported tools and
missing observations rather than excluding them. If a native host never starts,
keep its exit/error and mark the relevant behavioral outcomes unknown.

Use equivalent task and initial-source conditions across arms. Reset explicitly;
never let a previous arm's successful changes become another arm's baseline.
Keep elapsed runtime, model/token accounting when actually available, hook
process time, observer scan cost, and integrated correctness as distinct
measurements. A fast incorrect or unfinished result is not a useful completion.
Retain host permission waits and refusals; do not grant broader permissions to
make one arm faster.

## Same-file overlap while an admission is current

**Item ID:** `same-file-current-holder`

Have independent native sessions attempt a meaningful edit to the same physical
file, while A's supported admission is current. Capture B's actual pre-tool
attempt and denial/continuation, the holder state at that point, whether the tool
ran, and its resulting bytes. Include the host's normal permission behavior.

Measure current-conflict handling and how the work eventually completes. Check
that the integrated result contains both intended changes. A denial alone is
not integrated correctness; a recorded pre-tool response alone is not proof the
host applied it. Use distinct native attempt IDs for subsequent retries.

## Unrelated work remains useful

**Item ID:** `unrelated-files`

While A works on file F, B performs a meaningful change to unrelated file G in
the same checkout. Include a task check for each change and their integration.
Record whether B encounters a coordination denial, a host permission wait, or a
non-coordination failure. An unrelated-work denial is scored only after review
of actual scopes and other current holders, not from a generic error string.

Keep this negative-control workload adjacent to collision results. A mechanism
that blocks useful unrelated work can reduce collisions while worsening the
product outcome.

## Stale write after a completed, released lease

**Item ID:** `stale-write-after-release`

1. B reads the old file F, or obtains the older input through its ordinary task.
   Retain evidence of exactly what B read.
2. A changes F and completes its operation; its lease is released.
3. B then chooses and attempts a whole-file replacement derived from the older
   read, after A's lease has ended.
4. Inspect the actual tool input/base evidence, the execution result and bytes,
   then run an oracle requiring A's and B's intended changes to survive.

The current active-lease gate can admit B here because no holder remains. A
semantic input fingerprint protects retry identity; it does not prove which
source version B previously read. Do not relabel a later observation as stale
write prevention. Count a missed stale write only from retained read/base,
write, and final-result evidence. This is distinct from a permission prompt
outliving an active lease.

## Permission wait beyond expiry

**Item ID:** `permission-wait-expiry`

A supported pre-tool admission occurs, then the normal host permission prompt
remains open beyond expiry. B may be admitted in the meantime. Record the actual
permission decision, tool execution, post callback, lease relationship and final
content. Also keep refusal/cancel cases with no post callback.

A late post callback proves a late observation, not necessarily a late filesystem
write. Use the native execution and file evidence to score the write. Permission
approvals are human interventions; they are not human relays of another agent's
work information. Report those categories separately.

## Related work in separate worktrees

**Item ID:** `related-worktrees`

Use two physically separate worktrees of one repository. A changes an interface
while B implements a caller or related behavior in its worktree. Their same
relative filename is not the same physical file and must not be treated as a
local file collision. The task still needs advisory awareness and an integrated
result that respects the interface change.

Capture worktree roots/common repository identity, revisions, available context,
what B actually received before deciding, B's selected action, and the combined
check. A successful worktree-registration test establishes separation only;
queryable metadata does not establish agent awareness.

## Two-developer context ordering (M4 product-proof companion)

**Item ID:** `two-developer-before-decision`

Retain this as a separate M4 run series, not an inference from local harness
completion. Follow the build contract's product-proof test: two people,
two machines, ordinary sessions, and explicitly authorized advisory publication.
At least five complete attempts must be kept, including failed attempts and all
available artifacts.

The evidence must establish this causal sequence:

1. A's real work signal is created automatically from its work, then available
   to B's coordinator; record its identity and source/freshness.
2. The information is delivered to B's native model context **before B chooses
   the relevant edit**. Capture the actual framing and the receiving turn.
3. B visibly accounts for that work: changes sequence, builds against it,
   narrows its edit, or asks. Capture the selected tool input and its outcome.
4. No person transcribes or relays A's work to B. Keep intervention and relay
   logs, and separately show unrelated work progressing.

A pre-tool hook emitting context after the model has selected a tool call is
insufficient. Context later displayed next to the tool result does not establish
pre-decision receipt. A manually typed intent plus status query is a transport
smoke test, not this proof. Merely instructing the test agent to mention another
agent's work also fails the framing test.

Preserve the exact labelled context and its source, age, trust framing and turn
position. Treat another session's content as data, not instructions. Include a
case where the supplied shared-work text contains instruction-like language;
inspect whether the receiving agent keeps its own task and permission boundary.
Do not manufacture a model transcript to satisfy this criterion.

Use causal IDs and the receiving host's local sequence to establish ordering.
Do not compare unsynchronized machine wall clocks as proof. If an awareness delay
can be measured, keep it individually with clock/measurement uncertainty and
compare it with **Target: 60 seconds**. Leave the delay unknown otherwise. Five
runs cannot support a p99 estimate; any tail-latency study needs a separate stated
workload and adequate sampling.

## Interpreting the comparison

Report correct integrated completion, elapsed time, human interventions,
unnecessary denials, missed stale writes and observation coverage for every arm.
Keep all startup failures, interruptions, unsupported operations and unknowns.
Pair collision claims with the coverage denominator and state counts. A Git scan
cannot recover an intermediate write-then-revert it did not observe.

The result should identify which coordination mechanism changes useful outcomes.
Do not claim that lower hook latency proves better model quality, or that a
source graph is justified merely because it is absent. M5 evidence gates the
later symbol/context expansion; this harness alone is not that evidence.
