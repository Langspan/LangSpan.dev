"""The benchmark driver, end to end in dry-run mode.

Real temporary Git repositories, clones and worktrees, real child
processes, and in the LangSpan arm the real langspan binary with its SQLite
store and Unix socket. No model runs: the agent is standin_agent.py, or a
shell script standing in for the CLI where the tool guard is tested. Set
LANGSPAN_BENCH_LANGSPAN to the binary (`cargo test`
does, through tests/benchmark_harness.rs); without it the LangSpan-arm
tests fail.
"""

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
import unittest

import driver
import standin_agent
import tasks

HERE = Path(__file__).resolve().parent


def langspan_binary():
    value = os.environ.get("LANGSPAN_BENCH_LANGSPAN")
    if not value:
        raise AssertionError("set LANGSPAN_BENCH_LANGSPAN to the langspan binary built from this checkout")
    return Path(value)


def task_line(prompt):
    """The line LangSpan shares as a session's task, without its marker."""
    lines = [line for line in prompt.splitlines() if line.startswith("Task: ")]
    assert len(lines) == 1, prompt
    return lines[0][len("Task: "):]


class Temporary(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="langspan-k1-test-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()


class DriverUnitTests(Temporary):
    def test_assignment_is_seeded_sized_and_shaped(self):
        for n in tasks.SESSION_COUNTS:
            rename = tasks.assign("rename_vs_new_caller", n, 7)
            self.assertEqual(rename, tasks.assign("rename_vs_new_caller", n, 7))
            self.assertEqual(len(rename), n)
            self.assertEqual([t["role"] for t in rename].count("rename"), 1)
            self.assertEqual(len({t["id"] for t in rename}), n)
        self.assertNotEqual([t["id"] for t in tasks.assign("rename_vs_new_caller", 5, 1)],
                            [t["id"] for t in tasks.assign("rename_vs_new_caller", 5, 2)])
        with self.assertRaises(ValueError):
            tasks.assign("rename_vs_new_caller", 3, 1)

    def test_contradiction_keys_come_paired_or_alone_so_neither_fixed_edit_always_wins(self):
        """A key two tasks want at different values must stay unchanged; a key
        only one task wants must change. Without knowing whether a partner
        exists, neither edit is right in every cell."""
        layouts = {n: set() for n in tasks.SESSION_COUNTS}
        for seed in range(40):
            for n in tasks.SESSION_COUNTS:
                assigned = tasks.assign("contradictory_defaults", n, seed)
                self.assertEqual(assigned, tasks.assign("contradictory_defaults", n, seed))
                self.assertEqual(len(assigned), n)
                self.assertEqual(len({t["id"] for t in assigned}), n)
                keys = [t["pair"] for t in assigned if "pair" in t]
                self.assertTrue(all(keys.count(key) in (1, 2) for key in keys))
                for key in set(keys):
                    sides = [t for t in assigned if t.get("pair") == key]
                    self.assertEqual(len({t["value"] for t in sides}), len(sides), "a pair wants two values")
                pairs = sum(keys.count(key) == 2 for key in set(keys))
                alone = sum(keys.count(key) == 1 for key in set(keys))
                controls = [t["role"] for t in assigned].count("control")
                self.assertLessEqual(controls, 1)
                if n >= 5:
                    self.assertGreaterEqual(pairs, 1, (n, seed))
                    self.assertGreaterEqual(alone, 1, (n, seed))
                layouts[n].add((pairs, alone, controls))
        # The seed varies the layout wherever the pool allows it.
        self.assertGreater(len(layouts[2]), 1, layouts)
        self.assertGreater(len(layouts[5]), 1, layouts)

    def option(self, argv, flag):
        """The values after FLAG, up to the next option."""
        values = []
        for value in argv[argv.index(flag) + 1:]:
            if value.startswith("--"):
                break
            values.append(value)
        return values

    def test_claude_command_streams_its_transcript_keeps_prompts_and_limits_tools(self):
        for arm in driver.ARMS:
            argv = driver.claude_argv("claude", "do the task", arm)
            self.assertEqual(argv[:3], ["claude", "-p", "do the task"])
            self.assertEqual(argv[argv.index("--permission-mode") + 1], "default")
            # Nothing approves a permission prompt: anything that would ask is refused.
            self.assertEqual(self.option(argv, "--permission-prompts"), ["none"])
            # The whole transcript, one JSON object per line: -p needs --verbose for it.
            self.assertEqual(argv[argv.index("--output-format") + 1], "stream-json")
            self.assertIn("--verbose", argv)
            self.assertFalse(set(argv) & set(driver.FORBIDDEN_CLAUDE_FLAGS))
            self.assertFalse(any("bypass" in a.lower() or "acceptedits" in a.lower() for a in argv))
            allowed = self.option(argv, "--allowedTools")
            self.assertIn("Edit(./**)", allowed)
            self.assertNotIn("Bash", allowed, "no unrestricted shell")
            self.assertEqual(any(t.startswith("Bash(langspan") for t in allowed), arm == "langspan")
            # The built-in tools are exactly the arm's set: no cross-session tool.
            self.assertEqual(self.option(argv, "--tools"), [",".join(driver.session_tools(arm))])
            self.assertEqual("Skill" in driver.session_tools(arm), arm == "langspan")
            # No MCP server or hosted connector from any configuration.
            self.assertIn("--strict-mcp-config", argv)
            self.assertEqual(json.loads(self.option(argv, "--mcp-config")[0]), {"mcpServers": {}})
            denied = self.option(argv, "--disallowedTools")
            for tool in ("ListAgents", "SendMessage", "ToolSearch", "WebSearch", "WebFetch", "Task", "Workflow"):
                self.assertIn(tool, denied, arm)
                self.assertNotIn(tool, driver.session_tools(arm))
            # Every arm denies reading other sessions' branches, refs and stash.
            for rule in ("Bash(git log *bench/*)", "Bash(git diff *bench/*)", "Bash(git log *--all*)",
                         "Bash(git show *refs/*)", "Bash(git diff *--no-index*)", "Bash(git stash*)",
                         "Bash(git worktree*)", "Bash(git branch*)", "Bash(git -C *)"):
                self.assertIn(rule, denied, arm)
            # Writing one's own work stays allowed: no rule names `git add` or `git commit`.
            self.assertFalse([rule for rule in denied if rule.startswith(("Bash(git add", "Bash(git commit",
                                                                          "Bash(git *"))], denied)
        self.assertEqual(driver.claude_argv("claude", "x", "worktrees")[3:],
                         driver.claude_argv("claude", "x", "awareness_only")[3:])
        # Every file tool is denied the other sessions' checkouts and the run's repositories.
        work = Path("/runs/k1/work")
        paths = driver.peer_paths(work, 3, 2)
        self.assertEqual(sorted(path.name for path in paths),
                         ["integration", "repo", "s1", "s3", "shared", "skill-home"])
        denied = self.option(driver.claude_argv("claude", "x", "langspan", denied_paths=paths), "--disallowedTools")
        for rule in ("Read(//runs/k1/work/s1/**)", "Edit(//runs/k1/work/s3/**)", "Read(//runs/k1/work/repo/**)"):
            self.assertIn(rule, denied)
        self.assertFalse([rule for rule in denied if "/work/s2/" in rule], "a session keeps its own checkout")

    def test_interface_notices_are_counted_per_session_from_the_event_log(self):
        def registered(session_id, workspace):
            return {"event": {"type": "session_registered",
                              "session": {"session_id": session_id, "workspace_id": workspace}}}
        events = [registered("claude-code.a", "bench-s1"), registered("claude-code.b", "bench-s2"),
                  {"event": {"type": "inflight_delivered", "session_id": "claude-code.b", "keys": ["k1", "k2"]}},
                  {"event": {"type": "interface_alerted", "session_id": "claude-code.b", "notice_ids": ["n1"]}},
                  {"event": {"type": "inflight_delivered", "session_id": "claude-code.unknown", "keys": ["k3"]}},
                  {"event": {"type": "briefing_delivered", "session_id": "claude-code.a", "signature": "x"}}]
        self.assertEqual(driver.interface_notices(events), {2: 3})

    def guard(self, script, arm="worktrees", timeout=30):
        """Run SCRIPT as the session's agent through the tool guard, as
        `driver.py session` does; returns its exit code and transcript."""
        agent = self.root / "agent.sh"
        agent.write_text("#!/bin/sh\n" + script)
        agent.chmod(0o755)
        transcript = self.root / "transcript.jsonl"
        with open(transcript, "wb") as sink:
            code = driver.guard_session([str(agent)], dict(os.environ), driver.session_tools(arm), sink,
                                        stop_seconds=2)
        return code, transcript.read_text()

    def init_line(self, tools, servers=()):
        return "printf '%%s\\n' %s\n" % shlex.quote(json.dumps(
            {"type": "system", "subtype": "init", "tools": list(tools),
             "mcp_servers": [{"name": name, "status": "connected"} for name in servers]}))

    def test_the_tool_guard_passes_a_session_whose_tools_are_the_arms_own(self):
        code, text = self.guard(self.init_line(driver.session_tools("worktrees"))
                                + "printf '%s\\n' '{\"type\":\"result\",\"num_turns\":1}'\nexit 0\n")
        self.assertEqual(code, 0, text)
        self.assertEqual(driver.tool_check(text), {"status": "passed", "unexpected": []})
        self.assertEqual(driver.session_results(text)[0]["num_turns"], 1)

    def test_the_tool_guard_stops_a_session_offered_a_cross_session_tool_or_a_connector(self):
        """The preflight: the session's first event lists its tools, and a
        session offered anything else is stopped before it can act."""
        cases = {
            "cross-session tool": (self.init_line(driver.session_tools("worktrees") + ("ListAgents", "SendMessage")),
                                   ["ListAgents", "SendMessage"]),
            "hosted connector tool": (self.init_line(driver.session_tools("langspan")
                                                     + ("mcp__claude_ai_Gmail__send_message",)),
                                      ["mcp__claude_ai_Gmail__send_message"]),
            "MCP server": (self.init_line(driver.session_tools("worktrees"), ["claude.ai Slack"]),
                           ["MCP server: claude.ai Slack"]),
            "a later tool list": (self.init_line(driver.session_tools("worktrees"))
                                  + self.init_line(driver.session_tools("worktrees"), ["claude.ai Docs"]),
                                  ["MCP server: claude.ai Docs"]),
            "a call to an unlisted tool": (self.init_line(driver.session_tools("worktrees")) + "printf '%s\\n' "
                                           + shlex.quote(json.dumps({"type": "assistant", "message": {"content": [
                                               {"type": "tool_use", "name": "SendMessage", "input": {}}]}})) + "\n",
                                           ["SendMessage"]),
        }
        for name, (start, unexpected) in cases.items():
            with self.subTest(name):
                started = time.monotonic()
                arm = "langspan" if "Skill" in start else "worktrees"
                code, text = self.guard(start + "sleep 20\nprintf '%s\\n' '{\"type\":\"late\"}'\n", arm)
                self.assertLess(time.monotonic() - started, 15, "the session was stopped, not waited for")
                self.assertEqual(code, driver.TOOL_CHECK_EXIT, text)
                self.assertNotIn('"late"', text)
                self.assertEqual(driver.tool_check(text), {"status": "failed", "unexpected": unexpected})
        # A session that never lists its tools cannot be checked, so it fails too.
        code, text = self.guard("printf '%s\\n' '{\"type\":\"result\"}'\nexit 0\n")
        self.assertEqual(code, driver.TOOL_CHECK_EXIT)
        self.assertEqual(driver.tool_check(text)["status"], "failed")
        # An agent that cannot start fails, with a verdict all the same.
        with open(self.root / "missing.jsonl", "wb") as sink:
            code = driver.guard_session([str(self.root / "no-such-agent")], dict(os.environ),
                                        driver.session_tools("worktrees"), sink)
        self.assertEqual(code, driver.TOOL_CHECK_EXIT)
        self.assertEqual(driver.tool_check((self.root / "missing.jsonl").read_text())["status"], "failed")
        # A transcript the guard never finished has no verdict.
        self.assertEqual(driver.tool_check(""), {"status": "unknown", "unexpected": []})

    def test_a_failed_tool_check_fails_the_trial_and_reaches_the_record(self):
        plan = driver.make_plan("worktrees", "rename_vs_new_caller", 2, 1, agent="claude")
        runner_dir = self.root / "runner"
        (runner_dir / "r").mkdir(parents=True)
        lines = [{"type": "system", "subtype": "init", "tools": ["Read", "SendMessage"], "mcp_servers": []},
                 {"type": driver.TOOL_CHECK_TYPE, "status": "failed", "unexpected": ["SendMessage"]}]
        (runner_dir / "r" / "agent-s1.stdout").write_text("\n".join(json.dumps(line) for line in lines) + "\n")
        (runner_dir / "r" / "agent-s2.stdout").write_text("")
        final = {"run_id": "0" * 32, "status": "failed", "agent_phase_elapsed_ns": 1,
                 "commands": [{"phase": "agent", "id": "s%d" % i, "status": "failed" if i == 1 else "succeeded",
                               "stdout_path": "r/agent-s%d.stdout" % i} for i in (1, 2)]}
        plan.update(run_id="0" * 32, output=str(self.root), plan_path=str(self.root / "plan.json"),
                    work=str(self.root / "work"))
        record = driver.build_record(plan, final, runner_dir, None, None, "t", 0.0)
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["status"], "failed")
        self.assertEqual(record["tool_check"], {"status": "failed", "failed_sessions": [1]})
        self.assertEqual([t["tool_check"]["status"] for t in record["tasks"]], ["failed", "unknown"])
        self.assertEqual(record["tasks"][0]["tool_check"]["unexpected"], ["SendMessage"])

    def test_each_task_line_names_what_its_peers_need_within_what_langspan_shares(self):
        """LangSpan shares one `Task:` line of at most 200 bytes. The peer
        that needs to know of a task must recognize it from that line alone,
        and no other task's line may set it off."""
        for family in tasks.FAMILIES:
            for n in tasks.SESSION_COUNTS:
                assigned = tasks.assign(family, n, 3)
                lines = [task_line(driver.prompt_for("langspan", family, assigned, i)) for i in range(1, n + 1)]
                for index, task in enumerate(assigned):
                    self.assertLessEqual(len(lines[index].encode()), 200, lines[index])
                    if "trigger" not in task:
                        continue
                    peers = [line for other, line in enumerate(lines) if other != index]
                    hits = [line for line in peers if re.search(task["trigger"], line)]
                    if family == "discovered_change":
                        # No task line tells of the change: only the work does.
                        partnered = False
                    else:
                        partnered = family == "rename_vs_new_caller" or [t.get("pair") for t in assigned].count(
                            task["pair"]) == 2
                    # A key no other task wants sets off nothing.
                    self.assertEqual(len(hits), 1 if partnered else 0, (task["id"], hits))
                    if family == "rename_vs_new_caller":
                        self.assertIn("load_config", hits[0])
                    elif partnered:
                        self.assertIn(task["key"], hits[0])

    def test_only_the_awareness_arm_gives_peers_task_text_and_every_prompt_states_success(self):
        for family in tasks.FAMILIES:
            assigned = tasks.assign(family, 5, 3)
            for arm in driver.ARMS:
                prompt = driver.prompt_for(arm, family, assigned, 2)
                self.assertIn(assigned[1]["task"], prompt)
                self.assertIn(assigned[1]["details"], prompt)
                self.assertIn(tasks.SUCCESS[family], prompt)
                peers = [peer for i, peer in enumerate(assigned, 1) if i != 2]
                given = [peer["task"] in prompt and peer["details"] in prompt for peer in peers]
                self.assertEqual(all(given), arm == "awareness_only", arm)
                self.assertEqual(any(peer["task"] in prompt for peer in peers), arm == "awareness_only", arm)

    def test_prompts_outside_the_awareness_arm_carry_no_peer_outcome(self):
        # The shared text (preamble, success definition) is the same for every
        # arm, so it must not reveal what a peer changes: a session learns that
        # only through its arm's peer-information channel.
        for family in tasks.FAMILIES:
            for sessions in tasks.SESSION_COUNTS:
                assigned = tasks.assign(family, sessions, 3)
                for arm in ("worktrees", "langspan"):
                    for index, task in enumerate(assigned, 1):
                        prompt = driver.prompt_for(arm, family, assigned, index)
                        shared = prompt.replace(task["task"], "").replace(task["details"], "")
                        if "trigger" in task:
                            self.assertIsNone(re.search(task["trigger"], shared), (family, arm, task["id"]))
                        if family == "rename_vs_new_caller" and task["id"] != "rename":
                            self.assertNotIn(tasks.NEW_NAME, prompt, (arm, task["id"]))

    def test_plan_only_executes_nothing_and_names_ignored_options(self):
        output = self.root / "never"
        result = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                                 "--family", "contradictory_defaults", "--sessions", "5", "--seed", "1",
                                 "--langspan", "/nonexistent/langspan", "--output", str(output)],
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)
        self.assertEqual(plan["executed_commands"], 0)
        self.assertEqual(len(plan["sessions"]), 5)
        self.assertEqual(plan["ignored_options"], ["--langspan"])
        self.assertFalse(output.exists())
        self.assertEqual(driver.make_plan("langspan", "rename_vs_new_caller", 2, 1, agent="claude",
                                          standin_mode="ignore", langspan_given=True)["ignored_options"],
                         ["--standin-mode"])

    def run_claude_preflight(self, config, env_extra):
        output = self.root / "out"
        env = {k: v for k, v in os.environ.items() if k != driver.CLAUDE_CONFIG_ENV}
        env.update(env_extra)
        result = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                                 "--family", "rename_vs_new_caller", "--sessions", "2", "--seed", "1",
                                 "--agent", "claude", "--run", "--output", str(output)],
                                capture_output=True, text=True, timeout=30, env=env)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        record = json.loads((output / "record.json").read_text())
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["status"], "refused")
        self.assertTrue((output / "plan.json").is_file())
        self.assertFalse((output / "work").exists(), "nothing was prepared")
        self.assertEqual((record["integrated_tests"], record["tasks_accepted"]), ("unknown", None))
        return result, record

    def test_a_claude_configuration_with_langspan_entries_is_refused_named_by_kind_and_recorded(self):
        config = self.root / "claude-config"
        config.mkdir()
        command = "'/opt/bin/langspan' hook claude-code --state-dir '/private/state'"
        (config / "settings.json").write_text(json.dumps({"hooks": {"PreToolUse": [{"matcher": "Write", "hooks": [
            {"type": "command", "command": command}]}]}}))
        (config / "skills" / "langspan").mkdir(parents=True)
        (config / "skills" / "langspan" / "SKILL.md").write_text("skill")
        (config / ".claude.json").write_text(json.dumps({"mcpServers": {"coordination": {
            "command": "/opt/bin/langspan", "args": ["mcp", "--state-dir", "/private/state"]}}}))
        result, record = self.run_claude_preflight(config, {driver.CLAUDE_CONFIG_ENV: str(config)})
        for words in ("hook in settings.json", "skill in skills/langspan", "MCP server in .claude.json"):
            self.assertIn(words, result.stderr)
            self.assertIn(words, record["error"])
        for private in ("/opt/bin", "--state-dir", "/private/state", "hook claude-code"):
            self.assertNotIn(private, result.stderr)
            self.assertNotIn(private, record["error"])
        # Each kind alone is refused; a configuration without them is not.
        (config / ".claude.json").write_text(json.dumps({"mcpServers": {"docs": {"command": "docs-server"}}}))
        (config / "settings.json").write_text("{}")
        self.assertEqual(driver.user_level_langspan(config), ["skill in skills/langspan"])
        shutil.rmtree(config / "skills")
        self.assertEqual(driver.user_level_langspan(config), [])
        (config / ".claude.json").write_text(json.dumps({"projects": {"/x": {"mcpServers": {"langspan": {
            "command": "anything"}}}}}))
        self.assertEqual(driver.user_level_langspan(config), ["MCP server in .claude.json"])

    def test_a_claude_run_without_the_configuration_variable_is_refused_and_recorded(self):
        result, record = self.run_claude_preflight(None, {})
        self.assertIn(driver.CLAUDE_CONFIG_ENV, result.stderr)
        self.assertIn(driver.CLAUDE_CONFIG_ENV, record["error"])

    def test_the_record_keeps_the_requested_model_apart_from_reported_ones(self):
        plan = driver.make_plan("worktrees", "rename_vs_new_caller", 2, 1, agent="claude", model="model-x")
        self.assertEqual(plan["model"], "model-x")
        plan.update(run_id="0" * 32, output=str(self.root), plan_path=str(self.root / "plan.json"))
        record = driver.build_record(plan, None, self.root / "runner", None, None, "t", 0.0, status="refused",
                                     error="refused for the test")
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["versions"]["requested_model"], "model-x")
        self.assertEqual(record["versions"]["models"], [])

    def test_a_langspan_arm_claude_session_uses_its_own_coordinator(self):
        """The session's bare `langspan status` and `langspan brief` must
        reach the coordinator its hooks use, not a default state."""
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        langspan = bin_dir / "langspan"
        langspan.write_text("#!/bin/sh\nexit 0\n")
        langspan.chmod(0o755)
        seen = self.root / "seen.txt"
        claude = self.root / "fake-claude"
        claude.write_text('#!/bin/sh\nprintf "%%s\\n%%s\\n" "$LANGSPAN_STATE_DIR" "$(command -v langspan)" > %s\n%s'
                          % (seen, self.init_line(driver.session_tools("langspan"))))
        claude.chmod(0o755)
        config = self.root / "claude-config"
        config.mkdir()
        state = str(self.root / "lsb-1")
        plan = driver.make_plan("langspan", "rename_vs_new_caller", 2, 1, agent="claude")
        plan.update(langspan=str(langspan), state_dir=state, work=str(self.root / "work"))
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(plan))
        env = dict(os.environ, **{driver.CLAUDE_CONFIG_ENV: str(config), driver.CLAUDE_BIN_ENV: str(claude),
                                  "LANGSPAN_STATE_DIR": str(self.root / "operator-state")})
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "session", str(plan_path), "2"],
                              capture_output=True, text=True, timeout=30, env=env)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(seen.read_text().splitlines(), [state, str(langspan)])
        self.assertEqual(driver.tool_check(done.stdout)["status"], "passed", done.stdout)

    def test_each_arm_gets_its_checkout_kind_and_sessions_cannot_reach_peers_through_git_remotes(self):
        for arm in driver.ARMS:
            work = self.root / arm
            work.mkdir()
            repo, _, checkouts = driver.prepare_fixture(work, 2, arm)
            (checkouts[1] / "shelf" / "report.py").write_text("# peer work\n")
            driver.git("commit", "-qam", "peer-secret-commit", cwd=checkouts[1])
            common = [driver.git("rev-parse", "--path-format=absolute", "--git-common-dir", cwd=path).stdout
                      for path in checkouts]
            self.assertEqual(driver.git("remote", cwd=checkouts[0]).stdout.split(), [], arm)
            self.assertEqual([driver.git("branch", "--show-current", cwd=path).stdout.strip() for path in checkouts],
                             ["bench/s1", "bench/s2"])
            # The integration repository is never one of the sessions'.
            self.assertNotIn(str(repo), common[0])
            seen = driver.git("log", "--all", "--oneline", cwd=checkouts[0]).stdout
            if arm == "langspan":
                # Worktrees of one repository: what LangSpan supports for
                # sharing interface notices. Git can show a peer's branch,
                # which is why every arm's permission rules deny it.
                self.assertEqual(common[0], common[1])
                self.assertIn("peer-secret-commit", seen)
            else:
                self.assertNotEqual(common[0], common[1])
                self.assertNotIn("peer-secret-commit", seen, arm)
            # `.claude/` stays out of every branch.
            self.assertEqual(driver.git("check-ignore", ".claude/settings.local.json", cwd=checkouts[0],
                                        check=False).returncode, 0, arm)

    def test_duplicate_work_is_a_change_to_a_unit_another_task_owns(self):
        work = self.root / "work"
        work.mkdir()
        repo, base, clones = driver.prepare_fixture(work, 2, "worktrees")
        plan = {"sessions": 2, "work": str(work), "tasks": [dict(tasks.RENAME, role="rename"),
                                                            dict(tasks.feature_task(*tasks.FEATURES[0]), role="new_caller")]}
        # Session 2 adds an alias, which is not the rename: no duplicate.
        config = clones[1] / "shelf" / "config.py"
        config.write_text(config.read_text() + "\nload_config = parse_config\n")
        driver.git("commit", "-qam", "alias", cwd=clones[1])
        driver.collect_branches(plan, repo)
        self.assertEqual(driver.duplicates(plan, repo, base), [])
        # Session 2 now renames the function itself: that is session 1's task.
        config.write_text(config.read_text().replace("def parse_config", "def load_config"))
        driver.git("commit", "-qam", "rename too", cwd=clones[1])
        driver.collect_branches(plan, repo)
        self.assertEqual(len(driver.duplicates(plan, repo, base)), 2)  # parse_config gone, load_config added

    def oracle(self, plan, checkout):
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(plan))
        out = self.root / "verdict.json"
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "oracle.py"), str(plan_path),
                               str(checkout), str(out)], capture_output=True, text=True, timeout=120)
        return done.returncode, json.loads(out.read_text())

    def test_the_contradiction_oracle_accepts_exactly_the_reconciliation_the_tasks_define(self):
        key, old, a, b = tasks.CONTRADICTIONS[0]
        first, second = (dict(tasks.contradiction_task(key, old, side), role="contradiction") for side in (a, b))
        plan = {"family": "contradictory_defaults", "tasks": [first, second], "config_users": [],
                "integration": [{"session": 1, "merged": True}, {"session": 2, "merged": True}]}

        def tree(default, export_value, ui_value, note=None):
            checkout = self.root / "checkout"
            shutil.rmtree(checkout, ignore_errors=True)
            shutil.copytree(HERE / "fixture", checkout)
            settings = checkout / "shelf" / "settings.py"
            settings.write_text(settings.read_text().replace('"page_size": 20,', '"page_size": %d,' % default))
            for module, value in (("export", export_value), ("ui", ui_value)):
                if value is not None:
                    source = checkout / "shelf" / ("%s.py" % module)
                    source.write_text(source.read_text().replace('DEFAULTS["page_size"]', str(value)))
            if note:
                (checkout / "notes").mkdir()
                (checkout / "notes" / "s1.md").write_text(note)
            return self.oracle(plan, checkout)

        # One side changed the shared default: the other side silently lost.
        code, verdict = tree(100, None, None)
        self.assertEqual((code, verdict["integrated_tests"]), (1, "failed"))
        self.assertEqual([t["completed"] for t in verdict["tasks"]], [True, False])
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, False])
        self.assertEqual(len(verdict["semantic_conflicts"]), 1)
        # A note naming both values is kept for review; it is not the reconciliation.
        code, verdict = tree(100, None, None, note="page_size: export wants 100, listing wants 10.\n")
        self.assertEqual((code, verdict["integrated_tests"]), (1, "failed"))
        self.assertNotIn("contradictions_reported", verdict)
        # Both functions get their values but the shared default moved: not the reconciliation.
        code, verdict = tree(100, None, 10)
        self.assertEqual([t["completed"] for t in verdict["tasks"]], [True, True])
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, False])
        self.assertEqual(code, 1)
        # Each side sets its own value and the default stays: accepted.
        code, verdict = tree(20, 100, 10)
        self.assertEqual((code, verdict["integrated_tests"]), (0, "passed"), verdict)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, True])

        # A task whose key no other task wants: only changing the default is
        # accepted. Keeping it and setting the value locally is the hedge a
        # session makes without knowing whether a partner exists.
        plan["tasks"], plan["integration"] = [first], [{"session": 1, "merged": True}]
        code, verdict = tree(20, 100, None)
        self.assertEqual([t["completed"] for t in verdict["tasks"]], [True])
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False])
        self.assertIn("no other task", verdict["tasks"][0]["detail"])
        self.assertEqual((code, verdict["semantic_conflicts"]), (1, []))
        code, verdict = tree(100, None, None)
        self.assertEqual((code, verdict["integrated_tests"]), (0, "passed"), verdict)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True])

    def test_the_rename_oracle_needs_the_new_caller_on_the_new_name(self):
        feature = dict(tasks.feature_task(*tasks.FEATURES[0]), role="new_caller")
        plan = {"family": "rename_vs_new_caller", "tasks": [dict(tasks.RENAME, role="rename"), feature],
                "config_users": list(tasks.CONFIG_USERS),
                "integration": [{"session": 1, "merged": True}, {"session": 2, "merged": True}]}

        def tree(caller_uses, alias=False):
            checkout = self.root / "checkout"
            shutil.rmtree(checkout, ignore_errors=True)
            shutil.copytree(HERE / "fixture", checkout)
            for name in tasks.CONFIG_USERS:
                path = checkout / name
                path.write_text(path.read_text().replace("parse_config", "load_config"))
            if alias:
                config = checkout / "shelf" / "config.py"
                config.write_text(config.read_text() + "\n\nparse_config = load_config\n")
            op = feature["standin"]["follow" if caller_uses == "load_config" else "ignore"][0]
            (checkout / op["path"]).write_text(op["content"])
            return self.oracle(plan, checkout)

        code, verdict = tree("parse_config")
        self.assertEqual((code, verdict["integrated_tests"]), (1, "failed"))
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, False])
        code, verdict = tree("parse_config", alias=True)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, False], "an alias is not the rename")
        code, verdict = tree("load_config")
        self.assertEqual((code, verdict["integrated_tests"]), (0, "passed"), verdict)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, True])

    def test_no_task_text_of_the_discovered_change_family_names_the_change(self):
        """The changer's need is found in the code, so sharing task text
        cannot tell a dependent what changes: no prompt, in any arm and
        the awareness arm's peer list included, holds the changed
        signature or anything a dependent's trigger matches."""
        for n in tasks.SESSION_COUNTS:
            for seed in range(5):
                assigned = tasks.assign("discovered_change", n, seed)
                self.assertEqual(assigned, tasks.assign("discovered_change", n, seed))
                self.assertEqual([t["role"] for t in assigned].count("changer"), 1)
                self.assertEqual([t["role"] for t in assigned].count("dependent"), n - 1)
                self.assertEqual(len({t["id"] for t in assigned}), n)
                self.assertEqual(tasks.leader(assigned), [t["role"] for t in assigned].index("changer") + 1)
                for arm in driver.ARMS:
                    for index in range(1, n + 1):
                        prompt = driver.prompt_for(arm, "discovered_change", assigned, index)
                        self.assertNotIn("today)", prompt.replace("expired(items, today)", ""), (arm, index))
                        for task in assigned:
                            if "trigger" in task:
                                self.assertIsNone(re.search(task["trigger"], prompt), (arm, index, task["id"]))
        self.assertIsNone(tasks.leader(tasks.assign("contradictory_defaults", 5, 1)))
        self.assertEqual(tasks.assign("rename_vs_new_caller", 5, 1)[tasks.leader(
            tasks.assign("rename_vs_new_caller", 5, 1)) - 1]["role"], "rename")

    def apply(self, checkout, ops):
        """A stand-in edit list applied to CHECKOUT, without hooks."""
        for op in ops:
            for name in op.get("paths", [op.get("path")]):
                path = checkout / name
                if op["op"] == "write":
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(op["content"])
                else:
                    path.write_text(path.read_text().replace(op["old"], op["new"]))

    def test_the_discovered_change_oracle_needs_the_change_and_every_dependent_on_it(self):
        changer = dict(tasks.CHANGER, role="changer")
        dependent = dict(tasks.dependent_task(*tasks.DATED_FEATURES[1]), role="dependent")
        plan = {"family": "discovered_change", "tasks": [changer, dependent], "config_users": [],
                "integration": [{"session": 1, "merged": True}, {"session": 2, "merged": True}]}
        path = dependent["owns"][0]

        def tree(changer_mode, dependent_mode=None, content=None):
            checkout = self.root / "checkout"
            shutil.rmtree(checkout, ignore_errors=True)
            shutil.copytree(HERE / "fixture", checkout)
            self.apply(checkout, changer["standin"][changer_mode] if changer_mode else [])
            if dependent_mode:
                self.apply(checkout, dependent["standin"][dependent_mode])
            if content is not None:
                (checkout / path).write_text(content)
            return self.oracle(plan, checkout)

        # The change, and a dependent on the changed form: accepted.
        code, verdict = tree("follow", "follow")
        self.assertEqual((code, verdict["integrated_tests"]), (0, "passed"), verdict)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, True])
        # A dependent on the old form breaks the merged tree, which Git merged cleanly.
        code, verdict = tree("follow", "ignore")
        self.assertEqual((code, verdict["integrated_tests"]), (1, "failed"))
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, False])
        self.assertEqual(len(verdict["semantic_conflicts"]), 1, verdict)
        self.assertIn(path, verdict["semantic_conflicts"][0])
        # A dependent that counts the days itself works whatever the changer
        # does: a hedge, not the task.
        code, verdict = tree("follow", "hedge")
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, False])
        self.assertIn("days_left", verdict["tasks"][1]["detail"])
        # A dependent that tries the new form and falls back to the old one hedges too.
        guarded = ("import datetime\n\nfrom shelf.dates import days_left\n\n\ndef expiring_soon(items, within):\n"
                   "    today = datetime.date.today()\n\n    def left(day):\n        try:\n"
                   "            return days_left(day, today)\n        except TypeError:\n"
                   "            return days_left(day)\n"
                   "    return [name for name, day in items if left(day) is not None and left(day) <= within]\n")
        code, verdict = tree("follow", content=guarded)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, False])
        # A dependent that imports the helper under another name still calls it.
        aliased = ("import datetime\n\nfrom shelf.dates import days_left as remaining_days\n\n\n"
                   "def expiring_soon(items, within):\n    today = datetime.date.today()\n"
                   "    return [name for name, day in items\n"
                   "            if remaining_days(day, today) is not None and remaining_days(day, today) <= within]\n")
        code, verdict = tree("follow", content=aliased)
        self.assertEqual((code, verdict["integrated_tests"]), (0, "passed"), verdict)
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, True])
        # Under an alias, a fallback to the old form is still a hedge.
        code, verdict = tree("follow", content=guarded.replace(
            "import days_left", "import days_left as remaining_days").replace(
            "return days_left(", "return remaining_days("))
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, False])
        self.assertIn("old form", verdict["tasks"][1]["detail"])
        # A changer that keeps old calls working with a default hedges; so
        # its dependent's old call fits, and is not judged: the change was
        # not made.
        code, verdict = tree("hedge", "ignore")
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, None])
        self.assertIn("default", verdict["tasks"][0]["detail"])
        self.assertEqual(verdict["semantic_conflicts"], [])
        # A changer that wrote nothing fails, and its dependent is not judged.
        code, verdict = tree(None, "ignore")
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, None])
        # A changer that leaves the package's caller on the old form breaks it.
        tree("follow", "follow")
        orders = self.root / "checkout" / "shelf" / "orders.py"
        orders.write_text((HERE / "fixture" / "shelf" / "orders.py").read_text())
        code, verdict = self.oracle(plan, self.root / "checkout")
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [False, True])
        self.assertIn("shelf/orders.py", verdict["tasks"][0]["detail"])
        self.assertEqual(verdict["fixture_tests"], "failed")

    def test_a_changer_that_works_around_the_change_fails_and_the_trial_is_change_not_made(self):
        """Issue #218: in the third pilot a changer subtracted two
        one-argument calls instead of changing `days_left`. That workaround
        must fail the changer's own task, and the trial must say the change
        was not made instead of failing every dependent."""
        changer = dict(tasks.CHANGER, role="changer")
        dependent = dict(tasks.dependent_task(*tasks.DATED_FEATURES[1]), role="dependent")
        plan = {"family": "discovered_change", "tasks": [changer, dependent], "config_users": [],
                "integration": [{"session": 1, "merged": True}, {"session": 2, "merged": True}]}
        checkout = self.root / "checkout"

        def tree(changer_mode, dependent_mode):
            shutil.rmtree(checkout, ignore_errors=True)
            shutil.copytree(HERE / "fixture", checkout)
            self.apply(checkout, changer["standin"][changer_mode])
            self.apply(checkout, dependent["standin"][dependent_mode])

        # The value `expired` needs cannot be derived from one-argument
        # calls: the workaround returns the wrong names, or raises, on the
        # oracle's own checks.
        tree("workaround", "ignore")
        probe = ("import datetime, json, sys\nsys.path.insert(0, sys.argv[1])\n"
                 "from shelf.expiry import expired\n"
                 "day = datetime.date(2026, 3, 4)\n"
                 "for items, want in json.loads(sys.argv[2]):\n"
                 "    dated = [(name, day + datetime.timedelta(days=offset)) for name, offset in items]\n"
                 "    try:\n        got = expired(dated, day)\n"
                 "    except Exception as error:\n        got = type(error).__name__\n"
                 "    print(json.dumps(got == want))\n")
        checks = [[arguments[0], want] for arguments, want in changer["checks"]]
        done = subprocess.run([sys.executable, "-I", "-B", "-c", probe, str(checkout), json.dumps(checks)],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertNotIn("true", done.stdout.split(), "a one-argument workaround still finds the expired items")
        code, verdict = self.oracle(plan, checkout)
        self.assertEqual((code, verdict["integrated_tests"]), (1, "failed"))
        self.assertEqual(verdict["interface_change"], "change_not_made")
        self.assertEqual(verdict["tasks"][0]["accepted"], False)
        self.assertTrue(verdict["tasks"][0]["detail"].startswith("change_not_made"), verdict["tasks"][0])
        # The dependent is not judged: nothing changed for it to follow.
        self.assertEqual((verdict["tasks"][1]["completed"], verdict["tasks"][1]["accepted"]), (None, None))
        self.assertTrue(verdict["tasks"][1]["detail"].startswith("change_not_made"), verdict["tasks"][1])
        self.assertEqual(verdict["semantic_conflicts"], [])
        # A default for the new parameter is not the change either.
        tree("hedge", "ignore")
        self.assertEqual(self.oracle(plan, checkout)[1]["interface_change"], "change_not_made")
        # The change, made on the changer's branch, is recorded as made, and
        # a dependent on the old form fails as before.
        tree("follow", "ignore")
        code, verdict = self.oracle(plan, checkout)
        self.assertEqual(verdict["interface_change"], "made")
        self.assertEqual([t["accepted"] for t in verdict["tasks"]], [True, False])
        # The changer's own branch decides, not the merged tree: a change
        # that only another session's branch made is not the changer's.
        unchanged = (HERE / "fixture" / "shelf" / "dates.py").read_text()
        _, verdict = self.oracle(dict(plan, interface_source=unchanged), checkout)
        self.assertEqual(verdict["interface_change"], "change_not_made")
        _, verdict = self.oracle(dict(plan, interface_source=(checkout / tasks.DATES).read_text()), checkout)
        self.assertEqual(verdict["interface_change"], "made")
        # A branch that deleted the helper's file, broke it, or kept a
        # wrapper that takes anything has not made the change.
        for source in (None, "def days_left(day, today:\n", "def days_left(*args):\n    pass\n"):
            _, verdict = self.oracle(dict(plan, interface_source=source), checkout)
            self.assertEqual(verdict["interface_change"], "change_not_made", source)
        # A required keyword-only parameter is the change; the last definition counts.
        source = "def days_left(day):\n    pass\n\n\ndef days_left(day, *, today):\n    pass\n"
        self.assertEqual(self.oracle(dict(plan, interface_source=source), checkout)[1]["interface_change"], "made")
        # Other families have no interface change to make.
        rename = {"family": "rename_vs_new_caller", "tasks": [dict(tasks.RENAME, role="rename")],
                  "config_users": [], "integration": [{"session": 1, "merged": True}]}
        tree("follow", "follow")
        self.assertIsNone(self.oracle(rename, checkout)[1]["interface_change"])

    def test_every_claude_session_has_auto_memory_off_and_a_shared_directory_is_refused(self):
        """Claude Code keys auto memory by repository, so the worktrees of
        the LangSpan arm would share one directory: a channel between
        sessions the other arms lack. Every session turns it off with the
        CLI's own setting, and the preflight refuses a trial whose sessions
        would share one."""
        for arm in driver.ARMS:
            argv = driver.claude_argv("claude", "x", arm)
            self.assertEqual(json.loads(self.option(argv, "--settings")[0]), {"autoMemoryEnabled": False})
            self.assertFalse(driver.auto_memory_on(argv, {}))
            self.assertEqual(driver.shared_memory(arm, self.root / "work", [argv] * 3, [{}] * 3), [])
        on = [a for a in driver.claude_argv("claude", "x", "langspan") if "autoMemoryEnabled" not in a]
        on.remove("--settings")
        self.assertTrue(driver.auto_memory_on(on, {}))
        self.assertFalse(driver.auto_memory_on(on, {"CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1"}))
        # Worktrees of one repository share the directory; clones do not.
        self.assertEqual(driver.shared_memory("langspan", self.root / "work", [on] * 3, [{}] * 3), [1, 2, 3])
        self.assertEqual(driver.shared_memory("worktrees", self.root / "work", [on] * 3, [{}] * 3), [])
        # The session's environment holds the CLI's own switch as well.
        plan = driver.make_plan("langspan", "rename_vs_new_caller", 2, 1, agent="claude")
        self.assertEqual(driver.session_env(plan, {"PATH": "/bin"}, "/config")["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1")
        self.assertEqual(plan["auto_memory"], "off")
        self.assertEqual(driver.make_plan("langspan", "rename_vs_new_caller", 2, 1)["auto_memory"], "not_applicable")
        # The preflight's check refuses sessions that would keep auto memory
        # in one directory, and passes the commands the driver builds.
        plan.update(work=str(self.root / "work"))
        with self.assertRaisesRegex(driver.Refused, "sessions 1, 2 would share one Claude Code auto-memory"):
            driver.refuse_shared_memory(plan, [on] * 2, [{}] * 2)
        driver.refuse_shared_memory(plan, [driver.claude_argv("claude", "x", "langspan")] * 2, [{}] * 2)
        config = self.root / "claude-config"
        config.mkdir()
        args = type("Args", (), {"agent": "claude", "langspan": None})()
        saved = os.environ.get(driver.CLAUDE_CONFIG_ENV)
        os.environ[driver.CLAUDE_CONFIG_ENV] = str(config)
        try:
            # Past the memory check, the LangSpan arm's next one refuses.
            with self.assertRaisesRegex(driver.Refused, "--langspan"):
                driver.preflight(args, plan)
        finally:
            if saved is None:
                os.environ.pop(driver.CLAUDE_CONFIG_ENV, None)
            else:
                os.environ[driver.CLAUDE_CONFIG_ENV] = saved

    def test_stagger_starts_the_changing_session_first_and_needs_one(self):
        plan = driver.make_plan("langspan", "discovered_change", 5, 1, stagger=20)
        self.assertEqual(plan["stagger_s"], 20)
        lead = tasks.leader(plan["tasks"])
        self.assertEqual([driver.start_delay(plan, i) for i in range(1, 6)],
                         [0 if i == lead else 20 for i in range(1, 6)])
        self.assertEqual(driver.make_plan("langspan", "discovered_change", 5, 1)["stagger_s"], 0)
        self.assertEqual(driver.start_delay(driver.make_plan("langspan", "discovered_change", 5, 1), 2), 0)
        for bad in (-1, driver.MAX_STAGGER + 1, True, "5"):
            with self.assertRaises(ValueError, msg=bad):
                driver.make_plan("langspan", "discovered_change", 5, 1, stagger=bad)
        with self.assertRaises(ValueError):
            driver.make_plan("langspan", "contradictory_defaults", 5, 1, stagger=10)
        # A delayed session's time limit starts when it does.
        plan.update(plan_path="/p/plan.json")
        agents = driver.manifest_for(plan, [Path("/w/s%d" % i) for i in range(1, 6)], Path("/w/repo"))[
            "items"][0]["arms"]["langspan"]["agents"]
        self.assertEqual([a["timeout_seconds"] for a in agents],
                         [plan["time_limit_s"] + driver.start_delay(plan, i) for i in range(1, 6)])

    def test_permission_denials_are_split_by_kind(self):
        """`peer_checkout`: aimed at another session's checkout, branch or
        refs; `outside_clone`: Claude Code refused a path outside the
        session's own checkout; `not_allowed`: anything else. A lone `/` in
        a command's text (a heredoc of notes) is none of a peer's."""
        work = self.root / "work"
        root = work / "s1"
        root.mkdir(parents=True)
        peer = str(work / "s2" / "shelf" / "config.py")
        outside = {"decision_reason_type": "workingDir", "decision_reason": "Path is outside allowed working directories"}
        compound = {"decision_reason_type": "other", "decision_reason": "This command requires approval"}
        cases = [
            ({"tool_name": "Read", "tool_input": {"file_path": peer}}, outside, "peer_checkout"),
            ({"tool_name": "Grep", "tool_input": {"pattern": "x", "path": "../s2"}}, outside, "peer_checkout"),
            ({"tool_name": "Bash", "tool_input": {"command": "cat ../s2/shelf/ui.py"}}, compound, "peer_checkout"),
            ({"tool_name": "Bash", "tool_input": {"command": "git -C %s log" % (work / "s2")}}, None, "peer_checkout"),
            ({"tool_name": "Bash", "tool_input": {"command": "ls %s" % (work / "repo")}}, None, "peer_checkout"),
            ({"tool_name": "Bash", "tool_input": {"command": "git log --oneline bench/s2"}}, compound, "peer_checkout"),
            ({"tool_name": "Bash", "tool_input": {"command": "git log --all --oneline"}}, None, "peer_checkout"),
            ({"tool_name": "Bash", "tool_input": {"command": "git diff HEAD..refs/heads/bench/s3"}}, None,
             "peer_checkout"),
            ({"tool_name": "Bash", "tool_input": {"command": "git stash list"}}, None, "peer_checkout"),
            ({"tool_name": "Write", "tool_input": {"file_path": "/tmp/s1check/run.py"}}, outside, "outside_clone"),
            ({"tool_name": "Read", "tool_input": {"file_path": "/etc/hosts"}}, None, "outside_clone"),
            ({"tool_name": "Bash", "tool_input": {"command": "mkdir -p /tmp/s1check && cp -r shelf /tmp/s1check"}},
             outside, "outside_clone"),
            ({"tool_name": "Bash", "tool_input": {"command": "cat > notes/s1.md <<'EOF'\n`timeout_s` / `retries`\nEOF"}},
             compound, "not_allowed"),
            ({"tool_name": "Bash", "tool_input": {"command": "mkdir -p /tmp/x"}}, None, "not_allowed"),
            ({"tool_name": "Bash", "tool_input": {"command": "git log --oneline bench/s1"}}, compound, "not_allowed"),
            ({"tool_name": "Bash", "tool_input": {"command": "git add --all && git commit -m 'stash refs/x'"}},
             compound, "not_allowed"),
            ({"tool_name": "Bash", "tool_input": {"command": "sed -i s/a/b/ shelf/ui.py"}}, compound, "not_allowed"),
            ({"tool_name": "Read", "tool_input": {"file_path": str(root / "shelf" / "ui.py")}}, None, "not_allowed"),
            ({"tool_name": "Write", "tool_input": {}}, None, "not_allowed"),
            ("not an object", None, "not_allowed"),
        ]
        for denial, event, kind in cases:
            self.assertEqual(driver.classify_denial(denial, root, work, 1, event), kind, (denial, event))
        denials = [denial for denial, _, _ in cases]
        events = {}
        for position, (denial, event, _) in enumerate(cases):
            if isinstance(denial, dict) and event:
                denial["tool_use_id"] = "call-%d" % position
                events["call-%d" % position] = event
        self.assertEqual(driver.denial_counts(denials, root, work, 1, events),
                         {"total": 20, "peer_checkout": 9, "outside_clone": 3, "not_allowed": 8})
        self.assertIsNone(driver.denial_counts(None, root, work, 1, {}))

    def transcript(self, lines):
        return "\n".join(json.dumps(line) for line in lines) + "\n"

    def test_a_session_transcript_gives_its_result_cost_duration_and_turns(self):
        lines = [
            {"type": "system", "subtype": "init", "model": "model-x"},
            {"type": "assistant", "message": {"content": [{"type": "text", "text": "Done?"}]}},
            {"type": "result", "subtype": "success", "is_error": False, "result": "Done.", "num_turns": 7,
             "duration_ms": 41000, "total_cost_usd": 0.25, "permission_denials": [],
             "usage": {"input_tokens": 3, "output_tokens": 4}},
        ]
        text = self.transcript(lines)
        totals = driver.session_totals(driver.session_results(text))
        self.assertEqual(totals["final"]["result"], "Done.")
        self.assertEqual((totals["cost_usd"], totals["duration_ms"], totals["turns"], totals["results"]),
                         (0.25, 41000, 7, 1))
        self.assertEqual(totals["tokens"], {"input": 3, "output": 4, "cache_creation": 0, "cache_read": 0})
        bad = driver.session_totals([{"type": "result", "num_turns": -1, "total_cost_usd": "x"}])
        self.assertEqual((bad["cost_usd"], bad["duration_ms"], bad["turns"], bad["tokens"]), (None, None, None, None))
        # A transcript cut off before its result has none.
        self.assertEqual(driver.session_results(self.transcript(lines[:2])), [])
        self.assertIsNone(driver.session_totals([])["final"])

    def test_the_record_sums_every_result_line_of_a_session(self):
        """A session woken after its turn ended has a second result line. The
        record's totals cover both, as the transcript does: turns, duration,
        tokens and denials add up; the cost is already cumulative."""
        lines = [
            {"type": "system", "subtype": "init", "tools": list(driver.session_tools("worktrees")), "mcp_servers": []},
            {"type": "system", "subtype": "permission_denied", "tool_name": "Write", "tool_use_id": "w1",
             "decision_reason_type": "workingDir", "decision_reason": "Path is outside allowed working directories"},
            {"type": "result", "subtype": "success", "is_error": False, "result": "First.", "num_turns": 14,
             "duration_ms": 45287, "total_cost_usd": 0.3366,
             "usage": {"input_tokens": 18, "cache_creation_input_tokens": 26323, "cache_read_input_tokens": 294343,
                       "output_tokens": 3353},
             "modelUsage": {"model-x": {}},
             "permission_denials": [{"tool_name": "Write", "tool_use_id": "w1",
                                     "tool_input": {"file_path": "/tmp/s1check/run.py"}},
                                    {"tool_name": "Bash", "tool_use_id": "b1",
                                     "tool_input": {"command": "sed -i s/a/b/ x"}}]},
            {"type": "system", "subtype": "init", "tools": list(driver.session_tools("worktrees")), "mcp_servers": []},
            {"type": "result", "subtype": "success", "is_error": False, "result": "Second?", "num_turns": 2,
             "duration_ms": 8641, "total_cost_usd": 0.3704,
             "usage": {"input_tokens": 4, "cache_creation_input_tokens": 1217, "cache_read_input_tokens": 80365,
                       "output_tokens": 399},
             "modelUsage": {"model-y": {}},
             "permission_denials": [{"tool_name": "Bash", "tool_use_id": "b2",
                                     "tool_input": {"command": "git log --all"}}]},
            {"type": driver.TOOL_CHECK_TYPE, "status": "passed", "unexpected": []},
        ]
        runner_dir = self.root / "runner"
        (runner_dir / "r").mkdir(parents=True)
        (runner_dir / "r" / "agent-s1.stdout").write_text(self.transcript(lines))
        (runner_dir / "r" / "agent-s2.stdout").write_text(self.transcript([lines[0], lines[4]]))
        plan = driver.make_plan("worktrees", "rename_vs_new_caller", 2, 1, agent="claude")
        work = self.root / "work"
        plan.update(run_id="0" * 32, output=str(self.root), plan_path=str(self.root / "plan.json"), work=str(work))
        final = {"run_id": "0" * 32, "status": "succeeded", "agent_phase_elapsed_ns": 1,
                 "commands": [{"phase": "agent", "id": "s%d" % i, "status": "succeeded",
                               "stdout_path": "r/agent-s%d.stdout" % i} for i in (1, 2)]}
        record = driver.build_record(plan, final, runner_dir, None, None, "t", 0.0)
        driver.validate(record, driver.load_schema())
        first = record["tasks"][0]
        self.assertEqual((first["results"], first["turns"], first["duration_ms"], first["cost_usd"]),
                         (2, 16, 53928, 0.3704))
        self.assertEqual(first["tokens"], {"input": 22, "output": 3752, "cache_creation": 27540, "cache_read": 374708})
        self.assertEqual(first["permission_denials"],
                         {"total": 3, "peer_checkout": 1, "outside_clone": 1, "not_allowed": 1})
        # The final state is the last result's: it ends in a question.
        self.assertEqual((first["stopped"], first["asked_question"]), (False, True))
        self.assertEqual(record["versions"]["models"], ["model-x", "model-y"])
        self.assertEqual(record["tasks"][1]["results"], 1)
        self.assertEqual(record["turns"], 18)
        self.assertEqual(record["permission_denials"],
                         {"total": 4, "peer_checkout": 2, "outside_clone": 1, "not_allowed": 1})
        self.assertEqual(record["tokens"]["total"]["output"], 3752 + 399)
        # Session 2's transcript has no verdict of the guard.
        self.assertEqual(record["tool_check"], {"status": "unknown", "failed_sessions": []})

    def test_coordinator_scan_failures_and_recoveries_reach_the_record(self):
        events = [{"seq": 3, "event": {"type": "scan_failed", "failure": {
                      "workspace_id": "bench-s2", "detail": "Git repository metadata changed during observation",
                      "at_ms": 1, "generation": 1}}},
                  {"seq": 4, "event": {"type": "scan_recovered", "workspace_id": "bench-s2"}},
                  {"seq": 5, "event": {"type": "lease_granted"}}]
        scans = driver.scan_summary(events, "langspan")
        self.assertEqual((scans["status"], scans["failed"], scans["recovered"]), ("reported", 1, 1))
        self.assertEqual(scans["items"], [
            {"session": 2, "seq": 3, "type": "scan_failed",
             "detail": "Git repository metadata changed during observation"},
            {"session": 2, "seq": 4, "type": "scan_recovered", "detail": None}])
        self.assertEqual(driver.scan_summary(None, "worktrees")["status"], "not_applicable")
        self.assertEqual(driver.scan_summary(None, "langspan")["status"], "unknown")

    def test_the_hook_log_passes_a_refusal_through_even_when_it_cannot_log(self):
        command = "cat >/dev/null; printf '{\"decision\":\"deny\"}'; echo why >&2; exit 2"
        payload = json.dumps({"hook_event_name": "PreToolUse", "tool_use_id": "call-1", "session_id": "s"})
        for log in (self.root / "hooks.jsonl", self.root / "missing" / "hooks.jsonl"):
            done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "hook-log", str(log), command],
                                  input=payload, capture_output=True, text=True, timeout=30)
            self.assertEqual((done.returncode, done.stdout), (2, '{"decision":"deny"}'), log)
            self.assertIn("why", done.stderr)
        entry = json.loads((self.root / "hooks.jsonl").read_text())
        self.assertEqual((entry["event"], entry["tool_use_id"], entry["exit_code"]), ("PreToolUse", "call-1", 2))
        self.assertEqual(entry["stdout"], '{"decision":"deny"}')
        self.assertEqual(os.stat(self.root / "hooks.jsonl").st_mode & 0o777, 0o600)

    def test_notes_and_merge_logs_survive_a_failed_merge(self):
        work = self.root / "work"
        work.mkdir()
        run_dir = self.root / "run"
        run_dir.mkdir()
        repo, base, clones = driver.prepare_fixture(work, 2, "worktrees")
        key, old, a, b = tasks.CONTRADICTIONS[0]
        assigned = [dict(tasks.contradiction_task(key, old, side), role="contradiction") for side in (a, b)]
        for index, (clone, value) in enumerate(zip(clones, (100, 10)), 1):
            settings = clone / "shelf" / "settings.py"
            settings.write_text(settings.read_text().replace('"page_size": 20,', '"page_size": %d,' % value))
            (clone / "notes").mkdir()
            (clone / "notes" / ("s%d.md" % index)).write_text("session %d wanted page_size %d\n" % (index, value))
        plan = {"family": "contradictory_defaults", "sessions": 2, "tasks": assigned, "config_users": [],
                "work": str(work), "repo": str(repo), "base": base}
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(plan))
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "integrate", str(plan_path)],
                              capture_output=True, text=True, timeout=300,
                              env=dict(os.environ, LANGSPAN_BENCH_RUN_DIR=str(run_dir)))
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        integrated = json.loads((self.root / "integration.json").read_text())
        self.assertEqual([e["merged"] for e in integrated["integration"]], [True, False])
        # Session 2's branch never merged, and its note is still kept.
        self.assertEqual((run_dir / "notes" / "s2" / "s2.md").read_text(), "session 2 wanted page_size 10\n")
        self.assertEqual((run_dir / "notes" / "s1" / "s1.md").read_text(), "session 1 wanted page_size 100\n")
        log = (run_dir / "merge-s2.log").read_text()
        self.assertIn("CONFLICT", log)
        self.assertIn("<<<<<<<", log, "the conflicting hunks before the merge was aborted")
        self.assertIn("Merge made", (run_dir / "merge-s1.log").read_text())
        self.assertEqual(integrated["notes"], ["notes/s1/s1.md", "notes/s2/s2.md"])

    def test_the_repair_tree_keeps_every_branch_and_a_refused_merge_with_its_conflict_markers(self):
        """Issue #221: the repair session works in the combined tree, so a
        merge Git refused is in it too, as Git left it."""
        work = self.root / "work"
        work.mkdir()
        repo, base, clones = driver.prepare_fixture(work, 3, "worktrees")
        for index, (clone, value) in enumerate(zip(clones, (100, 10, None)), 1):
            if value is not None:
                settings = clone / "shelf" / "settings.py"
                settings.write_text(settings.read_text().replace('"page_size": 20,', '"page_size": %d,' % value))
            else:
                (clone / "shelf" / "text.py").write_text("X = 1\n")
            driver.git("add", "-A", cwd=clone)
            driver.git("commit", "-q", "-m", "work", cwd=clone)
        driver.collect_branches({"work": str(work), "sessions": 3}, repo)
        checkout, conflicted = driver.prepare_repair(repo, base, 3, work)
        self.assertEqual((checkout, conflicted), (work / "repair", [2]))
        self.assertEqual(driver.git("rev-parse", "--abbrev-ref", "HEAD", cwd=checkout).stdout.strip(), "bench/repair")
        text = (checkout / "shelf" / "settings.py").read_text()
        self.assertIn("<<<<<<<", text)
        self.assertIn('"page_size": 100,', text)
        self.assertIn('"page_size": 10,', text)
        self.assertEqual((checkout / "shelf" / "text.py").read_text(), "X = 1\n", "the merge after the refused one")
        self.assertEqual(driver.git("status", "--porcelain", cwd=checkout).stdout, "", "every merge is committed")
        for index in (1, 2, 3):
            self.assertEqual(driver.git("merge-base", "--is-ancestor", driver.branch(index), "HEAD", cwd=checkout,
                                        check=False).returncode, 0)

    def test_the_repair_session_runs_under_the_same_limits_in_the_combined_tree_only(self):
        work = Path("/runs/k1/work")
        for arm in driver.ARMS:
            plan = {"arm": arm, "agent": "claude", "model": None, "sessions": 3, "work": str(work)}
            argv = driver.repair_argv("claude", "repair it", plan)
            # The worktrees arm's configuration in every arm: the same repair job.
            self.assertEqual(argv[3:], driver.claude_argv("claude", "repair it", "worktrees", None,
                                                          driver.repair_denied_paths(work, 3))[3:])
            self.assertEqual(self.option(argv, "--tools"), [",".join(driver.SESSION_TOOLS)])
            self.assertIn("--strict-mcp-config", argv)
            self.assertEqual(self.option(argv, "--settings"), [driver.MEMORY_SETTINGS])
            self.assertFalse(set(argv) & set(driver.FORBIDDEN_CLAUDE_FLAGS))
            denied = self.option(argv, "--disallowedTools")
            for rule in ("ListAgents", "SendMessage", "Bash(git log *bench/*)", "Bash(git worktree*)",
                         "Read(//runs/k1/work/s1/**)", "Edit(//runs/k1/work/s3/**)", "Read(//runs/k1/work/repo/**)",
                         "Read(//runs/k1/work/integration/**)", "Read(//runs/k1/work/shared/**)"):
                self.assertIn(rule, denied, arm)
            self.assertFalse([rule for rule in denied if "/work/repair/" in rule], "the repair keeps its own tree")
            env = driver.session_env(dict(plan, arm=driver.REPAIR_ARM), {"PATH": "/bin"}, "/config")
            self.assertEqual(env[driver.MEMORY_ENV], "1")
            self.assertNotIn("LANGSPAN_STATE_DIR", env)
            self.assertFalse(driver.auto_memory_on(argv, env))

    def test_a_claude_repair_session_gets_its_prompt_and_the_worktrees_arm_s_limits(self):
        """`driver.py repair-session` starts Claude Code with the repair
        prompt and REPAIR_ARM's command line through the tool guard, even in
        the LangSpan arm."""
        seen = self.root / "seen.json"
        claude = self.root / "fake-claude"
        claude.write_text(
            "#!%s -I\nimport json, os, sys\n"
            "json.dump({'argv': sys.argv[1:], 'memory': os.environ.get(%r)}, open(%r, 'w'))\n"
            "print(json.dumps({'type': 'system', 'subtype': 'init', 'tools': %r, 'mcp_servers': []}))\n"
            % (sys.executable, driver.MEMORY_ENV, str(seen), list(driver.SESSION_TOOLS)))
        claude.chmod(0o755)
        config = self.root / "claude-config"
        config.mkdir()
        plan = {"agent": "claude", "model": None, "arm": "langspan", "sessions": 2, "work": str(self.root / "work"),
                "prompt": "repair the combined tree"}
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(plan))
        env = dict(os.environ, **{driver.CLAUDE_CONFIG_ENV: str(config), driver.CLAUDE_BIN_ENV: str(claude)})
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "repair-session", str(plan_path)],
                              capture_output=True, text=True, timeout=30, env=env)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(driver.tool_check(done.stdout)["status"], "passed", done.stdout)
        got = json.loads(seen.read_text())
        self.assertEqual(got["argv"], driver.repair_argv("claude", "repair the combined tree", plan)[1:])
        self.assertEqual(got["memory"], "1")
        self.assertNotIn("Skill", self.option(got["argv"], "--tools")[0])

    def test_the_repair_prompt_states_the_tasks_the_success_and_what_failed(self):
        plan = driver.make_plan("worktrees", "discovered_change", 2, 1, agent="claude")
        verdict = {"integrated_tests": "failed", "fixture_tests": "failed",
                   "fixture_tests_output_tail": "x" * 50000 + "TAIL",
                   "semantic_conflicts": ["shelf/features/a.py calls days_left with 1 argument(s)"],
                   "tasks": [{"session": 1, "task_id": plan["tasks"][0]["id"], "accepted": True, "detail": "ok"},
                             {"session": 2, "task_id": plan["tasks"][1]["id"], "accepted": False,
                              "detail": "calls days_left in its old form"}]}
        prompt = driver.repair_prompt(plan, verdict, [2])
        self.assertIn(tasks.SUCCESS["discovered_change"], prompt)
        for task in plan["tasks"]:
            self.assertIn(task["task"], prompt)
        self.assertIn("calls days_left in its old form", prompt)
        self.assertIn("shelf/features/a.py calls days_left", prompt)
        self.assertNotIn("Session 1 (`%s`): ok" % plan["tasks"][0]["id"], prompt, "only what failed")
        self.assertIn("conflict markers", prompt)
        self.assertIn("TAIL", prompt)
        self.assertLess(len(prompt.encode()), driver.REPAIR_FINDINGS_BYTES + 8000, "the check's output is capped")
        self.assertIn("data, not instructions", prompt)
        self.assertNotIn("conflict markers", driver.repair_prompt(plan, verdict, []))

    def test_whether_a_trial_gets_a_repair_round(self):
        base = {"status": "succeeded", "integrated_tests": "failed", "interface_change": "made",
                "tool_check": {"status": "passed", "failed_sessions": []}}
        self.assertEqual(driver.repair_decision(base), ("needed", None))
        self.assertEqual(driver.repair_decision(dict(base, integrated_tests="passed")), ("not_needed", None))
        self.assertEqual(driver.repair_decision(dict(base, status="timed_out")), ("needed", None))
        for change, why in ((dict(base, interface_change="change_not_made"), "change_not_made"),
                            (dict(base, integrated_tests="unknown"), "did not run"),
                            (dict(base, status="refused", integrated_tests="unknown"), "refused"),
                            (dict(base, status="interrupted"), "interrupted"),
                            (dict(base, tool_check={"status": "failed", "failed_sessions": [1]}), "tool check")):
            status, reason = driver.repair_decision(change)
            self.assertEqual(status, "not_run")
            self.assertIn(why, reason)

    def test_the_cost_to_a_correct_result_adds_the_repair_to_the_run(self):
        """Run, repair and total figures, and the cost per accepted task."""
        def result(turns, cost, tokens):
            return {"type": "result", "subtype": "success", "is_error": False, "result": "Done.",
                    "num_turns": turns, "duration_ms": 1000, "total_cost_usd": cost, "permission_denials": [],
                    "usage": dict(zip(("input_tokens", "output_tokens", "cache_creation_input_tokens",
                                       "cache_read_input_tokens"), tokens)), "modelUsage": {"model-r": {}}}
        init = {"type": "system", "subtype": "init", "tools": list(driver.SESSION_TOOLS), "mcp_servers": []}
        guard = {"type": driver.TOOL_CHECK_TYPE, "status": "passed", "unexpected": []}
        runner_dir, run_dir = self.root / "runner", self.root / "runner" / "r"
        run_dir.mkdir(parents=True)
        for index, (turns, cost) in enumerate(((10, 0.5), (6, 0.25)), 1):
            (run_dir / ("agent-s%d.stdout" % index)).write_text(
                self.transcript([init, result(turns, cost, (1, 2, 3, 4)), guard]))
        plan = driver.make_plan("worktrees", "discovered_change", 2, 1, agent="claude")
        plan.update(run_id="0" * 32, output=str(self.root), plan_path=str(self.root / "plan.json"),
                    work=str(self.root / "work"), repair_time_limit_s=900)
        verdict = {"integrated_tests": "failed", "interface_change": "made", "fixture_tests": "passed",
                   "semantic_conflicts": ["x"],
                   "tasks": [{"completed": True, "accepted": True, "detail": "ok"},
                             {"completed": False, "accepted": False, "detail": "old form"}]}
        (run_dir / "oracle.json").write_text(json.dumps(verdict))
        final = {"run_id": "0" * 32, "status": "succeeded", "agent_phase_elapsed_ns": 1,
                 "commands": [{"phase": "agent", "id": "s%d" % i, "status": "succeeded",
                               "stdout_path": "r/agent-s%d.stdout" % i} for i in (1, 2)]}
        record = driver.build_record(plan, final, runner_dir, run_dir, None, "t", 0.0)
        driver.validate(record, driver.load_schema())
        # Built before the repair round, it says none ran.
        self.assertEqual(record["repair"]["status"], "not_run")
        self.assertIsNone(record["cost"]["total"]["cost_usd"])

        repair_runner = self.root / "repair" / "runner"
        (repair_runner / "q").mkdir(parents=True)
        (repair_runner / "q" / "agent-repair.stdout").write_text(
            self.transcript([init, result(4, 0.125, (5, 6, 7, 8)), guard]))
        (repair_runner / "q" / "oracle.json").write_text(json.dumps(dict(
            verdict, integrated_tests="passed", semantic_conflicts=[],
            tasks=[{"completed": True, "accepted": True, "detail": "ok"}] * 2)))
        repair = {"status": "ran", "reason": None, "runner_dir": repair_runner, "run_dir": repair_runner / "q",
                  "conflicted_merges": [], "final": {
                      "run_id": "1" * 32, "status": "succeeded", "agent_phase_elapsed_ns": 3 * 10 ** 9,
                      "commands": [{"phase": "agent", "id": "repair", "status": "succeeded",
                                    "stdout_path": "q/agent-repair.stdout"}]}}
        record = driver.build_record(plan, final, runner_dir, run_dir, None, "t", 0.0, repair=repair)
        driver.validate(record, driver.load_schema())
        section = record["repair"]
        self.assertEqual((section["status"], section["succeeded"], section["integrated_tests"],
                          section["tasks_accepted"], section["time_limit_s"], section["elapsed_s"]),
                         ("ran", True, "passed", 2, 900, 3.0))
        self.assertEqual((section["turns"], section["cost_usd"], section["results"]), (4, 0.125, 1))
        self.assertEqual(section["tool_check"], {"status": "passed", "unexpected": []})
        self.assertEqual(section["permission_denials"],
                         {"total": 0, "peer_checkout": 0, "outside_clone": 0, "not_allowed": 0})
        self.assertEqual(section["evidence"]["transcript"], "repair/runner/q/agent-repair.stdout")
        cost = record["cost"]
        self.assertEqual(cost["run"], {"tokens": {"input": 2, "output": 4, "cache_creation": 6, "cache_read": 8},
                                       "turns": 16, "cost_usd": 0.75})
        self.assertEqual(cost["repair"], {"tokens": {"input": 5, "output": 6, "cache_creation": 7, "cache_read": 8},
                                          "turns": 4, "cost_usd": 0.125})
        self.assertEqual(cost["total"], {"tokens": {"input": 7, "output": 10, "cache_creation": 13, "cache_read": 16},
                                         "turns": 20, "cost_usd": 0.875})
        self.assertEqual((cost["correct_result"], cost["tasks_accepted"], cost["cost_per_accepted_task"]),
                         (True, 2, 0.4375))
        # The run's own figures stay the sessions' alone.
        self.assertEqual((record["cost_usd"], record["turns"], record["tasks_accepted"]), (0.75, 16, 1))
        self.assertIn("model-r", record["versions"]["models"])

        # A repair that fails: the result is not correct, and the cost per
        # accepted task counts only what the repaired tree accepts.
        (repair_runner / "q" / "oracle.json").write_text(json.dumps(verdict))
        record = driver.build_record(plan, final, runner_dir, run_dir, None, "t", 0.0, repair=repair)
        driver.validate(record, driver.load_schema())
        self.assertEqual((record["repair"]["succeeded"], record["cost"]["correct_result"],
                          record["cost"]["tasks_accepted"], record["cost"]["cost_per_accepted_task"]),
                         (False, False, 1, 0.875))
        # A repair whose session reported no cost leaves the total unmeasured.
        (repair_runner / "q" / "agent-repair.stdout").write_text(self.transcript([init, guard]))
        record = driver.build_record(plan, final, runner_dir, run_dir, None, "t", 0.0, repair=repair)
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["cost"]["total"], {"tokens": None, "turns": None, "cost_usd": None})
        self.assertIsNone(record["cost"]["cost_per_accepted_task"])
        # A repair whose tool guard failed keeps its evidence, but its result
        # is a host configuration failure, not a correct or incorrect result.
        failed_guard = dict(guard, status="failed", unexpected=["Bash"])
        (repair_runner / "q" / "agent-repair.stdout").write_text(
            self.transcript([init, result(4, 0.125, (5, 6, 7, 8)), failed_guard]))
        (repair_runner / "q" / "oracle.json").write_text(json.dumps(dict(
            verdict, integrated_tests="passed", semantic_conflicts=[],
            tasks=[{"completed": True, "accepted": True, "detail": "ok"}] * 2)))
        record = driver.build_record(plan, final, runner_dir, run_dir, None, "t", 0.0, repair=repair)
        driver.validate(record, driver.load_schema())
        self.assertEqual((record["repair"]["tool_check"], record["repair"]["succeeded"],
                          record["repair"]["cost_usd"], record["tool_check"]["status"]),
                         ({"status": "failed", "unexpected": ["Bash"]}, True, 0.125, "passed"))
        self.assertIsNone(record["cost"]["correct_result"])
        self.assertEqual(driver.excluded_as(record), "repair_tool_check_failed")

        # A tree that passed needs no repair: its repair figures are zero.
        verdict["integrated_tests"] = "passed"
        verdict["tasks"][1].update(completed=True, accepted=True)
        (run_dir / "oracle.json").write_text(json.dumps(verdict))
        record = driver.build_record(plan, final, runner_dir, run_dir, None, "t", 0.0,
                                     repair={"status": "not_needed", "reason": None})
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["repair"]["status"], "not_needed")
        self.assertIsNone(record["repair"]["succeeded"])
        self.assertEqual(record["cost"]["repair"], {"tokens": dict.fromkeys(driver.TOKEN_FIELDS, 0), "turns": 0,
                                                    "cost_usd": 0})
        self.assertEqual(record["cost"]["total"], record["cost"]["run"])
        self.assertEqual((record["cost"]["correct_result"], record["cost"]["cost_per_accepted_task"]), (True, 0.375))

    def summary_record(self, arm, run_cost, repair_status, repair_cost=None, succeeded=None, accepted=2,
                       change="made", status="succeeded", tool_check="passed", repair_tool_check=None):
        def figures(cost):
            if cost is None:
                return {"tokens": None, "turns": None, "cost_usd": None}
            return {"tokens": dict.fromkeys(driver.TOKEN_FIELDS, 1), "turns": 2, "cost_usd": cost}
        repair = figures(0) if repair_status == "not_needed" else figures(repair_cost)
        if repair_status == "not_needed":
            repair["tokens"] = dict.fromkeys(driver.TOKEN_FIELDS, 0)
            repair["turns"] = 0
        run = figures(run_cost)
        total = {"tokens": None if run["tokens"] is None or repair["tokens"] is None else
                 {key: run["tokens"][key] + repair["tokens"][key] for key in driver.TOKEN_FIELDS},
                 "turns": None if run["turns"] is None or repair["turns"] is None else run["turns"] + repair["turns"],
                 "cost_usd": None if run_cost is None or repair["cost_usd"] is None else run_cost + repair["cost_usd"]}
        correct = True if repair_status == "not_needed" else (succeeded if repair_status == "ran" else False)
        return {"schema_version": driver.SCHEMA_VERSION, "arm": arm, "family": "discovered_change", "sessions": 2,
                "agent": "claude-code", "stagger_s": 0, "status": status, "interface_change": change,
                "tool_check": {"status": tool_check, "failed_sessions": [1] if tool_check == "failed" else []},
                "tasks_total": 2, "repair": {
                    "status": repair_status, "succeeded": succeeded,
                    "tool_check": None if repair_status != "ran" else
                    {"status": repair_tool_check or "passed", "unexpected": []}},
                "cost": {"run": run, "repair": repair, "total": total, "tasks_accepted": accepted,
                         "correct_result": correct,
                         "cost_per_accepted_task": total["cost_usd"] / accepted if total["cost_usd"] and accepted else None}}

    def test_the_summary_reports_each_arm_s_cost_to_a_correct_result(self):
        records = [
            self.summary_record("langspan", 1.0, "not_needed"),
            self.summary_record("langspan", 1.5, "not_needed"),
            self.summary_record("worktrees", 1.0, "ran", 0.5, True),
            self.summary_record("worktrees", 1.0, "ran", 0.25, False, accepted=1),
            # Counted apart: the changer made no change, so nothing was compared.
            self.summary_record("worktrees", 0.75, "not_run", change="change_not_made", accepted=0),
            self.summary_record("awareness_only", 1.0, "ran", None, True),
        ]
        groups = {group["arm"]: group for group in driver.summarize(records)}
        langspan, worktrees, aware = groups["langspan"], groups["worktrees"], groups["awareness_only"]
        self.assertEqual((langspan["trials"], langspan["compared"], langspan["passed_without_repair"],
                          langspan["repairs"], langspan["correct_results"]), (2, 2, 2, 0, 2))
        self.assertEqual(langspan["total"]["cost_usd"], 2.5)
        self.assertEqual(langspan["repair"]["cost_usd"], 0)
        self.assertEqual(langspan["cost_per_accepted_task"], 0.625)
        self.assertEqual((worktrees["trials"], worktrees["change_not_made"], worktrees["compared"],
                          worktrees["repairs"], worktrees["repairs_succeeded"], worktrees["correct_results"]),
                         (3, 1, 2, 2, 1, 1))
        self.assertEqual((worktrees["tasks_total"], worktrees["tasks_accepted"]), (4, 3))
        self.assertEqual((worktrees["run"]["cost_usd"], worktrees["repair"]["cost_usd"],
                          worktrees["total"]["cost_usd"]), (2.0, 0.75, 2.75))
        self.assertEqual(worktrees["total"]["turns"], 8)
        self.assertEqual(worktrees["total"]["tokens"]["input"], 4)
        self.assertEqual(worktrees["cost_per_accepted_task"], round(2.75 / 3, 6))
        # One repair reported no cost: the arm's repair and total cost are unmeasured, never zero.
        self.assertEqual((aware["repair"]["cost_usd"], aware["total"]["cost_usd"], aware["cost_per_accepted_task"],
                          aware["unreported"]), (None, None, None, 1))
        self.assertEqual(aware["run"]["cost_usd"], 1.0)
        # A refused trial is counted, and no figure comes from it.
        refused = self.summary_record("langspan", None, "not_run", status="refused", accepted=None)
        group = driver.summarize(records[:2] + [refused])[0]
        self.assertEqual((group["trials"], group["refused"], group["compared"], group["total"]["cost_usd"]),
                         (3, 1, 2, 2.5))

    def test_the_summary_counts_a_failed_tool_check_apart_from_the_arm_comparison(self):
        # A failed tool check is a host configuration failure: the trial does
        # not count, so it adds no task, result or figure to its arm.
        records = [self.summary_record("langspan", 1.0, "not_needed"),
                   self.summary_record("langspan", 1.5, "not_needed"),
                   self.summary_record("langspan", 2.0, "not_run", status="failed", tool_check="failed", accepted=0)]
        group = driver.summarize(records)[0]
        self.assertEqual((group["trials"], group["tool_check_failed"], group["compared"], group["correct_results"],
                          group["tasks_total"], group["tasks_accepted"], group["total"]["cost_usd"],
                          group["repairs_not_run"]), (3, 1, 2, 2, 4, 4, 2.5, 0))

    def test_the_summary_counts_a_failed_repair_tool_check_apart_from_the_arm_comparison(self):
        # The sessions' guard passed but the repair session's failed: a host
        # configuration failure in the repair, so the trial does not count,
        # whether the repaired tree passed, failed or its cost went unreported.
        for succeeded, cost in ((True, 0.5), (False, 0.5), (True, None)):
            with self.subTest(succeeded=succeeded, cost=cost):
                records = [self.summary_record("worktrees", 1.0, "ran", 0.25, True),
                           self.summary_record("worktrees", 1.0, "ran", cost, succeeded,
                                               repair_tool_check="failed")]
                group = driver.summarize(records)[0]
                self.assertEqual((group["trials"], group["repair_tool_check_failed"], group["tool_check_failed"],
                                  group["compared"], group["repairs"], group["repairs_succeeded"],
                                  group["correct_results"], group["tasks_total"], group["total"]["cost_usd"],
                                  group["unreported"]), (2, 1, 0, 1, 1, 1, 1, 2, 1.25, 0))
        # A repair guard that wrote no verdict (a session stopped from
        # outside) is not a failed check: the trial counts, as for the sessions.
        records = [self.summary_record("worktrees", 1.0, "ran", 0.25, True, repair_tool_check="unknown")]
        group = driver.summarize(records)[0]
        self.assertEqual((group["repair_tool_check_failed"], group["compared"]), (0, 1))
        # A failed session guard and a failed repair guard count once, as the first.
        records = [self.summary_record("worktrees", 1.0, "ran", 0.25, True, tool_check="failed",
                                       repair_tool_check="failed")]
        group = driver.summarize(records)[0]
        self.assertEqual((group["tool_check_failed"], group["repair_tool_check_failed"], group["compared"]),
                         (1, 0, 0))

    def test_the_summary_command_reads_record_files_and_refuses_an_older_record(self):
        good = self.root / "a"
        good.mkdir()
        (good / "record.json").write_text(json.dumps(self.summary_record("langspan", 1.0, "not_needed")))
        old = self.root / "old.json"
        old.write_text(json.dumps({"schema_version": 5, "arm": "langspan"}))
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "summarize", str(good)],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(json.loads(done.stdout)["groups"][0]["total"]["cost_usd"], 1.0)
        done = subprocess.run([sys.executable, "-I", "-B", str(HERE / "driver.py"), "summarize", str(good), str(old)],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 2)
        self.assertIn("schema version 5", done.stderr)


class StandinHooks(Temporary):
    """How the stand-in runs a hook: to completion within its own bound,
    whatever the host timeout, and never leaving one running."""

    def session(self, command, timeout):
        settings = {"hooks": {"PreToolUse": [{"matcher": "*", "hooks": [
            {"type": "command", "command": command, "timeout": timeout}]}]}}
        log = open(self.root / "standin.jsonl", "w", encoding="utf-8")
        self.addCleanup(log.close)
        return standin_agent.Session(settings, log, str(self.root)), log

    def test_a_following_wait_lasts_until_just_before_the_session_limit(self):
        # A fixed wait ran out on a loaded machine before LangSpan's notice
        # arrived, and the session ended uninformed. Every wait now shares
        # the session's own time limit.
        started = time.monotonic() - 5
        self.assertEqual(standin_agent.wait_deadline({"time_limit_s": 600}, started),
                         started + 600 - standin_agent.WAIT_MARGIN_SECONDS)
        # A plan without a limit, or one too short for the margin, keeps the fixed wait.
        for plan in ({}, {"time_limit_s": standin_agent.WAIT_MARGIN_SECONDS}):
            before = time.monotonic()
            deadline = standin_agent.wait_deadline(plan, started)
            self.assertGreaterEqual(deadline, before + standin_agent.WAIT_SECONDS, plan)
            self.assertLessEqual(deadline, time.monotonic() + standin_agent.WAIT_SECONDS, plan)

    def test_a_hook_slower_than_its_host_timeout_still_decides_the_call(self):
        # On a loaded machine a hook can start late. Its decision is what the
        # dry run measures, so the edit waits for it, and the log says it was late.
        deny = json.dumps({"hookSpecificOutput": {"permissionDecision": "deny"}})
        session, log = self.session("sleep 2; printf '%%s' %s" % shlex.quote(deny), 1)
        denied = session.fire("PreToolUse", "Write", {"file_path": "x"}, "edit-1")
        log.flush()
        entry = json.loads((self.root / "standin.jsonl").read_text().splitlines()[0])
        self.assertTrue(denied, entry)
        self.assertEqual(entry["decision"], "deny", entry)
        self.assertTrue(entry["late"], entry)
        self.assertGreaterEqual(entry["elapsed_ms"], 2000, entry)
        self.assertEqual(session.late_hooks, 1)

    def test_a_hook_past_the_standin_bound_leaves_no_process_running(self):
        # A hook the stand-in gives up on must not go on acting, for example
        # by reading a file after the edit it was asked about was applied.
        # The child's heartbeat shows whether it still acts. Its PID alone
        # does not: where PID 1 does not reap orphans, a killed child stays
        # a zombie and `kill(pid, 0)` still succeeds.
        heartbeat, group = self.root / "heartbeat", self.root / "group"
        command = "echo $$ > %s; while :; do printf x >> %s; sleep 0.05; done & wait" % (
            shlex.quote(str(group)), shlex.quote(str(heartbeat)))
        session, _ = self.session(command, 1)
        original = standin_agent.HOOK_SECONDS
        standin_agent.HOOK_SECONDS = 2
        try:
            denied = session.fire("PreToolUse", "Write", {"file_path": "x"}, "edit-1")
            self.assertFalse(denied)
            self.assertGreater(heartbeat.stat().st_size, 0, "the hook's child never ran")
            self.assert_stopped(heartbeat)
        finally:
            standin_agent.HOOK_SECONDS = original
            self.kill_groups(group)

    def test_a_hook_that_exits_leaves_no_background_child_running(self):
        # A hook can start a child with its streams redirected and exit 0.
        # The child is in the hook's process group, outside the runner's, so
        # once the hook returns nothing else would stop it.
        heartbeat, group = self.root / "heartbeat", self.root / "group"
        command = "echo $$ > %s; (while :; do printf x >> %s; sleep 0.05; done) </dev/null >/dev/null 2>&1 & " \
                  "sleep 0.3; exit 0" % (shlex.quote(str(group)), shlex.quote(str(heartbeat)))
        session, log = self.session(command, 5)
        try:
            denied = session.fire("PreToolUse", "Write", {"file_path": "x"}, "edit-1")
            log.flush()
            entry = json.loads((self.root / "standin.jsonl").read_text().splitlines()[0])
            self.assertFalse(denied, entry)
            self.assertEqual(entry["exit_code"], 0, entry)
            self.assertEqual(standin_agent.RUNNING, [])
            self.assertGreater(heartbeat.stat().st_size, 0, "the hook's child never ran")
            self.assert_stopped(heartbeat)
        finally:
            self.kill_groups(group)

    def kill_groups(self, group_file, *groups):
        """Kill GROUPS and the hook's group named in GROUP_FILE, so a failed
        test leaves nothing running."""
        if group_file.exists() and group_file.read_text().strip():
            groups += (int(group_file.read_text()),)
        for group in groups:
            try:
                os.killpg(group, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass

    def assert_stopped(self, heartbeat):
        """HEARTBEAT stops growing within a bound, and stays the same for a
        second after that, many times the child's 0.05 s beat."""
        deadline = time.monotonic() + 10
        size = heartbeat.stat().st_size if heartbeat.exists() else 0
        while True:
            time.sleep(1)
            now = heartbeat.stat().st_size if heartbeat.exists() else 0
            if now == size:
                return
            self.assertLess(time.monotonic(), deadline, "the hook's child is still writing %s" % heartbeat)
            size = now

    def test_terminating_the_standin_stops_the_hook_it_is_running(self):
        # The runner ends an attempt by signaling the stand-in's process
        # group. Each hook runs in a group of its own, so the stand-in must
        # end the running hook's group before it exits.
        heartbeat = self.root / "heartbeat"
        started = self.root / "started"
        command = "echo $$ > %s; while :; do printf x >> %s; sleep 0.05; done" % (
            shlex.quote(str(started)), shlex.quote(str(heartbeat)))
        settings = {"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": command}]}]}}
        (self.root / ".claude").mkdir()
        (self.root / ".claude" / "settings.local.json").write_text(json.dumps(settings))
        plan = {"sessions": 1, "prompts": ["p"],
                "tasks": [{"task": "t", "details": "d", "standin": {"ignore": [], "follow": []}}]}
        (self.root / "plan.json").write_text(json.dumps(plan))
        env = dict(os.environ, LANGSPAN_BENCH_RUN_DIR=str(self.root))
        standin = subprocess.Popen([sys.executable, str(HERE / "standin_agent.py"), "plan.json", "1", "ignore"],
                                   cwd=self.root, env=env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
        try:
            deadline = time.monotonic() + 10
            while not (started.exists() and started.read_text().strip() and heartbeat.exists()):
                self.assertLess(time.monotonic(), deadline, "the hook never started")
                time.sleep(0.05)
            os.killpg(standin.pid, signal.SIGTERM)
            standin.wait(timeout=10)
            self.assert_stopped(heartbeat)
        finally:
            self.kill_groups(started, standin.pid)
            standin.wait()


class RefusalCollection(Temporary):
    def test_a_real_refusal_is_collected_with_what_followed(self):
        """Two stand-in sessions in one clone: B edits a file after A read
        it, so LangSpan refuses A's edit as stale; A reads again and is
        admitted. The record's refusal comes from the real event log."""
        work = self.root / "work"
        work.mkdir()
        _, _, clones = driver.prepare_fixture(work, 1, "langspan")
        state = Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, state, True)
        driver.install_hooks(langspan_binary(), state, 1, clones[0], self.root / "hook-log.jsonl")
        coordinator = driver.Coordinator(langspan_binary(), state, self.root / "coordinator.log")
        try:
            settings = json.loads((clones[0] / ".claude" / "settings.local.json").read_text())
            target = (clones[0] / "shelf" / "report.py").resolve()
            with open(self.root / "hooks.jsonl", "w") as log:
                a, b = (standin_agent.Session(settings, log, str(clones[0].resolve())) for _ in range(2))
                for session in (a, b):
                    session.fire("SessionStart", source="startup")
                a.read(target)
                b.read(target)

                def edit(session, old, new):
                    def apply():
                        target.write_text(target.read_text().replace(old, new))
                    return session.edit(target, "Edit", {"file_path": str(target), "old_string": old,
                                                         "new_string": new, "replace_all": False}, apply)

                self.assertTrue(edit(b, "nobody", "no owner"))
                self.assertTrue(edit(a, "one-line summary", "short summary"))
            self.assertEqual(len(a.refusals), 1, "A's first attempt is refused, its retry admitted")
            items = driver.refusal_items(coordinator.events())
        finally:
            coordinator.stop()
        self.assertEqual(len(items), 1, items)
        self.assertEqual(items[0]["kind"], "stale_base")
        self.assertEqual(items[0]["session"], 1)
        self.assertEqual(items[0]["paths"], ["shelf/report.py"])
        self.assertEqual(items[0]["followed_by"], "admitted_later")
        # The pass-through hook log kept what LangSpan said, and when.
        logged = [json.loads(line) for line in (self.root / "hook-log.jsonl").read_text().splitlines()]
        self.assertIn("PreToolUse", {entry["event"] for entry in logged})
        refused = [entry for entry in logged if '"deny"' in entry["stdout"]]
        self.assertEqual(len(refused), 1, logged)
        self.assertTrue(refused[0]["tool_use_id"].startswith("edit-"))
        self.assertTrue(all(entry["at"] and entry["elapsed_ms"] >= 0 for entry in logged))

    def test_a_pre_tool_hook_that_starts_late_refuses_nothing(self):
        """Issue #200: on a loaded runner a PreToolUse hook can start after
        its 5-second host timeout. The edit waits for it, so the hook reads
        the version the session read and LangSpan admits the edit. Before,
        the stand-in applied the edit at the timeout and the hook, still
        running, read the edited file and was refused as stale."""
        work = self.root / "work"
        work.mkdir()
        _, _, clones = driver.prepare_fixture(work, 1, "langspan")
        state = Path(tempfile.mkdtemp(prefix="lsb-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, state, True)
        driver.install_hooks(langspan_binary(), state, 1, clones[0], self.root / "hook-log.jsonl")
        coordinator = driver.Coordinator(langspan_binary(), state, self.root / "coordinator.log")
        try:
            settings = json.loads((clones[0] / ".claude" / "settings.local.json").read_text())
            # LangSpan's hooks start late, behind the hook-log wrapper, as
            # a slow start of the hook process on a loaded runner makes
            # them: PreToolUse past its 5 s timeout, PostToolUse within it.
            for event, delay in (("PreToolUse", 6), ("PostToolUse", 4)):
                for entry in settings["hooks"][event]:
                    for hook in entry["hooks"]:
                        self.assertEqual(hook["timeout"], 5, hook)
                        *wrapper, inner = shlex.split(hook["command"])
                        hook["command"] = " ".join(shlex.quote(part)
                                                   for part in wrapper + ["sleep %d; %s" % (delay, inner)])
            target = (clones[0] / "shelf" / "report.py").resolve()
            with open(self.root / "hooks.jsonl", "w") as log:
                session = standin_agent.Session(settings, log, str(clones[0].resolve()))
                session.fire("SessionStart", source="startup")
                session.read(target)
                edited = session.edit(target, "Edit", {"file_path": str(target), "old_string": "nobody",
                                                       "new_string": "no owner", "replace_all": False},
                                      lambda: target.write_text(target.read_text().replace("nobody", "no owner")))
            # Read the log once LangSpan has decided the call, however late.
            decided = ("lease_granted", "write_refused")
            deadline = time.monotonic() + 60
            while True:
                events = coordinator.events()
                if any(record.get("event", {}).get("type") in decided for record in events):
                    break
                self.assertLess(time.monotonic(), deadline, "LangSpan never decided the edit")
                time.sleep(0.1)
        finally:
            coordinator.stop()
        seen = "stand-in: %s\nevents: %s" % ((self.root / "hooks.jsonl").read_text(),
                                             [record.get("event", {}).get("type") for record in events])
        self.assertTrue(edited, seen)
        self.assertEqual(session.refusals, [], seen)
        self.assertEqual(driver.refusal_items(events), [], seen)
        self.assertIn("lease_granted", [record.get("event", {}).get("type") for record in events], seen)
        self.assertGreaterEqual(session.late_hooks, 1, seen)
        self.assertIn("no owner", target.read_text())


class DryRun(Temporary):
    """The whole pipeline for each arm, family and stand-in mode."""

    def run_driver(self, arm, family, n, mode, seed=11, stagger=None):
        output = self.root / ("%s-%s-%d-%s" % (arm, family, n, mode))
        # A following stand-in waits for its peers and LangSpan's notices
        # until shortly before this limit, so a loaded machine slows the run
        # instead of changing what the sessions learn. An unloaded run ends
        # its waits in seconds.
        argv = [sys.executable, "-B", str(HERE / "driver.py"), "--arm", arm, "--family", family,
                "--sessions", str(n), "--seed", str(seed), "--standin-mode", mode, "--time-limit", "600",
                "--run", "--output", str(output)]
        if stagger is not None:
            argv += ["--stagger", str(stagger)]
        if arm == "langspan":
            argv += ["--langspan", str(langspan_binary())]
        done = subprocess.run(argv, capture_output=True, text=True, timeout=900)
        record_path = output / "record.json"
        self.assertTrue(record_path.is_file(), done.stdout + done.stderr)
        record = json.loads(record_path.read_text())
        driver.validate(record, driver.load_schema())
        self.assertNotIn("error", record, record.get("error"))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual((output.stat().st_mode & 0o777), 0o700)
        self.check_common(arm, family, n, mode, output, record)
        return record

    def check_common(self, arm, family, n, mode, output, record):
        self.assertEqual((record["arm"], record["family"], record["sessions"]), (arm, family, n))
        self.assertTrue(record["dry_run"])
        self.assertEqual(record["standin_mode"], mode)
        self.assertEqual(record["status"], "succeeded")
        self.assertEqual(record["tasks_total"], n)
        self.assertEqual(record["stops"], 0)
        self.assertEqual(record["questions"], 0)
        self.assertEqual(record["permission_denials"],
                         {"total": 0, "peer_checkout": 0, "outside_clone": 0, "not_allowed": 0})
        # Every session went through the tool guard and listed only its arm's tools.
        self.assertEqual(record["tool_check"], {"status": "passed", "failed_sessions": []})
        self.assertEqual(record["isolation"], "worktree" if arm == "langspan" else "clone")
        self.assertEqual(record["tokens"], {"status": "unknown", "total": None})
        self.assertIsNone(record["cost_usd"])
        self.assertEqual(record["ignored_options"], [])
        self.assertIsNone(record["versions"]["requested_model"])
        self.assertEqual(record["versions"]["agent_cli"], "standin-agent")
        self.assertTrue(record["versions"]["git"].startswith("git version"))
        rows = [json.loads(line) for line in (output / "runner" / "runs.jsonl").read_text().splitlines()]
        self.assertEqual([r["record_type"] for r in rows], ["run_started", "run_finished"])
        self.assertEqual([c["phase"] for c in rows[1]["commands"]], ["version"] * 2 + ["agent"] * n + ["check"])
        self.assertEqual(rows[1]["metrics"]["integrated_correctness"]["status"], record["integrated_tests"])
        self.check_repair(output, record)
        plan = json.loads((output / "plan.json").read_text())
        prompts = plan["prompts"]
        self.assertEqual(plan["driver_command"][2:4], ["--arm", arm], "the exact driver command is kept")
        work = output / "work"
        for index, task in enumerate(record["tasks"], 1):
            self.assertEqual((task["turns"], task["cost_usd"], task["results"]), (1, None, 1))
            self.assertEqual(task["tool_check"], {"status": "passed", "unexpected": []})
            self.assertEqual(task["interface_notices"] is not None, arm == "langspan")
            transcript = output / task["transcript"]
            self.assertEqual(len(driver.session_results(transcript.read_text())), 1)
            clone = work / ("s%d" % index)
            settings = clone / ".claude" / "settings.local.json"
            skill = clone / ".claude" / "skills" / "langspan" / "SKILL.md"
            self.assertEqual(settings.is_file(), arm == "langspan")
            self.assertEqual(skill.is_file(), arm == "langspan")
            self.assertEqual("Other sessions are working" in prompts[index - 1], arm == "awareness_only")
            # Nothing of LangSpan's reaches a branch.
            tracked = driver.git("ls-tree", "-r", "--name-only", driver.branch(index), cwd=work / "repo").stdout
            self.assertNotIn(".claude", tracked)
            self.assertEqual(task["hook_log"] is not None, arm == "langspan")
        if arm == "langspan":
            refusals = record["refusals"]
            self.assertEqual(refusals["status"], "reported")
            self.assertEqual(refusals["count"], 0, self.refusal_report(output, record))
            self.assertEqual(refusals["sessions_registered"], n)
            self.assertGreaterEqual(refusals["admissions"], n)
            self.assertEqual(record["coordinator_scans"]["status"], "reported")
            # One coordinator for the repository whose worktrees the sessions use.
            self.assertEqual(record["evidence"]["events"], ["events.jsonl"])
            self.assertTrue(all((output / path).is_file() for path in record["evidence"]["events"]))
            self.assertEqual([t["refusals"] for t in record["tasks"]], [0] * n)
        else:
            self.assertEqual(record["refusals"]["status"], "not_applicable")
            self.assertIsNone(record["refusals"]["count"])
            self.assertEqual(record["coordinator_scans"]["status"], "not_applicable")
            self.assertEqual(record["evidence"]["events"], [])

    def check_repair(self, output, record):
        """Issue #221: one repair round for a failed tree and none for a
        passing one, or for a trial whose changer made no change."""
        repair, cost = record["repair"], record["cost"]
        ran = record["integrated_tests"] == "failed" and record["interface_change"] != "change_not_made"
        self.assertEqual(repair["status"], "ran" if ran else (
            "not_needed" if record["integrated_tests"] == "passed" else "not_run"), repair)
        self.assertEqual((output / "repair").exists(), ran)
        # The stand-in reports no cost, so no total is measured in a dry run.
        self.assertEqual(cost["run"], {"tokens": None, "turns": 1 * record["sessions"], "cost_usd": None})
        self.assertIsNone(cost["total"]["cost_usd"])
        self.assertIsNone(cost["cost_per_accepted_task"])
        if not ran:
            self.assertIsNone(repair["succeeded"])
            self.assertEqual(cost["correct_result"], record["integrated_tests"] == "passed")
            self.assertEqual(cost["tasks_accepted"], record["tasks_accepted"])
            if repair["status"] == "not_run":
                self.assertIn("change_not_made", repair["reason"])
            return
        self.assertIsInstance(repair["succeeded"], bool)
        self.assertEqual(repair["succeeded"], repair["integrated_tests"] == "passed")
        self.assertEqual(cost["correct_result"], repair["succeeded"])
        self.assertEqual(cost["tasks_accepted"], repair["tasks_accepted"])
        self.assertEqual((repair["process_status"], repair["time_limit_s"], repair["turns"], repair["results"]),
                         ("succeeded", 1200, 1, 1))
        self.assertEqual(repair["tool_check"], {"status": "passed", "unexpected": []})
        self.assertEqual(repair["permission_denials"],
                         {"total": 0, "peer_checkout": 0, "outside_clone": 0, "not_allowed": 0})
        rows = [json.loads(line) for line in (output / repair["evidence"]["runner_journal"]).read_text().splitlines()]
        self.assertEqual([r["record_type"] for r in rows], ["run_started", "run_finished"])
        self.assertEqual([c["phase"] for c in rows[1]["commands"]], ["version"] * 2 + ["agent", "check"])
        manifest = json.loads((output / "repair" / "manifest.json").read_text())
        agents = next(iter(manifest["items"][0]["arms"].values()))["agents"]
        self.assertEqual([(a["id"], Path(a["cwd"]).resolve()) for a in agents],
                         [("repair", (output / "work" / "repair").resolve())])
        transcript = (output / repair["evidence"]["transcript"]).read_text()
        self.assertEqual(driver.tool_check(transcript)["status"], "passed")
        self.assertTrue((output / repair["evidence"]["oracle"]).is_file())
        # Nothing of LangSpan's reaches the repair tree.
        self.assertFalse((output / "work" / "repair" / ".claude").exists())

    def refusal_report(self, output, record):
        """What a failed refusal check saw: each refusal, what LangSpan told
        the refused session, and that session's hooks slower than the host
        timeout."""
        lines = ["refusals: %s" % json.dumps(record["refusals"]["items"])]
        for index, task in enumerate(record["tasks"], 1):
            if not task["refusals"]:
                continue
            lines.append("session %d (%s, %s):" % (index, task["role"], task["task_id"]))
            log = output / task["hook_log"] if task["hook_log"] else None
            for line in log.read_text().splitlines() if log and log.is_file() else []:
                entry = json.loads(line)
                if '"deny"' in entry.get("stdout", ""):
                    lines.append("  %s at %s: %s" % (entry.get("event"), entry.get("at"), entry["stdout"][:1500]))
                elif entry.get("elapsed_ms", 0) > 5000:
                    lines.append("  %s %s at %s took %s ms" % (entry.get("event"), entry.get("tool"),
                                                              entry.get("at"), entry["elapsed_ms"]))
        return "\n".join(lines)

    def informed(self, output_record):
        return [t["standin_informed"] for t in output_record["tasks"]]

    def layout(self, record):
        """Per session: `pair`, `alone` or `control` for a contradictory run."""
        assigned = tasks.assign(record["family"], record["sessions"], record["seed"])
        keys = [task.get("pair") for task in assigned]
        return ["control" if key is None else ("pair" if keys.count(key) == 2 else "alone") for key in keys]

    def check_uninformed(self, record, family, n):
        """What sessions that know nothing of their peers produce: each
        makes its task's uninformed edit. That fails wherever a task depends
        on a peer's: always in the rename family, and wherever a key is
        paired in the contradictory one."""
        self.assertEqual(record["duplicate_work"], 0)
        if family in ("rename_vs_new_caller", "discovered_change"):
            self.assertEqual(record["integrated_tests"], "failed")
            # Every new caller uses the old form, which the changing task removed.
            self.assertEqual(record["merge_conflicts"], [])
            self.assertEqual(record["semantic_conflicts"], n - 1, record["semantic_conflict_details"])
            self.assertEqual(record["tasks_accepted"], 1)
            self.assertEqual([t["accepted"] for t in record["tasks"] if t["role"] in ("rename", "changer")], [True])
            # The stand-in's repair makes every task's informed edit, which fixes each call.
            self.assertEqual((record["repair"]["status"], record["repair"]["succeeded"]), ("ran", True), record["repair"])
            self.assertEqual((record["repair"]["tasks_accepted"], record["repair"]["conflicted_merges"]), (n, []))
            self.assertEqual(record["cost"]["correct_result"], True)
        else:
            # Both sides of each pair change the same default: Git refuses the
            # second. A task with no partner was right to change it.
            layout = self.layout(record)
            pairs = layout.count("pair") // 2
            self.assertEqual(record["integrated_tests"], "failed" if pairs else "passed")
            self.assertEqual(len(record["merge_conflicts"]), pairs)
            self.assertTrue(all(c["files"] == ["shelf/settings.py"] for c in record["merge_conflicts"]))
            self.assertEqual(record["semantic_conflicts"], 0)
            self.assertEqual([t["accepted"] for t in record["tasks"]], [kind != "pair" for kind in layout])
            self.assertEqual(sum(bool(t["completed"]) for t in record["tasks"]), n - pairs)
            if pairs:
                # The repair tree holds each refused merge with Git's conflict
                # markers, which the stand-in does not resolve: the repair fails.
                self.assertEqual(record["repair"]["status"], "ran")
                self.assertEqual(record["repair"]["conflicted_merges"], [c["session"] for c in record["merge_conflicts"]])
                self.assertEqual((record["repair"]["succeeded"], record["cost"]["correct_result"]), (False, False))
            else:
                self.assertEqual(record["repair"]["status"], "not_needed")

    def check_hedged(self, record, family, n):
        """Sessions that keep every shared default and set their value
        locally, whatever their arm told them: the hedge the pilot's
        sessions made. Only the pairs' reconciliation is accepted."""
        if family == "rename_vs_new_caller":
            self.check_uninformed(record, family, n)
            return
        if family == "discovered_change":
            # The changer keeps old calls working with a default, and each
            # dependent counts the days itself: the tree works, and no task
            # is what it asked for. A default is not the change, so no
            # dependent is judged.
            self.check_change_not_made(record, n)
            return
        layout = self.layout(record)
        self.assertIn("alone", layout, "every N = 5 layout has a task without a partner")
        self.assertEqual(record["integrated_tests"], "failed")
        self.assertEqual((record["merge_conflicts"], record["semantic_conflicts"]), ([], 0))
        self.assertEqual([t["accepted"] for t in record["tasks"]], [kind != "alone" for kind in layout])
        self.assertTrue(all(t["completed"] for t in record["tasks"]))
        self.assertTrue(all("no other task" in t["detail"] for t, kind in zip(record["tasks"], layout)
                            if kind == "alone"))
        # The stand-in's repair makes the same local edit, so the default a
        # task alone wanted changed still is not.
        self.assertEqual((record["repair"]["status"], record["repair"]["succeeded"]), ("ran", False))

    def check_change_not_made(self, record, n):
        """A discovered_change trial whose changer did not change the
        interface: the changer fails, and no dependent is judged."""
        self.assertEqual(record["interface_change"], "change_not_made")
        self.assertEqual(record["integrated_tests"], "failed")
        self.assertEqual((record["merge_conflicts"], record["semantic_conflicts"]), ([], 0))
        self.assertEqual(record["fixture_tests"], "passed")
        self.assertEqual(record["tasks_accepted"], 0)
        roles = [t["role"] for t in record["tasks"]]
        self.assertEqual([t["accepted"] for t in record["tasks"]], [False if r == "changer" else None for r in roles])
        self.assertTrue(all(t["detail"].startswith("change_not_made") for t in record["tasks"]), record["tasks"])

    def check_arm(self, arm, sizes, modes=("ignore", "follow")):
        for family in tasks.FAMILIES:
            # Peer task text informs the awareness arm, except where no task
            # text names the change: there only LangSpan's notices can.
            informs = arm == "langspan" or (arm == "awareness_only" and family != "discovered_change")
            for n in sizes:
                for mode in modes:
                    with self.subTest(family=family, n=n, mode=mode):
                        record = self.run_driver(arm, family, n, mode)
                        if mode != "hedge":
                            self.assertEqual(record["interface_change"],
                                             "made" if family == "discovered_change" else "not_applicable")
                        if family == "rename_vs_new_caller":
                            triggered = [t["role"] == "new_caller" for t in record["tasks"]]
                        elif family == "discovered_change":
                            triggered = [t["role"] == "dependent" for t in record["tasks"]]
                        else:
                            triggered = [kind == "pair" for kind in self.layout(record)]
                        # Measured only for a task that expects a notice, in the LangSpan arm.
                        for task in record["tasks"]:
                            measured = arm == "langspan" and task["role"] in ("new_caller", "dependent")
                            self.assertEqual(task["notice_before_write"] is not None, measured, task)
                        if mode == "follow" and informs:
                            # The information its arm gave let every session reconcile.
                            self.assertEqual(self.informed(record), triggered)
                            self.assertEqual(record["integrated_tests"], "passed", record)
                            self.assertEqual((record["semantic_conflicts"], record["merge_conflicts"]), (0, []))
                            self.assertEqual(record["tasks_accepted"], n)
                            self.assertEqual(record["fixture_tests"], "passed")
                        elif mode == "hedge":
                            self.check_hedged(record, family, n)
                        else:
                            self.assertEqual(self.informed(record), [False] * n)
                            self.check_uninformed(record, family, n)
                        if arm == "langspan" and mode == "follow" and family == "rename_vs_new_caller":
                            # LangSpan's interface notice of the rename reached every
                            # new caller's session, in the coordinator's own log.
                            notices = [t["interface_notices"] for t in record["tasks"] if t["role"] == "new_caller"]
                            self.assertTrue(all(count >= 1 for count in notices), notices)
                            output = self.root / ("%s-%s-%d-%s" % (arm, family, n, mode))
                            told = [(output / t["hook_log"]).read_text() for t in record["tasks"]
                                    if t["role"] == "new_caller"]
                            self.assertTrue(all("load_config" in log and "In flight" in log for log in told))
                        if arm == "langspan" and mode == "follow" and family == "discovered_change":
                            # Only LangSpan's notice of the changed signature told each dependent.
                            output = self.root / ("%s-%s-%d-%s" % (arm, family, n, mode))
                            dependents = [t for t in record["tasks"] if t["role"] == "dependent"]
                            self.assertTrue(all(t["interface_notices"] >= 1 for t in dependents), dependents)
                            told = [(output / t["hook_log"]).read_text() for t in dependents]
                            self.assertTrue(all("def days_left(day, today)" in log for log in told))
                        if arm != "langspan":
                            self.assertEqual([t["interface_notices"] for t in record["tasks"]], [None] * n)

    def test_dry_run_arm_worktrees(self):
        self.check_arm("worktrees", (2, 5))

    def test_dry_run_hedging_fails_in_every_arm(self):
        """The pilot's sessions hedged with no peer information, and the old
        oracle accepted it. Now a hedge fails wherever a task has no partner,
        and a change kept callable the old way fails as well."""
        for family in ("contradictory_defaults", "discovered_change"):
            for arm in driver.ARMS:
                with self.subTest(family=family, arm=arm):
                    record = self.run_driver(arm, family, 5, "hedge")
                    self.check_hedged(record, family, 5)

    def test_dry_run_a_changer_that_works_around_the_change_is_change_not_made_in_every_arm(self):
        """Issue #218: a changer that finishes without changing `days_left`
        fails its own task, and the trial is reported as change_not_made,
        not as a failure of every dependent."""
        for arm in driver.ARMS:
            with self.subTest(arm=arm):
                record = self.run_driver(arm, "discovered_change", 5, "workaround")
                self.check_change_not_made(record, 5)
                changer = next(t for t in record["tasks"] if t["role"] == "changer")
                self.assertIn("days_left", changer["detail"])

    def test_dry_run_with_stagger_tells_every_dependent_before_its_first_write(self):
        """Issue #194's third criterion: the changing session starts first,
        so LangSpan's notice of the changed signature reaches each dependent
        before it writes its call, and its first write is the informed one."""
        record = self.run_driver("langspan", "discovered_change", 5, "follow", stagger=15)
        self.assertEqual(record["stagger_s"], 15)
        self.assertEqual(record["integrated_tests"], "passed", record)
        dependents = [t for t in record["tasks"] if t["role"] == "dependent"]
        output = self.root / "langspan-discovered_change-5-follow"

        def order(task):
            """Each hook call of TASK's session: event, tool, path, and whether it told of days_left."""
            return [(entry["at"], entry["event"], entry["tool"], (entry.get("file_path") or "")[-30:],
                     "days_left" in entry["stdout"])
                    for entry in map(json.loads, (output / task["hook_log"]).read_text().splitlines())]
        self.assertEqual([t["notice_before_write"] for t in dependents], [True] * 4,
                         "\n".join("%s: %s" % (t["session"], order(t)) for t in dependents))
        self.assertEqual([t["standin_informed"] for t in dependents], [True] * 4)
        self.assertEqual([t["notice_before_write"] for t in record["tasks"] if t["role"] == "changer"], [None])
        # The changer started first: its edit of the helper came before any dependent began.
        changer = next(t for t in record["tasks"] if t["role"] == "changer")
        changed_at = min(at for at, event, tool, path, _ in order(changer)
                         if event == "PreToolUse" and tool == "Edit" and path.endswith("shelf/dates.py"))
        self.assertTrue(all(order(t)[0][1] == "SessionStart" and order(t)[0][0] > changed_at for t in dependents),
                        (changed_at, [order(t)[0] for t in dependents]))
        # Each dependent wrote its module once: informed from the start, never revised.
        for task in dependents:
            log = [json.loads(line) for line in (output / task["hook_log"]).read_text().splitlines()]
            writes = [entry for entry in log if entry["event"] == "PreToolUse" and entry["tool"] == "Write"]
            self.assertEqual(len(writes), 1, writes)

    def test_dry_run_arm_awareness_only(self):
        self.check_arm("awareness_only", (2, 5))

    def test_dry_run_arm_langspan_ignore(self):
        # Ten sessions: ten coordinators sharing through one remote.
        self.check_arm("langspan", (2, 5, 10), modes=("ignore",))

    def test_dry_run_arm_langspan_follow(self):
        self.check_arm("langspan", (2, 5, 10), modes=("follow",))

    def test_a_dry_run_records_an_option_its_arm_ignores(self):
        output = self.root / "ignored"
        done = subprocess.run([sys.executable, "-B", str(HERE / "driver.py"), "--arm", "worktrees",
                               "--family", "rename_vs_new_caller", "--sessions", "2", "--seed", "1",
                               "--langspan", str(langspan_binary()), "--run", "--output", str(output)],
                              capture_output=True, text=True, timeout=300)
        record = json.loads((output / "record.json").read_text())
        driver.validate(record, driver.load_schema())
        self.assertEqual(record["ignored_options"], ["--langspan"], done.stderr)
        self.assertIsNone(record["versions"]["langspan_binary_sha256"])
        self.assertEqual(json.loads((output / "plan.json").read_text())["ignored_options"], ["--langspan"])


if __name__ == "__main__":
    unittest.main()
