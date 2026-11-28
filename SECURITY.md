# Security policy

## Supported versions

LangSpan is before 1.0. Security fixes go into the latest release only.

## Reporting a vulnerability

Report it privately through GitHub: on the repository's Security tab choose
"Report a vulnerability", or open
<https://github.com/Langspan/LangSpan.dev/security/advisories/new>. Only the
maintainers see the report. Please do not open a public issue, pull request or
discussion for a suspected vulnerability.

Include:

- the version (`langspan --version`) and how you installed it;
- your operating system and architecture;
- the host and its version (Claude Code, Codex, Antigravity or Cursor), if one
  is involved;
- the steps that show the problem, and what you expected instead.

## What happens next

A report is acknowledged within 7 days, and an assessment (whether it is a
vulnerability, how severe, and the plan to fix it) follows within 30 days. A
vulnerability is disclosed when its fix is released, or 90 days after the
report if no fix is ready by then, unless the reporter and the maintainers
agree on another date.

A fixed vulnerability is published as a GitHub security advisory, with a CVE
where one is warranted, when the fix is released. Reporters who want credit
are credited in the advisory.

## Scope

In scope: the `langspan` binary, the hooks and MCP entries it installs, the
local coordinator and its socket, the dashboard, remote publication through
Git refs, and the release archives published on this repository's Releases
page.

Out of scope: Claude Code, Codex, Antigravity, Cursor and other hosts
themselves; report those to their vendors.

What LangSpan does and does not protect against, including the limits of
cooperative admission, is in
[docs/security/threat-model.md](docs/security/threat-model.md).
