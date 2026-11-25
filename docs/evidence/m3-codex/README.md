# M3 Codex adapter: contract verification and pending host acceptance

**Recorded:** 24 November 2026. **Installed host:** `codex-cli 0.172.0-alpha.2`,
Linux `x86_64-unknown-linux-musl`. The version was read from `codex --version`
and the installation's package metadata. The executable digest and the two
startup probes are retained in [host-probes.json](host-probes.json).

## Verified native surface

The [official hook guide](https://learn.chatgpt.com/docs/hooks) defines project
`.codex/hooks.json`, `PreToolUse`/`PostToolUse` for canonical `apply_patch`, and
`tool_input.command`. Empty stdout with exit 0 leaves the host's normal
permission flow in control; a conflict can return JSON `permissionDecision:
"deny"`. Hook trust remains a separate user review in Codex. Linked worktrees
can inherit the main checkout's hooks, so the adapter resolves the event's
working directory to its physical workspace.

The [native handler source](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/apply_patch.rs)
and [output types](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/context.rs)
support the payload mapping. These public `main` sources are reference material,
not proof of the installed alpha build's behavior. The exact version's source
tag could not be retrieved.

## What the adapter covers

Local patch additions, updates, deletions and moves are admitted as one complete
path set. A move includes both source and destination. Unknown directives,
remote-environment selectors, shell wrappers and paths outside the resolved
workspace receive no decision and explicit unsupported coverage. Subagent
envelopes carrying `agent_id` are also unsupported: those may carry a parent
session ID and need a separately verified child-identity mapping. Native patch
syntax remains the host's responsibility; LangSpan neither applies nor rewrites
the patch. Patch bodies are limited to 4 MiB, complete hook envelopes to 8 MiB,
and scope to 64 paths.

The post adapter recognizes the native successful patch response string. A
missing or unfamiliar result is `unknown`; it does not manufacture a successful
observation. No `PostToolUseFailure` mapping is claimed for Codex. Cancellation,
failure or permission rejection without a supported post callback leaves the
operation unobserved and its admission subject to normal expiry.

## Observed verification

- `cargo test --locked --lib codex::tests` passed all **12** focused tests using
  pinned Rust 1.100.0. They cover complete multi-file scope, move destinations,
  header-looking patch content, unsupported shapes, bounds, Unicode and CRLF;
  source-derived pre/post identity; changed patch content; missing identities;
  unknown post results; and inherited hooks in sibling and nested linked
  worktrees. Retry IDs are checked against independently calculated digest
  vectors and are distinct from Claude Code IDs.
- Before shared-module integration, the five standalone grammar tests passed
  after an observed failing stub test.
- A mutation in an isolated source copy omitted the move destination. The scope
  test failed because the destination was absent. The repository implementation
  was not changed for that mutation run.
- `cargo test --locked --test codex --no-run` compiled all **10** integration
  cases. These exercise the real hook executable and daemon with source-derived
  events: admission and duplicate posts, cross-adapter conflict and unrelated
  work, changed retry input, move destinations, late posts, sibling and nested
  worktrees, post without pre, stale conflict replay, and post scope/input
  mismatch. Compilation does not establish their runtime behavior.
- `cargo clippy --locked --lib --test codex -- -D warnings` passed.
- The first real-daemon Codex test could not reach its assertions. The daemon
  failed with `bind local Unix socket: Operation not permitted (os error 1)`.
  The integration suite remains enabled for CI; this is not a passing result.

Further shared-module and integration verification is recorded in the PR/check
results. [Protocol fixtures](../../../fixtures/codex-contract/README.md) are
explicitly source-derived examples, not captured native events.

## Acceptance still required

Both live probes failed before a model request with `failed to initialize
in-process app-server client: Read-only file system (os error 30)`. The second
used the documented scratch `sqlite_home` and `log_dir` overrides and failed
the same way. No credentials or private session transcripts were read, and no
persistent Codex configuration was changed.

This evidence does **not** establish M3 native-host acceptance. Still required:
capture the real pre/post shapes from the pinned host; run independent Claude
Code and Codex sessions in one checkout; demonstrate conflict denial, unrelated
work, permission coexistence and post observation; repeat with a linked
worktree pair. Retain the host tool results and LangSpan events, including
failures. The current executor has no Claude Code executable either.
