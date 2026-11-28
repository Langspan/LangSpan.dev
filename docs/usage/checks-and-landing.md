# Checks and landing

The CLI implements `check run`, `check status`, `integrate check`, and `land`.
A working-directory check records its command and result with a Git-tree ID,
including tracked files and untracked files that Git does not ignore. Different
before-and-after trees or an unfinished command produce an inconclusive result.
`check status` compares the recorded tree with the current one.

`integrate check` combines branches in the order given and runs the command in
a temporary checkout. `land` recombines them and requires a passing check of the
chosen name for the resulting tree; without one it exits 6. It pushes only with `--apply`, using a normal
non-forced push and the repository's pre-push hooks. With `--apply` it first
takes the push turn for the workspace and target. If another landing has it,
it waits in line for it, first come first served, for up to `--wait-s`
seconds (default 240, at most 3600; `--wait-s 0` returns at once, as before).
At most 8 landings wait per target. A landing that does not get the turn
(it did not wait, the line was full, its wait ran out, or it was cancelled)
exits 7 with the holder and the line, having fetched and pushed nothing. Only
once it has the turn does it fetch the target, so it combines onto whatever
the landings before it pushed and, with `--check-again`, checks that new tree.
`langspan land-queue WORKSPACE --onto BRANCH` shows who holds the turn and who
waits (also in `status` and on the dashboard);
`langspan land-cancel WORKSPACE --onto BRANCH --request-id ID` stops a waiting
landing. A waiting landing that is killed leaves the line once it has not asked
for 15 s; a daemon restart empties every line. If the push is refused
because the target moved, it combines again and retries, at most three
attempts, and lands a new tree only with a passing check for it:
`--check-again -- COMMAND...` runs the check inside the turn to get one. A
refusal by a hook or a protection rule is not retried. If the push went
through but recording the landing failed, exit code 8 names the pushed commit;
if the push did not finish, LangSpan asks the remote where the branch is: at
the pushed commit it is landed and recorded; still at the earlier tip, exit
code 2 says nothing was pushed; otherwise exit code 9 says whether it happened
is not known. Exit codes 8 and 9 print the `git ls-remote` command that shows
the remote branch. `--json` prints the
whole result, including every attempt.

When branches do not combine, `integrate check` and `land` name the conflicting
paths and, for the first 8 files, up to 4 conflicting hunks each: the target's
side and the branch's side, each with its first line number in that version,
its line count and its first 6 lines (at most 200 characters each, control
characters replaced). Git's conflict type is given per file, such as `contents`
or `modify/delete`. These lines are the repository's text, shown as data.

An inconclusive `integrate check` says why, in the same words on the CLI and in
the MCP result's `inconclusive` field: its `cause` (`changed_during_run`,
`checked_out_differently`, `did_not_finish`, or `replayed` for a retry of a
request whose record does not keep the reason), a `message`, up to 20 of the
`changed` files with whether each was added, modified or deleted and whether
it is untracked (`changed_total` counts them all), and what to do `next`. A
check command that writes a file the branch does not track, such as a lock
file a build tool creates, is the usual cause: commit the file on the branch or
add it to `.gitignore`, then run the check again under a new request ID. The
paths are the repository's text, redacted and without control characters.

`integrate check --symbols` (`symbols: true` on `langspan_integrate_check`)
also compares the branches' definitions before the command runs. For each
branch, the files it changed in an indexed language (up to 1000, each up to
1 MiB) are read from Git's object database at the branch and where it started
from the target, and indexed in memory; `graph.sqlite3`, the checkout and the
refs are not written. The record's `symbol_findings` holds at most 32 findings,
each a name match between two branches:

- `both_changed`: both branches changed one definition's signature, in the
  same file, to different signatures.
- `changed_and_called`: one branch changed a signature, and another added a
  call whose argument count fits the old signature but not the new one (the
  rule of the write-time break alert, ADR 0029). `args` is the call's count
  and `takes` the new signature's.
- `removed_and_used`: one branch removed or renamed (`renamed_to`) an
  exported definition, and another added a line that mentions its name.

A branch that itself changes or adds a definition of that name is not told
about it. Each finding gives the branch, path and line on both sides; `more`
counts the findings past 32, and `files_indexed` and `files_skipped` say what
was read. Findings are advisory: they never change the check's result, its exit
code or what `land` requires. If indexing takes more than 10 seconds or Git
fails, `note` says the findings were skipped, and the check runs as without
`--symbols`. A retry of the same request returns the recorded findings.

```text
$ langspan integrate check app --onto main --name unit --symbols feat/sync feat/import -- cargo test
...
unit passed on the combined tree 7ac1...
Definition findings (found by name; advisory):
- changed_and_called: "save" now takes 2 arguments (feat/sync, "src/store.rs":40); feat/import calls it with 1 at "src/import.rs":18
```

Combining always follows renames, whatever `merge.renames` is set to: when the
target renamed a file after a branch started, the branch's edits to the old name
are combined into the new file, and the result's `renames` (and the printed
output) says which. Inexact renames are looked for among at most 1000 files on
each side; exact renames are always found. By default combining is a merge:
the branches' commits are pushed as they are (see ADR 0007).

When the target renamed a function in the same file after a branch started
(same body, new name), and the branch's added lines still mention the old
name, `integrate check` and `land` list it in `identifier_renames`: `from`,
`to`, the defining `path`, and each use as a path and line in the branch's own
version (at most 16 renames and 32 uses each). Names are matched as whole
words, not resolved. `identifier_renames_incomplete` says when they could not
all be looked for, for example because a changed source file does not parse.

```sh
langspan land app --onto main --name tests my-branch --apply --adapt-renames --check-again -- cargo test
langspan land app --onto main --name tests my-branch --apply --rebase --check-again -- cargo test
```

`--adapt-renames` adds one commit on top of your branch, without checking it
out, that replaces each listed use of the old name with the new one, and lands
that tree only on its own passing check. The result's `adapted` names the
commit; your branch is not changed, so fetch that commit to keep it. When the
rewrite is not certain (for example, your branch already mentions the new name,
changed the old definition, or the target still defines the old name
elsewhere), nothing is rewritten or pushed: the result is `not_adapted`, exit
code 2, with the reason, and each rename's `uncertain` says why.

`--rebase` replays your branch's commits onto the target instead of merging,
for linear history: the new commits keep their author, date and message, only
the final tree is checked, and `rebased` lists each original and new commit. A
commit whose replay conflicts stops the landing as `not_combinable` with that
`commit` and its hunks; a branch with merge commits, even one that would
fast-forward, or a landing with more than 64 commits to replay across all its
branches, is `not_rebased`. Landing the same branch again afterwards finds its
changes on the target and reports nothing to land. Neither flag ever forces a
push or changes your checkout. Both also work on `integrate check`, so a check
recorded with the same flags is the evidence a later `land` with them uses;
through MCP, pass `rebase` and `adapt_renames` to `langspan_integrate_check`
and then to `langspan_land`. Check commands run with your
environment; environment identity and ignored files are outside the record.
A check's recorded command line is redacted like its output: a `--token`
value or a key-shaped argument is stored and shown as `[redacted]`, while the
check's name and program stay readable. Prefer passing secrets through the
environment, which is never recorded.

### An agent's push to the default branch

In a hooked session, an agent's `git push` that would update the remote's
default branch (the branch `REMOTE/HEAD` names) does not run. The hook
refuses it, every time, with the landing command to run instead:

```text
LangSpan: push routed to landing. main is the default branch of the remote, and a push to it goes through landing: ...
Run `langspan --state-dir DIR land WORKSPACE --onto main --name CHECK refs/heads/feat --apply --rebase --check-again -- TEST_COMMAND` ...
```

The agent fills in a check name and the project's test command. The branch
is named in full (`refs/heads/feat`), so a tag of the same name is never
landed in its place. To learn where a push goes, the hook runs it first as
`git push --dry-run --porcelain` with the same arguments. The push is routed when the dry
run would update, force or delete the default branch, whether through a
refspec, a mirror remote, `push.default`, `remote.pushDefault`
or a configured `remote.origin.push`. Pushes to other branches and tags pass.
An agent's push may name remotes and refspecs and use only `-u`, `-q`, `-v`,
`--tags`, `--[no-]follow-tags`, `--atomic` and `--[no-]progress`, spelled out;
force with `+SRC:DST` and delete with `:DST`, and set anything else with
`git config` (not `git -c`).
The hook refuses a push it cannot check this way: one combined with another
command (`git add -A && git push`), one with an expansion in its words, one
with any other option, `--` or `git -c`,
and one whose dry run fails, lists no refs or does not finish in the hook's
time. The message
names the word and says what to run instead. A push from another checkout, to a URL no remote has,
or with no `REMOTE/HEAD` runs. Set the remote's HEAD with
`git remote set-head origin --auto`. Pushes from your own terminal are never
touched. To let agents push the default branch directly, run
`git config langspan.defaultBranchPush direct` in the repository. See
[ADR 0007](../adr/0007-check-evidence-integration-and-landing.md#routing-an-agents-push).

Tests use real daemons, CLI commands and local file remotes. Hosted branch
protection and native-agent workflows remain unverified. See
[ADR 0007](../adr/0007-check-evidence-integration-and-landing.md) for command syntax,
input limits, and the gap between a successful push and recording it locally.
`langspan_integrate_check` and `langspan_land` expose the same operations to agents (below).

## Automatic checks after a session's writes

Off by default. Turn it on for one workspace:

```sh
langspan check auto WORKSPACE --name tests --timeout-s 600 -- cargo test --workspace
langspan check auto WORKSPACE          # show the setting
langspan check auto WORKSPACE --off
```

After a session writes files and reaches the end of a tool batch (Claude
Code's `PostToolBatch`) or of its reply (`Stop` on Claude Code and Codex), the
daemon runs the command. The hook never waits for it. Cursor and Antigravity
have no such event hooked, so their sessions start no automatic check. The daemon waits until
the workspace's sessions have paused for `auto_check_debounce_ms` (default
2000) and at most 30 s, so a burst of batches runs one check, then takes the
exact working tree as `check run` does and runs the command there, without a
shell, in its own process group, within the time limit (default 1800 s, at
most 3600). If a session writes again while it runs and the tree changed, it
is stopped and the new tree is checked. Changing the command or `--off` stops
a running check.

The result is an ordinary check record of that tree (`check status` shows it,
`land` can use it), and the sessions it ran after are told once, at their next
briefing point (a completed tool call, the next batch, or the next prompt):
passed, failed with the exit code, stopped at its time limit, or inconclusive,
with the tree ID and, unless it passed, up to 20 of the last output lines,
redacted and labelled as the command's own text. When the output is that of
`cargo test`, `go test -v` or `pytest`, the failing tests it named are listed,
and compared with the session's baseline when it has one (below). The command runs as you, with
the daemon's environment, outside the agent host's sandbox and permission
prompts: code an agent writes runs without the host asking. A command that
looks like it holds a secret is refused; pass secrets through the environment.
See [ADR 0030](../adr/0030-automatic-checks-after-write-batches.md).

### Which tests a write broke

A session's baseline is the newest result of the same check (name and
whole argument list) with named tests whose tree was taken before the session's
first write: a `check run` you made before the agents started, or an
automatic run after another session's writes. A run during which any write
was recorded is no baseline. The session's results are then
compared with it, test by test:

```text
LangSpan: the automatic check tests failed (exit 101) after your writes, on working tree 4b1c9e2… (took 41.0 s; recorded as check 7f3a…).
2 new failures: parser::rejects_empty, parser::keeps_order.
1 already failing before your writes: net::times_out.
1 fixed since before your writes: old::was_broken.
This tree also holds writes by codex session bob since the check before your writes.
```

A test the baseline did not name, or skipped, is listed as failing and not
named by the check before. The writers are the other sessions whose writes
were recorded between the two trees. With no baseline (run `langspan check
run WORKSPACE --name NAME -- COMMAND...` with the automatic check's command
before the agents start, so every session has one), or output no parser
recognises, the result is reported as before. The comparison never changes
the record: `check status` and `land` see the same failure.

### The wider check after an interface change

A fast check often builds only the package a session changed, so a broken
caller in another package shows up later, in someone else's work. Give the
automatic check a second, wider command:

```sh
langspan check auto WORKSPACE --name fast -- cargo test -p core \
  --on-interface-change all --timeout-s 1800 -- cargo test --workspace
```

The first `--timeout-s` (before the first `--`) is the regular command's; the
one after `--on-interface-change NAME` is the wider command's (default 1800 s).
Both commands are refused if they look like they hold a secret.

When the source index reports that a session's writes changed an exported
name that other files mention (an interface notice), the wider command runs
after the regular one finishes, on the same tree, with the same rules: no
shell, its own process group, cancelled and restarted with the regular
command when a session writes again and the tree changes. LangSpan runs the
owner's whole command and never adds paths to it, so a dependency the index
misses still gets tested. A body-only change produces no notice and runs only
the regular command. A change no other file mentions runs nothing extra.

The wider result is an ordinary check record under its own name, graded
`kept` when it passed and `broke` when it failed. The session that made the
change, and any session working on or having read a mentioning file, are told
at their next briefing:

```text
LangSpan: the automatic check ran its wider command. Interface change to parse_upload in core/src/lib.rs: "pub fn parse_upload(a: u32, b: u32) -> u32" -> "pub fn parse_upload(a: u32) -> u32": the wider check all failed (exit 101); files that mention it: cli/src/main.rs (a name lookup, not resolved uses). On working tree ...
```

The mentioning files come from the index's name lookup. The line shows at
most 8 of them and counts the rest; a session working on any of them, shown
or not, is told. The line also says how many source files the index could not read in full. If the
index has no complete scan of the workspace, no interface change can be found:
the regular result says so and the wider command does not run.

### Tests that flip on unchanged code

Two results of the same check (name and command, compared by its whole
argument list, not the shortened command line `check status` shows) on the
same tree, with no write recorded in the workspace between them, that
disagree about a test are one flip of that test. `check run` learns nothing
from a command that looks like it holds a secret. A failing automatic check that named a failing test and
took at most `flaky_rerun_max_s` (`config.json`, default 30, 0 turns it off)
is run once more on the same tree, stopped after at most that long, to get
that second result; a longer one is not run again, and a write recorded while
either run goes, even one undone, means neither is compared. The rerun is
not a record: the first
run's failure is the result, in `check status` and to `land`, whatever the
rerun found. Two `check run`s of one tree with no write between or during
them are compared too. A test the output names as skipped (`ignored` in
`cargo test`, `--- SKIP:` in `go test -v`, `SKIPPED`, `XFAIL` or `XPASS` in
`pytest -v`) neither passed nor failed in that run, even when the run passed,
and is not compared. A test that flipped at least twice in its last 20 such runs is known to
flip, until it has gone 50 runs without one. Its failures are then labelled in
the briefing, and still count as failures:

```text
1 failure named in its output: net::times_out (known to flip on unchanged code, 2 of 14 runs). Every one is of a test known to flip on unchanged code; the check still failed.
```

```sh
langspan check flaky WORKSPACE [--name NAME] [--json]
tests  "net::times_out"  runs 14  flips 2  last flip 3h ago
langspan check flaky WORKSPACE --forget net::times_out [--name NAME] --request-id ID
```

`--forget` clears what was learned about a test, including its last result,
so its next failure is not labelled. A test or check name that looks like a
secret is refused (`secret_in_test_name`) before anything is recorded: such a
name is never learned, so there is nothing to forget. The MCP tool `langspan_flaky_tests` reads the same list. Only test
names are kept, in the local event log; no output, and nothing is published.
Names are read from at most 4 MiB of output (at most 100 names and 10 KiB per
run); a runner LangSpan does not recognise teaches nothing.
