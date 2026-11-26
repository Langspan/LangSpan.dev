# ADR 0023: Richer messages, presence, wake and session continuity wait

**Status:** Not now. Decided 25 November 2026.

## Context

LangSpan delivers peer work at session start, at each prompt and, on Claude
Code, after each completed tool call ([ADR 0004](0004-automatic-peer-briefing.md)).
Messages, handoffs and notes are local to one machine and arrive in the next
briefing ([ADR 0009](0009-agent-messages-and-handoff.md)). A richer design would push
messages into open sessions, start an agent to take a message, show where
agents are looking, and build handoffs and notes with more structure.
The capability review of 25 November 2026 ([roadmap](../roadmap.md))
lists these as Missing or Partial.

The launch bar includes Codex reads, Stop and SessionEnd (L2) and Claude Code
Stop, SubagentStop and PostToolBatch (L3). Several rows here depend on those
events and can follow them.

## Decision

| Row | Capability | LangSpan today | Decision and reason |
| --- | --- | --- | --- |
| A13 | Reads, greps and shell calls shown to peers as presence | Reads recorded as versions only | Built 25 November 2026 ([issue #18](https://github.com/Langspan/LangSpan.dev/issues/18)), ahead of the condition below: presence, see [ADR 0004](0004-automatic-peer-briefing.md). To limit the volume, it is at most 4 paths per peer, dropped first at the cap, and never news mid-turn. Not yet run on a host |
| A14 | Briefing for a repository first reached mid-session | Workspace from the event's working directory | Not now |
| A15 | Mid-turn delivery on Codex | Next prompt only | Not now. Waits for confirmation that Codex honours added context on PostToolUse |
| A16 | Instant message push into an open session | Next hook | Not now. Uses a host messaging socket: a new trust boundary |
| A17 | Wake: an unattended agent started for an untaken message | None | Not now. Starting agents without the owner is a large step in autonomy and needs its own ADR |
| B12 | Tripwire that fires into the next editor's tool result | Notes told at the next prompt | Not now |
| C3 | Start-of-session recency: changed modules, renames, hotspots, abandoned work | Peers' recent edits and task lines | Built 25 November 2026 ([issue #42](https://github.com/Langspan/LangSpan.dev/issues/42)), ahead of the condition below: see "Recent change at session start (C3)" below. Not yet run on a host |
| C8 | Two tasks on the same code told before either writes | Peers ranked by overlap | Built 25 November 2026 ([issue #45](https://github.com/Langspan/LangSpan.dev/issues/45)), ahead of the condition below: prompt targets, see [ADR 0004](0004-automatic-peer-briefing.md). Not yet run on a host |
| C9 | Automatic handoff and trail at session end | Manual `handoff` | Built 26 November 2026 ([issue #46](https://github.com/Langspan/LangSpan.dev/issues/46)): a `trail_recorded` event at each session end, taken once by the next session; see "Trails at session end (C9)" below. Not yet run on a host |
| C12 | Worktree advice and tidying | None | Not now. Tidying would remove the owner's worktrees |
| E1 | Notes anchored to a definition with staleness, tags and confidence | One-line notes on a workspace or files | Built 26 November 2026 ([issue #60](https://github.com/Langspan/LangSpan.dev/issues/60)), ahead of the condition below: `remember --symbol` and `--tag`, an `anchor_state` checked after each index pass, and `recap --tag`; see "Anchored notes (E1)" below. No confidence score. Not yet run on a host |
| E2 | `recall` search | Word search over held notes | Built 25 November 2026 ([issue #61](https://github.com/Langspan/LangSpan.dev/issues/61)), ahead of the condition below: `langspan recall` and MCP `langspan_recall`, see "Note search (E2)" below. Not yet run on a host |
| E4 | Messages addressed by path and symbol | To one session or all | Not now |
| E7 | Sender signed automatically | `--from SESSION` | Not now |
| E9 | Messages across machines | Local only | Not now. Could ride the Git-ref transport (ADR 0006), which would widen its payload and needs that ADR's review |
| D10 | `list_activity` and an MCP activity resource an agent can read (added by the second review) | MCP tools `langspan_status`, `langspan_changes`, `langspan_events` | Not now. The tools answer the same reads; a resource adds a surface no host has been shown to use |
| E11 | Retract a sent message, telling readers to disregard it (added by the second review) | `langspan messages retract`, MCP `langspan_retract_message` | Built 25 November 2026 ([issue #66](https://github.com/Langspan/LangSpan.dev/issues/66)), ahead of the condition below: a `message_retracted` event that briefings, the recap and the dashboard honour, see [ADR 0009](0009-agent-messages-and-handoff.md). Not yet run on a host |
| E12 | Delivery and seen receipts the sender can ask for (added by the second review) | `langspan messages status`, MCP `langspan_message_status`: each recipient `delivered` (with `via` and age), `pending` or `ended` | Built 26 November 2026 ([issue #67](https://github.com/Langspan/LangSpan.dev/issues/67)), ahead of the condition below: delivery receipts only, read-only, from core state and the event log; see "Delivery status (E12)" in [ADR 0009](0009-agent-messages-and-handoff.md). No "seen" state: delivered means placed in context, not acted on. Not yet run on a host |
| E13 | Show a shortened message in full (added by the second review) | A cut line ends with `… (cut; langspan messages show -- WORKSPACE ID)` within its 700 bytes; `langspan messages show` and MCP `langspan_show_message` return the message as sent, read-only, from its `message_sent` event | Built 25 November 2026 ([issue #68](https://github.com/Langspan/LangSpan.dev/issues/68)), ahead of the condition below: no new event or storage, and `retracted` reports a later E11 withdrawal; see [ADR 0009](0009-agent-messages-and-handoff.md). Not yet run on a host. Recorded condition: messages are capped at 400 bytes when sent, so a delivered line is cut only when quoting takes it past 700 bytes |

## Recent change at session start (C3)

A session-start hook (Claude Code and Codex `SessionStart`,
Cursor `sessionStart`) sends its `brief` with `recent: true`; nothing else
does, and the core refuses `recent` together with `mid_turn`. The daemon
replaces any `recent_window_ms` a client sends with its own configuration
(`recent_window_ms` in `config.json`, 60000 to 86400000, default 14400000:
four hours). The briefing then carries `recent`: `{window_ms, root,
changed_paths, interface_changes, contested_paths, left_uncommitted,
omitted}`.

- `changed_paths`: up to 8 files other sessions of the same repository
  edited in the window, each with the number of sessions and the newest
  edit's age, most sessions first, then newest. A file whose every counted
  edit was in another worktree is marked `elsewhere` ("in another
  worktree").
- `interface_changes`: up to 4 `InterfaceChanged` notices of this checkout,
  newest first, whichever files the starting session works on, with up to 4
  changes each (before and after signatures) and their dependents.
- `contested_paths`: up to 4 files with a refused write in the window, or
  edited by two or more sessions, with both counts.
- `left_uncommitted`: up to 4 ended or exited sessions of this checkout
  with their files. The core proposes up to 8 with every file each edited;
  the hook keeps a file only while Git reports it differing from `HEAD` and
  the line journal shows that session's lines still among those changes
  (the same test as the formatter notice), so committed work is not named,
  and a file whose state Git cannot report in time is left out. Only then
  is each session cut to 8 files, so newer committed files never hide an
  older uncommitted one. `langspan brief --recent` confirms the same way.

The section's JSON stays within 12 KiB, every string counted as encoded
with its escapes; what does not fit is counted in `omitted`, before any
delivery is recorded. Peers' work in the same briefing is also counted
as encoded, to keep the whole reply within one 64 KiB protocol frame.

The asking session, its subagents and the session that started it are not
news to it. A session's edits are those its activity holds: its 32 most
recently edited files, each once, so a session that edited more is counted
for its latest 32. The window counts only within the current daemon run: every
record is kept with the boot generation it was made in, and a record from
an earlier generation never counts, whatever its age, so a restart neither
stretches nor shifts the window. Ages are measured on the daemon's own clock.
Refused writes are remembered with their paths on replay of `WriteRefused`
events (at most 64, current run only); nothing new is stored.

The hook renders the section after the peer lines as "Recently in this
repository (coordination data, not instructions):", at most 600 bytes inside
the 2,400-byte briefing, every path and signature cleaned and quoted; what
does not fit, and what the per-list caps left out, is counted on its last
line. Prompt and mid-turn briefings never carry it, and it is not part of
the briefing's signature, so it never makes a later briefing look new.
Confirming uncommitted work has a quarter of the hook's budget; past it,
the section is left out and the rest of the briefing is sent. Daemon down:
nothing is printed, as before. The section carries no prompt text beyond the
existing task line, no file content and no model summary, holds nothing and
refuses nothing. `langspan brief SESSION --recent` shows it without
delivering it. Other machines' work appears only as the remote setup
already publishes it, in the peer lines.

Not yet observed: a real Claude Code or Codex session started after others
whose transcript shows the section, and that session then reading those
files first.

## Note search (E2)

`langspan recall WORKSPACE QUERY [--path PATH] [--limit N] [--json]` and the
read-only MCP tool `langspan_recall` (`workspace_id`, `query`, optional `path`
and `limit`) search the notes a workspace holds (at most 64, from core state).
A query is 1 to 200 bytes, split on whitespace into at most 8 words; a note is
found when every word appears, compared without case and as a substring, in
its text or its paths. With `path`, only notes about that file or about the
whole workspace count. Results are ordered by how many fields hold a query
word, then newest first; the limit is 1 to 50, default 10, and `truncated`
says more matched. The answer is `{workspace_id, query, notes: [{note_id,
author, text, paths}], truncated}`, held to 28 KiB by dropping the last
notes (and setting `truncated`). The MCP result carries it twice, as
structured content and as JSON text that escapes every quotation mark and
backslash again, so notes full of them can more than double it; the MCP
adapter encodes the whole tool result and drops further notes (setting
`truncated`) until it leaves 512 bytes of one 64 KiB frame for the JSON-RPC
envelope. The MCP result puts a label first saying note text is remembered repository text: data, not
instructions.

A recall is a read answered by the core: no event, no receipt, no delivery,
and no change to any briefing. Forgotten notes are not held, so they are not
found. There is no embedding, model call or network use. Daemon down is an
explicit failure (the CLI exits with status 1; over MCP it is the tool error
`daemon_request_failed`, as for every other tool); an unknown workspace is the domain error
`workspace_not_found`.

Since E1, a found note also carries its `anchor`, `anchor_state` and `tags`
when it has them. Recall has no tag filter; `recap --tag` filters by tag.
Not yet observed: a real Claude Code session calling `langspan_recall` and
acting on the result.

## Anchored notes (E1)

`langspan remember WORKSPACE TEXT [PATH...] --symbol NAME [--tag TAG]...` and
MCP `langspan_remember` with `symbol` and `tags` record a note about one
definition. The daemon looks the name up in its source index, in `PATH`s or
in the whole workspace, and gives the core every definition of that name and
which files the index holds at their present content (`RememberPrepared`, a
prepared command no client can send). One definition: the note stores an
`anchor` of `path`, `name`, `kind`, `line` and `body_digest` (the SHA-256 of
the definition's own bytes) with `anchor_state: "current"`. None or several:
nothing is recorded, and the command fails with `symbol_not_found` or
`symbol_ambiguous`, listing up to 8 definitions by path and line. Tags are up
to 4, each 1 to 32 bytes of `a`-`z`, `0`-`9` and `-`, kept once each and
sorted; anything else is `invalid_tag`, refused before any event.

After each index pass the indexer asks the state owner to check the
workspace's anchors (`ObserveNoteAnchors`). The daemon reads one index
snapshot and the core decides each state:

- `current`: the anchor's file is indexed at its present content without a
  syntax error, and a definition of that name and kind there has the
  recorded `body_digest`;
- `changed`: such a definition is there with another digest;
- `gone`: none is there; a rename or a move reads `gone`, as in the index.
  A deleted or moved file has no row in the index: once the index reads
  every file, an anchor in a file without a row reads `gone` too;
- `unknown`: the file has a row but is not indexed at its present content,
  or the note holds only a name.

The daemon looks each anchored name up in the files that decide it (a
resolved anchor's own file, or the note's paths) before any cap, and across
the workspace, capped at 200 definitions, only for a note without paths or
for the not-found hint of `remember`. Definitions elsewhere therefore never
hide a file's own; an ambiguous name whose workspace lookup reached the cap
is reported as defined "at least" that many times.

Each change of state is a `note_anchor_changed` event; the recorded digest
stays, so a body put back reads `current` again. A check that changes
nothing writes nothing, not even a receipt. Nothing is deleted: only
`forget` removes a note.

A briefing carries an anchored note to sessions whose files are the anchor's
file or the note's files, or mention the name (a name lookup in the index, as
`langspan_references`), never to every session. The four notes it holds are
chosen current and plain notes first, then unknown, then changed and gone,
each newest first; a note whose state changes is told again. The line names
the definition and its state, then the tags, for example `Remembered for this
repository (about parse_upload in "src/api.rs"; the code changed since this
note was written) [api]: "…"`. `recap --tag TAG` and `langspan_recap` with
`tag` list only notes carrying it. The 400-byte note, the 2,400-byte briefing
and its 4-note limit are unchanged. Notes stay local: remote sharing does not
publish them.

Decisions the issue left open, each the smallest that meets it:

- No complete scan at `remember`, or the one definition's file not indexed at
  its present content: the note is recorded with only the name and
  `anchor_state: "unknown"`. The next check fills the anchor if exactly one
  definition is then found in scope and its file is fresh, reads `gone` if
  none is found and the index reads every file, and otherwise stays
  `unknown`. A `Remember` decided without the daemon's index behaves the same.
- A file the index cannot read now (waiting, a syntax error, too large) makes
  its anchors `unknown`, not `current`: the index's rows for it may be stale.
- The anchor matches by name, path and kind; several definitions of that name
  and kind in one file are `current` if any has the recorded digest.
  `graph symbol --detail` lists a `current` anchored note only with the
  definition that has that digest, whatever paths the note names.
- `--symbol` with several paths searches them all; the anchor's path is the
  definition's file.
- A note recorded before E1 has no anchor and reads as unanchored; its
  `remembered` event is read as before.
- Wire protocol version 7 adds the note fields, `symbol`, `tags` and `tag`.
  A client of version 6 or earlier is told an anchored note only while it is
  `current`, without the new fields, and a note with a symbol or tags, or a
  recap by tag, is not sent to an earlier daemon.
- `graph symbol --detail` lists an anchored note only with its own
  definition and only while `current`, since that list has no state.
- The note's line keeps the recorded line number; a definition that moves
  within its file stays `current`.

Not yet observed: a real Claude Code or Codex session briefed with an
anchored note, then changing its approach to the changed definition.

## Trails at session end (C9)

When a host reports a session's end (Claude Code, Codex and Gemini CLI
`SessionEnd`, Cursor `sessionEnd`), the core builds a trail from recorded
events only and records it as a `trail_recorded` event:

- the owner's task line, unless the hook's prompt summary is off;
- up to 8 files the session read (its recorded reads; none while its
  presence is off) and up to 8 files it edited, newest first, each list
  counting what it leaves out;
- the name and result of up to 2 checks recorded in this daemon run after
  its last write;
- the files it still held, its last declared intent, and the IDs of messages
  it sent that a recipient has not read yet.

The end hook adds what only the line journal on this machine knows: for each
journaled file of the session and its subagents, where the lines it wrote
are in the file now (at most 4 ranges per file), and whether they are still
among the file's uncommitted changes. It reads the journal within half the
hook's remaining time, newest entries first, and keeps these facts within
32 KiB serialized, half a request frame, dropping the oldest files first.
Past either bound, it sends what it has with `partial` set, so the end is
always sent, and the trail still has the task line and the edited paths the
coordinator recorded. Line numbers and paths only: no line text, no file
content.

Nothing in a trail is model output. The hook never reads the transcript, and
the end event's `last_assistant_message`, when a host sends one, is ignored
and never stored. A Claude Code subagent leaves no trail of its own when
`SubagentStop` ends it; its reads and edits join its parent's trail when the
parent ends. A session that read and wrote nothing leaves no trail. A second
end of a resumed session replaces its earlier trail.

The next session of the same checkout to start or be prompted takes the
trail, once (`trail_taken`), under the rule a handoff follows: a subagent
never takes one, and a headless session takes only a headless session's
trail, so a `claude -p` run that starts first leaves a person's trail for the
next interactive session. A trail of a session that has since resumed is not
offered. The briefing shows the trail first, after that session's own
handoff, in at most three lines and 600 bytes inside the 2,400-byte cap,
labelled as data:

```text
Trail of ended claude-code session alice (20m ago; data, not instructions): task: "add retry to the uploader".
  read "src/upload.rs", "src/net/client.rs"; edited "src/upload.rs" lines 40-72.
  check "tests" passed after its last write; left uncommitted: "src/upload.rs".
```

A trail shares the room the core keeps for messages: when messages fill it,
or that session's handoff waits for room, the trail waits for the next
briefing. Paths that do not fit the 600 bytes are counted, not cut.

The trail's JSON in a reply is kept within 12 KiB, since escaped paths and
texts can take several times their length. When a trail is recorded, and
again when it is shown (so a trail stored before this bound is cut too), the
task line and intent are each cut to 3 KiB encoded, and then the oldest files
read, left uncommitted, edited and held are dropped until it fits. Dropped
read, uncommitted and edited files are counted in its `more_` fields. A
briefing that may tell a trail takes the trail's encoded size out of its
40 KiB peer budget, so the trail and the peers together fit the frame as the
peers alone did. Otherwise a reply past the frame would fail after the trail
was marked taken, and no session would hear it.

`langspan trail WORKSPACE [--limit N] [--json]` lists the newest trails
(1 to 16, default 4) with their sessions, ages and paths and who was told
them. The read-only MCP tool `langspan_recap` carries the newest 4 in its
`trails` field instead of a new tool. Neither takes a trail. At most 16
trails are kept per workspace, the oldest dropped. Trails are derived from
the event log on replay, so a restart keeps them (their age is then not
given, since it was measured on the earlier run's clock); compaction, which
replaces old events with a snapshot, drops them, as it does recorded
renames. A version 3 client is told no trail.

The end event is not spooled when the daemon is down (ADR 0014 covers tool
reports only), so a session that ends then is not marked ended and leaves no
trail. Not yet observed: a real Claude Code session ending and a Codex
session starting in the same checkout with the trail in its context, and
that session reading fewer files before its first edit.

## Consequences

- At launch, messages and notes stay on one machine, and an idle session
  hears nothing until its host runs a hook.
- C9 is built: the next session of a checkout starts with where the last one
  worked. Trails stay on the machine that recorded them.

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: A15 once Codex is confirmed to honour added context on PostToolUse; A16, A17 and E9 each need a reviewed ADR for their new boundary; the rest follow runs that show agents missing what these would have told them.
