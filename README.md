<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/brand/langspan-lockup-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/brand/langspan-lockup-light.svg">
  <img alt="LangSpan" src="docs/assets/brand/langspan-lockup-light.svg" width="288" height="80">
</picture>

# Your agents. On the same page.

LangSpan gives your team's coding agents a shared view of work in progress.
The aim is to help people build related parts of a project together with less
avoidable rework and less manual relay between their agent sessions.
It connects Claude Code and Codex through hooks, with CLI and MCP access for
other agent workflows. Cursor and Gemini CLI adapters are built from
documentation and not yet verified on a real Cursor or a live Gemini CLI session.

[See the team story](https://langspan.dev/#team-story) ·
[Get started](#get-started) ·
[Usage guide](docs/usage.md) ·
[Implementation status](docs/IMPLEMENTATION_STATUS.md)

<a href="https://langspan.dev/#demo">
  <picture>
    <source media="(max-width: 600px)" srcset="https://langspan.dev/assets/product/langspan-overview-compact.svg">
    <img src="https://langspan.dev/assets/product/langspan-overview.svg" alt="LangSpan holds a file for Agent A, denies Agent B’s overlapping edit, and leaves an unrelated test file available." width="1400">
  </picture>
</a>

*An authored illustration of local coordination. The brief animation settles into
the complete picture; reduced motion shows that picture immediately.
[Explore each step](https://langspan.dev/#demo) with Play, Pause and Replay.*

## Why we're building LangSpan

Our mission is to help teams keep building together as their agents write more
code. When a swarm moves faster than the team can keep one another informed,
people can end up choosing unrelated work to avoid getting in its way. Even
separate worktrees can bring incompatible changes back together at integration.

Consider two developers working on one feature. Alice's agent changes session
refresh while Bob's agent updates the login flow. A useful briefing tells Bob's
agent what Alice changed before it chooses the affected edit. The intended
result is a working combined feature with less repair and fewer copied updates.

We will judge LangSpan by that team outcome: relevant information arriving in
time, an observable response, less avoidable rework and useful parallel progress,
with the same checks and review standard. Those benefits still need comparative
evidence. [The mission](docs/mission.md) defines the hypothesis and success measures.

One developer running several agents can use the same coordination tools. Each
agent stays in its own session, using its own tools and permission prompts.

## What reaches your agents

### Brief each session

At session start, LangSpan gives the agent a short briefing on related work:
other sessions' tasks, current files and recent edits. Each new prompt gets an
updated briefing when that information changes. Linked worktrees share these
updates while keeping their own checkouts.

Briefings can also include activity published from other machines through the
repository's Git remote. Sharing is off until enabled for that workspace;
remote activity informs the agent and never blocks a local edit. Task text
stays out of remote updates unless you choose to share it.

### Catch overlapping edits

For supported tools in one checkout, the hook denies edits that overlap
another session's file or region hold. It can admit edits to separate regions
of one file when it can locate them; otherwise it holds the whole file.
Where LangSpan has a recorded version, it also checks whether the file changed
before the edit. [Region admission and its limits](docs/adr/0012-region-admission.md)
describe which edits can proceed together.

### See what happened

Agents can ask through MCP which sessions are working on a file and which
files changed. The CLI shows the same records for you to inspect. A Git observer
also records file and branch changes outside the hooks, without assigning them
to an agent.

### See agent activity in one place

`langspan dashboard` serves a read-only page that refreshes every two seconds.
It shows local sessions and published teammate activity, files held now,
files several agents touched, recorded denials, checks, waiting messages and
workspace health. The view includes up to 50 agents and 20 entries in each
other list, with counts for entries left out.

```sh
./target/release/langspan dashboard --open
```

It defaults to `127.0.0.1:7311` and prints a URL with a token for this run;
a request without the token is refused. To read it from a phone, forward the
port with SSH. [ADR 0013](docs/adr/0013-dashboard.md) also describes explicitly
binding to a VPN address and the limits of that setup.

### Carry context between sessions

Within one workspace, agents can ask for a recap, save short notes, message
another session or leave a handoff for the next one. Notes survive coordinator
restarts. Messages and handoffs arrive through supported briefing events;
they do not wake idle sessions. These records stay local to the coordinator.

## Get started

### Install

Build from source. You need Rust through rustup (the repository pins the
version in `rust-toolchain.toml`) and a C compiler for the bundled SQLite and
Tree-sitter grammars. From a clone of this repository:

```sh
cargo build --workspace --release --locked
./target/release/langspan --help
```

Release binaries and Homebrew are prepared but not published yet. No release
has been made, and the Homebrew tap does not exist. Once they are published:

| Method | Command | Status |
| --- | --- | --- |
| Release archive | Download `langspan-vX.Y.Z-TARGET.tar.gz` for your platform from the [Releases page](https://github.com/Langspan/LangSpan.dev/releases) and put `langspan` on your `PATH` | Not published yet |
| Homebrew | `brew install langspan/tap/langspan` | Not published yet |

Archives are planned for macOS (Apple silicon and Intel) and Linux (x86_64
and arm64, glibc 2.35 or later). They are not signed with an Apple Developer
ID: a macOS archive downloaded in a browser is quarantined, and Gatekeeper
stops `langspan` from running until you allow it in System Settings or remove the
attribute with `xattr -d com.apple.quarantine langspan`. Downloads with `curl`
or Homebrew are not quarantined.

Under Homebrew, the hooks and MCP entries name the stable
`$(brew --prefix)/opt/langspan/bin/langspan`, so they keep working after
`brew upgrade`. After any upgrade, restart a running coordinator.

### Verify a download

Each release is planned to carry `SHA256SUMS`, a CycloneDX SBOM per archive
and a build provenance attestation made by the release workflow. To check an
archive with the [GitHub CLI](https://cli.github.com/):

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing   # on Linux: sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify langspan-vX.Y.Z-aarch64-apple-darwin.tar.gz \
  -R Langspan/LangSpan.dev \
  --signer-workflow Langspan/LangSpan.dev/.github/workflows/release.yml
```

The second command checks that the archive was built by this repository's
release workflow. Add `--predicate-type https://cyclonedx.org/bom` to check
its SBOM attestation instead. Builds are not bit-for-bit reproducible; the
C code and the macOS linker vary between builds.
[ADR 0031](docs/adr/0031-releases.md) records how releases are made.

### Run

Start the local coordinator and leave it running:

```sh
./target/release/langspan daemon
```

The default state directory is `$XDG_STATE_HOME/langspan` or
`$HOME/.local/state/langspan`. Choose another with `--state-dir DIR` or
`LANGSPAN_STATE_DIR`. Use that same directory for every participating hook,
CLI, and MCP client. The daemon requires owner-only permissions and does not
change workspace files.

## Connect a native agent

### Across your Git repositories

`langspan install` prepares user-level hooks and registers `langspan mcp` as
the MCP server `langspan`. Supported hosts can then discover LangSpan in Git
repositories without a per-project install:

```sh
./target/release/langspan install
```

This is a dry run that previews the proposed settings; for `~/.claude.json`,
only LangSpan's MCP entry is shown. Add `--apply` to write them, with Claude
Code closed. It writes `~/.claude/settings.json` (hooks) and the `mcpServers`
entry of `~/.claude.json` for Claude Code, and `~/.codex/hooks.json` and a
marked `[mcp_servers.langspan]` table in `~/.codex/config.toml` for Codex, and
hooks and the `mcpServers` entry in `~/.gemini/settings.json` for Gemini CLI.
The hosts default to Claude Code, Codex and Cursor where their `~/.claude`,
`~/.codex` or `~/.cursor` directory exists; Gemini CLI is added only by name,
with `--hosts gemini-cli` or `--hosts claude-code,codex,cursor,gemini-cli`.
Other settings, hooks and MCP servers are left as they are, and an existing
`langspan` server that LangSpan did not write stops the install with nothing
written. Codex runs a new hook only after you review and trust it with
`/hooks`. With `--hosts cursor`, or when `~/.cursor` exists, it also writes
`~/.cursor/hooks.json` and `~/.cursor/mcp.json`. Cursor and Gemini CLI: built from documentation, not
yet verified on a real host ([ADR 0032](docs/adr/0032-cursor-adapter.md),
[ADR 0033](docs/adr/0033-gemini-cli-adapter.md)).

If Claude Code has not created `~/.claude.json` yet, the installer adds its
hooks but leaves MCP registration for later. Start Claude Code once, close
it, then rerun the installer to add that entry.

The hook finds the repository from the directory the agent works in. Outside
a Git repository it does nothing. In a repository that also has a project
install (below), the user-level hook leaves the event to that one, so nothing
is coordinated twice. `langspan doctor` reports both kinds of install.

`langspan uninstall --apply` removes only what `install` wrote; the state
directory stays. [ADR 0010](docs/adr/0010-user-level-install.md) records the
host documentation these locations come from and what was not verifiable.

### One project

To cover a single checkout instead, inspect the proposed project settings.
Replace `/path/to/checkout` with the project your agent is editing:

```sh
./target/release/langspan hooks install claude-code \
  --workspace my-project --root /absolute/path/to/project
```

This is a dry run. Add `--apply` to install the reviewed settings. Replace
`claude-code` with `codex`, `cursor` or `gemini-cli` for their adapters, then
complete the host's own hook trust flow. Open your agent normally. `cursor`
writes `.cursor/hooks.json` and `.cursor/mcp.json`; `gemini-cli` writes
`.gemini/settings.json`. Cursor and Gemini CLI: built from documentation, not
yet verified on a real host.

Sessions coordinating local edits use the same daemon and state directory. By
default, briefings can include the first line of a peer's prompt as its task
summary. Add `--prompt-summary off` during installation to omit prompt text.

[Full setup, removal and CLI examples](docs/usage.md) ·
[MCP integration](docs/usage.md#mcp) ·
[Briefing details](docs/usage.md#automatic-peer-briefings)

## How it fits

One Rust executable runs the local coordinator, CLI, hooks and stdio MCP server,
with history stored in SQLite. You launch your agents as usual; LangSpan connects
to those sessions. No dashboard is required; `langspan dashboard` is an optional
read-only view.

| Interface | Use it for |
| --- | --- |
| Native hooks | Brief agents at session start or a new prompt; check supported edits |
| CLI | Set up a workspace and inspect what its agents are doing |
| MCP | Ask about related work and check whether files are already held |

Source checks refuse a stale change once; unrecorded versions can still proceed.
The coordinator cannot stop writes outside the hooks or edits approved after a
hold expires. See [source-version coverage](docs/usage.md#source-versions) and
[recovery](docs/usage.md#retries-and-recovery) for those limits.

## Current availability

LangSpan is available as a source preview. One
[recorded Claude Code/Codex run](docs/evidence/mixed-hosts/README.md) used
sequential turns in a shared checkout on macOS. Codex completed an edit,
Claude preserved that change in its own edit, and Codex later declined to edit
without rereading the changed file.

| Feature or platform | Current evidence |
| --- | --- |
| Claude Code | `Edit` / `Write` coordination, completed `Read` tracking and peer briefing. [M2 edit evidence](docs/evidence/m2-claude-code/README.md); [native SessionStart briefing receipt](docs/evidence/m4-briefing/README.md). The end of a reply (`Stop`, `StopFailure`, `SubagentStop`) releases the reply's open holds, and each tool batch (`PostToolBatch`) is recorded and can carry news; these events were [observed on Claude Code 2.1.330](docs/evidence/turn-events/README.md). |
| Codex | A [native mixed-host run](docs/evidence/mixed-hosts/README.md) records a completed `apply_patch` edit and a response to peer context. Simultaneous mixed-host conflicts remain unverified. Codex reports no reads, so its first edit of a file is not compared; that session's later edits are compared with the version recorded at the first. `Stop` and `SessionEnd` are hooked and tested in their documented shape, not yet seen on Codex. [ADR 0027](docs/adr/0027-turn-and-batch-events.md). |
| Cursor | Built from documentation, not yet verified on a real Cursor. `Write`, `Delete` and `Shell` are admitted, completed `Read`s recorded, and sessions briefed, all through `.cursor/hooks.json`. Tested with constructed events, the real daemon and Git, and the installed command run the way Cursor CLI 2026.11.22 runs it. No Cursor run has been captured. [Scope, sources and capture steps](docs/adr/0032-cursor-adapter.md). |
| Gemini CLI | Built from documentation, not yet verified on a real Gemini CLI. `write_file` / `replace` admission, completed `read_file` tracking, shell commands and briefings, from Gemini CLI 0.70.0's documentation and source. Tested with source-derived events through the real hook, daemon and Git; no live Gemini session yet. [Scope and limits](docs/adr/0033-gemini-cli-adapter.md). |
| Shell commands | Each host's shell tool (`Bash`; Cursor's `Shell`; Gemini CLI's `run_shell_command`), partly: literal writes (`> file`, `sed -i`, `mv`, `rm`, …) are held or refused like an edit before they run; other commands' changes are found afterwards by comparing Git status and recorded as the session's edits, or reported as not observed. A heredoc that rewrites a file (`cat > file <<'EOF'`) is not held yet, so it can overwrite another session's change from an old read; a fix is planned. Tested with the real daemon and Git; not yet on a host. [Scope and limits](docs/adr/0011-shell-writes.md). |
| Regions within one file | Supported edits can hold separate symbols or line ranges, with whole-file fallback. Tested with the real daemon and hooks; live-host region acceptance remains unverified. [Placement and limits](docs/adr/0012-region-admission.md). |
| Source versions | Comparisons pass real-daemon tests. [The live Claude run](docs/evidence/m6-stale-source/README.md) was stopped by Claude's own check; LangSpan's stale denial still needs a live host test. |
| Linked worktrees | Separate file holds and shared briefings, tested with real Git. The two-worktree test with live agents remains open. |
| Remote briefings | Opt-in sharing through Git refs, tested with two daemons and two clones on one machine. [One run between a Mac and a cloud machine through GitHub](docs/evidence/two-machine/README.md) briefed real sessions in both directions; no session changed its edit because of it, and only GitHub has been used. [Transport and limits](docs/adr/0006-remote-awareness-through-git-refs.md). |
| Lines another session wrote | An edit with no recorded read that replaces or removes lines another live session added in the last 30 minutes is refused once, naming the writer. Tested with the real daemon and hooks; not yet on a host. [Rules and limits](docs/adr/0028-line-ownership.md). |
| Commits in a shared checkout | A `git add -A`, `git commit -a` or similar that would take another live session's uncommitted lines is held once, naming the session and files; `langspan commit --mine` commits only the session's own files and hunks, with your identity and hooks. Tested with the real daemon, hooks and Git; not yet on a host. [Rules and limits](docs/adr/0026-commit-guard-and-commit-mine.md). |
| Checks and landing | Check records name a Git tree; combined branches are tested in a temporary checkout. Landing requires a matching pass and `--apply`; landings wait in a bounded line for the push turn, report conflict hunks and follow file renames that landed. Tested with local file remotes. [Scope and limits](docs/adr/0007-check-evidence-integration-and-landing.md). |
| Automatic checks | Off by default. `langspan check auto` runs the owner's command after a Claude Code or Codex session's write batch or reply, on the exact tree, and tells the session the result once at its next briefing: passed, failed with the exit code, stopped at its time limit, or inconclusive, with the output's last lines. Failures are not classified. The command runs as you, outside the agent host's sandbox. Tested with the real hook and daemon; not yet observed on a host. [ADR 0030](docs/adr/0030-automatic-checks-after-write-batches.md). |
| Break alerts at write time | A Claude Code or Codex write whose calls no longer fit a peer's new signature is told the file, line and old and new argument counts with that write's result, once per change. Argument counts only; never a refusal. Tested with the real hook and daemon; not yet on a host. [ADR 0029](docs/adr/0029-break-alerts-at-write-time.md). |
| Source context | Definitions, name mentions and one-level exported-name impact queries are implemented. Interface notices stay within one physical workspace. Extraction and real-daemon tests pass; a native-agent run remains unverified. [Scope and limits](docs/adr/0008-derived-source-index.md). |
| Recap, notes and messages | Workspace recaps, persistent local notes, messages and handoffs are implemented. Delivery and acknowledgement have real-daemon tests; native-host delivery remains unverified. Messages stay in one workspace and do not wake idle sessions. [Message scope](docs/adr/0009-agent-messages-and-handoff.md). |
| User-level installation | Hooks and MCP registration have temporary-home integration tests. The complete user-level setup still needs a live Claude Code or Codex run. [Install scope and limits](docs/adr/0010-user-level-install.md). |
| CLI + MCP | Twenty-three MCP tools, with CLI access to the same operations. [Command reference](docs/usage.md#mcp). |
| Linux | Full check passed in CI for the local implementation. Process probing is Linux-only. |
| macOS arm64 | Build, formatting, Clippy and test-suite passes are recorded. Native Claude briefing evidence is retained. |

The retained mixed-host run does not cover simultaneous overlapping edits or
live agents in linked worktrees, and the two-machine run did not show an agent
changing its approach. Comparative evidence of less
rework and manual relay remains open. Windows transport and signed releases
are not available. Source context matches names without resolving types, scopes
or dependency chains. The [roadmap](docs/roadmap.md) and
[implementation status](docs/IMPLEMENTATION_STATUS.md)
record the versions tested and the remaining checks.

## Explore the project

| Guide | What you'll find |
| --- | --- |
| [Mission](docs/mission.md) | The problem, intended team outcome and how we'll measure success |
| [Usage](docs/usage.md) | Setup, native hooks, CLI walkthrough, MCP, retries and configuration |
| [Implementation status](docs/IMPLEMENTATION_STATUS.md) | Current behavior, retained evidence and open acceptance |
| [Architecture decisions](docs/adr/0001-local-coordination.md) | Local coordination, native adapters and automatic briefing |
| [Engineering practice](docs/engineering.md) | Build, checks and contribution workflow |
| [AGENTS.md](AGENTS.md) / [CLAUDE.md](CLAUDE.md) | Context for agents working in this repository |

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. For
development, run `sh scripts/check.sh`; its prerequisites and individual
commands are in the engineering guide. The full suite uses real SQLite, Git,
processes and Unix sockets. Everyone taking part follows the
[code of conduct](CODE_OF_CONDUCT.md).

## Security

Report a suspected vulnerability privately, as [SECURITY.md](SECURITY.md)
describes, not in a public issue. What LangSpan protects against, and what it
does not, is in the [threat model](docs/security/threat-model.md).

## Licence

LangSpan is licensed under the [Apache License 2.0](LICENSE). [NOTICE](NOTICE)
lists the third-party material in this repository; each release archive
carries the licences of the crates built into the binary.

[Website](https://langspan.dev/) ·
[Website source](https://github.com/Langspan/langspan.github.io)
