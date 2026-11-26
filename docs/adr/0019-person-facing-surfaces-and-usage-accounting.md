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
| H3 | Desktop pings and a sound | Built 25 November 2026 ([issue #77](https://github.com/Langspan/LangSpan.dev/issues/77)): `langspan notify on\|off\|status [--sound on\|off]`; see "Desktop notices (H3)" below. Not yet run on a macOS or Linux desktop | First not now: needs OS integration outside the state directory |
| H4 | A daily summary | Built 25 November 2026 ([issue #78](https://github.com/Langspan/LangSpan.dev/issues/78)): `langspan today` and MCP `langspan_today`; see "Daily summary (H4)" below. Not yet run on a host over a working day | |
| C10 | A "since you left" briefing | Built 26 November 2026 ([issue #47](https://github.com/Langspan/LangSpan.dev/issues/47)): a section of a session-start briefing after a gap, `langspan recap WORKSPACE --since-away\|--since-seq SEQ` and `langspan_recap`'s `since_away` and `since_seq`, derived from the event log; see "Since the last session (C10)" below. Not yet run on a host after an overnight gap | First not now: briefings already carry current peer work. Built at the owner's request, from events alone, with no figure of time or effort |
| H5 | Monthly refusal count | Built 25 November 2026 ([issue #79](https://github.com/Langspan/LangSpan.dev/issues/79)): `langspan count`, a line in `langspan today` and on the local page; see "Monthly refusal count (H5)" below. Not yet run on a host | First not now: a count of refusals is not evidence of rework avoided. Built as a count of what LangSpan did, which claims no outcome |
| H6 | Daily "tokens saved" line | Not now | No such number is shown until it is measured |
| E3 | Recap with tokens, model and cost | Not now | Depends on J10 |
| J10 | Token and cost accounting from host transcripts | Not now | New trust boundary over private transcripts; would need a STRIDE review. Subscription sessions would store no dollars |
| J11 | Detecting that an agent ran out of quota | Not now | Depends on J10. Claude Code's StopFailure event is hooked (below), but nothing reads it for quota or usage |
| H8 | `compliance_report`: how often each person's and agent's writes were checked first, intents completed or abandoned, collisions, blocks and overrides (added by the second review) | Built 26 November 2026 ([issue #81](https://github.com/Langspan/LangSpan.dev/issues/81)): `langspan report` and MCP `langspan_report`, per session and host kind, from LangSpan's own events; see "Coordination report (H8)" below. Not yet run with both hosts on a shared task | First not now: on a hooked host every write is checked by construction, so the rate says little. Built when the rest was wanted counted from LangSpan's events; it gives counts, not a checked-first rate |
| H9 | Block outcomes: each refusal settled as adapted, overrode, waited or abandoned (added by the second review) | Built 26 November 2026 ([issue #82](https://github.com/Langspan/LangSpan.dev/issues/82)): `refusal_outcomes` in `langspan report`, derived from the event log; see "Refusal outcomes (H9)" below. No retained real-host run holds a `write_refused` event to compare with yet | First not now: a product figure would need the benchmark's definition first, as for H5. Built as counts of recorded events per outcome, which claim no saving and give no rate |
| H10 | Git trouble baseline: conflicts, rejected pushes and reverts seen in agents' shells, and 26 weeks of `git log` counts (added by the second review) | Not now | A new read of the owner's Git history for a figure nothing uses yet. A report built from the counts by a hosted service is out of scope ([ADR 0016](0016-hosted-service-features-out-of-scope.md)) |

**StopFailure (row A2), amended 25 November 2026.** This record first left
the Claude Code StopFailure hook out, since its only uses were J10 and J11.
The launch bar includes it, so it is hooked with Stop, SubagentStop and
PostToolBatch in launch track L3
([ADR 0027](0027-turn-and-batch-events.md)): a StopFailure is recorded as the
end of a turn, which releases that reply's open admissions as Stop does, and
only its `error` type is kept. Detecting quota or usage from it is J11 and
stays Not now.

**Desktop notices (H3), built 25 November 2026.** The daemon shows a
desktop notice for facts it already records, so no host integration is
needed and every host is covered:

- *Kinds.* `refusal` (each `write_refused` event, H2), `check_failed` (an
  automatic check's `check_reported` with a failed result, ADR 0030; a check
  recorded by hand has no report and raises none) and `handoff_waiting` (a
  handoff no session has acknowledged for `notify_handoff_after_ms`, 10
  minutes by default, told once). The store keeps the committed events of
  the first two kinds for the daemon's owner thread, which turns them into
  notices after they are durable; a request answered from its receipt
  commits nothing and raises nothing, and history replayed at start raises
  nothing. Handoffs carry no time of their own, so the daemon times each
  from when this run first saw it waiting: one waiting when the daemon
  starts is told 10 minutes after the start.
- *Text.* Title `LangSpan`; the body names the workspace's directory, the
  session (host and the first eight characters of its ID), the peer where
  the refusal names one and at most three paths (each cut to its last 48
  bytes) with a count of the rest. Control characters are replaced, and the
  body is cut to 200 bytes at a character boundary. It never holds a
  prompt, a message's text, a check's output or file content.
- *Delivery.* `osascript` on macOS, `notify-send` elsewhere, the first
  regular executable of that name in an absolute directory of the daemon's
  `PATH`, found at each notice. Title and body are arguments, never spliced
  into a script (`on run argv` for AppleScript; `--` before them for
  `notify-send`), and the title comes first so a body starting with `-` is
  not an option. A sound is an argument of the same call (`sound name` or a
  `sound-name` hint); no other command is run for it. Each runs in its own
  process group with null stdio on a dedicated thread fed by a queue of 16;
  the group is killed at 2 seconds. A full queue drops the notice.
- *Limits.* At most one notice of a kind per `notify_kind_interval_ms`
  (5 minutes) and `notify_hourly_limit` (10) in any hour; a dropped notice
  does not restart the interval. The daemon's log records each notice as
  `notice_sent` or `notice_dropped` with kind and reason; a notifier that is
  missing, fails or times out is logged once per daemon run for each of
  the three. No event is added.
- *Configuration.* `notify` and `notify_sound` in the owner-only
  `config.json`, both off by default. `langspan notify on|off` rewrites the
  file whole (a temporary owner-only file, then a rename), keeping other
  keys, and refuses a file others can read. The notifier thread reads the
  file again at each notice, so switching needs no restart; an unreadable
  file counts as off and is logged once per change. A daemon of an older
  build refuses to start with these keys in its configuration, as for any
  key it does not know.

Tests: `src/notify.rs` (limits on an injected clock, body length, path
and session shortening, notifier arguments) and `tests/notify.rs` (a real
daemon and hook executables with a stand-in notifier first on the daemon's
`PATH` that records its arguments: none started with notices off, exactly
one per refusal and the second dropped, a sound only when switched on, a
hung notifier killed while the daemon answers, a failed automatic check
and a waiting handoff, `notify status` with and without a notifier). A
notice shown on a real macOS or Linux desktop after a Claude Code refusal is
still to be retained.

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
  zone's offsets near that date's 00:00, read in-process from the system's
  zone files (see *Zone rules read in-process* below), so
  a day the clocks change starts at its own midnight; times are shown at the
  offset now for the whole window.

The two event kinds are read by this build only: an older build that opens a
store holding them stops at the first, as for any event kind it does not know
([ADR 0015](0015-version-tolerance.md)). Tests: `crates/langspan-core/tests/today.rs`
(injected clocks across a simulated midnight, a restart without an anchor and
a reboot), `tests/today.rs` (real daemon, Git, hooks and MCP adapter).

**Monthly refusal count (H5), built 25 November 2026.** `langspan count
[--month YYYY-MM] [--json]` counts the `write_refused` events (H2) of one
local month, at query time, from the event log; nothing is stored for it and
no event or receipt is written. It is shown also as one line in the text of
`langspan today` and on the local page, whose `/api/state` view carries it as
`refusals_this_month` ([ADR 0013](0013-dashboard.md)). Nothing of it reaches a
model: the MCP answer of `langspan_today` leaves the line out, and there is no
MCP tool. The decision's reason stands: every text says "refusals", never
"collisions prevented" or "saved", and carries the line "A refusal is not
proof of avoided rework." until H9's outcomes and a measurement exist. A test
holds the text against a list of words that would claim an outcome. Choices:

- *Counting once.* A refusal is counted once per session, workspace, kind and
  path set (the event's at most 8 sorted paths) per local day; the others are
  `repeats`. `by_kind` always names the four kinds, so they add up to
  `refusals`. This is a different unit from `today` and the status line,
  which count each `write_refused` event.
- *Local days without a time zone in the core.* The CLI turns the month into
  its local days' starts and the next month's first (2 to 33 instants,
  strictly increasing, checked by the daemon), each from `local_day` with
  the zone's offsets for that date, so a day the clocks change is 23 or 25
  hours long. Finding them takes about a hundred offsets from the zone's
  rules, read once; the local page finds them once per month per run and
  keeps them.
- *Zone rules read in-process.* Offsets first came from one `date` child per
  instant. A month needs about a hundred, and on a loaded macOS CI runner
  they did not all finish within the 2 s deadline, even 32 at a time, so
  `today` and `count` refused the month. `local_day` now reads the zone the
  way the C library does: the TZif file (RFC 8536) that `TZ` or
  `/etc/localtime` names, its 64-bit transitions and footer rule, or a POSIX
  `TZ` rule. Unlike the C library, a `TZ` that is neither is refused rather
  than taken as UTC. No child process is started, so no `date` arguments
  differ between GNU and BSD, and no deadline applies. Reading a file
  that is no zone file, or one of more than 256 KiB, gives no offset.
  Leap-second zones are read as plain Unix time, at most half a minute off
  at their changes. A Linux test compares the reader with GNU `date` in ten
  zones and rules every 5 hours of 2024 to 2027. The
  core places each refusal in wall time by its generation's anchor and on a
  day by those instants; a refusal of a generation without an anchor is
  `undated`, in no month.
- *Retried through.* A refusal is `retried_through` when its session was
  later admitted on the same paths (the admission's sorted paths, cut at 8
  as the refusal's are), in the same workspace, among the session's next 16
  admissions after the last repeat. A held commit is followed by no
  admission and is never counted there. This says a retry went through, not
  whether it was right to.
- *Bounds.* One count reads at most the newest 20,000 `write_refused` rows
  and every clock anchor, through partial indexes created when a store
  opens (`events_write_refused`; `events_grant_session` over admissions by
  session for the retry lookup; the generation markers as before), so its
  cost does not grow with the rest of the log. It says `truncated` when older
  refusal rows exist and the oldest read is not before the month, and
  gives `retained_since_ms` when the month starts before the cutoff of the
  history retention kept, since the removed refusals cannot be counted. The index
  changes no stored format; building it is one pass over the log at the
  first open. Its latency has not been measured.
- *Failure mode.* With the daemon down `count` exits 1 and shows no figure;
  `today` shows that the count could not be read; the local page says so in
  place of the line.

Tests: `crates/langspan-core/tests/count.rs` (injected days and anchors,
a New York day across a UTC midnight, the month's edges to the millisecond),
`src/count.rs` (a month with a clock change, the word list), and
`tests/count.rs` (a real daemon, Git and Claude Code hook events; the first
run's anchor is moved so its refusals fall at 02:30 UTC on 1 April 2027,
counted in April in UTC and in March in New York; the local page's total).
A real-host run with Claude Code and Codex producing each kind they support
is still to be retained.

**Coordination report (H8), built 26 November 2026.** `langspan report
WORKSPACE [--since RFC3339] [--json]` and the read-only MCP tool
`langspan_report` count, for each session and in total for each host kind
(`claude-code`, `codex`, `cursor`, `antigravity`, `other`; `antigravity`
replaced the `gemini` kind when Gemini CLI support was removed, ADR 0033,
and earlier Gemini CLI sessions count as `other`), from the event log:
tool calls admitted (`edits_admitted`), those with a file whose version was
not compared (`edits_base_unknown`), shell runs seen only after the command
(`shell_writes_after`), reads the host reported, intents set, ended and left
open at the session's end, refusals by kind, refusals retried unchanged and
briefings delivered. It is a read: no event, no receipt, no
acknowledgement. Four choices were needed:

- *Counts, not rates.* A share is shown only beside the count it is out of
  (`9 of 41`), and the answer holds no rate field. Nothing in it is a saving
  or an outcome; it says what was recorded.
- *Host differences are reported.* A host's total lists the fields its hooks
  cannot supply (`not_reported`; for Codex, `reads_reported`), and the text
  shows `-` there, so a zero is not read as behavior. The host kind is the
  prefix of the session ID, which the hook sets; the adapters' source names
  and the shell tool's name now come from one constant each in the core.
- *An identical retry.* `write_refused` now keeps the refused tool call's
  input digest (`input_digest`, absent for a held commit and in older
  records). A refusal in the window followed by an admission of the same
  session's call with the same digest, under a new tool-call ID, counts one
  `retried_unchanged`; each refused digest is matched once. A second
  delivery of the refused call is answered from its receipt and adds
  nothing. An older record without a digest is never matched. This is the
  only new stored field.
- *Placing and bounding.* Events are placed in wall time by the same code as
  `today` (`Placement`, shared), from the same newest 50,000 events; a
  window reaching past them says `truncated` with `covered_from_ms`, and so
  does one starting before the newest retention cutoff, whatever older
  generation markers retention kept: `covered_from_ms` is then the later of
  the cutoff and the oldest timed event read. An
  intent set before the window and still open when its session ends inside
  it counts as left open, even when it was set before the events read: the
  store folds the older intent events, found through a partial index, into
  the set of sessions holding one where the read starts. Removing a
  workspace ends the intents in it there, as it does in the state. That
  fold starts from the newest compaction snapshot and reads at most the
  newest 10,000 intent, registration and workspace-removal events before
  the events read (review 4705125393), so a report's cost, and so the time
  it holds the store worker and the admissions queued behind it, stays
  bounded however long the log grows. When more such events exist, the
  snapshot is not used and only the sessions those 10,000 events name are
  known: an event in the window whose session's workspace, or whose
  session's open intent at its end, rested on an older event is in no
  count, and the report says `partial`. At most
  16 sessions are listed, earliest first;
  host totals include every session. The MCP text, labelled as data, is at
  most 2,400 bytes, dropping session rows last first.

The new field is read by this build only where it is present; an older build
that opens a store holding it stops at the first such event, as for any field
it does not know ([ADR 0015](0015-version-tolerance.md)). Tests:
`crates/langspan-core/tests/report.rs` (decided events with injected clocks)
and `tests/report.rs` (a real daemon, Git, hooks fed with recorded Claude Code
and Codex events, and the MCP adapter with 40 sessions). A run of both real
hosts over a shared task is still to be retained.

**Refusal outcomes (H9), built 26 November 2026.** The coordination report's
counts gain `refusal_outcomes`: for each session and host kind, how its edit
refusals in the window were settled. Each `conflict`, `stale_base` or
`unread_lines` refusal opens one pending outcome per session, workspace, kind
and path set; a refusal of the same set before it settles joins it. The next
relevant event of that session settles it once. A conflict whose holder's
admissions of the refused paths ended (completed, expired or a restart)
before the session wrote one is `waited`. The holder's session ending ends
none of its admissions, as they are still enforced. Otherwise, a reported read of a refused path followed by a write is
`adapted`. A write identical to a refused call (same input digest) or a
shell write is `retried_unchanged`, or `unknown_read` on Codex, which reports
no reads. A write whose call differs from every refused one is `adapted`. No
write within 30 minutes of the latest refusal, or the session's end, is
`abandoned`. What the events cannot tell is `unknown`: a refusal without a
digest followed by a different call, or a conflict whose holder's admission
is not among the events read. An outcome not settled yet is counted as
`pending`. Choices that differ from the issue's proposal:

- *Derived, not stored.* The issue proposed a `refusal_settled` event
  appended by the daemon. The outcomes are instead computed by the core, in
  the report's one pass over the events it already reads (`Settler` in
  `crates/langspan-core/src/refusal_outcome.rs`). No event or schema change
  is needed. No pending state lives in the
  daemon, so a restart settles nothing on its own and needs no `restart`
  reason: a refusal before a restart is settled by the events after it,
  placed in wall time by their generation's anchors. A restart ends every
  admission of the earlier generation without a `lease_expired` event, so
  a conflict's holder counts as released at `booted`. Outcomes therefore do
  not appear in `langspan events`. A later rule change re-derives every
  outcome from the same log, which a stored event would freeze.
- *Precedence.* `waited` comes first, because a conflict keeps refusing the
  identical call while the holder holds the file
  ([ADR 0022](0022-a-conflicted-file-is-refused-once.md) is about stale
  versions), so a write after the holder released it is no blind retry.
  A reported read comes next, then the digest comparison.
- *A changed call without a read is `adapted`.* After a stale refusal the
  session has been told the new version, so an admitted changed call is
  compared with it; the call's change is the recorded response. Whether the
  change was correct is out of scope.
- *Time.* An admission carries no time of its own; it is placed at the timed
  event before it, as every event in the report is. A write that comes back
  after 30 minutes with no timed event between can therefore still settle a
  refusal. The 30 minutes are a constant (`REFUSAL_WINDOW_MS`), not a
  setting; tests shorten it with an undocumented environment variable that
  can only shorten it.
- *Held commits* open no outcome: no write answers them.
- *Wire protocol version 5.* A CLI or MCP adapter of an earlier build reads a
  report's counts with `deny_unknown_fields`, so an answer carrying the new
  field would fail to read as a whole. The protocol is version 5 instead. The
  daemon answers a version 4 or version 3 request in that version, with the
  counts' `refusal_outcomes` left out (`protocol_4_shape` in
  `src/protocol.rs`, applied before version 3's shape). Requests are the
  same in all three. A version 5 client refused with `unsupported_version` by
  a daemon of an earlier build asks once more in the version that daemon
  answered in. Tests:
  `clients_of_protocol_4_and_3_get_reports_without_refusal_outcomes`
  (`tests/refusal_outcomes.rs`),
  `v5_report_response_and_its_version_4_and_3_forms_match_the_golden_vectors`
  (`tests/protocol.rs`, against `fixtures/report-response-v5.json` and
  `fixtures/report-response-v4.json`) and
  `a_daemon_of_an_earlier_build_is_asked_again_in_its_version`
  (`src/client.rs`, for a daemon of version 4 and of version 3).

Nothing reaches a model, and nothing is held or refused because of an
outcome. The cost is bounded by the events the report already reads. Tests:
`crates/langspan-core/tests/report.rs` (the window's edge to the
millisecond, shell writes on each host kind, a restart between a conflict
and its retry, the unknown cases, a held
commit, a refusal before the window) and `tests/refusal_outcomes.rs` (a real
daemon and real Claude Code and Codex hook events for each acceptance
criterion of issue #82). The only retained real-host run with a judged
LangSpan refusal (Codex run 3c2 in
[the Codex evidence](../evidence/codex-real-host/README.md)) predates
`write_refused`, so its log holds no refusal to settle; comparing computed
outcomes with judged ones waits for a new real run.

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
  time-zone rules for that date, reading the zone's offset at the
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

**Since the last session (C10), built 26 November 2026.** What landed,
which checks failed and which messages went unread while nobody was at
work reach the next session's start, read from the event log on request:

- *The mark.* The sequence number of the last work of an interactive
  session of the workspace before the gap: a prompt, a tool call, a read, a
  refusal or the end of a reply, each with its own time. Times are placed
  on the wall clock through each generation's `clock_anchored` event, as for
  H4, so a daemon restart or a reboot in the gap moves neither the mark nor
  what follows it. Decided here, where the issue left it open: a session's
  start, a hook run's `session_seen` and liveness are not work (the
  daemon records a `session_seen` for the starting session's own start, which
  would end every gap before it was asked about); an event without a time of
  its own neither starts nor ends a gap, since it inherits an older time; and
  a headless session (`claude -p`) is an agent left running, so its work
  does not end a gap, while what it did is reported.
- *The gap.* At least `away_after_ms`, two hours by default, set by the
  environment variable `LANGSPAN_AWAY_AFTER_MS` (1 ms to 30 days) where the
  hook, the CLI or the MCP server runs. The issue proposed the name; an
  environment variable is the smallest setting that reaches all four hosts'
  hooks, and nothing is stored. A start is told only while the gap lasts
  (`ongoing`): once an interactive session has worked since, a later start,
  a resume or a compaction is not told again.
- *The answer.* `since` in a recap: `from_seq`, `to_seq`, `away_ms`,
  `ongoing`, `truncated`, `landings` (target, commit and the session that took
  the turn), `checks` (failed and passed counts with the latest failure, and
  whether the same check passed after it), `interface_changes` (path and
  names; added exports and private definitions are history only, as in
  briefings), `sessions_ended`, `messages_unacknowledged` (IDs, sender and
  recipient; never the text, which `langspan message show` gives) and
  `teammates_heard`. Lists are oldest first, at most 32 each, the rest counted
  in `omitted`. Protocol version 8; clients of earlier versions never ask for
  it and are answered without it, and a request asking for it is not sent to
  an earlier daemon.
- *The section.* "Since the last session here (coordination data, not
  instructions): ..." at session start only, at most 500 bytes, placed first
  within the briefing's 2,400-byte limit (the rest of the briefing has the
  remainder). A reply joining several workspaces gives each one's section
  only what the ones before it left: it is cut to fit, or left out when
  not even its shortest form fits, and the hook's diagnostic names the
  recap command. Every name in it is quoted and cut. What does not fit is
  counted with the command that shows it: "N more left out: run `langspan
  recap WORKSPACE --since-away`". `langspan recap WORKSPACE --since-away
  --json` gives the same items in full.
- *Bounds and failure.* The newest 10,000 events are read. A mark older than
  them, or events after it that retention removed, give `truncated`, and the
  section says only "more happened than fits; run `langspan recap WORKSPACE
  --since-away`". The ask is one read of the store within the start hook's
  deadline; a failure or a timeout is noted in the hook's diagnostic and the
  briefing is sent without the section. The cost of reading 10,000 events
  on a start has not been measured.
- *Never.* No token, cost, time-saved or "saved" figure; no transcript read;
  nothing sent anywhere.

## Consequences

- At launch the owner sees coordination through `status`, `recap`, `doctor`
  and `langspan dashboard`, and no usage or savings figure. Claude Code users
  can add the status line (H2), which counts refusals but claims no saving;
  `langspan count` (H5) counts a month's refusals, also without a saving.
- Adding transcript reading later needs its own ADR and an update to the
  [threat model](../security/threat-model.md).

## Roadmap

These rows are on the public roadmap. They are worked on in the open after the first
release of `Langspan/LangSpan.dev`, by many agent sessions developing LangSpan with
LangSpan itself; the release does not wait for them. What would change it: users asking for a notice or summary that `status`, `recap` and the local page do not give; for J10, J11 and E3, an accepted STRIDE review of reading transcripts; for H6, a measurement that ties a figure to avoided rework (H5 is built as a count that claims no outcome).
