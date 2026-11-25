# ADR 0029: Break alerts at write time, with argument counts

**Status:** Implementation decision for launch track L5 of the
[roadmap](../roadmap.md), 25 November 2026. Extends
[ADR 0008](0008-derived-source-index.md) (interface notices) and uses the edit
placement of [ADR 0012](0012-region-admission.md). Tested with the real daemon,
indexer and hook executables; not yet observed on a host.

## Context

When session A changes an exported signature, ADR 0008 tells the sessions
whose files mention the name at their next prompt. Until then, session B can
go on writing calls in the old shape. Roadmap rows B9 and B10 ask for a
check of the write itself: name the calls in the write that a teammate's
change makes wrong, with the old and new argument counts. Refusing the write
for this would take the tool away, which LangSpan does not do: a name match is not proof, and the write may be the fix.

## Decision

**Where.** At `PreToolUse` of a write, after the write is admitted (a denied
write does not land, so it is not checked): Claude Code `Edit`, `MultiEdit`
and `Write`, and Codex `apply_patch`. `NotebookEdit` and shell writes are not
checked. The check runs only on a host that gives the model the context of a
pre-tool reply (`Operation::pre_tool_context`): an alert the model never
reads would still use up its notice. Of the other two launch hosts, Gemini
CLI reads no context on `BeforeTool` ([ADR 0033](0033-gemini-cli-adapter.md)),
so its writes are not checked. Cursor documents `additional_context` on
`preToolUse` ([ADR 0032](0032-cursor-adapter.md)), but its adapter is built
from documentation and its `Write` input has not been seen from a real
Cursor, so its writes are not checked until a real capture shows what that
input holds.

**What the write puts in the file.** The hook works out the file's text after
the write and the lines that hold new text, from the host's input and the
file on disk:

| Tool | New text |
| --- | --- |
| `Write`, Codex `Add File` | The whole content; every line is checked. |
| `Edit`, `MultiEdit` | The replacements applied in order in memory. Each must match exactly once (all places for `replace_all`); otherwise the host refuses it and nothing is checked. The lines checked are those holding text a replacement wrote. The size of the result is worked out before each replacement is applied; past the parser's bound, the file is not checked, so a `replace_all` cannot build text larger than the bound. |
| Codex `Update File` | Each hunk placed where its context and removed lines match exactly one place, under region admission's loose comparison; the lines checked are the added ones. A hunk with no place, or only added lines, means the file is not checked. |

Files in the four indexed languages only, at most 8 per write, each at most
1 MiB (the parser's bound).

**Calls.** `langspan-graph` parses that text with Tree-sitter and returns each
call: the called name without its path or receiver, its line, the lines the
whole call spans, its argument count, and whether it is written through a
receiver or module (`a.f(x)`). A call is checked when any line it spans holds
new text, so a write that changes only an argument line of a call written
over several lines checks that call.
Macro arguments in Rust are tokens and are not parsed. A call with a spread
or splat argument (`f(*args)`, `f(...xs)`, `F(xs...)`) has no count and is
skipped. At most 64 calls go to the daemon in one read-only command,
`check_calls`.

**Parameter counts.** When the indexer produces an interface notice
(ADR 0008), each changed signature also gets its parameter count, parsed from
the signature text with a stub body: the minimum (parameters without a
default, not variadic), the maximum (none when a parameter is variadic) and
whether the first parameter is an explicit receiver (Rust `self`, Python
`self` or `cls`). Python `*` and `/` markers are not parameters; a Go
`a, b int` is two; a TypeScript `this:` parameter is never passed. Two
definitions of one name in one file, a signature cut at its 240-byte bound, or
any parameter shape not listed has no count, and its notice cannot alert.

**Which notices.** The core decides, from its own state and the index:

- the notice is in the session's workspace, from this daemon generation,
  within the briefing horizon (30 minutes);
- the session that last reported editing the file (the notice's `by_session`,
  a correlation as in ADR 0008) is another session, registered in this
  generation, not reported exited or ended;
- the session has not already been alerted to this notice;
- the change is a signature change of a function or method with a count;
- the index still holds that new signature for that name in that file. The
  daemon looks up only names that appear in both the write and such a notice,
  usually none.

A call to that name whose count does not fit is a break. A call through a
receiver fits the count without the receiver; any other call may write the
receiver out (`Store::put(&s, x)`), so either count fits. At most 8 breaks.

**Once per notice per session.** When a break is found the daemon records an
`interface_alerted` event naming the notices, before it answers; an alert it
cannot record is not given. It names only the notices the alert shows at
least one break of: the daemon lays the alert out with the same function the
hook renders it with (`breaks::shown_notices`), so a notice whose breaks were
all left out for room stays to be told at a later write. A notice is identified by its file transition
(ADR 0008), so a later change of the same function is a new notice. A call
that fits does not use the notice up. The prompt briefing of ADR 0008 is
separate: being told at a prompt that a file you work on mentions a changed
name does not stop the write-time alert, which names the call.

**Delivery.** The alert is `hookSpecificOutput.additionalContext` on the
`PreToolUse` reply, with no `permissionDecision`: the host's own permission
flow and the write are unchanged. Both hosts document this field for
`PreToolUse` ([Claude Code hooks reference](https://code.claude.com/docs/en/hooks),
[Codex hooks](https://developers.openai.com/codex/hooks), checked 25 November
2026). Claude Code places it "next to the tool result", read at the next model
request, so the model reads the alert together with the result of the write
it concerns: computed before the write lands, read just after. Nothing reaches
the model before the write except by refusing it, which this does not do.

The text: a header saying the calls are matched by name, not resolved, and
that the quoted text is repository data, not instructions; then one line per
break with the call's file and line, the name, its argument count, the count
it now takes, the new signature, the file it is in, the session and how long
ago, the old signature and its count. Every repository value is cleaned and
quoted as in briefings; each line at most 500 bytes, the whole at most 1,200,
and a break that does not fit is counted.

**Time.** The check starts only while a quarter of the hook budget (1.5 s by
default) remains, and the daemon call shares the hook's deadline. When the
index has no complete scan of the workspace, the daemon answers `ready:
false` without looking anything up and the hook adds only a diagnostic note.
Any failure is a note in the diagnostic, never a decision.

## Consequences and limits

- Name matching (ADR 0008) applies: a call of `open` is checked against
  every changed function named `open` in the workspace, so a call to a
  different function of the same name can be alerted to. Imports are not
  used to narrow it.
- Only argument counts are compared. A changed parameter type, order or name
  with the same count, a removed function, and a rename (a removal in
  ADR 0008) are not alerted at write time; they remain in the prompt
  briefing. Keyword arguments are counted as arguments, not matched to names.
  See "Not now" below for the same-count case.
- A notice attributed to no session (the file was changed outside any
  reported edit), or to a session that ended, does not alert.
- Calls already in the file are checked only for `Write` and `Add File`,
  which rewrite every line.
- Notices stay within one physical workspace, as in ADR 0008.
- Two new commands (`check_calls`, answered by the daemon and never
  recorded; `acknowledge_interface`), one new event (`interface_alerted`), an
  outcome, and two optional fields on an interface change (`before_arity`,
  `after_arity`). A store written by this build does not open in an earlier
  one ([ADR 0015](0015-version-tolerance.md)); earlier stores open in this one,
  and their notices have no counts. The protocol stays at version 3.
- Not observed on a host: the alert reaching a real Claude Code or Codex
  model, or a model changing its call because of it.

## Not now: signature changes that keep the argument count

A call could also be alerted when a signature changes without changing its
argument count: a parameter's type changes, or two parameters
swap places. LangSpan does not, for now. Deciding whether such a call still
fits needs the types of the call's arguments, and the index has names and
signature text only, not types at a call site (one-hop, name-level graph,
[ADR 0020](0020-source-graph-depth-after-launch.md)). Without them the only
rule available is "this name's signature changed, so every call to it is
suspect". Matched by name, that alerts every call of every same-named
function in the workspace, including the calls that are already right, and
uses up the notice on the first such write. The prompt briefing of ADR 0008
already names the changed signature, before and after, to every session
whose files mention it. This is reconsidered with the source-graph depth of
ADR 0020: when call sites resolve to their definition and argument types are
known, a same-count change can be checked as exactly as a count is now.
Recorded in [the roadmap](../roadmap.md), row B10.

## Tests

`tests/break_alerts.rs` drives the real daemon, observer, indexer and hook
executable in a temporary Git repository: a peer's Rust signature change is
alerted at another session's `Write` with both counts, signatures, file, line
and peer; a call with the new count gets nothing; the same notice is not
repeated to that session while a third session is still told; the changing
session is not told; only `interface_alerted` is recorded, never the code;
Python through a Claude Code `Edit`, TypeScript and Go through Codex patches
(`Update File` and `Add File`); a Gemini CLI `replace` with the same
broken call gets no alert and no check; and a workspace with no scan gets no alert,
a diagnostic note and no delay beyond the budget; and when three notices'
breaks do not all fit one alert, the ones left out are told at the next
write with the same calls, and the ones shown are not repeated.
`crates/langspan-graph/tests/calls.rs` covers call extraction and parameter
counts for the four languages, `crates/langspan-core/tests/call_breaks.rs`
the fit rule and the bounds, and `src/breaks.rs` placing replacements and
hunks, refusing a replacement whose result would pass the parser's bound,
and checking a multiline call when only its argument line is written.
