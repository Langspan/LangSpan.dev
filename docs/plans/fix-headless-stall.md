# Plan: headless Claude Code must not wait on the between-step watcher (#534)

## What the host does

The recorded benchmark run (`discovered_change-n2-s1-langspan`, run
`fdc30210c413444799b7999c8059f354`, Claude Code 2.1.332) shows `claude -p
… --output-format stream-json` emitting only `tool_progress` heartbeats for
`UserPromptSubmit` from 30 s to 1,170 s, with no `system/init` line and no
tool call. The synchronous prompt hook had answered in 63 ms
(`hooks-s1.jsonl`).

The hooks reference documents `asyncRewake` as "runs in the background and
wakes Claude on exit code 2", and says only that `-p` kills a still-running
async hook at teardown. It has no `-p` statement for `asyncRewake`, and no
documented hook input or environment variable that tells `-p` apart.

The installed 2.1.332 bundle decides it this way (from the shipped
JavaScript, minified names):

- `zs = !nonInteractive() || hasStreamingInput()`, and a command hook is
  backgrounded when `e.async || (e.asyncRewake && zs)`. So an `asyncRewake`
  hook runs in the foreground, and the turn waits for it, in a
  non-interactive session without streaming input: `claude -p "prompt"`.
  The Agent SDK passes streaming input, so its sessions background it.
- At start, Claude Code sets `CLAUDE_CODE_ENTRYPOINT` to `cli` (interactive)
  or `sdk-cli` (non-interactive), unless the variable is already set. A set
  value is kept, except that `cli` becomes `sdk-cli` in a non-interactive
  session. Hook commands inherit the environment.

So `CLAUDE_CODE_ENTRYPOINT=cli` is the one value that proves the session is
interactive, where the watcher runs in the background. `sdk-cli` means
non-interactive. `sdk-ts`, `sdk-py` or any other value may have been
inherited by a `claude -p` that an SDK session started, which would still
run the watcher in the foreground.

## Fix (the simplest correct one)

The watcher watches only when its own environment has
`CLAUDE_CODE_ENTRYPOINT=cli`. Otherwise it exits 0 at once, silent, with a
log detail that names the value. The watcher's only cost in a session that
backgrounds it is lost between-step notices; the prompt and tool hooks still
brief that session. No install, protocol or wire change.

Rejected:

- Reading the host's argv for `-p` / `--input-format stream-json`:
  `process::host_parent` states that process arguments and environment are
  never inspected (they can hold the prompt), and `-p` is not the only cause
  of non-interactive mode.
- Waiting for the transcript to show the turn started: it still delays each
  headless prompt and couples to the transcript format.
- Detaching a child: a detached child cannot wake the session; only the
  hook's own exit 2 does.

Known gap, kept for a follow-up: SDK sessions (`sdk-ts`, `sdk-py`) lose
between-step notices, because their value cannot be told from one inherited
by a nested `claude -p`.

## Changes, test first

1. `src/between_steps.rs`: unit test first. `watch_within` takes the
   entrypoint value; a row each for `None`, `sdk-cli`, `sdk-ts`, `""` and
   `CLI` ends at once with exit 0, no stderr and the reason, before any ask.
   Existing unit tests pass `Some("cli")`. Then a pure
   `runs_in_background(entrypoint)`, checked first in `watch_within`;
   `watch` reads the variable. Ratchet baseline: `--tighten` / `--record`
   for `src/between_steps.rs`.
2. `tests/between_steps.rs`: the hook processes get
   `CLAUDE_CODE_ENTRYPOINT=cli`, so the suite is hermetic when an agent's
   own SDK or `-p` session runs it, and interactive delivery stays covered by
   the existing real-coordinator tests. One process test (it checks that the
   binary reads its own environment): a watcher started with `sdk-cli`
   against a running coordinator, for a registered session, exits 0 in well
   under the start grace.
3. `tests/live/claude_headless.rs` plus a `[[test]]` entry in `Cargo.toml`:
   `#[ignore]` and gated on `LANGSPAN_LIVE_CLAUDE=1`, for the owner. It
   installs LangSpan's Claude Code hooks in a temporary repository, runs
   `claude -p … --output-format stream-json --verbose` with
   `CLAUDE_CODE_ENTRYPOINT` removed, and asserts that `system/init` arrives
   within 5 s of the `SessionStart` hook response and that the run ends.
4. Docs: ADR 0002's between-step bullet and its consequences record the
   host behavior and the rule. `docs/usage/automatic-peer-briefings.md`
   says where notices are off. `docs/IMPLEMENTATION_STATUS.md` records the
   fix and that the live run is pending.

5. `benchmarks/coordination/test_driver.py` (added after admission): the
   `WatcherDryRun` test starts the watcher with the runner's own
   environment and expects the #288 wrapper to time it out. The watcher's
   `Popen` gets `CLAUDE_CODE_ENTRYPOINT=cli`, the interactive host it
   stands in for, so the test is hermetic again and keeps its coverage.

The entrypoint check sits in `watch_within` after the event is parsed and
validated, so malformed input reports the same in any environment and
`tests/golden/cli/options.txt` stays unchanged.

## Shared contracts touched

None on the wire. `Cargo.toml` gains one `[[test]]` entry. The watcher's
log detail is not pinned by a golden file.

## Criteria of #534

- Real-host test: written under `tests/live`; the owner runs it (live runs
  are not allowed in the fast suite).
- Interactive notices: covered by the existing real-coordinator tests with
  `cli`; no live interactive run retained.
- Benchmark LangSpan arm completes a trial: needs a live benchmark run by
  the owner, so this PR says "Refs #534" unless that run is done.
- ADR: updated in this PR.
