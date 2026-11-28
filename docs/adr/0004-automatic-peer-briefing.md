# ADR 0004: Automatic peer briefing

**Status:** Implementation decision for the owner-directed coordination work,
24 November 2026. Extends [ADR 0003](0003-native-coordination-observation.md).
It adds awareness; it does not widen the admission guarantee of
[ADR 0001](0001-local-coordination.md).

## Context

Stored facts help an agent only if it receives them before it chooses an edit.
Until now the hooks admitted and observed edits, and an agent learned about
peers only by calling `langspan_status` or by being denied. The
M3 capability review (P1-02) named this the largest gap
between the implementation and the product.

## Decision

**Delivery points.** The hook adapter handles three session events for both
hosts, in addition to the edit events: `SessionStart`, `UserPromptSubmit` and
(Claude Code only) `SessionEnd`. The installer registers them without a tool
matcher. (Since [ADR 0027](0027-turn-and-batch-events.md), Codex `SessionEnd`,
both hosts' `Stop` and Claude Code's `PostToolBatch` are registered too.) Start and prompt register the session, record process evidence where
the platform supports it, and print peer context as
`hookSpecificOutput.additionalContext`, which the host places in the model's
context before the model acts. They never return a permission decision. Any
failure prints nothing: the session proceeds exactly as it would without
LangSpan.

**What a session's work is.** The core keeps one bounded activity record per
session, derived from events: the 32 most recently successfully edited paths
with their times, and one line of the owner's latest prompt (redacted,
whitespace collapsed, control and invisible characters removed, at most 200
bytes). That line is the first of the prompt's leading 32 non-empty lines to
start with `Task:` (any case, marker removed); otherwise its first non-empty
line. Only that one line leaves the session as text. `hooks install
--prompt-summary off` installs hooks that record no prompt text at all. Current admissions and declared intents are
already in state. No model call summarises anything.

**Sessions from one template (issue #114).** Several agents are often started
from one shared prompt whose first line is the same for all of them. With the
prompt's line, the hook also sends a 128-bit SHA-256 prefix of each of its
first 32 non-empty, redacted and cleaned lines. The core keeps them in the
session's activity and answers with the number of leading lines that the
latest prompt of another live session in the same repository (registered this
generation, not ended, not known to have exited) shares. When that number is
neither zero nor the whole prompt and the prompt has no `Task:` line, the hook
records the prompt again under its first line that differs. A one-line prompt
is therefore summarised as before. The comparison runs on digests so that no
other line of either prompt is sent or stored; a digest of a short line is
guessable by anyone who can read the local store, who can read the task lines
already. Digests are never shown in a briefing and never published. The
session that started first keeps its first line until its next prompt; its
peers' lines tell them apart. A declared intent replaces the task line in a
briefing while it is set, since it is the session's own account of its task.

**Who is relevant.** A briefing for session S names another session P only when
all of these hold:

- P is in S's physical workspace, or in a workspace the observer has placed in
  the same repository (below);
- P registered in the current daemon generation;
- P's process is not recorded as exited;
- P holds a current admission, has a declared intent, or edited or was prompted
  within 30 minutes. A session the host reported ended is named only for the
  edits it left behind, labelled ended and without its task line.

Peers that share a path with S's own work come first, then peers editing now,
then the most recent. At most 6 are delivered by the hooks (16 through the
command), each with at most 8 current and 8 recent paths; the rendered text is
capped at 2,400 bytes. Each peer is one line of at most 700 bytes with its
shared files first; a peer that does not fit is counted, not cut. Every value
that came from a peer or the repository is cleaned and quoted. The core also
stops adding peers once the result would pass 40 KiB, so a briefing always fits
one 64 KiB frame.

**Not twice.** The core stores a 64-bit fingerprint of the last briefing
delivered to each session, computed without ages. A prompt delivers only when
it changed. The fingerprint also leaves out the branch of a peer in S's own
checkout: that is S's branch too, and the Git observer may learn it only after
the start briefing, which would otherwise be delivered again unchanged at the
next prompt; a switch of that branch reaches S as a checkout change. A peer in
another worktree changing branch is news. A collision would withhold one repeat briefing and nothing else.
Only a request marked `deliver` is remembered: the hooks and the MCP tool set
it, and `langspan brief` from a terminal does not, so looking never suppresses
a session's next briefing. A prompt event the host numbered (`prompt_id` or
`turn_id`) is one request however often it arrives; session start and end carry
no number, so each delivery of those is a new request.
A session start always delivers a non-empty briefing, because a new, resumed or
compacted context no longer holds the earlier one.

**Repositories.** A complete Git scan now also records the workspace's
repository and branch: `local-` plus the first 128 bits of the SHA-256 of the
canonical Git common directory. Linked worktrees share it; separate clones do
not. It groups workspaces for awareness only. Admission domains stay per
physical workspace, and a shared file in another worktree is reported as a
logical overlap that may conflict at integration, never as a held file.

**One operation.** `brief` is a core command that goes through the same store
and receipt path as every mutation, exposed as `langspan brief SESSION` and the
MCP tool `langspan_brief`.

**Versions.** The wire protocol is version 3. The SQLite schema marker is 3;
no table changed, and the marker makes an older binary refuse a history that
contains the new event kinds. Version 2 histories are read unchanged.

**Mid-turn delivery (added later).** After a completed Claude Code tool call
(PostToolUse, including a recorded Read), the hook asks for the briefing
again and, only if it changed since the last delivery, returns it as
`additionalContext`. News that arrives during a long turn therefore reaches
the model at its next tool call instead of its next prompt. It is never sent
in place of a decision. Codex does not get it yet: its handling of context on
a completed tool call has not been observed on the host.

**Changes only (added 26 November 2026, issue #222).** The fingerprint above
made a briefing all or nothing: any change, including a peer re-editing a file
it had already edited or two peers swapping places in the order, delivered the
whole briefing again, every turn and after every tool call. In the third K1
pilot the `langspan` arm used 1,911k cache tokens against 516k for
`awareness_only`, mostly from the skill and these repeats. Now a briefing at a
prompt or mid-turn carries only what changed since the session was last told,
and the full view goes only to a session start (a new, resumed or compacted
context) and to an explicit request (`langspan brief SESSION` or the MCP tool
`langspan_brief`).

- *What is told.* Each briefing item has a 64-bit key of what it says, without
  ages: a peer's own line (its session, worktree, the branch of a peer in
  another worktree, its task line and declared intent, whether it ended or its
  process is no longer confirmed, its relation), and one key per listed path
  and kind (`overlap`, `target`, `editing`, `recent`, `looking`); a stale read
  (path and editor); a checkout change; a note; an interface change (path,
  changes and affected files); a teammate's published work. Delivering a
  briefing records, in the session's activity, the keys the session has now
  been told: those of the current full briefing that it was told before or
  is told now, in order, at most 256 (`MAX_TOLD`). An item past the bound is
  not remembered and is told again; nothing is dropped for space.
- *What is left out.* A `Brief` with `changes_only` leaves out every item whose
  key the session was told, and counts them in `unchanged`. A peer whose own
  line was told keeps only its new paths, without its task line. A peer with
  nothing new is left out. An item that goes away is forgotten, so if it comes
  back (a peer edits the file again after its edits aged out, a session
  resumes) it is news again. Messages, withdrawals, check results, in-flight
  signatures, uncoordinated changes and left work are not keyed: each is
  delivered once already, as before. The prompt and mid-turn hooks set
  `changes_only`; session start, `langspan brief` and `langspan_brief` do not.
  A session's first briefing is the full view either way, since it was told
  nothing. A request without the field, from an older client, gets the full
  briefing in its old shape.
- *What repeats, and why.* Three items repeat after they were told, once each
  time the session reads or edits one of their files after the last briefing
  that placed items in its context (`told_at_ms`): a peer's hold on a file in
  this checkout that the session works on (the `editing` and `overlap` path
  of that file), because admission refuses the session's edit while the hold
  lasts; an interface change, when the session reads or edits a file that
  mentions a changed name, because that is where a break would be written; and
  a stale read under the same rule, though reading the file again records the
  new version and so resolves it, and admission refuses an edit of it
  (ADR 0005). Everything else is advisory and is not repeated. Refusals are not
  briefing items: admission refuses every attempt on a held, stale or unread
  file and says why each time, and write-time interface alerts (ADR 0029) are
  given once per notice per session as before; neither changed. A read or edit
  in the same millisecond as the briefing that last told the session counts as
  told with it, since the clock cannot order the two.
- *Mid-turn presence.* Where a peer is looking is still not news by itself
  during a turn: it stays out of the told keys and waits for the next prompt.
- *What the text did not place (review comment 4711375989).* The core records
  the keys when it delivers, before the hook renders the text within its
  limits. The renderer also returns what it placed in full: a peer line or
  notice left out ("more sessions not shown", "more notices not shown"), a
  notice line cut short, a path or a task past a peer line's cut, and presence
  without room are not placed. The hook then sends `ForgetTold` with the keys
  of the items it did not place (from `briefing_keys`), at most 256, and the
  core records `ToldForgotten` and drops them from the told keys, so the next
  briefing of changes only names them. A retry, or keys the session was not
  told, records nothing. When the forget does not reach the daemon within the
  hook's deadline, the items stay told, as a missed deadline already leaves
  them; it never hides an item.
- *The pointer.* A briefing that left anything out says so: it opens with
  "What changed since your last briefing", and in place of the closing
  paragraph about holds, which the full view gave, it ends with one line that
  counts the unchanged items and names `langspan_brief` and `langspan brief
  SESSION --request-id NEW_ID` for the full view.
- *Older records.* A delivery stored before this change has no keys: the next
  briefing is the full view. Keys are kept per generation, like the rest of a
  session's activity, so after a daemon restart a session is told the full
  view again. The keys are bookkeeping and are left out of the bounded
  `status` view; `status --all` has them.

**Context measured (added 26 November 2026, issue #222).** Each hook that
places text in a session's context records how many bytes it placed:
`RecordContext` and its event `ContextAdded`, a measurement only. Session
start and prompt hooks record one at every turn, with zero bytes when they
had nothing to add, so the count of turns is exact. A tool hook records the
`additionalContext` its final reply carries, whatever placed it there: a
mid-turn briefing, an interface alert at a write (ADR 0029, including the
Gemini CLI one given after the tool), a formatter's notice, or a batch's
briefing (review comment 4711488164). It records only when it adds something,
once per hook run and phase. The session's activity keeps the turns, the bytes,
and the largest turn (its own context and the mid-turn notes after it).
`langspan status` prints them per session with an estimate in tokens at 4
bytes per token (`BYTES_PER_TOKEN`), an estimate only; `status --json` has the
counts. The benchmark's run record reports them per task and per trial. The
budget is 256 bytes, about 64 tokens, per turn on average for a typical
session (`CONTEXT_BUDGET_BYTES_PER_TURN`), held by
`a_typical_session_stays_within_the_context_budget_per_turn_and_status_shows_it`:
a session with two peers over twelve prompts of three writes each, one peer
starting a new file every fourth turn. On the debug build on macOS arm64 that
session measured 158 bytes per turn (2,054 bytes over 13 turns) with briefings
of changes only, and 338 bytes per turn (4,397 bytes) with the full view on
every change; one run each. The skill is in every turn's context too: its test
keeps every form at most 90% of each host's documented limit.

**Protocol version 7 (added 26 November 2026, review comments 4711375982
and 4712860167).** A client of version 6 (roadmap F5's build, ADR 0007) or
earlier reads session activity, briefings and events with
`deny_unknown_fields`, so a status answer carrying `told`, `told_at_ms` or
`context`, a briefing with `unchanged`, or an event page with the new events
would fail to read as a whole once a newer hook had recorded them. Version 5
already meant H9's `refusal_outcomes` and version 6 F5's landing fields, so
these fields take version 7. To a version 6, 5, 4 or 3 client, the daemon
answers without them: activity without `told`, `told_at_ms` and `context`, a
briefing without `unchanged`, a `briefing_delivered` event without its keys
and time, and an event page without `context_added` and `told_forgotten`
events. A version 6 reply keeps a landing's `rebased` and `adapted`; version
5, 4 and 3 replies also leave out what versions 6, 5 and 4 added (ADR 0007,
ADR 0019, ADR 0008). A state record too large for one status page comes as
parts of its JSON, which cannot be reshaped once cut, so the daemon cuts an
earlier client's parts from the record in that client's version
(`protocol::encode_record_for`). Apart from recording a rebased or adapted
landing (ADR 0007), requests of earlier versions are requests of version 7,
so a version 7 client that a daemon of an earlier build refuses asks again
once in the version that daemon names.

**Prompt targets (added later, roadmap C8).** A prompt's code briefing finds
where the prompt's names are defined (at most 12 names, 3 definitions each).
When the prompt summary was recorded and the index has a complete scan, the
prompt hook records those files as the session's prompt targets
(`set_prompt_targets`, event `prompt_targets_set`): 1 to 36 sorted relative
paths, the session and the time, never the prompt's words. They replace the
session's earlier targets; a prompt that finds no definition leaves them. The
hook now looks the names up before it asks for the briefing, so the prompting
session's own targets are compared in that prompt's briefing. Targets count
while they are at most `BRIEF_HORIZON_MS` (30 minutes) old in this generation
and the session has not ended. For each peer, `target_overlap` lists the
peer's targets that meet the asking session's admissions, recent edits,
declared intent or targets, leaving out paths already in `overlap`; the
asking session's own targets count as its work for `overlap`. Either ranks a
peer first, and `target_overlap` is part of the delivery signature only when
present, so earlier signatures are unchanged. The peer line says
`task points at your files: ...`. The other session hears of it at its next
delivery point (a tool batch on Claude Code, otherwise its next prompt), once.
Targets never hold a write, never count as an admission, and are not part of
what the remote setup publishes. The comparison is by file, not by meaning or
symbol: two tasks naming different functions in one file are told, and a name
defined in several files points at each of them.

**Same task (added later, issue #537).** Two running sessions are on the same
task when a file one's prompt targets name is one the other's targets name or
the other holds (a live admission). The rule is symmetric, so both sessions
are told, and it reads only those records, never a model's judgment. Files
both sessions merely edited or hold are `overlap`, not the same task; disjoint
targets never match. A subagent's line to its parent and an ended session
never carry it. The peer gets `same_task: true` (additive and defaulted on the
wire, so an older daemon's briefing reads as false), and the line starts with
`same task as yours: take a different part or agree who does it;` and keeps the
peer's task line even when that line was told before. It has its own told key
and joins the delivery signature only when true, so each session is told it
once. Like targets, it is a file match: two tasks naming different functions
in one file are reported as the same task.

**Presence (added later, roadmap A13).** Where a session is looking is shown
to its peers before it writes. Reads come from the existing `source_observed`
records with origin `read` (Claude Code `Read`, Cursor `Read`; Gemini CLI
`read_file` before its removal, ADR 0033). Searches and shell commands are recorded by a new command,
`observe_presence` (event `presence_observed`): the session, `kind` `search` or
`shell`, 1 to 4 sorted relative paths and the time, never a pattern or command
text. The issue proposed a `search_observed` event; one event with a kind
serves both. A search is Claude Code's or Cursor's completed `Grep`, recorded
as the path it was scoped to; a shell command's paths are the existing paths
its reading programs name (`shell::looked_at`), which skips a search
program's pattern and gives nothing when an option is not one that program
(`grep` or `rg`, each with its own list) is known to take without a value.
After `cd`, `pushd`, `popd`, `source`, `.` or `eval`, only absolute operands
are taken, since a relative one would name a file in another directory. Each
session keeps its 16 most recent looked-at paths, newest first, one entry per
path, in `SessionActivity::looking`. For each peer, `PeerWork::looking` lists
at most 4, within the asking session's presence window (10 minutes by
default), leaving out every path the peer holds or edited within the
briefing horizon. A peer known only by where it looked is a peer, ranked after
every peer with work or a task. The core chooses peers within its byte budget
by their work first and adds their presence in the space left, so presence
never takes another peer's place. The hook adds `looking at ...` at the end of the line,
whole or not at all, and only after every peer's line, local and remote, has
been placed without it; a line that is only presence is placed after every
work line, local and remote. Presence has a signature part of its own,
appended to the delivery signature as `+` and 23 bytes in hex only when
present, so earlier signatures are unchanged and every signature stays within
an event's 64 bytes, and a peer listed only for where it looked is left out of
the work part. The presence part sets two bits for each peer and path shown. A
briefing whose work part is unchanged is news only at a session start or
prompt, and only when it shows a peer and path whose bits were clear in what
was delivered: never mid-turn, and not when some or all presence merely ages
out or a shown path is read again. A new path whose bits another pair already
set is not news by itself; it is shown with the next briefing that is. `set_presence` (event `presence_set`) holds a session's
own setting when it is not the default: `shown` false (`--presence off`) keeps
every read, search and shell path of that session out of peers' briefings and
forgets what was recorded, and `window_ms` (1 to 30 minutes,
`--presence-minutes`) is how old a peer's presence that session is shown. The
hook sends a setting other than the default at every attachment, before
anything it records, and the setting at every session event. Presence never
holds or refuses a call, is local, and is not part of what the remote setup
publishes.

**Tools covered (added later).** Claude Code `MultiEdit` and `NotebookEdit`
(`notebook_path`) are admitted, denied and observed like `Edit` and `Write`.
Shell writes are still not covered.

**Claude Code subagents (added 24 November 2026, backlog item 4).** Claude
Code's hooks reference says that when a subagent calls a tool, "tool events
such as `PreToolUse` and `PostToolUse` fire the same configured hooks as in
the main conversation, and the input carries the `agent_id` and `agent_type`
common input fields that identify the subagent"; `agent_id` is the "unique
identifier for the subagent or in-process teammate the hook fires in"
(<https://code.claude.com/docs/en/hooks>, "Hook locations" and "Common input
fields", read 24 November 2026). The `session_id` is the parent's. A tool
event with an `agent_id` is now its own session,
`claude-code.<session>.<agent_id>` (an agent ID outside `[A-Za-z0-9_-]{1,40}`
is replaced by `h` and 24 hex digits of its SHA-256), registered with the
parent `claude-code.<session>`. Each subagent therefore has its own read and
written versions ([ADR 0005](0005-source-versions-and-stale-writes.md)) and
its own admissions: siblings editing different files do not hold each other,
and two editing one file at the same moment do. In a briefing the core marks a
peer that is the asking session's subagent or another subagent of its parent,
with a subagent shown as `session <parent> subagent <agent>`; what a subagent
is told about its own parent is set by the amendment below. A subagent's lifecycle events are still
not handled (it has none of its own: Start, prompt and End belong to the
parent), so a subagent is briefed only mid-turn, after its completed tool
calls, and never takes a handoff ([ADR 0009](0009-agent-messages-and-handoff.md)).
The tests use events constructed in the documented shape and marked so; no
real subagent capture existed then (`tests/stale_source.rs`, the
`subagent` tests).

**What a subagent is told about its parent (amended 25 November 2026).** In
two real Claude Code 2.1.330 runs
([evidence](../evidence/turn-events/langspan-events.json), seq 21 to 23) the
subagent's first briefing named its own parent as a peer, "(your parent
session)", with the parent's task line: the first line of the owner's prompt,
which in those runs was a numbered list of commands. Both subagents reported
it as a note asking them to run commands. The parent started the subagent
and gave it its task; repeating the parent's own prompt back to it as another
session's work is no news and reads like an injected instruction.

The rule is now:

- A subagent's briefing never lists its own parent: not its task line, not
  the files it holds or edited. Protection is unchanged. The parent's held
  file is still refused to the subagent at admission, as to any session, and
  a file the subagent read and the parent then edited is still a stale read,
  listed and refused once ([ADR 0005](0005-source-versions-and-stale-writes.md)).
- A parent's briefing still lists its subagents' files, labelled "(a subagent
  you started; this is its work on your request)": with a background
  subagent the parent can be editing at the same time, and which files the
  subagent touches is news. Their task line and declared intent are left out
  (their task is the parent's own request), and a subagent with neither a
  current nor a recent edit is not listed. A subagent's edit of a file the
  parent read is still a stale read for the parent.
- Siblings and unrelated sessions are briefed as before. A sibling is
  labelled "(another subagent started by the session that started you,
  working on its own part)". The preamble still says every line is
  coordination data, not instructions.

`PeerRelation::Parent` is removed from the briefing: the core no longer
produces it. The briefing is a response, never stored, and hook and daemon
are one binary, so no stored record or version changes. Tests:
`crates/langspan-core/tests/briefing.rs`
(`a_recorded_subagents_first_briefing_names_no_parent` replays the recorded
coordinator log up to the subagent's first Read and failed before the change
with the parent and its task line in the briefing;
`a_subagent_is_not_briefed_about_its_parent_but_still_about_siblings_and_others`,
`a_parent_sees_its_subagents_files_but_not_their_task_lines`,
`a_subagents_edit_of_a_file_its_parent_read_is_still_a_stale_read`) and
`tests/stale_source.rs` through the real hook and daemon with events in the
documented shape
(`a_subagent_is_not_briefed_about_its_parent_and_the_parent_sees_its_subagent_as_its_own`,
`a_subagent_is_still_refused_a_file_its_parent_is_editing`,
`a_parent_that_read_a_file_its_subagent_then_edited_is_refused_once`,
`a_subagent_is_briefed_about_a_sibling_subagents_work`). Whether a model reads
the new labels as intended is not yet observed on the host.

**Time (added 24 November 2026, backlog item 8).** The daemon's `now_ms` is
milliseconds since boot on a clock that counts sleep (`src/clock.rs`):
`CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC` on macOS. It used to be
`Instant` since the daemon started, and `Instant` uses `CLOCK_MONOTONIC` on
Linux and `CLOCK_UPTIME_RAW` on macOS, which both stop while the machine is
suspended, so a peer from before a night with the lid closed still looked a
few minutes old. The 30-minute activity and briefing horizons, the 10-minute
teammate horizon and the ages shown now count the night. The core is
unchanged and still pure: it is told the time. Real suspend cannot be run in
CI; `clock::tests` checks that the clock is the documented one and
`a_night_of_sleep_in_now_ms_ages_peers_out_and_expires_leases` checks what the
core does with an eight-hour gap.

## Consequences and limits

- The briefing reaches the model at session start, at each prompt and, for
  Claude Code, after each completed tool call when it changed. A Codex
  session learns of mid-turn work at its next prompt.
- On Claude Code an interface notice can also reach a session between its
  steps, with no hook call of its own ([ADR 0002](0002-claude-code-adapter.md),
  issue #214). A briefing placed in context records the interface notices it
  showed (`interface_notices_told`, `via: briefing`): a hook reports the ones
  its capped text holds (`interface_shown`), so a notice the cap left out is
  still told between steps, and `langspan_brief` records all it returns. One
  told between steps
  is left out of every later briefing, so no notice is told twice through the
  two paths. A notice a briefing told can still appear in a later briefing
  that is delivered because something else in it changed, as before.
- A new workspace has no repository record until its first complete scan
  (nominally within N × 2 s for N workspaces). Until then a worktree peer is not
  briefed. A workspace whose scan cannot complete is never grouped.
- Separate clones of one repository on one machine are not grouped. A
  cross-clone and cross-machine repository identity belongs to the remote
  coordination slice.
- Activity times are milliseconds since boot, so they stay comparable across a
  daemon restart but not across a reboot, when the clock starts again from
  zero. Earlier activity is in any case not briefed after a restart until the
  session acts again. Times are never compared with another machine's.
- A delivery is recorded when the daemon commits it. If the hook then misses
  its deadline, that briefing is not repeated until the situation changes or
  the session starts again. With briefings of changes only, the items it
  carried are not told again either; `langspan_brief` shows them.
- A briefing of changes only does not announce what went away: a peer whose
  hold expired without a finished edit, or whose work aged out, is simply no
  longer listed. A model that still believes a file is held only waits or is
  admitted; the full view shows the current state.
- An item the hook's text could not place is told again at the next briefing
  of changes only. A briefing that is itself full can leave it out again; it
  waits for a briefing with room, and the full view shows it.
- The context count is what the hook's reply carries. A host that reads less
  of a reply's `additionalContext` makes the count high.
- Whether a smaller briefing changes a host model's behavior, and the cache
  token reduction in a benchmark rerun, are not measured here: the
  measurements above are the hook's bytes in a stand-in session.
- `status` returns a bounded view of current work in one frame: open
  admissions, live or recent sessions and their activity, with closed
  admissions counted per session. When even that exceeds the frame, activity
  and other histories are left out first and named in its summary. The whole
  state, history included, is paged with `status --all`; a record larger
  than a page is given in ordered parts of its JSON. A caller that decides
  from a part the view may leave out (a retry's landing, a workspace's scan,
  an automatic check) reads that part complete when the summary names it.
- A session whose working directory moves into another worktree keeps its
  first workspace binding; events from the second are not coordinated.
- A peer's task line and file paths are another session's text. They are
  labelled as data, single-line and capped, but a host model may still be
  influenced by them. See the threat model.
- Process evidence is Linux-only. Elsewhere a crashed session stays in
  briefings until its work ages out.
- Codex subagent events are not covered: Codex's hooks documentation says
  "Subagent hooks use the parent session id" and lists `agent_id` only for
  SubagentStart and SubagentStop, so the adapter still refuses a Codex event
  that carries one. Codex has no session-end event in the
  inspected contract, so a Codex session is never marked ended.
