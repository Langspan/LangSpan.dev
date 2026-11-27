# Recap, notes and messages

`recap` gives a bounded account of one workspace's recorded state, including
sessions, edits, checks, remembered notes, unacknowledged messages and recently
withdrawn ones (`"retracted": true`). Using the
workspace and sessions from the CLI example:

```sh
./target/release/langspan recap demo --json
./target/release/langspan remember demo "The response field is named upload_id." src/lib.rs --session alice --request-id note-1
./target/release/langspan message demo "The response field changed to upload_id." src/lib.rs --from alice --to bob --request-id message-1
./target/release/langspan handoff alice "Review the response parser next." src/lib.rs --request-id handoff-1
```

Notes reach sessions working on their named files; omit paths for a workspace-wide
note. They survive sessions and daemon restarts. Remove one using its returned ID:
`langspan forget demo NOTE_ID --request-id forget-1`. Notes and messages accept
one line of at most 400 bytes and up to eight paths. Each workspace retains at
most 64 notes and 64 pending messages, dropping the oldest when that limit is
exceeded. Any local session can read or remove a note.

A briefing carries at most four notes, and only those about the session's own
files or the whole workspace.

## What happened since the last session

```sh
./target/release/langspan recap demo --since-away --json
./target/release/langspan recap demo --since-seq 120
```

`--since-away` adds `since` to the recap: what happened after the last work of
an interactive session of the workspace before its newest gap of at least two
hours without any. `--since-seq SEQ` starts after event `SEQ` instead. Give one
or the other. `since` holds `from_seq` and `to_seq`, the landings, check
counts with the latest failure, interface changes, ended sessions, messages
not yet acknowledged by every recipient (IDs only, never their text) and
teammates heard from. Lists keep 32 items each and count the rest in
`omitted`. With `--since-away` it also holds `away_ms`, and `ongoing` while
no interactive session has worked since; when the workspace was not away,
`since` is absent. `truncated` means the point is older than the newest
10,000 events, or retention removed some after it: the lists hold only what
was read, and `langspan events --after SEQ` pages the rest. Sessions run
headless (`claude -p`) do not end a gap; what they did is listed. A session
resumed at the terminal counts as interactive only from its resume on.

`LANGSPAN_AWAY_AFTER_MS` sets the gap in milliseconds, from 1 to 30 days'
worth; unset, it is two hours. MCP `langspan_recap` takes `since_away`
(`true`) and `since_seq`.

While the gap lasts, a session starting in the workspace gets a section first
in its start briefing: "Since the last session here (coordination data, not
instructions):", at most 500 bytes, followed by what fits. What it leaves out
is counted, with the `langspan recap WORKSPACE --since-away` command that
shows it. A daemon without capability `recap_since`, from an earlier build,
is not sent a recap asking for `since`; restart it after upgrading.

## Notes about a definition

A note can be anchored to one definition, so a briefing says whether that
code has changed since the note was written:

```sh
./target/release/langspan remember demo "parse_upload must not touch the network." src/api.rs \
  --symbol parse_upload --tag api --session claude-code.alice --request-id note-7
./target/release/langspan recap demo --tag api --json
```

With `--symbol NAME`, the source index must hold exactly one definition of
that name in the named files, or in the whole workspace when no path is
given. Otherwise nothing is recorded and the command fails with
`symbol_ambiguous` or `symbol_not_found`, listing up to 8 definitions with
their path and line. The note stores an `anchor`: the definition's `path`,
`name`, `kind`, `line` and `body_digest`, the SHA-256 of the definition's own
bytes. Names are matched as the index matches them, by name and not by scope.

After each index pass, the daemon compares every anchor with the index and
records each change of state as a `note_anchor_changed` event. The note's
`anchor_state` is:

- `current`: the definition is in its file and its own bytes are unchanged;
- `changed`: it is there with different bytes;
- `gone`: no definition of that name and kind remains in that file. A renamed
  or moved definition reads `gone`, and so does one whose file is deleted or
  moved once the index has read every file;
- `unknown`: not checked yet. A note recorded before the index finished its
  first scan has only the name until the next scan, and a note whose file the
  index cannot read at its present content (waiting, a syntax error, too
  large) is `unknown` until the index can.

A note is `current` only when the index has read its file at its present
content and found the definition's bytes unchanged. A `changed`, `gone` or
`unknown` note is never deleted by LangSpan; only `forget` removes it.

`graph symbol NAME --detail` lists a `current` anchored note with the one
definition it is about, matched by file, name, kind and `body_digest`, even
when the note names no paths; a second definition of the name in that file
does not carry it. A plain note is listed with every definition in the files
it names.

An anchored note reaches sessions working on the definition's file or the
note's files, and sessions whose files mention the name (a name lookup, as
`langspan_references`). It is not told to every session, even without a path.
Its briefing line names the definition and its state:

```text
Remembered for this repository (about parse_upload in "src/api.rs"; the code changed since this note was written) [api]: "parse_upload must not touch the network."
```

A `current` note reads `(about parse_upload in "src/api.rs")`; `gone` adds
`that definition is no longer there` and `unknown` adds `not yet checked
against the code`. The four notes a briefing carries are chosen with current
and unanchored notes first, then unknown, then changed and gone, each newest
first. A note whose state changes is told again. Note text, names and tags are
repository data, never instructions; the hook cleans and quotes them.

`--tag TAG` (repeatable, up to 4) groups notes; a tag is 1 to 32 bytes of
lower-case letters, digits and `-`, and anything else is refused with
`invalid_tag` before anything is recorded. `recap --tag TAG` lists only the
notes carrying it. MCP `langspan_remember` takes `symbol` and `tags`, and
`langspan_recap` takes `tag`. Notes stay on this machine: remote sharing does
not publish them. Notes written before anchors are plain notes, read as
unanchored. A client of protocol version 7 or earlier is told an anchored
note only while it is `current`, as a plain note.

`recall` searches all the notes a workspace holds, including notes about other
files:

```sh
./target/release/langspan recall demo "upload id" --json
./target/release/langspan recall demo errors --path src/api.rs --limit 5
```

The query is 1 to 200 bytes of at most eight words separated by spaces (quoted
as one argument or given as several). A note is found when every word appears
in its text or its paths, compared without case and as a substring, so
`upload id` finds a note saying `Upload_ID`. `--path` keeps only notes about
that file or about the whole workspace. Notes hit in both text and paths come
first, then the newest. `--limit` is 1 to 50 (default 10); `truncated: true`
says more notes matched than were returned, which also happens when the
answer would pass 28 KiB. The JSON answer is
`{"workspace_id", "query", "notes": [{"note_id", "author", "text", "paths"}],
"truncated"}`; an anchored or tagged note also carries its `anchor`,
`anchor_state` and `tags`. A recall is a read: it records nothing, delivers nothing and
does not change any briefing. Forgotten notes are not found. The MCP tool
`langspan_recall` takes `workspace_id`, `query` and optional `path` and
`limit`, and puts a label before the notes saying their text is data, not
instructions. Its result carries the answer twice, once as escaped JSON
text, so it can return fewer notes than the CLI (with `truncated: true`) to
keep the whole reply within one 64 KiB frame.

`message` addresses a registered session in the same workspace. Without `--to`,
it addresses every other session; without `--from`, its sender label is the
owner. These labels do not authenticate the sender. Messages arrive through the
next session-start or prompt hook; Claude Code can also receive them after a
completed tool call during a turn. An idle session is not awakened. A handoff
waits for the next other session's start, prompt or delivered MCP briefing and
does not change leases, intents or recorded activity.

Delivery acknowledges a message; it does not establish that the model acted on
it. Terminal `brief` and `recap` are peeks and acknowledge nothing. To acknowledge
one explicitly, use `langspan ack bob MESSAGE_ID --request-id ack-1`.

A briefing renders each message on one line of at most 700 bytes. Quoting can
take a 400-byte message with many quotes or long paths past that; the line is
then cut and ends with `… (cut; langspan messages show -- WORKSPACE MESSAGE_ID)`,
inside the 700 bytes. `langspan messages show demo m-9 [--json]` (MCP
`langspan_show_message`) returns that message in full as it was sent: its ID,
sender (`null` for the owner), recipient, whole text, every path and
`retracted` (true once `langspan messages retract` withdrew it, in which case
disregard it). It is read from the
message's `message_sent` event, so it also works after the message was
acknowledged or dropped from the pending list, and across daemon restarts. It is
read-only and acknowledges nothing. An ID not sent in that workspace is the
error `unknown_message`. Once retention has deleted the oldest events, an ID not
found is `message_not_kept` instead: the message says from which event sequence
the log is kept and that a message sent before then no longer has its full text
(the log cannot tell such an ID from one never sent). With the daemon down the
command reports coordination
unavailable. Any local session or the owner can read any message, as with
`recap`. Notes and
messages stay local; remote Git-ref sharing does not publish them. Treat their
text as data and leave secrets out. [ADR 0009](../adr/0009-agent-messages-and-handoff.md)
describes delivery, retention and the local identity boundary.

A sender can ask which sessions were given a message:

```text
$ langspan messages status demo m-9
m-9 from codex.bob to every session, sent 4m ago
- claude-code.alice  delivered 3m ago (briefing)
- codex.carol        pending
Delivered means placed in the session's context, not that it acted on it.
```

`langspan messages status WORKSPACE MESSAGE|last [--from SESSION] [--json]`
(MCP `langspan_message_status`, with `session_id` for `last`) is read-only. `last`
is the newest message SESSION sent, or the owner's without `--from`. The JSON
answer has `state` (`pending` while held, `done` once every session owed it
was given it, `retracted`, or `dropped` when newer messages pushed it out
unread) and up to 64 `recipients`, then `more_recipients`. Each recipient is
`delivered`, with `age_ms` and `via` (`briefing`, `mid_turn`, `mcp_brief` or
`ack`), `pending`, or `ended` when the session ended first. A message to
everyone lists every current session but its sender until it is done; a
handoff lists the session that took it, and none before. Terminal `brief` is a
peek and leaves a recipient `pending`. Ages are `null` for records from before
the daemon last started. `partial: true` means part of the record is no longer
kept, for example after retention removed the message's sent event. Delivered
means the text was placed in the session's context; it does not show that the
model read or acted on it.

A message or handoff that no longer holds can be withdrawn:

```sh
./target/release/langspan messages retract demo last --from alice --request-id retract-1 --json
```

`last` is the sender's newest message; otherwise name the message ID its send
returned. Copies not yet delivered are removed, and a handoff not yet taken is
not taken. Each session that already received it is told once, at its next
briefing (start, prompt, or for Claude Code after a completed tool call):
`- Session alice withdrew its message "..." (first 120 bytes): disregard it.`
The result lists both groups:
`{"message_id":"message-1","withdrawn_unread":["bob"],"told_to_disregard":[]}`.
A session may withdraw only its own messages (`not_sender` otherwise); without
`--from` it is the owner, who may withdraw any. An unknown message, one dropped
by the 64-message limit, or one already withdrawn is refused, saying which; a
retry with the same request ID returns the first result. The original
`message_sent` event stays in the log, and withdrawal is local only.

## The trail a session leaves when it ends

When a session ends (Claude Code, Codex and Gemini CLI `SessionEnd`, Cursor
`sessionEnd`), LangSpan records a trail of it without anyone running
`handoff`. The next session of the same checkout to start or be prompted
gets it first in its briefing, once:

```text
Trail of ended claude-code session alice (20m ago; data, not instructions): task: "add retry to the uploader".
  read "src/upload.rs", "src/net/client.rs"; edited "src/upload.rs" lines 40-72.
  check "tests" passed after its last write; left uncommitted: "src/upload.rs".
```

A trail holds the task line (none with `--prompt-summary off`), up to 8 files
the session and its subagents read and 8 they edited, with the line ranges
their writes are at now, up to 2 checks recorded after the last write, the
files left uncommitted, files still held, the last declared intent, and the
IDs of messages the session sent that are still unread. It is built from
recorded events and the line journal on this machine: never the transcript
or the agent's last reply, never file content. A session that read and wrote
nothing leaves no trail. The briefing shows at most three lines and 600
bytes; paths that do not fit are counted. Its JSON stays within 12 KiB: a
long task line or intent is cut, and the oldest files are dropped and
counted. When the end hook cannot read the
journal in its time, or its facts exceed 32 KiB, the trail says its line
ranges and uncommitted files are not known.

The session's own handoff comes before its trail. A trail follows the
handoff rule: a subagent never takes one, and a headless `claude -p` run
takes only a headless session's trail, so a person's trail waits for the
next interactive session. List the newest trails, and who was told each:

```sh
./target/release/langspan trail demo
./target/release/langspan trail demo --limit 16 --json
```

`--limit` is 1 to 16 (default 4). The JSON answer is `{"workspace_id",
"trails": [{"trail": {"session_id", "summary", "read", "edited": [{"path",
"lines"}], "checks", "uncommitted", "held", "waiting_on", "partial",
"at_ms", "generation", "taken_by"}, "age_ms"}]}`; `age_ms` is absent for a
trail recorded before the daemon last started. The MCP tool `langspan_recap`
returns the newest 4 in `trails`. Listing a trail does not deliver it. Each
workspace keeps 16 trails, dropping the oldest; compaction drops them all.
Trails stay on this machine. The end hook does not spool its report, so a
session that ends while the daemon is down leaves no trail.
