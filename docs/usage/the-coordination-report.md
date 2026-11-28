# The coordination report

`report` shows, for each session and in total for each host kind, how it
followed coordination: whether a new host version or model still reads before
editing, declares and ends intent, and what it does after a refusal.

```sh
./target/release/langspan report demo
./target/release/langspan report demo --since 2027-03-01T00:00:00Z --json
```

The text looks like this (the figures are an example, not a measurement):

```text
Coordination report for demo, since 2027-03-01T00:00:00Z
host         sessions  edits  base_unknown  shell_after  reads  intents set/ended/open  refusals  retried_unchanged  briefings
claude-code  6         88     0 of 88       5            61     12/11/1                 7         1 of 7             40
codex        4         41     9 of 41       14           -      3/3/0                   5         3 of 5             22
Refusals by kind: claude-code 5 conflict, 2 stale_base; codex 5 stale_base
Refusal outcomes: claude-code 3 adapted, 2 waited, 1 abandoned; codex 2 adapted, 3 unknown_read
```

Each row counts recorded events in the window:

- `edits_admitted`: tool calls a hook had admitted before they ran.
- `edits_base_unknown`: of those, admissions with a file whose version was
  not compared, because no read or earlier version of it was recorded. A
  Codex session's first edit of an existing file is one, since Codex reports
  no reads.
- `shell_writes_after`: shell runs whose changed files were seen only after
  the command ran, because no admission covered them first
  ([ADR 0011](../adr/0011-shell-writes.md)).
- `reads_reported`: completed reads the host reported.
- `intents_set`, `intents_ended` and `intents_left_open` (sessions that ended
  with an intent still set, including an intent set before the events read).
- `refusals`: `write_refused` events by kind, as `today` counts them.
- `retried_unchanged`: refusals followed by an admission of the same
  session's identical tool call (same input digest), under a new tool-call
  ID. A second delivery of the refused call itself is answered from its
  receipt and is not counted.
- `briefings_delivered`.
- `refusal_outcomes`: what the session did after its edit refusals
  (roadmap H9), described in the next section.

## What a session did after a refusal

Each `conflict`, `stale_base` or `unread_lines` refusal in the window opens
one outcome for its session, kind and path set; further refusals of the same
set before it settles join it, so three refusals of one file are one
outcome. The next relevant event of that session settles it, once:

- `waited`: a conflict whose holder's admissions of the refused files ended
  (completed or expired, or the coordinator restarted), after which the
  session
  wrote one of them.
- `adapted`: the session reported a read of a refused file and then wrote
  one, or wrote one with a call that differs from every refused call.
- `retried_unchanged`: the session wrote a refused file with a call
  identical to a refused one (same input digest), or with a shell command,
  and reported no read in between. A stale refusal lets the identical call
  through once; a conflict keeps refusing it while the holder holds the
  file.
- `unknown_read`: as `retried_unchanged`, on a host that reports no reads
  (Codex), so a read may have come first. A Codex call that differs from the
  refused one is `adapted`.
- `abandoned`: the session ended, or wrote none of the refused files within
  30 minutes of the latest refusal.
- `unknown`: the events cannot tell, because the refusal was recorded by a
  build that kept no input digest and the later call differs, or because the
  conflicting holder's admission is not among the events read.
- `pending`: not settled yet. This is not an outcome.

A write is a hook's admission of a tool call or a shell command seen
changing the file. Outcomes are derived from recorded events each time the
report runs, never from model text, and nothing is stored, held or sent to
a model because of one. A held commit opens none. The window is checked
against wall times placed as above: an admission carries no time of its own,
so it is placed at the timed event before it, normally the session's
previous hook event or prompt.

A share is always shown beside the count it is out of (`9 of 41`); no field
is a bare rate. A field a host cannot report is shown as `-` and listed under
`not_reported` in that host's total (Codex: `reads_reported`), so its zero is
not read as behavior. Hosts are `claude-code`, `codex`, `cursor`
and `other` (a session whose ID names none of them, earlier Gemini CLI
sessions included). The counts say nothing
about time or rework saved.

Without `--since` the report covers every recorded event; events are placed
in wall time as `today` places them, and those of a build before
`clock_anchored` are counted only as `undated`. At most 16 sessions are
listed, earliest first; the host totals include every session, and the rest
are counted in `omitted_sessions`. `--json` carries `window`, `hosts` (each
with `sessions`, `counts` and `not_reported`), `sessions` (`session_id`,
`host`, `counts`), `truncated` and, when there are any, `omitted_sessions`,
`covered_from_ms`, `undated` and `partial`. `counts` carries
`refusal_outcomes` only when it holds an outcome. A window with more than 50,000 events is
counted from the newest 50,000 and says `truncated: true`, with
`covered_from_ms`, the wall time those events start at. So does a window
that starts before the newest retention cutoff, with `covered_from_ms` no
earlier than that cutoff. Each session's workspace and open intent where
those events start come from at most the newest 10,000 intent and
registration events before them; when an event in the window needed an
older one, it is left out of every count and the report says
`partial: true` ("Partial: ..." in the text). The MCP tool
`langspan_report` (`workspace_id`, and `since`, optional) returns the same
text, labelled as data, within 2,400 bytes: session rows are left out, last
first, until it fits, and the host totals keep them. It is a read: it
refuses, holds and acknowledges nothing. With the daemon down it exits 1
with "coordination unavailable".

The daemon reads a report on its own read-only connection, from one
snapshot of the log, on a thread apart from the one that admits edits, so
a report of a long history never delays an admission. Up to four reports
wait behind the one being read; one more is refused with `report_busy`
("ask again when they finish").
