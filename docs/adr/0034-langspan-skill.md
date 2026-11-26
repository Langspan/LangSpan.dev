# ADR 0034: The LangSpan skill

**Status:** Implementation decision, 25 November 2026. Roadmap row I8, part 1
(issue #87), which replaces the earlier "not now" for I8 under
[ADR 0018](0018-other-hosts-after-launch.md). Tested with a temporary home, the
real executable and the real MCP server (`tests/user_install.rs`,
`tests/cli.rs`, `tests/mcp.rs`, unit tests in `src/skill.rs`). No agent has
been observed using the skill on a real host yet.

**Update, 26 November 2026:** Gemini CLI support was removed (issue #246,
[ADR 0033](0033-gemini-cli-adapter.md)), so the skill has no Gemini CLI form.
`langspan uninstall --hosts gemini-cli` still removes LangSpan's section from
`~/.gemini/GEMINI.md`. The Gemini CLI parts below record the decision as it
was made.

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
| Codex | `~/.codex/AGENTS.md` (under `CODEX_HOME` when set); `AGENTS.override.md` there is read instead when it exists | Global `AGENTS.md` guidance, plain Markdown | At most 32 KiB by default (`project_doc_max_bytes`) of the combined instruction chain: every AGENTS file Codex reads together, not `AGENTS.md` alone |
| Cursor | `.cursor/rules/langspan.mdc` in the project | A project rule: front matter (`description`, `globs`, `alwaysApply`) and Markdown | Its rules documentation advises keeping a rule under 500 lines. **Unverified:** the documentation does not say whether the Cursor CLI reads `.cursor/rules` |
| Gemini CLI (removed) | `~/.gemini/GEMINI.md` (under `GEMINI_CLI_HOME` when set) | The global context file, plain Markdown | None documented |

Cursor documents user rules only as a setting in its own interface, not as a
file, so its form is a project rule. Cursor's documentation covers project
rules in the editor; whether the Cursor CLI reads `.cursor/rules` is not
documented, and stays unverified until a real-host run shows it.

Because Codex's 32 KiB applies to the combined chain, LangSpan's section
shares that budget with the owner's own text in the global file and with
every project AGENTS file Codex adds.

## Decision

**One source text.** `src/skill.md` is the skill. It is compiled into the
executable, and every host's form carries it byte for byte with only the
host's wrapper around it: front matter and an ownership mark for Claude Code
and Cursor, and begin and end marker lines for Codex (and, before its removal,
Gemini CLI). It says
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
Claude Code and Codex (and, before its removal, Gemini CLI), among the chosen hosts, beside the hooks
and MCP entry; without `--skill` it is neither shown nor written. Cursor's
rule is written by `langspan hooks install cursor --workspace ID --root DIR
--skill`; `--skill` with any other host's project install is refused, naming
`langspan install --skill`. A dry run prints each file's path, size and full
text, and writes nothing.

**LangSpan's files only.** For Claude Code and Cursor, LangSpan owns the
whole file, marked with `<!-- langspan-managed:skill:v1 -->`. An existing file
of that name without the mark stops the install before anything is written,
and is never removed. For Codex and an earlier Gemini CLI install, LangSpan owns one section
between `<!-- langspan-managed:skill:v1 begin -->` and `… end -->`; every
other line is kept byte for byte. A second section, or a marker without its
partner, stops the install. Reinstalling replaces LangSpan's file or section
where it is, so hand edits inside it are replaced. `langspan uninstall`
removes the Claude Code file (and its `skills/langspan` folder when that is
then empty) and the Codex section, and `--hosts gemini-cli` removes an earlier
Gemini CLI section; a file that held only the
section is removed with it. `langspan hooks remove cursor` removes Cursor's
rule. Writes use the installer's guarded writer: atomic replacement, new files
`0600`, symlinks refused, 1 MiB bound, and a file changed since it was
planned is planned again, never overwritten; a removal is refused if the file
changed since it was planned. Since LangSpan never writes through a symlink,
a symlinked or otherwise non-regular `AGENTS.md`, `GEMINI.md` or `SKILL.md`
cannot hold LangSpan's file or section: `langspan uninstall` leaves it
untouched rather than failing, so an owner who keeps guidance files in
their dotfiles can always uninstall, and `install --skill` still refuses
it.

**Reported by `doctor`.** For each of Claude Code and Codex whose
configuration directory exists, and for Cursor in `doctor --root DIR` when
Cursor's project hooks are installed, `doctor` reports the skill as not
installed (not a problem, since it is optional), installed and up to date, or
different from this version's (a problem, with the command that replaces it).
A file of the skill's name that LangSpan did not write is reported as a
problem; a symlink is reported as left alone, not as a problem. For Codex,
when `~/.codex/AGENTS.md` holds LangSpan's section, `doctor` reports a
problem if `AGENTS.override.md` exists beside it (Codex reads that instead),
or if the file's bytes up to the end of the section already exceed 32 KiB.
It measures only the global file at the default limit: it does not read a
raised `project_doc_max_bytes` from Codex's configuration, and does not add
the project files a given session's chain includes.

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
- Codex's `AGENTS.override.md` is
  detected in the user directory only.
- Project AGENTS files share Codex's budget with the section, so a project
  whose own files are large can still leave part of the chain unread in a
  way `doctor` does not see.
- The maintainers checked the locations and limits above against each host's
  current documentation (Codex's AGENTS.md guide, Cursor's rules page, Gemini
  CLI's GEMINI.md page and Claude Code's skills page). That check corrected
  Codex's limit to the combined chain and found that Cursor's documentation
  does not say whether its CLI reads project rules; the rest was confirmed.
- Part 2 of I8, the task skills (`langspan-land` and others), builds on this
  module and is not part of this decision.
