# ADR 0040: Reserved numbers

**Status:** Implementation decision, 27 November 2026
([issue #299](https://github.com/Langspan/LangSpan.dev/issues/299), the first
slice of roadmap B16, [issue #32](https://github.com/Langspan/LangSpan.dev/issues/32)).
Meets the condition [ADR 0017](0017-large-coordination-features-wait-for-measurement.md)
set for B16.

## Context

On 26 November 2026, while LangSpan was built with LangSpan, five pull
requests took protocol versions 6 to 10 within hours. Each one that merged
second had to renumber, one of them four times. No edit overlapped while they
worked, so briefings and admission had nothing to report. The same happens
with ADR and migration numbers: the clash starts when a session decides on a
number, long before it writes a file.

## Decision

**A session reserves the next number of a named counter.**
`langspan reserve SESSION KEY [AFTER]` answers one more than the larger of
`AFTER` and the highest number the counter has handed out, and records the
claim. `AFTER` is the highest number the caller sees in its own checkout
(default 0). LangSpan reads no repository file to find it, so the command
adds no trust boundary.

- **Resources: counters only.** A key is 1 to 64 bytes of `[a-z0-9._-]`
  (`adr`, `protocol-version`, `migration`), so a key shown in a peer's
  briefing can't carry quoting or instructions. Names and slots are left for
  later.
- **Scope: a repository.** A counter counts across every worktree of one
  repository (its repository ID), or one workspace outside Git. One daemon
  serves every repository of a user, so a counter for the whole domain would
  give one repository another's numbers. Until the coordinator's first scan
  of a workspace records either its repository or that it has none, a
  reservation there is refused with `scope_unknown`: counting it alone would
  let two new worktrees of one repository take the same number. A Git
  checkout that counted alone because its scans failed joins its
  repository's counter, with its claims, as soon as a scan names the
  repository, so every worktree of it counts above every number the checkout
  handed out.
- **Release.** A claim is shown to other sessions while its session runs:
  until the session ends or its process is known to have exited.
  The highest number of each key is kept after its holder ends, so no number
  is handed out twice, and nothing has to be released or taken over. A
  number reserved and never used leaves a gap.
- **Advisory**, like an intent: it holds and refuses nothing.
- **Shown in briefings.** A briefing lists other running sessions' claims in
  the repository, newest first, at most 8, each once (changes-only
  briefings). The `reserve` reply names the other running claims of the key.
- **Retry.** The store's request-ID replay returns the same number.
- **Bounds.** At most 256 keys per repository; each key keeps its newest 16
  claims.
- **Wire.** The `reserve` command, the `reserved` event and outcome, and a
  briefing's `reserved` are the capability `reservations`
  ([ADR 0037](0037-protocol-capabilities.md)), with no new version. A client
  without it is sent no `reserved` field or event, and a `reserve` request
  isn't sent to a daemon without it.

## Consequences

- Two sessions in separate worktrees that each reserve the next ADR get
  different numbers, and each one's briefing names the other's claim.
- The caller must ask before it writes the number; a session that picks one
  without `reserve` is not stopped.
- A number reserved in one repository is not known on another machine; the
  cross-machine metadata doesn't carry claims.
- Not built here (still on issue #32): sequences defined by a directory and a
  file pattern, reading worktrees and the target branch for the highest
  number, expiry when the file appears or the change merges, an MCP tool, and
  names as resources.
