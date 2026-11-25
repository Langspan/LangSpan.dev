# ADR 0026: Commit guard and `commit --mine`

**Status:** Implementation decision, 24 November 2026. Extends
[ADR 0011](0011-shell-writes.md) (the Bash hook and its parser) and builds on
the attribution of edits to sessions in ADR 0002, ADR 0004 and ADR 0011.
Roadmap rows B6 and B7, the shared-index guard and `commit --mine`.
Tested with the real daemon, Git, hook executable, CLI and MCP adapter
(`tests/commit_guard.rs`); not yet observed with a real Claude Code or Codex
session.

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
  files (ADR 0011) are not journaled.

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

## Never covered

- The hook fallback for Git before 2.36 (running files from the hooks
  directory) is not exercised by the tests, which run Git 2.56. Hooks run
  twice when the branch moves during the first attempt.
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
  refused. A moved line is neither added nor removed by its write, so a hunk
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
- A new MCP tool, `langspan_commit_mine` (21 tools).

## Design input adopted

From the design of a shared-tree guard with a write journal: holding a commit that would sweep in another live session's uncommitted
work, naming that session; telling sessions' lines apart by content hashes
per write; building the commit in an index of its own from `HEAD`, then
teaching the shared index the commit; refusing what cannot be attributed.
Choices made here: the guard reads what Git itself would take (same directory
and pathspecs), holds once per command so the capability stays with the
agent, reads heredoc commit messages, and liveness comes from the
coordinator's record of ended and exited sessions.
