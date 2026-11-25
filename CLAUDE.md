# LangSpan, for Claude

@AGENTS.md
@docs/mission.md
@docs/engineering.md

## Claude-specific
- `.claude/settings.json` denies force pushes, `--no-verify` and reading secret files. These rules catch the usual way of writing a command, but they are not a security boundary. Never rephrase a command to get past one; ask the owner instead.
- Use subagents for broad research or parallel investigation, and keep the main context for decisions.
