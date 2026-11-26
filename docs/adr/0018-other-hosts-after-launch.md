# ADR 0018: Other hosts and host guidance files come after launch

**Status:** Not now. Decided 25 November 2026.

## Context

The launch hosts are Claude Code, Codex, Gemini CLI and Cursor. Claude Code
and Codex have adapters ([ADR 0002](0002-claude-code-adapter.md),
[ADR 0003](0003-native-coordination-observation.md)); Cursor and Gemini CLI
are launch tracks in the [roadmap](../roadmap.md). Hosts that have no hooks
can be supported only through rules files and MCP snippets. A rules file asks the agent to call a tool; it cannot hold or refuse
an edit, and nothing tells LangSpan whether the agent followed it.

## Decision

| Row | Capability | Decision | Reason |
| --- | --- | --- | --- |
| A5 | Hermes plugin | Not now | Not a launch host; no user has asked for it |
| A6 | Rules files for Windsurf, Cline, Copilot and `.cursor/rules` | Not now; after launch | Without hooks there is no admission and no briefing, only advice |
| A7 | Detecting agent tools installed after setup | Superseded: built (see below) | Install is still an explicit owner step ([ADR 0010](0010-user-level-install.md)); LangSpan now tells the owner when a host it would install is present and uncovered |
| I7 | An AGENTS.md block and rules-file text | Not now | Comes with the rules-file hosts; it changes files in the owner's repository |
| I8 | A LangSpan skill that teaches agents how to work with it: a general skill for every host, then task skills | Superseded: planned, part 1 built ([ADR 0034](0034-langspan-skill.md)) | The maintainers asked for it, and running LangSpan on its own development showed every agent needed the same instructions by hand |
| I11 | MCP menu trimming: tools hidden from read-only members, and bookkeeping tools hidden once hooks report (added by the second review) | Superseded: hooked-host part built (see below); role-based hiding not now | The cost was measured first ([benchmarks/mcp-tools](../../benchmarks/mcp-tools/README.md)); the list is shortened only on an owner's opt-in and only while the host's hooks are seen reporting. There are still no read-only members to hide tools from ([ADR 0016](0016-hosted-service-features-out-of-scope.md)) |

The Gemini CLI part of row A6 is launch track L10, not this ADR.

### A7, as built

Row A7 was built ahead of the user report it was waiting for. Detection only;
it never installs, upgrades or edits a host's configuration.

- `langspan hosts [--home DIR] [--json]` reports each of the four hosts:
  `name`, `config_dir`, `present`, `user_install` (`none`, `current`,
  `outdated`, `other_state_dir`, `unknown`), `next_step` and `error`. A
  directory that cannot be looked at is `present: null` with the error, never
  absent. It reads the directory entry and LangSpan's own entries in the host's
  hook and MCP files, with the 1 MiB, no-symlink bound `install` uses; it
  writes nothing.
- The daemon runs the same check at start and every `host_check_ms` (default
  one hour) in its own thread. Hosts that are present, have no user-level
  install and have no project install in a registered workspace reach the state
  owner as one `observe_hosts` request with their names only. The core records
  `host_not_installed` once per host and `host_cleared` when it is covered or
  gone, so an unchanged host is never recorded twice, across restarts too.
  A held host that cannot be inspected at a check (an unreadable directory,
  an unparseable file) stays held and the error is logged: neither an install
  nor a removal is established, so the notice is not cleared.
- `langspan status`, the dashboard and `doctor` show it to the owner; agents
  are not told. The install command `status` and the dashboard name always
  carries the coordinator's `--state-dir`, shell-quoted, since an install for
  another state directory would not coordinate with the coordinator that
  reported the host. `doctor` marks the host as needing attention only when sessions
  of another host use the coordinator.
- Gemini CLI and Antigravity ([ADR 0035](0035-antigravity-adapter.md)) are
  reported but never proposed or recorded, as `install` does not choose them
  by itself.
- The issue asked for no file content to be read. Telling `none` from an
  install needs LangSpan's entries in the host's hook file, so the check reads
  them, bounded, and keeps nothing from the file but the host's name.

### I11, as built

The hooked-host part of row I11 is built, after measuring what it saves:
about 2.6 KB of a 32 KB tool list (8.1% of the full list in `TOOL_NAMES` at the time), as recorded in
[benchmarks/mcp-tools](../../benchmarks/mcp-tools/README.md).

- `langspan mcp --tools all|hooked --host claude-code|codex`. The default is
  `all`. `hooked` is accepted only for Claude Code and Codex; Gemini CLI and
  Cursor follow once their adapters are verified on a real host.
  `install --mcp-tools hooked` writes the flags into those two hosts' entries,
  as a dry run until `--apply`; `uninstall --apply` removes them with the
  entry. LangSpan never switches the mode by itself.
- With `hooked`, the server leaves out `langspan_admit`, `langspan_complete`,
  `langspan_session_register` and `langspan_workspace_register` only while a
  session of that host (told by its ID prefix) in the server's workspace (the
  deepest registered root holding its working directory) has hook activity in
  the current daemon generation within the last 10 minutes. It asks `status`
  every 6 seconds; nothing new is stored.
- No hook event, a quiet period of 10 minutes, a new generation or a daemon
  that does not answer all mean every tool is listed. Each change sends the
  MCP list-changed notification.
- Every tool still answers when called by name. The server instructions name
  the hidden tools and say the hooks call them.
- Not established: a real Claude Code session showing the shorter list in
  `/mcp`, and whether hosts that defer tool schemas gain anything. Integration
  tests use a real daemon and hook events recorded from Claude Code 2.1.331
  and Codex 0.171.0.

## Consequences

- At launch, LangSpan coordinates only the four hooked hosts. The README and
  usage guide name them and no others.
- When rules-file hosts are added, their limits must be stated as such: they
  get advice through MCP, not a gate. That work gets its own ADR.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: a host that gains hooks moves into the adapter work; rules-file hosts start once the four hooked hosts are released and their advice-only limit is written down. A7 is built, as recorded above, and so is the hooked-host part of I11. I8 is no longer decided here: [ADR 0034](0034-langspan-skill.md) records its first part, and its issue holds the specification.
