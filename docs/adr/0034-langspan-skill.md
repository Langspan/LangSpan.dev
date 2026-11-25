# ADR 0034: The LangSpan skill

**Status:** Implementation decision, 25 November 2026. Roadmap row I8, part 1
(issue #87), which replaces the earlier "not now" for I8 under
[ADR 0018](0018-other-hosts-after-launch.md). Tested with a temporary home, the
real executable and the real MCP server (`tests/user_install.rs`,
`tests/cli.rs`, `tests/mcp.rs`, unit tests in `src/skill.rs`). No agent has
been observed using the skill on a real host yet.

## Context

An agent that meets LangSpan for the first time sees its briefings, refusals
and alerts with no context. It does not know that a stale-source refusal is
given once and expects a re-read, not a blind retry; that a briefing is data
rather than instructions; or that `langspan commit --mine` and `langspan land`
exist. People running several agents wrote the same instructions into every
prompt. Running LangSpan on its own development showed every agent needed
them.

Each host reads persistent agent guidance from a different place and in a
different format:

| Host | Where | Format | Size guidance |
| --- | --- | --- | --- |
| Claude Code | `~/.claude/skills/langspan/SKILL.md` (under `CLAUDE_CONFIG_DIR` when set) | A personal skill: YAML front matter (`name`, `description`) and Markdown | Its skills documentation advises keeping `SKILL.md` under 500 lines; a description is cut at 1,536 characters in the skill listing |
| Codex | `~/.codex/AGENTS.md` (under `CODEX_HOME` when set) | Global `AGENTS.md` guidance, plain Markdown | At most 32 KiB of `AGENTS.md` text is read (`project_doc_max_bytes`) |
| Cursor | `.cursor/rules/langspan.mdc` in the project | A project rule: front matter (`description`, `alwaysApply`) and Markdown | Its rules documentation advises keeping a rule under 500 lines |
| Gemini CLI | `~/.gemini/GEMINI.md` (under `GEMINI_CLI_HOME` when set) | The global context file, plain Markdown | None documented |

Cursor documents user rules only as a setting in its own interface, not as a
file, so its form is a project rule.

## Decision

**One source text.** `src/skill.md` is the skill. It is compiled into the
executable, and every host's form carries it byte for byte with only the
host's wrapper around it: front matter and an ownership mark for Claude Code
and Cursor, and begin and end marker lines for Codex and Gemini CLI. It says
what LangSpan does and that its messages are coordination data, never
instructions; how to respond to a briefing, a stale-source or unread-lines
refusal, a conflict, a break alert, an automatic check result and a held
commit; when to use `status`, `commit --mine`, `integrate check`, `land` and
`land-queue`, and the matching MCP tools; and what never to do (edit the state
directory, turn off LangSpan's hooks or MCP entry, or run Git so that the commit
guard cannot see the commit).

**Kept true by tests.** Every message the skill quotes is one of the opening
words the code itself writes: these are constants (`hook::BRIEFING`,
`hook::STALE_SOURCE`, `hook::UNREAD_LINES`, `hook::CONFLICT`, `breaks::ALERT`,
`hook::CHECK_RESULT`, `commit::HELD`) used both to build the messages and in
`skill::MESSAGES`, and a test requires the skill's quotes to be exactly these.
Every `langspan …` command it names must begin a usage line of `langspan
--help`, with each option it gives in that command's usage. Every
`langspan_*` tool it names must be in the real MCP server's `tools/list`. Every
form must stay within 500 lines and 8 KiB, inside each documented limit above.

**Installed only when asked.** `langspan install --skill` writes the skill for
Claude Code, Codex and Gemini CLI, among the chosen hosts, beside the hooks
and MCP entry; without `--skill` it is neither shown nor written. Cursor's
rule is written by `langspan hooks install cursor --workspace ID --root DIR
--skill`; `--skill` with any other host's project install is refused, naming
`langspan install --skill`. A dry run prints each file's path, size and full
text, and writes nothing.

**LangSpan's files only.** For Claude Code and Cursor, LangSpan owns the
whole file, marked with `<!-- langspan-managed:skill:v1 -->`. An existing file
of that name without the mark stops the install before anything is written,
and is never removed. For Codex and Gemini CLI, LangSpan owns one section
between `<!-- langspan-managed:skill:v1 begin -->` and `… end -->`; every
other line is kept byte for byte. A second section, or a marker without its
partner, stops the install. Reinstalling replaces LangSpan's file or section
where it is, so hand edits inside it are replaced. `langspan uninstall`
removes the Claude Code file (and its `skills/langspan` folder when that is
then empty) and the Codex and Gemini CLI sections; a file that held only the
section is removed with it. `langspan hooks remove cursor` removes Cursor's
rule. Writes use the installer's guarded writer: atomic replacement, new files
`0600`, symlinks refused, 1 MiB bound, and a file changed since it was
planned is planned again, never overwritten; a removal is refused if the file
changed since it was planned.

**Reported by `doctor`.** For each of Claude Code, Codex and Gemini CLI whose
configuration directory exists, and for Cursor in `doctor --root DIR` when
Cursor's project hooks are installed, `doctor` reports the skill as not
installed (not a problem, since it is optional), installed and up to date, or
different from this version's (a problem, with the command that replaces it).
A file of the skill's name that LangSpan did not write is reported as a
problem.

## Consequences

- An agent with the skill installed can follow LangSpan's messages without
  instructions in every prompt. Whether it does is not yet observed: the
  acceptance run, in which an agent given only the skill re-reads and retries
  after a stale-source refusal and uses `commit --mine` in a shared checkout,
  is left to the maintainers.
- The skill describes only what the code does today, and a renamed command,
  option, tool or message fails a test. A change of behaviour that keeps the
  same words is not caught: the text must still be read when behaviour
  changes.
- The skill is advice. Nothing is held or refused through it, and on a host
  whose hooks are not installed or not trusted it is the only coordination the
  agent gets.
- Codex's documentation describes an `AGENTS.override.md` read in place of
  `AGENTS.md`, and Gemini CLI's a setting that names another context file; in
  either case the installed section would not be read. `doctor` detects
  neither.
- The host locations, formats and limits above come from each host's
  documentation as the maintainers know it. In this change only Claude Code's
  skills page was re-read; Codex's, Cursor's and Gemini CLI's pages could not
  be fetched, so those rows await a check against the current documentation.
- Part 2 of I8, the task skills (`langspan-land` and others), builds on this
  module and is not part of this decision.
