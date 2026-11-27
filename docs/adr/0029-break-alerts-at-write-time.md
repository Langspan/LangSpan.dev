# ADR 0029: Break alerts at write time, with argument counts

**Status:** Implementation decision for launch track L5 of the
[roadmap](../roadmap.md), 25 November 2026. Extends
[ADR 0008](0008-derived-source-index.md) (interface notices) and uses the edit
placement of [ADR 0012](0012-region-admission.md). Tested with the real daemon,
indexer and hook executables; not yet observed on a host. Cursor
(behind a captured fixture) and quoted-heredoc shell writes are built
(roadmap B9, issue #28). Gemini CLI writes were built too; Gemini CLI support
was removed on 26 November 2026 ([ADR 0033](0033-gemini-cli-adapter.md)), and
the Gemini CLI parts below record the decision as it was made; `NotebookEdit` and real-host runs remain planned.
A call to a function a peer renamed or removed is told at the write too
(roadmap B10, issue #29); a change that keeps the argument count still is not
(see "Not now" below).

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
and `Write`, and Codex `apply_patch`. The check runs before the write only on
a host that gives the model the context of a pre-tool reply
(`Operation::pre_tool_context`): an alert the model never reads would still
use up its notice. The other two launch hosts, and shell writes, were added
later (roadmap B9, issue #28); they are built and tested with constructed
events, not yet observed on a host:

- **Gemini CLI** (removed, ADR 0033) reads no context on `BeforeTool` but adds `AfterTool`
  context to the tool's result ([ADR 0033](0033-gemini-cli-adapter.md)).
  At `BeforeTool` of an admitted `replace` or `write_file`, the hook finds
  the write's calls against the file as it is then and keeps them
  (`Operation::breaks_after`); at the matching `AfterTool` of a write that
  succeeded, it takes them, asks the daemon, and gives the alert in that
  reply's `hookSpecificOutput.additionalContext`. The model reads it with the
  write's result, as on Claude Code. The calls are kept in one owner-only
  file per tool call under `STATE/break-calls/`, named by a digest of the
  operation ID, holding the call sites only (file, line, name, argument
  count), never the code; it is removed when the `AfterTool` run reads it,
  whatever the result, and one whose `AfterTool` never came is removed a day
  later. A write that fails is not checked, so it uses up no notice. A
  Gemini `run_shell_command` is not checked.
- **Cursor** documents `additional_context` on `preToolUse`
  ([ADR 0032](0032-cursor-adapter.md)), but its adapter is built from
  documentation and its `Write` input has not been seen from a real Cursor.
  The check is wired for its `Write` (the whole `content`), and switched on
  only by `cursor::CAPTURED_WRITE` naming a captured `preToolUse` `Write`
  fixture in the repository. None exists, so Cursor writes are not checked
  yet; a test fails when a captured `pre-write.json` appears without that
  constant naming it.
- **A shell write with known new content** on Claude Code and Codex: a
  command that is exactly `cat > PATH <<'EOF'` or `cat <<'EOF' > PATH` (the
  delimiter in single or double quotes), its body, and a closing line that is
  exactly the delimiter with nothing after it. A quoted delimiter means the
  shell expands nothing, so the body is the file's new content and is checked
  like a `Write`. Any other shape (`>>`, `<<-`, an unquoted delimiter, a
  second heredoc or command, text after the closing line) is not checked.
  This reading admits nothing: the command's admission is unchanged
  ([ADR 0011](0011-shell-writes.md); a heredoc gets no decision before it
  runs until roadmap B5).

`NotebookEdit` is not checked: a cell's lines are not the file's lines, and
the cell's language is not in its input.

**What the write puts in the file.** The hook works out the file's text after
the write and the lines that hold new text, from the host's input and the
file on disk:

| Tool | New text |
| --- | --- |
| `Write`, Codex `Add File`, Gemini `write_file`, a quoted heredoc | The whole content; every line is checked. |
| `Edit`, `MultiEdit`, Gemini `replace` | The replacements applied in order in memory. Each must match exactly once (all places for `replace_all`); otherwise the host refuses it and nothing is checked. The lines checked are those holding text a replacement wrote. The size of the result is worked out before each replacement is applied; past the parser's bound, the file is not checked, so a `replace_all` cannot build text larger than the bound. |
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
`a, b int` is two; a TypeScript `this:` parameter and a Java receiver
parameter (`Orders this`) are never passed; a Java `T... items` is variadic.
A Java constructor's count is checked against `new T(...)`. A C# parameter
with a default is optional and `params T[] items` is variadic; a C# extension
method (`this Cart cart`) has no count, since `cart.F()` and `Carts.F(cart)`
are both member calls with different counts. A C# constructor's count is
checked against `new T(...)`; a target-typed `new(...)` names no type. Two
definitions of one name in one file, a signature cut at its 240-byte bound, or
any parameter shape not listed has no count, and its notice cannot alert.

**Which notices.** The core decides, from its own state and the index:

- the notice is in the session's workspace or in another worktree of its
  repository (added later, issue #194), from this daemon generation, within
  the briefing horizon (30 minutes);
- the session that last reported editing the file (the notice's `by_session`,
  a correlation as in ADR 0008) is another session, registered in this
  generation, not reported exited or ended;
- for another worktree's notice, Git decides the two rules above instead
  (added later, issue #217; see "Committed, unmerged changes" below);
- the session has not already been alerted to this notice;
- the change is to a function or method: a signature change with a count, a
  rename or a removal;
- the index still says so. For a signature change, it holds that new
  signature for that name in that file. For a rename, it holds the new
  name's signature in that file and no definition of the old name anywhere
  in the workspace. For a removal, it holds no definition of the name
  anywhere in the workspace, so a function moved to another file, or one of
  that name elsewhere, is not told as gone. Absence counts only while the
  index reads every supported file at its current content
  (`blind_in_scope` zero): a name moved into a file still waiting, partial
  or too large is not gone, so no rename or removal is told until then. The daemon looks up only names
  that appear in both the write and such a notice, and a renamed function's
  new name, usually none.

**Another worktree's change (added later, issue #194).** A notice of another
worktree of the session's repository is judged in that worktree's index,
with that worktree's coverage, and never in the session's own: the session's
checkout still defines the old name until the branches meet, and that is the
break being told. A worktree whose index has no complete scan contributes
nothing; the answer's coverage is still the asking workspace's. The alert's
line names the worktree after the file, for example
`which was renamed to "load_config" in "src/config.rs" in worktree "/repos/app-tree" by claude-code.alice 1m ago`.
Without this, a session in another worktree learned of the change only from a
later prompt briefing, after it had written the call (issue #194).

**Committed, unmerged changes (added later, issue #217).** Once the changing
session commits on its own branch, the change is still on its way to every
other worktree of the repository: it reaches a peer only when the peer's
branch takes the commit. A session that ends after committing, as a session
that finishes its task does, leaves the change exactly as much in flight. So
for a notice of another worktree, the daemon asks Git where the change
stands against the asking session's checkout:

- **Uncommitted**: the notice's file differs from its worktree's `HEAD`
  (staged counts as uncommitted). The rules above apply unchanged: within the
  horizon, while the session is live.
- **Unmerged**: the file equals its worktree's `HEAD`, and the newest commit
  of that `HEAD` touching the file (`rev-list -1 HEAD -- PATH`) is not an
  ancestor of the asking checkout's `HEAD` (`merge-base --is-ancestor`). The
  change counts at any age and after its session has ended, still within this
  daemon generation and once per notice per session.
- **Merged**: that commit is an ancestor of the asking checkout's `HEAD`. The
  notice is retired for that session: it is never alerted to, whatever the
  other rules say.

The core decides from these answers; it never runs Git. Before looking up any
name, the daemon asks `standing_questions` for the notices of other worktrees
that the write's calls name and that could count if committed, at most 8 per
write, each workspace's newest first, and gives Git at most 250 ms for them
all, at most five read-only processes per notice under
`--no-optional-locks`. The working file is hashed as raw bytes
(`hash-object --no-filters`), so no repository-configured filter runs; a
symbolic link or other non-regular file is not hashed. When that hash differs
from the `HEAD` blob and the file holds CRLF line endings, LangSpan reads at
most 1 MiB of the file and the blob (`cat-file blob`) and compares them with
CRLF turned into LF, the normalization Git applies under `core.autocrlf` or a
`text`/`eol=crlf` attribute. A file that differs from its commit only in line
endings therefore counts as committed. A notice Git does not answer for (a timeout, a failure, a file
that never was in a commit) keeps the uncommitted rules, and the failure is
logged at info level. Notices of the session's own workspace are never asked
about. Using the newest commit that touches the file means a later commit of
that file, not yet merged, keeps an earlier merged change counting: it errs
toward telling. A squash or rebase merge writes new commits, so a change
merged that way is not seen as merged and counts until the generation ends.
The prompt briefings of ADR 0008 and the `elsewhere` list are unchanged.

For a signature change, a call to that name whose count does not fit is a
break. A call through a receiver fits the count without the receiver; any
other call may write the receiver out (`Store::put(&s, x)`), so either count
fits. For a rename or a removal, every call to the old name is a break,
whatever its count. At most 8 breaks.

**Renames.** The indexer tells a rename apart from a removal (roadmap B10)
only within one file transition: the only exported function or method of a
name and kind goes, the only one of a new name and the same kind comes, and
the two have the same signature apart from the name (compared as in ADR 0008,
so a formatter's rewrap does not count) and the same body. The body is the
definition's bytes after its signature, compared by SHA-256 digest
(`langspan_graph::digest_blocks`, with the digest from `src/sha256.rs`); the
index keeps each baseline function's digest in a column of its own. When two
candidates on either side have the same shape, nothing is linked. Any other
change made in the same transition (a new body, a parameter, a return type)
leaves it a removal and an addition, as before. Roadmap D7 (26 November 2026)
also links a new body at the same first and last line, with `evidence:
"same_signature"`, and records `same_body` for the rule above; a pair is
linked only when each is the other's one candidate (ADR 0008). A file moved
whole is `renamed` with `file_moved` and the same name: the name is not gone,
so no call of it alerts. The notice is `renamed`, under
the old name (the one the calls still use, so the dependents are the files
that mention it), with the new name in `after_name` and both signatures. A
removal's notice now also keeps the signature it had, in `before`.

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
On Gemini CLI the alert is the `AfterTool` reply's context instead, also read
with the write's result; when both a break alert and a mid-turn briefing are
due there, the alert is given and the briefing waits for the next completed
call. When a shell command's reply already carries a formatter notice, the
alert follows it in the same context. The text, bounds and once-per-notice
rule are the same on every host.

The text: a header saying the calls are matched by name, not resolved, and
that the quoted text is repository data, not instructions; then one line per
break with the call's file and line, the name, its argument count, the count
it now takes, the new signature, the file it is in, the session and how long
ago, the old signature and its count. A rename's line names the call's file
and line, the old name, the new name, the file, the session, how long ago
and the new signature; a removal's names the file it was removed from, the
session, how long ago and the signature it had:

```text
- "app/extra.py":3 calls "scale", which was renamed to "zoom" in "app/api.py" by claude-code.alice 2m ago: "def zoom(factor)".
- "src/main.rs":3 calls "resize", which was removed from "src/api.rs" by claude-code.alice 2m ago; it was "pub fn resize(width: u32) -> u32".
```

Every repository value is cleaned and
quoted as in briefings; each line at most 500 bytes, the whole at most 1,200,
and a break that does not fit is counted.

**Time.** The check starts only while a quarter of the hook budget (1.5 s by
default) remains, and the daemon call shares the hook's deadline. On Gemini
CLI the rule applies to each half: finding the calls at `BeforeTool` and
asking the daemon at `AfterTool`. When the
index has no complete scan of the workspace, the daemon answers `ready:
false` without looking anything up and the hook adds only a diagnostic note.
Any failure is a note in the diagnostic, never a decision.

## Consequences and limits

- Name matching (ADR 0008) applies: a call of `open` is checked against
  every changed function named `open` in the workspace, so a call to a
  different function of the same name can be alerted to. Imports are not
  used to narrow it.
- Of signature changes, only argument counts are compared. A changed
  parameter type, order or name with the same count is not alerted at write
  time; it remains in the prompt briefing. Keyword arguments are counted as
  arguments, not matched to names. See "Not now" below for the same-count
  case.
- A rename is linked only when the signature apart from the name is
  unchanged and the body or the lines are too, in one file. A rename across
  files is a removal from one and an addition to the other, and its removal
  does not alert while the new file defines the old name. A recursive
  function's body names the function, so renaming it changes the body: it is
  a rename only while its lines stay the same (D7). A function without a body
  (a Rust trait method declaration) is linked only by its lines. Calls to a removed name may reach a library
  function of the same name that the index does not hold; such a call is
  alerted to once per notice, like any other name match.
- A rename is told under the old name in the prompt briefing too (`function
  "scale" was renamed to "zoom"`), and on the dashboard. An index from before
  this build is kept; its baselines have no body digests, so the next change
  of each file is told as before, a removal and an addition.
- A definition added or removed under a name that still has others
  (`definition_added`, `definition_removed`, ADR 0008) changes no signature
  a call was written against, so it carries no counts and never alerts at
  write time; it is told in the prompt briefing. A signature change among
  several same-named definitions has no single new signature, so it does
  not alert either (test
  `a_second_same_named_definition_coming_or_going_is_not_a_changed_signature_at_the_write`
  in `tests/break_alerts.rs`).
- A notice attributed to no session (the file was changed outside any
  reported edit) does not alert, nor does an uncommitted one whose session
  ended. A committed, unmerged change of another worktree alerts after its
  session ended (issue #217).
- Calls already in the file are checked only for `Write` and `Add File`,
  which rewrite every line.
- A notice is recorded in one physical workspace, as in ADR 0008. Its write
  check reaches sessions in other worktrees of the same repository on this
  machine (issue #194), never another repository or another machine. Each
  break of the `check_calls` answer has an optional `worktree`, the root of
  the worktree whose change it is, when that is not the asking session's.
- Two new commands (`check_calls`, answered by the daemon and never
  recorded; `acknowledge_interface`), one new event (`interface_alerted`), an
  outcome, and two optional fields on an interface change (`before_arity`,
  `after_arity`). A store written by this build does not open in an earlier
  one ([ADR 0015](0015-version-tolerance.md)); earlier stores open in this one,
  and their notices have no counts. Roadmap B10 adds one more optional field,
  `after_name`, the change label `renamed`, and `change` and `after_name` on
  each break of the `check_calls` answer, whose `after_arity` is now optional.
  The protocol stays at version 3.
- Issue #213 adds `renamed` to `check_calls`, behind the `rename_holds`
  capability ([ADR 0037](0037-protocol-capabilities.md)): the names a write
  renames or removes. The answer's `callers` list, per other worktree of
  the repository whose index has a complete scan, its files that differ
  from its `HEAD` and mention a name (a name match, at most
  `NOTICE_DEPENDENTS` per worktree and name, at most 8 worktrees), with the
  worktree's live sessions. Nothing is recorded. A daemon without the
  capability is asked about the calls only. The Claude Code and Codex
  file-tool hooks send `renamed` before admission. When the answer lists
  callers, the hook holds the write once per session, file and name (coverage
  `held_rename`), and the reason suggests a forwarding alias; repeating the
  write goes through. With no callers, or when the check fails, the write goes
  on to admission
  ([ADR 0035's amendment](0035-semantic-layer.md)).
- Not observed on a host: the alert reaching a real Codex or Cursor model, or a model changing its call because of it. A Claude Code
  alert reaching the model has been seen once and is not yet in the
  repository's evidence.
- On Gemini CLI the calls are found against the file before the write and
  judged against the notices after it, so a notice that arrives between the
  two hook runs can alert the write. Two calls on one file in flight at once
  can share a call identity (ADR 0033): the first `AfterTool` to arrive then
  takes the calls the later `BeforeTool` kept, and the other finds none.
- The heredoc reading covers one exact form. `cat << EOF` with an unquoted
  delimiter, `tee`, `printf > file` and scripts are not checked.

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
Recorded in [the roadmap](../roadmap.md), row B10, whose renames and removals
are built; this part is not. The test
`a_signature_change_that_keeps_the_count_is_not_told_at_the_write` in
`tests/break_alerts.rs` holds the current behavior.

## Tests

`tests/break_alerts.rs` drives the real daemon, observer, indexer and hook
executable in a temporary Git repository: a peer's Rust signature change is
alerted at another session's `Write` with both counts, signatures, file, line
and peer; a call with the new count gets nothing; the same notice is not
repeated to that session while a third session is still told; the changing
session is not told; only `interface_alerted` is recorded, never the code;
Python, Java and C# through a Claude Code `Edit`, TypeScript and Go through Codex patches
(`Update File` and `Add File`); a Gemini CLI `replace` gets the alert with
its `AfterTool` result once, not when it fails, not for the new count and not
again at the same session's next write, and leaves nothing kept
(`a_gemini_replace_is_told_of_a_broken_call_with_its_result_once`); a Cursor
`Write` is checked only once `cursor::CAPTURED_WRITE` names a captured
fixture
(`a_cursor_write_is_checked_only_once_a_captured_write_fixture_is_present`);
a Claude Code `Bash` quoted heredoc writing a broken call into a `.py` file
gets the alert, in both orders of redirection and heredoc, while the new
count and an unquoted delimiter get none
(`a_quoted_heredoc_writing_a_broken_call_is_told_before_it_runs`); and a workspace with no scan gets no alert,
a diagnostic note and no delay beyond the budget; and when three notices'
breaks do not all fit one alert, the ones left out are told at the next
write with the same calls, and the ones shown are not repeated.
`crates/langspan-graph/tests/calls.rs` covers call extraction and parameter
counts for every indexed language, `crates/langspan-core/tests/call_breaks.rs`
the fit rule and the bounds, and `src/breaks.rs` placing replacements and
hunks, refusing a replacement whose result would pass the parser's bound,
and checking a multiline call when only its argument line is written.
For roadmap B10, `tests/break_alerts.rs` also has a Python rename recorded as
`renamed` with its new name, whose old-name call gets one line naming the new
name and file while a call of the new name gets nothing, once per session
(`a_call_to_a_peers_renamed_function_is_told_the_new_name_once`); a Rust rename
with a new body over other lines recorded as a removal, whose call gets one
"removed" line, once
(`a_rename_with_a_changed_body_is_a_removal_and_a_call_to_it_is_told_once`);
a Go function moved to another file, not told as removed
(`a_removed_name_still_defined_elsewhere_is_not_told`); and a TypeScript type
change that keeps the count, not told
(`a_signature_change_that_keeps_the_count_is_not_told_at_the_write`).
`crates/langspan-graph/tests/renames.rs` covers rename detection in the four
languages, a renamed method and arrow function, a changed body at the same
lines as `same_signature`, a changed body over other lines, parameter or
return type staying a removal and an addition, and ambiguous, undigested and
type pairs never linked; `crates/langspan-core/tests/renames.rs`
the shape of a `renamed` change; `src/graph.rs` a rename through a real index
pass and an index from before body digests; and `src/breaks.rs` the rename and
removal lines and their bounds.
For issue #194, `tests/graph.rs` has two sessions in two worktrees of one
repository: Alice renames `parse_config` in a linked worktree and saves, and
Bob's `Write` in the main checkout that calls `parse_config` gets the alert
naming the rename and the worktree, once
(`a_peers_rename_in_another_worktree_is_told_at_the_write_that_calls_the_old_name`).
`crates/langspan-core/tests/elsewhere.rs` covers judging the change in its
own worktree's index: no alert without that index or with a partial one,
none for a session of another repository, and no worktree named for a
session of the worktree that made it.
`src/shell.rs` covers the heredoc forms read and refused, and
`src/cursor.rs` that a captured Cursor `Write` fixture is named by
`CAPTURED_WRITE`.
