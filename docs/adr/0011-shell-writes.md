# ADR 0011: Shell writes

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026. Extends [ADR 0002](0002-claude-code-adapter.md),
[ADR 0003](0003-native-coordination-observation.md) and
[ADR 0005](0005-source-versions-and-stale-writes.md). Tested with the real daemon,
Git and hook executable (`tests/shell_writes.rs`); not yet observed with a
real Claude Code or Codex session.

## Context

Agents change files through shell commands as often as through edit tools:
`sed -i`, `cat > file`, `mv`, formatters, code generators. Until now such a
change bypassed LangSpan. Only the Git observer saw it, unattributed, and the
session that made it was refused once on its next reported edit of the file,
because its known version was out of date.

A shell command's write set cannot be known before it runs. A script, a
formatter or a loop can write anything. Claiming to gate shell writes in
general would be false.

## Decision

Two layers, each with a stated reach.

**1. Before the command: admission, only when the write set is certain.** The
command is parsed by a small, conservative grammar
([`src/shell.rs`](../../src/shell.rs)). Only when the whole command is a
simple command, or a chain joined by `;`, `&&` or `|`, of these forms:

| Form | Writes |
| --- | --- |
| `> path`, `>> path`, `2> path` on any allowed command | `path` (`/dev/null` ignored) |
| `tee [-a] path…` | each `path` |
| `sed -i[SUFFIX] script path…` (also `-i ''`, `--in-place`, `-e`) | each `path`, and `path` + suffix |
| `mv a b` | `a` and `b` |
| `cp a b` | `b` |
| `rm [-f] path…`, `touch path…`, `truncate -s N path…` | each `path` |
| `echo`, `printf`, `cat`, `true`, and `sed` without `-i` | nothing |

and every target is a literal path that resolves to a file inside the
workspace, are the targets admitted as one edit. A current holder of any of
them makes the hook answer `deny`; a stale known version gives the once-only
stale refusal of ADR 0005.

Anything else gets no decision: variables, globs, `$(…)`, backticks,
backslashes, `~`, braces, subshells, `||`, `&`, `<`, heredocs (the parse stops
at `<<`, so a body is never read as shell; this loses a peer's change when a
heredoc rewrites a file, see "Known gap" below), comments, more than one line,
assignments, `cd`, `eval`, `xargs`, loops, any other command (including
`python3 fix.py` or `./x.sh`, whose targets are unknown), unsupported options,
a `sed` script with any command other than `s`, `y`, `d` and `p` or an `s`
flag of `w` or `e`, `-f` script files, `..`, a target outside the workspace
or a directory, more than 64 targets, a command over 8 KiB. A parse failure
never denies.

Nothing is denied either when the workspace snapshot (below) is incomplete,
or while a merge, rebase, cherry-pick or revert is under way or a file is
conflicted.

**2. After the command: observation, whenever the comparison is exact.** The
pre-tool hook records a snapshot of the workspace, keyed by the tool call, in
an owner-only file under `STATE/shell/`. The post-tool hook takes another and
compares them.

- A snapshot is `git status --porcelain=v1 -z --untracked-files=all
  --ignore-submodules=all --no-renames` plus the SHA-256 of each listed file.
  Only listed files are read, so its cost follows the number of changed
  files, not the repository's size. A file Git does not list is, on disk, as
  in `HEAD`. Git's own racy-clean check compares the content of a file too
  recently modified to trust its timestamp.
- A file listed in both snapshots changed when its content changed. A file
  listed only before is compared, on disk now, with its earlier content.
  A file listed only after changed when Git says it now differs from `HEAD`
  on disk (untracked, or one status column clean). Status codes are compared
  this way, not the index file, so another program refreshing the index does
  not spoil the comparison. A clean tracked file that a command deletes shows
  as ` D`, and `mv` of one shows as ` D` plus `??`: both paths are seen.
- The changed files are recorded as the session's edit: an observation (so
  the activity record and every peer's briefing name them), and the version
  on disk as the session's known version (so it is not refused for its own
  change). An admission made before the command is completed.

Recorded nothing, with coverage `shell_unobserved`, when the comparison is not
exact: the workspace is not the top level of a Git working tree; either
snapshot hit a bound (1,024 listed files, 64 MiB read, 4 MiB per file, 1 MiB
of status output, a symlink or special file among the changed files) or the
hook deadline; `HEAD` moved during the command; the index changed for a file
in a way that leaves its content against `HEAD` unknown (`git rm --cached`,
both status columns changed); more than 64 files changed; or no snapshot from
before the command exists.

**Contested changes.** Another session's shell run that overlapped this one
in time could have written the same files. A file changed in this run is
attributed to neither session when an overlapping run of another session
changed it too, or when that run's changes are unknown (still running, or not
observed). The coverage is `contested`. A run whose result never arrives
stops counting after 10 minutes; one still open when its session's reply or
the session ends is closed then, with its changes unknown (ADR 0026). Two exceptions (added 24 November 2026,
for shared-checkout cases): a file this command names as a literal
write target (its whole write set parsed) is this command's, unless the
overlapping run names it too; and a Claude Code call with `run_in_background`
counts as still running for 10 minutes after it started, since its process
outlives the call, so it contests other sessions' changes their commands do
not name. Tests: `concurrent_commands_credit_a_change_to_the_one_that_names_the_file`,
`a_change_no_concurrent_command_names_is_credited_to_no_session`,
`a_running_background_command_contests_changes_no_command_names`. Only runs in the same physical workspace are
compared: each run record keeps the workspace's canonical root, because
changed paths are relative to it. Sessions in other repositories or linked
worktrees that share the state directory never contest each other's changes.

**Another session's hooked edits during the run** (added 25 November 2026,
issue #1). Comparing the workspace before and after a command also finds the
files another session changed through its own edit tool (Edit, Write, a
patch) while the command ran. Those were once credited to the command's
session: a later edit from that session's old read was then admitted, and
`commit --mine` refused the editor's line as written by two sessions. Now
every edit tool call that is not refused leaves an *edit mark* in
`STATE/shell/edits/`: its session, its workspace's canonical root, when it
started and finished, and each file's version just before the call (taken by
the pre-tool hook) and after it (taken by the post-tool hook). The shell
post-tool hook reads the marks of other sessions in the same workspace that
overlapped the run (finished after it started, or still going and under 10
minutes old). For each changed file one or more of those marks touched:

- If the marked edits, in the order they started, form one chain of known
  versions from the file before the run to the file now, the edits alone
  explain the change. The file is the editing session's, which its own hooks
  already recorded; the command's session is not credited with it.
- Otherwise (the file changed before or after an edit, an edit is still
  under way, or a version is not known) both may have changed it and the
  order cannot be known. The file is `contested`: attributed to no session
  by the command's observation, journaled for neither, and the command's
  session gets no version of it. The editor keeps the version its own write
  left, which is no longer the file on disk, so each session's next edit
  from what it last knew is refused once as stale (ADR 0005).

A changed file no mark touched is the command's as before. A mark whose post
never arrives is closed, with its result unknown, when its session's reply or
the session ends. Marks are pruned as run records are: a finished mark is
kept while a shell run that could overlap it is still going, and nothing is
kept for more than a day; at most 4,096 are kept. Tests:
`tests/shell_concurrent_edits.rs` (a real command held open by a signal file
while the other session's real hooks edit), and
`an_edit_that_alone_explains_a_change_is_the_editors_and_anything_else_is_unknown`.

The window between an edit's pre-tool hook and the tool's own write is not
seen: a command that writes the file inside it, before the tool reads and
replaces it, is taken as part of the edit. That window is the time the host
takes to run the tool after its hook returns. The window after the tool is
closed: the post-tool hook reads the edit's files on entry, before any
daemon call, and the mark keeps those versions. A command's write that lands
while the hook waits on the daemon is then a change the edit does not
explain, so the file is `contested` rather than the editor's. Test:
`a_shell_write_during_an_edits_post_hook_is_not_taken_for_the_edits`.

**Recorded once, even when the daemon fails.** The post-tool hook first keeps
the comparison's outcome (the changed and contested paths, or why the change
set is unknown) in the run record, then records it with the daemon and
completes the admission. If the daemon cannot be reached, the hook says so
and the outcome stays kept; a redelivered post-tool hook for the same call
records that outcome rather than comparing again. Each daemon step has a
request ID derived from the tool call, so a retry never records twice. Only a
recorded outcome makes a later delivery answer "already handled". Tested with
a daemon that stops answering and resumes (`tests/shell_writes.rs`). A daemon
that restarts in between has already invalidated the admission and marks the
session stale until its next tool call registers it again; a peer's briefing
may not name a change recorded in that window. Since
[ADR 0014](0014-observation-spool.md) the next tool-call hook run of any
session sends a kept outcome too, without waiting for a redelivery.

**Hosts.**

- Claude Code: `Bash` in PreToolUse, PostToolUse and PostToolUseFailure. A
  failed command's changes are still changes. A subagent's `Bash` call is
  the subagent's own session, as its edits are
  ([ADR 0004](0004-automatic-peer-briefing.md)): a file its parent holds is
  held from it, and what its command changed is its own version. Test:
  `a_subagents_shell_write_is_its_own_sessions_not_its_parents`.
- Codex: `Bash` in PreToolUse and PostToolUse. Verified from the Codex source
  at tag `rust-v0.171.0` (the installed CLI's version), not from a captured
  event: `exec_command` emits `tool_name: "Bash"` with
  `tool_input: {"command": …}`
  (`codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs`), and the
  PostToolUse for it is sent only once the process has exited, under the
  originating tool-use ID, including when a later `write_stdin` poll observes
  the exit (`unified_exec.rs`, `write_stdin.rs`, `tools/context.rs`).
  `write_stdin` itself emits no PreToolUse.

Coverage values: `admitted`, `denied`, `denied_stale` (before, literal forms);
`shell_observing` (before, no decision, compared after); `shell_observed`,
`shell_unchanged`, `contested` (after); `shell_unobserved` (either).

## Never covered

- Writes outside the workspace, in ignored files, inside nested repositories
  or submodules.
- Writes by a process that outlives the tool call: a background job, a
  daemon, Claude Code's `run_in_background`. Only what changed by the time the
  post hook ran is attributed to it. A `run_in_background` call does contest
  other sessions' unnamed changes for 10 minutes after it started (see
  "Contested changes"); a job a command backgrounds itself (`&`, `nohup`) is
  not known to be running and contests nothing.
- Changes made by anything outside the hooks while the command ran (an
  editor, the owner, an unhooked tool) are attributed to the command's
  session. Another session's overlapping shell run and its hooked edit tool
  calls are detected (see "Contested changes" and "Another session's hooked
  edits during the run").
- A change of ignore rules during the command can make a previously ignored,
  unchanged file appear new.
- Files over the bounds, non-Git directories, a workspace root below its
  working tree's top level.
- A write before the command that the parser could not name is not
  prevented, only observed after.
- BSD `sed` (macOS) reads `sed -i 's/a/b/' f` as a backup suffix of
  `s/a/b/`; the parser reads the GNU form, so it may admit `f` for a command
  that then fails. It never admits fewer files than either form writes,
  except for a BSD backup file named by a non-empty separate suffix.

## Consequences

- Shell coverage is partial and the docs, `doctor` and the coverage values
  say so. It is not a sandbox and it fences nothing: like every admission it
  is cooperative.
- Every `Bash` call now runs the hook twice, with three short Git processes
  each time, inside the existing 1.5 s budget. The pre-tool snapshot may use
  two thirds of the remaining time so a decision is still possible.
- `STATE/shell/` holds one small file per running or recently finished
  command. A finished run is kept while a running run overlaps it, a running
  one for at most a day; at most 4,096 are kept. `STATE/shell/edits/` holds
  one small owner-only file per recent edit tool call, pruned the same way,
  so each edit tool call's hooks write one more file.
- The parser is a second grammar to keep conservative. A form is added only
  with a test that names its targets and the disqualifiers that go with it.

## Design input adopted

Cases and techniques adopted from a design study of shell-write handling:
snapshotting only dirty files with `HEAD` as the "before" of every clean
file; seeing deletions and renames of clean files; content comparison for
recently modified files (Git's racy-clean check provides it); a `contested`
outcome for overlapping runs; heredoc bodies never parsed as shell; no
denial during a merge, rebase or conflict; no decision for a command that
only runs a script. Kept from LangSpan's own design: an explicit
admit/deny for literal forms, including new files, and an honest
`shell_unobserved` instead of failing open silently.

## Known gap: a heredoc that rewrites a file

Recorded 25 November 2026, after the launch validation and the second
capability review.

**What happens.** A session that read a file earlier, and rewrites it whole
with `cat > path <<'EOF' … EOF`, is not refused, and a change another
session made since that read is lost. The parser stops at `<<`
(`src/shell.rs:151-153`: "input redirection, a heredoc or process
substitution"), so the hook gives no decision and the command is compared
after it runs. The comparison then records the whole file as this session's
change; nothing compares it with the version the session read. A whole-file
`Write` of the same content from the same old read is refused once as stale
(ADR 0005), and `sed -i` over the other session's lines is held; of the
bench's rewrite scenarios, the heredoc is the one that gets past both.

**How it was found.** The scenario bench of the launch validation (at
`9eaf3a8`, host-shaped events fed to the installed binary, not a live host)
failed scenario 5s: "admitted, B's change lost". A parser can set a heredoc's
body aside and still read the command's redirect, so that
`cat > notes.md <<'EOF'` names `notes.md` as the write target; that is the
fix below.

**Decision: a planned fix, not started.** The target of `cat > path` with a
heredoc is literal, so this is not the general case of B5 that
[ADR 0024](0024-write-check-and-landing-depth-beyond-the-launch-bar.md)
leaves for later. The fix adds one form to the grammar: `cat`, a literal
`>` target, and one heredoc whose body is skipped up to a line that is
exactly its delimiter, with nothing after that line. The target is then
admitted like any `> path`: a current holder gives `deny`, and a stale known
version gives the once-only stale refusal of ADR 0005. As with every form
added here, it comes with a test naming its target and its disqualifiers
(a second heredoc, an unquoted delimiter whose body holds `$` or a
backtick, since the shell would run what they expand, and text after the
closing line), and the bench's scenario 5s is re-run. Until it merges, the
README's shell-command row and the [roadmap](../roadmap.md)'s row B5 name this gap.

## Known limit: BSD `sed -i` on macOS

The parser reads `sed -i` in its GNU form, where `-i` takes no argument. BSD
`sed` on macOS reads the next word as a backup suffix, so `sed -i 's/a/b/' f`
there treats the script as the suffix and fails or edits the wrong thing.
LangSpan may then admit `f` for a command that does not change it. The admission
expires on its own and the after-the-fact comparison records nothing, so no
wrong attribution follows. The fix is to read `sed -i` per platform; it is not
done.

## In combination with region admission

A shell target is always held whole-file (ADR 0012): a command says nothing
about which part of a file it changes.
