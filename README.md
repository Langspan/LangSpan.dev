<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/brand/langspan-lockup-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/brand/langspan-lockup-light.svg">
  <img alt="LangSpan" src="docs/assets/brand/langspan-lockup-light.svg" width="288" height="80">
</picture>

# Your agents. On the same page.

**Run agents in parallel. Build as a team.**

[![Latest stable release](https://img.shields.io/github/v/release/Langspan/LangSpan.dev?style=flat-square&labelColor=151513&display_name=tag&sort=date&color=A34A34)](https://github.com/Langspan/LangSpan.dev/releases/latest) [![Rust checks on main](https://github.com/Langspan/LangSpan.dev/actions/workflows/check.yml/badge.svg?branch=main&event=push)](https://github.com/Langspan/LangSpan.dev/actions/workflows/check.yml) [![Core license](https://img.shields.io/github/license/Langspan/LangSpan.dev?style=flat-square&labelColor=151513&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/LICENSE) [![Supported release platforms: macOS and Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-3F3E44?style=flat-square&labelColor=151513)](https://langspan.dev/#start) [![Project website](https://img.shields.io/badge/website-langspan.dev-3F3E44?style=flat-square&labelColor=151513)](https://langspan.dev/)

Claude Code is changing a function in `src/gate.rs`. Codex tries to edit the same function.

LangSpan tells Codex about the active work, then refuses the overlapping edit:

```text
editing now: "src/gate.rs"

Command blocked by PreToolUse hook: LangSpan coordination conflict.
```

Codex can retry when the hold ends or choose different work. Both agents stay in their own sessions.

These are excerpts from a [recorded Codex session](docs/evidence/codex-real-host/README.md#2-a-conflicting-edit) on November 25, 2026; Claude Code was waiting at its edit permission prompt.

LangSpan is an open-source coordination layer for coding agents. Keep your agents and Git workflows. Share work in progress, coordinate supported edits, and verify combined changes.

**[Get LangSpan](#get-started)** · [Try the interactive demo](https://langspan.dev/) · [LangSpan Live](https://langspan.dev/#langspan-live)

<a href="https://langspan.dev/#edit-coordination">
  <picture>
    <source media="(max-width: 600px)" srcset="https://langspan.dev/assets/product/langspan-overview-compact.svg">
    <source media="(prefers-reduced-motion: reduce) and (prefers-color-scheme: dark)" srcset="https://langspan.dev/assets/product/langspan-card-dark-still.svg">
    <source media="(prefers-reduced-motion: reduce)" srcset="https://langspan.dev/assets/product/langspan-card-light-still.svg">
    <source media="(prefers-color-scheme: dark)" srcset="https://langspan.dev/assets/product/langspan-card-dark.svg">
    <img src="https://langspan.dev/assets/product/langspan-card-light.svg" alt="LangSpan: in a shared checkout Agent A holds src/auth/session.ts, Agent B’s overlapping request is refused, and B independently chooses tests/session.test.ts." width="1300">
  </picture>
</a>

*Interactive example: a held edit, a refusal, and independent work. [Explore the workflow](https://langspan.dev/#edit-coordination) or view the [static version](https://langspan.dev/assets/product/langspan-card-light-still.svg).*

## Get started

### Install

Download and extract the [latest stable release](https://github.com/Langspan/LangSpan.dev/releases/latest) for your platform. Verify the binary using the [instructions below](#verify-the-binary).

From the extracted directory, install the executable:

```sh
mkdir -p "$HOME/.local/bin"
install -m 0755 langspan "$HOME/.local/bin/langspan"
export PATH="$HOME/.local/bin:$PATH"
```

Close Claude Code if it's running. Then connect your agents:

```sh
langspan setup
```

Review the setup plan and confirm. LangSpan installs the hooks, MCP registration and skill for detected supported hosts, starts the coordinator and runs its health checks. In Codex, review and trust the hooks with `/hooks`.

### Run

Open your coding agents normally in a Git repository. See their work in one local dashboard:

```sh
langspan dashboard --open
```

[Explore the actual dashboard demo](https://langspan.dev/#langspan-live) · [Setup and configuration](docs/usage.md#set-up-in-one-command)

## What LangSpan does

- **Share work in progress.** Brief agents about related tasks, files and changes.
- **Coordinate edits.** Refuse supported overlapping or stale edits in a shared checkout. A workspace can instead wait briefly for an active hold and then revalidate the file ([coordination policies](docs/usage.md#coordination-policies)); this is off by default and is not an edit queue.
- **See related code changes.** Surface advisory interface notices, including relevant uncommitted signatures in linked worktrees.
- **Verify combined work.** Attribute changes to their sessions and record checks against the exact Git tree before landing.
- **Inspect activity.** See shared paths, holds, refusals and check evidence in LangSpan Live.

Worktrees isolate files, not intent. Two branches can merge without textual conflicts and still break the combined build. [Try the interface-change example](https://langspan.dev/#semantic-awareness).

## Local by default

LangSpan runs on your machine. Linked worktrees share local context; edit holds stay within each checkout. Opt-in Git publications let teammates share work metadata without publishing source contents. Interface notices and remote activity are advisory. Your agent's permissions and your review process still apply.

## Current availability

Released for macOS and Linux on ARM64 and x86-64. Native Claude Code and Codex coordination has retained real-session evidence. Cursor and Gemini CLI adapters lack retained live-session validation; Antigravity was corrected against an initial real run, with repeat validation pending.

[Implementation status](docs/IMPLEMENTATION_STATUS.md) · [Detailed evidence and host coverage](docs/reference-overview.md#current-availability) · [Roadmap](docs/roadmap.md)

<details>
<summary>Installation details and verification</summary>

Linux requires glibc 2.35 or later. macOS archives are not Apple Developer ID-signed; verify them before allowing execution in System Settings. Add the installation directory to your shell startup file if needed. The Homebrew tap and install script are not published.

<a id="verify-a-download"></a>

### Verify the binary

Each release includes `SHA256SUMS`, a CycloneDX SBOM per target and a
provenance bundle. Download the archive and checksum file from the same release.
For example, in an empty directory on macOS Apple silicon, use the
[GitHub CLI](https://cli.github.com/) to resolve the latest stable tag once
and download its matching archive and checksums:

```sh
tag=$(gh release view -R Langspan/LangSpan.dev --json tagName --jq .tagName)
archive="langspan-$tag-aarch64-apple-darwin.tar.gz"
gh release download "$tag" -R Langspan/LangSpan.dev \
  --pattern "$archive" --pattern SHA256SUMS
awk -v name="$archive" '$2 == name' SHA256SUMS | shasum -a 256 -c
gh attestation verify "$archive" \
  -R Langspan/LangSpan.dev \
  --signer-workflow Langspan/LangSpan.dev/.github/workflows/release.yml
```

Substitute the archive suffix for your platform (on Linux, use `sha256sum -c`).
The attestation command checks that the archive was built by this repository's
release workflow. Add `--predicate-type https://cyclonedx.org/bom` to check
its SBOM attestation instead. Builds are not bit-for-bit reproducible; the
C code and the macOS linker vary between builds.
[ADR 0031](docs/adr/0031-releases.md) records how releases are made.

### Build from source

```sh
git clone https://github.com/Langspan/LangSpan.dev.git
cd LangSpan.dev
cargo build --workspace --release --locked
export PATH="$PWD/target/release:$PATH"
langspan setup
```

Requires Rust and a C compiler. Restart a running coordinator after upgrading. See the [engineering guide](docs/engineering.md).

### Connect a native agent

For [user-level setup](docs/usage.md#all-repositories-for-this-user), use `langspan setup`. For a single checkout, see [project-level installation](docs/reference-overview.md#one-project). To remove installed hooks, MCP configuration and the service, run `langspan uninstall --apply`.

</details>

## Explore the project

[Usage guide](docs/usage.md) · [MCP reference](docs/usage.md#mcp) · [Architecture](docs/adr/0001-local-coordination.md) · [Mission](docs/mission.md) · [Benchmark methodology](benchmarks/coordination/README.md) · [Engineering practice](docs/engineering.md)

<sub>Comparative team outcomes are still being measured. See the benchmark methodology for the experimental protocol and limitations.</sub>

## Contributing

[![Rust (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2Frust-toolchain.toml&query=%24.toolchain.channel&label=Rust&color=3F3E44&logo=rust)](https://github.com/Langspan/LangSpan.dev/blob/main/rust-toolchain.toml) [![MCP SDK (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.rmcp.version&label=MCP%20SDK&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml) [![rusqlite (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.rusqlite.version&label=rusqlite&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml) [![Tree-sitter (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2Fcrates%2Flangspan-graph%2FCargo.toml&query=%24.dependencies%5B%22tree-sitter%22%5D&label=Tree-sitter&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/crates/langspan-graph/Cargo.toml) [![Tokio (pinned on main)](https://img.shields.io/badge/dynamic/toml?style=flat-square&labelColor=151513&url=https%3A%2F%2Fraw.githubusercontent.com%2FLangspan%2FLangSpan.dev%2Fmain%2FCargo.toml&query=%24.dependencies.tokio.version&label=Tokio&color=3F3E44)](https://github.com/Langspan/LangSpan.dev/blob/main/Cargo.toml)

<sub>Dependency badges track the pins on main. Release archives carry their own SBOMs and provenance.</sub>

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Run `sh scripts/check.sh` for development checks. Follow the [code of conduct](CODE_OF_CONDUCT.md).

## Security

Report vulnerabilities privately using [SECURITY.md](SECURITY.md). See the [threat model](docs/security/threat-model.md).

## Licence

[Apache License 2.0](LICENSE) · [Third-party notices](NOTICE)
