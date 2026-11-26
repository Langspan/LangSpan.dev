# LangSpan implementation status

## Current summary, 25 November 2026

`main` is at `d29ec9c` (PR #58 merged, after PR #57). Every launch-bar track
(L1 to L10 in the [roadmap](roadmap.md)) has merged, and
PRs #57 and #58 fixed four of the five faults the launch validation found.
This summary separates the merged implementation from retained host
evidence. It does not certify a fresh complete test run. Commit IDs, pull
request numbers and CI runs in this document refer to the project's
development history before this repository was published.

**CI on `main`.** The `Rust checks` workflow runs `Linux`, then `macOS` (only
after Linux passes), plus `Workflows`. The last completed run on `main`, for
`94fa888` (PR #57, run 45623704896),
passed on Linux, macOS and Workflows. The run for `d29ec9c`
(45625216724) was still running when this was written: Linux and Workflows had passed,
macOS had not finished. Since PR #48, a pull request's Linux job takes about
3 to 4 minutes and a push to `main`, which also builds and runs the release
binary, about 5. Recent Linux failures:

- `592e284` failed at `tests/graph.rs:231` (`left: ["module"]`,
  `right: ["module", "function"]`). That was a product race between a graph
  answer's rows and its coverage, fixed by PR #40.
- The graph tests' hooks have also answered `timeout` on CI. PR #50 made such
  a failure name the path that fired; see the open issue below.
- `e50a41f` (PR #37's merge) failed in
  `advisory_liveness_rejection_does_not_suppress_admission_or_observation`
  (`tests/codex.rs:635`, `Text file busy` while starting the copied test
  executable). PR #55 gave that test the shared retry for a freshly copied
  binary that PR #49 added for the Homebrew install test.

**What is merged.** The local coordinator; Claude Code and Codex adapters;
[Cursor](adr/0032-cursor-adapter.md) and [Gemini CLI](adr/0033-gemini-cli-adapter.md)
adapters built from documentation; briefings; source-version checks;
[the end of a reply and of a tool batch](adr/0027-turn-and-batch-events.md),
with Codex `Stop` and `SessionEnd`; [line ownership](adr/0028-line-ownership.md);
the [commit guard and `commit --mine`](adr/0026-commit-guard-and-commit-mine.md);
[break alerts at write time](adr/0029-break-alerts-at-write-time.md) (argument
counts, renames and removals; not same-count changes); [automatic checks after write batches](adr/0030-automatic-checks-after-write-batches.md)
(off by default, not classified); remote awareness through Git refs (with
`--publish-as-branch`); checks and landing with the landing queue, conflict
hunks and followed file renames ([ADR 0007](adr/0007-check-evidence-integration-and-landing.md));
the source index; notes, messages and handoffs; user-level installation;
partial [shell-write coverage](adr/0011-shell-writes.md);
[region admission](adr/0012-region-admission.md); the read-only
[dashboard](adr/0013-dashboard.md); the [observation spool](adr/0014-observation-spool.md);
[version tolerance](adr/0015-version-tolerance.md); the
[release pipeline](adr/0031-releases.md); the
[LangSpan skill](adr/0034-langspan-skill.md) (`install --skill`, off by
default; tested with a temporary home; a real Codex read its `AGENTS.md` section and
quoted its stale-source rule, `docs/evidence/codex-real-host/`). The MCP server's tools are listed once, in `TOOL_NAMES` (`src/mcp.rs`,
asserted in `tests/mcp.rs`). The repository has an Apache-2.0 `LICENSE`.

**Roadmap and the launch bar.** A source-level capability review on
25 November 2026 listed 121 coordination capabilities, and a second review
the same day, at `ab28b80`, added 12 more. Every capability that is not yet complete is a
[roadmap issue](https://github.com/Langspan/LangSpan.dev/issues?q=label%3Aroadmap)
holding its specification, each under the ADR that decides it (0011 for the
heredoc rewrite, 0016 to 0025, 0029, 0030); the [roadmap](roadmap.md) indexes
them by area.
A capability counted as built exists in the code; that does not mean a host
has been seen using it.

**Launch validation.** Recorded on `9eaf3a8` with Claude Code 2.1.330 on
macOS 27.2 arm64. Its evidence files were not published with this
repository; the results were:

- Hook latency, fresh state: every event within the 25 ms p99 **Target**
  (worst p99 13.51 ms, PreToolUse Write, two concurrent sessions). As the
  line journal grew, PreToolUse of a write grew by about 1.3 ms per 100
  entries; PR #57 removed most of that growth (below).
- Real Claude Code sessions: a conflicting edit refused with the holder and
  symbol named; LangSpan's own stale-source refusal, the first on a real
  host; `git commit -am` held once and `commit --mine` committing only the
  session's hunk; a write-time break alert reaching the model's context;
  landing, run from the terminal and not by an agent, refused on a failing
  combined tree and done once the check passed on exactly that tree; briefings between live sessions in a linked
  worktree.
- Scenario bench, LangSpan only: 13 of 14 scenarios pass (9 of 14 at
  `4e7d013`). The one fail is the heredoc rewrite (scenario 5s). The bench
  feeds host-shaped events to the installed binary and is not a live host. In that harness's
  user-level install, PreToolUse p50 was 18.8 to 19.0 ms, against 3 to 5 ms
  for the project-level hook on the same machine; the cause was not
  isolated.
- Codex could not be driven (no sign-in under a temporary `CODEX_HOME`, and
  project hooks need the owner's trust approval).

Fixed on `main` since the validation:

- PR #57: a write hook reads only the line-journal entries of the paths it
  writes. Measured in PR #57 at 4,000 to 4,300 entries: PreToolUse Write p50
  60.3 ms before, 6.8 ms after (n = 300, Apple M6, under load). Some cost
  still grows with the written file's own entries, which line ownership
  needs, and the commit guard still reads the whole workspace's journal.
- PR #58: Stop, StopFailure, SubagentStop and SessionEnd close a session's
  open shell runs, so a host-refused `commit --mine` call no longer blocks a
  later one; `commit --mine` under another state directory names it; in a
  linked worktree with its own install, the main checkout's hook steps
  aside; a redelivered refusal for unread lines or a stale source is refused
  again. All tested with constructed events, real Git and a real daemon; no
  host run after the fixes.
- Issue #1: a long shell command no longer claims a file another session
  changed through its hooked edit tool while the command ran. Edit tool calls
  leave edit marks, and the shell comparison credits such a file to the
  editor, or to no session when both may have changed it
  ([ADR 0011](adr/0011-shell-writes.md)). At the 4,096-mark cap an edit
  goes unmarked and a shell run it could affect credits nothing. Tested with a real command held
  open while the other session's hooks edit, real Git and a real daemon
  (`tests/shell_concurrent_edits.rs`); the real-host rerun of the reported
  scenario is pending.
- Issue #2: the main checkout's hook steps aside for a linked worktree's
  own install only on Claude Code, the one host seen running both there.
  On Codex (0.171.0 was seen running only the main checkout's hooks in a
  linked worktree) and on Cursor and Gemini CLI (not observed there), it
  decides under the worktree install's workspace instead; `doctor` reports
  a worktree install the host will not run. Tested with real linked
  worktrees, a real daemon and constructed events; the real Codex run in a
  linked worktree is pending.
- Issue #101: a file a session creates with a whole-file write is that
  session's recorded version and its own lines, including after its own
  formatter rewrites it while another session edits in the same checkout:
  the next edit is compared with that version and admitted, and
  `commit --mine` commits the file. The cause was #98's; these are its
  regression tests. Tested with two sessions' hooked writes, a
  formatter-like shell command (after and during the other session's write),
  real Git and a real daemon (`tests/shell_concurrent_edits.rs`); the
  dogfood run in a shared checkout is pending. Another session's formatter
  rewriting this session's uncommitted lines still leaves them credited to
  neither session (#98), so the next edit is refused once as stale.
- Issue #124: a session's formatter run while another session's shell
  command is still running keeps the lines it only reformatted, of the
  session's own uncommitted lines, that session's; `commit --mine` takes
  them. A line the other command also changed, and another session's lines
  the formatter rewrote, are credited to no session
  ([ADR 0011](adr/0011-shell-writes.md)). Tested with two sessions' real
  hooks, a real command held open by a signal file, a formatter-like `sed`
  rewrite, real Git and a real daemon (`tests/shell_concurrent_edits.rs`);
  the dogfood run in a shared checkout is pending.
- Issue #126: before `cargo fmt` (any form), `rustfmt FILE...` or
  `prettier --write PATH...`, the Bash pre-tool hook on Claude Code and Codex
  names, once, the other live sessions with uncommitted changes in files the
  formatter may rewrite (at most eight files), and never refuses the command;
  the skill advises formatting only one's own files by name
  ([ADR 0011](adr/0011-shell-writes.md)). Tested with two sessions' real
  hooks, a real daemon and real Git (`tests/formatter_notice.rs`); the
  formatters themselves are not run there. A real host reading the notice and
  the dogfood run in a shared checkout are pending.
- Issue #99: `commit --mine` no longer reports a file as wholly another
  session's because that session made one write whose lines are not known.
  Such a file is still attributed hunk by hunk; a hunk whose lines that
  write may share is refused with the reason ([ADR 0026](adr/0026-commit-guard-and-commit-mine.md)).
  Tested with two sessions' hooked edits in alternation, each inserting above
  the other's, real Git and a real daemon (`tests/commit_guard.rs`); the
  dogfood run in a shared checkout is pending.
- Issue #106: `commit --mine` splits a hunk Git joined from this session's
  lines and another session's at the boundaries between them and commits
  only its own; a hunk where one session's line replaced another's, or with
  a line no session wrote, is still refused with the reason, and a partial
  patch that does not apply exactly to the index commits nothing
  ([ADR 0026](adr/0026-commit-guard-and-commit-mine.md)). Tested with two
  sessions' real hooks adding adjacent list entries in both orders, real Git
  and a real daemon (`tests/commit_guard.rs`), and the exactness check
  against a real temporary repository's index (`src/commit.rs`); the dogfood
  run in a shared checkout is pending.
- Issue #40 (B27): `langspan status --mine` and MCP `langspan_my_changes`
  list this session's uncommitted files and hunks, each `mine`, `mixed` or
  `unknown`, with the attribution `commit --mine` uses, and change nothing
  ([ADR 0026](adr/0026-commit-guard-and-commit-mine.md), section 3). Tested
  with two sessions' hooked edits, changes no session made, a staged file,
  a `commit --mine` afterwards, real Git and a real daemon
  (`tests/commit_guard.rs`); the Claude Code run calling it before
  `commit --mine` is pending.

**Observed on real hosts (retained on `main`):**

- Claude Code: edits on 2.1.331 ([M2](evidence/m2-claude-code/README.md));
  on 2.1.330 a session-start briefing ([M4](evidence/m4-briefing/README.md)), read
  recording ([M6](evidence/m6-stale-source/README.md)), and the end-of-reply
  and tool-batch events: `Stop`, `StopFailure`, `SubagentStop` and
  `PostToolBatch` ([turn events](evidence/turn-events/README.md), on a debug
  build of the PR #37 branch). Whether the model received a batch briefing was
  not checked.
- Claude Code 2.1.330 and Codex 0.171.0 taking sequential turns in one
  checkout on macOS 27.2 arm64 ([mixed hosts](evidence/mixed-hosts/README.md)):
  Codex completed an edit, Claude preserved it, and Codex later declined to
  edit without rereading the changed file. It predates the shell, region,
  dashboard and launch-bar work.
- Two machines, run 3 ([evidence](evidence/two-machine/README.md), PR #45), at
  `592e284`: a Mac and a Claude Code cloud machine published through GitHub
  (the cloud in the branch namespace, the Mac under `refs/langspan/`). Real
  sessions were briefed in both directions: Codex on the Mac quoted a running
  cloud Claude Code session, and cloud Claude Code sessions were briefed about
  the Mac's sessions. No session changed its edit because of a briefing; one
  added a merge-conflict warning. `remote disable` could not delete the ref
  (HTTP 403), so the ref was left holding a withdrawn publication. On Linux an
  exited session leaves the publication at once; on macOS an ended session's
  edits stay published for the 30-minute horizon.

**Not observed on a real host:**

- Any Codex hook since the mixed-host run, including `Stop`, `SessionEnd` and
  the first-patch snapshot.
- Cursor and Gemini CLI at all. Both are built from documentation; their tests
  run on constructed or source-derived events. No Cursor run has been captured
  and the headless Gemini CLI runs stopped at sign-in.
- Line ownership, automatic checks, the landing queue and conflict hunks,
  shell writes, two regions of one file admitted together, user-level
  install loaded by a host, the source-graph tools, messages, and any MCP
  tool called by an agent.
- Across machines: a change of approach caused by another machine's facts,
  simultaneous edits with both sides told, landing, and network failure.

**Releases.** [v0.1.0](https://github.com/Langspan/LangSpan.dev/releases/tag/v0.1.0)
was published on 25 November 2026. Four macOS/Linux archives, matching CycloneDX
SBOMs, SHA256SUMS and a Sigstore provenance bundle are listed on the release.
This describes published artifacts; it does not extend the native-host
acceptance evidence above. No Homebrew tap has been published.

**Known open issues.**

- **Hook timeouts on a stalled disk.** A hook's daemon call has a 1.5 s
  budget. Under heavy disk-flush contention, a single SQLite
  `synchronous=FULL` commit can stall past that, and the hook answers
  `timeout`. PR #50 reproduced it in a constrained Linux container (4 of 20
  runs of `tests/graph.rs` under heavy disk load, 0 of 20 without), traced two
  contributors (queueing behind scan recording, and the request's own commit
  stalling), and measured one candidate fix that did not help (5 of 20 after).
  Nobody has measured fsync latency on GitHub's runners. The ways forward
  listed in PR #50 (wait for the next CI failure's detail, a larger
  `--budget-ms` in the graph tests, or revisiting durable commits on the hook
  path under ADR 0001) are the owner's decision, pending.
- **A heredoc that rewrites a file loses a peer's change.** `cat > path <<'EOF'`
  from an old read gets no decision and is recorded after it runs as the
  session's change, so another session's change since that read is
  overwritten without a refusal. The scenario bench in the launch validation
  failed this scenario. Planned
  fix, not started ([ADR 0011](adr/0011-shell-writes.md#known-gap-a-heredoc-that-rewrites-a-file)).
- **A session that ends with neither `Stop` nor `SessionEnd`** (a killed
  host, for example) keeps an unfinished shell run counting for two hours, a
  shell call's supported lifetime (ten minutes before PR #6, which a
  longer-running command outlived; ADR 0011):
  it can still contest other sessions' changes and make `commit --mine`
  without `--session` see two running sessions (PR #58).
- **No hook for the host's own permission denial.** None is registered or
  verified here, so a Bash call the host refuses stays open until its
  session's reply or the session ends (PR #58), and an admitted edit the host
  refuses holds its admission until the reply ends or the admission's TTL
  runs out (M2, ADR 0027).
- The commit guard still reads the whole workspace's line journal, and a
  write hook's lookup still grows with that file's own entries (PR #57).
- No Codex, Cursor or Gemini CLI hook of the launch-bar work has run on a
  real host.
- Comparative evidence of reduced rework or manual relay is still open: no
  comparison arm of the [coordination benchmark](../benchmarks/coordination/README.md)
  has been run on current `main`.
- Windows transport is not available.

For the current implementation and limits, start with the
[README](../README.md), [usage guide](usage.md), ADRs and their linked evidence.
Check results belong to their exact commit and run; historical pass counts
below are not a total for the current tree.

## Historical checkpoint and implementation records

The records below preserve the state, branch names, environment restrictions
and verification available during each earlier slice. Their statements about
then-missing host evidence or future work are historical. Use the current
summary above and the linked evidence for the latest retained host result.

**Recorded M3 checkpoint:** 24 November 2026. M3 is merged (PR #10, `f0cf4e7`) and
its Linux CI check passed. Automatic briefing is the first slice
after it. Native mixed-host acceptance for M3 is still open. The M2 evidence
further down is historical; it does not certify the shared lifecycle or the
Codex integration.

### Automatic briefing (branch `feat/automatic-briefing`)

M3 (PR #10) is merged at `f0cf4e7`; its full check passed on Linux CI. This
branch adds the first roadmap capability on top of it. Decisions and limits are
in [ADR 0004](adr/0004-automatic-peer-briefing.md).

| Area | Implemented | Remaining boundary |
| --- | --- | --- |
| Session discovery | `SessionStart`, `UserPromptSubmit` and Claude Code `SessionEnd` hooks register, describe and end sessions without any agent action | Codex has no session-end event; subagent events are unsupported. |
| Activity | Per-session record of the 32 most recent successful edits and the owner's task line, derived from events | Times are per daemon generation; a restart hides earlier activity until the session acts again. |
| Relevance | Same checkout or same repository (linked worktrees), current generation, not exited, active within 30 minutes; overlap with the asker's own work first | No grouping before a workspace's first complete scan; separate clones are not grouped. |
| Delivery | `additionalContext` at session start and at a prompt whose briefing changed; 6 peers, 2,400 bytes, labelled as data | Mid-turn changes reach a peer at its next prompt. |
| Interfaces | `langspan brief`, MCP `langspan_brief` (ten tools), protocol 3 with golden vectors | Status is still unpaginated. |
| Portability | Builds and passes the suite on macOS arm64 as well as Linux | Process evidence remains Linux-only. |

**Checks run on macOS 27.2 arm64, Rust 1.100.0:** `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, and
`cargo test --workspace --locked`. The counts are in the pull request. New
tests: seven pure-core briefing cases, four real-daemon cases with real Git
worktrees and the hook executable for both hosts, and a protocol 3 golden
vector. `cargo audit` runs in CI.

**Native host:** Claude Code 2.1.330 delivered a SessionStart briefing to the
model, which quoted it ([evidence](evidence/m4-briefing/README.md)). Codex
0.171.0 ran no hook without the owner's trust approval, so Codex delivery is
unverified on the host. No run yet shows an agent changing its approach.

### Source versions and stale writes (branch `feat/source-versions`)

Decisions, and the exact list of what is prevented, detected or not covered,
are in [ADR 0005](adr/0005-source-versions-and-stale-writes.md).

| Area | Implemented | Remaining boundary |
| --- | --- | --- |
| Read versions | Completed Claude Code `Read` and every reported successful edit record the file's SHA-256 for that session; 128 per session; kept across daemon restarts | Codex reads are not reported by any hook. Files over 4 MiB and symlinks have no version. |
| Refusal | An edit whose file differs from the session's recorded version gets `stale_base` and a host `deny` naming the file and, when known, the peer that edited it | Applies only to edits through a supported hook. Not a filesystem fence. |
| Honest unknowns | A file with no recorded read is admitted with `base_unknown`; the hook diagnostic says it was not compared | First edit of any file by Codex is always in this class. |
| Notice | Files read before a peer edited them are named in the next briefing | Reported edits only; an unreported write is found at the next compared edit. |

New tests: eight pure-core cases and five real-daemon cases in which files
really change on disk between hook calls. Native host: Claude Code 2.1.330
recorded reads and refused the stale write with its own check before
LangSpan's hook ran; LangSpan's briefing named the session that had changed
the file ([evidence](evidence/m6-stale-source/README.md)). LangSpan's own
denial has not been produced by a real host.

### Cross-machine awareness (branch `feat/remote-awareness`)

[ADR 0006](adr/0006-remote-awareness-through-git-refs.md). Off until the owner
enables it per workspace.

| Area | Implemented | Remaining boundary |
| --- | --- | --- |
| Transport | One LangSpan-owned ref per participant on the repository's Git remote; `git push` and `git fetch` on a separate thread with a 20 s deadline | Hosted remotes' handling of refs outside heads and tags is untested. |
| Payload | Name, participant ID, sequence; per session host kind, ID prefix, branch, paths editing and recently edited; task line only with `--share-task on`; at most 16 KiB | Source graph, semantic notices and check evidence are not in it yet. |
| Ordering and staleness | Per-sender rising sequence; age from local receipt; dropped after 10 minutes unheard; 2-minute heartbeat while work is listed | No cross-machine clock is used or needed. |
| Failure | Unreachable remote is logged once and retried each pass; local coordination is unaffected; a restart re-receives everything | A push rejected for non-fast-forward is retried on what the remote holds. |
| Control | `remote enable`, `remote disable` (deletes the ref), `remote outbox` (exact payload), `remote_published` events with the exact payload pushed, listed by `remote sent`; no MCP tool | Retention of the published commit chain. |

Tests: six pure-core cases and two integration cases with two daemons, two
clones and one shared remote. **Both daemons ran on one machine.** No
two-machine, hosted-remote or two-developer run exists.

### Check evidence, combined checks and landing (branch `feat/checks-and-landing`)

[ADR 0007](adr/0007-check-evidence-integration-and-landing.md).

| Area | Implemented | Remaining boundary |
| --- | --- | --- |
| Evidence | `check run` binds a command's result to the exact tree it could read; inconclusive if the tree moved or the command did not finish | The command's environment is not part of the identity. Ignored files are not in the tree. |
| Applicability | `check status` compares with the tree now; a pass elsewhere is labelled as not applying; observed changes mark records | Records are local to one daemon. |
| Combining | `integrate check` merges branches onto the fetched target in the object database, reports conflicting paths, runs the check in a temporary checkout | No conflict resolution; order is the owner's; dependencies are not inferred. |
| Landing | `land` pushes only with a passing check for exactly the combined tree; never forced; dry run unless `--apply`; with `--apply` it takes a push turn per workspace and target, fetches inside it, and retries a target that moved up to 3 attempts, landing a new tree only on its own passing check (`--check-again`); MCP `langspan_integrate_check` and `langspan_land` call the same operations | Hosted branch protection untested. Not transactional if recording fails after the push. A landing that finds the turn held waits in a bounded first-come line (8 per target, `--wait-s` default 240 s, cancellable; exit 7 if it does not get the turn). Conflicts are reported with hunks; renames that landed are followed and reported. |

Tests: five pure-core check cases and seven push-turn cases, and nine
end-to-end cases with a real daemon, the CLI, the real `langspan mcp`
subprocess and real repositories with a shared file remote: a concurrent push
between check and landing, two concurrent landings, a target moved by a
teammate's push during the owner's push (refused without a new check, landed
with `--check-again`), a hook refusal that is not retried, and an agent's check
and landing over MCP. No native-agent run uses these commands yet.

### Source index and interface notices (branch `feat/source-graph`)

[ADR 0008](adr/0008-derived-source-index.md).

| Area | Implemented | Remaining boundary |
| --- | --- | --- |
| Extraction | `langspan-graph`: definitions, mentions, imports, export status and signatures for Rust, Python, TypeScript/JavaScript, Go | Name matching only. Other languages are not indexed. |
| Index | Derived `graph.sqlite3`, keyed by workspace, path and content digest; incremental after each scan; rebuilt if deleted | Holds the files the observer's scan versioned (at most 4,096; partial scans keep the rest as last indexed); lags by one scan and one pass. |
| Queries | `graph symbol`, `graph references`, `graph impact`; three read-only MCP tools; bounded, with coverage. `graph symbol --detail` (roadmap D1) adds each definition's body digest, calls, callers by name, notes on its file and last check, from a `calls` table at schema marker 3 | One level of impact. Callers are name matches. Detail is tested with a real daemon, Git repository and MCP client (`tests/graph.rs`, `tests/mcp.rs`); no agent has called it on a host. |
| Notices | Removed exported symbols and changed signatures reach sessions whose files mention the name, at their next prompt | One workspace only. A rename that changes only the name, in one file, is a `renamed` notice (roadmap B10); any other rename shows as a removal. Bodies are compared only to link a rename. |

Tests: four extraction cases across the languages, including hostile and
oversized input, and two end-to-end cases with a real daemon in which files
change on disk. No native-agent run uses the index yet, and no measurement
shows it improving an outcome.

### Recap, remembered notes and diagnostics (branch `feat/recap-remember-doctor`)

| Area | Implemented | Remaining boundary |
| --- | --- | --- |
| Recap | `recap WORKSPACE` and MCP `langspan_recap`: a bounded account from projected state, under 48 KiB | One workspace at a time. `status` is bounded the same way and pages full history with `status --all`. |
| Since the last session (C10, [issue #47](https://github.com/Langspan/LangSpan.dev/issues/47)) | `recap --since-away` / `--since-seq SEQ` and `langspan_recap`'s `since_away` / `since_seq` add `since`, read from the newest 10,000 events; a session start after at least two hours (`LANGSPAN_AWAY_AFTER_MS`) without interactive work gets a section of at most 500 bytes within the 2,400-byte briefing. Protocol version 8. Tests: seven core cases over constructed logs in `crates/langspan-core/tests/recap.rs` (a 3-hour gap, recent activity, a daemon restart and a reboot in the gap, the owner back, a sequence number, truncation, ended sessions and messages); four renderer cases in `src/since.rs`; one real-daemon case in `tests/briefing.rs` (Claude Code and Codex starts, a headless agent in the gap, a daemon restart, the CLI's JSON compared with the section); one protocol case | No real-host run after an overnight gap is retained yet. The real-daemon test uses a 1.5 s gap, not hours, and cannot reboot the machine; the reboot is covered by the core test. |
| Notes | `remember` / `forget` and MCP tools; one line of at most 400 bytes about the workspace or up to 8 files; told to sessions working there; kept across restarts; 64 per workspace | Local only. Any local session can read and forget any note. |
| Scan outcome | A failed scan is state (`scan_failed`), recorded once per distinct reason and cleared by a complete scan | The reason is the observer's own bounded message. |
| Doctor | `doctor [--root DIR]` checks the coordinator, workspaces, scans, index coverage, publication and installed hooks; exit 1 if anything needs attention; `--json` | It cannot see whether Codex has approved its hooks. |

Sixteen MCP tools. Tests: four pure-core cases and two end-to-end cases,
including a directory that is not a Git repository becoming one.

### M3 implementation (merged)

[ADR 0003](adr/0003-native-coordination-observation.md) records the new local
decisions. [ADR 0001](adr/0001-local-coordination.md) still defines cooperative
admission: the daemon does not fence later filesystem writes. All local
participants must use the same daemon/state directory.

| Area | Implemented in this working tree | Remaining boundary |
| --- | --- | --- |
| Native hooks | Shared typed lifecycle for Claude Code and Codex; bounded semantic input digest; exact retry identity; canonical scope and input mismatch observations | The digest identifies tool input, not the source version read by the model. |
| Codex | Bounded canonical `apply_patch` pre/post parsing for additions, updates, deletions and moves; unsupported/unknown cases explicit | Source-derived fixtures against an unverified alpha-host contract; native acceptance pending. |
| Workspaces | Distinct physical worktree identities, daemon-verified nested linked roots, active-parent-claim check and permanent descendant-path exclusion | Related work in different worktrees is not automatically delivered to agents. |
| Process evidence | Linux process-incarnation identity, optional recognized-host ancestry, explicit CLI attachment, alive/exited/unknown status | Point-in-time evidence only; no lease extension, release or authority. No other-platform process backend. |
| Git observer | One bounded background worker, file content/kind observations, initial dirty baseline, local branch/HEAD commit and tree observations | Unattributed and periodic; incomplete scans retain the last complete view. No full logical change records. |
| Storage and reads | SQLite schema 2; protocol 2; durable individual facts and complete snapshot projection in one transaction; byte-bounded event/changes pages | Retention policy and durable coverage summary remain open. `status` is a bounded current view; `status --all` pages every record within the frame (issue 97, tested with 10,000 closed admissions), a record larger than a page in ordered parts (tested with an automatic check and a session's known versions at their limits). `land` retries, `doctor` and `auto-check` read what the view left out complete. |
| CLI/MCP | `changes WORKSPACE`, `session attach-pid`, and nine MCP tools including read-only `langspan_changes`; status includes liveness | Reading facts is not proof of automatic context delivery before an edit. |
| Installation | Claude/Codex project hooks with explicit apply, stable ownership markers across executable moves, settings preservation and symlink refusal | Host trust, native permissions and new native-host acceptance remain separate. |

#### Scheduling, freshness and input limits

The daemon offers one round-robin Git scan every two seconds with a two-second
scan budget. N registered workspaces therefore have a nominal revisit time of
about N × two seconds before load or overruns. This is a configured schedule,
not a measured global freshness bound. Scans version at most 4,096 files, read
at most 4 MiB per file and 64 MiB in all, and permit 4 MiB of Git output and
1,024 local branches. Over the content bounds a scan is partial and records its
coverage (ADR 0003); over the output or branch bounds it fails.
The snapshot projection is also bounded to 4 MiB when serialized.

Liveness maintenance visits at most 64 process bindings per one-second pass and
rotates through larger sets. Unchanged non-exited samples are coalesced for
15 seconds. Restart makes prior process status unknown until a fresh probe;
old scan summaries retain their original generation. Observation milliseconds
are monotonic within a daemon generation, not timestamps comparable across
restarts or machines.

Hooks retain the default 1.5-second absolute budget, 5-second installed host
timeout and 30-second admission TTL (maximum 60 seconds). The envelope limit is
8 MiB, Codex patch limit 4 MiB and admitted scope at most 64 paths. These are
bounds, not measured M3 p99 latency claims. An ordinary permission prompt can
outlive a lease; a later callback reports a late observation without proving
the write's exact time.

#### Current verification and native-host acceptance

Focused core, parser, real-Git, SQLite, process, installer and protocol checks
exercise the new boundaries; final commands and totals belong to the current
PR/check result. Critical regressions include observation scope/input mismatch,
same-session concurrent holders, process-incarnation replacement, restart
liveness, passive disk facts, same-digest file-kind changes, verified worktree
overlaps and an active parent claim during child registration. Guard-removal
mutation checks were run in isolated copies for these core invariants.

The current executor still cannot bind the daemon's Unix socket:

```text
bind local Unix socket: Operation not permitted (os error 1)
```

The real-daemon suites remain enabled for CI. Compiling them is not a passing
runtime result. The merged PR #9 CI result is a baseline result, not the new M3
check. Do not infer full-workspace green or native acceptance from targeted tests.

[M3 Codex evidence](evidence/m3-codex/README.md) records the installed
`codex-cli 0.172.0-alpha.2`, source-derived payload fixtures and two native launch
attempts. Both stopped before a model request with
`failed to initialize in-process app-server client: Read-only file system
(os error 30)`. The exact installed alpha source tag was not retrieved. No
Claude Code executable is installed in this executor. No current mixed-host or
linked-worktree native acceptance is claimed.

Still required for M3: retained actual host payloads and tool outcomes; Claude
Code plus Codex conflict and unrelated-work runs in one checkout; the linked
worktree pair; permission coexistence; supported post observation; unsupported,
cancellation/missing-post and bypass-edit cases. Protocol 2 and the shared
lifecycle changes make this a new acceptance task even though M2 has valid
historical Claude evidence.

#### Product gaps retained explicitly

The M3 capability review identified three distinctions:

- **Source-base validation:** M3 binds a tool call to its semantic input, but does
  not prove the file version the agent read. An old whole-file replacement may
  be admitted after the previous holder finishes. Keep this case in the benchmark.
- **Change records:** branch-head observations contain local ref/commit/tree
  facts. Stable change identities across rebases, affected paths, integration
  bases and check applicability belong to the later versioned-change slice.
- **Agent awareness:** edit hooks perform admission and record observations;
  they do not provide a briefing before the model selects an edit. M4 requires
  a verified context-delivery path and an observed change in approach, without
  owners relaying messages. A graph is not required for that first proof.

The M5 benchmark harness is being prepared alongside M3. Product-outcome results
remain pending. Coverage reporting is still needed before
the proposed product release; a last successful scan does not prove a later
failed scan covered the workspace. Failed hooks and unavailable daemons still
have only best-effort host-side diagnostics.

### Historical M1 foundation

Date: 24 November 2026. Original source baseline:
`c5ec9037c931e0cb0780e99dee4e759f6354db93`. The following records the initial
implementation and its observed local checks, not a current M3 test count.

#### Delivered in M1

- Pure Rust domain crate with event decisions, validated replay, normalized file
  keys, whole-set admission, intent lifecycle, expiry and restart invalidation.
- A single SQLite owner with WAL/FULL durability, schema versioning, an ordered
  event log and original-command/outcome receipts committed atomically.
- A foreground Unix-socket daemon with an exclusive lifetime lock, owner-only
  files, bounded clients/queue/frames and explicit unavailable outcomes.
- Physical workspace and path canonicalization, escaped symlink/traversal
  rejection, alias unification and overlap checks within one coordinator.
- A CLI with readable/JSON results, explicit mutation IDs and meaningful success,
  conflict, error and inactive-replay exit codes.
- Eight typed MCP tools using the official SDK and the same daemon operations.
- Pinned toolchain/dependencies, build/check commands, CI, ADR, dependency notes
  and a STRIDE review. No existing Claude security settings were weakened.

[ADR 0001](adr/0001-local-coordination.md) defines the guarantee. All participants
must use the same daemon/state directory. Logical session registration does not
establish vendor-agent integration or process liveness. A historical admitted
outcome is distinct from current lease validity. No filesystem fencing is claimed.

#### M1 verification evidence

Tests were written against deliberate unimplemented boundaries first. Observed
red results included missing registration events, `SQLite store is not implemented`,
`local coordination daemon is not implemented yet`, and an MCP initialize
connection closure from the adapter stub. They were not counted as passing tests.

The isolated Linux development environment runs Rust 1.100.0 with bundled SQLite.
The named compiler pin was repaired after concurrent rustup setup; temporary
`+stable` runs used the same verified Rust/Cargo 1.100.0 release.

| Verification | Observed locally |
| --- | --- |
| Pure core state-machine and behavioral suite | 20 passed |
| Core dependency/import boundary | 1 passed |
| Real SQLite store/recovery/rollback suite | 8 passed |
| CLI parsing/help and protocol vectors | 6 passed |
| Client deadline checks using real socket pairs | 3 passed |
| Real MCP subprocess discovery, typed errors and frame boundary | 3 passed |
| Live daemon, CLI-to-daemon and MCP-to-daemon socket suites | Blocked by this executor's Unix-socket bind restriction; tests remain enabled for CI |

The actual socket failure was:

```text
bind local Unix socket: Operation not permitted (os error 1)
```

An elevated test attempt was rejected because this executor disables sandbox
approvals. No socket test was changed to skip or pretend to pass. The
[GitHub workflow](../.github/workflows/check.yml) runs the complete unfiltered check
on a normal Linux runner; its check result is required before merging this slice.
The PR/check run is the authoritative record of that full-run result.

Critical-path mutation checks were performed in isolated copies or briefly
restored source:

- Disabling conflict rejection made the whole-set test fail: `Admitted` was
  returned where `Conflict` was expected.
- Publishing state before SQL commit made the real-trigger rollback test fail:
  `uncommitted state must never be published`.
- Raising the MCP frame cap made an oversized initialize request succeed and
  failed the frame-boundary test. The 64 KiB cap was restored and passed.

Local component verification commands:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p langspan-core --locked
cargo test --lib --locked
cargo test --test store --test cli --test protocol --locked
cargo test --test mcp --locked -- --skip stdio_operations_share_daemon_state_and_preserve_retry_and_conflict_results
cargo build --workspace --release --locked
cargo audit
```

The targeted MCP command explicitly excludes the known socket-dependent test in
this restricted executor. The full check and CI use `cargo test --workspace
--locked` without exclusions. No application performance, signed-release,
macOS/Windows or live vendor-agent benchmark is claimed by this implementation.

### Historical M2: Claude Code adapter

Date: 24 November 2026. Built on the proposed build contract, with the decisions in [ADR 0002](adr/0002-claude-code-adapter.md). These retained results concern the M2 implementation and its test environment.

#### Delivered

- `Observe` command and `OperationObserved` event in the pure core. An observation records the host-reported result and its relation to the named admission: current, completed, expired, invalidated or not admitted. It also lists other sessions' current holders. It never grants, renews, completes or reactivates a lease, and a test proves that.
- `langspan hook claude-code`:
  - PreToolUse admission for `Edit` and `Write`, with a JSON `deny` only for a current conflict and no decision otherwise.
  - PostToolUse and PostToolUseFailure observation, with completion only while the admission is still valid.
  - Never `allow`, never exit 2.
  - Versioned digest retry identities.
  - Bounded input and a 1.5 s total deadline.
- `langspan hooks install|remove claude-code`: a dry run by default; `--apply` writes `ROOT/.claude/settings.local.json` and preserves other settings.
- Sanitized Claude Code 2.1.331 hook event fixtures and an observe request golden vector.

#### Verification

The M2 evidence records `sh scripts/check.sh` passing in its Linux environment, Unix-socket suites included, with `cargo-audit` 0.23.0 installed for that run:

| Test target | Passed |
| --- | --- |
| Library unit tests (SHA-256 vectors, wire IDs, host fixtures, deny shape, file keys, installer, client deadlines) | 13 |
| `tests/cli.rs` (includes install dry-run, apply and remove) | 3 |
| `tests/daemon.rs` | 6 |
| `tests/hook.rs`: real daemon with the hook as a child process (conflict, duplicate delivery, changed input, expiry, restart, failure and interrupt, directly committed admission followed by a post, unavailable and unsupported) | 7 |
| `tests/mcp.rs` | 4 |
| `tests/protocol.rs` | 5 |
| `tests/store.rs` | 8 |
| `langspan-core` architecture, coordination and observation | 1, 20, 10 |
| **Total** | **77** |

`cargo audit` reported the existing `RUSTSEC-2026-0543` (`chrono` unmaintained) warning and no vulnerability.

**Mutation checks**, each restored afterwards:

- Letting an observation set its lease back to active failed two core tests.
- Replacing the JSON `deny` with `allow` failed the hook conflict test.
- Treating a conflict as no decision failed it too.
- Dropping the observation's admission link failed six hook tests.

**Real-host acceptance** with Claude Code 2.1.331, recorded in [the M2 evidence](evidence/m2-claude-code/README.md):

- a supported edit;
- a conflicting edit denied while another session waited at its permission prompt;
- unrelated work proceeding;
- the host's own prompt and permission refusal left intact;
- a prompt held past expiry, recorded as a late observation;
- Esc at the prompt, with no post event and the admission expiring;
- a daemon restart between pre and post, recorded as invalidated;
- daemon unavailable, with no decision.

Hook-path spawn-to-exit time with a warm daemon: PreToolUse 2.61 ms p50 / 3.70 ms p95 (n = 200), on the hardware named there.

#### Limits recorded at M2

These are detailed in the evidence:

- best-effort failure records only;
- a host-side permission denial holds its admission until the TTL ends;
- denials are receipts rather than events;
- event pages over 64 KiB fail;
- duplicate host delivery was not produced by the real host.

M3 implements the Codex parser and Linux process observations described above.
Byte-bounded event pages also address the M2 oversized-page failure. macOS,
cross-platform acceptance, coverage accounting and retention remain open.

<a id="next-implementation-work"></a>

### Historical next implementation work

1. Complete the current integration checks and new native-host M3 acceptance,
   retaining exact versions, failures and the linked-worktree case.
2. Run M5's product benchmark with failures kept, including stale replacement,
   permission waits, worktrees, unrelated work and observation coverage.
3. Implement bounded context delivery plus coverage and paginated current views
   for the proposed M4 product proof. Obtain the owner's concrete remote
   publication and participant choices before that data flow.
4. Add M6 versioned change/check evidence, with exact input identities and stale
   applicability; keep genuine source-base validation explicitly separate from
   semantic tool-input hashing.
5. Keep M7 code context conditional on M5's evidence. Validate the selected
   platforms and distribution/signing path, and settle the release/licence
   choices before release.

### Dependency audit caveat

The initial audit returned one unsuppressed maintenance warning for transitive
`chrono` 0.4.45 (`RUSTSEC-2026-0543`, introduced by the official MCP SDK/schema
stack), with no fixed upgrade available. No vulnerability finding was reported.
See [dependency notes](dependencies.md); do not describe this as a zero-warning
audit. It is separate from the socket-execution restriction.
