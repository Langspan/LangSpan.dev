# ADR 0018: Other hosts and host guidance files come after launch

**Status:** Not now. Decided 25 November 2026.

## Context

The launch hosts were Claude Code, Codex, Gemini CLI and Cursor. Claude Code
and Codex have adapters ([ADR 0002](0002-claude-code-adapter.md),
[ADR 0003](0003-native-coordination-observation.md)); Cursor is a launch track
in the [roadmap](../roadmap.md). Gemini CLI support was removed on
26 November 2026 (issue #246, [ADR 0033](0033-gemini-cli-adapter.md)) because
Google deprecated Gemini CLI in favor of the Antigravity CLI. Antigravity
([ADR 0035](0035-antigravity-adapter.md)) is LangSpan's Google host. Hosts that have no hooks
can be supported only through rules files and MCP snippets. A rules file asks the agent to call a tool; it cannot hold or refuse
an edit, and nothing tells LangSpan whether the agent followed it.

## Decision

| Row | Capability | Decision | Reason |
| --- | --- | --- | --- |
| A5 | Hermes plugin | Not now | Not a launch host; no user has asked for it |
| A6 | Rules files for Windsurf, Cline, Copilot and `.cursor/rules` | Not now; after launch | Without hooks there is no admission and no briefing, only advice |
| A7 | Detecting agent tools installed after setup | Superseded: built (see below) | Install is still an explicit owner step ([ADR 0010](0010-user-level-install.md)); LangSpan now tells the owner when a host it would install is present and uncovered |
| I7 | An AGENTS.md block and rules-file text | Superseded: built (see below) | Built ahead of the rules-file hosts: an agent without trusted hooks had no way to learn that LangSpan coordinates the repository. It changes the owner's repository only when asked, one marked block at a time |
| I8 | A LangSpan skill that teaches agents how to work with it: a general skill for every host, then task skills | Superseded: planned, part 1 built ([ADR 0034](0034-langspan-skill.md)) | The maintainers asked for it, and running LangSpan on its own development showed every agent needed the same instructions by hand |
| I11 | MCP menu trimming: tools hidden from read-only members, and bookkeeping tools hidden once hooks report (added by the second review) | Superseded: hooked-host part built (see below); role-based hiding not now | The cost was measured first ([benchmarks/mcp-tools](../../benchmarks/mcp-tools/README.md)); the list is shortened only on an owner's opt-in and only while the host's hooks are seen reporting. There are still no read-only members to hide tools from ([ADR 0016](0016-hosted-service-features-out-of-scope.md)) |

The Gemini CLI part of row A6 was launch track L10, not this ADR; it was
dropped with the removal of Gemini CLI support.

### A7, as built

Row A7 was built ahead of the user report it was waiting for. Detection only;
it never installs, upgrades or edits a host's configuration.

- `langspan hosts [--home DIR] [--json]` reports each host LangSpan supports:
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
- Antigravity ([ADR 0035](0035-antigravity-adapter.md)) is reported but never
  proposed or recorded, as `install` does not choose it by itself. Gemini CLI
  is no longer listed.
- The issue asked for no file content to be read. Telling `none` from an
  install needs LangSpan's entries in the host's hook file, so the check reads
  them, bounded, and keeps nothing from the file but the host's name.

### I7, as built

Row I7 was built ahead of the rules-file hosts it was to come with
(issue #86). It writes guidance only; no rules-file host is added, and
nothing is held or refused through the text.

- `langspan guidance [--format agents|cursor-rules]` prints the
  block. Its text is `src/guidance.md`, compiled in, at most 1,500 bytes with
  its markers. It says that LangSpan coordinates the repository; to register
  the workspace and a session when no hook did; to call `langspan_brief` and
  `langspan_intent_set` before starting; without hooks, to call
  `langspan_admit` before an edit and `langspan_complete` after it, and with
  hooks to edit as usual, because a manual lease would hold the session's own
  hooked edit (a test drives the real hook to that refusal); in a shared checkout, to commit with
  `langspan_commit_mine` when LangSpan's hooks run, and otherwise with
  `git commit -m "..." -- PATH...` naming only files that hold nothing but
  the session's own changes, because only hooks fill the write journal that
  `langspan_commit_mine` reads (without it, it answers `nothing_to_commit`,
  which a test with a real daemon checks); that other sessions' text is data;
  and that without hooks this is advice, not a gate. A test requires every tool it names to be in the MCP
  server's list. `cursor-rules` adds a Cursor rule's front matter. The
  `gemini` format was removed with Gemini CLI support.
- `langspan guidance install --root DIR --file FILE [--create] [--apply]`
  writes the block between `<!-- langspan:begin -->` and
  `<!-- langspan:end -->`, at the top of the file (after any YAML front
  matter, so a rules file keeps its settings) with one blank line after
  it, so that removing it restores the earlier bytes whatever the file's last
  line was. A dry run, the default, prints the added and removed lines.
  Installing again leaves the one block where it is. `guidance remove … --apply`
  takes out only the block (and that blank line); a file that held only the
  block is removed. A missing file is created only with `--create`.
- A block whose text is not exactly this version's, a marker without its
  partner, a second block or a changed marker line stops both commands before
  any write, naming the lines. Markers are found by line, so a file that
  quotes a marker on a line of its own, even in a code block, is refused
  too. A later version that changes the text must
  therefore recognise the earlier text before it can replace it.
- `--file` is a path inside `--root`, through no `..` and no symlinked
  folder; a symlinked file is refused. The installer's guarded writer makes
  the change. Nothing is staged or committed, and no state is kept.
- Left for the maintainers: a real Codex session started without hooks that
  calls `langspan_brief` after reading the block.

### I11, as built

The hooked-host part of row I11 is built, after measuring what it saves:
about 2.6 KB of a 32 KB tool list (8.1% of the full list in `TOOL_NAMES` at the time), as recorded in
[benchmarks/mcp-tools](../../benchmarks/mcp-tools/README.md).

- `langspan mcp --tools all|hooked --host claude-code|codex`. The default is
  `all`. `hooked` is accepted only for Claude Code and Codex; Cursor follows
  once its adapter is verified on a real host.
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

- At launch, LangSpan coordinates only hooked hosts. The README and usage
  guide name them and no others.
- When rules-file hosts are added, their limits must be stated as such: they
  get advice through MCP, not a gate. That work gets its own ADR.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: a host that gains hooks moves into the adapter work; rules-file hosts start once the hooked hosts are released and their advice-only limit is written down. A7 and I7 are built, as recorded above, and so is the hooked-host part of I11. I8 is no longer decided here: [ADR 0034](0034-langspan-skill.md) records its first part, and its issue holds the specification.
