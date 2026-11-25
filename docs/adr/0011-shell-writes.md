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
stops counting after the supported lifetime of a shell call (below); one still open when its session's reply or
the session ends is closed then, with its changes unknown (ADR 0026). Two exceptions (added 24 November 2026,
for shared-checkout cases): a file this command names as a literal
write target (its whole write set parsed) is this command's, unless the
overlapping run names it too; and a Claude Code call with `run_in_background`
counts as still running for that lifetime after it started, since its process
outlives the call, so it contests other sessions' changes their commands do
not name.

**A shell call's supported lifetime** (changed 25 November 2026, PR #6
review): two hours (`RUN_LIFETIME_MS`), the longest Claude Code lets a
`run_in_background` command run; a foreground call stops at ten minutes.
It was ten minutes, so a longer run's still-open record was taken as
abandoned: another session's next edit then pruned the finished edit marks
the run still needed, and the run's result credited that session's file to
the command's session. Until the lifetime has passed an unfinished run
counts as running, and the run records and marks it needs are kept. A run
whose result arrives after it (a Codex process kept alive past two hours,
for example) may have lost those marks, so all it changed is `contested`:
credited to no session. Tests:
`a_command_running_longer_than_ten_minutes_keeps_the_edit_marks_it_needs`,
`a_command_older_than_a_shell_calls_lifetime_is_credited_with_nothing`. Tests: `concurrent_commands_credit_a_change_to_the_one_that_names_the_file`,
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
overlapped the run (finished after it started, or still going and younger
than a shell call's supported lifetime). For each changed file one or more of those marks touched:

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

**A rewrite of another session's uncommitted lines** (added 25 November
2026, issue #98). A command that rewrites files across the workspace, such as
a formatter (`cargo fmt --all`), changes lines other sessions wrote and have
not committed, whether they wrote them before the command or during it. Each
changed file is classified on its own, so a contested file never makes the
command's other files contested: a file the command's session alone wrote,
reformatted by its own command, stays that session's line by line, and its
`commit --mine` can commit it. A file that no overlapping edit or run
contests but in which the command removed a line that another session's
journaled write added (or may have added, in lines that are not known), and
that the file gained since `HEAD`, is `contested`: that line was changed by
both sessions, so neither is credited with it, and the command's session
does not commit the other session's work. A file where this cannot be told
(its content before the command, now or in `HEAD` is not known) is
`contested` when another session wrote it. A command that only adds lines to
another session's file, or rewrites committed lines, is credited as before.
Tests: the `a_formatter_*` and
`a_command_adding_to_another_sessions_file_without_rewriting_its_lines_is_credited`
tests in `tests/shell_concurrent_edits.rs` (a real `sed` rewrite of two files
between two sessions' real hooks, followed by `commit --mine`). An
overlapping shell run of another session whose changes are unknown still
contests every file the command changed, as above: its writes cannot be told
from the command's, with the exceptions below.

**A reformat keeps each line's author** (added 25 November 2026, issue
#141; it widens #124's rule below from the command's own lines to every
session's). A formatter run in a shared checkout used to make
`commit --mine` refuse for every session: overlapping another session's
command, or rewriting another session's uncommitted lines, made each file it
touched no session's, new files one session alone had written included. Now,
in a file contested only because another session's shell run overlapped, or
only because the command rewrote another session's uncommitted lines (no
overlapping edit contests it, in either case), each run of changed lines is
split into groups that keep their tokens, as #124 describes. Each line a
group leaves keeps the author of the lines it removed: a line of the file
before the command is a session's when every copy of it there was gained
since `HEAD` and the journaled writes of that one session (a subagent's
count as its parent's) added it; it is committed when no copy was. A group
of one session's lines is that session's, whoever ran the command; of
committed lines, the command's session's when nothing overlapped and no
session's when something did; of several sessions' lines, or of a line not
known to be one session's, no session's. Lines without tokens (blank lines)
join the group next to them and do not decide it, and changes the line
comparison separated only by blank lines are compared as one. The rest of a
change, which alters tokens, keeps today's rules: the command's session's
when nothing overlapped and every line it removes is committed or that
session's own (#98), otherwise no session's (#124). The journal keeps a
partial entry per credited session with just its lines; when every line is
the command's session's, the file is attributed to it as usual. A file
whose lines all go to another session stays `contested` for the command's
session: it is not credited with it and gets no version of it, so the
other session's next edit is refused once as stale, naming the command
(#107). Two overlapping runs that both saw the change credit it once: an
entry with the same version and lines as one the session was given since
the command started is not written again. An earlier entry with the same
lines is an earlier write (one a revert may have undone since), so it does
not count.

Tokens depend on the file's extension. Rust, TypeScript and JavaScript
(`.rs`, `.ts`, `.tsx`, `.mts`, `.cts`, `.js`, `.jsx`, `.mjs`, `.cjs`): words
and single punctuation bytes, with a comma just before a closing `)`, `]`
or `}` dropped, since `rustfmt` and Prettier add or remove it when they wrap
a list (so `(x,)` and `(x)` compare equal). Markdown (`.md`, `.markdown`,
`.mdx`): words and punctuation, commas kept, so a re-wrapped paragraph keeps
its author. Any other file: its text with all whitespace removed, so words
that come to touch, or a change of indentation that matters (YAML, Python),
are taken as formatting. Whitespace inside a string literal is taken as
formatting in every language.

Cost and bound: the comparison runs only for the files above, each at most
512 KiB (the journal's version bound), at most 64 per command, within the
hook's deadline. Per file it reuses the line comparison of ADR 0028
(Myers, at most 256 edits, 4 million line comparisons and 64 MiB of compared
bytes; past that the differing middle is one change), tokenizes each changed
line once and then examines at most 4,000,000 tokens (`MAX_WORK` in
[`src/reformat.rs`](../../src/reformat.rs)). Past that, or past the
journal's 8,192 lines per side of one entry, nothing in the file is
credited by this rule and it stays `contested`. Measured in a debug test
build on an arm64 macOS machine, one run, toolchain 1.100.0: 8,000 lines
(170 KiB), every one re-indented, took 149 ms
(`a_large_reformat_is_compared_within_the_bound`); a release build is
faster.

Tests (two sessions' real hooks, a real daemon, a real Git repository, a
real `cargo fmt --all` running `rustfmt`, and `commit --mine` for each
session):
`cargo_fmt_during_another_sessions_command_keeps_each_sessions_lines_its_own`,
`a_new_file_another_sessions_formatter_reformats_stays_its_writers` and
`a_token_change_during_another_sessions_command_is_credited_to_no_session`
in `tests/shell_concurrent_edits.rs`, and the unit tests of
`src/reformat.rs` (Rust, Markdown and other files; trailing commas; several
sessions' lines in one group; the work bound).

**A reformat of the session's own lines during another session's command**
(added 25 November 2026, issue #124; widened by #141 above). A formatter keeps every token of the
lines it rewrites and changes only whitespace and line breaks. In a file
contested only because another session's shell run overlapped (no
overlapping edit contests it, and the command rewrote no other session's
lines), the command's change is compared line by line with the file before
it. Each run of changed lines is split into groups whose removed and added
lines have the same tokens, in order (a token is a run of letters, digits,
`_` and non-ASCII bytes, or one other non-whitespace byte); a joined or split
line is one group. A group is the command's session's when every line it
removes is one of that session's own uncommitted lines: a line the file
gained since `HEAD` that its journaled writes added and no other session's
did, counted per copy. Where tokens differ, one removed and one added line are
left out and grouping resumes after them. When every changed line is in such
a group, the file is attributed to the session as usual. When only some are,
the file stays `contested` (the reply says how many are partly the
session's) and the journal keeps a partial entry with just those lines: its
lines count for `commit --mine` and the commit guard, but the version it left
is not the session's alone, so it never makes a whole-file attribution the
session's. A line left out is credited to no session, so a hunk holding it is
refused; so is a credited line whose content the uncredited part also adds
or removes, since the journal matches lines by content. Nothing is credited
when the file has a write whose lines are not known, is not UTF-8, or its
content before the command, now or in `HEAD` is not known; nor on the paths
that contest every file (past a call's lifetime, an edit mark overflow).
A change that keeps tokens but changes whitespace inside a string literal is
taken as a reformat; another session's command that only did that to the
session's own line is indistinguishable from the formatter. Tests:
`a_formatter_reformatting_only_its_sessions_lines_during_another_sessions_command_is_credited`,
`a_line_the_other_sessions_command_also_changed_is_credited_to_no_session` and
`a_formatter_during_another_sessions_command_still_takes_none_of_its_lines`
in `tests/shell_concurrent_edits.rs` (two sessions' real hooks, a real
command held open by a signal file, a real `sed` rewrite and `commit
--mine`), and the unit tests of `src/reformat.rs`.

**A notice before a formatter reaches another session's files** (added 25
November 2026, issue #126). Crediting a rewrite to no one keeps attribution
right, but the other session's work still changes under it and its next edit
of the file is refused as stale. Before a shell command, the pre-tool hook
reads which recognised formatters it runs (`src/shell/formatters.rs`, a
lenient reader built on the commit guard's lexer) and how far each reaches:

- `cargo fmt` in any form: every `.rs` file of the workspace. Files given
  after `--` are added to its targets, not put in their place (observed with
  rustfmt 1.11.0: `cargo fmt -- src/a.rs` also reformatted the crate root).
- `rustfmt FILE...`: those files and the modules they declare (the directory
  beside a crate root or `mod.rs`, otherwise the one named after the file).
- `prettier --write PATH...` (also through `npx`, `pnpm exec`, `yarn`, a
  path, or `prettier@N`): the files named, and every file of the languages
  Prettier formats without a plugin under a directory or a glob's literal
  directory.
- A check (`--check`), a run that prints (`rustfmt --emit stdout`, no file),
  or a Prettier run without `--write` reaches nothing. An operand that is an
  expansion, or a `cd` the reader cannot follow, reaches the whole workspace.

A file is named when another session's journaled write
([ADR 0026](0026-commit-guard-and-commit-mine.md)) changed it, it still
differs from `HEAD` in the command's own snapshot, it is in the reach, that
session is live (registered, not ended and not known to have exited, the
commit guard's rule), and its journaled lines are still among the file's
changes from `HEAD`. The journal keeps a write for a week whatever commits
it, and only `commit --mine` forgets one, so a write committed through plain
Git would otherwise be named for as long as anyone else's change kept the
file dirty, and spend the once-only notice. A line the other session added
(or removed) counts while the file adds (or removes) more copies of it than
the formatting session's own writes account for, so a `}` both wrote is not
enough. A write of unknown lines, or a file whose versions cannot be read
within the journal's bound and the hook's budget, still names the session:
the notice errs toward telling. The reply carries the notice as
`hookSpecificOutput.additionalContext` with no permission decision, so the
command runs as it would without LangSpan. It names each session and at most
eight files in all, then counts the rest; paths are cleaned and the list is
capped like a refusal's. It is given once per session for each set of
session and file pairs (the commit guard's "told" record): the same files are
not named again whichever formatter runs, and a new file in the reach makes a
new notice. Only hosts that show pre-tool context to the model get it
(Claude Code and Codex); for Cursor and Gemini CLI nothing is computed, so a
notice is not spent unread. Claude Code shows the context next to the tool
result ([ADR 0029](0029-break-alerts-at-write-time.md)), so the model reads it
after the formatter ran: it says whom to tell and how to format only one's
own files next time. The skill gives the same advice beforehand. A failure
to reach the daemon for liveness gives no notice and says so in the hook's
detail. Other formatters (`gofmt`, `black`, `ruff format`) and a formatter run
by a script or `npm run` are not recognised. Tests: `tests/formatter_notice.rs`
(two sessions' real hooks, a real daemon and Git repository), the unit tests
of `src/shell/formatters.rs` and `src/formatter.rs`, and
`the_formatter_commands_the_skill_names_are_read_as_it_says`.

The cap holds on every path that creates a mark: an edit's pre-tool hook, and
its post-tool hook when the start was not kept. An edit call refused a mark at
the cap goes unmarked and leaves an overflow note (`STATE/shell/edits/overflow`,
the time it happened). A shell run then cannot vouch for any file it changed:
if the note is younger than a shell call's supported lifetime before the run
started, or the directory holds more marks than a listing reads (as an older
build could leave), every changed file is `contested` and the run's session
is credited with nothing. Tests:
`a_full_edit_mark_directory_stays_bounded_and_credits_the_command_nothing` and
`overflowing_edit_marks_make_a_runs_overlapping_edits_incomplete`.

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
  other sessions' unnamed changes for two hours after it started (see
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
- `STATE/shell-changes/` holds, per workspace and file, a change note: the
  last shell command known to have changed the file, its session, its command
  line (redacted and capped at 120 bytes, as the run record now keeps it) and
  the version it left (issue #107). A file the command is credited with, or
  one it is withheld from only because it rewrote another session's
  uncommitted lines, is noted; a file another session's overlapping run or
  edit could have written is not. When another session's next edit of the
  file is refused as stale and the version on disk is still the one the note
  records, the refusal says "changed by a shell command of `<host>` session
  `<id>`: `<command>`" instead of "changed outside any reported edit"; a
  later reported edit by a different session is named instead. Notes expire
  after a week, at most 256 are written per run and 4,096 kept per workspace.
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
