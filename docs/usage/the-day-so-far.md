# The day so far

`today` gives a one-screen account of a day: for each session its first and
last activity, how many files its edits named and how many prompts it had;
admission refusals by kind; check results; how landings ended; and what is
still open now (admissions held, intents not ended, messages not acknowledged,
handoffs not taken up, checks whose latest result failed).

```sh
./target/release/langspan today demo
./target/release/langspan today --since 2026-11-25T09:00:00+01:00 --json
```

Without a workspace it covers every workspace. The window starts at local
midnight, found from the system's time zone rules for that date (read from the
system's zone files, so a day the clocks change still starts at its 00:00), or at an
RFC 3339 `--since` time;
a time before the first event covers everything. Times in the text are shown
at the current local offset, so on a day the clocks change, times before the
change are an hour off. The text is at most 40 lines and lists at most eight
files per session, with a count of the rest; `--json` carries `window`
(`from_ms`, `to_ms`, Unix milliseconds), `sessions`, `refusals`, `checks`,
`landings`, `open`, `truncated` and, when there are any, `undated` and
`omitted_sessions`. The MCP tool `langspan_today` (`workspace_id` and `since`,
both optional) returns the same text, labelled as data, within 2,400 bytes.

It is a peek, like `recap`: it writes nothing and acknowledges no message.
Refusals are counted from `write_refused` events, the status line's, by kind:
a refused write (conflict, stale base or unread lines) or a held commit the
hook reported (`commit_held`). Each daemon start records a `clock_anchored` event
that places that run's times on the wall clock; events recorded by a build
before this one have no such anchor and are counted only as `undated`. A
landing is placed at the time it took its turn to land. A
window with more than 50,000 events is summarised from the newest 50,000 and
says `truncated: true`, as does a window that starts before the newest
retention cutoff, even though the generation markers retention keeps are
older than it; when the oldest of those carries no time of its own,
the first that does decides whether the window may reach past them. With the
daemon down it exits 1 with "coordination unavailable".

The text also carries one line with this month's refusal count (below); the
JSON and the MCP answer do not, so nothing of it reaches a model.
