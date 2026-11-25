# Coordination benchmark v0 harness

**Infrastructure, not a product-performance result.** This standard-library Python
runner starts M5 alongside M3. It executes explicitly supplied commands, retains
every attempt, and leaves unmeasured outcomes unknown. It neither chooses models
nor installs tools. No live agent benchmark has been run in the current executor:
Codex fails before its first model request, Claude Code is absent, and Unix
socket listeners are denied. See [M3 evidence](../../docs/evidence/m3-codex/README.md).

The scope follows the build contract's M5 milestone and the M3 capability
review. The contract remains labelled proposed; this harness does not resolve its owner decisions or claim
M3/M4 acceptance.

## Files

- `runner.py`: validate-only by default; opt-in real-process execution.
- `manifest.schema.json`: command manifest v1.
- `manifest.template.json`: owner-filled template for the three required arms.
- `records.schema.json`: raw per-item/per-arm/per-repetition JSONL journal v1.
- `SCENARIOS.md`: workload, ordering, and outcome evidence requirements.
- `test_runner.py`: real local process fixtures; no mocked model or host event.

Python 3.9+ on POSIX is required for live runs. There are no package dependencies.
The commands below assume the repository root.

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
| `no_tool` | Same task and native-agent conditions, without a coordination tool |
| `coarse_claims` | An explicitly described baseline using coarse file claims |
| `langspan` | Exact LangSpan commit, settings, native adapters and supported surfaces |
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
python3 -m unittest discover -s benchmarks/coordination -p test_runner.py -v
python3 -m py_compile benchmarks/coordination/runner.py benchmarks/coordination/test_runner.py
```

The deterministic fixtures use temporary directories and real Python children,
not models, mock agents, a fake coordinator, or sockets. They test normal and
failed output, parallel rendezvous, timeout/descendant cleanup, repetition
retention, output bounds, spawn errors, argv literalness, the dry-run gate,
metadata phases, and unknown/evidence-backed assessments. The output of these
fixture tests is not committed as product benchmark data.
