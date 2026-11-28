# The live dashboard

`langspan dashboard` serves LangSpan Live, a read-only page of what this
coordinator knows, on `127.0.0.1:7311`. It prints a URL with a token for this
run; `--open` opens it in the browser.

```sh
langspan dashboard --open
```

The page polls `GET /api/state` and `GET /api/events` every two seconds and
has two views, switched by the Live and Files buttons under the header. Live
opens first once any step is recorded.

**Live** shows recorded work as it happens, in the homepage hero's design.
Each local session is a lane, named by its branch or else its workspace, with
its harness beneath; a session that has ended takes its workspace's branch,
and sessions on one branch are numbered, such as "feat/billing (2)". A lane
never shows the session's prompt or task line. Paths Git ignores, such as
build output under `target/`, are left out, as everywhere else in LangSpan.
The title bar names the workspaces and branches of the
agents working now. Each card is one recorded step in words:
"Read", "Edited", "Ran a command", "Write held" with the reason, and "Saved
after re-reading" or "Saved on retry". Edits of one file, or reads, in a row
within 20 s share a card with a count, such as "3 edits". A line joins each
of an agent's cards to its next, dashed after a pause of more than 30 s. A
coral wire labelled "Held" runs from the change that caused a held write to
the held card; nothing else draws a wire, and work on another machine never
does. The line above the lanes counts the agents working, those with a
recorded tool step or lease in the last 10 minutes (a session that only
started doesn't count), and the writes held in the same 10 minutes.

- New work enters from the right while **Follow newest** is on. **Pause**
  stops the motion; any other interaction pauses it, and it resumes after 8
  seconds.
- **+** and **−** zoom, **Fit** shows all the work, and dragging or pinching
  pans and zooms. With the canvas focused, the arrow keys pan, `+` and `-`
  zoom, `0` fits, and `L` follows the newest work.
- At most 6 lanes show at once (3 on a phone). With more agents, the canvas
  opens on the lanes with the most work in view at the newest step, and
  keeps to them as work arrives until you scroll; once it has focus, the
  wheel scrolls through the lanes, and **Follow newest** scrolls back. Ctrl
  or Cmd with the wheel zooms.
- Click a card, a wire's label or a lane name for its recorded details: the
  file, the time, why a write was held (the refusal LangSpan recorded, such
  as "another agent is editing it"), the other agent, and what the held agent
  did next. Details never open on their own.
- With reduced motion, or `?motion=static` added to the URL, the view shows
  the newest work without motion.

The view keeps the newest 4,000 feed rows. On opening it reads the log to
its end before drawing, so a long log shows its current work, not its oldest. Steps without a recorded time
(a session starting, a lease) are never cards; a lane's details list them.

**Files** has four parts:

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

`langspan dashboard --demo` serves the same page with four fictional scenes
in the Files view, labeled "Simulated data", for demonstrations. It reads
nothing from the coordinator, and its `/api/state` and `/api/events` answer
404. The live page never contains the demo scenes.

`GET /api/events?after=SEQ&limit=N` serves the coordinator's event log in seq
order, for the Live view or a script: `after` defaults
to 0, `limit` to 100 (at most 1000), and the next poll asks after the reply's
`next`. A page can hold fewer rows than asked; read until one comes back
empty. Each row carries only the event's seq, type, session, workspace,
time and file paths, never prompt summaries or message text, and leaves out
the paths Git ignores in the row's workspace. Version 2 adds,
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
