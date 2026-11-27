"""Opens, updates or closes the `main-red` issue for one platform.

check.yml's job main-red runs this after every push to main, once for Linux
and once for macOS. When the platform's check (the job named `Linux` or
`macOS`) failed or was cancelled, it opens the issue "main-red: <platform>
checks failing on main", labelled main-red, or comments on it while it is
open. The report names the commit, the run, the platform's failing jobs,
the tests that failed every try (the run's JUnit reports, through
scripts/ci-failed-tests.sh), the first error lines of the failing jobs'
logs and the commits merged since the platform's last green run on main.
When the check passed, it comments the green run's link on the open issue
and closes it. Nothing is reverted.

Input, from the environment: PLATFORM (Linux or macOS), RESULT (the check
job's result: success, failure, cancelled or skipped), RUN_ID, REPO
(owner/name), SERVER_URL, DRY_RUN (`true` prints the action and the body,
to stdout and the step summary, and writes nothing). gh reads its token
from GH_TOKEN.

Log lines, test names and commit subjects are repository and job output:
they reach the issue as data, capped, in a fenced block or inline code, so
no mention or markup in them takes effect.
"""

import json
import os
import re
import subprocess
import sys
import tempfile

WORKFLOW = "check.yml"
LABEL = "main-red"
RUNS_LOOKED_AT = 20
COMMITS_LISTED = 50
TESTS_LISTED = 50
LOG_JOBS = 3
LOG_LINES = 15
LINE_CHARS = 300
LOG_BYTES = 32 * 1024 * 1024
TIMEOUT_S = 60

TIMESTAMP = re.compile(r"^\d{4}-\d\d-\d\dT[\d:.]+Z ")
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
ERROR = re.compile(
    r"##\[error\]|^error(\[|:)|^(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL) \[|panicked at"
)
PR_NUMBER = re.compile(r"\(#(\d+)\)\s*$")


class GhError(Exception):
    pass


def gh(args, stdin=None):
    """Runs gh with a timeout; returns stdout or raises GhError."""
    try:
        out = subprocess.run(
            ["gh", *args],
            input=stdin,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_S,
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        raise GhError(f"gh {args[0]} {args[1]}: {e}") from e
    if out.returncode != 0:
        first = (out.stderr.strip().splitlines() or ["no message"])[0]
        raise GhError(f"gh {args[0]} {args[1]} exited {out.returncode}: {first}")
    return out.stdout


def api(path):
    return json.loads(gh(["api", path]))


def inline(text, limit=LINE_CHARS):
    """Text for inline code: one line, no backtick, capped."""
    one = " ".join(str(text).split()).replace("`", "")
    return one[:limit]


def log_line(raw):
    line = ANSI.sub("", TIMESTAMP.sub("", raw)).strip()
    line = "".join(c for c in line if c.isprintable())
    # A fence of four tildes holds the block; no run of three closes it.
    return re.sub(r"~{3,}", "~~", line)[:LINE_CHARS]


def read_log(job_id):
    """The job's log text, at most LOG_BYTES, or raises GhError."""
    try:
        proc = subprocess.Popen(
            ["gh", "api", f"repos/{REPO}/actions/jobs/{job_id}/logs"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as e:
        raise GhError(f"gh api: {e}") from e
    try:
        data, err = proc.communicate(timeout=TIMEOUT_S * 2)
    except subprocess.TimeoutExpired as e:
        proc.kill()
        proc.communicate()
        raise GhError("gh api timed out reading the log") from e
    if proc.returncode != 0:
        first = (err.decode(errors="replace").strip().splitlines() or ["no message"])[0]
        raise GhError(first)
    return data[:LOG_BYTES].decode(errors="replace")


def platform_job(name):
    return name == PLATFORM or name.startswith(PLATFORM + " ")


def failing_jobs(jobs):
    bad = [
        j for j in jobs
        if platform_job(j.get("name", ""))
        and j.get("conclusion") in ("failure", "cancelled", "timed_out")
    ]
    # The aggregate check says least; it goes last.
    return sorted(bad, key=lambda j: j["name"] == PLATFORM)


def error_lines(jobs):
    """Markdown for the first error lines of up to LOG_JOBS failing jobs."""
    shown, notes, more = [], [], False
    count = 0
    for job in jobs[:LOG_JOBS]:
        try:
            text = read_log(job["id"])
        except GhError as e:
            notes.append(f"Could not read the log of {inline(job['name'])}: {inline(e)}")
            continue
        header = False
        for raw in text.splitlines():
            line = log_line(raw)
            if not ERROR.search(line):
                continue
            if count == LOG_LINES:
                more = True
                break
            if not header:
                shown.append(f"--- {inline(job['name'])}")
                header = True
            shown.append(line)
            count += 1
    parts = []
    if shown:
        parts.append("Log text from the jobs, quoted as data:\n\n~~~~text\n"
                     + "\n".join(shown) + "\n~~~~")
    else:
        parts.append("No error line was found in the logs.")
    if more:
        parts.append("More error lines are in the job logs.")
    parts.extend(notes)
    return "\n\n".join(parts)


def failed_tests(run_id):
    """Markdown list of the tests that failed every try, from the run's JUnit reports."""
    here = os.path.dirname(os.path.abspath(__file__))
    with tempfile.TemporaryDirectory() as dest:
        try:
            gh(["run", "download", run_id, "--repo", REPO,
                "--pattern", f"junit-{PLATFORM.lower()}-*", "--dir", dest])
        except GhError as e:
            if "no valid artifacts" not in str(e):
                return f"Could not download the JUnit reports: {inline(e)}"
        names = []
        for top, _, files in sorted(os.walk(dest)):
            for name in sorted(files):
                if not name.endswith(".xml"):
                    continue
                env = {k: v for k, v in os.environ.items() if k != "GITHUB_OUTPUT"}
                out = subprocess.run(
                    ["bash", os.path.join(here, "ci-failed-tests.sh"), os.path.join(top, name)],
                    capture_output=True, text=True, timeout=TIMEOUT_S, env=env,
                )
                names += [n for n in out.stdout.splitlines() if n]
    if not names:
        return ("No failed test was recorded: the job failed outside the tests, or "
                "stopped before their report. The failing jobs' logs say where.")
    listed = "\n".join(f"- `{inline(n)}`" for n in names[:TESTS_LISTED])
    if len(names) > TESTS_LISTED:
        listed += f"\n- and {len(names) - TESTS_LISTED} more; the JUnit reports list them all"
    return listed


def last_green(run):
    """The newest earlier push run on main whose platform check passed."""
    # `created=<T` (URL-encoded): the runs before this one, newest first.
    runs = api(f"repos/{REPO}/actions/workflows/{WORKFLOW}/runs"
               f"?branch=main&event=push&status=completed&per_page={RUNS_LOOKED_AT}"
               f"&created=%3C{run['created_at']}")
    for earlier in runs.get("workflow_runs", []):
        if earlier["id"] == run["id"] or earlier["created_at"] >= run["created_at"]:
            continue
        jobs = api(f"repos/{REPO}/actions/runs/{earlier['id']}/jobs?per_page=100")["jobs"]
        if any(j["name"] == PLATFORM and j["conclusion"] == "success" for j in jobs):
            return earlier
    return None


def merged_since(run):
    try:
        green = last_green(run)
    except (GhError, KeyError, ValueError) as e:
        return f"Could not find the last green {PLATFORM} run: {inline(e)}"
    if green is None:
        return (f"No green {PLATFORM} run on main among the last {RUNS_LOOKED_AT}, "
                "so the commits since are not listed.")
    head = f"Since the last green {PLATFORM} run on main, [run {green['id']}]({green['html_url']}) at `{green['head_sha'][:7]}`:"
    try:
        commits = api(f"repos/{REPO}/compare/{green['head_sha']}...{run['head_sha']}")["commits"]
    except (GhError, KeyError, ValueError) as e:
        return f"{head} could not compare the commits: {inline(e)}"
    rows = []
    for c in commits[-COMMITS_LISTED:]:
        subject = (c["commit"]["message"].splitlines() or [""])[0]
        number = PR_NUMBER.search(subject)
        ref = f"#{number.group(1)}" if number else f"`{c['sha'][:7]}`"
        rows.append(f"- {ref} `{inline(subject)}`")
    if len(commits) > COMMITS_LISTED:
        rows.insert(0, f"- and {len(commits) - COMMITS_LISTED} earlier commits")
    return head + "\n\n" + ("\n".join(rows) or "- no commit")


def red_body(run):
    sha = run["head_sha"]
    commit = f"[`{sha[:7]}`]({SERVER_URL}/{REPO}/commit/{sha})"
    try:
        jobs = failing_jobs(api(f"repos/{REPO}/actions/runs/{run['id']}/jobs?per_page=100")["jobs"])
        job_list = "\n".join(
            f"- [{inline(j['name'])}]({j['html_url']}): {j['conclusion']}" for j in jobs
        ) or "No failing job was listed."
        errors = error_lines(jobs)
    except (GhError, KeyError, ValueError) as e:
        job_list = f"Could not list the jobs: {inline(e)}"
        errors = "No log was read."
    sections = [
        f"The {PLATFORM} checks ended with {RESULT} on main at {commit}.\n\nRun: {run['html_url']}",
        f"### Failing jobs\n\n{job_list}",
        f"### Failed tests\n\n{failed_tests(str(run['id']))}",
        f"### First error lines\n\n{errors}",
        f"### Merged since the last green run\n\n{merged_since(run)}",
    ]
    tail = "Nothing is reverted automatically: fix it on a branch through the merge queue."
    if PLATFORM == "macOS":
        tail = ("macOS is optional in the merge queue, so the queue did not stop on this. " + tail)
    sections.append(tail + f" This issue closes when {PLATFORM} is green on main again.")
    return "\n\n".join(sections) + "\n"


def open_issue(title):
    issues = json.loads(gh(["issue", "list", "--repo", REPO, "--state", "open",
                            "--search", f'"{title}" in:title', "--json", "number,title",
                            "--limit", "20"]))
    numbers = [i["number"] for i in issues if i["title"] == title]
    return min(numbers) if numbers else None


def say(text):
    print(text)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(text + "\n")


def main():
    title = f"main-red: {PLATFORM} checks failing on main"
    red = RESULT in ("failure", "cancelled")
    if not red and RESULT != "success":
        say(f"The {PLATFORM} check ended with '{RESULT}': nothing to report.")
        return 0
    run = api(f"repos/{REPO}/actions/runs/{RUN_ID}")
    number = open_issue(title)
    if not red:
        if number is None:
            say(f"{PLATFORM} is green and no main-red issue is open.")
            return 0
        comment = (f"{PLATFORM} is green on main again at {run['head_sha']}: "
                   f"{run['html_url']}. Closing.")
        if DRY_RUN:
            say(f"Dry run: would close #{number} with: {comment}")
        else:
            gh(["issue", "close", str(number), "--repo", REPO, "--comment", comment])
            say(f"Closed #{number}.")
        return 0
    body = red_body(run)
    if DRY_RUN:
        action = f"comment on #{number}" if number else f'open "{title}" labelled {LABEL}'
        say(f"Dry run: would {action} with this body:\n\n{body}")
        return 0
    if number is None:
        gh(["label", "create", LABEL, "--repo", REPO, "--force", "--color", "B60205",
            "--description", "A check is failing on main"])
        url = gh(["issue", "create", "--repo", REPO, "--title", title, "--label", LABEL,
                  "--body-file", "-"], stdin=body)
        say(f"Opened {url.strip()}")
    else:
        gh(["issue", "comment", str(number), "--repo", REPO, "--body-file", "-"], stdin=body)
        say(f"Commented on #{number}.")
    return 0


def env(name, pattern):
    value = os.environ.get(name, "")
    if not re.fullmatch(pattern, value):
        sys.exit(f"ci-main-red: {name}={value!r} is not valid; check.yml's main-red job sets it")
    return value


PLATFORM = env("PLATFORM", r"Linux|macOS")
RESULT = env("RESULT", r"[a-z_]*")
RUN_ID = env("RUN_ID", r"[0-9]+")
REPO = env("REPO", r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
SERVER_URL = env("SERVER_URL", r"https://[A-Za-z0-9.-]+")
DRY_RUN = env("DRY_RUN", r"true|false") == "true"

if __name__ == "__main__":
    try:
        sys.exit(main())
    except (GhError, KeyError, ValueError) as e:
        sys.exit(f"ci-main-red: {e}. The run's log says which call failed; "
                 "re-run the job once GitHub answers.")
