#!/usr/bin/env python3
"""The combined-integration oracle: judge one integrated fixture checkout.

Run as ``python3 -I -B oracle.py PLAN CHECKOUT OUT``. It imports the code in
CHECKOUT, which agents wrote, so the driver runs it in its own process with a
time limit. It writes its verdict as JSON to OUT and exits 0 only when the
integrated result is accepted. The task definitions come from PLAN, written
by the driver; nothing is imported from this directory.
"""

import ast
import datetime
import importlib
import inspect
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


def references(text, name):
    """Each place TEXT names NAME (as `name`, `module.name`, or an alias
    that `from ... import name as alias` binds), in source order: the call's
    argument count, or None for a reference that is not a plain call (a call
    with `*args` or `**kwargs`, or the name passed or kept as a value). None
    when TEXT does not parse."""
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return None
    local = {name} | {alias.asname for node in ast.walk(tree) if isinstance(node, ast.ImportFrom)
                      for alias in node.names if alias.name == name and alias.asname}

    def names(node):
        return (isinstance(node, ast.Name) and node.id in local) or (isinstance(node, ast.Attribute) and node.attr == name)

    counts = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Call) and names(node.func):
            spread = any(isinstance(arg, ast.Starred) for arg in node.args) or any(k.arg is None for k in node.keywords)
            counts[id(node.func)] = None if spread else len(node.args) + len(node.keywords)
    found = [(node.lineno, node.col_offset, counts.get(id(node))) for node in ast.walk(tree) if names(node)]
    return [count for _, _, count in sorted(found, key=lambda item: item[:2])]


def arity(function):
    """The (fewest, most) arguments FUNCTION takes; most is None when it
    takes any number."""
    try:
        parameters = inspect.signature(function).parameters.values()
    except (TypeError, ValueError):
        return None
    variadic = (inspect.Parameter.VAR_POSITIONAL, inspect.Parameter.VAR_KEYWORD)
    fewest = sum(p.default is inspect.Parameter.empty and p.kind not in variadic for p in parameters)
    most = None if any(p.kind in variadic for p in parameters) else len(parameters)
    return fewest, most


def required_arguments(text, name):
    """How many arguments the last top-level definition of the function NAME
    in TEXT requires, or None when TEXT is None, does not parse or does not
    define NAME. Read from the source, so nothing in TEXT runs."""
    try:
        tree = ast.parse(text)
    except (SyntaxError, TypeError, ValueError, RecursionError, MemoryError):
        return None
    found = None
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == name:
            args = node.args
            found = (len(args.posonlyargs) + len(args.args) - len(args.defaults)
                     + sum(default is None for default in args.kw_defaults))
    return found


def interface_change(plan, checkout):
    """Whether the changer changed the interface its family is about: `made`
    when its branch's helper requires the changed form's arguments, else
    `change_not_made`; None in a family without one. The driver passes the
    helper's source on the changer's branch as `interface_source`, so a
    change another session's branch made does not count; without it the
    merged tree's source is read."""
    interface = next((task["interface"] for task in plan["tasks"] if task["role"] == "changer"), None)
    if plan["family"] != "discovered_change" or interface is None:
        return None
    path = interface["module"].replace(".", "/") + ".py"
    text = plan["interface_source"] if "interface_source" in plan else source_of(checkout, path)
    required = required_arguments(text, interface["name"])
    return "made" if required is not None and required >= interface["arity"] else "change_not_made"


def source_of(checkout, path):
    file = checkout / path
    return file.read_text(encoding="utf-8", errors="replace") if file.is_file() else None


def changed_form(task, checkout):
    """Whether TASK's code calls the changed interface only in its changed
    form: at least once, and every reference a call with at least the
    arguments the change requires. A fallback to the old form, a call with
    spread arguments, or code that counts without the helper is a hedge."""
    interface = task["interface"]
    own = task["module"].replace(".", "/") + ".py"
    for path in task.get("callers") or [own]:
        text = source_of(checkout, path)
        if text is None:
            return False, "%s is missing" % path
        found = references(text, interface["name"])
        if found is None:
            return False, "%s does not parse" % path
        if not found and path == own:
            return False, "%s does not call %s, so it does not use the shared helper" % (path, interface["name"])
        old = [count for count in found if count is None or count < interface["arity"]]
        if old:
            return False, "%s calls %s in its old form or through a fallback (%s)" % (
                path, interface["name"], ", ".join("not a plain call" if c is None else "%d argument(s)" % c for c in old))
    return True, None


def dated(task, arguments):
    """A check's arguments with each item's day offset made a date: from
    today for a dependent, which uses the clock, and from DAY for the
    changer, whose function is also given DAY."""
    base = DAY if task["dates"] == "fixed" else datetime.date.today()
    items = [(name, base + datetime.timedelta(days=offset)) for name, offset in arguments[0]]
    return [items, *arguments[1:]] + ([DAY] if task["dates"] == "fixed" else [])


def run_checks(task, checkout):
    """Import the task's module from the integrated tree and check it."""
    if task["role"] == "changer":
        interface = task["interface"]
        helper = getattr(load(interface["module"]), interface["name"], None)
        taken = arity(helper) if callable(helper) else None
        if taken is None:
            return False, "%s is not defined in %s" % (interface["name"], interface["module"])
        if taken[0] < interface["arity"]:
            return False, ("%s still accepts %d argument(s): a default or the old signature keeps its old calls "
                           "working" % (interface["name"], taken[0]))
    if task["role"] in ("changer", "dependent"):
        ok, why = changed_form(task, checkout)
        if not ok:
            return False, why
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
            if "dates" in task:
                arguments = dated(task, arguments)
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
    change = interface_change(plan, checkout)
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
    elif plan["family"] == "discovered_change":
        # A call that does not fit the integrated helper breaks the tree,
        # whichever branch wrote it.
        interface = next(task["interface"] for task in plan["tasks"] if "interface" in task)
        helper = getattr(load(interface["module"]), interface["name"], None)
        taken = arity(helper) if callable(helper) else None
        for path, text in python_sources(checkout).items():
            counts = [count for count in references(text, interface["name"]) or [] if count is not None]
            if counts and taken is None:
                conflicts.append("%s calls %s, which the integrated tree does not define" % (path, interface["name"]))
            elif taken is not None:
                unfit = sorted({count for count in counts
                                if count < taken[0] or (taken[1] is not None and count > taken[1])})
                if unfit:
                    conflicts.append("%s calls %s with %s argument(s); the integrated %s takes %d to %s" % (
                        path, interface["name"], " or ".join(map(str, unfit)), interface["module"], taken[0],
                        "any number" if taken[1] is None else taken[1]))
        for entry in tasks:
            entry["accepted"] = entry["completed"]
        if change == "change_not_made":
            # Nothing changed for a dependent to follow, so its work says
            # nothing about what it knew: it is not judged (issue #218).
            why = "the changer's branch does not make %s require %d arguments" % (
                interface["name"], interface["arity"])
            for entry, task in zip(tasks, plan["tasks"]):
                if task["role"] == "changer":
                    entry.update(accepted=False, detail="change_not_made: %s; %s" % (why, entry["detail"]))
                else:
                    entry.update(completed=None, accepted=None, detail="change_not_made: not judged, because " + why)
    else:
        # push_contention's tasks are independent: no pair, so each is
        # accepted when completed. What every contradiction task defines. Two tasks that want
        # different values of one default: each function gets its own value
        # and the default stays as it was. One task alone: the default
        # changes to its value. Keeping the default and setting the value
        # locally, which holds up whatever peers do, passes only the first.
        defaults = getattr(load("shelf.settings"), "DEFAULTS", None)
        pairs = {}
        for entry, task in zip(tasks, plan["tasks"]):
            if "pair" in task:
                pairs.setdefault(task["pair"], []).append((entry, task))
        for key, members in sorted(pairs.items()):
            old = members[0][1]["old"]
            present = isinstance(defaults, dict) and key in defaults
            if len(members) == 1:
                entry, task = members[0]
                changed = present and defaults[key] == task["value"]
                entry["accepted"] = entry["completed"] and changed
                if entry["completed"] and not changed:
                    entry["detail"] += ("; no other task wants %s, so the shared default should hold %s"
                                        % (key, json.dumps(task["value"])))
                continue
            kept = present and defaults[key] == old
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
        "interface_change": change,
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
