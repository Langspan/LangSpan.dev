# Commits in a shared checkout

When several sessions work in one checkout they share one Git index, and a
commit takes whole files. The shell pre-tool hook (Claude Code and Codex
`Bash`, Cursor `Shell`) holds a command that
would stage or commit another live session's uncommitted changes: `git add -A`, `git add .`,
`git add -u`, `git add PATH…`, `git commit -a`, `git commit PATH…`, or a plain
`git commit` while the index already holds such changes. The reason names the
other session and its files and lines, and gives the options. The same command
run again is not held: each is held once, so the choice stays with the agent
and the owner. Changes no session made (your own editor), this session's own
changes, and changes of sessions that have ended never hold a commit. The
hold has the same text and coverage, `denied_commit`, on every host. The
guard reads the line journal only for the files the command would take, so
its time grows with those files, not with the journal. With the coordinator
down, a Git listing over its bounds (16 MiB of diff, 1,024 untracked files)
or the hook deadline reached, it makes no decision, and the hook's detail
says which.

To commit only what this session wrote:

```sh
langspan commit --mine -m "Fix the parser" [--session ID] [--root DIR] [--json]
```

Files whose every change is the session's are committed whole; in a file
that also holds another session's changes, only its own hunks. The commit is
built in a private index from `HEAD`, so nothing anyone else staged rides
along, and made with your identity and Git hooks. The branch moves only if no
other commit landed meanwhile; if one did, the session's changes are planned
again on top of it once, or the command refuses and nothing is lost. Everything else stays as it
was, staged or not, and is listed. When Git joins the session's lines and
another session's into one hunk (two entries added one below the other to a
list), the hunk is split between them and only the session's own lines are
committed. If a hunk mixes the session's lines with lines no session wrote,
or one session's line there replaced another's, or the two sessions' lines
there could be in more than one order (one session changed "A B C" to "B X"
and the other removed B: X could go above B or below it), nothing is
committed (exit 1) and the hunk is named, with the reason. Nothing is
committed either if the partial patch would not keep the session's lines in
their one place among the other session's, or does not apply exactly to the
version in the index. Lines are matched by content, not position, so sessions
editing one file in turn keep their own hunks however their edits shift each
other's lines. When another session changed the file in a write whose lines
are not known, the session's hunks are still committed unless one of their
lines appears in the file's changes since `HEAD` more often than the session
wrote it, or the session's earlier write of that line may already have been
committed through Git; that hunk is then named, with the reason. A
formatter that only reflows lines keeps each line with the session that wrote
it; one that joins two sessions' lines into one makes a hunk whose owner is
not known, and the command refuses. Run from an agent's shell (Claude Code
`Bash`, Codex `Bash`, Cursor `Shell`), the
session is the one running the command; from a terminal, pass `--session` (a
briefing says "Your session ID is …"). It looks for the running command in its own state directory, so
with hooks installed under `--state-dir`, pass the same `--state-dir`; when
it finds no running session, the error names the directory it looked in and,
if a LangSpan hook installed in the checkout names another one, that one.
The MCP tool `langspan_commit_mine` does the same, and its result holds the
same fields as `--json` (`result`, `session_id`, `commit`, `paths`, `left`,
`refused`, `moved`, and `index` when the shared index could not be updated),
plus `exit_code` and `request_id`. The output lists what was left
uncommitted and, when anything was left or refused, points to
`langspan status --mine` for the lines still this session's. If one of your
`pre-commit`, `prepare-commit-msg` or `commit-msg` hooks fails, nothing is
committed and the error names that hook. With Git before 2.36, which has no
`git hook run`, the hooks run from `core.hooksPath` or the Git directory's
`hooks`.

To see first what `commit --mine` would take:

```sh
langspan status --mine [--session ID] [--root DIR] [--json]
```

```text
shared.txt  mixed  lines 10-14 mine, line 22 no session's, lines 40-41 claude-code.…
src/new.rs  mine   lines 1-30
3 changed file(s) hold other sessions' lines.
```

It lists, by path, each changed file that holds this session's lines:
`mine` (`commit --mine` would commit all of it), `mixed` (only its hunks
marked mine) or `unknown` (it would refuse, and why), whether some of it is
`staged` in the shared index, and each hunk's first line, line count and
owner: `mine`, another session's (`others`, with `sessions`), `none` (no
session wrote it) or `unknown` (with `reason`). A file attributed as a whole
(binary, over 512 KiB, a mode change) is `whole`, with no hunks. `others`
counts the changed files that hold other sessions' lines, also where they
share an `unknown` hunk with this session's. At most 200 files
are listed (`truncated` when there are more). It runs the same attribution as
`commit --mine`, finds its session the same way, and changes nothing: no
private index, no commit, and Git reads run with `--no-optional-locks`. The
MCP tool `langspan_my_changes` does the same. Other worktrees and machines
are not included.

## Take over an ended session's uncommitted work

A session that ends before it commits (its process crashed or was killed, the
disk filled) leaves its edits in the checkout, credited to it. A session that
carries on the same task takes them over:

```sh
langspan adopt SESSION [--session ID] [--root DIR] [--request-id ID] [--json]
```

```text
Session claude-code.… adopted the uncommitted work of ended session claude-code.…. Reason: both sessions have the same task line. Its lines in 2 file(s) are now this session's: a.txt, b.txt. …
```

The coordinator checks that `SESSION` has ended (the host reported its end,
or its process is known to have exited) and worked in the same workspace,
and records the transfer as a `session_adopted` event naming both sessions.
Then the journal entries of `SESSION` and its ended subagents are credited
to the adopting session, keeping their age, so `status --mine` and
`commit --mine` treat those lines as its own. Ending a session does not end
its subagents: a subagent that has not ended keeps its lines, and the report
names it (`live` in `--json`). Lines another session also wrote stay shared,
exactly as before, and other sessions' lines stay theirs.

The adoption is refused, and nothing changes, for a session that has not
ended (`session_live`: a killed session whose process LangSpan never observed
counts as live), for one of another workspace (`other_workspace`), and for one
another session already adopted (`already_adopted`). Running it again is safe:
the coordinator answers `already_adopted` with nothing new recorded, and any
lines still credited to the ended session are credited then. A retry with the
same `--request-id` is checked again too: if the ended session has resumed
since, the retry is refused with `session_live` and credits none of its lines.
An adopter that ends after its adoption is recorded, but before its lines are
credited, leaves them under the session it adopted. Adopting that adopter
credits them as well: the coordinator names every session whose adoptions
lead to it (`earlier` in the coordinator's answer), and their lines move with
its own, except those of subagents that have not ended and of sessions that
have resumed since they were adopted. A subagent that another session adopted
keeps its lines for that session, even before that session's adoption credits
them (`kept` in the coordinator's answer). The reason shown
is a message the ended session sent the adopter (addressed to it, or a
handoff it took) or a `Task:` line both share; neither is required, so the
person or agent decides. The adopting session is found as for
`commit --mine`. The MCP tool `langspan_adopt` does the same. `recap` names
the transfer under both sessions, as do `status` and the dashboard.

When a session starts in a checkout where an ended session's journaled lines
are still uncommitted and nobody adopted them, its briefing says so once,
with that session's task line, and suggests the command:

```text
Session claude-code.… ended and left uncommitted work in this checkout (its task: "Fix the upload handler"). If you are continuing that task, run `langspan adopt -- claude-code.…` to take its lines over as yours; otherwise leave them.
```

At most four such sessions are named, and at most 32 files are compared with
`HEAD` to find them, within half the start hook's time.

A formatter or other workspace-wide command (`cargo fmt --all`) does not
change who wrote a line it only reformats. Where the command kept a line's
tokens in order and changed only whitespace and line breaks (re-indented,
re-wrapped, joined or split it), each line it left keeps the author of the
uncommitted lines it came from: the session that ran the formatter, another
session, or both sessions' own lines in one file. This holds whoever ran the
command and whether or not another session's command ran at the same time,
so `cargo fmt --all` during another session's test run leaves each session's
`commit --mine` able to take its own hunks, and a new file one session wrote
stays that session's after another session's formatter rewrites it. Tokens
are compared by language: Rust, TypeScript and JavaScript as words and
punctuation, each string or character literal one token whitespace and all,
with a comma just before a closing `)`, `]` or `}` ignored (a formatter adds
and drops those when it wraps a list); Markdown as words and punctuation;
Python as words, punctuation and strings, with the indentation of each line
that starts a statement as content, so re-wrapping a call inside its
parentheses is formatting but re-indenting a block is not; YAML, Makefiles
and other indentation-sensitive files (`.haml`, `.pug`, `.sass`, `.styl`,
`.nim`, `.fs` and the others ADR 0011 lists) as words and punctuation with
every line's indentation as content; any other file as its text with all
whitespace removed. Trailing whitespace and blank lines are always
formatting.

A change that alters tokens (a rename, an added line) is credited as before:
to the command's session when nothing else could have made it and it
rewrote no other session's uncommitted line, and otherwise to no session, so
`commit --mine` leaves or refuses it. The same holds for a file another
session edited through its edit tool while the command ran
([ADR 0011](../adr/0011-shell-writes.md)). A line two sessions both wrote (a
blank line, a closing brace) goes with the lines around it in a hunk, so it
does not stop either session's `commit --mine`
([ADR 0026](../adr/0026-commit-guard-and-commit-mine.md)).

Before such a command runs, the Bash pre-tool hook on Claude Code and Codex
tells the session, once, which other live sessions have uncommitted changes in
files it may rewrite: "LangSpan: this command may reformat files that another
session in this checkout has not committed.", then each session and at most
eight of its files, then how many more. The command is not refused, and the
host's own permission prompt is unchanged. The hook recognises `cargo fmt` in
any form (it formats every file of the workspace, even when files follow
`--`), `rustfmt FILE...` (those files and the modules they declare) and
`prettier --write PATH...` (directly or through `npx`, `pnpm exec` or `yarn`).
A check such as `cargo fmt --check` or `prettier --check .` names nothing.
The same files are not named again whichever formatter runs; a new file is.
The hook's log line says why no notice was given (`formatter notice: …`). In a
shared checkout, format only the files you changed, by name: `rustfmt FILE...`
or `prettier --write FILE...`. The LangSpan skill says so too.

When a Git step fails, the error names the step, its exit status and what Git
said (its last lines, at most 1 KiB, redacted as other relayed text is). The
commit is built in the Git directory, so it needs to write there: inside an
agent's workspace sandbox that directory is often read-only, and the error then
says so. Run `commit --mine` outside the sandbox (from a terminal, or with the
host's permission to run it unsandboxed), or make the Git directory writable.
The commit guard's own Git reads report a failure the same way, in the hook's
log line, and give no decision.

Which lines are whose comes from a journal the hooks keep on this machine
(`STATE/journal/`, owner-only): for each reported edit and observed shell
change, the keys of the lines it added and removed, and a copy of file
versions up to 512 KiB. Nothing of it leaves the machine. See
[ADR 0026](../adr/0026-commit-guard-and-commit-mine.md) for the rules and what
is not covered (identical lines, moved lines, changes LangSpan did not see).
