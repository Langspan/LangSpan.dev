# ADR 0026: Commit guard and `commit --mine`

**Status:** Implementation decision, 24 November 2026. Extends
[ADR 0011](0011-shell-writes.md) (the Bash hook and its parser) and builds on
the attribution of edits to sessions in ADR 0002, ADR 0004 and ADR 0011.
Roadmap rows B6 and B7, the shared-index guard and `commit --mine`.
Tested with the real daemon, Git, hook executable, CLI and MCP adapter
(`tests/commit_guard.rs`), with each host's own edit and shell events for
Claude Code, Codex, Cursor and Gemini CLI (issue #26). A real Codex session
ran `commit --mine` from its shell
([Codex real-host evidence](../evidence/codex-real-host/README.md), section
6); the Claude Code run is not yet retained as evidence, and Cursor and
Gemini CLI have not run it on a real host.

## Context

Sessions that share one checkout share one Git index, and Git commits whole
files. One session's `git commit -a`, `git add -A` (or `.`, `-u`) followed by
a commit, or `git commit PATH…` takes every uncommitted change in what it
names, including another live session's half-finished edits, and ships them
under the wrong commit. LangSpan already knows which session wrote which file
(each completed edit and each observed shell change is attributed), but only
by file, and a commit can take part of a file.

## Decision

**A line journal, on this machine only.** At the points where an edit is
already attributed, the hook also records, under `STATE/journal/`, the file,
the session, and the keys (the first 64 bits of SHA-256, terminator
included) of the lines the write added and removed, counted as multisets
against the file as it was just before the write:

- an edit tool (`Edit`, `Write`, `MultiEdit`, `NotebookEdit`, `apply_patch`):
  the pre-tool hook keeps the files it names; the post-tool hook of a
  successful call compares;
- a `Bash` command: the version its pre-tool snapshot recorded (its content
  kept at that moment when it was not kept already, at most 4 MiB per
  command), or, for a file Git did not list, its `HEAD` content. Contested
  files (ADR 0011) are not journaled, except for the lines of one that the
  command only reformatted (issues #124 and #141): a partial entry per
  session whose lines were reformatted keeps just those lines, under that
  session, and its version is not taken as the session's for a file
  attributed whole. A shell write is not journaled twice for one session:
  an entry with the same resulting version and the same lines as one written
  since the command started, which a second overlapping run that saw the
  same change would write, is skipped.

When the "before" is not known exactly, or a write changes more than 8,192
lines on one side, the entry says the session changed the file in lines that
are not known. File versions up to 512 KiB are kept by digest so that the
next write can start from them. Entries expire after a week, at most 4,096
per workspace; `commit --mine` removes the committing session's entries for
the files it committed.

**Whose a change is.** The changes a command would take are read with
`git diff -U0 --no-renames` (and `git ls-files --others` for new files) and
split into hunks. A line belongs to the sessions whose entries for that file
added it (or, for a removed line, removed it); a session's subagents (IDs
`PARENT.AGENT`) count as the session itself. A hunk is:

- this session's, when each of its lines is this session's and not every one
  is also another's;
- another session's, when none of its lines is this session's and some are
  that session's;
- nobody's, when no session wrote any of it;
- not known otherwise: part this session's and part another's or nobody's,
  each line also another's, or a nobody's hunk in a file this session changed
  in unknown lines.

**A hunk split between sessions (issue #106).** Git joins changes on adjacent
lines into one hunk, so two sessions adding entries one below the other to a
list made one hunk, part each one's, and neither could commit. Before
attribution, a hunk whose every line some session wrote, and that holds both
this session's lines and other sessions' lines, is split where they meet, as
`git add -p` lets a person split a hunk, and each piece is attributed as a
hunk. A zero-context hunk lists all its removed lines before all its added
ones, so where an added line goes among the other session's removed lines is
not in the diff. A piece takes a whole run of one side's removed lines, a
whole run of the same side's added lines, or both; pieces alternate between
the sides. Of every order of pieces the hunk allows, LangSpan takes those
where the most pieces both remove and add (an added run takes the place of a
removed run of its side), searching them all. The hunk is split only when
exactly one order is likeliest. Otherwise it is not known, with the reason
that the sessions' lines could be in more than one order: for example, when
one session changed "A B C" to "B X" and the other then removed B, X could
have taken A's place or C's, so it could go above B or below it, and a split
that guessed would commit X on the wrong side of the other session's
uncommitted B. The same holds when one session removed a line where the
other added its own, or when an added run of one side could take the place
of its side's one removed run from either side of the other's lines. The
search's table is bounded (65,536 pairs of removed and added runs); a hunk
past it is not split. The hunk is also not split, and is not known with the
reason that one session's line replaced another's, when the journal shows a
line one of them added that the other removed: the replaced line is no
longer in the diff, and the replacing line makes sense only on top of it. Lines are known by text, not place, so that last check
holds for the whole file, and a common line (a blank line, a lone brace) one
session added and the other removed elsewhere keeps the file's mixed hunks
unsplit; that errs toward refusing. A hunk with a line no session wrote is not
split: it stays not known, with the reason it had before. A file that can be
committed only whole (new, removed, binary) is not split either.

A file that cannot be told apart by line (binary, over 512 KiB, a mode change,
or one this session changed in a write whose lines are not known) is
attributed whole: to the session whose journaled write left it as it is now,
and not known when another session also changed it. A subagent's changes
count as live while it or the session that started it is live.

Another session's write whose lines are not known (an edit whose pre-tool
hook did not run, a shell command whose starting version was not kept) does
not make the file whole. Until issue #99 it did, and the file went to its
latest writer: `commit --mine` reported a file as wholly another session's
and left it, although some hunks in it were this session's. Now its hunks are
attributed one by one. A hunk not wholly this session's may be that other
session's, so it is that session's (or not known, if it is partly this
session's); it is never this session's. A hunk wholly this session's stays so
only while, for each of its lines, this session's writes added (or removed)
at least as many copies as the whole file's diff holds; otherwise the other
session's unknown write may hold one of them, and the hunk is not known, with
that reason. Line numbers play no part in attribution: lines are matched by
content, so edits that shift each other's lines do not change whose they are.

Two refinements followed review. The copies are counted in the file's whole
diff from `HEAD` to the working tree (or the diff the command takes, if that
holds more), never in the staged part alone: a plain `git commit` of only the
other session's copy is otherwise counted as one copy, matched by this
session's one write, and taken without the guard holding it. And journal
entries survive a commit made through ordinary Git (only `commit --mine`
forgets them), so a copy this session already committed would still be
counted as its own. Each move of `HEAD` since this session's first write of
the file, from `HEAD`'s reflog, is compared with the one before it; a line
this session's writes added (or removed) that such a move also added (or
removed) may already be committed, and a hunk holding one is not known, with
that reason. The reflog's times are this machine's clock, as the journal's
are, in whole seconds, so a move in the same second as the write counts as
after it. When the reflog is missing, longer than 256 moves since the write,
or a version cannot be read in time, the hunk is not known. The move to the
commit `commit --mine` has just made is its own and does not count.

**1. The guard, before the command.** In the `Bash` pre-tool hook, before the
shell snapshot, a lenient reader ([`src/shell/git_commands.rs`](../../src/shell/git_commands.rs))
finds the Git steps of the command: `git add` with pathspecs, `-A`, `--all`,
`.`, `-u`; `git commit` plain (what is staged), with `-a`/`--all`, with
pathspecs (also `-i`). Quotes, escapes, `$(…)` and heredoc bodies are skipped,
so the usual `git commit -m "$(cat <<'EOF' … EOF)"` is read; `cd DIR` and
`git -C DIR` move the directory. Dry runs and interactive staging are not
steps. Anything it cannot be sure of gives no decision: an expansion or
unquoted glob in a pathspec or directory, `cd` to an unknown place,
`--pathspec-from-file`, `--git-dir` or `--work-tree`, a command over 64 KiB.

With Git steps found, the hook asks the coordinator which sessions of the
workspace are live (the recap: registered, not ended, not known to have
exited), runs the same Git listings the command's own Git would see (from its
directory and with its pathspecs: staged, unstaged and untracked as the step
takes them), and attributes each hunk. When some hunk or whole file is
another live session's, or holds lines only another live session wrote (a
hunk mixing its lines with this session's is held too), the hook answers `deny` (coverage `denied_commit`)
naming each such session with its files and lines, and the options:
`langspan commit --mine` (with the exact command line, including the session
ID), naming only paths of its own, or a message to that session. It says to
leave the other session's changes as they are and that the same command, run
again, is not held: each command with the same foreign changes is held once
per session, so the agent (or the owner through it) keeps the capability to
commit them together. Changes that are nobody's, this session's, or an ended
session's never hold a command. No decision, never a denial, when the
journal has no other session's entries, the coordinator cannot be reached,
or a Git listing fails or exceeds its bounds (16 MiB of diff, 1,024
untracked files); the hook detail says which.

**2. `langspan commit --mine -m MSG` and `langspan_commit_mine`.** Both run
`commit::commit_mine`:

1. Refuse during a merge, rebase, cherry-pick or revert, and before the
   first commit.
2. Diff `HEAD` against the working tree, add untracked files, attribute.
3. Any hunk whose owner is not known: refuse (exit 1, MCP `refused`) naming
   path, lines and reason; nothing is staged or committed.
4. Build the commit in a private index (`GIT_INDEX_FILE` under the Git
   directory, removed afterwards): `git read-tree` of the `HEAD` read in step
   2, then this session's hunks with `git apply --cached --unidiff-zero` (a
   whole text file is the full patch; a binary, mode-only or untracked file
   of its own is added with `git update-index`). A new or removed file that
   also holds others' changes is refused: it can only be committed whole.
   Chosen pieces of a split hunk that touch are made one hunk of the patch.
   Before the patch is applied, LangSpan checks each split hunk's pieces
   again, from the pieces themselves: the session takes every piece of its
   side and none of the other's, and the pieces are the one likeliest order
   of the two sides' lines. A patch that would move this session's lines
   across another session's uncommitted lines is never staged, even if it
   would apply. It then applies each file's hunks itself to
   the version in that index, each where its header says and byte for byte,
   with none of the searching for an offset `git apply` may do; after, the
   index must hold exactly that result. So the version committed is the
   index's with exactly this session's changes, each in its one place. If a
   check fails, nothing is committed, and the error names the file and says
   the patch does not keep this session's lines in place among another
   session's, or does not apply exactly. The same checks guard step 7 on the
   shared index.
5. Commit atomically, with plumbing, keeping the owner's hooks. The hooks
   run as `git commit -m` runs them, with `GIT_INDEX_FILE` naming the private
   index: `pre-commit`, `prepare-commit-msg FILE message`, `commit-msg FILE`;
   any refusal stops everything, nothing committed. They run through
   `git hook run` where Git has it (2.36 and later), otherwise from
   `core.hooksPath` or the Git directory's `hooks`. Then `git write-tree`,
   `git commit-tree -p HEAD-READ` with the message as `git commit -m` cleans
   it (and `-S` when `commit.gpgSign` is set), the owner's identity from Git's
   own configuration, and `git update-ref -m "langspan commit --mine" REF NEW
   HEAD-READ` on the branch `HEAD` names (or `HEAD` itself when detached): a
   compare-and-swap that fails if anything moved the branch since step 2.
   `post-commit` runs after, its result ignored, as `git commit` does.
6. If the branch moved, nothing was changed: plan again from the new `HEAD`
   (this session's changes as they now differ from it) and try once more. If
   they no longer apply cleanly on top of it (a hunk becomes not known), the
   command refuses with nothing committed and says `HEAD` moved; if the branch
   moves again, it stops with nothing committed. Another session's commit is
   never undone.
7. Bring the shared index up to date with the commit: for each committed
   file, this session's hunks of the shared index's own diff against the
   working tree are applied to it, so whatever else was staged there stays
   staged and nothing shows as staged undone. A failure here is reported
   with the commit, not hidden.

The CLI takes `--session ID`; without it, the session is the one whose shell
call is running the command now: the pre-tool hook marks the run record of a
command that runs `langspan commit --mine`, and exactly one such running
record in the workspace names the session (zero or several is an error
asking for `--session`). `--root DIR` defaults to the current directory.
A call the host refuses after the pre-tool hook (its own permission check)
sends no post event, so its record would stay "running" for a shell call's
supported lifetime in ADR 0011 (two hours; ten minutes before PR #6). The end of the session's reply (Stop, StopFailure,
SubagentStop) and the end of the session (SessionEnd) close that session's
open records, finished with their changes unknown, so an ended session is
never counted (added 25 November 2026 from the launch validation; test
`a_refused_commit_mine_call_stops_counting_once_its_reply_or_session_ends`).
A session that ends without either event still counts for those two hours.
When no running record is found, the error names the state directory
looked in; if a LangSpan project hook in the checkout (up to the directory
holding `.git`) names a different `--state-dir`, the error names it and the
command to run with it. That path is read from a repository file, so it is
only suggested, quoted, never used, and dropped if it holds a control
character (test
`commit_mine_in_the_wrong_state_directory_names_it_and_the_installed_one`).
A retry is safe without a receipt: what was committed is no longer
uncommitted.

Every host's shell tool reaches that marking through the same shell
operation (`hook::shell_operation`): Claude Code `Bash`, Codex `Bash` (its
`exec_command`), Cursor `Shell`, Gemini CLI `run_shell_command` and
Antigravity's shell tool; the tests cover the first four. Their MCP registrations call the same
`langspan_commit_mine`, whose result holds the CLI's `--json` fields plus
`exit_code` and `request_id`. When anything is left or refused, the text
output ends by pointing to `langspan status --mine`, which lists the lines
still this session's (issue #26). Tests in `tests/commit_guard.rs`:
`a_codex_shell_commit_mine_commits_only_its_hunk_of_a_file_both_changed`,
`a_gemini_shell_commit_mine_without_a_session_finds_its_own`,
`a_cursor_mcp_commit_mine_returns_the_cli_json_fields`,
`each_failing_commit_hook_is_named_and_nothing_is_committed` and
`commit_mine_text_lists_what_was_left_and_points_to_status_mine`.

A formatter run over a file that another live session also changed, its
lines uncommitted (a contested file, ADR 0011), reflows each session's lines
in place. Through the partial entries for reformatted lines above, each
reformatted line stays the session's that wrote it: `commit --mine` takes
the runner's reformatted hunk and leaves the other's, which that session's
own `commit --mine` then takes (test
`a_formatter_reflowing_both_sessions_lines_in_one_file_commits_only_the_runners`).
A reflow that joins one session's line with another's makes a line neither
wrote; its hunk is not known, and `commit --mine` refuses with nothing
committed (test
`a_formatter_joining_two_sessions_lines_into_one_is_refused_not_guessed`).

A failed Git step, in `commit --mine` or in the guard's own reads, is
reported with the step, its exit status and Git's standard error as
`git::said` gives it (the last six lines, at most 1 KiB, control characters
removed, redacted), labelled `Git said:`. When Git or LangSpan could not
write (`Read-only file system`, `Permission denied`, `Operation not
permitted`), the error adds the usual cause, a Git directory that is
read-only inside an agent's workspace sandbox, and the fix: run the command
outside the sandbox or make the Git directory writable (added from issue #4;
tests `a_read_only_git_directory_is_named_with_gits_own_words_and_the_fix`
and `a_failed_git_read_in_the_commit_guard_says_what_git_said`).

**3. `langspan status --mine` and `langspan_my_changes`** (roadmap B27,
issue #40, added after launch). Both run `commit::status_mine`: steps 1 and 2
of `commit --mine` without the merge check, the same `file_owners`
attribution, and nothing after. No private index is built, nothing is staged
or committed, and every Git read runs with `--no-optional-locks`, so the
shared index file is not rewritten either (test
`status_mine_lists_this_sessions_files_and_hunks_and_changes_nothing`
compares the index bytes, `git status` and every file before and after).
Each changed file that holds a hunk this session owns, or one whose owner is
not known, is listed by path: `mine` when `commit --mine` would commit all
of it, `mixed` when only its own hunks, `unknown` when it would refuse (a hunk
not known, or a new or removed file that also holds others' changes). Each
hunk carries its first line and line count (in the working file, or where it
was in `HEAD`'s when it only removes lines) and its owner: `mine`, `others`
with the sessions, `none` or `unknown` with the reason. A file attributed
whole has no hunks and `whole: true`. `staged` says the shared index holds
some of the file's changes. `others` counts the changed files holding other
sessions' lines, including lines in a hunk whose owner is `unknown`; a file with nothing of this session's is not listed. At most
200 files are listed, then `truncated`. The session is found as for
`commit --mine`: the pre-tool hook marks a running `status --mine` command's
record as it marks `commit --mine`'s, and the same missing-record error names
the state directory. Other worktrees and machines are out of scope. The
Claude Code run calling it before `commit --mine` is still to be retained.

## Never covered

- The hook fallback for Git before 2.36 (running files from the hooks
  directory) is tested with a `git` on `PATH` that answers `git hook` as
  Git before 2.36 does and passes every other command to the installed Git
  (test `without_git_hook_run_the_owners_hooks_run_from_the_hooks_directory`),
  not with a real Git before 2.36. Hooks run twice when the branch moves
  during the first attempt.
- Line keys are content only, per file, kept for a week. After a commit
  LangSpan did not make, an old claim stays: a later identical line written
  by someone else in that file counts as the earlier writer's too, so it can
  make a hunk not known, or, when that writer runs `commit --mine`, be
  committed as its own.
- A change of a file's mode is recorded by no write, so it is nobody's:
  `commit --mine` leaves it out (and lists the file as left) even when every
  line change in the file is the session's. A binary file whose mode changed
  is left whole.
- Clean filters and `core.autocrlf` change what `git diff` shows from the
  bytes the journal hashed: such files' lines do not match and are nobody's.
- A write of more than 64 files at once (a checkout, generated files) is not
  journaled. Entries are capped per workspace, not per session.
- Line ownership is by content. Two sessions adding the same line to one file
  make it each one's; a hunk made only of such lines is not known and is
  refused. Inside a hunk that holds one session's lines and another's, such
  a line (a blank line, a closing brace) goes with the lines on either side
  of it in its list when they are one session's, or with the only side next
  to it at the hunk's edge; between the two sides, or alone, it stays this
  session's as before. It goes with another session's lines only while this
  session's journaled writes still account for each copy of that line left
  in the file's diff, so a session's own only copy is never left behind
  (issue #141). Tests: `lines_both_sessions_wrote_go_with_the_hunk_around_them`
  and `a_line_both_wrote_stays_this_sessions_when_its_only_copy_is_by_the_others`
  in `tests/commit_guard.rs`. A moved line is neither added nor removed by its write, so a hunk
  that moves lines is nobody's or not known.
- Changes made by anything other than a reported edit or an observed shell
  command (the owner's editor, a background job after its call returned)
  are nobody's: they never hold a commit, and `commit --mine` leaves them.
- A shell command that rewrites another session's lines (`git stash`,
  `git checkout -- FILE`, a formatter) is journaled as the running session's
  change, as ADR 0011 attributes it.
- `git commit --amend` is read as a plain commit: it can also rewrite what
  the previous commit holds, which is not checked.
- Git commands reached through scripts, aliases, `xargs`, `bash -c` or a
  command substitution are not read (`if`, `{`, `time`, `env`, `sudo`,
  `timeout N`, `nohup` in front are). An unknown or abbreviated `git commit`
  long option gives no decision. The guard is cooperative, like every
  admission: it fences nothing.
- `commit --mine` reads the working tree for whole files when it stages
  them; a write between its plan and its staging can be committed with them.

## The journal as a shared module

`src/journal.rs` is the one record of which session wrote which lines;
nothing else keeps these facts. Its reading API, for any feature that needs
line ownership (the guard, `commit --mine`, line-level admission):

- `journal::entries(state_dir, root)`: every write kept for a workspace,
  each with its session, path, time, resulting version and line keys.
- `journal::by_file(&entries)`: per file, line key to the sessions that added
  or removed it, sessions whose lines are not known, and each write's
  version.
- `journal::written_lines(&entries, path, content)`: for a version of a file,
  each line a journaled write added, with who wrote it and when.
- `journal::line_key`, `journal::line_keys`: the keys themselves.

Writes are recorded only by the hooks (`before_edit`, `before_shell`,
`record`); `forget` is for a committer of its own entries.

## Consequences

- Every successful edit and observed shell change now writes one small entry
  and, for files up to 512 KiB, a copy of the file version: uncommitted file
  content is kept in the owner-only state directory for up to a week. It
  never leaves the machine and is never sent to the coordinator.
- A `Bash` call with a Git staging or commit step costs one coordinator call
  and two or three Git listings more, inside the existing hook budget. Every
  other `Bash` call costs one substring check, and a stat per changed file
  for the journal.
- The shared index is written only by `commit --mine`, which the agent runs
  for that purpose, and only for the files it commits.
- A new MCP tool, `langspan_commit_mine`; later
  `langspan_my_changes`, read-only (issue #40).

## Design input adopted

From the design of a shared-tree guard with a write journal: holding a commit that would sweep in another live session's uncommitted
work, naming that session; telling sessions' lines apart by content hashes
per write; building the commit in an index of its own from `HEAD`, then
teaching the shared index the commit; refusing what cannot be attributed.
Choices made here: the guard reads what Git itself would take (same directory
and pathspecs), holds once per command so the capability stays with the
agent, reads heredoc commit messages, and liveness comes from the
coordinator's record of ended and exited sessions.
