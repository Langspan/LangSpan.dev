# Configuration and limits

Optional `config.json` in the state directory must be owner-only (`0600`):

```json
{"io_timeout_ms":2000,"request_timeout_ms":5000,"max_clients":32,"auto_check_debounce_ms":2000,
 "flaky_rerun_max_s":30,"host_check_ms":3600000,"notify":false,"notify_sound":false,
 "notify_kind_interval_ms":300000,"notify_hourly_limit":10,"notify_handoff_after_ms":600000,
 "recent_window_ms":14400000,"outline_min_lines":1500,"outline_min_bytes":65536}
```

Timeout values accept 100–5000 ms; client count accepts 1–128;
`auto_check_debounce_ms` accepts 50–60000; `flaky_rerun_max_s` accepts
0–3600 (0: a failing automatic check is never run again); `host_check_ms`,
how often the coordinator looks for agent hosts with no LangSpan install,
accepts 100–86400000; `recent_window_ms`, how far back a starting session's
"Recently in this repository" section looks, accepts 60000–86400000 and is
read when the coordinator starts; `outline_min_lines` (1–1000000) and
`outline_min_bytes` (1–1048576), the size from which a whole read is
[outlined first](source-versions.md#outline-first-for-whole-reads-of-large-files), are read when
the coordinator starts; `retention_days` (unset by default) accepts
7–3650 and is described under [Retention](#retention). For [desktop notices](desktop-notices.md#desktop-notices),
`notify_kind_interval_ms` and `notify_handoff_after_ms` accept 1000–86400000
and `notify_hourly_limit` 1–60; `notify`, `notify_sound` and the two notice
limits are read again at each notice, `notify_handoff_after_ms` when the
coordinator starts. A build before notices refuses to start with a
`config.json` that has these keys, a build before C3 one with
`recent_window_ms`, and a build before A11 one with `outline_min_lines` or
`outline_min_bytes`. IPC and MCP frames
are limited to 64 KiB. The MCP SDK discards oversized input lines without replying
to that line. An MCP tool result carries each answer twice, as structured
content and as JSON text that escapes every quotation mark and backslash
again, so the adapter measures the whole encoded result and keeps it, with
512 bytes left for the JSON-RPC envelope, within one frame. An MCP page can
therefore be shorter than the CLI's: a page of `langspan_events`,
`langspan_changes` or `langspan_status` with `all` that would not fit is cut,
and the result ends with a text note starting "LangSpan cut this page to fit
one MCP reply" that names the `after` to call again with for the rest. The
`status` view and `langspan_recap` leave more out instead, counted as the
CLI's answer counts it (`summary`, `omitted_items`); every record is in
`langspan_status` with `all`. A result no rule can cut, such as an oversized
landing report, is replaced by the error `reply_too_large`, which keeps the
operation's `exit_code` and outcome; the CLI's `--json` gives the whole
answer. The CLI's answers are unchanged. File admission accepts up to 64 distinct normalized keys, 512 bytes
per key, with a TTL of 1–60000 ms. Intents have a 2048-byte summary limit
and prompt summaries a 512-byte one. A presence record names 1 to 4 paths, and
each session keeps its 16 most recent looked-at paths. Summaries are redacted on receipt, and
redaction never makes one longer than its limit: a summary that would outgrow
it keeps its longest redacted prefix that fits, cut at a character boundary
and ending in `[redacted]`, so a summary within its limit is never refused
because it held a secret.
Canonical path checks reject escapes, special files, and observed hard links.

History is read in bounded pages; large status snapshots remain subject to
the IPC response limit. Git observation also has explicit file, byte, branch,
and deadline bounds; an incomplete scan does not become a complete snapshot.
See [implementation status](../IMPLEMENTATION_STATUS.md) and the
[threat model](../security/threat-model.md) for current coverage,
platform limits, and the cooperative boundary.

## Commands that need the owner's own action

Seven commands make the coordinator act with your own authority beyond
coordination state ([ADR 0039](../adr/0039-caller-authority.md)). It carries
them out only while `"owner_grants": true` is in `config.json`, and otherwise
refuses each one with `owner_grant_off`, an error that says what the command
does and names the setting:

| Command | What it does with your authority |
| --- | --- |
| `langspan check auto WORKSPACE -- COMMAND` | Runs a program after a session's writes. |
| `langspan remote enable` | Sends work metadata to a remote. |
| `langspan graph export` | Writes a file at the path it names. |
| `langspan graph import` | Reads a file at the path it names. |
| `langspan policy set` | Changes the policy every session's admission meets. |
| `langspan workspace remove` | Ends a workspace's sessions, notes and messages. |
| `langspan compact` | Deletes events and receipts. |

A dry run of `compact`, `workspace remove` or `graph import` is refused too.
To run one, add `"owner_grants": true` to `config.json` (owner-only, `0600`;
create it if there is none), run the command, then set it back to `false`.
The coordinator reads the file again for each of these commands, so no
restart is needed, and a refused request records nothing, so the same
`--request-id` works when you run it again. While the setting is on, any
process running as you can send these commands, including an agent that can
reach the socket; the setting protects only against an agent that can reach
the socket but can't write the state directory. A `config.json` that can't be
read or isn't valid refuses them. Turning a grant off (`check auto --off`,
`remote disable`) and every other command are never refused, and the
coordinator's own compaction under `retention_days` doesn't need the setting.
A build before this setting refuses to start with a `config.json` that has
`owner_grants`.

## Retention

Events and receipts are kept until they are compacted
([ADR 0025](../adr/0025-distribution-lifecycle-and-store-beyond-the-first-release.md)).
Nothing is ever deleted unless you run `langspan compact --apply` or set
`retention_days`. For example (the numbers are illustrative):

```text
$ langspan compact --keep-days 90
would delete 812,440 events (seq 1..812440, before 2026-08-27T09:00:00Z) and 61,204 receipts
snapshot at seq 812440; about 410 MiB freed
dry run: add --apply to compact
```

`compact` (default 90 days, 7–3650) is a dry run: it deletes nothing and
reports what would go. With `--apply` the daemon writes the state those
events built into a snapshot and, in the same transaction, deletes the
events and receipts recorded before the window. It starts from the newest
snapshot and replays only the events after it. With `retention_days` in
`config.json` the daemon does the same when it starts and then once a day,
logging a `compacted` line.

- The snapshot is checked first: read back, with the later events applied, it
  must give the state the daemon holds, or the compaction stops with
  `snapshot_mismatch` and deletes nothing.
- It stops before anything still open: an admission, a push turn, a waiting
  landing or an unacknowledged message. The report says which (`held_by`).
- A retry whose receipt was removed is answered as a new request.
- The daemon start that dates the oldest event left is kept, so `today` can
  still place every remaining event on a day.
- `remote sent` lists every publication within the window and says from which
  date it is complete.
- Ages are wall-clock times recorded with each event and receipt; records
  from before this version count from the upgrade.
- The freed space is reused by SQLite; the file does not shrink.
- The daemon does nothing else while it compacts: about 0.6 s for 500,000
  small events on one laptop (see the ADR).
