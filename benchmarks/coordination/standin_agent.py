#!/usr/bin/env python3
"""The dry-run stand-in for an agent session. It makes no model call.

``standin_agent.py PLAN SESSION MODE`` applies the fixed edit that the
session's task lists for MODE (``naive`` or ``coordinated``) in the current
directory. Where Claude Code project hooks are installed
(``.claude/settings.local.json``), it runs them as Claude Code would around
each read and edit, so the LangSpan arm's dry run passes through LangSpan's
real hooks, coordinator and event log. A refused edit is read again and
retried once, then skipped. It prints one JSON result in the shape of
``claude -p --output-format json`` and keeps a log of every hook call.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import uuid

HOOK_SECONDS = 30


def hook_commands(settings, event, tool):
    for entry in settings.get("hooks", {}).get(event, []):
        matcher = entry.get("matcher") or "*"
        if matcher == "*" or tool is None or re.fullmatch(matcher, tool):
            for hook in entry.get("hooks", []):
                if hook.get("type") == "command":
                    yield hook["command"], min(hook.get("timeout", HOOK_SECONDS), HOOK_SECONDS)


class Session:
    def __init__(self, settings, log, cwd=None):
        self.settings = settings
        self.log = log
        self.cwd = cwd or os.getcwd()
        self.session_id = str(uuid.uuid4())
        self.calls = 0
        self.refusals = []

    def fire(self, event, tool=None, tool_input=None, tool_use_id=None, **extra):
        """Run every matching hook; return True when one refuses the call."""
        payload = {"session_id": self.session_id, "cwd": self.cwd, "hook_event_name": event, **extra}
        if tool is not None:
            payload.update(tool_name=tool, tool_input=tool_input, tool_use_id=tool_use_id)
        denied = False
        for command, timeout in hook_commands(self.settings, event, tool):
            self.calls += 1
            try:
                done = subprocess.run(["/bin/sh", "-c", command], input=json.dumps(payload), cwd=self.cwd,
                                      capture_output=True, text=True, timeout=timeout)
                stdout, code = done.stdout, done.returncode
            except subprocess.TimeoutExpired:
                stdout, code = "", None
            try:
                decision = json.loads(stdout).get("hookSpecificOutput", {}).get("permissionDecision") if stdout.strip() else None
            except (ValueError, AttributeError):
                decision = None
            denied |= decision == "deny" or code == 2
            self.log.write(json.dumps({"event": event, "tool": tool, "tool_use_id": tool_use_id,
                                       "exit_code": code, "decision": decision, "stdout": stdout[:4000]}) + "\n")
        return denied

    def read(self, path):
        self.fire("PostToolUse", "Read", {"file_path": str(path.resolve())}, "read-" + uuid.uuid4().hex)

    def edit(self, path, tool, tool_input, apply):
        for attempt in (1, 2):
            call = "edit-" + uuid.uuid4().hex
            if not self.fire("PreToolUse", tool, tool_input, call):
                apply()
                self.fire("PostToolUse", tool, tool_input, call)
                return True
            self.refusals.append({"path": str(path), "attempt": attempt})
            self.read(path)
        return False


def main(argv):
    if len(argv) != 3 or argv[2] not in ("naive", "coordinated"):
        print("usage: standin_agent.py PLAN SESSION naive|coordinated", file=sys.stderr)
        return 2
    plan = json.loads(Path(argv[0]).read_text(encoding="utf-8"))
    index = int(argv[1])
    task = plan["tasks"][index - 1]
    hooks_file = Path(".claude/settings.local.json")
    settings = json.loads(hooks_file.read_text(encoding="utf-8")) if hooks_file.is_file() else {}
    log_path = Path(os.environ.get("LANGSPAN_BENCH_RUN_DIR", ".")) / ("standin-s%d.jsonl" % index)
    edited = []
    with open(log_path, "a", encoding="utf-8") as log:
        session = Session(settings, log)
        session.fire("SessionStart", source="startup")
        session.fire("UserPromptSubmit", prompt=plan["prompts"][index - 1])
        for op in task["standin"][argv[2]]:
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
        session.fire("Stop", stop_hook_active=False)
        session.fire("SessionEnd", reason="other")
    print(json.dumps({
        "type": "result", "subtype": "success", "is_error": False, "num_turns": 1,
        "result": "Stand-in %s edit of %s." % (argv[2], ", ".join(edited) or "nothing"),
        "session_id": session.session_id, "permission_denials": [],
        "standin": {"mode": argv[2], "hook_calls": session.calls, "refusals": session.refusals, "edited": edited},
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
