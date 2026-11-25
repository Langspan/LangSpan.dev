# ADR 0019: Person-facing surfaces and usage accounting wait

**Status:** Not now. Decided 25 November 2026.

## Context

LangSpan is headless: its users are agents, through hooks and MCP, and the
owner, through the CLI and an optional read-only local page
([ADR 0013](0013-dashboard.md)). Other surfaces for the person are possible (a
status line, desktop notices, a daily summary, counters), as is reading host
transcripts for token, cost and quota figures. The capability review of 25 November 2026
([roadmap](../roadmap.md)) lists these as Missing or Partial.

Reading a host's transcript files is a new trust boundary: they hold the
owner's prompts and the model's output. Any "saved" figure shown to a person
is a performance claim, and [engineering practice](../engineering.md) requires
such numbers to be measured.

## Decision

| Row | Capability | Decision | Reason |
| --- | --- | --- | --- |
| H2 | Status line: agents here, catches today | Built (amended below) | First not now: the launch is about coordination between agents; `status` and the local page cover the owner |
| H3 | Desktop pings and a sound | Not now | Needs OS integration outside the state directory |
| H4 | A daily summary | Built 25 November 2026 ([issue #78](https://github.com/Langspan/LangSpan.dev/issues/78)): `langspan today` and MCP `langspan_today`; see "Daily summary (H4)" below. Not yet run on a host over a working day | |
| C10 | A "since you left" briefing | Not now | Can be derived from events later; briefings already carry current peer work |
| H5 | Monthly catch counter | Not now | A count of refusals is not evidence of rework avoided |
| H6 | Daily "tokens saved" line | Not now | No such number is shown until it is measured |
| E3 | Recap with tokens, model and cost | Not now | Depends on J10 |
| J10 | Token and cost accounting from host transcripts | Not now | New trust boundary over private transcripts; would need a STRIDE review. Subscription sessions would store no dollars |
| J11 | Detecting that an agent ran out of quota | Not now | Depends on J10. Claude Code's StopFailure event is hooked (below), but nothing reads it for quota or usage |
| H8 | `compliance_report`: how often each person's and agent's writes were checked first, intents completed or abandoned, collisions, blocks and overrides (added by the second review) | Not now | On a hooked host every write is checked by construction, so the rate says little; the rest would be counted from LangSpan's own events once someone needs it |
| H9 | Block outcomes: each refusal settled as adapted, overrode, waited or abandoned (added by the second review) | Not now | Whether a refusal changed what the agent did is judged in retained runs for now (the launch validation, PR #56); a product figure would need the benchmark's definition first, as for H5 |
| H10 | Git trouble baseline: conflicts, rejected pushes and reverts seen in agents' shells, and 26 weeks of `git log` counts (added by the second review) | Not now | A new read of the owner's Git history for a figure nothing uses yet. A report built from the counts by a hosted service is out of scope ([ADR 0016](0016-hosted-service-features-out-of-scope.md)) |

**StopFailure (row A2), amended 25 November 2026.** This record first left
the Claude Code StopFailure hook out, since its only uses were J10 and J11.
The launch bar includes it, so it is hooked with Stop, SubagentStop and
PostToolBatch in launch track L3
([ADR 0027](0027-turn-and-batch-events.md)): a StopFailure is recorded as the
end of a turn, which releases that reply's open admissions as Stop does, and
only its `error` type is kept. Detecting quota or usage from it is J11 and
stays Not now.

**Daily summary (H4), built 25 November 2026.** `langspan today [WORKSPACE]
[--since RFC3339] [--json]` and the read-only MCP tool `langspan_today`
summarise a window (local midnight to now by default) from the event log:
each session's first and last activity, distinct files its successful edits
named and its prompts; refusals by kind; check results; how each landing's
last attempt ended; and, from current state, admissions held, intents not
ended, messages not acknowledged, handoffs not taken up and checks whose
newest result failed. It is a read: no event, no receipt, no
acknowledgement. Three choices were needed:

- *Placing events on a day.* Event times are on the daemon's monotonic clock,
  which restarts at a reboot, and several events carry no time. Each daemon
  start records `clock_anchored` (generation, its clock, the wall clock)
  after `booted`, as for the status line, so a time of that generation maps
  to wall time. An event
  without its own time takes the time of the event before it in its
  generation; a landing's `turn_released` therefore takes the time its
  `turn_taken` recorded, even when it lands on check evidence from an earlier
  day. Generations started by an earlier build have no anchor; their
  events are counted as `undated` and in nothing else. A wall-clock change
  while the daemon runs shifts that run's placement by the change.
- *Counting refusals.* Refusals are the status line's `write_refused` events
  (H2, above), counted by kind in the window: `conflict`, `stale_base`,
  `unread_lines` and, as the hook reports a held commit, `commit_held`. Each
  is committed with its refusal's receipt, so a retry of the same request is
  not counted twice.
- *Bounds.* One answer reads at most the newest 50,000 events and says
  `truncated` when the window may reach past them: the first event read
  that carries its own time (or an anchor) bounds the ones left out, since an
  event without one only inherits an older time. The anchor of the oldest
  event read is found through a partial index of `booted` and
  `clock_anchored` rows (`events_generation_marker`, created when a store
  opens), so its cost does not grow with the generation's history: behind
  100,000 events of one generation it fell from 17.2 ms to 25 µs (one run,
  debug build, arm64 macOS); building the index added about 40 ms to
  the first open of that store. The text is at most 40
  lines and eight files per session; the MCP text, labelled as data, at most
  2,400 bytes, as a briefing. Local midnight is found by `local_day` from the
  zone's offsets near that date's 00:00, read from `date` (2 s deadline), so
  a day the clocks change starts at its own midnight; times are shown at the
  offset now for the whole window.

The two event kinds are read by this build only: an older build that opens a
store holding them stops at the first, as for any event kind it does not know
([ADR 0015](0015-version-tolerance.md)). Tests: `crates/langspan-core/tests/today.rs`
(injected clocks across a simulated midnight, a restart without an anchor and
a reboot), `tests/today.rs` (real daemon, Git, hooks and MCP adapter).

**Status line (row H2), amended 25 November 2026.** H2 is built
([issue #76](https://github.com/Langspan/LangSpan.dev/issues/76)), for Claude
Code only, the one launch host with a status-line command:

- `langspan statusline [--root DIR] [--json] [--deadline-ms N]` prints
  `langspan · 3 sessions here · 1 refusal today`. Sessions here are the
  conversations (not host subagents) of the asked directory's repository,
  linked worktrees included, of the current generation, not ended or known to
  have exited, and active within `BRIEF_HORIZON_MS`. It is a read
  (`status_line`): no receipt, no event, answered from memory.
- Every refusal appends one `write_refused` event (session, workspace, kind
  `conflict`, `stale_base`, `unread_lines` or `commit_held`, at most 8 paths,
  the peer it names, time and generation). The core records the first three
  with the refusal's receipt, so a retried request adds none; a manual
  `admit` refused is not a write and records none. A held commit is decided
  by the hook, which reports it with `record_refusal` under a request ID made
  from the hold's fingerprint; the core accepts only that kind there. H4, H5,
  H8 and H9 can read the same event.
- "Today" starts at local midnight. Event times are on the daemon's
  monotonic clock, so each daemon start appends `clock_anchored` (that clock
  with the wall clock); a refusal is placed in wall time by its generation's
  anchor as it is applied, and a refusal of an unanchored generation (older
  history) is never counted. The CLI finds local midnight with the system's
  time-zone rules for that date, asking `date` for the offset at the
  instants around midnight (so a daylight-saving day starts at its own
  00:00, not an hour off), falling back to UTC. At most 1024 refusals from
  the last 48 hours are kept in memory for the count; they are derived from the events on replay.
- Failure mode: one socket call within 200 ms (100 to 5000); when the
  coordinator is down or slow it prints `langspan · coordinator not running`
  and exits 0. It never starts the coordinator, uses no network and writes
  nothing. The line shows counts only, never paths, and nothing reaches the
  model.
- `langspan install --statusline on` sets `statusLine` in
  `~/.claude/settings.json` only when none is set, with a marked command, and
  says so in the dry run when one is; `--statusline off` and
  `uninstall --apply` remove only LangSpan's.

H4 (below) reads the same `write_refused` and `clock_anchored` events, with
one definition of each. A real Claude Code session showing the line
change after a refusal is still to be retained; the status-line input used by
the tests is constructed from Claude Code's documentation
(`fixtures/claude-code-statusline-constructed.json`).

## Consequences

- At launch the owner sees coordination through `status`, `recap`, `doctor`
  and `langspan dashboard`, and no usage or savings figure. Claude Code users
  can add the status line (H2), which counts refusals but claims no saving.
- Adding transcript reading later needs its own ADR and an update to the
  [threat model](../security/threat-model.md).

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: users asking for a notice or summary that `status`, `recap` and the local page do not give; for J10, J11 and E3, an accepted STRIDE review of reading transcripts; for H5 and H6, a measurement that ties the count to avoided rework.
