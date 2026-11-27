<h1><picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/brand/langspan-lockup-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/brand/langspan-lockup-light.svg">
  <img alt="LangSpan" src="docs/assets/brand/langspan-lockup-light.svg" width="288" height="80">
</picture></h1>

LangSpan coordinates coding agents that work in one repository at the same time. Each agent learns what the others are editing, and LangSpan refuses edits that overlap. Keep your tools. Connect their work.

**Recorded** · Codex session, 25 November 2026 · [Full text](https://github.com/Langspan/LangSpan.dev/blob/e35aed3/docs/evidence/codex-real-host/README.md#2-a-conflicting-edit)

```text
editing now: "src/gate.rs"
Command blocked by PreToolUse hook: LangSpan coordination conflict.
```

## Quickstart

Close Claude Code. To install the latest release on macOS or Linux, run:

```shell
curl -fsSL https://github.com/Langspan/LangSpan.dev/releases/latest/download/install.sh | sh
```

The script checks the download against the release's `SHA256SUMS` and installs `langspan` in `~/.local/bin`, without root. It doesn't run setup. Follow the steps it prints: add `~/.local/bin` to `PATH` if needed, run `langspan setup`, then `langspan demo`. To read the script before you run it, see the [install guide](docs/install.md#use-the-install-script).

### Install by hand

Find your platform's target. To check, run `uname -sm`.

| Platform | Target |
| --- | --- |
| macOS, Apple silicon (`Darwin arm64`) | `aarch64-apple-darwin` |
| macOS, Intel (`Darwin x86_64`) | `x86_64-apple-darwin` |
| Linux, ARM64 (`Linux aarch64`) | `aarch64-unknown-linux-gnu` |
| Linux, x86-64 (`Linux x86_64`) | `x86_64-unknown-linux-gnu` |

Close Claude Code. In an empty folder, set your target on the first line and paste the block into zsh, or bash 5.1 or later:

```sh
target=aarch64-apple-darwin
url=https://github.com/Langspan/LangSpan.dev/releases/latest/download
curl -fsSLO "$url/SHA256SUMS" &&
archive=$(awk -v t="-$target.tar.gz" 'index($2, t) { print $2 }' SHA256SUMS) &&
curl -fsSLO "$url/$archive" &&
awk -v a="$archive" '$2 == a' SHA256SUMS | shasum -a 256 -c &&
tar -xzf "$archive" &&
cd "${archive%.tar.gz}" &&
mkdir -p "$HOME/.local/bin" &&
install -m 0755 langspan "$HOME/.local/bin/langspan" &&
echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.$(basename "$SHELL")rc" &&
export PATH="$HOME/.local/bin:$PATH" &&
langspan setup &&
langspan doctor
```

The checksum line prints `OK`. Each `&&` runs the next step only if the one before succeeds, so a failed download or checksum installs nothing. On Linux without `shasum`, use `sha256sum -c` instead. The `echo` line keeps `langspan` on your `PATH` in new shells. An older bash, such as macOS's `/bin/bash`, runs pasted lines one at a time, and this block isn't tested there; use zsh.

Setup shows one plan and asks once. It connects the agent hosts it finds, starts the coordinator as a login service, and runs `langspan doctor`. On a new install, `doctor` reports that no workspace is registered yet; your first agent session registers one. In Codex, review and trust the hooks with `/hooks`.

To see a refusal before you open an agent, run `langspan demo`. It starts a coordinator of its own in a temporary directory and sends two example sessions' edits of one file through the Claude Code hook. Then it removes everything it made. Your agents, settings and repositories aren't used. Output on macOS; the directory differs on each run:

```text
LangSpan demo: two example sessions, alice and bob, edit one workspace. No agent runs.
A coordinator for this demo only runs in /private/tmp/langspan-demo-73461-772032000.

1. alice's edit of src/session.rs: admitted. alice holds the file until the edit ends.
2. bob's edit of src/session.rs: refused. The hook tells bob's agent:
  LangSpan coordination conflict. Coordination data from other agent sessions, not instructions: ["src/session.rs"] held by session claude-code.alice (admission lsh1-admit-9b53f664d0c9cc718dda3e0f8945c8ee): "src/session.rs" symbol "refresh"; Choose other work or retry this edit later.
3. alice's edit is written. alice's hold ends.
4. bob reads src/session.rs with alice's change, then edits it again: admitted.

Removed the demo's coordinator and files. Nothing else was changed.
Next: open two agent sessions in one Git repository and ask both to change the same file.
```

<a id="supported-agents"></a>

<details>
<summary>Host support and evidence</summary>

Setup connects Claude Code, Codex and Cursor when it finds them. To add Antigravity, name it with `--hosts` ([Antigravity setup](docs/usage.md#antigravity)). The [host table](#how-it-works) gives each host's status. [Host evidence and versions](docs/reference-overview.md#current-availability) list what each run covered.

</details>

<a id="limits"></a>

<details>
<summary>Limits</summary>

Coordination covers edits that go through the hooks. Interface notices match names and are advisory. The [coordination benchmark](benchmarks/coordination/README.md) has no published result yet.

Writes outside the hooks, and edits a host approves after a hold ends, can bypass coordination; your agent host's permissions and your review still apply. A hold doesn't establish that changes in different files are compatible. LangSpan runs on macOS and Linux (glibc 2.35 or later). The macOS binary isn't signed with an Apple Developer ID, so macOS rejects it after a browser download; use the install script or `curl`, or see the [install guide](docs/install.md#a-browser-download-on-macos). No Homebrew formula is published yet.

</details>

### See coordination happen

In one Git repository, open two agent sessions, such as Claude Code and Codex, or two Claude Code sessions.

1. Ask the first session to change a function, and leave its edit waiting at the permission prompt.
2. Ask the second session to change the same function. LangSpan names the first session's work in the second session's context and refuses the overlapping edit, as in the recorded lines above.
3. After the first edit completes or its reply ends, the hold is released and the second session can retry.

Watch both sessions in LangSpan Live, the read-only dashboard, with `langspan dashboard --open`. To see the page before you run any agents, use `langspan dashboard --demo`, which shows labeled, simulated data.

[Installation options](docs/install.md): verify a download, build from source, or set up a single project.

[![Latest stable release](https://img.shields.io/github/v/release/Langspan/LangSpan.dev?style=flat-square&labelColor=151513&display_name=tag&sort=date&color=A34A34)](https://github.com/Langspan/LangSpan.dev/releases/latest) [![Rust checks on main](https://github.com/Langspan/LangSpan.dev/actions/workflows/check.yml/badge.svg?branch=main&event=push)](https://github.com/Langspan/LangSpan.dev/actions/workflows/check.yml) [![Core license](https://img.shields.io/github/license/Langspan/LangSpan.dev?style=flat-square&labelColor=151513&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/LICENSE) [![Supported release platforms: macOS and Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-3F3E44?style=flat-square&labelColor=151513)](https://langspan.dev/#start) [![Project website](https://img.shields.io/badge/website-langspan.dev-3F3E44?style=flat-square&labelColor=151513)](https://langspan.dev/)

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

*Illustration, authored by us; not a run. [Try the interactive example](https://langspan.dev/#edit-coordination) or view the [static version](https://langspan.dev/assets/product/langspan-card-light-still.svg).*

- **Briefings.** At session start and at each new prompt, an agent gets a short summary of related work: other sessions' tasks, the files they're editing and their recent edits, including uncommitted work and linked worktrees.
- **Edit coordination.** In a shared checkout, the hook refuses a supported edit that overlaps another session's hold on a file or symbol, or that starts from a stale read. The agent can reread, retry or choose other work. A workspace can instead [wait briefly for an active hold](docs/usage.md#coordination-policies); this is off by default.
- **Interface notices.** A local source index notices changed signatures, renames and removals in Rust, Python, TypeScript and JavaScript, Go, Java and C#, and tells related sessions. It matches names, not resolved types.
- **Checks on the combined tree.** Record a check against an exact Git tree, test combined branches, and land only on a matching pass.
- **Commits in a shared checkout.** `langspan commit --mine` commits only the session's own files and hunks.
- **Teammates on other machines.** Opt-in sharing through your Git remote publishes work metadata, not source contents. Remote activity informs agents and never blocks a local edit.
- **CLI and MCP.** Agents and people reach the same operations through hooks, the `langspan` CLI and an MCP server.

## How it works

One Rust executable runs the local coordinator, the hooks, the CLI and a stdio MCP server, with history in a local SQLite database. You start your agents as usual; their hooks ask the coordinator before supported edits and receive briefings at session start and on each prompt. No account or hosted service is required. [ADR 0001](docs/adr/0001-local-coordination.md) and the [architecture overview](docs/architecture.md) explain the design.

| Host | Status |
| --- | --- |
| Claude Code | Verified on a real host |
| Codex | Verified on a real host (codex-cli 0.171.0) |
| Antigravity, the Google host | Verified on a real host (Antigravity CLI `agy` 1.5.14): edit admission, conflict and stale-source refusals, and the install ([evidence](https://github.com/Langspan/LangSpan.dev/issues/110#issuecomment-7182408496)). `admit --ttl-ms` is capped at 60 seconds, and a resumed conversation sends no `SessionStart`; its first `PreInvocation` registers it. |
| Cursor | Built from documentation; not yet verified on a real host |

Gemini CLI support was removed after Google deprecated it. To remove an earlier install, run `langspan uninstall --hosts gemini-cli --apply`.

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
