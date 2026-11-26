# ADR 0028: Lines another session wrote, protected from an unread write

**Status:** Implementation decision for launch track L1 of the
[roadmap](../roadmap.md), 25 November 2026. Extends [ADR 0005](0005-source-versions-and-stale-writes.md)
(source versions). Builds on the line journal of
[ADR 0026](0026-commit-guard-and-commit-mine.md) and the edit placement of
[ADR 0012](0012-region-admission.md). Tested with the real daemon and hook
executables; not yet observed on a host. Shell writes on all four hosts and
the whole-content writes of Cursor and Gemini CLI added 26 November 2026
(issue #23).

## Context

A scenario bench (scenario 2c) had a Claude Code session write a line, and two seconds later a Codex
session's `apply_patch` replace that line. LangSpan admitted it, and the first
session's line was lost. ADR 0005 compares a write with the version the
writing session last read or wrote. Codex reports no reads, so its first edit
of a file had no version to compare and was admitted as `base_unknown`.

The fix is to stop a patch or shell write that would change another live
session's recent lines unseen, from a per-checkout journal of which session
wrote which lines. Two choices follow. Only a change to existing lines is
stopped, never an addition beside them. A write is stopped at most once.

LangSpan already has that journal. The commit guard (ADR 0026) records, for
each write a session makes, the keys of the lines it added and removed, on
this machine only. This decision reads it. It keeps no second record of who
wrote what.

## Decision

**Which lines an edit changes.** At admission the hook works out the lines of
the file as it is on disk that the edit would remove or replace. It uses the
same bytes as the admission's base version.

| Tool | Changed lines found as |
| --- | --- |
| Claude Code `Edit`, `MultiEdit` | The replacements applied in memory as the host applies them, then the two versions compared line by line. A replacement must match exactly once, or every match with `replace_all`. The comparison is Myers' algorithm with the common head and tail cut first, within at most 256 edits, 4,000,000 line comparisons and 64 MiB of compared bytes; past any of these, the middle is one change. |
| Claude Code `Write`, Cursor `Write`, Gemini CLI `write_file` | The new content compared with the file in the same way. Only lines that differ are changed, so a whole-file write that keeps another session's line exactly does not replace it. |
| Codex `apply_patch`, `Update File` | Each hunk placed as ADR 0012 places it (one match under its loose comparison). Its `-` lines are the changed lines. A hunk that matches only once its trailing empty lines are dropped (the host's retry) is not located. |
| A shell write whose targets the parser names (`> path`, `>> path`, `tee path`, `sed -i path`), through the `Bash`, `Shell` and `run_shell_command` pre-tool hooks | Not located: the command may change any line of each target. |
| Anything else (`NotebookEdit`, Codex add, delete or move), an edit that does not apply exactly, or a file over 1 MiB or not UTF-8 | Not located: the edit may change any line. |
| Any edit, with less than half the hook budget left | Not looked up: no written lines are sent, so nothing is refused for them. |

An insertion changes no existing line.

**Whose lines they are.** The hook reads the workspace's journal entries for
the file that sessions other than this one wrote in the last 30 minutes
(`LINE_HORIZON_MS`, wall clock, as the journal keeps time). It passes them to
`journal::written_lines` with the current content. Each changed line whose
content one of those writes added is that writer's.

- A line of fewer than three letters or digits (a blank line, a lone brace,
  `);`) is too common to name anyone and is skipped.
- A recent entry whose lines the journal does not know (its "before" was not
  kept, or it changed more than the journal keeps) names its writer, with no
  lines, whenever the edit changes any line.

The hook sends the result in `admit_tool` as `overwrites`: per writer, the
path, the line ranges in the current file, and the age. It sends at most 64
entries of at most 32 ranges each, newest writer first.

**The rule, in the core.** After the conflict check and the stale-source
comparison, an entry refuses the edit when all of these hold:

- the file exists;
- the asking session has **no recorded version** of it;
- the writer is another session, registered, not ended and not known to have
  exited;
- the entry is within the 30 minutes.

The result is `unread_lines` and nothing is admitted. The hook denies, naming
each writer, the lines and their age, and asks the agent to read the file.

A session with a recorded version of the file is governed by ADR 0005 only, so
a change is never refused twice. Like a stale base, the refusal records the
current version as learned from a refusal: after a reported read, or on the
retry, the edit is admitted. Codex reports no reads, so for it the retry after
the refusal is the read. A Claude Code subagent is a session of its own
(ADR 0004), so its lines are protected from its parent and its siblings.

The same tool call delivered again (same tool-call ID, same input) is the same
request and gets the same refusal (added 25 November 2026 from the launch
validation). Each written-lines entry carries its age, measured when the hook
builds the request, so two deliveries differ in it; the coordinator compares
a retry without those ages (`Command::same_request`), and the hook denies a
replayed `unread_lines` or `stale_base` outcome as it did the first time.
Before, the second delivery was answered `request_id_conflict` and got no
decision (test
`a_redelivered_pre_tool_event_after_a_refusal_for_unread_lines_is_refused_again`).
A replayed conflict is still no decision (ADR 0003): its holder may be gone.

**Taking over an ended session's lines** (added 25 November 2026, issue
#161). A session that ends before it commits leaves its journaled lines
credited to it, and nothing else can take them: `status --mine` and
`commit --mine` of the session that continues the task show none of them.
`langspan adopt SESSION` and the MCP tool `langspan_adopt` move them,
explicitly:

- The core decides. `Adopt { session_id, from }` is refused unless `from`
  is a registered session of the adopter's workspace that has ended (an
  end report, or its process known to have exited; the same test as "not
  ended and not known to have exited" above), is not the adopter, and was
  not adopted by another session. It records one `session_adopted` event
  with both sessions, the workspace, the time and the reason, and the state
  keeps one adoption per ended session (`State::adoptions`). An adoption the
  adopter already made is answered again without a new event. The answer
  names `from`'s registered subagents that have not ended (`live`, at most
  64, else `session_live`): ending a session records its end alone, so a
  subagent can still be writing.
- A retry of an adoption's request ID is decided again rather than answered
  from its receipt, because the adapter rewrites the journal from the
  answer. If `from` has resumed since, the retry is refused with
  `session_live`; otherwise it carries the current `live` list.
- The adapter then credits the journal entries of `from` and its ended
  subagents to the adopter, in place, skipping those of a `live` subagent
  and its own subagents: each entry keeps its file name and
  modification time, so it expires when it would have and a retry finds
  nothing left to move. The journal stays the one record of who wrote which
  line; the event records the transfer. A replay after a restart gives the
  same adoption, and the journal on disk already holds the new owner.
- An adopter that ends after its adoption is recorded, but before the
  adapter credits the lines, leaves them under the session it adopted, which
  cannot be adopted again. So the answer also names `earlier`: every
  session whose adoptions lead to `from`, of the same workspace, at most 64
  (`adoption_chain` refuses more). The adapter credits their entries too,
  and `live` covers their subagents as well. A session in that chain that
  has resumed since its adoption is left out, along with the sessions its
  own adoptions lead to: what it writes again is its own.
- A subagent of `from` (or of a session in `earlier`) that another session
  adopted is that session's to credit, even when its adapter stopped before
  rewriting the journal. The answer names those subagents (`kept`, at most
  64, else `adoption_chain`), and the adapter skips their entries and their
  own subagents' entries, as for `live`. The other session's retry then
  credits them. An adoption the adopter itself made of such a subagent is
  not in `kept`: its entries move with the rest.
- Entries are rewritten whole, so a line both `from` and a third session
  wrote is the adopter's and that session's: a `mixed` file stays mixed, and
  the third session's lines stay its own.
- The reason is shown, never required: a message `from` sent the adopter
  (addressed to it, or a handoff it took), else the same task line. The
  person or agent decides.
- At a session start, the hook names to the coordinator the sessions whose
  journaled lines are still among the checkout's changes from `HEAD`
  (`adopt::left_sessions`, at most 32 files compared, within half the hook's
  time). The briefing names the ended, unadopted ones of the workspace, at
  most four, with their task lines, and suggests `langspan adopt`. Only the
  start hook names them, so a session is told once per start.

**Shell writes and whole-content hosts** (added 26 November 2026, issue
#23). A shell command whose targets the parser names is admitted through the
same path as an edit, with no stated edit, so each target may change any line
and every recent line of another live session in it is named. This holds on
all four hosts' shell tools, and is refused once as any edit is. Cursor
`Write` and Gemini CLI `write_file` state their content as an edit, as Claude
Code `Write` does, so a whole-file write that keeps another session's lines
exactly, or only appends, refuses nothing. Three choices the issue left open
were taken at their smallest:

- **Heredocs stay with B5.** `cat > path <<'EOF'` is outside the parsed write
  set, so it gets no decision (`shell_observing`) and is never refused for
  lines. Comparing its body as a `Write` waits until B5 admits heredocs.
- **Appends are not told apart.** `>> path` and `tee -a path` are shell
  writes like `>`: they may change any line and are refused once over a
  peer's recent lines, though they only add. Telling them apart needs the
  parser to report the redirection per target.
- **Too little time is no check.** With less than half the hook budget left
  the hook reads no journal, compares no lines and sends no `overwrites`, so
  the admission is decided without them. Before, an edit it could no longer
  place counted as changing every line, and lack of time could refuse it.
  Daemon down, journal unreadable or a shell snapshot incomplete already gave
  no decision for lines; the last says why in its coverage
  (`shell_unobserved`, "workspace snapshot incomplete").

## Consequences and limits

- **An adoption cannot be undone or passed on while the adopter is live.**
  Another session can adopt the adopter's work once the adopter has ended.
  A session whose death LangSpan never observed (no end report, no process
  observation) counts as live and cannot be adopted; its owner can end it
  with the host, or commit the files by path.
- **A write the ended session's hook journaled after the adoption** stays
  credited to it. An ended session writes nothing, so this needs a host that
  reports an end and then keeps writing; `langspan adopt` again moves it.

- **Lines are known by content, as the journal keys them.**
  - Identical lines cannot be told apart: a changed line counts as another
    session's when that session added any line with the same bytes, wherever
    it is.
  - A moved line is neither added nor removed, so it is not anyone's.
  - A line several sessions added names each of them.
  - A line a session added and a later write removed and then put back is
    still the first session's.
  - The trivial-line rule keeps the commonest false matches out.
- **Unknown-lines entries name their writer.** A write the journal holds with
  unknown lines names its writer for any changed line of the file, for 30
  minutes. Shell writes are journaled line by line when the version before
  was kept (ADR 0026).
- **The journal survives a daemon restart, and so does the protection.**
  Entries are owner-only files under the state directory, kept for a week and
  bounded by the journal (ADR 0026). This rule reads only the last 30
  minutes.
- **Prevention is one refusal with the reason, before the tool runs.** An
  agent that ignores it and retries is admitted, as with ADR 0005.
- **The changed lines are the hook's reading of the host's input at
  admission.**
  - A replacement or hunk that matches more than once gives no location.
  - Where two shortest comparisons exist (one of two identical adjacent
    lines removed), the comparison picks one. The resulting text is the same
    either way.
- **The rule matters most for Codex.** Claude Code's own read check refuses
  an `Edit` or `Write` of a file it has not read, and runs before LangSpan's
  hook ([evidence](../evidence/m6-stale-source/README.md)). The rule also
  covers Claude Code after a read dropped out of the 128 kept versions. It is
  applied to every host alike.
- **What the agent is shown.** Line numbers are the file's current ones, and
  no line text is relayed.
- **The refusal is logged like a stale-source refusal** (a `source_observed`
  event with origin `refused`), so the dashboard lists it among stale
  refusals.
- **Protocol.** It stays at version 3, which is unreleased. `admit_tool`
  gains an optional `overwrites`, and there is one new outcome,
  `unread_lines`. Golden vectors: `fixtures/admit-overwrites-request-v3.json`
  and `fixtures/unread-lines-response-v3.json`. Adoption adds the `adopt`
  request, the `adopted` outcome, the `session_adopted` event, an optional
  `left` on `brief` and an optional `left_work` on the briefing, all at
  version 3 (`fixtures/adopt-request-v3.json`,
  `fixtures/adopted-response-v3.json`,
  `fixtures/left-work-brief-request-v3.json`).
- **The start hook reads the whole workspace journal** to find ended
  sessions' work, as the commit guard does for a commit: at most the
  journal's 4,096 entries, then at most 32 files compared with `HEAD` within
  half the hook's time.
- **Latency.** The comparison and the journal read run in the hook. The
  journal keeps each workspace's entries in one directory per path, and the
  hook reads only the directories of the files the call writes, so its cost
  grows with that file's entries, not with the journal (the commit guard
  and `commit --mine` still read the whole workspace's, at most the
  journal's bound of 4,096). Entries an earlier version kept in one flat
  directory are moved into the per-path directories on the first access,
  under the same names and modification times, so they stay readable and
  expire as before. Measured on an Apple M6 (12 cores, 16 GB, macOS 27.2),
  release build, with the launch validation's growth run (one session
  writing 50 files in turn, 4,300 turns, 300 PreToolUse Write calls per
  row): p50 / p95 went from 5.2 / 7.1 ms at 0–300 entries, 18.5 / 20.7 at
  1,000–1,300 and 60.3 / 66.3 at 4,000–4,300 to 3.2 / 3.6, 4.0 / 4.6 and
  6.8 / 10.7. What remains grows with the written file's own entries (about
  86 per file at the end of that run).

Tests:

- `tests/line_ownership.rs`: real daemon and hook executables, a real Git
  checkout, shell commands run between their hooks on the Codex, Claude Code,
  Cursor and Gemini CLI shell tools, and constructed Cursor `Write` and Gemini
  CLI `write_file` events.
- `src/hook.rs` (`journal_lookup`): no written lines past the locating
  deadline.
- `crates/langspan-core/tests/line_ownership.rs`: the rule and its bounds.
- `src/lines.rs`: the changed lines and the journal lookup.
- `tests/adopt.rs` and `crates/langspan-core/tests/adoption.rs`: adoption,
  with a killed session's host, a third session's lines, a daemon restart and
  the MCP tool; `src/journal.rs`: crediting entries in place.
