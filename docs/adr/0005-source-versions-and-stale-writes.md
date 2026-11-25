# ADR 0005: Source versions and stale writes

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026. Extends [ADR 0003](0003-native-coordination-observation.md)
and [ADR 0004](0004-automatic-peer-briefing.md). It closes finding P1-01 of the
M3 capability review for the cases listed under
"Covered", and for no others.

## Context

A tool-input digest protects retry identity. It says nothing about the file
version the agent was looking at. After one session finished an edit and
released its admission, another could replace the whole file from an older
read and be admitted.

## Decision

**A session's known version.** The core records, per session and path, the
version that session last read or wrote: the SHA-256 of the file's bytes, or
"missing". It keeps the 128 most recent per session. The record survives a
daemon restart, because what a session read does not stop being what it read.

**Where versions come from.**

- A completed Claude Code `Read` (PostToolUse) records the file's version as
  the hook sees it on disk.
- A reported successful edit by either host records the resulting version as
  that session's own.
- Nothing else does. In particular a Codex session reads through shell
  commands, which no hook reports.

**The comparison.** When a hook asks to admit an edit it supplies each file's
version on disk at that moment. After the conflict check, the core compares:

| The session's record for the path | Result |
| --- | --- |
| Same version as on disk | Admitted; the base is current. |
| A different version | **`stale_base`: nothing is admitted.** The hook answers `deny` with the paths and, when a peer reported editing the path since, that session's ID, unless that peer's own last version of the file is not what is on disk now (a checkout, reset or other unreported change came after it). |
| None, and the file does not exist | Admitted; a new file has no earlier version. |
| None, and the file exists | Admitted, and the admission lists the path under `base_unknown`. The hook diagnostic says the version was not compared. |

Stale is all or none, like a conflict: one stale file refuses the whole set.
A current holder is reported before any version comparison.

**One refusal per change.** A refusal records the file's current version as
the session's known version, marked as learned from a refusal. The same change
is therefore refused once. This is deliberate. A Codex session has no reported
read with which to refresh its record, and a deleted file cannot be read at
all; without this rule either would be refused for ever. The cost is that an
agent which ignores the refusal and repeats a whole-file write is admitted the
second time. Prevention here is one refusal with the reason, before the tool
runs. It is not a lock on the file.

**After the session's own edit.** A reported successful edit first drops the
session's known version of each edited path, then records the new one. If the
second step is lost, the base is unknown, never the old version.

**Whose record (added 24 November 2026).** A Claude Code subagent is a
session of its own ([ADR 0004](0004-automatic-peer-briefing.md)), so each
subagent, and its parent, holds its own versions. A sibling's write to a file
another subagent read makes that subagent's next edit of it stale, and the
refusal names the sibling's session.

**A post-tool report that never arrived (backlog item 5).** If the daemon is
down when a session's PostToolUse runs, the session's own write is never
recorded, and after the restart its record still holds the version it read.
Its next edit of that file is then refused once, as "changed outside any
reported edit", and a peer is never named for it: only a peer's reported edit
after the read names that peer. The rule above admits the retry. Tested with a
real daemon stopped and started between the write and its report
(`a_post_tool_event_lost_while_the_daemon_is_down_costs_at_most_one_refusal_naming_no_peer`
in `tests/stale_source.rs`); the behaviour needed no change.

**One file, one key (backlog item 12).** A file key is the path below the
root as the filesystem resolves it (`fs::canonicalize`, which is `realpath`).
On macOS that returns the name as stored, so on a case-insensitive APFS
volume `A.TXT` and `a.txt`, and an NFD and an NFC spelling of `café.txt`, are
one key in the hook and in the daemon. On a case- and normalisation-sensitive
filesystem (usual on Linux) they are different files and different keys,
which is correct. **Limit:** a file that does not exist yet has no stored name
to resolve against, so two sessions creating it at once under two spellings
get two keys, and neither is held against the other; once it exists every
spelling resolves to one. Normalising a new name to NFC would need a Unicode
normalisation table, which is not worth a dependency for this case. Test:
`one_existing_file_under_two_spellings_is_one_file_key` in
`tests/stale_source.rs`, which skips with a message on a case- or
normalisation-sensitive filesystem.

**Notice after the fact.** A briefing also lists files the session read that a
peer in the same checkout has since reported editing, so the session learns
of it at its next prompt, before it tries to edit.

**Bounds.** A file over 4 MiB, a symlink or a special file has no version; its
base is unknown. At most 64 versions per request.

## Covered, and not covered

Prevention means the host is told `deny` before the tool runs. Detection means
it is reported afterwards.

| Case | Outcome |
| --- | --- |
| Claude Code edits a file it read, changed since by anyone or anything | Refused once. Claude Code's own read check also refuses this, and runs first ([evidence](../evidence/m6-stale-source/README.md)). |
| Either host edits a file it previously edited, changed since | Refused once, with the reason. A retry of the same change is admitted. |
| A file the session knew was deleted | Refused once; the next write is admitted as a new file. |
| Codex's first edit of a file | **Not compared.** Admitted with `base_unknown`. Codex's patch context must still match the file for the host to apply it. Since [ADR 0027](0027-turn-and-batch-events.md) the admission records the file as seen (origin `snapshot`), so a retry after a failed first patch is compared. |
| Any session's first edit of a file it never read through a reported tool | Not compared; `base_unknown`. |
| A write that does not pass through a supported hook (shell, editor, formatter, unsupported tool, a Codex subagent) | **Not prevented.** The next compared edit of that file by a session with an older record is refused, and the Git observer records the change unattributed. |
| A shell command run through a host's `Bash` tool | Since [ADR 0011](0011-shell-writes.md): literal write forms are compared like an edit before they run; other commands' changes are recorded afterwards as the session's own versions, when the comparison is exact. |
| A change between the hook's reading of the file and the host's write | Not prevented. Admission is cooperative, not a filesystem fence. |
| A file in another worktree or clone | Not compared. Versions are per physical workspace; cross-worktree overlap is advisory ([ADR 0004](0004-automatic-peer-briefing.md)). |

## Consequences

- A session that changes a file through an unreported path (for example a shell
  command) is refused once on its next reported edit of it. It should read the
  file again; it is not refused again for that change whether or not it does.
- A hook call redelivered by the host after the file changed carries a
  different base under the same request ID. It is answered as a conflicting
  retry and the hook gives no decision. Delivered again with the same base,
  it is the same request and is refused again with the same reason
  (ADR 0028).
- The recorded read version is the file when the read hook ran, not the bytes
  the model received. A change in the milliseconds between the two is missed.
- A read of part of a file records the whole file's version.
- `Admit` from the CLI or MCP carries no base and is never compared.
- The protocol stays at version 3, which is unreleased: `admit_tool` gains an
  optional `base`, and there are two new outcomes and one new command.
