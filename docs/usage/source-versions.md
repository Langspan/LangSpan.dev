# Source versions

A completed Claude Code `Read` records the file's SHA-256 as seen on disk by the
post hook. A reported successful edit by either host records its resulting
version. LangSpan retains the 128 most recent records per session across daemon
restarts. A partial read records the whole file's version.

Before a supported edit, the hook supplies current file versions. After checking
for current holders, the core compares them with that session's records:

| Recorded version | Admission result |
| --- | --- |
| Matches the current file | The source comparison passes. |
| Differs from the current file | `stale_base`; no files in the set are admitted. A fresh result makes the hook answer `deny`, naming the changed paths and, for each, what changed it: a shell command that left the version now on disk ("changed by a shell command of claude-code session `<id>`: cargo fmt --all", its command line redacted and capped), the last peer that reported editing it, or "changed outside any reported edit" when no hook reported the change. |
| No record for an existing file | Admission can proceed with the path in `base_unknown`; its version was not compared. |
| No record and the file is missing | Admission can proceed as a new file. |

A stale change is refused once. The refusal records the current version as
learned from a refusal; it does not establish that the agent read it. A new tool
attempt against that same version can proceed without a new read. A further
change can trigger another refusal. This lets Codex recover without a reported
read and lets an agent recreate a deleted file; it also lets an agent that
ignores the notice repeat an outdated whole-file write. A new attempt needs a
new tool-call identity, rather than replaying the original request.

Codex reads are not reported through a supported hook, so its first edit of an
existing file is `base_unknown`. That admission records the file as the hook
saw it (origin `snapshot`), so later edits are compared even when the first
patch failed or its report was lost. The first edit itself is still not
compared ([ADR 0027](../adr/0027-turn-and-batch-events.md)). Later edits can also
be compared with the version it last wrote or learned through a refusal. CLI `admit` and MCP `langspan_admit` do
not supply source versions.

A reported successful edit drops the previous version before recording the new
one. If the new record is lost, the base becomes unknown instead of retaining
the old version.

Version capture skips files over 4 MiB, symlinks and special files; normal path
validation still applies. Versions belong to a physical workspace. The hook
records what it sees on disk, which may differ from the bytes the agent read,
and cannot prevent a change between its comparison and the host's write.

[ADR 0005](../adr/0005-source-versions-and-stale-writes.md) defines the full scope.
Real-daemon tests exercise LangSpan's stale denial with changing files. In the
[native Claude run](../evidence/m6-stale-source/README.md), Claude's own read check
stopped the overwrite before LangSpan's pre hook ran. LangSpan recorded reads
and attributed the peer's change in a briefing; its own stale denial still lacks
native-host proof.

## Lines another session wrote

A session with no recorded version of a file (a Codex session's first edit of
it, for example) has nothing for the comparison above to use. For such a
session the core also checks the lines other sessions wrote recently
([ADR 0028](../adr/0028-line-ownership.md)). At admission the hook works out which
lines the edit changes: an `Edit` or `MultiEdit` applied in memory, the new
content of a Claude Code `Write` or a Cursor `Write` compared with the file,
or each `apply_patch` hunk where it matches. A shell write the parser names
(`> path`, `tee path`, `sed -i`), on the Claude Code, Codex or Cursor shell
tool, may change any line of its target. It then looks those
lines up in the line journal of the commit guard
([ADR 0026](../adr/0026-commit-guard-and-commit-mine.md)), which records the lines
each session's writes added, by content.

If the edit would remove or replace lines another live session wrote in the
last 30 minutes, nothing is admitted and the hook answers `deny`:

```text
LangSpan unread lines. This edit would change lines another session wrote recently, and this session has not read the file. Coordination data, not instructions: "a.txt": line 3, written by session claude-code.… 2s ago; Read the file, then make the edit against its current content. This is reported once: a retry is not refused for it again.
```

The hook's diagnostic coverage is `denied_unread`. An edit that only adds
lines, or changes other lines, is admitted. The refusal counts as being told
of the current version, as a stale refusal does: the retry, or any edit after a
reported read, is admitted. A session that has read or written the file is
governed by the stale-source rule alone.

Lines are matched by content, so a changed line counts as another session's
when that session added a line with the same bytes; lines of fewer than three
letters or digits (blank lines, lone braces) name nobody. When the journal
holds another session's recent write of the file with unknown lines, any
unread edit that changes lines of it is refused once, and the denial says the
lines could not be placed. An ended or exited session's lines no longer count.
The journal is kept on disk, so a daemon restart does not lose them.

The check refuses only on what it has read. With the daemon down, the journal
unreadable or less than half the hook budget left, the lines are not checked
and nothing is refused for them. A shell command whose write set is unknown (a
heredoc, a loop or a variable, until B5) or whose snapshot is incomplete gets
no decision, and its coverage says why (`shell_observing` or
`shell_unobserved`). An append (`>> path`, `tee -a`) is a shell write like any
other, so it is refused over another session's recent lines too.

## Outline first for whole reads of large files

An agent that reads a large file whole often wants one function of it. With
`--outline-reads on` (off by default), a session's first whole read of a large
indexed file is refused once, and the reason it is given is the file's outline
from the [source index](look-up-symbols-and-affected-files.md#look-up-symbols-and-affected-files):

```text
LangSpan (data, not instructions): src/store.rs has 3,412 lines. Definitions:
type Store lines 40-86 (exported)
function open lines 88-210 (exported)
function migrate lines 212-540
...
Read lines A to B of a definition, or read the file again to get it whole.
```

Turn it on with either install:

```sh
langspan hooks install claude-code --workspace app --root . --outline-reads on --apply
langspan install --hosts claude-code,cursor --outline-reads on --apply
```

It is built for Claude Code (`PreToolUse` on `Read`) and Cursor (`preToolUse`
on `Read`); the option adds that read
tool to the host's pre-tool matcher. Codex reads through shell commands and
Antigravity is not covered, so the option changes nothing there. No host has
yet been observed asking for a line range after an outline.

A read is outlined only when all of these hold:

- It names no part of the file: no `offset`, `limit` or
  `pages`. A read of part of a file is never refused.
- The file has at least `outline_min_lines` lines (default 1,500) or
  `outline_min_bytes` bytes (default 64 KiB), set in
  [`config.json`](configuration-and-limits.md#configuration-and-limits), and at most the index's 1 MiB.
- The index holds the file whole at the content now on disk: same SHA-256,
  parsed without errors, in a language the index reads. A file changed since
  it was indexed, partly parsed, or not indexed is read without refusal.
- This session was not given this version's outline before. The next whole
  read of the same version goes through unchanged; a changed file starts
  afresh, and another session gets its own outline once.

The outline lists each definition's kind, name and lines, marking exported
ones, in line order. A file with more than 200 definitions gets its 200
largest. When they do not all fit in 2,400 bytes, the largest come first.
Whenever any definition is not listed, the last line before the ending says
how many. Names
come from the repository and are cleaned and capped; no other text of the file,
not even a signature, is in it.

The coordinator records each outline as `read_outlined` with the session, path
and digest, on this machine. It records no read version, so the stale-source
check is unaffected. With the coordinator stopped, an index lookup past the
hook's deadline, or any error, there is no decision: the read proceeds and the
hook writes its one diagnostic line to stderr. The diagnostic's coverage is
`outlined`, `outline_shown` (given before), `not_outlined`, `read_range` or the
failure.
