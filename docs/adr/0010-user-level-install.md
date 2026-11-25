# ADR 0010: User-level install and MCP registration

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026. Tested against a temporary home with the real daemon, Git and
hook executable. Not yet observed in a real Claude Code or Codex session.

## Context

`langspan hooks install` covers one checkout and writes that checkout's
workspace ID and root into the hook command. A tool installed once per user,
with its MCP server registered in each host, avoids that. Without it, every
new repository needs a manual step, and the MCP tools need a hand-written host entry.

## Host facts and where they come from

Checked on 24 November 2026 against the hosts' own documentation and, read
only, the `--help` output of Claude Code 2.1.330 and codex-cli 0.171.0. No
command that writes was run, and no real user configuration was read.

| Fact | Source | Verified? |
| --- | --- | --- |
| Claude Code user hooks live in `~/.claude/settings.json` under `hooks`, same format as project settings | <https://code.claude.com/docs/en/settings>, <https://code.claude.com/docs/en/hooks> ("Hook locations"), <https://code.claude.com/docs/en/claude-directory> | Yes |
| Claude Code hooks from user, project and local settings all run ("Hook entries merge across settings levels") | <https://code.claude.com/docs/en/hooks> | Yes |
| The hook input's `cwd` is the session's working directory and follows `cd` and worktrees; `CLAUDE_PROJECT_DIR` is the directory the session started in | <https://code.claude.com/docs/en/hooks> | Yes |
| In a Git repository, Claude Code reads `.claude/settings.local.json` at the repository root, and for a worktree at the main checkout's root; `.claude/settings.json` from the session's primary working directory | <https://code.claude.com/docs/en/settings> | Yes |
| User-scope MCP servers are stored in `~/.claude.json` | <https://code.claude.com/docs/en/mcp> (scope table), <https://code.claude.com/docs/en/settings> | Yes |
| ...under its top-level `mcpServers` key | Inferred: the docs show local scope as `projects["<path>"].mcpServers` and the stdio entry shape `{"type":"stdio","command":...,"args":[...]}`, but never name the user-scope key path | **No: inferred** |
| Claude Code tolerates an unknown key (such as `"langspan-managed"`) inside an MCP server entry | Not documented | **No** |
| `CLAUDE_CONFIG_DIR` moves `~/.claude` and `~/.claude.json` | <https://code.claude.com/docs/en/claude-directory> | Yes |
| `claude mcp add-json --scope user NAME JSON` is the host's own way to add a user server | `claude mcp --help`, `claude mcp add-json --help` (2.1.330) | Yes |
| Codex user hooks live in `$CODEX_HOME/hooks.json`, `~/.codex/hooks.json` by default; project hooks in `.codex/hooks.json` of a trusted project | <https://learn.chatgpt.com/docs/hooks> (redirected from developers.openai.com/codex/hooks) | Yes |
| Codex hooks are on by default; matching hooks from multiple files all run, and the same command in two files can run twice | <https://learn.chatgpt.com/docs/hooks> | Yes |
| Codex runs a non-managed hook, user-level included, only after the user reviews and trusts that exact definition (`/hooks`) | <https://learn.chatgpt.com/docs/hooks> | Yes |
| Codex MCP servers are `[mcp_servers.<name>]` tables with `command` and optional `args`, `env`, in `~/.codex/config.toml` | <https://learn.chatgpt.com/docs/extend/mcp?surface=cli>, `codex mcp --help` (0.171.0) | Yes |
| Codex rejects unknown keys in an `mcp_servers` table | Not documented | **No**; not relied on |

## Decision

**Commands.** `langspan install [--home DIR] [--hosts claude-code,codex]
[--prompt-summary off] [--apply]` and `langspan uninstall [--home DIR]
[--hosts ...] [--apply]`. Dry run by default: every file is printed as it would
become, except `~/.claude.json`, where only LangSpan's entry is shown because
the rest of that file is the host's account and project state. Every file is planned, and every refusal raised, before any file is
written. Hosts default to those whose configuration directory exists. An
explicit `--home` ignores `HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`LANGSPAN_STATE_DIR` and `XDG_STATE_HOME`, so a run against a test home cannot
reach the real one; without it, `CLAUDE_CONFIG_DIR` and `CODEX_HOME` are
honoured as each host documents.

**What is written where.**

| Host | File | Entry |
| --- | --- | --- |
| Claude Code | `~/.claude/settings.json` | The same events and matchers as the project install, with a command that names no workspace and no root, marked `# langspan-managed:claude-code:user:v1` |
| Claude Code | `~/.claude.json` | `mcpServers.langspan = {"type":"stdio","command":"<langspan>","args":["mcp","--state-dir","<dir>"]}` |
| Codex | `~/.codex/hooks.json` | As for Claude Code, marked `# langspan-managed:codex:user:v1` |
| Codex | `~/.codex/config.toml` | A `[mcp_servers.langspan]` table between `# langspan-managed:mcp:v1 begin` and `... end` comment lines |

The user-level marker differs from the project marker, so `hooks remove` and
`uninstall` never remove each other's entries, and the deferral check below
never mistakes one for the other.

**Ownership of the MCP entry.** Claude Code's tolerance of an extra key is not
documented, so the JSON entry carries no marker. It is LangSpan's only in the
exact generated shape: `type` stdio or absent, an absolute executable named
`langspan`, exactly the arguments `mcp --state-dir DIR`, and at most an empty
`env`. In TOML the marker comments delimit the table. The TOML edit is
line-oriented rather than a TOML parser: the block is appended at the end,
replaced in place, or removed, and every other byte stays as it was. Anything
else that defines `mcp_servers.langspan` (a header, quoted or spaced, a
sub-table, a dotted key, an inline table, or a bare key after LangSpan's block)
makes `install` refuse with nothing written. A multi-line string that happens
to contain such text also causes a refusal, never an edit. No TOML crate was
added.

**The state directory is always explicit.** Both the hooks and the MCP server
get `--state-dir`. The task asked for it only when non-default; it is always
written because a host may start the MCP server with a different environment
than the one `install` ran in, and a `LANGSPAN_STATE_DIR` or `XDG_STATE_HOME`
there would point the MCP server at a different coordinator than the hooks.

**Claude Code's `~/.claude.json` is never created.** Claude Code writes it for
itself and keeps account and per-project state in it. When it is missing,
`install` still installs the hooks and says to start Claude Code once and run
again. Uninstall removes the `mcpServers` key only when removing LangSpan's
entry left it empty.

**Workspace from the event.** `langspan hook claude-code|codex` without
`--workspace` and `--root` is the user-level form. The root is the Git
toplevel of the event's `cwd` (`git::identity`), and the workspace ID is
`wire_id("root", [canonical root])`, `lsh1-root-<32 hex>`, so the same root
always gets the same ID and each linked worktree its own. An event without a
`cwd`, or with one outside a Git repository, gets no decision and the hook
prints nothing (`unsupported`). Giving only one of `--workspace` and `--root`
is an error. The project-level form is unchanged.

**A project install takes precedence.** Both hosts run user and project hooks
for the same event, so without a rule both would coordinate it under two
workspace IDs. Before doing anything, the user-level hook looks for a
LangSpan project-level entry for the same host in every directory from `cwd`
up to the repository root, in the main checkout (where Claude Code keeps a
worktree's local settings) and, for Claude Code, in `CLAUDE_PROJECT_DIR`. The
files are `.claude/settings.local.json` and `.claude/settings.json`, or
`.codex/hooks.json`. If one has a LangSpan-managed command, the user-level
hook prints nothing and logs coverage `deferred_to_project`. A project file
that cannot be read safely (a symlink, over 1 MiB, invalid JSON) gives no
decision rather than a guess.

**The executable path.** Hooks and the MCP entry name the running binary by
its canonical path, with one exception. Under Homebrew that path is the
versioned keg, `PREFIX/Cellar/langspan/VERSION/bin/langspan`, and `brew
upgrade` deletes the old keg, so every entry would name a missing file. When
the canonical path has that shape, `install` and `hooks install` write
`PREFIX/opt/langspan/bin/langspan`, or failing that `PREFIX/bin/langspan`, if
it exists and resolves to the same binary; Homebrew moves both links to the
new keg on upgrade. Otherwise the canonical path is written as before.
`install::installed_executable` owns this rule. When an entry does name a
missing file, `doctor` says which install command to run again.

**Doctor.** For each host whose configuration directory exists, `doctor`
reports whether the user-level hooks are installed and for how many events,
whether the MCP server is registered, whether the executable each names still
exists, and which state directory each points at. Hooks without the MCP entry,
or the reverse, or a `langspan` server LangSpan did not write, need attention.

## Consequences

- Every repository is covered after one command, for both hosts.
- The user-level hook reads up to three small project files per directory
  level on each event. They are bounded at 1 MiB each like every settings read;
  no timing was measured.
- Writing to `~/.claude.json` races with a running Claude Code, which also
  rewrites it. The replace is atomic, so neither write is torn. Just before
  the rename, LangSpan reads the file again and compares it with the bytes the
  plan was made from; if Claude Code changed it meanwhile, its change is kept
  and LangSpan's entry is planned once more against the new content. If the
  file changes a second time, nothing is written and the error asks to close
  the program changing it. The same check guards every file `install`,
  `uninstall` and `hooks install` write. What remains is the instant between
  that read and the rename: without a lock the hosts honour it cannot be
  closed, so running `install` with Claude Code closed is still safest. If the file is
  over 1 MiB, `install` refuses; `claude mcp add-json --scope user langspan
  '<entry>'` is the host's own route.
- If the user-scope key turns out not to be top-level `mcpServers`, Claude Code
  will ignore the entry and `doctor` will still report it as registered. A real
  session listing `langspan` in `/mcp` is the check still to do.
- A Codex user-level hook does nothing until the owner trusts it in `/hooks`.
- A repository can carry a committed `.claude/settings.json` with a
  LangSpan-shaped project hook. The user-level hook then defers to that hook,
  which runs only if the host trusts the repository. That is the same outcome
  as the repository opting out of coordination.
- Uninstall leaves a file that LangSpan created (for example a new
  `hooks.json`) in place as `{}` or empty text, rather than deleting files.
