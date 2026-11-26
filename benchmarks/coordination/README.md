# Coordination benchmark harness

**Infrastructure, not a product-performance result.** No real agent run has
been recorded through this harness yet. It compares three conditions for N
parallel agent sessions on seeded tasks ([issue #172](https://github.com/Langspan/LangSpan.dev/issues/172)):

| Arm | Sessions get |
| --- | --- |
| `worktrees` | One Git worktree each and their task, nothing about peers |
| `awareness_only` | The same, plus a fixed list in the prompt naming each peer session and its task. No LangSpan hooks |
| `langspan` | The same worktrees with LangSpan's Claude Code project hooks and skill installed, and one coordinator for the run |

Two parts do the work. `driver.py` runs one seeded trial: it prepares a fresh
fixture repository and the worktrees, sets up the arm, and writes one run
record. `runner.py`, the generic runner, launches the sessions and the oracle
for it, bounds them and keeps the raw journal. The runner also still runs
owner-written manifests, as described under [Owner manifests](#owner-manifests).

## Files

- `driver.py`: one trial of an arm, task family, session count and seed.
- `tasks.py`: the two task families, their prompts and what the oracle checks.
- `fixture/`: the fixture repository, a small Python package with its own tests.
- `oracle.py`: the combined-integration oracle, run on the merged result.
- `standin_agent.py`: the dry-run agent; it makes a fixed edit and no model call.
- `run-record.schema.json`: the driver's run record v1.
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

Each task is a prompt one session receives. The oracle judges the result
after every session's branch is merged, never a branch alone.

| Family | Sessions | What can go wrong | The oracle accepts |
| --- | --- | --- | --- |
| `rename_vs_new_caller` | One session renames `parse_config` to `load_config` across the package; each other session adds a feature module that must call the parser | A new caller uses the old name: every branch passes alone, and the merge breaks | The rename done, every feature working on the merged tree, no call to a name the merged `shelf/config.py` lacks |
| `contradictory_defaults` | Pairs of sessions; each is asked to change the same default in `shelf/settings.py` to a different value for its own caller. With N odd, one session gets an unrelated control task | Both change the default: Git refuses the second merge, or one requirement is silently lost | Per pair: both callers get the value their task asked for, or a session reported the contradiction in `notes/conflict-sN.md`: one note names the setting and both values the pair asked for |

N is 2, 5 or 10. The seed chooses which features or pairs run when N is below
the pool, and which session gets which task; the same arguments always give
the same assignment. `python3 benchmarks/coordination/driver.py --arm worktrees
--family rename_vs_new_caller --sessions 5 --seed 7` prints the plan and every
prompt without running anything.

## Run one trial

A trial runs the N sessions at once, each in its own worktree, and then:

1. commits whatever each session left uncommitted on its branch;
2. merges the branches in session order into a fresh integration worktree,
   recording each merge Git refuses and its files;
3. runs the oracle there, and the fixture's own tests;
4. reads LangSpan's event log (LangSpan arm) for refusals and what followed;
5. writes `record.json`, validated against `run-record.schema.json`.

### Dry run (no model)

```sh
cargo build --locked
python3 benchmarks/coordination/driver.py --arm langspan --family rename_vs_new_caller \
  --sessions 5 --seed 7 --langspan target/debug/langspan \
  --run --output /tmp/k1-dry
```

The stand-in agent makes its task's fixed edit (`--standin-mode naive`, the
default, or `coordinated`). In the LangSpan arm it runs the installed hooks
with Claude Code's event shapes around each read and edit, so the run passes
through the real hooks, coordinator and SQLite event log. A dry run shows
that the harness works; it says nothing about agents.

### Real agents (operator-started)

Only an operator starts these; the test suite never does. Each session is
`claude -p PROMPT --permission-mode default --output-format json
--allowedTools ...`, with the tool list in `driver.py` (`CLAUDE_TOOLS`): reads
and edits under the session's worktree, the fixture's tests and a few Git
commands, plus a few `langspan` commands in the LangSpan arm. No flag or tool
approves a permission prompt; a call outside the list is refused and counted.

1. Choose a Claude Code configuration directory that is signed in and has no
   LangSpan user-level hooks, skill or MCP entry. The driver refuses a
   configuration with LangSpan hooks or the skill, because they would put
   LangSpan into every arm.
2. Run each trial:

   ```sh
   export LANGSPAN_BENCH_CLAUDE_CONFIG_DIR=/path/to/claude-config
   # Optional: LANGSPAN_BENCH_CLAUDE (the claude executable), LANGSPAN_BENCH_MODEL (passed as --model)
   python3 benchmarks/coordination/driver.py --agent claude --arm awareness_only \
     --family contradictory_defaults --sessions 5 --seed 1 --time-limit 1200 \
     --run --output /path/outside/the/repository/k1/awareness_only-contradictory-5-1
   ```

   The LangSpan arm also needs `--langspan target/release/langspan`.
3. Keep every record, failed and timed-out ones included.

**Cost.** One trial starts N sessions at once, each bounded by `--time-limit`
seconds (default 1,200, at most 3,600). Both families under all three arms
cost 3 × 2 × (2 + 5) = 42 sessions per repetition at N = 2 and 5, and 102
with N = 10 as well.

### Where records go

`--output` must be a new directory outside the repository or under the ignored
`benchmarks/coordination/runs/`; it is created with mode `0700`. It holds:

| Path | Contents |
| --- | --- |
| `record.json` | The run record (`run-record.schema.json`) |
| `plan.json` | Tasks, prompts and branches, as the sessions received them |
| `runner/runs.jsonl` | The runner's raw journal; per-session stdout and stderr beside it |
| `runner/RUN_ID/oracle.json` | The oracle's verdict per task |
| `events.jsonl`, `coordinator.log` | LangSpan arm: the event log and the coordinator's log |
| `work/` | The fixture repository, each session's worktree and the integration worktree |

Nothing is committed automatically. Review transcripts before committing any
record under `docs/evidence/`.

### What the record measures

| Field | Source |
| --- | --- |
| `tasks_completed`, `integrated_tests` | The oracle on the merged tree; `integrated_tests` passes only when every branch merged, every task is accepted, no semantic conflict was found and the fixture's tests pass |
| `merge_conflicts` | Merges Git refused, with their files |
| `semantic_conflicts` | Conflicts the oracle found in a tree Git merged cleanly: a call to a name the merged code lacks, or a pair whose changes both merged while one requirement was lost and nobody reported it |
| `duplicate_work` | Branches that changed a code unit (a function or a file) another session's task owns |
| `stops`, `questions`, `permission_denials` | Each session's `claude -p` result: a failed, timed-out or error result is a stop; a final answer ending in `?` is a question; refused tool calls come from `permission_denials`. Confirm them against the transcripts |
| `refusals` | LangSpan arm: each `write_refused` event, and whether the same session was admitted to those paths later (`admitted_later`) or not (`no_later_admission`) |
| `tokens` | The sessions' reported usage, summed only when every session reported it; otherwise `partial` or `unknown` |
| `elapsed_s`, `total_elapsed_s` | Monotonic time of the session phase, and of the whole trial |
| `versions` | The agent CLI's `--version`, the models a session reported, the LangSpan commit and binary digest, Git and Python |

A value the run did not measure is `null`, never zero. In the arms without
LangSpan, `refusals.status` is `not_applicable`.

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

`test_driver.py` runs the whole dry-run pipeline for every arm and both
families at N = 2 and N = 5 (naive stand-in edits, which the oracle must catch)
and at N = 5 with coordinated edits (which it must accept). The LangSpan arm
goes through the real hooks, coordinator, Unix socket and SQLite store. It
also provokes a real stale-source refusal and checks that the record keeps it
and what followed, and checks the Claude Code command line, the seeded
assignment, prompts, duplicate detection and the contradiction report. None
of this output is committed as benchmark data.
