<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/brand/langspan-lockup-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/brand/langspan-lockup-light.svg">
  <img alt="LangSpan" src="docs/assets/brand/langspan-lockup-light.svg" width="288" height="80">
</picture>

# Your agents. On the same page.

**Coordinate the coding agents you already run.** Share work in progress, catch overlapping edits and check combined changes across sessions and worktrees.

[![Latest stable release](https://img.shields.io/github/v/release/Langspan/LangSpan.dev?style=flat-square&labelColor=151513&display_name=tag&sort=date&color=A34A34)](https://github.com/Langspan/LangSpan.dev/releases/latest) [![Rust checks on main](https://github.com/Langspan/LangSpan.dev/actions/workflows/check.yml/badge.svg?branch=main&event=push)](https://github.com/Langspan/LangSpan.dev/actions/workflows/check.yml) [![Core license](https://img.shields.io/github/license/Langspan/LangSpan.dev?style=flat-square&labelColor=151513&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/LICENSE) [![Supported release platforms: macOS and Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-3F3E44?style=flat-square&labelColor=151513)](https://langspan.dev/#start) [![Project website](https://img.shields.io/badge/website-langspan.dev-3F3E44?style=flat-square&labelColor=151513)](https://langspan.dev/)

Claude Code holds `src/gate.rs`. Codex tries to edit the same function. LangSpan tells Codex about the hold, then refuses the overlap:

```text
editing now: "src/gate.rs"
Command blocked by PreToolUse hook: LangSpan coordination conflict.
```

Codex can choose other work or retry when the hold ends. [Actual Codex session, November 25, 2026](docs/evidence/codex-real-host/README.md#2-a-conflicting-edit).

**[Get LangSpan](#get-started)** · [Try the product](https://langspan.dev/) · [LangSpan Live](https://langspan.dev/#langspan-live)

## Get started

### Install

[Download your binary](https://langspan.dev/#start) for macOS or Linux. Verify and extract the archive ([installation guide](docs/install.md)), then close Claude Code and run from the extracted folder:

```sh
mkdir -p "$HOME/.local/bin"
install -m 0755 langspan "$HOME/.local/bin/langspan"
export PATH="$HOME/.local/bin:$PATH"
langspan setup
```

Confirm the setup plan. LangSpan connects your supported agent hosts and starts the coordinator. In Codex, review and trust hooks with `/hooks`.

### Run

Open your agents normally in a Git repository. To see what they’re doing:

```sh
langspan dashboard --open
```

<a href="https://langspan.dev/#edit-coordination">
  <picture>
    <source media="(max-width: 600px)" srcset="https://langspan.dev/assets/product/langspan-overview-compact.svg">
    <source media="(prefers-reduced-motion: reduce) and (prefers-color-scheme: dark)" srcset="https://langspan.dev/assets/product/langspan-card-dark-still.svg">
    <source media="(prefers-reduced-motion: reduce)" srcset="https://langspan.dev/assets/product/langspan-card-light-still.svg">
    <source media="(prefers-color-scheme: dark)" srcset="https://langspan.dev/assets/product/langspan-card-dark.svg">
    <img src="https://langspan.dev/assets/product/langspan-card-light.svg" alt="LangSpan: in a shared checkout Agent A holds src/auth/session.ts, Agent B’s overlapping request is refused, and B independently chooses tests/session.test.ts." width="1300">
  </picture>
</a>

*[Try the interactive coordination example](https://langspan.dev/#edit-coordination) or view the [static version](https://langspan.dev/assets/product/langspan-card-light-still.svg).*

## What you get

- **Shared awareness.** Briefings about related tasks, files and interface changes.
- **Edit coordination.** Refuse supported overlapping or stale edits in a shared checkout.
- **Semantic notices.** See changed signatures and renamed functions that affect related code, including linked worktrees.
- **Verified integration.** Attribute edits to sessions and check the combined Git tree.
- **LangSpan Live.** Inspect sessions, shared paths, holds and coordination events.

## Local by default

Edit holds cover supported operations within a checkout. Interface notices are advisory. Across machines, opt-in Git sharing publishes work metadata rather than source contents. Your agent permissions and review still apply.

## Current availability

Native Claude Code and Codex coordination has recorded host evidence. For all adapters, platform requirements and unverified behavior, see [implementation status](docs/IMPLEMENTATION_STATUS.md).

| Host | Status |
| --- | --- |
| Claude Code | Verified on a real host |
| Codex | Verified on a real host (codex-cli 0.171.0) |
| Antigravity, the Google host | Verified on Antigravity CLI (`agy`) 1.5.14: edit admission, conflict and stale-source refusals, and the install ([evidence](https://github.com/Langspan/LangSpan.dev/issues/110#issuecomment-7182408496)). `admit --ttl-ms` is capped at 60 seconds, and a resumed conversation sends no `SessionStart`; its first `PreInvocation` registers it. |
| Cursor | Built from documentation; not yet verified on a real host |

Gemini CLI support was removed after Google deprecated it. To remove an earlier install, run `langspan uninstall --hosts gemini-cli --apply`.

## Documentation

[Install and verify](docs/install.md) · [Usage](docs/usage.md) · [MCP](docs/usage.md#mcp) · [Architecture](docs/adr/0001-local-coordination.md) · [Evidence and compatibility](docs/reference-overview.md) · [Benchmarks](benchmarks/coordination/README.md)

<a id="verify-the-binary"></a>
<a id="verify-a-download"></a>
<a id="build-from-source"></a>
<a id="connect-a-native-agent"></a>
<a id="one-project"></a>

[Binary verification, source builds and setup options](docs/install.md)

## Contributing

[![Rust (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2Frust-toolchain.toml&query=%24.toolchain.channel&label=Rust&color=3F3E44&logo=rust)](https://github.com/Langspan/LangSpan.dev/blob/main/rust-toolchain.toml) [![MCP SDK (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.rmcp.version&label=MCP%20SDK&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml) [![rusqlite (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.rusqlite.version&label=rusqlite&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml) [![Tree-sitter (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2Fcrates%2Flangspan-graph%2FCargo.toml&query=%24.dependencies%5B%22tree-sitter%22%5D&label=Tree-sitter&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/crates/langspan-graph/Cargo.toml) [![Tokio (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.tokio.version&label=Tokio&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml)

<sub>Dependency badges track the pins on main.</sub>

[Contributing](CONTRIBUTING.md) · [Engineering guide](docs/engineering.md) · [Security](SECURITY.md) · [Apache-2.0](LICENSE)
