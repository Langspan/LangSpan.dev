# Coordination policies

Each kind of coordination case has one policy
([ADR 0036](../adr/0036-coordination-policies.md)). `langspan policy WORKSPACE`
prints the table in effect, `--json` as rows, and the MCP tool
`langspan_policy` reads it:

| Case | Policy | Configurable |
| --- | --- | --- |
| `advisory`: briefings, peer notices, interface notices, break alerts, remote awareness | Advisory: informs, never holds a write | No |
| `conflict`: another session holds the file or an overlapping region now | `refuse` (default) or `wait` | Yes, and the wait's bound |
| `stale_base`: the file changed after this session last read it | Refuse | No |
| `unread_lines`: the edit would change lines another live session wrote, unread | Refuse | No |
| `commit_held`: a commit would take another live session's changes | Refuse | No |
| `landing_not_ready`: `land` without passing evidence on the exact tree (exit code 6) | Refuse | No |

One more pause is not in the table yet: a rename held once
([issue #213](https://github.com/Langspan/LangSpan.dev/issues/213),
[ADR 0035](../adr/0035-semantic-layer.md)). A Claude Code or Codex write
that renames or removes an exported name (deleting its file with Codex's
`*** Delete File:` removes every name in it), while another worktree of the
repository has uncommitted files that mention it, is refused once
("LangSpan: rename held once.", diagnostic coverage `held_rename`). The
reply names the worktree, its live sessions and the files, matched by name,
and suggests keeping the old name as a forwarding alias calling the new one
until those callers move. Repeating the write goes through: it is held once
per session, file and name (marks under `STATE/rename-held/`, removed after
a day). Without such callers, or when the index or coordinator cannot
answer, the write goes on to admission as usual, with a note in the
diagnostic. It is not configurable.

Only the policy for a current holder can be set, per workspace:

```sh
langspan policy set WORKSPACE conflict wait --wait-ms 1000 --request-id policy-1
langspan policy set WORKSPACE conflict refuse --request-id policy-2
```

`--wait-ms` is 0 to 2,000; `wait` without it waits up to 2,000 ms, and 0 is
`refuse`. `policy set` for any other case fails, naming the case and saying
it is fixed. The setting is local to this coordinator, survives restarts and
goes with the workspace when it is removed.

With `wait`, an edit of a file another session holds now waits for that
holder instead of being refused at once:

1. The hook asks the coordinator every 20 ms whether the holder's admission
   has ended.
2. The wait ends at the bound, or when half of the hook's remaining budget is
   left (about 750 ms at the default 1,500 ms budget), whichever is first.
3. A holder still there: the usual conflict refusal, which says how long the
   edit waited.
4. The holder gone: the hook compares each file with the version it saw
   before the wait. A changed file is refused as a stale source ("These files
   changed while this edit waited … for the session holding them."), naming
   the path and the holder. That refusal is given once: the retry is
   admitted. A file that can no longer be read (now a symlink, a directory
   or over the size bound) gets no decision, naming the path, since whether
   it changed is unknown.
5. Files unchanged: the hook asks again, with every usual check, and the
   edit is admitted.

When the holder writes the file, the wait ends in a stale-source refusal, not
an admission: the agent reads the file again instead of trying later. A wait
admits only when the holder's admission ends with the file unchanged, after a
refused permission prompt, a failed edit or the end of its turn. A write of a
file nobody holds never waits. If the coordinator stops answering during a
wait, the hook gives no decision, as when coordination is unavailable, and
logs `wait_unavailable`.

`langspan events` records each wait and what followed each refusal:

```json
{"type":"write_waited","session_id":"codex.…","workspace_id":"app","paths":["src/api.rs"],"holder_session_id":"claude-code.…","waited_ms":600,"result":"admitted","at_ms":…,"generation":1}
{"type":"refusal_settled","session_id":"claude-code.…","workspace_id":"app","kind":"stale_base","paths":["src/api.rs"],"refused_at_ms":…,"outcome":"retried_admitted","ms_to_next_admitted":11800,"at_ms":…,"generation":1}
```

`result` is `admitted`, `refused_conflict`, `refused_stale` or
`refused_unread`. A refused wait is also a `write_refused` event of its kind.
A conflict, stale or unread-lines refusal is settled by the session's next
admitted tool write or its stop: `retried_admitted` (the write names a
refused path, with `ms_to_next_admitted`), `moved_on` (other paths only) or
`stopped` (the turn stopped or the session ended first). A session keeps at
most 8 unsettled refusals, and a daemon restart leaves earlier ones
unsettled. Whether the agent asked its person is not inferred.

`benchmarks/coordination/holder_wait.py` compares `refuse` with `wait` on
the `same-file-current-holder` scenario as two arms, `langspan_refuse` and
`langspan_wait`, keeping every run in `runs.jsonl`; `--summarize DIR` prints
the stop rate, retries and elapsed time per arm with sample sizes.
