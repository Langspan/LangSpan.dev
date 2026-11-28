"""Person-touches per merged change, as a Markdown report (#529).

Usage:
  evidence-report.py --repo OWNER/NAME --person LOGIN... [--since DATE]
                     [--limit N] [--save FILE]
  evidence-report.py --from FILE --person LOGIN...

Reads the repository's merged pull requests with `gh pr list` and the
`needs-owner` issues with `gh issue list` (GET requests through gh's own
sign-in; this script reads no token). `--save FILE` keeps what was read, in
the recorded form below, and `--from FILE` reports from such a file without
GitHub, so a report can be reproduced exactly.

A person-touch is one of these, by a login given with `--person`:
  merge      GitHub names the person as having merged the pull request
  approval   a review with state APPROVED
  review     any other review
  comment    a conversation comment on the pull request
  commit     a commit authored by the person with no agent author, not a merge
  needs-owner  once per pull request that has, or closes an issue that has,
             the `needs-owner` label
A review or comment whose body carries AGENT_MARKER was posted by an agent
through the person's account and is not a touch. Bodies are only searched
for the marker: the recorded form keeps no text, and the report prints only
numbers, dates and the given logins.

Exit status: 0 report printed, 2 the input couldn't be read.
"""

import argparse
import json
import subprocess
import sys
from datetime import datetime, timezone

VERSION = 1
AGENT_MARKER = "Generated with [Claude Code]"
AGENT_EMAILS = {"noreply@anthropic.com"}
NEEDS_OWNER = "needs-owner"
MAX_LIMIT = 1000
# A saved report of MAX_LIMIT pull requests is far below this.
MAX_RECORD_BYTES = 64 * 1024 * 1024
KINDS = ["merge", "approval", "review", "comment", "commit", "needs-owner"]
PR_FIELDS = "number,mergedAt,mergedBy,labels,closingIssuesReferences,reviews,comments,commits"
CONFOUNDERS = (
    "These counts reflect the whole process (LangSpan plus planning, "
    "reservations and the merge queue), not LangSpan alone. The controlled "
    "comparison is #172."
)


class ReadError(Exception):
    pass


def login(actor):
    return (actor or {}).get("login") or ""


def agent_text(body):
    return AGENT_MARKER in (body or "")


def normalize(pulls, needs_owner_issues):
    """The recorded form of `gh pr list --json PR_FIELDS` output."""
    recorded = []
    for pr in pulls:
        commits = []
        for commit in pr.get("commits") or []:
            authors = commit.get("authors") or []
            commits.append(
                {
                    "authors": sorted({login(a) for a in authors if login(a)}),
                    "agent": any(a.get("email") in AGENT_EMAILS for a in authors),
                    "merge": (commit.get("messageHeadline") or "").startswith("Merge "),
                }
            )
        recorded.append(
            {
                "number": pr["number"],
                "merged_at": pr["mergedAt"],
                "merged_by": login(pr.get("mergedBy")),
                "labels": sorted(label["name"] for label in pr.get("labels") or []),
                "closes": sorted(i["number"] for i in pr.get("closingIssuesReferences") or []),
                "reviews": [
                    {"author": login(r.get("author")), "state": r.get("state"), "agent": agent_text(r.get("body"))}
                    for r in pr.get("reviews") or []
                ],
                "comments": [
                    {"author": login(c.get("author")), "agent": agent_text(c.get("body"))}
                    for c in pr.get("comments") or []
                ],
                "commits": commits,
            }
        )
    recorded.sort(key=lambda pr: (pr["merged_at"], pr["number"]))
    return {"version": VERSION, "needs_owner_issues": sorted(needs_owner_issues), "pulls": recorded}


def touches(pr, people, needs_owner_issues):
    """Each kind's count of person-touches on one recorded pull request."""
    count = dict.fromkeys(KINDS, 0)
    if pr["merged_by"] in people:
        count["merge"] = 1
    for review in pr["reviews"]:
        if review["author"] in people and not review["agent"]:
            count["approval" if review["state"] == "APPROVED" else "review"] += 1
    count["comment"] = sum(1 for c in pr["comments"] if c["author"] in people and not c["agent"])
    count["commit"] = sum(
        1
        for c in pr["commits"]
        if not c["agent"] and not c["merge"] and people.intersection(c["authors"])
    )
    if NEEDS_OWNER in pr["labels"] or set(pr["closes"]) & set(needs_owner_issues):
        count["needs-owner"] = 1
    return count


def week(merged_at):
    """The ISO week, in UTC, a pull request merged in: 2026-W48."""
    when = datetime.strptime(merged_at, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    year, number, _ = when.isocalendar()
    return f"{year}-W{number:02d}"


def per_change(total, merged):
    return f"{total / merged:.2f}" if merged else "-"


def row(label, merged, count):
    total = sum(count.values())
    cells = [label, str(merged), str(total), per_change(total, merged)]
    return "| " + " | ".join(cells + [str(count[k]) for k in KINDS]) + " |"


def render(record, people, source):
    people = set(people)
    weeks = {}
    changes = []
    for pr in record["pulls"]:
        count = touches(pr, people, record["needs_owner_issues"])
        entry = weeks.setdefault(week(pr["merged_at"]), [0, dict.fromkeys(KINDS, 0)])
        entry[0] += 1
        for kind in KINDS:
            entry[1][kind] += count[kind]
        changes.append((pr, count))
    head = "| {} | merged | touches | per change | " + " | ".join(KINDS) + " |"
    rule = "|" + " --- |" * (4 + len(KINDS))
    lines = [
        "# Person-touches per merged change",
        "",
        f"Source: {source}. People: {', '.join(sorted(people))}.",
        "",
        CONFOUNDERS,
        "",
        "## By week (UTC)",
        "",
        head.format("week"),
        rule,
    ]
    grand = dict.fromkeys(KINDS, 0)
    for label in sorted(weeks):
        merged, totals = weeks[label]
        lines.append(row(label, merged, totals))
        for kind in KINDS:
            grand[kind] += totals[kind]
    lines += [row("all", len(changes), grand), "", "## By change", "", head.format("pull request"), rule]
    for pr, count in changes:
        lines.append(row(f"#{pr['number']} ({pr['merged_at'][:10]})", 1, count))
    return "\n".join(lines) + "\n"


def gh(args):
    try:
        out = subprocess.run(["gh", *args], capture_output=True, text=True, timeout=300)
    except FileNotFoundError as err:
        raise ReadError("gh is not on PATH; install the GitHub CLI and run `gh auth login`") from err
    if out.returncode != 0:
        raise ReadError(
            f"gh {' '.join(args[:2])} failed: {out.stderr.strip()}; "
            "check the repository name and `gh auth status`"
        )
    return json.loads(out.stdout or "[]")


def fetch(repo, since, limit):
    search = ["--search", f"merged:>={since}"] if since else []
    pulls = gh(["pr", "list", "--repo", repo, "--state", "merged", "--limit", str(limit), *search, "--json", PR_FIELDS])
    issues = gh(
        ["issue", "list", "--repo", repo, "--state", "all", "--label", NEEDS_OWNER, "--limit", str(MAX_LIMIT), "--json", "number"]
    )
    return normalize(pulls, [i["number"] for i in issues]), len(pulls) >= limit


def load(path):
    try:
        with open(path, "rb") as file:
            data = file.read(MAX_RECORD_BYTES + 1)
    except OSError as err:
        raise ReadError(f"cannot read {path}: {err.strerror}") from err
    if len(data) > MAX_RECORD_BYTES:
        raise ReadError(f"{path} is over the {MAX_RECORD_BYTES}-byte bound; check what wrote it")
    try:
        record = json.loads(data)
    except ValueError as err:
        raise ReadError(f"{path} is not JSON: {err}") from err
    if not isinstance(record, dict) or record.get("version") != VERSION:
        raise ReadError(f"{path} is not a version {VERSION} record; save it again with --save")
    return record


def main(argv):
    parser = argparse.ArgumentParser(prog="evidence-report.py")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--repo", help="OWNER/NAME to read with gh")
    source.add_argument("--from", dest="path", help="a file written by --save")
    parser.add_argument("--person", action="append", required=True, help="a login whose actions are touches")
    parser.add_argument("--since", help="merged on or after this date (YYYY-MM-DD)")
    parser.add_argument("--limit", type=int, default=500, help=f"newest merged pull requests read, at most {MAX_LIMIT}")
    parser.add_argument("--save", help="also write what was read to this file")
    args = parser.parse_args(argv[1:])
    if not 1 <= args.limit <= MAX_LIMIT:
        parser.error(f"--limit must be from 1 to {MAX_LIMIT}")
    if args.since:
        try:
            datetime.strptime(args.since, "%Y-%m-%d")
        except ValueError:
            parser.error("--since must be a date, YYYY-MM-DD")
    where = args.path or f"gh's answer for {args.repo}"
    try:
        if args.path:
            record = load(args.path)
            described = f"recorded file {args.path}, {len(record['pulls'])} merged pull requests"
        else:
            record, truncated = fetch(args.repo, args.since, args.limit)
            described = f"GitHub {args.repo}, {len(record['pulls'])} merged pull requests"
            described += f" since {args.since}" if args.since else ""
            described += f" (the newest {args.limit} only)" if truncated else ""
            if args.save:
                with open(args.save, "w", encoding="utf-8") as out:
                    json.dump(record, out, indent=1, sort_keys=True)
                    out.write("\n")
        text = render(record, args.person, described)
    except subprocess.TimeoutExpired:
        print("evidence-report: gh gave no answer in 300 seconds; check the network and try again", file=sys.stderr)
        return 2
    except OSError as err:
        print(f"evidence-report: cannot write {args.save}: {err.strerror}", file=sys.stderr)
        return 2
    except ReadError as err:
        print(f"evidence-report: {err}", file=sys.stderr)
        return 2
    except (KeyError, TypeError, ValueError, AttributeError) as err:
        print(f"evidence-report: {where} has an unexpected shape ({err!r}); read it again", file=sys.stderr)
        return 2
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
