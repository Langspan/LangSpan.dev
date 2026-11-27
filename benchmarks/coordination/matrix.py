#!/usr/bin/env python3
"""Coordination benchmark matrix: many trials, then one results document.

``run`` runs driver.py once per trial (family, session count, seed and arm),
each into its own directory under ``--output``, one after another. It keeps
going past a failed, timed-out or refused trial and appends one line per
trial to ``matrix.jsonl``. A trial whose directory already holds a record
from the same agent is kept, not repeated, so an interrupted matrix resumes;
a directory without one, or with another agent's record, is reported and
left as it is. A timeout or a signal stops the driver and its sessions.
``--plan-only`` prints the trials and the session counts and runs nothing. ``--agent standin`` (the
default) makes no model call; ``--agent claude`` passes through to the
driver, which keeps its own sign-in folder and permission checks.

``report`` reads the records under one or more matrix directories and
writes the results document: every trial, every condition, the versions and
the seeds. It shows no price.
"""

import argparse
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import driver  # noqa: E402
import runner  # noqa: E402
import tasks as families  # noqa: E402

# A bound on one trial, above the driver's own: the longest sessions and
# stagger, the oracle and the repair round with its oracle.
TRIAL_SECONDS = 4 * 3600
Z95 = 1.959963984540054
TOKEN_ORDER = ("input", "cache_read", "cache_creation", "output")
MISSING = ("incomplete", "no_record", "timed_out", "agent_mismatch", "unusable_record")
# How long a stopped driver and its sessions get to exit before SIGKILL.
STOP_SECONDS = 30


def trials(family_list, session_counts, seeds, arms):
    """Every trial in run order. Within one family, N and seed the arm order
    rotates with the seed, so no arm always goes first."""
    arms = list(dict.fromkeys(arms))
    found = []
    for family in dict.fromkeys(family_list):
        for sessions in dict.fromkeys(session_counts):
            for seed in dict.fromkeys(seeds):
                offset = (seed - 1) % len(arms)
                for order, arm in enumerate(arms[offset:] + arms[:offset], 1):
                    found.append({"name": "%s-n%d-s%d-%s" % (family, sessions, seed, arm), "family": family,
                                  "sessions": sessions, "seed": seed, "arm": arm, "arm_order": order})
    return found


def driver_argv(trial, output, agent, langspan):
    argv = [sys.executable, "-I", "-B", str(HERE / "driver.py"), "--arm", trial["arm"], "--family", trial["family"],
            "--sessions", str(trial["sessions"]), "--seed", str(trial["seed"]), "--agent", agent,
            "--run", "--output", str(output / trial["name"])]
    if trial["arm"] == "langspan" and langspan is not None:
        argv += ["--langspan", str(langspan)]
    return argv


def process_groups(root):
    """The process groups of ROOT and of every descendant, from one `ps`
    listing: the driver starts its sessions and coordinator in groups of
    their own. Only ROOT's group when `ps` fails."""
    try:
        listing = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,pgid="], stdin=subprocess.DEVNULL,
                                 capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError) as error:
        print("matrix: cannot list the trial's processes (%s); stopping only the driver's group" % error,
              file=sys.stderr)
        listing = ""
    children, group_of = {}, {}
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) == 3 and all(field.isdigit() for field in fields):
            pid, ppid, pgid = map(int, fields)
            children.setdefault(ppid, []).append(pid)
            group_of[pid] = pgid
    found, pending = {root}, [root]
    while pending:
        for pid in children.get(pending.pop(), ()):
            if pid not in found:
                found.add(pid)
                pending.append(pid)
    return ({group_of.get(pid, root) for pid in found} | {root}) - {0, 1, os.getpgrp()}


def signal_groups(groups, signum):
    """Signal each of GROUPS; a group that has already exited is skipped."""
    for group in groups:
        try:
            os.killpg(group, signum)
        except (ProcessLookupError, PermissionError):
            pass


def stop_trial(child, grace=STOP_SECONDS):
    """Stop CHILD and every process group under it: SIGTERM, then after
    GRACE seconds SIGKILL, listing the groups again so that one started
    meanwhile is stopped too."""
    groups = process_groups(child.pid)
    signal_groups(groups, signal.SIGTERM)
    try:
        child.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        pass
    signal_groups(groups | process_groups(child.pid), signal.SIGKILL)
    try:
        child.wait(timeout=10)
    except subprocess.TimeoutExpired:
        print("matrix: driver %d survived SIGKILL for 10 s" % child.pid, file=sys.stderr)


def raise_interrupt(signum, _frame):
    raise KeyboardInterrupt("signal %d" % signum)


def run_trial(argv, log, timeout=TRIAL_SECONDS):
    """Run one driver; its exit code, or None when it outlived TIMEOUT and
    was stopped. SIGINT, SIGTERM or SIGHUP stops the driver and its
    sessions, then raises KeyboardInterrupt."""
    previous = {signum: signal.signal(signum, raise_interrupt) for signum in (signal.SIGTERM, signal.SIGHUP)}
    try:
        with open(log, "wb") as sink:
            child = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=sink, stderr=subprocess.STDOUT,
                                     start_new_session=True)
            try:
                return child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                stop_trial(child)
                return None
            except BaseException:
                stop_trial(child)
                raise
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def kept_outcome(source, agent):
    """`kept` when SOURCE is a record this version reads, made by AGENT;
    otherwise why the trial cannot be reused."""
    try:
        found, _ = runner.bounded_json(source, driver.MAX_CONFIG_BYTES)
    except (OSError, ValueError):
        return "unusable_record"
    if not isinstance(found, dict) or found.get("schema_version") != driver.SCHEMA_VERSION:
        return "unusable_record"
    return "kept" if found.get("agent") == driver.record_agent(agent) else "agent_mismatch"


def run_matrix(planned, output, agent, langspan):
    """Run each trial in PLANNED that has no directory yet. Returns how many
    trials have no record."""
    output.mkdir(mode=0o700, parents=True, exist_ok=True)
    journal = os.open(output / "matrix.jsonl", os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    missing = 0
    with os.fdopen(journal, "a", encoding="utf-8") as sink:
        for trial in planned:
            directory = output / trial["name"]
            started_at = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
            started, exit_code = time.monotonic(), None
            if (directory / "record.json").is_file():
                outcome = kept_outcome(directory / "record.json", agent)
            elif directory.exists():
                outcome = "incomplete"
            else:
                exit_code = run_trial(driver_argv(trial, output, agent, langspan),
                                      output / (trial["name"] + ".driver.log"))
                outcome = "timed_out" if exit_code is None else (
                    "ran" if (directory / "record.json").is_file() else "no_record")
            missing += outcome in MISSING
            line = dict(trial, agent=agent, outcome=outcome, exit_code=exit_code, started_at=started_at,
                        elapsed_s=round(time.monotonic() - started, 3))
            line["trial"] = line.pop("name")
            sink.write(json.dumps(line) + "\n")
            sink.flush()
    return missing


# --- The results document ----------------------------------------------------

def wilson(k, n, z=Z95):
    """The Wilson score interval for K successes in N trials, or None."""
    if n == 0:
        return None
    centre = (k + z * z / 2) / (n + z * z)
    half = z * math.sqrt(k * (n - k) / n + z * z / 4) / (n + z * z)
    return max(0.0, centre - half), min(1.0, centre + half)


def cell(value):
    if value is None:
        return "unknown"
    if isinstance(value, float):
        return "%g" % round(value, 1)
    return str(value).replace("|", "\\|")


def tokens(figures):
    found = (figures or {}).get("tokens")
    return "unknown" if found is None else " / ".join(str(found[name]) for name in TOKEN_ORDER)


def proportion(k, n):
    interval = wilson(k, n)
    return "%d of %d" % (k, n) + ("" if interval is None else " (%.3f to %.3f)" % interval)


def stale_cells(record):
    found = record.get("stale_callers") or {}
    if found.get("status") != "reported":
        return found.get("status", "unknown"), "not measured"
    if found["unfixed"]:
        return found["writes_admitted"], "%d unfixed" % found["unfixed"]
    return found["writes_admitted"], "none stale" if found["fixed_after_s"] is None else cell(found["fixed_after_s"])


def table(header, rows):
    lines = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    return lines + ["| " + " | ".join(cell(value) for value in row) + " |" for row in rows]


def report(named, missing):
    """The results document for NAMED, a list of (trial directory name,
    version 8 record), and MISSING, trial directories with no record."""
    records = [record for _, record in named]
    dry = sum(record["dry_run"] is True for record in records)
    lines = ["# Coordination benchmark results", ""]
    lines.append("Dry run: %d of %d trials used the stand-in agent, which makes no model call; those rows measure "
                 "the harness, not agents." % (dry, len(records)) if dry else
                 "Every trial ran real agent sessions; the agents and models are listed under Versions.")
    lines += ["", "## Trials", "",
              "Every trial is listed, refused, failed and timed out included. Tokens are input (uncached) / cache "
              "read / cache write / output, from the host's own usage report, before any repair.", ""]
    rows = []
    for name, record in named:
        writes, fixed = stale_cells(record)
        repair = record["repair"]
        rows.append([name, record["status"], driver.excluded_as(record) or "no", record["integrated_tests"],
                     "unknown" if record["tasks_accepted"] is None else
                     "%d of %d" % (record["tasks_accepted"], record["tasks_total"]),
                     writes, fixed, tokens(record["cost"]["run"]), record["turns"], record["elapsed_s"],
                     repair["status"] + (": " + repair["integrated_tests"] if repair["status"] == "ran" else "")])
    lines += table(["Trial", "Status", "Excluded", "Combined build", "Tasks accepted", "Stale-name writes admitted",
                    "Last stale caller fixed (s)", "Tokens", "Turns", "Elapsed (s)", "Repair"], rows)
    if missing:
        lines += ["", "No record (the trial stopped before writing one; its directory is kept): "
                  + ", ".join(sorted(missing)) + "."]
    lines += ["", "## Conditions", "",
              "Excluded trials are counted and add nothing else. Combined build passed is before any repair; a "
              "correct result may follow a repair. Intervals are Wilson 95% intervals.", ""]
    rows = []
    for group in driver.summarize(records):
        excluded = ", ".join("%d %s" % (group[kind], kind) for kind in
                             ("refused", "tool_check_failed", "repair_tool_check_failed", "change_not_made")
                             if group[kind]) or "none"
        fixed = ", ".join("none stale" if value is None else cell(value) for value in group["stale_fixed_after_s"])
        if group["stale_unfixed_trials"]:
            fixed += " (%d unfixed)" % group["stale_unfixed_trials"]
        rows.append([group["agent"], group["family"], group["sessions"], group["stagger_s"], group["arm"],
                     group["trials"], excluded, proportion(group["passed_without_repair"], group["compared"]),
                     proportion(group["correct_results"], group["compared"]), group["stale_writes_admitted"],
                     fixed or "none", tokens(group["run"]), tokens(group["repair"]), tokens(group["total"])])
    lines += table(["Agent", "Family", "N", "Stagger (s)", "Arm", "Trials", "Excluded", "Combined build passed",
                    "Correct results", "Stale-name writes admitted", "Last stale caller fixed (s)", "Tokens, run",
                    "Tokens, repair", "Tokens, total"], rows)
    lines += ["", "No result here is a LangSpan advantage unless the counts above show it.",
              "", "## Versions and seeds", ""]
    for label, key in (("Agent CLI", "agent_cli"), ("Requested model", "requested_model"), ("Models", "models"),
                       ("LangSpan commit", "langspan_commit"), ("LangSpan binary SHA-256", "langspan_binary_sha256"),
                       ("Git", "git"), ("Python", "python")):
        values = set()
        for record in records:
            value = record["versions"][key]
            values.update(value if isinstance(value, list) else [value])
        lines.append("- %s: %s" % (label, ", ".join(sorted(cell(value) for value in values)) or "none"))
    lines.append("- Seeds: " + ", ".join(str(seed) for seed in sorted({record["seed"] for record in records})))
    return "\n".join(lines) + "\n"


def read_trials(paths):
    """The (name, record) pairs and the names without a record under the
    matrix directories PATHS. Raises ValueError on a record this version
    cannot read."""
    named, missing = [], []
    for path in paths:
        for directory in sorted(child for child in Path(path).iterdir() if child.is_dir()):
            source = directory / "record.json"
            if not source.is_file():
                missing.append(directory.name)
                continue
            record, _ = runner.bounded_json(source, driver.MAX_CONFIG_BYTES)
            version = record.get("schema_version") if isinstance(record, dict) else None
            if version != driver.SCHEMA_VERSION:
                raise ValueError("%s has schema version %s; the report reads version %d records"
                                 % (source, version, driver.SCHEMA_VERSION))
            named.append((directory.name, record))
    return named, missing


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="run every trial of the matrix")
    run.add_argument("--families", nargs="+", required=True, choices=families.FAMILIES)
    run.add_argument("--sessions", nargs="+", required=True, type=int, choices=families.SESSION_COUNTS)
    run.add_argument("--seeds", nargs="+", required=True, type=int)
    run.add_argument("--arms", nargs="+", default=list(driver.ARMS), choices=driver.ARMS)
    run.add_argument("--agent", default="standin", choices=driver.AGENTS)
    run.add_argument("--langspan", type=Path, help="the langspan binary, for the langspan arm")
    run.add_argument("--output", type=Path, required=True, help="the matrix directory; reused to resume")
    run.add_argument("--plan-only", action="store_true", help="print the trials and run nothing")
    document = commands.add_parser("report", help="write the results document")
    document.add_argument("paths", nargs="+", type=Path, help="matrix directories")
    document.add_argument("--output", type=Path, required=True, help="the Markdown file to write")
    args = parser.parse_args(argv)
    try:
        if args.command == "report":
            named, missing = read_trials(args.paths)
            if not named:
                raise ValueError("no trial record under " + ", ".join(map(str, args.paths)))
            args.output.write_text(report(named, missing), encoding="utf-8")
            print(json.dumps({"report": str(args.output), "trials": len(named), "without_record": len(missing)}))
            return 0
        planned = trials(args.families, args.sessions, args.seeds, args.arms)
        if args.plan_only:
            print(json.dumps({"mode": "plan_only", "executed_commands": 0, "agent": args.agent,
                              "trials": len(planned), "sessions": sum(t["sessions"] for t in planned),
                              "max_repair_sessions": len(planned),
                              "trial_list": [t["name"] for t in planned]}, indent=2))
            return 0
        output = args.output.resolve()
        driver.check_output_location(output)
        missing = run_matrix(planned, output, args.agent, args.langspan)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(json.dumps({"error": "%s: %s" % (type(error).__name__, error)}), file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print(json.dumps({"error": "interrupted; the trial that was running has no record, and running the same "
                                   "command again reports it as incomplete"}), file=sys.stderr)
        return 130
    print(json.dumps({"journal": str(output / "matrix.jsonl"), "trials": len(planned), "without_record": missing}))
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
