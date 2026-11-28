# The live dashboard

`langspan dashboard` serves LangSpan Live, a read-only page of what this
coordinator knows, on `127.0.0.1:7311`. It prints a URL with a token for this
run; `--open` opens it in the browser.

```sh
langspan dashboard --open
```

The page polls `GET /api/state` every two seconds and has four parts:

- **Counts.** Active sessions (the coordinator's count on every machine),
  shared paths, held files, refusals, interface notices, checks and landings.
  Each count adds the rows shown to the rows the view left out. Refusals are
  searched in the newest 5,000 recorded events and receipts; a `+` after the
  count means more exist than are listed, so the number is a lower bound.
  "How the counts are made", under Coordinator health, defines each one.
- **Traffic scope.** Sessions in rows, grouped by workspace on this machine
  and by teammate, and file paths in columns. A column is one path in one
  repository, so linked worktrees share it and an unrelated repository's file
  of the same name does not. Each cell names its signal in text: refused,
  held, teammate editing, shared, interface notice, teammate published or
  recent. By default the matrix shows only paths with a coordination signal
  and sessions current in this run; the filters show every observed path,
  every listed session, this machine or teammates, and narrow paths by text.
  At most 40 path columns are drawn, with a count of the rest.
- **Coordination evidence.** Refusals, interface notices, checks, landings,
  push turns, messages and notes, newest first where a time was recorded and
  with a dash where none was. It is the current view, not an event log. Its
  footer carries this month's refusal count (see "Refusals this month").
- **Inspector.** Select a path, a cell, a session or a record to see its
  evidence: who touched it, holds, refusals, the interface change and the
  files that name it, messages about it, and the limits of each record.

Coordinator health keeps every workspace's root, branch, sessions, scans,
publishing and source index coverage, and the install command for each agent
host without LangSpan hooks. If the coordinator stops answering, the page
keeps the last view, dims the matrix and says since when. Selection, keyboard
focus and scroll position survive each refresh. The page follows the system's
light or dark theme; the theme button switches it.

`langspan dashboard --demo` serves the same page and renderer with four
fictional scenes, labeled "Simulated data", for demonstrations. It reads
nothing from the coordinator, and its `/api/state` and `/api/events` answer
404. The live page never contains the demo scenes.

`GET /api/events?after=SEQ&limit=N` serves the coordinator's event log in seq
order, for a script or a future Live view that replays it: `after` defaults
to 0, `limit` to 100 (at most 1000), and the next poll asks after the reply's
`next`. A page can hold fewer rows than asked; read until one comes back
empty. Each row carries only the event's seq, type, session, workspace,
time and file paths, never prompt summaries or message text. Version 2 adds,
where the event records them, a refusal's `peer_session_id` and `refusal`
kind, an operation's `tool` and `result`, a read or write's `origin`, and a
clock anchor's `wall_ms` and `clock_ms`. It needs the
same token and `Host` as `/api/state` ([ADR 0013](../adr/0013-dashboard.md)).

`scripts/dashboard-browser-check.mjs` drives the real binary and a headless
browser that speaks the DevTools protocol (`--browser PATH`; Node 22 or
later, no packages) at 390, 768, 1024, 1280 and 1600 px in both themes. It checks hostile text, polling, refused tokens,
the stale view, selection and focus across refreshes, page-level overflow,
requests beyond the page's origin and accessibility rules, and saves
screenshots under `target/dashboard-browser`. `--axe PATH` also runs
axe-core; `--state-dir DIR` only reads an existing coordinator.
