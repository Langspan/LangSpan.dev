# Person-touches per merged change

`scripts/evidence-report.py` counts how often a person stepped in on each
merged pull request of a GitHub repository, by week, and prints the result
as Markdown. It runs from a LangSpan source checkout; it is not part of the
`langspan` binary. It needs `python3` 3.9 or later and `gh`, signed in.

```sh
python3 -I -B scripts/evidence-report.py --repo OWNER/NAME --person LOGIN \
  --since 2026-11-01 --save touches.json > touches.md
python3 -I -B scripts/evidence-report.py --from touches.json --person LOGIN
```

`--person` names a login whose actions count, and can be repeated. Logins
are compared without regard to case, as on GitHub. Nothing else is a
person: bots, review apps and other logins are not counted. One touch is
any of these by a named person:

| Kind | What GitHub recorded |
| --- | --- |
| `merge` | The person is named as having merged the pull request. |
| `approval` | A review with state `APPROVED`. |
| `review` | Any other review. |
| `comment` | A conversation comment on the pull request. |
| `commit` | A commit authored by the person with no agent author (`noreply@anthropic.com`), other than a merge (headline starting `Merge `). |
| `needs-owner` | Once per pull request that has, or closes an issue in the same repository that has, the `needs-owner` label. A closed issue in another repository is not looked up. |

A review or comment whose body contains "Generated with [Claude Code]" was
posted by an agent through the person's account and is not counted. One an
agent posted without that line is counted, so the figure can only be too
high, never too low, for that reason.

The report has a table by ISO week of the merge (UTC) and one row per pull
request: merged changes, touches, touches per change and each kind's count.
Each count comes from those records alone; no text is read for meaning and
no model is asked. The report prints numbers, dates and the logins given,
in lowercase; it prints no title, comment or other repository text. Its
first lines state
that the counts reflect the whole process (LangSpan plus planning,
reservations and the merge queue), not LangSpan alone.

It reads the newest 500 merged pull requests (`--limit`, at most 1,000), and
says "the newest N only" when the limit was reached, plus up to 1,000
`needs-owner` issues. Both are GET requests through `gh pr list` and
`gh issue list`, with gh's own sign-in; the script reads no token. Reviews,
comments and commits are those gh returns for each pull request.

`--save FILE` writes what was read: for each pull request its number, merge
time, who merged it, labels, the issues it closes in the same repository,
and each review's, comment's and commit's author logins with whether it was
an agent's. It keeps no text.
`--from FILE` reports from that file without calling GitHub, so the same
file gives the same report. A file from another version of the script is
refused with "save it again with --save". It exits 2 when gh fails or a file
can't be read, saying which.
