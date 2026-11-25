# ADR 0027: The end of a reply, the end of a tool batch, and Codex's first edit

**Status:** Implementation decision, 25 November 2026. Extends
[ADR 0002](0002-claude-code-adapter.md), [ADR 0004](0004-automatic-peer-briefing.md)
and [ADR 0005](0005-source-versions-and-stale-writes.md).

## Context

A source audit found three gaps in the events LangSpan registers:

- **Claude Code** sends `Stop`, `StopFailure`, `SubagentStop` and
  `PostToolBatch`; none was registered.
- **Codex** sends `Stop` and `SessionEnd`; neither was registered.
- **Codex reports no reads.** Its agent reads files through shell commands, so
  no hook says which version it saw, and every first Codex edit of a file was
  admitted with the base unknown.

An admission whose post-tool event never arrives stays open until its TTL
(30 seconds by default) ends it. Claude Code sends no post-tool event for a
call the owner refused at a permission prompt; that case was observed on
Claude Code 2.1.330 (evidence below). While it is open, peers are refused the
file.

### What the hosts document and send

Checked against the hosts' documentation on 25 November 2026 and against
Claude Code 2.1.330 and Codex 0.171.0 as installed.

| Host and event | Documented input beyond the common fields | When | Output honoured |
| --- | --- | --- | --- |
| Claude Code `Stop` | `stop_hook_active`, `last_assistant_message`, `background_tasks`, `session_crons` | The main agent finished responding. Not after a user interrupt; an API error sends `StopFailure` instead | `decision: "block"` and `additionalContext` both **keep the conversation going** |
| Claude Code `StopFailure` | `error` (one of `rate_limit`, `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `invalid_request`, `model_not_found`, `server_error`, `max_output_tokens`, `cloud_credential_error`, `unknown`), optional `error_details`, optional `last_assistant_message` (the rendered error text) | The turn ended on an API error | Ignored |
| Claude Code `SubagentStop` | `stop_hook_active`, `agent_id`, `agent_type`, `agent_transcript_path`, `last_assistant_message`, `background_tasks`, `session_crons` | A subagent finished | As `Stop`: output keeps the subagent going |
| Claude Code `PostToolBatch` | `tool_calls`: `tool_name`, `tool_input`, `tool_use_id`, `tool_response` (the serialized result the model sees) per call | Every call of a batch resolved, before the next model request | `additionalContext` with `hookEventName: "PostToolBatch"`, injected once before the next model call |
| Codex `Stop` | `turn_id`, `stop_hook_active`, `last_assistant_message` | The turn finished | `decision: "block"` starts a new continuation prompt. "Exit 0 with no output is treated as success" |
| Codex `SessionEnd` | `reason` (always `other`) | The main thread closed, was archived or deleted, or was idle and unopened for 30 minutes. Never for subagents | None; advisory. Timeout default 1 s, at most 3 s |
| Codex `SubagentStop` | `turn_id`, `agent_id`, `agent_type`, `agent_transcript_path`, `stop_hook_active`, `last_assistant_message` | A subagent finished | Not registered: see below |

Sources: [Claude Code hooks reference](https://code.claude.com/docs/en/hooks)
and [Codex hooks](https://developers.openai.com/codex/hooks) (sections
"Common input fields", "Matcher patterns", "Tool coverage", "Config shape",
"Stop", "SessionEnd", "SubagentStop"). Codex's documentation lists no hook for
a file read: its tool coverage table names shell commands, unified exec,
`apply_patch`, MCP tools and other local function tools. Reading a file is a
shell command.

The real Claude Code events (`fixtures/claude-code-2.1.330`) match the
documentation, and add three facts it does not state: `prompt_id` and
`effort` are present on every one of them; a subagent's `PostToolBatch`
carries its `agent_id`; and a parent's `Stop` arrives while a background
subagent it started is still running (`background_tasks` lists it).

## Decision

**Stop releases the reply's open admissions.** `Stop` and `StopFailure` (Claude
Code) and `Stop` (Codex) send a new core command, `session_stop`. It completes
every admission that is still active, in the current generation, not past its
deadline, held by that session, and made for a host tool call. It records a
`session_stopped` event naming them. Admissions asked for by hand (`langspan
admit`, `langspan_admit`) are not part of a reply and are left to their TTL.
An admission already past its deadline is left to the expiry sweep. A
session the coordinator has no record of is not registered just to be
stopped; the hook answers `no_session`.

Only that session's own admissions are released. A Claude Code subagent is a
session of its own (`claude-code.<session>.<agent_id>`), so a parent's stop
leaves a running subagent's admissions alone, as the capture requires.

**SubagentStop ends the subagent.** It releases the subagent's open admissions
as above, then ends that subagent's session. Its parent is unaffected. A
resumed subagent becomes active again with its next event, as any session does.

**StopFailure records the error type, nothing else.** The `error` value is
kept when it has the documented shape (lower-case letters and underscores, at
most 40 bytes) and recorded as `unknown` otherwise. `error_details` and
`last_assistant_message` are never read, so no error text, prompt or model
output is recorded. This is bookkeeping only: detecting that an agent ran out
of quota, and telling anyone, stays out of scope as
[ADR 0019](0019-person-facing-surfaces-and-usage-accounting.md) decides.

**Stops print nothing.** On Claude Code any output from `Stop` or
`SubagentStop` keeps the agent going, which would turn bookkeeping into an
extra model turn. The hook never prints for them, and never exits 2.

**PostToolBatch records a boundary and may carry news.** The hook records a
`tool_batch_observed` event: the session, how many calls the batch had, and
the operation IDs of up to 64 of them (the same IDs the tool events use).
Nothing else from `tool_calls` is read; tool inputs and results are not
recorded. This is the point where checks after a write batch will be started
later; nothing runs there now. It is the session's first registration if no
earlier event registered it. Then, as after a completed tool call, the
session is told what changed around it since it was last told, if anything
did, through `additionalContext`. The briefing's "changed" rule keeps the same
news from being delivered twice. A batch redelivered with the same calls is
the same request.

**Codex: Stop and SessionEnd.** `Stop` is handled as Claude Code's, with no
output (Codex documents that exit 0 with no output is success). `SessionEnd`
was already understood by the shared lifecycle code and is now registered,
with a 3-second host timeout, Codex's maximum for that event. A Codex event
that names an `agent_id` is refused: Codex gives a subagent's events the
parent's session ID, so a subagent's stop must not end the parent's reply.
Codex `SubagentStop` is not registered for the same reason.

**Codex's first edit of a file: a snapshot instead of a read.** When a Codex
`apply_patch` is admitted and the admission lists files with no recorded
version (`base_unknown`), the hook records the version it gave the admission
for each of them, with a new origin, `snapshot`. The admission's own answer is
unchanged: those files were not compared, and the diagnostic says so, adding
that they are now recorded as seen.

What this protects:

- A later edit of the file by that session is compared with the snapshot when
  the first edit **failed** (for example its patch context did not match), or
  when its post-tool report never arrived. Before, such an edit was unknown
  again; now a peer's change in between is refused once, naming the peer.
- The same holds when the first patch was admitted and its post-tool report
  succeeded: the session's own write then replaces the snapshot, exactly as
  before.

What it does not protect:

- **The first edit itself is still not compared.** The snapshot is taken at
  the moment of the admission, so there is nothing earlier to compare with. A
  peer's change between Codex's shell read and its first patch is not
  detected by LangSpan. Codex's patch context must still match the file for
  Codex to apply it, which catches some of these changes, not all.
- An edit refused for a conflict records no snapshot: the core decides a
  conflict before it looks at versions, and nothing is admitted.
- A shell write by Codex (its `Bash` tool) is handled by
  [ADR 0011](0011-shell-writes.md) and gets no snapshot.
- A snapshot is not a read. If Codex reads the file again through a shell
  command after a peer changed it, LangSpan does not know, and refuses that
  session's next edit once as stale. The refusal's text says the file changed
  after the session last read it; for a snapshot that means "last recorded as
  seen".
- Claude Code reports its reads, so it gets no snapshot: an edit of a file it
  never read stays unknown (`a_claude_edit_of_an_unread_file_is_not_given_a_snapshot`).

**Install, upgrade and doctor.** `hooks install` and the user-level `install`
register the new events for each host (Claude Code: `Stop`, `StopFailure`,
`SubagentStop`, `PostToolBatch`; Codex: `Stop`, `SessionEnd`) with no
matcher. Reinstalling over an install from an earlier version adds them and
replaces the existing entries in place, one LangSpan entry per event.
`hooks remove` and `uninstall` remove them. `doctor` names the registered
events an install lacks and says to run the install again. The Codex
`SessionEnd` timeout goes through the same per-host hook entry as every other
event, so a host that takes milliseconds would get them.

**Cursor and Gemini CLI get no end-of-reply event here.** Both adapters
([ADR 0032](0032-cursor-adapter.md), [ADR 0033](0033-gemini-cli-adapter.md))
were built from documentation and are not yet verified on a real host, so
this record registers none of their end-of-reply events (for Cursor, `stop`
and `subagentStop`, which ADR 0032 also leaves out). They keep the session
events their own records register. Adding them is a follow-up once a real capture of each
host shows what those events carry.

## Consequences

- An admission left open by a refused or abandoned call ends when the reply
  does, instead of up to 30 seconds later. A turn the owner interrupts sends
  no Claude Code `Stop` (documented), so its admissions still wait for the
  TTL. Codex's `Interrupt` event is not registered.
- A stop is not retried when the coordinator is unreachable: it is not kept in
  the observation spool ([ADR 0014](0014-observation-spool.md)), and the TTL
  ends the admissions instead.
- Three new event types (`session_stopped`, `tool_batch_observed`, and source
  observations with origin `snapshot`) and two new commands (`session_stop`,
  `observe_tool_batch`). A store written by this build does not open in an
  earlier one, as [ADR 0015](0015-version-tolerance.md) states for any new
  event; earlier stores open in this one. The protocol stays at version 3.
- An empty event list the owner kept in a settings file for an event LangSpan
  now registers (for example `"Stop": []`) is removed together with
  LangSpan's entry on `hooks remove`. This already held for the events
  registered before.

## Evidence and what is not verified

- Captured from real Claude Code 2.1.330 headless runs in a throwaway
  repository: every event listed above, including a real `StopFailure`
  (`model_not_found`), a subagent's `PostToolBatch` and `SubagentStop`, and a
  `Stop` while that subagent ran ([fixtures](../../fixtures/claude-code-2.1.330/README.md)).
- A second real run with this build's `hooks install claude-code --apply`
  ([evidence](../evidence/turn-events/README.md)): an `Edit` refused at the
  permission check left an open admission, and `Stop` released it; batch
  boundaries, the subagent's stop and its session's end were recorded.
- **Codex: no hook ran.** `codex exec` 0.171.0 in a new throwaway repository
  reported "Untrusted project-local configuration is disabled" and loaded no
  project hooks. Trusting the project (`codex exec --trust`) would write the
  owner's user-level Codex configuration, and no bypass flag was used, so
  Codex `Stop` and `SessionEnd` are tested only in their documented shape.
  Whether Codex accepts the empty output of a `Stop` hook, and how it treats
  a 3-second `SessionEnd` timeout, are not observed.
- Not observed on a host: `PostToolBatch` delivering a briefing to the model,
  a `StopFailure` other than `model_not_found`, a resumed subagent, and the
  Codex snapshot on a real Codex session.
