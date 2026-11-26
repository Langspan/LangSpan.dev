#!/usr/bin/env python3
"""Coordination benchmark driver: one run of one arm, task family, N and seed.

Without ``--run`` it prints the plan (tasks, prompts, branches) and executes
nothing. With ``--run`` it checks the agent configuration, prepares a fresh
fixture repository and one isolated Git clone per session, sets up the arm,
hands the N sessions and the integration oracle to ``runner.py`` (which
launches, bounds and records them), then writes a schema-valid run record.
A refused check still writes a record. ``--agent standin`` (the default)
runs the dry-run stand-in, which makes no model call; ``--agent claude``
starts real Claude Code sessions and is for an operator to start by hand.
"""

import argparse
import ast
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
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
STANDIN_MODES = ("follow", "ignore")
SCHEMA_VERSION = 2
GIT_IDENTITY = ["-c", "user.name=Benchmark", "-c", "user.email=benchmark@example.invalid",
                "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]
MAX_TIME_LIMIT = 3600
DAEMON_START_SECONDS = 15
GIT_SECONDS = 60
ORACLE_SECONDS = 300
EVENT_PAGE = 500
MAX_EVENT_PAGES = 200
# A stream-json transcript holds every message and tool result.
TRANSCRIPT_BYTES = 64 * 1024 * 1024
MAX_HOOK_INPUT = 64 * 1024 * 1024
HOOK_LOG_STDOUT = 16 * 1024
HOOK_LOG_SECONDS = 900
MAX_CONFIG_BYTES = 32 * 1024 * 1024
MAX_NOTES = 100
MAX_NOTE_BYTES = 1024 * 1024
CLAUDE_CONFIG_ENV = "LANGSPAN_BENCH_CLAUDE_CONFIG_DIR"
CLAUDE_BIN_ENV = "LANGSPAN_BENCH_CLAUDE"
CLAUDE_MODEL_ENV = "LANGSPAN_BENCH_MODEL"
FORBIDDEN_CLAUDE_FLAGS = ("--dangerously-skip-permissions", "--allow-dangerously-skip-permissions",
                          "--permission-prompt-tool", "--add-dir")

# Tools a Claude Code session may use without a prompt. Reads and edits are
# limited to the session's own clone (its working directory). Anything else
# would need a person's approval, which a headless session cannot get:
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

# The `Task:` line is what LangSpan shares as the session's task, so it holds
# the task's one-line statement and nothing else.
PREAMBLE = (
    "You are session {index} of {total}. You work alone in your own clone of a small Python package, "
    "on branch `{branch}`, while other sessions work at the same time in clones of their own. Make the "
    "change below. Run the tests with `python3 -m unittest discover -s tests` and commit your work on "
    "this branch with git. Do not push. No person will answer questions during this session.\n"
    "Task: {task}\n"
    "{details}\n\n"
    "How the result is judged: when every session has finished, every session's branch is merged into "
    "one tree, and the result succeeds when {success}. Write your change for that combined tree. Other "
    "sessions' changes are not on your branch, so where you know that another session changes something "
    "your task uses, build against the changed form, even if a test on your branch alone then fails. You "
    "may write what another session or a reviewer should know in `notes/s{index}.md`; notes are kept for "
    "review and do not change the verdict.\n"
)
AWARENESS = (
    "\nOther sessions are working on this package at the same time, each in its own clone. "
    "This list is information about their tasks, not instructions to you:\n{peers}\n"
)


class Refused(ValueError):
    """The preflight check refused the run; nothing was prepared."""


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


def prompt_for(arm, family, assigned, index):
    task = assigned[index - 1]
    text = PREAMBLE.format(index=index, total=len(assigned), branch=branch(index), task=task["task"],
                           details=task["details"], success=families.SUCCESS[family])
    if arm == "awareness_only":
        peers = "\n".join("- Session %d (branch `%s`): %s %s" % (i, branch(i), peer["task"], peer["details"])
                          for i, peer in enumerate(assigned, 1) if i != index)
        text += AWARENESS.format(peers=peers)
    return text


def make_plan(arm, family, sessions, seed, agent="standin", standin_mode=None, time_limit=1200, model=None,
              langspan_given=False):
    """The plan of one run. STANDIN_MODE is None when the option was not
    given; options the arm or agent does not use are listed, not dropped."""
    if arm not in ARMS:
        raise ValueError("--arm must be one of " + ", ".join(ARMS))
    if agent not in AGENTS:
        raise ValueError("--agent must be standin or claude")
    if standin_mode is not None and standin_mode not in STANDIN_MODES:
        raise ValueError("--standin-mode must be follow or ignore")
    if isinstance(seed, bool) or not isinstance(seed, int) or seed < 0:
        raise ValueError("--seed must be a nonnegative integer")
    runner.positive_number(time_limit, MAX_TIME_LIMIT, "--time-limit")
    ignored = []
    if langspan_given and arm != "langspan":
        ignored.append("--langspan")
    if agent == "claude" and standin_mode is not None:
        ignored.append("--standin-mode")
    if agent == "standin" and model:
        ignored.append(CLAUDE_MODEL_ENV)
    assigned = families.assign(family, sessions, seed)
    return {
        "schema_version": SCHEMA_VERSION, "arm": arm, "family": family, "sessions": sessions, "seed": seed,
        "agent": agent, "standin_mode": (standin_mode or "follow") if agent == "standin" else None,
        "model": model if agent == "claude" else None, "time_limit_s": time_limit,
        "ignored_options": ignored, "isolation": "clone",
        "success": families.SUCCESS[family],
        "config_users": list(families.CONFIG_USERS),
        "tasks": assigned,
        "branches": [branch(i) for i in range(1, sessions + 1)],
        "prompts": [prompt_for(arm, family, assigned, i) for i in range(1, sessions + 1)],
    }


def claude_argv(executable, prompt, arm, model=None):
    """The exact Claude Code command line for one session. Permission
    prompts stay on (`--permission-mode default`); nothing approves them.
    stream-json keeps the whole transcript; with -p it needs --verbose."""
    argv = [executable, "-p", prompt, "--permission-mode", "default", "--output-format", "stream-json",
            "--verbose"]
    if model:
        argv += ["--model", model]
    tools = CLAUDE_TOOLS + (LANGSPAN_TOOLS if arm == "langspan" else ())
    argv += ["--allowedTools", *tools]
    assert not set(argv) & set(FORBIDDEN_CLAUDE_FLAGS)
    return argv


def mentions_langspan(name, spec):
    return "langspan" in str(name).lower() or "langspan" in json.dumps(spec).lower()


def user_level_langspan(config_dir):
    """What in a Claude Code configuration would bring LangSpan into every
    session, each named by its kind and the file that holds it: hook
    commands that run it, its skill, or an MCP server entry. Never the
    entries themselves, which can hold local paths."""
    config_dir = Path(config_dir)
    found = []

    def read(name):
        path = config_dir / name
        if not path.is_file():
            return None
        try:
            value, _ = runner.bounded_json(path, MAX_CONFIG_BYTES)
        except (OSError, ValueError) as error:
            raise Refused("cannot read %s in %s to check for LangSpan entries (%s)"
                          % (name, config_dir, type(error).__name__))
        return value if isinstance(value, dict) else {}

    settings = read("settings.json") or {}
    hooks = settings.get("hooks") if isinstance(settings.get("hooks"), dict) else {}
    for entries in hooks.values():
        for entry in entries if isinstance(entries, list) else []:
            for hook in (entry.get("hooks") or []) if isinstance(entry, dict) else []:
                if isinstance(hook, dict) and re.search(r"langspan\S*\s+hook\b", str(hook.get("command", ""))):
                    found.append("hook in settings.json")
    if (config_dir / "skills" / "langspan").exists():
        found.append("skill in skills/langspan")
    for name in ("settings.json", ".claude.json"):
        data = settings if name == "settings.json" else (read(name) or {})
        tables = [data.get("mcpServers")]
        projects = data.get("projects")
        if isinstance(projects, dict):
            tables += [project.get("mcpServers") for project in projects.values() if isinstance(project, dict)]
        for table in tables:
            if isinstance(table, dict) and any(mentions_langspan(key, spec) for key, spec in table.items()):
                found.append("MCP server in " + name)
    return sorted(set(found), key=found.index)


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

def prepare_fixture(work, sessions, arm):
    """The fixture repository and one isolated clone per session.

    `repo` holds `main` and is where integration happens; no session can
    reach it. Each session clones a bare copy, with its own objects and
    refs, so no session sees another's branches or commits. Only the
    LangSpan arm keeps the clone's `origin` remote: LangSpan shares work
    summaries through LangSpan-owned refs there (ADR 0006), and no session
    may push a branch to it.
    """
    repo = work / "repo"
    shutil.copytree(HERE / "fixture", repo, ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    git("init", "-q", "-b", "main", cwd=repo)
    git("add", "-A", cwd=repo)
    git("commit", "-q", "-m", "Fixture", cwd=repo)
    base = git("rev-parse", "HEAD", cwd=repo).stdout.strip()
    origin = work / "origin.git"
    git("clone", "-q", "--bare", "--no-hardlinks", str(repo), str(origin), cwd=work)
    clones = []
    for index in range(1, sessions + 1):
        path = work / ("s%d" % index)
        git("clone", "-q", "--no-hardlinks", str(origin), str(path), cwd=work)
        git("checkout", "-q", "-b", branch(index), cwd=path)
        if arm != "langspan":
            git("remote", "remove", "origin", cwd=path)
        (path / ".git" / "info").mkdir(exist_ok=True)
        with open(path / ".git" / "info" / "exclude", "a", encoding="utf-8") as exclude:
            exclude.write(".claude/\n")
        clones.append(path)
    return repo, base, clones


def langspan_cli(langspan, state_dir, *args, timeout=30, env=None):
    done = subprocess.run([str(langspan), *args, "--state-dir", str(state_dir)], capture_output=True,
                          text=True, timeout=timeout, env=env)
    if done.returncode != 0:
        raise RuntimeError("langspan %s failed (%d): %s" % (" ".join(args), done.returncode,
                                                            (done.stderr or done.stdout).strip()[:2000]))
    return done.stdout


def install_hooks(langspan, state_dir, index, clone, hook_log):
    """LangSpan's project hooks in one clone, each run through `hook-log`,
    which passes everything through unchanged and keeps what the hook said
    and when, so a briefing can be checked against the session's actions."""
    langspan_cli(langspan, state_dir, "hooks", "install", "claude-code", "--workspace",
                 workspace_id(index), "--root", str(clone), "--apply")
    path = clone / ".claude" / "settings.local.json"
    settings = json.loads(path.read_text(encoding="utf-8"))
    prefix = " ".join(shlex.quote(part) for part in (sys.executable, "-I", "-B", str(Path(__file__).resolve()),
                                                     "hook-log", str(hook_log)))
    for entries in settings.get("hooks", {}).values():
        for entry in entries:
            for hook in entry.get("hooks", []):
                if hook.get("type") == "command":
                    hook["command"] = prefix + " " + shlex.quote(hook["command"])
    path.write_text(json.dumps(settings, indent=2) + "\n", encoding="utf-8")


def install_skill(langspan, state_dir, clones, work):
    """The LangSpan skill beside each clone's hooks. It is rendered through
    the real installer into a throwaway home, so neither the operator's home
    nor their Claude Code configuration changes."""
    home = work / "skill-home"
    (home / ".claude").mkdir(parents=True)
    env = {key: value for key, value in os.environ.items()
           if key not in ("CLAUDE_CONFIG_DIR", "CODEX_HOME", "GEMINI_CLI_HOME")}
    langspan_cli(langspan, state_dir, "install", "--home", str(home), "--hosts", "claude-code",
                 "--skill", "--apply", env=env)
    skill = home / ".claude" / "skills" / "langspan" / "SKILL.md"
    if not skill.is_file():
        raise RuntimeError("langspan install --skill wrote no skill at " + str(skill))
    for path in clones:
        target = path / ".claude" / "skills" / "langspan"
        target.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(skill, target / "SKILL.md")


def enable_remote(langspan, state_dir, index, clone):
    """Share this session's work, task line included, through the run's own
    bare remote: how LangSpan connects separate clones (ADR 0006)."""
    langspan_cli(langspan, state_dir, "remote", "enable", workspace_id(index), "--name", "session %d" % index,
                 "--share-task", "on", "--root", str(clone), "--request-id", "bench-remote-%d" % index)


class Coordinator:
    """One LangSpan daemon, in its own process group."""

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
    env = dict(os.environ)
    if plan["arm"] == "langspan":
        # The session's own `langspan` commands must reach its coordinator,
        # the one its hooks name, not a default state directory.
        env["PATH"] = str(Path(plan["langspan"]).parent) + os.pathsep + env.get("PATH", "")
        env["LANGSPAN_STATE_DIR"] = plan["state_dirs"][index - 1]
    if plan["agent"] == "standin":
        argv = [sys.executable, "-I", "-B", str(HERE / "standin_agent.py"), str(plan_path), str(index),
                plan["standin_mode"]]
        os.execve(argv[0], argv, env)
    config_dir = os.environ.get(CLAUDE_CONFIG_ENV)
    if not config_dir:
        print("set %s to the Claude Code configuration directory to use" % CLAUDE_CONFIG_ENV, file=sys.stderr)
        return 2
    env["CLAUDE_CONFIG_DIR"] = config_dir
    argv = claude_argv(os.environ.get(CLAUDE_BIN_ENV, "claude"), plan["prompts"][index - 1], plan["arm"],
                       plan.get("model"))
    executable = shutil.which(argv[0], path=env["PATH"]) or argv[0]
    os.execve(executable, argv, env)


def hook_log_main(log, command):
    """Run one hook COMMAND exactly as the host would (stdin, stdout,
    stderr and exit code passed through) and append what it said, when, and
    for which call to LOG. Input past the bound is refused (exit 2), as a
    failed safety control must."""
    payload = sys.stdin.buffer.read(MAX_HOOK_INPUT + 1)
    if len(payload) > MAX_HOOK_INPUT:
        print("hook-log: the hook input exceeds %d bytes" % MAX_HOOK_INPUT, file=sys.stderr)
        return 2
    at, started = utc_now(), time.monotonic()
    try:
        done = subprocess.run(["/bin/sh", "-c", command], input=payload, capture_output=True,
                              timeout=HOOK_LOG_SECONDS)
        code, stdout, stderr = done.returncode, done.stdout, done.stderr
    except subprocess.TimeoutExpired:
        code, stdout, stderr = 2, b"", b"hook-log: the hook did not finish in time\n"
    sys.stdout.buffer.write(stdout)
    sys.stdout.flush()
    sys.stderr.buffer.write(stderr)
    try:
        call = json.loads(payload)
        call = call if isinstance(call, dict) else {}
    except ValueError:
        call = {}
    line = json.dumps({
        "at": at, "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
        "event": call.get("hook_event_name"), "tool": call.get("tool_name"),
        "tool_use_id": call.get("tool_use_id"), "session_id": call.get("session_id"),
        "exit_code": code, "stdout": stdout[:HOOK_LOG_STDOUT].decode("utf-8", "replace"),
        "stdout_truncated": len(stdout) > HOOK_LOG_STDOUT,
    }) + "\n"
    # A log that cannot be written must not change the hook's decision.
    try:
        descriptor = os.open(log, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
        try:
            os.write(descriptor, line.encode())
        finally:
            os.close(descriptor)
    except OSError as error:
        print("hook-log: cannot append to %s: %s" % (log, error), file=sys.stderr)
    return code


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


def collect_branches(plan, repo):
    """Copy each session's final commit into REPO as its branch."""
    work = Path(plan["work"])
    for index in range(1, plan["sessions"] + 1):
        git("fetch", "-q", "--no-tags", str(work / ("s%d" % index)), "+HEAD:refs/heads/" + branch(index), cwd=repo)


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


def keep_notes(plan, repo, run_dir):
    """Each branch's `notes/` files, copied out before any merge, so a note
    on a branch Git refuses to merge is kept too. Returns their paths
    relative to RUN_DIR."""
    kept = []
    for index in range(1, plan["sessions"] + 1):
        listed = git("ls-tree", "-r", "--name-only", branch(index), "--", "notes", cwd=repo).stdout.split("\n")
        for name in [name for name in listed if name.startswith("notes/")][:MAX_NOTES]:
            text = show(repo, branch(index), name)
            if text is None:
                continue
            target = run_dir / "notes" / ("s%d" % index) / name[len("notes/"):]
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text[:MAX_NOTE_BYTES], encoding="utf-8")
            kept.append(str(target.relative_to(run_dir)))
    return kept


def integrate_main(plan_path):
    """Commit what each session left, merge every branch, run the oracle.
    Each merge's output, and on a conflict the conflicting hunks, are kept
    in RUN_DIR/merge-sN.log before the merge is aborted."""
    plan_path = Path(plan_path)
    plan = json.loads(plan_path.read_text(encoding="utf-8"))
    work, repo, base = Path(plan["work"]), Path(plan["repo"]), plan["base"]
    run_dir = Path(os.environ.get("LANGSPAN_BENCH_RUN_DIR", work))
    for index in range(1, plan["sessions"] + 1):
        clone = work / ("s%d" % index)
        git("add", "-A", cwd=clone)
        if git("diff", "--cached", "--quiet", cwd=clone, check=False).returncode != 0:
            git("commit", "-q", "-m", "Benchmark: changes session %d left uncommitted" % index, cwd=clone)
    collect_branches(plan, repo)
    plan["notes"] = keep_notes(plan, repo, run_dir)
    checkout = work / "integration"
    git("worktree", "add", "-q", "-b", "bench/integration", str(checkout), base, cwd=repo)
    integration, logs = [], []
    for index in range(1, plan["sessions"] + 1):
        argv = ["merge", "--no-ff", "--no-edit", "-m", "Merge session %d" % index, branch(index)]
        merge = git(*argv, cwd=checkout, check=False)
        log = ["$ git " + " ".join(argv), merge.stdout, merge.stderr]
        files = []
        if merge.returncode != 0:
            files = git("diff", "--name-only", "--diff-filter=U", cwd=checkout, check=False).stdout.split()
            log += ["$ git diff", git("diff", cwd=checkout, check=False).stdout]
            git("merge", "--abort", cwd=checkout, check=False)
        name = "merge-s%d.log" % index
        (run_dir / name).write_text("\n".join(log), encoding="utf-8")
        logs.append(name)
        integration.append({"session": index, "merged": merge.returncode == 0, "files": files})
    plan["integration"] = integration
    plan["merge_logs"] = logs
    plan["duplicates"] = duplicates(plan, repo, base)
    integrated = plan_path.with_name("integration.json")
    integrated.write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
    verdict = run_dir / "oracle.json"
    done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "oracle.py"), str(integrated),
                           str(checkout), str(verdict)], timeout=ORACLE_SECONDS)
    print(json.dumps({"integration": integration, "oracle_exit": done.returncode}))
    return done.returncode


# --- The run record ----------------------------------------------------------

def session_result(text):
    """The result line of a session's stream-json transcript (or the single
    object of a json one); None when the session ended without one."""
    for line in reversed(text.strip().splitlines()):
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict) and value.get("type") == "result":
            return value
    try:
        value = json.loads(text)
    except ValueError:
        return None
    return value if isinstance(value, dict) and value.get("type") == "result" else None


def session_figures(result):
    """What the session reported about itself: cost, duration and turns."""
    result = result or {}

    def figure(key, kinds):
        value = result.get(key)
        return value if type(value) in kinds and value >= 0 else None
    return {"cost_usd": figure("total_cost_usd", (int, float)), "duration_ms": figure("duration_ms", (int,)),
            "turns": figure("num_turns", (int,))}


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


def classify_denial(denial, root):
    """`outside_clone` for a refused call aimed at a path outside the
    session's clone (a peer's clone, the integration repository, the rest
    of the machine); `not_allowed` for any other call outside the allowed
    tool list, such as `sed -i` or a pipe."""
    tool_input = denial.get("tool_input") if isinstance(denial, dict) else None
    if not isinstance(tool_input, dict):
        return "not_allowed"
    root = Path(root).resolve()
    candidates = [tool_input[key] for key in ("file_path", "path", "notebook_path")
                  if isinstance(tool_input.get(key), str)]
    command = tool_input.get("command")
    if isinstance(command, str):
        candidates += [token for token in re.split(r"[\s;|&<>()'\"=]+", command)
                       if ("/" in token or token == "..") and not token.startswith("/dev/")]
    for candidate in candidates:
        path = Path(candidate) if os.path.isabs(candidate) else root / candidate
        if not Path(os.path.normpath(path.resolve())).is_relative_to(root):
            return "outside_clone"
    return "not_allowed"


def denial_counts(denials, root):
    if not isinstance(denials, list):
        return None
    kinds = [classify_denial(denial, root) for denial in denials]
    return {"total": len(kinds), "outside_clone": kinds.count("outside_clone"),
            "not_allowed": kinds.count("not_allowed")}


def session_of(event):
    match = re.fullmatch(r"bench-s(\d+)", str(event.get("workspace_id")
                                              or (event.get("failure") or {}).get("workspace_id") or ""))
    return int(match.group(1)) if match else None


def refusal_items(events):
    """LangSpan's refusals, each with what the same session did next."""
    items = []
    for position, record in enumerate(events):
        event = record.get("event", {})
        if event.get("type") != "write_refused":
            continue
        paths = set(event.get("paths", []))
        later = "no_later_admission"
        for following in events[position + 1:]:
            lease = following.get("event", {}).get("lease") if following.get("event", {}).get("type") == "lease_granted" else None
            if lease and lease.get("session_id") == event.get("session_id") and (not paths or paths & set(lease.get("paths", []))):
                later = "admitted_later"
                break
        items.append({"seq": record.get("seq", 0), "session": session_of(event),
                      "kind": str(event.get("kind")), "paths": sorted(paths),
                      "peer_session": event.get("peer_session_id"), "followed_by": later})
    return items


def scan_summary(events, arm):
    """Each coordinator's failed and recovered scans of its clone."""
    if arm != "langspan":
        return {"status": "not_applicable", "failed": None, "recovered": None, "items": []}
    if events is None:
        return {"status": "unknown", "failed": None, "recovered": None, "items": []}
    items = []
    for session, records in sorted(events.items()):
        for record in records:
            event = record.get("event", {})
            if event.get("type") in ("scan_failed", "scan_recovered"):
                detail = (event.get("failure") or {}).get("detail")
                items.append({"session": session, "seq": record.get("seq", 0), "type": event["type"],
                              "detail": str(detail)[:500] if detail is not None else None})
    return {"status": "reported", "failed": sum(item["type"] == "scan_failed" for item in items),
            "recovered": sum(item["type"] == "scan_recovered" for item in items), "items": items}


def first_line(path):
    try:
        text = path.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None
    return text.splitlines()[0][:200] if text else None


def build_record(plan, final, runner_dir, run_dir, events, started_at, total_elapsed, error=None, status=None):
    """EVENTS maps each session to its coordinator's event log (LangSpan
    arm), or is None when none was read."""
    output = Path(plan["output"])
    commands = {(c["phase"], c["id"]): c for c in final["commands"]} if final else {}
    oracle_path = run_dir / "oracle.json" if run_dir else None
    oracle = json.loads(oracle_path.read_text(encoding="utf-8")) if oracle_path and oracle_path.is_file() else None
    integration, duplicate_details, notes, merge_logs = {}, None, [], []
    integration_file = Path(plan["plan_path"]).with_name("integration.json")
    if integration_file.is_file():
        integrated_plan = json.loads(integration_file.read_text(encoding="utf-8"))
        integration = {entry["session"]: entry for entry in integrated_plan["integration"]}
        duplicate_details = integrated_plan["duplicates"]
        if run_dir:
            notes = [str((run_dir / name).relative_to(output)) for name in integrated_plan["notes"]]
            merge_logs = [str((run_dir / name).relative_to(output)) for name in integrated_plan["merge_logs"]]
    refusals = [item for session in sorted(events) for item in refusal_items(events[session])] if events is not None else []
    tasks, models, token_list = [], set(), []
    for index, task in enumerate(plan["tasks"], 1):
        command = commands.get(("agent", "s%d" % index))
        result, transcript = None, None
        if command is not None:
            transcript = str(Path(runner_dir.name) / command["stdout_path"])
            result = session_result((runner_dir / command["stdout_path"]).read_text(encoding="utf-8", errors="replace"))
        verdict = oracle["tasks"][index - 1] if oracle else {}
        process_status = command["status"] if command else None
        if process_status is None:
            stopped = None
        elif process_status != "succeeded":
            stopped = True
        elif result is None:
            stopped = None
        else:
            stopped = bool(result.get("is_error")) or result.get("subtype") != "success"
        answer = result.get("result") if result else None
        asked = answer.strip().endswith("?") if isinstance(answer, str) else None
        tokens = tokens_of(result)
        token_list.append(tokens)
        if result and isinstance(result.get("modelUsage"), dict):
            models.update(result["modelUsage"])
        hook_log = output / ("hooks-s%d.jsonl" % index)
        informed = (result.get("standin") or {}).get("informed") if result else None
        tasks.append({
            "session": index, "task_id": task["id"], "role": task["role"], "branch": branch(index),
            "process_status": process_status, "merged": bool(integration.get(index, {}).get("merged")),
            "completed": verdict.get("completed"), "accepted": verdict.get("accepted"),
            "detail": verdict.get("detail"), "stopped": stopped, "asked_question": asked,
            "permission_denials": denial_counts(result.get("permission_denials") if result else None,
                                                Path(plan.get("work", output / "work")) / ("s%d" % index)),
            "tokens": tokens, **session_figures(result),
            "refusals": sum(item["session"] == index for item in refusals) if events is not None else None,
            "transcript": transcript,
            "hook_log": hook_log.name if hook_log.is_file() else None,
            "standin_informed": informed if isinstance(informed, bool) else None,
        })

    def total(field):
        values = [task[field] for task in tasks]
        return None if any(value is None for value in values) else sum(int(value) for value in values)

    denials = [task["permission_denials"] for task in tasks]
    costs = [task["cost_usd"] for task in tasks]
    reported = [tokens for tokens in token_list if tokens is not None]

    def version(name):
        command = commands.get(("version", name))
        return first_line(runner_dir / command["stdout_path"]) if command and command["status"] == "succeeded" else None
    record = {
        "schema_version": SCHEMA_VERSION, "record_type": "coordination_run",
        "run_id": final["run_id"] if final else plan["run_id"],
        "arm": plan["arm"], "family": plan["family"], "sessions": plan["sessions"], "seed": plan["seed"],
        "agent": "standin" if plan["agent"] == "standin" else "claude-code",
        "dry_run": plan["agent"] == "standin",
        "status": status or (final["status"] if final else "setup_failed"),
        "started_at": started_at,
        "elapsed_s": final["agent_phase_elapsed_ns"] / 1e9 if final and final["agent_phase_elapsed_ns"] is not None else None,
        "total_elapsed_s": total_elapsed, "time_limit_s": plan["time_limit_s"],
        "ignored_options": plan["ignored_options"],
        "versions": {
            "agent_cli": version("agent"), "requested_model": plan.get("model"), "models": sorted(models),
            "langspan_commit": plan.get("langspan_commit"),
            "langspan_binary_sha256": plan.get("langspan_sha256"),
            "git": version("git"), "python": sys.version.split()[0],
        },
        "tasks": tasks, "tasks_total": len(tasks),
        "tasks_accepted": sum(task["accepted"] is True for task in tasks) if oracle else None,
        "integrated_tests": oracle["integrated_tests"] if oracle else "unknown",
        "merge_conflicts": [{"session": s, "files": e["files"]} for s, e in sorted(integration.items()) if not e["merged"]],
        "semantic_conflicts": len(oracle["semantic_conflicts"]) if oracle else None,
        "semantic_conflict_details": oracle["semantic_conflicts"] if oracle else [],
        "duplicate_work": len(duplicate_details) if duplicate_details is not None else None,
        "duplicate_work_details": duplicate_details or [],
        "fixture_tests": oracle["fixture_tests"] if oracle else "unknown",
        "stops": total("stopped"), "questions": total("asked_question"),
        "permission_denials": None if any(d is None for d in denials) else
        {key: sum(d[key] for d in denials) for key in ("total", "outside_clone", "not_allowed")},
        "cost_usd": None if any(cost is None for cost in costs) else round(sum(costs), 6),
        "refusals": {
            "status": "not_applicable" if plan["arm"] != "langspan" else ("reported" if events is not None else "unknown"),
            "count": len(refusals) if events is not None else None,
            "items": refusals,
            "sessions_registered": sum(r.get("event", {}).get("type") == "session_registered"
                                       for records in events.values() for r in records) if events is not None else None,
            "admissions": sum(r.get("event", {}).get("type") == "lease_granted"
                              for records in events.values() for r in records) if events is not None else None,
        },
        "coordinator_scans": scan_summary(events, plan["arm"]),
        "tokens": {
            "status": "reported" if len(reported) == len(tasks) else ("partial" if reported else "unknown"),
            "total": {key: sum(t[key] for t in reported) for key in ("input", "output", "cache_creation", "cache_read")}
            if len(reported) == len(tasks) else None,
        },
        "evidence": {
            "runner_journal": "runner/runs.jsonl" if (runner_dir / "runs.jsonl").is_file() else None,
            "plan": "plan.json",
            "integration": "integration.json" if integration_file.is_file() else None,
            "oracle": str(oracle_path.relative_to(output)) if oracle else None,
            "events": ["events-s%d.jsonl" % session for session in sorted(events)] if events is not None else [],
            "notes": notes,
            "merge_logs": merge_logs,
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

def manifest_for(plan, clones, repo):
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
                     "cwd": str(path), "timeout_seconds": plan["time_limit_s"], "max_output_bytes": TRANSCRIPT_BYTES}
                    for index, path in enumerate(clones, 1)
                ],
                "checks": [
                    {"id": "integrate", "argv": [python, "-I", "-B", driver, "integrate", plan["plan_path"]],
                     "cwd": str(repo), "timeout_seconds": ORACLE_SECONDS + 120, "assesses": "integrated_correctness"},
                ],
            }},
        }],
    }


def preflight(args, plan):
    """What must hold before anything is prepared. Returns the langspan
    binary for the LangSpan arm; raises Refused with the reason."""
    if args.agent == "claude":
        config_dir = os.environ.get(CLAUDE_CONFIG_ENV)
        if not config_dir or not Path(config_dir).is_dir():
            raise Refused("--agent claude needs %s set to an existing Claude Code configuration directory"
                          % CLAUDE_CONFIG_ENV)
        found = user_level_langspan(config_dir)
        if found:
            raise Refused("%s has LangSpan entries that would reach every session in every arm: %s. Use a clean "
                          "Claude Code configuration (benchmarks/coordination/README.md, \"Prepare a clean agent "
                          "configuration\")." % (config_dir, "; ".join(found)))
    if plan["arm"] != "langspan":
        return None
    if args.langspan is None:
        raise Refused("the langspan arm needs --langspan PATH to the binary built from this checkout")
    langspan = args.langspan.resolve()
    if not langspan.is_file() or not os.access(langspan, os.X_OK):
        raise Refused("--langspan is not an executable file: " + str(langspan))
    return langspan


def run(args):
    started_at, started = utc_now(), time.monotonic()
    output = args.output.resolve()
    check_output_location(output)
    plan = make_plan(args.arm, args.family, args.sessions, args.seed, args.agent, args.standin_mode, args.time_limit,
                     model=os.environ.get(CLAUDE_MODEL_ENV) or None, langspan_given=args.langspan is not None)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    work = output / "work"
    runner_dir = output / "runner"
    plan.update(output=str(output), work=str(work), plan_path=str(output / "plan.json"),
                langspan_commit=langspan_commit(), langspan=None, langspan_sha256=None)
    try:
        langspan = preflight(args, plan)
    except Refused as refusal:
        (output / "plan.json").write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
        plan["run_id"] = os.urandom(16).hex()
        record = build_record(plan, None, runner_dir, None, None, started_at, time.monotonic() - started,
                              error=str(refusal), status="refused")
        validate(record, load_schema())
        (output / "record.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        return record
    work.mkdir(mode=0o700)
    if langspan:
        plan.update(langspan=str(langspan), langspan_sha256=sha256_file(langspan))
    if args.agent == "claude":
        # The exact command each session runs, with its prompt from `prompts`.
        plan["agent_command"] = claude_argv(os.environ.get(CLAUDE_BIN_ENV, "claude"), "<prompts[N-1]>",
                                            plan["arm"], plan["model"])
    coordinators, state_dirs, events, final, error, run_dir = [], [], None, None, None, None
    try:
        repo, base, clones = prepare_fixture(work, plan["sessions"], plan["arm"])
        plan.update(repo=str(repo), base=base)
        if langspan:
            # A Unix socket path holds about 100 bytes: keep each state short.
            state_dirs = [Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp")) for _ in clones]
            plan["state_dirs"] = [str(path) for path in state_dirs]
            for index, (clone, state) in enumerate(zip(clones, state_dirs), 1):
                install_hooks(langspan, state, index, clone, output / ("hooks-s%d.jsonl" % index))
            install_skill(langspan, state_dirs[0], clones, work)
            for index, (clone, state) in enumerate(zip(clones, state_dirs), 1):
                coordinators.append(Coordinator(langspan, state, output / ("coordinator-s%d.log" % index)))
                enable_remote(langspan, state, index, clone)
        (output / "plan.json").write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
        manifest = output / "manifest.json"
        manifest.write_text(json.dumps(manifest_for(plan, clones, repo), indent=2) + "\n", encoding="utf-8")
        runner.run_manifest(manifest, runner_dir)
        rows = [json.loads(line) for line in (runner_dir / "runs.jsonl").read_text(encoding="utf-8").splitlines()]
        final = next(row for row in rows if row["record_type"] == "run_finished")
        run_dir = runner_dir / final["run_id"]
        if coordinators:
            events = {}
            for index, coordinator in enumerate(coordinators, 1):
                events[index] = coordinator.events()
                with open(output / ("events-s%d.jsonl" % index), "w", encoding="utf-8") as sink:
                    for record in events[index]:
                        sink.write(json.dumps(record) + "\n")
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError, StopIteration) as failure:
        error = "%s: %s" % (type(failure).__name__, failure)
    finally:
        for coordinator in coordinators:
            coordinator.stop()
        for state in state_dirs:
            shutil.rmtree(state, ignore_errors=True)
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
    if argv[:1] == ["hook-log"] and len(argv) == 3:
        return hook_log_main(argv[1], argv[2])
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--arm", required=True, choices=ARMS)
    parser.add_argument("--family", required=True, choices=families.FAMILIES)
    parser.add_argument("--sessions", required=True, type=int, choices=families.SESSION_COUNTS)
    parser.add_argument("--seed", required=True, type=int)
    parser.add_argument("--agent", default="standin", choices=AGENTS)
    parser.add_argument("--standin-mode", choices=STANDIN_MODES,
                        help="follow (default): use what the arm tells the session; ignore: use nothing")
    parser.add_argument("--time-limit", type=float, default=1200, help="seconds per session (at most 3600)")
    parser.add_argument("--langspan", type=Path, help="the langspan binary, for the langspan arm")
    parser.add_argument("--output", type=Path, help="a new result directory; required with --run")
    parser.add_argument("--run", action="store_true", help="prepare and run; without it, only print the plan")
    args = parser.parse_args(argv)
    try:
        if not args.run:
            plan = make_plan(args.arm, args.family, args.sessions, args.seed, args.agent, args.standin_mode,
                             args.time_limit, model=os.environ.get(CLAUDE_MODEL_ENV) or None,
                             langspan_given=args.langspan is not None)
            print(json.dumps({"mode": "plan_only", "executed_commands": 0,
                              "ignored_options": plan["ignored_options"], "success": plan["success"],
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
    if record["status"] == "refused":
        print(json.dumps({"error": record["error"], "record": str(args.output.resolve() / "record.json")}),
              file=sys.stderr)
        return 2
    print(json.dumps({"record": str(args.output.resolve() / "record.json"), "status": record["status"],
                      "integrated_tests": record["integrated_tests"], "tasks_accepted": record["tasks_accepted"],
                      "semantic_conflicts": record["semantic_conflicts"], "error": record.get("error")}, indent=2))
    return 0 if record["status"] == "succeeded" and "error" not in record else 1


if __name__ == "__main__":
    sys.exit(main())
