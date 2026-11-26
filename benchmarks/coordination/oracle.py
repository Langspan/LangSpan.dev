#!/usr/bin/env python3
"""The combined-integration oracle: judge one integrated fixture checkout.

Run as ``python3 -I -B oracle.py PLAN CHECKOUT OUT``. It imports the code in
CHECKOUT, which agents wrote, so the driver runs it in its own process with a
time limit. It writes its verdict as JSON to OUT and exits 0 only when the
integrated result is accepted. The task definitions come from PLAN, written
by the driver; nothing is imported from this directory.
"""

import datetime
import importlib
import json
from pathlib import Path
import re
import subprocess
import sys
import traceback

DAY = datetime.date(2026, 3, 4)
FIXTURE_TEST_SECONDS = 120


def observe(function, call):
    """The effective setting a contradictory-defaults caller uses."""
    if function == "export_batch":
        return len(call(list(range(1000)))[0])
    if function == "list_page":
        return len(call(list(range(1000)), 0))
    if function in ("invoice_total", "price_label"):
        return call(2).split()[-1]
    if function == "export_filename":
        return call(DAY)
    if function == "receipt_date":
        return call(DAY)
    return call()


def expected(function, value):
    if function == "export_filename":
        return "export-%s.csv" % DAY.strftime(value)
    if function == "receipt_date":
        return DAY.strftime(value)
    return value


def run_checks(task, checkout):
    """Import the task's module from the integrated tree and check it."""
    if task["role"] == "new_caller":
        # The new caller must use the new name, not reach the parser through
        # an alias or a wrapper the rename was told not to keep.
        source = checkout / (task["module"].replace(".", "/") + ".py")
        text = source.read_text(encoding="utf-8", errors="replace") if source.is_file() else ""
        if re.search(r"\bparse_config\b", text) or not re.search(r"\bload_config\b", text):
            return False, "%s does not call load_config, or still names parse_config" % source.relative_to(checkout)
    module = importlib.import_module(task["module"])
    call = getattr(module, task["function"])
    if "checks" in task:
        for arguments, result in task["checks"]:
            got = call(*arguments)
            if got != result:
                return False, "%s%r returned %r, expected %r" % (task["function"], tuple(arguments), got, result)
        return True, "every check passed"
    got = observe(task["function"], call)
    if got != expected(task["function"], task["value"]):
        return False, "%s() uses %r, expected %r" % (task["function"], got, expected(task["function"], task["value"]))
    return True, "%s() uses %r" % (task["function"], got)


class Unimportable:
    """Stands in for a module the integrated tree cannot import: it has no names."""


def load(name):
    try:
        return importlib.import_module(name)
    except Exception:  # Agent code can fail in any way; it then defines nothing.
        return Unimportable()


def python_sources(checkout):
    root = checkout / "shelf"
    return {str(path.relative_to(checkout)): path.read_text(encoding="utf-8", errors="replace")
            for path in sorted(root.rglob("*.py"))}


def judge(plan, checkout):
    """The verdict on one combined tree. Success is what tasks.SUCCESS states
    for the family; notes sessions wrote are kept as evidence, never judged."""
    sys.path.insert(0, str(checkout))
    merged = {entry["session"] for entry in plan["integration"] if entry["merged"]}
    tasks = []
    for session, task in enumerate(plan["tasks"], 1):
        if task["role"] == "rename":
            config = load("shelf.config")
            missing = [name for name in ("load_config",) if not hasattr(config, name)]
            stale = [path for path, text in python_sources(checkout).items()
                     if path in plan["config_users"] and re.search(r"\bparse_config\b", text)]
            ok = not missing and not hasattr(config, "parse_config") and not stale
            detail = "renamed" if ok else "load_config missing: %s; old name still in: %s; parse_config defined: %s" % (
                missing, stale, hasattr(config, "parse_config"))
            entry = {"completed": ok, "detail": detail}
        else:
            try:
                ok, detail = run_checks(task, checkout)
            except Exception as error:  # Agent code can fail in any way; record it.
                ok, detail = False, "%s: %s" % (type(error).__name__, error)
                if "--traceback" in sys.argv:
                    traceback.print_exc()
            entry = {"completed": ok, "detail": detail}
        entry.update(session=session, task_id=task["id"], merged=session in merged)
        tasks.append(entry)

    conflicts = []
    if plan["family"] == "rename_vs_new_caller":
        config = load("shelf.config")
        defined = {name for name in ("parse_config", "load_config") if hasattr(config, name)}
        for path, text in python_sources(checkout).items():
            for name in ("parse_config", "load_config"):
                if name not in defined and re.search(r"\b%s\b" % name, text):
                    conflicts.append("%s calls %s, which the integrated shelf/config.py does not define" % (path, name))
        for entry in tasks:
            entry["accepted"] = entry["completed"]
    else:
        # The reconciliation every contradiction task defines: each function
        # gets its own value, and the shared default stays as it was.
        defaults = getattr(load("shelf.settings"), "DEFAULTS", None)
        pairs = {}
        for entry, task in zip(tasks, plan["tasks"]):
            if "pair" in task:
                pairs.setdefault(task["pair"], []).append((entry, task))
        for key, members in sorted(pairs.items()):
            old = members[0][1]["old"]
            kept = isinstance(defaults, dict) and key in defaults and defaults[key] == old
            reconciled = kept and all(entry["completed"] for entry, _ in members)
            for entry, _ in members:
                entry["accepted"] = reconciled
                if entry["completed"] and not kept:
                    entry["detail"] += "; the shared default %s is no longer %s" % (key, json.dumps(old))
            if all(entry["merged"] for entry, _ in members) and not all(entry["completed"] for entry, _ in members):
                lost = [task["function"] for entry, task in members if not entry["completed"]]
                conflicts.append("%s: both changes merged cleanly, but %s no longer gets what its task asked for" % (key, ", ".join(lost)))
        for entry in tasks:
            entry.setdefault("accepted", entry["completed"])

    try:
        suite = subprocess.run(
            # The command the task prompts name, minus the environment and
            # user site (-E -s). Not -I: the checkout must be on sys.path.
            [sys.executable, "-E", "-s", "-B", "-m", "unittest", "discover", "-s", "tests"],
            cwd=checkout, capture_output=True, text=True, timeout=FIXTURE_TEST_SECONDS)
        fixture_tests = "passed" if suite.returncode == 0 else "failed"
        fixture_output = suite.stderr[-4000:]
    except subprocess.TimeoutExpired:
        fixture_tests, fixture_output = "failed", "timed out after %d s" % FIXTURE_TEST_SECONDS

    all_merged = len(merged) == len(plan["tasks"])
    accepted = all_merged and fixture_tests == "passed" and not conflicts and all(t["accepted"] for t in tasks)
    return {
        "integrated_tests": "passed" if accepted else "failed",
        "tasks": tasks,
        "semantic_conflicts": conflicts,
        "fixture_tests": fixture_tests,
        "fixture_tests_output_tail": fixture_output,
    }


def main(argv):
    if len(argv) not in (3, 4):
        print("usage: oracle.py PLAN CHECKOUT OUT [--traceback]", file=sys.stderr)
        return 2
    plan = json.loads(Path(argv[0]).read_text(encoding="utf-8"))
    verdict = judge(plan, Path(argv[1]).resolve())
    Path(argv[2]).write_text(json.dumps(verdict, indent=2) + "\n", encoding="utf-8")
    return 0 if verdict["integrated_tests"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
