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
and closes it. Nothing is reverted. When no test failed and each failing
job's error lines all name a failure of GitHub's runner or services (INFRA),
the run's first attempt opens no issue: it starts rerun-failed.yml, which
reruns the failed jobs once the run has ended, and the rerun reports again.

Input, from the environment: PLATFORM (Linux or macOS), RESULT (the check
job's result: success, failure, cancelled or skipped), RUN_ID, REPO
(owner/name), SERVER_URL, DRY_RUN (`true` prints the action and the body,
to stdout and the step summary, and writes nothing). gh reads its token
from GH_TOKEN.

Log lines, test names and commit subjects are repository and job output:
they reach the issue as data, capped, in a fenced block or inline code, so
no mention or markup in them takes effect.
"""

import functools
import json
import os
import re
import subprocess
import sys
import tempfile
import threading

WORKFLOW = "check.yml"
LABEL = "main-red"
RUNS_LOOKED_AT = 20
COMMITS_LISTED = 50
TESTS_LISTED = 50
LOG_JOBS = 3
LOG_LINES = 15
LINE_CHARS = 300
LOG_BYTES = 32 * 1024 * 1024
OUT_BYTES = 16 * 1024 * 1024
ERR_BYTES = 64 * 1024
CHUNK = 64 * 1024
TIMEOUT_S = 60

TIMESTAMP = re.compile(r"^\d{4}-\d\d-\d\dT[\d:.]+Z ")
# CSI sequences (colours) and OSC sequences (titles, links).
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)")
ERROR = re.compile(
    r"##\[error\]|^error(\[|:)|^(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL) \[|panicked at"
)
# An error line that GitHub's runner or services wrote, not the checks.
INFRA = re.compile(
    r"^##\[error\].*(Failed to CreateArtifact|ETIMEDOUT|ECONNRESET|"
    r"Failed to (restore|save) cache|Cache service responded with 5\d\d|"
    r"runner has received a shutdown signal|lost communication with the server)"
)
RERUN_WORKFLOW = "rerun-failed.yml"
PR_NUMBER = re.compile(r"\(#(\d+)\)\s*$")


class GhError(Exception):
    pass


def run_capped(cmd, limit, timeout, stdin=None, env=None):
    """Runs cmd, reading at most `limit` bytes of its stdout.

    Returns (exit code, stdout bytes, first stderr line, whether stdout
    reached the limit). At the limit or the timeout the child is killed and
    reaped; nothing is buffered past the limit. stderr is drained alongside,
    to at most ERR_BYTES, so a child that fills it cannot stall.
    """
    try:
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
    except OSError as e:
        raise GhError(f"{cmd[0]}: {e}") from e
    expired = threading.Event()

    def expire():
        expired.set()
        proc.kill()

    timer = threading.Timer(timeout, expire)
    timer.start()
    err = bytearray()

    def drain():
        while chunk := proc.stderr.read1(CHUNK):
            err.extend(chunk[: max(0, ERR_BYTES - len(err))])

    def feed():
        try:
            proc.stdin.write(stdin.encode())
            proc.stdin.close()
        except OSError:
            pass

    threads = [threading.Thread(target=drain, daemon=True)]
    if stdin is not None:
        threads.append(threading.Thread(target=feed, daemon=True))
    for t in threads:
        t.start()
    out = bytearray()
    full = False
    while chunk := proc.stdout.read1(min(CHUNK, limit + 1 - len(out))):
        out.extend(chunk)
        if len(out) > limit:
            full = True
            proc.kill()
            break
    proc.stdout.close()
    proc.wait()
    timer.cancel()
    for t in threads:
        t.join(timeout=5)
    if expired.is_set():
        raise GhError(f"{cmd[0]} {cmd[1]} timed out after {timeout} s; stopped it")
    first = (err.decode(errors="replace").strip().splitlines() or ["no message"])[0]
    return proc.returncode, bytes(out[:limit]), first, full


def gh(args, stdin=None):
    """Runs gh with a timeout; returns stdout or raises GhError."""
    code, out, first, full = run_capped(["gh", *args], OUT_BYTES, TIMEOUT_S, stdin)
    if full:
        raise GhError(f"gh {args[0]} printed more than {OUT_BYTES} bytes; stopped it")
    if code != 0:
        raise GhError(f"gh {args[0]} {args[1]} exited {code}: {first}")
    return out.decode(errors="replace")


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


@functools.cache
def escape_flag():
    """["--allow-escape-sequences"] when this gh has the flag, else [].

    Every log holds escape sequences (the runner colours each step's
    command). A gh that has the flag refuses to print them without it; an
    older gh rejects the flag as unknown and passes them through. log_line
    strips them either way.
    """
    code, out, first, _ = run_capped(["gh", "api", "--help"], OUT_BYTES, TIMEOUT_S)
    if code != 0:
        raise GhError(f"gh api --help exited {code}: {first}")
    return ["--allow-escape-sequences"] if b"--allow-escape-sequences" in out else []


def read_log(job_id):
    """The job's log text, at most its first LOG_BYTES, or raises GhError."""
    code, data, first, full = run_capped(
        ["gh", "api", f"repos/{REPO}/actions/jobs/{job_id}/logs", *escape_flag()],
        LOG_BYTES, TIMEOUT_S * 2)
    # A log cut at the cap was killed there, so its exit code says nothing.
    if code != 0 and not full:
        raise GhError(first)
    return data.decode(errors="replace")


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


def read_logs(jobs):
    """{job id: its error lines, cleaned} for up to LOG_JOBS failing jobs, and
    a note for each log that could not be read."""
    logs, notes = {}, []
    for job in jobs[:LOG_JOBS]:
        try:
            text = read_log(job["id"])
        except GhError as e:
            notes.append(f"Could not read the log of {inline(job['name'])}: {inline(e)}")
            continue
        logs[job["id"]] = [line for line in map(log_line, text.splitlines()) if ERROR.search(line)]
    return logs, notes


def error_lines(jobs, logs, notes):
    """Markdown for the first error lines of the failing jobs' logs."""
    shown, more = [], False
    count = 0
    for job in jobs:
        header = False
        for line in logs.get(job["id"], []):
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


def failed_test_names(run_id):
    """(the tests that failed every try, from the run's JUnit reports, or
    None with a note saying why the reports could not be read)."""
    here = os.path.dirname(os.path.abspath(__file__))
    names = []
    with tempfile.TemporaryDirectory() as dest:
        try:
            gh(["run", "download", run_id, "--repo", REPO,
                "--pattern", f"junit-{PLATFORM.lower()}-*", "--dir", dest])
        except GhError as e:
            if "no valid artifacts" not in str(e):
                return None, f"Could not download the JUnit reports: {inline(e)}"
        for top, _, files in sorted(os.walk(dest)):
            for name in sorted(files):
                if not name.endswith(".xml"):
                    continue
                env = {k: v for k, v in os.environ.items() if k != "GITHUB_OUTPUT"}
                try:
                    _, out, _, _ = run_capped(
                        ["bash", os.path.join(here, "ci-failed-tests.sh"), os.path.join(top, name)],
                        OUT_BYTES, TIMEOUT_S, env=env)
                except GhError as e:
                    return None, f"Could not read the JUnit report {inline(name)}: {inline(e)}"
                names += [n for n in out.decode(errors="replace").splitlines() if n]
    return names, None


def failed_tests(names, note):
    """Markdown list of the tests that failed every try."""
    if names is None:
        return note
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


def infrastructure(jobs, logs, names):
    """(a failing job, its first error line) when no test failed and every
    failing job failed on GitHub's runner or services; otherwise None.

    Each failing job but the platform's aggregate check, which fails because
    a job it needs did, must have a log that was read and whose error lines
    all match INFRA. Anything unread or unrecognised is not infrastructure.
    """
    if names != []:
        return None
    shards = [j for j in jobs if j["name"] != PLATFORM]
    if not shards:
        return None
    for job in shards:
        lines = logs.get(job["id"])
        if not lines or not all(INFRA.search(line) for line in lines):
            return None
    return shards[0], logs[shards[0]["id"]][0]


def attempt(run):
    return int(run.get("run_attempt") or 1)


def red_report(run):
    """(the issue body, infrastructure(...) for the run's failing jobs)."""
    sha = run["head_sha"]
    commit = f"[`{sha[:7]}`]({SERVER_URL}/{REPO}/commit/{sha})"
    names, note = failed_test_names(str(run["id"]))
    infra = None
    try:
        jobs = failing_jobs(api(f"repos/{REPO}/actions/runs/{run['id']}/jobs?per_page=100")["jobs"])
        job_list = "\n".join(
            f"- [{inline(j['name'])}]({j['html_url']}): {j['conclusion']}" for j in jobs
        ) or "No failing job was listed."
        logs, notes = read_logs(jobs)
        errors = error_lines(jobs, logs, notes)
        infra = infrastructure(jobs, logs, names)
    except (GhError, KeyError, ValueError) as e:
        job_list = f"Could not list the jobs: {inline(e)}"
        errors = "No log was read."
    run_line = f"Run: {run['html_url']}"
    if attempt(run) > 1:
        run_line += f" (attempt {attempt(run)}: its failed jobs were rerun)"
    sections = [
        f"The {PLATFORM} checks ended with {RESULT} on main at {commit}.\n\n{run_line}",
        f"### Failing jobs\n\n{job_list}",
        f"### Failed tests\n\n{failed_tests(names, note)}",
        f"### First error lines\n\n{errors}",
        f"### Merged since the last green run\n\n{merged_since(run)}",
    ]
    tail = "Nothing is reverted automatically: fix it on a branch through the merge queue."
    if PLATFORM == "macOS":
        tail = ("macOS is optional in the merge queue, so the queue did not stop on this. " + tail)
    sections.append(tail + f" This issue closes when {PLATFORM} is green on main again.")
    return "\n\n".join(sections) + "\n", infra


def request_rerun(run, infra):
    """On a run's first attempt, asks RERUN_WORKFLOW to rerun its failed jobs
    once it has finished (a run can't rerun itself while it runs). Returns
    None when the rerun was asked for (or, in a dry run, would be), or a note
    for the issue saying why it could not be."""
    job, line = infra
    why = (f"every test passed and {inline(job['name'])} failed on GitHub's "
           f"infrastructure: `{inline(line)}`")
    if DRY_RUN:
        say(f"Dry run: would rerun the failed jobs of run {run['id']} once: {why}")
        return None
    try:
        gh(["workflow", "run", RERUN_WORKFLOW, "--repo", REPO, "--ref", "main",
            "-f", f"run_id={run['id']}"])
    except GhError as e:
        return f"Could not request a rerun of the failed jobs: {inline(e)}"
    say(f"Asked {RERUN_WORKFLOW} to rerun the failed jobs of run {run['id']} once, so no "
        f"issue is opened: {why}. The rerun reports again, and opens the issue if it fails too.")
    return None


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
    body, infra = red_report(run)
    if infra and attempt(run) == 1:
        note = request_rerun(run, infra)
        if note is None:
            return 0
        body += f"\n{note}\n"
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
