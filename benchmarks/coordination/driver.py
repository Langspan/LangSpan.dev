#!/usr/bin/env python3
"""Coordination benchmark driver: one run of one arm, task family, N and seed.

Without ``--run`` it prints the plan (tasks, prompts, branches) and executes
nothing. With ``--run`` it checks the agent configuration, prepares a fresh
fixture repository and one checkout per session (an isolated clone, or in
the LangSpan arm a worktree of one shared repository), sets up the arm,
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
STANDIN_MODES = ("follow", "ignore", "hedge")
SCHEMA_VERSION = 4
GIT_IDENTITY = ["-c", "user.name=Benchmark", "-c", "user.email=benchmark@example.invalid",
                "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]
MAX_TIME_LIMIT = 3600
# The most a staggered session waits to start. Its time limit starts when it
# does, so the runner's bound for it is the two added.
MAX_STAGGER = 600
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
# limited to the session's own checkout (its working directory). Anything else
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
# The only built-in tools a session has (`--tools`). Claude Code's
# cross-session tools, web access and tool search are not among them, so no
# arm reaches another session or an outside service through the host. The
# LangSpan arm adds Skill, which loads LangSpan's skill.
SESSION_TOOLS = ("Bash", "Edit", "Glob", "Grep", "Read", "Write")
# Denied by name as well, in case a host version offers them anyway.
DENIED_TOOLS = (
    "ListAgents", "SendMessage", "Task", "Agent", "Workflow", "ToolSearch", "WebSearch", "WebFetch",
    "RemoteTrigger", "ScheduleWakeup", "CronCreate", "CronDelete", "CronList", "PushNotification", "Monitor",
    "TaskStop", "EnterWorktree", "ExitWorktree", "DesignSync", "McpTaskCheck", "NotebookEdit",
)
# Every arm denies reading other sessions' branches, refs, stash and files
# through Git, so that in the LangSpan arm, whose sessions share one
# repository, the only extra information is what LangSpan sends. Reads of
# files outside the session's checkout are refused by Read(./**) already.
# The rules name Git's read commands, so `git add --all` stays allowed.
GIT_READS = ("log", "diff", "show")
PEER_GIT_RULES = tuple(
    "Bash(git %s *%s*)" % (verb, pattern) for verb in GIT_READS
    for pattern in ("bench/", "--all", "--branches", "--remotes", "--glob", "--reflog", "refs/", "stash",
                    "--no-index")
) + tuple("Bash(git %s*)" % command for command in ("stash", "worktree", "branch", "for-each-ref", "reflog",
                                                       "-C ", "--git-dir", "--work-tree"))
# No MCP server or hosted connector reaches a session.
MCP_CONFIG = '{"mcpServers": {}}'
# Claude Code keeps auto memory in one directory per repository, so the
# worktrees of the LangSpan arm would share one: a channel between sessions
# that the other arms lack. Every session turns it off, with the setting and
# with the environment variable Claude Code reads, which also wins over a
# setting from elsewhere.
MEMORY_SETTINGS = '{"autoMemoryEnabled": false}'
MEMORY_ENV = "CLAUDE_CODE_DISABLE_AUTO_MEMORY"
# The tool guard's verdict, the last line of a transcript it checked.
TOOL_CHECK_TYPE = "benchmark_tool_check"
TOOL_CHECK_EXIT = 3
GUARD_LINE_BYTES = 8 * 1024 * 1024
GUARD_STOP_SECONDS = 10
MAX_UNEXPECTED = 20

# The `Task:` line is what LangSpan shares as the session's task, so it holds
# the task's one-line statement and nothing else.
PREAMBLE = (
    "You are session {index} of {total}. You work alone in your own checkout of a small Python package, "
    "on branch `{branch}`, while other sessions work at the same time in checkouts of their own. Make the "
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
    "\nOther sessions are working on this package at the same time, each in its own checkout. "
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
              langspan_given=False, stagger=0):
    """The plan of one run. STANDIN_MODE is None when the option was not
    given; options the arm or agent does not use are listed, not dropped.
    STAGGER is how many seconds the sessions other than the one whose task
    changes an interface wait to start."""
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
    if isinstance(stagger, bool) or type(stagger) not in (int, float) or not 0 <= stagger <= MAX_STAGGER:
        raise ValueError("--stagger must be a number of seconds from 0 to %d" % MAX_STAGGER)
    if time_limit + stagger > MAX_TIME_LIMIT:
        raise ValueError("--time-limit and --stagger together must be at most %d seconds" % MAX_TIME_LIMIT)
    if stagger and families.leader(assigned) is None:
        raise ValueError("--stagger needs a family with a task that changes an interface its peers use; "
                         "%s has none" % family)
    return {
        "schema_version": SCHEMA_VERSION, "arm": arm, "family": family, "sessions": sessions, "seed": seed,
        "agent": agent, "standin_mode": (standin_mode or "follow") if agent == "standin" else None,
        "model": model if agent == "claude" else None, "time_limit_s": time_limit,
        "ignored_options": ignored, "isolation": isolation(arm), "stagger_s": stagger,
        "auto_memory": "off" if agent == "claude" else "not_applicable",
        "allowed_tools": list(session_tools(arm)),
        "success": families.SUCCESS[family],
        "config_users": list(families.CONFIG_USERS),
        "tasks": assigned,
        "branches": [branch(i) for i in range(1, sessions + 1)],
        "prompts": [prompt_for(arm, family, assigned, i) for i in range(1, sessions + 1)],
    }


def session_tools(arm):
    """The built-in tools a session of ARM has, and the only tools its
    first event may list."""
    return SESSION_TOOLS + (("Skill",) if arm == "langspan" else ())


def peer_paths(work, sessions, index):
    """The run's directories session INDEX must not read: the other
    sessions' checkouts and the run's repositories."""
    work = Path(work)
    others = ["s%d" % other for other in range(1, sessions + 1) if other != index]
    return [work / name for name in ["repo", "shared", "integration", "skill-home"] + others]


def claude_argv(executable, prompt, arm, model=None, denied_paths=()):
    """The exact Claude Code command line for one session. Permission
    prompts stay on (`--permission-mode default`), and nobody answers them
    (`--permission-prompts none`), so a call outside the allowed list is
    refused. stream-json keeps the whole transcript; with -p it needs
    --verbose. The tool and MCP options are the same in every arm but for
    LangSpan's own skill and commands. DENIED_PATHS are denied to every
    file tool (Claude Code's `//` marks an absolute path)."""
    argv = [executable, "-p", prompt, "--permission-mode", "default", "--permission-prompts", "none",
            "--output-format", "stream-json", "--verbose", "--tools", ",".join(session_tools(arm)),
            "--strict-mcp-config", "--mcp-config", MCP_CONFIG, "--settings", MEMORY_SETTINGS]
    if model:
        argv += ["--model", model]
    paths = [rule % ("/" + str(path).lstrip("/")) for path in denied_paths for rule in ("Read(/%s/**)", "Edit(/%s/**)")]
    argv += ["--disallowedTools", *DENIED_TOOLS, *PEER_GIT_RULES, *paths]
    tools = CLAUDE_TOOLS + (LANGSPAN_TOOLS if arm == "langspan" else ())
    argv += ["--allowedTools", *tools]
    assert not set(argv) & set(FORBIDDEN_CLAUDE_FLAGS)
    return argv


def auto_memory_on(argv, env):
    """Whether a Claude Code session started with ARGV and ENV keeps auto
    memory: unless its environment or a `--settings` value turns it off."""
    if str(env.get(MEMORY_ENV, "")).strip().lower() in ("1", "true", "yes", "on"):
        return False
    for position, value in enumerate(argv[:-1]):
        if value == "--settings":
            try:
                settings = json.loads(argv[position + 1])
            except ValueError:
                continue
            if isinstance(settings, dict) and settings.get("autoMemoryEnabled") is False:
                return False
    return True


def memory_directory(arm, work, index):
    """Where Claude Code would keep session INDEX's auto memory: one
    directory per repository, so worktrees of one repository share it."""
    return Path(work) / ("shared" if isolation(arm) == "worktree" else "s%d" % index)


def shared_memory(arm, work, argvs, envs):
    """The sessions, counted from 1, that would share an auto-memory
    directory with another session of the trial."""
    places = {}
    for index, (argv, env) in enumerate(zip(argvs, envs), 1):
        if auto_memory_on(argv, env):
            places.setdefault(memory_directory(arm, work, index), []).append(index)
    return sorted(index for members in places.values() if len(members) > 1 for index in members)


def session_env(plan, environ, config_dir=None):
    """The environment of one session: the LangSpan arm's own coordinator,
    and for Claude Code its configuration with auto memory off."""
    env = dict(environ)
    if plan["arm"] == "langspan" and plan.get("langspan"):
        # The session's own `langspan` commands must reach the coordinator
        # its hooks name, not a default state directory.
        env["PATH"] = str(Path(plan["langspan"]).parent) + os.pathsep + env.get("PATH", "")
        env["LANGSPAN_STATE_DIR"] = plan["state_dir"]
    if plan["agent"] == "claude":
        env["CLAUDE_CONFIG_DIR"] = config_dir
        env[MEMORY_ENV] = "1"
    return env


def start_delay(plan, index):
    """Seconds session INDEX waits to start: the stagger, unless its task
    changes the interface its peers use."""
    return 0 if index == families.leader(plan["tasks"]) else plan.get("stagger_s", 0)


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

def isolation(arm):
    """`worktree` for the LangSpan arm, `clone` for the others."""
    return "worktree" if arm == "langspan" else "clone"


def exclude_claude(checkout):
    """Keep `.claude/` (LangSpan's hooks and skill) out of every branch."""
    common = Path(git("rev-parse", "--path-format=absolute", "--git-common-dir", cwd=checkout).stdout.strip())
    (common / "info").mkdir(exist_ok=True)
    with open(common / "info" / "exclude", "a", encoding="utf-8") as exclude:
        exclude.write(".claude/\n")


def prepare_fixture(work, sessions, arm):
    """The fixture repository and one checkout per session, on its own
    branch `bench/sN`, with no remote.

    `repo` holds `main` and is where integration happens; no session can
    reach it. In `worktrees` and `awareness_only` each session has its own
    clone, with its own objects and refs, so it cannot see another's
    branches or commits. In `langspan` the sessions use worktrees of one
    repository, `shared`: what LangSpan supports for telling one session
    about another's interface changes. There Git could show a peer's
    branch, and the session's permission rules deny it (PEER_GIT_RULES),
    as they do in every arm.
    """
    repo = work / "repo"
    shutil.copytree(HERE / "fixture", repo, ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    git("init", "-q", "-b", "main", cwd=repo)
    git("add", "-A", cwd=repo)
    git("commit", "-q", "-m", "Fixture", cwd=repo)
    base = git("rev-parse", "HEAD", cwd=repo).stdout.strip()
    checkouts = []
    if isolation(arm) == "worktree":
        shared = work / "shared"
        git("clone", "-q", "--no-hardlinks", str(repo), str(shared), cwd=work)
        git("remote", "remove", "origin", cwd=shared)
        exclude_claude(shared)
        for index in range(1, sessions + 1):
            path = work / ("s%d" % index)
            git("worktree", "add", "-q", "-b", branch(index), str(path), base, cwd=shared)
            checkouts.append(path)
        return repo, base, checkouts
    for index in range(1, sessions + 1):
        path = work / ("s%d" % index)
        git("clone", "-q", "--no-hardlinks", str(repo), str(path), cwd=work)
        git("checkout", "-q", "-b", branch(index), cwd=path)
        git("remote", "remove", "origin", cwd=path)
        exclude_claude(path)
        checkouts.append(path)
    return repo, base, checkouts


def langspan_cli(langspan, state_dir, *args, timeout=30, env=None):
    done = subprocess.run([str(langspan), *args, "--state-dir", str(state_dir)], capture_output=True,
                          text=True, timeout=timeout, env=env)
    if done.returncode != 0:
        raise RuntimeError("langspan %s failed (%d): %s" % (" ".join(args), done.returncode,
                                                            (done.stderr or done.stdout).strip()[:2000]))
    return done.stdout


def install_hooks(langspan, state_dir, index, clone, hook_log):
    """LangSpan's project hooks in one checkout, each run through `hook-log`,
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
    """The LangSpan skill beside each checkout's hooks. It is rendered through
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
    """Run session INDEX's agent (the runner's agent command) through the
    tool guard."""
    plan = json.loads(Path(plan_path).read_text(encoding="utf-8"))
    config_dir = os.environ.get(CLAUDE_CONFIG_ENV)
    if plan["agent"] == "claude" and not config_dir:
        print("set %s to the Claude Code configuration directory to use" % CLAUDE_CONFIG_ENV, file=sys.stderr)
        return 2
    env = session_env(plan, os.environ, config_dir)
    # Under --stagger, a session whose task does not change the interface
    # starts after the others have had that long to change it.
    time.sleep(start_delay(plan, index))
    if plan["agent"] == "standin":
        argv = [sys.executable, "-I", "-B", str(HERE / "standin_agent.py"), str(plan_path), str(index),
                plan["standin_mode"]]
    else:
        argv = claude_argv(os.environ.get(CLAUDE_BIN_ENV, "claude"), plan["prompts"][index - 1], plan["arm"],
                           plan.get("model"), peer_paths(plan["work"], plan["sessions"], index))
        argv[0] = shutil.which(argv[0], path=env["PATH"]) or argv[0]
    return guard_session(argv, env, session_tools(plan["arm"]), sys.stdout.buffer)


def unexpected_tools(event, allowed):
    """What one stream-json event offers or calls beyond ALLOWED: a tool
    or an MCP server in a tool list (`system`/`init`, which a host can send
    again later), or a call to an unlisted tool."""
    if not isinstance(event, dict):
        return []
    found = []
    if event.get("type") == "system" and event.get("subtype") == "init":
        tools = event.get("tools")
        found += [str(tool) for tool in (tools if isinstance(tools, list) else [])
                  if not isinstance(tool, str) or tool not in allowed]
        servers = event.get("mcp_servers")
        for server in servers if isinstance(servers, list) else []:
            name = server.get("name") if isinstance(server, dict) else server
            found.append("MCP server: %s" % name)
    elif event.get("type") == "assistant":
        content = (event.get("message") or {}).get("content") if isinstance(event.get("message"), dict) else None
        for block in content if isinstance(content, list) else []:
            if isinstance(block, dict) and block.get("type") == "tool_use" and block.get("name") not in allowed:
                found.append(str(block.get("name")))
    return [name[:200] for name in found]


def guard_session(argv, env, allowed, sink, stop_seconds=GUARD_STOP_SECONDS):
    """Run one agent session, passing its stream-json transcript to SINK
    line by line, and stop it at the first event that offers or calls a
    tool outside ALLOWED. The preflight is the first event's tool list,
    which the host reports before the model's first turn. The verdict is
    the transcript's last line (TOOL_CHECK_TYPE); a stopped session, or
    one that never lists its tools, exits TOOL_CHECK_EXIT, so the runner
    marks the trial failed. A line longer than GUARD_LINE_BYTES is passed
    on unchecked."""
    allowed = set(allowed)
    try:
        process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE)
    except OSError as error:
        sink.write((json.dumps({"type": TOOL_CHECK_TYPE, "status": "failed", "unexpected": [],
                                "reason": "the agent did not start: %s" % error.strerror}) + "\n").encode())
        sink.flush()
        return TOOL_CHECK_EXIT
    unexpected, listed, partial = [], False, False
    for line in iter(lambda: process.stdout.readline(GUARD_LINE_BYTES), b""):
        sink.write(line)
        sink.flush()
        continued, partial = partial, not line.endswith(b"\n")
        if continued or partial:
            continue
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict) and event.get("type") == "system" and event.get("subtype") == "init":
            listed = True
        for name in unexpected_tools(event, allowed):
            if name not in unexpected:
                unexpected.append(name)
        if unexpected:
            break
    if unexpected:
        process.terminate()
        try:
            process.wait(timeout=stop_seconds)
        except subprocess.TimeoutExpired:
            process.kill()
    code = process.wait()
    process.stdout.close()
    passed = listed and not unexpected
    verdict = {"type": TOOL_CHECK_TYPE, "status": "passed" if passed else "failed",
               "unexpected": unexpected[:MAX_UNEXPECTED]}
    if not listed:
        verdict["reason"] = "the session reported no tool list"
    sink.write((json.dumps(verdict) + "\n").encode())
    sink.flush()
    return code if passed else TOOL_CHECK_EXIT


def tool_check(text):
    """The tool guard's verdict in a transcript: `passed`, `failed`, or
    `unknown` when the guard wrote none (a session stopped from outside)."""
    for line in reversed(text.strip().splitlines()):
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict) and value.get("type") == TOOL_CHECK_TYPE:
            status = value.get("status") if value.get("status") in ("passed", "failed") else "unknown"
            names = value.get("unexpected") if isinstance(value.get("unexpected"), list) else []
            return {"status": status, "unexpected": [str(name)[:200] for name in names[:MAX_UNEXPECTED]]}
    return {"status": "unknown", "unexpected": []}


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
    tool_input = call.get("tool_input") if isinstance(call.get("tool_input"), dict) else {}
    file_path = tool_input.get("file_path")
    line = json.dumps({
        "at": at, "elapsed_ms": round((time.monotonic() - started) * 1000, 1),
        "event": call.get("hook_event_name"), "tool": call.get("tool_name"),
        "tool_use_id": call.get("tool_use_id"), "session_id": call.get("session_id"),
        "file_path": file_path[:4096] if isinstance(file_path, str) else None,
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

def transcript_events(text):
    """Every JSON object of a stream-json transcript, in order (or the
    single object of a json one)."""
    events = []
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict):
            events.append(value)
    if not events:
        try:
            value = json.loads(text)
        except ValueError:
            return []
        events = [value] if isinstance(value, dict) else []
    return events


def session_results(text):
    """Every result line of a session, in order. A session has one per
    turn it ended: a session woken again after its turn ended, by a message
    for example, has more than one."""
    return [event for event in transcript_events(text) if event.get("type") == "result"]


def denial_reasons(text):
    """Claude Code's own reason for each refused call, by tool call ID,
    from the transcript's `permission_denied` events."""
    return {event["tool_use_id"]: event for event in transcript_events(text)
            if event.get("type") == "system" and event.get("subtype") == "permission_denied"
            and isinstance(event.get("tool_use_id"), str)}


TOKEN_FIELDS = {"input": "input_tokens", "output": "output_tokens",
                "cache_creation": "cache_creation_input_tokens", "cache_read": "cache_read_input_tokens"}


def session_totals(results):
    """What a session reported about itself, over every result line. Turns,
    duration and token usage are per result, so they add up; the cost is
    cumulative, so the last result's is the session's. Each figure is None
    unless every result reported it. Denials are collected from every
    result, once per tool call."""

    def figure(result, key, kinds):
        value = result.get(key)
        return value if type(value) in kinds and value >= 0 else None

    def summed(key, kinds):
        values = [figure(result, key, kinds) for result in results]
        return None if not values or None in values else sum(values)

    tokens = None
    usages = [result.get("usage") for result in results]
    if usages and all(isinstance(usage, dict) for usage in usages):
        values = [{name: usage.get(key, 0) for name, key in TOKEN_FIELDS.items()} for usage in usages]
        if all(type(value) is int and value >= 0 for entry in values for value in entry.values()):
            tokens = {name: sum(entry[name] for entry in values) for name in TOKEN_FIELDS}
    denials, seen = None, set()
    if results and all(isinstance(result.get("permission_denials"), list) for result in results):
        denials = []
        for result in results:
            for denial in result["permission_denials"]:
                key = denial.get("tool_use_id") if isinstance(denial, dict) else None
                if key is None or key not in seen:
                    seen.add(key)
                    denials.append(denial)
    models = set()
    for result in results:
        if isinstance(result.get("modelUsage"), dict):
            models.update(result["modelUsage"])
    return {"final": results[-1] if results else None, "results": len(results),
            "cost_usd": figure(results[-1], "total_cost_usd", (int, float)) if results else None,
            "duration_ms": summed("duration_ms", (int,)), "turns": summed("num_turns", (int,)),
            "tokens": tokens, "denials": denials, "models": models}


# Git reads that reach other sessions' work in a shared repository: a read
# command naming all refs or the stash, or a command that lists them.
PEER_GIT_READ = re.compile(r"\bgit\s+(%s)\b(?P<rest>[^;&|]*)" % "|".join(GIT_READS))
PEER_GIT_ARGS = re.compile(r"(^|\s)--(all|branches|remotes|glob|reflog)\b|\brefs/|\bstash\b")
PEER_GIT_LISTS = re.compile(r"\bgit\s+(stash|worktree|branch|for-each-ref|reflog)\b")


def classify_denial(denial, root, work, index, reason=None):
    """The kind of one call Claude Code refused, from its tool input and,
    when the transcript has it, Claude Code's own REASON (its
    `permission_denied` event):

    - `peer_checkout`: aimed at another session's checkout, the run's other
      repositories (under WORK, outside ROOT), or through Git at another
      session's branch, all refs, or the stash;
    - `outside_clone`: refused as outside the session's own checkout, such
      as a scratch copy in /tmp;
    - `not_allowed`: any other call outside the allowed list, such as
      `sed -i` or a compound command.

    Words in a command's text are read as paths only to find a peer's
    checkout, so a `/` in a heredoc is never one."""
    tool_input = denial.get("tool_input") if isinstance(denial, dict) else None
    if not isinstance(tool_input, dict):
        return "not_allowed"
    root, work = Path(root).resolve(), Path(work).resolve()

    def resolved(candidate):
        path = Path(candidate) if os.path.isabs(candidate) else root / candidate
        return Path(os.path.normpath(path.resolve()))

    fields = [tool_input[key] for key in ("file_path", "path", "notebook_path") if isinstance(tool_input.get(key), str)]
    words = []
    command = tool_input.get("command")
    if isinstance(command, str):
        words = [token for token in re.split(r"[\s;|&<>()'\"=]+", command) if "/" in token or token == ".."]
        for read in PEER_GIT_READ.finditer(command):
            branches = {int(number) for number in re.findall(r"\bbench/s(\d+)\b", read.group("rest"))}
            if branches - {index} or PEER_GIT_ARGS.search(read.group("rest")):
                return "peer_checkout"
        if PEER_GIT_LISTS.search(command):
            return "peer_checkout"
    for candidate in fields + words:
        path = resolved(candidate)
        if path.is_relative_to(work) and not path.is_relative_to(root):
            return "peer_checkout"
    if isinstance(reason, dict):
        return "outside_clone" if reason.get("decision_reason_type") == "workingDir" else "not_allowed"
    # No reason recorded: only a file tool's own path says where it aimed.
    return "outside_clone" if any(not resolved(field).is_relative_to(root) for field in fields) else "not_allowed"


DENIAL_KINDS = ("peer_checkout", "outside_clone", "not_allowed")


def denial_counts(denials, root, work, index, reasons):
    if not isinstance(denials, list):
        return None
    kinds = [classify_denial(denial, root, work, index,
                             reasons.get(denial.get("tool_use_id")) if isinstance(denial, dict) else None)
             for denial in denials]
    return {"total": len(kinds), **{kind: kinds.count(kind) for kind in DENIAL_KINDS}}


def workspace_session(workspace):
    match = re.fullmatch(r"bench-s(\d+)", str(workspace or ""))
    return int(match.group(1)) if match else None


def session_of(event):
    return workspace_session(event.get("workspace_id") or (event.get("failure") or {}).get("workspace_id"))


def session_numbers(events):
    """Each LangSpan session ID's benchmark session, from its registration."""
    numbers = {}
    for record in events:
        event = record.get("event", {})
        session = event.get("session") if event.get("type") == "session_registered" else None
        if isinstance(session, dict) and workspace_session(session.get("workspace_id")):
            numbers[session.get("session_id")] = workspace_session(session.get("workspace_id"))
    return numbers


def interface_notices(events):
    """Per benchmark session, how many interface notices LangSpan placed in
    its context: another worktree's in-flight signatures (C7) and alerts
    at write time (ADR 0029), from the coordinator's own event log."""
    numbers, counts = session_numbers(events), {}
    for record in events:
        event = record.get("event", {})
        items = {"inflight_delivered": "keys", "interface_alerted": "notice_ids"}.get(event.get("type"))
        session = numbers.get(event.get("session_id")) if items else None
        if session is not None and isinstance(event.get(items), list):
            counts[session] = counts.get(session, 0) + len(event[items])
    return counts


WRITE_TOOLS = ("Write", "Edit", "MultiEdit")


def hook_context(stdout):
    """The text a hook's reply placed in the model's context, or the reply
    itself when it is not a JSON object."""
    try:
        reply = json.loads(stdout)
    except ValueError:
        # A reply cut at the log's bound: its quotes are still escaped.
        return stdout.replace('\\"', '"')
    output = reply.get("hookSpecificOutput") if isinstance(reply, dict) else None
    context = output.get("additionalContext") if isinstance(output, dict) else None
    return context if isinstance(context, str) else ""


def notice_before_write(task, hook_log):
    """For a task that expects an interface notice (its `notice`), whether
    LangSpan's hooks placed one in the session's context before its first
    Write, Edit or MultiEdit of a file the task owns, in the order the hook
    log records them. A notice given by that write's own hook (a break
    alert, read with the write's result) is not before it. None when the
    task expects no notice, or there is no hook log or no such write."""
    if "notice" not in task or hook_log is None or not hook_log.is_file():
        return None
    owned = tuple("/" + unit.split("::", 1)[0] for unit in task["owns"])
    entries = []
    for line in hook_log.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if isinstance(entry, dict):
            entries.append(entry)
    first = next((position for position, entry in enumerate(entries)
                  if entry.get("event") == "PreToolUse" and entry.get("tool") in WRITE_TOOLS
                  and str(entry.get("file_path") or "").endswith(owned)), None)
    if first is None:
        return None
    return any(re.search(task["notice"], hook_context(str(entry.get("stdout") or ""))) for entry in entries[:first])


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
    """The coordinator's failed and recovered scans of each session's
    worktree."""
    if arm != "langspan":
        return {"status": "not_applicable", "failed": None, "recovered": None, "items": []}
    if events is None:
        return {"status": "unknown", "failed": None, "recovered": None, "items": []}
    items = []
    for record in events:
        event = record.get("event", {})
        if event.get("type") in ("scan_failed", "scan_recovered") and session_of(event) is not None:
            detail = (event.get("failure") or {}).get("detail")
            items.append({"session": session_of(event), "seq": record.get("seq", 0), "type": event["type"],
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
    """EVENTS is the coordinator's event log (LangSpan arm), or None when
    none was read."""
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
    refusals = refusal_items(events) if events is not None else []
    notices = interface_notices(events) if events is not None else {}
    work = Path(plan.get("work", output / "work"))
    tasks, models, token_list = [], set(), []
    for index, task in enumerate(plan["tasks"], 1):
        command = commands.get(("agent", "s%d" % index))
        text, transcript = "", None
        if command is not None:
            transcript = str(Path(runner_dir.name) / command["stdout_path"])
            text = (runner_dir / command["stdout_path"]).read_text(encoding="utf-8", errors="replace")
        totals = session_totals(session_results(text))
        result = totals["final"]
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
        token_list.append(totals["tokens"])
        models.update(totals["models"])
        hook_log = output / ("hooks-s%d.jsonl" % index)
        informed = (result.get("standin") or {}).get("informed") if result else None
        tasks.append({
            "session": index, "task_id": task["id"], "role": task["role"], "branch": branch(index),
            "process_status": process_status, "merged": bool(integration.get(index, {}).get("merged")),
            "completed": verdict.get("completed"), "accepted": verdict.get("accepted"),
            "detail": verdict.get("detail"), "stopped": stopped, "asked_question": asked,
            "results": totals["results"] if command is not None else None,
            "tool_check": tool_check(text),
            "permission_denials": denial_counts(totals["denials"], work / ("s%d" % index), work, index,
                                                denial_reasons(text)),
            "tokens": totals["tokens"], "cost_usd": totals["cost_usd"], "duration_ms": totals["duration_ms"],
            "turns": totals["turns"],
            "refusals": sum(item["session"] == index for item in refusals) if events is not None else None,
            "interface_notices": notices.get(index, 0) if events is not None else None,
            "transcript": transcript,
            "hook_log": hook_log.name if hook_log.is_file() else None,
            "standin_informed": informed if isinstance(informed, bool) else None,
            "notice_before_write": notice_before_write(task, hook_log) if plan["arm"] == "langspan" else None,
        })

    def total(field):
        values = [task[field] for task in tasks]
        return None if any(value is None for value in values) else sum(int(value) for value in values)

    denials = [task["permission_denials"] for task in tasks]
    costs = [task["cost_usd"] for task in tasks]
    reported = [tokens for tokens in token_list if tokens is not None]
    checks = [task["tool_check"]["status"] for task in tasks]
    failed_checks = [task["session"] for task in tasks if task["tool_check"]["status"] == "failed"]

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
        "isolation": isolation(plan["arm"]),
        "stagger_s": plan.get("stagger_s", 0),
        "auto_memory": plan.get("auto_memory", "not_applicable"),
        "tool_check": {
            "status": "failed" if failed_checks else ("passed" if checks and set(checks) == {"passed"} else "unknown"),
            "failed_sessions": failed_checks,
        },
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
        {key: sum(d[key] for d in denials) for key in ("total",) + DENIAL_KINDS},
        "cost_usd": None if any(cost is None for cost in costs) else round(sum(costs), 6),
        "turns": total("turns"),
        "refusals": {
            "status": "not_applicable" if plan["arm"] != "langspan" else ("reported" if events is not None else "unknown"),
            "count": len(refusals) if events is not None else None,
            "items": refusals,
            "sessions_registered": sum(r.get("event", {}).get("type") == "session_registered"
                                       for r in events) if events is not None else None,
            "admissions": sum(r.get("event", {}).get("type") == "lease_granted"
                              for r in events) if events is not None else None,
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
            "events": ["events.jsonl"] if events is not None else [],
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
                     "cwd": str(path), "timeout_seconds": plan["time_limit_s"] + start_delay(plan, index),
                     "max_output_bytes": TRANSCRIPT_BYTES}
                    for index, path in enumerate(clones, 1)
                ],
                "checks": [
                    {"id": "integrate", "argv": [python, "-I", "-B", driver, "integrate", plan["plan_path"]],
                     "cwd": str(repo), "timeout_seconds": ORACLE_SECONDS + 120, "assesses": "integrated_correctness"},
                ],
            }},
        }],
    }


def refuse_shared_memory(plan, argvs, envs):
    """Refuse a trial whose sessions, started with ARGVS and ENVS, would
    keep auto memory in one directory."""
    shared = shared_memory(plan["arm"], plan["work"], argvs, envs)
    if shared:
        raise Refused("sessions %s would share one Claude Code auto-memory directory, a channel between "
                      "sessions; every session must turn auto memory off" % ", ".join(map(str, shared)))


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
        work = Path(plan["work"])
        refuse_shared_memory(plan, [
            claude_argv(os.environ.get(CLAUDE_BIN_ENV, "claude"), "", plan["arm"], plan["model"],
                        peer_paths(work, plan["sessions"], index)) for index in range(1, plan["sessions"] + 1)],
            [session_env(plan, os.environ, config_dir)] * plan["sessions"])
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
                     model=os.environ.get(CLAUDE_MODEL_ENV) or None, langspan_given=args.langspan is not None,
                     stagger=args.stagger)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    work = output / "work"
    runner_dir = output / "runner"
    plan.update(output=str(output), work=str(work), plan_path=str(output / "plan.json"),
                langspan_commit=langspan_commit(), langspan=None, langspan_sha256=None,
                driver_command=getattr(args, "command", None))
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
        plan["agent_commands"] = [
            claude_argv(os.environ.get(CLAUDE_BIN_ENV, "claude"), "<prompts[%d]>" % (index - 1), plan["arm"],
                        plan["model"], peer_paths(work, plan["sessions"], index))
            for index in range(1, plan["sessions"] + 1)]
    coordinator, state, events, final, error, run_dir = None, None, None, None, None, None
    try:
        repo, base, checkouts = prepare_fixture(work, plan["sessions"], plan["arm"])
        plan.update(repo=str(repo), base=base)
        if langspan:
            # One coordinator for the repository whose worktrees the sessions
            # use. A Unix socket path holds about 100 bytes: keep it short.
            state = Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp"))
            plan["state_dir"] = str(state)
            for index, checkout in enumerate(checkouts, 1):
                install_hooks(langspan, state, index, checkout, output / ("hooks-s%d.jsonl" % index))
            install_skill(langspan, state, checkouts, work)
            coordinator = Coordinator(langspan, state, output / "coordinator.log")
        (output / "plan.json").write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
        manifest = output / "manifest.json"
        manifest.write_text(json.dumps(manifest_for(plan, checkouts, repo), indent=2) + "\n", encoding="utf-8")
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
        if state:
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
    parser.add_argument("--stagger", type=float, default=0,
                        help="start the session whose task changes an interface first, and the others this many "
                             "seconds later (default 0: all at once; at most %d)" % MAX_STAGGER)
    parser.add_argument("--langspan", type=Path, help="the langspan binary, for the langspan arm")
    parser.add_argument("--output", type=Path, help="a new result directory; required with --run")
    parser.add_argument("--run", action="store_true", help="prepare and run; without it, only print the plan")
    args = parser.parse_args(argv)
    # The exact driver command line, kept in plan.json with the trial.
    args.command = [sys.executable, str(Path(__file__).resolve()), *argv]
    try:
        if not args.run:
            plan = make_plan(args.arm, args.family, args.sessions, args.seed, args.agent, args.standin_mode,
                             args.time_limit, model=os.environ.get(CLAUDE_MODEL_ENV) or None,
                             langspan_given=args.langspan is not None, stagger=args.stagger)
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
