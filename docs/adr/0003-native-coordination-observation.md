# ADR 0003: Native coordination and local observations

**Status:** Implementation decision for the owner-authorized M3 work, 24 November
2026. Integration and native-host acceptance are in progress. The
build contract remains Proposed, including its
owner selections. This ADR extends [ADR 0001](0001-local-coordination.md) and
[ADR 0002](0002-claude-code-adapter.md); it does not widen the admission guarantee.

## Context

The first native adapter established a useful local hook path. M3 adds a second
host, detects changes outside those hooks and distinguishes an attached process
from a registered logical session. These additions must preserve durable retry
identity, independent host permissions, one local state owner and the distinction
between physical workspaces and related branch work.

Review also found two observation-attribution defects: a post event could name
paths outside its admission, and a concurrent admission from the same session
could be omitted from the holder list. Worktree support exposed a further
boundary: a linked checkout can be nested inside another registered checkout,
but admitting the same physical file under two workspace IDs would break local
coordination. These are concrete correctness changes, not grounds for treating
observation as authority.

The M3 capability review identified product
gaps that this implementation must name accurately: source-base validation,
timely agent awareness and full change records are still separate work.

## Decisions

### One typed hook lifecycle

[`src/hook.rs`](../../src/hook.rs) owns the shared admission/observation lifecycle.
Vendor modules parse host envelopes and identify supported operations. Both use
the same daemon and pure-core commands as CLI/MCP.

- `AdmitTool` carries an immutable `ToolIdentity`: operation ID, adapter, tool
  name and a SHA-256 digest of canonical semantic tool input. Object-key ordering
  is normalized; meaningful input changes produce a different digest. Raw edit
  bodies do not enter the coordination store.
- The stable native session/tool-call identity derives bounded, versioned wire
  IDs. Admission, observation and completion have separate request IDs. Workspace
  attachment is scoped to the operation as well as its physical root, so a
  transient registration failure does not poison every later operation.
- The original typed command remains the receipt identity. Reusing its request
  ID with different input is an error, including changes that touch the same file
  paths. The receipt is never rewritten to grant a fresh attempt.
- `ObserveTool` checks the stored tool identity and canonical covered paths.
  `input_mismatch` and `scope_mismatch` remain evidence and cannot complete the
  admission. Holder reporting excludes only the observed lease, so another
  active lease from the same session remains visible.
- Legacy untyped admissions and observations remain readable. Replaying older
  observations does not retroactively change their historical classification.

The adapter emits a JSON denial only for a newly returned conflict. Other
outcomes supply no permission decision; it never answers `allow`. A replayed
historical conflict is not represented as a newly checked current holder. A
supported post callback may complete an admission only while it remains valid.
An expired or invalidated callback is a late observation, not proof of the exact
time a filesystem write happened. Optional process reporting runs after the
coordination lifecycle and cannot suppress admission or post-result handling.

**The input digest is not source-base validation.** It proves which tool input
belongs to the operation. It does not prove which file version the agent read.
A replacement prepared from old contents can be admitted after another holder
finishes. Preserve that scenario in the product benchmark; a stronger stale-base
decision requires a separately verified read/version contract.

### Explicit host coverage

| Adapter | Implemented mapping | Evidence boundary |
| --- | --- | --- |
| Claude Code | `Edit`/`Write` pre events, successful and failed post events | Historical M2 captures and native runs exist. The changed M3 lifecycle needs fresh acceptance. |
| Codex | Canonical `apply_patch` pre/post events; additions, updates, deletions and move source/destination admitted as a whole set | Current fixtures are source-derived examples. Installed `0.172.0-alpha.2` host behavior is unverified. |

Codex shell wrappers, remote selectors, unknown directives and subagent
`agent_id` envelopes receive no decision and explicit unsupported coverage.
Missing or unfamiliar post results are unknown, not fabricated successes. No
Codex failure callback is assumed. Missing posts leave admissions to normal
expiry. The adapter parses affected paths but does not execute or rewrite patches.

Complete envelopes are bounded to 8 MiB; the Codex patch body to 4 MiB and 64
paths. One absolute hook deadline covers stdin, workspace resolution and IPC
(1.5 seconds by default); installed host timeouts are 5 seconds. These are
configured bounds, not an M3 latency result.

Project-level installation remains dry-run by default, with explicit `--apply`.
Generated hook commands have a stable ownership marker so executable relocation
does not duplicate registrations. The installer preserves unrelated settings,
recognizes the narrow legacy generated Claude format, refuses settings-file
symlinks and never silently replaces their links. Host trust and permissions
remain outside installation's authority.

### Distinct physical workspaces, including nested linked worktrees

An inherited hook resolves the host's working directory against the installed
Git checkout and its common directory. A separate linked worktree receives a
distinct workspace identity. Equal relative paths in two worktrees are separate
files and do not conflict merely because they share a branch lineage.

A linked worktree can also have its own project install. Hosts differ in
which project hooks they run for a session started there, so the inherited
hook (the main checkout's) first looks, by reading files only, for a
LangSpan hook of the same host in the nearest directory holding `.git` under
the event's cwd that names that worktree as its root and the same state
directory. What it does with one depends on the host's rule
(`Vendor::worktree_hooks` in `src/install.rs`). Every hooked event must be
decided by a LangSpan hook, so it steps aside only where the host is known to
run the worktree's own install for the same event:

| Host | Hooks run in a linked worktree with its own install | Observed on | Inherited hook |
| --- | --- | --- | --- |
| Claude Code | Both: the worktree's own project hooks and the main checkout's local settings | Claude Code 2.1.330, macOS 27.2 (launch validation, bug 3) | Steps aside (`deferred_to_worktree`) |
| Codex | Only the main checkout's project hooks; the worktree's own never run | Codex 0.171.0, macOS (issue #2) | Decides, as the worktree's install would |
| Cursor | Not known: no run in a linked worktree is retained | — | Decides, as the worktree's install would |
| Gemini CLI | Not known: its source reads project settings from the directory it starts in (ADR 0033), but no run in a linked worktree is retained | — | Decides, as the worktree's install would |

On Claude Code the inherited copy used to answer every event a second time,
as `unknown` ("workspace root overlaps an existing physical workspace") at
29–53 ms, and ran the commit guard and Stop twice. It now prints nothing,
logs coverage `deferred_to_worktree` and calls neither Git nor the
coordinator, as a user-level hook defers to a project install (ADR 0010).

On Codex that step-aside left a session in the worktree with no LangSpan
decision at all, since Codex never runs the worktree's hooks (issue #2). Where
the host runs only the main checkout's hooks, or where that is not known, the
inherited hook instead checks, with Git, that the worktree shares the
installed checkout's common directory, then decides under the worktree
install's workspace ID and root. A session there is coordinated in the same
workspace as sessions the worktree's own install covers (for example Claude
Code there), and a host that turns out to run both reaches one decision for
one workspace: the same call is the same idempotent request. A prompt-summary
or code-briefing opt-out in either install holds: the prompt's summary and
names are derived only after the workspace is resolved, from its settings, in
every host adapter. An unconfirmed host
therefore falls back to deciding, never to silence; a decision made twice
costs one extra hook run, while silence leaves edits uncoordinated. An
install there with another state directory, or none, leaves the inherited
hook covering the worktree under a workspace derived from its own, as
before. `langspan doctor` reports each linked worktree whose install the
host will not run, and fails when nothing the host runs covers it; a main
checkout hook whose executable is gone or that lacks a required event does
not count as covering it. A rule
moves to `Both` only with a retained real-host run in a linked worktree.

Tests (real linked worktrees, a real daemon, constructed host events):
`the_main_checkouts_hook_leaves_a_worktree_with_its_own_install_to_that_install`
(Claude Code),
`codex_in_a_worktree_with_its_own_install_is_coordinated_by_the_main_checkouts_hook`,
`an_unconfirmed_host_decides_in_a_worktree_with_its_own_install_rather_than_stepping_aside`
(Gemini CLI and Cursor),
`a_worktree_installs_prompt_opt_outs_hold_when_the_main_checkouts_hook_decides`
(Codex and Cursor),
`doctor_reports_a_worktree_install_the_host_will_not_run` and
`doctor_does_not_count_a_broken_main_checkout_install_as_covering_a_worktree`. A real Codex run
in a linked worktree with a conflicting edit refused is pending.

Overlapping roots still require explicit daemon verification of distinct Git
toplevels with the same common directory. An internal prepared registration
records the verified existing workspace IDs. Clients cannot submit that
verification through the wire format, and receipts retain the ordinary original
registration command. Legacy workspace records have no verification metadata.

A new nested registration is rejected while an active parent admission covers
its files. Once registered, the descendant's paths are excluded from the parent
domain even if a `.git` marker later disappears. Registering the parent after a
child does not invalidate the child's leases. Same-root aliases and unverified
ordinary overlapping roots remain rejected. Related-work awareness across these
domains is a later advisory delivery path, not shared-file locking.

### Liveness is observed evidence

`ProcessIdentity` contains PID plus a start token derived on Linux from boot
identity and process start metadata. The adapter checks same-user ownership and
may recognize a native-host ancestor; it never reads process arguments,
environment or credentials. Ambiguous ancestry is left unbound. The explicit
`session attach-pid` CLI command can associate an owned process with a registered
session. There is no new MCP process-binding tool.

`SessionLivenessObserved` records `alive`, `exited` or `unknown`, with observation
time and daemon generation. PID reuse is not the same process incarnation. A
different current-generation binding is rejected rather than silently replacing
the old one. Restart makes the projected status unknown until re-observed.

The daemon polls up to 64 bindings per one-second maintenance pass, rotating
through larger sets. Unchanged non-exited results are coalesced for 15 seconds;
an unchanged exited incarnation needs no repeated event. This is scheduling
behavior, not a freshness SLA. Liveness never re-registers a session, renews,
releases or grants a lease. Linux reads `/proc`; macOS reads the kernel's
process table through `/bin/ps` ([ADR 0025](0025-distribution-lifecycle-and-store-beyond-the-first-release.md),
row J12); other platforms report unknown or unsupported. A recent hook run
also keeps a session current where no process is bound (row J12).

### Bounded, passive Git observations

One background worker reads Git metadata and file contents outside the state
owner. It uses one two-second scan budget and a round-robin scan opportunity
every two seconds. With N workspaces, nominal revisit time is about N × two
seconds before load and scan overruns; no global two-second freshness guarantee
is implied.

The scanner uses fixed read-only Git operations with optional locks disabled.
`GIT_NO_LAZY_FETCH=1` prevents missing promisor objects from triggering a fetch;
missing required local data fails the scan instead.
It hashes regular-file bytes and symlink target text without following symlinks
outside the workspace. It retains file kind as well as digest, so a same-digest
regular-file/symlink transition remains a change. Incomplete reads, warnings,
metadata movement, Git output over 4 MiB or more than 1,024 local branches fail
the scan; they do not manufacture deletions or replace the last complete view.

#### Content bounds make a scan partial, not failed

Amended 24 November 2026. Before this, a
repository over 4,096 files, one file over 4 MiB, 64 MiB of content, a single
submodule or a sparse checkout failed every scan, so LangSpan observed nothing
there at all.

- **Versioning order.** When the listing has more than 4,096 file keys, the
  observer asks `git status` which paths changed (staged, modified, unmerged,
  untracked) and versions those first, then clean files in path order until
  4,096 are versioned. Within the count bound, clean files are versioned in
  path order while they fit; the first time one does not fit the byte budget,
  `git status` is asked and only changed files are versioned after that.
- **Bounds.** At most 4,096 files are versioned. A file over 4 MiB is listed,
  not read, and counted as too large. Clean files may use at most half of the
  64 MiB content budget, so changed files always have the other half. Clean
  files are versioned only in the first half of the scan's deadline, so the
  changed files and the final consistency listing fit in the rest. The
  two-second daemon deadline still bounds the whole scan; exceeding it still
  fails the scan.
- **No process per file.** A clean file is compared with its indexed blob by
  computing the Git blob ID of its working bytes (SHA-1 or SHA-256, as the
  repository's object IDs are), not by `git cat-file`. Before this, each clean
  file started a Git process; at about 8 ms each on the development Mac, a
  repository of a few hundred files could not be scanned inside two seconds.
  That figure is one shell loop of 200 `cat-file` calls on an Apple-silicon
  Mac, not a benchmark.
- **Submodules** (gitlinks, mode 160000) are recorded as present, by count
  and the first eight paths, and never read into. A change inside a submodule
  is not an observation of this workspace.
- **Sparse checkouts.** A skip-worktree entry (`S` in `ls-files -t`) is not in
  the working tree. It is counted, not read, and never reported as deleted.
  Widening the sparse checkout makes its files appear, and they are reported
  as present.
- **Merges and rebases.** Unmerged entries were already scanned; a file in
  conflict is now marked `conflicted` in the snapshot as well as dirty.
- **`git status` runs no repository program.** It is asked with fsmonitor and
  the untracked cache off, submodules ignored, and every configured filter
  driver's `clean`, `smudge` and `process` set empty and `required` false. Only
  the driver names are read from the configuration.
- **Coverage is stated.** The snapshot carries counts of what was left out
  (`omitted`), not versioned because too large (`too_large`, first eight
  paths), submodules and skip-worktree entries. A scan with `omitted` or
  `too_large` above zero is partial. The counts are in the scan summary
  (`changes`), in state for `recap` and `doctor`, which call such a scan
  partial and say what that means. A partial scan does not make `doctor` exit
  non-zero: it is a stated limit, not something to fix.
- **Nothing left out is a change.** A path the current scan listed but did not
  version is not reported deleted. After a partial scan, a path it did not
  version is treated like a first baseline: reported only if it is dirty, with
  its earlier version unknown. The source index keeps such paths as they were
  indexed and does not report their exports as removed.
- **Limits.** In a partial scan, a clean file that is left out is not
  watched: a change that leaves it clean (a checkout that rewrites thousands
  of files) is seen only for the files that were versioned. Which clean
  files are versioned can differ from one scan to the next, as the changed
  set and the time taken change. Tests: `tests/git_observer.rs`
  (`a_repository_over_the_file_bound_scans_changed_files_first_and_reports_coverage`,
  `the_byte_budget_leaves_clean_files_out_before_changed_ones`,
  `a_partial_scan_asks_git_for_changes_without_running_repository_filters`,
  `a_submodule_is_recorded_as_present_and_never_read_into`,
  `a_sparse_checkout_scans_without_reporting_files_outside_it_as_deleted`,
  `a_scan_during_a_conflicted_merge_or_rebase_completes_and_marks_the_conflicts`),
  `tests/store_scan_integration.rs`
  (`a_partial_scan_is_recorded_with_its_coverage_and_omissions_are_not_deletions`)
  and, through the real daemon with its own bounds, `tests/repository_shapes.rs`
  (`a_repository_over_the_scan_bounds_is_scanned_partially_and_says_so`: 4,202
  files and a 5 MiB file).

#### Checkout changes under a shared checkout

Amended 24 November 2026. When a scan sees the
branch change, HEAD move while files changed with it, or at least two changed
files go back to their committed version with HEAD unmoved (`checkout`,
`switch`, `reset --hard`, `stash`, `pull`), it records a checkout change with
the old and new branch and commit and the number of files that changed. A
commit moves HEAD without changing a file and is not one. Who made the change
is not known and not claimed. A session in that checkout that had read or
edited a file there before the change is told at its next prompt, in one
briefing line naming the old and new branch and commit. A stale refusal no
longer names a peer whose own last version of the file is not what is on
disk, so after a checkout the refusal names nobody; it still comes once per
file and change (ADR 0005). Tests:
`a_checkout_switch_is_briefed_to_the_sessions_in_it_and_refused_once_per_file`
and `a_merge_conflict_is_scanned_and_an_edit_from_an_old_read_is_refused_once`
in `tests/repository_shapes.rs`, through the real hook executable,
`stash_and_reset_hard_are_checkout_changes_and_a_commit_is_not` in
`tests/store_scan_integration.rs`, and
`a_file_changed_after_a_peers_edit_is_refused_once_without_naming_the_peer` in
`crates/langspan-core/tests/source_versions.rs`.

Amended 25 November 2026 (issue #139). A session's own Git command is not
news to it. The shell hooks (ADR 0011) read each command line for a Git
operation that can change the checkout (`git merge`, `pull`, `rebase`,
`reset`, `checkout`, `switch`, `restore`, `stash`, `cherry-pick`, `revert`,
`am`, `commit`; not one naming another `--git-dir` or `--work-tree`). When
such a command finishes with the branch or HEAD moved, or files changed, the
post-tool hook records a checkout run: the session, its redacted command line
(at most 120 bytes), the branch and HEAD it left, how long it ran, and how
long ago it finished. The hook reads the daemon's own clock (time since
boot, sleep included; not wall time) when the command starts and finishes
and keeps both readings in its record; it sends durations, not times, and
the daemon places the run back from its own now by them, so the run never
reaches past the daemon's now. A report redelivered after the daemon was
unreachable sends the age measured at that delivery, so the run keeps its
own window rather than moving to the retry's arrival: a retry neither loses
the command's change nor credits a later one to it. A retry differing from
a recorded report only in that age is the same request. A record written
before these readings, or with a finish reading later than the clock now
(taken before a reboot restarted it), reports the run as finishing on
arrival; a run reported after a reboot is in a new generation and explains
no change from before it. A duration counts as two hours at most. The
run explains a checkout change the scan observed while it ran, or the first
change observed after it, within 60 seconds, at the branch and HEAD it left.
Once a scan while it ran has seen that branch and HEAD, no later change is
its, so a later file restore at the same HEAD is still told. The run also
records whether the command changed any file's content. One that changed
none, as a commit does, gets no checkout change from the scan, so a change
after it with branch and HEAD in place, such as a file restore, is not its.
Either order
works: the scan may see the change before the hook reports, or
after. The session that ran it is not told of a change it explains; every
other session in the checkout still is, with that session and its command
named. A change no run explains, such as the owner's `git checkout` in a
terminal, is told to every session as before, naming nobody. This is
attribution by timing and the command line, not proof: a change someone else
makes while the command runs is taken as the command's. Stale refusals (ADR
0005) are unchanged. Tests:
`a_sessions_own_merge_is_not_briefed_to_it_and_is_named_to_the_others` in
`tests/repository_shapes.rs`, with two sessions in one checkout and one
merging through its shell tool, and `crates/langspan-core/tests/checkout_runs.rs`.

#### Changes no hook reported, with a Codex session registered

Amended 25 November 2026 (issue #129). Codex keys hook trust to the exact path
of the project's hooks file. A real run (`docs/evidence/codex-real-host/`)
trusted LangSpan's hooks with Codex started at `/tmp/…`; `codex exec resume`
reopened the session at `/private/tmp/…`, the same directory on macOS, where
Codex ran none of them, warned nobody, and the edit landed seen only as a
`disk_observed` fact. LangSpan cannot observe a hook that does not run, so it
reports the effect instead:

- When a scan sees a file's content change since a complete earlier scan, and
  no checkout change accounts for the scan, the daemon passes those files at
  their new versions to the core (`ObserveUncoordinated`, internal to the scan
  like `ObserveDisk`). The core drops files under an active admission (an edit
  whose report has not arrived) and versions any session of the workspace
  read or wrote. If any remain and a Codex session is registered there in this
  generation, it records `UncoordinatedChanged` naming up to two Codex
  sessions, most recently active first, and up to eight files. A Codex
  session already named in a notice still within the briefing horizon is not
  named again, so one resumed session produces one notice, not one per scan.
- The next briefing to each session of that workspace carries the newest
  owed notice once (`UncoordinatedDelivered`), as one line naming the session,
  the likely cause and the files still at an unreported version; a report
  arriving after the scan removes its file from the line, and a notice whose
  files are all explained is dropped.
- The named session is the likely cause, not a proven author: a hand edit or
  another tool looks the same, and the line says so. With no Codex session
  registered nothing is recorded, since there is no cause to name and a hand
  edit is ordinary. Only Codex has this observed path-keyed trust failure,
  so the rule names Codex sessions only.
- `langspan hooks install codex` prints the resolved root (no symbolic links)
  and recommends starting and resuming Codex there; the usage guide and the
  skill say the same.

The notice adds no trust boundary: its inputs are the daemon's own scan and
existing session records. Paths in it are repository paths already validated
by the scan, quoted and bounded like every briefing line. Tests:
`crates/langspan-core/tests/uncoordinated.rs` (decision, once-per-session
delivery, horizon, validation and wire vector),
`a_real_change_no_session_reported_names_the_registered_codex_session` in
`tests/store_scan_integration.rs` (a real Git checkout and SQLite store),
`a_change_no_hook_reported_is_told_once_naming_the_registered_codex_session`
in `tests/briefing.rs` (a real daemon, its background scan and the real hook
executable) and `codex_hook_install_names_the_resolved_root_to_start_codex_in`
in `tests/cli.rs`. A real Codex run resumed under an aliased path, showing the
warning, is pending.

The owner commits individual `DiskObserved` and `BranchObserved` events with the
complete current snapshot projection in the same SQLite transaction. All
snapshot paths and file states are validated, including clean or unchanged
files. On the first complete scan, dirty paths have an unknown prior version;
they are not claimed to be newly created. Later observations compare content
and kind, without claiming who caused the change. Correlated operation IDs, if
present, are unverified context, not authorship. A periodic scan cannot recover
an intermediate write that was reverted before the next sample.

Branch observations contain local ref/HEAD, commit and tree identity, workspace,
time and generation. They do not establish a logical change through rebases,
affected paths, integration base or integrated/superseded state. Those fuller
change records belong to the later versioned-change/check slice.

### One store, versioned reads

The protocol is version 2 and SQLite schema is version 2. Migration adds the
snapshot projection and observation query index without discarding events or
receipts. Existing persisted types default newly added optional fields, and
archived v1 golden vectors remain readable. Old wire clients are not accepted
as version 2 clients; upgrade the daemon and its clients together.

`changes WORKSPACE` and the MCP tool `langspan_changes` return paginated
disk/branch events plus the last complete scan summary. `events` and `changes`
are read-only, do not cache reads as mutation receipts, and cap their event
payload budgets below the 64 KiB frame limit. `status` includes process evidence
but remains an unpaginated projection. A large status response can still fail
explicitly; coverage aggregation and paginated status are pending work.

Observation milliseconds are local monotonic values within a daemon generation.
A prior generation's scan or process result is not current merely because it
exists. A failed scan retains the previous complete summary and emits a daemon
diagnostic. A failed scan and the last completed scan's coverage are both
recorded per workspace and shown by `recap` and `doctor`.

## Consequences and acceptance

This delivers primitives for local coordination and observation. The edit hooks
do not brief the model before it chooses work. Queryable facts do not by
themselves establish automatic agent awareness. M4 needs a bounded, verified
turn-start or MCP-preflight delivery path, with relevant facts and their age
received before the edit decision and no human relaying context. A graph is not
required to prove that loop.

[Implementation status](../IMPLEMENTATION_STATUS.md) separates local checks,
historical evidence, CI and actual host runs. The current executor cannot bind
the daemon's Unix socket, has no Claude Code executable and could not initialize
the installed alpha Codex app-server before a model request. Consequently,
source fixtures and compiled integration tests are not M3 native acceptance.
Keep the mixed-host and linked-worktree runs open, with success, denial,
unrelated work, permissions, unsupported inputs and missing-post cases retained.

The M5 benchmark harness starts alongside this work and must retain failures and
timeouts. Useful integrated completion, unnecessary denials, missed stale writes,
human intervention and observation coverage matter in addition to hook latency.
Remote publication, full change/check records, code graphs, Windows transport,
platform release commitments and the licence remain outside this local slice.
