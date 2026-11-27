# Refusals this month

`count` says how often LangSpan refused a write or a commit in one local
month. It says what LangSpan did, never what it is assumed to have saved, and
every answer carries the line "A refusal is not proof of avoided rework."

```text
$ ./target/release/langspan count --month 2027-03
March 2027: 17 refusals (9 conflict, 5 stale_base, 2 unread_lines, 1 commit_held)
By workspace: app 12, web 5
Retried and let through: 3 (the same session was admitted on the same files afterwards).
6 repeat(s) of a refusal by the same session, kind and files on the same day are not counted again.
A refusal is not proof of avoided rework.
```

Without `--month YYYY-MM` it counts the current local month. Months and days
are local: each day starts at its own local midnight, found from the system's
time zone rules for that date as `today` does, so a refusal at 02:30 UTC on
1 April is April in UTC and March in New York. Refusals are the
`write_refused` events of every workspace on this machine, read at query time;
nothing is stored for the count. A refusal is counted once per session,
workspace, kind and path set per local day, so a session refused again and
again for the same edit is one refusal; the others are reported as `repeats`.
`retried_through` counts refusals whose session was later admitted on the
same files, among its next 16 admissions after its last repeat; a held commit
has no admission and is never counted there.

`--json` prints `month`, `from_ms` and `to_ms` (the window, Unix
milliseconds), `refusals`, `by_kind` (`conflict`, `stale_base`,
`unread_lines`, `commit_held`, which add up to `refusals`), `by_workspace`
(at most 64, the rest in `omitted_workspaces`), `retried_through`, `repeats`,
`truncated` and, when there are any, `undated` (refusals recorded by a run
without a clock anchor, in no month). One count reads at most the newest
20,000 refusal records and says `truncated: true` when the month may reach
past them; the line in `today` and on the local page then ends with
"(truncated: counted from the newest 20000 refusal records only)". When the
month starts before the history a compaction kept, `retained_since_ms` gives
that cutoff and the line says "(incomplete: retention removed the records from
before ...)": the refusals of that part are gone, so the total is not the
month's complete count. A count is
never shown for a month whose local day starts could not all be read from the
time zone rules: `count` exits 1 and `today` and the local page say the count
could not be read. With the daemon down it exits 1 with "coordination
unavailable"; no earlier figure is kept or shown. The local page (`langspan dashboard`)
shows the same count for the current month, and its `/api/state` view
carries it as `refusals_this_month`.
