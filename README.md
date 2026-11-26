<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/brand/langspan-lockup-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/brand/langspan-lockup-light.svg">
  <img alt="LangSpan" src="docs/assets/brand/langspan-lockup-light.svg" width="288" height="80">
</picture>

# Your agents. On the same page.

**For developers who run several coding agents on one repository, where each agent sees only its own part of the work.** LangSpan brings each agent's related work into the others' sessions, refuses supported overlapping or stale edits, and checks combined changes against the exact Git tree. Keep your tools. Connect their work.

[![Latest stable release](https://img.shields.io/github/v/release/Langspan/LangSpan.dev?style=flat-square&labelColor=151513&display_name=tag&sort=date&color=A34A34)](https://github.com/Langspan/LangSpan.dev/releases/latest) [![Rust checks on main](https://github.com/Langspan/LangSpan.dev/actions/workflows/check.yml/badge.svg?branch=main&event=push)](https://github.com/Langspan/LangSpan.dev/actions/workflows/check.yml) [![Core license](https://img.shields.io/github/license/Langspan/LangSpan.dev?style=flat-square&labelColor=151513&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/LICENSE) [![Supported release platforms: macOS and Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-3F3E44?style=flat-square&labelColor=151513)](https://langspan.dev/#start) [![Project website](https://img.shields.io/badge/website-langspan.dev-3F3E44?style=flat-square&labelColor=151513)](https://langspan.dev/)

Claude Code holds `src/gate.rs`. A real Codex session tries to edit the same function. LangSpan tells Codex about the hold, then refuses the overlap:

```text
- claude-code session a990eb45 in this checkout on branch "main": editing now: "src/gate.rs"; …
Command blocked by PreToolUse hook: LangSpan coordination conflict. … ["src/gate.rs"] held by session claude-code.a990eb45-… : "src/gate.rs" symbol "name"; Choose other work or retry this edit later.
```

Codex can choose other work or retry when the hold ends. Shortened from the [recorded Codex session of 25 November 2026](docs/evidence/codex-real-host/README.md#2-a-conflicting-edit).

## Quickstart

1. [Download the archive](https://github.com/Langspan/LangSpan.dev/releases/latest) for macOS or Linux, then [verify and extract it](docs/install.md).
2. Close Claude Code. From the extracted folder, install the binary and run setup:

   ```sh
   mkdir -p "$HOME/.local/bin"
   install -m 0755 langspan "$HOME/.local/bin/langspan"
   export PATH="$HOME/.local/bin:$PATH"
   langspan setup
   ```

   Setup shows one plan and asks once. It connects the agent hosts it finds, starts the coordinator as a login service, and runs `langspan doctor`. In Codex, review and trust the hooks with `/hooks`.

3. See coordination happen. In one Git repository, open two agent sessions, such as Claude Code and Codex, or two Claude Code sessions.
   1. Ask the first session to change a function, and leave its edit waiting at the permission prompt.
   2. Ask the second session to change the same function. LangSpan names the first session's work in the second session's context and refuses the overlapping edit, as in the example earlier.
   3. After the first edit completes or its reply ends, the hold is released and the second session can retry.
4. Watch both sessions in LangSpan Live, the read-only dashboard:

   ```sh
   langspan dashboard --open
   ```

   To see the page before you run any agents, use `langspan dashboard --demo`, which shows labeled, simulated data.

[Installation options](docs/install.md): verify a download, build from source, or set up a single project.

<a id="get-started"></a>
<a id="verify-the-binary"></a>
<a id="verify-a-download"></a>
<a id="build-from-source"></a>
<a id="connect-a-native-agent"></a>
<a id="one-project"></a>

## What you get

<a href="https://langspan.dev/#edit-coordination">
  <picture>
    <source media="(max-width: 600px)" srcset="https://langspan.dev/assets/product/langspan-overview-compact.svg">
    <source media="(prefers-reduced-motion: reduce) and (prefers-color-scheme: dark)" srcset="https://langspan.dev/assets/product/langspan-card-dark-still.svg">
    <source media="(prefers-reduced-motion: reduce)" srcset="https://langspan.dev/assets/product/langspan-card-light-still.svg">
    <source media="(prefers-color-scheme: dark)" srcset="https://langspan.dev/assets/product/langspan-card-dark.svg">
    <img src="https://langspan.dev/assets/product/langspan-card-light.svg" alt="LangSpan: in a shared checkout Agent A holds src/auth/session.ts, Agent B’s overlapping request is refused, and B independently chooses tests/session.test.ts." width="1300">
  </picture>
</a>

*An authored illustration of the same flow. [Try the interactive example](https://langspan.dev/#edit-coordination) or view the [static version](https://langspan.dev/assets/product/langspan-card-light-still.svg).*

- **Briefings.** At session start and at each new prompt, an agent gets a short summary of related work: other sessions' tasks, the files they're editing and their recent edits, including uncommitted work and linked worktrees.
- **Edit coordination.** In a shared checkout, the hook refuses a supported edit that overlaps another session's hold on a file or symbol, or that starts from a stale read. The agent can reread, retry or choose other work. A workspace can instead [wait briefly for an active hold](docs/usage.md#coordination-policies); this is off by default.
- **Interface notices.** A local source index notices changed signatures, renames and removals in Rust, Python, TypeScript and JavaScript, Go, Java and C#, and tells related sessions. It matches names, not resolved types.
- **Checks on the combined tree.** Record a check against an exact Git tree, test combined branches, and land only on a matching pass.
- **Commits in a shared checkout.** `langspan commit --mine` commits only the session's own files and hunks.
- **Teammates on other machines.** Opt-in sharing through your Git remote publishes work metadata, not source contents. Remote activity informs agents and never blocks a local edit.
- **CLI and MCP.** Agents and people reach the same operations through hooks, the `langspan` CLI and an MCP server.

## Supported agents

| Host | Status |
| --- | --- |
| Claude Code | Recorded on a real host: edits, reads and briefings |
| Codex | Recorded on a real host: edits, stale-source refusal, linked worktrees and `commit --mine` |
| Cursor | Built from documentation; not yet verified on a real host |
| Antigravity | Corrected against a first real run; a repeat run is pending |

Setup connects Claude Code, Codex and Cursor when it finds them. To add Antigravity, name it with `--hosts` ([Antigravity setup](docs/usage.md#antigravity)). [Host evidence and versions](docs/reference-overview.md#current-availability) list what each run covered.

## Limits

- LangSpan runs on macOS and Linux. Linux needs glibc 2.35 or later. The macOS binary isn't signed with an Apple Developer ID. An install script and a Homebrew formula aren't published yet.
- Coordination is cooperative. It covers edits that go through the supported hooks; writes outside the hooks, and edits a host approves after a hold ends, can bypass it. Your agent host's permissions and your review still apply.
- A hold doesn't establish that changes in different files are compatible. Interface notices are advisory.
- The goal is compatible parallel work with less repair and manual relay. That outcome isn't measured yet; the [coordination benchmark](benchmarks/coordination/README.md) has no published result.

## How it works

One Rust executable runs the local coordinator, the hooks, the CLI and a stdio MCP server, with history in a local SQLite database. You start your agents as usual; their hooks ask the coordinator before supported edits and receive briefings at session start and on each prompt. No account or hosted service is required. [ADR 0001](docs/adr/0001-local-coordination.md) and the [architecture overview](docs/architecture.md) explain the design.

## Documentation

- [Install and verify](docs/install.md)
- [Usage guide](docs/usage.md): setup, hooks, CLI, [MCP](docs/usage.md#mcp) and configuration
- [Detailed product reference](docs/reference-overview.md): what agents see, and evidence per feature
- [Implementation status](docs/IMPLEMENTATION_STATUS.md)
- [Security model](docs/security/threat-model.md)
- [Coordination benchmark](benchmarks/coordination/README.md)

## Contributing

[![Rust (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2Frust-toolchain.toml&query=%24.toolchain.channel&label=Rust&color=3F3E44&logo=rust)](https://github.com/Langspan/LangSpan.dev/blob/main/rust-toolchain.toml) [![MCP SDK (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.rmcp.version&label=MCP%20SDK&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml) [![rusqlite (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.rusqlite.version&label=rusqlite&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml) [![Tree-sitter (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2Fcrates%2Flangspan-graph%2FCargo.toml&query=%24.dependencies%5B%22tree-sitter%22%5D&label=Tree-sitter&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/crates/langspan-graph/Cargo.toml) [![Tokio (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.tokio.version&label=Tokio&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml)

<sub>Dependency badges track the pins on main.</sub>

[Contributing](CONTRIBUTING.md) · [Engineering guide](docs/engineering.md) · [Agent context](AGENTS.md) · [Security](SECURITY.md) · [Apache-2.0](LICENSE)
