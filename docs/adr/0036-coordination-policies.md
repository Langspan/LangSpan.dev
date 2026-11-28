# ADR 0036: A coordination policy for each kind of refusal

**Status:** Implementation decision for roadmap B28
([issue #171](https://github.com/Langspan/LangSpan.dev/issues/171)),
26 November 2026. Collects the refusals of
[ADR 0005](0005-source-versions-and-stale-writes.md) (stale source),
[ADR 0012](0012-region-admission.md) (holds),
[ADR 0026](0026-commit-guard-and-commit-mine.md) (commit holds),
[ADR 0028](0028-line-ownership.md) (unread lines) and
[ADR 0007](0007-check-evidence-integration-and-landing.md) (landing) under one
table, and adds an opt-in wait for a current holder. Tested with two sessions
through the real hook executables and a real daemon; not yet observed on a
host. The comparison run with real agents is scripted and has not been run.

## Context

LangSpan answers each coordination case in one of two ways: it informs, or it
refuses. Nothing stated which answer each case gets, or why. A refusal costs
the agent a turn and can make it stop and ask its person, while some holds end
milliseconds later. A conflict refusal returned at once, even when the holder's
admission was about to end.

A one-day measurement on 25 November 2026 (one model, agents with the LangSpan
skill) found 18 refused edits, nearly all `stale_base`; all were retried and
admitted, with a median of about 12 seconds to the admitted retry, and no
session stopped. It held no refusal for a conflicting hold. An external
coordination tool reported 4 of 13 sessions stopping after hard refusals
against 1 of 216 with queued edits, in different scenarios: directional only.
Neither run's records are in `docs/evidence/`.

## Decision

**One table.** Each case has one policy. `langspan policy WORKSPACE`, the MCP
tool `langspan_policy` and `docs/usage.md` show the table in effect.

| Case | Policy | Configurable | Why |
| --- | --- | --- | --- |
| `advisory`: briefings, peer notices, interface notices, break alerts, remote awareness | Advisory | No | A name match or a teammate's summary is not proof; the write may be the fix |
| `conflict`: another session holds the file or an overlapping region now | Refuse (default) or wait | `refuse` or `wait`, and the wait's bound | The holder may end in milliseconds, or never |
| `stale_base`: the file changed after this session last read it | Refuse | No | The edit was planned against content that is gone |
| `unread_lines`: the edit would change lines another live session wrote, unread | Refuse | No | Ownership of those lines is not this session's to decide |
| `commit_held`: a commit would take another live session's changes | Refuse | No | Same as above, for a commit |
| `landing_not_ready`: `land` without passing evidence on the exact tree (exit code 6) | Refuse | No | Landing is the point of the evidence |

**The default stays `refuse`.** The issue proposed `wait` as the conflict
default. Today's refusal stays the default so that behavior is unchanged
until measurement shows waiting helps; `langspan policy set WORKSPACE conflict
wait` turns it on. A refusal of every other kind stays a refusal: `policy set`
for any other case fails with `policy_fixed`, naming the case and its fixed
policy, and no setting admits a write whose base changed during a wait.

**The setting.** `langspan policy set WORKSPACE conflict refuse|wait
[--wait-ms N] --request-id ID`. `--wait-ms` is 0 to 2,000; without it, `wait`
waits up to 2,000 ms, and 0 is `refuse`. The setting is a `policy_set` event,
per workspace, local only. It is derived state on replay and kept in a
retention snapshot; removing the workspace removes it.

**Wait and revalidate.** In a workspace set to `wait`:

1. The admission that meets a holder records no refusal; the hook's wait does.
2. The hook keeps the versions it sent with the admission and polls the
   coordinator every 20 ms with a read (`holders`) that records nothing.
3. The wait ends at the workspace's bound, or when half of the hook's
   remaining budget is left, whichever is first, so the final admission
   keeps its time. The hook's budget is 1,500 ms by default (`--budget-ms`,
   1 to 4,000), so a default wait lasts at most about 750 ms.
4. If a holder still holds a path, the hook refuses with today's conflict
   refusal, which says how long it waited.
5. If the holders are gone, the hook reads each path's version on disk
   again. A path whose version changed is refused as a stale base, naming the
   path and the holder. As with an admission's stale refusal, the version the
   session was told about becomes its known version, so the retry is
   admitted: one refusal per change. A path whose version can no longer be
   read (now a symlink, a directory or over the size bound) is not known to
   be unchanged: the hook gives no decision (`unknown`, naming the path) and
   records no `write_waited`. That is a stopgap (review follow-up #244); a
   refusal of its own would need a new `WaitResult` here.
6. Otherwise the hook asks again under a second request ID. The coordinator
   repeats every check, including the session's own known versions, so a
   stale or unread-lines refusal can still follow. An admission holds under
   that second ID; the pre-tool run leaves an owner-only mark under
   `STATE/waited/` so that its post-tool run observes and completes it.

When the holder writes the file, the wait ends in `stale_base`, not an
admission. The wait admits only when the holder's admission ends with the
file unchanged: a refused permission prompt, a failed edit, a stop. In the
other case the gain is the reason: the agent is told to read the file again,
not to try later. If the coordinator cannot be reached during a wait, the
hook gives no decision, as for unavailable coordination at any other time,
and logs `wait_unavailable`. A wait never extends past the hook's deadline.

**What followed.** Two events record it, in `langspan events`:

- `write_waited`: `session_id`, `paths`, `holder_session_id`, `waited_ms` and
  `result` (`admitted`, `refused_conflict`, `refused_stale`,
  `refused_unread`). A refused wait is also a `write_refused` of its kind.
- `refusal_settled`: each write refusal (`conflict`, `stale_base`,
  `unread_lines`) is settled by the session's next admitted tool write or its
  stop. `outcome` is `retried_admitted` (the write names a refused path, with
  `ms_to_next_admitted`), `moved_on` (other paths only) or `stopped` (the
  turn stopped or the session ended first). A session keeps at most 8
  unsettled refusals; a daemon restart drops them unsettled. `asked_person`
  would come only from a host event that reports it, and no host reports one
  today, so no record carries it; it is not inferred.

This builds the settle step that roadmap H9
([issue #82](https://github.com/Langspan/LangSpan.dev/issues/82)) shares. H9's
window and its outcomes (`adapted`, `retried_unchanged`, `waited`,
`abandoned`) extend `refusal_settled`.

**Mixed versions.** A hook of an earlier build in a workspace set to `wait`
refuses at once, and that refusal is not recorded, since the coordinator
leaves recording to the wait. The table is a new request, which an earlier
coordinator refuses; the hook then refuses at once.

## Consequences

- Behavior is unchanged unless the owner sets `wait`.
- A wait costs a hook up to half its budget, only on a held file; an
  unrelated write never waits.
- A comparison of `refuse` against `wait` on the `same-file-current-holder`
  scenario is scripted as two arms, `langspan_refuse` and `langspan_wait`, in
  `benchmarks/coordination/holder_wait.py`. It keeps every run and reports
  the stop rate, retries and elapsed time per arm with sample sizes. Its
  stand-in mode checks the harness only; no real-agent run is retained yet.

## What would change each default

- `conflict` becomes `wait` by default when the comparison run shows fewer
  stops (or fewer turns lost) under `wait` without worse integrated results,
  on a retained sample of both arms.
- The 2,000 ms bound rises only with a hook budget above 4,000 ms; it is cut
  to fit the budget either way.
- `stale_base`, `unread_lines` and `commit_held` would become a wait only with
  a design that revalidates the agent's plan, not only the file, which no host
  supports today.
- An advisory case would become a refusal only with evidence beyond a name
  match.

## Not now

Queuing edits across turns, waiting longer than one hook call, waiting on any
refusal other than an active hold, and policies for remote peers.
