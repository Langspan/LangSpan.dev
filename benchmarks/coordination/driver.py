#!/usr/bin/env python3
"""Coordination benchmark driver: one run of one arm, task family, N and seed.

Without ``--run`` it prints the plan (tasks, prompts, branches) and executes
nothing. With ``--run`` it prepares a fresh fixture repository and one Git
worktree per session, sets up the arm, hands the N sessions and the
integration oracle to ``runner.py`` (which launches, bounds and records
them), then writes a schema-valid run record. ``--agent standin`` (the
default) runs the dry-run stand-in, which makes no model call;
``--agent claude`` starts real Claude Code sessions and is for an operator
to start by hand.
"""

import argparse
import ast
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import runner  # noqa: E402
import tasks as families  # noqa: E402

ARMS = ("worktrees", "awareness_only", "langspan")
AGENTS = ("standin", "claude")
GIT_IDENTITY = ["-c", "user.name=Benchmark", "-c", "user.email=benchmark@example.invalid",
                "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]
MAX_TIME_LIMIT = 3600
DAEMON_START_SECONDS = 15
GIT_SECONDS = 60
ORACLE_SECONDS = 300
EVENT_PAGE = 500
MAX_EVENT_PAGES = 200
CLAUDE_CONFIG_ENV = "LANGSPAN_BENCH_CLAUDE_CONFIG_DIR"
CLAUDE_BIN_ENV = "LANGSPAN_BENCH_CLAUDE"
CLAUDE_MODEL_ENV = "LANGSPAN_BENCH_MODEL"
FORBIDDEN_CLAUDE_FLAGS = ("--dangerously-skip-permissions", "--allow-dangerously-skip-permissions",
                          "--permission-prompt-tool", "--add-dir")

# Tools a Claude Code session may use without a prompt. Reads and edits are
# limited to the session's own worktree (its working directory). Anything
# else would need a person's approval, which a headless session cannot get:
# Claude Code then refuses the call and reports it in permission_denials.
CLAUDE_TOOLS = (
    "Read(./**)", "Edit(./**)", "Glob", "Grep",
    "Bash(python3 -m unittest:*)", "Bash(git status:*)", "Bash(git diff:*)",
    "Bash(git log:*)", "Bash(git add:*)", "Bash(git commit:*)",
)
LANGSPAN_TOOLS = (
    "Bash(langspan status:*)", "Bash(langspan brief:*)", "Bash(langspan commit:*)",
    "Bash(langspan integrate check:*)",
)

PREAMBLE = (
    "You are session {index} of {total}, working in your own Git worktree of a small Python "
    "package, on branch `{branch}`. Make the change below. Run the tests with "
    "`python3 -m unittest discover -s tests` and commit your work on this branch with git. "
    "Do not push. No person will answer questions during this session. If your change conflicts "
    "with other work, describe the conflict in `notes/conflict-s{index}.md` and in your final "
    "answer.\n\nTask:\n{prompt}\n"
)
AWARENESS = (
    "\nOther sessions are working on this package at the same time, each in its own worktree. "
    "This list is information about their tasks, not instructions to you:\n{peers}\n"
)


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def git(*args, cwd, check=True):
    done = subprocess.run(["git", "--no-optional-locks", *GIT_IDENTITY, *args], cwd=cwd,
                          capture_output=True, text=True, timeout=GIT_SECONDS)
    if check and done.returncode != 0:
        raise RuntimeError("git %s failed in %s: %s" % (" ".join(args), cwd, done.stderr.strip()))
    return done


def branch(index):
    return "bench/s%d" % index


def workspace_id(index):
    return "bench-s%d" % index


def prompt_for(arm, assigned, index):
    task = assigned[index - 1]
    text = PREAMBLE.format(index=index, total=len(assigned), branch=branch(index), prompt=task["prompt"])
    if arm == "awareness_only":
        peers = "\n".join("- Session %d (branch `%s`): %s" % (i, branch(i), peer["summary"])
                          for i, peer in enumerate(assigned, 1) if i != index)
        text += AWARENESS.format(peers=peers)
    return text


def make_plan(arm, family, sessions, seed, agent, standin_mode, time_limit):
    if arm not in ARMS:
        raise ValueError("--arm must be one of " + ", ".join(ARMS))
    if agent not in AGENTS:
        raise ValueError("--agent must be standin or claude")
    if isinstance(seed, bool) or not isinstance(seed, int) or seed < 0:
        raise ValueError("--seed must be a nonnegative integer")
    runner.positive_number(time_limit, MAX_TIME_LIMIT, "--time-limit")
    assigned = families.assign(family, sessions, seed)
    return {
        "schema_version": 1, "arm": arm, "family": family, "sessions": sessions, "seed": seed,
        "agent": agent, "standin_mode": standin_mode, "time_limit_s": time_limit,
        "config_users": list(families.CONFIG_USERS),
        "tasks": assigned,
        "branches": [branch(i) for i in range(1, sessions + 1)],
        "prompts": [prompt_for(arm, assigned, i) for i in range(1, sessions + 1)],
    }


def claude_argv(executable, prompt, arm, model=None):
    """The exact Claude Code command line for one session. Permission
    prompts stay on (`--permission-mode default`); nothing approves them."""
    argv = [executable, "-p", prompt, "--permission-mode", "default", "--output-format", "json"]
    if model:
        argv += ["--model", model]
    tools = CLAUDE_TOOLS + (LANGSPAN_TOOLS if arm == "langspan" else ())
    argv += ["--allowedTools", *tools]
    assert not set(argv) & set(FORBIDDEN_CLAUDE_FLAGS)
    return argv


def user_level_langspan(config_dir):
    """What in a Claude Code configuration would bring LangSpan into every
    session: user-level hook commands that run it, or its skill."""
    found = []
    skill = Path(config_dir) / "skills" / "langspan" / "SKILL.md"
    if skill.exists():
        found.append(str(skill))
    path = Path(config_dir) / "settings.json"
    if not path.is_file():
        return found
    try:
        settings, _ = runner.bounded_json(path, runner.MAX_MANIFEST_BYTES)
    except (OSError, ValueError) as error:
        raise ValueError("cannot read %s to check for LangSpan hooks: %s" % (path, error))
    for entries in (settings.get("hooks") or {}).values():
        for entry in entries or []:
            for hook in entry.get("hooks", []):
                if re.search(r"langspan\S*\s+hook\b", str(hook.get("command", ""))):
                    found.append(hook["command"])
    return found


def check_output_location(output):
    repository = next((parent for parent in HERE.parents if (parent / ".git").exists()), None)
    if repository is not None and output.is_relative_to(repository) and not output.is_relative_to(HERE / "runs"):
        raise ValueError("keep results outside the repository or under benchmarks/coordination/runs/")


def langspan_commit():
    repository = next((parent for parent in HERE.parents if (parent / ".git").exists()), None)
    if repository is None:
        return None
    head = git("rev-parse", "HEAD", cwd=repository, check=False)
    if head.returncode != 0:
        return None
    dirty = git("status", "--porcelain", "--untracked-files=no", cwd=repository, check=False).stdout.strip()
    return head.stdout.strip() + ("-dirty" if dirty else "")


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


# --- Preparation -----------------------------------------------------------

def prepare_fixture(work, sessions):
    repo = work / "repo"
    shutil.copytree(HERE / "fixture", repo, ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    git("init", "-q", "-b", "main", cwd=repo)
    with open(repo / ".git" / "info" / "exclude", "a", encoding="utf-8") as exclude:
        exclude.write(".claude/\n")
    git("add", "-A", cwd=repo)
    git("commit", "-q", "-m", "Fixture", cwd=repo)
    base = git("rev-parse", "HEAD", cwd=repo).stdout.strip()
    worktrees = []
    for index in range(1, sessions + 1):
        path = work / ("s%d" % index)
        git("worktree", "add", "-q", "-b", branch(index), str(path), "main", cwd=repo)
        worktrees.append(path)
    return repo, base, worktrees


def langspan_cli(langspan, state_dir, *args, timeout=30, env=None):
    done = subprocess.run([str(langspan), *args, "--state-dir", str(state_dir)], capture_output=True,
                          text=True, timeout=timeout, env=env)
    if done.returncode != 0:
        raise RuntimeError("langspan %s failed (%d): %s" % (" ".join(args), done.returncode,
                                                            (done.stderr or done.stdout).strip()[:2000]))
    return done.stdout


def install_langspan(langspan, state_dir, worktrees, work):
    """Project hooks in each worktree, and the LangSpan skill beside them."""
    for index, path in enumerate(worktrees, 1):
        langspan_cli(langspan, state_dir, "hooks", "install", "claude-code", "--workspace",
                     workspace_id(index), "--root", str(path), "--apply")
    # Render the skill through the real installer into a throwaway home, so
    # neither the operator's home nor their Claude Code configuration changes.
    home = work / "skill-home"
    (home / ".claude").mkdir(parents=True)
    env = {key: value for key, value in os.environ.items()
           if key not in ("CLAUDE_CONFIG_DIR", "CODEX_HOME", "GEMINI_CLI_HOME")}
    langspan_cli(langspan, state_dir, "install", "--home", str(home), "--hosts", "claude-code",
                 "--skill", "--apply", env=env)
    skill = home / ".claude" / "skills" / "langspan" / "SKILL.md"
    if not skill.is_file():
        raise RuntimeError("langspan install --skill wrote no skill at " + str(skill))
    for path in worktrees:
        target = path / ".claude" / "skills" / "langspan"
        target.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(skill, target / "SKILL.md")


class Coordinator:
    """One LangSpan daemon for the run, in its own process group."""

    def __init__(self, langspan, state_dir, log_path):
        self.langspan, self.state_dir = langspan, state_dir
        self.log = open(log_path, "ab")
        self.process = subprocess.Popen([str(langspan), "daemon", "--state-dir", str(state_dir)],
                                        stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log,
                                        start_new_session=True)
        deadline = time.monotonic() + DAEMON_START_SECONDS
        while True:
            probe = subprocess.run([str(langspan), "status", "--state-dir", str(state_dir)],
                                   capture_output=True, timeout=10)
            if probe.returncode == 0:
                return
            if self.process.poll() is not None or time.monotonic() > deadline:
                self.stop()
                raise RuntimeError("the LangSpan coordinator did not start; see " + str(log_path))
            time.sleep(.1)

    def events(self):
        """Every persisted event, in order, page by page."""
        events, after = [], 0
        for _ in range(MAX_EVENT_PAGES):
            page = json.loads(langspan_cli(self.langspan, self.state_dir, "events", "--after", str(after),
                                           "--limit", str(EVENT_PAGE), "--json"))
            records = (page.get("outcome") or page).get("events") or []
            if not records:
                return events
            events.extend(records)
            after = records[-1]["seq"]
        raise RuntimeError("the event log has more than %d pages" % MAX_EVENT_PAGES)

    def stop(self):
        if self.process.poll() is None:
            runner.signal_group(self.process, signal.SIGTERM)
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                runner.signal_group(self.process, signal.SIGKILL)
                self.process.wait(timeout=5)
        self.log.close()


# --- Phases the runner starts ----------------------------------------------

def session_main(plan_path, index):
    """Become session INDEX's agent process (the runner's agent command)."""
    plan = json.loads(Path(plan_path).read_text(encoding="utf-8"))
    if plan["agent"] == "standin":
        argv = [sys.executable, "-I", "-B", str(HERE / "standin_agent.py"), str(plan_path), str(index),
                plan["standin_mode"]]
        os.execv(argv[0], argv)
    config_dir = os.environ.get(CLAUDE_CONFIG_ENV)
    if not config_dir:
        print("set %s to the Claude Code configuration directory to use" % CLAUDE_CONFIG_ENV, file=sys.stderr)
        return 2
    env = dict(os.environ, CLAUDE_CONFIG_DIR=config_dir)
    if plan["arm"] == "langspan":
        # The session's own `langspan` commands must reach the run's
        # coordinator, the one its hooks name, not a default state directory.
        env["PATH"] = str(Path(plan["langspan"]).parent) + os.pathsep + env.get("PATH", "")
        env["LANGSPAN_STATE_DIR"] = plan["state_dir"]
    argv = claude_argv(os.environ.get(CLAUDE_BIN_ENV, "claude"), plan["prompts"][index - 1], plan["arm"],
                       os.environ.get(CLAUDE_MODEL_ENV))
    executable = shutil.which(argv[0], path=env["PATH"]) or argv[0]
    os.execve(executable, argv, env)


def unit_source(text, unit):
    """The source of one owned unit: a whole file, or one top-level function."""
    if text is None:
        return None
    if "::" not in unit:
        return text
    name = unit.split("::", 1)[1]
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return "<unparsable>" + text
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == name:
            return ast.get_source_segment(text, node)
    return None


def show(repo, revision, path):
    done = git("show", "%s:%s" % (revision, path), cwd=repo, check=False)
    return done.stdout if done.returncode == 0 else None


def duplicates(plan, repo, base):
    """Sessions that changed a unit another session's task owns."""
    found = []
    for owner, task in enumerate(plan["tasks"], 1):
        for unit in task["owns"]:
            path = unit.split("::", 1)[0]
            before = unit_source(show(repo, base, path), unit)
            for other in range(1, plan["sessions"] + 1):
                if other != owner and unit_source(show(repo, branch(other), path), unit) != before:
                    found.append("session %d changed %s, which session %d's task owns" % (other, unit, owner))
    return found


def integrate_main(plan_path):
    """Commit what each session left, merge every branch, run the oracle."""
    plan_path = Path(plan_path)
    plan = json.loads(plan_path.read_text(encoding="utf-8"))
    work, repo, base = Path(plan["work"]), Path(plan["repo"]), plan["base"]
    for index in range(1, plan["sessions"] + 1):
        worktree = work / ("s%d" % index)
        git("add", "-A", cwd=worktree)
        if git("diff", "--cached", "--quiet", cwd=worktree, check=False).returncode != 0:
            git("commit", "-q", "-m", "Benchmark: changes session %d left uncommitted" % index, cwd=worktree)
    checkout = work / "integration"
    git("worktree", "add", "-q", "-b", "bench/integration", str(checkout), base, cwd=repo)
    integration = []
    for index in range(1, plan["sessions"] + 1):
        merge = git("merge", "--no-ff", "--no-edit", "-m", "Merge session %d" % index, branch(index),
                    cwd=checkout, check=False)
        files = []
        if merge.returncode != 0:
            files = git("diff", "--name-only", "--diff-filter=U", cwd=checkout, check=False).stdout.split()
            git("merge", "--abort", cwd=checkout, check=False)
        integration.append({"session": index, "merged": merge.returncode == 0, "files": files})
    plan["integration"] = integration
    plan["duplicates"] = duplicates(plan, repo, base)
    integrated = plan_path.with_name("integration.json")
    integrated.write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
    verdict = Path(os.environ.get("LANGSPAN_BENCH_RUN_DIR", work)) / "oracle.json"
    done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "oracle.py"), str(integrated),
                           str(checkout), str(verdict)], timeout=ORACLE_SECONDS)
    print(json.dumps({"integration": integration, "oracle_exit": done.returncode}))
    return done.returncode


# --- The run record ----------------------------------------------------------

def last_json_object(text):
    for line in reversed(text.strip().splitlines()):
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict):
            return value
    try:
        value = json.loads(text)
        return value if isinstance(value, dict) else None
    except ValueError:
        return None


def tokens_of(result):
    usage = (result or {}).get("usage")
    if not isinstance(usage, dict):
        return None
    fields = {"input": "input_tokens", "output": "output_tokens",
              "cache_creation": "cache_creation_input_tokens", "cache_read": "cache_read_input_tokens"}
    values = {name: usage.get(key, 0) for name, key in fields.items()}
    if not all(type(value) is int and value >= 0 for value in values.values()):
        return None
    return values


def refusal_items(events):
    """LangSpan's refusals, each with what the same session did next."""
    items = []
    for position, record in enumerate(events):
        event = record.get("event", {})
        if event.get("type") != "write_refused":
            continue
        match = re.fullmatch(r"bench-s(\d+)", event.get("workspace_id", ""))
        paths = set(event.get("paths", []))
        later = "no_later_admission"
        for following in events[position + 1:]:
            lease = following.get("event", {}).get("lease") if following.get("event", {}).get("type") == "lease_granted" else None
            if lease and lease.get("session_id") == event.get("session_id") and (not paths or paths & set(lease.get("paths", []))):
                later = "admitted_later"
                break
        items.append({"seq": record.get("seq", 0), "session": int(match.group(1)) if match else None,
                      "kind": str(event.get("kind")), "paths": sorted(paths),
                      "peer_session": event.get("peer_session_id"), "followed_by": later})
    return items


def first_line(path):
    try:
        text = path.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None
    return text.splitlines()[0][:200] if text else None


def build_record(plan, final, runner_dir, run_dir, events, started_at, total_elapsed, error=None):
    commands = {(c["phase"], c["id"]): c for c in final["commands"]} if final else {}
    oracle_path = run_dir / "oracle.json" if run_dir else None
    oracle = json.loads(oracle_path.read_text(encoding="utf-8")) if oracle_path and oracle_path.is_file() else None
    integration = {entry["session"]: entry for entry in plan.get("integration", [])}
    integration_file = Path(plan["plan_path"]).with_name("integration.json")
    if integration_file.is_file():
        integrated_plan = json.loads(integration_file.read_text(encoding="utf-8"))
        integration = {entry["session"]: entry for entry in integrated_plan["integration"]}
        duplicate_details = integrated_plan["duplicates"]
    else:
        duplicate_details = None
    refusals = refusal_items(events) if events is not None else []
    tasks, models, token_list = [], set(), []
    for index, task in enumerate(plan["tasks"], 1):
        command = commands.get(("agent", "s%d" % index))
        result = None
        if command is not None:
            result = last_json_object((runner_dir / command["stdout_path"]).read_text(encoding="utf-8", errors="replace"))
        verdict = oracle["tasks"][index - 1] if oracle else {}
        status = command["status"] if command else None
        if status is None:
            stopped = None
        elif status != "succeeded":
            stopped = True
        elif result is None:
            stopped = None
        else:
            stopped = bool(result.get("is_error")) or result.get("subtype") != "success"
        answer = result.get("result") if isinstance(result, dict) else None
        asked = answer.strip().endswith("?") if isinstance(answer, str) else None
        denials = result.get("permission_denials") if isinstance(result, dict) else None
        tokens = tokens_of(result)
        token_list.append(tokens)
        if isinstance(result, dict) and isinstance(result.get("modelUsage"), dict):
            models.update(result["modelUsage"])
        tasks.append({
            "session": index, "task_id": task["id"], "role": task["role"], "branch": branch(index),
            "process_status": status, "merged": bool(integration.get(index, {}).get("merged")),
            "completed": verdict.get("completed"), "accepted": verdict.get("accepted"),
            "detail": verdict.get("detail"), "stopped": stopped, "asked_question": asked,
            "permission_denials": len(denials) if isinstance(denials, list) else None,
            "tokens": tokens,
            "refusals": sum(item["session"] == index for item in refusals) if events is not None else None,
        })

    def total(field):
        values = [task[field] for task in tasks]
        return None if any(value is None for value in values) else sum(int(value) for value in values)

    reported = [tokens for tokens in token_list if tokens is not None]
    def version(name):
        command = commands.get(("version", name))
        return first_line(runner_dir / command["stdout_path"]) if command and command["status"] == "succeeded" else None
    record = {
        "schema_version": 1, "record_type": "coordination_run",
        "run_id": final["run_id"] if final else plan["run_id"],
        "arm": plan["arm"], "family": plan["family"], "sessions": plan["sessions"], "seed": plan["seed"],
        "agent": "standin" if plan["agent"] == "standin" else "claude-code",
        "dry_run": plan["agent"] == "standin",
        "status": final["status"] if final else "setup_failed",
        "started_at": started_at,
        "elapsed_s": final["agent_phase_elapsed_ns"] / 1e9 if final and final["agent_phase_elapsed_ns"] is not None else None,
        "total_elapsed_s": total_elapsed, "time_limit_s": plan["time_limit_s"],
        "versions": {
            "agent_cli": version("agent"), "models": sorted(models),
            "langspan_commit": plan.get("langspan_commit"),
            "langspan_binary_sha256": plan.get("langspan_sha256"),
            "git": version("git"), "python": sys.version.split()[0],
        },
        "tasks": tasks, "tasks_total": len(tasks),
        "tasks_completed": sum(bool(task["completed"]) for task in tasks) if oracle else None,
        "integrated_tests": oracle["integrated_tests"] if oracle else "unknown",
        "merge_conflicts": [{"session": s, "files": e["files"]} for s, e in sorted(integration.items()) if not e["merged"]],
        "semantic_conflicts": len(oracle["semantic_conflicts"]) if oracle else None,
        "semantic_conflict_details": oracle["semantic_conflicts"] if oracle else [],
        "contradictions_reported": oracle["contradictions_reported"] if oracle else [],
        "duplicate_work": len(duplicate_details) if duplicate_details is not None else None,
        "duplicate_work_details": duplicate_details or [],
        "fixture_tests": oracle["fixture_tests"] if oracle else "unknown",
        "stops": total("stopped"), "questions": total("asked_question"),
        "permission_denials": total("permission_denials"),
        "refusals": {
            "status": "not_applicable" if plan["arm"] != "langspan" else ("reported" if events is not None else "unknown"),
            "count": len(refusals) if events is not None else None,
            "items": refusals,
            "sessions_registered": sum(r.get("event", {}).get("type") == "session_registered" for r in events) if events is not None else None,
            "admissions": sum(r.get("event", {}).get("type") == "lease_granted" for r in events) if events is not None else None,
        },
        "tokens": {
            "status": "reported" if len(reported) == len(tasks) else ("partial" if reported else "unknown"),
            "total": {key: sum(t[key] for t in reported) for key in ("input", "output", "cache_creation", "cache_read")}
            if len(reported) == len(tasks) else None,
        },
        "evidence": {
            "runner_journal": "runner/runs.jsonl" if (runner_dir / "runs.jsonl").is_file() else None,
            "plan": "plan.json",
            "oracle": str(oracle_path.relative_to(Path(plan["output"]))) if oracle else None,
            "events": "events.jsonl" if events is not None else None,
        },
    }
    if plan["agent"] == "standin":
        record["standin_mode"] = plan["standin_mode"]
    if error:
        record["error"] = error
    return record


def validate(instance, schema, root=None, where="record"):
    """A small JSON Schema check: the keywords run-record.schema.json uses."""
    root = root or schema
    if "$ref" in schema:
        schema = root["$defs"][schema["$ref"].rsplit("/", 1)[1]]
    kinds = schema.get("type")
    if kinds is not None:
        kinds = [kinds] if isinstance(kinds, str) else kinds
        checks = {"object": lambda v: isinstance(v, dict), "array": lambda v: isinstance(v, list),
                  "string": lambda v: isinstance(v, str), "boolean": lambda v: isinstance(v, bool),
                  "null": lambda v: v is None,
                  "integer": lambda v: type(v) is int,
                  "number": lambda v: type(v) in (int, float)}
        if not any(checks[kind](instance) for kind in kinds):
            raise ValueError("%s: expected %s, got %r" % (where, "/".join(kinds), instance))
    if "const" in schema and instance != schema["const"]:
        raise ValueError("%s: expected %r" % (where, schema["const"]))
    if "enum" in schema and instance not in schema["enum"]:
        raise ValueError("%s: %r is not one of %r" % (where, instance, schema["enum"]))
    if "minimum" in schema and type(instance) in (int, float) and instance < schema["minimum"]:
        raise ValueError("%s: below %r" % (where, schema["minimum"]))
    if "pattern" in schema and isinstance(instance, str) and not re.fullmatch(schema["pattern"], instance):
        raise ValueError("%s: does not match %s" % (where, schema["pattern"]))
    if isinstance(instance, dict):
        missing = set(schema.get("required", ())) - instance.keys()
        if missing:
            raise ValueError("%s: missing %s" % (where, sorted(missing)))
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False and instance.keys() - properties.keys():
            raise ValueError("%s: unexpected %s" % (where, sorted(instance.keys() - properties.keys())))
        for key, value in instance.items():
            if key in properties:
                validate(value, properties[key], root, where + "." + key)
    if isinstance(instance, list) and "items" in schema:
        for position, value in enumerate(instance):
            validate(value, schema["items"], root, "%s[%d]" % (where, position))


def load_schema():
    return json.loads((HERE / "run-record.schema.json").read_text(encoding="utf-8"))


# --- One run -----------------------------------------------------------------

def manifest_for(plan, worktrees, repo):
    python = sys.executable
    driver = str(Path(__file__).resolve())
    agent_version = ([python, "-I", "-B", "-c", "print('standin-agent')"] if plan["agent"] == "standin"
                     else [os.environ.get(CLAUDE_BIN_ENV, "claude"), "--version"])
    return {
        "schema_version": 1, "benchmark_id": "coordination-k1",
        "items": [{
            "id": "%s-n%d-seed%d" % (plan["family"], plan["sessions"], plan["seed"]),
            "description": "K1 %s, %d sessions, seed %d, %s agent" % (
                plan["family"], plan["sessions"], plan["seed"], plan["agent"]),
            "arms": {plan["arm"]: {
                "versions": [
                    {"id": "git", "argv": ["git", "--version"], "cwd": str(repo), "timeout_seconds": 10},
                    {"id": "agent", "argv": agent_version, "cwd": str(repo), "timeout_seconds": 30},
                ],
                "agents": [
                    {"id": "s%d" % index, "argv": [python, "-I", "-B", driver, "session", plan["plan_path"], str(index)],
                     "cwd": str(path), "timeout_seconds": plan["time_limit_s"]}
                    for index, path in enumerate(worktrees, 1)
                ],
                "checks": [
                    {"id": "integrate", "argv": [python, "-I", "-B", driver, "integrate", plan["plan_path"]],
                     "cwd": str(repo), "timeout_seconds": ORACLE_SECONDS + 120, "assesses": "integrated_correctness"},
                ],
            }},
        }],
    }


def run(args):
    started_at, started = utc_now(), time.monotonic()
    output = args.output.resolve()
    check_output_location(output)
    plan = make_plan(args.arm, args.family, args.sessions, args.seed, args.agent, args.standin_mode, args.time_limit)
    langspan = None
    if args.agent == "claude":
        config_dir = os.environ.get(CLAUDE_CONFIG_ENV)
        if not config_dir or not Path(config_dir).is_dir():
            raise ValueError("--agent claude needs %s set to an existing Claude Code configuration directory" % CLAUDE_CONFIG_ENV)
        found = user_level_langspan(config_dir)
        if found:
            raise ValueError("%s has LangSpan hooks or the LangSpan skill for every session (%s), which would put "
                             "LangSpan in every arm. Use a Claude Code configuration without them."
                             % (config_dir, "; ".join(found)[:500]))
    if args.arm == "langspan":
        if args.langspan is None:
            raise ValueError("the langspan arm needs --langspan PATH to the binary built from this checkout")
        langspan = args.langspan.resolve()
        if not os.access(langspan, os.X_OK):
            raise ValueError("--langspan is not an executable file: " + str(langspan))
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    work = output / "work"
    work.mkdir(mode=0o700)
    plan.update(output=str(output), work=str(work), plan_path=str(output / "plan.json"),
                langspan_commit=langspan_commit(),
                langspan=str(langspan) if langspan else None,
                langspan_sha256=sha256_file(langspan) if langspan else None)
    if args.agent == "claude":
        # The exact command each session runs, with its prompt from `prompts`.
        plan["agent_command"] = claude_argv(os.environ.get(CLAUDE_BIN_ENV, "claude"), "<prompts[N-1]>",
                                            plan["arm"], os.environ.get(CLAUDE_MODEL_ENV))
    coordinator, state_dir, events, final, error = None, None, None, None, None
    runner_dir, run_dir = output / "runner", None
    try:
        repo, base, worktrees = prepare_fixture(work, plan["sessions"])
        plan.update(repo=str(repo), base=base)
        if langspan:
            # A Unix socket path holds about 100 bytes: keep the state short.
            state_dir = Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp"))
            plan["state_dir"] = str(state_dir)
            install_langspan(langspan, state_dir, worktrees, work)
            coordinator = Coordinator(langspan, state_dir, output / "coordinator.log")
        (output / "plan.json").write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
        manifest = output / "manifest.json"
        manifest.write_text(json.dumps(manifest_for(plan, worktrees, repo), indent=2) + "\n", encoding="utf-8")
        runner.run_manifest(manifest, runner_dir)
        rows = [json.loads(line) for line in (runner_dir / "runs.jsonl").read_text(encoding="utf-8").splitlines()]
        final = next(row for row in rows if row["record_type"] == "run_finished")
        run_dir = runner_dir / final["run_id"]
        if coordinator:
            events = coordinator.events()
            with open(output / "events.jsonl", "w", encoding="utf-8") as sink:
                for record in events:
                    sink.write(json.dumps(record) + "\n")
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError, StopIteration) as failure:
        error = "%s: %s" % (type(failure).__name__, failure)
    finally:
        if coordinator:
            coordinator.stop()
        if state_dir:
            shutil.rmtree(state_dir, ignore_errors=True)
    if not (output / "plan.json").exists():
        (output / "plan.json").write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
    plan["run_id"] = final["run_id"] if final else os.urandom(16).hex()
    record = build_record(plan, final, runner_dir, run_dir, events, started_at, time.monotonic() - started, error)
    validate(record, load_schema())
    (output / "record.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    return record


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ["session"] and len(argv) == 3:
        return session_main(argv[1], int(argv[2]))
    if argv[:1] == ["integrate"] and len(argv) == 2:
        return integrate_main(argv[1])
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--arm", required=True, choices=ARMS)
    parser.add_argument("--family", required=True, choices=families.FAMILIES)
    parser.add_argument("--sessions", required=True, type=int, choices=families.SESSION_COUNTS)
    parser.add_argument("--seed", required=True, type=int)
    parser.add_argument("--agent", default="standin", choices=AGENTS)
    parser.add_argument("--standin-mode", default="naive", choices=("naive", "coordinated"))
    parser.add_argument("--time-limit", type=float, default=1200, help="seconds per session (at most 3600)")
    parser.add_argument("--langspan", type=Path, help="the langspan binary, for the langspan arm")
    parser.add_argument("--output", type=Path, help="a new result directory; required with --run")
    parser.add_argument("--run", action="store_true", help="prepare and run; without it, only print the plan")
    args = parser.parse_args(argv)
    try:
        if not args.run:
            plan = make_plan(args.arm, args.family, args.sessions, args.seed, args.agent, args.standin_mode, args.time_limit)
            print(json.dumps({"mode": "plan_only", "executed_commands": 0,
                              "sessions": [{"session": i, "branch": b, "task": t["id"], "prompt": p}
                                           for i, (b, t, p) in enumerate(zip(plan["branches"], plan["tasks"], plan["prompts"]), 1)]},
                             indent=2))
            return 0
        if args.output is None:
            raise ValueError("--run needs --output pointing to a new directory")
        record = run(args)
    except (OSError, ValueError) as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        return 2
    print(json.dumps({"record": str(args.output.resolve() / "record.json"), "status": record["status"],
                      "integrated_tests": record["integrated_tests"], "tasks_completed": record["tasks_completed"],
                      "semantic_conflicts": record["semantic_conflicts"], "error": record.get("error")}, indent=2))
    return 0 if record["status"] == "succeeded" and "error" not in record else 1


if __name__ == "__main__":
    sys.exit(main())
