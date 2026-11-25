# ADR 0014: Observations kept while the coordinator is unreachable

**Status:** Implementation decision, 24 November 2026.

## Context

A hook never waits for the coordinator beyond its budget: when the daemon is
down, restarting or slow, the hook answers `unavailable` or `timeout` and the
host goes on. Before this, what a completed tool call would have reported was
then simply lost: an edit's PostToolUse, a read, a shell command's comparison.
Peers were not told, the event log had a gap, and the session's written version
was not recorded. Only a shell command whose host redelivered the same hook
recorded its kept outcome (ADR 0011), and hosts do not redeliver.

## Decision

**What is kept.** When a completed edit's report (PostToolUse or
PostToolUseFailure), a completed read, or a shell command's post-tool report
makes a call that does not reach the daemon or gets no answer in time, the
hook keeps an entry under `STATE/spool/`. An entry holds what the report needs
and nothing more: the request ID of the report (the existing deterministic
`wire_id`), the workspace ID and root, the host, host session and tool-use IDs,
the tool name, the input digest, the parent session of a subagent, and for an
edit its paths, result and the file versions (path and SHA-256) read when the
report failed; for a read, the versions read; for a shell command, nothing
else, since its outcome is in its run record (ADR 0011). No file content, no
command text, no prompt.

**When it is sent.** At the start of the next hook run for a tool call (any
session, any host sharing the state directory) that can connect to the daemon,
before the run's own event. A daemon that is plainly down (no socket, refused
connection) spends nothing. The check is the socket client's own nonblocking
connect, bounded by half of what is left of the hook's budget, so a daemon that
stopped accepting with its listen backlog full cannot hold the hook (test:
`a_coordinator_whose_backlog_is_full_does_not_hold_a_hook_past_its_budget`). Each entry is sent as the run that kept it would
have sent it, with the same request IDs, so whatever the daemon already
recorded is answered from its receipt and recorded once.

**Bounds:**

| Bound | Value |
| --- | --- |
| Tries per entry | 3, counted before sending: a run killed mid-send still spends one |
| Age | 1 hour, by the sleep-inclusive monotonic clock and the wall clock both; an entry from before a reboot is dropped |
| Entries | 200, the oldest dropped first |
| Bytes | 512 KiB in all, 64 KiB per entry |
| Sends per hook run | 20, within half of what is left of the run's budget |
| On failure | Stop at the first entry that does not reach the daemon |

Least-tried entries go first, then the oldest, so one that keeps failing does
not stand in front of the rest. An entry the daemon answers, even with a
refusal, is owed no longer.

**Superseded entries.** A newer edit report by the same session of the same
files replaces an older edit or read report of them; a newer read replaces
only an older read, since an edit also tells peers who changed the file. A
shell report is never replaced. The same holds for the run's own live report:
what it supersedes, and an entry for the very same report (a redelivered
hook), is dropped unsent, so a stale resend never goes out in front of the
live one.

**Concurrent hook runs** share the directory without a lock. Each entry is its
own file named by its request ID, written to a private temporary file and
renamed into place, so adds never lose each other. A try is counted only when
the entry still reads as this run read it; a run that loses that race skips
the entry. Two runs can still send the same entry at once; receipts make that
harmless.

**Against a restarted daemon** the restart has already ended the session's
generation and its admission. The resend's registration is answered from the
receipt of the original call, so the session stays from the earlier
generation: a resend does not make a session that may have finished look
present. What is recorded:

- the observation, as history: the change is in the event log, attributed to
  the session, with its admission `invalidated`;
- not the session's written or read version (recording one needs a current
  session), so its own next edit of the file is admitted uncompared
  (`base_unknown`) rather than refused as stale;
- no completion (the admission is already over);
- nothing in peers' briefings: a briefing names only what sessions did since
  the restart (ADR 0004).

Against a daemon that only stopped answering for a while, the same session is
current and the resend records the observation, the version and the
completion, as the original run would have.

Tests (`tests/spool.rs`, real daemon, real hook executable, real Git):
`an_edit_finished_while_the_coordinator_was_down_is_recorded_once_at_the_next_hook_run`
(daemon process really killed and started again),
`an_observation_older_than_an_hour_is_dropped_unsent`,
`an_entry_is_tried_three_times_at_most_and_each_try_is_counted_before_sending`
(daemon stopped with `SIGSTOP`, so connections are accepted and never
answered), `a_spooled_report_superseded_by_a_live_one_is_dropped_and_not_sent_again`,
`a_shell_commands_changes_found_while_the_coordinator_was_down_are_recorded_once`.
Unit tests in `src/spool.rs`:
`the_spool_is_bounded_in_entries_and_bytes_and_owner_only`,
`a_report_is_kept_once_and_a_newer_one_of_the_same_files_replaces_it`.

## Consequences

- A report lost while the daemon was down is recorded when the next tool-call
  hook run reaches it, within an hour. Lifecycle events (start, prompt, end)
  do not send what is owed; the next tool call does.
- A resend after a restart is history only; see above.
- The versions an edit report carries are read when the report fails, so a
  resend records what the edit left, not what is on disk later. A shell
  command's resend still reads its changed files' versions when it is sent,
  as a redelivered hook did before.
- A timed-out request was often slow, not lost. An entry that keeps timing out
  is dropped after its third try, whatever the daemon made of it.
- An edit report dropped as superseded leaves its admission to expire by its
  TTL (30 s by default) instead of being completed.
- The spool is cooperative state in the owner's state directory, like the
  shell run records: a same-user process can edit or delete it.
