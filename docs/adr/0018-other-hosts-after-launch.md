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
| A7 | Detecting agent tools installed after setup | Not now | Install is an explicit owner step ([ADR 0010](0010-user-level-install.md)); running `langspan install` again covers a new host |
| I7 | An AGENTS.md block and rules-file text | Not now | Comes with the rules-file hosts; it changes files in the owner's repository |
| I8 | A LangSpan skill that teaches agents how to work with it: a general skill for every host, then task skills | Superseded: planned, part 1 built ([ADR 0034](0034-langspan-skill.md)) | The maintainers asked for it, and running LangSpan on its own development showed every agent needed the same instructions by hand |
| I11 | MCP menu trimming: tools hidden from read-only members, and bookkeeping tools hidden once hooks report (added by the second review) | Not now | All 24 tools are always listed. Hiding tools once hooks run needs to know, per host, that they do, and the cost of the 24 descriptions has not been measured |

The Gemini CLI part of row A6 is launch track L10, not this ADR.

## Consequences

- At launch, LangSpan coordinates only the four hooked hosts. The README and
  usage guide name them and no others.
- When rules-file hosts are added, their limits must be stated as such: they
  get advice through MCP, not a gate. That work gets its own ADR.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: a host that gains hooks moves into the adapter work; rules-file hosts start once the four hooked hosts are released and their advice-only limit is written down; A7 follows a user report that re-running `langspan install` was not enough. I8 is no longer decided here: [ADR 0034](0034-langspan-skill.md) records its first part, and its issue holds the specification.
