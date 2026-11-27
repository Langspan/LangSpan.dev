# Coordination benchmark harness

**Infrastructure, not a product-performance result.** It compares three
conditions for N parallel agent sessions on seeded tasks
([issue #172](https://github.com/Langspan/LangSpan.dev/issues/172)). The first
real run (12 trials, before the fixes of
[#188](https://github.com/Langspan/LangSpan.dev/issues/188) and
[#193](https://github.com/Langspan/LangSpan.dev/issues/193)) measured the
harness design rather than coordination, and so did the N = 5 pilot that
[#208](https://github.com/Langspan/LangSpan.dev/issues/208) fixes. Neither is
a result.

Two parts do the work. `driver.py` runs one seeded trial: it prepares a fresh
fixture repository and one checkout per session, sets up the arm, and writes
one run record. `runner.py`, the generic runner, launches the sessions and the
oracle for it, bounds them and keeps the raw journal. The runner also still
runs owner-written manifests, as described under
[Owner manifests](#owner-manifests).

## What each arm provides

Every session works in its own checkout of the fixture, on branch `bench/sN`,
with no remote. What a session learns about its peers comes only from its arm:

| Arm | Checkout | What a session learns about its peers |
| --- | --- | --- |
| `worktrees` | Its own clone, with its own objects and refs | Nothing until integration. The arm keeps its first name, although each session has a clone, not a worktree |
| `awareness_only` | Its own clone | Each peer's task text (its `Task:` line and details), listed in the prompt as information. No LangSpan |
| `langspan` | A worktree of one repository, `work/shared`, which no session's prompt names | LangSpan's briefings and notices, nothing else. Each worktree has LangSpan's Claude Code project hooks and skill; one coordinator serves them all, as on one developer's machine |

The `langspan` arm uses worktrees of one repository because that is the setup
in which LangSpan tells one session about another's interface changes. With
separate clones, each clone is its own repository, and the rename notice of
the N = 5 pilot stayed with the renaming session. A session in this arm is
told:

- each peer's task line (at most 200 bytes), branch and worktree path, and the
  files it is editing and recently edited, at session start, at a prompt and
  after each completed tool call; never file contents;
- each new or changed exported signature in another worktree that is not
  committed yet, once, when a file the session read or edited mentions the
  name or imports the module: "In flight in worktree ...: function
  `load_config` in `shelf/config.py`" ([usage](../../docs/usage.md));
- a peer's task line no longer once that session ends.

Edit admission stays per worktree: a session is never held or refused because
of a peer's worktree.

### The same limits in every arm

Every session, in every arm, gets the same tools and the same denials, so in
`langspan` the only extra information is what LangSpan sends:

- **Built-in tools only, and only these:** Bash, Edit, Glob, Grep, Read and
  Write (`--tools`), plus Skill in the LangSpan arm to load LangSpan's skill.
  Claude Code's cross-session tools (`ListAgents`, `SendMessage`), subagents,
  web search and fetch, tool search and scheduling tools are absent, and also
  named in `--disallowedTools`.
- **No MCP server or hosted connector:** `--strict-mcp-config` with an empty
  `--mcp-config`, so neither the configuration's servers nor the account's
  connectors load.
- **Nothing outside the session's own checkout:** reads and edits are allowed
  under the working directory only, and every file tool is denied the other
  sessions' checkouts and the run's repositories by path. `git log`, `git
  diff` and `git show` are denied any `bench/` branch, `--all`, `--branches`,
  `--remotes`, `--glob`, `--reflog`, `refs/`, the stash and `--no-index`;
  `git stash`, `git worktree`, `git branch`, `git for-each-ref`, `git
  reflog`, `git -C`, `--git-dir` and `--work-tree` are denied outright. A
  session uses `HEAD` for its own branch; `git add` and `git commit` stay
  allowed.
- **Nobody answers a permission prompt** (`--permission-prompts none`): a call
  outside the allowed list is refused, and the record counts it by kind.
- **No auto memory:** Claude Code keeps auto memory in one directory per
  repository, so the LangSpan arm's worktrees of one repository would share
  one, a channel between sessions that the other arms lack. Every session
  turns it off with Claude Code's own `autoMemoryEnabled` setting
  (`--settings '{"autoMemoryEnabled": false}'`) and its
  `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` environment variable. The preflight
  check refuses a trial whose sessions would share a memory directory, and
  the record's `auto_memory` is `off`.

The **tool guard** checks this when the session starts. The driver runs every
session, stand-in included, through it: it passes each stream-json line on to
the transcript and reads the session's first event, the tool list Claude Code
reports before the model's first turn. A tool outside the arm's set, or any
MCP server, there or in a later tool list or a tool call, stops the session at
once. The guard then writes its verdict as the transcript's last line
(`benchmark_tool_check`) and exits 3, so the trial fails and the record's
`tool_check` names the session and what it was offered. A session that never
lists its tools fails too.

## Files

- `driver.py`: one trial of an arm, task family, session count and seed.
- `tasks.py`: the three task families, their prompts and what the oracle checks.
- `fixture/`: the fixture repository, a small Python package with its own tests.
- `oracle.py`: the combined-integration oracle, run on the merged result.
- `standin_agent.py`: the dry-run agent; it makes one of its task's fixed
  edits, or a repair round's, and no model call.
- `run-record.schema.json`: the driver's run record v7.
- `runner.py`: validate-only by default; opt-in real-process execution.
- `manifest.schema.json`: command manifest v1.
- `manifest.template.json`: owner-filled template for the three required arms.
- `records.schema.json`: raw per-item/per-arm/per-repetition JSONL journal v1.
- `SCENARIOS.md`: workload, ordering, and outcome evidence requirements.
- `test_runner.py`, `test_driver.py`: real local processes, Git and, for the
  LangSpan arm, the real `langspan` binary; no mocked model or host event.

Python 3.9+ on POSIX is required. There are no package dependencies.
The commands below assume the repository root.

## Task families

Each task is a one-line statement, the prompt's `Task:` line, and details
beneath it. Every prompt also states how the result is judged: every branch is
merged into one tree, and the family's success definition below must hold in
that tree. The prompt states that definition in terms of the tasks, never of
what a peer task changes, so it gives no arm the renamed interface; the last
column below spells out what it means for the fixture. A prompt tells the
session to build against a peer's changed form when it knows of one, even if
its own branch's tests then fail. The oracle checks exactly the success
definition, on the merged tree, never on a branch alone.

| Family | Sessions | What can go wrong | A successful combined result |
| --- | --- | --- | --- |
| `rename_vs_new_caller` | One session renames `parse_config` to `load_config` across the package; each other session adds a feature module that calls the parser, and "if another session renames that parser, call it by its new name" | A new caller uses the old name: every branch passes alone, and the merge breaks | The combined tree has `load_config` and no `parse_config`, every caller in the package uses `load_config`, every new feature module calls `load_config` (not an alias) and works, and the package's tests pass |
| `contradictory_defaults` | Each session needs its own function to use a value of one of the defaults in `shelf/settings.py`. Some defaults are wanted by two sessions at different values (a pair), others by one session alone. Each task defines what to do: "if another session's task sets the default to a different value, leave the default unchanged and give your function its value in its own module; otherwise change the default". A session may get an unrelated control task where the keys run out | A pair both change the default: Git refuses the second merge, or one requirement is silently lost. A session alone keeps the default and sets its value locally, a hedge that ignores what its task asks | Each function returns the value its task asked for; every shared default two tasks wanted changed to different values is unchanged; every shared default only one task wanted changed holds that task's value; and the package's tests pass |
| `discovered_change` | One session, the changer, adds `expired(items, today)` in `shelf/expiry.py` and counts the days with `days_left` from `shelf/dates.py`. Each other session, a dependent, adds a feature module that counts the days with `days_left`, and "if another session changes how `days_left` is called, call it the new way". No task text says that `days_left` changes | `days_left(day)` counts from the clock and gives `None` once a day has passed, so it cannot count from a past `today`, and no arithmetic on one-argument calls recovers the count: the changer finds that while working and gives it a second, required parameter. A dependent still calls `days_left(day)`: every branch passes alone, and the merge breaks | `days_left` requires two arguments, with no default, alias or wrapper that keeps one-argument calls working; `shelf/orders.py`, `expired` and every new feature module call it with two arguments; every function returns what its task asked for; and the package's tests pass. A module that counts the days itself, or falls back to the one-argument call, fails |

In `discovered_change`, a dependent's result means something only when the
changer made the change. In the third pilot, a changer subtracted two
one-argument calls in 2 of 5 trials, which the fixture then allowed, and the
oracle failed every dependent whatever it knew. The fixture's `days_left` now
gives `None` for a passed day, so that workaround finds no expired item, and
the changer's own check requires the changed signature. The oracle reads
`shelf/dates.py` on the changer's own branch: when `days_left` there does not
require two arguments, the record's `interface_change` is `change_not_made`,
the changer is not accepted, and no dependent is judged (`completed` and
`accepted` are `null`). Count such a trial apart from the arm comparison
instead of as dependent failures.

Notes a session writes in `notes/sN.md` are kept as evidence for review,
including notes on a branch Git refuses to merge. They never change the
verdict.

In the first two families, a session can meet its family's success definition
only by knowing its peers' tasks, which is what the arms differ in. Without
that knowledge, a new caller uses the name it can see, and a contradiction
task cannot tell whether its default has a partner: changing the default fails
every pair, and keeping it fails every task alone. The pilot's sessions all
hedged, which the earlier definition (pairs only) accepted in every arm.

Both of those families put the conflict in the task text: the rename session's
task names the rename. The second pilot's `awareness_only` arm, which shares
task text, therefore tied the `langspan` arm. In `discovered_change`, task
text is not enough. The changer's need for the change comes from the code it
works on, so only information from the work itself can tell a dependent:
LangSpan's notice of the changed signature in another worktree, or its alert
at the write of a call that no longer fits. Sharing every peer's task, as
`awareness_only` does, says that the changer uses `days_left`, not that it
changes it. A model can still guess that a helper that reads the clock must
change for `expired(items, today)`; the stand-in never guesses.

N is 2, 5 or 10. In `contradictory_defaults` the seed chooses the layout of
pairs, keys alone and at most one control task. At N = 2 it is one pair, two
keys alone, or one key alone and the control task; at N = 5 every layout has
at least one pair and one key alone (2 pairs and 1 alone, 1 pair and 3 alone,
or 1 pair, 2 alone and the control task); at N = 10 the five keys allow only 4
pairs, 1 key alone and the control task. The seed also chooses which features
or keys run, which side a key alone gets, and which session gets which task;
the same arguments always give the same assignment. `discovered_change` has
one changer and N - 1 dependents, chosen from nine features.
`python3 benchmarks/coordination/driver.py --arm worktrees
--family rename_vs_new_caller --sessions 5 --seed 7` prints the plan and every
prompt without running anything, and lists any option the arm ignores.

## Run one trial

A trial first checks the agent configuration (a real run) and the
`--langspan` binary (the LangSpan arm). A refused check writes `plan.json` and
a `record.json` with status `refused` and the reason, prepares nothing and
exits 2. Otherwise the trial runs the N sessions at once (or staggered, see
[Staggered start](#staggered-start)), each in its own checkout and through
the tool guard, and then:

1. commits whatever each session left uncommitted, and copies each checkout's
   final commit into the fixture repository as `bench/sN`;
2. copies every branch's `notes/` files into the run directory;
3. merges the branches in session order into a fresh integration worktree,
   keeping each merge's output, and for a merge Git refuses its conflicting
   hunks, in `merge-sN.log` before the merge is aborted;
4. runs the oracle there, and the fixture's own tests;
5. reads the coordinator's event log (LangSpan arm) for refusals, what
   followed them, interface notices delivered, and failed and recovered
   scans;
6. when the combined result fails, runs one repair round (see
   [Cost to a correct result](#cost-to-a-correct-result));
7. writes `record.json`, validated against `run-record.schema.json`.

### Cost to a correct result

A trial's own cost covers only its sessions. When their combined tree is
broken, someone still has to fix it, so the run's cost alone overstates how
cheap an arm that leaves broken trees is. The cost to a correct result adds
that repair.

For a trial whose combined result fails, the driver runs one repair round
after every session, the oracle and the coordinator have stopped:

1. It builds a repair tree, `work/repair` on branch `bench/repair`: every
   session's branch merged in session order, as in integration. A merge Git
   refuses is committed as Git left it, conflict markers and all, so every
   task's work is in the tree.
2. It starts one session there, asked to make the combined result succeed.
   Its prompt lists every task, the family's success definition and what the
   oracle found: each failed task's detail, the semantic conflicts and the
   end of the fixture tests' output, labelled as data and capped at 16 KiB.
3. It runs the oracle on the repaired tree, with every branch counted as
   merged and the changed helper read from the tree itself.

The repair is the same job in every arm, so it runs with the `worktrees`
arm's configuration everywhere: the same tools, denials, allowed list,
strict MCP configuration, auto memory off and tool guard as every session,
every file tool denied every session's checkout and the run's repositories,
and no LangSpan hooks, skill or peer text. Its time limit is
`--repair-time-limit` (default 1,200 seconds, at most 3,600).

No repair runs when the combined result passed (`not_needed`), or, with the
reason recorded (`not_run`), when the trial was refused or interrupted, the
combined check did not run, the tool check failed, or the changer made no
change (`change_not_made`, a trial counted apart). One round is all a trial
gets; a repair that fails leaves the result incorrect.

Read the record's `cost` this way:

- `run`, `repair` and `total` each give tokens (input, output, cache
  creation, cache read), turns and cost in US dollars. `run` is the sessions'
  figures, the record's own `tokens.total`, `turns` and `cost_usd`. `repair`
  is the repair session's, zero when no repair was needed. `total` is their
  sum.
- `correct_result` is `true` when the combined result passed, or its repair
  did. It is `null` when the repair session's tool check failed: that is a
  host configuration failure, and the repair's evidence and figures stay in
  `repair`.
- `tasks_accepted` counts the tasks accepted in the final tree, the repaired
  one when a round ran, and `cost_per_accepted_task` is the total cost over
  it.
- A figure a session did not report is `null`, and so is every total that
  needs it. A stand-in reports no cost, so a dry run's totals are `null`.

Compare arms by `correct_result` first, then by total cost and cost per
accepted task: an arm whose trials pass without repair spends only `run`,
while an arm that leaves broken trees spends `run` plus `repair` and can
still end with fewer accepted tasks.

`driver.py summarize` prints these per arm, from the records of any number
of trials:

```sh
python3 benchmarks/coordination/driver.py summarize /path/to/k1/*/
```

It groups trials by agent, family, session count, stagger and arm, so
staggered trials stay apart, and gives per group the trials, those refused,
those whose sessions' tool check failed (`tool_check_failed`), those whose
repair session's tool check failed (`repair_tool_check_failed`) and those
`change_not_made` (each counted, contributing no task, result or figure), those passed without
repair, repairs run and succeeded, correct results, tasks total and accepted,
the summed `run`, `repair` and `total` figures, the cost per accepted task,
`unreported`, the trials with no total cost, and `context_added`, the
context LangSpan added summed over the trials it counts (`null` outside the
LangSpan arm). For the stale-caller measure it gives `stale_writes_admitted`,
summed, `stale_fixed_after_s`, each trial's time listed rather than
averaged, and `stale_unfixed_trials`, the trials that ended with a stale
caller. A group figure is `null` when any trial it counts lacks it.
It reads version 8 records only.

### Staggered start

With all sessions started at once, a change reaches its peers only after they
have begun writing: in the second pilot, the rename landed about 15 seconds
after the new callers started writing, so LangSpan's notice reached each one
11 to 18 seconds after its first write. `--stagger SECONDS` starts the session
whose task changes an interface its peers use first, the rename session or
the changer, and every other session that many seconds later. The default is
0, all at once. A family without such a task (`contradictory_defaults`)
refuses the option.

A delayed session's time limit starts when the session does, so its runner
bound is `--time-limit` plus the stagger; the two together are at most 3,600
seconds, and the stagger at most 600. The record keeps the stagger as
`stagger_s`, and, per task in the LangSpan arm, `notice_before_write`: whether
the session's hook log shows LangSpan's notice of the change before the
session's first write of a file its task owns. A notice counts from a hook's
reply or from the between-step watcher's stderr when it woke the model (exit
2). A file written through Bash is not seen as a write, so the first Write,
Edit or MultiEdit can come after it.

```sh
python3 benchmarks/coordination/driver.py --arm langspan --family discovered_change \
  --sessions 5 --seed 1 --stagger 60 --langspan target/release/langspan \
  --run --output /tmp/k1-stagger
```

Choose a stagger longer than the changing session takes to make its change;
LangSpan tells peers of the change once it is on disk. In `discovered_change`,
use at least 60 seconds: in the third pilot the changer took 30 to 45 seconds
to edit `shelf/dates.py`, and a 30-second stagger started the dependents
before the change. A stagger favors the LangSpan arm's notices, so keep
staggered trials out of the comparison between arms.

### Dry run (no model)

```sh
cargo build --locked
python3 benchmarks/coordination/driver.py --arm langspan --family rename_vs_new_caller \
  --sessions 5 --seed 7 --langspan target/debug/langspan \
  --run --output /tmp/k1-dry
```

The stand-in agent makes one of the fixed edits its task defines:

- `--standin-mode follow` (the default) uses what its arm told it about its
  peers: the peer task text in the awareness-only prompt, or LangSpan's hook
  briefings and `langspan brief` in the LangSpan arm. When that information
  shows the peer work its task depends on, it makes the edit the task's
  details ask for; otherwise the uninformed edit. A dependent in
  `discovered_change` first reads `shelf/dates.py` and the package's caller,
  `shelf/orders.py`, as an agent reads how a helper is called. In the
  LangSpan arm, a session whose task depends on a peer's waits until
  LangSpan has told it every peer's task. Before it ends, it waits until
  every peer shows edits and, for a new caller or a dependent, until
  LangSpan's notice of the change has arrived; the session whose task changes
  the interface waits until every peer has ended. Each wait lasts at most
  until 30 seconds before the session's `--time-limit`, so a loaded machine
  slows a dry run instead of changing what its sessions learn. When
  LangSpan's notice or alert tells a session of the change only after its
  edit, it makes the informed edit then, as an agent told of a broken call
  fixes it.
- `--standin-mode ignore` always makes the uninformed edit.
- `--standin-mode hedge` makes the edit that holds up whatever peers do,
  without knowing: a contradiction task keeps the default and sets its value
  locally; a changer gives the new parameter a default, so old calls keep
  working; a dependent counts the days itself without `days_left`. In
  `rename_vs_new_caller` it is the uninformed edit.
- `--standin-mode workaround` makes the changer in `discovered_change` finish
  without the change, as the third pilot's did: `expired` subtracts two
  one-argument calls of `days_left`. Every other task makes the uninformed
  edit.

In a repair round, whatever the mode, the stand-in makes every task's
informed edit in the combined tree, in session order, and skips a
replacement whose result is already there. It does not resolve Git's
conflict markers, so a tree with a refused merge stays broken and its repair
fails.

The record's `standin_informed` says, per session, whether the information
changed its edit, before or after its first write. In the LangSpan arm the stand-in runs the installed hooks
with Claude Code's event shapes around each read and edit, so the run passes
through the real hooks, the coordinator and its SQLite event log. Every
stand-in prints a first line listing its arm's tools, which the tool guard
checks as it checks Claude Code's.

Unlike a host, the stand-in waits for each hook's decision past the hook's
configured timeout, for up to 30 seconds, and then ends the hook with every
process it started. A slow machine therefore cannot change a dry run's
result. A hook slower than its configured timeout is logged as `late`. Each
hook runs in a process group of its own, so when the runner stops the
stand-in with SIGTERM, SIGINT or SIGHUP, the stand-in first ends the running
hook's group. A SIGKILL sent to the stand-in alone can leave that hook
running; the runner always sends SIGTERM first.

The stand-in ends a hook's group with SIGTERM, and with SIGKILL only if
the group is still there 2 seconds later. In the LangSpan arm each hook
runs behind the hook log wrapper ([Where records go](#where-records-go)),
in a further group
that no signal to the wrapper's group reaches: the SIGTERM lets the
wrapper end that group, where SIGKILL at once would leave it running.

The test suite runs every mode and checks what reaches the sessions and that
the oracle tells the arms apart:

- in `worktrees`, no session is informed, and `follow` fails the combined
  test as `ignore` does wherever a task depends on a peer's;
- in `awareness_only`, the prompt's peer task text informs every session that
  depends on a peer in the first two families, and `follow` passes; in
  `discovered_change` it informs none, and `follow` fails;
- in `langspan`, LangSpan's briefings and notices inform them, `follow`
  passes in every family at N = 2, 5 and 10, every new caller's session
  receives LangSpan's in-flight notice of `load_config`, and every
  dependent's session receives LangSpan's notice of `def days_left(day,
  today)`, in its hook log and as an event in the coordinator's log;
- with `--stagger 15`, in `discovered_change` at N = 5, every dependent starts
  after the changer's edit, receives the notice before its first write
  (`notice_before_write` is `true`), and writes its module once;
- `ignore` fails in every arm, and `hedge` fails `contradictory_defaults` and
  `discovered_change` at N = 5 in every arm;
- in `discovered_change` at N = 5, in every arm, `workaround` and `hedge` fail
  the changer's task, and the record reports `change_not_made` with no
  dependent judged and no repair round; `follow` and `ignore` report the
  change as `made`;
- every trial whose combined result fails, and only such a trial, gets a
  repair round, except a `change_not_made` one: a broken call is repaired
  (`repair.succeeded` is `true`), while a tree with Git's conflict markers,
  or with the hedge's kept defaults, stays broken (`false`).

A dry run shows that the harness and the oracle work; it says nothing about
agents.

### Prepare a clean agent configuration

A real run needs a Claude Code configuration directory with no LangSpan in
it. LangSpan's user-level hooks, its skill or a LangSpan MCP server would reach
every session in every arm. A configuration used for LangSpan dogfooding has
them, and the driver refuses it, naming only what it found and where, for
example `hook in settings.json; skill in skills/langspan`.

Make a new one for the benchmark:

1. Choose a new, empty directory outside any repository, for example
   `~/.config/langspan-bench/claude`.
2. Start Claude Code once with that directory and sign in:

   ```sh
   CLAUDE_CONFIG_DIR=~/.config/langspan-bench/claude claude
   ```

   Run `/login` if it does not ask, then exit.
3. Install nothing into it: no `langspan install`, no hooks in its
   `settings.json`, no skill under `skills/`, and no MCP server
   (`claude mcp add`) that runs LangSpan.
4. Pass it to the driver as `LANGSPAN_BENCH_CLAUDE_CONFIG_DIR`. The driver
   reads `settings.json`, the `mcpServers` tables of `.claude.json` and
   whether `skills/langspan` exists, and nothing else there.

The LangSpan arm installs its own hooks and skill into each worktree's
ignored `.claude/`, so the configuration stays clean. The account's hosted
connectors need no change there: `--strict-mcp-config` keeps them out, and
the tool guard fails the trial if one appears.

### Real agents (operator-started)

Only an operator starts these; the test suite never does. Each session is
`claude -p PROMPT --permission-mode default --permission-prompts none
--output-format stream-json --verbose --tools ... --strict-mcp-config
--mcp-config '{"mcpServers": {}}' --settings '{"autoMemoryEnabled": false}'
--disallowedTools ... --allowedTools ...`, with
`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` in its environment, built by
`claude_argv` and `session_env` in `driver.py` with the limits of
[The same limits in every arm](#the-same-limits-in-every-arm). A repair
session's command is the `worktrees` arm's in every arm, built by
`repair_argv`. The allowed
list (`CLAUDE_TOOLS`) holds reads and edits under the session's checkout, the
fixture's tests and a few Git commands, plus a few `langspan` commands in the
LangSpan arm. No flag or tool approves a permission prompt, and the driver
refuses to build a command line with a flag that bypasses permissions or adds
a directory. `stream-json` keeps every message and tool call, in order, as the
session's transcript. `plan.json` keeps the driver's own command line and
each session's exact command (`agent_commands`), with the prompt left out.

Before the first full run, run one trial per arm at N = 2 and check that its
record's `tool_check` is `passed`. A host version that adds a tool the
`--tools` list cannot remove fails there, on its first event, before any
model turn.

1. [Prepare a clean agent configuration](#prepare-a-clean-agent-configuration).
2. Run each trial:

   ```sh
   export LANGSPAN_BENCH_CLAUDE_CONFIG_DIR=/path/to/claude-config
   # Optional: LANGSPAN_BENCH_CLAUDE (the claude executable), LANGSPAN_BENCH_MODEL (passed as --model)
   python3 benchmarks/coordination/driver.py --agent claude --arm awareness_only \
     --family contradictory_defaults --sessions 5 --seed 1 --time-limit 1200 \
     --run --output /path/outside/the/repository/k1/awareness_only-contradictory-5-1
   ```

   The LangSpan arm also needs `--langspan target/release/langspan`. Another
   arm records `--langspan` in `ignored_options` instead of using it.
3. Keep every record, refused, failed and timed-out ones included.

**Cost.** One trial starts N sessions, each bounded by `--time-limit` seconds
(default 1,200, at most 3,600), and a trial whose combined result fails one
repair session more, bounded by `--repair-time-limit`. The three families
under all three arms cost 3 × 3 × (2 + 5) = 63 sessions per repetition at
N = 2 and 5, and 153 with N = 10 as well, plus at most one repair session per
trial, 18 or 27.

### Where records go

`--output` must be a new directory outside the repository or under the ignored
`benchmarks/coordination/runs/`; it is created with mode `0700`. It holds:

| Path | Contents |
| --- | --- |
| `record.json` | The run record (`run-record.schema.json`) |
| `plan.json` | Tasks, prompts, branches, the requested model and ignored options, as the sessions received them, the driver's command line and, for real agents, each session's command |
| `integration.json` | The plan with each merge's outcome, the duplicate work found, and the notes and merge logs kept |
| `runner/runs.jsonl` | The runner's raw journal |
| `runner/RUN_ID/agent-sN.stdout` | Session N's transcript (`stream-json`, one JSON object per line: its tool list first, one result line per turn it ended, and the tool guard's verdict last); stderr beside it |
| `runner/RUN_ID/oracle.json` | The oracle's verdict per task |
| `runner/RUN_ID/notes/sN/`, `runner/RUN_ID/merge-sN.log` | Each branch's notes, and each merge's output |
| `repair/plan.json`, `repair/integration.json`, `repair/runner/` | A trial with a repair round: the repair session's plan (its prompt and, for real agents, its command), what the oracle judged the repaired tree by, and the runner's journal, the session's transcript and the oracle's verdict, laid out as `runner/` is |
| `events.jsonl`, `coordinator.log` | LangSpan arm: the coordinator's event log and its log |
| `hooks-sN.jsonl` | LangSpan arm: every hook call of session N, with its time, event, tool call ID, the file a file tool names, exit code and what LangSpan returned (a briefing's text, a notice, a refusal) |
| `work/` | The fixture repository, each session's checkout (in the LangSpan arm, worktrees of `work/shared`), the integration worktree and, after a repair round, the repair worktree `work/repair` |

To check what LangSpan said before an action, find the action's tool call in
the transcript and the same `tool_use_id` in `hooks-sN.jsonl`: its
`PreToolUse` line holds any refusal, its `PostToolUse` line the briefing
delivered after it, and `at` orders them against the other sessions' calls.
Hook context does not appear in `stream-json`, so the hook log is the only
copy of what LangSpan said.

Each line also keeps the first 2 KiB of the hook's stderr (`stderr`), where
LangSpan's hook logs its coverage and why it added nothing (issue #262),
and says whether the hook ran in the background (`background`)
and whether the wrapper ended it at its bound (`timed_out`). A foreground
hook still running after 900 s is refused (exit 2), as a failed safety
control must be. A background hook, such as LangSpan's between-step
watcher (`asyncRewake`), gets 60 s more than its entry's own `timeout`, so
Claude Code ends it first; if it still runs past that, the wrapper ends it
with exit 0 and refuses nothing (issue #288). Ending a hook ends every
process in its process group, so no child it started acts after the
wrapper returns; the wrapper also ends that group when the hook returns
by itself, and when the host ends the wrapper with `SIGTERM`, `SIGINT` or
`SIGHUP`. `SIGKILL` ends the wrapper alone and can leave the hook
running, so the stand-in sends `SIGTERM` first. The watcher's notice
goes to stderr, which the log does not keep; the coordinator's event log
records it as `interface_notices_told` with `via` `between_steps`.

Nothing is committed automatically. Review transcripts before committing any
record under `docs/evidence/`.

### What the record measures

| Field | Source |
| --- | --- |
| `tasks_accepted`, `integrated_tests` | The oracle on the merged tree. A task is accepted only when its family's success definition holds for it: a contradiction task whose own function works is still not accepted if its pair's shared default moved, or, for a key alone, if the default did not change to its value. Each task's `completed` says only whether its own check passed. `integrated_tests` passes only when every branch merged, every task is accepted, no semantic conflict was found and the fixture's tests pass |
| `interface_change` | `discovered_change`: `made` when `days_left` on the changer's own branch requires two arguments, `change_not_made` when it does not (a workaround, a default for the new parameter, or no change), and then no dependent is judged; `unknown` when the oracle did not run; `not_applicable` in the other families |
| `stale_callers` | `discovered_change`, every arm (issue #213), from each session's transcript, never from what an agent says: each `Write` and `Edit` call's input, replayed over the fixture's file; whether its result was an error (a refused write is not admitted); and the clock line (`langspan_bench_clock`, one monotonic clock for every session on the machine) the tool guard adds after each line of tool results. The oracle's definitions decide what counts. The change is the changer's first admitted write after which `days_left` requires two arguments. From then on, a dependent's file that names `days_left` in its old form is a stale caller, fixed at the admitted write that leaves it without one. `writes_admitted` counts the admitted dependent writes that leave such a file, `stale_files` the files stale at any point, `unfixed` those still stale at the end, and `fixed_after_s` the seconds from the change to the last stale caller fixed (`null` when none was stale or one stays stale). `items` lists each file becoming stale (`stale_at_change`, `stale_write`) or `fixed`, with seconds after the change. `unknown`, with a `reason`, when a transcript, its file or a clock line is missing, or a committed file (any Python file the session added, changed or removed, as Git lists them) is not what the replayed calls left, as after a write through a shell command; `not_applicable` in the other families and when the change was not made. The repair round's fixes are not counted |
| `merge_conflicts` | Merges Git refused, with their files |
| `semantic_conflicts` | Conflicts the oracle found in a tree Git merged cleanly: a call to a name the merged code lacks, or a pair whose changes both merged while one requirement was lost |
| `duplicate_work` | Branches that changed a code unit (a function or a file) another session's task owns |
| `tool_check`, and per task `tool_check` | The tool guard's verdict from each transcript's last line: `passed`, `failed` with the tools or MCP servers that made it fail, or `unknown` when the guard wrote none (a session stopped from outside). The record's is `failed` when any session's is, with `failed_sessions`, and `passed` only when every session's is |
| `results` (per task) | How many result lines the session's transcript has, one per turn it ended. More than one means something woke the session after its turn ended |
| `stops`, `questions` | Each session's last result line: a failed, timed-out or error result is a stop; a final answer ending in `?` is a question. Confirm them against the transcripts |
| `permission_denials` | Calls Claude Code refused, collected from every result line once per tool call, and classified from the tool input and Claude Code's own `permission_denied` event: `peer_checkout` for a call aimed at another session's checkout, the run's other repositories, or through Git at another session's branch, all refs or the stash; `outside_clone` for a call Claude Code refused as outside the session's own checkout, such as a scratch copy in `/tmp`; `not_allowed` for any other call outside the allowed list, such as `sed -i` or a compound command |
| `cost_usd`, `turns`, and per task `cost_usd`, `duration_ms`, `turns` | What each session reported over all its result lines: `num_turns` and `duration_ms` summed, `total_cost_usd` from the last line (it is cumulative); the totals only when every session reported them |
| `refusals` | LangSpan arm: each `write_refused` event, and whether the same session was admitted to those paths later (`admitted_later`) or not (`no_later_admission`) |
| `interface_notices` (per task) | LangSpan arm: how many interface notices the coordinator placed in the session's context, from its `inflight_delivered` (another worktree's uncommitted signatures), `interface_alerted` events and `interface_notices_told` events with `via` `between_steps` (issue #214); a briefing's `interface_notices_told` record is not counted; `null` in the other arms |
| `context_added`, and per task `context_added` | LangSpan arm: the context LangSpan's hooks placed in the sessions, read from the coordinator with `langspan status --all --json` after the sessions finish and before it stops. A record too large for one page comes as `part` records, which the driver joins by offset before reading it; parts that do not join make the read fail. Per task, the sum over every session registered from the task's checkout, a subagent's included: `turns` (a session start or resume, and each prompt), `bytes` (every briefing, code section and mid-turn note), `bytes_per_turn` (rounded down), `estimated_tokens_per_turn` and `largest_turn_bytes`. Tokens are an estimate, not a tokenizer's count: bytes divided by 4, rounded up, as `langspan status` gives them. A task is `null` when no session registered from its checkout, its figures are malformed or the coordinator's state was not read; a session registered with nothing added counts as zero. The record's `status` is `reported` with the tasks' sums (the largest single turn for `largest_turn_bytes`) when every task was measured, `partial` or `unknown` with `null` figures otherwise, and `not_applicable` with `null` per task in the other arms. A failed read leaves the trial running and its status `unknown` |
| `coordinator_scans` | LangSpan arm: each `scan_failed` and `scan_recovered` event, with the session whose worktree it concerns and the failure's detail |
| `tokens` | The sessions' reported usage, each session's `usage` summed over its result lines, and the sessions summed only when every session reported it; otherwise `partial` or `unknown` |
| `notice_before_write` (per task) | LangSpan arm, a new caller or a dependent: `true` when its hook log shows LangSpan's notice of the change in its context before its first `Write`, `Edit` or `MultiEdit` of a file its task owns, `false` when the notice came with that write or later, or never; `null` in the other arms, for other tasks, or when the session wrote no such file. A write through a shell command is not counted. The alert LangSpan gives at a write is read with the write's result, so it does not count as before |
| `repair` | The repair round: `status` (`not_needed`, `ran`, or `not_run` with its `reason`); for a round that ran, whether the oracle accepted the repaired tree (`succeeded`) and its verdict (`integrated_tests`, `tasks_accepted`, `fixture_tests`, `semantic_conflicts`), the sessions whose refused merge the tree keeps with conflict markers (`conflicted_merges`), and the repair session's figures, tool check and denials, taken as for a task; `null` when no round ran |
| `cost` | The cost to a correct result: `run`, `repair` and `total` tokens, turns and cost, `correct_result`, `tasks_accepted` in the final tree and `cost_per_accepted_task` (see [Cost to a correct result](#cost-to-a-correct-result)) |
| `stagger_s` | The `--stagger` the trial ran with; 0 when every session started at once |
| `auto_memory` | `off` for Claude Code sessions, which all run with auto memory off; `not_applicable` for the stand-in |
| `isolation` | `worktree` in the LangSpan arm, `clone` in the others |
| `elapsed_s`, `total_elapsed_s` | Monotonic time of the session phase, and of the whole trial, any repair round included |
| `versions` | The agent CLI's `--version`, the requested model (`LANGSPAN_BENCH_MODEL`, or `null` for the configuration's default), the models sessions reported, the LangSpan commit and binary digest, Git and Python |
| `ignored_options` | Options the run accepted but did not use: `--langspan` outside the LangSpan arm, `--standin-mode` with real agents, `LANGSPAN_BENCH_MODEL` with the stand-in |
| `evidence` | Paths, relative to the output directory, of the journal, plan, integration, oracle verdict, event logs, kept notes and merge logs. Each task names its `transcript` and, in the LangSpan arm, its `hook_log` |

A value the run did not measure is `null`, never zero. In the arms without
LangSpan, `refusals.status`, `coordinator_scans.status` and
`context_added.status` are `not_applicable`.

## Refuse against wait on a current holder

`holder_wait.py` compares the two conflict policies of
[ADR 0036](../../docs/adr/0036-coordination-policies.md) on the
`same-file-current-holder` scenario ([SCENARIOS.md](SCENARIOS.md)) as two
LangSpan arms: `langspan_refuse` (`policy set ... conflict refuse`, the
default) and `langspan_wait` (`policy set ... conflict wait`). Two sessions
share one checkout of the fixture, and each adds one setting to
`shelf/settings.py`; the second starts `--stagger-s` seconds after the first
(default 5). A run is accepted when both settings are in `DEFAULTS` and every
fixture setting is kept. Each run starts from a fresh fixture, coordinator
and state directory, and appends one line to `OUT/runs.jsonl` whatever
happened (`completed`, `failed`, `timed_out` or `error`): the policy table,
each session's exit status, the refusals, waits and settled outcomes from the
event log, and the elapsed time.

```sh
python3 benchmarks/coordination/holder_wait.py --arm langspan_refuse --agent claude \
  --runs 10 --out runs/holder --langspan target/release/langspan
python3 benchmarks/coordination/holder_wait.py --arm langspan_wait --agent claude \
  --runs 10 --out runs/holder --langspan target/release/langspan
python3 benchmarks/coordination/holder_wait.py --summarize runs/holder
```

`--summarize` reports per arm the runs by status, the runs that met a holder,
the stop rate among those (a session whose refusal was settled as `stopped`),
retries (`retried_admitted`), refusals, waits, and the median elapsed time and
time to the admitted retry, each with its sample size. `--agent claude` runs
real Claude Code sessions under the operator's own sign-in with the same
permission settings as `driver.py`: prompts stay on and nobody answers them.
`--agent standin` (the default) drives the real hook executable with a
scripted pair and makes no model call; its runs check the harness and are
labeled as such in the summary. No real-agent run is retained yet.

## Arms in owner manifests

The runner accepts the three arms above and three more. `no_tool` and
`coarse_claims` are the v0 names; manifests that use them still validate and
run. A `no_tool` manifest with one worktree per session is the `worktrees`
condition. `coarse_claims` is no longer a required comparison arm, so a
manifest without `worktrees` and `awareness_only` lists them in
`missing_comparison_arms`. `comparable_tool` stays optional.

## Owner manifests

The rest of this document describes `runner.py` on its own, with commands an
owner supplies.

## Configure, validate, then opt in

Make a private copy of `manifest.template.json` outside the checkout, or under the
ignored `benchmarks/coordination/manifests/` directory. Fill in actual command
arrays and existing working directories. Its placeholder paths are deliberately
not runnable. Native-host argument syntax is **owner-provided**: use the flags
supported by the exact installed host version, without changing its permissions
for the benchmark.

Validate without running anything, including version commands:

```sh
python3 benchmarks/coordination/runner.py /path/to/private-manifest.json
```

The result reports `validate_only` and `executed_commands: 0`. An invalid manifest
fails before creating result files. To execute the supplied commands:

```sh
python3 benchmarks/coordination/runner.py /path/to/private-manifest.json \
  --run --repetitions 5 --output /path/to/new-private-result-directory
```

The output directory must not already exist. It is created with mode `0700`;
journal and captured stream files use `0600`. Inside this Git repository, outputs
are permitted only beneath the ignored `benchmarks/coordination/runs/` directory.
No result, prompt, private checkout content, host transcript, or private manifest
is committed by the harness. Review retained files before sharing them.

`--run` authorizes the exact configured commands, including their normal side
effects. The runner always uses an argv array with `shell=False`. It performs no
shell interpolation, environment expansion in arguments, automatic install,
network setup, authentication, or Git reset. A shell runs only if the owner puts
that shell explicitly in `argv`, for example as the executable of a setup script.
It does not override the agent's normal host permissions or sandbox.

## Manifest and execution order

Each item has an ID, description, and named arms:

| Arm | Required configuration |
| --- | --- |
| `worktrees` | Same task and native-agent conditions, one worktree per session, without a coordination tool |
| `awareness_only` | The same, plus a fixed description of each peer's task in the prompt |
| `langspan` | Exact LangSpan commit, settings, native adapters and supported surfaces |
| `no_tool`, `coarse_claims` | v0 names, still accepted (see [Arms in owner manifests](#arms-in-owner-manifests)) |
| `comparable_tool` | Optional owner-configured installation of another coordination tool; never installed or enabled automatically |

A partial manifest is useful for debugging. Its raw rows list
`missing_comparison_arms`; it does not constitute the full three-arm comparison.
The `comparable_tool` arm is optional and requires an explicit, comparable setup. Do not attribute
landing, model, or integration-workflow differences to the file gate.

Each arm provides these phases, in order:

1. `versions`: sequential commands capturing exact host/tool/source versions in
   their stdout/stderr files. Omitted or unsuccessful capture stays `unknown`.
2. `setup`: optional sequential owner commands that prepare the trial. Use these
   for explicit resets or isolated fixtures. A failed setup prevents the agent
   phase and produces `setup_failed`; verification commands may still diagnose
   the state.
3. `agents`: 1–16 commands launched concurrently in independent process groups.
   A real coordination workload normally requires two native sessions; a
   single-process mechanics check does not establish coordination.
4. `checks`: sequential commands after all agent processes have finished or been
   bounded and stopped, even when an agent failed or timed out. Only a check
   explicitly marked `"assesses": "integrated_correctness"` contributes a
   correctness verdict. Other checks are diagnostics.

Arms execute sequentially; their declared order rotates across repetitions and
is retained as `arm_order`. This is simple counterbalancing, not statistical
randomization. Reset source and tool state explicitly between trials. Pin the
same task, agent/model versions, permissions, budgets and initial inputs across
arms. A fixed cwd does not create a fresh checkout or clean a dirty one.

Each command requires `id`, `argv`, and `cwd`. Relative working directories are
resolved against the manifest directory. Optional `timeout_seconds` defaults to
600 and must be positive and at most 3600. `max_output_bytes` defaults to 4 MiB
per stream and is bounded to 64 MiB. The manifest is limited to 1 MiB; argument
vectors to 64 KiB. Unknown keys and command strings in place of arrays are refused.

The runner supplies these environment values to commands:

- `LANGSPAN_BENCH_RUN_DIR`: this attempt's unique private artifact directory.
- `LANGSPAN_BENCH_RUN_ID`, `LANGSPAN_BENCH_ITEM_ID`, `LANGSPAN_BENCH_ARM`, and
  `LANGSPAN_BENCH_REPETITION`: identifiers for evidence collectors.

Other inherited environment values are not recorded. The journal retains the
executable basename and argv digest, not complete task arguments; keep the exact
owner manifest privately alongside its recorded SHA-256 for reproduction. Native
host/model versions come from the explicitly supplied version commands, not an
inference from an executable filename.

Start necessary long-lived coordinators separately, with their actual explicit
configuration, or manage their lifetime in an owner-supplied foreground command.
The runner closes stdin and does not offer an interactive TTY. It terminates
remaining descendants in the process groups it created when a command ends.
A detached background service launched by a command is not an automatically
managed benchmark service. Commands must not deliberately escape their process
group; escaped processes cannot be safely discovered and killed by this runner.

On timeout/output overflow/interrupt, it signals its own group with SIGTERM,
then SIGKILL after a bounded grace. Partial output is retained. Pipe draining is
bounded; incomplete cleanup is explicit. SIGINT/SIGTERM during the running phase
stops the current attempt and prevents subsequent experiments. Uncatchable
termination or interruption outside that phase may leave a start row without a
finish; retain it as an incomplete unknown attempt.

## Raw results and timing

`runs.jsonl` contains a flushed/fsynced `run_started` row before launch and a
`run_finished` row after the attempt. Join by `run_id` and count an attempt once.
**Never discard an unmatched start, failed run, timeout, launch error, output
limit, or interrupted run.** An unmatched start means incomplete observation,
not success or zero coverage. Reusing an existing output directory is refused.

Each final row contains:

- benchmark, item, arm, repetition and execution order; manifest/runner hashes;
- OS release, architecture, CPU count, Python version and monotonic resolution;
- per-command start/end, exit code or signal, status, elapsed nanoseconds,
  output paths, byte counts, truncation and process-cleanup state;
- exact version-command artifacts, and whether capture succeeded;
- `agent_phase_elapsed_ns` for concurrent commands and `elapsed_ns` for the whole
  attempt, including versions, setup, checks, and cleanup;
- explicit metrics, evidence references, and invalid/missing-evidence errors.

Timings use a monotonic clock. They are process elapsed time, not model latency,
token cost, awareness delay, or isolated hook overhead. Clock resolution and
hardware/environment metadata are descriptive, not a claim of comparable load.
`summary.json` counts execution outcomes and explicit correctness assessments;
it does not compute coverage percentages, model scores, or tail percentiles.
CLI exit 0 means the configured execution completed without a detected failure,
not that unknown product outcomes passed. Nonzero agent/setup outcomes, timeouts,
or failed designated correctness checks yield a nonzero CLI result.

## Unknown means unknown

Successful agent exit does not establish a correct integrated result. A passing
diagnostic is not automatically a task oracle. A designated correctness check
must assert that **both intended changes survive integration and the task's
behavior is correct**, rather than merely compiling an unchanged baseline.
A designated check's nonzero result is retained as failed; timeout/launch or
cleanup uncertainty stays unknown. All check output remains inspectable.

By default, the following are unknown with null values:

- host tool-call coverage and every coverage-state count;
- human intervention, unnecessary denial and missed-stale-write counts;
- integrated correctness;
- context receipt before the relevant decision, changed approach, human relay,
  and awareness delay.

To provide observed assessments, set the arm's `evidence_file` to a relative path
such as `assessment.json`. An owner-provided collector/check writes that file
inside `LANGSPAN_BENCH_RUN_DIR`. It may contain any of these entries:

```json
{
  "coverage": {
    "observed_tool_calls": 3,
    "counts": {
      "admitted": 1, "denied": 1, "unavailable": 0, "timeout": 0,
      "unsupported": 1, "late_observation": 0, "unattributed": 0, "unknown": 0
    },
    "evidence_refs": ["host-calls.jsonl", "coordination-events.jsonl"]
  },
  "human_interventions": {"count": 1, "evidence_refs": ["interventions.jsonl"]},
  "unnecessary_denials": {"count": 0, "evidence_refs": ["denial-review.json"]},
  "missed_stale_writes": {"count": 0, "evidence_refs": ["stale-write-review.json"]},
  "integrated_correctness": {"status": "passed", "evidence_refs": ["task-check.txt"]},
  "awareness": {
    "received_before_decision": true, "changed_approach": true,
    "human_relay": false, "delay_ms": 1250,
    "evidence_refs": ["decision-order.jsonl", "native-transcript.jsonl"]
  }
}
```

This is a **schema illustration with invented values**, not measured evidence.
Do not copy its counts into a real run. Counts must be explicitly measured;
required reference files must exist within the same run directory. Missing,
malformed or escaping evidence remains unknown and produces an error. Conflicting
check/evidence correctness assessments become unknown. Files' existence does not
verify their truth: `reported` means an owner-supplied assessment with retained
references. The runner does not infer semantic success from transcript text.

Coverage denominator is unique host tool calls observed at any phase, not disk
changes or process exits. Coverage-state counts may include multiple phases of
one call, so do not sum them into a denominator. `unattributed` counts content
change observations separately. Explicitly documented zero observed calls still
provides no meaningful coverage percentage. Unknown outcomes must appear next
to any collision or benefit claim.

## M4 and M5 remain separate

The runner can organize M5 process experiments. It cannot by itself establish
M4's two-person, two-machine proof or agent receipt before a decision. Preserve
**at least five M4 runs separately**, failures included, with both native
transcripts, both event logs, causal ordering, no human message relay, and the
selected action's outcome. Report each awareness delay against the contract's
**Target: 60 seconds**. Five runs cannot support a p99 claim. See
[SCENARIOS.md](SCENARIOS.md) for the required framing and ordering capture.

Likewise, small process or hook microbenchmarks cannot establish model quality,
correct integrated completion, reduced token cost, or a product benefit. The
mechanics tests below use real child processes to verify the harness only.

## Validation

```sh
cargo test --locked --test benchmark_harness
```

`tests/benchmark_harness.rs` runs both Python suites with the `langspan`
binary built from the checkout, so `cargo test` and CI run them on Linux and
macOS. To run one suite by hand:

```sh
LANGSPAN_BENCH_LANGSPAN=target/debug/langspan \
  python3 -B -m unittest discover -s benchmarks/coordination -p 'test_*.py' -v
```

`test_runner.py` uses temporary directories and real Python children. It
tests normal and failed output, parallel rendezvous, timeout/descendant
cleanup, repetition retention, output bounds, spawn errors, argv literalness,
the dry-run gate, metadata phases, and unknown/evidence-backed assessments.

`test_driver.py` runs the whole dry-run pipeline for every arm and all three
families at N = 2 and N = 5 with `follow` and `ignore`, the LangSpan arm at
N = 10 with both, `hedge` in `contradictory_defaults` and `discovered_change`
at N = 5 in every arm, `workaround` in `discovered_change` at N = 5 in every
arm, and `discovered_change` with `--stagger 15` in the
LangSpan arm, with the expectations listed under [Dry run](#dry-run-no-model). The
LangSpan arm goes through the real hooks, coordinator, Unix socket and SQLite
store. The suite also provokes a real stale-source refusal and checks that
the record and the hook log keep it. It checks the Claude Code command line,
the tool guard against scripts that list a cross-session tool, a hosted
connector's tool, an MCP server, a later tool list, an unlisted call or no
tool list, the record of a failed check, the seeded assignment and its
layouts, that each task line names what a peer needs, that no
`discovered_change` prompt in any arm names the change, prompts, each arm's
checkouts, every family oracle's success definition and the hedges it
refuses, that the third pilot's workaround cannot find the expired items and
is reported as `change_not_made`, notes and merge logs kept across a failed merge, the repair tree
with a refused merge's conflict markers, the repair session's command line, prompt and decision, the
run, repair and total figures and the per-arm summary, denial kinds, a
session with two result lines, scan events and interface notices, that every
Claude Code session has auto memory off and the check that refuses shared
memory, the stagger's validation and time limits, and the configuration
preflight's refusal and its record. None of this output is committed as benchmark data.
