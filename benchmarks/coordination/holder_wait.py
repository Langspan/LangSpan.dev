#!/usr/bin/env python3
"""Refuse against wait and revalidate on `same-file-current-holder` (B28).

Two sessions share one checkout of the fixture and each adds one setting to
the same file, `shelf/settings.py`, so the second can meet the first's
admission while it is current. Two LangSpan arms differ only in the
workspace's conflict policy (ADR 0036):

- `langspan_refuse`: `policy set ... conflict refuse`, the default;
- `langspan_wait`: `policy set ... conflict wait`.

Each run starts from a fresh fixture, coordinator and state directory, and
appends one JSON line to `runs.jsonl` in the output directory, whatever
happened: completed, failed, timed out or broken. `--summarize DIR` reports
per arm the stop rate, retries and elapsed time with their sample sizes.

`--agent claude` starts real Claude Code sessions under the operator's own
sign-in, with permission prompts on and nobody to answer them, as `driver.py`
does; nothing approves a prompt. `--agent standin` (the default) makes no
model call: a scripted pair drives the real hook executable, for checking
the harness itself. Stand-in runs are not results.
"""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import driver  # noqa: E402

ARMS = ("langspan_refuse", "langspan_wait")
AGENTS = ("standin", "claude")
SCHEMA_VERSION = 1
WORKSPACE = "holder"
SETTINGS = "shelf/settings.py"
MAX_RUNS = 200
# The two settings, one per session. Both must be in the combined file.
SETTINGS_ADDED = (("low_stock", 5, "How few items count as low stock."),
                  ("max_order", 50, "The most items one order may hold."))
FIXTURE_DEFAULTS = {"page_size": 20, "timeout_s": 30, "retries": 3, "currency": "USD", "date_format": "%Y-%m-%d"}
TASK = (
    "You are session {index} of 2, working in a checkout of a small Python package that another session "
    "edits at the same time. Add the setting `\"{key}\": {value},` to `DEFAULTS` in `shelf/settings.py`, "
    "with the comment `# {why}` above it. Keep every other setting, including any the other session adds. "
    "Do not change any other file, and do not commit. No person will answer questions during this session.\n"
)
# The stand-in's hold: how long its first session keeps its admission open,
# as a write waiting at a permission prompt would.
STANDIN_HOLD_S = 0.4
STANDIN_RETRIES = 3
# Between a refusal and the stand-in's retry, as an agent takes a turn.
STANDIN_TURN_S = 0.2


def utc_now():
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def prepare(run_dir):
    """A fresh fixture checkout, committed, with nothing else in it."""
    checkout = run_dir / "checkout"
    shutil.copytree(HERE / "fixture", checkout, ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    driver.git("init", "-q", "-b", "main", cwd=checkout)
    driver.git("add", "-A", cwd=checkout)
    driver.git(*driver.GIT_IDENTITY, "commit", "-q", "-m", "Fixture", cwd=checkout)
    driver.exclude_claude(checkout)
    return checkout


def owner_grants(state_dir, on):
    """Write the coordinator's owner-only `config.json` with `owner_grants` set to `on`."""
    fd = os.open(Path(state_dir) / "config.json", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as config:
        json.dump({"owner_grants": on}, config)


def set_policy(langspan, state_dir, arm, checkout, wait_ms):
    driver.langspan_cli(langspan, state_dir, "workspace", "add", WORKSPACE, str(checkout),
                        "--request-id", "holder-workspace")
    args = ["policy", "set", WORKSPACE, "conflict"]
    args += ["wait", "--wait-ms", str(wait_ms)] if arm == "langspan_wait" else ["refuse"]
    # Setting a policy is an owner grant (issue #335): the operator allows it
    # for this one command and turns it off again before any session starts.
    owner_grants(state_dir, True)
    driver.langspan_cli(langspan, state_dir, *args, "--request-id", "holder-policy")
    owner_grants(state_dir, False)
    return json.loads(driver.langspan_cli(langspan, state_dir, "policy", WORKSPACE, "--json"))["outcome"]["rows"]


def added_line(key, value, why):
    return "    # %s\n    \"%s\": %d,\n" % (why, key, value)


def oracle(checkout):
    """Both settings are in `DEFAULTS`, with every setting the fixture had
    kept as it was. The fixture's own test lists the settings it ships, so
    it is not the judge here."""
    probe = ("import json, sys; sys.path.insert(0, '.'); from shelf.settings import DEFAULTS; "
             "print(json.dumps(DEFAULTS))")
    done = subprocess.run([sys.executable, "-B", "-I", "-c", probe], cwd=checkout, capture_output=True,
                          text=True, timeout=driver.ORACLE_SECONDS)
    try:
        defaults = json.loads(done.stdout)
    except ValueError:
        return {"settings": {}, "kept": False, "accepted": False,
                "error": (done.stderr or done.stdout).strip()[:2000]}
    present = {key: defaults.get(key) == value for key, value, _ in SETTINGS_ADDED}
    kept = all(defaults.get(key) == value for key, value in FIXTURE_DEFAULTS.items())
    return {"settings": present, "kept": kept, "accepted": all(present.values()) and kept}


# --- The stand-in pair ------------------------------------------------------

class Hook:
    """The real `langspan hook claude-code` for one checkout."""

    def __init__(self, langspan, state_dir, checkout):
        self.langspan, self.state_dir, self.checkout = langspan, state_dir, checkout

    def run(self, event):
        done = subprocess.run(
            [str(self.langspan), "hook", "claude-code", "--state-dir", str(self.state_dir), "--workspace",
             WORKSPACE, "--root", str(self.checkout), "--budget-ms", "4000"],
            input=json.dumps(event), capture_output=True, text=True, timeout=30,
            env=dict(os.environ, LANGSPAN_AUTOSTART="off"))
        diagnostic = json.loads(done.stderr.strip().splitlines()[-1])
        return diagnostic["coverage"]

    def event(self, session, call, phase, tool, tool_input):
        return {"session_id": session, "cwd": str(self.checkout), "hook_event_name": phase, "tool_name": tool,
                "tool_use_id": call, "tool_input": tool_input,
                "tool_response": {"filePath": tool_input.get("file_path")}}


def standin_session(hook, index, hold, log):
    """Read the file, then write it with this session's setting added. A
    refusal is followed by a fresh read and a retry, at most
    STANDIN_RETRIES times; then the session stops."""
    session = "00000000-0000-4000-8000-%012d" % index
    path = hook.checkout / SETTINGS
    key, value, why = SETTINGS_ADDED[index - 1]
    for attempt in range(STANDIN_RETRIES + 1):
        hook.run(hook.event(session, "read-%d-%d" % (index, attempt), "PostToolUse", "Read",
                            {"file_path": str(path)}))
        text = path.read_text(encoding="utf-8")
        content = text.replace("}\n", added_line(key, value, why) + "}\n", 1)
        tool_input = {"file_path": str(path), "content": content}
        call = "write-%d-%d" % (index, attempt)
        coverage = hook.run(hook.event(session, call, "PreToolUse", "Write", tool_input))
        log.append({"session": index, "attempt": attempt, "coverage": coverage})
        if coverage == "admitted":
            if hold:
                time.sleep(STANDIN_HOLD_S)
            # The host writes what the tool was given, as Claude Code's
            # Write does, over whatever is on disk now.
            path.write_text(content, encoding="utf-8")
            hook.run(hook.event(session, call, "PostToolUse", "Write", tool_input))
            break
        time.sleep(STANDIN_TURN_S)
    hook.run({"session_id": session, "cwd": str(hook.checkout), "hook_event_name": "Stop",
              "stop_hook_active": False})


def run_standin(langspan, state_dir, checkout, time_limit):
    hook = Hook(langspan, state_dir, checkout)
    log = []
    first = threading.Thread(target=standin_session, args=(hook, 1, True, log))
    first.start()
    time.sleep(STANDIN_HOLD_S / 4)
    standin_session(hook, 2, False, log)
    first.join(time_limit)
    return [{"session": 1, "exit": 0}, {"session": 2, "exit": 0}], log


# --- Real Claude Code sessions ---------------------------------------------

def run_claude(plan, state_dir, checkout, run_dir, time_limit):
    executable = os.environ.get(driver.CLAUDE_BIN_ENV) or shutil.which("claude")
    if not executable:
        raise RuntimeError("no Claude Code executable: put `claude` on PATH or set " + driver.CLAUDE_BIN_ENV)
    langspan = plan["langspan"]
    driver.install_hooks(langspan, state_dir, 1, checkout, run_dir / "hooks.jsonl")
    # `install_hooks` names the workspace of session 1; both sessions share
    # this one checkout, registered under WORKSPACE.
    settings = checkout / ".claude" / "settings.local.json"
    text = settings.read_text(encoding="utf-8").replace(driver.workspace_id(1), WORKSPACE)
    settings.write_text(text, encoding="utf-8")
    driver.install_skill(langspan, state_dir, [checkout], run_dir)
    env = dict(os.environ)
    env["PATH"] = str(Path(langspan).parent) + os.pathsep + env.get("PATH", "")
    env["LANGSPAN_STATE_DIR"] = str(state_dir)
    env[driver.MEMORY_ENV] = "1"
    if os.environ.get(driver.CLAUDE_CONFIG_ENV):
        env["CLAUDE_CONFIG_DIR"] = os.environ[driver.CLAUDE_CONFIG_ENV]
    processes = []
    for index, (key, value, why) in enumerate(SETTINGS_ADDED, 1):
        prompt = TASK.format(index=index, key=key, value=value, why=why)
        argv = driver.claude_argv(executable, prompt, "langspan", model=plan.get("model"),
                                  skill=driver.skill_text(checkout))
        transcript = open(run_dir / ("s%d.jsonl" % index), "wb")
        processes.append((index, transcript, subprocess.Popen(argv, cwd=checkout, env=env, stdout=transcript,
                                                              stderr=subprocess.STDOUT,
                                                              stdin=subprocess.DEVNULL)))
        if index == 1:
            time.sleep(plan["stagger_s"])
    sessions = []
    deadline = time.monotonic() + time_limit
    for index, transcript, process in processes:
        try:
            code = process.wait(max(1, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            process.kill()
            code = None
        transcript.close()
        sessions.append({"session": index, "exit": code})
    return sessions, None


# --- What the event log says -------------------------------------------------

def measure(events):
    """Refusals, waits and what followed each refusal, from the events."""
    kinds = [record["event"] for record in events]
    refused = [event for event in kinds if event.get("type") == "write_refused"]
    waited = [event for event in kinds if event.get("type") == "write_waited"]
    settled = [event for event in kinds if event.get("type") == "refusal_settled"]
    outcomes = {}
    for event in settled:
        outcomes[event["outcome"]] = outcomes.get(event["outcome"], 0) + 1
    return {
        "refusals": len(refused),
        "refusals_by_kind": {kind: sum(1 for event in refused if event["kind"] == kind)
                             for kind in sorted({event["kind"] for event in refused})},
        "waits": len(waited),
        "waits_by_result": {result: sum(1 for event in waited if event["result"] == result)
                            for result in sorted({event["result"] for event in waited})},
        "waited_ms": [event["waited_ms"] for event in waited],
        "settled": outcomes,
        "unsettled": len(refused) - len(settled),
        "retries": outcomes.get("retried_admitted", 0),
        "stopped": outcomes.get("stopped", 0),
        "ms_to_next_admitted": [event["ms_to_next_admitted"] for event in settled
                                if event.get("ms_to_next_admitted") is not None],
        "held": bool(waited) or any(event["kind"] == "conflict" for event in refused),
    }


def one_run(plan, index):
    run_dir = Path(plan["out"]) / ("%s-%03d" % (plan["arm"], index))
    run_dir.mkdir(parents=True)
    started, clock = utc_now(), time.monotonic()
    record = {"schema_version": SCHEMA_VERSION, "item": "same-file-current-holder", "arm": plan["arm"],
              "agent": plan["agent"], "run": index, "started_at": started, "status": "error", "error": None}
    coordinator, state_dir = None, None
    try:
        checkout = prepare(run_dir)
        # A Unix socket path holds about 100 bytes, and --out can be deep (a
        # temporary folder on macOS is): keep the state folder short.
        state_dir = Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp"))
        coordinator = driver.Coordinator(plan["langspan"], state_dir, run_dir / "daemon.log")
        record["policy"] = set_policy(plan["langspan"], state_dir, plan["arm"], checkout, plan["wait_ms"])
        if plan["agent"] == "standin":
            sessions, log = run_standin(plan["langspan"], state_dir, checkout, plan["time_limit"])
            record["standin"] = log
        else:
            sessions, _ = run_claude(plan, state_dir, checkout, run_dir, plan["time_limit"])
        record["sessions"] = sessions
        record["measures"] = measure(coordinator.events())
        record["oracle"] = oracle(checkout)
        if any(session["exit"] is None for session in sessions):
            record["status"] = "timed_out"
        else:
            record["status"] = "completed" if record["oracle"]["accepted"] else "failed"
    except Exception as error:  # Every run is kept, a broken one too.
        record["error"] = "%s: %s" % (type(error).__name__, str(error)[:2000])
    finally:
        if coordinator is not None:
            coordinator.stop()
        if state_dir is not None:
            shutil.rmtree(state_dir, ignore_errors=True)
    record["elapsed_s"] = round(time.monotonic() - clock, 3)
    record["ended_at"] = utc_now()
    with open(Path(plan["out"]) / "runs.jsonl", "a", encoding="utf-8") as journal:
        journal.write(json.dumps(record, sort_keys=True) + "\n")
    return record


def summarize(out):
    """Per arm: runs, runs that met a holder, stop rate among those, retries
    and elapsed time. Every run in the journal counts, failures included."""
    runs = [json.loads(line) for line in (Path(out) / "runs.jsonl").read_text(encoding="utf-8").splitlines()
            if line.strip()]
    report = {}
    for arm in ARMS:
        mine = [run for run in runs if run["arm"] == arm]
        measured = [run for run in mine if run.get("measures")]
        held = [run for run in measured if run["measures"]["held"]]
        stopped = [run for run in held if run["measures"]["stopped"]]
        elapsed = [run["elapsed_s"] for run in mine]
        times = [ms for run in measured for ms in run["measures"]["ms_to_next_admitted"]]
        report[arm] = {
            "runs": len(mine),
            "by_status": {status: sum(1 for run in mine if run["status"] == status)
                          for status in sorted({run["status"] for run in mine})},
            "standin_runs": sum(1 for run in mine if run.get("agent") == "standin"),
            "held_runs": len(held),
            "stop_rate": (len(stopped) / len(held)) if held else None,
            "stopped_runs": len(stopped),
            "retries": sum(run["measures"]["retries"] for run in measured),
            "refusals": sum(run["measures"]["refusals"] for run in measured),
            "waits": sum(run["measures"]["waits"] for run in measured),
            "median_elapsed_s": statistics.median(elapsed) if elapsed else None,
            "median_ms_to_next_admitted": statistics.median(times) if times else None,
            "ms_to_next_admitted_n": len(times),
        }
    return report


def text(report):
    lines = ["arm               runs  held  stopped  stop rate  retries  refusals  waits  median s  median ms to retry"]
    for arm, row in report.items():
        rate = "-" if row["stop_rate"] is None else "%.2f" % row["stop_rate"]
        elapsed = "-" if row["median_elapsed_s"] is None else "%.1f" % row["median_elapsed_s"]
        retry = "-" if row["median_ms_to_next_admitted"] is None else "%d (n=%d)" % (
            row["median_ms_to_next_admitted"], row["ms_to_next_admitted_n"])
        lines.append("%-17s %4d  %4d  %7d  %9s  %7d  %8d  %5d  %8s  %s" % (
            arm, row["runs"], row["held_runs"], row["stopped_runs"], rate, row["retries"], row["refusals"],
            row["waits"], elapsed, retry))
        if row["standin_runs"]:
            lines.append("  %d of these runs used the stand-in: a harness check, not a result" % row["standin_runs"])
    lines.append("Stop rate: runs where a session stopped after a refusal, of the runs that met a holder.")
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--arm", choices=ARMS)
    parser.add_argument("--agent", choices=AGENTS, default="standin")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--langspan", default=shutil.which("langspan"))
    parser.add_argument("--wait-ms", type=int, default=2000)
    parser.add_argument("--stagger-s", type=float, default=5.0)
    parser.add_argument("--time-limit", type=int, default=900)
    parser.add_argument("--model", default=os.environ.get(driver.CLAUDE_MODEL_ENV))
    parser.add_argument("--summarize", type=Path, metavar="DIR")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.summarize:
        report = summarize(args.summarize)
        print(json.dumps(report, indent=2, sort_keys=True) if args.json else text(report))
        return 0
    if not args.arm or not args.out:
        parser.error("--arm and --out are required to run; --summarize DIR reports")
    if not 1 <= args.runs <= MAX_RUNS:
        parser.error("--runs is 1 to %d" % MAX_RUNS)
    if not args.langspan:
        parser.error("--langspan PATH is required: no langspan on PATH")
    plan = {"arm": args.arm, "agent": args.agent, "out": str(args.out.resolve()),
            "langspan": str(Path(args.langspan).resolve()), "wait_ms": args.wait_ms,
            "stagger_s": args.stagger_s, "time_limit": args.time_limit, "model": args.model}
    args.out.mkdir(parents=True, exist_ok=True)
    first = 1 + sum(1 for path in args.out.iterdir() if path.name.startswith(args.arm + "-"))
    for index in range(first, first + args.runs):
        record = one_run(plan, index)
        print(json.dumps({"arm": record["arm"], "run": record["run"], "status": record["status"],
                          "elapsed_s": record["elapsed_s"], "error": record["error"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
