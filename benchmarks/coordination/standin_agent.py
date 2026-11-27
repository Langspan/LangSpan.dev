#!/usr/bin/env python3
"""The dry-run stand-in for an agent session. It makes no model call.

``standin_agent.py PLAN SESSION MODE`` applies one of the edit lists its
task defines (see tasks.py) in the current directory:

- ``ignore`` makes the edit a session would make knowing nothing of its
  peers, whatever its arm told it.
- ``hedge`` makes the edit that holds up whatever peers do, without
  knowing: for a contradiction task, its value set locally and the shared
  default kept, as the pilot's sessions did.
- ``workaround`` makes a changer finish without the interface change, as
  the third pilot's did by subtracting two one-argument calls. Every other
  task makes the same edit as ``ignore``.
- ``follow`` reads the information its arm gives about peers: in the
  awareness-only arm, the peer task text in its prompt; in the LangSpan arm,
  the briefings LangSpan's hooks return and ``langspan brief``. When that
  information matches the task's trigger, it makes the edit the task's
  details ask for; otherwise it makes the same edit as ``ignore``.

Where Claude Code project hooks are installed (``.claude/settings.local.json``),
it runs them as Claude Code would around each read and edit, so the
LangSpan arm's dry run passes through LangSpan's real hooks, coordinator and
event log. Unlike a host, it waits for each hook's decision past the hook's
configured timeout (see HOOK_SECONDS), so the dry run's result does not
depend on how loaded the machine is. When a hook returns, any process it
left in its group is ended, and a signal that stops the stand-in first
ends the hook it is running: SIGTERM, then SIGKILL (see TERM_SECONDS), so
that the driver's ``hook-log`` wrapper can end the hook it runs in a group
of its own. A ``follow`` session there whose task depends
on a peer's first waits, within a bound, until LangSpan has told it every
peer's task. Before it ends it waits until every peer shows edits and
LangSpan's interface notice its task expects (the task's ``notice``) has
arrived, so that no peer acts before it could have been told of this one; a
session whose task changes an interface (``leads``) waits until every peer
has ended, so its change stays in flight while they work. When the notice
tells it of the change only after its edit, it makes the informed edit then.
A refused edit is read again and retried once, then skipped. It prints a
short transcript in the shape of ``claude -p --output-format stream-json``:
a first line listing its tools, as Claude Code's does, each edit's
``tool_use`` and ``tool_result``, and the result line last.
"""

import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time
import uuid

# How long the stand-in waits for one hook. A host gives a hook its
# configured timeout (5 s for LangSpan's) and goes on without it; the
# stand-in waits for its decision up to this bound instead, so a loaded
# machine that starts a hook late does not change what the dry run does.
# A hook still running at the bound is ended with every process it started.
HOOK_SECONDS = 30
# How long a hook's process group has to end after SIGTERM, before SIGKILL.
# The driver's `hook-log` wrapper runs its hook in another group, which no
# signal to the wrapper's group reaches: on SIGTERM the wrapper ends that
# group, and SIGKILL at once would leave it running. The wait ends as soon
# as the group is empty.
TERM_SECONDS = 2
BRIEF_SECONDS = 10
# How long a following session waits to hear from its peers, per phase, when
# its plan gives no session time limit. With one, every wait may run until
# WAIT_MARGIN_SECONDS before the driver would stop the session: on a loaded
# machine, a peer's edit and LangSpan's index of it can take longer than any
# fixed wait, and a wait that ran out would make the session act uninformed
# instead of failing for the reason it did.
WAIT_SECONDS = 75
WAIT_MARGIN_SECONDS = 30
POLL_SECONDS = 1
MAX_PEERS = 16


def hook_commands(settings, event, tool):
    """The foreground command hooks for EVENT. A background hook (`async`
    or `asyncRewake`, such as LangSpan's between-step watcher) is left out:
    the stand-in has no model for it to wake, and run in the foreground it
    would hold every prompt."""
    for entry in settings.get("hooks", {}).get(event, []):
        matcher = entry.get("matcher") or "*"
        if matcher == "*" or tool is None or re.fullmatch(matcher, tool):
            for hook in entry.get("hooks", []):
                if hook.get("type") == "command" and not (hook.get("async") or hook.get("asyncRewake")):
                    yield hook["command"], hook.get("timeout", HOOK_SECONDS)


# The hook running now, if any. Its process group is outside the runner's,
# so the stand-in ends it before it exits on a signal (see stop_on_signal).
RUNNING = []


def signal_group(process, signum):
    """Whether SIGNUM reached a member of PROCESS's group."""
    try:
        os.killpg(process.pid, signum)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def kill_group(process):
    """End every process in PROCESS's group: SIGTERM, then SIGKILL to
    whatever is left after TERM_SECONDS. A group already empty gets
    neither, so a reused ID is not signaled."""
    if not signal_group(process, signal.SIGTERM):
        return
    deadline = time.monotonic() + TERM_SECONDS
    while time.monotonic() < deadline:
        # An unreaped leader would keep the group in existence.
        process.poll()
        if not signal_group(process, 0):
            return
        time.sleep(0.01)
    signal_group(process, signal.SIGKILL)


def run_hook(command, payload, cwd):
    """Run one hook in a process group of its own, for up to HOOK_SECONDS.
    Returns its stdout and exit code, or ("", None) when it was ended.
    Every process left in the hook's group when it returns is ended too
    (see kill_group), so a background child the hook started cannot act
    after the call."""
    process = subprocess.Popen(["/bin/sh", "-c", command], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, cwd=cwd, text=True, start_new_session=True)
    RUNNING.append(process)
    try:
        stdout, _ = process.communicate(payload, timeout=HOOK_SECONDS)
        return stdout, process.returncode
    except subprocess.TimeoutExpired:
        kill_group(process)
        process.communicate()
        return "", None
    finally:
        # The leader is reaped, but its group ID is not reused while any
        # member of the group is alive, so this reaches only the hook's own.
        kill_group(process)
        RUNNING.remove(process)


def stop_on_signal(signum, _frame):
    """End the running hook's process group, then die of SIGNUM. The runner
    stops an attempt by signaling the stand-in's group, which does not
    reach the hook's. SIGKILL cannot be caught; the runner sends SIGTERM
    first. The hook's group has its SIGTERM before any wait, so a wrapper
    there ends its own hook even when the runner kills the stand-in
    during the wait."""
    for process in list(RUNNING):
        # A reaped leader's PID can be reused; its group is no longer ours.
        if process.returncode is None:
            kill_group(process)
    signal.signal(signum, signal.SIG_DFL)
    os.kill(os.getpid(), signum)


class Session:
    def __init__(self, settings, log, cwd=None):
        self.settings = settings
        self.log = log
        self.cwd = cwd or os.getcwd()
        self.session_id = str(uuid.uuid4())
        self.calls = 0
        self.refusals = []
        self.context = []
        # Hooks that took longer than their host timeout.
        self.late_hooks = 0

    def fire(self, event, tool=None, tool_input=None, tool_use_id=None, **extra):
        """Run every matching hook; return True when one refuses the call."""
        payload = {"session_id": self.session_id, "cwd": self.cwd, "hook_event_name": event, **extra}
        if tool is not None:
            payload.update(tool_name=tool, tool_input=tool_input, tool_use_id=tool_use_id)
        denied = False
        for command, timeout in hook_commands(self.settings, event, tool):
            self.calls += 1
            started = time.monotonic()
            stdout, code = run_hook(command, json.dumps(payload), self.cwd)
            elapsed = time.monotonic() - started
            late = elapsed > timeout
            self.late_hooks += late
            try:
                output = json.loads(stdout).get("hookSpecificOutput", {}) if stdout.strip() else {}
            except (ValueError, AttributeError):
                output = {}
            decision = output.get("permissionDecision") if isinstance(output, dict) else None
            context = output.get("additionalContext") if isinstance(output, dict) else None
            if isinstance(context, str):
                self.context.append(context)
            denied |= decision == "deny" or code == 2
            self.log.write(json.dumps({"event": event, "tool": tool, "tool_use_id": tool_use_id,
                                       "exit_code": code, "decision": decision, "stdout": stdout[:4000],
                                       "elapsed_ms": round(elapsed * 1000, 1), "late": late}) + "\n")
        return denied

    def read(self, path):
        self.fire("PostToolUse", "Read", {"file_path": str(path.resolve())}, "read-" + uuid.uuid4().hex)

    def edit(self, path, tool, tool_input, apply):
        """Make one edit through the hooks, printing its call and result as
        Claude Code's stream-json does: a refused call's result is an error."""
        for attempt in (1, 2):
            call = "edit-" + uuid.uuid4().hex
            print(json.dumps({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": call, "name": tool, "input": tool_input}]}}), flush=True)
            denied = self.fire("PreToolUse", tool, tool_input, call)
            if not denied:
                apply()
            print(json.dumps({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": call, "is_error": denied,
                 "content": "refused by a hook" if denied else "done"}]}}), flush=True)
            if not denied:
                self.fire("PostToolUse", tool, tool_input, call)
                return True
            self.refusals.append({"path": str(path), "attempt": attempt})
            self.read(path)
        return False


def briefed_peers(session):
    """The peers in this session's `langspan brief`, by workspace: the task
    line each was last heard with and whether it shows edits."""
    langspan = shutil.which("langspan")
    if not langspan or not os.environ.get("LANGSPAN_STATE_DIR"):
        return {}
    try:
        done = subprocess.run([langspan, "brief", "claude-code." + session.session_id, "--max-peers", str(MAX_PEERS),
                               "--json", "--request-id", "standin-brief-" + uuid.uuid4().hex],
                              capture_output=True, text=True, timeout=BRIEF_SECONDS, cwd=session.cwd)
        briefing = json.loads(done.stdout)["outcome"]["briefing"]
    except (OSError, subprocess.TimeoutExpired, ValueError, KeyError, TypeError):
        return {}
    peers = {}
    for peer in briefing.get("peers") or []:
        peers[str(peer.get("workspace_id"))] = {"task": peer.get("summary"), "ended": peer.get("ended") is True,
                                                 "edited": bool(peer.get("editing") or peer.get("recent"))}
    return peers


def wait_deadline(plan, started):
    """When a wait ends: WAIT_MARGIN_SECONDS before the session's time limit
    from STARTED, or WAIT_SECONDS from now for a plan without one."""
    limit = plan.get("time_limit_s")
    if isinstance(limit, (int, float)) and limit > WAIT_MARGIN_SECONDS:
        return started + limit - WAIT_MARGIN_SECONDS
    return time.monotonic() + WAIT_SECONDS


def wait_for(session, peers_expected, ready, deadline, also=lambda: True):
    """Poll LangSpan until READY holds for every expected peer and ALSO
    holds, or until DEADLINE (monotonic). Each poll is a read of a file the
    task uses, as an agent's would be, so hook briefings arrive as well.
    Returns what was heard."""
    heard = {}
    while True:
        for name, peer in briefed_peers(session).items():
            # A peer's task line goes once it ends; keep the one heard before.
            heard[name] = dict(peer, task=peer["task"] or heard.get(name, {}).get("task"))
        if len([name for name, peer in heard.items() if ready(peer)]) >= peers_expected and also():
            return heard
        if time.monotonic() > deadline:
            return heard
        session.read(Path("shelf") / "settings.py")
        time.sleep(POLL_SECONDS)


def apply_edits(session, ops):
    """Make each edit of OPS through SESSION; return the paths edited."""
    edited = []
    for op in ops:
        for name in op.get("paths", [op.get("path")]):
            path = Path(name)
            if op["op"] == "write":
                path.parent.mkdir(parents=True, exist_ok=True)
                content = op["content"]
                done = session.edit(path, "Write", {"file_path": str(path.resolve()), "content": content},
                                    lambda: path.write_text(content, encoding="utf-8"))
            else:
                session.read(path)
                before = path.read_text(encoding="utf-8")
                if op["old"] not in before:
                    continue
                after = before.replace(op["old"], op["new"]) if op.get("all") else before.replace(op["old"], op["new"], 1)
                done = session.edit(path, "Edit", {"file_path": str(path.resolve()), "old_string": op["old"],
                                                   "new_string": op["new"], "replace_all": bool(op.get("all"))},
                                    lambda: path.write_text(after, encoding="utf-8"))
            if done:
                edited.append(name)
    return edited


def repair(plan):
    """The repair round's stand-in, in the combined tree: every task's
    informed edit, in session order, skipping a replacement whose result
    is already there. It does not resolve Git's conflict markers, so a tree
    with a refused merge stays broken."""
    print(json.dumps({"type": "system", "subtype": "init", "session_id": str(uuid.uuid4()), "model": "standin",
                      "tools": plan.get("allowed_tools", []), "mcp_servers": []}), flush=True)
    edited = []
    for task in plan["tasks"]:
        for op in task["standin"]["follow"]:
            for name in op.get("paths", [op.get("path")]):
                path = Path(name)
                if op["op"] == "write":
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(op["content"], encoding="utf-8")
                else:
                    before = path.read_text(encoding="utf-8") if path.is_file() else ""
                    if op["old"] not in before or op["new"] in before:
                        continue
                    count = -1 if op.get("all") else 1
                    path.write_text(before.replace(op["old"], op["new"], count), encoding="utf-8")
                if name not in edited:
                    edited.append(name)
    print(json.dumps({
        "type": "result", "subtype": "success", "is_error": False, "num_turns": 1,
        "result": "Stand-in repair edit of %s." % (", ".join(edited) or "nothing"),
        "session_id": str(uuid.uuid4()), "permission_denials": [],
        "standin": {"mode": "repair", "edited": edited},
    }))
    return 0


def main(argv):
    if len(argv) == 3 and argv[2] == "repair":
        return repair(json.loads(Path(argv[0]).read_text(encoding="utf-8")))
    if len(argv) != 3 or argv[2] not in ("ignore", "follow", "hedge", "workaround"):
        print("usage: standin_agent.py PLAN SESSION ignore|follow|hedge|workaround, or PLAN 0 repair",
              file=sys.stderr)
        return 2
    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, stop_on_signal)
    started = time.monotonic()
    plan = json.loads(Path(argv[0]).read_text(encoding="utf-8"))
    deadline = wait_deadline(plan, started)
    index, mode = int(argv[1]), argv[2]
    task = plan["tasks"][index - 1]
    prompt = plan["prompts"][index - 1]
    hooks_file = Path(".claude/settings.local.json")
    settings = json.loads(hooks_file.read_text(encoding="utf-8")) if hooks_file.is_file() else {}
    log_path = Path(os.environ.get("LANGSPAN_BENCH_RUN_DIR", ".")) / ("standin-s%d.jsonl" % index)
    informed, revised, heard = False, False, {}
    session_id = str(uuid.uuid4())
    # Claude Code's first event lists the session's tools; the tool guard checks it.
    print(json.dumps({"type": "system", "subtype": "init", "session_id": session_id, "model": "standin",
                      "tools": plan.get("allowed_tools", []), "mcp_servers": []}), flush=True)
    with open(log_path, "a", encoding="utf-8") as log:
        session = Session(settings, log)
        session.session_id = session_id
        session.fire("SessionStart", source="startup")
        session.fire("UserPromptSubmit", prompt=prompt)
        sharing = bool(settings) and shutil.which("langspan") is not None
        # The whole prompt but the session's own task counts, as it would
        # for a real agent, so peer information leaking into the shared
        # text shows in a dry run.
        shared = prompt.replace(task["task"], "").replace(task["details"], "")

        def told():
            information = "\n".join(
                [shared] + session.context + [peer["task"] for peer in heard.values() if peer["task"]])
            return "trigger" in task and re.search(task["trigger"], information) is not None

        # Only a task that depends on its peers' work waits to hear of it.
        if mode == "follow" and sharing and "trigger" in task:
            heard = wait_for(session, plan["sessions"] - 1, lambda peer: bool(peer["task"]), deadline)
        for name in task.get("reads", []):
            session.read(Path(name))
        informed = mode == "follow" and told()
        if informed:
            edits = "follow"
        elif mode == "hedge" or (mode == "workaround" and "workaround" in task["standin"]):
            edits = mode
        else:
            edits = "ignore"
        edited = apply_edits(session, task["standin"][edits])
        if mode == "follow" and sharing:
            if task.get("leads"):
                # Peers learn of this session's change while it is in
                # flight, so it stays until every peer has ended.
                wait_for(session, plan["sessions"] - 1, lambda peer: peer["ended"], deadline)
            else:
                # A peer that shows edits has finished waiting for this one.
                # A notice the task expects arrives with a later briefing.
                notice = task.get("notice")
                wait_for(session, plan["sessions"] - 1, lambda peer: peer["edited"], deadline,
                         lambda: not notice or any(re.search(notice, text) for text in session.context))
            if not informed and told():
                # Told only after its edit, as by an alert at the write:
                # an agent that follows it fixes its call.
                informed = revised = True
                edited += apply_edits(session, task["standin"]["follow"])
        session.fire("Stop", stop_hook_active=False)
        session.fire("SessionEnd", reason="other")
    print(json.dumps({
        "type": "result", "subtype": "success", "is_error": False, "num_turns": 1,
        "result": "Stand-in %s edit of %s." % (mode, ", ".join(edited) or "nothing"),
        "session_id": session.session_id, "permission_denials": [],
        "standin": {"mode": mode, "informed": informed, "revised": revised, "peers_heard": sorted(heard),
                    "hook_calls": session.calls, "late_hooks": session.late_hooks,
                    "refusals": session.refusals, "edited": edited},
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
