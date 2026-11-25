# Codex protocol examples, not captured host events

These two JSON files were constructed for deterministic protocol tests from the
official Codex hook documentation and source inspected on 24 November 2026.
They are **not** a replay of a successful local Codex session. The installed CLI
reports `codex-cli 0.172.0-alpha.2`, but its runtime could not initialize in this
executor. See [the evidence record](../../docs/evidence/m3-codex/README.md).

The examples use invented session/tool identities, paths, model names and file
contents. They contain no account data or private transcript material.

Sources:

- [OpenAI hook documentation](https://learn.chatgpt.com/docs/hooks): native
  `apply_patch` pre/post events, `tool_input.command`, session/tool-call identity,
  hook decisions and normal permission flow.
- [Codex apply_patch handler](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/apply_patch.rs):
  canonical tool name and pre/post payload construction.
- [Codex tool output types](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/context.rs):
  `ApplyPatchToolOutput` supplies a string post-tool response.
- [Codex patch grammar](https://github.com/openai/codex/blob/main/codex-rs/apply-patch/src/parser.rs):
  file headers and move destinations.

The source links identify the inspected public `main` branch, not a verified
source tag for the installed alpha executable. A request for the exact tag's
source could not be retrieved. These fixtures establish the implementation's
tested contract; real events from the intended CLI version must still be
captured before claiming native-host acceptance.
