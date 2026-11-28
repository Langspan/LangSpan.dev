# ADR 0013: A loopback dashboard

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026. It adds a reader, not a control: nothing on the page changes
coordination state.

## Context

The owner asked for a live view of every agent, readable from a laptop or a
phone. Everything such a view needs is already in each machine's coordinator:
local sessions in full, and teammates' sessions as their publications arrive
([ADR 0006](0006-remote-awareness-through-git-refs.md)). What was missing was a
page, and a page needs a listener, which LangSpan has never had on a TCP port.

## Decision

**One read-only core command.** `Command::Dashboard` returns a purpose-built,
bounded view, not the stored `State`: agents (at most 50), and at most 20 rows
in every other list (files held now, files several agents touched in the last
30 minutes, interface changes, checks, landings, waiting messages and handoffs,
notes, workspaces), each with the number left out. Every string is cut at 240
characters, every row lists at most 8 files, and each section has a byte
budget, so the encoded view stays under 48 KiB, inside the 64 KiB frame with
room for the rest. It records no event and no receipt. The store adds recent
denials: conflicts from receipts (they are not events, and carry no time) and
stale-source refusals from events, searching only the newest 5,000 rows of
each. The daemon adds its uptime and each workspace's index coverage. The
dashboard server fetches the view through the ordinary socket client, as every
other interface does.

**A small server in the same binary.** `langspan dashboard` uses the standard
library's `TcpListener`: no new dependency. It serves `GET /` (one page with its
script and style inline and nothing loaded from anywhere) and
`GET /api/state` (the view as JSON, with this local month's refusal count as
`refusals_this_month`, the fields `langspan count --json` prints; see
[ADR 0019](0019-person-facing-surfaces-and-usage-accounting.md), H5), and
`GET /api/events?after=SEQ&limit=N` (issue #500): a page of the coordinator's
event log through the existing `Command::Events`, one row per stored event in
seq order, so a poll from the last `next` misses none. `after` defaults to 0
and `limit` to 100, at most 1000; anything else is a 400 that says the range.
The page is `{"available": true, "version": 2, "next": SEQ, "events": [...]}`
and each row is an allowlist: `seq`, `type`, the first `session_id`,
`workspace_id` and `at_ms` (or an operation's `observed_at_ms`) the event
names, and its `path` and `paths` values, at most 8 and every string clipped
as in the view (`more_paths` counts the rest). Another session's holdings
named in an operation (`concurrent_holders`) are not its paths. Every other
field, and any added later, is left out, so prompt summaries, message and
note text, task lines and publication payloads are never sent; the page still
shows message and note text only through `/api/state`. Version 2 (issue #491)
adds the facts the Live view's cards and wires read, each optional and taken
from one event type only: a `write_refused` row's `peer_session_id` and
`refusal` kind, an `operation_observed` row's `tool` (clipped) and `result`,
a `source_observed` row's `origin`, and a `clock_anchored` row's `wall_ms`
and `clock_ms`. They are IDs, enums and numbers; the input digest stays out.
A version 1 reader ignores them, and a version 2 reader decodes a version 1
row without them (`fixtures/dashboard-events-v2.json` is the golden page).
`--demo` answers it with 404, and an unreachable coordinator
with 503 and `available: false`. It reads only a request head of at most
8 KiB and 64 headers, within 2 s per read and 5 s in total; refuses every other
method, any request with a body, and unknown paths; and serves at most 16
connections at once, closing the rest unanswered.

**Loopback, with a token.** The default address is `127.0.0.1:7311`. A
non-loopback address is refused unless `--allow-network` is given, and then
the server warns. Loopback alone is not enough: every local user, and every web
page in the owner's browser, can reach a loopback port. So each run makes a
256-bit token from the operating system's randomness, prints the URL with it,
and refuses any request without it (401) or with another (403). A page on
another site cannot read the view: it does not know the token, and the server
sends no CORS headers. A DNS-rebinding page, served under a hostile name that
resolves to 127.0.0.1, is refused because its `Host` header does not name the
address the connection arrived at (421). Responses carry a content security
policy that allows only the page's own script and style, by SHA-256 hash, and
`fetch` to the same origin; framing, referrers and caching are refused. In the
JSON, `<`, `>` and `&` are escaped even inside strings, and the page places
every value from state with `textContent`; no value is ever parsed as markup.
`--open` hands the browser an owner-only file in the state directory that
forwards to the URL, because a URL given to `open` or `xdg-open` would show the
token in the process list, which other users can read; the file is removed 30
seconds later.

**Cost.** The view is computed on the daemon's single state-owner thread,
like every other request: one pass over projected state, plus at most 5,000
receipts and 5,000 events read for denials. Its latency has not been measured;
the budget is the socket client's 5 s deadline, after which the page reports
the coordinator unavailable.

**Polling, not a socket.** The page asks for `/api/state` every 2 seconds. The
coordinator answers in milliseconds, a view is a few kilobytes, and polling
needs no long-lived connection, upgrade handshake or server push to bound. When
the coordinator is unreachable the server answers 503 with the reason, and the
page keeps the last view, dims it, and says since when.

**LangSpan Live (25 November 2026).** The page was rebuilt as a coordination
display from an owner-approved prototype, on the same server, view and
security boundaries. It leads with counts the view reports, then a traffic
scope (sessions in rows, file paths in columns), the coordination evidence and
an inspector for the selected path, session or record, then every earlier
diagnostic: workspace health, index coverage, publishing, install advice and
the counts of rows the view left out. Its styles copy the design tokens of the
LangSpan site's `assets/site.css`, and its header embeds the brand kit's
lockups from `docs/assets/brand/` line for line; nothing is loaded at run
time. It changes nothing, and it adds no route, listener or CORS header.

**The Live view (issue #491, in progress).** A second view, with the file
matrix kept as the other, shows recorded work as it happens. It has no story
mode: no selected incident, scene tour, caption or panel that opens by
itself. One renderer serves both the dashboard and the homepage hero (site PR
Langspan/langspan.github.io#41): the site passes its recorded fixture, and
the dashboard passes the model of `/api/state` and `/api/events`. The model,
`src/dashboard-live.js`, is pure functions with no DOM, clock or network,
tested under `node --test`, and returns the hero's data shape without its
playback fields (`fixtures/live-view-model.json` pins it):

- one lane per local session with a timed step, labelled from its
  `/api/state` row (task line, harness, shared or own checkout, branch) and
  never by an ID. Remote sessions are not in the local log, so a remote edit
  is never a card or a wire;
- one card per recorded step: a read, a successful edit (`Edit`, `Write`,
  `MultiEdit`, `NotebookEdit` or `apply_patch`), another successful command,
  a held write with its reason in words, and the held session's next
  successful edit of the held path, "Saved after re-reading" when a read of
  it came between and "Saved on retry" otherwise. Edits of one file, or
  reads, in a row within 20 s merge into one card with a neutral count, only
  for legibility;
- exactly one wire per held write that names a peer with a lane, from the
  peer's newest card on that path, or from its lane when it has none. A hold
  without a recorded peer has no wire, and nothing else draws one.

A row without a time (`session_registered`, lease events, a `message_sent`
recorded before messages kept one) is never a card; it goes to the inspector
in seq order, and the view never invents its time. Daemon times become wall
times through the newest `clock_anchored` row; `at_ms` is monotonic across
generations. A page of the feed can hold fewer rows than asked, so the view
reads until a page comes back empty, at most a bounded number of pages per
poll. Path text from the feed is data; the renderer places it with
`textContent`. Later slices of the issue: the renderer ported from the hero
with its QA checks as layout tests, then the dashboard's Live and Files
switch, tested against a real coordinator, and then the site's hero loading
the shared file.

**Identities for joins.** Overlap rows named their sessions only by shortened
labels, which collide: two sessions with the same host and first eight
characters, or two teammates with the same name. So the view now carries
additive, optional fields: each agent row's opaque `id` (`local:` and the
session ID, or `peer:` and the publication's participant, host and session)
and `group` (the overlap group of its workspace: the repository when known,
otherwise the workspace); each overlap row's `group`, `members` (the row IDs
of its labels, index for index) and `more_agents` (labels left out; absent
from an older coordinator, which cut the list without a count); and each
workspace row's `group`. The page joins cells to overlaps by these IDs and
never by label. With an older coordinator that sends none of them, it matches
a label only when it names exactly one listed row, lists the rest in the
inspector unjoined, and says so. A column is one path in one group: linked
worktrees share it, an unrelated repository's file of the same name does not,
and a hold stays in the row of the checkout that admitted it. The fields are
`serde(default)`, so a newer page reads an older coordinator's view; the
protocol version is unchanged.

**A separate demonstration.** `langspan dashboard --demo` serves the same page
and renderer with fictional scenes from `src/dashboard-demo.js`, labeled
"Simulated data" in the header and a banner, behind the same token and policy;
its `/api/state` answers 404. The live page is composed without those scenes,
and a test checks that it never contains them.

**From a phone.** Keep the default and forward the port over an encrypted
channel to the Mac: run `ssh -L 7311:127.0.0.1:7311 mac` in the phone's SSH
client and open the printed URL on the phone; the request arrives at the Mac's
loopback address, which the `Host` check accepts. With a VPN to the Mac (such
as WireGuard or Tailscale), either forward over SSH through it, or bind only to
the Mac's VPN address, `--listen 100.x.y.z:7311 --allow-network`, which only
VPN peers can reach, and open `http://100.x.y.z:7311/?t=...`. Do not use
`--allow-network` with `0.0.0.0` or a public or shared network's address: the
page is plain HTTP, so anyone on the path can read the token and the view.

## Consequences

- The owner can see every agent on every machine heard from, held files,
  overlaps, recorded denials, evidence and health on one page, without anything
  leaving the machine.
- A teammate's session shows only what they published: no ages for their files,
  no admission, and a task line only if they share task lines.
- Denials older than the newest 5,000 events or receipts are not shown, and a
  conflict has no time because none is recorded. Whether publication is failing
  is in the daemon's log, not in state; the page shows the last recorded
  sequence instead.
- A check's applicability is "files changed since" or "no change seen since":
  the view does not compute the working tree, so it never claims a check
  applies. `langspan check status` compares trees.
- A same-user process can read the token from the terminal or the state
  directory, as it can read the database; the token protects against other
  users and other origins, not against the owner's own processes.
- No TLS. Off the machine, the transport's security is the tunnel's.
- The counts are what one bounded view reports, not daily totals or measured
  outcomes. Refusals are a lower bound when the store found more than it
  passed. A daily count through `langspan today` remains a possible later
  addition. The page does not yet read `/api/events`; until the Live view
  ships, it does not build a chronology from successive polls.
- A binary older than this change cannot decode a newer coordinator's view,
  as with every earlier addition to it; the dashboard and the daemon normally
  run from one binary.
